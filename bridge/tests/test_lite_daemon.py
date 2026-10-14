"""lite.py: eve lite's daemon (docs/plans/eve-lite.md M1, WP-9), in this process and without its firewall (the
firewall's own boot is test_lite_boot_firewall.py, in a subprocess).

A real LiteDaemon runs on a temporary home under eve lite's environment (profile.apply), with its Mac page on an
ephemeral loopback port, the LAN door on loopback when a test opens it, and its IPC socket in a short temporary folder.
The parts are the real ones; only what would leave the Mac is replaced: the spend figures' loop, the Composio bridge,
the OpenRouter credit read and sign-in exchange (httpx MockTransport), the Local Network alert and probe, and the Mac's
name and address. A guard makes any other host name lookup fail, and every test asserts none was made.

* With a key (READY), only the lite parts are built, each once: the brain lent nothing of the Mac, one LinkChat whose
  uploads are the brain's, PhoneCalls that take no Telegram owner, one RtcHub, the page on loopback, no LAN door.
* Without one (NEEDS_OPENROUTER), the setup page and the IPC alone.
* A signed-in Composio builds the bridge and starts it.
* The IPC answers the M11 contract on a 0600 socket, never raises, and logs no code.
* The LAN door is off until enabled, remembered in web.json, and opens again at the next start.
* A sign-in restarts eve: graceful stop, exit 75, the key in the env file (0600) and in no log line.
* The stop runs in order, and closes a conversation the brain left open.
* The process: full eve's LaunchAgent keeps eve lite down (exit 0), another profile exits 78, a socket in use exits 2,
  and eve lite's module never imports full eve's daemon (with a positive control).
"""

from __future__ import annotations

import asyncio
import contextlib
import ipaddress
import json
import logging
import os
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import textwrap
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any, AsyncIterator, Iterator, Optional
from urllib.parse import parse_qs, urlsplit

import httpx
import pytest
from test_profile_classified import all_modules, closure

from eve_bridge import (
    composio_connect,
    ipc,
    link_chat,
    lite,
    lite_rtc,
    lite_web,
    memory,
    onboarding,
    phone_call,
    profile,
    spend_sync,
    telegram,
    web_auth,
)

SRC = Path(__file__).resolve().parents[1] / "src"
CANARY_KEY = "CANARY-NOT-A-KEY-7f3c-openrouter"
INDEX = b"<!doctype html><title>eve</title><div id=root></div><!-- the test page -->"
MAC_SAFARI = ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) "
              "Version/18.0 Safari/605.1.15")
HOSTNAME, LAN_IP = "Test-Mac", "192.168.50.7"
LITE_PATH = "/usr/bin:/bin:/usr/sbin:/sbin"                  # the lite LaunchAgent's PATH (spec M8)
BANNED = ("bleak", "eve_bridge.ble", "eve_bridge.daemon", "sounddevice", "Quartz")
LOCAL_NAMES = (None, "localhost", "127.0.0.1", "::1")
EXPECTED_LENT = {"memory", "chat_log", "watcher", "apps", "thinker", "brief", "agent_factory", "agent_enabled",
                 "screen", "launcher", "launch_recent", "claude_sessions", "codex_folders"}


# ---- stand-ins for what would leave the Mac ----------------------------------------------------------------------

class SpendLoop:
    """spend_sync.loop, which would read OpenRouter's figures: counts its starts and waits for the shutdown."""

    def __init__(self) -> None:
        self.started = 0

    async def loop(self, shutdown: asyncio.Event, *args: Any, **kwargs: Any) -> None:
        self.started += 1
        await shutdown.wait()


class FakeBridge:
    """composio_connect.ConnectBridge as the daemon and the brain use it: built, started, closed."""

    made: list["FakeBridge"] = []
    names: frozenset[str] = frozenset()

    def __init__(self, token_source: Any, **kwargs: Any) -> None:
        self.token_source = token_source
        self.starts = 0
        self.closed = False
        self.needs_sign_in = False
        FakeBridge.made.append(self)

    @property
    def started(self) -> bool:
        return self.starts > 0

    def tools(self) -> list[dict[str, Any]]:
        return []

    def accounts_note(self) -> str:
        return ""

    def start(self) -> None:
        self.starts += 1

    def close(self) -> None:
        self.closed = True

    def connections(self, toolkits: Any = (), **kwargs: Any) -> dict[str, bool]:
        return {}

    def connect_link(self, toolkit: str) -> Optional[str]:
        return None


class Offline:
    """A host name lookup for anything but this Mac fails, from any thread, and is remembered with the thread that
    asked. The tests assert none came from the daemon: its loop's thread or the loop's worker threads (asyncio names
    them asyncio_<n>, and asyncio.run joins them before it returns, so none is another test's)."""

    def __init__(self) -> None:
        self.asked: list[tuple[str, str]] = []
        self.real = socket.getaddrinfo

    def __call__(self, host: Any, *args: Any, **kwargs: Any) -> Any:
        name = host.decode() if isinstance(host, bytes) else host
        if name in LOCAL_NAMES:
            return self.real(host, *args, **kwargs)
        self.asked.append((threading.current_thread().name, str(name)))
        raise socket.gaierror(socket.EAI_NONAME, "no network in this test")

    def by_the_daemon(self) -> list[str]:
        return [host for thread, host in self.asked if thread == "MainThread" or thread.startswith("asyncio_")]


@dataclass
class Stubs:
    spend: SpendLoop
    offline: Offline
    alerts: list[Any]
    probes: list[Any]


@pytest.fixture(autouse=True)
def stubs(monkeypatch: pytest.MonkeyPatch) -> Iterator[Stubs]:
    spend, offline = SpendLoop(), Offline()
    monkeypatch.setattr(spend_sync, "loop", spend.loop)
    monkeypatch.setattr(composio_connect, "ConnectBridge", FakeBridge)
    monkeypatch.setattr(socket, "getaddrinfo", offline)
    FakeBridge.made = []
    yield Stubs(spend, offline, [], [])
    assert offline.by_the_daemon() == [], f"the daemon looked up a host on the network: {offline.by_the_daemon()}"


