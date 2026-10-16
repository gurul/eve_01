"""eve lite end to end in a real browser (docs/plans/eve-lite.md WP-16; gate L16 in docs/lite-gates.md).

Headless Chromium against the real lite composition: a LiteDaemon on a temporary home, its Mac page and its LAN door
(TLS) on loopback and on ephemeral ports, serving the web app built now from ios/EveLink/Web. The brain and the voice
are stand-ins, and so is what would leave the Mac (the spend figures, the credit read, the Mac's name and address,
the Local Network alert and probe; no host name is looked up while a test runs): the brain answers a typed turn with
two lines and a call with one sentence; the voice hears "what time is it" in any press, keeps the press's audio, and
speaks a tone. Each count below is the server's own (RtcHub.stats, and what LinkChat was asked and how it came).

* Setup: with no model key the Mac's page is the setup page. Sign in with OpenRouter comes back with a key, which eve
  saves (the env file, 0600) and restarts into (exit 75), while the page waits and carries on by itself.
* Pairing from #p=: the Mac's page signs in from #login= and skips Composio; its chat goes over loopback fetch. Add a
  phone shows QR codes carrying the links with the secret; a phone opens the iPhone's link and pairs: the secret
  leaves the address bar, the device cookie is HttpOnly, Secure and never in the page's reach, and the link is spent
  (another phone is turned away, then pairs with a new window's six digits, as a laptop does).
* A turn streams over the WebRTC ``link`` channel, and so does a turn naming a photo the browser made a JPEG and
  uploaded over HTTPS. A first-time phone, which offers mDNS names instead of its addresses, connects through the
  Mac's rewrite of them. The phone shows the conversation so far, and a line eve writes while it is open.
* A forced RTC failure: a reply streaming when the Mac drops the peer and refuses new offers carries on over HTTPS,
  heard once and shown once; the page says it is on HTTPS and the next turn works there; once offers are answered
  again chat is back on the link. A phone whose link never opens (as with Local Network denied) chats over HTTPS
  after the first wait, and its Talk view shows the fix.
* Voice: a press of the hold button reaches speech-to-text with its audio (the microphone is a 1 kHz tone), and eve's
  answer comes back as sound on the audio track (inbound bytes and audio energy), with its caption. The Mac's own
  page talks the same way on a peer it lets go when the call ends.
* A device the Mac revokes loses its peer at once and is asked to pair again.

Chromium runs with --ignore-certificate-errors (the phone's click-through), --use-fake-ui-for-media-stream and
--use-fake-device-for-media-stream. Only a checkout without playwright skips this module (pytest.importorskip, as
test_miniapp_wheel.py); a Chromium or a web build that cannot be had fails it with the line that fixes it, since
gate L16 would read a skip as a pass."""

from __future__ import annotations

import asyncio
import contextlib
import ipaddress
import json
import logging
import os
import pwd
import shutil
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import wave
import zlib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, AsyncIterator, Callable, Iterator, Optional
from urllib.parse import parse_qs, urlsplit

import httpx
import numpy as np
import pytest
from test_link_chat import FakeInlet, store_at
from test_lite_daemon import ask, http, mac_sign_in, until
from test_lite_rtc_loopback import ToneVoice, dominant_hz

from eve_bridge import (
    envfile,
    lan_net,
    link_chat,
    lite,
    lite_rtc,
    lite_web,
    onboarding,
    openrouter,
    phone_call,
    profile,
    spend_sync,
    telegram,
    web_auth,
)

pw = pytest.importorskip("playwright.async_api")

WEB_SRC = Path(__file__).resolve().parents[2] / "ios" / "EveLink" / "Web"
CANARY_KEY = "CANARY-NOT-A-KEY-7f3c-openrouter"
MINTED = "CANARY-NOT-A-KEY-7f3c-minted"         # the key OpenRouter's sign-in mints here
HOSTNAME, LAN_IP = "Test-Mac", "192.168.50.7"
APPLE_HOST = f"{HOSTNAME.lower()}.local"           # the iPhone's link; the browser maps it to the LAN door on loopback
HEARD = "what time is it"                           # what the stand-in voice hears in any press (ToneVoice)
SAID = "It is three o'clock."                       # the stand-in brain's answer on a call
BLOCKED = "Direct link blocked, using HTTPS"        # the top bar's line while chat goes over HTTPS (Shell.tsx)
TONE_HZ = 1000.0                                    # the browser's microphone
ANDROID = ("Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/153.0.0.0 "
           "Mobile Safari/537.36")
# A phone that has allowed eve's microphone, which also makes the browser offer its addresses rather than mDNS names:
# the Mac points those names at the address the offer came from, and the LAN door here is on loopback, which it never
# rewrites to. A first-time phone, with no microphone allowed yet, has its own test on this Mac's private address.
PHONE = {"viewport": {"width": 412, "height": 915}, "device_scale_factor": 2.625, "is_mobile": True,
         "has_touch": True, "user_agent": ANDROID, "ignore_https_errors": True, "permissions": ["microphone"]}
LOCAL_NAMES = (None, "localhost", "127.0.0.1", "::1")
WAIT_MS = 30_000                                    # a page's slowest step here: a WebRTC connect under a loaded suite

# Every RTCPeerConnection the page makes, kept where the test can ask it for its stats (the page holds them in
# closures). A wrapper around the constructor, set before the page's own scripts run.
PEERS = """(() => {
  const Real = window.RTCPeerConnection;
  if (typeof Real !== "function") return;
  const peers = [];
  window.__evePeers = peers;
  window.RTCPeerConnection = new Proxy(Real, {
    construct(target, args, newTarget) {
      const pc = Reflect.construct(target, args, newTarget);
      peers.push(pc);
      return pc;
    },
  });
})();"""

# The page's inbound audio: what came in on the Mac's audio track, over every peer still open.
INBOUND = """async () => {
  let bytes = 0, packets = 0, energy = 0;
  for (const pc of window.__evePeers || []) {
    if (pc.connectionState === "closed") continue;
    (await pc.getStats()).forEach((s) => {
      if (s.type === "inbound-rtp" && s.kind === "audio") {
        bytes += s.bytesReceived || 0;
        packets += s.packetsReceived || 0;
        energy += s.totalAudioEnergy || 0;
      }
    });
  }
  return {bytes, packets, energy};
}"""


# ---- the stand-ins -------------------------------------------------------------------------------------------------

def first_line(words: str) -> str:
    return f"You said: {words}"


def second_line(words: str) -> str:
    return f"That was all for: {words}"


