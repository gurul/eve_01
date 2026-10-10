"""tools/fastlane_eval.py with a fake predictor over tiny fixtures: the menu and the
keyword baseline, the Wilson interval, the coverage table, the k buckets, the cost
model, style and threshold selection on select/ only, the count validation, the
fixture make/redact round trips, the ship decision's four conditions, and the shadow
report's confusion counting."""

from __future__ import annotations

import json
from pathlib import Path

import fastlane_eval as fe  # tools/ is on the pytest path (pyproject.toml)
import pytest

from eve_bridge import ax_candidates as ax
from eve_bridge.decider import Decider

# ---- fakes -------------------------------------------------------------------------


def node(role: str, title: str = "", *, frame=(0, 0, 10, 10), children=(), actions=(), value=None,
         subrole: str = "") -> dict:
    return {"role": role, "subrole": subrole, "title": title, "description": "", "value": value,
            "frame": list(frame), "actions": list(actions), "enabled": True, "focused": False,
            "children": list(children), "ancestor_roles": []}


def _ancestors(n: dict, above: list[str]) -> None:
    n["ancestor_roles"] = list(above)
    for c in n["children"]:
        _ancestors(c, above + [n["role"]])


def raw(*children: dict, app="Calendar", bundle="com.apple.iCal", title="September 2026") -> dict:
    root = node("AXWindow", title, frame=(0, 0, 900, 600), subrole="AXStandardWindow", children=children)
    _ancestors(root, [])

    def count(n: dict) -> int:
        return 1 + sum(count(c) for c in n["children"])
    return {"app": app, "bundle": bundle, "pid": 7, "title": title, "fullscreen": False, "node_count": count(root),
            "truncated": False, "secs": 0.05, "root": root}


def calendar_raw() -> dict:
    return raw(node("AXRadioButton", "Day", frame=(10, 10, 40, 20)),
               node("AXRadioButton", "Week", frame=(60, 10, 40, 20)),
               node("AXRadioButton", "Month", frame=(110, 10, 40, 20), value=1),
               node("AXButton", "Today", frame=(200, 10, 40, 20)),
               node("AXButton", "Delete Event", frame=(300, 10, 60, 20)),
               node("AXStaticText", "Dentist 3pm", frame=(10, 100, 100, 20), actions=["AXPress"]))


def settings_raw() -> dict:
    return raw(node("AXButton", "Back", frame=(10, 10, 30, 20)),
               node("AXRow", "", frame=(10, 40, 200, 20), children=[node("AXStaticText", "", frame=(12, 42, 100, 16),
                                                                          value="Appearance")]),
               node("AXRow", "", frame=(10, 70, 200, 20), children=[node("AXStaticText", "", frame=(12, 72, 100, 16),
                                                                          value="Software Update")]),
               app="System Settings", bundle="com.apple.systempreferences", title="General")


def dump(r: dict) -> dict:
    """What `python -m eve_bridge.ax_candidates --json` prints."""
    snap = ax.snapshot_from_raw(r, seq=1, screen=(1440, 900))
    return {"raw": r, "snapshot": snap.to_dict(), "screen": [1440, 900], "candidates": []}


def ids(snapshot: ax.Snapshot) -> dict[str, str]:
    return {c.label: c.id for c in snapshot.elements}


def fixture_file(root: Path, set_name: str, name: str, r: dict, cases: list[dict]) -> Path:
    data = fe.make_fixture(dump(r), set_name=set_name, redacted=True)
    data["cases"] = cases
    path = root / set_name / f"{name}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data), encoding="utf-8")
    return path


def calendar_cases(snapshot: ax.Snapshot) -> list[dict]:
    by = ids(snapshot)
    return [
        {"goal": "switch to week view", "expected_id": by["Week"], "expected": "click", "overlap": True,
         "distractor": False, "text": None, "must_not_offer": [by["Delete Event"]], "note": ""},
        {"goal": "jump to today", "expected_id": by["Today"], "expected": "click", "overlap": True,
         "distractor": False, "text": None, "must_not_offer": [], "note": ""},
        {"goal": "show the whole month", "expected_id": by["Month"], "expected": "click", "overlap": True,
         "distractor": False, "text": None, "must_not_offer": [], "note": ""},
        {"goal": "go to the dentist", "expected_id": by["Dentist 3pm"], "expected": "click", "overlap": True,
         "distractor": False, "text": None, "must_not_offer": [], "note": ""},
        {"goal": "print the calendar", "expected_id": None, "expected": "abstain", "overlap": False,
         "distractor": False, "text": None, "must_not_offer": [], "note": "no print control"},
    ]


