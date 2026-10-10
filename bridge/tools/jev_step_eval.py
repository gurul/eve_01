#!/usr/bin/env python3
"""Jev on the click path: which control does a step press, scored on the Accessibility fixtures.

    .venv/bin/python tools/jev_step_eval.py --fixtures tests/fixtures/ax                # asks Jev (sends labels out)
    .venv/bin/python tools/jev_step_eval.py --fixtures tests/fixtures/ax --answers a.json   # …and keeps the answers
    .venv/bin/python tools/jev_step_eval.py --fixtures tests/fixtures/ax --replay a.json    # rescore, no network
    .venv/bin/python tools/jev_step_eval.py --fixtures tests/fixtures/ax --check-default    # the ship decision

tools/fastlane_eval.py measured laya (the lane's local model until 2026-09-24) and the keyword gate on
these fixtures and never Jev, so until this tool there was no number anywhere for Jev picking a native control. It asks typed_ask.ask_jev_step — the
target choice over the lane's own menu plus an explicit "none", and the absolute nouls beside it, in one
request — for every case, fits the cut-offs on `select` with zero wrong presses allowed, and scores
`holdout` three ways: the keyword gate alone (today's click path), Jev alone, and the gate first with Jev
on what the gate leaves. A wrong press is a control pressed that is not the expected one, including any
press on a case that expects none.

Both sets have been read (the laya work tuned on `select` and read `holdout`), so the holdout numbers here
are a tuning-set number by this project's own rule. `--check-default` therefore needs `--fresh DIR`, a set
nobody has read, before it lets typed_ask.JEV_STEP_DEFAULT be True.

What leaves the Mac: the step's words, the app's name, the focused control and the menu's control labels, to
the route in EVE_JEV_ROUTE — never the window title (fast_lane.jev_context). The fixtures are redacted
captures, so an eval run sends no personal label.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
from dataclasses import asdict
from pathlib import Path
from typing import Any, Callable, Optional

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import fastlane_eval as fe  # noqa: E402

from eve_bridge import typed_ask as ta  # noqa: E402
from eve_bridge.decider import RESERVED  # noqa: E402
from eve_bridge.fast_lane import jev_context  # noqa: E402

# The bar, fixed before the first run (2026-09-21). On an unread set: the gate-then-Jev path must press
# nothing wrong, must decide at least 10 points more of the cases than the gate alone, and Jev's own
# presses must be at least 90% right on at least 10 of them. A request must stay under a second at p90.
BAR_WRONG = 0
BAR_COVERAGE_GAIN = 0.10
BAR_JEV_PRECISION = 0.90
BAR_JEV_MIN_PRESSES = 10
BAR_P90_MS = 1000.0
DEFAULT_MENU = 25                # select, 2026-09-21: the right control is on a 25-menu in 60 of 69 cases, a 10-menu in 58;
                                 # Jev's top-1 on the offered ones went 52/58 -> 57/60, and the request stayed at ~235 ms


def case_key(fixture: fe.Fixture, case: dict[str, Any], menu: int, grouped: bool = False) -> str:
    return f"{fixture.set_name}/{fixture.name}/{case['goal']}/m{menu}" + ("g" if grouped else "")


def expected_of(case: dict[str, Any]) -> str:
    return "abstain" if case.get("expected") == "abstain" else str(case.get("expected_id"))


def ask_case(predict: ta.Predict, fixture: fe.Fixture, case: dict[str, Any], menu: int,
             grouped: bool = False) -> dict[str, Any]:
    """One case: the lane's menu, the gate's pick on it, and Jev's answer. JSON-ready. `grouped` asks
    typed_ask.ask_jev_step_grouped (a menu past 25 in groups, then a final), as EVE_JEV_GROUPS=1 does."""
    goal = str(case["goal"])
    options, by_id, _dropped = fe.build_menu(fixture.snapshot, goal, max_out=menu)
    real = {k: v for k, v in options.items() if k not in RESERVED}
    gate = fe.gate_pick(goal, options, by_id) if by_id else ""
    if real:
        ask = ta.ask_jev_step_grouped if grouped else ta.ask_jev_step
        a = ask(predict, goal, app=fixture.snapshot.app, context=jev_context(fixture.snapshot),
                options=real, clock=time.perf_counter)
    else:
        a = ta.StepAnswer("", 0.0, 0.0, 0.0, 0.0, 0.0, error="no candidates offered")
    return {"set": fixture.set_name, "fixture": fixture.name, "app": fixture.app, "goal": goal,
            "expected": expected_of(case), "overlap": bool(case.get("overlap", True)),
            "distractor": bool(case.get("distractor")), "offered": expected_of(case) in real,
            "gate": gate, "answer": asdict(a)}


def collect(predict: Optional[ta.Predict], sets: dict[str, list[fe.Fixture]], menu: int,
            cached: dict[str, Any], progress: Callable[[str], None] = lambda _s: None,
            grouped: bool = False) -> dict[str, Any]:
    rows: dict[str, Any] = {}
    for fixtures in sets.values():
        for f in fixtures:
            for c in f.cases:
                key = case_key(f, c, menu, grouped)
                if key in cached:
                    rows[key] = cached[key]
                    continue
                if predict is None:
                    raise SystemExit(f"--replay has no answer for {key}")
                rows[key] = ask_case(predict, f, c, menu, grouped)
                progress(key)
    return rows


def answer_of(row: dict[str, Any]) -> ta.StepAnswer:
    return ta.StepAnswer(**row["answer"])


def said(row: dict[str, Any], gates: ta.StepGates, path: str) -> str:
    """The control a path presses on this case, or "none". Paths: gate | jev | both | veto.
    `veto` presses the gate's pick only when Jev's own top control is the same one, and otherwise
    leaves the case to Jev's gated answer: the gate proposes, Jev can refuse."""
    if path in ("gate", "both") and row["gate"]:
        return str(row["gate"])
    if path == "gate":
        return ta.STEP_NONE
    if path == "veto" and row["gate"] and row["answer"]["target"] == row["gate"]:
        return str(row["gate"])
    pick = gates.decide(answer_of(row))
    return pick if pick not in ("confirm", "done") else ta.STEP_NONE


