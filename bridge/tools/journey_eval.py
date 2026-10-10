#!/usr/bin/env python3
"""Fit and score jev_verify's cut-offs on real apps: which control a journey step means, and whether the screen
shows what a journey expects.

    .venv/bin/python tools/journey_eval.py write  OUT APPS_DIR [APPS_DIR ...]   # Claude writes each app's journeys
    .venv/bin/python tools/journey_eval.py record OUT                           # walk them, the oracle acting
    .venv/bin/python tools/journey_eval.py ask    OUT                           # Jev answers every case (network)
    .venv/bin/python tools/journey_eval.py fit    OUT                           # fit on one half, score the other
    .venv/bin/python tools/journey_eval.py copy   OUT APPS_DIR [APPS_DIR ...]   # apps built WITH journeys, as they are
    .venv/bin/python tools/journey_eval.py score  OUT                           # the shipped cut-offs, no fitting

``write`` asks Claude (``claude-opus-5-5`` under the maker's own system prompt, so the journeys come from the
distribution real builds produce, mistakes included) for the ```journeys block of each app already built, and
copies the app to ``OUT/apps/<kind>--<n>/``. ``record`` walks every journey in app_check's sandbox with a CODE
ORACLE in Jev's place: the control whose normalized label equals the step's (jev_verify._norm, emoji and "+" and
case dropped), else none. At each step it keeps exactly the state Jev would get; after the last step, the
screen's lines with each expectation, labelled by the oracle (the normalized text is in the normalized screen).
A journey whose step the oracle cannot place stops there; that step is a real "not on screen" case.

It then adds the negatives the verifier exists to catch, on the same screens:
  - a step whose label is not on the screen (a label from another app's journeys);
  - the same step with its control REMOVED from the list (the app lost its core button);
  - an expectation with a number changed, or taken from another app;
  - each expectation read against the FIRST screen, before any step ran (a feature that did nothing);
  - the same screen with the expectation's words and numbers kept and its meaning flipped (``flips``): two names
    or two numbers trading places ("Sam owes Alex $15.00" for "Alex owes Sam $15.00"), or the phrase broken up
    around its number ("streak ended · best 1 day" for "1-day streak"). The number veto cannot catch these;
    only Jev's reading can, and a live build let "1-day streak" pass over "Streak ended · best 1 day" at 0.56.
``OUT/labels.json`` overrides any label by case id: the cases where the oracle and a person disagree (a loose
label match, "$15" against "$15.00") were read by hand and are listed there with the reason.

``copy`` takes apps the maker built with their own journeys (tools/app_eval.py's OUT/apps) instead of ``write``,
and ``score`` scores every case of OUT under the cut-offs jev_verify ships, fitting nothing: the measurement on a
set nobody tuned on. Label (labels.json) before ``ask``, so no label is written after seeing Jev's answer.

``fit`` splits by app KIND (every build of the habit tracker on the same side), fits
``typed_ask.fit_step_gates`` (zero wrong presses allowed) and the expectation cut-off (zero false "shown"
allowed) on the calibration half, and scores the held-out half once. ``OUT/report.json`` gets every number.
"""

from __future__ import annotations

import argparse
import asyncio
import concurrent.futures
import hashlib
import json
import os
import re
import statistics
import sys
import time
from dataclasses import asdict
from pathlib import Path
from typing import Any, Optional

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from eve_bridge import jev_verify as jv  # noqa: E402
from eve_bridge.typed_ask import StepAnswer, StepGates, fit_step_gates  # noqa: E402

ENV_FILE = Path.home() / ".config" / "eve-bridge" / "env"
EXPECT_GRID = (0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 0.95)


def owner_env() -> dict[str, str]:
    from eve_bridge.envfile import parse_env_file

    try:
        file = parse_env_file(ENV_FILE.read_text())
    except OSError:
        file = {}
    keys = ("OPENROUTER_API_KEY", "TYPESAFE_API_KEY", "EVE_JEV_ROUTE", "EVE_JEV_MODEL")
    return {k: os.environ.get(k) or file.get(k, "") for k in keys}


