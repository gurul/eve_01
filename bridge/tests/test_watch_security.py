"""Security replays for the watcher (watch.py) as the Telegram door uses it. Each test replays one defect found in
the security review of 2026-09-25 and FAILS on the code as it was reviewed; it passes once that defect is fixed.

Servers here are real sockets on this Mac's loopback, standing in for a public site. Two seams let them through
the address checks: ``watch.check_url`` (the check before a request) and ``watch._ip_public`` (the check a fixed
connection makes on the address it is about to use; it does not exist before the fix, so it is set with
``raising=False``). The rebinding replay patches neither: its fake DNS answers a public address to the check and a
loopback one to the connect, which is the attack. The two browser replays start a real Chromium, so they are
``live`` (EVE_LIVE=1), like the other Chromium tests.
"""

from __future__ import annotations

import asyncio
import http.server
import json
import os
import signal
import socket
import socketserver
import sys
import textwrap
import threading
import time
from pathlib import Path
from typing import Any, Iterator

import pytest

from eve_bridge import telegram_format, watch
from eve_bridge.watch import FetchError, RateLimiter, Watch, WatchConfig, Watcher

# ---- local servers --------------------------------------------------------------------------------

def _serve(handler: type, host: str = "127.0.0.1") -> tuple[http.server.ThreadingHTTPServer, int]:
    srv = http.server.ThreadingHTTPServer((host, 0), handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, srv.server_address[1]


class _Quiet(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args: Any) -> None:
        pass


@pytest.fixture
def loopback_is_public(monkeypatch: pytest.MonkeyPatch) -> None:
    """The loopback test servers pass both address checks (see the module docstring)."""
    monkeypatch.setattr(watch, "check_url", lambda url, **k: "")
    monkeypatch.setattr(watch, "_ip_public", lambda ip: True, raising=False)


class Clock:
    def __init__(self, t: float = 1_800_000_000.0) -> None:
        self.t = t

    def __call__(self) -> float:
        return self.t


# ---- SSRF: the address is checked at one lookup and connected at another -------------------------

def test_dns_rebinding_between_the_check_and_the_connect_is_refused(monkeypatch: pytest.MonkeyPatch) -> None:
    """check_url resolves the host, then urllib resolves it again to connect. A name that answers a public address
    first and 127.0.0.1 second reaches the loopback service. Reviewed code: the LAN page comes back, status 200."""
    hits: list[str] = []

    class Lan(_Quiet):
        def do_GET(self) -> None:
            hits.append(self.path)
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            self.wfile.write(b"<title>router admin</title>LAN SECRET")

    srv, port = _serve(Lan)
    real = socket.getaddrinfo
    lookups = {"n": 0}

    def rebinding(host: Any, port_: Any, *a: Any, **k: Any) -> Any:
        if host == "rebind.attacker.example":
            lookups["n"] += 1
            ip = "93.184.215.14" if lookups["n"] == 1 else "127.0.0.1"
            return [(socket.AF_INET, socket.SOCK_STREAM, 6, "", (ip, port_ or 0))]
        return real(host, port_, *a, **k)

    monkeypatch.setattr(socket, "getaddrinfo", rebinding)
    try:
        with pytest.raises(FetchError):
            watch.http_request(f"http://rebind.attacker.example:{port}/admin", timeout=3)
        assert hits == [], "the loopback service was reached"
    finally:
        srv.shutdown()


# ---- one unreadable answer must be a FetchError, and must never stop the loop ---------------------

def test_an_unknown_charset_is_a_fetch_error(loopback_is_public: None) -> None:
    class Bogus(_Quiet):
        def do_GET(self) -> None:
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=bogus-8")
            self.end_headers()
            self.wfile.write(b"<p>hi</p>")

    srv, port = _serve(Bogus)
    try:
        try:                                         # reviewed code: LookupError("unknown encoding: bogus-8")
            _, body = watch.http_request(f"http://127.0.0.1:{port}/", timeout=3)
            assert "hi" in body                      # read in a charset that exists is a fix too
        except FetchError:
            pass
    finally:
        srv.shutdown()


def test_a_garbage_status_line_is_a_fetch_error(loopback_is_public: None) -> None:
    class Raw(socketserver.BaseRequestHandler):
        def handle(self) -> None:
            self.request.recv(4096)
            self.request.sendall(b"HELLO THERE\r\n\r\n")

    srv = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Raw)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    try:
        with pytest.raises(FetchError):              # reviewed code: http.client.BadStatusLine
            watch.http_request(f"http://127.0.0.1:{srv.server_address[1]}/", timeout=3)
    finally:
        srv.shutdown()


