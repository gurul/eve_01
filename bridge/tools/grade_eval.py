#!/usr/bin/env python3
"""Fit and score the grade router (grade_router.py): Jev grading app turns instant or think.

    .venv/bin/python tools/grade_eval.py --tune              # live Jev over the tuning set, cached; fit the gate
    .venv/bin/python tools/grade_eval.py --holdout           # live Jev ONCE over the blind holdout; the verdict
    .venv/bin/python tools/grade_eval.py --check-default     # grade_router.SHIPPED equals the kept verdict

The recipe and the bar are grade_router.py's module text (the house recipe of tools/toolset_eval.py). Jev is asked
with an 8 s budget here so a slow answer is measured rather than cut; the bar's p95 gate judges the latency the turn
would see. The rows kept carry the case id, its label, Jev's Noul, the milliseconds and any error: never the text.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
from pathlib import Path
from typing import Any, Optional

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "src"))

from eve_bridge import grade_router as gr  # noqa: E402

DATA = ROOT / "tests" / "fixtures"
TUNING, TUNING_ANSWERS = "grade_tuning.json", "grade_tuning_answers.json"
HOLDOUT, HOLDOUT_SHA, RESULT = "grade_holdout.json", "grade_holdout.sha256", "grade_result.json"


def predict():
    from eve_bridge import jev
    from eve_bridge.envfile import load_env_file

    load_env_file()
    url, key, model = jev.route_config(os.environ)
    return jev.make_predict(url, key, model, timeout_s=8.0)


def previous_of(case: dict[str, Any]) -> str:
    """The owner's previous message, as the turn would pass it (the last owner line of the context)."""
    owner = [t["text"] for t in case.get("previous") or [] if t.get("role") == "owner"]
    return owner[-1] if owner else ""


def run(cases: list[dict[str, Any]]) -> list[gr.GradeAnswer]:
    p = predict()
    return [gr.ask(p, c["message"], previous_of(c), time.perf_counter) for c in cases]


def tune(data: Path = DATA) -> dict[str, Any]:
    cases = json.loads((data / TUNING).read_text(encoding="utf-8"))["cases"]
    cache = data / TUNING_ANSWERS
    if cache.exists():
        answers = [gr.GradeAnswer.from_json(r) for r in json.loads(cache.read_text(encoding="utf-8"))["answers"]]
    else:
        answers = run(cases)
        cache.write_text(json.dumps({"answers": [a.to_json() for a in answers]}, indent=1) + "\n", encoding="utf-8")
    acceptable = [c["acceptable"] for c in cases]
    gate, recall = gr.fit(answers, acceptable)
    grades = [gr.decide(a, gate) for a in answers]
    report = gr.bar(grades, acceptable, answers)            # on the TUNING set: a look, not the verdict
    gr.GATE_FILE.write_text(json.dumps({"gate": gate, "tuning_recall": round(recall, 3), "fit_on": TUNING},
                                       indent=1) + "\n", encoding="utf-8")
    return {"gate": gate, "tuning_recall": round(recall, 3), "tuning_look": report}


def holdout(data: Path = DATA) -> dict[str, Any]:
    if (data / RESULT).exists():
        raise SystemExit("the holdout was already scored; it is scored once")
    want = (data / HOLDOUT_SHA).read_text(encoding="utf-8").split()[0]
    if hashlib.sha256((data / HOLDOUT).read_bytes()).hexdigest() != want:
        raise SystemExit("the holdout does not match its committed sha256")
    if not gr.GATE_FILE.exists():
        raise SystemExit("fit the gate first (--tune)")
    cases = json.loads((data / HOLDOUT).read_text(encoding="utf-8"))["cases"]
    gate = gr.load_gate()
    answers = run(cases)
    acceptable = [c["acceptable"] for c in cases]
    grades = [gr.decide(a, gate) for a in answers]
    verdict = gr.bar(grades, acceptable, answers)
    rows = [{"id": c["id"], "acceptable": c["acceptable"], "grade": g, **a.to_json()}
            for c, g, a in zip(cases, grades, answers, strict=True)]
    (data / RESULT).write_text(json.dumps({"sha256": want, "gate": gate, "bar": verdict, "rows": rows}, indent=1)
                               + "\n", encoding="utf-8")
    return verdict


def check_default(data: Path = DATA) -> str:
    path = data / RESULT
    if not path.exists():
        assert gr.SHIPPED is False, "unscored: the grade router does not ship"
        return "unscored: off"
    kept = json.loads(path.read_text(encoding="utf-8"))
    rows = kept["rows"]
    answers = [gr.GradeAnswer.from_json(r) for r in rows]
    verdict = gr.bar([r["grade"] for r in rows], [r["acceptable"] for r in rows], answers)
    assert gr.SHIPPED is verdict["shipped"], f"SHIPPED is {gr.SHIPPED}; the verdict says {verdict['shipped']}"
    return f"{'passed' if verdict['shipped'] else 'failed'}: SHIPPED={gr.SHIPPED}"


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--tune", action="store_true")
    p.add_argument("--holdout", action="store_true")
    p.add_argument("--check-default", action="store_true")
    a = p.parse_args(argv)
    if a.tune:
        print(json.dumps(tune(), indent=1))
    if a.holdout:
        print(json.dumps(holdout(), indent=1))
    if a.check_default:
        print("GRADE_DEFAULT_OK", check_default())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
