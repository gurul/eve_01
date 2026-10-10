#!/usr/bin/env python3
"""Read-only Decisions/Jev comparison on authored toolbar screens; never drives the desktop.

Measures labelled and image-dependent target choices plus positive/negative result verification.
These are synthetic diagnostic cases, not an unread deployment holdout or proof of a speedup.
Run: .venv/bin/python tools/decisions_eval.py --live --out ../docs/decisions-eval.json
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import time
from dataclasses import asdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from PIL import Image, ImageDraw  # noqa: E402

from eve_bridge import decisions, jev, typed_ask  # noqa: E402
from eve_bridge.decision_lane import Gates  # noqa: E402
from eve_bridge.envfile import load_env_file  # noqa: E402


def screen(labels: list[str], selected: str = "") -> Image.Image:
    image = Image.new("RGB", (800, 400), "white")
    draw = ImageDraw.Draw(image)
    for i, label in enumerate(labels):
        x = 40 + i * 180
        draw.rectangle((x, 60, x + 150, 120), fill="#b6d7ff" if label == selected else "#eeeeee", outline="black")
        draw.text((x + 20, 80), label, fill="black", font_size=24)
    draw.text((50, 190), f"Current view: {selected or 'Day'}", fill="black", font_size=28)
    return image


def summarize(rows):
    output = {}
    for provider in ("decisions", "jev"):
        subset = [r for r in rows if r["provider"] == provider]
        output[provider] = {}
        for kind in ("target", "verification"):
            group = [r for r in subset if r["kind"] == kind]
            answered = [r for r in group if not r.get("error")]
            ms = sorted(r["ms"] for r in answered)
            output[provider][kind] = {
                "cases": len(group), "answered": len(answered), "correct": sum(r["correct"] for r in answered),
                "wrong": sum(not r["correct"] and (kind != "target" or r.get("picked") not in ("", "none"))
                             for r in answered),
                "abstained": sum(r.get("picked") in ("", "none") for r in answered) if kind == "target" else 0,
                "errors": len(group) - len(answered),
                "p50_ms": statistics.median(ms) if ms else None,
                "p90_ms": ms[min(len(ms) - 1, int(len(ms) * 0.9))] if ms else None,
                "input_tokens": sum(r.get("usage", {}).get("input_tokens", 0) for r in group)}
    return output


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true", help="Call paid APIs with authored screens only.")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if not args.live:
        parser.error("--live is required; this tool does not fabricate model evidence")
    load_env_file()
    client = decisions.Client(os.environ.get("OPENAI_API_KEY", ""))
    rows, failures = [], []
    predict = None
    try:
        url, key, model = jev.route_config(os.environ)
        predict = jev.make_predict(url, key, model, timeout_s=3)
    except jev.JevError:
        failures.append("Jev credentials unavailable")
    try:
        # First establish endpoint access. Stop on failure rather than incur a run of failing calls.
        probe = client.evaluate({"purpose": "read-only beta access check"}, [],
                                [decisions.predicate("ready", "The input describes an access check.")])
        if probe.error:
            failures.append(probe.error)
        else:
            labels = ["Day", "Week", "Month", "Year"]
            for visible_labels in (True, False):
                for target in ("Week", "Month", "Year"):
                    options = {str(i): f"{label if visible_labels else f'Control {i}'} (radio button), "
                               f"rectangle in screen points {(40 + i * 180, 60, 150, 60)}"
                               for i, label in enumerate(labels)}
                    goal = f"switch to {target.lower()} view"
                    state = {"step": goal, "app": "Synthetic calendar", "controls": list(options.values()),
                             "enabled": True}
                    a = client.evaluate(state, [screen(labels)], decisions.selection_questions(options))
                    picked = Gates().pick(a)
                    rows.append({"provider": "decisions", "kind": "target", "visual_only": not visible_labels,
                                 "expected": str(labels.index(target)), "picked": picked,
                                 "correct": picked == str(labels.index(target)), **asdict(a)})
                    if predict:
                        before = jev.METER.snapshot()
                        j = typed_ask.ask_jev_step(predict, goal, app="Synthetic calendar", context="",
                                                  options=options, clock=time.perf_counter)
                        picked = typed_ask.JEV_STEP_GATES.decide(j)
                        rows.append({"provider": "jev", "kind": "target", "visual_only": not visible_labels,
                                     "expected": str(labels.index(target)), "picked": picked,
                                     "correct": picked == str(labels.index(target)), "ms": j.ms, "error": j.error,
                                     "usage": {"input_tokens": jev.Meter.delta(before, jev.METER.snapshot())["input_tokens"]}})
            for visible_labels in (True, False):
                for satisfied in (True, False):
                    after = "Week" if satisfied else "Month"
                    state = {"step": "switch to week view", "image_order": ["before", "after"],
                             "before": {"view": "Day" if visible_labels else "unlabelled canvas"},
                             "after": {"view": after if visible_labels else "unlabelled canvas"}}
                    instruction = "The AFTER observation visibly proves the entire step is satisfied. " \
                                  "Image one is before, image two after; a changed screen alone is insufficient."
                    a = client.evaluate(state, [screen(labels, "Day"), screen(labels, after)],
                                        [decisions.predicate("verified", instruction)])
                    value = a.values.get("verified", 0) >= Gates().verified
                    rows.append({"provider": "decisions", "kind": "verification", "visual_only": not visible_labels,
                                 "expected": satisfied, "verified": value, "correct": value == satisfied, **asdict(a)})
                    if predict:
                        start = time.perf_counter()
                        before = jev.METER.snapshot()
                        error, value = "", False
                        try:
                            result = predict(state, {"verified": {"type": "noul", "instructions": instruction}})
                            probability = typed_ask._noul((result.get("answers") or {}).get("verified"))
                            value = probability >= Gates().verified
                        except Exception as e:  # noqa: BLE001 — record provider failure without its private payload
                            error = type(e).__name__
                        rows.append({"provider": "jev", "kind": "verification", "visual_only": not visible_labels,
                                     "expected": satisfied, "verified": value, "correct": value == satisfied,
                                     "ms": (time.perf_counter() - start) * 1000, "error": error,
                                     "usage": {"input_tokens": jev.Meter.delta(before, jev.METER.snapshot())["input_tokens"]}})
    finally:
        client.close()
    report = {"synthetic": True, "deployment_holdout": False, "failures": failures,
              "summary": summarize(rows), "rows": rows}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"failures": failures, "summary": report["summary"]}, indent=2))
    return 1 if failures or any(r.get("error") for r in rows) else 0


if __name__ == "__main__":
    raise SystemExit(main())