def test_one_watch_whose_read_crashes_never_starves_the_others(tmp_path: Path) -> None:
    asyncio.run(_one_watch_whose_read_crashes_never_starves_the_others(tmp_path))


async def _one_watch_whose_read_crashes_never_starves_the_others(tmp_path: Path) -> None:
    """tick() checks due watches oldest first. A read that raises anything but FetchError escapes check() before
    next_at moves, so that watch stays first and due, and every tick dies on it: the watch behind it is never
    checked and the owner is never told. Reviewed code: after ten ticks, B has 0 checks."""
    clock = Clock()

    def fetch(url: str, **k: Any) -> tuple[int, str]:
        if "bad.example" in url:
            raise LookupError("unknown encoding: bogus-8")
        return 200, '<meta property="product:price:amount" content="50">'

    w = Watcher(WatchConfig(path=tmp_path / "w.json", browser=False, host_gap_secs=0), fetch=fetch,
                offload=False, clock=clock, resolve=lambda h: True)
    a = Watch(id="w1", kind="page", target="https://bad.example/", label="A", condition="below", value=5,
              every_secs=300, next_at=clock.t - 10)
    b = Watch(id="w2", kind="page", target="https://good.example/", label="B", condition="below", value=5,
              every_secs=300, next_at=clock.t - 5)
    w.watches = [a, b]
    for _ in range(10):                              # run()'s loop: a tick, and 60 s after one that failed
        try:
            await w.tick()
        except Exception:  # noqa: BLE001 — what run() does with a failed tick
            pass
        clock.t += 60
    assert b.checks >= 1, "the healthy watch was never checked"
    assert a.errors >= 1 and a.next_at > clock.t - 600, "the broken watch was never counted or rescheduled"


# ---- the event loop is the whole daemon's --------------------------------------------------------

def test_a_crafted_page_never_blocks_the_event_loop(tmp_path: Path) -> None:
    asyncio.run(_a_crafted_page_never_blocks_the_event_loop(tmp_path))


async def _a_crafted_page_never_blocks_the_event_loop(tmp_path: Path) -> None:
    """structured() and main_text() run on the event loop, and their lazy `.*?</script>` scans are quadratic in
    unclosed <script> tags: 400 KB stalled the loop 17 s when measured, 3 MB (MAX_BYTES) is minutes. Reviewed
    code: this 250 KB page stalls the loop for seconds."""
    unit = '<script type="application/ld+json">'
    page = unit * (250_000 // len(unit))
    w = Watcher(WatchConfig(path=tmp_path / "w.json", browser=False, model_calls_per_day=0),
                fetch=lambda url, **k: (200, page), resolve=lambda h: True)          # offload on, as in the daemon
    wt = Watch(id="w1", kind="page", target="https://shop.example/x", label="x", condition="below", value=5,
               every_secs=300)
    gaps: list[float] = []

    async def heartbeat() -> None:
        last = time.perf_counter()
        while True:
            await asyncio.sleep(0.02)
            now = time.perf_counter()
            gaps.append(now - last)
            last = now

    beat = asyncio.create_task(heartbeat())
    await asyncio.sleep(0.1)
    try:
        await w._read_page(wt)
    except FetchError:
        pass                                         # no model calls today: the reading ends there, as intended
    await asyncio.sleep(0.1)
    beat.cancel()
    assert max(gaps) < 1.0, f"the event loop stalled {max(gaps):.1f} s"


# ---- secrets --------------------------------------------------------------------------------------

def test_a_cross_host_redirect_never_carries_the_openrouter_key(loopback_is_public: None,
                                                                 monkeypatch: pytest.MonkeyPatch) -> None:
    """urllib copies every non-content header onto a redirect, and turns a POST answered 302 into a GET, to any
    host the Location names. Reviewed code: the other host receives `Authorization: Bearer <key>`."""
    seen: dict[str, Any] = {}

    class Other(_Quiet):
        def do_GET(self) -> None:
            seen["auth"] = self.headers.get("Authorization")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b"{}")

    other, po = _serve(Other)

    class Bounce(_Quiet):
        def do_POST(self) -> None:
            self.rfile.read(int(self.headers["Content-Length"]))
            self.send_response(302)
            self.send_header("Location", f"http://localhost:{po}/elsewhere")
            self.end_headers()

    bounce, pb = _serve(Bounce)
    monkeypatch.setattr(watch, "OPENROUTER_URL", f"http://127.0.0.1:{pb}/api/v1/chat/completions")
    monkeypatch.setenv("OPENROUTER_API_KEY", "sk-or-v1-TESTKEY")
    try:
        try:
            watch.openrouter({"model": "x"})
        except FetchError:
            pass                                     # refusing the redirect outright is a fix too
        assert "TESTKEY" not in str(seen.get("auth")), "the key went to another host"
    finally:
        other.shutdown()
        bounce.shutdown()


