"""spotify.py: the token, the Web API calls spotKnob makes, the code path, the tools, the Mini App panel, the CLI."""

from __future__ import annotations

import asyncio
import json
import stat
import urllib.parse
from typing import Any, Optional

import pytest

from eve_bridge import spotify as S

TRACK = {"name": "One More Time", "uri": "spotify:track:1", "duration_ms": 320000,
         "artists": [{"name": "Daft Punk"}],
         "album": {"name": "Discovery", "images": [{"url": "https://i.scdn.co/image/abc"}]}}
MAC = {"id": "dev-mac", "name": "Studio MacBook", "type": "Computer", "is_active": True, "volume_percent": 40}
KITCHEN = {"id": "dev-kitchen", "name": "Kitchen Speaker", "type": "Speaker", "is_active": False, "volume_percent": 30}
TV = {"id": "dev-tv", "name": "Living Room TV", "type": "TV", "is_active": False, "is_restricted": True}


class FakeSpotify:
    """The Web API and the accounts service, enough of each to drive every path."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, str, dict[str, Any], Any]] = []
        self.devices = [MAC, KITCHEN, TV]
        self.playing = True
        self.item: Optional[dict[str, Any]] = TRACK
        self.volume = 40
        self.active = True
        self.premium = True
        self.expire_next = False          # the next API call answers 401 once
        self.refreshes = 0
        self.rotate = False
        self.search_hit = True

    def __call__(self, method: str, url: str, headers: dict[str, str], body: Optional[bytes]):
        u = urllib.parse.urlparse(url)
        q = {k: v[0] for k, v in urllib.parse.parse_qs(u.query).items()}
        if u.netloc == "accounts.spotify.com":
            form = dict(urllib.parse.parse_qsl((body or b"").decode()))
            self.calls.append((method, u.path, q, form))
            if form.get("refresh_token") == "dead":
                return 400, {}, json.dumps({"error": "invalid_grant", "error_description": "Refresh token revoked"}).encode()
            self.refreshes += 1
            out = {"access_token": f"at{self.refreshes}", "expires_in": 3600}
            if self.rotate:
                out["refresh_token"] = f"rt{self.refreshes}"
            return 200, {}, json.dumps(out).encode()
        data = json.loads(body) if body else None
        self.calls.append((method, u.path, q, data))
        if self.expire_next:
            self.expire_next = False
            return 401, {}, json.dumps({"error": {"status": 401, "message": "The access token expired"}}).encode()
        p = u.path.removeprefix("/v1")
        no_device = (404, {}, json.dumps({"error": {"status": 404, "message": "Player command failed: No active device found",
                                                    "reason": "NO_ACTIVE_DEVICE"}}).encode())
        if p == "/me/player" and method == "GET":
            if not self.active:
                return 204, {}, b""
            return 200, {}, json.dumps({"is_playing": self.playing, "progress_ms": 61000, "item": self.item,
                                        "device": {**MAC, "volume_percent": self.volume}}).encode()
        if p == "/me/player/devices":
            return 200, {}, json.dumps({"devices": self.devices}).encode()
        if not self.premium:
            return 403, {}, json.dumps({"error": {"status": 403, "message": "Premium required",
                                                  "reason": "PREMIUM_REQUIRED"}}).encode()
        if p == "/search":
            kind = q["type"]
            items = [{"name": "Daft Punk", "uri": "spotify:artist:dp"}] if kind == "artist" else [TRACK]
            return 200, {}, json.dumps({kind + "s": {"items": items if self.search_hit else []}}).encode()
        if p in ("/me/player/play", "/me/player/pause", "/me/player/next", "/me/player/previous", "/me/player/volume"):
            if not self.active and "device_id" not in q:
                return no_device
            if p.endswith("play"):
                self.playing, self.active = True, True
            elif p.endswith("pause"):
                self.playing = False
            elif p.endswith("volume"):
                self.volume = int(q["volume_percent"])
            return 204, {}, b""
        if p == "/me/player" and method == "PUT":
            self.active = True
            return 204, {}, b""
        return 404, {}, b"{}"

    def api(self, path: str) -> list[tuple[str, str, dict[str, Any], Any]]:
        return [c for c in self.calls if c[1] == "/v1" + path]


async def _no_sleep(_s: float) -> None:
    return None


def hub(fake: Optional[FakeSpotify] = None, wake: Any = None) -> tuple[S.Spotify, FakeSpotify]:
    fake = fake or FakeSpotify()
    tok = S.Token({"client_id": "cid", "refresh_token": "rt0"}, fake)
    return S.Spotify(S.SpotifyApi(tok, fake), wake=wake or (lambda: False), sleep=_no_sleep, wake_secs=3), fake


def run(coro: Any) -> Any:
    return asyncio.run(coro)


# ---- the token ----

def test_token_is_cached_then_refreshed_and_a_rotated_one_is_saved(tmp_path: Any) -> None:
    fake = FakeSpotify()
    fake.rotate = True
    path = tmp_path / "spotify.json"
    S.save({"client_id": "cid", "refresh_token": "rt0"}, path)
    now = {"t": 0.0}
    tok = S.Token(S.load(path), fake, save_to=path, clock=lambda: now["t"])
    assert tok.get() == "at1" and tok.get() == "at1"            # cached
    form = fake.calls[0][3]
    assert form == {"grant_type": "refresh_token", "refresh_token": "rt0", "client_id": "cid"}   # PKCE: no secret
    assert S.load(path)["refresh_token"] == "rt1"                # rotated, and kept at once
    now["t"] = 3600 - S.TOKEN_MARGIN_SECS + 1
    assert tok.get() == "at2"
    assert stat.S_IMODE(path.stat().st_mode) == 0o600


def test_a_client_secret_goes_in_basic_auth_not_the_body() -> None:
    seen: list = []

    def http(method: str, url: str, headers: dict, body: Any):
        seen.append((headers, dict(urllib.parse.parse_qsl(body.decode()))))
        return 200, {}, b'{"access_token": "x", "expires_in": 3600}'
    S.Token({"client_id": "cid", "client_secret": "sec", "refresh_token": "r"}, http).get()
    headers, form = seen[0]
    assert headers["Authorization"].startswith("Basic ") and "client_id" not in form and "sec" not in str(form)


def test_a_revoked_login_says_how_to_fix_it() -> None:
    fake = FakeSpotify()
    tok = S.Token({"client_id": "cid", "refresh_token": "dead"}, fake)
    h = S.Spotify(S.SpotifyApi(tok, fake), wake=lambda: False, sleep=_no_sleep)
    out = run(h.control("pause"))
    assert out["ok"] is False and "spotify login" in out["line"] and "Refresh token revoked" in out["line"]


def test_an_expired_access_token_is_refreshed_and_the_call_retried_once() -> None:
    h, fake = hub()
    fake.expire_next = True
    assert run(h.control("pause"))["ok"] is True
    assert len(fake.api("/me/player/pause")) == 2 and fake.refreshes == 2


def test_load_needs_an_id_and_a_token(tmp_path: Any) -> None:
    p = tmp_path / "s.json"
    assert S.load(p) == {}
    p.write_text('{"client_id": "x"}')
    assert S.load(p) == {}
    p.write_text("not json")
    assert S.load(p) == {}


def test_pkce_pair_and_authorize_url() -> None:
    import base64
    import hashlib
    v, c = S.pkce_pair()
    assert 43 <= len(v) <= 128
    assert c == base64.urlsafe_b64encode(hashlib.sha256(v.encode()).digest()).rstrip(b"=").decode()
    q = dict(urllib.parse.parse_qsl(urllib.parse.urlparse(S.authorize_url("cid", c, "st")).query))
    assert q["redirect_uri"] == "http://127.0.0.1:8888/callback" and q["code_challenge_method"] == "S256"
    assert set(q["scope"].split()) >= {"user-read-playback-state", "user-modify-playback-state"}


# ---- the knob's powers ----

def test_now_playing_is_trimmed_and_names_the_device() -> None:
    h, _ = hub()
    now = run(h.now_playing())
    assert now["ok"] and now["track"] == "One More Time" and now["artists"] == ["Daft Punk"]
    assert now["art"] == "https://i.scdn.co/image/abc" and now["device"]["volume"] == 40
    assert [d["name"] for d in now["devices"]] == ["Studio MacBook", "Kitchen Speaker", "Living Room TV"]
    assert now["devices"][2]["restricted"] is True and "id" not in now["devices"][0]
    assert S.said_now(now) == "Playing One More Time by Daft Punk on Studio MacBook."


def test_art_from_anywhere_else_is_dropped() -> None:
    item = {**TRACK, "album": {"name": "x", "images": [{"url": "http://evil.example/a.png"}]}}
    assert "art" not in S.now_of({"is_playing": True, "item": item})


def test_nothing_playing() -> None:
    h, fake = hub()
    fake.active = False
    now = run(h.now_playing())
    assert now["ok"] and now["track"] is None and S.said_now(now) == "Nothing is playing on Spotify."


def test_pause_play_next_previous() -> None:
    h, fake = hub()
    assert run(h.control("pause"))["line"] == "Paused." and fake.playing is False
    assert run(h.control("play"))["line"] == "Playing." and fake.playing is True
    assert run(h.control("next"))["line"] == "Skipped. Now One More Time by Daft Punk."
    assert run(h.control("previous"))["line"].startswith("Back one.")
    assert [c[0] for c in fake.api("/me/player/next")] == ["POST"]


def test_volume_absolute_relative_and_clamped() -> None:
    h, fake = hub()
    assert run(h.control("volume", volume=65))["line"] == "Spotify volume 65%."
    assert run(h.control("volume", volume_change=10))["volume"] == 75
    assert run(h.control("volume", volume_change=-100))["volume"] == 0
    assert run(h.control("volume", volume=250))["volume"] == 100 and fake.volume == 100
    assert run(h.control("volume"))["ok"] is False


def test_transfer_picks_a_device_by_name_word_or_kind() -> None:
    h, fake = hub()
    assert run(h.control("transfer", device="the kitchen speaker"))["line"] == "Playing on Kitchen Speaker."
    assert fake.api("/me/player")[-1][3] == {"device_ids": ["dev-kitchen"], "play": True}
    assert run(h.control("transfer", device="kitchen"))["ok"]
    assert run(h.control("transfer", device="mac"))["line"] == "Playing on Studio MacBook."
    tv = run(h.control("transfer", device="living room tv"))
    assert tv["ok"] is False and "remote control" in tv["line"]
    none = run(h.control("transfer", device="garage"))
    assert none["ok"] is False and "Kitchen Speaker" in none["line"] and "Living Room TV" not in none["line"]


def test_play_by_name_searches_then_plays_the_track_or_the_context() -> None:
    h, fake = hub()
    out = run(h.control("play", query="one more time"))
    assert out["line"] == "Playing One More Time by Daft Punk."
    assert fake.api("/me/player/play")[-1][3] == {"uris": ["spotify:track:1"]}
    out = run(h.control("play", query="daft punk", kind="artist", device="kitchen"))
    assert out["line"] == "Playing Daft Punk on Kitchen Speaker."
    last = fake.api("/me/player/play")[-1]
    assert last[3] == {"context_uri": "spotify:artist:dp"} and last[2] == {"device_id": "dev-kitchen"}
    fake.search_hit = False
    assert run(h.control("play", query="zzzz"))["line"] == "Spotify found nothing for 'zzzz'."


def test_play_with_nothing_active_starts_on_the_mac() -> None:
    h, fake = hub()
    fake.active = False
    assert run(h.control("play"))["ok"] is True
    plays = fake.api("/me/player/play")
    assert plays[0][2] == {} and plays[1][2] == {"device_id": "dev-mac"}


def test_play_with_no_device_online_opens_the_mac_app_and_waits() -> None:
    fake = FakeSpotify()
    fake.active, fake.devices = False, []
    woke: list = []

    def wake() -> bool:
        woke.append(1)
        fake.devices = [MAC]              # the app comes online
        return True
    h, _ = hub(fake, wake=wake)
    assert run(h.control("play"))["ok"] is True and woke == [1]
    # no Mac app to open: says what to do
    fake.devices = []
    h2, _ = hub(fake, wake=lambda: False)
    fake.active = False
    out = run(h2.control("play"))
    assert out["ok"] is False and "Open Spotify" in out["line"]


def test_refusals_in_plain_words() -> None:
    h, fake = hub()
    fake.premium = False
    assert run(h.control("next"))["line"] == "Controlling playback needs Spotify Premium."
    h, fake = hub()
    fake.active = False
    assert run(h.control("pause"))["line"] == "Spotify is already quiet."

    def down(*_a: Any) -> Any:
        raise OSError("no route to host")
    h = S.Spotify(S.SpotifyApi(S.Token({"client_id": "c", "refresh_token": "r"}, down), down), sleep=_no_sleep)
    assert run(h.control("pause"))["line"] == "Couldn't reach Spotify."


# ---- the code path ----

@pytest.mark.parametrize("text,cmd", [
    ("pause the music", S.Command("pause")),
    ("Stop music please", S.Command("pause")),
    ("hey eve, pause spotify", S.Command("pause")),
    ("play music", S.Command("play")),
    ("resume spotify", S.Command("play")),
    ("play music on Spotify", S.Command("play")),
    ("next song", S.Command("next")),
    ("skip this song", S.Command("next")),
    ("previous track", S.Command("previous")),
    ("music volume 40", S.Command("volume", volume=40)),
    ("spotify volume to 45%", S.Command("volume", volume=45)),
    ("set the volume of the music to 30", S.Command("volume", volume=30)),
    ("turn the music up", S.Command("volume", change=S.VOLUME_STEP)),
    ("music quieter", S.Command("volume", change=-S.VOLUME_STEP)),
    ("what's playing?", S.Command("now")),
    ("what song is this", S.Command("now")),
    ("play the music on the kitchen speaker please", S.Command("transfer", device="kitchen speaker")),
    ("move spotify to my phone", S.Command("transfer", device="phone")),
    ("play Daft Punk on Spotify", S.Command("play", query="daft punk")),
])
def test_match(text: str, cmd: S.Command) -> None:
    assert hub()[0].match(text) == cmd


@pytest.mark.parametrize("text", [
    "pause", "skip", "next", "volume 40", "turn the volume down", "music volume 400", "what is this", "what's on",
    "pause the music in ten minutes", "play some jazz", "turn off the lights", "stop", "what's the weather",
])
def test_match_leaves_the_rest_to_the_model(text: str) -> None:
    assert hub()[0].match(text) is None


def test_run_by_code_says_what_happened() -> None:
    h, _ = hub()
    assert run(h.run(S.Command("now"))) == "Playing One More Time by Daft Punk on Studio MacBook."
    assert run(h.run(S.Command("volume", volume=20))) == "Spotify volume 20%."


# ---- the tools ----

def test_tools_are_strict_and_complete() -> None:
    for t in S.TOOLS:
        params = t["parameters"]
        assert t["strict"] is True and params["additionalProperties"] is False
        assert set(params["required"]) == set(params["properties"])
    assert S.TOOL_NAMES == ("spotify_now_playing", "spotify_control")


def test_handle() -> None:
    h, fake = hub()
    assert run(h.handle("spotify_now_playing", {}))["track"] == "One More Time"
    args = {"action": "volume", "query": None, "kind": None, "volume": None, "volume_change": -10, "device": None}
    assert run(h.handle("spotify_control", args))["volume"] == 30
    args = {"action": "play", "query": "daft punk", "kind": "artist", "volume": None, "volume_change": None,
            "device": None}
    assert run(h.handle("spotify_control", args))["ok"]
    assert run(h.handle("spotify_control", {**args, "kind": "podcast"}))["ok"] is False
    assert run(h.handle("nope", {}))["ok"] is False


# ---- the Mini App panel ----

def test_panel_answers_with_fresh_state_and_refuses_search() -> None:
    h, fake = hub()
    st = run(h.panel({"action": "status"}))
    assert st["ok"] and st["playing"] is True
    paused = run(h.panel({"action": "pause"}))
    assert paused["ok"] and paused["line"] == "Paused." and paused["playing"] is False
    moved = run(h.panel({"action": "transfer", "device": "Kitchen Speaker"}))
    assert moved["line"] == "Playing on Kitchen Speaker." and moved["devices"]
    assert run(h.panel({"action": "play", "query": "anything"}))["ok"] is False     # the card only drives
    assert run(h.panel({"action": "rm -rf"}))["ok"] is False


# ---- construction and the CLI ----

def test_make_spotify_off_switch_and_no_login(tmp_path: Any) -> None:
    p = tmp_path / "spotify.json"
    assert S.make_spotify(p, environ={}) is None
    S.save({"client_id": "c", "refresh_token": "r"}, p)
    assert S.make_spotify(p, environ={"EVE_SPOTIFY": "0"}) is None
    assert isinstance(S.make_spotify(p, environ={}), S.Spotify)


def test_cli_status_volume_and_not_logged_in(tmp_path: Any) -> None:
    p = tmp_path / "spotify.json"
    lines: list[str] = []
    assert S.cli(["status"], path=p, out=lines.append) == 1 and "spotify login" in lines[0]
    S.save({"client_id": "c", "refresh_token": "r"}, p)
    fake = FakeSpotify()
    lines.clear()
    assert S.cli(["status"], path=p, out=lines.append, http=fake) == 0
    assert lines[0].startswith("Playing One More Time") and any("Kitchen Speaker" in x for x in lines)
    assert any("no remote control" in x for x in lines)
    lines.clear()
    assert S.cli(["volume", "55"], path=p, out=lines.append, http=fake) == 0 and fake.volume == 55
    assert S.cli(["login"], path=p, out=lines.append) == 2               # no client id
    assert S.cli(["logout"], path=p, out=lines.append) == 0 and not p.exists()


def test_pausing_what_is_already_paused_is_not_a_failure() -> None:
    h, fake = hub()
    real = fake.__call__

    def http(method: str, url: str, headers: dict, body: Any):
        if url.endswith("/me/player/pause"):
            fake.playing = False
            return 403, {}, b'{"error": {"status": 403, "message": "Player command failed: Restriction violated", "reason": "UNKNOWN"}}'
        return real(method, url, headers, body)
    h.api.http = http
    assert run(h.control("pause"))["line"] == "Spotify is already quiet."


def test_login_trades_the_code_with_the_injected_transport(tmp_path: Any, monkeypatch: Any) -> None:
    """The browser half is simulated: the redirect is sent to the loopback server the login opened."""
    import threading
    import urllib.request

    got: list = []

    def http(method: str, url: str, headers: dict, body: Any):
        got.append(dict(urllib.parse.parse_qsl(body.decode())))
        return 200, {}, b'{"access_token": "a", "refresh_token": "r-new", "expires_in": 3600}'

    monkeypatch.setattr(S, "LOGIN_PORT", 0)                  # any free port
    lines: list[str] = []
    ports: list[int] = []
    real_server = __import__("http.server", fromlist=["HTTPServer"]).HTTPServer

    class Server(real_server):
        def __init__(self, addr: Any, handler: Any) -> None:
            super().__init__(addr, handler)
            ports.append(self.server_address[1])

    monkeypatch.setattr("http.server.HTTPServer", Server)

    def browser(url: str) -> None:
        state = dict(urllib.parse.parse_qsl(urllib.parse.urlparse(url).query))["state"]

        def visit() -> None:
            urllib.request.urlopen(f"http://127.0.0.1:{ports[0]}/callback?code=c0de&state={state}", timeout=5).read()
        threading.Thread(target=visit, daemon=True).start()

    path = tmp_path / "spotify.json"
    assert S.login("cid", path=path, http=http, out=lines.append, open_browser=browser, timeout=10) == 0
    assert got[0]["code"] == "c0de" and got[0]["client_id"] == "cid" and got[0]["code_verifier"]
    assert S.load(path) == {"client_id": "cid", "refresh_token": "r-new", "scopes": S.SCOPES}


# ---- each device's colour ----

def test_each_device_keeps_its_own_colour_and_green_and_red_are_never_used(tmp_path: Any) -> None:
    path = tmp_path / "spotify-colors.json"
    c = S.DeviceColors(path)
    mac, tv, phone = c.hex("MacBook"), c.hex("TV"), c.hex("Phone")
    assert len({mac, tv, phone}) == 3                                   # distinct
    assert S.DeviceColors(path).hex("TV") == tv                         # kept across restarts
    assert S.DeviceColors(path).hex("Speaker") not in {mac, tv, phone}  # a new one takes a free colour
    rgbs = [rgb for _, rgb in S.DEVICE_COLORS]
    assert all(not (g > 150 and r < 100 and b < 150) for r, g, b in rgbs)     # no green: Spotify mode's
    assert all(not (r > 150 and g < 60 and b < 60) for r, g, b in rgbs)       # no red: a refusal's
    for i in range(20):                                                 # past seven, colours repeat evenly
        c.index(f"extra {i}")
    counts = [[e["i"] for e in c.known.values()].count(i) for i in range(len(S.DEVICE_COLORS))]
    assert max(counts) - min(counts) <= 1


def test_now_playing_gives_each_device_its_colour() -> None:
    h, _ = hub()
    devices = run(h.now_playing())["devices"]
    assert [d["color"] for d in devices] == [h.colors.hex(d["name"]) for d in devices]
    assert len({d["color"] for d in devices}) == len(devices)


def test_a_broken_colours_file_starts_fresh(tmp_path: Any) -> None:
    path = tmp_path / "c.json"
    path.write_text("not json")
    assert S.DeviceColors(path).hex("A") == "#%02x%02x%02x" % S.DEVICE_COLORS[0][1]


def test_a_device_not_seen_for_30_days_gives_its_colour_back(tmp_path: Any) -> None:
    path = tmp_path / "spotify-colors.json"
    day = {"d": 20000}
    c = S.DeviceColors(path, today=lambda: day["d"])
    mac, guest = c.index("MacBook"), c.index("Guest phone")
    day["d"] += 20
    assert c.index("MacBook") == mac                           # seen again: its day moves on
    day["d"] += 15                                             # the guest: 35 days unseen; the Mac: 15
    new = S.DeviceColors(path, today=lambda: day["d"]).index("New speaker")
    assert new == guest                                        # the guest's colour is free again
    kept = json.loads(path.read_text())
    assert "Guest phone" not in kept and kept["MacBook"]["i"] == mac


def test_the_first_colours_file_shape_still_reads(tmp_path: Any) -> None:
    path = tmp_path / "spotify-colors.json"
    path.write_text(json.dumps({"MacBook": 0, "TV": 1}))
    c = S.DeviceColors(path, today=lambda: 20000)
    assert c.index("MacBook") == 0 and c.index("TV") == 1 and c.index("Phone") == 2