def settings_cases(snapshot: ax.Snapshot) -> list[dict]:
    by = ids(snapshot)
    return [
        {"goal": "open the appearance settings", "expected_id": by["Appearance"], "expected": "click",
         "overlap": True, "distractor": False, "text": None, "must_not_offer": [], "note": ""},
        {"goal": "check for updates", "expected_id": by["Software Update"], "expected": "click", "overlap": False,
         "distractor": True, "text": None, "must_not_offer": [], "note": "update vs updates"},
        {"goal": "go back", "expected_id": by["Back"], "expected": "click", "overlap": True, "distractor": False,
         "text": None, "must_not_offer": [], "note": ""},
    ]


def sets_root(tmp_path: Path) -> Path:
    cal = ax.snapshot_from_raw(calendar_raw(), seq=1, screen=(1440, 900))
    st = ax.snapshot_from_raw(settings_raw(), seq=1, screen=(1440, 900))
    fixture_file(tmp_path, "select", "calendar-month", calendar_raw(), calendar_cases(cal))
    fixture_file(tmp_path, "holdout", "settings-general", settings_raw(), settings_cases(st))
    return tmp_path


def result(**kw) -> fe.CaseResult:
    base = dict(fixture="f", set_name="select", app="A", goal="g", expected="1", overlap=True, distractor=False,
                k=4, pick="1", correct=True, wrong_click=False, escalated=False, p_top=0.6, margin=0.3,
                confidence=0.3, ms=10.0, tokens=100, overflow=False, dropped_budget=0, dropped_cap=0, kw_pick="1",
                kw_correct=True)
    base.update(kw)
    return fe.CaseResult(**base)


# ---- menu and baseline ------------------------------------------------------------------


def test_menu_mirrors_the_lane_and_never_offers_sensitive_labels() -> None:
    snap = ax.snapshot_from_raw(calendar_raw(), seq=1, screen=(1440, 900))
    options, by_id, dropped = fe.build_menu(snap, "switch to week view")
    assert list(options)[-2:] == ["reobserve", "abstain"] and dropped == 0
    labels = [by_id[k].label for k in options if k in by_id]
    assert labels[0] == "Week" and "Delete Event" not in labels
    assert options[ids(snap)["Week"]] == "Week (radio button)"                     # decider.render_option
    assert fe.gate_pick("switch to week view", options, by_id) == ids(snap)["Week"]   # the lane's keyword gate
    assert fe.gate_pick("show the next seven days", options, by_id) == ""


def test_keyword_baseline_picks_overlap_ties_by_menu_order_and_abstains_on_none() -> None:
    options = {"1": "click button: Today", "2": "click radio button: Week", "3": "click button: Week calendar",
               "reobserve": "…", "abstain": "…"}
    assert fe.keyword_baseline("show the week calendar", options) == "3"   # two tokens beat one
    assert fe.keyword_baseline("switch to week view", options) == "2"      # "view" is a stop-word: tie → menu order
    assert fe.keyword_baseline("print everything", options) == "abstain"
    # with the candidates it scores label + value as whole tokens, never the "(role)" suffix
    snap = ax.snapshot_from_raw(calendar_raw(), seq=1, screen=(1440, 900))
    opts, by_id, _ = fe.build_menu(snap, "press the button")
    assert fe.keyword_baseline("press the button", opts, by_id) == "abstain"       # "button" is only in the role
    assert fe.keyword_baseline("switch to week view", opts, by_id) == ids(snap)["Week"]


