from __future__ import annotations

import asyncio
from dataclasses import replace

import pytest
from PIL import Image
from test_fast_lane import Effectors, calendar, cand, snap

from eve_bridge import decision_lane as dl
from eve_bridge import decisions as d
from eve_bridge.desk_lane import DeskLane, codex_note
from eve_bridge.desktop_helpers import _LaneAdapter


class Senses:
    def __init__(self, states, *, stale_image=False, pid=42):
        self.states, self.calls, self.pid = states, 0, pid
        self.stale_image = stale_image

    def observe(self):
        s = self.states[min(self.calls, len(self.states) - 1)]
        self.calls += 1
        return s, Image.new("RGB", (800, 500), "red" if self.stale_image and self.calls == 2 else "white")

    image_matches = staticmethod(_LaneAdapter.image_matches)

    def frontmost_pid(self):
        return self.pid


class Client:
    def __init__(self, target="2", *, verified=0.99, error="", risky=0.01, covered=0.99, present=0.99, p=0.99):
        self.target, self.verified, self.error = target, verified, error
        self.risky, self.covered, self.present, self.p = risky, covered, present, p
        self.calls = []

    def evaluate(self, evidence, images, questions):
        self.calls.append((evidence, images, questions))
        if len(questions) == 1:
            return d.Answer({"verified": self.verified}, error=self.error)
        return d.Answer({"target": {"id": self.target, "p": self.p, "margin": 0.98}, "present": self.present,
                         "covered": self.covered, "risky": self.risky}, error=self.error)


def run(states=None, client=None, effectors=None, **kw):
    senses = Senses(states or [calendar(), calendar(), calendar(True)])
    eff = effectors or Effectors()
    cli = client or Client()
    out = dl.route("show the next seven days", senses=senses, effectors=eff, client=cli, apps=[], **kw)
    return out, eff, cli


def test_observe_select_revalidate_act_verify_completes_without_planner():
    r, eff, cli = run()
    assert r.status == "complete" and r.clicked == ["Week"] and eff.clicks == ["Week"]
    assert len(cli.calls) == 2 and len(cli.calls[0][1]) == 1 and len(cli.calls[1][1]) == 2
    assert cli.calls[1][0]["before"]["snapshot_seq"] != cli.calls[1][0]["after"]["snapshot_seq"]
    assert "Week" in r.sentence


@pytest.mark.parametrize("settings", [{"risky": 0.9}, {"covered": 0.5}, {"present": 0.3}, {"p": 0.2},
                                      {"error": "refused"}, {"target": "unoffered"}, {"target": "none"}])
def test_model_abstention_and_risk_stop_before_input(settings):
    r, eff, _ = run(client=Client(**settings))
    assert r.status == "none" and not eff.clicks


@pytest.mark.parametrize("mutation", ["moved", "disabled", "dialog", "app", "truncated", "value"])
def test_stale_target_or_state_prevents_click(mutation):
    before = calendar()
    after = before
    if mutation in ("moved", "disabled", "value"):
        changes = {"moved": {"frame": (5, 5, 60, 30)}, "disabled": {"enabled": False}, "value": {"value": "selected"}}
        after = replace(before, elements=tuple(replace(c, **changes[mutation]) if c.id == "2" else c for c in before.elements))
    else:
        after = replace(before, **{"dialog": {"dialog_text": "Confirm"}, "app": {"pid": 99}, "truncated": {"truncated": True}}[mutation])
    r, eff, _ = run([before, after])
    assert r.reason == "stale_observation" and not eff.clicks


def test_pixels_can_invalidate_even_an_identical_ax_tree():
    eff = Effectors()
    r = dl.route("show the next seven days", senses=Senses([calendar()], stale_image=True),
                 effectors=eff, client=Client(), apps=[])
    assert r.reason == "stale_observation" and not eff.clicks


def test_unknown_effect_is_partial_and_named_in_codex_handoff():
    r, eff, cli = run(client=Client(verified=0.2))
    assert r.status == "partial" and r.reason == "unverified" and eff.clicks == ["Week"]
    assert "'Week'" in codex_note(r.to_dict()) and len(cli.calls) == 2


