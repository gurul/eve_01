#!/usr/bin/env python3
"""The bill per computer task, summed: what each run cost and what they cost together.

    .venv/bin/python tools/bill_report.py                       # ~/.config/eve-bridge/agent-runs
    .venv/bin/python tools/bill_report.py --runs DIR --days 7   # one week
    .venv/bin/python tools/bill_report.py --json                # rows for a script

Every run log (computer_agent.py, one JSONL file per task) ends with a `bill` line since 2026-09-21: the
model's tokens in / cached / out and USD at the grounded rates (pricing.py), the Jev calls, input tokens
and USD made during the run, the wall seconds. This prints one row per run and a total, so the Jev
Engineering article's rule holds: "Track the bill per completed task."

A directory with no bill lines is reported as such, never as zero dollars: an older log has tokens per turn
and no bill, and a run with a model the table does not know has `usd: null` and is counted in `unpriced`.
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Optional

DEFAULT_RUNS = Path("~/.config/eve-bridge/agent-runs")


def read_bills(runs: Path, *, since: Optional[datetime] = None) -> tuple[list[dict[str, Any]], int]:
    """(rows, logs_without_a_bill). A row is the bill plus `file` and `goal`; malformed lines are skipped."""
    rows: list[dict[str, Any]] = []
    without = 0
    for path in sorted(runs.glob("*.jsonl")):
        if since is not None:
            try:
                stamp = datetime.strptime(path.stem[:17], "%Y-%m-%d-%H%M%S")
            except ValueError:
                stamp = datetime.fromtimestamp(path.stat().st_mtime)
            if stamp < since:
                continue
        goal, bill = "", None
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            try:
                entry = json.loads(line)
            except ValueError:
                continue
            if not isinstance(entry, dict):
                continue
            if "goal" in entry and isinstance(entry["goal"], str):
                goal = entry["goal"]
            if isinstance(entry.get("bill"), dict):
                bill = entry["bill"]
        if bill is None:
            without += 1
            continue
        rows.append({"file": path.name, "goal": goal, **bill})
    return rows, without


def summarize(rows: list[dict[str, Any]]) -> dict[str, Any]:
    total = {"runs": len(rows), "usd": 0.0, "unpriced": 0, "model_in": 0, "model_cached": 0, "model_out": 0,
             "jev_calls": 0, "jev_tokens": 0, "jev_usd": 0.0, "secs": 0.0}
    for r in rows:
        tokens = r.get("tokens") or {}
        total["model_in"] += int(tokens.get("in") or 0)
        total["model_cached"] += int(tokens.get("cached") or 0)
        total["model_out"] += int(tokens.get("out") or 0)
        jev = r.get("jev") or {}
        total["jev_calls"] += int(jev.get("calls") or 0)
        total["jev_tokens"] += int(jev.get("input_tokens") or 0)
        total["jev_usd"] += float(jev.get("usd") or 0.0)
        total["secs"] += float(r.get("secs") or 0.0)
        if r.get("total_usd") is None:
            total["unpriced"] += 1
        else:
            total["usd"] += float(r["total_usd"])
    total["usd"] = round(total["usd"], 4)
    total["jev_usd"] = round(total["jev_usd"], 6)
    total["secs"] = round(total["secs"], 1)
    return total


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--runs", type=Path, default=DEFAULT_RUNS, help="the agent-runs directory")
    p.add_argument("--days", type=float, default=0.0, help="only runs from the last N days (0: all)")
    p.add_argument("--json", action="store_true", help="print the rows and the total as JSON")
    args = p.parse_args(argv)
    runs = args.runs.expanduser()
    if not runs.is_dir():
        print(f"no runs directory at {runs}", file=sys.stderr)
        return 1
    since = datetime.now() - timedelta(days=args.days) if args.days > 0 else None
    rows, without = read_bills(runs, since=since)
    total = summarize(rows)
    if args.json:
        print(json.dumps({"rows": rows, "total": total, "logs_without_a_bill": without}, indent=1))
    else:
        if not rows:
            print(f"no bill lines in {runs}" + (f" ({without} older logs without one)" if without else ""))
            return 0
        for r in rows:
            usd = "   n/a  " if r.get("total_usd") is None else f"${r['total_usd']:7.4f}"
            tokens = r.get("tokens") or {}
            jev = r.get("jev") or {}
            print(f"{r['file'][:17]}  {usd}  {r.get('model', '?')[:14]:14}  in {tokens.get('in', 0):>7} "
                  f"(cached {tokens.get('cached', 0):>6})  out {tokens.get('out', 0):>6}  "
                  f"jev {jev.get('calls', 0):>3} calls  {r.get('secs', 0):>6.1f} s  {r.get('goal', '')[:40]}")
        print(f"\n{total['runs']} runs: ${total['usd']:.4f}" + (f" ({total['unpriced']} unpriced)" if total["unpriced"] else "")
              + f"; model in {total['model_in']} (cached {total['model_cached']}) out {total['model_out']}; "
              f"jev {total['jev_calls']} calls, {total['jev_tokens']} tokens, ${total['jev_usd']:.6f}; {total['secs']:.0f} s"
              + (f"; {without} older logs without a bill" if without else ""))
    print("BILL_REPORT_OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
