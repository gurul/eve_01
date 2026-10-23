"""eve lite's two web doors: the Mac's own page on loopback, and the LAN door phones and laptops use
(docs/plans/eve-lite.md, M4).

The owner, 2026-10-13: "make the eve app a web app so that it can just be accessed from same wifi through web rtc".
The web app (ios/EveLink/Web/src/web, built into ``web_lite/``) is served by two listeners:

* **LocalServer**: ``http://localhost:48780`` (else the first free of 48782-48789), on 127.0.0.1 and ::1 only, plain
  HTTP: localhost is a secure context, so the Mac's browser may use the microphone. Always on. It holds the setup
  (the two sign-ins), the Mac's own settings (adding a phone, the devices, the LAN switch) and the chat, which the
  Mac's page fetches over loopback.
* **LanServer**: ``https://<LocalHostName>.local:48781`` (else 48790-48799), on every interface, IPv4 and IPv6. On
  from the first "Add a phone" (remembered), off from Settings. A connection is judged before TLS: a peer that is not
  loopback, private, link-local, ULA or on one of the Mac's own links (lan_net.peer_allowed) is dropped unread. Then
  ``writer.start_tls`` with the Mac's own certificate (lite_tls.py), TLS 1.2 or later. It holds pairing, the chat (for
  a phone whose WebRTC link is down), WebRTC signaling and the CA downloads; never the setup or the Mac's settings.

Every request is checked in this order, and refused at the first that fails:

1. ``Host`` is one of this door's names with its port (421): ``localhost``, ``127.0.0.1`` and ``[::1]`` here; the
   certificate's names and the Mac's current LAN addresses there. A rebound DNS name stops here.
2. The path is one of this door's routes (404), and the method one of the route's (405).
3. A request that is not a GET carries an ``Origin`` equal to the page's own origin (403), no chunked body (411), and
   a JSON body, or raw bytes for an upload (415): a page elsewhere cannot send either without a preflight, which is
   never answered. A GET is not asked for ``Origin``: browsers leave it off a same-origin GET.
4. The device cookie (401): ``eve_mac`` here, ``eve_dev`` there (web_auth.py). A browser keeps a cookie per host and
   not per port, so each door reads only its own name. A cookie the devices no longer know is cleared.

The routes (spec M4) are the constants below. The ones without a cookie: the page itself (``/``, ``/assets/*``),
``/healthz``, the Mac's one-time login code (``POST /api/mac/login``, from ``eve-bridge web open``), the two OAuth
callbacks (checked by their single-use ``state`` alone, since a SameSite=Strict cookie is not sent on the provider's
redirect back: onboarding.py), pairing (``POST /api/web/pair``, the open window's secret or 6-digit code) and the CA
downloads.

* **The chat** goes to the unchanged LinkChat (link_chat.py). ``/api/web/link/{turn,messages,upload,file/<id>}``
  are passed to ``LinkChat.serve`` on the real stream with ``Origin`` and the cookie removed and ``Authorization:
  Bearer <a nonce of this process>``, the only token it is given to check; every other ``/api/web/link/`` path is a
  404 at this door. The iOS app's rule stays: any Origin on ``/api/link`` is still refused there.
* **Sign-ins.** ``/onboard/<provider>/start`` sends the browser to OpenRouter or Composio with a fresh ``state``; the
  callback finishes the exchange, writes the credential (the env file, or composio-connect.json), sends the browser
  back to the setup page, and asks the daemon to restart (``WebContext.restart``) RESTART_DELAY_SECS later, once the
  page has loaded and can show that it is waiting. A callback that does not finish answers onboarding's status with a
  short page, and the setup page shows the same line (``error``).
* **Pairing.** ``POST /api/web/pair/open`` (the Mac's page) opens the LAN door, raises macOS's Local Network alert on
  purpose (lan_net.py), reissues the certificate when the Mac has a new name or address, and opens a 5-minute window:
  two links with the secret in the fragment (``.local`` for Apple devices, the IP for the rest), each as a QR code
  drawn here (segno, SVG data URIs: the page needs no npm dependency) and a 6-digit code.

Files: ``web.json`` (0600) keeps the ports each door last had (so the address and bookmarks stay put), the LAN switch
and the setup's Composio skip. Responses carry the spec's security headers, and the API's ``Cache-Control: no-store``.
There is no access log: lines carry counts, ports and exception types only (AGENTS rule 9), never a path, an
address, a name, a cookie or a code.

This is eve lite's only: ``start`` refuses outside EVE_PROFILE=lite, and nothing in full eve imports this module.
"""

from __future__ import annotations

import asyncio
import contextlib
import errno
import gzip
import hashlib
import hmac
import html
import inspect
import json
import logging
import math
import os
import re
import secrets
import socket
import ssl
import threading
import time
import urllib.parse
from dataclasses import KW_ONLY, dataclass, field
from pathlib import Path
from typing import Any, Awaitable, Callable, Iterable, Mapping, MutableMapping, Optional, Sequence

from . import lan_net, onboarding, openrouter, profile, web_auth
from .apps_mcp import _read_json, write_private_json

log = logging.getLogger(__name__)

# ---- where -------------------------------------------------------------------------------------------------------

LOCAL_PORTS: tuple[int, ...] = (48780, *range(48782, 48790))      # disjoint from the LAN door's
LAN_PORTS: tuple[int, ...] = (48781, *range(48790, 48800))
LOCAL_HOSTS: tuple[str, ...] = ("127.0.0.1", "::1")
LAN_HOSTS: tuple[str, ...] = ("0.0.0.0", "::")                     # every interface, IPv4 and IPv6
LOOPBACK_NAMES = ("localhost", "127.0.0.1", "[::1]")
WEB_FILE = Path("~/.config/eve-bridge/web.json")
WEB_DIR_ENV = "EVE_WEB_DIR"                                        # profile.LITE_CLASSIFIED: "kept"
PACKAGED_WEB_DIR = Path(__file__).with_name("web_lite")           # `pnpm build:web` (ios/EveLink/Web)

# ---- limits ------------------------------------------------------------------------------------------------------

HEAD_SECS = 15.0                      # to read a request's head
BODY_SECS = 30.0                      # to read a JSON body
TLS_SECS = 10.0                       # a TLS handshake on the LAN door
CLOSE_SECS = 2.0                      # how long stopping waits for open connections, and a connection for its close
RTC_SECS = 30.0                       # an offer's answer (aiortc gathers every candidate first)
APPS_SECS = 75.0                      # Settings → Apps' calls to Composio (composio_connect waits 60 s itself)
CONNECTED_SECS = 10.0                 # the second look after Composio sent no Connect Link
CREDIT_TTL_SECS = 15.0                # the OpenRouter credit is read again after this ("Check again")
CREDIT_WAIT_SECS = 4.0                # a page waits this long for a fresh read; then the last one known, and it lands later
NAMES_TTL_SECS = 5.0                  # the LAN door's Host allowlist is read again after this
RESTART_DELAY_SECS = 2.0              # after a sign-in: the redirect and the page load before eve restarts
MAX_CONNECTIONS = 128                 # open connections per door; one more is closed at once
MAX_PER_PEER = 32                     # and from one address: one device on the Wi-Fi cannot take every slot
MAX_HEADERS = 64
MAX_TARGET_CHARS = 8192
MAX_JSON_BYTES = 64 * 1024            # an SDP offer is at most 20,000 characters (lite_rtc.MAX_SDP_CHARS)
MAX_HEAD_HOLD = 64 * 1024             # a LinkChat head this long without its end goes out as it is
CACHE_MAX_BYTES = 64 * 1024 * 1024    # the page's files kept in memory, raw and gzipped
GZIP_MIN_BYTES = 1024

# ---- routes (spec M4) --------------------------------------------------------------------------------------------

HEALTH_PATH = "/healthz"
MAC_LOGIN_PATH = "/api/mac/login"
ONBOARD_STATE_PATH = "/onboard/state"
OPENROUTER_START_PATH = "/onboard/openrouter/start"
OPENROUTER_CALLBACK_PATH = "/onboard/openrouter/callback"
COMPOSIO_START_PATH = "/onboard/composio/start"
COMPOSIO_CALLBACK_PATH = "/onboard/composio/callback"
COMPOSIO_SKIP_PATH = "/onboard/composio/skip"
PAIR_OPEN_PATH = "/api/web/pair/open"
PAIR_PATH = "/api/web/pair"
DEVICES_PATH = "/api/web/devices"
REVOKE_PATH = "/api/web/devices/revoke"
LAN_PATH = "/api/web/lan"
RTC_PATH = "/api/web/rtc"
APPS_PATH = "/api/web/apps"
STATUS_PATH = "/api/web/status"
LOGOUT_PATH = "/api/web/logout"
CA_PATH = "/api/web/ca.crt"
MOBILECONFIG_PATH = "/api/web/eve.mobileconfig"
LINK_PREFIX = "/api/web/link/"        # → LinkChat's /api/link/
ASSET_RE = re.compile(r"^/assets/([A-Za-z0-9_][A-Za-z0-9._-]{0,127})$")    # one flat folder (vite.web.config.ts)
APP_CONNECT_RE = re.compile(r"^/api/web/apps/([a-z0-9][a-z0-9_]{0,63})/connect$")
LINK_FILE_RE = re.compile(r"^file/[0-9a-f]{32}$")                 # link_uploads.ID_RE

# ---- what goes out ------------------------------------------------------------------------------------------------

CSP = ("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; "
       "font-src 'self' data:; media-src 'self' blob:; connect-src 'self'; object-src 'none'; base-uri 'none'; "
       "form-action 'self'; frame-ancestors 'none'")    # inline styles: OpenUI injects <style> elements
