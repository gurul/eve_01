#!/usr/bin/env python3
"""How well does browser_intent.expected_outcome name what a request asks for? Scored with the REAL local Laya.

    .venv/bin/python tools/browser_intent_eval.py [--holdout PATH] [--out PATH] [--label NAME] [--note TEXT]

--label replaces the holdout path in the report's "holdout" key (for example "v2"); the path is kept in
"holdout_path". --note is stored as "note", for example to mark a re-run on a set the builders have since seen.

Each line of the holdout is {"id", "request", "kind", "site_host", "query", "n"}, labelled by hand by someone who
did not read browser_intent.py. Nothing here is stubbed: the shared LayaKindEngine loads the pinned cklxx/laya-browser
v10s checkpoint through laya-mlx on this Mac, and every request goes through expected_outcome exactly as routing
calls it. The checkpoint must already be cached; this tool never downloads it.

Per case it records:
- kind correct       the outcome's kind equals the label's (an abstention is not correct)
- full correct       kind, host, query and n all equal the label's (query compared case- and space-folded)
- abstain            expected_outcome returned None
- WRONG              it returned an outcome and that outcome is not fully correct. This is the costly case: the
                     success check built from it would fail a correct plan or pass a wrong one.

Summary: coverage = answered / n, precision = full correct / answered, accuracy = full correct / n. Latency is
the wall time of expected_outcome per case (p50 / p95), and separately the Laya call alone; warm time is the
checkpoint load plus one throwaway question that is not in the holdout. RAM is the process's resident set before
and after warm-up (ps), plus MLX's own active and peak memory.

eve's bar for a delegate that may veto plans: precision on answered cases >= 80% and wrong <= 5% of n.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Optional

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from eve_bridge import browser_intent  # noqa: E402

HOLDOUT = ROOT / "tests" / "fixtures" / "browser_intent" / "holdout.jsonl"
OUT = ROOT.parent / "docs" / "browser-intent-eval.json"
BAR_PRECISION, BAR_WRONG_SHARE = 0.80, 0.05
WARM_REQUEST = "search for release notes"  # not a holdout request; reaches Laya (a literal query is read)


def rss_mb() -> Optional[float]:
    try:
        out = subprocess.run(["ps", "-o", "rss=", "-p", str(os.getpid())], capture_output=True, text=True,
                             timeout=5).stdout.strip()
        return round(int(out) / 1024, 1)
    except Exception:  # noqa: BLE001 — no ps: report None, never a guess
        return None


def mlx_mb() -> dict[str, Optional[float]]:
    try:
        import mlx.core as mx

        return {"active_mb": round(mx.get_active_memory() / 2**20, 1), "peak_mb": round(mx.get_peak_memory() / 2**20, 1)}
    except Exception:  # noqa: BLE001
        return {"active_mb": None, "peak_mb": None}


def fold(query: Any) -> Optional[str]:
    if query is None:
        return None
    return " ".join(re.sub(r"[^\w\s]", " ", str(query).casefold()).split()) or None


def percentile(values: list[float], p: float) -> Optional[float]:
    if not values:
        return None
    ordered = sorted(values)
    k = (len(ordered) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(ordered) - 1)
    return round(ordered[lo] + (ordered[hi] - ordered[lo]) * (k - lo), 2)


def score(case: dict, outcome: Optional[browser_intent.RequestOutcome]) -> dict:
    row: dict[str, Any] = {"id": case["id"], "request": case["request"],
                           "expected": {k: case.get(k) for k in ("kind", "site_host", "query", "n")}}
    if outcome is None:
        row.update(got=None, kind_correct=False, full_correct=False, abstain=True, wrong=False, fields_wrong=[])
        return row
    got = {"kind": outcome.kind, "site_host": outcome.site_host, "query": outcome.query, "n": outcome.n}
    fields_wrong = [k for k in ("kind", "site_host", "query", "n")
                    if (fold(got[k]) != fold(case.get(k)) if k == "query" else got[k] != case.get(k))]
    row.update(got={**got, "confidence": outcome.confidence, "source": outcome.source},
               kind_correct=got["kind"] == case["kind"], full_correct=not fields_wrong, abstain=False,
               wrong=bool(fields_wrong), fields_wrong=fields_wrong)
    return row


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--holdout", type=Path, default=HOLDOUT)
    parser.add_argument("--out", type=Path, default=OUT)
    parser.add_argument("--label", default=None, help='short name for the holdout, stored as "holdout"')
    parser.add_argument("--note", default=None, help='free text stored as "note"')
    args = parser.parse_args(argv)

    cases = [json.loads(line) for line in args.holdout.read_text().splitlines() if line.strip()]
    environ = {"EVE_BROWSER_INTENT": "laya"}
    engine = browser_intent.shared_engine()

    rss_before = rss_mb()
    started = time.perf_counter()
    try:
        engine.warm()
    except browser_intent.Abstain as exc:
        print(f"Laya did not load: {exc}", file=sys.stderr)
        return 2
    load_ms = (time.perf_counter() - started) * 1000
    warm = browser_intent.expected_outcome(WARM_REQUEST, environ=environ)
    if warm is None or warm.source != "laya":
        print(f"warm-up request did not reach Laya: {warm}", file=sys.stderr)
        return 2
    warm_ms = (time.perf_counter() - started) * 1000
    rss_after_warm = rss_mb()
    laya_seen = len(browser_intent.recent_latencies())

    rows, wall = [], []
    for case in cases:
        t0 = time.perf_counter()
        outcome = browser_intent.expected_outcome(case["request"], environ=environ)
        wall.append((time.perf_counter() - t0) * 1000)
        rows.append({**score(case, outcome), "ms": round(wall[-1], 2)})
    laya_ms = browser_intent.recent_latencies()[laya_seen:]
    rss_end = rss_mb()

    n = len(rows)
    answered = [r for r in rows if not r["abstain"]]
    full = sum(r["full_correct"] for r in rows)
    wrong = sum(r["wrong"] for r in rows)
    per_kind = {}
    for kind in browser_intent.KINDS:
        group = [r for r in rows if r["expected"]["kind"] == kind]
        ans = [r for r in group if not r["abstain"]]
        per_kind[kind] = {
            "n": len(group), "answered": len(ans), "kind_correct": sum(r["kind_correct"] for r in group),
            "full_correct": sum(r["full_correct"] for r in group), "abstain": len(group) - len(ans),
            "wrong": sum(r["wrong"] for r in group),
        }
    precision = round(full / len(answered), 4) if answered else None
    holdout_path = (str(args.holdout.resolve().relative_to(ROOT.parent)) if args.holdout.resolve().is_relative_to(ROOT.parent)
                    else str(args.holdout))
    report = {
        "n": n,
        "accuracy": round(full / n, 4) if n else None,
        "precision": precision,
        "coverage": round(len(answered) / n, 4) if n else None,
        "wrong": wrong,
        "wrong_share": round(wrong / n, 4) if n else None,
        "kind_accuracy": round(sum(r["kind_correct"] for r in rows) / n, 4) if n else None,
        "abstain": n - len(answered),
        # A "none" with no site, query or number is code's answer, not Laya's: the split shows which half erred.
        "wrong_by_source": {src: sum(r["wrong"] and r["got"]["source"] == src for r in answered)
                            for src in ("laya", "code")},
        "p50_ms": percentile(wall, 0.50),
        "p95_ms": percentile(wall, 0.95),
        "laya_calls": len(laya_ms),
        "laya_p50_ms": percentile(laya_ms, 0.50),
        "laya_p95_ms": percentile(laya_ms, 0.95),
        "load_ms": round(load_ms, 1),
        "warm_ms": round(warm_ms, 1),
        "rss_mb": {"before_warm": rss_before, "after_warm": rss_after_warm, "after_run": rss_end,
                   "delta_warm": (round(rss_after_warm - rss_before, 1)
                                  if rss_before is not None and rss_after_warm is not None else None)},
        "mlx": mlx_mb(),
        "per_kind": per_kind,
        "bar": {"precision_min": BAR_PRECISION, "wrong_share_max": BAR_WRONG_SHARE,
                "clears": bool(precision is not None and precision >= BAR_PRECISION
                               and n and wrong / n <= BAR_WRONG_SHARE)},
        "model": f"{browser_intent.BROWSER_MODEL}@{browser_intent.BROWSER_REVISION[:7]}/{browser_intent.BROWSER_SUBFOLDER}",
        "holdout": args.label or holdout_path,
        "holdout_path": holdout_path,
        **({"note": args.note} if args.note else {}),
        "measured_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "misses": [{k: r[k] for k in ("id", "request", "expected", "got", "abstain", "wrong", "fields_wrong")}
                   for r in rows if not r["full_correct"]],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({k: v for k, v in report.items() if k != "misses"}, indent=2))
    print(f"misses: {len(report['misses'])}  ->  {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
