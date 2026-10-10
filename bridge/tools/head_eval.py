#!/usr/bin/env python3
"""Could a typed-decision model (or plain code) pick eve's head pose, instead of the backend?

Today "hey eve, look left" goes voice → gpt-6-astra backend → `move_head(yaw, pitch, relative)`:
about 3.2 s from the end of the words to the head moving (daemon log, 2026-09-20 16:42). The pose
is a closed set, so this asks three cheaper deciders the same question on the same utterances
(tests/fixtures/routes/head_moves.json, written by an author who saw no code):

    rule   a keyword parser, microseconds, nothing leaves the Mac
    laya   the local typed-decision model (laya-mlx), ~10 ms, nothing leaves the Mac
    jev    TypeSafe's hosted model, 200–400 ms, the utterance leaves the Mac

    .venv/bin/python tools/head_eval.py                 # the rule
    .venv/bin/python tools/head_eval.py --model laya     # asked in ITS idiom (typed_ask.py), cut-offs fitted
    .venv/bin/python tools/head_eval.py --model jev      # asked in ITS idiom, cut-offs fitted
    .venv/bin/python tools/head_eval.py --model laya --uniform   # the contrast: one 16-option question, p ≥ 0.80

A native arm is scored honestly: the utterances are split in two by position (even = calibration, odd =
test); the model's cut-offs are fitted on the calibration half (typed_ask.fit_gates, zero false moves
allowed) and every number printed as "test" comes from the half the cut-offs never saw.

Two numbers matter. `false moves`: a "none" utterance ("scroll down", "look up the weather", "I feel
down today") that got a pose — the head twitching at ordinary speech is the failure nobody forgives.
`pose accuracy`: of the real pose commands, how many got exactly the right pose. A model acts only
at p ≥ 0.80; below that it abstains, and an abstention is "none" (the backend then decides as today).
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
import sys
import time
from pathlib import Path
from typing import Callable, Optional

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

DATA = Path(__file__).resolve().parents[1] / "tests" / "fixtures" / "routes" / "head_moves.json"
ACT_AT = 0.80
OPTIONS = {
    "left": "turn the robot's head to its left",
    "right": "turn the robot's head to its right",
    "up": "tilt the robot's head up",
    "down": "tilt the robot's head down",
    "center": "face the owner again, look straight ahead",
    "far_left": "turn the head all the way to the left, as far as it goes",
    "far_right": "turn the head all the way to the right, as far as it goes",
    "bit_left": "nudge the head a little more to the left",
    "bit_right": "nudge the head a little more to the right",
    "bit_up": "nudge the head a little more up",
    "bit_down": "nudge the head a little more down",
    "desk": "look down at the desk or the table",
    "ceiling": "look straight up at the ceiling",
    "none": "not a request to move the robot's head: a computer request, a question, or just talk",
    "reobserve": "the words are unclear",
    "abstain": "none of these",
}

HEAD_VERB = re.compile(r"\b(look|turn|face|tilt|pan|swivel|rotate|point|nod|head|eyes|gaze|glance|peek|swing)\b")
NOT_HEAD = re.compile(
    r"\b(scroll|volume|window|tab|tabs|swipe|page|brightness|maps|street|list|click|menu|screen|cursor|arrow|song|track|"
    r"music|video|file|folder|app|browser|zoom|slide|line|column|row|feel|feeling|looking up|things are)\b|"
    r"\blook(?:ing)? (?:at|for|around|into|through|like|up (?:the|that|this|a|an|how|what|who|where|my|some)\b)|"
    r"\b(?:what|where|who|why|how|which|do you|can you see|did you)\b.*\b(?:see|is|are)\b|\bfind\b|\bscan\b|\bstop\b|\bcancel\b")
BIT = re.compile(r"\b(a bit|a little|little|slightly|a touch|a hair|a tad|a smidge|tiny bit|just a|bit more|nudge)\b")
FAR = re.compile(r"\b(all the way|as far as|fully|hard|far|max|maximum|completely|extreme)\b")


def rule(text: str) -> str:
    """A pose from keywords, or "none". Untuned: written before the dataset was seen."""
    t = " " + " ".join(re.findall(r"[a-z']+", text.casefold())) + " "
    if NOT_HEAD.search(t):
        return "none"
    words = t.split()
    if not HEAD_VERB.search(t) and len(words) > 4:
        return "none"
    if re.search(r"\b(ceiling|straight up)\b", t):
        return "ceiling"
    if re.search(r"\b(desk|table)\b", t):
        return "desk"
    if re.search(r"\b(at me|face me|facing me|eyes front|straight ahead|cent(?:er|re)|come back|forward|front|back here)\b", t):
        return "center"
    found = [d for d in ("left", "right", "up", "down") if re.search(rf"\b{d}(?:wards?)?\b", t)]
    if len(found) != 1:
        return "none"
    d = found[0]
    if BIT.search(t):
        return f"bit_{d}"
    if FAR.search(t) and d in ("left", "right"):
        return f"far_{d}"
    return d


def raw_predict(name: str):
    """The model's own predict(state, questions), with nothing of ours wrapped round it."""
    from eve_bridge.envfile import load_env_file

    load_env_file()
    if name == "laya":
        import os

        import laya_mlx
        from laya_decider import model_path

        agent = laya_mlx.Agent(os.path.expanduser(model_path()), dtype="float16", device="gpu", batch_size=16,
                               compile=True, cache_prompts=False)
        agent.predict({"owner said": "look left"}, {"q": {"type": "choice", "instructions": "which way?",
                                                           "criteria": {"left": "left", "right": "right"}}})   # warm
        return agent.predict
    import os

    from eve_bridge import jev

    url, key, model = jev.route_config(os.environ)
    return jev.make_predict(url, key, model, timeout_s=8.0)


