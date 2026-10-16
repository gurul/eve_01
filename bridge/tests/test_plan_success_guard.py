"""A plan is DONE only when something observable says so (plan_contract.py, plan_executor.py).

Guards for a browser search, each a way a plan used to report `complete` without having shown anything:

* a browser plan's success is derived by code from the ORIGINAL REQUEST (browser_intent.expected_outcome), never
  from the planner's choice: "search duck duck go for cats" is not done by a Google search;
* that derived check is read once before step 1, and one that already holds is rejected: it proves nothing;
* it must be read true at the end.

Only plans with the derived check are held to this (review F1, 2026-10-16): a Mac/app plan with no check, or with
a check that already holds, ends as it always did (test_plan_scope).

Fakes only: test_fast_lane's senses/effectors, and a page whose URL is whatever the plan last opened.
"""

from __future__ import annotations

from typing import Any, Optional

import pytest
from test_browser_intent import pick
from test_fast_lane import Clock, Effectors, Senses, calendar, snap

from eve_bridge import browser_intent as bi
from eve_bridge import plan_contract as pc
from eve_bridge.plan_executor import run_plan


def raw(*steps: dict[str, Any], success: Optional[dict[str, str]] = None, **extra: Any) -> dict[str, Any]:
    full = [{"kind": "click", "target": "", "label_hint": "", "text": "", "key": "", "expect": None,
             "consequential": False, **s} for s in steps]
    return {"needs_eyes": False, "why": "", "steps": full, "final_say": "All set.", "success": success, **extra}


class Desk:
    def __init__(self, front: str = "Calendar") -> None:
        self.front, self.opened = front, []

    def open_app(self, name: str) -> str:
        self.opened.append(name)
        self.front = name
        return f"opened {name} (frontmost after 0.4 s)"

    def open_url(self, url: str) -> str:
        self.opened.append(url)
        return f"opened {url}"

    def frontmost_app(self) -> str:
        return self.front


def go(p: pc.Plan, senses: Any, eff: Effectors, desk: Optional[Desk] = None, **kw: Any):
    desk = desk or Desk()
    return run_plan(p, senses=senses, effectors=eff, asker=None, open_app=desk.open_app, open_url=desk.open_url,
                    frontmost_app=desk.frontmost_app, clock=Clock(), decide="keyword", **kw)


class Page:
    """A browser page: its URL is the last one opened, its text is the set for that URL."""

    def __init__(self, url: str = "about:blank", text: Optional[dict[str, set[str]]] = None) -> None:
        self.url, self.text = url, text or {}
        self.desk = Desk(front="browser")
        self.desk.open_url = self.open_url  # type: ignore[method-assign]

    def open_url(self, url: str) -> str:
        self.desk.opened.append(url)
        self.url = url
        return f"opened {url}"

    def snapshot(self):
        return snap(app="browser", title=self.url)

    def text_visible(self, s: str) -> bool:
        return s in self.text.get(self.url, set())

    def current_url(self) -> str:
        return self.url

    def focused(self):
        return None

    def frontmost_pid(self) -> int:
        return 42

    def screen_changed(self):
        return True


def outcome(kind: str, host: Optional[str], query: Optional[str], n: Optional[int] = None) -> bi.RequestOutcome:
    return bi.RequestOutcome(kind, host, query, n, 0.9, "laya")


def browser_plan(request: str, url: str, *, success: Optional[dict[str, str]] = None, intent: Any) -> pc.Plan:
    return pc.parse_plan(raw({"kind": "open_url", "target": url}, success=success, app="browser"), request,
                         intent=intent)


# ---- (a) a check that holds only after the steps ----------------------------------------------------------


def test_a_success_check_that_holds_only_after_the_steps_is_complete() -> None:
    p = pc.parse_plan(raw({"kind": "open_app", "target": "Calendar"},
                          success={"kind": "text_visible", "value": "Week"}), "open calendar")
    senses = Senses([calendar(), calendar()], visible=[set(), {"Week"}])
    r = go(p, senses, Effectors(), Desk(front="Finder"))
    assert r.status == "complete" and r.sentence == "All set.", r.to_dict()


# ---- (b) a check that already holds before step 1 proves nothing --------------------------------------------


# The Mac-plan versions of these two tests expected a text_visible check to be rejected; that was the F1 bug,
# and they now live in test_plan_scope with the opposite expectation. These are the browser-search versions.


def test_a_success_check_already_true_before_step_1_is_rejected() -> None:
    p = browser_plan("search duck duck go for cats", "https://duckduckgo.com/?q=cats",
                     intent=lambda r: outcome("search", "duckduckgo.com", "cats"))
    page = Page(url="https://duckduckgo.com/?q=Cats")                  # the results were open before anything was done
    r = go(p, page, Effectors(), page.desk, planned_for="browser")
    assert r.status == "partial" and r.reason == "success_already_held", r.to_dict()
    assert r.success_rejected and r.to_dict()["success_rejected"] is True
    assert any("already holds before step 1" in line for line in r.log_lines)


