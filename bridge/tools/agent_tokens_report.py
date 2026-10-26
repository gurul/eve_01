#!/usr/bin/env python3
"""Does a Mac-agent turn get slower as its input grows? The numbers that decide whether to drop old screenshots.

    .venv/bin/python tools/agent_tokens_report.py                     # ~/.config/eve-bridge/agent-runs
    .venv/bin/python tools/agent_tokens_report.py --runs DIR --json   # rows for a script

Every run log (computer_agent.py, one JSONL file per task) starts with a `{"goal", "model"}` line and writes
one `{"turn", "effort", "api_secs", "tokens": {"in", "out", "reasoning"}}` line per planner call. This groups
the runs by model and prints, per turn index: how many calls, the median input tokens and the median
api seconds; then the Pearson correlation and the least-squares slope of api_secs against input tokens over
every call, and the share of runs that reached turn 8 or later.

The decision rule is the plan's (docs/stackchan/routing.md, "Old screenshots"), fixed here before a result:
`prune_images` is worth building only if latency rises with input for the default model, which this tool
reads as at least MIN_CALLS calls, r >= MIN_R and a positive slope. Fewer calls is "not measured", never
"build". It prints counts, model names, token numbers and seconds only; no goal or other text from a log.
"""

from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
from pathlib import Path
from typing import Any, Optional

DEFAULT_RUNS = Path("~/.config/eve-bridge/agent-runs")
DEFAULT_MODEL = "gpt-6.1-terra"     # computer_agent.DEFAULT_MODEL; kept literal so the tool runs without the package
MIN_CALLS = 30                      # fewer planner calls than this cannot decide
MIN_R = 0.3                         # the correlation that counts as "latency rises with input"
LATE_TURN = 8


def read_runs(runs: Path) -> list[dict[str, Any]]:
    """One row per run log: {"model", "calls": [(turn, in_tokens, api_secs)]}. Malformed lines are skipped."""
    out: list[dict[str, Any]] = []
    for path in sorted(runs.glob("*.jsonl")):
        model: Optional[str] = None
        calls: list[tuple[int, int, float]] = []
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            try:
                entry = json.loads(line)
            except ValueError:
                continue
            if not isinstance(entry, dict):
                continue
            if model is None and isinstance(entry.get("model"), str):
                model = entry["model"]
            secs, turn, tokens = entry.get("api_secs"), entry.get("turn"), entry.get("tokens")
            if not isinstance(tokens, dict) or isinstance(turn, bool) or not isinstance(turn, int):
                continue
            tin = tokens.get("in")
            if isinstance(secs, (int, float)) and isinstance(tin, int) and not isinstance(tin, bool):
                calls.append((turn, tin, float(secs)))
        out.append({"model": model or "unknown", "calls": calls})
    return out


def fit(xs: list[float], ys: list[float]) -> dict[str, Optional[float]]:
    """Pearson r and the least-squares slope of ys on xs; None when either side does not vary."""
    n = len(xs)
    if n < 2:
        return {"r": None, "slope": None}
    mx, my = sum(xs) / n, sum(ys) / n
    sxx = sum((x - mx) ** 2 for x in xs)
    syy = sum((y - my) ** 2 for y in ys)
    sxy = sum((x - mx) * (y - my) for x, y in zip(xs, ys, strict=True))
    if sxx == 0:
        return {"r": None, "slope": None}
    slope = sxy / sxx
    r = sxy / math.sqrt(sxx * syy) if syy > 0 else None
    return {"r": r, "slope": slope}