@pytest.fixture
def home(tmp_path: Path) -> Iterator[Path]:
    """eve lite's environment as profile.apply makes it, on a temporary home with a built web page. Restored after
    (apply edits os.environ itself, and the daemon hands its key to telegram.hide_token)."""
    saved = dict(os.environ)
    home = tmp_path / "home"
    home.mkdir()
    web = tmp_path / "web"
    (web / "assets").mkdir(parents=True)
    (web / "index.html").write_bytes(INDEX)
    for name in [n for n in os.environ if n.startswith("EVE_") or n == "OPENROUTER_API_KEY"]:
        del os.environ[name]                       # nothing from the shell running the tests
    os.environ.update({"HOME": str(home), "EVE_PROFILE": "lite", "EVE_WEB_DIR": str(web)})
    profile.apply(os.environ)
    try:
        yield home
    finally:
        os.environ.clear()
        os.environ.update(saved)
        telegram.unhide_tokens()


@pytest.fixture
def sock(tmp_path: Path) -> Iterator[Path]:
    """The IPC socket's path, in a short folder: macOS keeps a socket's path in 104 bytes, and tmp_path is longer."""
    base = tempfile.gettempdir()
    folder = Path(tempfile.mkdtemp(prefix="evl-", dir=base if len(base) <= 60 else "/tmp"))
    try:
        yield folder / "ipc.sock"
    finally:
        shutil.rmtree(folder, ignore_errors=True)


def keyed() -> None:
    os.environ["OPENROUTER_API_KEY"] = CANARY_KEY


def signed_in_to_composio(home: Path) -> Path:
    path = home / ".config" / "eve-bridge" / "composio-connect.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"version": 1, "client_id": "eve-lite-test-client", "access_token": "placeholder",
                                "refresh_token": "placeholder", "expires_at": 4102444800}))
    path.chmod(0o600)
    return path


def counting(monkeypatch: pytest.MonkeyPatch, module: Any, name: str) -> list[tuple[Any, ...]]:
    """Every object ``module.name`` makes from now on, by its arguments (a subclass that records, then builds)."""
    real = getattr(module, name)
    made: list[tuple[Any, ...]] = []

    class Counted(real):  # type: ignore[misc, valid-type]
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            made.append((args, kwargs))
            super().__init__(*args, **kwargs)

    Counted.__name__ = Counted.__qualname__ = real.__name__
    monkeypatch.setattr(module, name, Counted)
    return made


# ---- running a daemon in the test's loop ------------------------------------------------------------------------

async def _credit(key: str) -> dict[str, Any]:
    return {"ok": True, "usd": 4.25, "funded": True}


def web_options(stubs: Stubs, **extra: Any) -> dict[str, Any]:
    """WebContext's keywords for a test: ephemeral ports, the LAN door on loopback, and macOS and the network replaced.
    The page keeps its own hosts (loopback), so a test sees where it really listens."""
    return {"local_ports": (0,), "lan_ports": (0,), "lan_hosts": ("127.0.0.1",), "hostname": lambda: HOSTNAME,
            "addresses": lambda: [LAN_IP], "alert": lambda: stubs.alerts.append(1),
            "probe": lambda peer: stubs.probes.append(peer) or "ok", "credit": _credit, "restart_delay": 0.0, **extra}


@contextlib.asynccontextmanager
async def running(sock: Path, stubs: Stubs, **kwargs: Any) -> AsyncIterator[tuple[lite.LiteDaemon, asyncio.Task]]:
    """A LiteDaemon serving until the block ends (then shut down): the daemon and its run task, whose result is the
    exit code."""
    ready = asyncio.Event()
    daemon = lite.LiteDaemon(socket_path=str(sock), web_options=web_options(stubs, **kwargs.pop("web", {})),
                             on_ready=lambda d: ready.set(), **kwargs)
    task = asyncio.create_task(daemon.run())
    waiter = asyncio.create_task(ready.wait())
    done, _ = await asyncio.wait({task, waiter}, timeout=60, return_when=asyncio.FIRST_COMPLETED)
    if waiter not in done:
        waiter.cancel()
        if task.done():
            task.result()                          # raises what stopped it
        raise AssertionError("the daemon was not ready in time")
    try:
        yield daemon, task
    finally:
        daemon.shutdown()
        with contextlib.suppress(Exception):
            await asyncio.wait_for(asyncio.shield(task), 60)


async def ask(sock: Path, req: Any) -> dict[str, Any]:
    """One IPC request, as `eve-bridge web` sends it: one JSON line, one back."""
    reader, writer = await asyncio.open_unix_connection(str(sock))
    try:
        writer.write((req if isinstance(req, bytes) else json.dumps(req).encode()) + b"\n")
        await writer.drain()
        return json.loads(await asyncio.wait_for(reader.readline(), 20))
    finally:
        writer.close()
        with contextlib.suppress(Exception):
            await writer.wait_closed()


@dataclass
class Reply:
    status: int
    headers: dict[str, str]
    body: bytes
    cookies: list[str]

    def json(self) -> Any:
        return json.loads(self.body)

    def cookie(self, name: str) -> str:
        for line in self.cookies:
            pair = line.split(";", 1)[0]
            if pair.startswith(name + "="):
                return pair
        raise AssertionError(f"no {name} cookie in {self.cookies}")


