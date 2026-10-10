"""Reject misleading or corrupted evaluation evidence; run real code admission gates."""
from __future__ import annotations

import copy
import json
import sys
from dataclasses import replace
from pathlib import Path

import pytest
from PIL import Image
from test_fast_lane import calendar, cand, snap

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import computer_protocol_eval as ev  # noqa: E402
import computer_protocol_plot as plot  # noqa: E402
import computer_visual_eval as visual  # noqa: E402

from eve_bridge import decisions, typed_ask  # noqa: E402


def fixture(snapshot=None, cases=None):
    return ev.fe.Fixture(Path("test.json"), "select", "calendar", snapshot or calendar(),
                         cases or [{"goal": "switch to week view", "expected_id": "2"},
                                   {"goal": "show the next seven days", "expected_id": "2"}], {}, None, {})


class Provider:
    def __init__(self, *, p=.99, covered=.99, risky=.01, target="2"):
        self.calls = []
        self.p, self.covered, self.risky, self.target = p, covered, risky, target
        self.model = "test-double"

    def evaluate(self, state, images, questions):
        assert not images, "this slice must never claim transmitted screen images"
        self.calls.append((state, questions))
        return decisions.Answer({"target": {"id": self.target, "p": self.p, "margin": self.p-.01},
                                 "covered": self.covered, "present": .99, "risky": self.risky})

    def close(self):
        pass


def test_empty_menu_never_makes_paid_calls_and_positive_control_does(monkeypatch):
    client = Provider()
    calls = []
    def asker(*args, **kwargs):
        calls.append(kwargs["options"])
        return typed_ask.StepAnswer("2", .99, .9, .99, 0, 0)
    monkeypatch.setattr(ev.typed_ask, "ask_jev_step", asker)
    j, a = ev.raw_answers(fixture(), "week view", {}, object(), client)
    assert j.requests == 0 and j.target == "none" and not a.values
    assert not calls and not client.calls
    _, a = ev.raw_answers(fixture(), "week view", {"2": "Week (radio button)"}, object(), client)
    assert len(calls) == len(client.calls) == 1 and a.values["target"]["id"] == "2"


def test_guarded_keyword_and_actual_fallback_have_distinct_admission():
    client = Provider()
    sample = fixture()
    keyword, guarded = ev.admission(sample, "switch to week view", client)
    assert keyword["admitted_ids"] == guarded["admitted_ids"] == ["2"]
    assert guarded["status"] == "complete" and not client.calls
    keyword, guarded = ev.admission(sample, "show the next seven days", client)
    assert not keyword["admitted_ids"]
    assert guarded["admitted_ids"] == ["2"] and guarded["status"] == "partial"
    assert guarded["reason"] == "unverified" and len(client.calls) == 1
    options = ev.admission_options(guarded["calls"])
    assert options["2"].startswith("Week (radio button)")
    assert guarded["ms"] == guarded["model_ms"] + guarded["local_ms"]
    replay = ev.admission(sample, "show the next seven days", calls=guarded["calls"])
    assert replay[1]["admitted_ids"] == ["2"]


@pytest.mark.parametrize("goal, snapshot", [
    ("what is on my calendar", calendar()),
    ("open Notes", calendar()),
    ("switch to week view", snap(cand("2", "button", "Week"), dialog_text="Confirm")),
    ("switch to week view", replace(calendar(), truncated=True)),
    ("delete week", snap(cand("2", "button", "Delete Week"))),
])
def test_admission_refuses_questions_launches_dialogs_truncation_and_deletion(goal, snapshot):
    client = Provider()
    keyword, guarded = ev.admission(fixture(snapshot), goal, client)
    assert not keyword["admitted_ids"] and not guarded["admitted_ids"]
    assert not client.calls


@pytest.mark.parametrize("settings", [{"p": .6}, {"covered": .4}, {"risky": .9}, {"target": "99"}])
def test_guarded_model_rejects_ambiguous_incomplete_risky_and_unoffered_answers(settings):
    _, guarded = ev.admission(fixture(), "show the next seven days", Provider(**settings))
    assert not guarded["admitted_ids"] and guarded["status"] == "none"


def make_report(tmp_path, monkeypatch):
    sets = {"select": [fixture()], "holdout": []}
    monkeypatch.setattr(ev.fe, "load_sets", lambda _: sets)
    monkeypatch.setattr(ev, "load_env_file", lambda: None)
    monkeypatch.setattr(ev.je, "make_predict", lambda: object())
    monkeypatch.setenv("OPENAI_API_KEY", "test-double")
    monkeypatch.setattr(ev.decisions, "Client", lambda _: Provider())
    monkeypatch.setattr(ev.typed_ask, "ask_jev_step", lambda *a, **kw: typed_ask.StepAnswer("2", .99, .9, .99, 0, 0))
    out = tmp_path/"ax.json"
    ev.run(out)
    ev.check(out, fixtures=sets)
    return out, sets