def summarize(rows: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    """Per model: runs, runs with a planner call, calls, per-turn medians, the fit and the late-turn share."""
    by_model: dict[str, list[dict[str, Any]]] = {}
    for row in rows:
        by_model.setdefault(row["model"], []).append(row)
    report: dict[str, dict[str, Any]] = {}
    for model, model_rows in sorted(by_model.items()):
        calls = [c for row in model_rows for c in row["calls"]]
        with_calls = [row for row in model_rows if row["calls"]]
        turns: dict[int, list[tuple[int, float]]] = {}
        for turn, tin, secs in calls:
            turns.setdefault(turn, []).append((tin, secs))
        per_turn = [{"turn": t, "calls": len(v), "median_in": statistics.median(x for x, _ in v),
                     "median_secs": statistics.median(s for _, s in v)} for t, v in sorted(turns.items())]
        f = fit([float(tin) for _, tin, _ in calls], [secs for _, _, secs in calls])
        late = sum(1 for row in with_calls if max(t for t, _, _ in row["calls"]) >= LATE_TURN)
        report[model] = {
            "runs": len(model_rows), "runs_with_calls": len(with_calls), "calls": len(calls),
            "per_turn": per_turn, "r": f["r"],
            "secs_per_1k_in": None if f["slope"] is None else f["slope"] * 1000,
            "max_in": max((tin for _, tin, _ in calls), default=None),
            "late_share": (late / len(with_calls)) if with_calls else None, "late_runs": late,
        }
    return report


def decide(report: dict[str, dict[str, Any]], model: str = DEFAULT_MODEL) -> tuple[str, str]:
    """("build" | "skip" | "not measured", why), by the rule in the module docstring."""
    m = report.get(model)
    if not m or m["calls"] < MIN_CALLS:
        n = m["calls"] if m else 0
        return "not measured", f"{n} {model} planner calls, fewer than {MIN_CALLS}"
    r, slope = m["r"], m["secs_per_1k_in"]
    if r is not None and slope is not None and r >= MIN_R and slope > 0:
        return "build", f"r = {r:.2f} >= {MIN_R} and slope {slope:+.3f} s per 1k input tokens"
    rs = "n/a" if r is None else f"{r:.2f}"
    ss = "n/a" if slope is None else f"{slope:+.3f}"
    return "skip", f"r = {rs} (< {MIN_R} or slope {ss} s per 1k not positive)"


def _fmt(v: Optional[float], spec: str) -> str:
    return "n/a" if v is None else format(v, spec)


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--runs", type=Path, default=DEFAULT_RUNS, help="the agent-runs directory")
    p.add_argument("--model", default=DEFAULT_MODEL, help="the model the decision is about")
    p.add_argument("--json", action="store_true", help="print the report and the decision as JSON")
    args = p.parse_args(argv)
    runs = args.runs.expanduser()
    if not runs.is_dir():
        print(f"no runs directory at {runs}", file=sys.stderr)
        return 1
    report = summarize(read_runs(runs))
    verdict, why = decide(report, args.model)
    if args.json:
        print(json.dumps({"models": report, "decision": {"model": args.model, "verdict": verdict, "why": why}},
                         indent=1))
    else:
        for model, m in report.items():
            mark = "  (decides)" if model == args.model else ""
            print(f"{model}{mark}: {m['runs']} runs, {m['runs_with_calls']} with planner calls, {m['calls']} calls,"
                  f" max input {m['max_in']}")
            for t in m["per_turn"]:
                print(f"  turn {t['turn']:>2}: {t['calls']:>4} calls  median in {t['median_in']:>8.0f}"
                      f"  median {t['median_secs']:6.2f} s")
            print(f"  api_secs vs input: r = {_fmt(m['r'], '.2f')}, slope {_fmt(m['secs_per_1k_in'], '+.3f')} s"
                  f" per 1k tokens; runs reaching turn {LATE_TURN}+: {m['late_runs']}"
                  f" ({_fmt(None if m['late_share'] is None else m['late_share'] * 100, '.1f')} %)")
        if args.model not in report:
            print(f"{args.model}: 0 runs")
        print(f"\nprune_images for {args.model}: {verdict} ({why})")
    print("AGENT_TOKENS_REPORT_OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