def test_exception_after_delivering_input_preserves_progress():
    class Broken(Effectors):
        def click_candidate(self, c):
            super().click_candidate(c)
            raise RuntimeError("after delivery")

    r, eff, _ = run(effectors=Broken())
    assert r.status == "partial" and r.clicked == eff.clicks == ["Week"]


def test_post_action_capture_failure_preserves_progress():
    class Broken(Senses):
        def observe(self):
            if self.calls == 2:
                raise RuntimeError("screen unavailable")
            return super().observe()

    r = dl.route("show the next seven days", senses=Broken([calendar()]), effectors=Effectors(), client=Client(), apps=[])
    assert r.status == "partial" and r.clicked == ["Week"]


def test_refused_hit_test_is_zero_input_and_dry_run_does_not_act():
    r, eff, _ = run(effectors=Effectors(refuse_click=True))
    assert r.status == "none" and not r.clicked and not eff.clicks
    r, eff, cli = run(dry_run=True)
    assert r.reason == "dry_run" and not eff.clicks and len(cli.calls) == 1


@pytest.mark.parametrize("goal", ["delete the event", "send it", "type hello", "what is on my screen", "turn off wifi"])
def test_request_gates_refuse_before_observation(goal):
    senses, eff, cli = Senses([calendar()]), Effectors(), Client()
    r = dl.route(goal, senses=senses, effectors=eff, client=cli, apps=[])
    assert r.status == "none" and senses.calls == 0 and not eff.clicks and not cli.calls


def test_withheld_and_system_settings_controls_are_never_offered():
    before = snap(cand("1", "button", "Delete Event"), cand("2", "button", "Today"))
    assert dl.menu("delete the event", before) == {}
    r, eff, cli = run([replace(calendar(), bundle=dl.SYSTEM_SETTINGS_BUNDLE)])
    assert r.reason == "unsafe_observation" and not eff.clicks and not cli.calls


def test_selected_target_is_satisfied_without_input():
    r, eff, cli = run([calendar(True)])
    assert r.status == "complete" and r.already == ["Week"] and not eff.clicks and len(cli.calls) == 1


def test_focus_unknown_never_authorizes_input():
    eff = Effectors()
    r = dl.route("show the next seven days", senses=Senses([calendar()], pid=0), effectors=eff, client=Client(), apps=[])
    assert r.reason == "focus_changed" and not eff.clicks


def test_multi_step_stops_on_unverified_first_click():
    eff, cli = Effectors(), Client(verified=0.1)
    r = dl.route("week view then month view", senses=Senses([calendar()]), effectors=eff, client=cli, apps=[])
    assert r.status == "partial" and eff.clicks == ["Week"] and len(cli.calls) == 2


def test_same_control_cannot_be_repeated_after_a_verified_action():
    eff, cli = Effectors(), Client()
    r = dl.route("week view then week view", senses=Senses([calendar()]), effectors=eff, client=cli, apps=[])
    assert r.status == "partial" and r.reason == "repeat" and eff.clicks == ["Week"]


def test_successful_multiple_steps_have_separate_observations_and_verifications():
    class Ordered(Client):
        def evaluate(self, evidence, images, questions):
            self.target = "3" if evidence.get("step") == "month view" else "2"
            return super().evaluate(evidence, images, questions)

    eff, cli = Effectors(), Ordered()
    states = [calendar(), calendar(), calendar(True), calendar(True), calendar(True), calendar()]
    r = dl.route("week view then month view", senses=Senses(states), effectors=eff, client=cli, apps=[])
    assert r.status == "complete" and eff.clicks == ["Week", "Month"] and len(cli.calls) == 4


def test_adapter_observation_uses_captured_retina_frame_in_screen_points(monkeypatch):
    from test_desktop_helpers import Bench, FakeAutoGUI, img

    from eve_bridge import ax_candidates

    before = calendar()
    monkeypatch.setattr(ax_candidates, "ax_snapshot", lambda *a, **kw: before)
    bench = Bench(gui=FakeAutoGUI(points=(800, 500)), frames=[img(size=(1600, 1000))])
    s, image = _LaneAdapter(bench.h, None).observe()
    assert s is before and image.size == (800, 500) and len(bench.capture.calls) == 1


