"""The plan contract (plan_contract.py) and the executor that walks it (plan_executor.py).

Fakes are test_fast_lane's and test_jev_step's: nothing here opens an app, presses a key or a socket.
"""

from __future__ import annotations

from typing import Any

import pytest
from test_fast_lane import Clock, Effectors, Senses, calendar, cand, message_compose, notes_search, snap
from test_jev_step import asker_saying

from eve_bridge import plan_contract as pc
from eve_bridge.plan_executor import run_plan


def plan(*steps: dict[str, Any], request: str = "", **kw: Any) -> pc.Plan:
    full = [{"kind": "click", "target": "", "label_hint": "", "text": "", "key": "", "expect": None,
             "consequential": False, **s} for s in steps]
    return pc.parse_plan({"needs_eyes": False, "why": "", "steps": full, "final_say": kw.pop("final_say", "All set."),
                          "success": kw.pop("success", None)}, request)


class Desk:
    """open_app / open_url / frontmost_app, recorded."""

    def __init__(self, front: str = "Calendar", fail: bool = False) -> None:
        self.front, self.fail, self.opened = front, fail, []

    def open_app(self, name: str) -> str:
        if self.fail:
            raise RuntimeError(f"no app called {name!r}")
        self.opened.append(name)
        self.front = name
        return f"opened {name} (frontmost after 0.4 s)"

    def open_url(self, url: str) -> str:
        self.opened.append(url)
        return f"opened {url} in Safari"

    def frontmost_app(self) -> str:
        return self.front


def go(p: pc.Plan, senses: Senses, eff: Effectors, desk: Desk | None = None, asker: Any = None, **kw: Any):
    desk = desk or Desk()
    return run_plan(p, senses=senses, effectors=eff, asker=asker, open_app=desk.open_app, open_url=desk.open_url,
                    frontmost_app=desk.frontmost_app, clock=Clock(), **kw)


# ---- the contract ------------------------------------------------------------------------------


def test_a_plan_the_code_cannot_fully_read_is_not_a_plan() -> None:
    for bad in ({"steps": []}, {"steps": [{"kind": "drag", "target": "x"}]},
                {"steps": [{"kind": "click", "target": ""}]},
                {"steps": [{"kind": "open_url", "target": "http://plain.example"}]},
                {"steps": [{"kind": "open_url", "target": "https://a b.example"}]},
                {"steps": [{"kind": "press_key", "key": "cmd+q"}]},
                {"steps": [{"kind": "type", "target": "Search", "text": ""}]},
                {"steps": [{"kind": "click", "target": "x"}] * (pc.MAX_STEPS + 1)}, "nope"):
        with pytest.raises(pc.PlanError):
            pc.parse_plan(bad, "anything")


def test_where_text_came_from_is_checked_not_trusted() -> None:
    p = pc.parse_plan({"steps": [{"kind": "type", "target": "Search", "text": "Blue  Adapter", "text_source": "utterance"},
                                 {"kind": "type", "target": "Message", "text": "Running ten minutes late",
                                  "text_source": "utterance"}]},
                      "search my notes for blue adapter and tell Sam I'm late")
    assert [s.text_source for s in p.steps] == ["utterance", "composed"]


def test_text_inside_a_longer_word_is_not_the_owners() -> None:
    """"rm" sits inside "terminal": a raw substring match called it the owner's words and skipped the shell gate
    (review A4, 2026-10-18). Only whole words of the request count."""
    def source(text: str, request: str) -> str:
        return pc.parse_plan({"steps": [{"kind": "type", "target": "Terminal", "text": text}]}, request).steps[0].text_source

    assert source("rm", "open terminal and list the files") == "composed"
    assert source("ls -la", "open terminal and run ls -lah") == "composed"
    assert source("ls -la", "open terminal and run ls -la please") == "utterance"
    assert source("blue adapter", "search for Blue Adapter.") == "utterance"


def test_the_planner_can_decline() -> None:
    p = pc.parse_plan({"needs_eyes": True, "why": "it asks what is on the calendar", "steps": []}, "what's on tomorrow")
    assert p.needs_eyes and p.steps == ()


def test_the_planner_is_shown_labels_and_roles_never_the_title() -> None:
    text = pc.plan_request_text("make it the week view", app="Calendar", outline=["Week (radio button)"], apps=["Calendar"])
    assert "Week (radio button)" in text and "Calendar" in text and "title" not in text.casefold()
    assert set(pc.PLAN_SCHEMA["properties"]["steps"]["items"]["properties"]["kind"]["enum"]) == set(pc.KINDS)


