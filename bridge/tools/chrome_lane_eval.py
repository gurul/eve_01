#!/usr/bin/env python3
"""Head-to-head on the owner's REAL, logged-in Chrome: the Chrome lane (chrome_lane.py) vs Codex browser use.

    .venv/bin/python tools/chrome_lane_eval.py [--arms lane,codex] [--tasks 1,2,3] [--out results.json]

Each task runs through each arm, one after the other, and the wall time, the answer and whether the arm
finished on its own are recorded. The lane arm is measured ALONE (no Codex fallback), so its number is
the lane's. The tasks are read-only by construction, and every question either agent asks the owner is
answered "no" by this tool, so nothing consequential can happen. The lane uses ONE connection for all
tasks: Chrome asks the owner "Allow remote debugging?" once, and the owner clicks Allow.

Correctness is judged by the owner (or the operator) from the printed answers: this tool measures, it
does not grade what it cannot see.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

TASKS = [
    ("public", "What is the title of the top story on Hacker News (news.ycombinator.com) right now?"),
    ("account", "How many unread emails are in my Gmail inbox?"),
    ("account", "Open my GitHub notifications and tell me how many notifications there are."),
    ("search", "Search Amazon for AA batteries and tell me the price of the first result. Do not buy anything."),
    ("public", "What is the current temperature in San Francisco on weather.com?"),
]


async def _no(question: str) -> str:
    print(f"      (asked: {question[:120]!r} -> no)")
    return "no"


async def run_lane(goals: list[str]) -> list[dict]:
    from eve_bridge import computer_agent as ca
    from eve_bridge.browser_lane import BrowserLane, BrowserLaneConfig, make_step_asker
    from eve_bridge.envfile import load_env_file

    load_env_file()
    env = {**os.environ, "EVE_JEV_STEP": "1"}
    lane = BrowserLane(BrowserLaneConfig(attach=True), step_asker=make_step_asker(env))
    create = ca.make_response_creator()
    out = []
    try:
        t = time.perf_counter()
        await lane.outline()                                  # the one connection, and the owner's one Allow
        print(f"  lane connected (incl. the Allow click) in {time.perf_counter() - t:.1f} s")
        for goal in goals:
            agent = ca.ComputerAgent(create, config=ca.configured(), on_event=lambda ev: None, ask_user=_no,
                                     browser=lane)
            t = time.perf_counter()
            answer, note = await agent.run_in_browser(goal)
            secs = time.perf_counter() - t
            out.append({"arm": "lane", "goal": goal, "secs": round(secs, 1), "finished": bool(answer),
                        "answer": answer, "handoff_note": note.strip()[:300], "bill": agent.bill()})
            print(f"  lane  {secs:6.1f} s  {'DONE ' if answer else 'NOT  '} {(answer or note.strip() or '(nothing)')[:140]}")
    finally:
        await lane.close()
    return out


async def run_codex(goals: list[str]) -> list[dict]:
    from eve_bridge.codex_computer import CodexComputerAgent
    from eve_bridge.envfile import load_env_file

    load_env_file()
    out = []
    for goal in goals:
        agent = CodexComputerAgent(on_event=lambda ev: None, ask_user=_no)
        t = time.perf_counter()
        answer = await agent.run(goal)
        secs = time.perf_counter() - t
        ok = bool(answer) and not answer.startswith(("Codex could not", "Codex task stopped"))
        out.append({"arm": "codex", "goal": goal, "secs": round(secs, 1), "finished": ok, "answer": answer})
        print(f"  codex {secs:6.1f} s  {'DONE ' if ok else 'NOT  '} {answer[:140]}")
    return out


def main() -> int:
    p = argparse.ArgumentParser(prog="chrome_lane_eval")
    p.add_argument("--arms", default="lane,codex")
    p.add_argument("--tasks", default="")
    p.add_argument("--out")
    args = p.parse_args()
    picked = [int(x) for x in args.tasks.split(",") if x.strip()] or list(range(1, len(TASKS) + 1))
    goals = [TASKS[i - 1][1] for i in picked]
    results: list[dict] = []
    for arm in [a.strip() for a in args.arms.split(",") if a.strip()]:
        print(f"== {arm}")
        results += asyncio.run(run_lane(goals) if arm == "lane" else run_codex(goals))
    by_arm: dict[str, list[dict]] = {}
    for r in results:
        by_arm.setdefault(r["arm"], []).append(r)
    for arm, rows in by_arm.items():
        done = [r for r in rows if r["finished"]]
        med = sorted(r["secs"] for r in rows)[len(rows) // 2]
        print(f"{arm}: finished {len(done)}/{len(rows)}, median {med:.1f} s, total {sum(r['secs'] for r in rows):.1f} s")
    if args.out:
        Path(args.out).write_text(json.dumps(results, indent=1, ensure_ascii=False))
    print("CHROME_LANE_EVAL_COMPLETE")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
