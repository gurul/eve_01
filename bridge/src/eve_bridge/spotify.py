"""The owner's Spotify: what spotKnob does on the knob — now playing, play/pause, skip, volume, where it plays —
from the chat, the voice and the Mini App.

The owner asked on 2026-09-27 to "give eve spotKnob capabilities". spotKnob (gurul/spotify-knob) is a round ESP32
dial that drives the Spotify Web API directly: turn for volume, press to play or pause, swipe to skip, hold to pick
the Spotify Connect device the music plays on. This module is the same set of powers, on the same Web API, with
two more that a voice needs and a knob does not: **play something by name** ("play Daft Punk"), and **wake the
Mac's Spotify** when nothing is online to play on.

==================  ======================================  =================================================
What                Web API call                            Door
==================  ======================================  =================================================
now playing         ``GET  /v1/me/player``                  all three
play / pause        ``PUT  /v1/me/player/play|pause``       all three
next / previous     ``POST /v1/me/player/next|previous``    all three
volume              ``PUT  /v1/me/player/volume``           all three (relative: read, then set)
devices             ``GET  /v1/me/player/devices``          all three
move the music      ``PUT  /v1/me/player`` (``play: true``)  all three (spotKnob's device picker)
play by name        ``GET  /v1/search`` then ``PUT play``    the chat and the voice
==================  ======================================  =================================================

Three doors use one ``Spotify``, the way lights.py is wired:

* **Telegram** (and what rides its brain: calls, the Voice PE). A message that is **only** a music command
  ("pause the music", "next song", "music volume 40", "what's playing", "play the music on the kitchen speaker",
  "play Daft Punk on Spotify") is done by code, no model turn (``Spotify.match``). Anything else goes to the model,
  which has ``spotify_now_playing`` and ``spotify_control``.
* **The robot's live voice** (voice_agent.py) gets the same two tools.
* **The Mini App** has a Music card (``/api/spotify``): the track and its art, previous / play-pause / next, a
  volume slider and the device list.

Setup is one login, on this Mac (docs/spotify.md): ``eve-bridge spotify login --client-id ID``. It runs
Spotify's authorization-code flow with PKCE against the loopback redirect ``http://127.0.0.1:8888/callback`` — the
same redirect spotKnob's app already registers, so spotKnob's Spotify app can be reused as-is. PKCE needs no client
secret. The refresh token is kept in ``~/.config/eve-bridge/spotify.json`` (mode 600), never in the repo, and
is rewritten whenever Spotify rotates it. Playback control needs Spotify Premium, as on the knob.

``EVE_SPOTIFY=0`` turns it all off. With no login it is off too, and no tool is lent.
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import logging
import os
import re
import secrets
import shutil
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Optional, Sequence

log = logging.getLogger(__name__)

CONFIG_PATH = Path("~/.config/eve-bridge/spotify.json")
OFF_REASON = "Spotify is not set up on this computer (eve-bridge spotify login; EVE_SPOTIFY)"
# Said when a music command arrives with no login: the code path still knows the words, so the model never gets
# a Spotify request it has no tools for (2026-09-27: "spotify mode" on the Voice PE went to Composio and failed).
NOT_SET_UP_LINE = ("Spotify isn't set up on this Mac yet. Run: eve-bridge spotify login --client-id ID, "
                   "then restart eve.")
API = "https://api.spotify.com/v1"
ACCOUNTS = "https://accounts.spotify.com"
REDIRECT_URI = "http://127.0.0.1:8888/callback"     # spotKnob's registered redirect: one Spotify app serves both
LOGIN_PORT = 8888
# spotKnob's two, plus the currently-playing read. user-modify-playback-state covers volume, skip and transfer.
SCOPES = "user-read-playback-state user-modify-playback-state user-read-currently-playing"
HTTP_TIMEOUT_SECS = 10.0
TOKEN_MARGIN_SECS = 60.0           # refresh an access token this long before Spotify says it expires
VOLUME_STEP = 10                   # "turn it up": the knob moves 2% a detent; a spoken step is five detents
WAKE_SECS = 12.0                   # how long a freshly opened Spotify app gets to show up as a Connect device
MAC_APP = Path("/Applications/Spotify.app")
# Each Spotify Connect device keeps one colour, on the Voice PE's ring in the picker and on the Mini App's key
# (owner, 2026-09-27: "can each device be represented with another color"). Green is Spotify mode's own and red
# is a refusal, so neither is here. A device's colour is kept in COLORS_FILE from the first time it is seen.
DEVICE_COLORS: tuple[tuple[str, tuple[int, int, int]], ...] = (
    ("blue", (0, 90, 255)), ("orange", (255, 110, 0)), ("purple", (170, 0, 255)), ("cyan", (0, 200, 255)),
    ("yellow", (255, 200, 0)), ("pink", (255, 40, 140)), ("white", (200, 200, 200)))
COLORS_FILE = "spotify-colors.json"   # beside the login, ~/.config/eve-bridge/
ART_HOST = "https://i.scdn.co/"    # album art; anything else is dropped before it reaches a page

Http = Callable[[str, str, dict[str, str], Optional[bytes]], tuple[int, dict[str, str], bytes]]


class SpotifyError(Exception):
    """A Web API refusal, in words the owner can act on."""

    def __init__(self, status: int, reason: str, line: str) -> None:
        super().__init__(line)
        self.status, self.reason, self.line = status, reason, line


def urllib_http(method: str, url: str, headers: dict[str, str], body: Optional[bytes]) -> tuple[int, dict[str, str], bytes]:
    """One blocking HTTPS call: (status, headers, body). Never raises for an HTTP status, only for no answer."""
    req = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=HTTP_TIMEOUT_SECS) as r:   # noqa: S310 — fixed Spotify hosts
            return r.status, {k.lower(): v for k, v in r.headers.items()}, r.read()
    except urllib.error.HTTPError as e:
        return e.code, {k.lower(): v for k, v in (e.headers or {}).items()}, e.read() or b""


# ---- the login and the token ---------------------------------------------------------------------------------

def load(path: Optional[Path] = None) -> dict[str, Any]:
    p = (path or CONFIG_PATH).expanduser()
    try:
        d = json.loads(p.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return d if isinstance(d, dict) and d.get("client_id") and d.get("refresh_token") else {}


def save(cfg: dict[str, Any], path: Optional[Path] = None) -> Path:
    """Written private (600) from the first byte: the refresh token is the owner's Spotify account."""
    p = (path or CONFIG_PATH).expanduser()
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(cfg, f, indent=2)
    os.chmod(tmp, 0o600)
    os.replace(tmp, p)
    return p