# ---- resources ------------------------------------------------------------------------------------

def test_a_trickling_server_is_cut_at_the_deadline(loopback_is_public: None) -> None:
    """The timeout is per socket read, so a server that sends one byte just inside it holds the check (and the
    watcher's one lock) as long as it likes. Reviewed code: timeout=1.0 and the call runs 6.4 s."""

    class Trickle(_Quiet):
        def do_GET(self) -> None:
            self.send_response(200)
            self.send_header("Content-Length", "100")
            self.end_headers()
            try:
                for _ in range(8):
                    self.wfile.write(b"x")
                    self.wfile.flush()
                    time.sleep(0.8)
            except OSError:
                pass

    srv, port = _serve(Trickle)
    t0 = time.perf_counter()
    try:
        try:
            watch.http_request(f"http://127.0.0.1:{port}/", timeout=1.0)
        except FetchError:
            pass
        assert time.perf_counter() - t0 < 4.0, f"ran {time.perf_counter() - t0:.1f} s with timeout=1.0"
    finally:
        srv.shutdown()


# ---- what a page can make eve text the owner -----------------------------------------------------

def _model(answer: dict[str, Any]) -> dict[str, Any]:
    return {"choices": [{"message": {"content": json.dumps(answer)}}]}


def test_a_page_alert_links_only_the_page_the_owner_gave(tmp_path: Path) -> None:
    asyncio.run(_a_page_alert_links_only_the_page_the_owner_gave(tmp_path))


async def _a_page_alert_links_only_the_page_the_owner_gave(tmp_path: Path) -> None:
    """model_reading keeps a `url` from ANY model answer (the page and vision prompts never ask for one), and
    evaluate() links `w.last_url or w.target`. So words on a watched page that talk the reader model into a url
    replace the owner's link in eve's own alert. Reviewed code: the alert ends with the attacker's link."""
    answers = iter([_model({"available": False, "note": "Sold out."}),
                    _model({"available": True, "note": "Back.", "url": "https://kettle-restock.example/login"})])
    sent: list[str] = []

    async def notify(text: str) -> None:
        sent.append(text)

    w = Watcher(WatchConfig(path=tmp_path / "w.json", browser=False), fetch=lambda url, **k: (200, "<p>Kettle</p>"),
                ask=lambda body: next(answers), offload=False, resolve=lambda h: True, notify=notify)
    out = await w.add({"kind": "page", "target": "https://shop.example/kettle", "label": "Blue Kettle",
                       "condition": "available", "value": None, "text": None, "city": None, "every_minutes": None})
    assert out["ok"]
    wt = w.watches[0]
    wt.digest, wt.next_at = "", 0.0                  # the page changed: a fresh model reading, due now
    w.limiter = RateLimiter(host_gap_secs=0)
    await w.tick()
    assert sent, "the second reading should have fired"
    assert "kettle-restock.example" not in sent[-1]
    assert sent[-1].rstrip().endswith("https://shop.example/kettle")


def test_a_search_note_cannot_hide_a_link_behind_words() -> None:
    """A search watch's alert carries the model's note, and the Telegram formatter turns `[words](url)` into a
    hyperlink. So a web page the search model read can put "claim your presale code" over any link, in eve's
    own message. Reviewed code: the composed message holds <a href="https://tix-claim.example/sso">."""
    wt = Watch(id="w1", kind="search", target="tickets for X in Seattle", label="X in Seattle",
               condition="available", checks=1, armed=True)
    reading = watch.model_reading(_model({"available": True,
                                          "note": "[Presale code: claim here](https://tix-claim.example/sso)"}),
                                  "search")
    alert = watch.evaluate(wt, reading)
    assert alert is not None
    assert "<a href" not in telegram_format.compose(alert, title="Watching")


# ---- the browser (a real Chromium: live) ------------------------------------------------------------

class _V6Server(http.server.ThreadingHTTPServer):
    address_family = socket.AF_INET6