async def http(port: int, method: str, path: str, body: Any = None, *, cookie: str = "") -> Reply:
    """One request to the Mac's page, as its own browser sends it (Host, and Origin on a POST)."""
    host = f"localhost:{port}"
    data = json.dumps(body).encode() if body is not None else b""
    lines = [f"{method} {path} HTTP/1.1", f"Host: {host}", f"User-Agent: {MAC_SAFARI}"]
    if method != "GET":
        lines += [f"Origin: http://{host}", "Content-Type: application/json", f"Content-Length: {len(data)}"]
    if cookie:
        lines.append(f"Cookie: {cookie}")
    reader, writer = await asyncio.open_connection("127.0.0.1", port)
    try:
        writer.write(("\r\n".join(lines) + "\r\n\r\n").encode() + data)
        await writer.drain()
        raw = await asyncio.wait_for(reader.read(), 20)
    finally:
        writer.close()
    head, _, payload = raw.partition(b"\r\n\r\n")
    head_lines = head.decode("latin-1").split("\r\n")
    headers: dict[str, str] = {}
    cookies: list[str] = []
    for line in head_lines[1:]:
        name, _, value = line.partition(":")
        if name.strip().lower() == "set-cookie":
            cookies.append(value.strip())
        headers[name.strip().lower()] = value.strip()
    return Reply(int(head_lines[0].split(" ")[1]), headers, payload, cookies)


async def mac_sign_in(sock: Path, port: int) -> str:
    """The Mac's browser signs in as `eve-bridge web open` has it: a code over the IPC, then the page. Its cookie."""
    login = await ask(sock, {"evt": "web_login"})
    assert login["ok"] is True and login["port"] == port
    reply = await http(port, "POST", lite_web.MAC_LOGIN_PATH, {"code": login["code"]})
    assert reply.status == 200, reply.body
    return reply.cookie(web_auth.COOKIE_MAC)


async def until(pred: Any, secs: float = 10.0) -> None:
    for _ in range(int(secs / 0.01)):
        if pred():
            return
        await asyncio.sleep(0.01)
    raise AssertionError("timed out waiting")


def test_the_network_guard_catches_a_lookup(stubs: Stubs) -> None:
    """Positive control for the guard every test here ends on: a lookup from the loop's thread or one of its workers
    fails and is attributed to the daemon; one from an unrelated thread fails too, and is not."""
    async def go() -> None:
        with pytest.raises(socket.gaierror):
            socket.getaddrinfo("openrouter.ai", 443)
        with pytest.raises(socket.gaierror):
            await asyncio.to_thread(socket.getaddrinfo, "connect.composio.dev", 443)

    asyncio.run(go())
    other = threading.Thread(target=_lookup_quietly, args=("example.com",), name="another-test")
    other.start()
    other.join()
    assert stubs.offline.by_the_daemon() == ["openrouter.ai", "connect.composio.dev"]
    assert ("another-test", "example.com") in stubs.offline.asked
    assert socket.getaddrinfo("127.0.0.1", 80)                     # this Mac still resolves
    stubs.offline.asked.clear()                                    # so the fixture's own check passes


def _lookup_quietly(host: str) -> None:
    with contextlib.suppress(socket.gaierror):
        socket.getaddrinfo(host, 443)


# ---- READY: only the lite parts --------------------------------------------------------------------------------

def test_with_a_key_it_builds_only_the_lite_parts_each_once(home: Path, sock: Path, stubs: Stubs,
                                                            monkeypatch: pytest.MonkeyPatch) -> None:
    keyed()
    made = {name: counting(monkeypatch, module, name) for module, name in (
        (link_chat, "LinkChat"), (phone_call, "PhoneCalls"), (lite_rtc, "RtcHub"), (lite_web, "LocalServer"),
        (lite_web, "LanServer"))}
    lent: dict[str, Any] = {}
    real_make_inlet = telegram.make_inlet

    def make_inlet(config: telegram.TelegramConfig, **kwargs: Any) -> Any:
        lent.update(kwargs, config=config)
        return real_make_inlet(config, **kwargs)

    monkeypatch.setattr(telegram, "make_inlet", make_inlet)

    async def go() -> int:
        async with running(sock, stubs) as (d, task):
            assert d.state == lite.READY
            # the brain: app-only, eve lite's one chat, lent memory, the chat log, the watcher and think_hard
            config = lent.pop("config")
            assert config.enabled and config.app_only and config.token == ""
            assert config.owner_ids == frozenset({profile.LITE_CHAT_ID})
            assert set(lent) == EXPECTED_LENT
            assert isinstance(d.memory, memory.Memory) and lent["memory"] is d.memory
            assert d.chat_log is not None and lent["chat_log"] is d.chat_log and d.memory.chat_log is d.chat_log
            assert d.watcher is not None and lent["watcher"] is d.watcher and d.watcher.notify == d.inlet.tell_owner
            assert lent["apps"] is None and d.apps is None and FakeBridge.made == []
            assert lent["thinker"] is not None and isinstance(lent["brief"](), str)
            # and none of the Mac
            assert lent["agent_factory"] is None and lent["agent_enabled"] is False
            assert lent["screen"]() is None
            assert lent["launch_recent"]() == [] and lent["claude_sessions"]() == [] and lent["codex_folders"]() == []
            assert await lent["launcher"](Path("/tmp/project"), "claude") == telegram.LITE_NOT_HERE
            assert isinstance(d.inlet, telegram.TelegramInlet) and isinstance(d.inlet.api, telegram.MutedBotApi)
            # one chat door, its uploads the brain's; calls on the same brain that take no Telegram owner
            assert len(made["LinkChat"]) == 1 and d.link._inlet() is d.inlet and d.link._work() is None
            assert d.inlet.link_uploads is d.link.uploads
            assert len(made["PhoneCalls"]) == 1 and d.calls.brain is d.inlet and d.calls.voice is not None
            assert d.calls.check_owner("user=%7B%22id%22%3A1%7D&hash=0") is None
            assert len(made["RtcHub"]) == 1 and d.rtc.link is d.link and d.rtc.calls is d.calls
            assert d.rtc.voice_ready() and d.rtc.stats()["peers"] == 0
            # the page, on loopback only; no LAN door
            assert len(made["LocalServer"]) == 1 and d.local.running and made["LanServer"] == []
            assert d.web.shared.lan is None
            assert d.local.bound and all(ipaddress.ip_address(addr).is_loopback for addr, _ in d.local.bound)
            assert (d.web.link, d.web.rtc, d.web.apps) == (d.link, d.rtc, None)
            port = d.local.port
            assert (await http(port, "GET", lite_web.HEALTH_PATH)).json() == {"ok": True, "state": "READY"}
            index = await http(port, "GET", "/")
            assert index.status == 200 and index.body == INDEX
            # beside them: the brain, the watcher, the spend figures and the dream
            assert sorted(t.get_name() for t in d._tasks if not t.done()) == ["lite-brain", "lite-spend-sync",
                                                                             "lite-watch"]
            assert stubs.spend.started == 1
            assert [t.get_name() for t in d._background] == ["memory-dream"]
            # the IPC socket, 0600
            info = os.stat(sock)
            assert stat.S_ISSOCK(info.st_mode) and stat.S_IMODE(info.st_mode) == 0o600
            assert stubs.alerts == [] and stubs.probes == []    # nothing asked macOS about the network
        code = await task
        # stopped: the page, the peers, the brain's goodbye, the socket
        assert not d.local.running and d.rtc.closed and d.inlet._stopping
        assert all(t.done() for t in d._tasks) and not sock.exists()
        return code

    assert asyncio.run(go()) == lite.EXIT_OK