def pkce_pair() -> tuple[str, str]:
    """(verifier, S256 challenge), RFC 7636."""
    verifier = secrets.token_urlsafe(64)[:128]
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    return verifier, challenge


def authorize_url(client_id: str, challenge: str, state: str) -> str:
    return f"{ACCOUNTS}/authorize?" + urllib.parse.urlencode({
        "client_id": client_id, "response_type": "code", "redirect_uri": REDIRECT_URI, "scope": SCOPES,
        "code_challenge_method": "S256", "code_challenge": challenge, "state": state})


def _token_post(form: dict[str, str], client_id: str, client_secret: str, http: Http) -> dict[str, Any]:
    headers = {"Content-Type": "application/x-www-form-urlencoded"}
    if client_secret:
        headers["Authorization"] = "Basic " + base64.b64encode(f"{client_id}:{client_secret}".encode()).decode()
    else:
        form = {**form, "client_id": client_id}          # PKCE: the client id rides in the body, no secret
    status, _, body = http("POST", f"{ACCOUNTS}/api/token", headers, urllib.parse.urlencode(form).encode())
    try:
        data = json.loads(body or b"{}")
    except ValueError:
        data = {}
    if status != 200 or not data.get("access_token"):
        # the body names the error ("invalid_grant") and never echoes the token or the secret
        why = str(data.get("error_description") or data.get("error") or f"HTTP {status}")
        raise SpotifyError(status, "token", f"Spotify refused the login ({why}). Run: eve-bridge spotify login")
    return data


class Token:
    """The access token, refreshed before it runs out; a rotated refresh token is saved at once."""

    def __init__(self, cfg: dict[str, Any], http: Http, save_to: Optional[Path] = None,
                 clock: Callable[[], float] = time.monotonic) -> None:
        self.cfg = dict(cfg)
        self.http = http
        self.save_to = save_to
        self.clock = clock
        self._access = ""
        self._until = 0.0
        self._lock = threading.Lock()      # two calls off the event loop at once refresh once, not twice

    def get(self, force: bool = False) -> str:
        with self._lock:
            return self._get(force)

    def _get(self, force: bool) -> str:
        if self._access and not force and self.clock() < self._until:
            return self._access
        data = _token_post({"grant_type": "refresh_token", "refresh_token": self.cfg["refresh_token"]},
                           self.cfg["client_id"], self.cfg.get("client_secret", ""), self.http)
        self._access = data["access_token"]
        self._until = self.clock() + float(data.get("expires_in", 3600)) - TOKEN_MARGIN_SECS
        rotated = data.get("refresh_token")
        if rotated and rotated != self.cfg["refresh_token"]:
            # PKCE refresh tokens rotate: the old one may stop working, so the new one is kept before anything else
            self.cfg["refresh_token"] = rotated
            if self.save_to is not None:
                save(self.cfg, self.save_to)
        log.info("spotify: access token refreshed")
        return self._access


# ---- the Web API ---------------------------------------------------------------------------------------------

def _refusal(status: int, data: dict[str, Any], headers: dict[str, str]) -> SpotifyError:
    err = data.get("error") if isinstance(data.get("error"), dict) else {}
    reason = str(err.get("reason") or "")
    message = str(err.get("message") or "")
    if status == 404 and (reason == "NO_ACTIVE_DEVICE" or "device" in message.lower()):
        return SpotifyError(status, "NO_ACTIVE_DEVICE", "Spotify isn't playing on any device right now.")
    if status == 403 and reason == "PREMIUM_REQUIRED":
        return SpotifyError(status, reason, "Controlling playback needs Spotify Premium.")
    if status == 403:
        return SpotifyError(status, reason or "RESTRICTED",
                            "Spotify won't take that command on this device" + (f" ({message})." if message else "."))
    if status == 429:
        wait = headers.get("retry-after", "")
        return SpotifyError(status, "RATE_LIMITED", "Spotify says slow down" + (f"; try in {wait} s." if wait else "."))
    if status == 401:
        return SpotifyError(status, "UNAUTHORIZED", "Spotify turned the login down. Run: eve-bridge spotify login")
    return SpotifyError(status, reason or "HTTP", f"Spotify answered {status}" + (f": {message}." if message else "."))


