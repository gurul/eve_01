#!/usr/bin/env python3
"""Blind A/B of Inception's Mercury (a diffusion model, on OpenRouter) against the model a lane uses today.

    env $(grep -E '^OPENROUTER_API_KEY=' ~/.config/eve-bridge/env | xargs) \\
        .venv/bin/python tools/mercury_ab_eval.py search      # web search answer step: luna vs mercury-3
    env $(grep -E '^OPENROUTER_API_KEY=' ~/.config/eve-bridge/env | xargs) \\
        .venv/bin/python tools/mercury_ab_eval.py small-talk  # small-talk reply: Haiku vs mercury-3

search: the queries of tests/fixtures/routes/search_holdout2.json (written blind, 2026-09-29; their provider labels
are not used), every other one: 45. Each query goes through websearch.engine_search twice, once per answer model,
on the same engine (Perplexity), with Monid off and the router off, so only the answer model differs.
small-talk: the small_talk cases of tests/fixtures/small_talk_blind_v2.json (30), through small_talk.ask with the
production prompt and limits, voice none.

Then one judge call per pair (JUDGE, a model from neither side), the two replies in a random order it is not told
the meaning of. The judge picks A, B or tie, and (search) flags each answer that states something its own sources
do not support.

THE BAR, fixed 2026-10-19 before the first run (owner: "run both evals ... wire it in"):
  search      mercury wins + ties >= 50% of judged pairs; mercury unsupported flags <= the incumbent's; mercury
              failed searches <= the incumbent's; mercury p50 wall time < the incumbent's.
  small-talk  mercury wins + ties >= 50%; mercury p50 < the incumbent's; and, separately, tools/small_talk_eval.py's
              S3b bar on the v2 set with EVE_SMALL_TALK_MODEL set to mercury (docs/small-talk-gates.md).
Prints MERCURY_AB_OK only when every part of the lane's bar holds; exits 1 otherwise. The spend ledger goes to a
scratch directory, not the owner's day.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import statistics
import sys
import tempfile
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "src"))
os.environ.setdefault("EVE_SPEND_DIR", tempfile.mkdtemp(prefix="mercury-ab-spend-"))

from eve_bridge import openrouter, small_talk, websearch  # noqa: E402

MERCURY = "inception/mercury-3"
JUDGE = "openai/gpt-6.1-sol"
SEED = 20261019
LANES = {
    "search": {"incumbent": websearch.DEFAULT_MODEL, "set": ROOT / "tests/fixtures/routes/search_holdout2.json"},
    "small-talk": {"incumbent": small_talk.DEFAULT_MODEL, "set": ROOT / "tests/fixtures/small_talk_blind_v2.json"},
}

SEARCH_RUBRIC = """Two answers to the same web-search query follow, each with the sources it was written from. Judge \
which answer serves the person who asked better: does it answer the query directly, with concrete facts, correctly \
according to its own sources, in at most four plain sentences? An answer that says it could not find the answer \
loses to one that answers correctly from its sources. Also flag each answer that states a fact its own sources do \
not support. Reply with JSON only: {"winner": "A" | "B" | "tie", "a_unsupported": bool, "b_unsupported": bool, \
"why": "<one sentence>"}"""
TALK_RUBRIC = """Two replies to the same small-talk message from a personal assistant's owner follow. The assistant \
should sound warm, natural and brief, like a friend texting back, in one short sentence, with no offers or promises. \
Which reply is better? Reply with JSON only: {"winner": "A" | "B" | "tie", "why": "<one sentence>"}"""