SECURITY_HEADERS: dict[str, str] = {
    "Content-Security-Policy": CSP,
    "X-Content-Type-Options": "nosniff",
    "Referrer-Policy": "no-referrer",
    "Permissions-Policy": "microphone=(self), camera=(), geolocation=()",
    "X-Frame-Options": "DENY",                          # frame-ancestors, for a browser that predates it
    "Cross-Origin-Resource-Policy": "same-origin",      # no other site's page may embed or read an answer
}
# Added to LinkChat's own answers where it did not set them. Not the CSP: a PDF it serves inline opens in the
# browser's viewer, which a document policy can break.
LINK_HEADERS: dict[str, str] = {"Referrer-Policy": "no-referrer", "X-Content-Type-Options": "nosniff"}
LINK_DROPPED = frozenset({"origin", "cookie", "authorization"})    # never reach LinkChat
JSON_TYPE = "application/json"
HTML_TYPE = "text/html; charset=utf-8"
NO_STORE = "no-store"
IMMUTABLE = "public, max-age=31536000, immutable"                  # /assets/* names carry their content's hash
TYPES = {".html": HTML_TYPE, ".js": "text/javascript; charset=utf-8", ".mjs": "text/javascript; charset=utf-8",
         ".css": "text/css; charset=utf-8", ".json": JSON_TYPE, ".map": JSON_TYPE, ".svg": "image/svg+xml",
         ".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".webp": "image/webp",
         ".gif": "image/gif", ".ico": "image/x-icon", ".woff2": "font/woff2", ".woff": "font/woff", ".ttf": "font/ttf",
         ".otf": "font/otf", ".txt": "text/plain; charset=utf-8", ".wasm": "application/wasm",
         ".webmanifest": "application/manifest+json", ".mp3": "audio/mpeg", ".wav": "audio/wav"}
GZIP_TYPES = ("text/", "application/json", "image/svg+xml", "application/manifest+json")
REASONS = {200: "OK", 302: "Found", 303: "See Other", 400: "Bad Request", 401: "Unauthorized", 403: "Forbidden",
           404: "Not Found", 405: "Method Not Allowed", 409: "Conflict", 411: "Length Required",
           413: "Payload Too Large", 414: "URI Too Long", 415: "Unsupported Media Type", 421: "Misdirected Request",
           429: "Too Many Requests", 431: "Request Header Fields Too Large", 500: "Internal Server Error",
           502: "Bad Gateway", 503: "Service Unavailable", 504: "Gateway Timeout"}

READY, NEEDS_OPENROUTER = "READY", "NEEDS_OPENROUTER"      # lite.py's states; any other is shown only if it reads so

# What a page shows: one plain line each.
WRONG_HOST = "That address is not one of eve's on this Mac."
NOT_FOUND = "not found"
WRONG_METHOD = "wrong method"
WRONG_ORIGIN = "Only eve's own page can do that."
NO_CHUNKED = "Send the body with a Content-Length."
WRONG_TYPE = "Send the body as {want}."
BAD_BODY = "The body must be a JSON object."
TOO_LARGE = "The body is too large."
SIGNED_OUT_MAC = "This browser is not signed in to eve. Open Eve from Applications on this Mac."
SIGNED_OUT_DEV = "This device is not paired with eve. On the Mac: Settings, Phone access, Add a phone."
LOGIN_SPENT = "That sign-in link was already used or is out of date. Open Eve from Applications again."
NO_WINDOW = "No phone pairing is open on the Mac. On the Mac: Settings, Phone access, Add a phone."
WRONG_CODE = "That code didn't match. Check the six digits on the Mac and try again."
TOO_MANY = "Too many wrong tries, so that code is closed. Make a new one on the Mac: Settings, Phone access."
DEVICE_NOT_SAVED = "eve could not save this device on the Mac. Check the Mac's disk, then try again."
NOT_SAVED = "eve could not save that on the Mac. Check the Mac's disk, then try again."
NOT_READY = "eve isn't set up yet. Finish setting it up on the Mac, then try again."
RTC_OFF = "Direct links are off on this Mac, so chat goes over HTTPS and voice can't connect."
OFFER_REFUSED = "eve could not answer that link offer."
RTC_SLOW = "eve took too long to answer the link offer. Try again."
LAN_FAILED = ("eve could not start listening for phones on this Mac: every port it uses is taken. Restart the Mac, "
              "then try again.")
NO_ADDRESS = "This Mac has no address a phone on the Wi-Fi can reach. Join the Wi-Fi, then try again."
LAN_FLAG = "on must be true or false."
DEVICE_ID = "id must be a device's id, or all."
COMPOSIO_OUT = "Sign in to Composio first: on the Mac, Settings, Apps."
COMPOSIO_SLOW = "Composio is slow to answer. Try again in a minute."
COMPOSIO_DOWN = "Composio could not be reached from this Mac. Try again in a minute."
NOT_AN_APP = "eve does not connect that app."
NO_LINK = "Composio didn't send a sign-in link for {name}. Try again in a minute."
ALREADY = "{name} is already connected."
SIGNIN_BROKE = "That sign-in did not finish on this Mac. Start it again from eve's setup page."
NO_SIGNIN = "That sign-in is not available on this Mac right now. Restart eve, then try again."
TLS_OFF = "eve's certificate could not be read on this Mac. Turn phone access off and on again in Settings."
NOT_BUILT = "eve's web page is not installed on this Mac. Run eve lite's install line again."
FAILED = "That didn't work on the Mac. Try again."

_METHOD = re.compile(r"[A-Z]{1,16}")
_FIELD = re.compile(r"[!#$%&'*+.^_`|~0-9A-Za-z-]{1,64}")         # an HTTP field name (a token)
_SINGLE = frozenset({"host", "content-length", "content-type", "origin", "authorization", "transfer-encoding",
                     "x-filename"})                               # twice in one request is an attack, not a list
_STATE = re.compile(r"[A-Z][A-Z_]{0,31}")
_CREDIT_WHY = re.compile(r"HTTP \d{3}|[a-z][a-z ]{0,39}|[A-Z][A-Za-z]{0,39}")   # onboarding.credit's reasons
_OPAQUE = re.compile(r"[A-Za-z0-9_\-+/=.%]{24,}")                 # a run this long is a key or a code, not a word
_KINDS = (("iPhone", "iPhone"), ("iPad", "iPad"), ("Android", "Android"), ("CrOS", "Chromebook"),
          ("Macintosh", "Mac"), ("Windows", "Windows PC"), ("Linux", "Linux computer"))
_BROWSERS = (("EdgiOS", "Edge"), ("Edg/", "Edge"), ("OPR/", "Opera"), ("FxiOS", "Firefox"), ("Firefox/", "Firefox"),
             ("CriOS", "Chrome"), ("Chrome/", "Chrome"), ("Safari/", "Safari"))


# ---- small pure helpers -----------------------------------------------------------------------------------------

def _port(value: Any) -> Optional[int]:
    return value if isinstance(value, int) and not isinstance(value, bool) and 0 < value < 65536 else None


def _ports(candidates: Iterable[int], saved: Optional[int]) -> list[int]:
    """The ports to try, in order: the one this door had last time first (the address stays put), then the rest."""
    ports = list(dict.fromkeys(int(p) for p in candidates))
    if saved is not None and saved in ports:
        ports.remove(saved)
        ports.insert(0, saved)
    return ports


def _plain(line: Any, fallback: str) -> str:
    """A line for a page, at most 300 characters on one line; ``fallback`` when there is none, or when it holds a
    run that reads as a key, a token or a code. No page and no JSON from here carries one, whatever raised it."""
    text = " ".join(str(line or "").split())[:300]
    return fallback if not text or _OPAQUE.search(text) else text


def _credit_view(raw: Any) -> Optional[dict[str, Any]]:
    """onboarding.credit's answer as the page reads it, field by field: {ok, usd, funded} and a short ``error``
    reason. A field of any other shape is left out or null, so nothing else can ride along."""
    if not isinstance(raw, Mapping):
        return None
    usd = raw.get("usd")
    try:
        usd = float(usd) if isinstance(usd, (int, float)) and not isinstance(usd, bool) else None
    except OverflowError:
        usd = None
    if usd is not None and not math.isfinite(usd):
        usd = None
    funded = raw.get("funded")
    out: dict[str, Any] = {"ok": raw.get("ok") is True, "usd": usd, "funded": funded if isinstance(funded, bool) else None}
    why = raw.get("error")
    if isinstance(why, str) and _CREDIT_WHY.fullmatch(why):
        out["error"] = why
    if raw.get("rejected") is True:
        out["rejected"] = True
    return out


def _key_rejected(view: Any) -> bool:
    """Whether the credit read says the OpenRouter key was turned down (a 401), so a new sign-in is the one fix."""
    return isinstance(view, Mapping) and view.get("rejected") is True


def device_label(user_agent: Any, where: str) -> str:
    """How the device list names a browser: "iPhone · Safari", "Android · Chrome", "This Mac · Safari". Coarse on
    purpose: the kind of device and the browser, never a version or a model."""
    ua = user_agent if isinstance(user_agent, str) else ""
    browser = next((name for mark, name in _BROWSERS if mark in ua), "")
    kind = "This Mac" if where == "local" else next((name for mark, name in _KINDS if mark in ua), "A device")
    return f"{kind} · {browser}" if browser else kind


def _host_name(host: str) -> str:
    """The Host header without its port: "kitchen-mac.local", "192.168.1.20", "localhost", "[::1]"."""
    return host.rsplit(":", 1)[0] if host.rsplit(":", 1)[-1].isdigit() else host


def _query(qs: str) -> dict[str, str]:
    """A query's fields, the first of each name; {} for one that will not parse."""
    out: dict[str, str] = {}
    try:
        pairs = urllib.parse.parse_qsl(qs, keep_blank_values=True, max_num_fields=32)
    except ValueError:
        return out
    for name, value in pairs:
        out.setdefault(name, value)
    return out


def _media(headers: Mapping[str, str]) -> str:
    return headers.get("content-type", "").split(";", 1)[0].strip().lower()


def _content_type(name: str) -> str:
    return TYPES.get(os.path.splitext(name)[1].lower(), "application/octet-stream")


def _accepts_gzip(header: str) -> bool:
    """Whether Accept-Encoding takes gzip (or anything, ``*``) with a weight above zero."""
    for part in (header or "").split(","):
        coding, _, params = part.strip().partition(";")
        if coding.strip().lower() not in ("gzip", "*"):
            continue
        weight = 1.0
        for param in params.split(";"):
            name, _, value = param.strip().partition("=")
            if name.strip().lower() == "q":
                try:
                    weight = float(value)
                except ValueError:
                    weight = 0.0
        return weight > 0
    return False