def test_without_a_key_it_is_the_setup_page_and_the_ipc_only(home: Path, sock: Path, stubs: Stubs,
                                                             monkeypatch: pytest.MonkeyPatch) -> None:
    built: list[str] = []
    monkeypatch.setattr(telegram, "make_inlet", lambda *a, **k: built.append("brain"))
    monkeypatch.setattr(lite, "build_memory", lambda cfg: built.append("memory"))

    async def go() -> int:
        async with running(sock, stubs) as (d, task):
            assert d.state == lite.NEEDS_OPENROUTER
            assert (d.memory, d.chat_log, d.watcher, d.apps, d.inlet, d.link, d.calls, d.rtc) == (None,) * 8
            assert d.web.link is None and d.web.rtc is None and d.web.apps is None
            assert d._tasks == [] and d._background == set()
            port = d.local.port
            assert (await http(port, "GET", lite_web.HEALTH_PATH)).json() == {"ok": True, "state": "NEEDS_OPENROUTER"}
            status = await ask(sock, {"evt": "status"})
            assert status["state"] == "NEEDS_OPENROUTER" and status["openrouter"] is False and status["page"] is True
            assert status["voice"] is False and status["apps"] is False and status["chat"] is False
            assert status["port"] == port
            cookie = await mac_sign_in(sock, port)
            onboard = await http(port, "GET", lite_web.ONBOARD_STATE_PATH, cookie=cookie)
            assert onboard.status == 200 and onboard.json()["openrouter"] is False
            # the chat door answers that eve is not set up, rather than a chat with no brain
            turn = await http(port, "POST", "/api/web/link/turn", {"text": "hi"}, cookie=cookie)
            assert turn.status == 503 and turn.json()["error"] == lite_web.NOT_READY
        return await task

    assert asyncio.run(go()) == lite.EXIT_OK
    assert built == [] and stubs.spend.started == 0


def test_a_composio_sign_in_builds_the_bridge_and_starts_it(home: Path, sock: Path, stubs: Stubs,
                                                             monkeypatch: pytest.MonkeyPatch) -> None:
    keyed()
    token_file = signed_in_to_composio(home)
    lent: dict[str, Any] = {}
    real_make_inlet = telegram.make_inlet
    monkeypatch.setattr(telegram, "make_inlet", lambda config, **kw: lent.update(kw) or real_make_inlet(config, **kw))

    async def go() -> lite.LiteDaemon:
        async with running(sock, stubs) as (d, task):
            assert len(FakeBridge.made) == 1
            bridge = FakeBridge.made[0]
            assert d.apps is bridge and lent["apps"] is bridge and d.web.apps is bridge
            assert isinstance(bridge.token_source, composio_connect.ConnectTokenSource)
            assert bridge.token_source.path == token_file
            assert bridge.starts == 1
            status = await ask(sock, {"evt": "status"})
            assert status["composio"] is True and status["apps"] is True
            bridge.needs_sign_in = True                # only a new sign-in can help now: Settings says so
            assert (await ask(sock, {"evt": "status"}))["composio"] is False
        assert await task == lite.EXIT_OK
        return d

    d = asyncio.run(go())
    assert d.apps is not None and d.apps.closed


# ---- the IPC ---------------------------------------------------------------------------------------------------