def load_uniform(name: str) -> Callable[[str], tuple[str, float, float]]:
    """The contrast arm: both models asked the same 16-option question through decider.Decider."""
    from eve_bridge.envfile import load_env_file

    load_env_file()
    if name == "laya":
        import laya_decider

        decider = laya_decider.load(style="compact")
    else:
        from eve_bridge import jev

        decider = jev.load(style="compact")

    def choose(text: str) -> tuple[str, float, float]:
        t0 = time.perf_counter()
        c = decider.choose("what head movement is the robot being asked for?", app="eve",
                           context=f"the owner said: {text}", options=OPTIONS)
        return (c.id or "abstain"), float(c.p_top), (time.perf_counter() - t0) * 1000.0

    return choose


def report(title: str, cases: list[dict], got: list[str], ms: list[float], show: bool, conf: Optional[list[float]] = None) -> None:
    poses = [(c, g) for c, g in zip(cases, got, strict=True) if c["label"] != "none"]
    nones = [(c, g) for c, g in zip(cases, got, strict=True) if c["label"] == "none"]
    right = sum(1 for c, g in poses if g == c["label"])
    wrong = sum(1 for c, g in poses if g not in (c["label"], "none"))
    messy = [(c, g) for c, g in poses if c.get("messy")]
    false_moves = [c["text"] for c, g in nones if g != "none"]
    print(f"== head pose / {title}: n={len(cases)} ({len(poses)} pose commands, {len(nones)} none)")
    print(f"   pose accuracy {100 * right / max(1, len(poses)):5.1f}%  (right {right}, wrong pose {wrong}, "
          f"left to the backend {len(poses) - right - wrong})")
    print(f"   messy speech  {100 * sum(1 for c, g in messy if g == c['label']) / max(1, len(messy)):5.1f}%  (n={len(messy)})")
    print(f"   false moves   {len(false_moves)} of {len(nones)} none utterances "
          f"({100 * len(false_moves) / max(1, len(nones)):.1f}%)")
    if ms:
        print(f"   latency ms    p50 {statistics.median(ms):.1f}  p95 {sorted(ms)[int(0.95 * (len(ms) - 1))]:.1f}")
    for text in false_moves[:8]:
        print(f"   FALSE MOVE {text!r}")
    if show:
        for c, g in zip(cases, got, strict=True):
            if g != c["label"]:
                print(f"   miss  {c['text']!r}: said {g}, truth {c['label']}")


def main(argv: Optional[list[str]] = None) -> int:
    from eve_bridge import typed_ask as ta

    p = argparse.ArgumentParser(prog="head_eval", description=__doc__.split("\n\n")[0])
    p.add_argument("--model", choices=("laya", "jev"))
    p.add_argument("--uniform", action="store_true", help="the one-size-fits-all contrast arm")
    p.add_argument("--data", default=str(DATA))
    p.add_argument("--show", action="store_true", help="print every miss")
    args = p.parse_args(argv)
    cases = json.loads(Path(args.data).read_text(encoding="utf-8"))["cases"]
    if not args.model:
        t0 = time.perf_counter()
        got = [rule(c["text"]) for c in cases]
        each = (time.perf_counter() - t0) * 1000.0 / len(cases)
        report("rule (keywords, untuned)", cases, got, [each] * len(cases), args.show)
    elif args.uniform:
        choose = load_uniform(args.model)
        out = [choose(c["text"]) for c in cases]
        got = [k if conf >= ACT_AT and k not in ("reobserve", "abstain") else "none" for k, conf, _ in out]
        report(f"{args.model}, UNIFORM (one 16-option question, p ≥ {ACT_AT:.2f})", cases, got, [m for *_, m in out], args.show)
    else:
        predict = raw_predict(args.model)
        ask = ta.ask_laya_head if args.model == "laya" else ta.ask_jev_head
        questions = ta.LAYA_HEAD_QUESTIONS if args.model == "laya" else ta.JEV_HEAD_QUESTIONS
        answers = [ask(predict, c["text"], time.perf_counter) for c in cases]
        errors = sum(1 for a in answers if a.error)
        calib = list(range(0, len(cases), 2))
        test = list(range(1, len(cases), 2))
        gates = ta.fit_gates([answers[i] for i in calib], [cases[i]["label"] for i in calib])
        print(f"protocol for {args.model}: {ta.describe(questions)}")
        print(f"cut-offs fitted on the {len(calib)} calibration utterances (zero false moves allowed): "
              f"gate ≥ {gates.gate:.2f}, direction ≥ {gates.direction:.2f}; model errors {errors}")
        report(f"{args.model}, NATIVE protocol / calibration half", [cases[i] for i in calib],
               [gates.decide(answers[i]) for i in calib], [], False)
        report(f"{args.model}, NATIVE protocol / TEST half (unseen by the cut-offs)", [cases[i] for i in test],
               [gates.decide(answers[i]) for i in test], [answers[i].ms for i in test], args.show)
    print("HEAD_EVAL_COMPLETE")
    return 0


if __name__ == "__main__":
    sys.exit(main())