# ---- walking a plan ------------------------------------------------------------------------------


def test_a_two_step_plan_runs_with_no_model_when_the_labels_are_exact() -> None:
    month = calendar()
    eff = Effectors()
    senses = Senses([month, calendar(True), calendar(True), calendar(True)], changed=[True, True])
    p = plan({"kind": "open_app", "target": "Calendar"}, {"kind": "click", "target": "the week view", "label_hint": "Week"})
    r = go(p, senses, eff, Desk(front="Finder"), decide="keyword")
    assert r.status == "complete" and r.sentence == "All set.", r.to_dict()
    assert eff.clicks == ["Week"] and [e.effect for e in r.ledger] == ["confirmed", "confirmed"]
    assert [e.how for e in r.ledger] == ["helper", "keyword"]


def test_the_description_is_tried_when_the_hint_finds_nothing() -> None:
    eff = Effectors()
    asker = asker_saying("Week")
    p = plan({"kind": "click", "target": "the seven day view", "label_hint": "Weekly"})
    r = go(p, Senses([calendar(), calendar(), calendar(True)], changed=[True]), eff, asker=asker)
    assert r.status == "complete" and eff.clicks == ["Week"] and r.ledger[0].how == "jev", r.to_dict()


def test_a_step_no_control_answers_hands_back_with_zero_input() -> None:
    eff = Effectors()
    p = plan({"kind": "click", "target": "the print button"})
    r = go(p, Senses([calendar()]), eff, asker=asker_saying("none"))
    assert r.status == "none" and r.next_index == 0 and eff.clicks == [] and r.reason == "jev_none", r.to_dict()


def test_jev_refusing_the_gates_pick_presses_nothing() -> None:
    eff = Effectors()
    p = plan({"kind": "click", "target": "week", "label_hint": "Week"}, {"kind": "click", "target": "the print button"})
    # the keyword gate proposes Week, Jev's top answer is none: a veto, under the hint and under the description
    r = go(p, Senses([calendar()]), eff, asker=asker_saying("none"), decide="jev")
    assert r.status == "none" and r.next_index == 0 and r.reason == "jev_veto" and eff.clicks == [], r.to_dict()


def test_a_later_failure_is_partial_and_the_ledger_says_what_was_applied() -> None:
    eff = Effectors()
    p = plan({"kind": "click", "target": "week", "label_hint": "Week"}, {"kind": "click", "target": "the print button"})
    r = go(p, Senses([calendar(), calendar(True), calendar(True)], changed=[True]), eff, decide="keyword")
    assert r.status == "partial" and r.next_index == 1 and r.reason == "no_match" and eff.clicks == ["Week"]
    assert [e.effect for e in r.ledger] == ["confirmed", "refused"]


def test_a_click_that_changes_nothing_stops_the_plan() -> None:
    eff = Effectors()
    same = snap(cand("1", "button", "Refresh"), cand("2", "button", "Today"))
    p = plan({"kind": "click", "target": "refresh", "label_hint": "Refresh"}, {"kind": "click", "target": "today", "label_hint": "Today"})
    r = go(p, Senses([same, same, same], changed=[False]), eff, decide="keyword")
    assert r.status == "partial" and eff.clicks == ["Refresh"] and r.ledger[0].effect == "suspected_noop"
    assert r.reason == "the screen did not change"


def test_an_unconfirmed_step_is_never_reported_as_plain_success() -> None:
    eff = Effectors()
    same = snap(cand("1", "button", "Refresh"), cand("2", "button", "Today"))
    p = plan({"kind": "click", "target": "refresh", "label_hint": "Refresh"})
    r = go(p, Senses([same, same], changed=[None]), eff, decide="keyword")
    assert r.status == "complete" and r.ledger[0].effect == "unverifiable"
    assert "couldn't confirm" in r.sentence and r.sentence != "All set."


def test_an_expectation_that_does_not_hold_stops_the_plan() -> None:
    eff = Effectors()
    p = plan({"kind": "click", "target": "week", "label_hint": "Week", "expect": {"kind": "title_contains", "value": "2031"}})
    r = go(p, Senses([calendar(), calendar(True)], changed=[True]), eff, decide="keyword")
    assert r.status == "partial" and "did not see it" in r.reason and eff.clicks == ["Week"]