def test_the_ipc_answers_the_contract_on_a_private_socket(home: Path, sock: Path, stubs: Stubs,
                                                          caplog: pytest.LogCaptureFixture) -> None:
    keyed()
    caplog.set_level(logging.INFO)

    async def go() -> None:
        async with running(sock, stubs) as (d, _):
            assert stat.S_IMODE(os.stat(sock).st_mode) == 0o600
            port = d.local.port
            # web_login: a one-time code and the page's port; web_login_used turns true once a browser used it
            login = await ask(sock, {"evt": "web_login"})
            assert set(login) == {"ok", "code", "port"} and login["ok"] is True and login["port"] == port
            assert web_auth.TOKEN.fullmatch(login["code"])
            assert await ask(sock, {"evt": "web_login_used", "code": login["code"]}) == {"ok": True, "used": False}
            signed = await http(port, "POST", lite_web.MAC_LOGIN_PATH, {"code": login["code"]})
            assert signed.status == 200
            cookie = signed.cookie(web_auth.COOKIE_MAC)
            assert await ask(sock, {"evt": "web_login_used", "code": login["code"]}) == {"ok": True, "used": True}
            again = await http(port, "POST", lite_web.MAC_LOGIN_PATH, {"code": login["code"]})
            assert again.status == 403                                     # single use
            assert (await ask(sock, {"evt": "web_login_used", "code": "nope"}))["used"] is False
            # status: words, booleans, ports, the firewall's counts; never the key or a code
            status = await ask(sock, {"evt": "status"})
            assert status == {"ok": True, "state": "READY", "openrouter": True, "openrouter_rejected": False,
                              "chat": True, "composio": False, "apps": False, "voice": True, "page": True,
                              "port": port, "lan": False, "lan_port": 0, "local_network": "unknown",
                              "restarting": False, "firewall": {}}
            # devices: the Mac's browser; revoke by id signs it out
            devices = await ask(sock, {"evt": "devices"})
            assert devices["ok"] is True and [row["label"] for row in devices["devices"]] == ["This Mac · Safari"]
            assert set(devices["devices"][0]) == {"id", "label", "host", "created", "last_seen"}
            mac_id = devices["devices"][0]["id"]
            assert (await http(port, "GET", lite_web.STATUS_PATH, cookie=cookie)).status == 200
            assert await ask(sock, {"evt": "revoke", "id": mac_id.upper()}) == {"ok": True, "revoked": [mac_id]}
            assert (await http(port, "GET", lite_web.STATUS_PATH, cookie=cookie)).status == 401
            assert await ask(sock, {"evt": "revoke", "id": "all"}) == {"ok": True, "revoked": []}
            # refusals: one plain line each, and the daemon goes on
            assert await ask(sock, {"evt": "revoke", "id": "../etc"}) == {"ok": False, "error": lite_web.DEVICE_ID}
            assert await ask(sock, {"evt": "revoke"}) == {"ok": False, "error": lite_web.DEVICE_ID}
            assert await ask(sock, {"evt": "lan", "on": "yes"}) == {"ok": False, "error": lite_web.LAN_FLAG}
            assert await ask(sock, {"evt": "pretooluse", "tool_name": "Bash"}) == {"ok": False,
                                                                                "error": lite.UNKNOWN_EVENT}
            assert await ask(sock, ["web_login"]) == {"ok": False, "error": lite.UNKNOWN_EVENT}
            assert (await ask(sock, b"{not json"))["ok"] is False
            assert (await ask(sock, {"evt": "status"}))["state"] == "READY"
            text = json.dumps([login, status, devices])
            assert CANARY_KEY not in text
            assert login["code"] not in caplog.text                        # the code is in no log line
            assert CANARY_KEY not in caplog.text

    asyncio.run(go())


def test_an_ipc_request_that_fails_is_a_plain_line_and_logs_its_type_only(home: Path, sock: Path, stubs: Stubs,
                                                                          caplog: pytest.LogCaptureFixture,
                                                                          monkeypatch: pytest.MonkeyPatch) -> None:
    def broken() -> list[Any]:
        raise ValueError("a device list that holds CANARY-NOT-A-KEY-7f3c")

    async def go() -> None:
        async with running(sock, stubs) as (d, _):
            monkeypatch.setattr(d.devices, "list", broken)
            answer = await ask(sock, {"evt": "devices", "code": "CANARY-NOT-A-KEY-7f3c-code"})
            assert answer == {"ok": False, "error": lite_web.FAILED}
            assert (await ask(sock, {"evt": "status"}))["ok"] is True       # and it goes on

    caplog.set_level(logging.INFO)
    asyncio.run(go())
    assert "an IPC request failed (ValueError)" in caplog.text
    assert "CANARY" not in caplog.text


def test_the_lan_door_is_off_until_enabled_and_remembered(home: Path, sock: Path, stubs: Stubs,
                                                          monkeypatch: pytest.MonkeyPatch) -> None:
    keyed()
    lan_made = counting(monkeypatch, lite_web, "LanServer")
    web_json = home / ".config" / "eve-bridge" / "web.json"

    async def first() -> None:
        async with running(sock, stubs) as (d, _):
            assert d.web.shared.lan is None and lan_made == []
            assert (await ask(sock, {"evt": "status"}))["lan"] is False
            assert await ask(sock, {"evt": "lan", "on": True}) == {"ok": True, "lan": True}
            status = await ask(sock, {"evt": "status"})
            assert status["lan"] is True and status["lan_port"] > 0 and status["lan_port"] != status["port"]
            assert len(lan_made) == 1
            assert all(addr == "127.0.0.1" for addr, _ in d.web.shared.lan.bound)   # the test's lan_hosts
            assert json.loads(web_json.read_text())["lan"] is True
            assert stat.S_IMODE(os.stat(web_json).st_mode) == 0o600

    async def second() -> None:
        async with running(sock, stubs) as (d, _):
            # remembered: the next start opens it again by itself
            assert d.web.shared.lan is not None and d.web.shared.lan.running
            assert (await ask(sock, {"evt": "status"}))["lan"] is True
            assert await ask(sock, {"evt": "lan", "on": False}) == {"ok": True, "lan": False}
            assert not d.web.shared.lan.running and json.loads(web_json.read_text())["lan"] is False

    async def third() -> None:
        async with running(sock, stubs) as (d, _):
            assert d.web.shared.lan is None

    asyncio.run(first())
    asyncio.run(second())
    asyncio.run(third())
    assert stubs.alerts == []                       # turning the door on is not "Add a phone": no Local Network alert