def judge(rubric: str, item: str, a: str, b: str) -> dict[str, Any]:
    body = {"model": JUDGE, "max_tokens": 2048, "reasoning": {"effort": "low"},
            "response_format": {"type": "json_object"},
            "messages": [{"role": "system", "content": rubric},
                         {"role": "user", "content": f"{item}\n\n=== A ===\n{a}\n\n=== B ===\n{b}"}]}
    req = urllib.request.Request("https://openrouter.ai/api/v1/chat/completions", data=json.dumps(body).encode(),
                                 headers={"Authorization": f"Bearer {openrouter.key()}",
                                          "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as r:
        content = json.load(r)["choices"][0]["message"]["content"]
    return json.loads(content[content.find("{"):content.rfind("}") + 1])


def show_search(out: dict[str, Any]) -> str:
    if not out.get("ok"):
        return f"(the search failed: {out.get('reason')})"
    sources = "\n".join(f"- {s['title']}: {s['snippet']}" for s in out["sources"]) or "(no sources)"
    return f"{out['answer']}\n\nSources:\n{sources}"


def run_search(model: str, query: str) -> dict[str, Any]:
    cfg = replace(websearch.configured(), engine="openrouter-perplexity", providers=(), model=model)
    t0 = time.perf_counter()
    out = websearch.engine_search(query, cfg, monid_ok=False)
    out["secs"] = time.perf_counter() - t0
    return out


def run_talk(model: str, text: str) -> dict[str, Any]:
    import asyncio

    client = openrouter.async_client(timeout=small_talk.TIMEOUT_S, max_retries=0)   # one per loop: a thread runs its own
    ans = asyncio.run(small_talk.ask(text, client=client, model=model, record_spend=False))
    return {"ok": ans.answered, "text": ans.reply or f"(no reply: {ans.reason})", "secs": ans.secs}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("lane", choices=sorted(LANES))
    args = ap.parse_args(argv)
    if not openrouter.available():
        print(f"{openrouter.KEY_ENV} is not set", file=sys.stderr)
        return 1
    lane = LANES[args.lane]
    incumbent = lane["incumbent"]
    cases = json.loads(lane["set"].read_text(encoding="utf-8"))["cases"]
    if args.lane == "search":
        items = [c["goal"] for c in cases][::2]
        go, show, rubric = run_search, show_search, SEARCH_RUBRIC
    else:
        items = [c["text"] for c in cases if c["kind"] == "small_talk"]
        go, show, rubric = run_talk, (lambda o: o["text"]), TALK_RUBRIC
    print(f"{args.lane}: {len(items)} items, {incumbent} vs {MERCURY}, judge {JUDGE}")

    def pair(i_item: tuple[int, str]) -> dict[str, Any]:
        i, item = i_item
        inc, mer = go(incumbent, item), go(MERCURY, item)
        flip = random.Random(SEED + i).random() < 0.5          # mercury is A when flip
        a, b = (mer, inc) if flip else (inc, mer)
        try:
            v = judge(rubric, f"Query: {item}" if args.lane == "search" else f"Owner's message: {item}",
                      show(a), show(b))
        except Exception as e:  # noqa: BLE001 — an unjudged pair is reported, not scored
            v = {"winner": "error", "why": type(e).__name__}
        w = v.get("winner")
        side = "tie" if w == "tie" else "error" if w not in ("A", "B") else \
            ("mercury" if (w == "A") == flip else "incumbent")
        return {"item": item, "inc": inc, "mer": mer, "side": side, "why": v.get("why", ""),
                "mer_unsup": bool(v.get("a_unsupported" if flip else "b_unsupported")),
                "inc_unsup": bool(v.get("b_unsupported" if flip else "a_unsupported"))}

    with ThreadPoolExecutor(max_workers=4) as pool:
        rows = list(pool.map(pair, enumerate(items)))

    for r in rows:
        if r["side"] != "mercury":
            print(f"\n[{r['side']}] {r['item']}\n  incumbent: {show(r['inc'])[:300]!r}\n"
                  f"  mercury:   {show(r['mer'])[:300]!r}\n  judge: {r['why']}")
    judged = [r for r in rows if r["side"] != "error"]
    wins = sum(r["side"] == "mercury" for r in judged)
    ties = sum(r["side"] == "tie" for r in judged)
    losses = len(judged) - wins - ties
    share = (wins + ties) / len(judged) if judged else 0.0
    p50 = {k: statistics.median(r[k]["secs"] for r in rows) for k in ("inc", "mer")}
    fails = {k: sum(not r[k]["ok"] for r in rows) for k in ("inc", "mer")}
    unsup = {k: sum(r[f"{k}_unsup"] for r in judged) for k in ("inc", "mer")}
    print(f"\nmercury wins {wins}, ties {ties}, losses {losses} of {len(judged)} judged "
          f"({len(rows) - len(judged)} unjudged): wins+ties {share:.1%}")
    print(f"p50 wall time: incumbent {p50['inc']:.2f} s, mercury {p50['mer']:.2f} s")
    print(f"failed calls: incumbent {fails['inc']}, mercury {fails['mer']}")
    checks = [share >= 0.5, p50["mer"] < p50["inc"]]
    if args.lane == "search":
        print(f"unsupported flags: incumbent {unsup['inc']}, mercury {unsup['mer']}")
        checks += [unsup["mer"] <= unsup["inc"], fails["mer"] <= fails["inc"]]
    ok = all(checks) and len(judged) >= 0.9 * len(rows)
    print("\nMERCURY_AB_OK" if ok else "\nMERCURY_AB_FAILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