class SpotifyApi:
    """The few Web API calls spotKnob makes, plus search. Blocking; ``Spotify`` runs them off the event loop."""

    def __init__(self, token: Token, http: Http) -> None:
        self.token = token
        self.http = http

    def call(self, method: str, path: str, query: Optional[dict[str, Any]] = None,
             body: Optional[dict[str, Any]] = None) -> Any:
        url = API + path + ("?" + urllib.parse.urlencode({k: v for k, v in query.items() if v is not None})
                            if query else "")
        payload = json.dumps(body).encode() if body is not None else (b"" if method in ("PUT", "POST") else None)
        for attempt in (0, 1):
            headers = {"Authorization": "Bearer " + self.token.get(force=attempt == 1),
                       "Content-Type": "application/json"}
            status, h, raw = self.http(method, url, headers, payload)
            if status == 401 and attempt == 0:
                continue                     # an expired access token: refreshed and retried once, like the knob
            break
        if status in (200, 201, 202, 204) and not raw:
            return None
        try:
            data = json.loads(raw) if raw else {}
        except ValueError:
            data = {}
        if 200 <= status < 300:
            return data
        raise _refusal(status, data if isinstance(data, dict) else {}, h)

    def player(self) -> Optional[dict[str, Any]]:
        return self.call("GET", "/me/player", {"additional_types": "track,episode"})

    def devices(self) -> list[dict[str, Any]]:
        return list((self.call("GET", "/me/player/devices") or {}).get("devices") or [])

    def play(self, device_id: Optional[str] = None, uris: Optional[list[str]] = None,
             context_uri: Optional[str] = None) -> None:
        body: dict[str, Any] = {}
        if uris:
            body["uris"] = uris
        if context_uri:
            body["context_uri"] = context_uri
        self.call("PUT", "/me/player/play", {"device_id": device_id} if device_id else None, body or None)

    def pause(self) -> None:
        self.call("PUT", "/me/player/pause")

    def next(self) -> None:
        self.call("POST", "/me/player/next")

    def previous(self) -> None:
        self.call("POST", "/me/player/previous")

    def volume(self, percent: int, device_id: Optional[str] = None) -> None:
        self.call("PUT", "/me/player/volume", {"volume_percent": int(percent), "device_id": device_id})

    def transfer(self, device_id: str, play: bool = True) -> None:
        self.call("PUT", "/me/player", body={"device_ids": [device_id], "play": play})

    def search(self, query: str, kind: str) -> Optional[dict[str, Any]]:
        data = self.call("GET", "/search", {"q": query, "type": kind, "limit": 1}) or {}
        items = ((data.get(kind + "s") or {}).get("items") or [])
        return next((i for i in items if isinstance(i, dict) and i.get("uri")), None)


FORGET_DEVICE_DAYS = 30   # a device not seen this long gives its colour back (a guest's phone, a hotel TV)