# ---- what only the human can approve ------------------------------------------------------------------


def test_the_plans_own_flag_stops_before_any_input_and_resumes_on_a_yes() -> None:
    eff = Effectors()
    p = plan({"kind": "click", "target": "week", "label_hint": "Week", "consequential": True})
    r = go(p, Senses([calendar()]), eff, decide="keyword")
    assert r.status == "needs_human" and r.next_index == 0 and eff.clicks == [] and "week" in r.confirm
    r2 = go(p, Senses([calendar(), calendar(True)], changed=[True]), eff, decide="keyword", start=0, approved={0: r.confirm})
    assert r2.status == "complete" and eff.clicks == ["Week"]


def test_a_sensitive_control_needs_the_human_whatever_the_plan_says() -> None:
    eff = Effectors()
    p = plan({"kind": "click", "target": "delete the event", "label_hint": "Delete Event", "consequential": False})
    r = go(p, Senses([calendar()]), eff, decide="keyword")
    assert r.status == "needs_human" and r.confirm == "Delete Event" and eff.clicks == []
    # the human's yes lets exactly that label through, once
    r2 = go(p, Senses([calendar(), calendar()], changed=[True]), eff, decide="keyword", approved={0: r.confirm})
    assert r2.status == "complete" and eff.clicks == ["Delete Event"]


def test_jevs_risky_answer_stops_a_step_the_code_table_does_not_know() -> None:
    eff = Effectors()
    p = plan({"kind": "click", "target": "week", "label_hint": "Week"})
    r = go(p, Senses([calendar()]), eff, asker=asker_saying("Week", risky=0.9))
    assert r.status == "needs_human" and r.confirm == "Week" and eff.clicks == []
    r2 = go(p, Senses([calendar(), calendar(True)], changed=[True]), eff, asker=asker_saying("Week", risky=0.9),
            approved={0: "Week"})
    assert r2.status == "complete" and eff.clicks == ["Week"]


def test_a_dictated_search_may_be_submitted() -> None:
    eff = Effectors()
    p = plan({"kind": "type", "target": "the search field", "text": "blue adapter"}, {"kind": "press_key", "key": "return"},
             request="search my notes for blue adapter")
    r = go(p, Senses([notes_search()]), eff, Desk(front="Notes"))
    assert r.status == "complete" and eff.typed == [("Search", "blue adapter")] and eff.presses == ["return"]


def test_composed_text_is_typed_but_never_submitted_without_a_yes() -> None:
    eff = Effectors()
    p = plan({"kind": "type", "target": "the message field", "text": "Running ten minutes late"},
             {"kind": "press_key", "key": "return"}, request="tell Maya I'm late")
    r = go(p, Senses([message_compose()]), eff, Desk(front="Messages"))
    assert r.status == "needs_human" and r.next_index == 1 and eff.presses == []
    assert eff.typed == [("Message", "Running ten minutes late")] and "Running ten minutes late" in r.confirm


def test_composed_text_into_a_shell_asks_before_the_first_key() -> None:
    eff = Effectors()
    shell = snap(cand("1", "text area", "shell"), app="Terminal", bundle="com.apple.Terminal", title="zsh")
    p = plan({"kind": "type", "target": "the terminal", "text": "rm -rf build"}, request="clean the build folder")
    r = go(p, Senses([shell]), eff, Desk(front="Terminal"))
    assert r.status == "needs_human" and eff.typed == [] and "rm -rf build" in r.confirm


def test_a_secure_field_or_a_dialog_is_never_typed_into() -> None:
    eff = Effectors()
    login = snap(cand("1", "text field", "Password", secure=True), app="Safari", bundle="com.apple.Safari", title="Sign in")
    p = plan({"kind": "type", "target": "the password field", "text": "hunter2"}, request="type hunter2")
    assert go(p, Senses([login]), eff, Desk(front="Safari")).status == "none" and eff.typed == []
    sheet = snap(cand("1", "text field", "Name"), dialog_text="Save changes?")
    assert go(plan({"kind": "type", "target": "name", "text": "x"}, request="type x"), Senses([sheet]), eff).reason == "dialog_open"


