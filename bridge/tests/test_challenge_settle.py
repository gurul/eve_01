"""A Cloudflare "Just a moment…" page is read again for a few seconds before the lane blocks; hard blocks stop at once.

Cloudflare's JavaScript check usually clears itself in about five seconds and then loads the real page, so a
lane that blocked on the first look stopped tasks that would have gone through. A hard block (Attention
Required, Access Denied, eBay's /splashui/challenge, Google's /sorry/) never clears by waiting, so it stops
with no wait. The wait is bounded (CHALLENGE_SETTLE_SECS) and the clock and the sleep are injected here, so
no test sleeps for real.
"""

from __future__ import annotations

import asyncio
from typing import Any, Optional

import pytest
from browser_fakes import FakePage, button, lane_on, link

from eve_bridge import browser_lane as bl

CF_TEXT = "Checking your browser before accessing shop.example. This process is automatic."


class Clock:
    """A fake clock and sleep. ``clear_at``: when the fake page swaps the check for the real page."""

    def __init__(self, page: FakePage, clear_at: Optional[float]) -> None:
        self.t, self.page, self.clear_at = 100.0, page, clear_at
        self.slept: list[float] = []

    def __call__(self) -> float:
        return self.t

    def sleep(self, secs: float) -> None:
        self.slept.append(secs)
        self.t += secs
        if self.clear_at is not None and self.t - 100.0 >= self.clear_at and self.page.title_text.startswith("Just"):
            self.page.url = "https://shop.example/item"
            self.page.title_text, self.page.body = "Blue mug", "Blue mug, 12 dollars. Add to cart."
            self.page.elements = [button("0", "Add to cart"), link("1", "Reviews", "https://shop.example/r")]
            self.page.doc += 1.0                       # Cloudflare loads the real page as a new document


def cloudflare_page() -> FakePage:
    return FakePage("https://shop.example/item", "Just a moment...", "Enable JavaScript and cookies to continue",
                    [link("0", "Cloudflare", "https://www.cloudflare.com/")])


@pytest.mark.parametrize(("url", "title", "text"), [
    ("https://shop.example/", "Just a moment...", "Enable JavaScript and cookies to continue"),
    ("https://shop.example/", "Just a moment…", ""),
    ("https://shop.example/", "", CF_TEXT),
    ("https://shop.example/", "", "Verifying you are human. shop.example needs to review the security of your "
                                  "connection before proceeding."),
])
def test_cloudflare_checks_are_soft(url: str, title: str, text: str) -> None:
    assert bl.browser_blocker(url, title, text) == "browser_challenge"
    assert bl.challenge_kind(url, title, text) == "soft"


@pytest.mark.parametrize(("url", "title", "text"), [
    ("https://www.google.com/sorry/index?continue=x", "", "Our systems have detected unusual traffic"),
    ("https://www.ebay.com/splashui/challenge?ap=1", "Security Measure", ""),
    ("https://shop.example/", "Attention Required! | Cloudflare", "Sorry, you have been blocked"),
    ("https://shop.example/item", "Access Denied", "You don't have permission to access this server."),
    ("https://www.amazon.com/errors/validateCaptcha", "Robot Check", ""),
    ("https://www.zillow.com/homes/", "Pardon Our Interruption", ""),
])
def test_hard_blocks_are_hard(url: str, title: str, text: str) -> None:
    assert bl.challenge_kind(url, title, text) == "hard"


def test_ordinary_pages_have_no_kind() -> None:
    assert bl.challenge_kind("https://shop.example/", "Just a moment of calm", "Breathe in.") == ""
    assert bl.challenge_kind("", "", "") == ""


def test_a_check_that_clears_in_five_seconds_is_waited_out() -> None:
    page = cloudflare_page()
    clock = Clock(page, clear_at=5.0)
    p = bl._Page(page, clock=clock, sleep=clock.sleep)
    snap = p.snapshot()
    assert p.blocked == ""
    assert [c.label for c in snap.elements] == ["Add to cart", "Reviews"]   # the real page, not the check
    assert 5.0 <= sum(clock.slept) <= bl.CHALLENGE_SETTLE_SECS
    assert snap.title == "Blue mug"
    # The recorded document is the real page's, so a click on it is not refused as stale.
    reviews = next(c for c in snap.elements if c.label == "Reviews")
    said = p.click_candidate(reviews)
    assert said.startswith("clicked"), said
    assert page.clicks == ["1"]