class DeviceColors:
    """Which colour each device has: the first colour no known device holds. A device keeps it while it is seen;
    one not seen for ``FORGET_DEVICE_DAYS`` is forgotten, so one-off devices do not use up the seven colours
    (owner, 2026-09-27). Past seven devices colours repeat, the least-used first.

    The file maps a name to ``{"i": colour index, "seen": day}``; the day is days since 1970 (UTC). A bare index
    (the first version's shape) reads as seen today."""

    def __init__(self, path: Optional[Path] = None, today: Callable[[], int] = lambda: int(time.time() // 86400),
                 forget_days: int = FORGET_DEVICE_DAYS) -> None:
        self.path = path.expanduser() if path is not None else None
        self.today = today
        self.forget_days = forget_days
        self.known: dict[str, dict[str, int]] = {}
        if self.path is not None:
            try:
                raw = json.loads(self.path.read_text(encoding="utf-8"))
                for k, v in raw.items():
                    if isinstance(v, dict):
                        self.known[str(k)] = {"i": int(v["i"]) % len(DEVICE_COLORS), "seen": int(v.get("seen", 0))}
                    else:
                        self.known[str(k)] = {"i": int(v) % len(DEVICE_COLORS), "seen": self.today()}
            except (OSError, ValueError, AttributeError, TypeError, KeyError):
                self.known = {}

    def _save(self) -> None:
        if self.path is None:
            return
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            self.path.write_text(json.dumps(self.known, indent=2), encoding="utf-8")
        except OSError as e:
            log.warning("spotify: could not keep device colours (%s)", e)

    def index(self, name: str) -> int:
        today = self.today()
        entry = self.known.get(name)
        if entry is None:
            gone = [n for n, e in self.known.items() if today - e["seen"] > self.forget_days]
            for n in gone:
                del self.known[n]
            if gone:
                log.info("spotify: forgot %d device(s) not seen in %d days", len(gone), self.forget_days)
            used = [e["i"] for e in self.known.values()]
            entry = self.known[name] = {"i": min(range(len(DEVICE_COLORS)), key=lambda i: (used.count(i), i)),
                                        "seen": today}
            self._save()
        elif entry["seen"] != today:
            entry["seen"] = today                    # written at most once a day per device
            self._save()
        return entry["i"]

    def rgb(self, name: str) -> tuple[int, int, int]:
        return DEVICE_COLORS[self.index(name)][1]

    def hex(self, name: str) -> str:
        return "#%02x%02x%02x" % self.rgb(name)


# ---- what is playing, in plain words -------------------------------------------------------------------------

def now_of(player: Optional[dict[str, Any]]) -> dict[str, Any]:
    """The player state, trimmed to what a person or a model needs. No ids beyond the device's name."""
    if not player or not player.get("item"):
        dev = (player or {}).get("device") or {}
        return {"playing": False, "track": None, **({"device": _device_public(dev)} if dev else {})}
    item = player["item"]
    artists = [a.get("name", "") for a in item.get("artists") or [] if a.get("name")]
    show = (item.get("show") or {}).get("name")                      # a podcast episode has a show, not artists
    album = item.get("album") or {}
    images = album.get("images") or item.get("images") or []
    art = next((i.get("url") for i in images if str(i.get("url", "")).startswith(ART_HOST)), None)
    out: dict[str, Any] = {
        "playing": bool(player.get("is_playing")),
        "track": item.get("name") or "",
        "artists": artists or ([show] if show else []),
        "album": album.get("name") or show or "",
        "progress_s": int((player.get("progress_ms") or 0) / 1000),
        "duration_s": int((item.get("duration_ms") or 0) / 1000),
        "shuffle": bool(player.get("shuffle_state")),
        "repeat": player.get("repeat_state") or "off",
    }
    if art:
        out["art"] = art
    if player.get("device"):
        out["device"] = _device_public(player["device"])
    return out


def _device_public(d: dict[str, Any]) -> dict[str, Any]:
    out = {"name": d.get("name") or "", "type": (d.get("type") or "").lower(), "active": bool(d.get("is_active"))}
    if isinstance(d.get("volume_percent"), int):
        out["volume"] = d["volume_percent"]
    if d.get("is_restricted"):
        out["restricted"] = True                            # the Web API refuses to command it: listed, not pickable
    return out


def said_now(now: dict[str, Any]) -> str:
    if not now.get("track"):
        return "Nothing is playing on Spotify."
    by = _join(now.get("artists") or [])
    where = (now.get("device") or {}).get("name")
    state = "Playing" if now.get("playing") else "Paused on"
    return f"{state} {now['track']}" + (f" by {by}" if by else "") + (f" on {where}" if where else "") + "."


def _join(names: Sequence[str]) -> str:
    names = [n for n in names if n]
    if len(names) <= 1:
        return names[0] if names else ""
    return ", ".join(names[:-1]) + " and " + names[-1]


def _norm(text: str) -> str:
    return re.sub(r"\s+", " ", re.sub(r"[^\w%'\s-]", " ", (text or "").lower().replace("’", "'"))).strip()


# ---- the tools -----------------------------------------------------------------------------------------------

ACTIONS = ("play", "pause", "next", "previous", "volume", "transfer", "mode_on", "mode_off")
KINDS = ("track", "artist", "album", "playlist")
TOOLS: list[dict[str, Any]] = [
    {"type": "function", "name": "spotify_now_playing", "strict": True,
     "description": "What the owner's Spotify is playing (track, artists, album, playing or paused, volume, on "
                    "which device) and the Spotify Connect devices it could play on.",
     "parameters": {"type": "object", "additionalProperties": False, "required": [], "properties": {}}},
    {"type": "function", "name": "spotify_control", "strict": True,
     "description": "Control the owner's Spotify: play (resume, or play something by name), pause, next, "
                    "previous, volume, or transfer the music to another device. mode_on / mode_off turn Spotify "
                    "mode on the Voice PE on or off (its ring green, its dial the volume, clicks play/pause, skip "
                    "and go back). Pass null for what you don't use.",
     "parameters": {"type": "object", "additionalProperties": False,
                    "required": ["action", "query", "kind", "volume", "volume_change", "device"],
                    "properties": {
                        "action": {"type": "string", "enum": list(ACTIONS)},
                        "query": {"type": ["string", "null"],
                                  "description": "play only: what to play by name (\"Daft Punk\", \"Blinding "
                                                 "Lights\", \"lofi beats\"); null resumes what was playing."},
                        "kind": {"type": ["string", "null"], "enum": [*KINDS, None],
                                 "description": "play with a query: track (default), artist, album or playlist."},
                        "volume": {"type": ["integer", "null"], "description": "volume: 0-100 percent; or null."},
                        "volume_change": {"type": ["integer", "null"],
                                          "description": "volume: a change such as 10 or -10, when no level was "
                                                         "asked; or null."},
                        "device": {"type": ["string", "null"],
                                   "description": "transfer (required) or play: a device's name from "
                                                  "spotify_now_playing; null for the current device."}}}},
]
TOOL_NAMES = tuple(t["name"] for t in TOOLS)

INSTRUCTIONS = """You can control the owner's Spotify with spotify_control (play, pause, next, previous, volume,
transfer to another device) and read it with spotify_now_playing. To play something by name, pass it as the query:
an artist's name with kind artist plays that artist; a mood ("something chill") is a playlist query. "Turn it up"
is volume_change 10. Say what you did in a few words."""


# ---- the code path -------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Command:
    """A music command the code understood."""
    action: str                         # now, play, pause, next, previous, volume, transfer
    query: str = ""
    volume: Optional[int] = None
    change: Optional[int] = None
    device: str = ""
    on: Optional[bool] = None           # "mode": Spotify mode on the Voice PE (music_mode.py)


_LEAD = r"(?:(?:hey|ok|okay)\s+)?(?:eve\s+)?(?:(?:please|pls|can you|could you|would you)\s+)?"
_TAIL = r"(?:\s+(?:please|pls|thanks|thank you|eve))?"
_MUSIC = r"(?:the\s+|my\s+)?(?:music|spotify|song|track|playback|tunes)"
_ON_SPOTIFY = r"(?:on|in|from|with)\s+spotify"
_PATTERNS: list[tuple[re.Pattern[str], Callable[[re.Match[str]], Command]]] = [
    (re.compile(r"(?:(?:turn|switch|put)\s+(on|off)\s+)?(?:the\s+)?spotify\s+mode(?:\s+(on|off))?"
                r"|(?:start|enter|go\s+into)\s+spotify\s+mode"),
     lambda m: Command("mode", on=(m.group(1) or m.group(2) or "on") == "on")),
    (re.compile(r"(?:exit|leave|stop|end|quit)\s+(?:the\s+)?spotify\s+mode"), lambda m: Command("mode", on=False)),
    (re.compile(r"(?:what'?s|what is)\s+(?:playing|this\s+song|this\s+track)(?:\s+on\s+spotify)?"
                r"|(?:what|which)\s+song\s+is\s+(?:this|playing|on)|now playing|what am i listening to"),
     lambda m: Command("now")),
    (re.compile(rf"(?:pause|stop)\s+{_MUSIC}|{_MUSIC}\s+(?:pause|stop|off)"), lambda m: Command("pause")),
    (re.compile(rf"(?:play|resume|unpause|start)\s+{_MUSIC}(?:\s+again|\s+{_ON_SPOTIFY})?|{_MUSIC}\s+(?:play|resume|on)"),
     lambda m: Command("play")),
    (re.compile(rf"(?:next|skip)(?:\s+(?:this|the))?(?:\s+(?:song|track))(?:\s+{_ON_SPOTIFY})?|skip this"),
     lambda m: Command("next")),
    (re.compile(r"(?:previous|prev|last|go back a|back a)\s+(?:song|track)|(?:play\s+)?(?:the\s+)?previous song"),
     lambda m: Command("previous")),
    (re.compile(rf"(?:set\s+)?{_MUSIC}\s+volume\s+(?:to\s+)?(\d{{1,3}})\s*%?|(?:set\s+)?(?:the\s+)?volume\s+of\s+"
                rf"{_MUSIC}\s+to\s+(\d{{1,3}})\s*%?|{_MUSIC}\s+(?:to|at)\s+(\d{{1,3}})\s*%"),
     lambda m: Command("volume", volume=int(next(g for g in m.groups() if g)))),
    (re.compile(rf"(?:turn\s+)?{_MUSIC}\s+(?:up|louder)|(?:turn\s+up|louder)\s+{_MUSIC}"),
     lambda m: Command("volume", change=VOLUME_STEP)),
    (re.compile(rf"(?:turn\s+)?{_MUSIC}\s+(?:down|quieter|softer)|(?:turn\s+down|quieter)\s+{_MUSIC}"),
     lambda m: Command("volume", change=-VOLUME_STEP)),
    (re.compile(rf"(?:play|move|switch|send|put|transfer)\s+{_MUSIC}\s+(?:on|to|onto)\s+(?:the\s+|my\s+)?"
                r"(?!spotify\b)([\w' -]{1,40}?)"),
     lambda m: Command("transfer", device=m.group(1).strip())),
    (re.compile(rf"play\s+(?!{_MUSIC}\s+{_ON_SPOTIFY})(.{{1,80}}?)\s+{_ON_SPOTIFY}"), lambda m: Command("play", query=m.group(1).strip())),
]


def match(text: str) -> Optional[Command]:
    """``Spotify.match`` without a Spotify: the words alone, for the not-logged-in reply."""
    return Spotify.match(None, text)  # type: ignore[arg-type]  — match reads no state


# ---- the whole thing -----------------------------------------------------------------------------------------

class Spotify:
    """The owner's Spotify: the tools, the code path, the Mini App panel."""

    def __init__(self, api: SpotifyApi, *, wake: Optional[Callable[[], bool]] = None,
                 sleep: Callable[[float], Any] = asyncio.sleep, wake_secs: float = WAKE_SECS,
                 colors: Optional[DeviceColors] = None) -> None:
        self.api = api
        # Spotify mode on the Voice PE (music_mode.MusicMode), lent by the daemon when the board is its controller
        self.mode: Any = None
        self.colors = colors if colors is not None else DeviceColors()     # kept only in memory
        self.wake = wake if wake is not None else open_mac_app
        self._sleep = sleep
        self.wake_secs = wake_secs

    async def _run(self, fn: Callable[..., Any], *args: Any, **kw: Any) -> Any:
        return await asyncio.to_thread(fn, *args, **kw)

    # -- reading --
    async def now_playing(self) -> dict[str, Any]:
        try:
            now = now_of(await self._run(self.api.player))
            devices = [{**_device_public(d), "color": self.colors.hex(d.get("name") or "")}
                       for d in await self._run(self.api.devices)]
        except SpotifyError as e:
            return {"ok": False, "reason": e.line}
        except OSError as e:                                  # no network, DNS, a timeout
            log.warning("spotify: now playing failed: %s", e)
            return {"ok": False, "reason": "Couldn't reach Spotify."}
        return {"ok": True, **now, "devices": devices}

    # -- which device --
    @staticmethod
    def pick(devices: Sequence[dict[str, Any]], name: str) -> tuple[Optional[dict[str, Any]], str]:
        """A device by name: exact, then a word of its name, then its kind ("phone", "speaker", "computer")."""
        n = _norm(name)
        n = re.sub(r"^(the|my)\s+", "", n)
        n = re.sub(r"\s+(speaker|device)$", "", n) if n not in ("speaker", "device") else n
        usable = [d for d in devices if not d.get("is_restricted")]
        kinds = {"mac": "computer", "laptop": "computer", "computer": "computer", "phone": "smartphone",
                 "iphone": "smartphone", "speaker": "speaker", "tv": "tv", "ipad": "tablet", "tablet": "tablet"}
        for test in (lambda d: _norm(d.get("name", "")) == n,
                     lambda d: bool(re.search(rf"\b{re.escape(n)}\b", _norm(d.get("name", "")))),
                     lambda d: (d.get("type") or "").lower() == kinds.get(n, "\0")):
            found = [d for d in usable if test(d)]
            if len(found) == 1:
                return found[0], ""
            if len(found) > 1:
                return None, f"More than one Spotify device matches {name!r}: " + _join([d["name"] for d in found]) + "."
        if any(_norm(d.get("name", "")) == n for d in devices):
            return None, f"{name} doesn't take remote control from Spotify."
        names = _join([d.get("name", "") for d in usable])
        return None, (f"No Spotify device called {name!r}. " + (f"The devices are {names}." if names else
                      "No device is online: open Spotify on a phone, computer or speaker."))

    async def _devices_or_wake(self) -> list[dict[str, Any]]:
        """The devices; when there are none, open the Mac's Spotify and wait for it to come online."""
        devices = await self._run(self.api.devices)
        if devices or not await self._run(self.wake):
            return devices
        log.info("spotify: no device online; opened the Mac's Spotify")
        waited = 0.0
        while waited < self.wake_secs:
            await self._sleep(1.0)
            waited += 1.0
            devices = await self._run(self.api.devices)
            if devices:
                break
        return devices

    async def _play(self, device_id: Optional[str], uris: Optional[list[str]] = None,
                    context: Optional[str] = None) -> None:
        """Play, and when nothing is active, start on a device that is online (the Mac's Spotify, opened if
        need be) rather than fail the way the Web API does."""
        try:
            await self._run(self.api.play, device_id, uris, context)
            return
        except SpotifyError as e:
            if e.reason != "NO_ACTIVE_DEVICE" or device_id:
                raise
        devices = [d for d in await self._devices_or_wake() if not d.get("is_restricted")]
        if not devices:
            raise SpotifyError(404, "NO_ACTIVE_DEVICE",
                               "No Spotify device is online. Open Spotify on a phone, computer or speaker.")
        target = next((d for d in devices if (d.get("type") or "").lower() == "computer"), devices[0])
        await self._run(self.api.play, target["id"], uris, context)

    # -- doing --
    async def control(self, action: str, *, query: str = "", kind: str = "", volume: Optional[int] = None,
                      volume_change: Optional[int] = None, device: str = "") -> dict[str, Any]:
        """One command; ``{"ok", "line", ...}``. The line is what to tell the owner."""
        try:
            return await self._control(action, query=query.strip(), kind=kind or "track", volume=volume,
                                        volume_change=volume_change, device=device.strip())
        except SpotifyError as e:
            log.info("spotify: %s refused (%s %s)", action, e.status, e.reason)
            return {"ok": False, "reason": e.line, "line": e.line}
        except OSError as e:                                  # no network, DNS, a timeout
            log.warning("spotify: %s failed: %s", action, e)
            return {"ok": False, "reason": "Couldn't reach Spotify.", "line": "Couldn't reach Spotify."}

    async def _control(self, action: str, *, query: str, kind: str, volume: Optional[int],
                       volume_change: Optional[int], device: str) -> dict[str, Any]:
        if action in ("mode_on", "mode_off"):
            if self.mode is None:
                line = "Spotify mode needs the Voice PE connected as eve's controller."
                return {"ok": False, "reason": line, "line": line}
            return {"ok": True, "line": await self.mode.set(action == "mode_on")}
        if action == "now":
            now = await self.now_playing()
            return {**now, "line": said_now(now) if now.get("ok") else now["reason"]}
        device_id: Optional[str] = None
        picked: dict[str, Any] = {}
        if device:
            found, why = self.pick(await self._devices_or_wake(), device)
            if found is None:
                return {"ok": False, "reason": why, "line": why}
            device_id, picked = found["id"], found
        if action == "play":
            if query:
                if kind not in KINDS:
                    return {"ok": False, "reason": f"kind must be one of {', '.join(KINDS)}", "line": "Unknown kind."}
                item = await self._run(self.api.search, query, kind)
                if item is None:
                    line = f"Spotify found nothing for {query!r}."
                    return {"ok": False, "reason": line, "line": line}
                uri = item["uri"]
                await self._play(device_id, uris=[uri] if kind == "track" else None,
                                 context=None if kind == "track" else uri)
                name = item.get("name") or query
                by = _join([a.get("name", "") for a in item.get("artists") or []]) if kind in ("track", "album") else ""
                owner = ((item.get("owner") or {}).get("display_name") or "") if kind == "playlist" else ""
                line = f"Playing {name}" + (f" by {by}" if by else "") + (f" ({owner}'s playlist)" if owner else "") \
                       + (f" on {picked['name']}" if picked else "") + "."
                return {"ok": True, "line": line, "playing": name}
            await self._play(device_id)
            return {"ok": True, "line": "Playing" + (f" on {picked['name']}." if picked else ".")}
        if action == "pause":
            try:
                await self._run(self.api.pause)
            except SpotifyError as e:
                if e.reason == "NO_ACTIVE_DEVICE":
                    return {"ok": True, "line": "Spotify is already quiet."}
                # pausing what is already paused is a 403 "restriction violated", not a failure
                if e.status == 403 and e.reason != "PREMIUM_REQUIRED" and \
                        not ((await self._run(self.api.player)) or {}).get("is_playing"):
                    return {"ok": True, "line": "Spotify is already quiet."}
                raise
            return {"ok": True, "line": "Paused."}
        if action in ("next", "previous"):
            await self._run(self.api.next if action == "next" else self.api.previous)
            await self._sleep(0.6)                       # the player takes a moment to change track
            now = now_of(await self._run(self.api.player))
            head = "Skipped." if action == "next" else "Back one."
            return {"ok": True, "line": head + (f" Now {now['track']}" + (f" by {_join(now['artists'])}"
                                                                          if now.get("artists") else "") + "."
                                                if now.get("track") else ""), **now}
        if action == "volume":
            if volume is None and volume_change is None:
                return {"ok": False, "reason": "give a volume or a volume_change", "line": "What volume?"}
            if volume is None:
                player = await self._run(self.api.player) or {}
                current = (player.get("device") or {}).get("volume_percent")
                if not isinstance(current, int):
                    raise SpotifyError(404, "NO_ACTIVE_DEVICE", "Spotify isn't playing on any device right now.")
                volume = current + int(volume_change or 0)
            level = max(0, min(100, int(volume)))
            await self._run(self.api.volume, level, device_id)
            return {"ok": True, "line": f"Spotify volume {level}%.", "volume": level}
        if action == "transfer":
            if not device_id:
                return {"ok": False, "reason": "transfer needs a device", "line": "Which device?"}
            await self._run(self.api.transfer, device_id, True)
            return {"ok": True, "line": f"Playing on {picked['name']}."}
        return {"ok": False, "reason": f"unknown action {action!r}", "line": "I can't do that with Spotify."}

    # -- the model's tools --
    def tools(self) -> list[dict[str, Any]]:
        return json.loads(json.dumps(TOOLS))

    def instructions(self) -> str:
        return INSTRUCTIONS

    async def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        if name == "spotify_now_playing":
            return await self.now_playing()
        if name == "spotify_control":
            vol, step = args.get("volume"), args.get("volume_change")
            out = await self.control(str(args.get("action") or ""), query=str(args.get("query") or ""),
                                     kind=str(args.get("kind") or ""),
                                     volume=int(vol) if isinstance(vol, (int, float)) and not isinstance(vol, bool)
                                     else None,
                                     volume_change=int(step) if isinstance(step, (int, float))
                                     and not isinstance(step, bool) else None,
                                     device=str(args.get("device") or ""))
            return out
        return {"ok": False, "reason": f"unknown tool {name}"}

    # -- a message that is only a music command --
    def match(self, text: str) -> Optional[Command]:
        """A Command when the whole message is a music command, else None (the model answers). Anchored at both
        ends: "pause the music in ten minutes" leaves words over, so the model gets it."""
        t = _norm(text)
        if not t or len(t) > 100:
            return None
        for pattern, make in _PATTERNS:
            # the lead and the tail only have non-capturing groups: the pattern's own groups come through as they are
            m = re.fullmatch(_LEAD + "(?:" + pattern.pattern + ")" + _TAIL, t)
            if m:
                cmd = make(m)
                return None if cmd.volume is not None and cmd.volume > 100 else cmd
        return None

    async def run(self, cmd: Command) -> str:
        if cmd.action == "mode":
            return str((await self.control("mode_on" if cmd.on else "mode_off"))["line"])
        out = await self.control(cmd.action, query=cmd.query, volume=cmd.volume, volume_change=cmd.change,
                                 device=cmd.device)
        log.info("spotify: by code, %s -> %s", cmd.action, "done" if out.get("ok") else "refused")
        return str(out.get("line") or out.get("reason") or "")

    # -- the Mini App's Music card --
    async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
        """``{"action": "status"}`` reads the player and the devices; ``play``, ``pause``, ``next``, ``previous``,
        ``volume`` (``volume``), ``transfer`` (``device``) change it and answer with the fresh state."""
        action = str(body.get("action") or "status")
        if action == "status":
            return {**await self.now_playing(), "mode": self.mode.on if self.mode is not None else None}
        if action == "mode":
            out = await self.control("mode_on" if body.get("on") is True else "mode_off")
            return {**await self.now_playing(), "ok": out["ok"], "line": out["line"],
                    "mode": self.mode.on if self.mode is not None else None}
        if action not in ACTIONS or action == "play" and body.get("query"):
            return {"ok": False, "line": "Not something the Music card does."}
        vol = body.get("volume")
        out = await self.control(action, volume=int(vol) if isinstance(vol, (int, float)) and not isinstance(vol, bool)
                                 else None, device=str(body.get("device") or ""))
        if not out.get("ok"):
            return {"ok": False, "line": out.get("line") or out.get("reason")}
        if action in ("play", "pause", "transfer"):
            await self._sleep(0.4)                       # the player's state lags a command by a beat
        now = await self.now_playing()
        return {**now, "ok": True, "line": out.get("line", ""),
                "mode": self.mode.on if self.mode is not None else None}

    async def close(self) -> None:
        return None


def open_mac_app() -> bool:
    """Open the Mac's Spotify in the background, so it shows up as a Connect device. False when there is none."""
    if sys.platform != "darwin" or not MAC_APP.exists() or shutil.which("open") is None:
        return False
    try:
        subprocess.run(["open", "-g", "-a", "Spotify"], check=True, timeout=10, capture_output=True)
        return True
    except (OSError, subprocess.SubprocessError):
        return False


# ---- construction --------------------------------------------------------------------------------------------

def enabled(environ: Any = None) -> bool:
    env = os.environ if environ is None else environ
    return str(env.get("EVE_SPOTIFY", "")).strip().lower() not in ("0", "false", "no", "off")


def make_spotify(path: Optional[Path] = None, environ: Any = None, http: Http = urllib_http) -> Optional[Spotify]:
    """The owner's Spotify, or None when switched off or never logged in (then no tool is lent)."""
    if not enabled(environ):
        log.info("spotify: off (EVE_SPOTIFY)")
        return None
    p = (path or CONFIG_PATH).expanduser()
    cfg = load(p)
    if not cfg:
        log.info("spotify: not logged in (eve-bridge spotify login)")
        return None
    log.info("spotify: on")
    return Spotify(SpotifyApi(Token(cfg, http, save_to=p), http), colors=DeviceColors(p.with_name(COLORS_FILE)))


# ---- the CLI -------------------------------------------------------------------------------------------------

def login(client_id: str, client_secret: str = "", path: Optional[Path] = None, http: Http = urllib_http,
          out: Callable[[str], Any] = print, open_browser: Optional[Callable[[str], Any]] = None,
          timeout: float = 300.0) -> int:
    """The one-time authorization-code + PKCE flow on the loopback redirect; saves the refresh token."""
    import http.server as http_server  # not `http`: that name is the injected transport
    import threading
    import webbrowser

    verifier, challenge = pkce_pair()
    state = secrets.token_urlsafe(16)
    got: dict[str, str] = {}
    done = threading.Event()

    class Handler(http_server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802 — stdlib naming
            u = urllib.parse.urlparse(self.path)
            if u.path != "/callback":
                self.send_response(404)
                self.end_headers()
                return
            q = urllib.parse.parse_qs(u.query)
            if q.get("state", [""])[0] != state:
                got["error"] = "state mismatch"
            else:
                got["code"] = q.get("code", [""])[0]
                got["error"] = q.get("error", [""])[0]
            page = (b"<h2>eve has Spotify. You can close this tab.</h2>" if got.get("code") and not got.get("error")
                    else b"<h2>Spotify login failed. Check the terminal.</h2>")
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(page)))
            self.end_headers()
            self.wfile.write(page)
            done.set()

        def log_message(self, *args: Any) -> None:
            return None

    try:
        server = http_server.HTTPServer(("127.0.0.1", LOGIN_PORT), Handler)
    except OSError as e:
        out(f"Port {LOGIN_PORT} is busy ({e}). Close whatever holds it and run this again.")
        return 1
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = authorize_url(client_id, challenge, state)
    out(f"Listening on {REDIRECT_URI}. The Spotify app's Redirect URI must be exactly that.")
    out("If no browser opens, paste this:\n\n" + url + "\n")
    (open_browser or webbrowser.open)(url)
    try:
        if not done.wait(timeout):
            out("Timed out waiting for Spotify's redirect.")
            return 1
    finally:
        server.shutdown()
    if got.get("error") or not got.get("code"):
        out(f"Login failed: {got.get('error') or 'no code came back'}.")
        return 1
    form = {"grant_type": "authorization_code", "code": got["code"], "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier}
    try:
        data = _token_post(form, client_id, client_secret, http)
    except SpotifyError as e:
        out(e.line)
        return 1
    cfg = {"client_id": client_id, "refresh_token": data.get("refresh_token", ""), "scopes": SCOPES}
    if client_secret:
        cfg["client_secret"] = client_secret
    if not cfg["refresh_token"]:
        out("Spotify sent no refresh token.")
        return 1
    p = save(cfg, path)
    out(f"Logged in. Saved to {p} (private). Restart the daemon to lend it to the chat and the voice.")
    return 0


def cli(argv: Sequence[str], path: Optional[Path] = None, out: Callable[[str], Any] = print,
        http: Http = urllib_http) -> int:
    """``eve-bridge spotify login [--client-id ID] [--secret-stdin] | status | devices | play [QUERY]
    [--kind K] [--device D] | pause | next | previous | volume N | transfer DEVICE | logout``."""
    import argparse

    p = argparse.ArgumentParser(prog="eve-bridge spotify")
    sub = p.add_subparsers(dest="act", required=True)
    lg = sub.add_parser("login", help="Log eve in to Spotify, once (a browser opens)")
    lg.add_argument("--client-id", default=os.environ.get("SPOTIFY_CLIENT_ID", ""),
                    help="the Spotify app's client id (or SPOTIFY_CLIENT_ID); spotKnob's app works")
    lg.add_argument("--secret-stdin", action="store_true",
                    help="read a client secret from stdin (not needed: PKCE logs in with the id alone)")
    sub.add_parser("logout", help="Forget the Spotify login on this Mac")
    sub.add_parser("status", help="What is playing, and where")
    sub.add_parser("devices", help="The Spotify Connect devices online")
    pl = sub.add_parser("play", help="Resume, or play something by name")
    pl.add_argument("query", nargs="*")
    pl.add_argument("--kind", choices=KINDS, default="track")
    pl.add_argument("--device", default="")
    for name in ("pause", "next", "previous"):
        sub.add_parser(name)
    v = sub.add_parser("volume")
    v.add_argument("percent", type=int)
    t = sub.add_parser("transfer", help="Move the music to another device")
    t.add_argument("device", nargs="+")
    a = p.parse_args(list(argv))
    cfg_path = (path or CONFIG_PATH).expanduser()

    if a.act == "login":
        if not a.client_id:
            out("Give the Spotify app's client id: --client-id ID (developer.spotify.com/dashboard). See docs/spotify.md")
            return 2
        secret = sys.stdin.readline().strip() if a.secret_stdin else ""
        return login(a.client_id.strip(), secret, cfg_path, http, out)
    if a.act == "logout":
        try:
            cfg_path.unlink()
            out(f"Forgot the Spotify login ({cfg_path}).")
        except FileNotFoundError:
            out("Spotify was not logged in.")
        return 0
    cfg = load(cfg_path)
    if not cfg:
        out(f"Not logged in ({cfg_path}). Run: eve-bridge spotify login --client-id ID")
        return 1
    hub = Spotify(SpotifyApi(Token(cfg, http, save_to=cfg_path), http),
                  colors=DeviceColors(cfg_path.with_name(COLORS_FILE)))

    async def go() -> dict[str, Any]:
        if a.act == "status":
            now = await hub.now_playing()
            return {**now, "line": said_now(now) if now.get("ok") else now.get("reason")}
        if a.act == "devices":
            now = await hub.now_playing()
            return {"ok": now.get("ok", False), "devices": now.get("devices", []), "line": now.get("reason", "")}
        if a.act == "play":
            return await hub.control("play", query=" ".join(a.query), kind=a.kind, device=a.device)
        if a.act == "volume":
            return await hub.control("volume", volume=a.percent)
        if a.act == "transfer":
            return await hub.control("transfer", device=" ".join(a.device))
        return await hub.control(a.act)
    result = asyncio.run(go())
    if result.get("line"):
        out(result["line"])
    if a.act in ("status", "devices"):
        for d in result.get("devices") or []:
            out(f"  {'●' if d.get('active') else '○'} {d['name']}  [{d['type']}]"
                + (f"  {d['volume']}%" if "volume" in d else "") + ("  (no remote control)" if d.get("restricted") else ""))
    return 0 if result.get("ok") else 1