def kind_of(name: str) -> str:
    return re.sub(r"-\d+$", "", name.split("--")[0])


def cid(*parts: Any) -> str:
    return hashlib.sha1(json.dumps(parts, ensure_ascii=False).encode()).hexdigest()[:12]


# ---- write: Claude writes each app's journeys ---------------------------------------------------------------

async def write(out: Path, dirs: list[Path], model: str) -> None:
    from eve_bridge import apps_maker, openrouter

    env = owner_env()
    client = openrouter.anthropic_client(env["OPENROUTER_API_KEY"])     # Claude through OpenRouter
    apps = sorted({p.parent for d in dirs for p in d.glob("*/index.html")})
    (out / "apps").mkdir(parents=True, exist_ok=True)
    spent = 0.0
    from eve_bridge.miniapp import cost_usd

    async def one(i: int, src: Path) -> float:
        name = f"{src.name}--{i}"
        dst = out / "apps" / name
        if (dst / "journeys.json").is_file():
            return 0.0
        html = (src / "index.html").read_text()
        msg = await client.messages.create(
            model=model, max_tokens=16000, thinking={"type": "adaptive"}, output_config={"effort": "medium"},
            system=[{"type": "text", "text": apps_maker.MAKER_PROMPT,
                     "cache_control": {"type": "ephemeral", "ttl": apps_maker.SYSTEM_CACHE_TTL}}],
            messages=[{"role": "user", "content": (
                "Here is an app you built earlier:\n```html\n" + html + "\n```\n\nWrite its ```journeys block "
                "(see <journeys>) for the app exactly as this file is, and nothing else.")}])
        text = "".join(getattr(b, "text", "") for b in msg.content)
        found, problems = jv.extract_journeys(text)
        dst.mkdir(parents=True, exist_ok=True)
        (dst / "index.html").write_text(html)
        (dst / "journeys.json").write_text(json.dumps(jv.journeys_json(found or []), indent=1, ensure_ascii=False))
        usd = cost_usd(model, msg.usage)
        print(f"{name}: {len(found or [])} journeys {problems or ''} ${usd:.3f}", file=sys.stderr)
        return usd

    gate = asyncio.Semaphore(4)

    async def run(i: int, src: Path) -> float:
        async with gate:
            return await one(i, src)

    spent = sum(await asyncio.gather(*(run(i, s) for i, s in enumerate(apps, 1))))
    print(f"wrote journeys for {len(apps)} apps, ${spent:.3f}")
    log = out / "spend.json"
    prior = json.loads(log.read_text()) if log.is_file() else {}
    prior["write_usd"] = round(prior.get("write_usd", 0.0) + spent, 4)
    log.write_text(json.dumps(prior, indent=1))


# ---- record: walk with the oracle --------------------------------------------------------------------------

def oracle_match(step: jv.Step, by_id: dict[str, jv.Control]) -> tuple[str, str]:
    """(the option id the step means, how sure): "exact" when one control's normalized label is the step's,
    "loose" when exactly one label holds it or is held by it (a person reads these), "none" otherwise."""
    want = jv._norm(step.target)
    exact = [k for k, c in by_id.items() if jv._norm(c.label) == want]
    if len(exact) == 1:
        return exact[0], "exact"
    if len(exact) > 1:
        return "", "ambiguous"
    loose = [k for k, c in by_id.items() if want and (want in jv._norm(c.label) or jv._norm(c.label) in want)
             and len(jv._norm(c.label)) >= 3]
    if len(loose) == 1:
        return loose[0], "loose"
    return "", "none"


def shown_oracle(expect: str, lines: tuple[str, ...]) -> bool:
    return jv._norm(expect) in jv._norm(" ".join(lines))