def test_the_page_comes_up_once_its_port_is_free(home: Path, sock: Path, stubs: Stubs,
                                                 monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(lite, "PAGE_RETRY_SECS", (0.05,))
    holder = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    holder.bind(("127.0.0.1", 0))
    holder.listen(1)
    taken = holder.getsockname()[1]

    async def go() -> None:
        async with running(sock, stubs, web={"local_hosts": ("127.0.0.1",), "local_ports": (taken,)}) as (d, _):
            assert not d.local.running
            assert await ask(sock, {"evt": "web_login"}) == {"ok": False, "error": lite.PAGE_DOWN}
            assert (await ask(sock, {"evt": "status"}))["page"] is False
            holder.close()
            await until(lambda: d.local.running)
            login = await ask(sock, {"evt": "web_login"})
            assert login["ok"] is True and login["port"] == taken

    try:
        asyncio.run(go())
    finally:
        holder.close()


def test_a_stop_during_a_late_page_start_leaves_no_page_open(home: Path, sock: Path, stubs: Stubs,
                                                             monkeypatch: pytest.MonkeyPatch) -> None:
    """The retry's start is under way when eve stops: it is let finish, then the page is stopped with the rest, so no
    socket opens after the stop. Regression: the page was stopped first, the retry bound its port while the next
    step of the stop ran, and the page stayed open behind the stop. Here the bind is late and that next step slow,
    so the old order leaves it open every time."""
    keyed()                                            # READY: the stop's next step, closing the WebRTC peers, exists
    monkeypatch.setattr(lite, "PAGE_RETRY_SECS", (0.05,))
    holder = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    holder.bind(("127.0.0.1", 0))
    holder.listen(1)
    taken = holder.getsockname()[1]
    real_bind, real_close = lite_web.LocalServer._bind, lite_rtc.RtcHub.close
    binding = asyncio.Event()

    async def late_bind(self: lite_web.LocalServer, hosts: Any, ports: Any) -> Any:
        binding.set()
        await asyncio.sleep(0.2)                       # the stop lands here, before the page has its sockets
        return await real_bind(self, hosts, ports)

    async def slow_close(self: lite_rtc.RtcHub) -> None:
        await asyncio.sleep(0.5)                       # the step after the page's: the late bind finishes meanwhile
        await real_close(self)

    async def go() -> lite.LiteDaemon:
        async with running(sock, stubs, web={"local_hosts": ("127.0.0.1",), "local_ports": (taken,)}) as (d, task):
            monkeypatch.setattr(lite_web.LocalServer, "_bind", late_bind)
            monkeypatch.setattr(lite_rtc.RtcHub, "close", slow_close)
            holder.close()
            await asyncio.wait_for(binding.wait(), 10)
            d.shutdown()
            assert await asyncio.wait_for(asyncio.shield(task), 30) == lite.EXIT_OK
        return d

    try:
        d = asyncio.run(go())
    finally:
        holder.close()
    assert not d.local.running and d.local.bound == []


# ---- restarts and stopping ------------------------------------------------------------------------------------

def test_an_openrouter_sign_in_restarts_eve_into_its_key(home: Path, sock: Path, stubs: Stubs,
                                                         caplog: pytest.LogCaptureFixture) -> None:
    """The setup page's sign-in, end to end on the real routes: the key is exchanged (MockTransport), saved, and eve
    stops gracefully and exits 75 for launchd to start it again into READY."""
    caplog.set_level(logging.INFO)
    exchanged: list[str] = []

    def provider(request: httpx.Request) -> httpx.Response:
        exchanged.append(str(request.url))
        if str(request.url) == onboarding.OPENROUTER_KEYS_URL:
            return httpx.Response(200, json={"key": CANARY_KEY})
        return httpx.Response(404)

    async def go() -> int:
        client = httpx.AsyncClient(transport=httpx.MockTransport(provider))
        try:
            async with running(sock, stubs, openrouter_login=onboarding.OpenRouterLogin(http=client)) as (d, task):
                assert d.state == lite.NEEDS_OPENROUTER
                port = d.local.port
                cookie = await mac_sign_in(sock, port)
                start = await http(port, "GET", lite_web.OPENROUTER_START_PATH, cookie=cookie)
                assert start.status == 302 and start.headers["location"].startswith(onboarding.OPENROUTER_AUTH_URL)
                state = parse_qs(urlsplit(start.headers["location"]).query)["state"][0]
                back = await http(port, "GET", f"{lite_web.OPENROUTER_CALLBACK_PATH}?code=code-1234&state={state}")
                assert back.status == 303 and back.headers["location"] == "/"
                code = await asyncio.wait_for(asyncio.shield(task), 30)       # it stops by itself
                assert not d.local.running and not sock.exists()
                return code
        finally:
            await client.aclose()

    assert asyncio.run(go()) == lite.EXIT_RESTART
    assert exchanged == [onboarding.OPENROUTER_KEYS_URL]
    env_file = home / ".config" / "eve-bridge" / "env"
    assert f"OPENROUTER_API_KEY={CANARY_KEY}" in env_file.read_text().splitlines()
    assert stat.S_IMODE(os.stat(env_file).st_mode) == 0o600
    assert "restarts into it (exit 75)" in caplog.text
    assert CANARY_KEY not in caplog.text


def test_a_restart_asked_twice_is_one_restart(home: Path, caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.INFO)
    d = lite.LiteDaemon(socket_path="/nonexistent/ipc.sock")
    d.restart()
    d.restart()
    assert d.exit_code == lite.EXIT_RESTART and d.stopping
    assert caplog.text.count("restarts into it") == 1
    d.shutdown()                                     # a stop after a restart is still the restart
    assert d.exit_code == lite.EXIT_RESTART


def test_the_stop_runs_in_order_and_closes_an_open_conversation(home: Path, sock: Path, stubs: Stubs,
                                                                monkeypatch: pytest.MonkeyPatch) -> None:
    keyed()
    signed_in_to_composio(home)
    order: list[str] = []

    def spy(name: str, real: Any) -> Any:
        async def wrapped(*args: Any, **kwargs: Any) -> Any:
            order.append(name)
            return await real(*args, **kwargs)
        return wrapped

    for cls, method, name in ((lite_web.LocalServer, "stop", "page"), (lite_rtc.RtcHub, "close", "peers"),
                              (telegram.TelegramInlet, "_shutdown", "brain"),
                              (lite.LiteDaemon, "_close_open_conversation", "conversation"),
                              (lite.LiteDaemon, "_drain_background", "dream"), (ipc.IPCServer, "stop", "ipc")):
        monkeypatch.setattr(cls, method, spy(name, getattr(cls, method)))
    monkeypatch.setattr(FakeBridge, "close", lambda self: order.append("composio"))
    # the brain's own close marker does not run (a task cancelled before it could): the daemon's backstop closes it
    monkeypatch.setattr(telegram.TelegramInlet, "_close_chat", lambda self: None)

    async def go() -> tuple[lite.LiteDaemon, str]:
        async with running(sock, stubs) as (d, task):
            store = d.memory.transcripts
            conv = store.new_conv(telegram.CHANNEL)
            assert store.append(telegram.CHANNEL, conv, "owner", "say", "a line that was said")
            d.inlet._conv = conv
        assert await task == lite.EXIT_OK
        return d, conv

    d, conv = asyncio.run(go())
    assert order == ["page", "peers", "brain", "composio", "conversation", "dream", "ipc"]
    kinds = [line.get("kind") for line in d.memory.transcripts.conv_lines(conv)]
    assert kinds[-1] == "close" and kinds.count("close") == 1
    assert d.inlet._conv is None


def test_a_stop_from_another_thread_lands_on_the_loop(home: Path, sock: Path, stubs: Stubs) -> None:
    """shutdown from a thread that is not the loop's (a supervisor's): the run still ends, with exit 0."""
    keyed()

    async def go() -> int:
        async with running(sock, stubs) as (d, task):
            await asyncio.to_thread(d.shutdown)
            return await asyncio.wait_for(asyncio.shield(task), 30)

    assert asyncio.run(go()) == lite.EXIT_OK


def test_a_brain_or_voice_that_cannot_be_built_leaves_the_page_up(home: Path, sock: Path, stubs: Stubs,
                                                                  monkeypatch: pytest.MonkeyPatch,
                                                                  caplog: pytest.LogCaptureFixture) -> None:
    """A failure there would crash the daemon, and launchd would start it again every 5 s with no page to reach: it
    costs the chat and the calls instead, said by type."""
    keyed()

    def broken(*args: Any, **kwargs: Any) -> Any:
        raise RuntimeError("a part that holds CANARY-NOT-A-KEY-7f3c")

    monkeypatch.setattr(telegram, "make_inlet", broken)
    monkeypatch.setattr(phone_call, "make_voice", broken)
    caplog.set_level(logging.INFO)

    async def go() -> None:
        async with running(sock, stubs) as (d, _):
            assert d.state == lite.READY and d.inlet is None and d.link is not None
            assert d.calls.brain is None and d.calls.voice is None and not d.rtc.voice_ready()
            status = await ask(sock, {"evt": "status"})
            assert status["chat"] is False and status["voice"] is False and status["page"] is True
            cookie = await mac_sign_in(sock, d.local.port)
            turn = await http(d.local.port, "POST", "/api/web/link/turn", {"text": "hi"}, cookie=cookie)
            assert turn.status == 503 and turn.json()["error"] == link_chat.NO_CHAT
            assert sorted(t.get_name() for t in d._tasks if not t.done()) == ["lite-spend-sync"]

    asyncio.run(go())
    assert "the chat brain could not be built (RuntimeError)" in caplog.text
    assert "the voice could not be built (RuntimeError)" in caplog.text
    assert "CANARY" not in caplog.text


def test_memory_that_cannot_open_leaves_eve_ready_without_it(home: Path, sock: Path, stubs: Stubs,
                                                             monkeypatch: pytest.MonkeyPatch,
                                                             caplog: pytest.LogCaptureFixture) -> None:
    keyed()

    def broken(cfg: Any) -> Any:
        raise OSError("a disk that holds CANARY-NOT-A-KEY-7f3c")

    monkeypatch.setattr(lite, "build_memory", broken)
    caplog.set_level(logging.INFO)

    async def go() -> None:
        async with running(sock, stubs) as (d, _):
            assert d.state == lite.READY and d.memory is None and d.inlet is not None
            assert d._background == set()                                 # no memory: no dream

    asyncio.run(go())
    assert "memory: could not open (OSError)" in caplog.text and "CANARY" not in caplog.text


# ---- small parts ---------------------------------------------------------------------------------------------

def test_the_ipc_takes_a_socket_file_only(home: Path, caplog: pytest.LogCaptureFixture) -> None:
    default = Path(home) / ".eve-lite" / "ipc.sock"
    assert lite.ipc_path() == Path(os.environ["EVE_BRIDGE_SOCK"]) == default       # apply's default
    del os.environ["EVE_BRIDGE_SOCK"]
    assert lite.ipc_path() == default
    assert lite.ipc_path("/tmp/elsewhere.sock") == Path("/tmp/elsewhere.sock")
    caplog.set_level(logging.WARNING)
    for tcp in ("127.0.0.1:48765", ":48765", "0.0.0.0:48765"):
        assert lite.ipc_path(tcp) == default, tcp
    os.environ["EVE_BRIDGE_SOCK"] = "localhost:48765"
    assert lite.ipc_path() == default
    assert caplog.text.count("not a TCP port") == 4


def test_the_log_is_full_eves_with_urls_and_addresses_kept_out() -> None:
    names = ("httpx", "httpcore", "aioice", "aiortc")
    saved = {name: logging.getLogger(name).level for name in names}
    try:
        for name in names:
            logging.getLogger(name).setLevel(logging.NOTSET)
        lite.setup_logging("debug")
        assert all(logging.getLogger(name).level == logging.WARNING for name in names)
        assert set(names) == set(lite.QUIET_LOGGERS)
        lite.setup_logging("not-a-level")                            # a bad level is INFO, not a crash
    finally:
        for name, level in saved.items():
            logging.getLogger(name).setLevel(level)


def test_full_eves_launch_agent_is_found_by_its_label(home: Path) -> None:
    from eve_bridge import _service_launchd

    assert lite.FULL_LABEL == _service_launchd.LABEL
    assert not lite.full_eve_installed()
    plist = home / "Library" / "LaunchAgents" / f"{_service_launchd.LABEL}.plist"
    plist.parent.mkdir(parents=True)
    plist.write_bytes(b"<plist/>")
    assert lite.full_eve_installed()


# ---- the process (each in a fresh Python: run_daemon installs the firewall, which cannot be removed) -----------

PROCESS = '''
import json, os, subprocess, sys
from eve_bridge import lite
code = lite.run_daemon(None, web_options={"local_ports": (0,), "lan_ports": (0,)})
try:
    subprocess.run(["/usr/bin/true"], check=True)
    ran = True
except PermissionError:
    ran = False
print(json.dumps({"exit": code, "program_ran": ran}))
'''


def run_process(home: Path, sock: Path, *, profile_value: Optional[str] = "lite",
                script: str = PROCESS) -> tuple[dict[str, Any], str]:
    env = {"PATH": LITE_PATH, "HOME": str(home), "PYTHONPATH": str(SRC), "EVE_BRIDGE_SOCK": str(sock),
           "TMPDIR": tempfile.gettempdir()}
    if profile_value is not None:
        env["EVE_PROFILE"] = profile_value
    done = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True, env=env, timeout=120,
                          cwd=str(home))
    assert done.returncode == 0, done.stderr[-3000:]
    return json.loads(done.stdout.strip().splitlines()[-1]), done.stderr