@pytest.fixture
def lan_and_page() -> Iterator[tuple[str, list[str], list[str]]]:
    """A "LAN" service on 127.0.0.1 and an attacker's page on ::1. The test treats IPv6 as public and IPv4 as
    private, so the page is reachable through the guard and the LAN is not: the two are told apart by the
    address the proxy connects to, which is exactly what the guard checks."""
    lan_hits: list[str] = []
    page_hits: list[str] = []

    class Lan(_Quiet):
        def do_GET(self) -> None:
            lan_hits.append(self.path + (" [websocket]" if self.headers.get("Upgrade") else ""))
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            self.wfile.write(b"<h1>LAN-SECRET-4242</h1>")

        do_POST = do_GET

    lan, pl = _serve(Lan)
    base = f"http://127.0.0.1:{pl}"

    class Page(_Quiet):
        def do_GET(self) -> None:
            page_hits.append(self.path)
            if self.path in ("/redir", "/r2"):
                self.send_response(302)
                self.send_header("Location", base + ("/top-redirect" if self.path == "/redir" else "/sub-redirect"))
                self.end_headers()
                return
            if self.path == "/ok.png":                   # the positive control: the page's own asset loads
                self.send_response(200)
                self.send_header("Content-Type", "image/png")
                self.end_headers()
                return
            body = (f'<h1>$20</h1><img src="/ok.png"><img src="/r2"><iframe src="{base}/iframe"></iframe>'
                    f'<script>fetch("{base}/fetch").catch(() => {{}});'
                    f'try {{ new WebSocket("ws://127.0.0.1:{pl}/ws"); }} catch (e) {{}}'
                    f'try {{ window.open("{base}/popup"); }} catch (e) {{}}'
                    f'try {{ navigator.sendBeacon("{base}/beacon", "x"); }} catch (e) {{}}</script>')
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            self.wfile.write(body.encode())

    page = _V6Server(("::1", 0), Page)
    threading.Thread(target=page.serve_forever, daemon=True).start()
    yield f"http://[::1]:{page.server_address[1]}", lan_hits, page_hits
    lan.shutdown()
    page.shutdown()


@pytest.mark.live
def test_the_render_guard_covers_redirects_websockets_and_popups(monkeypatch: pytest.MonkeyPatch,
                                                                   lan_and_page: tuple[str, list[str], list[str]]) -> None:
    """page.route never saw a redirect's next hop, a WebSocket, or a popup's requests, so the reviewed code let
    all three reach the LAN. Every connection now goes through the guard proxy, which checks the address it
    connects to (verification/Eve/WatchSsrf.lean). Run in-process (_render_here) so the address rule can be
    set for the test; render() runs this same function in a child with a deadline."""
    pytest.importorskip("playwright")
    origin, lan_hits, page_hits = lan_and_page
    monkeypatch.setattr(watch, "_ip_public", lambda ip: ip.version == 6)
    try:
        html, _ = watch._render_here(origin + "/redir", timeout=15)
        assert "LAN-SECRET-4242" not in html
    except FetchError:
        pass                                           # a refused top-level redirect is a fix too
    html, _ = watch._render_here(origin + "/page", timeout=15)
    assert "$20" in html
    assert "/ok.png" in page_hits, "the page's own asset never loaded: the guard is blocking everything"
    assert lan_hits == [], f"the LAN service was reached: {sorted(set(lan_hits))}"


def test_a_render_that_never_answers_is_killed_at_its_deadline(tmp_path: Path) -> None:
    """A page whose script spins after load held Playwright's evaluate and keyboard calls, and the watcher's one
    lock, forever. render() now runs the browser in a child process group and kills the group at the deadline.
    The child here never answers and has started a grandchild (as the real one starts Chromium): both must go."""
    pidfile = tmp_path / "grandchild.pid"
    child = textwrap.dedent(f"""
        import subprocess, sys, time
        g = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"])
        open({str(pidfile)!r}, "w").write(str(g.pid))
        time.sleep(600)
    """)
    t0 = time.perf_counter()
    with pytest.raises(FetchError, match="too long to render"):
        watch.render("https://93.184.215.14/", argv=[sys.executable, "-c", child], deadline=3)
    assert time.perf_counter() - t0 < 10
    grandchild = int(pidfile.read_text())
    for _ in range(50):
        try:
            os.kill(grandchild, 0)
        except ProcessLookupError:
            break
        time.sleep(0.1)
    else:
        os.kill(grandchild, signal.SIGKILL)
        pytest.fail("the render's grandchild (Chromium's stand-in) outlived the deadline")
