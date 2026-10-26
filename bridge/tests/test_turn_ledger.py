"""turn_ledger.py: every turn's timings under one trace id, names and numbers only, never a failed turn."""

from __future__ import annotations

import asyncio
import json
from datetime import date, datetime
from pathlib import Path
from typing import Any

import pytest
from test_telegram import OWNER, FakeApi, FakeCreate, Rig, call, say

from eve_bridge import spend, telegram, tool_gate, turn_ledger, websearch

SENTINEL = "PURPLE-OTTER-4417"          # in the message, the tool's args, its result and the reply


def ledger_text(folder: Path) -> str:
    return "".join(p.read_text(encoding="utf-8") for p in sorted(folder.glob("*.jsonl")))


def assert_no_sentinel(folder: Path) -> None:
    """The check the acceptance gate runs with grep -F: the sentinel is nowhere in the ledger."""
    assert SENTINEL not in ledger_text(folder), "the turn ledger holds words from the conversation"


def all_rows(folder: Path) -> list[dict[str, Any]]:
    return [json.loads(line) for line in ledger_text(folder).splitlines() if line.strip()]


@pytest.fixture
def folder(_turn_ledger_in_tmp: Path) -> Path:
    return _turn_ledger_in_tmp


def _rig_with_search(monkeypatch: pytest.MonkeyPatch) -> Rig:
    monkeypatch.setattr(websearch, "search", lambda query, cfg: {"ok": True, "answer": f"the answer is {SENTINEL}",
                                                                 "sources": [{"url": f"https://x.test/{SENTINEL}"}]})
    cfg = telegram.configured({"EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": "t", "EVE_TELEGRAM_OWNER": str(OWNER),
                               "OPENROUTER_API_KEY": "r", "EVE_WEB_SEARCH": "openrouter-exa"})
    return Rig(FakeApi(), FakeCreate(call("web_search", {"query": f"what is {SENTINEL}"}),
                                     say(f"It is {SENTINEL}.")), config=cfg)


# ---- a real turn through the inlet --------------------------------------------------------------------------

def test_a_turn_writes_its_timings_under_one_trace_and_no_words(monkeypatch: pytest.MonkeyPatch,
                                                                 folder: Path) -> None:
    rig = _rig_with_search(monkeypatch)
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, f"look up {SENTINEL}", message_id=3)))
    assert rig.api.sent[-1] == (OWNER, f"It is {SENTINEL}.")          # the turn itself ran as ever

    rows = all_rows(folder)
    assert [r["ev"] for r in rows] == ["turn.started", "model", "call", "model", "turn.settled"]
    traces = {r["trace"] for r in rows}
    assert len(traces) == 1 and len(next(iter(traces))) == 16
    assert {r["door"] for r in rows} == {"telegram"}
    tool = rows[2]
    assert tool["tool"] == "web_search" and tool["ok"] is True and tool["gated"] is False and tool["ms"] >= 0
    settled = rows[-1]
    assert settled["outcome"] == "answered" and settled["rounds"] == 2 and settled["total_ms"] >= 0
    for r in rows:                                   # every key is on its event's allowlist
        assert set(r) <= {"t", "ev"} | set(turn_ledger.ALLOWED[r["ev"]])
    assert_no_sentinel(folder)

    # the spend rows of the same turn carry its trace: `eve-bridge trace` joins them
    spent = [r for p in spend.spend_dir().glob("*.jsonl") for r in map(json.loads, p.read_text().splitlines())]
    assert spent and {r.get("trace") for r in spent} == traces


def test_a_deliberately_leaky_writer_fails_the_same_check(monkeypatch: pytest.MonkeyPatch, folder: Path) -> None:
    """Positive control: the check above can fail. A writer that keeps the tool's args and result, as a careless
    one would, puts the sentinel in the file, and assert_no_sentinel catches it."""
    rig = _rig_with_search(monkeypatch)

    def leaky_call(tool: str, ok: bool, ms: float, gated: Any = None) -> None:
        turn_ledger._append({"t": 1_800_000_000.0, "ev": "call", "tool": tool,
                             "args": f"what is {SENTINEL}", "result": f"the answer is {SENTINEL}"})

    monkeypatch.setattr(turn_ledger, "call", leaky_call)
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, f"look up {SENTINEL}", message_id=3)))
    with pytest.raises(AssertionError):
        assert_no_sentinel(folder)