def test_a_check_that_never_clears_blocks_after_a_bounded_wait() -> None:
    page = cloudflare_page()
    clock = Clock(page, clear_at=None)
    p = bl._Page(page, clock=clock, sleep=clock.sleep)
    p.snapshot()
    assert p.blocked and "shop.example" in p.blocked
    assert bl.CHALLENGE_SETTLE_SECS - bl.CHALLENGE_POLL_SECS <= sum(clock.slept) <= bl.CHALLENGE_SETTLE_SECS
    assert 7.0 <= bl.CHALLENGE_SETTLE_SECS <= 10.0
    # A second look at the same stuck check does not wait again: the plan is already stopping.
    before = len(clock.slept)
    p.snapshot()
    assert p.blocked and len(clock.slept) == before


def test_the_wait_is_bounded_even_when_the_clock_does_not_move() -> None:
    page = cloudflare_page()
    sleeps: list[float] = []
    p = bl._Page(page, clock=lambda: 5.0, sleep=sleeps.append)
    p.snapshot()
    assert p.blocked
    assert len(sleeps) <= int(bl.CHALLENGE_SETTLE_SECS / bl.CHALLENGE_POLL_SECS) + 1


@pytest.mark.parametrize(("url", "title", "body"), [
    ("https://www.google.com/sorry/index?continue=x", "", "Our systems have detected unusual traffic"),
    ("https://shop.example/", "Attention Required! | Cloudflare", "Sorry, you have been blocked"),
    ("https://shop.example/item", "Access Denied", "You don't have permission to access this server."),
    ("https://www.ebay.com/splashui/challenge?ap=1", "Security Measure", "Please verify yourself"),
])
def test_hard_blocks_stop_at_once(url: str, title: str, body: str) -> None:
    page = FakePage(url, title, body, [button("0", "Submit")])
    clock = Clock(page, clear_at=None)
    p = bl._Page(page, clock=clock, sleep=clock.sleep)
    p.snapshot()
    assert p.blocked
    assert clock.slept == []


def test_an_ordinary_page_never_waits() -> None:
    page = FakePage("https://shop.example/", "Eve Shop", "Welcome", [button("0", "Search")])
    clock = Clock(page, clear_at=None)
    p = bl._Page(page, clock=clock, sleep=clock.sleep)
    p.snapshot()
    assert p.blocked == "" and clock.slept == []


def run(lane: bl.BrowserLane, coro_fn: Any) -> Any:
    async def go() -> Any:
        try:
            return await coro_fn()
        finally:
            await lane.close()
    return asyncio.run(go())


def test_a_plan_on_a_check_that_clears_runs_on_the_real_page(tmp_path: Any) -> None:
    page = cloudflare_page()
    clock = Clock(page, clear_at=5.0)
    lane = lane_on(page, tmp_path)
    lane._page_options = {"clock": clock, "sleep": clock.sleep}
    plan = {"steps": [{"kind": "click", "target": "the Reviews link", "label_hint": "Reviews"}],
            "final_say": "Opened.", "success": {"kind": "text_visible", "value": "Blue mug"}}
    result = run(lane, lambda: lane.run_plan(plan, "open the blue mug reviews on shop.example"))
    assert result["status"] != "blocked", result
    assert page.clicks == ["1"]


def test_page_text_waits_out_a_check_that_clears(tmp_path: Any) -> None:
    page = cloudflare_page()
    clock = Clock(page, clear_at=5.0)
    lane = lane_on(page, tmp_path)
    lane._page_options = {"clock": clock, "sleep": clock.sleep}
    read = run(lane, lane.page_text)
    assert read["blocked"] == "", read
    assert "Blue mug" in read["text"]
    assert sum(clock.slept) <= bl.CHALLENGE_SETTLE_SECS + bl.READ_WAIT_SECS


def test_page_text_blocks_a_hard_block_without_waiting(tmp_path: Any) -> None:
    page = FakePage("https://www.google.com/sorry/index?continue=x", "", "Our systems have detected unusual traffic")
    clock = Clock(page, clear_at=None)
    lane = lane_on(page, tmp_path)
    lane._page_options = {"clock": clock, "sleep": clock.sleep}
    read = run(lane, lane.page_text)
    assert "google.com" in read["blocked"]
    assert clock.slept == []
