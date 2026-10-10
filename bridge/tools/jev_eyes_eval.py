#!/usr/bin/env python3
"""Score the eye pickers on the 84 hand-written six-label cases (tests/fixtures/emotion: the 48 test rows of
scenarios.json and the 36 of confirmation.json). Both pickers answer the ELEVEN-label eye question the live
worker asks (eye_model.LABELS); the pick is folded onto the six labels with FOLD so the sets can score it.

    cd bridge && set -a && . ~/.config/eve-bridge/env && set +a
    PYTHONPATH=src .venv/bin/python tools/jev_eyes_eval.py [--laya] [--out report.json]

Writes accuracy and macro-F1 per set and per picker, latency, and Jev's cost. The fold is a scoring device,
not a claim: the six-label sets were written for a different question, so a wrong fold can cost either
picker a point. It is the same fold for both, which is the comparison that matters.
"""

from __future__ import annotations

import argparse
import json
import os
import time
from pathlib import Path

from eve_bridge import jev, pricing
from eve_bridge.eye_model import LABELS, JevEyeModel

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests/fixtures/emotion"
SIX = ("affection", "calm", "curious", "happy", "startled", "surprised")
FOLD = {                                    # eleven eye labels → the six the hand-written sets use
    "affection": "affection", "happy": "happy", "excited": "happy", "wink": "happy",
    "calm": "calm", "skeptical": "calm", "frustrated": "calm", "sad": "calm",
    "curious": "curious", "surprised": "surprised", "worried": "startled",
}
assert set(FOLD) == set(LABELS) and set(FOLD.values()) == set(SIX)


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
            picks.append({"id": row["id"], "label": row["label"], "eye": answer["label"], "folded": pred[-1]})
        ms.sort()
        out["sets"][set_name] = {
            "n": len(rows), "accuracy": sum(1 for a, b in zip(y, pred, strict=True) if a == b) / len(rows),
            "macro_f1": macro_f1(y, pred), "errors": errors,
            "ms_p50": ms[len(ms) // 2] if ms else None, "ms_p95": ms[int(len(ms) * 0.95)] if ms else None,
            "picks": picks,
        }
    return out


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--laya", action="store_true", help="also score the local Laya picker (needs laya_mlx)")
    parser.add_argument("--out", type=Path, default=None)
    args = parser.parse_args()
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
    report = {"fold": FOLD, "elapsed_s": round(time.perf_counter() - t0, 1), "results": results}
    for r in results:
        for set_name, s in r["sets"].items():
            print(f"{r['picker']:5} {set_name:15} n={s['n']:2} acc={s['accuracy']:.3f} macroF1={s['macro_f1']:.3f} "
                  f"errors={s['errors']} p50={s['ms_p50'] and round(s['ms_p50'])}ms p95={s['ms_p95'] and round(s['ms_p95'])}ms")
        if "cost_usd" in r:
            print(f"{r['picker']:5} calls={r['calls']} cost=${r['cost_usd']}")
    if args.out:
        args.out.write_text(json.dumps(report, indent=1) + "\n")
        print("wrote", args.out)


if __name__ == "__main__":
    main()