def score(rows: list[dict[str, Any]], gates: ta.StepGates, path: str) -> dict[str, Any]:
    n = len(rows)
    clicks = [r for r in rows if r["expected"] != "abstain"]
    picks = [(r, said(r, gates, path)) for r in rows]
    pressed = [(r, p) for r, p in picks if p != ta.STEP_NONE]
    right = [(r, p) for r, p in pressed if p == r["expected"]]
    wrong = [(r, p) for r, p in pressed if p != r["expected"]]
    jev_pressed = [(r, p) for r, p in pressed if not (path == "both" and r["gate"])
                   and not (path == "veto" and p == r["gate"])] if path != "gate" else []
    jev_right = [1 for r, p in jev_pressed if p == r["expected"]]
    return {"n": n, "click_cases": len(clicks), "pressed": len(pressed), "right": len(right), "wrong": len(wrong),
            "precision": len(right) / len(pressed) if pressed else 0.0,
            "coverage": len(right) / len(clicks) if clicks else 0.0,
            "jev_pressed": len(jev_pressed), "jev_right": len(jev_right),
            "jev_precision": len(jev_right) / len(jev_pressed) if jev_pressed else 0.0,
            "wrong_cases": [f"{r['fixture']}: {r['goal']!r} pressed {p} expected {r['expected']}" for r, p in wrong]}


def latency(rows: list[dict[str, Any]]) -> dict[str, float]:
    ms = sorted(r["answer"]["ms"] for r in rows if r["answer"]["ms"] > 0 and not r["answer"]["error"])
    if not ms:
        return {"n": 0, "p50": 0.0, "p90": 0.0, "max": 0.0}
    return {"n": len(ms), "p50": statistics.median(ms), "p90": ms[min(len(ms) - 1, int(0.9 * len(ms)))], "max": ms[-1]}


def ship_decision(fresh: list[dict[str, Any]], gates: ta.StepGates) -> tuple[bool, list[str]]:
    """The bar above, on a set nobody has read. No fresh set means no: a read set cannot decide."""
    if not fresh:
        return False, ["no unread set was given (--fresh DIR): the fixtures in tests/fixtures/ax have been read"]
    gate, both, lat = score(fresh, gates, "gate"), score(fresh, gates, "both"), latency(fresh)
    why = []
    if both["wrong"] > BAR_WRONG:
        why.append(f"{both['wrong']} wrong presses (bar {BAR_WRONG})")
    if both["coverage"] - gate["coverage"] < BAR_COVERAGE_GAIN:
        why.append(f"coverage gain {both['coverage'] - gate['coverage']:+.1%} (bar +{BAR_COVERAGE_GAIN:.0%})")
    if both["jev_pressed"] < BAR_JEV_MIN_PRESSES or both["jev_precision"] < BAR_JEV_PRECISION:
        why.append(f"jev pressed {both['jev_pressed']} at {both['jev_precision']:.1%} "
                   f"(bar ≥{BAR_JEV_MIN_PRESSES} at ≥{BAR_JEV_PRECISION:.0%})")
    if lat["p90"] > BAR_P90_MS:
        why.append(f"p90 {lat['p90']:.0f} ms (bar {BAR_P90_MS:.0f})")
    return not why, why


