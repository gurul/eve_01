#!/usr/bin/env python3
"""Plan once, execute with Jev — on this Mac, from a terminal.

    .venv/bin/python tools/plan_live.py "put the calendar on the year view"          # DRY RUN: touches nothing
    .venv/bin/python tools/plan_live.py "put the calendar on the year view" --act    # really does it
    .venv/bin/python tools/plan_live.py --plan plan.json "…" --act                   # a plan you wrote, no planner call

What it does is what computer_agent._plan_once does with EVE_PLAN_EXEC=1, without the daemon: read the
front window's outline, ask the planner ONCE for a typed plan (plan_contract.py), print it, and walk it with
plan_executor.py — every click grounded on a fresh snapshot by the keyword gate and Jev. A dry run grounds each
click and reports the control it WOULD press; it opens no app, types nothing and presses nothing, so steps
after the first screen change are grounded against the screen as it is now.

A step that needs the human's yes stops the run and is printed; --yes answers yes to every such question and
is for a desk you are watching.

What leaves the Mac: the request, the installed apps' names and the front window's control labels to the
planner (an OpenAI model, through OpenRouter), and per grounded step the step's words and that window's control labels to Jev
(EVE_JEV_ROUTE) — never the window title, a field's contents or a screenshot.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("request")
    p.add_argument("--act", action="store_true", help="really run the plan (default: dry run)")
    p.add_argument("--yes", action="store_true", help="with --act: answer yes to every confirm")
    p.add_argument("--plan", metavar="FILE", help="run this plan JSON instead of asking the planner")
    p.add_argument("--keyword-only", action="store_true", help="no Jev: the keyword gate decides alone (the A/B's third arm)")
    args = p.parse_args(argv)

    from eve_bridge import plan_contract as pc
    from eve_bridge import task_router
    from eve_bridge.computer_agent import configured
    from eve_bridge.envfile import load_env_file

    load_env_file()
    import pyautogui

    from eve_bridge.desktop_helpers import Helpers
    from eve_bridge.desktop_worker import start_jev_asker

    helpers = Helpers(pyautogui)
    helpers.bind(lambda *values: print("   ", *values), lambda _image: None)
    helpers.begin()
    if not args.keyword_only:
        status = start_jev_asker(helpers, {**os.environ, "EVE_PLAN_EXEC": "1"})
        print(f"jev: {status}")
    cfg = configured()
    t0 = time.perf_counter()
    if args.plan:
        raw = json.loads(Path(args.plan).read_text(encoding="utf-8"))
    else:
        outline = helpers.outline()
        named = task_router.find_app_mention(args.request, task_router.installed_apps())
        if args.act and named and named.casefold() != outline["app"].casefold():
            helpers.open_app(named)                          # as _plan_once does: plan against the right window
            outline = helpers.outline()
        print(f"outline: {outline['app']}, {len(outline['lines'])} controls ({time.perf_counter() - t0:.2f} s)")
        from eve_bridge import openrouter

        t1 = time.perf_counter()
        response = openrouter.client().responses.create(**pc.plan_request(
            cfg.model, args.request, app=outline["app"], outline=outline["lines"],
            apps=task_router.installed_apps(), effort=cfg.plan_exec_effort, timeout=cfg.api_timeout_secs))
        raw = json.loads(response.output_text)
        print(f"planner: {cfg.model}, one call, {time.perf_counter() - t1:.2f} s")
    plan = pc.parse_plan(raw, args.request)
    if plan.needs_eyes:
        print(f"the planner declined to plan: {'; '.join(plan.reasons)}")
        return 0
    for i, step in enumerate(plan.steps, start=1):
        flags = (" [consequential]" if step.consequential else "") + (f" [hint: {step.label_hint}]" if step.label_hint else "")
        print(f"  {i}. {step.describe()}{flags}")
    print(f'  then say: "{plan.final_say}"')

    plan_dict, start, approved = {**pc.plan_to_dict(plan), "app": "" if args.plan else outline["app"]}, 0, {}
    while True:
        t2 = time.perf_counter()
        result = helpers.run_plan(plan_dict, args.request, start=start, approved=approved, dry_run=not args.act)
        for e in result["ledger"]:
            print(f"  step {e['index']}: {e['effect']:<15} {e['how']:<12} {e['step']}   | {e['note'][:110]}")
        print(f"executor: {result['status']} in {time.perf_counter() - t2:.2f} s"
              + (f" — {result['reason']}" if result.get("reason") else ""))
        if result["status"] != "needs_human":
            break
        print(f"NEEDS THE HUMAN: {result['confirm']}")
        if not (args.act and args.yes):
            break
        start = int(result["next_index"])
        approved[str(start)] = result["confirm"]
    if result["status"] == "complete" and result.get("sentence"):
        print(f'eve says: "{result["sentence"]}"')
    print(f"total {time.perf_counter() - t0:.2f} s; planner calls: {0 if args.plan else 1}")
    return 0 if result["status"] in ("complete", "needs_human") else 1


if __name__ == "__main__":
    raise SystemExit(main())