class Oracle:
    """Stands where jev_verify.Verifier stands in `walk`, and records what Jev would have been asked."""

    step_gates = StepGates()
    expect_shown = 0.5

    def __init__(self, app: str, journey: str, labels: Optional[dict[str, Any]] = None) -> None:
        self.app, self.journey = app, journey
        self.labels = labels or {}
        self.steps: list[dict[str, Any]] = []
        self.expects: list[dict[str, Any]] = []
        self.n = 0

    async def pick(self, step: jv.Step, screen: jv.Screen) -> tuple[Optional[jv.Control], StepAnswer, dict[str, jv.Control]]:
        self.n += 1
        by_id = jv.offered(step, screen)
        key, how = oracle_match(step, by_id)
        hand = self.labels.get(cid("step", self.app, self.journey, self.n, asdict(step), "real"), {})
        if hand.get("expected") in by_id:                 # a person read this one: walk on as they labelled it
            key, how = hand["expected"], "hand"
        self.steps.append({"app": self.app, "journey": self.journey, "n": self.n, "step": asdict(step),
                           "screen": screen.summary(), "options": {k: c.render() for k, c in by_id.items()},
                           "labels": {k: c.label for k, c in by_id.items()}, "expected": key or "abstain",
                           "oracle": how})
        a = StepAnswer(key or jv.STEP_NONE, 1.0, 1.0, 1.0 if key else 0.0, 0.0, 0.0)
        return (by_id[key] if key and how in ("exact", "loose", "hand") else None), a, by_id

    async def shown(self, lines: tuple[str, ...], expects: tuple[str, ...]) -> list[float]:
        for e in expects:
            self.expects.append({"app": self.app, "journey": self.journey, "expect": e, "lines": list(lines),
                                 "shown": shown_oracle(e, lines), "source": "after"})
        return [1.0 if shown_oracle(e, lines) else 0.0 for e in expects]


async def record(out: Path) -> None:
    from playwright.async_api import async_playwright

    from eve_bridge import app_check

    steps: list[dict[str, Any]] = []
    expects: list[dict[str, Any]] = []
    walks: list[dict[str, Any]] = []
    labels = json.loads((out / "labels.json").read_text()) if (out / "labels.json").is_file() else {}
    apps = sorted(p.parent for p in (out / "apps").glob("*/journeys.json"))
    async with async_playwright() as p:
        b = await p.chromium.launch(headless=True, args=app_check.CHROMIUM_ARGS)
        for app in apps:
            html = (app / "index.html").read_text()
            journeys, _ = jv.parse_journeys(json.loads((app / "journeys.json").read_text()))
            for j in journeys:
                sess = app_check._Session(None)
                ctx, page, loaded = await app_check._open(b, html, sess, app_check.PHONE, dark=False, clock=False)
                try:
                    if not loaded:
                        continue
                    first = jv.screen_from({"text": await page.evaluate(jv.TEXT_JS)}).lines
                    o = Oracle(app.name, j.name, labels)
                    res = await jv.walk(page, j, o, errors=sess.issues)  # type: ignore[arg-type]
                finally:
                    await ctx.close()
                steps += o.steps
                expects += o.expects
                for e in j.expect:              # the feature did nothing: the first screen
                    expects.append({"app": app.name, "journey": j.name, "expect": e, "lines": list(first),
                                    "shown": shown_oracle(e, first), "source": "first"})
                walks.append({"app": app.name, "journey": j.name, "status": res.status, "step": res.step,
                              "reason": res.reason, "errors": res.errors})
                print(f"{app.name} / {j.name}: {res.status} {res.step or ''} {res.reason[:120]}", file=sys.stderr)
        await b.close()
    for s in steps:
        s["id"] = cid("step", s["app"], s["journey"], s["n"], s["step"], "real")
        s["case"] = "real"
    for e in expects:
        e["id"] = cid("expect", e["app"], e["journey"], e["expect"], e["source"])
        e["case"] = e["source"]
    (out / "recorded.json").write_text(json.dumps({"steps": steps, "expects": expects, "walks": walks}, indent=1,
                                                  ensure_ascii=False))
    print(f"recorded {len(steps)} steps, {len(expects)} expectations over {len(walks)} journeys")


def _bump(text: str) -> Optional[str]:
    """``text`` with its first number changed (15.00 -> 22.00, 3 -> 10), or None when it has no number."""
    m = re.search(r"\d+(?:[.,]\d+)?", text)
    if not m:
        return None
    raw = m.group(0)
    n = float(raw.replace(",", ".")) + 7
    new = f"{n:.{len(raw.split('.')[-1]) if '.' in raw else 0}f}"
    return text[:m.start()] + new + text[m.end():]