def test_fake_predictor_is_system_one_shaped_and_deterministic() -> None:
    predict = fe.make_fake_predict()
    q = {"pick": {"type": "choice", "instructions": "x", "criteria": {"1": "click button: Today",
                                                                       "2": "click radio button: Week",
                                                                       "reobserve": "r", "abstain": "a"}}}
    out = predict({"goal": "switch to week view"}, q)
    a = out["answers"]["pick"]
    assert a["choice"] == "2" and abs(sum(a["probabilities"].values()) - 1.0) < 0.01
    assert out["usage"]["input_tokens"] > 0 and predict({"goal": "switch to week view"}, q) == out


# ---- statistics --------------------------------------------------------------------------


def test_wilson_interval_known_value() -> None:
    lo, hi = fe.wilson(24, 30)
    assert abs(lo - 0.627) < 0.002 and abs(hi - 0.905) < 0.002
    assert fe.wilson(0, 0) == (0.0, 1.0)
    assert abs(fe.wilson(10, 10)[1] - 1.0) < 1e-9


def test_coverage_table_and_threshold_choice_use_only_the_given_results() -> None:
    results = [result(p_top=0.9, margin=0.5), result(p_top=0.45, margin=0.12, correct=False, wrong_click=True),
               result(p_top=0.35, margin=0.06), result(pick="", correct=False, escalated=False, p_top=0.0, margin=0.0)]
    g = fe.gated(results, 0.4, 0.10)
    assert g["n_covered"] == 2 and g["coverage"] == 0.5 and g["gated_top1"] == 0.5
    table = fe.coverage_table(results)
    assert len(table) == len(fe.P_GRID) * len(fe.MARGIN_GRID)
    low = next(r for r in table if r["p_min"] == 0.2 and r["margin_min"] == 0.05)
    assert low["coverage"] == 0.75 and abs(low["gated_top1"] - 2 / 3) < 1e-9
    # the highest grid point with coverage ≥ 0.70: (0.3, 0.05) covers 3/4, (0.4, x) only 2/4
    assert fe.pick_thresholds(results) == (0.3, 0.05, True)
    sparse = [result(p_top=0.25, margin=0.01)] * 3
    assert fe.pick_thresholds(sparse) == (0.2, 0.05, False)


def test_k_buckets() -> None:
    results = [result(k=4), result(k=8, correct=False, wrong_click=True, kw_correct=False), result(k=12)]
    rows = fe.k_buckets(results)
    assert [(r["bucket"], r["n"]) for r in rows] == [("2-5", 1), ("6-10", 1), ("11+", 1)]
    assert rows[1]["top1"] == 0.0 and rows[1]["keyword"] == 0.0 and rows[0]["top1"] == 1.0


def test_cost_weighted_score_uses_the_three_costs() -> None:
    p, m = 0.4, 0.1
    assert fe.cost_weighted([result(p_top=0.9, margin=0.5)], p, m) == fe.COST_CORRECT
    assert fe.cost_weighted([result(p_top=0.9, margin=0.5, correct=False, wrong_click=True)], p, m) == -fe.COST_WRONG
    assert fe.cost_weighted([result(p_top=0.3, margin=0.5)], p, m) == -fe.COST_ESCALATE      # uncovered
    abstain_ok = result(expected="abstain", pick="abstain", p_top=0.9, margin=0.5)
    assert fe.cost_weighted([abstain_ok], p, m) == -fe.COST_ESCALATE                          # astra still acts
    mixed = [result(p_top=0.9, margin=0.5), result(p_top=0.9, margin=0.5, correct=False, wrong_click=True)]
    assert abs(fe.cost_weighted(mixed, p, m) - (fe.COST_CORRECT - fe.COST_WRONG) / 2) < 1e-9
    assert fe.cost_weighted([], p, m) == 0.0


def test_summarize_reports_every_field() -> None:
    results = [result(), result(overlap=False, correct=False, wrong_click=True, kw_correct=False, p_top=0.5, margin=0.1),
               result(distractor=True, dropped_budget=2, overflow=True), result(pick="", correct=False, p_top=0.0)]
    s = fe.summarize(results, 0.4, 0.10)
    assert s["n"] == 4 and s["top1"] == 0.5 and s["keyword_top1"] == 0.75 and s["rejected"] == 1
    assert s["no_overlap"] == {"n": 1, "real": 0.0, "keyword": 0.0}
    assert s["distractor"]["n"] == 1 and s["distractor"]["real"] == 1.0
    assert s["options_truncated"] == 1 and s["state_truncated"] == 1
    assert s["thresholds"] == (0.4, 0.10) and s["n_covered"] == 3
    assert len(s["p_top_correct"]) == 3 and s["ms_p50"] == 10.0 and s["tokens_mean"] == 100.0
    lines: list[str] = []
    fe.print_summary("t", s, lines.append)
    assert lines[0].startswith("== t: n=4") and any("accuracy-vs-coverage" in ln for ln in lines)


