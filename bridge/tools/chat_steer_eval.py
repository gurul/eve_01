#!/usr/bin/env python3
"""Chat steering eval: the owner sends a message and, while eve's turn for it is still running, a second one (a
double-text). Does steering (EVE_CHAT_STEER=1: the second message is folded into the running turn as an owner
message before its next model call) reach the right outcome at least as well as today's queue (the second message
runs as its own turn afterwards)? The bar is docs/evals/chat-steer-BAR.md, written before this harness; it is not
changed here.

    .venv/bin/python tools/chat_steer_eval.py --arm off --json off.json     # today's queue, live model
    .venv/bin/python tools/chat_steer_eval.py --arm on  --json on.json      # steering, live model
    .venv/bin/python tools/chat_steer_eval.py --arm on --fake-model --cases tests/fixtures/chat_steer_smoke.json

Built on tools/gate_withdraw_eval.py and imports it: the same fresh TelegramInlet per case, built from the owner's
env file loaded the daemon's way (``telegram.configured``: the model, its effort), the same recorder Bot API, temp
folders and temp HOME once the env file is read (none of the owner's own data is read), the same executors and the
same canned, made-up world (example.com only; its docstring lists them). The two arms differ only in EVE_CHAT_STEER
(unset for off, "1" for on); EVE_GATE_WITHDRAW is unset in both. As there, the toolset router runs on keywords (no
Jev call), the small-talk lane and the app-turn grader are off, and the one network call is the chat model's.

Per case (``tests/fixtures/chat_steer_eval.json``:
``{"cases": [{"id", "kind": "double"|"control", "subtype", "setup": [...], "first", "second",
"delay": "during"|"after", "intent"}]}``):

1. ``setup`` lines (``{"who": "owner"|"eve", "text"}``) are noted as earlier chat lines (``TelegramInlet._note``).
2. ``first`` goes through ``TelegramInlet._handle``, the path every typed message takes.
3. ``second`` goes through ``_handle`` too, at ``delay``:
   - ``during``: as soon as the first turn has started its first model call. That call is held until ``second``
     has been handed to ``_handle`` (no more than that: the harness never steers), so the message always arrives
     while the turn runs, before any tool of it. In the on arm it waits in the turn's inbox and is folded in before
     the next model call, if the turn makes one; in the off arm it queues behind the turn lock and runs as its own
     turn afterwards. A first turn that never reaches a model call (it was answered by code) gets ``second`` once
     it has ended, and the row says so (``second_sent``: ``after``).
   - ``after``: once the first turn has fully finished (no turn, job or question left). Both arms must behave the
     same there (the bar's controls).
4. An owner stand-in answers every firm question (a gate's yes/no) "yes", through ``_handle``, in both arms, at any
   point of the case. A free question (no buttons) is left to time out at ``--ask-timeout`` (2 s; the daemon's is
   ``TelegramConfig.ask_timeout_secs``, written to the JSON header).
5. The case ends when no turn, job or question is left.

Executors (gate_withdraw_eval.route_of): every tool that could act is a recorder, nothing is sent, deleted or bought,
and nothing but the model is called. Read tools answer canned, made-up results; ``web_search`` (offered only when the
env file has a search key) answers from a small canned set here (WEB_FACTS) instead of "nothing found". The tools the
cases can drive are in tests/fixtures/chat_steer_tools.json.

Per case per arm the JSON row holds: every executed call in order (tool, args, ``preceded_by_yes``: an unspent yes
for that exact call came before it, ``needs_yes``: False only for a direct app's routine private write, which runs
unasked in the daemon too; ``phase``: before or after ``second`` was sent), ``unapproved_executed`` (needed a yes,
had none), ``duplicate_acts`` (the same acting call, tool and args, executed more than once: the extra runs),
every gate question with its answer, ``folds`` (each time the running turn folded messages in: its round and how
many), ``turns`` (model turns run) and ``second_own_turn`` (``second`` ran as a turn of its own), every eve reply in
order, the final one, provider errors, and ``latency_s`` (seconds from handing ``second`` to ``_handle`` to the last
reply, as measured). Replies are in the JSON (a local eval artifact, not a log), and printed only with ``--show``.

``--fake-model``: a scripted brain, no API call and no env file. A case may carry ``fake``:
``{"first": {"tool", "args"}, "second": {"tool", "args"}}``; for the newest owner message it calls that message's
tool once (if any), then says "Done.". It proves the plumbing, nothing about the model.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
import tempfile
import time
from collections import Counter
from pathlib import Path
from typing import Any, Optional

TOOLS_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS_DIR.parent / "src"))
sys.path.insert(0, str(TOOLS_DIR))

import gate_withdraw_eval as gw  # noqa: E402  — the executors, the canned world and the inlet's build

BRIDGE = gw.BRIDGE
CASES = BRIDGE / "tests" / "fixtures" / "chat_steer_eval.json"
TOOLS_FILE = BRIDGE / "tests" / "fixtures" / "chat_steer_tools.json"
STEER_ENV = "EVE_CHAT_STEER"
DELAYS = ("during", "after")
CHAT = gw.CHAT
POLL = gw.POLL
CASE_DEADLINE = gw.CASE_DEADLINE

# web_search's canned answers (made up): the first entry whose words all appear in the query answers it.
WEB_FACTS: list[tuple[tuple[str, ...], str]] = [
    (("weather",), "Forecast: 18 C and sunny, light wind, no rain expected."),
    (("open",), "Hours: open today 9 am to 9 pm."),
    (("showtime",), "Showtimes today: 4:10 pm, 7:00 pm, 9:45 pm."),
    (("price",), "Listed at 89 dollars, down from 120."),
]
WEB_TOOLS = frozenset({"web_search"})


def web_answer(args: dict[str, Any]) -> dict[str, Any]:
    q = str(args.get("query") or args.get("q") or args.get("question") or "").lower()
    answer = next((a for words, a in WEB_FACTS if all(w in q for w in words)), "No results found for that search.")
    return {"ok": True, "answer": answer, "sources": ["https://news.example.com/result"]}


# ---- the run's record ---------------------------------------------------------------------------------------------
class Record(gw.Record):
    """gate_withdraw_eval's record; ``replied_at`` is when ``second`` was handed to ``_handle``."""

    def __init__(self) -> None:
        super().__init__()
        self.folds: list[dict[str, Any]] = []

    def phase(self) -> str:
        return "after_second" if self.replied_at is not None else "first"


