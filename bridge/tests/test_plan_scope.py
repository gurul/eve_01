"""The strict success rule is for browser searches only (plan_executor.py, review F1 of feat/browser-guards).

A plan whose success is the code-derived `url_query` check must read it true at the end, and one that already
held before step 1 proves nothing. Every other plan — a Mac/app plan, or a browser plan with no derived check —
ends the way it always did: complete when every step applied and its own check (if any) did not read false. The
planner is told to give no check when unsure, and an app_frontmost or text_visible check is often already true
once `planned_for` has brought the app back, so applying the strict rule there handed routine plans to the slow
screenshot loop.

Fakes only: test_fast_lane's senses/effectors and test_plan_success_guard's page.
"""

from __future__ import annotations

from test_fast_lane import Effectors, Senses, calendar
from test_plan_success_guard import Desk, Page, browser_plan, go, outcome, raw

from eve_bridge import plan_contract as pc

# ---- Mac / app plans: as before --------------------------------------------------------------------------


def test_a_mac_plan_with_no_success_check_is_complete() -> None:
    p = pc.parse_plan(raw({"kind": "open_app", "target": "Calendar"}), "open calendar")
    r = go(p, Senses([calendar()]), Effectors(), Desk(front="Finder"))
    assert r.status == "complete" and r.sentence == "All set." and r.reason == "", r.to_dict()
    assert not r.success_rejected


def test_a_mac_check_already_true_before_step_1_still_completes() -> None:
    p = pc.parse_plan(raw({"kind": "open_app", "target": "Calendar"},
                          success={"kind": "text_visible", "value": "Week"}), "open calendar")
    r = go(p, Senses([calendar()], visible=[{"Week"}]), Effectors(), Desk(front="Finder"))
    assert r.status == "complete" and not r.success_rejected, r.to_dict()
    assert not any("already holds" in line for line in r.log_lines)


def test_an_app_frontmost_check_true_after_planned_for_completes() -> None:
    # planned_for brought Calendar back before step 1, so app_frontmost "Calendar" holds from the start.
    p = pc.parse_plan(raw({"kind": "click", "target": "week", "label_hint": "Week"},
                          success={"kind": "app_frontmost", "value": "Calendar"}), "show the week in calendar")
    desk = Desk(front="Finder")
    r = go(p, Senses([calendar(), calendar(True)], changed=[True]), Effectors(), desk, planned_for="Calendar")
    assert desk.opened == ["Calendar"] and r.status == "complete", r.to_dict()


def test_a_mac_check_that_reads_false_at_the_end_is_still_partial() -> None:
    p = pc.parse_plan(raw({"kind": "open_app", "target": "Calendar"},
                          success={"kind": "text_visible", "value": "Week"}), "open calendar")
    r = go(p, Senses([calendar()], visible=[set()]), Effectors(), Desk(front="Finder"))
    assert r.status == "partial" and "Week" in r.reason, r.to_dict()


def test_a_mac_plan_ignores_a_rejection_handed_back_on_resume() -> None:
    p = pc.parse_plan(raw({"kind": "open_app", "target": "Calendar"}, {"kind": "open_app", "target": "Notes"}),
                      "open calendar then notes")
    r = go(p, Senses([calendar()]), Effectors(), Desk(front="Finder"), start=1, success_rejected=True)
    assert r.status == "complete", r.to_dict()


# ---- browser plans with no derived check: as before ------------------------------------------------------


def test_a_browser_plan_with_no_derived_check_and_no_check_is_complete() -> None:
    p = browser_plan("show me example's pricing", "https://example.com/pricing", intent=lambda r: None)
    page = Page()
    r = go(p, page, Effectors(), page.desk, planned_for="browser")
    assert r.status == "complete" and r.sentence == "All set.", r.to_dict()


def test_a_browser_planners_check_already_true_still_completes() -> None:
    url = "https://example.com/pricing"
    p = browser_plan("show me example's pricing", url, success={"kind": "url_is", "value": url},
                     intent=lambda r: outcome("open_site", "github.com", None))
    page = Page(url=url)
    r = go(p, page, Effectors(), page.desk, planned_for="browser")
    assert r.status == "complete" and not r.success_rejected, r.to_dict()


# ---- browser plans with the derived url_query check: strict ----------------------------------------------


def test_a_derived_check_already_true_before_step_1_is_rejected() -> None:
    p = browser_plan("search duck duck go for cats", "https://duckduckgo.com/?q=cats",
                     intent=lambda r: outcome("search", "duckduckgo.com", "cats"))
    page = Page(url="https://duckduckgo.com/?q=cats")
    r = go(p, page, Effectors(), page.desk, planned_for="browser")
    assert r.status == "partial" and r.reason == "success_already_held" and r.success_rejected, r.to_dict()


def test_a_derived_check_false_at_the_end_is_partial() -> None:
    p = browser_plan("search duck duck go for cats", "https://www.google.com/search?q=cats",
                     intent=lambda r: outcome("search", "duckduckgo.com", "cats"))
    page = Page()
    r = go(p, page, Effectors(), page.desk, planned_for="browser")
    assert r.status == "partial" and "cats" in r.reason, r.to_dict()


def test_a_resumed_browser_search_keeps_the_rejection_it_was_handed() -> None:
    p = pc.parse_plan(raw({"kind": "open_url", "target": "https://example.com/"},
                          {"kind": "open_url", "target": "https://duckduckgo.com/?q=cats"}, app="browser"),
                      "search duck duck go for cats", intent=lambda r: outcome("search", "duckduckgo.com", "cats"))
    page = Page()
    r = go(p, page, Effectors(), page.desk, planned_for="browser", start=1, success_rejected=True)
    assert r.status == "partial" and r.reason == "success_already_held", r.to_dict()


def test_a_derived_check_true_only_after_the_steps_is_complete() -> None:
    p = browser_plan("search duck duck go for cats", "https://duckduckgo.com/?q=cats",
                     intent=lambda r: outcome("search", "duckduckgo.com", "cats"))
    page = Page()
    r = go(p, page, Effectors(), page.desk, planned_for="browser")
    assert r.status == "complete" and r.ledger[-1].effect == "confirmed", r.to_dict()
