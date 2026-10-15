"""The WebMCP probe: measurement only, once per page, host and tool names only; no hosted model calls."""

from __future__ import annotations

import asyncio
import logging
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import browser_lane as bl


class ProbePage:
    def __init__(self, url: str, tools: Any = None, fail: bool = False) -> None:
        self.url = url
        self.tools = tools
        self.fail = fail
        self.probes = 0

    def evaluate(self, script: str) -> Any:
        if script == bl._COLLECT_JS:
            return {"title": "T", "url": self.url, "text_hash": "x", "elements": []}
        assert script == bl._WEBMCP_JS
        self.probes += 1
        if self.fail:
            raise RuntimeError("probe blew up")
        return {"api": False} if self.tools is None else {"api": True, "names": self.tools}


def probe_lines(caplog: pytest.LogCaptureFixture) -> list[str]:
    return [r.getMessage() for r in caplog.records if "webmcp probe" in r.getMessage()]


def test_off_by_default_so_direct_pages_read_once() -> None:
    page = ProbePage("https://shop.test/cart")
    bl._Page(page).snapshot()
    assert page.probes == 0


def test_logs_host_and_names_once_per_page(caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.INFO, logger=bl.__name__)
    page = ProbePage("https://shop.test/cart?secret=1#frag", tools=["add_to_cart", "checkout\nINJECT"])
    senses = bl._Page(page, webmcp_probe=True)
    senses.snapshot()
    senses.snapshot()
    page.url = "https://shop.test/cart?secret=2"   # same page, new query: not probed again
    senses.snapshot()
    assert page.probes == 1
    assert probe_lines(caplog) == ["browser lane: webmcp probe host=shop.test api=True tools=2 "
                                   "names=add_to_cart,checkoutINJECT"]
    page.url = "https://shop.test/checkout"
    senses.snapshot()
    assert page.probes == 2


def test_page_without_api_is_still_counted(caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.INFO, logger=bl.__name__)
    bl._Page(ProbePage("https://plain.test/"), webmcp_probe=True).snapshot()
    assert probe_lines(caplog) == ["browser lane: webmcp probe host=plain.test api=False tools=- names=-"]


@pytest.mark.parametrize("url", ["about:blank", "file:///tmp/x.html", "chrome://newtab/", ""])
def test_non_web_pages_are_not_probed(url: str) -> None:
    page = ProbePage(url, tools=["x"])
    bl._Page(page, webmcp_probe=True).snapshot()
    assert page.probes == 0


def test_failed_probe_is_silent_and_never_fails_the_snapshot(caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.INFO, logger=bl.__name__)
    page = ProbePage("https://broken.test/", fail=True)
    snap = bl._Page(page, webmcp_probe=True).snapshot()
    assert snap.title == "T" and page.probes == 1 and probe_lines(caplog) == []


@pytest.mark.live
def test_real_chromium_sees_registered_tools_and_their_absence(tmp_path: Path,
                                                               caplog: pytest.LogCaptureFixture) -> None:
    pytest.importorskip("playwright.sync_api")
    caplog.set_level(logging.INFO, logger=bl.__name__)
    # Chromium without the WebMCP flag has no document.modelContext, so the positive page shims one with the
    # spec's getTools() shape; the negative page is untouched.
    shim = """<script>Object.defineProperty(document, 'modelContext', {value: {
        getTools: async () => [{name: 'search_flights'}, {name: 'book'}]}});</script>"""
    pages = {"https://tools.test/": f"<title>Tools</title>{shim}<p>hi</p>",
             "https://plain.test/": "<title>Plain</title><p>hi</p>"}
    lane = bl.BrowserLane(bl.BrowserLaneConfig(profile=tmp_path / "profile", headless=True))

    async def go() -> None:
        try:
            def setup() -> None:
                page = lane._ensure().page
                def serve(html: str) -> Any:
                    return lambda route: route.fulfill(content_type="text/html", body=html)
                for url, html in pages.items():
                    page.route(url, serve(html))
            await lane._run(setup)
            for url in pages:
                await lane.open_url(url)
                await lane.outline()
        finally:
            await lane.close()

    asyncio.run(go())
    assert probe_lines(caplog) == [
        "browser lane: webmcp probe host=tools.test api=True tools=2 names=search_flights,book",
        "browser lane: webmcp probe host=plain.test api=False tools=- names=-",
    ]