_NAME = re.compile(r"\b[A-Z][a-z]{2,}\b")
_NUM = re.compile(r"(?<![\d.,])\d+(?:[.,]\d+)?(?![\d.,]*\d)")


def _swap(line: str, a: str, b: str, rx: str) -> str:
    """``line`` with ``a`` and ``b`` trading places (``rx`` bounds a match: a whole word, a whole number)."""
    one, two = "\x00", "\x01"
    line = re.sub(rx.format(re.escape(a)), one, line)
    line = re.sub(rx.format(re.escape(b)), two, line)
    return line.replace(one, b).replace(two, a)


def flips(expect: str, lines: tuple[str, ...]) -> list[tuple[str, list[str]]]:
    """Screens that keep ``expect``'s words and numbers and lose its meaning, each ("swap" | "reword", lines):
    the line that shows it rewritten. Only those where the expectation is really gone (the oracle) and every
    number it holds is still on screen (numbers_shown cannot veto it; words_shown and Jev must)."""
    want = jv._norm(expect)
    at = next((i for i, line in enumerate(lines) if want and want in jv._norm(line)), None)
    if at is None:
        return []
    line, out = lines[at], []
    names, nums = list(dict.fromkeys(_NAME.findall(expect))), list(dict.fromkeys(_NUM.findall(expect)))
    # two names swap only across a word that relates them ("Alex owes Sam"): a title's words ("Weekly Summary")
    # or a label and its value ("Today · Good") read the same either way round
    between = expect[expect.find(names[0]) + len(names[0]):expect.find(names[1])] if len(names) >= 2 else ""
    names = names if re.search(r"\b[a-z]{2,}\b", between) else []
    swapped = (_swap(line, names[0], names[1], r"\b{}\b") if len(names) >= 2 else
               _swap(line, nums[0], nums[1], r"(?<![\d.,]){}(?![\d.,]*\d)") if len(nums) >= 2 else "")
    if swapped and swapped != line:
        out.append(("swap", list(lines[:at]) + [swapped] + list(lines[at + 1:])))
    m = re.search(r"(\d+(?:[.,]\d+)?)(?:[-\s]([^\W\d_]+))?", expect)
    if m:
        rest = " ".join((expect[:m.start()] + " " + expect[m.end():]).replace("-", " ").split()).strip(" ·,:;")
        unit = f"{m.group(1)} {m.group(2)}" if m.group(2) else m.group(1)
        prefix = expect[:m.start()][-1:] if expect[:m.start()][-1:] in "$€£₹" else ""
        rest = rest[:-1].strip() if prefix and rest.endswith(prefix) else rest
        if re.search(r"[^\W\d_]{2,}", rest):
            reworded = f"{rest} ended · best {prefix}{unit}"
            out.append(("reword", list(lines[:at]) + [reworded] + list(lines[at + 1:])))
    return [(k, new) for k, new in out
            if not shown_oracle(expect, tuple(new)) and jv.numbers_shown(expect, tuple(new))]