def test_the_allowlist_drops_any_key_and_any_sentence_a_caller_passes(folder: Path) -> None:
    with turn_ledger.turn("app"):
        turn_ledger._emit("call", tool="web_search", ok=True, ms=12, args={"q": SENTINEL}, result=SENTINEL)
        turn_ledger._emit("call", tool=f"say {SENTINEL} now", ok=True, ms=3)      # a sentence is not a name
        turn_ledger._emit("model", model="gpt-6-luna", ms=40, note=SENTINEL, usd=0.001)
        turn_ledger._emit("not.an.event", text=SENTINEL)
        turn_ledger.link("codex", f"{SENTINEL} and more words")
    assert_no_sentinel(folder)
    calls = [r for r in all_rows(folder) if r["ev"] == "call"]
    assert [c["tool"] for c in calls] == ["web_search", "?"]
    assert not any(r["ev"] == "not.an.event" for r in all_rows(folder))


def test_a_gated_call_says_so_and_the_asking_is_timed(monkeypatch: pytest.MonkeyPatch, folder: Path) -> None:
    rig = _rig_with_search(monkeypatch)
    real = tool_gate.needs_confirmation
    monkeypatch.setattr(tool_gate, "needs_confirmation",
                        lambda name, args, taint="": name == "web_search" or real(name, args, taint))

    async def yes(name: str, args: dict[str, Any], chat_id: int, read: Any = None) -> None:
        return None                                   # the owner said yes

    monkeypatch.setattr(rig.inlet, "_gate", yes)
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "look it up", message_id=3)))
    rows = all_rows(folder)
    gate = next(r for r in rows if r["ev"] == "gate")
    tool = next(r for r in rows if r["ev"] == "call")
    assert gate["tool"] == "web_search" and gate["allowed"] is True
    assert tool["gated"] is True and tool["ok"] is True


def test_a_refused_gate_runs_no_tool(monkeypatch: pytest.MonkeyPatch, folder: Path) -> None:
    rig = _rig_with_search(monkeypatch)
    ran: list[str] = []
    monkeypatch.setattr(websearch, "search", lambda query, cfg: ran.append(query) or {"ok": True})
    monkeypatch.setattr(tool_gate, "needs_confirmation", lambda name, args, taint="": name == "web_search")

    async def no(name: str, args: dict[str, Any], chat_id: int, read: Any = None) -> dict[str, Any]:
        return {"ok": False, "reason": "the owner said no"}

    monkeypatch.setattr(rig.inlet, "_gate", no)
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "look it up", message_id=3)))
    rows = all_rows(folder)
    assert ran == [] and not any(r["ev"] == "call" for r in rows)
    assert next(r for r in rows if r["ev"] == "gate")["allowed"] is False


# ---- never a failed turn ------------------------------------------------------------------------------------

def test_a_write_error_never_reaches_the_turn(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    blocker = tmp_path / "a-file-not-a-folder"
    blocker.write_text("x")
    turn_ledger.set_dir(blocker / "turns")           # mkdir fails under a file
    rig = _rig_with_search(monkeypatch)
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "look it up", message_id=3)))
    assert rig.api.sent[-1] == (OWNER, f"It is {SENTINEL}.")

    def boom(row: dict[str, Any]) -> None:
        raise OSError("disk full")

    monkeypatch.setattr(turn_ledger, "_append", boom)
    rig = _rig_with_search(monkeypatch)
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "look it up", message_id=4)))
    assert rig.api.sent[-1] == (OWNER, f"It is {SENTINEL}.")


def test_the_turns_own_exception_is_never_swallowed(folder: Path) -> None:
    with pytest.raises(ValueError):
        with turn_ledger.turn("telegram"):
            raise ValueError("the turn's own failure")

    async def cancelled() -> None:
        async with turn_ledger.turn("app"):
            raise asyncio.CancelledError

    with pytest.raises(asyncio.CancelledError):
        asyncio.run(cancelled())
    outcomes = [r["outcome"] for r in all_rows(folder) if r["ev"] == "turn.settled"]
    assert outcomes == ["error", "cancelled"]
    assert turn_ledger.current_trace() == ""


def test_two_concurrent_turns_get_different_trace_ids(folder: Path) -> None:
    async def one(door: str, gate: asyncio.Event, other: asyncio.Event) -> str:
        async with turn_ledger.turn(door) as t:
            other.set()
            await gate.wait()                         # both turns are open at once
            turn_ledger.call("web_search", True, 5)
            return t.trace

    async def both() -> tuple[str, str]:
        a, b = asyncio.Event(), asyncio.Event()
        return tuple(await asyncio.gather(one("app", a, b), one("call", b, a)))  # type: ignore[return-value]

    first, second = asyncio.run(both())
    assert first and second and first != second
    calls = {r["door"]: r["trace"] for r in all_rows(folder) if r["ev"] == "call"}
    assert calls == {"app": first, "call": second}


