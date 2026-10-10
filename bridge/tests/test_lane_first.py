"""The keyword-only lane, scripts, the router and the System Settings doors (GATES.md G3–G5, G8).

The fakes are test_fast_lane's. Snapshots are consumed one per `senses.snapshot()` call, and a
script step takes two (before the click, after it), so a two-step script lists four.
"""

from __future__ import annotations

from test_fast_lane import Clock, Decider, Effectors, Senses, calendar, cand, snap

from eve_bridge import lane_router as lr
from eve_bridge.decider import Choice
from eve_bridge.fast_lane import (
    DEFAULT_DECIDE,
    MAX_SCRIPT_STEPS,
    SYSTEM_SETTINGS_BUNDLE,
    keyword_verdict,
    run_delegate,
    run_script,
    uncovered_tokens,
)


class NeverAsked:
    """A decider that fails the test the moment anyone asks it: the positive control for "the
    model is out of the click path"."""

    available = True

    def choose(self, *a, **k) -> Choice:
        raise AssertionError("the keyword lane asked the model")


def year_view() -> list:
    """Calendar, month view → click Year → year view → click next year → next year."""
    month = snap(cand("1", "radio button", "Day"), cand("2", "radio button", "Week"),
                 cand("3", "radio button", "Month", "selected"), cand("4", "radio button", "Year"),
                 cand("5", "button", "Today"), title="September 2026")
    year = snap(cand("1", "radio button", "Day"), cand("2", "radio button", "Week"),
                cand("3", "radio button", "Month"), cand("4", "radio button", "Year", "selected"),
                cand("5", "button", "Today"), cand("6", "button", "previous year"), cand("7", "button", "next year"),
                title="2026")
    nxt = snap(cand("1", "radio button", "Day"), cand("2", "radio button", "Week"),
               cand("3", "radio button", "Month"), cand("4", "radio button", "Year", "selected"),
               cand("5", "button", "Today"), cand("6", "button", "previous year"), cand("7", "button", "next year"),
               title="2027")
    return [month, year, year, nxt]


# ---- G3: keyword decide mode ---------------------------------------------------------------


def test_keyword_mode_clicks_without_a_decider() -> None:
    assert DEFAULT_DECIDE == "keyword"
    eff = Effectors()
    r = run_delegate("switch to week view", senses=Senses([calendar(), calendar(True)]), effectors=eff,
                     decider=None, clock=Clock())
    assert r.status == "stopped" and "reason=one_step" in r.line, r.line
    assert eff.clicks == ["Week"] and r.steps[-1].via == "keyword" and r.steps[-1].changed is True
    assert r.steps[-1].label == "Week"


def test_keyword_mode_tie_escalates_and_never_asks_the_model() -> None:
    two = snap(cand("1", "button", "Done"), cand("2", "button", "Done", frame=(400, 400, 60, 30)),
               cand("3", "button", "Cancel"))
    eff = Effectors()
    r = run_delegate("press done", senses=Senses([two]), effectors=eff, decider=NeverAsked(), clock=Clock())
    assert r.status == "escalate" and "reason=ambiguous_target" in r.line, r.line
    assert eff.clicks == [] and "steps=0" in r.line


def test_keyword_mode_zero_overlap_escalates_no_match() -> None:
    eff = Effectors()
    r = run_delegate("show the next seven days", senses=Senses([calendar()]), effectors=eff, decider=NeverAsked(),
                     clock=Clock())
    assert r.status == "escalate" and "reason=no_match" in r.line and eff.clicks == [], r.line
    # and the keyword lane only clicks: text or a key is refused with zero input
    typed = run_delegate("search", senses=Senses([calendar()]), effectors=eff, decider=None, text="jazz",
                         clock=Clock())
    assert typed.status == "unavailable" and "only clicks" in typed.line and eff.typed == []


def test_model_mode_still_asks_the_model_on_a_tie() -> None:
    d = Decider(["Day"])
    eff = Effectors()
    r = run_delegate("show the next seven days", senses=Senses([calendar(), calendar()]), effectors=eff, decider=d,
                     decide="model", clock=Clock())
    assert len(d.menus) == 1 and eff.clicks == ["Day"] and r.steps[-1].via == "model"
    none = run_delegate("x y z", senses=Senses([calendar()]), effectors=eff, decider=None, decide="model",
                        clock=Clock())
    assert none.status == "unavailable" and "not loaded" in none.line