def test_choose_style_prefers_cost_then_top1() -> None:
    good = [result(p_top=0.9, margin=0.5)] * 4
    bad = [result(p_top=0.9, margin=0.5, correct=False, wrong_click=True)] * 4
    same_cost_more_top1 = [result(p_top=0.9, margin=0.5)] * 3 + [result(p_top=0.3, margin=0.5, correct=False,
                                                                          wrong_click=True)]
    style, thresholds, reached = fe.choose_style({"jev": bad, "compact": good, "hinted": same_cost_more_top1})
    assert style == "compact" and thresholds == (0.6, 0.15) and reached


# ---- ship decision -------------------------------------------------------------------------


def _holdout(n_correct: int = 9, n_wrong: int = 1, kw_correct_no_overlap: bool = False) -> list[fe.CaseResult]:
    out = [result(overlap=False, kw_correct=kw_correct_no_overlap, p_top=0.9, margin=0.5) for _ in range(n_correct)]
    out += [result(overlap=False, correct=False, wrong_click=True, kw_correct=kw_correct_no_overlap, p_top=0.9,
                   margin=0.5) for _ in range(n_wrong)]
    return out


def test_ship_decision_needs_all_four_conditions() -> None:
    ok, numbers = fe.ship_decision(_holdout(), 0.4, 0.10)
    assert ok and numbers["gated_top1"] == 0.9 and numbers["coverage"] == 1.0
    # 1. gated top-1 under 0.80
    assert not fe.ship_decision(_holdout(7, 3), 0.4, 0.10)[0]
    # 2. coverage under 0.70 (thresholds above every p_top)
    assert not fe.ship_decision(_holdout(), 0.95, 0.10)[0]
    # 3. the keyword baseline is as good on overlap:false
    assert not fe.ship_decision(_holdout(kw_correct_no_overlap=True), 0.4, 0.10)[0]
    # 4. cost-weighted ≤ 0 although the accuracy conditions hold: correct abstains earn nothing
    abstains = [result(expected="abstain", pick="abstain", overlap=False, kw_correct=False, kw_pick="1", p_top=0.9,
                       margin=0.5) for _ in range(10)]
    ok4, numbers4 = fe.ship_decision(abstains, 0.4, 0.10)
    assert numbers4["gated_top1"] == 1.0 and numbers4["coverage"] == 1.0 and numbers4["cost_weighted"] < 0 and not ok4
    assert not fe.ship_decision([], 0.4, 0.10)[0]


# ---- fixtures: counts, make, redact, end to end ----------------------------------------------


def test_count_errors_name_every_shortfall(tmp_path: Path) -> None:
    sets = fe.load_sets(sets_root(tmp_path))
    assert fe.count_errors(sets, min_select=5, min_holdout=3, min_apps=1, min_no_overlap=1, min_distractor=1) == []
    errors = fe.count_errors(sets, min_select=30, min_holdout=40, min_apps=5, min_no_overlap=12, min_distractor=6)
    assert [e.split(",")[0] for e in errors] == ["select has 5 cases", "holdout has 3 cases", "holdout covers 1 apps (System Settings)",
                                                 "holdout has 1 overlap:false cases", "holdout has 1 distractor cases"]
    fixture_file(tmp_path, "holdout", "calendar-month", calendar_raw(), [])
    sets = fe.load_sets(tmp_path)
    assert any(e.startswith("fixtures in both sets: calendar-month") for e in
               fe.count_errors(sets, min_select=0, min_holdout=0, min_apps=0, min_no_overlap=0, min_distractor=0))