def _page_html(title: str, line: str) -> bytes:
    """A short page with no script: a sign-in that did not finish, or no web page installed."""
    t, body = html.escape(title), html.escape(line)
    return ("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">"
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
            "<meta name=\"color-scheme\" content=\"light dark\">"
            f"<title>{t}</title><style>body{{font:16px/1.5 -apple-system,system-ui,sans-serif;max-width:32rem;"
            "margin:15vh auto;padding:0 1rem}a{color:inherit}</style></head>"
            f"<body><h1>{t}</h1><p>{body}</p><p><a href=\"/\">Back to eve</a></p></body></html>").encode()


_NO_SEGNO: list[bool] = []


def qr_svg(url: str) -> str:
    """``url`` as a QR code: an SVG data URI that scales to its box, black on white (readable on a dark page), with
    the standard quiet zone. "" when segno is missing (said once): the page then shows the link alone."""
    try:
        import segno
    except ImportError:
        if not _NO_SEGNO:
            _NO_SEGNO.append(True)
            log.warning("lite web: segno is not installed, so the pairing links have no QR code")
        return ""
    return segno.make(url, error="m").svg_data_uri(scale=1, border=4, dark="#000", light="#fff", omitsize=True)


def _quietly(fn: Callable[[], Any]) -> None:
    try:
        fn()
    except Exception as e:  # noqa: BLE001 — the type only; a hook that fails must not fail the request
        log.info("lite web: %s failed (%s)", getattr(fn, "__name__", "a hook"), type(e).__name__)


# ---- web.json ----------------------------------------------------------------------------------------------------

class _Saved:
    """``web.json`` (0600): the port each door had last, the LAN switch, and whether setup's Composio step was
    skipped. Read once; written whole and atomically on each change. Thread-safe."""

    def __init__(self, path: Path) -> None:
        self.path = Path(path).expanduser()
        data = _read_json(self.path)
        self.local_port = _port(data.get("local_port"))
        self.lan_port = _port(data.get("lan_port"))
        self.lan = data.get("lan") is True
        self.composio_skipped = data.get("composio_skipped") is True
        self._lock = threading.Lock()

    def save(self, **changes: Any) -> bool:
        """Write the file with ``changes``, then hold them. False (logged) when it could not be written: then nothing
        changed, so what the page shows is what the next start will do."""
        unknown = set(changes) - {"local_port", "lan_port", "lan", "composio_skipped"}
        if unknown:
            raise ValueError(f"web.json has no {sorted(unknown)}")
        with self._lock:
            data = {"version": 1, "local_port": self.local_port, "lan_port": self.lan_port, "lan": self.lan,
                    "composio_skipped": self.composio_skipped, **changes}
            try:
                write_private_json(self.path, data)
            except OSError as e:
                log.warning("lite web: web.json was not saved (%s)", type(e).__name__)
                return False
            for name, value in changes.items():
                setattr(self, name, value)
        return True


# ---- the page's files --------------------------------------------------------------------------------------------

class _Files:
    """The page's files, each read once and kept with its gzip until it changes on disk. Thread-safe."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._kept: dict[str, tuple[tuple[int, int, int], bytes, Optional[bytes]]] = {}
        self._size = 0

    def read(self, path: Path, root: Path, compress: bool) -> Optional[tuple[bytes, bool]]:
        """(body, gzipped) of ``path``, a regular file inside ``root`` once every link is followed; None for
        anything else. Blocking."""
        try:
            base = root.resolve(strict=True)
            real = path.resolve(strict=True)
            if not real.is_relative_to(base) or not real.is_file():
                return None
            st = real.stat()
        except (OSError, RuntimeError, ValueError):
            return None
        key, stamp = str(real), (st.st_ino, st.st_size, st.st_mtime_ns)
        with self._lock:
            kept = self._kept.get(key)
        if kept is None or kept[0] != stamp:
            try:
                data = real.read_bytes()
            except OSError:
                return None
            kept = (stamp, data, None)
        stamp, data, zipped = kept
        use = compress and len(data) >= GZIP_MIN_BYTES
        if use and zipped is None:
            zipped = gzip.compress(data, compresslevel=6, mtime=0)
            kept = (stamp, data, zipped)
        size = len(data) + len(zipped or b"")
        with self._lock:
            old = self._kept.pop(key, None)
            if old is not None:
                self._size -= len(old[1]) + len(old[2] or b"")
            if size <= CACHE_MAX_BYTES // 4:              # a bigger file is read each time, never kept
                if self._size + size > CACHE_MAX_BYTES:
                    self._kept.clear()
                    self._size = 0
                self._kept[key] = kept
                self._size += size
        return (zipped, True) if use and zipped is not None else (data, False)


# ---- the context -------------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class _Daemon:
    """The daemon's state as the pages may read it: a state word, a model key saved, calls can be taken."""

    state: str
    openrouter: bool
    voice: bool


class _Shared:
    """What the two doors share while the daemon runs: one per WebContext."""

    def __init__(self, state_path: Path) -> None:
        self.nonce = secrets.token_urlsafe(32)         # LinkChat's only token here (the LinkChat door)
        self.saved = _Saved(state_path)
        self.restarting = False
        self.error = ""                                # the last sign-in that did not finish, as its page said it
        self.local_network = "unknown"                 # lan_net.probe's last word on the Local Network privilege
        self.credit: Optional[tuple[str, float, dict[str, Any]]] = None   # (SHA-256 of the key, when, credit)
        self.credit_task: Optional[asyncio.Future[dict[str, Any]]] = None
        self.credit_for = ""
        self.rtc_doors: dict[str, str] = {}            # device id -> the door its WebRTC peer was signaled through
        self.lan: Optional[LanServer] = None           # the LAN door, once one was made (LocalServer.set_lan)
        self.tasks: set[asyncio.Task[Any]] = set()
        self.files = _Files()
        self.warned: set[str] = set()
        self._lan_lock: Optional[asyncio.Lock] = None

    @property
    def lan_lock(self) -> asyncio.Lock:
        if self._lan_lock is None:
            self._lan_lock = asyncio.Lock()
        return self._lan_lock


@dataclass(eq=False, repr=False)
class WebContext:
    """What the two doors are given (M11), read again on every request, so the daemon may set a part that comes up
    later (``link``, ``rtc``, ``apps``) on the same context:

    * ``link``: link_chat.LinkChat, the one chat door; None while eve needs its OpenRouter key (setup only).
    * ``rtc``: lite_rtc.RtcHub, or None (no WebRTC: the page chats over HTTPS, and voice is off).
    * ``devices``, ``pairing``, ``mac_login``: web_auth.Devices, PairingWindow and MacLogin.
    * ``tls``: lite_tls.LocalTls, the LAN door's certificate.
    * ``openrouter``, ``composio_login``: onboarding.OpenRouterLogin and ComposioConnectLogin.
    * ``apps``: composio_connect.ConnectBridge once Composio is signed in, else None.
    * ``status``: the daemon's own state, a mapping (or an awaitable of one): ``state`` (lite.py's ``READY`` or
      ``NEEDS_OPENROUTER``), and optionally ``openrouter`` and ``voice`` as booleans. Nothing else is read from it.
    * ``restart``: asks the daemon to restart, after a credential was written (sync, or returning an awaitable).
    * ``web_dir``: the page's files; None reads EVE_WEB_DIR, else the packaged ``web_lite``.
    * ``state_path``: ``web.json``.

    The keyword fields that follow are the daemon's defaults; tests replace them (ports 0, fakes for the network)."""

    link: Any
    rtc: Any
    devices: Any
    pairing: Any
    mac_login: Any
    tls: Any
    openrouter: Any
    composio_login: Any
    apps: Any
    status: Optional[Callable[[], Any]]
    restart: Optional[Callable[[], Any]]
    web_dir: Optional[Path]
    state_path: Path
    _: KW_ONLY
    environ: MutableMapping[str, str] = field(default_factory=lambda: os.environ)   # the model key, EVE_WEB_DIR
    env_path: Optional[Path] = None              # where save_openrouter_key writes; None: envfile.env_path(environ)
    credit: Callable[[str], Awaitable[Any]] = onboarding.credit
    hostname: Callable[[], str] = lan_net.local_hostname
    addresses: Callable[[], list[str]] = lan_net.private_addresses
    peer_allowed: Callable[[Any], bool] = lan_net.peer_allowed
    alert: Callable[[], Any] = lan_net.trigger_local_network_alert
    probe: Callable[[Any], str] = lan_net.probe
    local_hosts: Sequence[str] = LOCAL_HOSTS
    local_ports: Sequence[int] = LOCAL_PORTS
    lan_hosts: Sequence[str] = LAN_HOSTS
    lan_ports: Sequence[int] = LAN_PORTS
    restart_delay: float = RESTART_DELAY_SECS
    shared: _Shared = field(init=False)

    def __post_init__(self) -> None:
        self.state_path = Path(self.state_path).expanduser()
        self.shared = _Shared(self.state_path)

    def __repr__(self) -> str:                     # never the environment: it holds the model key
        return f"WebContext({self.state_path.name}, link {'on' if self.link is not None else 'off'})"

    def web_root(self) -> Path:
        if self.web_dir is not None:
            return Path(self.web_dir).expanduser()
        named = str(self.environ.get(WEB_DIR_ENV) or "").strip()
        return Path(named).expanduser() if named else PACKAGED_WEB_DIR


def _require_lite(ctx: WebContext) -> None:
    if not profile.is_lite(ctx.environ):
        raise RuntimeError("the web doors are eve lite's (EVE_PROFILE=lite); this is full eve")


# ---- HTTP ---------------------------------------------------------------------------------------------------------

class PortsTaken(OSError):
    """Every port a door may use is taken (LOCAL_PORTS, LAN_PORTS)."""


class _Refused(Exception):
    """A request refused before it reached a route: the status, and one plain line."""

    def __init__(self, status: int, line: str) -> None:
        super().__init__(line)
        self.status, self.line = status, line


