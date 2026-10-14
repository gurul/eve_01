"""lite_web.py: eve lite's web doors (docs/plans/eve-lite.md, M4). This file: the Mac's own page (LocalServer) on a
loopback port, and the helpers test_lite_web_lan.py and test_lite_web_security.py share.

A ``Rig`` is a real LocalServer (and, once a phone is added, a real LanServer) on ephemeral ports, with the real
web_auth objects, a real LocalTls and the real onboarding sign-ins in a tmp folder. Only the network is replaced:
the providers by httpx MockTransport, WebRTC, the Composio bridge and the daemon's status by small fakes, and the Local
Network alert and probe by recorders (the real ones would make macOS ask this test's python about the local network).
The client is a raw HTTP client, so a test sets every header itself (Host, Origin, Cookie) and reads every byte back.
"""
from __future__ import annotations

import asyncio
import contextlib
import gzip
import html
import json
import os
import socket
import ssl
import stat
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional
from urllib.parse import parse_qs, urlsplit

import httpx
import pytest
from test_link_chat import FakeInlet, events, store_at, two_lines

from eve_bridge import lite_web, onboarding, telegram, web_auth
from eve_bridge.composio_connect import APPS
from eve_bridge.link_chat import HiddenJobs, LinkChat
from eve_bridge.link_uploads import UploadStore
from eve_bridge.lite_rtc import RtcError
from eve_bridge.lite_tls import LocalTls

CANARY_KEY = "CANARY-NOT-A-KEY-7f3c-openrouter"         # what the OpenRouter exchange mints here
CANARY_TOKEN = "CANARY-NOT-A-KEY-7f3c-composio"          # Composio's tokens here
IPHONE = ("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) "
          "Version/18.0 Mobile/15E148 Safari/604.1")
MAC_SAFARI = ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) "
              "Version/18.0 Safari/605.1.15")
HOSTNAME, LAN_IP = "Test-Mac", "192.168.50.7"
SAME = "same"                                            # the request's own origin (http's default for a POST)
INDEX = b"<!doctype html><title>eve</title><div id=root></div><!-- the test page -->"
SCRIPT = b"export const eve = 1;\n" + b"console.log('eve lite web app');\n" * 200    # > GZIP_MIN_BYTES


# ---- the client ------------------------------------------------------------------------------------------------

@dataclass
class Reply:
    status: int
    headers: dict[str, str]
    cookies: list[str]
    body: bytes

    def json(self) -> Any:
        return json.loads(self.body)


def dechunk(raw: bytes) -> bytes:
    out, rest = b"", raw
    while True:
        size_line, sep, rest = rest.partition(b"\r\n")
        assert sep, "a chunk without its size line"
        size = int(size_line, 16)
        if size == 0:
            return out
        out, rest = out + rest[:size], rest[size + 2:]


def parse_reply(raw: bytes) -> Reply:
    top, sep, body = raw.partition(b"\r\n\r\n")
    assert sep, f"no complete head in {raw[:200]!r}"
    lines = top.decode("latin-1").split("\r\n")
    status = int(lines[0].split(" ")[1])
    headers: dict[str, str] = {}
    cookies: list[str] = []
    for line in lines[1:]:
        name, _, value = line.partition(":")
        name, value = name.strip().lower(), value.strip()
        if name == "set-cookie":
            cookies.append(value)
        headers[name] = f"{headers[name]}, {value}" if name in headers else value
    if headers.get("transfer-encoding") == "chunked":
        body = dechunk(body)
    return Reply(status, headers, cookies, body)


async def send_raw(port: int, data: bytes, *, tls: Optional[ssl.SSLContext] = None, secs: float = 20.0) -> bytes:
    """``data`` as it is, and everything the server writes until it closes."""
    reader, writer = await asyncio.open_connection("127.0.0.1", port, ssl=tls,
                                                   server_hostname="localhost" if tls is not None else None)
    try:
        writer.write(data)
        await writer.drain()
        return await asyncio.wait_for(reader.read(), secs)
    finally:
        writer.close()
        with contextlib.suppress(Exception):
            await writer.wait_closed()


async def http(port: int, method: str, path: str, body: Any = None, *, cookie: Optional[str] = None,
               host: Optional[str] = None, origin: Optional[str] = SAME, ctype: Optional[str] = None,
               headers: Optional[dict[str, str]] = None, tls: Optional[ssl.SSLContext] = None) -> Reply:
    """One request. ``host`` defaults to ``localhost:<port>``; ``origin`` to the page's own for a POST (None: no
    Origin); ``ctype`` to JSON for a POST ("": no Content-Type)."""
    host = f"localhost:{port}" if host is None else host
    scheme = "https" if tls is not None else "http"
    data = json.dumps(body).encode() if isinstance(body, (dict, list)) else (body or b"")
    lines = [f"{method} {path} HTTP/1.1"]
    if host:
        lines.append(f"Host: {host}")
    if origin == SAME:
        origin = f"{scheme}://{host}" if method not in ("GET", "HEAD") else None
    if origin is not None:
        lines.append(f"Origin: {origin}")
    if cookie:
        lines.append(f"Cookie: {cookie}")
    if method not in ("GET", "HEAD"):
        ctype = "application/json" if ctype is None else ctype
        if ctype:
            lines.append(f"Content-Type: {ctype}")
        lines.append(f"Content-Length: {len(data)}")
    lines += [f"{name}: {value}" for name, value in (headers or {}).items()]
    return parse_reply(await send_raw(port, ("\r\n".join(lines) + "\r\n\r\n").encode() + data, tls=tls))


def cookie_of(reply: Reply, name: str) -> str:
    """The value a Set-Cookie of ``name`` gives."""
    for line in reply.cookies:
        pair = line.split(";", 1)[0]
        if pair.startswith(name + "="):
            return pair[len(name) + 1:]
    raise AssertionError(f"no {name} cookie in {reply.cookies}")