async def _two_lines(inlet: Brain, app: str) -> None:
    """The brain's answer to a typed turn, on the turn's stream: the words back, then a second line, held back while
    the test holds ``inlet.hold`` (a reply still streaming)."""
    words = inlet.heard[-1][0]
    await asyncio.sleep(0.05)
    inlet.out(app, first_line(words))
    if inlet.hold is not None:
        await inlet.hold.wait()
    await asyncio.sleep(0.05)
    inlet.out(app, second_line(words))
    inlet.busy = False


class Brain(FakeInlet):
    """The chat brain LiteDaemon lends to LinkChat and PhoneCalls (telegram.make_inlet's TelegramInlet), played by
    test_link_chat's FakeInlet: a typed turn is answered on the app's stream (``_two_lines``); words a call heard are
    answered with SAID on that call (phone_call.Brain: ``listen``, and ``hear`` with ``call``). Plus what LiteDaemon
    gives it and starts: its uploads, the watcher's notify, and ``run``."""

    def __init__(self, store: Any) -> None:
        super().__init__(store, _two_lines)
        self._chat_id = profile.LITE_CHAT_ID
        self.hold: Optional[asyncio.Event] = None       # set: a typed turn's second line waits for it
        self.say: Optional[Callable[[str], None]] = None
        self.called: list[str] = []                     # words heard on a call
        self.attached: list[list[Any]] = []             # each typed turn's attachments (link_uploads.Upload)
        self.told: list[str] = []
        self.link_uploads: Any = None

    def hear(self, text: str, *, on_call: bool = True, app: Optional[str] = None, grade: str = "auto",
             call: Optional[Callable[[str], None]] = None, attachments: Any = None) -> None:
        if app is None:                                 # a press on a call: answered on the call that heard it
            self.called.append(text)
            reader = call if call is not None else self.say
            if reader is not None:
                reader(SAID)
            return
        self.attached.append(list(attachments or ()))
        super().hear(text, on_call=on_call, app=app, grade=grade)

    def listen(self, say: Optional[Callable[[str], None]], owner: Optional[Callable[[str], None]] = None) -> bool:
        if say is not None or owner is None or self.say == owner:    # as TelegramInlet.listen
            self.say = say
        return True

    def tell_owner(self, text: str) -> None:
        self.told.append(text)

    async def run(self) -> None:
        await asyncio.Event().wait()                    # TelegramInlet.run, app-only: nothing to poll


class Voice(ToneVoice):
    """test_lite_rtc_loopback's voice: hears HEARD in any press and keeps each press's audio (``presses``); speaks
    ``secs`` of a 440 Hz tone for each sentence."""

    def __init__(self, secs: float = 1.5) -> None:
        super().__init__()
        self.secs = secs
        self.presses: list[bytes] = []

    def transcribe(self, pcm: bytes) -> str:
        self.presses.append(bytes(pcm))
        return super().transcribe(pcm)

    def speak_stream(self, text: str) -> Iterator[bytes]:
        self.spoken.append(text)
        t = np.arange(int(self.secs * lite_rtc.SAMPLE_RATE)) / lite_rtc.SAMPLE_RATE
        data = (np.sin(2 * np.pi * 440 * t) * 8000).astype(np.int16).tobytes()
        for i in range(0, len(data), 4800):
            yield data[i:i + 4800]


@dataclass
class StandIns:
    voice: Voice = field(default_factory=Voice)
    brain: Optional[Brain] = None
    alerts: list[int] = field(default_factory=list)
    probes: list[Any] = field(default_factory=list)


class Offline:
    """A host name lookup from this process fails, unless it is this Mac or an address written out (aiortc binds its
    candidates' addresses): nothing here may reach the network. Each one refused is remembered."""

    def __init__(self) -> None:
        self.asked: list[tuple[str, str]] = []
        self.real = socket.getaddrinfo

    def __call__(self, host: Any, *args: Any, **kwargs: Any) -> Any:
        name = host.decode() if isinstance(host, bytes) else host
        if name in LOCAL_NAMES or _is_address(name):
            return self.real(host, *args, **kwargs)
        self.asked.append((threading.current_thread().name, str(name)))
        raise socket.gaierror(socket.EAI_NONAME, "no network in this test")


def _is_address(name: Any) -> bool:
    try:
        ipaddress.ip_address(str(name))
    except ValueError:
        return False
    return True


async def _no_spend(shutdown: asyncio.Event, *args: Any, **kwargs: Any) -> None:
    await shutdown.wait()                               # spend_sync.loop would read OpenRouter's figures


async def _credit(key: str) -> dict[str, Any]:
    return {"ok": True, "usd": 4.25, "funded": True}


def png(width: int = 64, height: int = 48, rgb: tuple[int, int, int] = (200, 40, 40)) -> bytes:
    """A photo as a phone's camera roll may hand it over: a PNG of one colour."""
    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    rows = b"".join(b"\x00" + bytes(rgb) * width for _ in range(height))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


# ---- fixtures ------------------------------------------------------------------------------------------------------

def _browsers_path() -> Optional[str]:
    """Where `python -m playwright install chromium` put the browsers for this account. The `home` fixture moves
    HOME to a temporary folder right after asking, and Playwright looks under $HOME, so the folder found here is
    always handed on, even when it is under the HOME of the moment (docs/lite-gates.md).
    None when PLAYWRIGHT_BROWSERS_PATH is set already or no browsers are installed."""
    if os.environ.get("PLAYWRIGHT_BROWSERS_PATH"):
        return None
    sub = Path("Library/Caches/ms-playwright") if sys.platform == "darwin" else Path(".cache/ms-playwright")
    homes = [Path.home()]
    with contextlib.suppress(KeyError, OSError):
        homes.append(Path(pwd.getpwuid(os.getuid()).pw_dir))
    for home in homes:
        folder = home / sub
        if any(folder.glob("chromium*")):
            return str(folder)
    return None