def test_among_several_fields_jev_picks_or_nothing_is_typed() -> None:
    form = snap(cand("1", "text field", "First name"), cand("2", "text field", "City"), app="Safari",
                bundle="com.apple.Safari", title="Form")
    p = plan({"kind": "type", "target": "the city field", "text": "Lisbon"}, request="put Lisbon as the city")
    eff = Effectors()
    assert go(p, Senses([form]), eff, Desk(front="Safari"), asker=asker_saying("City")).status == "complete"
    assert eff.typed == [("City", "Lisbon")]
    unsure = Effectors()                                   # a step that names no field: Jev's pick alone, under the cut-offs
    vague = plan({"kind": "type", "target": "the place field", "text": "Lisbon"}, request="put Lisbon as the city")
    r = go(vague, Senses([form]), unsure, Desk(front="Safari"), asker=asker_saying("City", p=0.5))
    assert r.status == "none" and r.reason == "ambiguous_field" and unsure.typed == []


def test_a_checkpoint_hands_back_with_the_ledger() -> None:
    eff = Effectors()
    p = plan({"kind": "click", "target": "week", "label_hint": "Week"}, {"kind": "checkpoint"},
             {"kind": "click", "target": "today", "label_hint": "Today"})
    r = go(p, Senses([calendar(), calendar(True)], changed=[True]), eff, decide="keyword")
    assert r.status == "checkpoint" and r.next_index == 2 and eff.clicks == ["Week"] and len(r.ledger) == 1


def test_an_app_that_does_not_open_stops_the_plan() -> None:
    r = go(plan({"kind": "open_app", "target": "Nonesuch"}, {"kind": "click", "target": "x"}), Senses([calendar()]),
           Effectors(), Desk(fail=True))
    assert r.status == "none" and r.reason == "open_app_failed"


def test_a_dry_run_touches_nothing() -> None:
    eff, desk = Effectors(), Desk(front="Finder")
    p = plan({"kind": "open_app", "target": "Calendar"}, {"kind": "click", "target": "week", "label_hint": "Week"},
             {"kind": "type", "target": "search", "text": "x"}, request="x")
    r = go(p, Senses([calendar()]), eff, desk, decide="keyword", dry_run=True)
    assert r.status == "complete" and r.reason == "dry_run" and eff.clicks == [] and eff.typed == [] and desk.opened == []


def test_the_app_the_plan_was_written_for_is_brought_back_before_the_first_step() -> None:
    eff, desk = Effectors(), Desk(front="Finder")          # the human clicked elsewhere while the planner thought
    p = plan({"kind": "click", "target": "week", "label_hint": "Week"})
    r = go(p, Senses([calendar(), calendar(True)], changed=[True]), eff, desk, decide="keyword", planned_for="Calendar")
    assert desk.opened == ["Calendar"] and r.status == "complete" and eff.clicks == ["Week"]
    still = Desk(front="Calendar")
    go(p, Senses([calendar(), calendar(True)], changed=[True]), Effectors(), still, decide="keyword", planned_for="Calendar")
    assert still.opened == []                              # already in front: nothing is opened


# ---- a form of several fields (browser_model_eval contact_form, 2026-09-24) ----------------------------------


def _contact_form(focused: str = "") -> Any:
    web = {"app": "browser", "actions": ("AXPress", "AXFocus")}   # a page's field is clickable too (browser_lane)
    fields = (cand("1", "text field", "Full name", **web), cand("2", "text field", "Email address", **web),
              cand("3", "text field", "Your message", **web))
    return snap(*fields, cand("4", "button", "Send message", app="browser"), app="browser", bundle="eve.browser",
                title="Contact us", focused=next((c for c in fields if c.label == focused), None))


def test_the_field_the_step_names_is_typed_into_when_jev_agrees_even_below_its_cut_offs() -> None:
    # The eval's failure: three labelled fields, Jev's top pick right but under the step cut-offs -> ambiguous_field.
    p = plan({"kind": "type", "target": "the focused Email address text field", "text": "ada@example.com"},
             request="fill in the email ada@example.com")
    eff = Effectors()
    r = go(p, Senses([_contact_form()]), eff, Desk(front="browser"), asker=asker_saying("Email address", p=0.4))
    assert r.status == "complete" and eff.typed == [("Email address", "ada@example.com")], r.to_dict()
    assert r.ledger[0].how == "code+jev"