# ---- the fakes -----------------------------------------------------------------------------------------------

class FakeRtc:
    """lite_rtc.RtcHub as lite_web uses it: answer, close_device, voice_ready."""

    def __init__(self) -> None:
        self.offers: list[tuple[str, str, str]] = []
        self.closed: list[str] = []
        self.voice = True

    async def answer(self, device_id: str, sdp: Any, peer_addr: str) -> dict[str, Any]:
        if not isinstance(sdp, str) or not sdp.startswith("v=0"):
            raise RtcError("the offer must be SDP of at most 20000 characters")
        self.offers.append((device_id, sdp, peer_addr))
        return {"sdp": "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\n", "voice": self.voice}

    async def close_device(self, device_id: str) -> None:
        self.closed.append(device_id)

    def voice_ready(self) -> bool:
        return self.voice


class FakeBridge:
    """composio_connect.ConnectBridge as Settings → Apps uses it."""

    def __init__(self, connected: Optional[dict[str, bool]] = None,
                 link: Optional[str] = "https://connect.composio.dev/link/ln_test") -> None:
        self.connected = dict(connected or {})
        self.link = link
        self.needs_sign_in = False
        self.asked: list[str] = []

    def connections(self, toolkits: Any = tuple(APPS), *, max_age: float = 60.0) -> dict[str, bool]:
        return {t: self.connected.get(t, False) for t in toolkits}

    def connect_link(self, toolkit: str) -> Optional[str]:
        self.asked.append(toolkit)
        return None if self.connected.get(toolkit) else self.link


def make_web(folder: Path) -> Path:
    """A built web page as WP-11 lays it out: index.html and one flat assets folder."""
    (folder / "assets").mkdir(parents=True, exist_ok=True)
    (folder / "index.html").write_bytes(INDEX)
    (folder / "assets" / "eve-abc123.js").write_bytes(SCRIPT)
    (folder / "assets" / "eve-abc123.css").write_bytes(b"body{margin:0}\n")
    return folder


@dataclass
class Rig:
    """A LocalServer on an ephemeral loopback port (module docstring). ``async with Rig(tmp_path) as r``."""

    tmp: Path
    link: Any = None
    rtc: Any = field(default_factory=FakeRtc)
    apps: Any = None
    status: dict[str, Any] = field(default_factory=lambda: {"state": "READY"})
    environ: Optional[dict[str, str]] = None
    web: Optional[Path] = None
    peer_allowed: Any = None
    local_ports: tuple[int, ...] = (0,)
    lan_ports: tuple[int, ...] = (0,)
    provider_calls: list[str] = field(default_factory=list)
    credit_calls: int = 0
    credit_answer: dict[str, Any] = field(default_factory=lambda: {"ok": True, "usd": 4.25, "funded": True})
    register_status: int = 201                           # Composio's answer to the client registration
    restarts: int = 0
    alerts: int = 0
    probes: list[Any] = field(default_factory=list)

    async def __aenter__(self) -> "Rig":
        self.tmp.mkdir(parents=True, exist_ok=True)
        self.client = httpx.AsyncClient(transport=httpx.MockTransport(self._provider))
        env = self.environ if self.environ is not None else {"EVE_PROFILE": "lite"}
        web_dir = self.web if self.web is not None else make_web(self.tmp / "web")
        self.ctx = lite_web.WebContext(
            self.link, self.rtc, web_auth.Devices(self.tmp / "web-devices.json"), web_auth.PairingWindow(),
            web_auth.MacLogin(), LocalTls(self.tmp / "web-tls"), onboarding.OpenRouterLogin(http=self.client),
            onboarding.ComposioConnectLogin(self.tmp / "composio-connect.json", http=self.client), self.apps,
            lambda: self.status, self._restart, web_dir, self.tmp / "web.json",
            environ=env, env_path=self.tmp / "env", credit=self._credit, hostname=lambda: HOSTNAME,
            addresses=lambda: [LAN_IP], alert=self._alert, probe=self._probe,
            local_hosts=("127.0.0.1",), local_ports=self.local_ports, lan_hosts=("127.0.0.1",),
            lan_ports=self.lan_ports, restart_delay=0.0,
            **({"peer_allowed": self.peer_allowed} if self.peer_allowed is not None else {}))
        self.local = lite_web.LocalServer(self.ctx)
        try:
            self.port = await self.local.start()
        except BaseException:
            await self.client.aclose()
            raise
        return self

    async def __aexit__(self, *exc: Any) -> None:
        await self.local.stop()
        await self.client.aclose()

    # -- the providers, the daemon, macOS --
    def _provider(self, request: httpx.Request) -> httpx.Response:
        url = str(request.url)
        self.provider_calls.append(url)
        if url == onboarding.OPENROUTER_KEYS_URL:
            return httpx.Response(200, json={"key": CANARY_KEY})
        if url == onboarding.CONNECT_REGISTER_URL:
            if self.register_status != 201:
                return httpx.Response(self.register_status, json={"error": "invalid_redirect_uri"})
            asked = json.loads(request.content)
            return httpx.Response(201, json={"client_id": "eve-lite-test-client", "redirect_uris": asked["redirect_uris"]})
        if url == onboarding.CONNECT_TOKEN_URL:
            return httpx.Response(200, json={"access_token": CANARY_TOKEN + "-access",
                                             "refresh_token": CANARY_TOKEN + "-refresh", "expires_in": 3600})
        return httpx.Response(404)

    async def _credit(self, key: str) -> dict[str, Any]:
        self.credit_calls += 1
        return dict(self.credit_answer)

    def _restart(self) -> None:
        self.restarts += 1

    def _alert(self) -> None:
        self.alerts += 1

    def _probe(self, peer: Any) -> str:
        self.probes.append(peer)
        return "ok"

    # -- the browsers --
    def trust(self) -> ssl.SSLContext:
        """A client that trusts this rig's local CA, as a phone that installed it."""
        return ssl.create_default_context(cafile=str(self.ctx.tls.ca_cert_path))

    async def mac(self, method: str, path: str, body: Any = None, **kw: Any) -> Reply:
        return await http(self.port, method, path, body, **kw)

    async def lan(self, method: str, path: str, body: Any = None, **kw: Any) -> Reply:
        lan = self.ctx.shared.lan
        assert lan is not None and lan.running, "the LAN door is not open"
        kw.setdefault("tls", self.trust())
        return await http(lan.port, method, path, body, **kw)

    async def login(self, ua: str = MAC_SAFARI) -> str:
        """The Mac's browser signs in as `eve-bridge web open` has it do: its eve_mac token."""
        reply = await self.mac("POST", lite_web.MAC_LOGIN_PATH, {"code": self.ctx.mac_login.issue()},
                               headers={"User-Agent": ua})
        assert reply.status == 200, reply.body
        return cookie_of(reply, web_auth.COOKIE_MAC)

    async def open_pairing(self, mac_token: str) -> dict[str, Any]:
        reply = await self.mac("POST", lite_web.PAIR_OPEN_PATH, {}, cookie=f"eve_mac={mac_token}")
        assert reply.status == 200, reply.body
        return reply.json()

    async def pair(self, mac_token: Optional[str] = None, ua: str = IPHONE) -> str:
        """A phone pairs through the QR code's secret, the window opened by the Mac's page (signed in now unless
        ``mac_token`` is given): its eve_dev token."""
        opened = await self.open_pairing(mac_token or await self.login())
        secret = opened["links"][0]["url"].split("#p=", 1)[1]
        reply = await self.lan("POST", lite_web.PAIR_PATH, {"secret": secret}, headers={"User-Agent": ua})
        assert reply.status == 200, reply.body
        return cookie_of(reply, web_auth.COOKIE_DEV)


