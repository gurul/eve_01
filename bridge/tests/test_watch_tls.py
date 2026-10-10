"""The Chrome-like read (watch.tls_request, curl_cffi) and where the watcher uses it.

tls_request replaces http_request's guards with its own, so each guard is replayed here against a real server on
this Mac's loopback: every hop checked and pinned to the address its one lookup checked, redirects followed by
eve (never libcurl) and checked, no proxy from the environment, a deadline for the whole answer, a size cap.
The loopback servers pass the address checks through the same two seams test_watch_security uses. The watcher
tests lend fakes, as test_watch does. Skipped when curl_cffi is not installed (it is an optional extra).
"""

from __future__ import annotations

import asyncio
import http.server
import socket
import threading
import time
from pathlib import Path
from typing import Any

import pytest
from test_watch import JSON_LD_EVENT, args, make_watcher

from eve_bridge import watch
from eve_bridge.watch import FetchError

pytest.importorskip("curl_cffi")


def _serve(handler: type) -> tuple[http.server.ThreadingHTTPServer, int]:
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, srv.server_address[1]


class _Quiet(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a: Any) -> None:
        pass

    def send(self, status: int, body: bytes = b"", **headers: str) -> None:
        self.send_response(status)
        for k, v in headers.items():
            self.send_header(k.replace("_", "-"), v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


@pytest.fixture
def loopback_is_public(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(watch, "check_url", lambda url, **k: "")
    monkeypatch.setattr(watch, "_ip_public", lambda ip: True)


# ---- tls_request's guards ---------------------------------------------------------------------

def test_it_reads_a_page_and_its_charset(loopback_is_public: None) -> None:
    class H(_Quiet):
        def do_GET(self) -> None:
            self.send(200, "prix 12 €".encode("latin-1", "replace"), Content_Type="text/html; charset=latin-1")

    srv, port = _serve(H)
    try:
        status, body = watch.tls_request(f"http://127.0.0.1:{port}/p")
        assert status == 200 and body.startswith("prix 12")
    finally:
        srv.shutdown()


def test_the_connection_goes_to_the_pinned_address_not_a_second_lookup(monkeypatch: pytest.MonkeyPatch) -> None:
    """``pinned.example`` does not exist in real DNS: the read succeeds only if libcurl connects to the address
    eve's one lookup answered, which is the pin."""
    class H(_Quiet):
        def do_GET(self) -> None:
            self.send(200, b"<html>pinned</html>")

    srv, port = _serve(H)
    real = socket.getaddrinfo

    def fake(host: Any, *a: Any, **k: Any) -> Any:
        return real("127.0.0.1", *a, **k) if host == "pinned.example" else real(host, *a, **k)

    monkeypatch.setattr(watch, "check_url", lambda url, **k: "")
    monkeypatch.setattr(watch, "_ip_public", lambda ip: True)
    monkeypatch.setattr(socket, "getaddrinfo", fake)
    try:
        assert watch.tls_request(f"http://pinned.example:{port}/")[1] == "<html>pinned</html>"
    finally:
        srv.shutdown()


def test_a_name_answering_a_private_address_is_refused_at_the_connect(monkeypatch: pytest.MonkeyPatch) -> None:
    """check_url let it through (rebinding: its lookup saw a public address); the pin's own lookup sees loopback,
    and the real address check refuses it before any connection."""
    monkeypatch.setattr(watch, "check_url", lambda url, **k: "")
    real = socket.getaddrinfo
    monkeypatch.setattr(socket, "getaddrinfo", lambda host, *a, **k: real("127.0.0.1", *a, **k))
    with pytest.raises(FetchError, match="private or unknown"):
        watch.tls_request("http://rebind.example/")


def test_a_redirect_is_checked_like_the_first_link(monkeypatch: pytest.MonkeyPatch) -> None:
    class H(_Quiet):
        def do_GET(self) -> None:
            self.send(302, Location="http://169.254.169.254/latest/meta-data/")

    srv, port = _serve(H)
    monkeypatch.setattr(watch, "check_url",
                        lambda url, **k: "" if "127.0.0.1" in url else "that link goes to a private or unknown address")
    monkeypatch.setattr(watch, "_ip_public", lambda ip: True)
    try:
        with pytest.raises(FetchError, match="redirected somewhere not allowed"):
            watch.tls_request(f"http://127.0.0.1:{port}/")
    finally:
        srv.shutdown()


def test_redirects_are_followed_then_capped(loopback_is_public: None) -> None:
    hops: list[str] = []

    class H(_Quiet):
        def do_GET(self) -> None:
            hops.append(self.path)
            if self.path == "/end":
                self.send(200, b"arrived")
            elif self.path == "/loop":
                self.send(301, Location="/loop")
            else:
                self.send(307, Location="/end")

    srv, port = _serve(H)
    try:
        assert watch.tls_request(f"http://127.0.0.1:{port}/start")[1] == "arrived"
        assert hops == ["/start", "/end"]
        with pytest.raises(FetchError, match="redirected too many times"):
            watch.tls_request(f"http://127.0.0.1:{port}/loop")
        assert hops.count("/loop") == watch.TLS_MAX_HOPS + 1
    finally:
        srv.shutdown()


def test_a_page_past_the_size_cap_is_refused(loopback_is_public: None, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(watch, "MAX_BYTES", 1000)

    class H(_Quiet):
        def do_GET(self) -> None:
            if self.path == "/sized":
                self.send(200, b"x" * 5000)
                return
            self.send_response(200)                      # no Content-Length: only the read loop can stop it
            self.send_header("Connection", "close")
            self.end_headers()
            for _ in range(10):
                self.wfile.write(b"y" * 1000)

    srv, port = _serve(H)
    try:
        for path in ("/sized", "/streamed"):
            with pytest.raises(FetchError, match="too big"):
                watch.tls_request(f"http://127.0.0.1:{port}{path}")
    finally:
        srv.shutdown()


def test_a_proxy_in_the_environment_is_ignored(loopback_is_public: None, monkeypatch: pytest.MonkeyPatch) -> None:
    """A proxy would make the connection, and the pin with it, moot. Port 9 on loopback has nothing listening, so
    a read that used the proxy would fail."""
    class H(_Quiet):
        def do_GET(self) -> None:
            self.send(200, b"direct")

    srv, port = _serve(H)
    for name in ("http_proxy", "HTTP_PROXY", "https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"):
        monkeypatch.setenv(name, "http://127.0.0.1:9")
    monkeypatch.delenv("no_proxy", raising=False)
    monkeypatch.delenv("NO_PROXY", raising=False)
    try:
        assert watch.tls_request(f"http://127.0.0.1:{port}/")[1] == "direct"
    finally:
        srv.shutdown()


def test_a_refusal_carries_its_status_and_retry_after(loopback_is_public: None) -> None:
    class H(_Quiet):
        def do_GET(self) -> None:
            if self.path == "/slow-down":
                self.send(429, b"no", Retry_After="7")
            else:
                self.send(403, b"<html>Access Denied</html>")

    srv, port = _serve(H)
    try:
        with pytest.raises(FetchError) as e:
            watch.tls_request(f"http://127.0.0.1:{port}/")
        assert e.value.status == 403 and e.value.reason == "the site answered HTTP 403"
        with pytest.raises(FetchError) as e:
            watch.tls_request(f"http://127.0.0.1:{port}/slow-down")
        assert e.value.status == 429 and e.value.retry_after == 7.0
    finally:
        srv.shutdown()


def test_a_trickling_server_is_cut_at_the_deadline(loopback_is_public: None) -> None:
    class H(_Quiet):
        def do_GET(self) -> None:
            self.send_response(200)
            self.send_header("Content-Length", "100000")
            self.end_headers()
            try:
                for _ in range(100):
                    self.wfile.write(b"z")
                    self.wfile.flush()
                    time.sleep(0.3)
            except OSError:
                pass

    srv, port = _serve(H)
    try:
        t = time.monotonic()
        with pytest.raises(FetchError, match="took too long"):
            watch.tls_request(f"http://127.0.0.1:{port}/", timeout=1.0)
        assert time.monotonic() - t < 4
    finally:
        srv.shutdown()


def test_only_other_schemes_and_credentials_are_refused_before_any_lookup() -> None:
    for url in ("file:///etc/passwd", "ftp://example.com/", "https://user:pw@example.com/"):
        with pytest.raises(FetchError):
            watch.tls_request(url)


def test_which_failures_are_worth_the_chrome_like_read() -> None:
    assert watch.tls_worth(FetchError("the site answered HTTP 403", 403))
    assert watch.tls_worth(FetchError("the site answered HTTP 401", 401))
    assert watch.tls_worth(FetchError(watch.TOO_LONG))
    assert not watch.tls_worth(FetchError("the site answered HTTP 429", 429, 5.0))       # honoured, not dodged
    assert not watch.tls_worth(FetchError("the site answered HTTP 404", 404))
    assert not watch.tls_worth(FetchError("the site could not be reached"))
    assert not watch.tls_worth(FetchError("the site answered HTTP 403", 403, host="openrouter.ai"))


def test_the_switch(monkeypatch: pytest.MonkeyPatch) -> None:
    assert watch._tls_on({}) is True                                    # auto, and curl_cffi is installed here
    assert watch._tls_on({"EVE_WATCH_TLS": "0"}) is False
    monkeypatch.setattr(watch, "tls_available", lambda: False)
    assert watch._tls_on({}) is False and watch._tls_on({"EVE_WATCH_TLS": "1"}) is False


# ---- the watcher: where the Chrome-like read sits on the ladder ---------------------------------

def refused(status: int = 403) -> Any:
    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        raise FetchError(f"the site answered HTTP {status}", status)
    return fetch


def test_a_refused_page_is_read_as_chrome_would_and_remembered(tmp_path: Path) -> None:
    tls_calls: list[str] = []
    w = make_watcher(tmp_path / "w.json", fetch=refused(403), tls=True)
    w._tls = lambda url, **kw: tls_calls.append(url) or (200, JSON_LD_EVENT)
    out = asyncio.run(w.add(args(target="https://shop.example/p", condition="below", value=100)))
    assert out["ok"] and "85" in out["now"] and tls_calls == ["https://shop.example/p"]
    item = w.watches[0]
    assert item.via == "tls" and item.every_secs == watch.DEFAULT_EVERY_SECS["page"]   # no browser floor
    # from now on straight to the Chrome-like read: the plain read is not tried again
    w._fetch = lambda url, **kw: pytest.fail("a tls page is not fetched plainly")
    asyncio.run(w.read(item))
    assert len(tls_calls) == 2
    # and it survives a restart
    again = make_watcher(tmp_path / "w.json", tls=True)
    assert again.watches[0].via == "tls"


def test_a_site_that_never_answers_is_read_as_chrome_would(tmp_path: Path) -> None:
    def hang(url: str, **kw: Any) -> tuple[int, str]:
        raise FetchError(watch.TOO_LONG)

    w = make_watcher(tmp_path / "w.json", fetch=hang, tls=True)
    w._tls = lambda url, **kw: (200, JSON_LD_EVENT)
    assert asyncio.run(w.add(args(condition="below", value=100)))["ok"] and w.watches[0].via == "tls"


def test_a_rate_limit_is_honoured_not_dodged(tmp_path: Path) -> None:
    w = make_watcher(tmp_path / "w.json", fetch=refused(429), tls=True)
    w._tls = lambda url, **kw: pytest.fail("a 429 is not retried another way")
    out = asyncio.run(w.add(args(condition="below", value=100)))
    assert "first check failed" in out["now"] and w.watches[0].via == ""


def test_when_the_chrome_like_read_is_refused_too_the_browser_comes_next(tmp_path: Path) -> None:
    renders: list[str] = []
    w = make_watcher(tmp_path / "w.json", fetch=refused(403), tls=True, browser=True)
    w._tls = refused(403)
    w._render = lambda url, **kw: renders.append(url) or (JSON_LD_EVENT, b"")
    out = asyncio.run(w.add(args(target="https://shop.example/p", condition="below", value=100)))
    assert out["ok"] and renders == ["https://shop.example/p"] and w.watches[0].via == "browser"


def test_off_means_off(tmp_path: Path) -> None:
    w = make_watcher(tmp_path / "w.json", fetch=refused(403), tls=False)
    w._tls = lambda url, **kw: pytest.fail("the Chrome-like read is off")
    asyncio.run(w.add(args(condition="below", value=100)))
    assert w.watches[0].via in ("", "search")
    # make_watcher in watch.py lends the real reader; the Watcher alone lends none
    assert watch.Watcher(watch.WatchConfig(enabled=True, path=tmp_path / "x.json", tls=True))._tls is None