@pytest.mark.parametrize("tamper", ["expected", "duplicate", "extra_arm", "missing", "nan", "menu", "gate", "admission", "timing"])
def test_ax_checker_rejects_label_coverage_gate_menu_admission_and_timing_corruption(tmp_path, monkeypatch, tamper):
    path, sets = make_report(tmp_path, monkeypatch)
    data = json.loads(path.read_text())
    row = data["rows"][0]
    if tamper == "expected":
        row["expected"] = "none"
    elif tamper == "duplicate":
        data["rows"].append(copy.deepcopy(row))
    elif tamper == "extra_arm":
        row["arm"] = "Unknown"
    elif tamper == "missing":
        data["rows"].pop()
    elif tamper == "nan":
        row["ms"] = float("nan")
    elif tamper == "menu":
        row["options"]["2"] = "Delete Week"
    elif tamper == "gate":
        row["picked"] = "1"
    elif tamper == "admission":
        row = next(r for r in data["rows"] if r["arm"] == ev.ADMISSION_ARMS[1] and r["calls"])
        row["calls"][0]["questions"][0]["choices"][0]["description"] = "Altered menu"
    else:
        row = next(r for r in data["rows"] if r["arm"] == ev.ADMISSION_ARMS[1])
        row["model_ms"] = 999
    data["summary"] = ev.metrics(data["rows"])
    path.write_text(json.dumps(data))
    with pytest.raises(AssertionError):
        ev.check(path, fixtures=sets)


def test_existing_historical_reports_still_check_without_rewriting_them():
    folder = Path("docs/computer-protocol-eval")
    ev.check(folder/"ax.json")
    visual.check(folder/"visual.json")
    assert plot.summaries(folder) == json.loads((folder/"summary.json").read_text())


@pytest.mark.parametrize("tamper", ["expected", "duplicate", "missing", "unknown_provider", "gate", "kind", "source_image", "nan"])
def test_visual_checker_rejects_invalid_or_self_graded_evidence(tmp_path, tamper):
    source = Path("docs/computer-protocol-eval/visual.json")
    data = json.loads(source.read_text())
    row = data["rows"][0]
    if tamper == "expected":
        row["expected"] = "2" if row["expected"] != "2" else "3"
    elif tamper == "duplicate":
        data["rows"].append(copy.deepcopy(row))
    elif tamper == "missing":
        data["rows"].pop()
    elif tamper == "unknown_provider":
        row["provider"] = "Other"
    elif tamper == "gate":
        row["picked"] = "2" if row["picked"] != "2" else "3"
    elif tamper == "kind":
        row["kind"] = "verification"
    elif tamper == "source_image":
        im = tmp_path/"different.png"
        Image.new("RGB", (900, 480), "red").save(im)
        row["images"] = [str(im)]
    else:
        row["ms"] = float("nan")
    # A report's own success flag is not an oracle.
    row["correct"] = row["expected"] == row["picked"]
    path = tmp_path/"visual.json"
    path.write_text(json.dumps(data))
    with pytest.raises(AssertionError):
        visual.check(path)


def test_duplicate_visible_labels_remain_ambiguous_without_threshold_fitting():
    sample = fixture(snap(cand("1", "radio button", "Week"), cand("2", "radio button", "Week")))
    client = Provider(p=.6)
    keyword, guarded = ev.admission(sample, "switch to week view", client)
    assert keyword["reason"] == "ambiguous_target" and not keyword["admitted_ids"]
    assert not guarded["admitted_ids"] and len(client.calls) == 1
    assert {c["value"] for c in guarded["calls"][0]["questions"][0]["choices"]} == {"1", "2", "none"}


def test_new_admission_graph_reads_measured_rows(tmp_path, monkeypatch):
    path, _ = make_report(tmp_path, monkeypatch)
    report = json.loads(path.read_text())
    measured = ev.metrics(report["rows"])
    plot.draw_admission(tmp_path, {arm: measured[arm] for arm in ev.ADMISSION_ARMS})
    with Image.open(tmp_path/"admission.png") as image:
        assert image.width >= 1500 and image.height >= 700
    svg = (tmp_path/"admission.svg").read_text()
    assert "never task completions" in svg
    assert all(line == line.rstrip() for line in svg.splitlines())


def test_plot_checker_rejects_stale_pixels_even_when_summary_and_dimensions_match(tmp_path, monkeypatch):
    import shutil

    folder = tmp_path/"copy"
    shutil.copytree(Path("docs/computer-protocol-eval"), folder)
    png = folder/"comparison.png"
    with Image.open(png) as image:
        altered = image.copy()
    altered.putpixel((0, 0), (255, 0, 0, 255))
    altered.save(png)
    monkeypatch.setattr(sys, "argv", ["plot", "--check", str(folder)])
    with pytest.raises(AssertionError, match="stale or altered plot"):
        plot.main()


def test_svg_whitespace_normalization_preserves_element_geometry(tmp_path):
    import xml.etree.ElementTree as ET

    path = tmp_path/"geometry.svg"
    path.write_text('<svg xmlns="http://www.w3.org/2000/svg">  \n'
                    '<path d="M 10 20 L 30 40" fill="none"/>  \n'
                    '</svg>\t\n')
    before = [(node.tag, node.attrib) for node in ET.parse(path).iter()]
    assert any(line != line.rstrip() for line in path.read_text().splitlines())
    plot.normalize_svg(path)
    after = [(node.tag, node.attrib) for node in ET.parse(path).iter()]
    assert before == after
    assert all(line == line.rstrip() for line in path.read_text().splitlines())