def test_make_fixture_rederives_and_refuses_truncated_walks() -> None:
    d = dump(calendar_raw())
    data = fe.make_fixture(d, set_name="holdout")
    assert data["captured"]["ax_candidates_sha"] == fe.ax_candidates_sha() and len(data["captured"]["ax_candidates_sha"]) == 64
    assert data["captured"]["redacted"] is False and data["captured"]["truncated"] is False
    assert data["snapshot"] == d["snapshot"] and data["cases"] == [] and data["screen"] == [1440, 900]
    d["raw"]["truncated"] = True
    with pytest.raises(ValueError, match="truncated"):
        fe.make_fixture(d, set_name="select")
    with pytest.raises(ValueError, match="set must be"):
        fe.make_fixture(dump(calendar_raw()), set_name="train")


def test_redact_replaces_whole_labels_in_raw_and_rederives_the_snapshot() -> None:
    data = fe.make_fixture(dump(calendar_raw()), set_name="select")
    out, hits = fe.redact_fixture(data, {"Dentist 3pm": "Meeting A", "Nope": "x"})
    assert hits == ["Dentist 3pm"] and out["captured"]["redacted"] is True
    snap = ax.snapshot_from_raw(out["raw"], seq=1, screen=(1440, 900))
    assert snap.to_dict() == out["snapshot"]
    assert "Meeting A" in ids(snap) and "Dentist 3pm" not in ids(snap)
    assert "Dentist 3pm" not in json.dumps(out)


def test_cli_make_fixture_and_redact_round_trip(tmp_path: Path, capsys) -> None:
    rawfile = tmp_path / "raw.json"
    rawfile.write_text(json.dumps(dump(calendar_raw())), encoding="utf-8")
    out = tmp_path / "select" / "calendar-month.json"
    assert fe.main(["--make-fixture", str(rawfile), "--out", str(out), "--set", "select"]) == 0
    assert "cases: 0" in capsys.readouterr().out
    assert fe.main(["--redact", str(out), "--map", "Dentist 3pm=Meeting A", "--map", "Ghost=x"]) == 0
    text = capsys.readouterr().out
    assert "1 replacements" in text and "not found: ['Ghost']" in text
    data = json.loads(out.read_text())
    assert data["captured"]["redacted"] is True and "Dentist 3pm" not in out.read_text()
    assert fe.main(["--redact", str(out), "--map", "bad"]) == 2
    with pytest.raises(SystemExit):
        fe.main(["--redact", str(out), "--shadow-report"])                       # one mode at a time


def test_offline_eval_end_to_end_with_the_fake_predictor(tmp_path: Path, capsys) -> None:
    root = sets_root(tmp_path)
    results_out = tmp_path / "results.json"
    code = fe.main(["--fixtures", str(root), "--fake-predictor", "--min-select", "5", "--min-holdout", "3",
                    "--results-out", str(results_out)])
    out = capsys.readouterr().out
    assert code == 0 and out.rstrip().endswith("EVAL_COMPLETE")
    for style in fe.STYLES:
        assert f"== style {style} / select" in out and f"== style {style} / holdout" in out
    assert "keyword baseline" in out and "Wilson 95%" in out and "by k:" in out and "WINNER: style=" in out
    assert "keyword gate decided" in out and "not independent" in out
    assert "SHIP DECISION:" in out
    data = json.loads(results_out.read_text())
    sel = data["styles"]["compact"]["select"]
    assert len(sel) == 5 and all(r["set_name"] == "select" for r in sel)
    week = next(r for r in sel if r["goal"] == "switch to week view")
    assert week["correct"] and week["kw_correct"] and week["offered_forbidden"] == [] and week["sensitive_offered"] == 0
    assert week["via"] == "keyword" and all(r["via"] in ("keyword", "model", "none") for r in sel)
    assert next(r for r in sel if r["expected"] == "abstain")["kw_pick"] == "abstain"


def test_offline_eval_refuses_small_sets(tmp_path: Path, capsys) -> None:
    root = sets_root(tmp_path)
    assert fe.main(["--fixtures", str(root), "--fake-predictor", "--min-select", "30"]) == 1
    assert "FIXTURES_INSUFFICIENT: select has 5 cases, needs 30" in capsys.readouterr().out