def test_full_eve_installed_keeps_eve_lite_down(tmp_path: Path, sock: Path) -> None:
    home = tmp_path / "home"
    plist = home / "Library" / "LaunchAgents" / f"{lite.FULL_LABEL}.plist"
    plist.parent.mkdir(parents=True)
    plist.write_bytes(b"<plist/>")
    out, log = run_process(home, sock)
    assert out == {"exit": lite.EXIT_OK, "program_ran": False}           # exit 0: launchd leaves it down
    assert "full eve's LaunchAgent" in log and "eve lite stays down" in log
    assert not sock.exists() and not (home / ".config").exists()          # nothing started, nothing written


def test_another_profile_is_exit_78_with_no_firewall(tmp_path: Path, sock: Path) -> None:
    home = tmp_path / "home"
    home.mkdir()
    out, log = run_process(home, sock, profile_value=None)
    assert out == {"exit": lite.EXIT_CONFIG, "program_ran": True}         # full eve: no firewall was installed
    assert "EVE_PROFILE is not lite" in log
    out, log = run_process(home, sock, profile_value="mini")
    assert out == {"exit": lite.EXIT_CONFIG, "program_ran": True}
    assert not sock.exists()


def test_a_socket_in_use_is_exit_2(tmp_path: Path, sock: Path) -> None:
    home = tmp_path / "home"
    home.mkdir()
    other = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        other.bind(str(sock))
        other.listen(1)
        out, log = run_process(home, sock)
    finally:
        other.close()
    assert out["exit"] == lite.EXIT_IN_USE and "already listening" in log
    assert not (home / ".config" / "eve-bridge" / "web.json").exists()   # no page was started