def print_score(title: str, s: dict[str, Any]) -> None:
    print(f"  {title:<34} pressed {s['pressed']:>3}  right {s['right']:>3}  wrong {s['wrong']:>2}  "
          f"precision {s['precision']:6.1%}  coverage {s['coverage']:6.1%}"
          + (f"  (jev's own: {s['jev_right']}/{s['jev_pressed']})" if s["jev_pressed"] else ""))
    for line in s["wrong_cases"]:
        print(f"      wrong: {line}")


def make_predict() -> ta.Predict:
    import os

    from eve_bridge import jev
    from eve_bridge.envfile import load_env_file

    load_env_file()
    url, key, model = jev.route_config(os.environ)
    print(f"asking {model} at {url.split('/')[2]}")
    return jev.make_predict(url, key, model, timeout_s=8.0)


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--fixtures", required=True, metavar="DIR", help="DIR/select and DIR/holdout")
    p.add_argument("--fresh", metavar="DIR", help="a directory of fixture files nobody has read: the ship decision's set")
    p.add_argument("--menu", type=int, default=DEFAULT_MENU, help="real options offered per step")
    p.add_argument("--grouped", action="store_true",
                   help="ask in groups of 25 then a final (typed_ask.ask_jev_step_grouped); use with --menu 100")
    p.add_argument("--answers", metavar="FILE", help="write every answer here (and reuse the ones already in it)")
    p.add_argument("--replay", metavar="FILE", help="score the answers in FILE; nothing is sent")
    p.add_argument("--check-default", action="store_true", help="assert typed_ask.JEV_STEP_DEFAULT equals the decision")
    args = p.parse_args(argv)

    sets = fe.load_sets(Path(args.fixtures))
    if args.fresh:
        sets["fresh"] = [fe.load_fixture(path, "fresh") for path in sorted(Path(args.fresh).glob("*.json"))]
    store = Path(args.replay or args.answers) if (args.replay or args.answers) else None
    cached = json.loads(store.read_text(encoding="utf-8")) if store and store.is_file() else {}
    predict = None if args.replay else make_predict()
    rows = collect(predict, sets, args.menu, cached, progress=lambda k: print(f"  asked {k}", flush=True),
                   grouped=args.grouped)
    if args.answers:
        Path(args.answers).write_text(json.dumps(rows, indent=1), encoding="utf-8")

    by_set = {name: [r for r in rows.values() if r["set"] == name] for name in sets}
    errors = [r for r in rows.values() if r["answer"]["error"] and r["answer"]["error"] != "no candidates offered"]
    fit_rows = by_set["select"]
    gates = ta.fit_step_gates([answer_of(r) for r in fit_rows], [r["expected"] for r in fit_rows])
    print(f"\ncut-offs fitted on select ({len(fit_rows)} cases, zero wrong presses allowed): {gates}")
    print(f"shipped cut-offs: {ta.JEV_STEP_GATES}")
    for name, group in by_set.items():
        if not group:
            continue
        note = {"select": "fitted on", "holdout": "read before: a tuning-set number", "fresh": "unread"}[name]
        print(f"\n{name} — {len(group)} cases ({note}), menu {args.menu}")
        for path, title in (("gate", "keyword gate alone (today)"), ("jev", "jev alone"),
                            ("both", "gate, then jev on the rest"), ("veto", "gate only if jev agrees, else jev")):
            print_score(title, score(group, gates, path))
        lat = latency(group)
        print(f"  jev request: p50 {lat['p50']:.0f} ms  p90 {lat['p90']:.0f} ms  max {lat['max']:.0f} ms  (n={lat['n']})")
    if errors:
        print(f"\n{len(errors)} requests failed, e.g. {errors[0]['answer']['error']}")

    ship, why = ship_decision(by_set.get("fresh", []), gates)
    print(f"\nship decision: {'ENABLE' if ship else 'keep off'}" + ("" if ship else " — " + "; ".join(why)))
    if args.check_default:
        if ta.JEV_STEP_DEFAULT != ship:
            print(f"CHECK FAILED: typed_ask.JEV_STEP_DEFAULT is {ta.JEV_STEP_DEFAULT}, the decision is {ship}")
            return 1
        print("JEV_STEP_DEFAULT_OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