class Steer:
    """The running case's timing of ``second``: the first model call waits on ``sent`` once it has set ``started``."""

    started: Optional[asyncio.Event] = None
    sent: Optional[asyncio.Event] = None
    hold: bool = False                       # the case's delay is "during" and ``second`` is not sent yet


S = Steer()


# ---- the scripted brain (--fake-model) ----------------------------------------------------------------------------
async def fake_create(payload: dict[str, Any]) -> dict[str, Any]:
    case = gw.H.case
    fake = case.get("fake") or {}
    items = [i for i in payload.get("input") or [] if isinstance(i, dict)]
    last_user = max((n for n, i in enumerate(items) if i.get("type") == "message" and i.get("role") == "user"),
                    default=-1)
    said = gw._text_of(items[last_user]).strip() if last_user >= 0 else ""
    after = items[last_user + 1:]
    which = "first" if said == str(case.get("first", "")).strip() else \
        "second" if said == str(case.get("second", "")).strip() else ""
    spec = fake.get(which) or {}
    if spec.get("tool") and not any(i.get("type") == "function_call" for i in after):
        return {"output": [{"type": "function_call", "name": spec["tool"], "call_id": f"fc-{which}",
                            "arguments": json.dumps(spec.get("args") or {})}], "usage": {}}
    return {"output": [{"type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": "Done." if spec else "Okay."}]}], "usage": {}}


# ---- one case -----------------------------------------------------------------------------------------------------
def _environment(fake: bool, arm: str) -> Path:
    root = gw._environment(fake, "off")          # EVE_GATE_WITHDRAW unset in both arms
    os.environ.pop(STEER_ENV, None)
    if arm == "on":
        os.environ[STEER_ENV] = "1"
    return root


