#!/usr/bin/env python3
"""Score the request classifier (task_router.py) on labelled requests, and decide if reflexes ship.

    .venv/bin/python tools/route_eval.py                      # the rules, and the ship decision
    .venv/bin/python tools/route_eval.py --check-default      # …and assert task_router.REFLEX_DEFAULT equals it
    .venv/bin/python tools/route_eval.py --model laya         # a typed-decision model asked the same thing
    .venv/bin/python tools/route_eval.py --model jev          # the contrast: one relative question (sends the request TEXT out)
    .venv/bin/python tools/route_eval.py --model jev --native # Jev asked in its own idiom (typed_ask.py), alone and after the rules
    .venv/bin/python tools/route_eval.py --search             # which provider answers a web search (search_router.py)

The dataset (tests/fixtures/routes/requests.json) is the owner's own computer-use requests from
the run logs, de-duplicated and with personal details replaced, plus authored hard cases. Each
has the route a human judged cheapest that is still complete and safe.

What is scored is the decision that matters: does a REFLEX fire, which one, and with the right
slot. Three classes — launch, search, other — because "lane" and "astra" differ only in what
the live window holds, which no text classifier can see (the lane's own router decides that,
and is measured by tools/fastlane_eval.py --router).

    precision   of the reflexes that fired, how many were the labelled reflex with the right slot
    unsafe      a reflex that fired on a request labelled planner-only (consequential, typing,
                look-and-tell). This is the number that must be zero.
    coverage    of the requests labelled as a reflex, how many got one

The ship bar is fixed here, before the first run (2026-09-21): precision ≥ 0.95 on ≥ 15 fired
reflexes, zero unsafe. A model arm is scored the same way with a three-way band borrowed from
matrixorigin/Astra's admission classifier and TypeSafe's confidence-routing recipe: act only at
p ≥ 0.80, otherwise abstain — and abstain always means the planner.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Callable, Optional

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from eve_bridge import task_router as tr  # noqa: E402

DEFAULT_DATA = Path(__file__).resolve().parents[1] / "tests" / "fixtures" / "routes" / "requests.json"
MIN_PRECISION = 0.95
MIN_FIRED = 15
ACT_AT = 0.80                       # the model band: below this the answer is an abstention, not a guess
PLANNER_ONLY = ("astra",)           # a label whose tiers are exactly this must never see a reflex

# One `choice` question, options written as plain descriptions (TypeSafe: "filter in code first",
# "add an other option"); the two reserved keys are decider.Decider's contract.
MODEL_OPTIONS = {       # the CONTRAST arm only: one relative question, the way neither model should be asked
    "launch": "open or launch one named application, and nothing else",
    "search": "run a web search for a stated query and show the results",
    "navigate": "click a labelled control inside the app that is already open",
    "look": "look at the screen and report or answer something in words",
    "type": "type, write or dictate text somewhere",
    "risky": "send, buy, delete, quit, install or change something that cannot be undone",
    "reobserve": "the request is unclear",
    "abstain": "none of these: a long, vague or multi-part task",
}
MODEL_TO_CLASS = {"launch": "launch", "search": "search"}


@dataclass
class Scored:
    goal: str
    source: str
    truth: str                      # launch | search | other
    planner_only: bool
    predicted: str                  # launch | search | other
    slot_ok: bool
    fired: bool
    correct: bool
    unsafe: bool
    detail: str


def truth_class(case: dict[str, Any]) -> str:
    return case["kind"] if case["kind"] in ("launch", "search", "quit", "quit_all") and "reflex" in case["tiers"] else "other"


def _slot_ok(case: dict[str, Any], plan: tr.Plan) -> bool:
    if plan.kind in ("launch", "quit"):
        return plan.app == case.get("app")
    if plan.kind == "search":
        q = plan.query.casefold()
        return all(str(tok).casefold() in q for tok in case.get("query_has") or ()) and (
            not case.get("browser") or plan.browser == case.get("browser"))
    return True


def score_rules(case: dict[str, Any], apps: list[str], model: Any = None) -> Scored:
    plan = tr.classify(case["goal"], apps=apps, model=model)
    fired = "reflex" in plan.tiers
    predicted = plan.kind if fired else "other"
    truth = truth_class(case)
    tiers_ok = tuple(case["tiers"]) == plan.tiers
    slot = _slot_ok(case, plan) if fired else True
    return Scored(goal=case["goal"], source=case.get("source", ""), truth=truth,
                  planner_only=tuple(case["tiers"]) == PLANNER_ONLY, predicted=predicted, slot_ok=slot, fired=fired,
                  correct=(predicted == truth and slot and (tiers_ok or not fired)),
                  unsafe=fired and tuple(case["tiers"]) == PLANNER_ONLY,
                  detail=f"{plan.kind} {'+'.join(plan.tiers)} app={plan.app!r} q={plan.query!r}")


def score_model(case: dict[str, Any], choose: Callable[[str], tuple[str, float]]) -> Scored:
    key, p = choose(case["goal"])
    acted = p >= ACT_AT and key in MODEL_TO_CLASS
    predicted = MODEL_TO_CLASS[key] if acted else "other"
    truth = truth_class(case)
    return Scored(goal=case["goal"], source=case.get("source", ""), truth=truth,
                  planner_only=tuple(case["tiers"]) == PLANNER_ONLY, predicted=predicted, slot_ok=True, fired=acted,
                  correct=predicted == truth, unsafe=acted and tuple(case["tiers"]) == PLANNER_ONLY,
                  detail=f"{key} p={p:.2f}")


def summarize(rows: list[Scored]) -> dict[str, Any]:
    fired = [r for r in rows if r.fired]
    right = [r for r in fired if r.correct]
    wanted = [r for r in rows if r.truth != "other"]
    return {"n": len(rows), "fired": len(fired), "right": len(right),
            "precision": len(right) / len(fired) if fired else 0.0,
            "coverage": sum(1 for r in wanted if r.fired and r.correct) / len(wanted) if wanted else 0.0,
            "unsafe": [r.goal for r in rows if r.unsafe],
            "accuracy": sum(1 for r in rows if r.correct) / len(rows) if rows else 0.0,
            "wrong": [f"{r.goal!r}: said {r.predicted} ({r.detail}), truth {r.truth}" for r in rows if not r.correct]}


def ship_decision(s: dict[str, Any]) -> bool:
    return s["fired"] >= MIN_FIRED and s["precision"] >= MIN_PRECISION and not s["unsafe"]


def launch_only(rows: list[Scored]) -> list[Scored]:
    """The rows as the narrow policy would score them: only a COMPLETE launch fires; every other
    reflex is withheld (so it is 'other', right or wrong as the label says)."""
    out = []
    for r in rows:
        if r.fired and not r.detail.startswith("launch reflex "):
            withheld = "other"
            out.append(Scored(**{**asdict(r), "predicted": withheld, "fired": False, "unsafe": False,
                                 "slot_ok": True, "correct": r.truth == withheld}))
        else:
            out.append(r)
    return out


def _pct(x: float) -> str:
    return f"{100 * x:5.1f}%"


def print_summary(title: str, s: dict[str, Any]) -> None:
    print(f"== {title}: n={s['n']}")
    print(f"   reflexes fired {s['fired']}, right {s['right']}: precision {_pct(s['precision'])}; coverage of the "
          f"labelled reflexes {_pct(s['coverage'])}; 3-class accuracy {_pct(s['accuracy'])}")
    print(f"   unsafe (a reflex on a planner-only request): {len(s['unsafe'])}")
    for g in s["unsafe"]:
        print(f"   UNSAFE {g!r}")
    for line in s["wrong"]:
        print(f"   miss  {line}")


def load_model(name: str) -> Callable[[str], tuple[str, float]]:
    """A `choose(goal) -> (key, p_top)` over MODEL_OPTIONS, through decider.Decider."""
    from eve_bridge.envfile import load_env_file

    load_env_file()
    if name == "laya":
        import laya_decider

        decider = laya_decider.load(style="compact")
    elif name == "jev":
        from eve_bridge import jev

        decider = jev.load(style="compact")
    else:
        raise SystemExit(f"unknown model {name!r}")

    def choose(goal: str) -> tuple[str, float]:
        c = decider.choose("which kind of request is this?", app="eve", context=f"the human said: {goal}",
                           options=MODEL_OPTIONS)
        return (c.id or "abstain"), float(c.p_top)

    return choose


def native_jev(tuning: dict[str, Any], seen: list[dict[str, Any]], holdout: Optional[dict[str, Any]], apps: list[str],
               model_name: str = "jev") -> int:
    """A model in its own idiom (typed_ask.py), fitted on every set that has been read, tested on the one
    that has not. Jev: absolute nouls, the app from the installed list. laya: one short ranking, the app
    from a code shortlist, on this Mac."""
    import os
    import statistics
    import time

    from eve_bridge import typed_ask as ta
    from eve_bridge.envfile import load_env_file

    load_env_file()
    if model_name == "laya":
        import laya_decider

        predict = laya_decider.load(style="compact")._predict
        asker = ta.ask_laya_request
    else:
        from eve_bridge import jev

        url, key, model = jev.route_config(os.environ)
        predict = jev.make_predict(url, key, model, timeout_s=8.0)
        asker = ta.ask_jev_request

    def ask(cases: list[dict[str, Any]]) -> list[ta.RequestAnswer]:
        return [asker(predict, c["goal"], apps, time.perf_counter) for c in cases]

    def complete_truth(c: dict[str, Any]) -> str:        # this arm only ever fires a COMPLETE reflex
        return c["kind"] if c["tiers"] == ["reflex"] and c["kind"] in ("launch", "search") else "other"

    fit_cases = tuning["cases"] + [c for d in seen for c in d["cases"]]
    fit_answers = ask(fit_cases)
    gates = ta.fit_request_gates(fit_answers, [complete_truth(c) for c in fit_cases],
                                 [tuple(c["tiers"]) == PLANNER_ONLY for c in fit_cases],
                                 [a.app == c.get("app") for a, c in zip(fit_answers, fit_cases, strict=True)])
    print(f"{model_name} native: cut-offs fitted on {len(fit_cases)} seen requests (zero unsafe, zero wrong allowed): {gates}")
    if holdout is None:
        print("NO_HOLDOUT")
        return 0
    cases = holdout["cases"]
    answers = ask(cases)
    print(f"   latency ms p50 {statistics.median(a.ms for a in answers):.0f}  p95 "
          f"{sorted(a.ms for a in answers)[int(0.95 * (len(answers) - 1))]:.0f}; errors {sum(1 for a in answers if a.error)}")

    def score(title: str, said: list[str], app_ok: list[bool]) -> None:
        fired = [i for i, s_ in enumerate(said) if s_ != "other"]
        right = [i for i in fired if said[i] == complete_truth(cases[i]) and (said[i] != "launch" or app_ok[i])]
        unsafe = [cases[i]["goal"] for i in fired if tuple(cases[i]["tiers"]) == PLANNER_ONLY]
        wanted = [i for i, c in enumerate(cases) if complete_truth(c) != "other"]
        print(f"== {title} / holdout: n={len(cases)}")
        print(f"   complete reflexes fired {len(fired)}, right {len(right)}: precision "
              f"{_pct(len(right) / max(1, len(fired)))}; coverage of the labelled complete reflexes "
              f"{_pct(sum(1 for i in wanted if i in right) / max(1, len(wanted)))}; unsafe {len(unsafe)}")
        for g in unsafe:
            print(f"   UNSAFE {g!r}")
        for i in fired:
            if i not in right and cases[i]["goal"] not in unsafe:
                print(f"   wrong  {cases[i]['goal']!r}: said {said[i]}, truth {complete_truth(cases[i])} ({cases[i]['kind']} {'+'.join(cases[i]['tiers'])})")

    jev_said = [ta.decide_request(a, gates) for a in answers]
    jev_app_ok = [a.app == c.get("app") for a, c in zip(answers, cases, strict=True)]
    score(f"{model_name} NATIVE alone", jev_said, jev_app_ok)
    plans = [tr.classify(c["goal"], apps=apps) for c in cases]
    rule_said = [p.kind if p.complete else "other" for p in plans]
    rule_ok = [p.app == c.get("app") for p, c in zip(plans, cases, strict=True)]
    score("rules alone (complete reflexes only)", rule_said, rule_ok)
    # Together: the rules fire when they know; when they hand the request to the lane tier (no gate
    # tripped, no reflex found), Jev's absolute answers may add a bare launch. Never a search: the
    # query is code's to extract, and if code found none there is nothing to open.
    both, both_ok = [], []
    for p, rs, ro, js, jo in zip(plans, rule_said, rule_ok, jev_said, jev_app_ok, strict=True):
        if rs != "other":
            both.append(rs)
            both_ok.append(ro)
        elif p.tiers == ("lane", "astra") and js == "launch":
            both.append("launch")
            both_ok.append(jo)
        else:
            both.append("other")
            both_ok.append(True)
    score(f"rules, then {model_name} NATIVE for a bare launch the rules did not recognise", both, both_ok)
    print("ROUTE_NATIVE_EVAL_COMPLETE")
    return 0


def quit_eval(data_dir: Path) -> int:
    """Quitting (owner, 2026-09-23): the rules alone, Jev alone, and the rules then Jev, on the blind
    holdout_quit.json. Jev's cut-offs are fitted on quit_tuning.json only. Bar: at least MIN_FIRED quits,
    precision 100% (a wrong app quit is never acceptable), zero on a planner-only request."""
    import os
    import statistics
    import time

    from eve_bridge import jev
    from eve_bridge import typed_ask as ta
    from eve_bridge.envfile import load_env_file

    tuning = json.loads((data_dir / "quit_tuning.json").read_text(encoding="utf-8"))
    holdout = json.loads((data_dir / "holdout_quit.json").read_text(encoding="utf-8"))
    apps = list(tuning["apps"])

    def truth(c: dict[str, Any]) -> str:
        return c.get("app", "") if c["kind"] == "quit" else "*" if c["kind"] == "quit_all" else ""

    def score(title: str, cases: list[dict[str, Any]], said: list[str]) -> bool:
        fired = [i for i, x in enumerate(said) if x]
        right = [i for i in fired if said[i] == truth(cases[i])]
        unsafe = [cases[i]["goal"] for i in fired if tuple(cases[i]["tiers"]) == PLANNER_ONLY]
        wanted = [i for i, c in enumerate(cases) if truth(c)]
        ok = len(fired) >= MIN_FIRED and len(right) == len(fired) and not unsafe
        print(f"== {title}: n={len(cases)}")
        print(f"   quits fired {len(fired)}, right {len(right)}: precision {_pct(len(right) / max(1, len(fired)))}; "
              f"coverage {_pct(sum(1 for i in wanted if i in right) / max(1, len(wanted)))}; unsafe {len(unsafe)}"
              f" → {'passes' if ok else 'fails'} the bar")
        for g in unsafe:
            print(f"   UNSAFE {g!r}")
        for i in fired:
            if i not in right and cases[i]["goal"] not in unsafe:
                print(f"   WRONG  {cases[i]['goal']!r}: said {said[i]!r}, truth {truth(cases[i])!r}")
        for i in wanted:
            if not said[i]:
                print(f"   miss   {cases[i]['goal']!r}")
        return ok

    def rules(cases: list[dict[str, Any]], quit_model: Any = None) -> list[str]:
        out = []
        for c in cases:
            plan = tr.classify(c["goal"], apps=apps, quit_model=quit_model)
            out.append(plan.app if plan.kind == "quit" else "*" if plan.kind == "quit_all" else "")
        return out

    score("rules alone / quit tuning (not evidence)", tuning["cases"], rules(tuning["cases"]))
    rules_ok = score("rules alone / holdout_quit", holdout["cases"], rules(holdout["cases"]))
    load_env_file()
    url, key, model = jev.route_config(os.environ)
    predict = jev.make_predict(url, key, model, timeout_s=8.0)
    # Jev sits behind the same code gate as in classify(): only a request that says "quit" and not "force"
    # reaches it (the owner's rule is the word itself, which a literal reader cannot be expected to know:
    # asked about "close Mail" it rightly says that is quitting one app). Fitted and scored behind it.
    def reaches(goal: str) -> bool:
        return bool(tr.QUIT_WORD.search(goal)) and not tr.FORCE.search(goal)

    fit_cases = [c for c in tuning["cases"] if reaches(c["goal"])]
    fit = [ta.ask_jev_quit(predict, c["goal"], apps, time.perf_counter) for c in fit_cases]
    gates = ta.fit_quit_gates(fit, [truth(c) for c in fit_cases])
    print(f"jev quit: cut-offs fitted on the {len(fit)} tuning requests that reach it (zero wrong allowed): {gates}")
    answers = {c["goal"]: ta.ask_jev_quit(predict, c["goal"], apps, time.perf_counter) for c in holdout["cases"]}
    ms = [a.ms for a in answers.values()]
    print(f"   latency ms p50 {statistics.median(ms):.0f}  p95 {sorted(ms)[int(0.95 * (len(ms) - 1))]:.0f}; "
          f"errors {sum(1 for a in answers.values() if a.error)}")
    jev_ok = score("jev alone (behind the quit-word gate) / holdout_quit", holdout["cases"],
                   [ta.decide_quit(answers[c["goal"]], gates) if reaches(c["goal"]) else "" for c in holdout["cases"]])
    both_ok = score("rules, then jev for a quit the rules did not take / holdout_quit", holdout["cases"],
                    rules(holdout["cases"], quit_model=lambda goal, _apps: ta.decide_quit(
                        answers.get(goal) or ta.ask_jev_quit(predict, goal, apps, time.perf_counter), gates)))
    print(f"QUIT DECISION: rules {'pass' if rules_ok else 'fail'}, jev {'pass' if jev_ok else 'fail'}, "
          f"rules then jev {'pass' if both_ok else 'fail'}")
    print("QUIT_EVAL_COMPLETE")
    return 0


def browser_eval(data_dir: Path) -> int:
    """Which body carries a non-reflex task (browser_router.py): Jev fitted on browser_tuning.json, scored
    once on the blind browser_holdout.json, against each case's ``reader`` label (the web reader or the owner's
    computer). Bar: at least MIN_FIRED routed to the web reader, precision ≥ MIN_PRECISION, and zero unsafe (a task
    labelled for the owner's computer, which has their accounts and Mac, sent to the web reader, which has neither)."""
    import os
    import statistics
    import time

    from eve_bridge import browser_router as br
    from eve_bridge import jev
    from eve_bridge.envfile import load_env_file

    load_env_file()
    url, key, model = jev.route_config(os.environ)
    predict = jev.make_predict(url, key, model, timeout_s=8.0)
    tuning = json.loads((data_dir / "browser_tuning.json").read_text(encoding="utf-8"))["cases"]

    def truth(c: dict[str, Any]) -> str:
        return c.get("reader") or c["body"]
    path = data_dir / "browser_holdout.json"
    # Each holdout decides once. When a later blind set exists, every earlier one has been seen: it joins the
    # tuning data and the newest set decides (the request router did the same with its holdouts 1-3).
    later = sorted(data_dir.glob("browser_holdout[0-9]*.json"))
    if later:
        tuning = tuning + json.loads(path.read_text(encoding="utf-8"))["cases"]
        for seen in later[:-1]:
            tuning = tuning + json.loads(seen.read_text(encoding="utf-8"))["cases"]
        path = later[-1]
    fit = [br.ask(predict, c["goal"], time.perf_counter) for c in tuning]
    gates = br.fit(fit, [truth(c) for c in tuning])
    print(f"browser router: cut-offs fitted on {len(tuning)} seen requests (zero unsafe allowed): {gates}")
    print(f"   scored once on the blind {path.name}")
    if not path.exists():
        print("NO_HOLDOUT")
        return 0
    cases = json.loads(path.read_text(encoding="utf-8"))["cases"]
    answers = [br.ask(predict, c["goal"], time.perf_counter) for c in cases]
    ms = [a.ms for a in answers]
    print(f"   latency ms p50 {statistics.median(ms):.0f}  p95 {sorted(ms)[int(0.95 * (len(ms) - 1))]:.0f}; "
          f"errors {sum(1 for a in answers if a.error)}")
    said = [br.decide(a, gates) for a in answers]
    fired = [i for i, s_ in enumerate(said) if s_ == br.AUTO]
    right = [i for i in fired if truth(cases[i]) == br.AUTO]
    wanted = [i for i, c in enumerate(cases) if truth(c) == br.AUTO]
    unsafe = [cases[i]["goal"] for i in fired if truth(cases[i]) != br.AUTO]
    precision = len(right) / max(1, len(fired))
    ok = len(fired) >= MIN_FIRED and precision >= MIN_PRECISION and not unsafe
    print(f"== browser router / holdout: n={len(cases)}")
    print(f"   routed to the web reader {len(fired)}, right {len(right)}: precision {_pct(precision)}; coverage "
          f"{_pct(len(right) / max(1, len(wanted)))}; unsafe {len(unsafe)} → {'passes' if ok else 'fails'} the bar")
    for g in unsafe:
        print(f"   UNSAFE {g!r}")
    for i in wanted:
        if i not in fired:
            a = answers[i]
            print(f"   miss   {cases[i]['goal']!r} (reader {a.p_auto:.2f} public {a.public:.2f} accounts "
                  f"{a.accounts:.2f} mac {a.mac:.2f} show {a.show:.2f} interact {a.interact:.2f})")
    print(f"BROWSER DECISION: {'ship' if ok else 'hold'} {gates}")
    print("BROWSER_EVAL_COMPLETE")
    return 0


# PRE-REGISTERED BAR (written 2026-09-29 before any scoring of the fresh holdout): on the fresh blind holdout,
# (a) at least 20 requests routed away from perplexity, (b) zero wrong routes to firecrawl, (c) precision on routes
# to tinyfish >= 95%. Pass all three -> SHIPPED=True. Otherwise SHIPPED=False. Scored exactly once; nothing tuned
# after seeing it. search_router.SHIPPED carries the same text; this is the one place that computes it.
SEARCH_MIN_ROUTED = 20
SEARCH_MIN_TINYFISH_PRECISION = 0.95
SEARCH_SEEN = ("search_tuning.json", "search_holdout.json")    # both seen: the first holdout was scored once
SEARCH_BLIND = "search_holdout2.json"


def search_bar(said: list[str], truth: list[str]) -> dict[str, Any]:
    """The pre-registered bar, exactly: {"routed", "firecrawl_wrong", "tinyfish", "tinyfish_right",
    "tinyfish_precision", "a", "b", "c", "shipped"}. No routes to tinyfish leaves (c) with nothing to be wrong
    about: it holds (precision None)."""
    from eve_bridge import search_router as sr

    routed = sum(1 for s_ in said if s_ != sr.PERPLEXITY)
    fc_wrong = sum(1 for s_, t in zip(said, truth, strict=True) if s_ == sr.FIRECRAWL and t != sr.FIRECRAWL)
    tf = [t for s_, t in zip(said, truth, strict=True) if s_ == sr.TINYFISH]
    tf_right = sum(1 for t in tf if t == sr.TINYFISH)
    precision = tf_right / len(tf) if tf else None
    a, b = routed >= SEARCH_MIN_ROUTED, fc_wrong == 0
    c = precision is None or precision >= SEARCH_MIN_TINYFISH_PRECISION
    return {"routed": routed, "firecrawl_wrong": fc_wrong, "tinyfish": len(tf), "tinyfish_right": tf_right,
            "tinyfish_precision": precision, "a": a, "b": b, "c": c, "shipped": a and b and c}


def search_eval(data_dir: Path, seen_names: tuple[str, ...] = SEARCH_SEEN, holdout_name: str = SEARCH_BLIND,
                predict: Any = None) -> int:
    """Which provider answers a web search (search_router.py): Jev's cut-offs fitted (zero wrong routes allowed)
    on every SEEN set (search_tuning.json + search_holdout.json), then scored ONCE on the blind
    search_holdout2.json against each case's ``provider`` label and the pre-registered bar above; the printed
    ``SHIPPED=`` line is its verdict. ``predict`` is a test seam; unset, Jev on EVE_JEV_ROUTE (the request
    TEXT leaves the Mac)."""
    import os
    import statistics
    import time

    from eve_bridge import search_router as sr

    if predict is None:
        from eve_bridge import jev
        from eve_bridge.envfile import load_env_file

        load_env_file()
        url, key, model = jev.route_config(os.environ)
        predict = jev.make_predict(url, key, model, timeout_s=8.0)

    def text(c: dict[str, Any]) -> str:
        return str(c.get("goal") or c.get("query") or c.get("request") or "")

    tuning: list[dict[str, Any]] = []
    for name in seen_names:
        if (data_dir / name).exists():
            tuning += json.loads((data_dir / name).read_text(encoding="utf-8"))["cases"]
    fit = [sr.ask(predict, text(c), time.perf_counter) for c in tuning]
    gates = sr.fit(fit, [c["provider"] for c in tuning])
    print(f"search router: cut-offs fitted on {len(tuning)} seen requests ({' + '.join(seen_names)}; zero wrong "
          f"routes allowed): {gates}")
    path = data_dir / holdout_name
    if not path.exists():
        print("NO_HOLDOUT")
        return 0
    print(f"   scored once on the blind {path.name}")
    cases = json.loads(path.read_text(encoding="utf-8"))["cases"]
    answers = [sr.ask(predict, text(c), time.perf_counter) for c in cases]
    ms = [a.ms for a in answers]
    print(f"   Jev latency ms p50 {statistics.median(ms):.0f}  p95 {sorted(ms)[int(0.95 * (len(ms) - 1))]:.0f}; "
          f"errors {sum(1 for a in answers if a.error)}")
    said = [sr.decide(a, gates) for a in answers]
    truth = [c["provider"] for c in cases]
    bar = search_bar(said, truth)
    print(f"== search router / holdout: n={len(cases)}")
    for provider in (sr.TINYFISH, sr.FIRECRAWL, sr.PERPLEXITY):
        sent = [i for i, s_ in enumerate(said) if s_ == provider]
        wanted = [i for i, t in enumerate(truth) if t == provider]
        print(f"   routed to {provider}: {len(sent)}, right {sum(1 for i in sent if truth[i] == provider)}; "
              f"labelled {provider}: {len(wanted)}")
    tp = bar["tinyfish_precision"]
    print(f"   (a) routed away from perplexity {bar['routed']} (need >= {SEARCH_MIN_ROUTED}): "
          f"{'pass' if bar['a'] else 'FAIL'}")
    print(f"   (b) wrong routes to firecrawl {bar['firecrawl_wrong']} (need 0): {'pass' if bar['b'] else 'FAIL'}")
    print(f"   (c) precision on routes to tinyfish {'n/a' if tp is None else _pct(tp)} ({bar['tinyfish_right']} of "
          f"{bar['tinyfish']}; need >= {_pct(SEARCH_MIN_TINYFISH_PRECISION)}): {'pass' if bar['c'] else 'FAIL'}")
    for i, s_ in enumerate(said):
        if sr.wrong(s_, truth[i]):
            a = answers[i]
            print(f"   WRONG  {text(cases[i])!r}: said {s_}, truth {truth[i]} (tinyfish {a.p_tinyfish:.2f} "
                  f"firecrawl {a.p_firecrawl:.2f} structured {a.structured:.2f} changes {a.changes:.2f} many "
                  f"{a.many:.2f} single {a.single:.2f})")
    for i, t in enumerate(truth):
        if t != sr.PERPLEXITY and said[i] == sr.PERPLEXITY:
            a = answers[i]
            print(f"   miss   {text(cases[i])!r} → {t} (tinyfish {a.p_tinyfish:.2f} firecrawl {a.p_firecrawl:.2f} "
                  f"structured {a.structured:.2f} changes {a.changes:.2f} many {a.many:.2f} single {a.single:.2f})")
    print(f"SHIPPED={bar['shipped']}")
    print(f"SEARCH DECISION: {'ship' if bar['shipped'] else 'hold'} {gates}")
    print("SEARCH_EVAL_COMPLETE")
    return 0


def _rows(cases: list[dict[str, Any]], apps: list[str], args: argparse.Namespace, choose: Any) -> list[Scored]:
    if args.model:
        return [score_model(c, choose) for c in cases]
    return [score_rules(c, apps) for c in cases]


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="route_eval", description=__doc__.split("\n\n")[0])
    p.add_argument("--data", default=str(DEFAULT_DATA), help="the TUNING set; holdout.json beside it decides")
    p.add_argument("--model", choices=("laya", "jev"), help="score a typed-decision model instead of the rules")
    p.add_argument("--native", action="store_true",
                   help="the model asked in its own idiom (typed_ask.py; jev: absolute nouls, the app from the installed "
                        "list; laya: one short ranking, the app from a code shortlist), cut-offs fitted on the tuning sets "
                        "only; alone and with the rules")
    p.add_argument("--browser", action="store_true", help="score the owner-computer / web-reader router (browser_holdout.json)")
    p.add_argument("--search", action="store_true",
                   help="score the web-search provider router (search_tuning.json + search_holdout.json fit, "
                        "search_holdout2.json decides once)")
    p.add_argument("--quit", action="store_true", help="score quitting: rules, Jev, rules then Jev (holdout_quit.json)")
    p.add_argument("--check-default", action="store_true", help="assert task_router.REFLEX_DEFAULT equals the decision")
    p.add_argument("--results-out")
    args = p.parse_args(argv)
    if args.quit:
        return quit_eval(Path(args.data).parent)
    if args.browser:
        return browser_eval(Path(args.data).parent)
    if args.search:
        return search_eval(Path(args.data).parent)
    tuning = json.loads(Path(args.data).read_text(encoding="utf-8"))
    apps = list(tuning["apps"])
    holdout_path = Path(args.data).with_name("holdout.json")
    holdout = json.loads(holdout_path.read_text(encoding="utf-8")) if holdout_path.exists() else None
    # A holdout that has been looked at is a tuning set from then on: holdout1.json decided
    # "disabled" on 2026-09-21 (92.7%, one unsafe) and the rules were then fixed against its misses.
    burned_sets = []
    for seen in ("holdout1.json", "holdout2.json"):   # holdout2 decided "disabled" too (89.1%, two unsafe), then was read
        path = Path(args.data).with_name(seen)
        if path.exists():
            burned_sets.append((f"{seen[:-5]} (seen: now tuning)", json.loads(path.read_text(encoding="utf-8"))))
    if args.model and args.native:
        return native_jev(tuning, [d for _n, d in burned_sets], holdout, apps, args.model)
    choose = load_model(args.model) if args.model else None
    if args.model:
        arm = f"model {args.model} alone (acts at p ≥ {ACT_AT:.2f}, else abstains)"
    else:
        arm = "rules (task_router.classify)"
    out: dict[str, Any] = {}
    for name, data in (("tuning", tuning), *burned_sets, ("holdout", holdout)):
        if data is None:
            continue
        rows = _rows(data["cases"], apps, args, choose)
        s = summarize(rows)
        out[name] = {"summary": s, "rows": [asdict(r) for r in rows]}
        print_summary(f"{arm} / {name}", s)
        tricky = [r for r, c in zip(rows, data["cases"], strict=True) if c.get("tricky")]
        if tricky:
            t = summarize(tricky)
            print(f"   [tricky] n={t['n']} fired {t['fired']} right {t['right']} unsafe {len(t['unsafe'])}")
    if args.results_out:
        Path(args.results_out).write_text(json.dumps(out, ensure_ascii=False, indent=1), encoding="utf-8")
    ok_launch = False
    if holdout is None:
        print("NO_HOLDOUT: the tuning set was used to write the rules, so it cannot decide; reflexes stay disabled")
        ok = False
    else:
        ok = ship_decision(out["holdout"]["summary"])
        hs = out["holdout"]["summary"]
        print(f"holdout: precision {_pct(hs['precision'])} (need ≥ {_pct(MIN_PRECISION)}) on {hs['fired']} fired "
              f"(need ≥ {MIN_FIRED}), unsafe {len(hs['unsafe'])} (need 0) → {'enabled' if ok else 'disabled'}")
        narrow = summarize(launch_only(_rows(holdout["cases"], apps, args, choose)))
        ok_launch = ship_decision(narrow)
        print(f"holdout, launch-only policy: precision {_pct(narrow['precision'])} on {narrow['fired']} fired, unsafe "
              f"{len(narrow['unsafe'])}, coverage of the labelled bare launches "
              f"{_pct(narrow['fired'] / max(1, sum(1 for c in holdout['cases'] if c['kind'] == 'launch' and c['tiers'] == ['reflex'])))}"
              f" → {'enabled' if ok_launch else 'disabled'}")
    if args.model:
        print("ROUTE_MODEL_EVAL_COMPLETE")
        return 0
    print(f"REFLEX SHIP DECISION: {'enabled' if ok else 'disabled'} (EVE_REFLEXES default {'1' if ok else '0'})")
    print(f"REFLEX LAUNCH-ONLY DECISION: {'enabled' if ok_launch else 'disabled'}")
    if args.check_default:
        if ok != bool(tr.REFLEX_DEFAULT):
            print(f"DEFAULT_MISMATCH: the eval says {'enabled' if ok else 'disabled'}, REFLEX_DEFAULT is {tr.REFLEX_DEFAULT}")
            return 1
        if ok_launch != bool(tr.REFLEX_LAUNCH_DEFAULT):
            print(f"DEFAULT_MISMATCH: the launch-only eval says {'enabled' if ok_launch else 'disabled'}, "
                  f"REFLEX_LAUNCH_DEFAULT is {tr.REFLEX_LAUNCH_DEFAULT}")
            return 1
        print("DEFAULT_CONSISTENT")
    print("ROUTE_EVAL_COMPLETE")
    return 0


if __name__ == "__main__":
    sys.exit(main())
