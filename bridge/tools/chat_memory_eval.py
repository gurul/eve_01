#!/usr/bin/env python3
"""Recall eval for the Telegram chat: does the real text brain answer "what did I say before that" from what was
said, and say so plainly when it was not.

    .venv/bin/python tools/chat_memory_eval.py                  # this checkout's code, live model (cents)
    .venv/bin/python tools/chat_memory_eval.py --json out.json  # the same, with the rows written out

The cases are tests/fixtures/chat_memory_eval.json: twelve written 2026-10-08 before any run, and four added that
evening from the owner's live failure (memory on, a brand-new chat log, "before that" after a recall answer quoted
the same message again), written before this code's first run on them. Each one seeds a chat through the inlet's
own code paths, then asks its recall questions of the live model:

- ``owner``/``eve``: one exchange, noted as a turn notes it (``TelegramInlet._note``);
- ``gap``: seconds of silence, then the idle check the poll loop runs (``_close_quiet_chat``);
- ``restart``: the inlet's own shutdown, then a new inlet (with the chat log, the same file reopened);
- ``task``: the owner's request, ``_start_task`` with a stand-in agent, an optional steer through
  ``_tool_steer_task``, then the agent's result as ``_run_agent`` reports it;
- ``many``: that many filler exchanges;
- ``before_log``: one exchange written to the memory transcripts only, as an install that had memory on before the
  chat log existed holds it (a case with ``"memory": true``).

A case with ``"memory": true`` lends the inlet a real memory (memory.Memory) over transcripts in a temp folder; its
read tools (memory_search, memory_read, recent_conversation) run against that folder. ``asks`` is several questions,
each a real turn answered by the live model; only the last is scored.

The question is a real turn (``TelegramInlet._turn``): the request is built exactly as a text builds it, from the
owner's env file loaded the daemon's way (``telegram.configured``: the model, its effort). Memory is off (the default
install) unless the case turns it on, and nothing else is lent beyond the chat log, which this checkout's daemon opens by default (in a temp folder
here; the owner's real one is never read). Only ``recent_messages`` runs; any other tool call is refused as its
result, so no task, app or message leaves the eval. The Telegram API is a recorder.

Scored by code: a knowable case passes when the reply holds one of its ``expect`` strings (case-insensitive); a case
marked knowable=false passes when the reply says it does not have it (``ABSTAIN``). The code under test is whatever
``src`` this file sits beside, so the same file run from a checkout of main measures main ("before").
Rows carry the case id, pass/fail and the tools called; replies are printed only with ``--show``.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import sys
import tempfile
from pathlib import Path
from typing import Any, Optional

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

CASES = Path(__file__).resolve().parents[1] / "tests" / "fixtures" / "chat_memory_eval.json"
DAEMON_ENV = ("PATH", "HOME", "EVE_SERIAL_PORT")
MEMORY_READ_TOOLS = ("memory_search", "memory_read", "recent_conversation")
ATTEMPTS = 3
LINE_SECS = 20.0                     # between two seeded lines
# A reply that says it does not have the earlier message. Positive phrasings only; scored case-insensitively.
# The apostrophe may be typographic (’): the model writes "don’t" as often as "don't" (first run, 2026-10-08), and
# "isn’t in the chat history" (second run) and "there isn’t an earlier message" (eighth) are abstentions too, and so
# is "isn’t available in the chat history I can see" (2026-10-08 evening, run 3 of the live-* pass). Every saved run
# is rescored with the final pattern (--rescore).
ABSTAIN = re.compile(r"\b(don['’]?t|do not|can['’]?t|cannot|didn['’]?t|did not|no longer)\s+(have|see|find|remember|"
                     r"recall|keep|know)|\bnothing (before|earlier)|\bno (earlier|record|message)|"
                     r"\bnot (kept|shown|see)|\bhaven['’]?t (got|seen|kept)|\bisn['’]?t (anything|there)|"
                     r"\bwas the first|\bfirst (message|thing) (you|i)|\bisn['’]?t in (the|this|my)|\bnot in (the|this|my)"
                     r"|\b(isn['’]?t|is not|wasn['’]?t) an? (earlier|previous)|\bno (previous|prior)"
                     r"|\b(isn['’]?t|is not|not) available",
                     re.I)


class Recorder:
    """The Bot API as a recorder: every message the inlet sends is kept; nothing reaches Telegram."""

    def __init__(self) -> None:
        self.sent: list[str] = []
        self._id = 100

    async def send_message(self, chat_id: int, text: str, *a: Any, **k: Any) -> None:
        self.sent.append(text)

    async def send_inline(self, chat_id: int, text: str, *a: Any, **k: Any) -> int:
        self.sent.append(text)
        self._id += 1
        return self._id

    def __getattr__(self, name: str) -> Any:
        async def nothing(*a: Any, **k: Any) -> Any:
            self._id += 1
            return self._id
        return nothing


class Agent:
    """A stand-in computer agent: runs until released, takes steers, returns the case's result."""

    def __init__(self, result: str) -> None:
        self.result, self.release, self.steers = result, asyncio.Event(), []

    async def run(self, goal: str, note: str = "") -> str:
        await self.release.wait()
        return self.result

    def steer(self, text: str) -> bool:
        self.steers.append(text)
        return True

    def cancel(self, reason: str = "") -> None:
        self.release.set()