def run(coro: Any) -> Any:
    return asyncio.run(coro)


async def until(pred: Any, secs: float = 10.0) -> None:
    """Wait for ``pred()`` to hold: work the door does in the background (a probe, a read, a closing connection)."""
    for _ in range(int(secs / 0.01)):
        if pred():
            return
        await asyncio.sleep(0.01)
    raise AssertionError("timed out waiting")


def link_door(tmp_path: Path, inlet: Any) -> LinkChat:
    return LinkChat(lambda: inlet, uploads=UploadStore(tmp_path / "uploads"),
                    hidden=HiddenJobs(tmp_path / "hidden-jobs.json"), settle_secs=0.01)


@pytest.fixture(autouse=True)
def _plain_logging():
    """The sign-ins hand their key or token to telegram.hide_token, which redacts it in every log record for the
    rest of the process: undone after each test, as test_onboarding does."""
    yield
    telegram.unhide_tokens()


# ---- the page ----------------------------------------------------------------------------------------------------

def test_the_page_and_its_assets_come_from_eve_web_dir(tmp_path: Path) -> None:
    async def go() -> None:
        web = make_web(tmp_path / "elsewhere")
        async with Rig(tmp_path / "rig", environ={"EVE_PROFILE": "lite", "EVE_WEB_DIR": str(web)}) as r:
            r.ctx.web_dir = None                                      # EVE_WEB_DIR decides, as in the daemon
            page = await r.mac("GET", "/")
            assert page.status == 200 and page.body == INDEX
            assert page.headers["content-type"] == "text/html; charset=utf-8"
            assert page.headers["cache-control"] == "no-store"
            script = await r.mac("GET", "/assets/eve-abc123.js")
            assert script.status == 200 and script.body == SCRIPT
            assert script.headers["content-type"] == "text/javascript; charset=utf-8"
            assert script.headers["cache-control"] == "public, max-age=31536000, immutable"
            assert "content-encoding" not in script.headers                       # not asked for gzip
            style = await r.mac("GET", "/assets/eve-abc123.css")
            assert style.headers["content-type"] == "text/css; charset=utf-8"
            # gzip when the browser takes it, the same bytes once undone
            squeezed = await r.mac("GET", "/assets/eve-abc123.js", headers={"Accept-Encoding": "gzip, deflate, br"})
            assert squeezed.headers["content-encoding"] == "gzip" and squeezed.headers["vary"] == "Accept-Encoding"
            assert gzip.decompress(squeezed.body) == SCRIPT and len(squeezed.body) < len(SCRIPT)
            refused = await r.mac("GET", "/assets/eve-abc123.js", headers={"Accept-Encoding": "gzip;q=0"})
            assert "content-encoding" not in refused.headers and refused.body == SCRIPT
            # HEAD: the head alone
            head = await r.mac("HEAD", "/")
            assert head.status == 200 and head.body == b"" and head.headers["content-length"] == str(len(INDEX))
            # a changed file is served changed (the cache follows the file)
            (web / "index.html").write_bytes(INDEX + b"<!-- v2 -->")
            assert (await r.mac("GET", "/")).body.endswith(b"<!-- v2 -->")

    run(go())