def test_a_resumed_plan_keeps_the_rejection_it_was_handed() -> None:
    p = pc.parse_plan(raw({"kind": "open_url", "target": "https://example.com/"},
                          {"kind": "open_url", "target": "https://duckduckgo.com/?q=cats"}, app="browser"),
                      "search duck duck go for cats", intent=lambda r: outcome("search", "duckduckgo.com", "cats"))
    page = Page()
    r = go(p, page, Effectors(), page.desk, planned_for="browser", start=1, success_rejected=True)
    assert r.status == "partial" and r.reason == "success_already_held", r.to_dict()


def test_the_pre_check_does_not_wait_for_a_check_that_is_false() -> None:
    p = pc.parse_plan(raw({"kind": "open_url", "target": "https://example.com/"},
                          {"kind": "open_url", "target": "https://www.google.com/search?q=dogs"}, app="browser"),
                      "search duck duck go for cats", intent=lambda r: outcome("search", "duckduckgo.com", "cats"))
    fresh, resumed = Effectors(), Effectors()
    page = Page()
    go(p, page, fresh, page.desk, planned_for="browser")              # start 0: reads the check before step 1
    page = Page(url="https://example.com/")
    go(p, page, resumed, page.desk, planned_for="browser", start=1)  # start 1: no pre-check
    assert fresh.settles.count(0.4) == resumed.settles.count(0.4)     # the pre-check added no polling


# ---- (c) a browser search is checked from the request -------------------------------------------------------


def test_url_query_is_code_only_and_validated() -> None:
    e = pc.url_query_expect("duckduckgo.com", "  Cats  ")
    assert e.kind == "url_query" and e.host == "duckduckgo.com" and e.value == "cats"
    assert pc.url_query_expect(None, "cats").host == ""               # any known search engine
    for host, query in (("duckduckgo.com", ""), ("evil.example", "cats"), ("duckduckgo.com/x", "cats"),
                        ("duckduckgo.com", "x" * (pc.MAX_TEXT_CHARS + 1))):
        with pytest.raises(pc.PlanError):
            pc.url_query_expect(host, query)
    # The planner cannot choose it: a url_query success from the plan is not a plan.
    with pytest.raises(pc.PlanError):
        pc.parse_plan(raw({"kind": "open_url", "target": "https://www.google.com/"},
                          success={"kind": "url_query", "value": "cats"}), "search for cats")
    assert "url_query" not in pc.PLAN_SCHEMA["properties"]["success"]["properties"]["kind"]["enum"]


@pytest.mark.parametrize("url, host, query, ok", [
    ("https://duckduckgo.com/?q=cats", "duckduckgo.com", "cats", True),
    ("https://duckduckgo.com/?q=Alan+%20Turing&ia=web", "duckduckgo.com", "alan turing", True),
    ("https://www.google.com/search?q=cats", "duckduckgo.com", "cats", False),          # the wrong engine
    ("https://duckduckgo.com/?q=dogs", "duckduckgo.com", "cats", False),                # the wrong query
    ("https://duckduckgo.com/?q=cats+and+dogs", "duckduckgo.com", "cats", False),       # equal, not contains
    ("https://duckduckgo.com.evil.example/?q=cats", "duckduckgo.com", "cats", False),   # a lookalike host
    ("https://duckduckgo.com/?x=cats", "duckduckgo.com", "cats", False),                # not the query key
    ("https://www.youtube.com/results?search_query=lofi+beats", "youtube.com", "lofi beats", True),
    ("https://en.wikipedia.org/w/index.php?search=Alan+Turing", "wikipedia.org", "alan turing", True),
    ("https://www.google.com/search?q=cats", None, "cats", True),                        # no site: any engine
    ("https://duckduckgo.com/?q=cats", None, "cats", True),
    ("https://www.youtube.com/results?search_query=cats", None, "cats", False),         # a site, not an engine
    ("about:blank", None, "cats", False),
])
def test_url_query_matches_host_and_the_literal_query(url: str, host: Optional[str], query: str, ok: bool) -> None:
    assert pc.url_query_matches(url, pc.url_query_expect(host, query)) is ok


def test_duck_duck_go_by_google_is_not_done() -> None:
    request = "search duck duck go for cats"
    intent = lambda r: bi.expected_outcome(r, engine=pick("search"), environ={})  # noqa: E731
    p = browser_plan(request, "https://www.google.com/search?q=cats",
                     success={"kind": "text_visible", "value": "cats"}, intent=intent)
    assert p.success is not None and p.success.kind == "url_query" and p.success.host == "duckduckgo.com"
    page = Page(text={"https://www.google.com/search?q=cats": {"cats"}})  # the planner's own check would hold
    r = go(p, page, Effectors(), page.desk, planned_for="browser")
    assert r.status == "partial" and "cats" in r.reason, r.to_dict()


