"""Jev on one step: the asker (typed_ask.ask_jev_step), its gates, and the lane's "jev" decide mode.

The fakes are test_fast_lane's. Nothing here touches a socket: `predict` is a function that returns
the envelope jev.make_predict returns.
"""

from __future__ import annotations

from typing import Any

from test_fast_lane import Clock, Effectors, Senses, calendar, cand, snap

from eve_bridge import typed_ask as ta
from eve_bridge.fast_lane import JEV_MAX_OPTIONS, run_delegate


def envelope(target: str, options: dict[str, str], *, p: float = 0.95, present: float = 0.95,
             already_done: float = 0.05, risky: float = 0.05) -> dict[str, Any]:
    keys = [*options, ta.STEP_NONE]
    rest = (1.0 - p) / max(1, len(keys) - 1)
    return {"answers": {
        "target": {"type": "choice", "choice": target, "probabilities": {k: (p if k == target else rest) for k in keys}},
        "present": {"type": "noul", "noul": present},
        "already_done": {"type": "noul", "noul": already_done},
        "risky": {"type": "noul", "noul": risky}}}


def asker_saying(label: str, **kw: Any):
    """An asker that picks the option whose text starts with `label` ("none" picks none), and records what it was sent."""
    seen: list[dict[str, Any]] = []

    def predict(state: Any, questions: dict[str, Any]) -> dict[str, Any]:
        seen.append({"state": state, "questions": questions})
        options = {k: v for k, v in questions["target"]["criteria"].items() if k != ta.STEP_NONE}
        target = next((k for k, v in options.items() if v.startswith(label)), ta.STEP_NONE)
        return envelope(target, options, **kw)

    def asker(step: str, **k: Any) -> ta.StepAnswer:
        return ta.ask_jev_step(predict, step, clock=Clock(), **k)

    asker.seen = seen  # type: ignore[attr-defined]
    return asker


# ---- the asker ---------------------------------------------------------------------------------


def test_one_request_carries_the_choice_with_none_and_three_nouls() -> None:
    asker = asker_saying("Week")
    a = asker("switch to week view", app="Calendar", context="title: September 2026",
              options={"1": "Day (radio button)", "2": "Week (radio button)"})
    assert len(asker.seen) == 1
    q = asker.seen[0]["questions"]
    assert {name: spec["type"] for name, spec in q.items()} == {
        "target": "choice", "present": "noul", "already_done": "noul", "risky": "noul"}
    assert list(q["target"]["criteria"]) == ["1", "2", ta.STEP_NONE]
    assert a.target == "2" and a.k == 3 and a.error == "" and a.margin > 0.8


def test_the_nouls_can_see_the_controls_because_the_state_lists_them() -> None:
    # The nouls are answered against the state: with the list only in the choice's criteria, `present`
    # could not tell a right control from a missing one (select, 2026-09-21).
    asker = asker_saying("Week")
    asker("switch to week view", app="Calendar", context="title: x", options={"1": "Day (radio button)"})
    assert asker.seen[0]["state"]["controls"] == ["Day (radio button)"]
    assert asker.seen[0]["state"]["step"] == "switch to week view"


def test_a_failing_or_malformed_answer_abstains() -> None:
    def boom(state: Any, questions: dict[str, Any]) -> dict[str, Any]:
        raise RuntimeError("HTTP 503")

    a = ta.ask_jev_step(boom, "x", app="A", context="", options={"1": "One"}, clock=Clock())
    assert a.target == "" and "503" in a.error and ta.JEV_STEP_GATES.decide(a) == ta.STEP_NONE

    def stranger(state: Any, questions: dict[str, Any]) -> dict[str, Any]:
        return {"answers": {"target": {"choice": "99", "probabilities": {"99": 1.0}}}}

    b = ta.ask_jev_step(stranger, "x", app="A", context="", options={"1": "One"}, clock=Clock())
    assert b.target == "" and "not an option" in b.error


def test_gates_press_only_when_the_absolute_and_the_relative_both_clear() -> None:
    g = ta.StepGates(present=0.9, p_target=0.9, margin=0.3, risky_max=0.5)
    ok = ta.StepAnswer("2", 0.95, 0.9, present=0.95, already_done=0.0, risky=0.1)
    assert g.decide(ok) == "2"
    assert g.decide(ta.StepAnswer("2", 0.95, 0.9, present=0.5, already_done=0.0, risky=0.1)) == ta.STEP_NONE
    assert g.decide(ta.StepAnswer("2", 0.6, 0.5, present=0.95, already_done=0.0, risky=0.1)) == ta.STEP_NONE
    assert g.decide(ta.StepAnswer("2", 0.95, 0.1, present=0.95, already_done=0.0, risky=0.1)) == ta.STEP_NONE
    assert g.decide(ta.StepAnswer(ta.STEP_NONE, 0.99, 0.9, present=0.95, already_done=0.0, risky=0.1)) == ta.STEP_NONE
    # risk outranks a perfect pick
    assert g.decide(ta.StepAnswer("2", 0.99, 0.9, present=0.99, already_done=0.0, risky=0.6)) == "confirm"