def test_a_nested_turn_names_its_parent(folder: Path) -> None:
    with turn_ledger.turn("telegram") as outer:
        with turn_ledger.turn("telegram") as inner:
            pass
        assert turn_ledger.current_trace() == outer.trace
    started = [r for r in all_rows(folder) if r["ev"] == "turn.started"]
    assert started[1]["trace"] == inner.trace and started[1]["parent"] == outer.trace


def test_off_writes_nothing_and_spend_rows_are_as_they_were(monkeypatch: pytest.MonkeyPatch, folder: Path) -> None:
    monkeypatch.setenv(turn_ledger.ENV, "0")
    rig = _rig_with_search(monkeypatch)
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "look it up", message_id=3)))
    assert not folder.exists() or not list(folder.glob("*.jsonl"))
    spent = [r for p in spend.spend_dir().glob("*.jsonl") for r in map(json.loads, p.read_text().splitlines())]
    assert spent and not any("trace" in r for r in spent)


def test_link_joins_another_systems_id_only_inside_a_turn(folder: Path) -> None:
    turn_ledger.link("codex", "turn-abc123")        # no turn: nothing to join it to
    assert not folder.exists() or ledger_text(folder) == ""
    with turn_ledger.turn("telegram") as t:
        turn_ledger.link("agent_run", "2026-10-26-101010")
    links = [r for r in all_rows(folder) if r["ev"] == "link"]
    assert links == [{"t": links[0]["t"], "ev": "link", "trace": t.trace, "door": "telegram",
                      "kind": "agent_run", "ref": "2026-10-26-101010"}]


# ---- the reader and the CLI ---------------------------------------------------------------------------------

def _synthetic(folder: Path, day: str = "2026-10-26") -> str:
    """Two turns' rows in one day file, written as the ledger writes them, at noon that day. Returns the first
    turn's trace."""
    folder.mkdir(parents=True, exist_ok=True)
    t = datetime.fromisoformat(f"{day}T12:00:00").timestamp()
    rows = [
        {"t": t, "ev": "turn.started", "trace": "aaaaaaaaaaaaaaaa", "door": "app"},
        {"t": t + 0.3, "ev": "model", "trace": "aaaaaaaaaaaaaaaa", "door": "app", "model": "gpt-6-luna",
         "ms": 300, "in": 3000, "cached": 2900, "out": 40, "usd": 0.0004},
        {"t": t + 0.5, "ev": "call", "trace": "aaaaaaaaaaaaaaaa", "door": "app", "tool": "web_search",
         "ok": True, "ms": 200, "gated": False},
        {"t": t + 0.6, "ev": "call", "trace": "aaaaaaaaaaaaaaaa", "door": "app", "tool": "memory_search",
         "ok": True, "ms": 40, "gated": False},
        {"t": t + 0.9, "ev": "model", "trace": "aaaaaaaaaaaaaaaa", "door": "app", "model": "gpt-6-luna",
         "ms": 280, "in": 3200, "cached": 3000, "out": 60, "usd": 0.0005},
        {"t": t + 1.0, "ev": "turn.settled", "trace": "aaaaaaaaaaaaaaaa", "door": "app", "outcome": "answered",
         "rounds": 2, "total_ms": 1000},
        {"t": t + 5, "ev": "turn.started", "trace": "bbbbbbbbbbbbbbbb", "door": "telegram"},
        {"t": t + 5.1, "ev": "call", "trace": "bbbbbbbbbbbbbbbb", "door": "telegram", "tool": "web_search",
         "ok": False, "ms": 1000, "gated": False},
        {"t": t + 6, "ev": "turn.settled", "trace": "bbbbbbbbbbbbbbbb", "door": "telegram",
         "outcome": "failed", "rounds": 1, "total_ms": 1500},
    ]
    with open(folder / f"{day}.jsonl", "w", encoding="utf-8") as f:
        f.write("".join(json.dumps(r) + "\n" for r in rows) + "{torn line\n")
    return "aaaaaaaaaaaaaaaa"


def test_percentile_is_nearest_rank() -> None:
    assert turn_ledger.percentile([], 50) == 0
    assert turn_ledger.percentile([5], 95) == 5
    values = list(range(1, 101))
    assert turn_ledger.percentile(values, 50) == 50 and turn_ledger.percentile(values, 95) == 95


