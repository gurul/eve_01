"""A bot-check page ends the browser lane as "blocked", with a line for the owner, and nothing is clicked.

browser_lane.browser_blocker is ported from LayaBrowse (MIT, goals.py browser_blocker). The negatives matter as
much as the positives: an ordinary page that only mentions "access denied" must never stop a task.
"""

from __future__ import annotations

import asyncio
from typing import Any

import pytest

from browser_fakes import FakePage, button, lane_on, link
from eve_bridge import browser_lane as bl

CF_TEXT = "Checking your browser before accessing example.com. This process is automatic."


@pytest.mark.parametrize(("url", "title", "text"), [
    ("https://www.google.com/sorry/index?continue=https://www.google.com/search%3Fq%3Dramen", "", "Our systems have detected unusual traffic"),
    ("https://ipv4.google.com/sorry/index?q=x", "", ""),
    ("https://www.ebay.com/splashui/challenge?ap=1&appName=orch", "Security Measure", ""),
    ("https://shop.example/", "Just a moment...", "Enable JavaScript and cookies to continue"),
    ("https://shop.example/", "Just a moment…", ""),
    ("https://shop.example/", "", CF_TEXT),
    ("https://shop.example/", "", "Verifying you are human. shop.example needs to review the security of your connection before proceeding."),
    ("https://www.amazon.com/errors/validateCaptcha", "Robot Check", "Enter the characters you see below"),
    ("https://shop.example/item", "Access Denied", "You don't have permission to access this server."),
    ("https://www.zillow.com/homes/", "Pardon Our Interruption", "As you were browsing something about your browser made us think you were a bot."),
    ("https://shop.example/", "Attention Required! | Cloudflare", "Sorry, you have been blocked"),
])
def test_challenges_are_recognised(url: str, title: str, text: str) -> None:
    assert bl.browser_blocker(url, title, text) == "browser_challenge"


@pytest.mark.parametrize(("url", "title", "text"), [
    ("https://www.google.com/search?q=sorry", "sorry - Google Search", "About 1,000,000 results"),
    ("https://www.google.com/", "Google", "Google Search I'm Feeling Lucky"),
    ("https://shop.example/sorry/", "Sorry, we're closed today", "Come back tomorrow."),
    ("https://docs.aws.amazon.com/s3/troubleshoot", "Troubleshoot Access Denied (403 Forbidden) errors in Amazon S3",
     "If you get an access denied error, check the bucket policy."),
    ("https://en.wikipedia.org/wiki/Robot", "Robot - Wikipedia", "A robot is a machine capable of carrying out actions"),
    ("https://news.ycombinator.com/", "Hacker News", "1. Show HN: robot check for my homelab"),
    ("https://blog.cloudflare.com/x", "How our challenge pages work", "x" * 600 + " Checking your browser before accessing"),
    ("https://shop.example/", "Just a moment of calm: a meditation guide", "Breathe in."),
    ("https://shop.example/cart", "Your cart", "Access denied? Sign in again to see your saved items."),
    ("about:blank", "", ""),
    ("", "", ""),
])
def test_ordinary_pages_are_never_challenges(url: str, title: str, text: str) -> None:
    assert bl.browser_blocker(url, title, text) == ""


def test_the_owner_line_names_the_site_and_says_what_to_do() -> None:
    line = bl.blocked_line("https://www.google.com/sorry/index?continue=secret-query")
    assert "google.com" in line and "secret-query" not in line
    assert "bot check" in line and "ask me again" in line
    assert "that site" in bl.blocked_line("")


def challenge_page() -> FakePage:
    return FakePage("https://www.google.com/sorry/index?continue=x", "", "Our systems have detected unusual traffic "
                    "from your computer network.", [{"id": "0", "role": "checkbox", "name": "I'm not a robot",
                                                    "value": "unchecked", "x": 10, "y": 10, "w": 20, "h": 20,
                                                    "enabled": True, "href": ""}, button("1", "Submit")])


def ordinary_page() -> FakePage:
    return FakePage("https://shop.example/", "Eve Shop", "Welcome to the shop. Access denied pages are rare here.",
                    [link("0", "Top stories", "https://shop.example/news"), button("1", "Search")])


def test_snapshot_marks_a_challenge_and_leaves_an_ordinary_page_alone() -> None:
    p = bl._Page(challenge_page())
    p.snapshot()
    assert p.blocked and "google.com" in p.blocked
    q = bl._Page(ordinary_page())
    q.snapshot()
    assert q.blocked == ""


def test_effectors_touch_nothing_on_a_challenge_page() -> None:
    page = challenge_page()
    p = bl._Page(page)
    snap = p.snapshot()
    robot = next(c for c in snap.elements if c.label == "I'm not a robot")
    assert p.click_candidate(robot).startswith("refused: blocked:")
    assert p.focus_and_type(robot, "x").startswith("refused: blocked:")
    assert p.press("return").startswith("refused: blocked:")
    assert page.clicks == [] and page.keys == []