def test_duck_duck_go_by_duck_duck_go_is_done() -> None:
    request = "search duck duck go for cats"
    intent = lambda r: bi.expected_outcome(r, engine=pick("search"), environ={})  # noqa: E731
    p = browser_plan(request, "https://duckduckgo.com/?q=Cats", intent=intent)  # the planner gave no success
    page = Page()
    r = go(p, page, Effectors(), page.desk, planned_for="browser")
    assert r.status == "complete" and r.sentence == "All set.", r.to_dict()


def test_the_request_check_replaces_the_planners_choice() -> None:
    # The planner checked for a page it chose; the request said github.
    p = browser_plan("search git hub for laya", "https://www.google.com/search?q=laya",
                     success={"kind": "url_is", "value": "https://www.google.com/search?q=laya"},
                     intent=lambda r: outcome("search", "github.com", "laya"))
    page = Page()
    r = go(p, page, Effectors(), page.desk, planned_for="browser")
    assert r.status == "partial", r.to_dict()


def test_a_request_check_already_true_before_step_1_is_rejected() -> None:
    p = browser_plan("search duck duck go for cats", "https://duckduckgo.com/?q=cats",
                     intent=lambda r: outcome("search", "duckduckgo.com", "cats"))
    page = Page(url="https://duckduckgo.com/?q=cats")                  # already on that results page
    r = go(p, page, Effectors(), page.desk, planned_for="browser")
    assert r.status == "partial" and r.reason == "success_already_held", r.to_dict()


def test_no_outcome_falls_back_to_the_planners_check() -> None:
    url = "https://example.com/pricing"
    for intent in (lambda r: None, lambda r: outcome("open_site", "github.com", None), lambda r: outcome("none", None, None)):
        p = browser_plan("show me example's pricing", url, success={"kind": "url_is", "value": url}, intent=intent)
        assert p.success is not None and p.success.kind == "url_is"
        page = Page()
        assert go(p, page, Effectors(), page.desk, planned_for="browser").status == "complete"
    p = browser_plan("show me example's pricing", url, intent=lambda r: None)
    page = Page()
    # No derived check and none from the planner: as before F1's rule, it completes (test_plan_scope).
    assert go(p, page, Effectors(), page.desk, planned_for="browser").status == "complete"


def test_only_a_browser_plan_asks_for_the_request_outcome() -> None:
    asked: list[str] = []

    def intent(r: str) -> Optional[bi.RequestOutcome]:
        asked.append(r)
        return outcome("search", "duckduckgo.com", "cats")

    pc.parse_plan(raw({"kind": "open_app", "target": "Notes"}, app="Notes"), "search duck duck go for cats",
                  intent=intent)
    assert asked == []
    p = pc.parse_plan(raw({"kind": "open_url", "target": "https://duckduckgo.com/?q=cats"}, app="browser"),
                      "search duck duck go for cats", intent=intent)
    assert asked == ["search duck duck go for cats"] and p.success is not None and p.success.kind == "url_query"


def test_a_failing_intent_is_an_abstention_not_a_bad_plan() -> None:
    def boom(r: str) -> None:
        raise RuntimeError("metal went away")
    p = browser_plan("search for cats", "https://www.google.com/search?q=cats", intent=boom)
    assert p.success is None


def test_the_derived_check_survives_the_round_trip_the_lane_makes() -> None:
    # computer_agent sends plan_to_dict(plan) + app to the lane, which parses it again with the same request.
    intent = lambda r: outcome("search", "duckduckgo.com", "cats")  # noqa: E731
    first = pc.parse_plan(raw({"kind": "open_url", "target": "https://duckduckgo.com/?q=cats"}),
                          "search duck duck go for cats", intent=intent)
    again = pc.parse_plan({**pc.plan_to_dict(first), "app": "browser"}, "search duck duck go for cats", intent=intent)
    assert again.success == pc.url_query_expect("duckduckgo.com", "cats")


# ---- a resume after the human's yes keeps the first call's rejection --------------------------------------

def test_a_resumed_plan_is_handed_back_the_rejection(tmp_path) -> None:
    import asyncio

    from test_agent_browser_lane import WEB_ASK, WEB_DONE, _browser_agent

    seen: list[bool] = []
    a, lane, _ = _browser_agent(tmp_path, [{**WEB_ASK, "success_rejected": True}, WEB_DONE], answers=["yes"])
    plain = lane.run_plan

    async def run_plan(plan, request, start=0, approved=None, success_rejected=False):
        seen.append(success_rejected)
        return await plain(plan, request, start=start, approved=approved)

    lane.run_plan = run_plan
    asyncio.run(a.run_in_browser("buy the thing"))
    assert seen == [False, True]


def test_the_desktop_worker_passes_the_rejection_to_the_helpers() -> None:
    from eve_bridge import desktop_worker

    got: dict = {}

    class Helpers:
        def bind(self, *a): pass
        def begin(self): pass
        def timing_ms(self): return {}
        def run_plan(self, plan, request, **kw):
            got.update(kw)
            return {"status": "partial"}

    desktop_worker.run_plan({"plan": {"steps": []}, "request": "x", "start": 1, "success_rejected": True}, Helpers())
    assert got["success_rejected"] is True
