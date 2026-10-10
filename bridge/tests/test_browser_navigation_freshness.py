"""Fresh navigation observations and negative controls; no hosted model calls."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import browser_lane as bl


class NavigatingPage:
    def __init__(self, failures: int = 1, error: str = "Execution context was destroyed, most likely because of a navigation"):
        self.failures = failures
        self.error = error
        self.reads = 0
        self.waits: list[int] = []
        self.now = 0.0

    def evaluate(self, script: str) -> Any:
        self.reads += 1
        if self.reads <= self.failures:
            raise RuntimeError(self.error)
        if script == bl._COLLECT_JS:
            return {"title": "Destination", "url": "https://test/destination", "text_hash": "Fresh body", "elements": []}
        return "Fresh destination body"

    def title(self) -> str:
        return "Destination"

    def wait_for_timeout(self, ms: int) -> None:
        self.waits.append(ms)
        self.now += ms / 1000


def test_snapshot_retries_navigation_race_and_reads_destination() -> None:
    page = NavigatingPage()
    senses = bl._Page(page, clock=lambda: page.now)
    observed = senses.snapshot()
    assert observed.title == "Destination"
    assert "url: https://test/destination" in observed.context_lines
    assert page.reads == 2 and page.waits == [bl.NAV_READ_POLL_MS]


@pytest.mark.parametrize(("url", "expected"), [
    ("HTTPS://Example.TEST:443/guide", "https://example.test/guide"),
    ("http://example.test:80", "http://example.test/"),
    ("https://example.test:8443/guide", "https://example.test:8443/guide"),
    ("https://[::1]:443/guide", "https://[::1]/guide"),
    ("https://other.test/guide", "https://other.test/guide"),
    ("https://example.test/other", "https://example.test/other"),
    ("https://owner:password@example.test/guide", ""),
    ("https://owner@example.test/guide", ""),
    ("https://example.test/guide?lang=en", ""),
    ("https://example.test/guide#section", ""),
    ("https://example.test/guide?", ""),
    ("https://example.test/guide#", ""),
    ("javascript:void(0)", ""),
    ("file:///guide", ""),
    ("/relative", ""),
    ("https://example.test:invalid/guide", ""),
    ("https://example.test/\nsecret", ""),
])
def test_navigation_metadata_preserves_destination_or_omits_ambiguity(url: str, expected: str) -> None:
    assert bl.navigation_destination(url) == expected


@pytest.mark.parametrize(("url", "expected"), [
    ("https://EXAMPLE.test/mail?q=1#inbox", "example.test"),
    ("https://example.test/", "example.test"),
    ("https://owner:password@example.test/mail#inbox", ""),
    ("https://owner@example.test/mail#inbox", ""),
    ("javascript:void(0)", ""),
    ("https://[invalid/", ""),
])
def test_host_context_survives_query_fragment_without_leaking_them(url: str, expected: str) -> None:
    assert bl.observed_site(url) == expected


def test_outline_metadata_is_attached_to_visible_labelled_links_only() -> None:
    raw = {"title": "Test", "url": "https://test/home", "elements": [
        {"id": "1", "role": "link", "name": "Guide", "href": "https://test/guide"},
        {"id": "2", "role": "link", "name": "Guide elsewhere", "href": "https://other/guide"},
        {"id": "3", "role": "link", "name": "Other path", "href": "https://test/other"},
        {"id": "4", "role": "link", "name": "Query", "href": "https://test/guide?q=1"},
        {"id": "5", "role": "link", "name": "Offscreen", "href": "https://test/below", "offscreen": True},
        {"id": "6", "role": "link", "name": "", "href": "https://test/nameless"},
        {"id": "7", "role": "button", "name": "Button", "href": "https://test/button"},
    ]}
    lines = bl.outline_lines(bl.snapshot_from_page(raw, 1))
    assert lines == ["link: Guide; destination: https://test/guide",
                     "link: Guide elsewhere; destination: https://other/guide",
                     "link: Other path; destination: https://test/other", "link: Query",
                     "link: Offscreen", "button: Button"]


def test_current_url_is_full_snapshot_evidence_without_another_read() -> None:
    page = NavigatingPage(failures=0)
    senses = bl._Page(page, clock=lambda: page.now)
    assert senses.current_url() == "" and page.reads == 0
    original_evaluate = page.evaluate

    def evaluate(script: str) -> Any:
        raw = original_evaluate(script)
        raw["url"] = "https://test/destination?q=1#section"
        return raw

    page.evaluate = evaluate
    senses.snapshot()
    assert senses.current_url() == "https://test/destination?q=1#section" and page.reads == 1
    page.failures = 1000
    with pytest.raises(RuntimeError, match="Execution context was destroyed"):
        senses.snapshot()
    assert senses.current_url() == ""


@pytest.mark.live
def test_actual_relative_href_is_resolved_and_ambiguous_href_is_omitted(tmp_path: Path) -> None:
    pytest.importorskip("playwright.sync_api")
    lane = bl.BrowserLane(bl.BrowserLaneConfig(profile=tmp_path / "profile", headless=True))
    html = '''<title>Metadata</title><a href="/guide">Guide</a>
        <a href="?q=1">Query only</a><a href="#section">Hash only</a>
        <a href="https://owner:password@other.test/guide">Credentials</a>
        <a href="javascript:void(0)">Script</a><a href="https://other.test/guide">Other origin</a>'''

    async def go() -> tuple[dict[str, Any], dict[str, Any], str]:
        try:
            await lane._run(lambda: lane._ensure().page.route("https://metadata.test/**",
                           lambda route: route.fulfill(status=200, content_type="text/html", body=html)))
            await lane.open_url("https://metadata.test/home")
            ordinary = await lane.outline()
            await lane.open_url("https://metadata.test/home?q=1#section")
            ambiguous = await lane.outline()
            observed = await lane._run(lambda: lane._ensure().current_url())
            return ordinary, ambiguous, observed
        finally:
            await lane.close()

    ordinary, ambiguous, observed = asyncio.run(go())
    assert ordinary["url"] == "https://metadata.test/home"
    assert ordinary["site"] == "metadata.test"
    assert "link: Guide; destination: https://metadata.test/guide" in ordinary["lines"]
    assert "link: Other origin; destination: https://other.test/guide" in ordinary["lines"]
    for label in ("Query only", "Hash only", "Credentials", "Script"):
        assert f"link: {label}" in ordinary["lines"]
    assert ambiguous["url"] == "" and observed == "https://metadata.test/home?q=1#section"
    assert ambiguous["site"] == "metadata.test"


def test_text_oracle_retries_navigation_race_with_fresh_body() -> None:
    page = NavigatingPage(error="Cannot find context with specified id")
    senses = bl._Page(page, clock=lambda: page.now)
    assert senses.text_visible("Fresh destination body") is True
    assert senses.text_visible("missing expectation") is False
    assert page.waits == [bl.NAV_READ_POLL_MS]


def test_unknown_read_error_is_not_retried_or_reported_as_visible() -> None:
    page = NavigatingPage(failures=10, error="Target page, context or browser has been closed")
    senses = bl._Page(page, clock=lambda: page.now)
    with pytest.raises(RuntimeError, match="has been closed"):
        senses.snapshot()
    assert senses.text_visible("Fresh destination body") is False
    assert page.waits == [] and page.reads == 2


@pytest.mark.parametrize("clock_moves", [True, False])
def test_repeated_navigation_errors_are_bounded_and_never_reuse_snapshot(clock_moves: bool) -> None:
    page = NavigatingPage(failures=0)
    senses = bl._Page(page, clock=lambda: page.now if clock_moves else 0.0)
    assert senses.snapshot().title == "Destination"
    page.failures = 1000
    with pytest.raises(RuntimeError, match="Execution context was destroyed"):
        senses.snapshot()
    assert senses._snapshot is None
    assert len(page.waits) <= bl.SNAPSHOT_TIMEOUT_MS // bl.NAV_READ_POLL_MS
    assert page.reads <= bl.SNAPSHOT_TIMEOUT_MS // bl.NAV_READ_POLL_MS + 2


@pytest.mark.live
@pytest.mark.parametrize("target", ["destination", "wrong", "noop"])
def test_navigation_uses_actual_destination_and_never_repeats_click(tmp_path: Path, target: str) -> None:
    """A real Chromium action with a fresh context race injected at observation.

    The destination case verifies the original title criterion; wrong destination
    and a click with no navigation must hand off. No extra input is allowed.
    """
    pytest.importorskip("playwright.sync_api")
    (tmp_path / "destination.html").write_text("<title>Destination evidence</title><h1>Fresh destination body</h1>")
    (tmp_path / "wrong.html").write_text("<title>Unrelated page</title><h1>Unrelated body</h1>")
    action = "event.preventDefault()" if target == "noop" else ""
    (tmp_path / "home.html").write_text(
        f'<title>Home</title><a href="{target}.html" onclick="{action}">Open destination</a><button>Other action</button>')
    lane = bl.BrowserLane(bl.BrowserLaneConfig(profile=tmp_path / "profile", headless=True))
    plan = {"steps": [{"kind": "click", "target": "Open destination", "label_hint": "Open destination",
                       "expect": {"kind": "title_contains", "value": "Destination evidence"}}],
            "success": {"kind": "text_visible", "value": "Fresh destination body"}, "final_say": "Opened."}

    async def go() -> tuple[dict[str, Any], str, int, int]:
        await lane.open_url((tmp_path / "home.html").as_uri())
        clicks = 0
        races = 0

        def instrument() -> None:
            nonlocal clicks, races
            adapter = lane._ensure()
            original_click = adapter.click_candidate
            original_evaluate = adapter.page.evaluate

            def click(candidate: Any) -> str:
                nonlocal clicks
                clicks += 1
                return original_click(candidate)

            def evaluate(script: str, *args: Any, **kwargs: Any) -> Any:
                nonlocal races
                if clicks and not races and script == bl._COLLECT_JS:
                    races += 1
                    raise RuntimeError("Execution context was destroyed, most likely because of a navigation")
                return original_evaluate(script, *args, **kwargs)

            adapter.click_candidate = click
            adapter.page.evaluate = evaluate

        try:
            await lane._run(instrument)
            result = await lane.run_plan(plan, "open destination")
            url = await lane._run(lambda: lane._ensure().page.url)
            return result, url, clicks, races
        finally:
            await lane.close()

    result, url, clicks, races = asyncio.run(go())
    assert clicks == 1 and races == 1, result
    if target == "destination":
        assert result["status"] == "complete" and result["sentence"] == "Opened.", result
        assert url.endswith("destination.html")
        assert result["ledger"][0]["effect"] == "confirmed"
    else:
        assert result["status"] == "partial" and "did not see it" in result["reason"], result
        assert url.endswith("home.html" if target == "noop" else "wrong.html")