def test_benchmark_counts_abstentions_separately_from_wrong_presses():
    from decisions_eval import summarize

    summary = summarize([{"provider": "decisions", "kind": "target", "correct": False, "picked": "", "ms": 10},
                         {"provider": "decisions", "kind": "target", "correct": False, "picked": "bad", "ms": 20},
                         {"provider": "decisions", "kind": "target", "correct": True, "picked": "ok", "ms": 30}])
    assert summary["decisions"]["target"]["wrong"] == 1 and summary["decisions"]["target"]["abstained"] == 1


def test_pixel_checker_has_positive_and_negative_controls():
    white, red = Image.new("RGB", (100, 100), "white"), Image.new("RGB", (100, 100), "red")
    assert _LaneAdapter.image_matches(white, white.copy())
    assert not _LaneAdapter.image_matches(white, red)


def test_daemon_wiring_only_tries_decisions_after_zero_input_keyword_mismatch(monkeypatch):
    from test_desk_lane import FakeHelpers

    from eve_bridge import decisions

    seen = []

    class Helpers(FakeHelpers):
        def decisions_first(self, goal, client, **kw):
            seen.append(goal)
            return {"status": "complete", "clicked": ["Week"], "sentence": "Done."}

    monkeypatch.setenv(d.ENV, "1")
    monkeypatch.setattr(decisions, "from_env", lambda: Client())
    h = Helpers({"status": "none", "reason": "no_match"})
    r = asyncio.run(DeskLane(make_helpers=lambda: h, trusted=lambda: True).attempt("next seven days"))
    assert r["status"] == "complete" and seen == ["next seven days"]
    for result in ({"status": "complete"}, {"status": "partial", "reason": "unverified"},
                   {"status": "none", "reason": "confirm"}):
        h.result = result
        asyncio.run(DeskLane(make_helpers=lambda: h, trusted=lambda: True).attempt("x"))
    assert seen == ["next seven days"]


def test_shadow_mode_decides_as_a_dry_run_and_leaves_the_task_to_codex(monkeypatch):
    from test_desk_lane import FakeHelpers

    from eve_bridge import decisions

    seen = []

    class Helpers(FakeHelpers):
        def decisions_first(self, goal, client, **kw):
            seen.append(kw)
            return {"status": "none", "reason": "dry_run"}

    monkeypatch.setenv(d.ENV, "shadow")
    monkeypatch.setattr(decisions, "from_env", lambda: Client())
    h = Helpers({"status": "none", "reason": "no_match"})
    r = asyncio.run(DeskLane(make_helpers=lambda: h, trusted=lambda: True).attempt("next seven days"))
    assert seen and seen[0]["dry_run"] is True
    assert r["status"] == "none" and r["reason"] == "no_match"         # the keyword lane's miss stands: Codex


def test_with_the_switch_off_the_judge_is_never_built(monkeypatch):
    from test_desk_lane import FakeHelpers

    from eve_bridge import decisions

    built = []
    monkeypatch.setenv(d.ENV, "0")
    monkeypatch.setattr(decisions, "from_env", lambda: built.append(1))
    h = FakeHelpers({"status": "none", "reason": "no_match"})
    asyncio.run(DeskLane(make_helpers=lambda: h, trusted=lambda: True).attempt("next seven days"))
    assert not built


def test_a_missing_judge_is_not_rebuilt_per_task(monkeypatch):
    from test_desk_lane import FakeHelpers

    from eve_bridge import decisions

    built = []
    monkeypatch.setenv(d.ENV, "1")
    monkeypatch.setattr(decisions, "from_env", lambda: built.append(1))
    lane = DeskLane(make_helpers=lambda: FakeHelpers({"status": "none", "reason": "no_match"}), trusted=lambda: True)
    for _ in range(3):
        asyncio.run(lane.attempt("next seven days"))
    assert built == [1]