def test_jev_still_refuses_a_named_field_it_reads_as_another() -> None:
    p = plan({"kind": "type", "target": "the Email address field", "text": "ada@example.com"},
             request="fill in the email ada@example.com")
    eff = Effectors()
    r = go(p, Senses([_contact_form()]), eff, Desk(front="browser"), asker=asker_saying("Full name", p=0.4))
    assert r.status == "none" and r.reason == "ambiguous_field" and eff.typed == [], r.to_dict()
    risky = Effectors()
    r = go(p, Senses([_contact_form()]), risky, Desk(front="browser"), asker=asker_saying("Email address", risky=0.9))
    assert r.status == "needs_human" and risky.typed == []


def test_right_after_clicking_a_field_the_focused_field_is_the_proposal() -> None:
    p = plan({"kind": "click", "target": "the message box", "label_hint": "Your message"},
             {"kind": "type", "target": "the text area", "text": "Please call me back"},
             request="write Please call me back in the message")
    eff = Effectors()
    senses = Senses([_contact_form(), _contact_form("Your message"), _contact_form("Your message")], changed=[True])
    r = go(p, senses, eff, Desk(front="browser"), asker=asker_saying("Your message", p=0.4))
    assert r.status == "complete" and eff.clicks == ["Your message"], r.to_dict()
    assert eff.typed == [("Your message", "Please call me back")]


def test_the_longest_label_wins_and_a_repeated_label_names_nothing() -> None:
    from eve_bridge.plan_executor import _named_field

    pool = [cand("1", "text field", "Name"), cand("2", "text field", "Name for the booking")]
    step = pc.parse_step({"kind": "type", "target": "the Name for the booking field", "text": "x"}, 1, "x")
    assert _named_field(step, pool).id == "2"
    twins = [cand("1", "text field", "Name"), cand("2", "text field", "Name")]
    assert _named_field(pc.parse_step({"kind": "type", "target": "the name field", "text": "x"}, 1, "x"), twins) is None


class ConsentEffectors(Effectors):
    """A browser page's effectors: they can dismiss a cookie notice, and say what they pressed."""

    def __init__(self, said: str = "dismissed the cookie notice with 'Reject all'") -> None:
        super().__init__()
        self.said, self.cleared = said, 0

    def clear_consent(self) -> str:
        self.cleared += 1
        return self.said


def test_a_cookie_notice_is_dismissed_before_a_click_and_is_in_the_ledger() -> None:
    eff = ConsentEffectors()
    p = plan({"kind": "click", "target": "week", "label_hint": "Week"})
    r = go(p, Senses([calendar(), calendar(True)], changed=[True]), eff, decide="keyword")
    assert r.status == "complete" and eff.cleared == 1 and eff.clicks == ["Week"], r.to_dict()
    assert [(e.step, e.effect, e.how) for e in r.ledger][0] == ("dismiss the cookie notice", "confirmed", "code")
    quiet = ConsentEffectors(said="")                       # nothing to dismiss: nothing in the ledger
    r = go(p, Senses([calendar(), calendar(True)], changed=[True]), quiet, decide="keyword")
    assert [e.step for e in r.ledger] == ["click week"]


class GatedEffectors(Effectors):
    """Effectors with their own sensitive-label gate, as browser_lane's page and desktop_helpers' adapter have."""

    def __init__(self) -> None:
        super().__init__()
        self.approve = ""

    def click_candidate(self, c) -> str:
        from eve_bridge.ax_candidates import is_sensitive

        if is_sensitive(c.label) and self.approve != " ".join(c.label.split()).casefold():
            return f"refused: {c.label!r} is a sensitive control without approval"
        return super().click_candidate(c)


def test_the_humans_yes_to_a_flagged_step_reaches_the_effectors_own_gate_for_that_step_only() -> None:
    form = _contact_form()
    p = plan({"kind": "click", "target": "button labelled Send message", "label_hint": "Send message", "consequential": True},
             request="send the contact form")
    eff = GatedEffectors()
    first = go(p, Senses([form]), eff, Desk(front="browser"), asker=asker_saying("Send message"))
    assert first.status == "needs_human" and eff.clicks == []
    r = go(p, Senses([form, form, form, form]), eff, Desk(front="browser"), asker=asker_saying("Send message"),
           approved={0: first.confirm})
    assert eff.clicks == ["Send message"] and r.status != "partial", r.to_dict()
    assert eff.approve == ""                                   # nothing carries over to a later step