def build_cases(out: Path) -> dict[str, list[dict[str, Any]]]:
    """Recorded cases plus the negatives, with the hand labels applied."""
    rec = json.loads((out / "recorded.json").read_text())
    labels = json.loads((out / "labels.json").read_text()) if (out / "labels.json").is_file() else {}
    steps: list[dict[str, Any]] = []
    for s in rec["steps"]:
        steps.append(dict(s))
        if s["oracle"] != "exact" and s["id"] not in labels:
            continue
        if s["expected"] == "abstain":
            continue
        # absent label: another app's target of the same kind that no control here carries
        others = sorted({t["step"]["target"] for t in rec["steps"] if t["app"].split("--")[0] != s["app"].split("--")[0]
                         and t["step"]["do"] == s["step"]["do"]})
        pool = [t for t in others if not any(jv._norm(t) == jv._norm(lab) or jv._norm(t) in jv._norm(lab)
                                             or jv._norm(lab) in jv._norm(t) for lab in s["labels"].values())]
        if pool:
            t = pool[int(hashlib.sha1(s["id"].encode()).hexdigest(), 16) % len(pool)]
            steps.append({**s, "id": cid("step", s["id"], "absent"), "case": "absent",
                          "step": {**s["step"], "target": t}, "expected": "abstain"})
        # the control removed from the screen
        gone = s["expected"]
        opts = {k: v for k, v in s["options"].items() if k != gone}
        if opts:
            steps.append({**s, "id": cid("step", s["id"], "removed"), "case": "removed",
                          "options": opts, "labels": {k: v for k, v in s["labels"].items() if k != gone},
                          "expected": "abstain"})
    expects: list[dict[str, Any]] = []
    for e in rec["expects"]:
        expects.append(dict(e))
        if e["source"] != "after" or not labels.get(e["id"], {}).get("shown", e["shown"]):
            continue
        wrong = _bump(e["expect"])
        how = "number"
        if wrong is None or shown_oracle(wrong, tuple(e["lines"])):
            pool = sorted({x["expect"] for x in rec["expects"] if x["app"].split("--")[0] != e["app"].split("--")[0]})
            pool = [x for x in pool if not shown_oracle(x, tuple(e["lines"]))]
            wrong = pool[int(hashlib.sha1(e["id"].encode()).hexdigest(), 16) % len(pool)] if pool else None
            how = "other_app"
        if wrong:
            expects.append({**e, "id": cid("expect", e["id"], how), "expect": wrong, "shown": False, "case": how})
        for kind, lines in flips(e["expect"], tuple(e["lines"])):
            expects.append({**e, "id": cid("expect", e["id"], kind), "lines": lines, "shown": False, "case": kind})
    for c in steps + expects:
        if c["id"] in labels:
            c.update({k: v for k, v in labels[c["id"]].items() if k in ("expected", "shown")})
            c["hand"] = labels[c["id"]].get("why", "hand label")
    return {"steps": steps, "expects": expects}


# ---- ask: Jev answers every case ------------------------------------------------------------------------------

def ask(out: Path, jobs: int) -> None:
    from eve_bridge import jev

    env = owner_env()
    url, key, model = jev.route_config(env)
    predict = jev.make_predict(url, key, model, timeout_s=10.0)
    cases = build_cases(out)
    path = out / "answers.json"
    answers: dict[str, Any] = json.loads(path.read_text()) if path.is_file() else {}

    def one(kind: str, c: dict[str, Any]) -> tuple[str, Any]:
        t0 = time.perf_counter()
        if kind == "step":
            step = jv.Step(**c["step"])
            state = {"about": jv.STEP_ABOUT, "screen": c["screen"], "step": step.words(),
                     "controls": list(c["options"].values())}
            questions = jv.step_questions(step, c["options"])
        else:
            state = jv.expect_state(tuple(c["lines"]), (c["expect"],))
            questions = jv.expect_questions([c["expect"]])
        try:
            r = predict(state, questions)
        except Exception as e:  # noqa: BLE001
            return c["id"], {"error": str(e)[:160]}
        ms = (time.perf_counter() - t0) * 1000.0
        return c["id"], {"answers": r.get("answers"), "ms": round(ms), "input_tokens": (r.get("usage") or {}).get(
            "input_tokens", 0)}

    todo = [("step", c) for c in cases["steps"] if c["id"] not in answers and c["options"]] + \
           [("expect", c) for c in cases["expects"] if c["id"] not in answers]
    t0 = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        for n, (k, v) in enumerate(pool.map(lambda x: one(*x), todo), 1):
            answers[k] = v
            if n % 50 == 0:
                print(f"  {n}/{len(todo)}", file=sys.stderr)
                path.write_text(json.dumps(answers))
    path.write_text(json.dumps(answers))
    errors = sum(1 for _, c in todo if "error" in answers[c["id"]])
    print(f"asked {len(todo)} in {time.perf_counter() - t0:.0f} s ({errors} errors); {len(answers)} answers kept")


# ---- fit and score --------------------------------------------------------------------------------------------