def test_keyword_verdict_says_why_it_abstained() -> None:
    from eve_bridge.fast_lane import MenuItem

    items = [MenuItem("1", "click", "Week", cand("1", "radio button", "Week")),
             MenuItem("2", "click", "Week numbers", cand("2", "checkbox", "Week numbers"))]
    assert keyword_verdict(items, "switch to week view") == (None, "tie")
    assert keyword_verdict(items, "print it") == (None, "no_match")
    pick, why = keyword_verdict(items, "show week numbers")
    assert pick is items[1] and why == ""


# ---- G4: scripts ------------------------------------------------------------------------------


def test_script_runs_each_step_in_order() -> None:
    eff = Effectors()
    s = run_script(["Year", "next year"], senses=Senses(year_view()), effectors=eff, clock=Clock())
    assert eff.clicks == ["Year", "next year"]
    assert s.status == "complete" and s.complete and s.labels == ["Year", "next year"] and s.already == []
    assert s.line.startswith("script: complete; applied=[Year (radio button), next year (button)]; step 2/2 "), s.line
    assert len(s.results) == 2 and s.log_lines[-1].startswith("delegate script: complete")
    # a step that asks for the view the window is already in costs no input and still counts
    eff2 = Effectors()
    same = run_script(["Month"], senses=Senses(year_view()[:1]), effectors=eff2, clock=Clock())
    assert same.status == "complete" and eff2.clicks == [] and same.already == ["Month"] and same.labels == []


def test_script_stops_at_the_first_step_that_does_not_act() -> None:
    eff = Effectors()
    s = run_script(["Year", "the fortnight view", "next year"], senses=Senses(year_view()), effectors=eff,
                   clock=Clock())
    assert eff.clicks == ["Year"]                                   # step 3 never ran
    assert s.status == "partial" and s.labels == ["Year"] and "step 2/3 escalate:" in s.line
    assert "reason=no_match" in s.line
    # nothing applied at all is "none"
    eff0 = Effectors()
    none = run_script(["the fortnight view"], senses=Senses(year_view()), effectors=eff0, clock=Clock())
    assert none.status == "none" and eff0.clicks == [] and none.labels == []
    # a click nobody saw change anything is not "complete": unknown is not proof
    still = [calendar(), calendar()]
    eff1 = Effectors()
    unseen = run_script(["Today"], senses=Senses(still, changed=[None]), effectors=eff1, clock=Clock())
    assert eff1.clicks == ["Today"] and unseen.status == "partial"
    eff2 = Effectors()
    dead = run_script(["Today", "Week"], senses=Senses(still, changed=[False]), effectors=eff2, clock=Clock())
    assert eff2.clicks == ["Today"] and dead.status == "partial" and "step 1/2" in dead.line


def test_script_sensitive_step_confirms_and_clicks_nothing_more() -> None:
    eff = Effectors()
    s = run_script(["Week", "Delete Event", "Today"], senses=Senses([calendar(), calendar(True), calendar(True)]),
                   effectors=eff, clock=Clock())
    assert eff.clicks == ["Week"]
    assert s.status == "partial" and 'step 2/3 confirm: "Delete Event" needs the human\'s yes' in s.line


def test_script_is_capped_at_six_steps() -> None:
    eff = Effectors()
    s = run_script(["Week"] * (MAX_SCRIPT_STEPS + 1), senses=Senses([calendar()]), effectors=eff, clock=Clock())
    assert s.status == "none" and "at most 6 steps" in s.line and eff.clicks == []
    empty = run_script(["", "  "], senses=Senses([calendar()]), effectors=eff, clock=Clock())
    assert empty.status == "none" and "at least one step" in empty.line


# ---- G5: the router ---------------------------------------------------------------------------

APPS = frozenset({"notes", "calendar", "system settings", "safari", "mail"})


def route(goal: str, snapshots: list, *, front: str = "Calendar", changed: list | None = None):
    eff = Effectors()
    r = lr.route(goal, senses=Senses(snapshots, changed=changed), effectors=eff, frontmost_app=front, apps=APPS,
                 clock=Clock())
    return r, eff