@dataclass(eq=False, repr=False)                  # no repr: its headers hold the cookie
class _Request:
    method: str
    path: str
    query: str
    headers: dict[str, str]
    reader: asyncio.StreamReader
    writer: Any
    peername: Any
    host: str = ""
    device: Optional[web_auth.Device] = None
    token: str = ""
    responded: bool = False

    @property
    def peer(self) -> str:
        """The TCP peer's address, without its port."""
        return str(self.peername[0]) if isinstance(self.peername, (tuple, list)) and self.peername else ""


async def _read_head(reader: asyncio.StreamReader) -> tuple[str, str, str, dict[str, str]]:
    """(method, path, query, headers) of the next request; field names lowercase. _Refused for a head that is not
    plain HTTP/1.x in origin form; the asyncio errors for one that never arrives whole."""
    head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), HEAD_SECS)
    lines = head[:-4].decode("latin-1").split("\r\n")
    parts = lines[0].split(" ")
    if len(parts) != 3 or parts[2] not in ("HTTP/1.1", "HTTP/1.0") or not _METHOD.fullmatch(parts[0]):
        raise _Refused(400, "bad request line")
    method, target = parts[0], parts[1]
    if len(target) > MAX_TARGET_CHARS:
        raise _Refused(414, "the address is too long")
    if not target.startswith("/") or any(ord(c) <= 0x20 or ord(c) >= 0x7F for c in target):
        raise _Refused(400, "bad request target")
    if len(lines) - 1 > MAX_HEADERS:
        raise _Refused(431, "too many header fields")
    headers: dict[str, str] = {}
    for line in lines[1:]:
        name, sep, value = line.partition(":")
        if not sep or not _FIELD.fullmatch(name):       # an obs-fold or a space before the colon lands here too
            raise _Refused(400, "bad header field")
        name, value = name.lower(), value.strip(" \t")
        if any((ord(c) < 0x20 and c != "\t") or ord(c) == 0x7F for c in value):   # a bare LF, a NUL
            raise _Refused(400, "bad header field")
        if name in headers:
            if name in _SINGLE:
                raise _Refused(400, f"{name} given twice")
            headers[name] += ("; " if name == "cookie" else ", ") + value
        else:
            headers[name] = value
    path, _, query = target.partition("?")
    return method, path, query, headers


def _head_bytes(status: int, ctype: str, length: int, cache: str, headers: Iterable[tuple[str, str]]) -> bytes:
    lines = [f"HTTP/1.1 {status} {REASONS.get(status, 'OK')}", f"Content-Type: {ctype}", f"Content-Length: {length}",
             f"Cache-Control: {cache}", *(f"{k}: {v}" for k, v in SECURITY_HEADERS.items())]
    for name, value in headers:
        if "\r" in value or "\n" in value:
            raise ValueError("a header value with a line break")
        lines.append(f"{name}: {value}")
    lines.append("Connection: close")
    return ("\r\n".join(lines) + "\r\n\r\n").encode("latin-1")


class _AddedHeaders:
    """The stream LinkChat answers on, with LINK_HEADERS added to its answer's head where it did not set them.
    LinkChat writes each head whole and then the body (link_chat.py), with ``write`` and ``drain`` only; the head is
    held until its blank line, so a head written in pieces still gets them."""

    def __init__(self, writer: Any, extra: Mapping[str, str]) -> None:
        self._writer = writer
        self._extra = extra
        self._head: Optional[bytearray] = bytearray()       # None once the head went out

    def write(self, data: bytes) -> None:
        if self._head is None:
            self._writer.write(data)
            return
        self._head += data
        end = self._head.find(b"\r\n\r\n")
        if end < 0:
            if len(self._head) > MAX_HEAD_HOLD:
                self.flush()
            return
        head, rest = bytes(self._head[:end]), bytes(self._head[end:])
        self._head = None
        have = {line.split(b":", 1)[0].strip().lower() for line in head.split(b"\r\n")[1:]}
        added = [f"{name}: {value}".encode() for name, value in self._extra.items()
                 if name.lower().encode() not in have]
        self._writer.write(b"\r\n".join([head, *added]) + rest)

    def flush(self) -> None:
        held, self._head = self._head, None
        if held:
            self._writer.write(bytes(held))

    async def drain(self) -> None:
        await self._writer.drain()

    def __getattr__(self, name: str) -> Any:
        return getattr(self._writer, name)


# ---- the routes -------------------------------------------------------------------------------------------------

GET_HEAD = frozenset({"GET", "HEAD"})
GET = frozenset({"GET"})
POST = frozenset({"POST"})
LOCAL, LAN, BOTH = frozenset({"local"}), frozenset({"lan"}), frozenset({"local", "lan"})


@dataclass(frozen=True)
class _Route:
    methods: frozenset[str]
    doors: frozenset[str]
    handler: str
    cookie: bool = True                 # the door's device cookie; the rest prove themselves (module docstring)
    body: str = "json"                  # what a POST carries: "json", or "bytes" (an upload)


ROUTES: dict[str, _Route] = {
    "/": _Route(GET_HEAD, BOTH, "_index", cookie=False),
    "/index.html": _Route(GET_HEAD, BOTH, "_index", cookie=False),
    HEALTH_PATH: _Route(GET_HEAD, BOTH, "_health", cookie=False),
    MAC_LOGIN_PATH: _Route(POST, LOCAL, "_mac_login", cookie=False),
    ONBOARD_STATE_PATH: _Route(GET, LOCAL, "_onboard_state"),
    OPENROUTER_START_PATH: _Route(GET, LOCAL, "_openrouter_start"),
    OPENROUTER_CALLBACK_PATH: _Route(GET, LOCAL, "_openrouter_callback", cookie=False),
    COMPOSIO_START_PATH: _Route(GET, LOCAL, "_composio_start"),
    COMPOSIO_CALLBACK_PATH: _Route(GET, LOCAL, "_composio_callback", cookie=False),
    COMPOSIO_SKIP_PATH: _Route(POST, LOCAL, "_composio_skip"),
    PAIR_OPEN_PATH: _Route(POST, LOCAL, "_pair_open"),
    PAIR_PATH: _Route(POST, LAN, "_pair", cookie=False),
    DEVICES_PATH: _Route(GET, LOCAL, "_devices"),
    REVOKE_PATH: _Route(POST, LOCAL, "_devices_revoke"),
    LAN_PATH: _Route(POST, LOCAL, "_lan"),
    RTC_PATH: _Route(POST, BOTH, "_rtc"),
    APPS_PATH: _Route(GET, BOTH, "_apps"),
    STATUS_PATH: _Route(GET, BOTH, "_status"),
    LOGOUT_PATH: _Route(POST, BOTH, "_logout"),
    CA_PATH: _Route(GET_HEAD, LAN, "_ca", cookie=False),
    MOBILECONFIG_PATH: _Route(GET_HEAD, LAN, "_mobileconfig", cookie=False),
}
ASSET_ROUTE = _Route(GET_HEAD, BOTH, "_asset", cookie=False)
CONNECT_ROUTE = _Route(POST, BOTH, "_connect")
# LinkChat's paths this door passes on; any other /api/web/link/ path is a 404 here
LINK_ROUTES: dict[str, _Route] = {
    "turn": _Route(POST, BOTH, "_link"),
    "messages": _Route(GET, BOTH, "_link"),
    "upload": _Route(POST, BOTH, "_link", body="bytes"),
}
LINK_FILE_ROUTE = _Route(GET, BOTH, "_link")


def route_for(path: str) -> tuple[Optional[_Route], str]:
    """The route a path names, and its argument (an asset's name, LinkChat's rest of the path, a toolkit)."""
    route = ROUTES.get(path)
    if route is not None:
        return route, ""
    if path.startswith(LINK_PREFIX):
        rest = path[len(LINK_PREFIX):]
        if rest in LINK_ROUTES:
            return LINK_ROUTES[rest], rest
        return (LINK_FILE_ROUTE, rest) if LINK_FILE_RE.fullmatch(rest) else (None, "")
    asset = ASSET_RE.match(path)
    if asset is not None:
        return ASSET_ROUTE, asset.group(1)
    toolkit = APP_CONNECT_RE.match(path)
    if toolkit is not None:
        return CONNECT_ROUTE, toolkit.group(1)
    return None, ""


# ---- a door ------------------------------------------------------------------------------------------------------

