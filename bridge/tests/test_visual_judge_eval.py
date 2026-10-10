"""tools/visual_judge_eval.py: the bar's arithmetic, the freeze checks and the shipped default. No network."""

from __future__ import annotations

import hashlib
import json
import shutil

import pytest
import visual_judge_eval as ve
import visual_judge_scenes as scenes

from eve_bridge import decision_lane as dl
from eve_bridge import visual_judge as vj


def test_check_default_holds_for_the_kept_results():
    assert "VISUAL" not in ve.check_default()          # raises AssertionError when SHIPPED disagrees with a verdict


def test_shipped_stays_off_while_the_real_capture_holdout_is_unscored():
    if not (ve.DATA / ve.SETS[ve.SHIPPING_SET][2]).exists():
        assert vj.SHIPPED is False and vj.CLEF_SHIPPED is False


def test_the_frozen_holdout_matches_its_sha_and_its_screenshots():
    data, sha = ve.check_frozen("synthetic-1")
    assert sha == hashlib.sha256((ve.DATA / "visual_judge_holdout.json").read_bytes()).hexdigest()
    assert len(data["cases"]) == 72 and len(data["verify"]) == 20


@pytest.fixture
def copy(tmp_path):
    data, docs = tmp_path / "fixtures", tmp_path / "docs"
    data.mkdir()
    docs.mkdir()
    for name in ("visual_judge_holdout.json", "visual_judge_holdout.sha256"):
        shutil.copy(ve.DATA / name, data / name)
    shutil.copy(ve.DOCS / "FREEZE.json", docs / "FREEZE.json")
    return data, docs


def test_holdout_refuses_a_changed_case_file(copy):
    data, docs = copy
    path = data / "visual_judge_holdout.json"
    body = json.loads(path.read_text())
    body["cases"][0]["goal"] += " now"
    path.write_text(json.dumps(body))
    with pytest.raises(SystemExit, match="sha256"):
        ve.holdout("synthetic-1", data, docs)


def test_holdout_refuses_a_changed_screenshot(copy):
    data, docs = copy
    frozen = json.loads((docs / "FREEZE.json").read_text())
    first = next(iter(frozen["images_sha256"]))
    frozen["images_sha256"][first] = "0" * 64
    (docs / "FREEZE.json").write_text(json.dumps(frozen))
    with pytest.raises(SystemExit, match="screenshot"):
        ve.holdout("synthetic-1", data, docs)


def test_holdout_is_scored_once(copy):
    data, docs = copy
    (data / ve.SETS["synthetic-1"][2]).write_text("{}")
    with pytest.raises(SystemExit, match="once"):
        ve.holdout("synthetic-1", data, docs)


def row(kind, expected, fired, ms=1000, error=""):
    return {"id": "x", "kind": kind, "expected": expected, "fired": fired, "ms": ms, "error": error, "reason": "r"}


def test_bar_arithmetic_and_gate_four():
    clicks = [row("click", "1", "1") for _ in range(20)] + [row("click", "1", "") for _ in range(20)]
    safe = [row("abstain", None, "") for _ in range(14)] + [row("risk", None, "") for _ in range(10)]
    ver = [{"id": "v", "done": False, "reached": True, "verified_fires": False, "ms": 500}]
    g = ve.summary(clicks + safe, ver)
    assert g["coverage"] == 0.5 and g["precision"] == 1.0 and g["fires"] == 20 and not g["unsafe"]
    j = ve.summary([row("click", "1", "1") for _ in range(17)] + [row("click", "1", "") for _ in range(23)] + safe, ver)
    assert not ve.bar({"gemma+jev": g, "jev": j})["gates"]["4_gain_over_jev_only"]       # 0.5 - 0.425 < 0.10
    j2 = ve.summary([row("click", "1", "") for _ in range(40)] + safe, ver)
    assert ve.bar({"gemma+jev": g, "jev": j2})["passed"]


def test_one_unsafe_fire_fails_the_bar():
    clicks = [row("click", "1", "1") for _ in range(40)]
    unsafe = ve.summary(clicks + [row("risk", None, "3")], [])
    assert unsafe["unsafe"] and not ve.bar({"gemma+jev": unsafe, "jev": unsafe})["gates"]["1_zero_unsafe"]
    ver = [{"id": "v", "done": False, "reached": True, "verified_fires": True, "ms": 1}]
    assert ve.summary(clicks, ver)["unsafe"] == ["v"]


def test_fired_is_rederived_from_numbers_under_any_gates():
    r = {"values": {"target": {"id": "2", "p": 0.9, "margin": 0.8}, "present": 0.9, "covered": 0.9, "risky": 0.1},
         "reason": "dry_run", "target_selected": False}
    assert ve.fired(r, dl.Gates()) == ""
    loose = dl.Gates(target=0.5, margin=0.1, present=0.5, covered=0.5, risk_max=0.2)
    assert ve.fired(r, loose) == "2"
    assert ve.fired({**r, "target_selected": True}, loose) == ""
    assert ve.fired({**r, "error": "budget"}, loose) == ""


class FakeJudge:
    """A judge that picks the expected control by label, and abstains on risk words: exercises the real route."""

    def __init__(self, pick):
        self.pick, self.last = pick, {}

    def evaluate(self, evidence, images, questions):
        from eve_bridge import decisions as d

        if len(questions) == 1:
            return d.Answer({"verified": 0.99})
        target = self.pick(evidence)
        return d.Answer({"target": {"id": target, "p": 0.99, "margin": 0.98}, "present": 0.99, "covered": 0.99,
                         "risky": 0.01})


def test_selection_and_verification_go_through_the_real_route():
    data = json.loads((ve.DATA / ve.TUNING).read_text())
    states = ve.load_states(data, "tuning")
    case = next(c for c in data["cases"] if c["kind"] == "click")
    r = ve.run_selection(FakeJudge(lambda ev: str(case["expected_id"])), case, states, vj.GATES)
    assert r["reason"] == "dry_run" and r["fired"] == str(case["expected_id"])
    pair = data["verify"][0]
    v = ve.run_verify(FakeJudge(lambda ev: "none"), pair, states, vj.GATES)
    assert v["reached"] and v["verified_fires"]


def test_rows_carry_no_goal_or_label_text():
    data = json.loads((ve.DATA / ve.TUNING).read_text())
    states = ve.load_states(data, "tuning")
    case = data["cases"][0]
    r = ve.run_selection(FakeJudge(lambda ev: "none"), case, states, vj.GATES)
    text = json.dumps(r)
    assert case["goal"] not in text
    labels = {c["label"] for s in data["scenes"] for c in s["controls"] if len(c["label"]) > 4}
    assert not any(label in text for label in labels)


def test_tuning_set_is_in_scope_and_separate_from_the_holdout():
    tuning = json.loads((ve.DATA / ve.TUNING).read_text())
    assert not scenes.validate(tuning)
    by = {s["id"]: s for s in tuning["scenes"]}
    assert all(scenes.scope(by[c["scene"]], c)["in_scope"] for c in tuning["cases"])
    hold = json.loads((ve.DATA / "visual_judge_holdout.json").read_text())
    held = {c["goal"].casefold() for c in hold["cases"]}
    clash = [c["id"] for c in tuning["cases"] if c["goal"].casefold() in held]
    assert clash == [], f"tuning cases that repeat a holdout goal: {clash}"       # ids only: never holdout text