def _build(cfg: Any, create: Any, scratch: Path) -> Any:
    inlet = gw._build(cfg, create, scratch)
    wrapped_dispatch, real_steer_into = inlet._dispatch_tool, inlet._steer_into

    async def dispatch(name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:
        if name in WEB_TOOLS:
            return web_answer(args)
        return await wrapped_dispatch(name, args, chat_id)

    def steer_into(items: list[dict[str, Any]], round_no: int) -> list[dict[str, Any]]:
        out = real_steer_into(items, round_no)
        if len(out) > len(items):
            gw.H.run.folds.append({"round": round_no, "count": len(out) - len(items),
                                   "t": round(time.monotonic() - gw.H.run.t0, 3)})
        return out

    inlet._dispatch_tool, inlet._steer_into = dispatch, steer_into
    return inlet


def route_of(inlet: Any, name: str) -> str:
    return "canned-web" if name in WEB_TOOLS else gw.route_of(inlet, name)


def _stand_in(inlet: Any, answered: set[int], ids: Any) -> None:
    """The owner stand-in: every firm question gets a "yes", once."""
    from eve_bridge import telegram as tg

    live = inlet._live_question()
    if live is not None and live.firm and id(live) not in answered:
        answered.add(id(live))
        inlet._handle(tg.Inbound(CHAT, CHAT, "yes", message_id=next(ids)))


def duplicate_acts(executed: list[dict[str, Any]]) -> int:
    """Extra runs of the same acting call (tool and args) in one case."""
    seen = Counter((e["tool"], json.dumps(e["args"], sort_keys=True, default=str))
                   for e in executed if e["needs_yes"])
    return sum(n - 1 for n in seen.values() if n > 1)


async def run_case(case: dict[str, Any], arm: str, cfg: Any, create: Any, scratch: Path,
                   show: bool) -> dict[str, Any]:
    from eve_bridge import telegram as tg

    delay = case.get("delay", "during")
    if delay not in DELAYS:
        raise SystemExit(f"BAD_CASE: {case.get('id')}: delay must be one of {DELAYS}")
    gw.H.run, gw.H.case = Record(), case
    run: Record = gw.H.run                                              # type: ignore[assignment]
    S.started, S.sent, S.hold = asyncio.Event(), asyncio.Event(), delay == "during"
    inlet = _build(cfg, create, scratch / case["id"])
    for line in case.get("setup") or []:
        inlet._note("user" if line.get("who") == "owner" else "eve", str(line["text"]))
    ids = iter(range(5000, 6000))
    answered: set[int] = set()
    deadline = time.monotonic() + CASE_DEADLINE
    outcome = "ok"
    second_sent = ""
    inlet._handle(tg.Inbound(CHAT, CHAT, case["first"], message_id=next(ids)))
    while time.monotonic() < deadline:
        _stand_in(inlet, answered, ids)
        if delay == "during" and S.started.is_set():
            second_sent = "during"
            break
        if await gw._settled(inlet):
            second_sent = "after"
            break
        await asyncio.sleep(POLL)
    else:
        outcome = "stuck"
    if second_sent:
        run.replied_at = time.monotonic()
        inlet._handle(tg.Inbound(CHAT, CHAT, case["second"], message_id=next(ids)))
        S.hold = False
        S.sent.set()                                                     # the held model call goes on
        while time.monotonic() < deadline:
            _stand_in(inlet, answered, ids)
            if inlet._live_question() is None and await gw._settled(inlet):
                break
            await asyncio.sleep(POLL)
        else:
            outcome = "stuck"
    S.hold = False
    S.sent.set()
    if outcome == "stuck":
        for job in list(inlet._jobs):
            job.cancel()
    try:
        await inlet._shutdown()
    except Exception:  # noqa: BLE001 — the case is recorded either way
        pass
    ended = sorted(run.turns, key=lambda t: t["t"])
    final = (ended or [{}])[-1]
    late = run.replied_at is not None and bool(ended) and final["t"] >= run.replied_at
    error = next((t["error"] for t in run.turns if t.get("error")), "") or (run.provider_errors[0]
                                                                           if run.provider_errors else "")
    row = {
        "id": case["id"], "kind": case.get("kind", ""), "subtype": case.get("subtype", ""), "arm": arm,
        "delay": delay, "second_sent": second_sent, "intent": case.get("intent", ""), "outcome": outcome,
        "executed": run.executed,
        "unapproved_executed": sum(1 for e in run.executed if e["needs_yes"] and not e["preceded_by_yes"]),
        "routine_unasked_executed": sum(1 for e in run.executed if not e["needs_yes"]),
        "duplicate_acts": duplicate_acts(run.executed),
        "questions": [{k: v for k, v in q.items() if k != "spent"} for q in run.questions],
        "gate_questions": sum(1 for q in run.questions if q.get("firm")),
        "folds": run.folds, "folded": sum(f["count"] for f in run.folds),
        "turns": len(run.turns),
        "second_own_turn": any(t["said"] == case["second"] for t in run.turns),
        "provider_error": error,
        "replies": [{"said_by_owner": t["said"], "began": t["phase"],
                     "ended_after_second": run.replied_at is not None and t["t"] >= run.replied_at,
                     "reply": t["reply"], "error": t["error"]} for t in ended],
        "final_reply": final.get("reply", ""),
        "latency_s": round(final["t"] - run.replied_at, 3) if late and run.replied_at is not None else None,
        "offered": sorted(run.offered),
        "routes": {n: route_of(inlet, n) for n in sorted(run.offered)},
    }
    if show:
        for t in row["replies"]:
            print(f"--- {case['id']} [{t['began']}] {t['said_by_owner']!r} -> {t['reply']!r}")
    return row


async def main_async(args: argparse.Namespace) -> dict[str, Any]:
    import dataclasses

    from eve_bridge import telegram as tg

    cfg = tg.configured()
    if not cfg.enabled:
        raise SystemExit("NO_TELEGRAM: the env does not turn the Telegram door on")
    real_timeout = cfg.ask_timeout_secs
    cfg = dataclasses.replace(cfg, ask_timeout_secs=float(args.ask_timeout))   # the harness only
    if args.fake_model:
        model_create: Any = fake_create
    else:
        from eve_bridge.computer_agent import make_response_creator

        model_create = make_response_creator()

    async def create(payload: dict[str, Any]) -> dict[str, Any]:
        gw.H.run.offered |= {str(t.get("name")) for t in payload.get("tools") or [] if isinstance(t, dict)}
        if S.hold and S.started is not None and S.sent is not None:
            S.started.set()                       # "during": the second message goes in before this call returns
            await S.sent.wait()
        if args.fake_model:
            return await model_create(payload)
        last: Optional[BaseException] = None
        for attempt in range(gw.ATTEMPTS):
            try:
                return await model_create(payload)
            except Exception as e:  # noqa: BLE001 — retried, then the case records a provider error
                last = e
                await asyncio.sleep(2.0 * (attempt + 1))
        assert last is not None
        gw.H.run.provider_errors.append(type(last).__name__)
        raise last

    cases = json.loads(Path(args.cases).read_text())["cases"]
    rows = []
    scratch = Path(tempfile.mkdtemp(prefix="chat-steer-cases-"))
    for case in cases:
        if args.only and case["id"] != args.only:
            continue
        r = await run_case(case, args.arm, cfg, create, scratch, args.show)
        rows.append(r)
        acts = ",".join(e["tool"] for e in r["executed"]) or "-"
        print(f"{r['id']:<8} {args.arm:<3} {r['delay']:<6} {r['outcome']:<6} turns={r['turns']} "
              f"folded={r['folded']} dup={r['duplicate_acts']} acts={acts}"
              f"{'  ERROR ' + r['provider_error'] if r['provider_error'] else ''}", flush=True)
    return {"arm": args.arm, "fake_model": bool(args.fake_model), "commit": gw._commit(),
            "model": "fake" if args.fake_model else cfg.model, "effort": "" if args.fake_model else cfg.effort,
            "ask_timeout_real_secs": real_timeout, "ask_timeout_harness_secs": float(args.ask_timeout),
            "steer_env": os.environ.get(STEER_ENV, ""), "withdraw_env": os.environ.get(gw.WITHDRAW_ENV, ""),
            "cases_file": Path(args.cases).name,
            "drivable_tools": [t["name"] for t in json.loads(TOOLS_FILE.read_text())["tools"]], "rows": rows}


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="chat_steer_eval", description=(__doc__ or "").split("\n\n")[0])
    p.add_argument("--arm", choices=("off", "on"), required=True, help="off: EVE_CHAT_STEER unset; on: =1")
    p.add_argument("--cases", default=str(CASES), help="the case file (JSON with a 'cases' list)")
    p.add_argument("--json", default="", help="write the header and rows here")
    p.add_argument("--show", action="store_true", help="print every reply")
    p.add_argument("--only", default="", help="run one case by id")
    p.add_argument("--fake-model", action="store_true", help="a scripted brain: no API call, no env file")
    p.add_argument("--ask-timeout", type=float, default=gw.ASK_TIMEOUT, help="a free question's timeout, seconds")
    args = p.parse_args(argv)
    args.cases = str(Path(args.cases).resolve())
    _environment(args.fake_model, args.arm)
    out = asyncio.run(main_async(args))
    rows = out["rows"]
    print(f"== {len(rows)} cases, arm {args.arm}: {sum(r['outcome'] == 'ok' for r in rows)} ok, "
          f"{sum(r['folded'] > 0 for r in rows)} folded, {sum(r['duplicate_acts'] for r in rows)} duplicate acts, "
          f"{sum(r['unapproved_executed'] for r in rows)} unapproved, "
          f"{sum(bool(r['provider_error']) for r in rows)} provider errors")
    if args.json:
        Path(args.json).write_text(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