def split(names: list[str]) -> tuple[set[str], set[str]]:
    kinds = sorted({kind_of(n) for n in names})
    calib = {k for i, k in enumerate(kinds) if i % 2 == 0}
    return calib, set(kinds) - calib


def step_answer(c: dict[str, Any], a: dict[str, Any]) -> StepAnswer:
    if not a or "error" in a:
        return StepAnswer("", 0.0, 0.0, 0.0, 0.0, 0.0, error=(a or {}).get("error", "no answer"))
    return jv.read_step(a["answers"], c["options"], float(a.get("ms", 0)))


def score_steps(cases: list[dict[str, Any]], answers: list[StepAnswer], g: StepGates) -> dict[str, Any]:
    said = [g.decide(a) for a in answers]
    real = [c["expected"] != "abstain" for c in cases]
    acted = [s != jv.STEP_NONE for s in said]
    right = sum(1 for s, c in zip(said, cases, strict=True) if s != jv.STEP_NONE and s == c["expected"])
    wrong = sum(1 for s, c in zip(said, cases, strict=True) if s != jv.STEP_NONE and s != c["expected"])
    by_case: dict[str, dict[str, int]] = {}
    for s, c in zip(said, cases, strict=True):
        d = by_case.setdefault(c["case"], {"n": 0, "acted": 0, "right": 0})
        d["n"] += 1
        d["acted"] += s != jv.STEP_NONE
        d["right"] += s != jv.STEP_NONE and s == c["expected"]
    n_real, n_neg = sum(real), len(real) - sum(real)
    false_acts = sum(1 for a, r in zip(acted, real, strict=True) if a and not r)
    return {"cases": len(cases), "should_act": n_real, "should_abstain": n_neg, "acted": sum(acted), "right": right,
            "wrong": wrong, "false_acts_on_absent": false_acts,
            "precision": round(right / sum(acted), 4) if sum(acted) else None,
            "recall": round(right / n_real, 4) if n_real else None,
            "abstain_recall": round(1 - false_acts / n_neg, 4) if n_neg else None, "by_case": by_case}


def vetoes_pass(c: dict[str, Any]) -> bool:
    """jev_verify's code vetoes on a "shown": its numbers are on the screen, and its words in its order."""
    lines = tuple(c["lines"])
    return jv.numbers_shown(c["expect"], lines) and jv.words_shown(c["expect"], lines)


def fit_expect(scores: list[float], truths: list[bool], max_false: int = 0) -> float:
    """The lowest cut-off that calls at most ``max_false`` absent texts shown: the most recall at that bar."""
    for t in EXPECT_GRID:
        if sum(1 for s, y in zip(scores, truths, strict=True) if s >= t and not y) <= max_false:
            return t
    return 1.01


def score_expects(cases: list[dict[str, Any]], scores: list[float], t: float) -> dict[str, Any]:
    said = [s >= t for s in scores]
    truth = [bool(c["shown"]) for c in cases]
    tp = sum(1 for s, y in zip(said, truth, strict=True) if s and y)
    fp = sum(1 for s, y in zip(said, truth, strict=True) if s and not y)
    fn = sum(1 for s, y in zip(said, truth, strict=True) if not s and y)
    tn = sum(1 for s, y in zip(said, truth, strict=True) if not s and not y)
    by_case: dict[str, dict[str, int]] = {}
    for s, c in zip(said, cases, strict=True):
        d = by_case.setdefault(c["case"] + (":shown" if c["shown"] else ":absent"), {"n": 0, "said_shown": 0})
        d["n"] += 1
        d["said_shown"] += s
    return {"cases": len(cases), "shown": sum(truth), "absent": len(truth) - sum(truth), "tp": tp, "fp": fp, "fn": fn,
            "tn": tn, "precision": round(tp / (tp + fp), 4) if tp + fp else None,
            "recall": round(tp / (tp + fn), 4) if tp + fn else None,
            "absent_caught": round(tn / (tn + fp), 4) if tn + fp else None, "by_case": by_case}


