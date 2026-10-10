"""memory_sources.py: Sources under a reply. What a turn's memory reads found rides on its reply's transcript line
(``src``) and reaches the app as ``sources`` (link_chat._row); a turn that read nothing writes none; the model's
requests are the same bytes with the feature as without it; forget scrubs a source that quotes the words.

Every fixture is made up: no real names, ids or words."""
from __future__ import annotations

import asyncio
import json
from datetime import datetime
from pathlib import Path
from typing import Any

import pytest
from test_telegram import FakeApi, FakeCreate, FakeMemory, Rig, _in, call, say

from eve_bridge import link_chat, memory_sources
from eve_bridge.link_chat import read_messages
from eve_bridge.transcripts import FORGOTTEN, TranscriptConfig, Transcripts

# memory.Memory.search's shape: record facts, recalled meanings, and the words said
SEARCH_RESULT: dict[str, Any] = {
    "ok": True,
    "hits": [{"id": "plumber", "line": "comes on Fridays", "src": "2026-09-30"}],
    "recalled": ["(2026-10-01 09:12, voice) the plumber prefers mornings"],
    "said": [{"day": "2026-10-02", "time": "18:40", "ch": "telegram", "who": "owner",
              "text": "the plumber is booked for Friday", "before": "", "after": ""}],
}


class SearchMemory(FakeMemory):
    """FakeMemory whose memory_search answers as memory.Memory.search does."""

    def handle_tool(self, name: str, args: dict[str, Any], *, since: Any = None, channel: str = "") -> dict[str, Any]:
        self.calls.append((name, args, channel))
        if name == "memory_search":
            return json.loads(json.dumps(SEARCH_RESULT))
        return {"ok": True}


def replies(memory: FakeMemory) -> list[dict[str, Any]]:
    return [ln for ln in memory.said() if ln["who"] == "eve" and ln["kind"] == "say"]


# ---- the pure parts ------------------------------------------------------------------------------------------

def test_a_search_result_becomes_sources_of_each_kind_in_turn() -> None:
    got = memory_sources.from_result("memory_search", SEARCH_RESULT)
    assert got == [
        {"kind": "fact", "text": "plumber: comes on Fridays", "when": "2026-09-30"},
        {"kind": "remembered", "text": "the plumber prefers mornings", "when": "2026-10-01 09:12, voice"},
        {"kind": "said", "text": "You: the plumber is booked for Friday", "when": "2026-10-02 18:40"},
    ]


def test_a_read_is_a_fact_or_a_stretch_said_and_a_failure_is_nothing() -> None:
    assert memory_sources.from_result("memory_read", {"ok": True, "record": "# plumber\n- comes on Fridays"}) == [
        {"kind": "fact", "text": "# plumber - comes on Fridays"}]
    assert memory_sources.from_result("memory_read", {"ok": True, "day": "2026-10-02", "text": "18:40 Owner: hi"}) == [
        {"kind": "said", "text": "18:40 Owner: hi", "when": "2026-10-02"}]
    assert memory_sources.from_result("memory_search", {"ok": False, "reason": "x"}) == []
    assert memory_sources.from_result("forget_preview", SEARCH_RESULT) == []
    assert memory_sources.from_result("memory_search", "not a dict") == []


def test_sources_are_capped_in_number_and_length_and_never_repeat() -> None:
    many = {"ok": True, "hits": [{"id": f"r{i}", "line": "x" * 500} for i in range(10)]}
    got = memory_sources.from_result("memory_search", many)
    assert len(got) == memory_sources.MAX_SOURCES
    assert all(len(s["text"]) <= memory_sources.MAX_TEXT_CHARS for s in got)
    kept: list[dict[str, str]] = []
    memory_sources.add(kept, got[:2])
    memory_sources.add(kept, got[:3])
    assert kept == got[:3]


def test_sanitize_keeps_well_formed_entries_only() -> None:
    raw = [{"kind": "fact", "text": "a"}, {"kind": "evil", "text": "b"}, {"kind": "said", "text": 3},
           "nope", {"kind": "said", "text": "c", "when": 5}, {"kind": "remembered", "text": " ", "when": "x"},
           *({"kind": "fact", "text": f"n{i}", "when": "w" * 99} for i in range(10))]
    got = memory_sources.sanitize(raw)
    assert got[:2] == [{"kind": "fact", "text": "a"}, {"kind": "said", "text": "c"}]
    assert len(got) == memory_sources.MAX_SOURCES
    assert all(len(s.get("when", "")) <= memory_sources.MAX_WHEN_CHARS for s in got)
    assert memory_sources.sanitize("x") == [] and memory_sources.sanitize(None) == []


# ---- the daemon ----------------------------------------------------------------------------------------------