class _Door:
    """What both listeners do: bind, take a connection, read one request, check it (module docstring), route it,
    answer with ``Connection: close``. The routes both doors have are here; each door adds its own."""

    where = ""
    scheme = ""
    cookie = ""
    signed_out = ""

    def __init__(self, ctx: WebContext) -> None:
        self.ctx = ctx
        self.shared = ctx.shared
        self.port = 0
        self.refused = 0                               # connections past a cap, closed at once
        self._servers: list[asyncio.Server] = []
        self._conns: set[asyncio.Task[Any]] = set()
        self._per_peer: dict[str, int] = {}            # open connections by the peer's address

    @property
    def running(self) -> bool:
        return bool(self._servers)

    @property
    def bound(self) -> list[tuple[str, int]]:
        """Where this door listens, as (address, port): the sockets themselves, for a status line or a test."""
        out = []
        for server in self._servers:
            for sock in server.sockets:
                name = sock.getsockname()
                out.append((str(name[0]), int(name[1])))
        return out

    def _hosts(self) -> frozenset[str]:
        raise NotImplementedError

    # -- the listening sockets --
    @staticmethod
    def _socket(host: str, port: int) -> socket.socket:
        family = socket.AF_INET6 if ":" in host else socket.AF_INET
        sock = socket.socket(family, socket.SOCK_STREAM)
        try:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            if family == socket.AF_INET6:
                sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)   # "::" is IPv6 only; 0.0.0.0 is beside it
            sock.bind((host, port))
            sock.listen(128)
            sock.setblocking(False)
        except BaseException:
            sock.close()
            raise
        return sock

    async def _bind(self, hosts: Sequence[str], ports: Sequence[int]) -> tuple[list[asyncio.Server], int]:
        """Listen on every host at the first port all of them can have. A host after the first that this Mac
        cannot have at all (no IPv6) is left out; a port taken on any host moves on to the next port. PortsTaken when
        no port is free."""
        last: Optional[OSError] = None
        for want in ports:
            servers: list[asyncio.Server] = []
            port = want
            try:
                for i, host in enumerate(hosts):
                    try:
                        sock = self._socket(host, port)
                    except OSError as e:
                        if i and e.errno in (errno.EADDRNOTAVAIL, errno.EAFNOSUPPORT, errno.EPROTONOSUPPORT):
                            continue
                        raise
                    port = sock.getsockname()[1]
                    servers.append(await asyncio.start_server(self._connection, sock=sock))
                if servers:
                    return servers, port
            except OSError as e:
                last = e
                for server in servers:
                    server.close()
        raise PortsTaken(errno.EADDRINUSE, f"no free port among {len(ports)}") from last

    async def _close(self) -> None:
        """Stop listening, cut the open connections, and wait for them a little (CLOSE_SECS)."""
        servers, self._servers = self._servers, []
        for server in servers:
            server.close()
        conns = [task for task in self._conns if not task.done()]
        for task in conns:
            task.cancel()
        if conns:
            await asyncio.wait(conns, timeout=CLOSE_SECS)
        for server in servers:
            with contextlib.suppress(Exception):
                await asyncio.wait_for(server.wait_closed(), CLOSE_SECS)

    # -- one connection, one request --
    async def _connection(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        task = asyncio.current_task()
        peername = writer.get_extra_info("peername")
        peer = str(peername[0]) if isinstance(peername, (tuple, list)) and peername else ""
        if task is not None:
            self._conns.add(task)
        counted = False
        try:
            if len(self._conns) > MAX_CONNECTIONS or self._per_peer.get(peer, 0) >= MAX_PER_PEER:
                self.refused += 1
                if self.refused == 1:
                    log.warning("lite web: too many connections at once (%d in all, %d from one address); new ones "
                                "are closed", MAX_CONNECTIONS, MAX_PER_PEER)
                return
            self._per_peer[peer] = self._per_peer.get(peer, 0) + 1
            counted = True
            # LanServer: the peer filter and the TLS handshake. Nothing is awaited before start_tls in there.
            if not await self._admit(writer, peername):
                return
            await self._serve_one(reader, writer, peername)
        except (ConnectionError, ssl.SSLError, asyncio.IncompleteReadError, TimeoutError, OSError):
            pass                                        # the peer went away, or never spoke TLS: nothing to say
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("lite web: a request failed (%s)", type(e).__name__)
        finally:
            if task is not None:
                self._conns.discard(task)
            if counted:
                left = self._per_peer.get(peer, 1) - 1
                if left > 0:
                    self._per_peer[peer] = left
                else:
                    self._per_peer.pop(peer, None)
            await self._shut(writer)

    @staticmethod
    async def _shut(writer: asyncio.StreamWriter) -> None:
        with contextlib.suppress(Exception):
            writer.close()
            await asyncio.wait_for(writer.wait_closed(), CLOSE_SECS)

    async def _admit(self, writer: asyncio.StreamWriter, peername: Any) -> bool:
        return True

    async def _serve_one(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter, peername: Any) -> None:
        try:
            method, path, query, headers = await _read_head(reader)
        except _Refused as e:
            await self._send(writer, e.status, json.dumps({"ok": False, "error": e.line}).encode())
            return
        except asyncio.LimitOverrunError:
            await self._send(writer, 431, json.dumps({"ok": False, "error": "the head is too long"}).encode())
            return
        req = _Request(method, path, query, headers, reader, writer, peername)
        try:
            await self._handle(req)
        except (ConnectionError, OSError, asyncio.IncompleteReadError):
            raise
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("lite web: a request failed (%s)", type(e).__name__)
            if not req.responded:
                await self._error(req, 500, FAILED)

    async def _handle(self, req: _Request) -> None:
        host = req.headers.get("host", "").strip().lower()
        if host not in self._hosts():
            await self._error(req, 421, WRONG_HOST)
            return
        req.host = host
        route, arg = route_for(req.path)
        if route is None or self.where not in route.doors:
            await self._error(req, 404, NOT_FOUND)
            return
        if req.method not in route.methods:
            await self._error(req, 405, WRONG_METHOD, [("Allow", ", ".join(sorted(route.methods)))])
            return
        if req.method not in GET_HEAD:
            if req.headers.get("origin", "").strip().lower() != f"{self.scheme}://{host}":
                await self._error(req, 403, WRONG_ORIGIN)
                return
            if "transfer-encoding" in req.headers:
                await self._error(req, 411, NO_CHUNKED)
                return
            want = "application/octet-stream" if route.body == "bytes" else JSON_TYPE
            if _media(req.headers) != want:
                await self._error(req, 415, WRONG_TYPE.format(want=want))
                return
        if route.cookie and not await self._signed_in(req):
            return
        await getattr(self, route.handler)(req, arg)

    async def _signed_in(self, req: _Request) -> bool:
        """The request's device, from this door's cookie; False after answering 401 (and clearing a cookie the
        devices no longer know)."""
        token = web_auth.cookie_value(req.headers.get("cookie"), self.cookie)
        device = await asyncio.to_thread(self.ctx.devices.check, token, self.where) if token else None
        if device is None:
            await self._error(req, 401, self.signed_out,
                              [("Set-Cookie", web_auth.clear_cookie(self.cookie))] if token else [])
            return False
        req.device, req.token = device, token
        return True

    # -- answering --
    @staticmethod
    async def _send(writer: Any, status: int, body: bytes = b"", ctype: str = JSON_TYPE, *,
                    headers: Iterable[tuple[str, str]] = (), cache: str = NO_STORE, head_only: bool = False) -> None:
        writer.write(_head_bytes(status, ctype, len(body), cache, headers) + (b"" if head_only else body))
        await writer.drain()

    async def _json(self, req: _Request, status: int, obj: Mapping[str, Any],
                    headers: Iterable[tuple[str, str]] = ()) -> None:
        req.responded = True
        await self._send(req.writer, status, json.dumps(obj).encode(), headers=headers,
                         head_only=req.method == "HEAD")

    async def _error(self, req: _Request, status: int, line: str, headers: Iterable[tuple[str, str]] = ()) -> None:
        await self._json(req, status, {"ok": False, "error": line}, headers)

    async def _page(self, req: _Request, status: int, title: str, line: str) -> None:
        req.responded = True
        await self._send(req.writer, status, _page_html(title, line), HTML_TYPE, head_only=req.method == "HEAD")

    async def _redirect(self, req: _Request, status: int, location: str) -> None:
        req.responded = True
        await self._send(req.writer, status, b"", "text/plain; charset=utf-8", headers=[("Location", location)])

    async def _body(self, req: _Request) -> Optional[dict[str, Any]]:
        """The request's JSON object ({} for no body); None once a refusal is answered."""
        raw = req.headers.get("content-length", "").strip() or "0"
        if not (raw.isascii() and raw.isdigit()):           # "²".isdigit() is True too, and int() of it raises
            await self._error(req, 400, BAD_BODY)
            return None
        size = int(raw)
        if size > MAX_JSON_BYTES:
            await self._error(req, 413, TOO_LARGE)
            return None
        if size == 0:
            return {}
        try:
            data = await asyncio.wait_for(req.reader.readexactly(size), BODY_SECS)
        except (asyncio.IncompleteReadError, TimeoutError):
            await self._error(req, 400, BAD_BODY)
            return None
        try:
            body = json.loads(data)
        except (ValueError, RecursionError):
            body = None
        if not isinstance(body, dict):
            await self._error(req, 400, BAD_BODY)
            return None
        return body

    def _cookie_again(self, req: _Request) -> tuple[str, str]:
        """The device's cookie set again, so it lasts MAX_AGE from this visit, not from the pairing."""
        return "Set-Cookie", web_auth.set_cookie(self.cookie, req.token)

    def _spawn(self, coro: Awaitable[Any]) -> None:
        task = asyncio.ensure_future(coro)
        self.shared.tasks.add(task)
        task.add_done_callback(self.shared.tasks.discard)

    def _warn_once(self, key: str, msg: str, *args: Any) -> None:
        if key not in self.shared.warned:
            self.shared.warned.add(key)
            log.warning(msg, *args)

    # -- the daemon, the key, the apps --
    async def _daemon(self) -> _Daemon:
        """The daemon's state, read field by field (WebContext.status): nothing else in its mapping reaches a page."""
        raw: Any = {}
        try:
            raw = self.ctx.status() if self.ctx.status is not None else {}
            if inspect.isawaitable(raw):
                raw = await raw
        except Exception as e:  # noqa: BLE001 — the type only
            self._warn_once("status", "lite web: the daemon's status could not be read (%s)", type(e).__name__)
            raw = {}
        if not isinstance(raw, Mapping):
            raw = {}
        keyed = bool(str(self.ctx.environ.get(openrouter.KEY_ENV) or "").strip()) or raw.get("openrouter") is True
        state = raw.get("state")
        if not (isinstance(state, str) and _STATE.fullmatch(state)):
            state = READY if keyed else NEEDS_OPENROUTER
        voice = raw.get("voice")
        if not isinstance(voice, bool):
            try:
                voice = bool(self.ctx.rtc is not None and self.ctx.rtc.voice_ready())
            except Exception:  # noqa: BLE001 — a hub that cannot say has no voice
                voice = False
        return _Daemon(state, keyed, voice)

    async def _credit(self) -> Optional[dict[str, Any]]:
        """What the saved key can spend (onboarding.credit, through _credit_view), read at most every
        CREDIT_TTL_SECS and shared by every page asking at once. A page waits CREDIT_WAIT_SECS at most: past that it
        gets the last figure known (or null), and the read lands for the next page. None without a key."""
        key = str(self.ctx.environ.get(openrouter.KEY_ENV) or "").strip()
        if not key:
            return None
        shared = self.shared
        digest = hashlib.sha256(key.encode()).hexdigest()
        kept = shared.credit if shared.credit is not None and shared.credit[0] == digest else None
        if kept is not None and time.monotonic() - kept[1] < CREDIT_TTL_SECS:
            return kept[2]
        task = shared.credit_task
        if task is None or task.done() or shared.credit_for != digest:
            task = asyncio.ensure_future(self._read_credit(key, digest))
            shared.credit_task, shared.credit_for = task, digest
        try:
            return await asyncio.wait_for(asyncio.shield(task), CREDIT_WAIT_SECS)
        except TimeoutError:
            return kept[2] if kept is not None else None

    async def _read_credit(self, key: str, digest: str) -> dict[str, Any]:
        try:
            raw = await self.ctx.credit(key)
        except Exception as e:  # noqa: BLE001 — onboarding.credit never raises; a stand-in might
            raw = {"ok": False, "usd": None, "funded": None, "error": type(e).__name__}
        view = _credit_view(raw) or {"ok": False, "usd": None, "funded": None}
        self.shared.credit = (digest, time.monotonic(), view)
        return view

    def _composio_signed_in(self) -> bool:
        """composio-connect.json exists: onboarding writes it only once a token arrived."""
        path = getattr(self.ctx.composio_login, "token_path", None)
        try:
            return path is not None and Path(path).is_file()
        except OSError:
            return False

    def _composio_live(self) -> bool:
        """Signed in, and not one the bridge says only a new sign-in helps (composio_connect.needs_sign_in). No
        bridge yet (eve restarting into it) counts as live."""
        if not self._composio_signed_in():
            return False
        bridge = self.ctx.apps
        try:
            return bridge is None or not bool(getattr(bridge, "needs_sign_in", False))
        except Exception:  # noqa: BLE001 — a bridge that cannot say is taken as signed in
            return True

    def lan_on(self) -> bool:
        lan = self.shared.lan
        return lan is not None and lan.running

    async def status(self) -> dict[str, Any]:
        """``GET /api/web/status``: booleans, words and the credit figure. Built field by field, so no key, token or
        code can reach it."""
        daemon = await self._daemon()
        credit = await self._credit() if daemon.openrouter and not self.shared.restarting else None
        return {"state": daemon.state, "openrouter": daemon.openrouter, "openrouter_rejected": _key_rejected(credit),
                "composio": self._composio_live(), "voice": daemon.voice, "lan": self.lan_on(),
                "local_network": self.shared.local_network, "restarting": self.shared.restarting, "credit": credit}

    async def _close_peers(self, device_ids: Iterable[str]) -> None:
        """Those devices' WebRTC peers close now, with their requests and calls."""
        rtc = self.ctx.rtc
        for device_id in list(device_ids):
            self.shared.rtc_doors.pop(device_id, None)
            if rtc is None:
                continue
            try:
                await rtc.close_device(device_id)
            except Exception as e:  # noqa: BLE001 — the type only; the revoke itself stands
                log.warning("lite web: a revoked device's link did not close (%s)", type(e).__name__)

    async def _revoke_ids(self, id_or_all: str) -> list[str]:
        gone = await asyncio.to_thread(self.ctx.devices.revoke, id_or_all)
        await self._close_peers(gone)
        return gone

    def _probe_soon(self, peername: Any) -> None:
        """Ask in the background whether macOS lets eve reach the device (lan_net.probe), for the status line."""
        async def run() -> None:
            try:
                word = await asyncio.to_thread(self.ctx.probe, peername)
            except Exception:  # noqa: BLE001 — not known
                word = "unknown"
            if word in ("ok", "blocked"):
                self.shared.local_network = word

        self._spawn(run())

    # -- sign in: a browser gets its cookie --
    async def _sign_in(self, req: _Request) -> None:
        """Answer {ok} with this door's cookie: the browser's own device when its cookie is still good (opening Eve
        again, scanning again), else a new one."""
        token = web_auth.cookie_value(req.headers.get("cookie"), self.cookie)
        device = await asyncio.to_thread(self.ctx.devices.check, token, self.where) if token else None
        if device is None:
            try:
                _, token = await asyncio.to_thread(self.ctx.devices.add,
                                                   device_label(req.headers.get("user-agent"), self.where),
                                                   _host_name(req.host), door=self.where)
            except OSError as e:
                log.warning("lite web: a device could not be saved (%s)", type(e).__name__)
                await self._error(req, 500, DEVICE_NOT_SAVED)
                return
            log.info("lite web: %s signed in", "the Mac's browser" if self.where == "local" else "a device paired")
        await self._json(req, 200, {"ok": True}, [("Set-Cookie", web_auth.set_cookie(self.cookie, token))])

    # -- the routes both doors have --
    async def _index(self, req: _Request, _arg: str) -> None:
        await self._static(req, "index.html")

    async def _asset(self, req: _Request, name: str) -> None:
        await self._static(req, f"assets/{name}")

    async def _static(self, req: _Request, rel: str) -> None:
        root = self.ctx.web_root()
        ctype = _content_type(rel)
        squeezable = ctype.startswith(GZIP_TYPES)
        found = await asyncio.to_thread(self.shared.files.read, root / rel, root,
                                        squeezable and _accepts_gzip(req.headers.get("accept-encoding", "")))
        page = rel == "index.html"
        if found is None:
            if page:
                self._warn_once("not-built", "lite web: the web page is not installed (no index.html where it is "
                                "looked for)")
                await self._page(req, 503, "eve", NOT_BUILT)
            else:
                await self._error(req, 404, NOT_FOUND)
            return
        body, zipped = found
        headers = [("Vary", "Accept-Encoding")] if squeezable else []
        if zipped:
            headers.append(("Content-Encoding", "gzip"))
        req.responded = True
        await self._send(req.writer, 200, body, ctype, headers=headers, cache=NO_STORE if page else IMMUTABLE,
                         head_only=req.method == "HEAD")

    async def _health(self, req: _Request, _arg: str) -> None:
        daemon = await self._daemon()
        await self._json(req, 200, {"ok": True, "state": daemon.state})

    async def _rtc(self, req: _Request, _arg: str) -> None:
        body = await self._body(req)
        if body is None:
            return
        rtc = self.ctx.rtc
        if rtc is None:
            await self._error(req, 503, RTC_OFF)
            return
        assert req.device is not None
        try:
            answer = await asyncio.wait_for(rtc.answer(req.device.id, body.get("sdp"), req.peer), RTC_SECS)
        except TimeoutError:
            await self._error(req, 504, RTC_SLOW)
            return
        except ValueError as e:                         # lite_rtc.RtcError: an offer this Mac will not answer
            await self._error(req, 400, _plain(e, OFFER_REFUSED))
            return
        sdp = answer.get("sdp") if isinstance(answer, Mapping) else None
        if not isinstance(sdp, str):
            await self._error(req, 500, OFFER_REFUSED)
            return
        self.shared.rtc_doors[req.device.id] = self.where
        if self.where == "lan":
            self._probe_soon(req.peername)
        await self._json(req, 200, {"sdp": sdp, "voice": answer.get("voice") is True})

    async def _link(self, req: _Request, rest: str) -> None:
        """LinkChat, unchanged, on the real stream: Origin and the cookie removed, the nonce as its Bearer token."""
        link = self.ctx.link
        if link is None:
            await self._error(req, 503, NOT_READY)
            return
        headers = {name: value for name, value in req.headers.items() if name not in LINK_DROPPED}
        headers["authorization"] = f"Bearer {self.shared.nonce}"
        writer = _AddedHeaders(req.writer, LINK_HEADERS)
        req.responded = True
        try:
            await link.serve(req.method, "/api/link/" + rest, req.query, headers, req.reader, writer,
                             token_check=self._nonce_ok)
        finally:
            writer.flush()

    def _nonce_ok(self, token: str) -> bool:
        return hmac.compare_digest(str(token).encode("utf-8"), self.shared.nonce.encode("utf-8"))

    async def _apps(self, req: _Request, _arg: str) -> None:
        """Settings → Apps: {composio, apps: [{toolkit, name, connected}]} in composio_connect.APPS's order."""
        from .composio_connect import APPS

        bridge = self.ctx.apps
        if not self._composio_live():
            await self._json(req, 200, {"composio": False, "apps": []})
            return
        connected: Mapping[str, Any] = {}
        if bridge is not None:
            try:
                connected = await asyncio.wait_for(asyncio.to_thread(bridge.connections), APPS_SECS)
            except TimeoutError:
                await self._error(req, 504, COMPOSIO_SLOW)
                return
            if not isinstance(connected, Mapping):
                connected = {}
        await self._json(req, 200, {"composio": True, "apps": [
            {"toolkit": slug, "name": name, "connected": connected.get(slug) is True} for slug, name in APPS.items()]})

    async def _connect(self, req: _Request, toolkit: str) -> None:
        """The Connect Link for one app: {ok, url}, ``https`` on composio.dev only (composio_connect.link_ok)."""
        from .composio_connect import APPS, link_ok, toolkit_slug
        from .composio_tools import WEB_TOOLKITS

        if await self._body(req) is None:
            return
        slug = toolkit_slug(toolkit)
        if slug is None or slug in WEB_TOOLKITS:
            await self._error(req, 404, NOT_AN_APP)
            return
        bridge = self.ctx.apps
        if not self._composio_live():
            await self._error(req, 409, COMPOSIO_OUT)
            return
        if bridge is None:                              # signed in, and eve restarting into the bridge
            await self._error(req, 503, COMPOSIO_DOWN)
            return
        name = APPS.get(slug, slug)
        try:
            url = await asyncio.wait_for(asyncio.to_thread(bridge.connect_link, slug), APPS_SECS)
        except TimeoutError:
            await self._error(req, 504, COMPOSIO_SLOW)
            return
        if isinstance(url, str) and link_ok(url):
            log.info("lite web: a Connect Link went to a page")
            await self._json(req, 200, {"ok": True, "url": url})
            return
        try:      # no link: most often the app is connected already (connect_link remembered it)
            known = await asyncio.wait_for(asyncio.to_thread(bridge.connections, [slug]), CONNECTED_SECS)
        except Exception:  # noqa: BLE001 — TimeoutError or the bridge's own: not known
            known = {}
        if isinstance(known, Mapping) and known.get(slug) is True:
            await self._json(req, 409, {"ok": False, "error": ALREADY.format(name=name), "connected": True})
            return
        await self._error(req, 502, NO_LINK.format(name=name))

    async def _status(self, req: _Request, _arg: str) -> None:
        await self._json(req, 200, await self.status(), [self._cookie_again(req)])

    async def _logout(self, req: _Request, _arg: str) -> None:
        """This browser signs out: its device is revoked (its peer closes) and its cookie cleared."""
        if await self._body(req) is None:
            return
        assert req.device is not None
        try:
            await self._revoke_ids(req.device.id)
        except OSError as e:
            log.warning("lite web: a sign-out was not saved (%s)", type(e).__name__)
            await self._error(req, 500, NOT_SAVED)
            return
        await self._json(req, 200, {"ok": True}, [("Set-Cookie", web_auth.clear_cookie(self.cookie))])


# ---- the Mac's own page -----------------------------------------------------------------------------------------

class LocalServer(_Door):
    """``http://localhost:<port>``, on loopback only (module docstring). The daemon starts it at boot and stops it
    last; it brings the LAN door back when web.json says it was on, and owns turning it on and off. Besides the
    routes, the daemon's IPC uses ``port``, ``set_lan``, ``revoke`` and ``status``."""

    where, scheme, cookie, signed_out = "local", "http", web_auth.COOKIE_MAC, SIGNED_OUT_MAC

    def __init__(self, ctx: WebContext) -> None:
        super().__init__(ctx)
        self._allowed: frozenset[str] = frozenset()

    @property
    def url(self) -> str:
        return f"http://localhost:{self.port}"

    @property
    def lan(self) -> Optional["LanServer"]:
        return self.shared.lan

    def _hosts(self) -> frozenset[str]:
        return self._allowed

    async def start(self, *, resume_lan: bool = True) -> int:
        """Listen, on the port web.json names when it is free, else the first free of ``local_ports``; the LAN door
        opens too when it was on (a LAN door that cannot is logged, and left off). The port. PortsTaken when none is
        free; RuntimeError outside eve lite."""
        _require_lite(self.ctx)
        if self.running:
            return self.port
        saved = self.shared.saved
        self._servers, self.port = await self._bind(self.ctx.local_hosts, _ports(self.ctx.local_ports, saved.local_port))
        self._allowed = frozenset(f"{name}:{self.port}" for name in LOOPBACK_NAMES)
        if saved.local_port != self.port:
            await asyncio.to_thread(saved.save, local_port=self.port)
        log.info("lite web: the Mac's page is up on port %d", self.port)
        if resume_lan and saved.lan:
            try:
                await self.set_lan(True)
            except (OSError, ValueError) as e:
                log.warning("lite web: the LAN door did not open again (%s)", type(e).__name__)
        return self.port

    async def stop(self) -> None:
        """LinkChat's open streams end with their [DONE] first (as miniapp.MiniAppServer closes), then the LAN door
        closes, then this one. The RtcHub is the daemon's to close."""
        link = self.ctx.link
        if link is not None:
            with contextlib.suppress(Exception):
                link.close()
                await link.settle()
        lan = self.shared.lan
        if lan is not None:
            await lan.stop()
        me = asyncio.current_task()
        for task in list(self.shared.tasks):
            if task is not me:              # a restart that stops eve through here goes on
                task.cancel()
        await self._close()
        log.info("lite web: the Mac's page is down")

    async def set_lan(self, on: bool) -> Optional["LanServer"]:
        """Turn the LAN door on (start it if it is not running) or off, and remember it in web.json. Off also closes
        a pairing window and the WebRTC peers of the devices that came through it. The LanServer when on. PortsTaken
        when no port is free; another OSError, or ValueError, when the certificate could not be made or loaded."""
        async with self.shared.lan_lock:
            saved = self.shared.saved
            lan = self.shared.lan
            if on:
                if lan is None:
                    lan = self.shared.lan = LanServer(self.ctx)
                if not lan.running:
                    await lan.start()
                if not saved.lan:
                    await asyncio.to_thread(saved.save, lan=True)
                return lan
            if lan is not None and lan.running:
                await lan.stop()
            self.ctx.pairing.close()
            if saved.lan:
                await asyncio.to_thread(saved.save, lan=False)
            await self._close_peers([dev for dev, door in self.shared.rtc_doors.items() if door == "lan"])
            return None

    async def revoke(self, id_or_all: str) -> list[str]:
        """Forget one device by its id, or every device with "all", and close their WebRTC peers: the ids revoked.
        OSError when the list could not be saved."""
        return await self._revoke_ids(id_or_all)

    # -- the routes only the Mac's page has --
    async def _mac_login(self, req: _Request, _arg: str) -> None:
        """``eve-bridge web open``'s one-time code, from the page's #login= fragment: eve_mac for this browser."""
        body = await self._body(req)
        if body is None:
            return
        if not self.ctx.mac_login.consume(body.get("code")):
            await self._error(req, 403, LOGIN_SPENT)
            return
        await self._sign_in(req)

    async def _onboard_state(self, req: _Request, _arg: str) -> None:
        """The setup page's state: booleans, the credit figure and one plain line."""
        daemon = await self._daemon()
        restarting = self.shared.restarting
        credit = await self._credit() if daemon.openrouter and not restarting else None
        await self._json(req, 200, {
            "openrouter": daemon.openrouter, "openrouter_rejected": _key_rejected(credit), "credit": credit,
            "composio": self._composio_signed_in(), "composio_skipped": self.shared.saved.composio_skipped,
            "restarting": restarting, "error": _plain(self.shared.error, SIGNIN_BROKE) if self.shared.error else ""},
            [self._cookie_again(req)])

    async def _openrouter_start(self, req: _Request, _arg: str) -> None:
        login = self.ctx.openrouter
        if login is None:
            await self._page(req, 503, "eve", NO_SIGNIN)
            return
        try:
            url = login.start(f"http://{req.host}{OPENROUTER_CALLBACK_PATH}")
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("lite web: the OpenRouter sign-in did not start (%s)", type(e).__name__)
            await self._page(req, 500, "eve", SIGNIN_BROKE)
            return
        self.shared.error = ""
        await self._redirect(req, 302, url)

    async def _composio_start(self, req: _Request, _arg: str) -> None:
        login = self.ctx.composio_login
        if login is None:
            await self._page(req, 503, "eve", NO_SIGNIN)
            return
        try:
            url = await login.start(f"http://{req.host}{COMPOSIO_CALLBACK_PATH}")
        except onboarding.OnboardingError as e:
            await self._signin_failed(req, "Composio", e.status, str(e))
            return
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("lite web: the Composio sign-in did not start (%s)", type(e).__name__)
            await self._signin_failed(req, "Composio", 500, SIGNIN_BROKE)
            return
        self.shared.error = ""
        await self._redirect(req, 302, url)

    async def _openrouter_callback(self, req: _Request, _arg: str) -> None:
        """OpenRouter's redirect back, checked by its single-use state alone: the key is exchanged and saved."""
        login = self.ctx.openrouter
        if login is None:
            await self._page(req, 503, "eve", NO_SIGNIN)
            return
        q = _query(req.query)
        try:
            key = await login.finish(q.get("code", ""), q.get("state", ""), error=q.get("error", ""))
            await asyncio.to_thread(onboarding.save_openrouter_key, key, path=self.ctx.env_path,
                                    environ=self.ctx.environ)
        except onboarding.OnboardingError as e:
            await self._signin_failed(req, "OpenRouter", e.status, str(e))
            return
        except Exception as e:  # noqa: BLE001 — the type only: never the key
            log.warning("lite web: the OpenRouter sign-in failed (%s)", type(e).__name__)
            await self._signin_failed(req, "OpenRouter", 500, SIGNIN_BROKE)
            return
        self.shared.credit = None
        await self._signed_up(req, "OpenRouter")

    async def _composio_callback(self, req: _Request, _arg: str) -> None:
        """Composio's redirect back, checked by its single-use state alone: the token is exchanged and saved."""
        login = self.ctx.composio_login
        if login is None:
            await self._page(req, 503, "eve", NO_SIGNIN)
            return
        q = _query(req.query)
        try:
            await login.finish(q.get("code", ""), q.get("state", ""), error=q.get("error", ""))
        except onboarding.OnboardingError as e:
            await self._signin_failed(req, "Composio", e.status, str(e))
            return
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("lite web: the Composio sign-in failed (%s)", type(e).__name__)
            await self._signin_failed(req, "Composio", 500, SIGNIN_BROKE)
            return
        await self._signed_up(req, "Composio")

    async def _signed_up(self, req: _Request, provider: str) -> None:
        """A credential is saved: back to the setup page (the code leaves the address bar), then a restart into it."""
        self.shared.error = ""
        log.info("lite web: %s signed in; eve restarts", provider)
        await self._redirect(req, 303, "/")
        self._restart_soon()

    async def _signin_failed(self, req: _Request, provider: str, status: int, line: str) -> None:
        """The provider's status, a short page with the line, and the same line on the setup page."""
        text = _plain(line, SIGNIN_BROKE)
        self.shared.error = text
        log.info("lite web: the %s sign-in did not finish (HTTP %d)", provider, status)
        await self._page(req, status if 400 <= status < 600 else 500, "Sign-in did not finish", text)

    def _restart_soon(self) -> None:
        """Ask the daemon to restart in ``restart_delay`` seconds, once: the page has loaded by then and shows that
        eve is restarting (``restarting``) until the new one answers."""
        if self.shared.restarting:
            return
        self.shared.restarting = True

        async def go() -> None:
            await asyncio.sleep(max(float(self.ctx.restart_delay), 0.0))
            try:
                done = self.ctx.restart() if self.ctx.restart is not None else None
                if inspect.isawaitable(done):
                    await done
            except Exception as e:  # noqa: BLE001 — the type only
                log.warning("lite web: eve did not restart (%s); the sign-in counts from the next start",
                            type(e).__name__)
                self.shared.restarting = False

        self._spawn(go())

    async def _composio_skip(self, req: _Request, _arg: str) -> None:
        if await self._body(req) is None:
            return
        if not await asyncio.to_thread(self.shared.saved.save, composio_skipped=True):
            await self._error(req, 500, NOT_SAVED)
            return
        await self._json(req, 200, {"ok": True})

    async def _pair_open(self, req: _Request, _arg: str) -> None:
        """Add a phone: the LAN door on, the Local Network alert, a fresh window, its links as QR codes."""
        if await self._body(req) is None:
            return
        was_on = self.lan_on()
        try:
            lan = await self.set_lan(True)
            assert lan is not None
            # a door that was already open names the Mac as it is now: a new address reissues the certificate
            hostname, addresses = await lan.refresh_names() if was_on else (lan.hostname, lan.lan_addresses)
        except PortsTaken as e:
            log.warning("lite web: the LAN door did not open (%s)", type(e).__name__)
            await self._error(req, 503, LAN_FAILED)
            return
        except (OSError, ValueError) as e:              # the certificate could not be made or loaded
            log.warning("lite web: the LAN certificate is not usable (%s)", type(e).__name__)
            await self._error(req, 503, TLS_OFF)
            return
        await asyncio.to_thread(_quietly, self.ctx.alert)
        secret, code = self.ctx.pairing.open()
        links: list[dict[str, str]] = []
        if hostname:
            links.append({"kind": "apple", "label": "iPhone or iPad",
                          "url": f"https://{hostname.lower()}.local:{lan.port}/#p={secret}"})
        if addresses:
            links.append({"kind": "other", "label": "Android or other",
                          "url": f"https://{addresses[0]}:{lan.port}/#p={secret}"})
        if not links:
            self.ctx.pairing.close()
            await self._error(req, 503, NO_ADDRESS)
            return
        codes = await asyncio.to_thread(lambda: [qr_svg(link["url"]) for link in links])
        payload: dict[str, Any] = {"ok": True, "code": code}
        for link, qr in zip(links, codes, strict=True):
            link["qr"] = qr
            payload[f"url_{link['kind']}"], payload[f"qr_{link['kind']}"] = link["url"], qr
        left = self.ctx.pairing.remaining
        payload.update(expires=int(time.time() + left), expires_in=int(left),
                       links=[{k: v for k, v in link.items() if k != "kind"} for link in links])
        log.info("lite web: a pairing window opened (%d links)", len(links))
        await self._json(req, 200, payload)

    async def _devices(self, req: _Request, _arg: str) -> None:
        assert req.device is not None
        rows = await asyncio.to_thread(self.ctx.devices.list)
        me = req.device.id
        await self._json(req, 200, {"devices": [{**row, "current": row["id"] == me} for row in rows]})

    async def _devices_revoke(self, req: _Request, _arg: str) -> None:
        body = await self._body(req)
        if body is None:
            return
        target = body.get("id")
        if not isinstance(target, str) or not (target == "all" or web_auth.DEVICE_ID.fullmatch(target)):
            await self._error(req, 400, DEVICE_ID)
            return
        try:
            gone = await self._revoke_ids(target)
        except OSError as e:
            log.warning("lite web: a revoke was not saved (%s)", type(e).__name__)
            await self._error(req, 500, NOT_SAVED)
            return
        await self._json(req, 200, {"ok": True, "revoked": gone})

    async def _lan(self, req: _Request, _arg: str) -> None:
        body = await self._body(req)
        if body is None:
            return
        on = body.get("on")
        if not isinstance(on, bool):
            await self._error(req, 400, LAN_FLAG)
            return
        try:
            await self.set_lan(on)
        except PortsTaken as e:
            log.warning("lite web: the LAN door did not open (%s)", type(e).__name__)
            await self._error(req, 503, LAN_FAILED)
            return
        except (OSError, ValueError) as e:
            log.warning("lite web: the LAN certificate is not usable (%s)", type(e).__name__)
            await self._error(req, 503, TLS_OFF)
            return
        if self.shared.saved.lan != on:
            # the door is as asked for now, but web.json still says otherwise: the next start would undo it
            await self._error(req, 500, NOT_SAVED)
            return
        await self._json(req, 200, {"ok": True, "lan": self.lan_on()})


# ---- the LAN door ------------------------------------------------------------------------------------------------

class LanServer(_Door):
    """``https://<LocalHostName>.local:<port>`` on every interface (module docstring). LocalServer turns it on and
    off; started on its own (a test, the daemon), it still registers as the context's LAN door."""

    where, scheme, cookie, signed_out = "lan", "https", web_auth.COOKIE_DEV, SIGNED_OUT_DEV

    def __init__(self, ctx: WebContext) -> None:
        super().__init__(ctx)
        self.dropped = 0                               # connections from outside the home network, dropped unread
        self.hostname = ""
        self.lan_addresses: list[str] = []
        self._allowed: frozenset[str] = frozenset()
        self._allowed_at = 0.0

    async def start(self) -> int:
        """Make sure the certificate names this Mac, then listen on the port web.json names when it is free, else
        the first free of ``lan_ports``. The port. PortsTaken when none is free; another OSError, or ValueError, when
        the certificate could not be made or loaded."""
        _require_lite(self.ctx)
        if self.running:
            return self.port
        other = self.shared.lan
        if other is not None and other is not self and other.running:
            raise RuntimeError("this context's LAN door is open already (LocalServer.set_lan opens it)")
        await self.refresh_names()
        await asyncio.to_thread(self.ctx.tls.context)  # made now, not during the first phone's handshake
        saved = self.shared.saved
        self._servers, self.port = await self._bind(self.ctx.lan_hosts, _ports(self.ctx.lan_ports, saved.lan_port))
        self._allowed_at = 0.0
        self.shared.lan = self
        if saved.lan_port != self.port:
            await asyncio.to_thread(saved.save, lan_port=self.port)
        log.info("lite web: the LAN door is open on port %d", self.port)
        return self.port

    async def stop(self) -> None:
        if not self.running:
            return
        await self._close()
        log.info("lite web: the LAN door is closed")

    async def refresh_names(self) -> tuple[str, list[str]]:
        """This Mac's LocalHostName and private addresses now, and the certificate reissued when it lacks one (a new
        name or address; lite_tls.LocalTls.refresh). (hostname, addresses)."""
        def names() -> tuple[str, list[str]]:
            try:
                hostname = str(self.ctx.hostname() or "")
            except Exception:  # noqa: BLE001 — no name: the IP link alone
                hostname = ""
            try:
                addresses = [str(a) for a in self.ctx.addresses()]
            except Exception:  # noqa: BLE001 — no address: the .local link alone
                addresses = []
            return hostname, addresses

        hostname, addresses = await asyncio.to_thread(names)
        await asyncio.to_thread(self.ctx.tls.refresh, addresses, hostname)
        self.hostname, self.lan_addresses = hostname, addresses
        self._allowed_at = 0.0
        return hostname, addresses

    def _hosts(self) -> frozenset[str]:
        """The certificate's names and the Mac's current private addresses, each with this door's port; read again
        after NAMES_TTL_SECS."""
        now = time.monotonic()
        if self._allowed_at and now - self._allowed_at < NAMES_TTL_SECS:
            return self._allowed
        names: set[str] = set()
        try:
            names |= {str(n).lower() for n in self.ctx.tls.names()}
        except Exception as e:  # noqa: BLE001 — the type only; the addresses still count
            self._warn_once("names", "lite web: the certificate's names could not be read (%s)", type(e).__name__)
        try:
            names |= {str(a) for a in self.ctx.addresses()}
        except Exception:  # noqa: BLE001 — the certificate's names still count
            pass
        self._allowed = frozenset(f"{name}:{self.port}" for name in names if name and ":" not in name)
        self._allowed_at = now
        return self._allowed

    async def _admit(self, writer: asyncio.StreamWriter, peername: Any) -> bool:
        """Drop a peer from outside the home network before a byte is read, then TLS. Nothing may be awaited before
        start_tls: the peer's ClientHello would land in the plain stream and be lost to the handshake. (The handler's
        first step runs before the loop polls the socket, so the hello is still unread when start_tls pauses it.)"""
        try:
            allowed = bool(self.ctx.peer_allowed(peername))
        except Exception:  # noqa: BLE001 — a peer that cannot be judged is not let in
            allowed = False
        if not allowed:
            self.dropped += 1
            if self.dropped == 1:
                log.warning("lite web: the LAN door dropped a connection from outside this network (said once)")
            return False
        try:
            context = self.ctx.tls.context()
        except Exception as e:  # noqa: BLE001 — the type only
            self._warn_once("tls", "lite web: the LAN certificate could not be loaded (%s); no phone can connect",
                            type(e).__name__)
            return False
        await writer.start_tls(context, ssl_handshake_timeout=TLS_SECS)
        return True

    # -- the routes only the LAN door has --
    async def _pair(self, req: _Request, _arg: str) -> None:
        """A phone or laptop pairs with the window's secret (the QR code's #p=) or its 6-digit code: eve_dev."""
        body = await self._body(req)
        if body is None:
            return
        window = self.ctx.pairing
        left = window.remaining
        if left <= 0:
            await self._error(req, 403, NO_WINDOW)
            return
        if not window.pair(secret=body.get("secret"), code=body.get("code")):
            if window.is_open:
                await self._error(req, 403, WRONG_CODE)
            else:                                        # that try closed it, or it ran out meanwhile
                await self._error(req, 429 if left > 1 else 403, TOO_MANY if left > 1 else NO_WINDOW)
            return
        await self._sign_in(req)
        self._probe_soon(req.peername)

    async def _ca(self, req: _Request, _arg: str) -> None:
        await self._download(req, "ca_pem", "application/x-x509-ca-cert", "eve-lite-ca.crt")

    async def _mobileconfig(self, req: _Request, _arg: str) -> None:
        await self._download(req, "mobileconfig", "application/x-apple-aspen-config", "eve-lite.mobileconfig")

    async def _download(self, req: _Request, what: str, ctype: str, filename: str) -> None:
        """The CA certificate, alone or in an iPhone profile: lite_tls encodes both from the certificate, never a key."""
        try:
            data = await asyncio.to_thread(getattr(self.ctx.tls, what))
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("lite web: the CA download could not be made (%s)", type(e).__name__)
            await self._error(req, 503, TLS_OFF)
            return
        req.responded = True
        await self._send(req.writer, 200, bytes(data), ctype,
                         headers=[("Content-Disposition", f'attachment; filename="{filename}"')],
                         head_only=req.method == "HEAD")