def fit(out: Path) -> dict[str, Any]:
    cases = build_cases(out)
    answers = json.loads((out / "answers.json").read_text())
    steps = [c for c in cases["steps"] if c["options"] and c["id"] in answers and c["oracle"] != "ambiguous"
             and (c["oracle"] in ("exact", "none") or "hand" in c or c["case"] != "real")]
    skipped = [c["id"] for c in cases["steps"] if c not in steps]
    expects = [c for c in cases["expects"] if c["id"] in answers]
    calib, hold = split([c["app"] for c in steps + expects])
    report: dict[str, Any] = {"calibration_kinds": sorted(calib), "holdout_kinds": sorted(hold),
                              "unlabelled_steps_left_out": len(skipped)}
    s_cal = [c for c in steps if kind_of(c["app"]) in calib]
    s_hold = [c for c in steps if kind_of(c["app"]) in hold]
    a_cal = [step_answer(c, answers[c["id"]]) for c in s_cal]
    a_hold = [step_answer(c, answers[c["id"]]) for c in s_hold]
    exp = ["abstain" if c["expected"] == "abstain" else c["expected"] for c in s_cal]
    g = fit_step_gates(a_cal, exp, max_wrong=0)
    g = StepGates(present=g.present, p_target=g.p_target, margin=g.margin, already_done=2.0, risky_max=2.0)
    report["step_gates"] = {"present": g.present, "p_target": g.p_target, "margin": g.margin}
    report["steps_calibration"] = score_steps(s_cal, a_cal, g)
    report["steps_holdout"] = score_steps(s_hold, a_hold, g)
    # the choice alone (no gate): what the noul buys
    loose = StepGates(present=0.0, p_target=0.0, margin=0.0, already_done=2.0, risky_max=2.0)
    report["steps_holdout_choice_alone"] = score_steps(s_hold, a_hold, loose)

    def noul(c: dict[str, Any]) -> float:
        a = answers[c["id"]]
        return float(((a.get("answers") or {}).get("e1") or {}).get("noul", 0.0)) if "error" not in a else 0.0

    e_cal = [c for c in expects if kind_of(c["app"]) in calib]
    e_hold = [c for c in expects if kind_of(c["app"]) in hold]
    t = fit_expect([noul(c) for c in e_cal], [bool(c["shown"]) for c in e_cal])
    report["expect_shown"] = t
    report["expects_calibration"] = score_expects(e_cal, [noul(c) for c in e_cal], t)
    report["expects_holdout_jev_alone"] = score_expects(e_hold, [noul(c) for c in e_hold], t)

    def gated(c: dict[str, Any]) -> float:            # what jev_verify decides: Jev's noul, and code's vetoes
        return noul(c) if vetoes_pass(c) else 0.0

    report["expects_calibration"] = score_expects(e_cal, [gated(c) for c in e_cal], t)
    report["expects_holdout"] = score_expects(e_hold, [gated(c) for c in e_hold], t)
    # the code oracle (normalized substring) against the labels: what a person's reading changed
    report["expects_holdout_substring"] = score_expects(
        e_hold, [1.0 if shown_oracle(c["expect"], tuple(c["lines"])) else 0.0 for c in e_hold], 0.5)
    ms = [float(a["ms"]) for a in answers.values() if "ms" in a]
    toks = [int(a.get("input_tokens") or 0) for a in answers.values() if "ms" in a]
    ms.sort()
    report["jev"] = {"requests": len(ms), "p50_ms": round(statistics.median(ms)), "p90_ms": round(ms[int(0.9 * len(ms)) - 1]),
                     "input_tokens": sum(toks), "mean_input_tokens": round(sum(toks) / len(toks)),
                     "usd": round(sum(toks) * 0.042 / 1e6, 5)}
    report["misses_holdout"] = {
        "steps": [{"id": c["id"], "case": c["case"], "step": c["step"], "expected": c["expected"],
                   "said": g.decide(a), "p": round(a.p_target, 2), "present": round(a.present, 2),
                   "target": a.target} for c, a in zip(s_hold, a_hold, strict=True)
                  if (g.decide(a) != jv.STEP_NONE) != (c["expected"] != "abstain") or
                  (g.decide(a) != jv.STEP_NONE and g.decide(a) != c["expected"])],
        "expects": [{"id": c["id"], "case": c["case"], "expect": c["expect"], "shown": c["shown"],
                     "noul": noul(c)} for c in e_hold if (gated(c) >= t) != bool(c["shown"])]}
    (out / "report.json").write_text(json.dumps(report, indent=1, ensure_ascii=False))
    return report