IMPORTS = '''
import importlib, json, os, sys
BANNED = {banned!r}
import eve_bridge.lite as lite
lite.LiteDaemon(socket_path=os.environ["EVE_BRIDGE_SOCK"])
after_lite = sorted(m for m in BANNED if m in sys.modules)
importlib.import_module("eve_bridge.daemon")                  # positive control: full eve's daemon brings them in
after_daemon = sorted(m for m in BANNED if m in sys.modules)
print(json.dumps({{"lite": after_lite, "daemon": after_daemon}}))
'''


def test_eve_lite_never_imports_full_eves_daemon(tmp_path: Path, sock: Path) -> None:
    """At run time: eve lite's module and its daemon load none of BLE, full eve's daemon, the Mac's audio or Quartz
    (the boot under the firewall checks the same after a whole start). Positive control: importing full eve's daemon
    in the same process loads them, so the check sees them."""
    home = tmp_path / "home"
    home.mkdir()
    out, _ = run_process(home, sock, script=IMPORTS.format(banned=BANNED))
    assert out["lite"] == []
    assert {"eve_bridge.daemon", "eve_bridge.ble", "bleak"} <= set(out["daemon"])


def test_no_import_of_lite_reaches_full_eves_daemon() -> None:
    """In the source: everything lite.py imports, function-level imports included, leaves out full eve's daemon, BLE
    and the CLI (which imports the daemon). Positive control: the same walk from the CLI reaches them."""
    modules = all_modules()
    reached = closure(["eve_bridge.lite"], modules, top_only=False)
    assert {"eve_bridge.lite", "eve_bridge.telegram", "eve_bridge.lite_web", "eve_bridge.lite_rtc",
            "eve_bridge.loop_watchdog"} <= reached
    assert not {"eve_bridge.daemon", "eve_bridge.ble", "eve_bridge.cli"} & reached
    assert {"eve_bridge.daemon", "eve_bridge.ble"} <= closure(["eve_bridge.cli"], modules, top_only=False)


def test_lite_reads_only_classified_switches() -> None:
    """lite.py reads one switch of its own, the IPC address, and profile.py has the reason for it."""
    from test_profile_classified import _tree, switches_in

    found = switches_in(_tree(all_modules()["eve_bridge.lite"]))
    assert found == {"EVE_BRIDGE_SOCK"} and found <= set(profile.LITE_CLASSIFIED)


@pytest.mark.parametrize("line", [lite.PAGE_DOWN, lite.STOPPING, lite.UNKNOWN_EVENT])
def test_the_lines_are_plain(line: str) -> None:
    assert line.endswith(".") and "\n" not in line and len(line) < 120


def test_the_module_is_documented_with_its_contract() -> None:
    doc = textwrap.dedent(lite.__doc__ or "")
    for evt in ("web_login", "web_login_used", "status", "devices", "revoke", "lan"):
        assert f'"evt": "{evt}"' in doc, evt
    assert "exits 75" in doc and "NEEDS_OPENROUTER" in doc and "READY" in doc