def test_router_clauses() -> None:
    assert lr.split_clauses("go to year view and then next year") == ["go to year view", "next year"]
    assert lr.split_clauses("Can you switch to week view, then today") == ["switch to week view", "today"]
    assert lr.split_clauses("week") == ["week"]
    assert lr.normalise("hey eve, could you please switch to week view") == "switch to week view"
    assert lr.refuse_reason("what is on my calendar this week") == "question"
    assert lr.refuse_reason("is week selected?") == "question"
    assert lr.refuse_reason("to the") == "empty"
    assert lr.refuse_reason("alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo") == "too_long"
    assert lr.refuse_reason("switch to week view") == ""
    assert lr.spoken_sentence(["Week"]) == "Done. I clicked Week."
    assert lr.spoken_sentence(["Year", "next year"]) == "Done. I clicked Year, then next year."
    assert lr.spoken_sentence([], ["Month"]) == "Month is already selected."
    assert len(lr.spoken_sentence(["x" * 60] * 6)) <= lr.MAX_SPOKEN_CHARS


def test_router_engages_on_a_fully_covered_goal() -> None:
    r, eff = route("Can you switch the calendar to week view", [calendar(), calendar(True)])
    assert eff.clicks == ["Week"], r
    assert r.status == "complete" and r.clicked == ["Week"] and r.sentence == "Done. I clicked Week."
    d = r.to_dict()
    assert d["status"] == "complete" and d["clicked"] == ["Week"] and d["log"] and d["ms"] >= 0
    # "the week button": the role word is accounted for, so is the generic "tab"
    assert uncovered_tokens("click the week button", cand("2", "radio button", "Week"), "Calendar") == []
    assert uncovered_tokens("week tab", cand("2", "radio button", "Week"), "Calendar", lr.GENERIC_WORDS) == []
    # the value never counts: a selected Week does not account for "selected"
    assert uncovered_tokens("week selected", cand("2", "radio button", "Week", "selected"), "") == ["selected"]


def test_router_refuses_a_goal_with_uncovered_words() -> None:
    # "week" matches the Week control, but the request is about a review page: nothing is clicked
    r, eff = route("go to the week in review page for september", [calendar()])
    assert eff.clicks == [] and r.status == "none" and r.reason == "uncovered", r
    assert any("unexplained" in line for line in r.log_lines)
    # a sensitive control is never the router's to click
    r2, eff2 = route("delete event", [calendar()])
    assert eff2.clicks == [] and r2.status == "none" and r2.reason == "confirm", r2
    # a question is refused before a single snapshot
    senses = Senses([calendar()])
    r3 = lr.route("what is on this week", senses=senses, effectors=Effectors(), frontmost_app="Calendar", apps=APPS,
                  clock=Clock())
    assert r3.status == "refused" and r3.reason == "question" and senses.calls == 0
    # a dialog is never operated
    sheet = snap(cand("1", "button", "Week", in_dialog=True), dialog_text="Save changes?")
    r4, eff4 = route("week", [sheet])
    assert eff4.clicks == [] and r4.status == "none" and r4.reason == "dialog_open", r4


def test_router_refuses_a_goal_naming_another_app() -> None:
    finder = snap(cand("1", "row", "Notes"), cand("2", "row", "Downloads"), app="Finder", bundle="com.apple.finder")
    senses = Senses([finder])
    eff = Effectors()
    r = lr.route("open notes", senses=senses, effectors=eff, frontmost_app="Finder", apps=APPS, clock=Clock())
    assert r.status == "refused" and r.reason == "names_app:notes" and eff.clicks == [] and senses.calls == 0
    # the frontmost app's own name is not "another app"
    assert lr.names_other_app("switch the calendar to week view", "Calendar", APPS) == ""
    assert lr.names_other_app("open system settings", "Finder", APPS) == "system settings"
    assert lr.names_other_app("go to week view", "Calendar", APPS) == ""
    listed = lr.installed_app_names(dirs=("/x",), listdir=lambda d: ["Notes.app", "Safari.app", "readme.txt"])
    assert listed == frozenset({"notes", "safari"})
    assert lr.installed_app_names(dirs=("/missing",), listdir=lambda d: (_ for _ in ()).throw(OSError())) == frozenset()