def test_a_turn_that_searched_memory_writes_src_on_its_reply_and_one_that_did_not_writes_none(tmp_path: Path) -> None:
    memory = SearchMemory(tmp_path)
    rig = Rig(FakeApi(), FakeCreate(call("memory_search", {"query": "plumber", "days_back": None}),
                                    say("On Friday, in the morning."), say("Hello.")), memory=memory)

    async def go() -> None:
        await rig.inlet._turn(_in("when does the plumber come?", 1))
        await rig.inlet._turn(_in("hi", 2))

    asyncio.run(go())
    first, second = replies(memory)
    assert first["text"] == "On Friday, in the morning."
    assert [s["kind"] for s in first["src"]] == ["fact", "remembered", "said"]
    assert second["text"] == "Hello." and "src" not in second
    assert not any("src" in ln for ln in memory.said() if ln is not first)       # only the reply that read memory


def test_the_model_requests_are_byte_identical_with_and_without_sources(tmp_path: Path,
                                                                        monkeypatch: pytest.MonkeyPatch) -> None:
    def run(root: Path) -> tuple[list[str], list[dict[str, Any]]]:
        memory = SearchMemory(root)
        rig = Rig(FakeApi(), FakeCreate(call("memory_search", {"query": "plumber", "days_back": None}),
                                        say("On Friday."), say("You are welcome.")), memory=memory)

        async def go() -> None:
            await rig.inlet._turn(_in("when does the plumber come?", 1))
            await rig.inlet._turn(_in("thanks", 2))      # its request shows the first reply in the history

        asyncio.run(go())
        return [json.dumps(r, sort_keys=True) for r in rig.create.requests], replies(memory)

    with_sources, lines_on = run(tmp_path / "on")
    monkeypatch.setattr(memory_sources, "from_result", lambda name, result: [])
    without, lines_off = run(tmp_path / "off")
    assert "src" in lines_on[0] and "src" not in lines_off[0]                    # the switch did switch
    assert len(with_sources) == 3 and with_sources == without
    assert "comes on Fridays" in with_sources[1]                                 # the tool result, as returned
    assert "Sources" not in with_sources[2] and '"src"' not in with_sources[2]


def test_a_reply_written_without_a_turn_keeps_no_sources(tmp_path: Path) -> None:
    memory = SearchMemory(tmp_path)
    rig = Rig(FakeApi(), FakeCreate(say("x")), memory=memory)
    rig.inlet._note("eve", "a reminder", src=None)
    assert "src" not in replies(memory)[0]


# ---- the transcript and forget -------------------------------------------------------------------------------

def store_at(tmp_path: Path) -> Transcripts:
    store = Transcripts(TranscriptConfig(enabled=True, root=tmp_path / "transcripts"))
    assert store.enabled
    return store


def test_forget_takes_a_source_that_quotes_the_words_and_keeps_the_reply(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    now = datetime(2026, 10, 9, 12, 0).astimezone()
    src = [{"kind": "said", "text": "You: the gate code is blue"}, {"kind": "fact", "text": "plumber: Fridays"}]
    store.append("telegram", "t-1", "eve", "say", "Here is what I have.", now=now, src=src)
    store.append("telegram", "t-1", "eve", "say", "Another line.", now=now,
                 src=[{"kind": "said", "text": "the gate code is blue"}])
    assert store.redact("gate code") == 2
    first, second = store.lines(store.day_of(now))
    assert first["text"] == "Here is what I have." and first["src"] == [{"kind": "fact", "text": "plumber: Fridays"}]
    assert second["text"] == "Another line." and "src" not in second
    assert "gate" not in (tmp_path / "transcripts").joinpath(store.path_of(store.day_of(now)).name).read_text()


def test_a_forgotten_reply_keeps_no_sources(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    now = datetime(2026, 10, 9, 12, 0).astimezone()
    store.append("telegram", "t-1", "eve", "say", "the boat is green", now=now,
                 src=[{"kind": "fact", "text": "boat: blue hull"}])
    store.redact("boat green")
    (line,) = store.lines(store.day_of(now))
    assert line["text"] == FORGOTTEN and "src" not in line


# ---- the app's row -------------------------------------------------------------------------------------------

def test_the_app_row_carries_sources_on_eve_lines_only_sanitized(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    now = datetime(2026, 10, 9, 12, 0).astimezone()
    store.append("telegram", "t-1", "owner", "say", "when?", now=now)
    store.append("telegram", "t-1", "eve", "say", "Friday.", now=now,
                 src=[{"kind": "fact", "text": "plumber: Fridays", "when": "2026-09-30"}])
    store.append("telegram", "t-1", "eve", "say", "Anything else?", now=now)
    rows, _ = read_messages(store)
    assert "sources" not in rows[0] and "sources" not in rows[2]
    assert rows[1]["sources"] == [{"kind": "fact", "text": "plumber: Fridays", "when": "2026-09-30"}]
    # a hand-edited or older line: only well-formed entries reach the app, and never on an owner line
    line = {"ts": now.isoformat(), "ch": "telegram", "conv": "t-1", "who": "eve", "kind": "say", "text": "x",
            "src": [{"kind": "fact", "text": "ok"}, {"kind": "script", "text": "<b>"}, 7]}
    assert link_chat._row("2026-10-09", 0, line)["sources"] == [{"kind": "fact", "text": "ok"}]
    assert "sources" not in link_chat._row("2026-10-09", 0, {**line, "who": "owner"})
    assert "sources" not in link_chat._row("2026-10-09", 0, {**line, "src": "nope"})
