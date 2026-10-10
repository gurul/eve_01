"""Behavioral checks for the offline expression experiment and its failure paths."""

import copy
import json
import re
from pathlib import Path

import numpy as np
import pytest

from eve_bridge.emotion_policy import (
    CHIRPS,
    LABELS,
    ExpressionController,
    fit_temperature,
    fit_threshold,
    load_dataset,
    metrics,
    probabilities,
    selection_metrics,
)

DATASET = Path(__file__).parent / "fixtures/emotion/scenarios.json"


def test_expression_and_chirp_vocabulary_exists_in_actual_firmware():
    header = Path(__file__).resolve().parents[2] / "firmware/claude_pet_stackchan/src/mood.h"
    source = header.read_text()
    kinds = set(re.findall(r"\bMOOD_([A-Z]+)\b", source))
    sounds = set(re.findall(r"\bMOODCHIRP_([A-Z]+)\b", source))
    assert {label.upper() for label in LABELS} <= kinds
    assert {sound.upper() for sound in CHIRPS.values() if sound} <= sounds


def p(label):
    return np.eye(len(LABELS))[LABELS.index(label)]


def test_dataset_disjoint_and_negative_controls(tmp_path):
    rows = load_dataset(DATASET)
    assert len(rows) == 216
    assert {s: sum(r["split"] == s for r in rows) for s in {r["split"] for r in rows}} == {
        "train": 96, "dev": 36, "calibration": 36, "test": 48}
    for corruption in ("duplicate_text", "family_leak", "bad_label", "empty_text", "duplicate_id", "missing_class"):
        data = json.loads(DATASET.read_text())
        if corruption == "duplicate_text":
            data["rows"][1]["text"] = data["rows"][0]["text"]
        elif corruption == "family_leak":
            data["rows"][0]["split"] = "test"
        elif corruption == "bad_label":
            data["rows"][0]["label"] = "nonexistent"
        elif corruption == "empty_text":
            data["rows"][0]["text"] = " "
        elif corruption == "duplicate_id":
            data["rows"][1]["id"] = data["rows"][0]["id"]
        else:
            data["rows"] = [r for r in data["rows"] if not (r["split"] == "test" and r["label"] == "calm")]
        path = tmp_path / "bad.json"
        path.write_text(json.dumps(data))
        with pytest.raises(ValueError):
            load_dataset(path)


def test_metrics_perfect_wrong_uniform_and_nonfinite():
    y = np.arange(len(LABELS))
    perfect = metrics(np.eye(len(LABELS)), y)
    assert perfect["accuracy"] == perfect["macro_f1"] == 1
    assert perfect["brier"] == perfect["ece_10_bins"] == perfect["nll"] == 0
    wrong = metrics(np.roll(np.eye(len(LABELS)), 1, axis=1), y)
    assert wrong["accuracy"] == wrong["macro_f1"] == 0
    assert wrong["brier"] == 2 and wrong["ece_10_bins"] == 1
    uniform = metrics(np.full((6, 6), 1 / 6), y)
    assert uniform["accuracy"] == pytest.approx(1 / 6)
    assert uniform["nll"] == pytest.approx(np.log(6))
    for bad in (np.zeros((6, 6)), np.full((6, 6), np.nan), np.ones((6, 5))):
        with pytest.raises(ValueError):
            metrics(bad, y)


def test_temperature_fits_nll_without_changing_ranking():
    z = np.array([[20, 0, 0, 0, 0, 0], [20, 0, 0, 0, 0, 0]], dtype=float)
    y = [0, 1]
    t = fit_temperature(z, y)
    assert t > 1
    assert metrics(probabilities(z, t), y)["nll"] < metrics(probabilities(z), y)["nll"]
    assert np.array_equal(probabilities(z, t).argmax(1), z.argmax(1))
    with pytest.raises(ValueError):
        probabilities(z, 0)


def test_threshold_cannot_pass_by_abstaining_or_accepting_errors():
    confident_wrong = np.tile(p("happy"), (10, 1))
    assert fit_threshold(confident_wrong, [0] * 10) == 1.01
    result = selection_metrics(confident_wrong, [0] * 10, 1.01)
    assert result == {"accepted": 0, "wrong": 0, "coverage": 0, "precision": None}
    confident_correct = np.eye(6)[[0, 1] * 5]
    assert fit_threshold(confident_correct, [0, 1] * 5) == 1
    assert selection_metrics(confident_correct, [0, 1] * 5, 1)["precision"] == 1


def test_controller_dwell_chirp_cooldown_duplicate_mute_and_ttl():
    c = ExpressionController(0.8)
    first = c.update(p("happy"), event_id="a", event_at=1, now=1)
    assert first.label == "happy" and first.chirp == "warble"
    duplicate = c.update(p("curious"), event_id="a", event_at=1.1, now=1.1)
    assert duplicate.label == "happy" and duplicate.chirp is None
    too_soon = c.update(p("curious"), event_id="b", event_at=1.2, now=1.2)
    assert too_soon.label == "happy"
    later = c.update(p("curious"), event_id="c", event_at=3, now=3)
    assert later.label == "curious" and later.chirp is None
    muted = c.update(p("affection"), event_id="d", event_at=10, now=10, muted=True)
    assert muted.label == "affection" and muted.chirp is None
    expired = c.update(p("happy"), event_id="d", event_at=10, now=15)
    assert expired.label == "calm" and expired.chirp is None


@pytest.mark.parametrize("phase", ["listening", "thinking", "speaking", "working", "asking", "done", "error", "sleep"])
def test_agent_phases_keep_control(phase):
    c = ExpressionController(0.8)
    result = c.update(p("happy"), event_id="a", event_at=1, now=1, phase=phase)
    assert result.label == "calm" and result.chirp is None and result.reason == "phase owns expression"


def test_startle_requires_fresh_physical_event():
    c = ExpressionController(0.8)
    quoted_bang = c.update(p("startled"), event_id="a", event_at=1, now=1)
    assert quoted_bang.label == "calm" and quoted_bang.chirp is None
    actual_bang = c.update(p("startled"), event_id="b", event_at=3, now=3, fresh_physical_event=True)
    assert actual_bang.label == "startled" and actual_bang.chirp == "startle"


def test_stale_future_out_of_order_invalid_and_uncertain_inputs_do_not_chirp():
    c = ExpressionController(0.8)
    for event_at in (-10, 5):
        assert c.update(p("happy"), event_id="x", event_at=event_at, now=1).chirp is None
    for bad in ([1, 2], [np.nan] * 6, [-1, 1, 1, 0, 0, 0], None):
        result = c.update(bad, event_id="bad", event_at=1, now=1)
        assert result.label == "calm" and result.chirp is None
    assert c.update([1 / 6] * 6, event_id="a", event_at=1, now=1).label == "calm"
    c.update(p("happy"), event_id="b", event_at=3, now=3)
    result = c.update(p("startled"), event_id="late", event_at=2, now=4, fresh_physical_event=True)
    assert result.label == "happy" and result.chirp is None
    before = copy.deepcopy(c.__dict__)
    with pytest.raises(ValueError):
        c.update(p("happy"), event_id="nan", event_at=float("nan"), now=5)
    assert c.__dict__ == before