def test_only_files_inside_the_web_folder_are_served(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            web = r.ctx.web_root()
            (tmp_path / "secret.txt").write_text("not for the page")
            (web / "assets" / "eve-link.js").symlink_to(tmp_path / "secret.txt")   # a link out of the folder
            (web / "assets" / "deeper").mkdir()
            (web / "assets" / "deeper" / "x.js").write_text("x")
            for path in ("/assets/eve-link.js", "/assets/missing.js", "/assets/deeper/x.js", "/assets/../index.html",
                         "/assets/%2e%2e/secret.txt", "/assets/.hidden", "/assets/", "/../secret.txt",
                         "/secret.txt", "/web.json", "/env"):
                reply = await r.mac("GET", path)
                assert reply.status in (400, 404), (path, reply.status)
                assert b"not for the page" not in reply.body
            assert (await r.mac("GET", "/assets/eve-abc123.js")).status == 200                 # positive control

    run(go())


def test_no_built_page_is_a_plain_503_page_not_a_crash(tmp_path: Path) -> None:
    async def go() -> None:
        empty = tmp_path / "empty"
        empty.mkdir()
        async with Rig(tmp_path / "rig", web=empty) as r:
            reply = await r.mac("GET", "/")
            assert reply.status == 503 and reply.headers["content-type"] == "text/html; charset=utf-8"
            assert html.escape(lite_web.NOT_BUILT).encode() in reply.body and b"<script" not in reply.body
            assert (await r.mac("GET", "/assets/eve-abc123.js")).status == 404
            assert (await r.mac("GET", lite_web.HEALTH_PATH)).status == 200                   # the door itself is up

    run(go())


def test_healthz_says_the_state_with_no_cookie(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            assert (await r.mac("GET", lite_web.HEALTH_PATH)).json() == {"ok": True, "state": "READY"}
            r.status = {"state": "NEEDS_OPENROUTER"}
            assert (await r.mac("GET", lite_web.HEALTH_PATH)).json() == {"ok": True, "state": "NEEDS_OPENROUTER"}
            r.status = {"state": "sk-like lower-case words"}                  # not a state word: not passed on
            assert (await r.mac("GET", lite_web.HEALTH_PATH)).json()["state"] == "NEEDS_OPENROUTER"

    run(go())


# ---- the Mac's sign-in -------------------------------------------------------------------------------------------

def test_the_macs_browser_signs_in_once_with_its_code(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            code = r.ctx.mac_login.issue()
            reply = await r.mac("POST", lite_web.MAC_LOGIN_PATH, {"code": code}, headers={"User-Agent": MAC_SAFARI})
            assert reply.status == 200 and reply.json() == {"ok": True}
            token = cookie_of(reply, web_auth.COOKIE_MAC)
            assert reply.cookies == [f"eve_mac={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=34560000"]
            assert r.ctx.mac_login.used(code)                               # `web open` deletes its file now
            again = await r.mac("POST", lite_web.MAC_LOGIN_PATH, {"code": code})
            assert again.status == 403 and again.json() == {"ok": False, "error": lite_web.LOGIN_SPENT}
            assert again.cookies == []
            wrong = await r.mac("POST", lite_web.MAC_LOGIN_PATH, {"code": "A" * 22})
            assert wrong.status == 403
            # the cookie opens the Mac's routes
            state = await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=f"eve_mac={token}")
            assert state.status == 200
            # opening Eve again with a live cookie keeps this browser's one device
            reopened = await r.mac("POST", lite_web.MAC_LOGIN_PATH, {"code": r.ctx.mac_login.issue()},
                                   cookie=f"eve_mac={token}")
            assert reopened.status == 200 and cookie_of(reopened, web_auth.COOKIE_MAC) == token
            devices = r.ctx.devices.list()
            assert len(devices) == 1 and devices[0]["label"] == "This Mac · Safari"
            assert devices[0]["host"] == "localhost"

    run(go())


def test_the_setup_state_reads_the_key_the_skip_and_the_credit(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path, environ={"EVE_PROFILE": "lite"}) as r:
            mac = f"eve_mac={await r.login()}"
            state = await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)
            assert state.json() == {"openrouter": False, "openrouter_rejected": False, "credit": None,
                                    "composio": False, "composio_skipped": False, "restarting": False, "error": ""}
            assert r.credit_calls == 0                                      # no key: nothing to ask OpenRouter
            assert cookie_of(state, web_auth.COOKIE_MAC) == mac.split("=", 1)[1]   # set again: 400 days from now
            r.ctx.environ["OPENROUTER_API_KEY"] = CANARY_KEY
            state = await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)
            assert state.json()["openrouter"] is True
            assert state.json()["credit"] == {"ok": True, "usd": 4.25, "funded": True}
            await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)
            assert r.credit_calls == 1                                      # read once per CREDIT_TTL_SECS
            skipped = await r.mac("POST", lite_web.COMPOSIO_SKIP_PATH, {}, cookie=mac)
            assert skipped.status == 200 and skipped.json() == {"ok": True}
            assert (await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)).json()["composio_skipped"] is True
            saved = json.loads((tmp_path / "web.json").read_text())
            assert saved["composio_skipped"] is True
            assert stat.S_IMODE((tmp_path / "web.json").stat().st_mode) == 0o600
        async with Rig(tmp_path) as again:                                   # remembered across a restart
            mac = f"eve_mac={await again.login()}"
            assert (await again.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)).json()["composio_skipped"] is True

    run(go())


def test_a_turned_down_key_shows_as_rejected_on_state_and_status(tmp_path: Path) -> None:
    """onboarding.credit answers {"rejected": True} on a 401 from /api/v1/key: both the setup state and the status
    carry a top-level openrouter_rejected so the page can offer a new sign-in, and the key stays "present"."""
    async def go() -> None:
        env = {"EVE_PROFILE": "lite", "OPENROUTER_API_KEY": CANARY_KEY}
        async with Rig(tmp_path, environ=env) as r:
            r.credit_answer = {"ok": False, "usd": None, "funded": None, "error": "HTTP 401", "rejected": True}
            mac = f"eve_mac={await r.login()}"
            state = (await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)).json()
            assert state["openrouter"] is True and state["openrouter_rejected"] is True
            status = (await r.mac("GET", lite_web.STATUS_PATH, cookie=mac)).json()
            assert status["openrouter"] is True and status["openrouter_rejected"] is True
            assert status["credit"]["rejected"] is True                     # the flag rides the credit view too
            # a 500 is a passing error, not a rejection: the key is not turned down
            r.ctx.shared.credit = None
            r.credit_answer = {"ok": False, "usd": None, "funded": None, "error": "HTTP 500"}
            again = (await r.mac("GET", lite_web.STATUS_PATH, cookie=mac)).json()
            assert again["openrouter_rejected"] is False

    run(go())