def test_router_two_clauses_two_clicks() -> None:
    # the whole goal ties/uncovers on one control, so it is split: [month] + year_view()'s four
    views = year_view()
    r, eff = route("go to year view and then next year", [views[0], *views])
    assert eff.clicks == ["Year", "next year"], r
    assert r.status == "complete" and r.sentence == "Done. I clicked Year, then next year."
    # "desktop and dock" is ONE row: the whole goal is tried first and never split
    settings = snap(cand("1", "row", "Desktop & Dock"), cand("2", "row", "General"), cand("3", "row", "Displays"),
                    app="System Settings", bundle=SYSTEM_SETTINGS_BUNDLE, title="")
    opened = snap(cand("1", "row", "Desktop & Dock", "selected"), cand("2", "row", "General"),
                  cand("3", "row", "Displays"), cand("4", "pop up button", "Position on screen"),
                  app="System Settings", bundle=SYSTEM_SETTINGS_BUNDLE, title="Desktop & Dock")
    r2, eff2 = route("open desktop and dock", [settings, opened], front="System Settings")
    assert eff2.clicks == ["Desktop & Dock"] and r2.status == "complete", r2
    # "security" is a sensitive word (ax_candidates.SENSITIVE_LABEL): that row is the planner's, with ask_user
    privacy = snap(cand("1", "row", "Privacy & Security"), cand("2", "row", "General"), app="System Settings",
                   bundle=SYSTEM_SETTINGS_BUNDLE, title="")
    r_p, eff_p = route("open privacy and security", [privacy], front="System Settings")
    assert eff_p.clicks == [] and r_p.status == "none" and r_p.reason == "confirm", r_p
    # a second clause that finds nothing leaves a partial: the planner is told what was clicked
    r3, eff3 = route("year view and then the fortnight", [views[0], *views])
    assert eff3.clicks == ["Year"] and r3.status == "partial" and r3.clicked == ["Year"] and r3.sentence == ""


# ---- G8: System Settings doors ------------------------------------------------------------------


def general_pane() -> list:
    before = snap(cand("1", "row", "General", "selected"), cand("2", "row", "Appearance"),
                  cand("3", "button", "About"), cand("4", "button", "Language & Region"),
                  cand("5", "button", "Transfer or Reset"), app="System Settings", bundle=SYSTEM_SETTINGS_BUNDLE,
                  title="General")
    after = snap(cand("1", "row", "General", "selected"), cand("2", "button", "Back"),
                 cand("3", "pop up button", "Region"), app="System Settings", bundle=SYSTEM_SETTINGS_BUNDLE,
                 title="Language & Region")
    return [before, after]


def test_settings_general_navigation_button_is_clicked() -> None:
    eff = Effectors()
    r = run_delegate("Language & Region", senses=Senses(general_pane()), effectors=eff, decider=None, clock=Clock())
    assert eff.clicks == ["Language & Region"] and r.status == "stopped", r.line
    routed, eff2 = route("open language and region", general_pane(), front="System Settings")
    assert eff2.clicks == ["Language & Region"] and routed.status == "complete", routed


def test_settings_value_button_still_confirms() -> None:
    appearance = snap(cand("1", "row", "Appearance", "selected"), cand("2", "button", "Light"),
                      cand("3", "button", "Dark"), app="System Settings", bundle=SYSTEM_SETTINGS_BUNDLE,
                      title="Appearance")
    eff = Effectors()
    r = run_delegate("Dark", senses=Senses([appearance]), effectors=eff, decider=None, clock=Clock())
    assert r.status == "confirm" and '"Dark" needs the human\'s yes' in r.line and eff.clicks == []
    # a door that is NOT in the closed set is a value like any other button
    eff2 = Effectors()
    r2 = run_delegate("Transfer or Reset", senses=Senses(general_pane()), effectors=eff2, decider=None, clock=Clock())
    assert r2.status == "confirm" and eff2.clicks == []
    routed, eff3 = route("switch to dark", [appearance], front="System Settings")
    assert eff3.clicks == [] and routed.status == "none" and routed.reason == "confirm"