def run(lane: bl.BrowserLane, coro_fn: Any) -> Any:
    async def go() -> Any:
        try:
            return await coro_fn()
        finally:
            await lane.close()
    return asyncio.run(go())


CLICK_ROBOT = {"steps": [{"kind": "click", "target": "the I'm not a robot checkbox", "label_hint": "I'm not a robot"}],
               "final_say": "Done.", "success": {"kind": "text_visible", "value": "results"}}


def test_a_plan_on_a_challenge_page_ends_blocked_without_a_click(tmp_path: Any) -> None:
    page = challenge_page()
    lane = lane_on(page, tmp_path)
    result = run(lane, lambda: lane.run_plan(CLICK_ROBOT, "search google for ramen"))
    assert result["status"] == "blocked", result
    assert result["reason"] == "browser_challenge"
    assert "google.com" in result["sentence"] and "bot check" in result["sentence"]
    assert page.clicks == [] and page.keys == []


def test_a_plan_that_lands_on_a_challenge_mid_run_ends_blocked(tmp_path: Any) -> None:
    sorry = "https://www.google.com/sorry/index?continue=q"
    page = FakePage("https://shop.example/", "Eve Shop", "Welcome", [button("0", "Search")],
                    routes={sorry: ("", "Our systems have detected unusual traffic", [button("0", "Submit")])})
    plan = {"steps": [{"kind": "open_url", "target": sorry},
                      {"kind": "click", "target": "the Submit button", "label_hint": "Submit"}],
            "final_say": "Done.", "success": {"kind": "text_visible", "value": "results"}}
    lane = lane_on(page, tmp_path)
    lane._plain_http = {sorry: sorry}
    result = run(lane, lambda: lane.run_plan(plan, "search google for ramen"))
    assert result["status"] == "blocked", result
    assert page.clicks == []


def test_an_ordinary_plan_is_not_blocked(tmp_path: Any) -> None:
    page = ordinary_page()
    lane = lane_on(page, tmp_path)
    plan = {"steps": [{"kind": "click", "target": "the Search button", "label_hint": "Search"}],
            "final_say": "Searched.", "success": {"kind": "text_visible", "value": "Welcome"}}
    result = run(lane, lambda: lane.run_plan(plan, "click search on the shop site"))
    assert result["status"] != "blocked", result
    assert page.clicks == ["1"]


def test_outline_and_page_text_carry_the_owner_line(tmp_path: Any) -> None:
    lane = lane_on(challenge_page(), tmp_path)

    async def both() -> tuple[dict[str, Any], dict[str, Any]]:
        return await lane.outline(), await lane.page_text()

    outline, page = run(lane, both)
    assert "google.com" in outline["blocked"]
    assert page["blocked"] == outline["blocked"]
    lane2 = lane_on(ordinary_page(), tmp_path)
    outline2 = run(lane2, lane2.outline)
    assert outline2["blocked"] == ""


# ---- the agent: a bot check is said and never handed to Codex or the screenshot planner ----------------------

def test_the_agent_says_the_owner_line_and_never_hands_off(tmp_path: Any) -> None:
    from test_agent_browser_lane import WEB_PLAN, FakeLane, _cfg
    from test_agent_plan_once import PlanWorker, _plan_response
    from test_computer_agent import FakeClient, _agent

    line = bl.blocked_line("https://www.google.com/sorry/index?continue=x")
    stopped = {"status": "blocked", "next_index": 1, "reason": "browser_challenge", "confirm": "",
               "sentence": line, "ledger": []}
    client = FakeClient([_plan_response(WEB_PLAN)])            # a second (hand-off) call would raise
    worker, lane = PlanWorker([]), FakeLane([stopped])
    a, _ = _agent(client, worker, tmp_path, config=_cfg(tmp_path), browser=lane)
    assert asyncio.run(a.run("open google news and show me the top headline")) == line
    assert client.calls == 1 and worker.observed == 0 and worker.runs == []


def test_the_agent_stops_before_planning_when_the_outline_is_a_bot_check(tmp_path: Any) -> None:
    from test_agent_browser_lane import FakeLane, _cfg
    from test_agent_plan_once import PlanWorker
    from test_computer_agent import FakeClient, _agent

    line = bl.blocked_line("https://news.example/")

    class Blocked(FakeLane):
        async def outline(self) -> dict[str, Any]:
            self.outlines += 1
            return {"app": "browser", "lines": ["link: Verify"], "title": "Just a moment...",
                    "url": "https://news.example/", "blocked": line}

    client = FakeClient([])                                     # any model call raises
    worker, lane = PlanWorker([]), Blocked([])
    a, _ = _agent(client, worker, tmp_path, config=_cfg(tmp_path), browser=lane)
    assert asyncio.run(a.run("open google news and show me the top headline")) == line
    assert client.calls == 0 and lane.runs == [] and worker.observed == 0