def _environment() -> None:
    from eve_bridge.envfile import load_env_file

    keep = {k: os.environ[k] for k in DAEMON_ENV if k in os.environ}
    os.environ.clear()
    os.environ.update(keep)
    load_env_file()
    os.environ["EVE_MEMORY"] = "0"
    os.environ["EVE_RECORDS"] = "0"
    os.environ["EVE_SPEND_DIR"] = tempfile.mkdtemp(prefix="chat-eval-spend-")


def score(case: dict[str, Any], reply: str) -> bool:
    """A knowable case: one of ``expect`` is in the reply. Otherwise: the reply says it does not have it."""
    if case["knowable"]:
        return any(x.lower() in reply.lower() for x in case["expect"])
    return bool(ABSTAIN.search(reply))


async def run_case(case: dict[str, Any], scratch: Path, show: bool) -> dict[str, Any]:
    from eve_bridge import telegram as tg
    from eve_bridge.computer_agent import make_response_creator

    try:
        from eve_bridge import chat_log as chat_log_mod
    except ImportError:
        chat_log_mod = None
    cfg = tg.configured()
    if not cfg.enabled:
        raise SystemExit("NO_TELEGRAM: the env does not turn the Telegram door on")
    real = make_response_creator()
    clock = {"mono": 1000.0, "wall": 1_800_000_000.0}
    agents: list[Agent] = []
    pending: dict[str, str] = {"result": ""}
    calls: list[str] = []
    call_args: list[dict[str, Any]] = []
    log_path = scratch / case["id"] / "chat" / "log.json"
    memory = None
    if case.get("memory"):
        from datetime import datetime

        from eve_bridge.memory import Memory
        from eve_bridge.recall import RecallConfig
        from eve_bridge.transcripts import TranscriptConfig, Transcripts

        rcfg = RecallConfig(store=scratch / case["id"] / "memory", notes=scratch / case["id"] / "notes")
        rcfg.transcripts_dir.mkdir(parents=True, exist_ok=True)
        tx = Transcripts(TranscriptConfig(enabled=True, root=rcfg.transcripts_dir),
                         wall=lambda: datetime.fromtimestamp(clock["wall"]).astimezone())
        assert tx.enabled, "the temp folder must pass the transcript guards"
        memory = Memory(rcfg, tx, None)

    async def create(payload: dict[str, Any]) -> dict[str, Any]:
        last: Optional[BaseException] = None
        for attempt in range(ATTEMPTS):
            try:
                return await real(payload)
            except Exception as e:  # noqa: BLE001 — retried, then the case is an error
                last = e
                await asyncio.sleep(2.0 * (attempt + 1))
        assert last is not None
        raise last

    def factory(on_event: Any, ask_user: Any, floor: Any = None) -> Agent:
        agents.append(Agent(pending["result"]))
        return agents[-1]

    def make() -> Any:
        lent: dict[str, Any] = {}
        if chat_log_mod is not None and chat_log_mod.CHAT_LOG_DEFAULT:
            lent["chat_log"] = chat_log_mod.ChatLog(log_path, wall=lambda: clock["wall"])
        if memory is not None:
            lent["memory"] = memory
            memory.chat_log = lent.get("chat_log")
        inlet = tg.TelegramInlet(cfg, Recorder(), create, agent_factory=factory, toolset_route=None,
                                 clock=lambda: clock["mono"], wall=lambda: clock["wall"], **lent)
        real_tool = inlet._tool

        async def tool(name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:
            calls.append(name)
            call_args.append({"tool": name, "args": args})
            if name == "recent_messages" or (memory is not None and name in MEMORY_READ_TOOLS):
                return await real_tool(name, args, chat_id)
            return {"ok": False, "reason": "that tool is not available right now"}

        inlet._tool = tool

        async def no_owner_to_ask(name: str, args: Any, chat_id: int) -> None:
            return None               # the taint gate passes the call on to tool, which runs only reads

        inlet._gate = no_owner_to_ask
        return inlet

    def tick(secs: float = LINE_SECS) -> None:
        clock["mono"] += secs
        clock["wall"] += secs

    inlet = make()
    chat = inlet._chat_id or 1
    inlet._chat_id = chat
    for step in case["steps"]:
        if "owner" in step:
            inlet._note("user", step["owner"])
            tick()
            inlet._note("eve", step["eve"])
            tick()
        elif "before_log" in step:
            assert memory is not None, "before_log needs a case with memory on"
            conv = memory.transcripts.new_conv("telegram")
            memory.transcripts.append("telegram", conv, "owner", "say", step["before_log"]["owner"])
            tick()
            memory.transcripts.append("telegram", conv, "eve", "say", step["before_log"]["eve"])
            tick()
            memory.transcripts.close("telegram", conv)
        elif "gap" in step:
            tick(float(step["gap"]))
            inlet._close_quiet_chat()
        elif step.get("restart"):
            await inlet._shutdown()
            inlet = make()
            inlet._chat_id = chat
        elif "many" in step:
            for i in range(int(step["many"])):
                inlet._note("user", f"And in bed {i + 1}, what should go there?")
                tick()
                inlet._note("eve", f"Bed {i + 1}: herbs would do well.")
                tick()
        elif "task" in step:
            t = step["task"]
            pending["result"] = t["result"]
            inlet._note("user", t["owner"])
            tick()
            started = inlet._start_task(t["goal"], chat)
            assert started.get("ok"), started
            tick()
            if t.get("steer"):
                inlet._note("user", t["steer_owner"])
                tick()
                await inlet._tool_steer_task("steer_task", {"text": t["steer"]}, chat)
                inlet._note("eve", "Told it.")
                tick()
            agents[-1].release.set()
            await inlet._agent_task
            tick()
    asks = case.get("asks") or [case["ask"]]
    replies: list[str] = []
    error = ""
    for n, ask in enumerate(asks):
        sent_before = len(inlet.api.sent)
        tick()
        try:
            await inlet._turn(tg.Inbound(chat, chat, ask, message_id=999 + n))
            replies.append("\n".join(inlet.api.sent[sent_before:]).strip())
        except Exception as e:  # noqa: BLE001
            replies.append("")
            error = type(e).__name__
            break
        tick(30)
    reply = replies[-1] if replies and not error else ""
    ok = score(case, reply)
    if show:
        for ask, said in zip(asks, replies, strict=False):
            print(f"--- {case['id']}: {ask!r} -> {said!r}")
    return {"id": case["id"], "knowable": case["knowable"], "pass": bool(ok and not error), "error": error,
            "tools": calls, "args": call_args, "reply": reply, "replies": replies, "chat_log": chat_log_mod is not None}


async def main_async(show: bool, only: str) -> list[dict[str, Any]]:
    cases = json.loads(CASES.read_text())["cases"]
    rows = []
    with tempfile.TemporaryDirectory(prefix="chat-eval-") as tmp:
        for case in cases:
            if only and case["id"] != only:
                continue
            rows.append(await run_case(case, Path(tmp), show))
            r = rows[-1]
            print(f"{'PASS' if r['pass'] else 'FAIL'}  {r['id']:<30} tools={','.join(r['tools']) or '-'}"
                  f"{'  ERROR ' + r['error'] if r['error'] else ''}", flush=True)
    return rows


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="chat_memory_eval", description=__doc__.split("\n\n")[0])
    p.add_argument("--json", default="", help="write the rows here")
    p.add_argument("--show", action="store_true", help="print each reply (the fixtures are made up)")
    p.add_argument("--only", default="", help="run one case by id")
    p.add_argument("--rescore", nargs="*", default=None, help="score saved --json files again, offline")
    args = p.parse_args(argv)
    if args.rescore is not None:
        cases = {c["id"]: c for c in json.loads(CASES.read_text())["cases"]}
        for path in args.rescore:
            rows = json.loads(Path(path).read_text())
            passed = sum(1 for r in rows if not r["error"] and score(cases[r["id"]], r["reply"]))
            print(f"{Path(path).name}: {passed}/{len(rows)}")
        return 0
    _environment()
    rows = asyncio.run(main_async(args.show, args.only))
    passed = sum(r["pass"] for r in rows)
    print(f"== {passed}/{len(rows)} passed (chat log {'on' if rows and rows[0]['chat_log'] else 'off'})")
    if args.json:
        Path(args.json).write_text(json.dumps(rows, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
