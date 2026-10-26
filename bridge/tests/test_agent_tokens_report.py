"""tools/agent_tokens_report.py on synthetic run logs: per-turn medians, the fit, the late-turn share, the rule."""

from __future__ import annotations

import json
from pathlib import Path

import agent_tokens_report as rep  # tools/ is on the pytest path (pyproject.toml)


def _run(path: Path, model: str, calls: list[tuple[int, int, float]], extra: str = "") -> None:
    lines = [json.dumps({"t": 0, "goal": "a goal that must never be printed", "model": model}),
             json.dumps({"turn": 0, "context": "screen text"})]
    for turn, tin, secs in calls:
        lines.append(json.dumps({"turn": turn, "effort": "low", "api_secs": secs,
                                 "tokens": {"in": tin, "out": 50, "reasoning": 10}}))
        lines.append(json.dumps({"turn": turn, "exec": "print(1)"}))
    path.write_text("\n".join(lines) + "\n" + extra, encoding="utf-8")


def test_report_groups_by_model_and_reads_each_turn(tmp_path: Path) -> None:
    _run(tmp_path / "2026-10-26-100000.jsonl", "gpt-6.1-terra", [(1, 1000, 2.0), (2, 2000, 4.0)])
    _run(tmp_path / "2026-10-26-110000.jsonl", "gpt-6.1-terra",
         [(t, 1000 * t, 2.0 * t) for t in range(1, 10)], extra="not json\n")
    _run(tmp_path / "2026-10-26-120000.jsonl", "gpt-6-astra", [(1, 5000, 9.0)])
    _run(tmp_path / "2026-10-26-130000.jsonl", "gpt-6.1-terra", [])          # handed on before any planner call
    report = rep.summarize(rep.read_runs(tmp_path))
    terra = report["gpt-6.1-terra"]
    assert terra["runs"] == 3 and terra["runs_with_calls"] == 2 and terra["calls"] == 11
    t1, t2 = terra["per_turn"][0], terra["per_turn"][1]
    assert (t1["turn"], t1["calls"], t1["median_in"], t1["median_secs"]) == (1, 2, 1000, 2.0)
    assert (t2["calls"], t2["median_in"], t2["median_secs"]) == (2, 2000, 4.0)
    assert terra["late_runs"] == 1 and terra["late_share"] == 0.5 and terra["max_in"] == 9000
    assert terra["r"] is not None and terra["r"] > 0.999 and abs(terra["secs_per_1k_in"] - 2.0) < 1e-9
    assert report["gpt-6-astra"]["calls"] == 1 and report["gpt-6-astra"]["r"] is None


def test_decision_follows_the_rule(tmp_path: Path) -> None:
    # too few calls: never "build"
    assert rep.decide({}, "gpt-6.1-terra")[0] == "not measured"
    rising = [(t, 1000 * t, 1.0 + 0.5 * t) for t in range(1, 11)]
    for i in range(3):
        _run(tmp_path / f"2026-10-26-10000{i}.jsonl", "gpt-6.1-terra", rising)
    assert rep.decide(rep.summarize(rep.read_runs(tmp_path)))[0] == "build"
    flat = tmp_path / "flat"
    flat.mkdir()
    for i in range(3):
        _run(flat / f"2026-10-26-10000{i}.jsonl", "gpt-6.1-terra",
             [(t, 1000 * t, 3.0 + (0.4 if t % 2 else -0.4)) for t in range(1, 11)])
    assert rep.decide(rep.summarize(rep.read_runs(flat)))[0] == "skip"


def test_main_prints_numbers_only(tmp_path: Path, capsys) -> None:
    _run(tmp_path / "2026-10-26-100000.jsonl", "gpt-6.1-terra", [(1, 1000, 2.0)])
    assert rep.main(["--runs", str(tmp_path)]) == 0
    out = capsys.readouterr().out
    assert "AGENT_TOKENS_REPORT_OK" in out and "not measured" in out
    assert "goal" not in out and "screen text" not in out and "print(1)" not in out
    assert rep.main(["--runs", str(tmp_path), "--json"]) == 0
    assert json.loads(capsys.readouterr().out.rsplit("AGENT_TOKENS_REPORT_OK", 1)[0])["decision"]["verdict"] \
        == "not measured"
    assert rep.main(["--runs", str(tmp_path / "missing")]) == 1