def test_check_default_compares_the_decision_with_the_shipped_constant(tmp_path: Path, capsys, monkeypatch) -> None:
    root = sets_root(tmp_path)
    # the router's own decision on these few cases, so this test isolates the FAST_LANE half first
    routed = [fe.route_case(f, c) for f in fe.load_sets(root)["holdout"] for c in f.cases]
    router_ok, _numbers = fe.router_ship_decision(routed)
    monkeypatch.setattr(fe, "shipped_router", lambda: router_ok)
    monkeypatch.setattr(fe, "shipped_settings", lambda: (0.2, 0.05, "compact", False))
    fe.main(["--fixtures", str(root), "--check-default", "--fake-predictor"])
    out = capsys.readouterr().out
    hold = fe.run_set(Decider(fe.make_fake_predict(), style="compact"), fe.load_sets(root)["holdout"], "compact")
    ok, _ = fe.ship_decision(hold, 0.2, 0.05)
    assert ("DEFAULT_CONSISTENT" in out) == (ok is False)
    monkeypatch.setattr(fe, "shipped_settings", lambda: (0.2, 0.05, "compact", True))
    code = fe.main(["--fixtures", str(root), "--check-default", "--fake-predictor"])
    out = capsys.readouterr().out
    assert (code == 0 and "DEFAULT_CONSISTENT" in out) == (ok is True)
    assert (code == 1 and "DEFAULT_MISMATCH" in out) == (ok is False)
    # and the router half: a shipped LANE_FIRST_DEFAULT that disagrees with its eval is a mismatch
    monkeypatch.setattr(fe, "shipped_settings", lambda: (0.2, 0.05, "compact", ok))
    monkeypatch.setattr(fe, "shipped_router", lambda: not router_ok)
    code = fe.main(["--fixtures", str(root), "--check-default", "--fake-predictor"])
    out = capsys.readouterr().out
    assert code == 1 and "DEFAULT_MISMATCH: the router eval says" in out and "DEFAULT_CONSISTENT" not in out
    monkeypatch.setattr(fe, "shipped_router", lambda: router_ok)
    assert fe.main(["--fixtures", str(root), "--check-default", "--fake-predictor"]) == 0
    assert "DEFAULT_CONSISTENT" in capsys.readouterr().out


def test_router_eval_runs_the_real_router_and_counts_wrong_and_sensitive_clicks(tmp_path: Path, capsys) -> None:
    cal = ax.snapshot_from_raw(calendar_raw(), seq=1, screen=(1440, 900))
    fx = fe.Fixture(path=Path("x"), set_name="holdout", name="x", snapshot=cal, cases=[], captured={}, screen=None,
                    raw={})
    week = ids(cal)["Week"]
    right = fe.route_case(fx, {"goal": "switch to week view", "expected_id": week, "expected": "click"})
    assert right.engaged and right.correct and right.clicked == [week] and not right.sensitive
    declined = fe.route_case(fx, {"goal": "show the next seven days", "expected_id": week, "expected": "click"})
    assert not declined.engaged and declined.reason == "no_match" and declined.clicked == []
    # a click on an abstain-expected case is an engaged WRONG case: the counter the ship bar reads
    wrong = fe.route_case(fx, {"goal": "switch to week view", "expected_id": None, "expected": "abstain"})
    assert wrong.engaged and not wrong.correct
    s = fe.router_summary([right, declined, wrong])
    assert (s["engaged"], s["right"], s["engaged_on_abstain"], s["sensitive"]) == (2, 1, 1, 0)
    assert s["precision"] == 0.5 and len(s["wrong"]) == 1 and s["declined"] == {"no_match": 1}
    ok, _ = fe.router_ship_decision([right] * 9)
    assert ok is False                                   # 9 engaged is under ROUTER_MIN_ENGAGED
    assert fe.router_ship_decision([right] * 10)[0] is True
    assert fe.router_ship_decision([right] * 17 + [wrong] * 3)[0] is False      # 85% is under the bar
    root = sets_root(tmp_path)
    assert fe.main(["--fixtures", str(root), "--router"]) == 0
    out = capsys.readouterr().out
    assert "ROUTER SHIP DECISION:" in out and "ROUTER_EVAL_COMPLETE" in out


