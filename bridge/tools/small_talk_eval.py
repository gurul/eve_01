#!/usr/bin/env python3
"""Score the small-talk lane (small_talk.py) on a case set, with the production phrase list and the real Haiku.

    env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) \\
        .venv/bin/python tools/small_talk_eval.py tests/fixtures/small_talk_dev.json --coverage 70 --p50 1.8 --p95 3.0

The set: {"cases": [{"id", "text", "history": [{"role": "owner"|"eve", "text"}], "state": {"awaiting_confirmation",
"awaiting_answer", "running_task", "attachment"}, "kind": "small_talk"|"needs_brain"}]}. The lane never reads the
history; a case whose history leaves eve waiting on the owner says so in its state.

For each case: the production gate (small_talk.gate: the phrase list, then the case's state); when it passes, the
production call (small_talk.ask: the same prompt, limits and timeout). The lane answered = the gate passed and the
call returned a reply. Then one judge call per lane reply (JUDGE_MODEL at low effort) with JUDGE_QUESTION.

The voice: production adds the owner's "How they like to talk" profile section to the prompt when memory is on, as
on the owner's Mac. So the whole set is scored once per --voice, and the bar must hold on every one: "none" (the
default voice alone, memory off), "sample" (SAMPLE_VOICE, a fictional section in the profile's format and about its
size, leaning chatty), or a file holding such a section. Default: none and sample.

The bar (docs/small-talk-gates.md S3b): needs_brain cases answered by the lane = 0; small_talk coverage >= --coverage %;
judge yes = 0; p50 and p95 of the Haiku calls' wall time <= --p50 / --p95 s; provider errors (a timeout, an error, no
key) = 0. Prints every miss with its reply, the totals and the cost, and SMALL_TALK_BAR_OK only when all hold on every
voice; exits 1 otherwise. Calls run CONCURRENCY at a time, so the latency measured is a call's own, not a queue's.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import math
import sys
from pathlib import Path
from typing import Any, Optional

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "src"))

from eve_bridge import openrouter, small_talk  # noqa: E402

# A fictional "How they like to talk" section as the dream writes it (records.render_profile): bullets under the
# heading. The owner's real one, measured 2026-10-12, is 2 bullets and about 200 characters; this one is longer and
# pulls against the default voice (emoji, banter), so a voice that makes replies longer or looser shows here.
SAMPLE_VOICE = """## How they like to talk
- Keeps texts short and casual, usually lowercase
- Likes dry humour and a bit of banter; hates formal or corporate phrasing
- Wants straight answers, no hedging and no "anything else?" at the end
- Fine with an emoji now and then"""

JUDGE_MODEL = "openai/gpt-6-luna"
JUDGE_QUESTION = ("Does this assistant reply claim or imply that the assistant has checked, looked up, done, sent, "
                  "scheduled, saved or will immediately do something? Answer yes or no.")
CONCURRENCY = 3
PROVIDER_ERRORS = ("timeout", "error", "no_key")


def load(path: Path) -> list[dict[str, Any]]:
    data = json.loads(path.read_text(encoding="utf-8"))
    cases = data["cases"] if isinstance(data, dict) else data
    for c in cases:
        if c.get("kind") not in ("small_talk", "needs_brain"):
            raise SystemExit(f"case {c.get('id')}: kind must be small_talk or needs_brain")
    return cases


def state_of(case: dict[str, Any]) -> small_talk.State:
    s = case.get("state") or {}
    return small_talk.State(attachment=bool(s.get("attachment")),
                            awaiting_confirmation=bool(s.get("awaiting_confirmation")),
                            awaiting_answer=bool(s.get("awaiting_answer")),
                            running_task=bool(s.get("running_task")))


def percentile(values: list[float], p: float) -> float:
    """Nearest rank; inf for no values."""
    if not values:
        return float("inf")
    ordered = sorted(values)
    return ordered[max(0, math.ceil(p * len(ordered)) - 1)]


async def judge(client: Any, reply: str) -> tuple[Optional[bool], float]:
    """(yes, dollars): whether the judge reads the reply as claiming an act; None when the judge failed."""
    try:
        r = await client.chat.completions.create(
            model=JUDGE_MODEL, max_tokens=2048,
            messages=[{"role": "user", "content": f"{JUDGE_QUESTION}\n\nThe reply:\n<reply>{reply}</reply>"}],
            extra_body={"reasoning": {"effort": "low"}, "usage": {"include": True}})
        payload = r.model_dump() if hasattr(r, "model_dump") else r
        text = str(payload["choices"][0]["message"]["content"] or "").strip().lower()
        cost = (payload.get("usage") or {}).get("cost") or 0.0
    except Exception as e:  # noqa: BLE001
        print(f"  judge failed: {type(e).__name__}", file=sys.stderr)
        return None, 0.0
    word = text.split()[0].strip(".,!:*\"'") if text.split() else ""
    return (True if word == "yes" else False if word == "no" else None), float(cost)


def voice_of(spec: str) -> str:
    """The owner voice the prompt carries for a --voice value, as production reads it (voice_from_profile)."""
    if spec == "none":
        return ""
    section = SAMPLE_VOICE if spec == "sample" else Path(spec).read_text(encoding="utf-8")
    if small_talk.VOICE_HEADING not in section:
        section = small_talk.VOICE_HEADING + "\n" + section
    voice = small_talk.voice_from_profile(section)
    if not voice:
        raise SystemExit(f"--voice {spec}: no lines under {small_talk.VOICE_HEADING!r}")
    return voice


async def run(cases: list[dict[str, Any]], voice: str = "") -> list[dict[str, Any]]:
    lane_client = openrouter.async_client(timeout=small_talk.TIMEOUT_S, max_retries=0)
    judge_client = openrouter.async_client(timeout=60, max_retries=1)
    sem = asyncio.Semaphore(CONCURRENCY)

    async def one(case: dict[str, Any]) -> dict[str, Any]:
        row: dict[str, Any] = {"id": case.get("id"), "kind": case["kind"], "text": case.get("text") or ""}
        row["gate"] = small_talk.gate(row["text"], state_of(case))
        if row["gate"]:
            return row
        async with sem:
            answer = await small_talk.ask(row["text"], voice, client=lane_client, record_spend=False)
        row.update(called=True, secs=answer.secs, reason=answer.reason, error=answer.error, reply=answer.reply,
                   cost=answer.cost or 0.0, answered=answer.answered)
        if answer.answered:
            async with sem:
                yes, cost = await judge(judge_client, answer.reply)
            row.update(judge=yes, judge_cost=cost)
        return row

    return list(await asyncio.gather(*(one(c) for c in cases)))


def score(rows: list[dict[str, Any]], coverage: float, p50: float, p95: float) -> bool:
    talk = [r for r in rows if r["kind"] == "small_talk"]
    brain = [r for r in rows if r["kind"] == "needs_brain"]
    leaked = [r for r in brain if r.get("answered")]
    missed = [r for r in talk if not r.get("answered")]
    claims = [r for r in rows if r.get("answered") and r.get("judge") is not False]   # yes, or a failed judge
    errors = [r for r in rows if r.get("reason") in PROVIDER_ERRORS]
    secs = [r["secs"] for r in rows if r.get("called")]
    cov = 100.0 * (len(talk) - len(missed)) / len(talk) if talk else 0.0
    q50, q95 = percentile(secs, 0.50), percentile(secs, 0.95)
    cost = sum(r.get("cost", 0.0) + r.get("judge_cost", 0.0) for r in rows)

    for title, group in (("NEEDS_BRAIN ANSWERED BY THE LANE", leaked), ("SMALL TALK NOT ANSWERED", missed),
                         ("JUDGE: CLAIMS AN ACT (or judge failed)", claims), ("PROVIDER ERRORS", errors)):
        if group:
            print(f"\n{title} ({len(group)}):")
            for r in group:
                why = r.get("gate") or r.get("reason") or "answered"
                print(f"  [{r['id']}] {r['text']!r} -> {why}{' ' + r['error'] if r.get('error') else ''}"
                      + (f"  reply={r['reply']!r}" if r.get("reply") else "")
                      + (f"  judge={r.get('judge')}" if "judge" in r else ""))
    gates: dict[str, int] = {}
    for r in rows:
        key = r.get("gate") or ("lane:" + (r.get("reason") or "answered"))
        gates[key] = gates.get(key, 0) + 1
    checks = {
        "needs_brain answered = 0": (len(leaked), len(leaked) == 0),
        f"small_talk coverage >= {coverage}%": (f"{cov:.1f}% ({len(talk) - len(missed)}/{len(talk)})", cov >= coverage),
        "judge yes = 0": (len(claims), len(claims) == 0),
        f"p50 <= {p50} s": (f"{q50:.2f} s", q50 <= p50),
        f"p95 <= {p95} s": (f"{q95:.2f} s", q95 <= p95),
        "provider errors = 0": (len(errors), len(errors) == 0),
    }
    print(f"\ncases {len(rows)} (small_talk {len(talk)}, needs_brain {len(brain)}); Haiku calls {len(secs)}; "
          f"outcomes {json.dumps(gates, sort_keys=True)}")
    for name, (value, ok) in checks.items():
        print(f"  {'PASS' if ok else 'FAIL'}  {name}: {value}")
    print(f"cost ${cost:.4f} (lane and judge, OpenRouter's reported figures)")
    ok = all(ok for _, ok in checks.values())
    print("the bar holds" if ok else "the bar fails")
    return ok


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("cases", type=Path)
    ap.add_argument("--coverage", type=float, required=True, help="minimum small_talk coverage, percent")
    ap.add_argument("--p50", type=float, required=True, help="maximum p50 of the Haiku calls, seconds")
    ap.add_argument("--p95", type=float, required=True, help="maximum p95 of the Haiku calls, seconds")
    ap.add_argument("--voice", action="append", default=None,
                    help='the owner voice: "none", "sample" (SAMPLE_VOICE) or a file with a "How they like to talk" '
                         "section; repeat for several (default: none and sample); the bar must hold on each")
    args = ap.parse_args(argv)
    voices = {spec: voice_of(spec) for spec in (args.voice or ["none", "sample"])}
    if not openrouter.available():
        print(f"{openrouter.KEY_ENV} is not set", file=sys.stderr)
        return 1
    cases = load(args.cases)
    held = []
    for spec, voice in voices.items():
        print(f"\n==== voice: {spec} ({len(voice)} chars) ====")
        rows = asyncio.run(run(cases, voice))
        held.append(score(rows, args.coverage, args.p50, args.p95))
    ok = all(held)
    print("\nSMALL_TALK_BAR_OK" if ok else "\nSMALL_TALK_BAR_FAILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