@pytest.fixture(scope="module")
def web_dir(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """The web app built now from ios/EveLink/Web (vite.web.config.ts), into a folder of this module's own: the page
    under test is the source as it is, never a web_lite/ left by an older build."""
    node = shutil.which("node")
    vite = WEB_SRC / "node_modules" / "vite" / "bin" / "vite.js"
    if node is None or not vite.is_file():
        pytest.fail("the web app cannot be built here: run `pnpm install --frozen-lockfile` in ios/EveLink/Web "
                    "(Node 22.12 or later)")
    out = tmp_path_factory.mktemp("web-lite")
    done = subprocess.run([node, str(vite), "build", "--config", "vite.web.config.ts", "--outDir", str(out),
                           "--emptyOutDir", "--logLevel", "error"], cwd=WEB_SRC, capture_output=True, text=True,
                          timeout=300, check=False)
    if done.returncode != 0 or not (out / "web.html").is_file():
        pytest.fail(f"the web app did not build:\n{(done.stdout + done.stderr)[-3000:]}")
    (out / "web.html").rename(out / "index.html")    # vite.web.config.ts renames it only in web_lite/
    return out


@pytest.fixture(scope="module")
def tone_wav(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """The browser's microphone: four seconds of a 1 kHz tone, 48 kHz mono, which Chromium loops."""
    path = tmp_path_factory.mktemp("mic") / "tone.wav"
    rate = 48000
    t = np.arange(rate * 4) / rate
    pcm = (np.sin(2 * np.pi * TONE_HZ * t) * 12000).astype(np.int16)
    with wave.open(str(path), "wb") as out:
        out.setnchannels(1)
        out.setsampwidth(2)
        out.setframerate(rate)
        out.writeframes(pcm.tobytes())
    return path


@pytest.fixture
def home(tmp_path: Path, web_dir: Path) -> Iterator[Path]:
    """eve lite's environment as profile.apply makes it, on a temporary home, with a model key and the web app just
    built. Restored after (apply edits os.environ itself, and the daemon hands its key to telegram.hide_token)."""
    saved = dict(os.environ)
    browsers = _browsers_path()
    home = tmp_path / "home"
    home.mkdir()
    for name in [n for n in os.environ if n.startswith("EVE_") or n == "OPENROUTER_API_KEY"]:
        del os.environ[name]                            # nothing from the shell running the tests
    os.environ.update({"HOME": str(home), "EVE_PROFILE": "lite", "EVE_WEB_DIR": str(web_dir),
                       "OPENROUTER_API_KEY": CANARY_KEY})
    if browsers is not None:
        os.environ["PLAYWRIGHT_BROWSERS_PATH"] = browsers
    profile.apply(os.environ)
    try:
        yield home
    finally:
        os.environ.clear()
        os.environ.update(saved)
        telegram.unhide_tokens()


@pytest.fixture
def sock() -> Iterator[Path]:
    """The IPC socket's path, in a short folder: macOS keeps a socket's path in 104 bytes, and tmp_path is longer."""
    base = tempfile.gettempdir()
    folder = Path(tempfile.mkdtemp(prefix="evl-", dir=base if len(base) <= 60 else "/tmp"))
    try:
        yield folder / "ipc.sock"
    finally:
        shutil.rmtree(folder, ignore_errors=True)


@pytest.fixture
def stand_ins(home: Path, monkeypatch: pytest.MonkeyPatch) -> Iterator[StandIns]:
    """The brain and the voice LiteDaemon builds are the stand-ins; the spend figures never leave; and no host name
    is looked up while the test runs (checked at its end)."""
    made = StandIns()

    def make_inlet(config: Any, **lent: Any) -> Brain:
        memory = lent.get("memory")
        made.brain = Brain(memory.transcripts if memory is not None else store_at(home))
        return made.brain

    offline = Offline()
    monkeypatch.setattr(telegram, "make_inlet", make_inlet)
    monkeypatch.setattr(phone_call, "make_voice", lambda environ=None: made.voice)
    monkeypatch.setattr(spend_sync, "loop", _no_spend)
    monkeypatch.setattr(socket, "getaddrinfo", offline)
    yield made
    assert offline.asked == [], f"a host name was looked up: {offline.asked}"


# ---- eve, and the browser ------------------------------------------------------------------------------------------

class Refusal:
    """Offers the hub stops answering, on the test's word: ``refuse`` answers each with an RtcError (the page gets a
    400 and chats over HTTPS); ``unreachable`` answers it with no candidate either way, so the link never opens, as
    when macOS denies eve the Local Network. ``refused`` counts the offers that met either."""

    def __init__(self, hub: lite_rtc.RtcHub) -> None:
        self.refuse = False
        self.unreachable = False
        self.refused = 0
        real = hub.answer

        async def answer(device_id: str, sdp: str, peer_addr: str) -> dict[str, Any]:
            if self.refuse:
                self.refused += 1
                raise lite_rtc.RtcError("refused by the test")
            if not self.unreachable:
                return await real(device_id, sdp, peer_addr)
            self.refused += 1
            got = await real(device_id, _no_candidates(sdp), peer_addr)
            return {**got, "sdp": _no_candidates(got["sdp"])}

        hub.answer = answer                             # type: ignore[method-assign]


def _no_candidates(sdp: str) -> str:
    return "\r\n".join(ln for ln in sdp.split("\r\n") if not ln.startswith("a=candidate:"))


def watch_link(link: link_chat.LinkChat) -> list[tuple[str, str]]:
    """Each request the one LinkChat serves, as (its path, how it came): "channel" over a WebRTC link channel, or
    "lan" or "local", the door it came through over HTTP. The token check each passes says which."""
    served: list[tuple[str, str]] = []
    real = link.serve

    async def serve(method: str, path: str, query: str, headers: dict[str, str], reader: Any, writer: Any,
                    token_check: Any = None, **kwargs: Any) -> None:
        owner = getattr(token_check, "__self__", None)
        how = "channel" if isinstance(owner, lite_rtc.LinkChannel) else str(getattr(owner, "where", "unknown"))
        served.append((path, how))
        await real(method, path, query, headers, reader, writer, token_check, **kwargs)

    link.serve = serve                                  # type: ignore[method-assign]
    return served


@dataclass
class Eve:
    daemon: lite.LiteDaemon
    stand_ins: StandIns
    sock: Path
    served: list[tuple[str, str]]
    offers: Refusal

    @property
    def hub(self) -> lite_rtc.RtcHub:
        assert self.daemon.rtc is not None
        return self.daemon.rtc

    @property
    def voice(self) -> Voice:
        return self.stand_ins.voice

    def turns(self, how: str) -> int:
        return sum(1 for path, came in self.served if path == link_chat.TURN_PATH and came == how)


def web_options(made: StandIns, **extra: Any) -> dict[str, Any]:
    """WebContext's keywords: both doors on loopback (unless ``extra`` says otherwise) and on ephemeral ports, and
    macOS and the network replaced."""
    return {"local_ports": (0,), "lan_ports": (0,), "lan_hosts": ("127.0.0.1",), "hostname": lambda: HOSTNAME,
            "addresses": lambda: [LAN_IP], "alert": lambda: made.alerts.append(1),
            "probe": lambda peer: made.probes.append(peer) or "ok", "credit": _credit, "restart_delay": 0.0,
            **extra}


@contextlib.asynccontextmanager
async def serving(daemon: lite.LiteDaemon, exit_code: int = lite.EXIT_OK) -> AsyncIterator[asyncio.Task[int]]:
    """``daemon`` running until the block ends, then stopped (unless it stopped itself first): its run task, whose
    exit code must be ``exit_code``."""
    ready = asyncio.Event()
    daemon.on_ready = lambda d: ready.set()
    task = asyncio.create_task(daemon.run())
    waiter = asyncio.create_task(ready.wait())
    done, _ = await asyncio.wait({task, waiter}, timeout=60, return_when=asyncio.FIRST_COMPLETED)
    if waiter not in done:
        waiter.cancel()
        if task.done():
            task.result()                               # raises what stopped it
        raise AssertionError("eve lite was not ready in time")
    try:
        yield task
    finally:
        daemon.shutdown()
        with contextlib.suppress(Exception):
            await asyncio.wait_for(asyncio.shield(task), 60)
    assert task.result() == exit_code


@contextlib.asynccontextmanager
async def running(sock: Path, made: StandIns, **web: Any) -> AsyncIterator[Eve]:
    """A LiteDaemon with a model key, serving until the block ends; its composition is the real one but for the
    stand-ins. ``web``: WebContext keywords over ``web_options``."""
    daemon = lite.LiteDaemon(socket_path=str(sock), web_options=web_options(made, **web))
    async with serving(daemon):
        assert daemon.state == lite.READY and daemon.local is not None and daemon.local.running
        assert daemon.inlet is made.brain and daemon.calls is not None and daemon.calls.voice is made.voice
        assert daemon.link is not None and daemon.rtc is not None and daemon.rtc.voice_ready()
        yield Eve(daemon, made, sock, watch_link(daemon.link), Refusal(daemon.rtc))


@contextlib.asynccontextmanager
async def chromium(wav: Path, lan_door: str = "127.0.0.1") -> AsyncIterator[Any]:
    """Headless Chromium as a phone that clicked through eve's certificate warning, with a microphone it may use
    (the tone in ``wav``). The iPhone's link (APPLE_HOST) resolves to ``lan_door``, where the LAN door listens; every
    other name but localhost resolves to nothing, so the browser cannot reach the internet either."""
    hosts = f"MAP {APPLE_HOST} {lan_door}, MAP * ~NOTFOUND, EXCLUDE localhost"
    args = ["--ignore-certificate-errors", "--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream",
            f"--use-file-for-fake-audio-capture={wav}", f"--host-resolver-rules={hosts}"]
    async with pw.async_playwright() as play:
        try:
            browser = await play.chromium.launch(args=args)
        except Exception as e:  # noqa: BLE001 — the line that fixes it
            pytest.fail(f"Chromium does not start here ({type(e).__name__}): run `python -m playwright install "
                        f"chromium`\n{e}")
        try:
            yield browser
        finally:
            await browser.close()


@dataclass
class Page:
    context: Any
    page: Any
    errors: list[str]


async def open_page(browser: Any, url: str, **options: Any) -> Page:
    """``url`` in a context of its own (its own cookies), every peer it makes kept for its stats, and every error the
    page throws kept."""
    context = await browser.new_context(**options)
    await context.add_init_script(PEERS)
    page = await context.new_page()
    errors: list[str] = []
    page.on("pageerror", lambda e: errors.append(str(e)))
    await page.goto(url)
    return Page(context, page, errors)


async def add_a_phone(eve: Eve) -> dict[str, Any]:
    """The Mac's side of Add a phone, as its page asks it: signed in with `eve-bridge web open`'s code, then
    ``POST /api/web/pair/open``. The window: its links (each with #p=<secret>), QR codes and code."""
    local = eve.daemon.local
    assert local is not None
    cookie = await mac_sign_in(eve.sock, local.port)
    reply = await http(local.port, "POST", lite_web.PAIR_OPEN_PATH, {}, cookie=cookie)
    assert reply.status == 200, reply.body
    return reply.json()


def apple_link(window: dict[str, Any]) -> str:
    """The iPhone's link of a pairing window: https://<LocalHostName>.local:<port>/#p=<secret>."""
    url = next(link["url"] for link in window["links"] if link["label"] == "iPhone or iPad")
    assert url.startswith(f"https://{APPLE_HOST}:") and "/#p=" in url
    return url


async def paired_phone(eve: Eve, browser: Any) -> Page:
    """A phone that scanned the Mac's code: the link opened, paired, the chat showing."""
    phone = await open_page(browser, apple_link(await add_a_phone(eve)), **PHONE)
    await pw.expect(phone.page.get_by_role("textbox", name="Message eve")).to_be_visible(timeout=WAIT_MS)
    return phone


async def on_the_link(eve: Eve, phone: Page) -> None:
    """The phone's chat goes over its WebRTC link channel: the peer is connected and the top bar says nothing."""
    await until(lambda: eve.hub.stats()["connected"] == 1, WAIT_MS / 1000)
    await pw.expect(phone.page.locator(".eve-topbar__route")).to_have_count(0, timeout=WAIT_MS)


async def send(page: Any, text: str) -> None:
    await page.get_by_role("textbox", name="Message eve").fill(text)
    await page.get_by_role("button", name="Send", exact=True).click()


async def reply_shows(page: Any, text: str) -> None:
    """The brain's whole reply to ``text`` is in the chat, each line once, and the composer can send again."""
    await pw.expect(page.get_by_text(second_line(text)).first).to_be_visible(timeout=WAIT_MS)
    await pw.expect(page.get_by_role("button", name="Send", exact=True)).to_be_visible(timeout=WAIT_MS)
    shown = await page.locator("body").inner_text()
    assert shown.count(first_line(text)) == 1 and shown.count(second_line(text)) == 1, shown


async def inbound(page: Any) -> dict[str, float]:
    return await page.evaluate(INBOUND)


async def mac_page(eve: Eve, browser: Any, **options: Any) -> Page:
    """The Mac's own browser as `eve-bridge web open` opens it (its one-time code in the fragment), past setup's
    Composio step with Skip for now, the chat showing."""
    local = eve.daemon.local
    assert local is not None
    login = await ask(eve.sock, {"evt": "web_login"})
    assert login["ok"] is True and login["port"] == local.port
    mac = await open_page(browser, f"http://localhost:{local.port}/#login={login['code']}", **options)
    await pw.expect(mac.page.get_by_role("heading", name="Set up eve")).to_be_visible(timeout=WAIT_MS)
    assert "#" not in mac.page.url and login["code"] not in await mac.page.evaluate("location.href")
    await mac.page.get_by_role("button", name="Skip for now").click()
    await pw.expect(mac.page.get_by_role("textbox", name="Message eve")).to_be_visible(timeout=WAIT_MS)
    return mac


async def hold_to_talk(page: Any, secs: float) -> None:
    """Hold the Talk view's button down for ``secs``, as a finger does, then let go. The button works once the call
    channel said connected."""
    hold = page.get_by_role("button", name="Hold to talk")
    await pw.expect(hold).to_be_enabled(timeout=WAIT_MS)
    box = await hold.bounding_box()
    assert box is not None
    await page.mouse.move(box["x"] + box["width"] / 2, box["y"] + box["height"] / 2)
    await page.mouse.down()
    await pw.expect(page.get_by_text("Listening…")).to_be_visible()
    await asyncio.sleep(secs)
    await page.mouse.up()


def press_heard(voice: Voice) -> None:
    """The one press speech-to-text got: more than 0.3 s of the browser microphone's tone, through Opus both ways."""
    (pcm,) = voice.presses
    assert len(pcm) >= 0.3 * phone_call.BYTES_PER_SEC, len(pcm)
    assert abs(dominant_hz(pcm) - TONE_HZ) <= 30, dominant_hz(pcm)


# ---- the tests -----------------------------------------------------------------------------------------------------

def test_setup_signs_in_with_openrouter_and_eve_restarts_into_the_key(sock: Path, stand_ins: StandIns,
                                                                      tone_wav: Path,
                                                                      caplog: pytest.LogCaptureFixture) -> None:
    """User flow A: a fresh install has no model key, so the Mac's page is the setup page. Sign in with OpenRouter
    goes to OpenRouter's consent page (played here: Authorize sends the browser straight back) and returns; eve
    exchanges the code for a key (on a mock transport), saves it in the env file and restarts into it (exit 75, which
    launchd answers by starting it again), while the page says so and comes back by itself to the next step."""
    caplog.set_level(logging.INFO)
    del os.environ[openrouter.KEY_ENV]                  # a fresh install: no model key yet
    exchanged: list[dict[str, Any]] = []

    def keys(request: httpx.Request) -> httpx.Response:
        exchanged.append({"url": str(request.url), **json.loads(request.content)})
        return httpx.Response(200, json={"key": MINTED, "user_id": "user-not-real"})

    sent_to: list[str] = []

    async def consent(route: Any) -> None:
        """eve's own start runs, and sends the browser to openrouter.ai; there the person signs in and clicks
        Authorize, and OpenRouter sends the browser back to eve's callback with a code. That middle is played here,
        on the start's answer: a route sees no request a redirect made, and this browser resolves no outside name."""
        started = await route.fetch(max_redirects=0)
        sent_to.append(started.headers.get("location", ""))
        asked = parse_qs(urlsplit(sent_to[-1]).query)
        back = f"{asked['callback_url'][0]}?code=code-not-real&state={asked['state'][0]}"
        await route.fulfill(status=302, headers={"Location": back})

    # The page's port, as eve's own list of them has it (lite_web.LOCAL_PORTS): one free now, the same one first at
    # both starts, so the page's address stays put across the restart, as it does at 48780 (the page can only come
    # back to its own origin). The next start finds it again in web.json.
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        ports = (probe.getsockname()[1], 0)

    async def go() -> None:
        async with httpx.AsyncClient(transport=httpx.MockTransport(keys)) as client, chromium(tone_wav) as browser:
            first = lite.LiteDaemon(socket_path=str(sock), openrouter_login=onboarding.OpenRouterLogin(http=client),
                                    web_options=web_options(stand_ins, local_ports=ports,
                                                            restart_delay=lite_web.RESTART_DELAY_SECS))
            async with serving(first, lite.EXIT_RESTART) as task:
                assert first.state == lite.NEEDS_OPENROUTER and first.inlet is None and first.link is None
                local = first.local
                assert local is not None
                code = await ask(sock, {"evt": "web_login"})
                mac = await open_page(browser, f"http://localhost:{local.port}/#login={code['code']}")
                page = mac.page
                await mac.context.route(f"**{lite_web.OPENROUTER_START_PATH}", consent)
                await pw.expect(page.get_by_role("heading", name="Set up eve")).to_be_visible(timeout=WAIT_MS)
                await page.get_by_role("button", name="Sign in with OpenRouter").click()
                await pw.expect(page.get_by_role("heading", name="Restarting eve")).to_be_visible(timeout=WAIT_MS)
                await asyncio.wait_for(asyncio.shield(task), WAIT_MS / 1000)     # eve stopped itself to restart
            # OpenRouter was asked for a key by PKCE, to come back to this page's own callback
            (url,) = sent_to
            asked = parse_qs(urlsplit(url).query)
            assert url.startswith(onboarding.OPENROUTER_AUTH_URL + "?") and asked["code_challenge_method"] == ["S256"]
            assert asked["callback_url"] == [f"http://localhost:{local.port}{lite_web.OPENROUTER_CALLBACK_PATH}"]
            assert [(x["url"], x["code"], x["code_challenge_method"]) for x in exchanged] == [
                (onboarding.OPENROUTER_KEYS_URL, "code-not-real", "S256")]
            env = envfile.env_path()
            assert stat.S_IMODE(env.stat().st_mode) == 0o600
            assert envfile.parse_env_file(env.read_text()) == {openrouter.KEY_ENV: MINTED}
            # launchd starts eve again, and the CLI reads the env file before anything else (cli.main)
            del os.environ[openrouter.KEY_ENV]
            assert envfile.load_env_file() == [openrouter.KEY_ENV]
            profile.apply(os.environ)
            async with running(sock, stand_ins, local_ports=ports) as eve:
                assert eve.daemon.local is not None and eve.daemon.local.port == local.port == ports[0]
                await pw.expect(page.get_by_text("OpenRouter connected · $4.25 credit")).to_be_visible(
                    timeout=WAIT_MS)
                await page.get_by_role("button", name="Skip for now").click()
                await send(page, "hello after setup")
                await reply_shows(page, "hello after setup")
                assert eve.turns("local") == 1
            assert mac.errors == []
        assert MINTED not in caplog.text and "<token>" not in caplog.text     # the key never reached a log line

    asyncio.run(_within(go()))


def test_the_mac_adds_a_phone_and_the_phone_pairs_from_the_qr_link(sock: Path, stand_ins: StandIns,
                                                                   tone_wav: Path) -> None:
    async def go() -> None:
        async with running(sock, stand_ins) as eve, chromium(tone_wav) as browser:
            # the Mac's browser, signed in as `eve-bridge web open` opens it, past setup
            mac = await mac_page(eve, browser)
            page = mac.page
            cookies = [c for c in await mac.context.cookies() if c["name"] == web_auth.COOKIE_MAC]
            assert len(cookies) == 1 and cookies[0]["httpOnly"] and not cookies[0]["secure"]   # plain http, loopback
            # the chat on the Mac goes over loopback fetch, never WebRTC
            await send(page, "hello from the Mac")
            await reply_shows(page, "hello from the Mac")
            assert eve.turns("local") == 1 and eve.hub.stats()["answers"] == 0
            # Add a phone: the LAN door opens, the Local Network alert is raised, two links with their QR codes
            await page.get_by_role("button", name="Settings", exact=True).click()
            async with page.expect_response(lambda r: r.url.endswith(lite_web.PAIR_OPEN_PATH)) as opened:
                await page.get_by_role("button", name="Add a phone").click()
            window = await (await opened.value).json()
            url = apple_link(window)
            lan = eve.daemon.web.shared.lan if eve.daemon.web is not None else None
            assert stand_ins.alerts == [1] and lan is not None and lan.running
            secret = url.split("#p=", 1)[1]
            assert {link["label"]: link["url"] for link in window["links"]} == {
                "iPhone or iPad": f"https://{APPLE_HOST}:{lan.port}/#p={secret}",
                "Android or other": f"https://{LAN_IP}:{lan.port}/#p={secret}"}
            qr = page.get_by_role("img", name="QR code for iPhone or iPad")
            assert await qr.get_attribute("src") == lite_web.qr_svg(url)       # the code shown carries the link
            await pw.expect(page.get_by_text(url.split("#", 1)[0], exact=True)).to_be_visible()
            assert secret not in await page.locator("body").inner_text()       # only the QR code holds the secret
            # the phone scans it: pairs, and the secret leaves the address bar
            phone = await open_page(browser, url, **PHONE)
            await pw.expect(phone.page.get_by_role("textbox", name="Message eve")).to_be_visible(timeout=WAIT_MS)
            assert await phone.page.evaluate("location.hash") == "" and "#" not in phone.page.url
            cookies = [c for c in await phone.context.cookies() if c["name"] == web_auth.COOKIE_DEV]
            assert len(cookies) == 1 and cookies[0]["httpOnly"] and cookies[0]["secure"]
            assert cookies[0]["sameSite"] == "Strict" and cookies[0]["domain"] == APPLE_HOST
            kept = await phone.page.evaluate("[document.cookie, JSON.stringify(localStorage), "
                                             "JSON.stringify(sessionStorage)].join(' ')")
            assert cookies[0]["value"] not in kept and secret not in kept       # no token in the page's reach
            labels = sorted(d["label"] for d in eve.daemon.devices.list())
            assert labels == ["Android · Chrome", "This Mac · Chrome"]
            # the Mac asks whether macOS lets it reach the device that just paired (the Local Network probe)
            await until(lambda: stand_ins.probes != [], WAIT_MS / 1000)
            assert all(peer[0] == "127.0.0.1" for peer in stand_ins.probes), stand_ins.probes
            # the link is spent: another phone that opens it is asked to pair, and nothing is added
            late = await open_page(browser, url, **PHONE)
            await pw.expect(late.page.get_by_role("heading", name="Pair with eve")).to_be_visible(timeout=WAIT_MS)
            await pw.expect(late.page.get_by_role("alert")).to_contain_text("No phone pairing is open")
            assert len(eve.daemon.devices.list()) == 2
            assert not [c for c in await late.context.cookies() if c["name"] == web_auth.COOKIE_DEV]
            # a new window from the Mac: that phone types its six digits instead, as a laptop does
            await page.get_by_role("button", name="Done", exact=True).click()
            async with page.expect_response(lambda r: r.url.endswith(lite_web.PAIR_OPEN_PATH)) as reopened:
                await page.get_by_role("button", name="Add a phone").click()
            code = str((await (await reopened.value).json())["code"])
            await pw.expect(page.locator(".eve-pair__code")).to_have_text(code)
            await late.page.get_by_role("textbox", name="Six-digit code").fill(code)
            await late.page.get_by_role("button", name="Pair", exact=True).click()
            await pw.expect(late.page.get_by_role("textbox", name="Message eve")).to_be_visible(timeout=WAIT_MS)
            assert len(eve.daemon.devices.list()) == 3
            assert mac.errors == [] and phone.errors == [] and late.errors == []

    asyncio.run(_within(go()))


def test_a_turn_streams_over_the_link_channel(sock: Path, stand_ins: StandIns, tone_wav: Path) -> None:
    async def go() -> None:
        async with running(sock, stand_ins) as eve, chromium(tone_wav) as browser:
            phone = await paired_phone(eve, browser)
            page = phone.page
            await on_the_link(eve, phone)
            await send(page, "hello over the link")
            await reply_shows(page, "hello over the link")
            stats = eve.hub.stats()
            assert stats["link_turns"] == 1 and stats["answers"] == 1 and stats["peers"] == 1
            assert eve.turns("channel") == 1 and eve.turns("lan") == 0      # never over HTTPS
            brain = stand_ins.brain
            assert brain is not None
            assert [(text, app is not None) for text, _, app in brain.heard] == [("hello over the link", True)]
            # a photo: made a JPEG in the browser, uploaded over HTTPS, and named by a turn that goes on the link
            await page.locator("input[type=file]").set_input_files(
                {"name": "photo.png", "mimeType": "image/png", "buffer": png()})
            await pw.expect(page.get_by_role("img", name="Photo photo.jpg")).to_be_visible(timeout=WAIT_MS)
            await send(page, "what is in this photo")
            await reply_shows(page, "what is in this photo")
            assert eve.served.count((link_chat.UPLOAD_PATH, "lan")) == 1 and eve.turns("channel") == 2
            assert eve.turns("lan") == 0 and eve.hub.stats()["link_turns"] == 2
            (photo,) = brain.attached[-1]
            assert photo.kind == "image" and photo.name == "photo.jpg"
            assert photo.path.read_bytes()[:3] == b"\xff\xd8\xff"         # a JPEG now, as the iOS app sends it
            assert phone.errors == []

    asyncio.run(_within(go()))


def test_a_first_time_phone_hides_its_addresses_and_the_mac_points_them_where_it_signaled_from(
        sock: Path, stand_ins: StandIns, tone_wav: Path) -> None:
    """A phone that never granted the microphone offers mDNS names (``<uuid>.local``), not its addresses: a browser's
    privacy default. The Mac points each at the address the offer came from (lite_rtc.rewrite_local_candidates),
    which only a peer on the LAN has, never one on loopback; so here the LAN door listens on this Mac's own private
    address, as for a real phone (for the few seconds the test runs, behind its TLS, peer filter and one-time link)."""
    ip = next(iter(lan_net.private_addresses()), None)
    if ip is None:
        pytest.skip("this Mac has no private IPv4 address for a phone's link to meet it on")

    async def go() -> None:
        async with running(sock, stand_ins, lan_hosts=(ip,), addresses=lambda: [ip]) as eve, \
                chromium(tone_wav, lan_door=ip) as browser:
            first_time = {name: value for name, value in PHONE.items() if name != "permissions"}
            phone = await open_page(browser, apple_link(await add_a_phone(eve)), **first_time)
            await pw.expect(phone.page.get_by_role("textbox", name="Message eve")).to_be_visible(timeout=WAIT_MS)
            await on_the_link(eve, phone)
            offers = await phone.page.evaluate("window.__evePeers.map((pc) => pc.localDescription.sdp).join('')")
            hosts = [ln.split(" ")[4] for ln in offers.split("\r\n")
                     if ln.startswith("a=candidate:") and " typ host" in ln]
            assert hosts and all(host.endswith(".local") for host in hosts), hosts      # no address of its own
            (peer,) = eve.hub.peers.values()
            remote = peer.pc.remoteDescription.sdp
            seen = [ln.split(" ")[4] for ln in remote.split("\r\n") if ln.startswith("a=candidate:")]
            assert ".local" not in remote and seen and set(seen) == {ip}, seen      # each pointed at the signaling
            await send(phone.page, "hello from a first-time phone")
            await reply_shows(phone.page, "hello from a first-time phone")
            assert eve.turns("channel") == 1 and eve.turns("lan") == 0 and eve.hub.stats()["link_turns"] == 1
            assert phone.errors == []

    asyncio.run(_within(go()))


def test_a_forced_rtc_failure_falls_back_to_https_and_the_next_turn_works(sock: Path, stand_ins: StandIns,
                                                                          tone_wav: Path) -> None:
    async def go() -> None:
        async with running(sock, stand_ins) as eve, chromium(tone_wav) as browser:
            phone = await paired_phone(eve, browser)
            page = phone.page
            brain = stand_ins.brain
            assert brain is not None
            await on_the_link(eve, phone)
            # a reply still streaming over the link when the Mac drops the peer and answers no new offer: the page
            # carries the same turn on over HTTPS (its client_id joins it), so it is heard once and shows once
            brain.hold = asyncio.Event()
            await send(page, "first over the link")
            await pw.expect(page.get_by_text(first_line("first over the link"))).to_be_visible(timeout=WAIT_MS)
            assert eve.turns("channel") == 1 and eve.turns("lan") == 0
            eve.offers.refuse = True
            await eve.hub.close_device(next(iter(eve.hub.peers)))
            await until(lambda: eve.turns("lan") == 1, WAIT_MS / 1000)
            brain.hold.set()
            await reply_shows(page, "first over the link")
            assert [text for text, _, _ in brain.heard] == ["first over the link"]
            # the page says it is on HTTPS, and the next turn works there
            await pw.expect(page.get_by_role("button", name=BLOCKED)).to_be_visible(timeout=WAIT_MS)
            await send(page, "second over https")
            await reply_shows(page, "second over https")
            assert eve.turns("lan") == 2 and eve.turns("channel") == 1 and eve.hub.stats()["link_turns"] == 1
            # offers are answered again: the page's next try brings the link back, and chat returns to it
            await until(lambda: eve.offers.refused >= 1, WAIT_MS / 1000)
            eve.offers.refuse = False
            for _ in range(WAIT_MS // 1000):
                if eve.hub.stats()["connected"] == 1:
                    break
                await page.evaluate("window.dispatchEvent(new Event('online'))")   # the page's own wake
                await asyncio.sleep(1)
            await on_the_link(eve, phone)
            await send(page, "third over the link")
            await reply_shows(page, "third over the link")
            assert eve.turns("channel") == 2 and eve.turns("lan") == 2 and eve.hub.stats()["link_turns"] == 2
            assert [text for text, _, _ in brain.heard] == ["first over the link", "second over https",
                                                            "third over the link"]
            assert phone.errors == []

    asyncio.run(_within(go()))


def test_a_phone_whose_link_never_opens_chats_over_https_and_talk_shows_the_fix(sock: Path, stand_ins: StandIns,
                                                                               tone_wav: Path) -> None:
    async def go() -> None:
        async with running(sock, stand_ins) as eve, chromium(tone_wav) as browser:
            eve.offers.unreachable = True               # as with Local Network denied: answered, never connected
            phone = await paired_phone(eve, browser)
            page = phone.page
            await pw.expect(page.get_by_role("button", name=BLOCKED)).to_be_visible(timeout=WAIT_MS)
            await send(page, "hello without a link")
            await reply_shows(page, "hello without a link")
            assert eve.turns("lan") == 1 and eve.turns("channel") == 0 and eve.offers.refused >= 1
            # Talk needs the link: while it tries, and once it gives up, the page shows the Local Network fix rather
            # than a dead button
            await page.get_by_role("button", name="Talk with eve").click()
            await pw.expect(page.get_by_text("Chat is reaching the Mac over HTTPS right now")).to_be_visible(
                timeout=WAIT_MS)
            fix = page.get_by_text("Privacy & Security, then Local Network")
            await pw.expect(fix).to_be_visible()
            await pw.expect(page.get_by_text("Voice needs a direct link to the Mac.")).to_be_visible(
                timeout=WAIT_MS)
            await pw.expect(fix).to_be_visible()
            await pw.expect(page.get_by_role("button", name="Try again")).to_be_visible()
            assert eve.daemon.calls is not None and eve.daemon.calls.active is None
            assert phone.errors == []

    asyncio.run(_within(go()))


def test_a_voice_press_reaches_speech_to_text_and_eve_answers_aloud_with_a_caption(sock: Path, stand_ins: StandIns,
                                                                                   tone_wav: Path) -> None:
    async def go() -> None:
        async with running(sock, stand_ins) as eve, chromium(tone_wav) as browser:
            phone = await paired_phone(eve, browser)
            page = phone.page
            await on_the_link(eve, phone)
            calls = eve.daemon.calls
            assert calls is not None
            await page.get_by_role("button", name="Talk with eve").click()
            await pw.expect(page.get_by_role("button", name="Hold to talk")).to_be_enabled(timeout=WAIT_MS)
            await until(lambda: calls.active is not None, WAIT_MS / 1000)
            before = await inbound(page)
            await hold_to_talk(page, 2.0)
            # speech-to-text got the press, and the brain the words
            await until(lambda: eve.voice.presses != [], WAIT_MS / 1000)
            press_heard(eve.voice)
            assert stand_ins.brain is not None and stand_ins.brain.called == [HEARD]
            # eve's answer: what it heard, then its sentence as a caption while its sound arrives on the track. Until
            # then the track carried silence, which has no audio energy: energy now is eve's voice, played
            await pw.expect(page.locator(".eve-voice__heard")).to_have_text(HEARD, timeout=WAIT_MS)
            await pw.expect(page.locator(".eve-voice__said")).to_have_text(SAID, timeout=WAIT_MS)
            assert eve.voice.spoken == [SAID]
            after = before
            for _ in range(WAIT_MS // 100):
                after = await inbound(page)
                if after["energy"] > before["energy"] + 1e-3:
                    break
                await asyncio.sleep(0.1)
            assert after["energy"] > before["energy"] + 1e-3, (before, after)
            assert after["bytes"] > before["bytes"] > 0, (before, after)
            assert eve.hub.stats()["calls"] == 1
            assert phone.errors == []

    asyncio.run(_within(go()))


def test_the_macs_own_page_talks_over_webrtc_and_lets_its_peer_go_after(sock: Path, stand_ins: StandIns,
                                                                       tone_wav: Path) -> None:
    """The Mac's page chats over loopback fetch, so it holds no peer; for a call it makes one, voice only, and lets
    it go when the call ends (transport.ts)."""
    async def go() -> None:
        async with running(sock, stand_ins) as eve, chromium(tone_wav) as browser:
            mac = await mac_page(eve, browser, permissions=["microphone"])
            page = mac.page
            await asyncio.sleep(0.5)
            assert eve.hub.stats()["answers"] == 0                         # no peer for chat
            await page.get_by_role("button", name="Talk with eve").click()
            await hold_to_talk(page, 1.5)
            await until(lambda: eve.voice.presses != [], WAIT_MS / 1000)
            press_heard(eve.voice)
            await pw.expect(page.locator(".eve-voice__said")).to_have_text(SAID, timeout=WAIT_MS)
            assert eve.hub.stats()["answers"] == 1 and eve.hub.stats()["calls"] == 1
            # the call ends: the page closes its peer at once, and the Mac sees it go well before the hub's own
            # deadline for a peer that never connected (CONNECT_SECS) could have closed it instead
            await page.get_by_role("button", name="End voice").click()
            await page.wait_for_function("() => window.__evePeers.length === 1 && "
                                         "window.__evePeers[0].connectionState === 'closed'", timeout=WAIT_MS)
            assert lite_rtc.CONNECT_SECS > 10
            await until(lambda: eve.hub.peers == {}, 10)
            calls = eve.daemon.calls
            assert calls is not None
            await until(lambda: calls.active is None, 10)
            assert eve.turns("channel") == 0 and mac.errors == []

    asyncio.run(_within(go()))


def test_the_phone_shows_the_conversation_so_far_and_what_eve_says_later(sock: Path, stand_ins: StandIns,
                                                                        tone_wav: Path) -> None:
    async def go() -> None:
        async with running(sock, stand_ins) as eve, chromium(tone_wav) as browser:
            brain = stand_ins.brain
            assert brain is not None and brain.transcripts is not None
            store = brain.transcripts                      # the memory LiteDaemon built: what the app reads back
            conv = store.new_conv(telegram.CHANNEL)
            assert store.append(telegram.CHANNEL, conv, "owner", "say", "an earlier question", via="app")
            assert store.append(telegram.CHANNEL, conv, "eve", "say", "An earlier answer.", via="app")
            phone = await paired_phone(eve, browser)
            page = phone.page
            await pw.expect(page.get_by_text("An earlier answer.")).to_be_visible(timeout=WAIT_MS)
            await pw.expect(page.get_by_text("an earlier question")).to_be_visible()
            await on_the_link(eve, phone)
            # a line eve writes while the page is open (a watch's alert, say) reaches it through its live poll
            assert store.append(telegram.CHANNEL, conv, "eve", "say", "Gold is above 2,500 dollars now.")
            await pw.expect(page.get_by_text("Gold is above 2,500 dollars now.")).to_be_visible(timeout=WAIT_MS)
            assert (link_chat.MESSAGES_PATH, "channel") in eve.served
            shown = await page.locator("body").inner_text()
            assert shown.count("An earlier answer.") == 1 and shown.count("Gold is above 2,500 dollars now.") == 1
            assert phone.errors == []

    asyncio.run(_within(go()))


def test_a_device_the_mac_revokes_loses_its_link_and_is_asked_to_pair_again(sock: Path, stand_ins: StandIns,
                                                                           tone_wav: Path) -> None:
    async def go() -> None:
        async with running(sock, stand_ins) as eve, chromium(tone_wav) as browser:
            phone = await paired_phone(eve, browser)
            page = phone.page
            await on_the_link(eve, phone)
            phones = [d for d in eve.daemon.devices.list() if d["label"] == "Android · Chrome"]
            assert len(phones) == 1
            # `eve-bridge web revoke <id>`: the device is forgotten and its WebRTC peer closes at once
            assert await ask(eve.sock, {"evt": "revoke", "id": phones[0]["id"]}) == {
                "ok": True, "revoked": [phones[0]["id"]]}
            assert eve.hub.peers == {}
            await pw.expect(page.get_by_role("heading", name="Pair with eve")).to_be_visible(timeout=WAIT_MS)
            await pw.expect(page.get_by_role("alert")).to_contain_text("isn't paired with eve any more")
            assert [c for c in await phone.context.cookies() if c["name"] == web_auth.COOKIE_DEV] == []
            assert eve.hub.stats()["peers"] == 0 and eve.turns("channel") == 0 and eve.turns("lan") == 0
            assert phone.errors == []

    asyncio.run(_within(go()))


def test_the_network_guard_refuses_a_host_name_and_lets_addresses_through() -> None:
    """The control for the check every browser test here ends on (``stand_ins``): a name is refused and remembered;
    this Mac and written-out addresses, which aiortc binds, still resolve."""
    offline = Offline()
    with pytest.raises(socket.gaierror):
        offline("openrouter.ai", 443)
    assert offline.asked == [("MainThread", "openrouter.ai")]
    for host in ("localhost", "127.0.0.1", "::1", LAN_IP, "fe80::1%lo0"):
        assert offline(host, 443, type=socket.SOCK_STREAM)
    assert offline.asked == [("MainThread", "openrouter.ai")]


async def _within(coro: Any, secs: float = 150.0) -> None:
    """A test's whole run, cut off with a failure rather than left to hang the suite."""
    async with asyncio.timeout(secs):
        await coro