def test_stats_per_tool_model_and_door(tmp_path: Path) -> None:
    _synthetic(tmp_path)
    rows = turn_ledger.day_rows("2026-10-26", tmp_path)
    assert len(rows) == 9                                     # the torn line is skipped
    table = turn_ledger.stats(rows)
    tools = {line.name: line for line in table["tool"]}
    assert tools["web_search"].count == 2 and tools["web_search"].failed == 1
    assert tools["web_search"].p50 == 200 and tools["web_search"].p95 == 1000
    models = {line.name: line for line in table["model"]}
    assert models["gpt-6-luna"].count == 2 and models["gpt-6-luna"].usd == pytest.approx(0.0009)
    doors = {line.name: line for line in table["door"]}
    assert doors["app"].p50 == 1000 and doors["telegram"].failed == 1
    only = turn_ledger.stats(rows, tool="memory_search")
    assert [line.name for line in only["tool"]] == ["memory_search"]
    assert [line.name for line in only["door"]] == ["app"]    # only the turn that made one


def test_cli_turns_prints_the_table(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    _synthetic(tmp_path)
    assert turn_ledger.cli(["turns", "--day", "2026-10-26", "--dir", str(tmp_path)]) == 0
    out = capsys.readouterr().out
    assert "Turn ledger, 2026-10-26" in out and "web_search" in out and "gpt-6-luna" in out
    assert "p50 ms" in out and "p95 ms" in out
    assert turn_ledger.cli(["turns", "--day", "someday", "--dir", str(tmp_path)]) == 2
    assert turn_ledger.cli(["turns", "--day", "2026-10-25", "--dir", str(tmp_path)]) == 0
    assert "No turn ledger rows" in capsys.readouterr().out


def test_cli_turns_help(capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit) as done:
        turn_ledger.cli(["turns", "--help"])
    assert done.value.code == 0 and "--tool" in capsys.readouterr().out


def test_cli_trace_shows_the_turn_and_its_spend_rows(tmp_path: Path, capsys: pytest.CaptureFixture[str],
                                                     monkeypatch: pytest.MonkeyPatch) -> None:
    today = date.today().isoformat()
    trace = _synthetic(tmp_path, today)
    noon = datetime.fromisoformat(f"{today}T12:00:00").timestamp()
    spend.record("openrouter", "gpt-6-luna", spend.CHAT, 0.0001, now=noon)          # no turn: no trace
    with monkeypatch.context() as m:
        m.setattr(turn_ledger, "current_trace", lambda: trace)                      # made inside the turn
        spend.record("openrouter", "gpt-6-luna", spend.CHAT, 0.0004, now=noon + 0.3)

    tid, rows, days = turn_ledger.trace_rows(trace[:8], folder=tmp_path)
    assert tid == trace and len(rows) == 6 and days == [today]
    assert turn_ledger.cli(["trace", trace[:8], "--dir", str(tmp_path)]) == 0
    out = capsys.readouterr().out
    assert f"Trace {trace}" in out and "web_search ok=True" in out and "usd=0.0004" in out
    assert "Spend rows (1, $0.00040)" in out
    assert turn_ledger.cli(["trace", "abc", "--dir", str(tmp_path)]) == 1          # too short to be a trace
    assert turn_ledger.cli(["trace", "cccccccccccccccc", "--dir", str(tmp_path)]) == 1


def test_parse_day() -> None:
    today = date(2026, 10, 26)
    assert turn_ledger.parse_day("today", today) == "2026-10-26"
    assert turn_ledger.parse_day("yesterday", today) == "2026-10-25"
    assert turn_ledger.parse_day("2026-10-01", today) == "2026-10-01"
    with pytest.raises(ValueError):
        turn_ledger.parse_day("next week", today)


def test_a_small_talk_turn_is_one_model_row_and_its_outcome(folder: Path) -> None:
    from eve_bridge import small_talk

    async def lane(text: str, voice: str) -> small_talk.Answer:
        return small_talk.Answer(reply=f"hey {SENTINEL}", secs=0.4, model="anthropic/claude-haiku-4.5",
                                 cost=0.00002)

    rig = Rig(FakeApi(), FakeCreate(), small_talk_ask=lane)
    rig.inlet._chat_id = OWNER
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "hi", message_id=2), lane=True))
    assert rig.api.sent[-1] == (OWNER, f"hey {SENTINEL}") and rig.create.requests == []
    rows = all_rows(folder)
    assert [r["ev"] for r in rows] == ["turn.started", "model", "turn.settled"]
    assert rows[1]["model"] == "anthropic/claude-haiku-4.5" and rows[1]["ms"] == 400 and rows[1]["usd"] == 0.00002
    assert rows[2]["outcome"] == "small_talk"
    assert_no_sentinel(folder)


def test_a_failed_turn_settles_as_failed(folder: Path) -> None:
    rig = Rig(FakeApi(), FakeCreate(RuntimeError("provider down")))
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "what's on today", message_id=2)))
    assert rig.api.sent[-1][1] == telegram.FAILED_LINE
    assert [r["outcome"] for r in all_rows(folder) if r["ev"] == "turn.settled"] == ["failed"]