def test_run_case_records_forbidden_offers_and_rejections() -> None:
    cal = ax.snapshot_from_raw(calendar_raw(), seq=1, screen=(1440, 900))
    fx = fe.Fixture(path=Path("x"), set_name="select", name="x", snapshot=cal, cases=[], captured={}, screen=None,
                    raw={})
    case = {"goal": "switch to week view", "expected_id": ids(cal)["Week"], "expected": "click", "overlap": True,
            "distractor": False, "must_not_offer": [ids(cal)["Today"]]}
    r = fe.run_case(Decider(fe.make_fake_predict(), style="compact"), fx, case)
    assert r.correct and r.offered_forbidden == [ids(cal)["Today"]] and r.k == 7 and r.error == ""
    assert r.via == "keyword" and r.p_top == 1.0                       # the gate decided; the model was not asked

    def broken(state, questions):
        raise RuntimeError("no metal")
    r = fe.run_case(Decider(broken, style="compact"), fx, case)
    assert r.correct and r.via == "keyword" and r.error == ""          # a gate case never touches the model
    model_case = {**case, "goal": "show the next seven days", "overlap": False}
    r = fe.run_case(Decider(broken, style="compact"), fx, model_case)
    assert r.via == "model" and r.pick == "" and not r.correct and r.escalated and "no metal" in r.error
    assert r.kw_pick == "abstain" and not r.kw_correct
    r = fe.run_case(Decider(fe.make_fake_predict(), style="compact"), fx, model_case)
    assert r.via == "model" and r.error == ""                          # the fake answers, on no tokens
    # an empty menu (every control dropped) never reaches the model: correct only for an abstain case
    empty = ax.snapshot_from_raw(raw(node("AXButton", "Delete Event", frame=(10, 10, 40, 20))), seq=1,
                                 screen=(1440, 900))
    fx2 = fe.Fixture(path=Path("y"), set_name="select", name="y", snapshot=empty, cases=[], captured={}, screen=None,
                     raw={})
    r = fe.run_case(Decider(broken, style="compact"), fx2, {**case, "expected_id": None, "expected": "abstain"})
    assert r.correct and r.pick == "" and r.escalated and r.error == "" and r.kw_pick == "abstain"
    r = fe.run_case(Decider(broken, style="compact"), fx2, {**case, "expected_id": "0"})
    assert not r.correct and r.error == "no candidates offered"


# ---- the shadow report -----------------------------------------------------------------------


def test_shadow_entries_and_confusion(tmp_path: Path, capsys) -> None:
    lines = [json.dumps({"t": 1, "verify": {"valid": True, "guidance": "", "local": {"p_true": 0.9}}}),
             json.dumps({"t": 2, "verify": {"valid": False, "local": {"p_true": 0.7}}}),
             json.dumps({"t": 3, "verify": {"valid": True, "local": {"p_true": 0.2}}}),
             json.dumps({"t": 4, "verify": {"valid": False, "local": {"p_true": 0.1}}}),
             json.dumps({"t": 5, "verify": {"valid": True, "local": {"error": "timeout"}}}),      # no p_true
             json.dumps({"t": 6, "verify": {"error": "RuntimeError", "local": {"p_true": 0.5}}}),  # no valid
             "not json"]
    entries = fe.shadow_entries(lines)
    assert len(entries) == 4
    assert fe.shadow_confusion(entries) == {"tp": 1, "fp": 1, "fn": 1, "tn": 1}
    runs = tmp_path / "runs"
    runs.mkdir()
    (runs / "a.jsonl").write_text("\n".join(lines), encoding="utf-8")
    assert fe.main(["--shadow-report", "--runs-dir", str(runs), "--min-runs", "5"]) == 1
    out = capsys.readouterr().out
    assert "n_shadow=4" in out and "tp 1 fp 1 fn 1 tn 1" in out and "SHADOW_REPORT_INCOMPLETE: n=4, need 5" in out
    assert fe.main(["--shadow-report", "--runs-dir", str(runs), "--min-runs", "4"]) == 0
    assert "SHADOW_REPORT_OK" in capsys.readouterr().out
