#!/usr/bin/env python3
"""Score the eye pickers. Two sets, two modes.

LEGACY (the default, or ``--legacy``): the 84 hand-written six-label cases (tests/fixtures/emotion: the 48 test
rows of scenarios.json and the 36 of confirmation.json). Both pickers answer the eye question the live worker
asks over every label in eye_model.LABELS; the pick is folded onto the six labels with FOLD so the sets can
score it, and each case is sent as ``User: <text>``, as on 2026-09-24. With ``--min-scenario`` and
``--min-confirm`` (percent) it prints EYES_LEGACY_OK when both sets reach their bar, and exits 1 otherwise.

BLIND (``--blind set.json``): a set of conversations labelled with eye labels, ``{"cases": [{"id", "turns":
[{"who": "owner"|"eve", "text"}], "label"}]}``. Each case's state is built from its turns exactly as the live
worker builds it (live_expressions.LiveExpressions.offer: the text squeezed and cut to 800 characters, then
eye_model.ConversationContext), and production Jev is asked once for the last turn. Prints per-label recall,
the confusion pairs, overall top-1, p50/p95 latency and errors; EYES_BAR_OK when top-1 >= ``--min-acc``,
every label with ``MIN_CASES_FOR_RECALL`` or more cases has recall >= ``--min-recall``, p50 <= ``--max-p50-ms``
and errors <= ``MAX_ERROR_RATE``; exit 1 otherwise. An error is a miss.

    cd bridge && env $(grep -E '^(EVE_JEV_[A-Z_]*|JEV_[A-Z_]*|OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) \\
        .venv/bin/python tools/jev_eyes_eval.py --legacy --min-scenario 57.5 --min-confirm 67.2
    ... tools/jev_eyes_eval.py --blind tests/fixtures/<set>.json --min-acc 65 --min-recall 55 --max-p50-ms 400

The fold is a scoring device, not a claim: the six-label sets were written for a different question, so a wrong
fold can cost either picker a point. It is the same fold for both, which is the comparison that matters.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from collections import Counter
from pathlib import Path
from typing import Any, Optional

from eve_bridge import jev, pricing
from eve_bridge.eye_model import LABELS, ConversationContext, JevEyeModel

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests/fixtures/emotion"
SIX = ("affection", "calm", "curious", "happy", "startled", "surprised")
FOLD = {                                    # every eye label → the six the hand-written sets use
    "affection": "affection", "happy": "happy", "excited": "happy", "wink": "happy",
    "calm": "calm", "skeptical": "calm", "frustrated": "calm", "sad": "calm",
    "curious": "curious", "surprised": "surprised", "worried": "startled",
    # the seven added 2026-10-12 (docs/expressions-design.json; thinking dropped); none folds to startled
    "laughing": "happy", "proud": "happy", "relieved": "happy", "flattered": "affection",
    "confused": "curious", "sleepy": "calm", "sheepish": "calm",
}
assert set(FOLD) == set(LABELS) and set(FOLD.values()) == set(SIX)
LEGACY_LABELS = frozenset(LABELS[:11])      # the labels the 2026-09-24 baseline was measured with
LEGACY_SETS = {"scenarios-test": "min_scenario", "confirmation": "min_confirm"}
MIN_CASES_FOR_RECALL = 5
MAX_ERROR_RATE = 0.02
SPEAKERS = {"owner": "user", "eve": "assistant"}


def cases() -> dict[str, list[dict]]:
    scenarios = json.loads((FIXTURES / "scenarios.json").read_text())["rows"]
    confirmation = json.loads((FIXTURES / "confirmation.json").read_text())["rows"]
    return {"scenarios-test": [r for r in scenarios if r["split"] == "test"], "confirmation": confirmation}


def macro_f1(y: list[str], pred: list[str]) -> float:
    f1s = []
    for label in SIX:
        tp = sum(1 for a, b in zip(y, pred, strict=True) if a == label and b == label)
        fp = sum(1 for a, b in zip(y, pred, strict=True) if a != label and b == label)
        fn = sum(1 for a, b in zip(y, pred, strict=True) if a == label and b != label)
        p = tp / (tp + fp) if tp + fp else 0.0
        r = tp / (tp + fn) if tp + fn else 0.0
        f1s.append(2 * p * r / (p + r) if p + r else 0.0)
    return sum(f1s) / len(f1s)


def percentile(sorted_ms: list[float], q: float) -> Optional[float]:
    """The nearest-rank-below percentile this tool has always reported (p50 = ms[n // 2])."""
    return sorted_ms[min(len(sorted_ms) - 1, int(len(sorted_ms) * q))] if sorted_ms else None


def score(name: str, model, sets: dict[str, list[dict]]) -> dict:
    out: dict = {"picker": name, "sets": {}}
    for set_name, rows in sets.items():
        y, pred, ms, errors, picks = [], [], [], 0, []
        for row in rows:
            try:
                answer = model.predict(f"User: {row['text']}")
            except Exception as e:  # noqa: BLE001 — an error is a miss, and is counted
                errors += 1
                picks.append({"id": row["id"], "label": row["label"], "eye": "", "error": type(e).__name__})
                y.append(row["label"])
                pred.append("")
                continue
            ms.append(answer["ms"])
            y.append(row["label"])
            pred.append(FOLD[answer["label"]])
            picks.append({"id": row["id"], "label": row["label"], "eye": answer["label"], "folded": pred[-1],
                          "text": row["text"]})
        ms.sort()
        out["sets"][set_name] = {
            "n": len(rows), "accuracy": sum(1 for a, b in zip(y, pred, strict=True) if a == b) / len(rows),
            "macro_f1": macro_f1(y, pred), "errors": errors,
            "ms_p50": percentile(ms, 0.5), "ms_p95": percentile(ms, 0.95),
            "picks": picks,
        }
    return out


def legacy_verdict(result: dict, min_scenario: float, min_confirm: float) -> tuple[bool, list[str]]:
    """Both sets at or above their bar (percent). Returns (ok, the reasons it is not)."""
    bars = {"min_scenario": min_scenario, "min_confirm": min_confirm}
    reasons = []
    for set_name, bar_name in LEGACY_SETS.items():
        acc = result["sets"][set_name]["accuracy"] * 100
        if acc < bars[bar_name]:
            reasons.append(f"{set_name} {acc:.1f}% < {bars[bar_name]:g}%")
    return not reasons, reasons


def blind_state(turns: list[dict]) -> str:
    """The state the live worker would hand the picker for the last turn of ``turns``."""
    context = ConversationContext()
    state = ""
    for i, turn in enumerate(turns):
        text = " ".join(str(turn["text"]).split())[-800:]           # LiveExpressions.offer's squeeze
        state = context.state(SPEAKERS[turn["who"]], text, float(i))
    return state


def load_blind(path: Path) -> list[dict]:
    data = json.loads(path.read_text())
    rows = data.get("cases") if isinstance(data, dict) else None
    if not isinstance(rows, list) or not rows:
        raise SystemExit(f"{path}: no cases")
    for row in rows:
        turns = row.get("turns")
        if (row.get("label") not in LABELS or not isinstance(turns, list) or not turns
                or any(t.get("who") not in SPEAKERS or not isinstance(t.get("text"), str) for t in turns)):
            raise SystemExit(f"{path}: case {row.get('id')!r} is malformed")
    return rows


def score_blind(model, rows: list[dict]) -> dict:
    gold: list[str] = []
    pred: list[str] = []
    ms: list[float] = []
    picks: list[dict] = []
    errors = 0
    for row in rows:
        gold.append(row["label"])
        try:
            answer = model.predict(blind_state(row["turns"]))
            label = answer["label"]
            if label not in LABELS:
                raise ValueError("label outside LABELS")
        except Exception as e:  # noqa: BLE001 — an error is a miss, and is counted
            errors += 1
            pred.append("")
            picks.append({"id": row["id"], "label": row["label"], "eye": "", "error": type(e).__name__})
            continue
        ms.append(float(answer["ms"]))
        pred.append(label)
        picks.append({"id": row["id"], "label": row["label"], "eye": label})
    ms.sort()
    support = Counter(gold)
    hits = Counter(a for a, b in zip(gold, pred, strict=True) if a == b)
    confusion = Counter((a, b or "(error)") for a, b in zip(gold, pred, strict=True) if a != b)
    n = len(rows)
    return {
        "n": n,
        "accuracy": sum(hits.values()) / n if n else 0.0,
        "recall": {label: {"n": support[label], "hits": hits[label], "recall": hits[label] / support[label]}
                   for label in LABELS if support[label]},
        "confusion": [{"gold": a, "eye": b, "count": c} for (a, b), c in confusion.most_common()],
        "errors": errors, "error_rate": errors / n if n else 0.0,
        "ms_p50": percentile(ms, 0.5), "ms_p95": percentile(ms, 0.95),
        "picks": picks,
    }


def blind_verdict(report: dict, min_acc: float, min_recall: float, max_p50_ms: float) -> tuple[bool, list[str]]:
    """The pre-registered bar (docs/more-expressions-gates.md X3). ``min_acc``/``min_recall`` are percent."""
    reasons = []
    if report["accuracy"] * 100 < min_acc:
        reasons.append(f"top-1 {report['accuracy'] * 100:.1f}% < {min_acc:g}%")
    for label, r in report["recall"].items():
        if r["n"] >= MIN_CASES_FOR_RECALL and r["recall"] * 100 < min_recall:
            reasons.append(f"{label} recall {r['recall'] * 100:.1f}% < {min_recall:g}% (n={r['n']})")
    if report["ms_p50"] is None or report["ms_p50"] > max_p50_ms:
        reasons.append("no successful call, so no p50" if report["ms_p50"] is None
                       else f"p50 {report['ms_p50']:.0f} ms > {max_p50_ms:g} ms")
    if report["error_rate"] > MAX_ERROR_RATE:
        reasons.append(f"errors {report['errors']}/{report['n']} > {MAX_ERROR_RATE:.0%}")
    return not reasons, reasons


def _ms(x: Optional[float]) -> str:
    return "-" if x is None else f"{round(x)}ms"


def run_legacy(args: argparse.Namespace) -> int:
    sets = cases()
    if sum(len(v) for v in sets.values()) != 84:
        raise SystemExit(f"expected 84 cases, found {sum(len(v) for v in sets.values())}")
    t0 = time.perf_counter()
    results = [score("jev", JevEyeModel(), sets)]
    results[0]["cost_usd"] = round(pricing.estimate_jev_cost(jev.METER.input_tokens), 5)
    results[0]["calls"] = jev.METER.calls
    if args.laya:
        from eve_bridge.eye_model import LiveEyeModel
        from eve_bridge.live_expressions import DEFAULT_MODEL

        path = Path(os.environ.get("EVE_EXPRESSION_MODEL", str(DEFAULT_MODEL))).expanduser()
        results.append(score("laya", LiveEyeModel(path), sets))
    report: dict[str, Any] = {"fold": FOLD, "elapsed_s": round(time.perf_counter() - t0, 1), "results": results}
    for r in results:
        for set_name, s in r["sets"].items():
            print(f"{r['picker']:5} {set_name:15} n={s['n']:2} acc={s['accuracy']:.3f} macroF1={s['macro_f1']:.3f} "
                  f"errors={s['errors']} p50={_ms(s['ms_p50'])} p95={_ms(s['ms_p95'])}")
        if "cost_usd" in r:
            print(f"{r['picker']:5} calls={r['calls']} cost=${r['cost_usd']}")
    # Drift onto the labels the baseline never had: the cases a reviewer checks by name.
    for set_name, s in results[0]["sets"].items():
        for p in s["picks"]:
            if p.get("eye") and p["eye"] not in LEGACY_LABELS:
                mark = "ok" if p["folded"] == p["label"] else "MISS"
                print(f"new-label {mark:4} {set_name:15} {p['id']:28} {p['label']:9} -> {p['eye']:9} {p['text']}")
    code = 0
    if args.min_scenario is not None or args.min_confirm is not None:
        if args.min_scenario is None or args.min_confirm is None:
            raise SystemExit("--min-scenario and --min-confirm go together")
        ok, reasons = legacy_verdict(results[0], args.min_scenario, args.min_confirm)
        report["verdict"] = {"ok": ok, "reasons": reasons}
        print("EYES_LEGACY_OK" if ok else "EYES_LEGACY_FAIL: " + "; ".join(reasons))
        code = 0 if ok else 1
    if args.out:
        args.out.write_text(json.dumps(report, indent=1) + "\n")
        print("wrote", args.out)
    return code


def run_blind(args: argparse.Namespace) -> int:
    if args.min_acc is None or args.min_recall is None or args.max_p50_ms is None:
        raise SystemExit("--blind needs --min-acc, --min-recall and --max-p50-ms")
    rows = load_blind(args.blind)
    t0 = time.perf_counter()
    report = score_blind(JevEyeModel(), rows)
    report["elapsed_s"] = round(time.perf_counter() - t0, 1)
    report["cost_usd"] = round(pricing.estimate_jev_cost(jev.METER.input_tokens), 5)
    print(f"blind {args.blind.name}: n={report['n']} top1={report['accuracy']:.3f} errors={report['errors']} "
          f"p50={_ms(report['ms_p50'])} p95={_ms(report['ms_p95'])} cost=${report['cost_usd']}")
    for label, r in report["recall"].items():
        print(f"  recall {label:10} {r['hits']:3}/{r['n']:<3} {r['recall']:.3f}")
    for c in report["confusion"]:
        print(f"  confused {c['gold']:10} -> {c['eye']:10} x{c['count']}")
    ok, reasons = blind_verdict(report, args.min_acc, args.min_recall, args.max_p50_ms)
    report["verdict"] = {"ok": ok, "reasons": reasons}
    if args.out:
        args.out.write_text(json.dumps(report, indent=1) + "\n")
        print("wrote", args.out)
    print("EYES_BAR_OK" if ok else "EYES_BAR_FAIL: " + "; ".join(reasons))
    return 0 if ok else 1


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--legacy", action="store_true", help="the 84 folded cases (the default)")
    mode.add_argument("--blind", type=Path, default=None, help="a blind set of eye-labelled conversations")
    parser.add_argument("--laya", action="store_true", help="legacy: also score the local Laya picker")
    parser.add_argument("--min-scenario", type=float, default=None, help="legacy bar, percent")
    parser.add_argument("--min-confirm", type=float, default=None, help="legacy bar, percent")
    parser.add_argument("--min-acc", type=float, default=None, help="blind bar: top-1, percent")
    parser.add_argument("--min-recall", type=float, default=None, help="blind bar: per-label recall, percent")
    parser.add_argument("--max-p50-ms", type=float, default=None, help="blind bar: p50 latency")
    parser.add_argument("--out", type=Path, default=None)
    args = parser.parse_args(argv)
    return run_blind(args) if args.blind is not None else run_legacy(args)


if __name__ == "__main__":
    sys.exit(main())
