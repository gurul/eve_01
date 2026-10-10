"""Fixed synthetic image-dependent cases. Jev has AX text; Decisions also has image input.

This measures modality limits and typed result checks, not real application execution.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import random
import sys
import time
from dataclasses import asdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import jev_step_eval as je  # noqa: E402
from computer_protocol_eval import validate_answer  # noqa: E402
from PIL import Image, ImageChops, ImageDraw  # noqa: E402

from eve_bridge import decisions, typed_ask  # noqa: E402
from eve_bridge.decision_lane import Gates  # noqa: E402
from eve_bridge.envfile import load_env_file  # noqa: E402


def scene(seed, selected="Day", absent=False):
    rng = random.Random(seed)
    labels = ["Day", "Week", "Month", "Year"]
    rng.shuffle(labels)
    if absent:
        labels[labels.index("Week")] = "Agenda"
    dark = seed % 2 == 0
    image = Image.new("RGB", (900, 480), "#202529" if dark else "#f6f4ee")
    draw = ImageDraw.Draw(image)
    options = {}
    for i, label in enumerate(labels):
        x, y = 30+(i%2)*430, 35+(i//2)*105
        draw.rounded_rectangle((x, y, x+370, y+80), radius=12,
                               fill="#497dc9" if label == selected else "#535a63" if dark else "#dadde2")
        draw.text((x+20, y+20), label, fill="white" if dark or label == selected else "black",
                  font_size=24+seed%3*4)
        options[str(i)] = f"Control {i} (radio button), rectangle in screen points {(x,y,370,80)}"
    draw.text((40, 350), f"Current view: {selected}", fill="white" if dark else "black", font_size=30)
    return image, options, "none" if absent else str(labels.index("Week"))


def check(path):
    data = json.loads(path.read_text())
    assert data["synthetic"] and not data["deployment_holdout"]
    keys = {(provider, rep, i) for provider in ("Decisions", "Jev") for rep in (1, 2) for i in range(36)}
    assert len(data["rows"]) == len(keys)
    assert {(r["provider"], r["repeat"], r["case"]) for r in data["rows"]} == keys
    for provider in ("Decisions", "Jev"):
        subset = [r for r in data["rows"] if r["provider"] == provider]
        assert len(subset) == 72
        assert {(r["repeat"], r["case"]) for r in subset} == {(rep, i) for rep in (1,2) for i in range(36)}
        for r in subset:
            i=r["case"]
            if i<24:
                image,options,expected=scene(1100+i,absent=i%4==0)
                source_images=[image]
            else:
                expected=i%2==0
                source_images=[scene(1100+i,"Day")[0],scene(1100+i,"Week" if expected else "Month")[0]]
            assert type(r["expected"]) is type(expected) and r["expected"] == expected
            assert r["kind"] == ("target" if i < 24 else "verification")
            answer = r["answer"]
            if i < 24:
                validate_answer(answer, provider)
                if provider == "Decisions":
                    picked = Gates().pick(decisions.Answer(**answer)) or "none"
                else:
                    picked = typed_ask.JEV_STEP_GATES.decide(typed_ask.StepAnswer(**answer))
                    picked = picked if picked not in ("", "confirm", "done") else "none"
                assert r["picked"] == picked and picked in ("none", *options)
            else:
                p = answer["values"].get("verified", 0) if provider == "Decisions" else answer["probability"]
                assert not isinstance(p, bool) and math.isfinite(p) and 0 <= p <= 1
                assert type(r["picked"]) is bool and r["picked"] == (not answer["error"] and p >= Gates().verified)
            assert r["ms"] == answer["ms"] and r["error"] == answer["error"]
            assert type(r["correct"]) is bool and r["correct"] == (r["picked"] == r["expected"])
            assert math.isfinite(r["ms"]) and r["ms"] >= 0
            for image,source in zip(r["images"],source_images,strict=True):
                with Image.open(image) as im:
                    assert im.size == (900,480)
                    assert ImageChops.difference(im.convert("RGB"),source).getbbox() is None
    print("VISUAL_EVAL_CHECKED", len(data["rows"]), "provider responses")


def run(out):
    load_env_file()
    predict = je.make_predict()
    client = decisions.Client(os.environ["OPENAI_API_KEY"])
    images = out.parent / "visual-inputs"
    images.mkdir(parents=True, exist_ok=True)
    report = {"synthetic": True, "deployment_holdout": False,
              "modality": "Jev gets AX text; Decisions gets AX text and authored screen images", "rows": []}
    try:
        for repeat in (1, 2):
            for i in range(36):
                if i < 24:
                    im, options, expected = scene(1100+i, absent=i%4==0)
                    path = images/f"target-{i}.png"
                    im.save(path)
                    state = {"step": "switch to week view", "app": "Authored calendar", "controls": list(options.values())}
                    a = client.evaluate(state, [im], decisions.selection_questions(options))
                    j = typed_ask.ask_jev_step(predict, state["step"], app=state["app"], context="",
                                              options=options, clock=time.perf_counter)
                    jp = typed_ask.JEV_STEP_GATES.decide(j)
                    jp = jp if jp not in ("", "confirm", "done") else "none"
                    answers = [("Decisions", Gates().pick(a) or "none", asdict(a)), ("Jev", jp, asdict(j))]
                    paths, kind = [str(path.resolve())], "target"
                else:
                    expected = i%2 == 0
                    before, _, _ = scene(1100+i, "Day")
                    after, _, _ = scene(1100+i, "Week" if expected else "Month")
                    paths = []
                    for name, im in (("before",before), ("after",after)):
                        path = images/f"verification-{i}-{name}.png"
                        im.save(path)
                        paths.append(str(path.resolve()))
                    state = {"step": "switch to week view", "image_order": ["before", "after"],
                             "before": "unlabelled canvas", "after": "unlabelled canvas"}
                    instruction = "The AFTER observation visibly proves the entire requested step is satisfied. " \
                                  "Image one is before and image two after; mere screen change is insufficient."
                    a = client.evaluate(state, [before,after], [decisions.predicate("verified", instruction)])
                    t0, error, p = time.perf_counter(), "", 0
                    try:
                        response = predict(state, {"verified": {"type": "noul", "instructions": instruction}})
                        p = typed_ask._noul((response.get("answers") or {}).get("verified"))
                    except Exception as e:  # noqa: BLE001 — retain provider failure without private payload
                        error = type(e).__name__
                    answers = [("Decisions", not a.error and a.values.get("verified",0)>=Gates().verified, asdict(a)),
                               ("Jev", not error and p>=Gates().verified, {"ms":(time.perf_counter()-t0)*1000,
                                                            "error":error,"probability":p})]
                    kind = "verification"
                for provider, picked, answer in answers:
                    report["rows"].append({"provider":provider,"case":i,"repeat":repeat,"kind":kind,
                                           "expected":expected,"picked":picked,"correct":picked==expected,
                                           "images":paths,"answer":answer,"ms":answer["ms"],"error":answer["error"]})
                out.write_text(json.dumps(report,indent=2)+"\n")
                if i%12==11:
                    print(f"visual repeat {repeat}: {i+1}/36 cases",flush=True)
    finally:
        client.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out",type=Path)
    parser.add_argument("--check",type=Path)
    args = parser.parse_args()
    if args.check:
        check(args.check)
    elif args.out:
        run(args.out)
    else:
        parser.error("choose --out or --check")