def copy(out: Path, dirs: list[Path]) -> None:
    apps = sorted({p.parent for d in dirs for p in d.glob("*/journeys.json")})
    for i, src in enumerate(apps, 1):
        dst = out / "apps" / f"{src.name}--{i}"
        dst.mkdir(parents=True, exist_ok=True)
        for name in ("index.html", "journeys.json"):
            (dst / name).write_text((src / name).read_text())
    print(f"copied {len(apps)} apps with their journeys")


def score(out: Path) -> dict[str, Any]:
    cases = build_cases(out)
    answers = json.loads((out / "answers.json").read_text())
    steps = [c for c in cases["steps"] if c["options"] and c["id"] in answers and c["oracle"] != "ambiguous"
             and (c["oracle"] in ("exact", "none") or "hand" in c or c["case"] != "real")]
    expects = [c for c in cases["expects"] if c["id"] in answers]
    g, t = jv.JOURNEY_STEP_GATES, jv.EXPECT_SHOWN
    a = [step_answer(c, answers[c["id"]]) for c in steps]

    def gated(c: dict[str, Any]) -> float:
        r = answers[c["id"]]
        n = float(((r.get("answers") or {}).get("e1") or {}).get("noul", 0.0)) if "error" not in r else 0.0
        return n if vetoes_pass(c) else 0.0

    ms = sorted(float(x["ms"]) for x in answers.values() if "ms" in x)
    toks = [int(x.get("input_tokens") or 0) for x in answers.values() if "ms" in x]
    report = {"gates": {"present": g.present, "p_target": g.p_target, "margin": g.margin, "expect_shown": t},
              "apps": sorted({c["app"] for c in steps + expects}),
              "steps": score_steps(steps, a, g), "expects": score_expects(expects, [gated(c) for c in expects], t),
              "jev": {"requests": len(ms), "p50_ms": round(statistics.median(ms)) if ms else 0,
                      "p90_ms": round(ms[max(0, int(0.9 * len(ms)) - 1)]) if ms else 0, "input_tokens": sum(toks),
                      "usd": round(sum(toks) * 0.042 / 1e6, 5)},
              "misses": {"steps": [{"id": c["id"], "case": c["case"], "step": c["step"], "expected": c["expected"],
                                    "said": g.decide(x), "p": round(x.p_target, 2), "present": round(x.present, 2)}
                                   for c, x in zip(steps, a, strict=True)
                                   if (g.decide(x) != jv.STEP_NONE or c["expected"] != "abstain")
                                   and g.decide(x) != c["expected"]],
                         "expects": [{"id": c["id"], "case": c["case"], "expect": c["expect"], "shown": c["shown"],
                                      "score": gated(c)} for c in expects if (gated(c) >= t) != bool(c["shown"])]}}
    (out / "score.json").write_text(json.dumps(report, indent=1, ensure_ascii=False))
    return report


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("cmd", choices=("write", "copy", "record", "ask", "fit", "score"))
    ap.add_argument("out", type=Path)
    ap.add_argument("dirs", nargs="*", type=Path, help="write: folders of apps (<slug>/index.html)")
    ap.add_argument("--model", default="claude-opus-5-5")
    ap.add_argument("--jobs", type=int, default=8)
    args = ap.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if args.cmd == "write":
        asyncio.run(write(out, [d.resolve() for d in args.dirs], args.model))
    elif args.cmd == "copy":
        copy(out, [d.resolve() for d in args.dirs])
    elif args.cmd == "score":
        r = score(out)
        print(json.dumps({k: v for k, v in r.items() if k != "misses"}, indent=1))
    elif args.cmd == "record":
        asyncio.run(record(out))
    elif args.cmd == "ask":
        ask(out, args.jobs)
    else:
        r = fit(out)
        print(json.dumps({k: v for k, v in r.items() if not k.startswith("misses")}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