def test_fit_allows_no_wrong_press_and_prefers_the_stricter_tie() -> None:
    answers = [ta.StepAnswer("1", 0.95, 0.9, 0.95, 0.0, 0.0), ta.StepAnswer("2", 0.95, 0.9, 0.95, 0.0, 0.0),
               ta.StepAnswer("3", 0.75, 0.5, 0.65, 0.0, 0.0),                   # a wrong pick, moderately sure
               ta.StepAnswer(ta.STEP_NONE, 0.9, 0.8, 0.1, 0.0, 0.0)]
    g = ta.fit_step_gates(answers, ["1", "2", "9", "abstain"])
    said = [g.decide(a) for a in answers]
    assert said == ["1", "2", ta.STEP_NONE, ta.STEP_NONE]
    assert g.present == 0.9 and g.p_target == 0.9              # the strictest setting that still presses both


# ---- the lane in jev mode ------------------------------------------------------------------------


def test_jev_mode_presses_the_gate_pick_when_jev_agrees() -> None:
    eff = Effectors()
    asker = asker_saying("Week")
    r = run_delegate("switch to week view", senses=Senses([calendar(), calendar(True)]), effectors=eff,
                     decider=None, decide="jev", asker=asker, clock=Clock())
    assert eff.clicks == ["Week"] and r.steps[-1].via == "keyword+jev", r.line
    assert len(asker.seen) == 1


def test_jev_mode_refuses_a_gate_pick_jev_does_not_share() -> None:
    eff = Effectors()
    r = run_delegate("switch to week view", senses=Senses([calendar()]), effectors=eff, decider=None,
                     decide="jev", asker=asker_saying("none"), clock=Clock())
    assert r.status == "escalate" and "reason=jev_veto" in r.line and eff.clicks == [], r.line


def test_jev_mode_decides_a_step_the_gate_cannot() -> None:
    # "show the next seven days" shares no word with "Week": the keyword gate says no_match, Jev says Week.
    eff = Effectors()
    r = run_delegate("show the next seven days", senses=Senses([calendar(), calendar(True)]), effectors=eff,
                     decider=None, decide="jev", asker=asker_saying("Week"), clock=Clock())
    assert eff.clicks == ["Week"] and r.steps[-1].via == "jev" and r.steps[-1].p_top >= 0.9, r.line


def test_jev_mode_presses_nothing_on_a_weak_or_failed_answer() -> None:
    eff = Effectors()
    weak = run_delegate("show the next seven days", senses=Senses([calendar()]), effectors=eff, decider=None,
                        decide="jev", asker=asker_saying("Week", p=0.6), clock=Clock())
    assert weak.status == "escalate" and "reason=jev_none" in weak.line and eff.clicks == [], weak.line

    def dead(step: str, **k: Any) -> ta.StepAnswer:
        return ta.StepAnswer("", 0.0, 0.0, 0.0, 0.0, 1.0, error="HTTP 503")

    failed = run_delegate("switch to week view", senses=Senses([calendar()]), effectors=eff, decider=None,
                          decide="jev", asker=dead, clock=Clock())
    assert failed.status == "escalate" and "reason=jev_error" in failed.line and eff.clicks == [], failed.line
    missing = run_delegate("switch to week view", senses=Senses([calendar()]), effectors=eff, decider=None,
                           decide="jev", clock=Clock())
    assert missing.status == "unavailable" and "jev is not configured" in missing.line


def test_jev_mode_a_risky_step_confirms_and_clicks_nothing() -> None:
    eff = Effectors()
    r = run_delegate("switch to week view", senses=Senses([calendar()]), effectors=eff, decider=None,
                     decide="jev", asker=asker_saying("Week", risky=0.8), clock=Clock())
    assert r.status == "confirm" and '"Week"' in r.line and eff.clicks == [], r.line


def test_jev_mode_never_offers_a_sensitive_control_and_the_code_gate_still_confirms() -> None:
    # The sensitive control is withheld from the menu, so Jev cannot pick it; the lane confirms it in code.
    window = snap(cand("1", "button", "Delete Event"), cand("2", "button", "Add Event"), cand("3", "button", "Today"))
    eff = Effectors()
    asker = asker_saying("Add")
    r = run_delegate("delete the event", senses=Senses([window]), effectors=eff, decider=None, decide="jev",
                     asker=asker, clock=Clock())
    assert r.status == "confirm" and "Delete Event" in r.line and eff.clicks == [] and asker.seen == [], r.line


def test_jev_mode_offers_the_wider_menu() -> None:
    many = snap(*[cand(str(i), "button", f"Item {i}", frame=(10, 10 + 30 * i, 80, 24)) for i in range(1, 41)])
    asker = asker_saying("Item 7 ")
    run_delegate("the seventh thing", senses=Senses([many, many]), effectors=Effectors(), decider=None,
                 decide="jev", asker=asker, clock=Clock())
    offered = [k for k in asker.seen[0]["questions"]["target"]["criteria"] if k != ta.STEP_NONE]
    assert len(offered) == JEV_MAX_OPTIONS - 2