def test_a_slow_credit_read_never_holds_the_page_up(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    async def go() -> None:
        monkeypatch.setattr(lite_web, "CREDIT_WAIT_SECS", 0.05)
        gate = asyncio.Event()
        async with Rig(tmp_path, environ={"EVE_PROFILE": "lite", "OPENROUTER_API_KEY": CANARY_KEY}) as r:
            async def slow(key: str) -> dict[str, Any]:
                await gate.wait()
                return {"ok": True, "usd": 1.5, "funded": True}

            r.ctx.credit = slow
            mac = f"eve_mac={await r.login()}"
            first = await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)
            assert first.status == 200 and first.json()["credit"] is None   # not known yet, and not waited for
            gate.set()
            await until(lambda: r.ctx.shared.credit is not None)              # the read lands for the next page
            later = await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)
            assert later.json()["credit"] == {"ok": True, "usd": 1.5, "funded": True}

    run(go())


# ---- adding a phone, the devices, the LAN switch ---------------------------------------------------------------

def test_add_a_phone_opens_the_lan_door_and_draws_two_codes(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            assert not r.local.lan_on() and r.alerts == 0
            opened = await r.open_pairing(await r.login())
            lan = r.ctx.shared.lan
            assert lan is not None and lan.running and r.local.lan_on()
            assert r.alerts == 1                                            # macOS's Local Network alert, on purpose
            assert opened["code"].isdigit() and len(opened["code"]) == 6
            assert 290 <= opened["expires_in"] <= 300
            assert [link["label"] for link in opened["links"]] == ["iPhone or iPad", "Android or other"]
            apple, other = (link["url"] for link in opened["links"])
            assert apple.startswith(f"https://test-mac.local:{lan.port}/#p=")
            assert other.startswith(f"https://{LAN_IP}:{lan.port}/#p=")
            secret = apple.split("#p=", 1)[1]
            assert other.endswith(secret) and len(secret) >= 16
            for link in opened["links"]:
                assert link["qr"].startswith("data:image/svg+xml") and "%3Csvg" in link["qr"]
            assert opened["url_apple"] == apple and opened["qr_other"].startswith("data:image/svg+xml")
            assert r.ctx.pairing.is_open
            # the certificate names this Mac, and web.json remembers the door
            assert {"test-mac.local", LAN_IP, "localhost"} <= r.ctx.tls.names()
            saved = json.loads((tmp_path / "web.json").read_text())
            assert saved["lan"] is True and saved["lan_port"] == lan.port and saved["local_port"] == r.port

    run(go())


def test_no_address_for_a_phone_says_so_and_opens_no_window(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            r.ctx.hostname = lambda: ""
            r.ctx.addresses = lambda: []
            reply = await r.mac("POST", lite_web.PAIR_OPEN_PATH, {}, cookie=f"eve_mac={await r.login()}")
            assert reply.status == 503 and reply.json()["error"] == lite_web.NO_ADDRESS
            assert not r.ctx.pairing.is_open

    run(go())


def test_the_device_list_marks_this_browser_and_a_revoke_closes_the_peer(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            mac_token = await r.login()
            mac = f"eve_mac={mac_token}"
            phone = await r.pair(mac_token)
            # the phone signals a WebRTC peer through the LAN door
            assert (await r.lan("POST", lite_web.RTC_PATH, {"sdp": "v=0\r\n"}, cookie=f"eve_dev={phone}")).status == 200
            listed = (await r.mac("GET", lite_web.DEVICES_PATH, cookie=mac)).json()["devices"]
            by_label = {d["label"]: d for d in listed}                        # paired in one second: any order
            assert sorted(by_label) == ["This Mac · Safari", "iPhone · Safari"]
            this_mac, iphone = by_label["This Mac · Safari"], by_label["iPhone · Safari"]
            assert this_mac["current"] is True and iphone["current"] is False
            assert iphone["host"] == "localhost" and set(iphone) == {"id", "label", "host", "created", "last_seen",
                                                                     "current"}
            assert phone not in json.dumps(listed) and mac_token not in json.dumps(listed)   # hashes stay on disk
            phone_id = iphone["id"]
            gone = await r.mac("POST", lite_web.REVOKE_PATH, {"id": phone_id}, cookie=mac)
            assert gone.status == 200 and gone.json() == {"ok": True, "revoked": [phone_id]}
            assert r.rtc.closed == [phone_id]                                # its live peer closed
            assert (await r.lan("GET", lite_web.STATUS_PATH, cookie=f"eve_dev={phone}")).status == 401
            assert (await r.mac("POST", lite_web.REVOKE_PATH, {"id": phone_id}, cookie=mac)).json() == {
                "ok": True, "revoked": []}                                   # twice is nothing more
            for bad in ({"id": "../x"}, {"id": 7}, {}):
                assert (await r.mac("POST", lite_web.REVOKE_PATH, bad, cookie=mac)).status == 400
            everything = await r.mac("POST", lite_web.REVOKE_PATH, {"id": "all"}, cookie=mac)
            assert everything.json()["revoked"] == [this_mac["id"]]
            assert (await r.mac("GET", lite_web.DEVICES_PATH, cookie=mac)).status == 401   # this browser too

    run(go())


def test_signing_out_revokes_this_browser_and_clears_its_cookie(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            token = await r.login()
            out = await r.mac("POST", lite_web.LOGOUT_PATH, {}, cookie=f"eve_mac={token}")
            assert out.status == 200 and out.json() == {"ok": True}
            assert out.cookies == ["eve_mac=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"]
            assert r.ctx.devices.list() == []
            refused = await r.mac("GET", lite_web.STATUS_PATH, cookie=f"eve_mac={token}")
            assert refused.status == 401 and refused.cookies == ["eve_mac=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"]

    run(go())


def test_the_lan_switch_turns_the_door_off_and_on_and_is_remembered(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            mac_token = await r.login()
            mac = f"eve_mac={mac_token}"
            phone = await r.pair(mac_token)
            await r.lan("POST", lite_web.RTC_PATH, {"sdp": "v=0\r\n"}, cookie=f"eve_dev={phone}")
            await r.mac("POST", lite_web.RTC_PATH, {"sdp": "v=0\r\n"}, cookie=mac)     # the Mac's own voice peer
            lan_port = r.ctx.shared.lan.port
            r.ctx.pairing.open()
            off = await r.mac("POST", lite_web.LAN_PATH, {"on": False}, cookie=mac)
            assert off.status == 200 and off.json() == {"ok": True, "lan": False}
            assert not r.ctx.pairing.is_open                                  # a window closes with the door
            phone_id = next(d["id"] for d in r.ctx.devices.list() if d["label"].startswith("iPhone"))
            assert r.rtc.closed == [phone_id]                                 # the LAN's peers, not the Mac's own
            with pytest.raises(OSError):
                await asyncio.open_connection("127.0.0.1", lan_port)
            assert json.loads((tmp_path / "web.json").read_text())["lan"] is False
            status = await r.mac("GET", lite_web.STATUS_PATH, cookie=mac)
            assert status.json()["lan"] is False
            r.ctx.lan_ports = (lan_port, 0)                                  # as the daemon's range holds it
            on = await r.mac("POST", lite_web.LAN_PATH, {"on": True}, cookie=mac)
            assert on.json() == {"ok": True, "lan": True} and r.ctx.shared.lan.port == lan_port   # the same address
            assert (await r.lan("GET", lite_web.STATUS_PATH, cookie=f"eve_dev={phone}")).status == 200
            for bad in ({"on": "yes"}, {"on": 1}, {}):
                assert (await r.mac("POST", lite_web.LAN_PATH, bad, cookie=mac)).status == 400
        async with Rig(tmp_path, lan_ports=(lan_port, 0)) as again:          # web.json says on: it opens at start
            assert again.local.lan_on() and again.ctx.shared.lan.port == lan_port

    run(go())


# ---- the chat, signaling, apps, status ------------------------------------------------------------------------

def test_the_macs_page_chats_with_the_unchanged_linkchat_over_loopback(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), two_lines)
        link = link_door(tmp_path, inlet)
        async with Rig(tmp_path / "rig", link=link) as r:
            mac = f"eve_mac={await r.login()}"
            turn = await r.mac("POST", "/api/web/link/turn", {"text": "  hello eve  ", "client_id": "web-turn-1"},
                               cookie=mac)
            assert turn.status == 200 and turn.headers["content-type"] == "text/event-stream"
            contents, done, _ = events(turn.body)
            assert contents == ["First line.", "\n\nSecond line."] and done
            assert inlet.heard == [("hello eve", False, "web-turn-1")]
            assert turn.headers["referrer-policy"] == "no-referrer"          # this door's headers, added
            assert turn.headers["x-content-type-options"] == "nosniff"
            history = await r.mac("GET", "/api/web/link/messages", cookie=mac)
            assert history.status == 200 and isinstance(history.json()["messages"], list)
            up = await r.mac("POST", "/api/web/link/upload", b"some notes\n", ctype="application/octet-stream",
                             headers={"X-Filename": "notes.txt"}, cookie=mac)
            assert up.status == 200 and up.json()["ok"] is True and up.json()["kind"] == "text"
            kept = await r.mac("GET", f"/api/web/link/file/{up.json()['id']}", cookie=mac)
            assert kept.status == 200 and kept.body == b"some notes\n"
            assert kept.headers["x-content-type-options"] == "nosniff"       # LinkChat's own, not doubled
            assert kept.headers["referrer-policy"] == "no-referrer"

    run(go())


def test_the_chat_before_setup_is_a_503_with_a_line(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path, link=None) as r:
            mac = f"eve_mac={await r.login()}"
            reply = await r.mac("GET", "/api/web/link/messages", cookie=mac)
            assert reply.status == 503 and reply.json() == {"ok": False, "error": lite_web.NOT_READY}

    run(go())


def test_rtc_signaling_passes_the_device_and_its_address(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            mac = f"eve_mac={await r.login()}"
            me = r.ctx.devices.list()[0]["id"]
            answer = await r.mac("POST", lite_web.RTC_PATH, {"sdp": "v=0\r\noffer"}, cookie=mac)
            assert answer.status == 200 and answer.json() == {"sdp": "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\n",
                                                              "voice": True}
            assert r.rtc.offers == [(me, "v=0\r\noffer", "127.0.0.1")]
            bad = await r.mac("POST", lite_web.RTC_PATH, {"sdp": 42}, cookie=mac)
            assert bad.status == 400 and "SDP" in bad.json()["error"]
            assert r.probes == []                                           # loopback: nothing to probe
            r.ctx.rtc = None
            off = await r.mac("POST", lite_web.RTC_PATH, {"sdp": "v=0\r\n"}, cookie=mac)
            assert off.status == 503 and off.json()["error"] == lite_web.RTC_OFF

    run(go())


def test_apps_list_the_connected_apps_and_hand_out_composio_links_only(tmp_path: Path) -> None:
    async def go() -> None:
        bridge = FakeBridge({"gmail": True})
        async with Rig(tmp_path, apps=bridge) as r:
            mac = f"eve_mac={await r.login()}"
            assert (await r.mac("GET", lite_web.APPS_PATH, cookie=mac)).json() == {"composio": False, "apps": []}
            (tmp_path / "composio-connect.json").write_text("{}")             # signed in (onboarding writes it)
            listed = (await r.mac("GET", lite_web.APPS_PATH, cookie=mac)).json()
            assert listed["composio"] is True
            assert [a["toolkit"] for a in listed["apps"]] == list(APPS)
            assert {a["toolkit"]: a["connected"] for a in listed["apps"]}["gmail"] is True
            assert {a["toolkit"]: a["name"] for a in listed["apps"]}["googlecalendar"] == "Google Calendar"
            link = await r.mac("POST", "/api/web/apps/slack/connect", {}, cookie=mac)
            assert link.status == 200 and link.json() == {"ok": True, "url": "https://connect.composio.dev/link/ln_test"}
            already = await r.mac("POST", "/api/web/apps/gmail/connect", {}, cookie=mac)
            assert already.status == 409 and already.json()["error"] == "Gmail is already connected."
            assert (await r.mac("POST", "/api/web/apps/tavily/connect", {}, cookie=mac)).status == 404   # web search
            assert (await r.mac("POST", "/api/web/apps/Gmail%20x/connect", {}, cookie=mac)).status == 404
            bridge.link = "https://composio.dev.example.com/link"             # a look-alike host
            foreign = await r.mac("POST", "/api/web/apps/notion/connect", {}, cookie=mac)
            assert foreign.status == 502 and "url" not in foreign.json()
            bridge.needs_sign_in = True                                       # the sign-in lapsed for good
            assert (await r.mac("GET", lite_web.APPS_PATH, cookie=mac)).json()["composio"] is False
            assert (await r.mac("POST", "/api/web/apps/slack/connect", {}, cookie=mac)).status == 409

    run(go())


def test_status_is_booleans_words_and_the_credit(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path, environ={"EVE_PROFILE": "lite", "OPENROUTER_API_KEY": CANARY_KEY}) as r:
            mac = f"eve_mac={await r.login()}"
            status = (await r.mac("GET", lite_web.STATUS_PATH, cookie=mac)).json()
            assert status == {"state": "READY", "openrouter": True, "openrouter_rejected": False, "composio": False,
                              "voice": True, "lan": False, "local_network": "unknown", "restarting": False,
                              "credit": {"ok": True, "usd": 4.25, "funded": True}}
            r.rtc.voice = False
            r.status = {"state": "READY", "voice": True}                     # the daemon's word wins
            assert (await r.mac("GET", lite_web.STATUS_PATH, cookie=mac)).json()["voice"] is True
            r.status = {"state": "READY"}
            assert (await r.mac("GET", lite_web.STATUS_PATH, cookie=mac)).json()["voice"] is False

    run(go())


# ---- the listener ------------------------------------------------------------------------------------------------

def test_the_mac_door_listens_on_loopback_only(tmp_path: Path) -> None:
    async def go() -> None:
        rig = Rig(tmp_path)
        async with rig as r:
            await r.local.stop()
            r.ctx.local_hosts = lite_web.LOCAL_HOSTS                          # the daemon's: 127.0.0.1 and ::1
            port = r.port = await r.local.start()
            bound = r.local.bound
            assert {address for address, _ in bound} <= {"127.0.0.1", "::1"} and ("127.0.0.1", port) in bound
            assert all(p == port for _, p in bound)
            assert (await r.mac("GET", lite_web.HEALTH_PATH)).status == 200
            if socket.has_ipv6 and any(a == "::1" for a, _ in bound):
                reader, writer = await asyncio.open_connection("::1", port)
                writer.write(f"GET /healthz HTTP/1.1\r\nHost: [::1]:{port}\r\n\r\n".encode())
                assert (await asyncio.wait_for(reader.read(), 10)).startswith(b"HTTP/1.1 200")
                writer.close()

    run(go())


def test_the_port_web_json_names_comes_first_and_a_taken_one_is_passed_over(tmp_path: Path) -> None:
    async def go() -> None:
        taken = socket.socket()
        taken.bind(("127.0.0.1", 0))
        taken.listen()
        busy = taken.getsockname()[1]
        try:
            rig = Rig(tmp_path)
            async with rig as r:
                await r.local.stop()
                r.ctx.local_ports = (busy, 0)
                port = r.port = await r.local.start()
                assert port != busy and (await r.mac("GET", lite_web.HEALTH_PATH)).status == 200
                assert json.loads((tmp_path / "web.json").read_text())["local_port"] == port
                await r.local.stop()
                r.ctx.local_ports = (0, port)                                 # the remembered port first
                r.ctx.shared.saved.local_port = port
                assert await r.local.start() == port
                await r.local.stop()
                r.ctx.local_ports = (busy,)
                with pytest.raises(lite_web.PortsTaken):
                    await r.local.start()
                r.ctx.local_ports = (0,)
                await r.local.start()                                        # for the rig's own stop
        finally:
            taken.close()

    run(go())


def test_the_doors_are_eve_lites_only(tmp_path: Path) -> None:
    async def go() -> None:
        with pytest.raises(RuntimeError, match="EVE_PROFILE=lite"):
            async with Rig(tmp_path, environ={}):
                pass
        with pytest.raises(RuntimeError):
            async with Rig(tmp_path / "full", environ={"EVE_PROFILE": "full"}):
                pass

    run(go())


def test_stopping_ends_an_open_turn_with_done_first(tmp_path: Path) -> None:
    async def go() -> None:
        async def forever(inlet: FakeInlet, app: str) -> None:
            inlet.out(app, "Working on it.")                                 # and the turn stays in flight

        inlet = FakeInlet(store_at(tmp_path), forever)
        link = link_door(tmp_path, inlet)
        rig = Rig(tmp_path / "rig", link=link)
        await rig.__aenter__()
        mac = f"eve_mac={await rig.login()}"
        pending = asyncio.ensure_future(rig.mac("POST", "/api/web/link/turn", {"text": "hi", "client_id": "t-stop-1"},
                                                cookie=mac))
        for _ in range(200):
            if inlet.heard:
                break
            await asyncio.sleep(0.01)
        await rig.__aexit__(None, None, None)
        turn = await asyncio.wait_for(pending, 10)
        contents, done, _ = events(turn.body)
        assert contents == ["Working on it."] and done                       # [DONE], not a cut stream

    run(go())


def test_a_door_never_logs_a_path_an_address_or_a_cookie(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    async def go() -> None:
        caplog.set_level("DEBUG")
        async with Rig(tmp_path) as r:
            token = await r.login()
            phone = await r.pair()
            for path in ("/onboard/state", "/no/such/path-7f3c", "/api/web/status"):
                await r.mac("GET", path, cookie=f"eve_mac={token}")
            await r.lan("GET", lite_web.STATUS_PATH, cookie=f"eve_dev={phone}")
            await r.mac("GET", "/", host="evil.example:80")
        text = "\n".join(record.getMessage() for record in caplog.records)
        for leak in (token, phone, "/no/such/path-7f3c", "/onboard/state", "evil.example", HOSTNAME.lower(), LAN_IP,
                     "127.0.0.1"):
            assert leak not in text, leak
        assert "lite web: the Mac's page is up on port" in text              # positive control: it does log

    run(go())


def test_pairing_and_devices_files_are_owner_only(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            await r.pair()
            for name in ("web.json", "web-devices.json"):
                assert stat.S_IMODE((tmp_path / name).stat().st_mode) == 0o600, name
            assert stat.S_IMODE((tmp_path / "web-tls").stat().st_mode) == 0o700
            assert os.listdir(tmp_path / "web-tls")

    run(go())


def test_a_callback_url_names_this_door_and_its_port(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            mac = f"eve_mac={await r.login()}"
            start = await r.mac("GET", lite_web.OPENROUTER_START_PATH, cookie=mac)
            assert start.status == 302
            asked = parse_qs(urlsplit(start.headers["location"]).query)
            assert asked["callback_url"] == [f"http://localhost:{r.port}{lite_web.OPENROUTER_CALLBACK_PATH}"]
            assert asked["code_challenge_method"] == ["S256"] and asked["state"][0]
            composio = await r.mac("GET", lite_web.COMPOSIO_START_PATH, cookie=mac)
            assert composio.status == 302
            asked = parse_qs(urlsplit(composio.headers["location"]).query)
            assert asked["redirect_uri"] == [f"http://localhost:{r.port}{lite_web.COMPOSIO_CALLBACK_PATH}"]
            assert asked["resource"] == [onboarding.CONNECT_RESOURCE]

    run(go())


def test_a_restart_that_stops_the_doors_itself_is_not_cut_short(tmp_path: Path) -> None:
    async def go() -> None:
        rig = Rig(tmp_path)
        async with rig as r:
            stopped: list[bool] = []

            async def restart() -> None:                                       # as lite.py may: stop, then exit 75
                await r.local.stop()
                await asyncio.sleep(0.01)                                     # more to close after the doors
                stopped.append(True)

            r.ctx.restart = restart
            mac = f"eve_mac={await r.login()}"
            start = await r.mac("GET", lite_web.OPENROUTER_START_PATH, cookie=mac)
            state = parse_qs(urlsplit(start.headers["location"]).query)["state"][0]
            back = await r.mac("GET", f"{lite_web.OPENROUTER_CALLBACK_PATH}?code=c&state={state}")
            assert back.status == 303
            for _ in range(500):
                if stopped:
                    break
                await asyncio.sleep(0.01)
            assert stopped == [True] and not r.local.running              # the restart ran to its end
            r.port = await r.local.start()                                  # for the rig's own stop

    run(go())


def test_a_composio_sign_in_that_cannot_start_says_why_on_a_page_and_in_setup(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path, register_status=400) as r:
            mac = f"eve_mac={await r.login()}"
            reply = await r.mac("GET", lite_web.COMPOSIO_START_PATH, cookie=mac)
            line = onboarding.COMPOSIO_NOT_REGISTERED.format(status=400)
            assert reply.status == 502 and reply.headers["content-type"] == "text/html; charset=utf-8"
            assert html.escape(line).encode() in reply.body and "location" not in reply.headers
            assert r.provider_calls == [onboarding.CONNECT_REGISTER_URL]
            assert (await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)).json()["error"] == line
            r.ctx.openrouter = None
            gone = await r.mac("GET", lite_web.OPENROUTER_START_PATH, cookie=mac)
            assert gone.status == 503 and html.escape(lite_web.NO_SIGNIN).encode() in gone.body

    run(go())


def test_turning_the_lan_door_off_says_so_when_web_json_cannot_keep_it(tmp_path: Path,
                                                                         monkeypatch: pytest.MonkeyPatch) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            mac = f"eve_mac={await r.login()}"
            await r.open_pairing(mac.split("=", 1)[1])

            def full_disk(path: Any, data: Any) -> None:
                raise OSError(28, "No space left on device")

            monkeypatch.setattr(lite_web, "write_private_json", full_disk)
            off = await r.mac("POST", lite_web.LAN_PATH, {"on": False}, cookie=mac)
            assert off.status == 500 and off.json()["error"] == lite_web.NOT_SAVED
            assert not r.local.lan_on()                                       # off for now, as asked
            assert r.ctx.shared.saved.lan is True                             # and the page was told it will not last
            assert json.loads((tmp_path / "web.json").read_text())["lan"] is True
            skip = await r.mac("POST", lite_web.COMPOSIO_SKIP_PATH, {}, cookie=mac)
            assert skip.status == 500 and r.ctx.shared.saved.composio_skipped is False

    run(go())
