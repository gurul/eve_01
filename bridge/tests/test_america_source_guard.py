"""Additional source restrictions cover the complete guarded HTTP exchange."""

from urllib.parse import urlsplit

import pytest
from test_watch_security import _Quiet, _serve, loopback_is_public  # noqa: F401

from eve_bridge import watch


@pytest.mark.usefixtures("loopback_is_public")
def test_source_guard_checks_initial_and_redirect_urls_with_real_exchange():
    hits = []

    class Pages(_Quiet):
        def do_GET(self):
            hits.append(self.path)
            if self.path.startswith("/redirect"):
                self.send_response(302)
                self.send_header("Location", "/ok" if self.path.endswith("ok") else "/outside")
                self.end_headers()
            else:
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"official source")

    server, port = _serve(Pages)
    base = f"http://127.0.0.1:{port}"
    checked = []

    def guard(url):
        checked.append(url)
        return "outside source restriction" if urlsplit(url).path == "/outside" else ""

    try:
        assert watch.http_request(base + "/redirect-ok", timeout=2, url_guard=guard) == (200, "official source")
        assert checked == [base + "/redirect-ok", base + "/ok"]
        with pytest.raises(watch.FetchError, match="outside source restriction"):
            watch.http_request(base + "/redirect-bad", timeout=2, url_guard=guard)
        assert "/outside" not in hits
        with pytest.raises(watch.FetchError, match="outside source restriction"):
            watch.http_request(base + "/outside", timeout=2, url_guard=guard)
        assert "/outside" not in hits
        # The existing public-reader behavior remains when no extra guard is
        # supplied; this is also the positive control for the excluded target.
        assert watch.http_request(base + "/redirect-bad", timeout=2) == (200, "official source")
        assert "/outside" in hits
    finally:
        server.shutdown()
        server.server_close()


def test_source_guard_supplements_public_dns_check(monkeypatch):
    checked = []
    monkeypatch.setattr(watch, "check_url", lambda url: checked.append(url) or "private address")
    with pytest.raises(watch.FetchError, match="private address"):
        watch.http_request("https://agency.gov/", timeout=2, url_guard=lambda url: "")
    assert checked == ["https://agency.gov/"]
