"""Forget vs the dream: the counterexample of verification/Eve/Forget.lean, replayed on the real classes.

The Lean model found this interleaving (``current_violates``): the dream reads the day and the records, a
forget runs to completion while the dream's model call is out, and the dream then writes what the model
made of the pre-forget input — and commits it after the forget squashed the history. The same shape for
mem0: the index reads a conversation, the forget runs, the index adds what it read. Each test drives that
interleaving deterministically: the fake model call (or the transcript read) runs ``forget_apply`` itself.
"""

from __future__ import annotations

import asyncio
import json
import subprocess
from datetime import datetime
from pathlib import Path
from typing import Any

from eve_bridge import records
from eve_bridge.dream import Dreamer
from eve_bridge.mem0_memory import Mem0Config, OwnerMemory
from eve_bridge.memory import Memory
from eve_bridge.recall import RecallConfig
from eve_bridge.transcripts import TranscriptConfig, Transcripts

TZ = datetime(2026, 9, 21, 12, 0).astimezone().tzinfo
SENTINEL = "zebra-sentinel-7Q"
DAY = "2026-09-21"


def at(hh: int, mm: int = 0) -> datetime:
    return datetime(2026, 9, 21, hh, mm, tzinfo=TZ)


def build(tmp_path: Path, index: Any = None) -> tuple[Memory, Transcripts, RecallConfig]:
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    tx = Transcripts(TranscriptConfig(enabled=True, root=cfg.transcripts_dir), cfg.archive_dir,
                     wall=lambda: datetime(2026, 9, 23, 12, 0, tzinfo=TZ))
    conv = f"v-{int(at(9).timestamp() * 1000)}-aaaa"
    tx.append("voice", conv, "owner", "say", f"my cat {SENTINEL} is asleep", now=at(9))
    tx.append("voice", conv, "eve", "say", "Sweet dreams to the cat.", now=at(9, 1))
    return Memory(cfg, tx, index), tx, cfg


def everything_in(folder: Path) -> str:
    """Every file in the records folder, and every commit's patch in its history."""
    text = "".join(p.read_text(encoding="utf-8", errors="replace")
                   for p in folder.rglob("*") if p.is_file() and ".git" not in p.parts)
    if (folder / ".git").is_dir():
        text += subprocess.run(["git", "-C", str(folder), "log", "--all", "-p"],
                               capture_output=True, text=True).stdout
    return text


class ForgetsMidCall:
    """The dream's model client. Its reconcile call is where the owner's forget lands: it confirms the
    forget (the whole forget_apply runs) and then answers from the body it was given, which still holds
    the words."""

    def __init__(self, memory: Memory, token: str) -> None:
        self.memory = memory
        self.token = token
        self.forgot: dict[str, Any] = {}

    def reconcile(self, body: str) -> str:
        assert SENTINEL in body                                    # the model saw the pre-forget day
        self.forgot = self.memory.forget_apply(self.token)
        return json.dumps({
            "records": [{"id": "pets", "type": "person", "aliases": ["cat"],
                         "facts": [f"Has a cat called {SENTINEL} (said {DAY})."]}],
            "profile": {"summary": f"Owns a cat, {SENTINEL}."},
            "journal": {"happened": [f"Talked about the cat {SENTINEL}."]},
        })

    def consolidate(self, body: str) -> str:
        return json.dumps({"records": [], "merged": []})


def test_a_forget_during_the_dreams_model_call_is_not_undone_by_the_dream(tmp_path: Path) -> None:
    memory, tx, cfg = build(tmp_path)
    preview = memory.forget_preview(SENTINEL)
    assert preview["ok"] and preview["total"] >= 1               # control: there is something to forget
    client = ForgetsMidCall(memory, preview["token"])
    dreamer = Dreamer(cfg, tx, client, index=None)

    asyncio.run(dreamer.dream(DAY))

    assert client.forgot.get("ok") is True and client.forgot["total"] >= 1   # the forget ran, mid-call
    folder = records.records_dir(cfg)
    assert (folder / ".git").is_dir()                            # control: the history is really checked
    assert SENTINEL not in everything_in(folder)
    assert SENTINEL not in tx.path_of(DAY).read_text()


def test_the_dream_after_the_forget_writes_again_from_what_is_left(tmp_path: Path) -> None:
    """The fix drops a night a forget overlapped; the next night reads the redacted day and is written."""
    memory, tx, cfg = build(tmp_path)
    client = ForgetsMidCall(memory, memory.forget_preview(SENTINEL)["token"])
    dreamer = Dreamer(cfg, tx, client, index=None)
    asyncio.run(dreamer.dream(DAY))

    class Plain:
        def reconcile(self, body: str) -> str:
            assert SENTINEL not in body
            return json.dumps({"records": [{"id": "pets", "type": "person", "aliases": [],
                                            "facts": ["Has a cat (said 2026-09-21)."]}],
                               "profile": {}, "journal": {"happened": ["Talked about the cat."]}})

        def consolidate(self, body: str) -> str:
            return json.dumps({"records": [], "merged": []})

    dreamer.client = Plain()
    report = asyncio.run(dreamer.dream(DAY))
    assert report.dreamt is True
    assert "Has a cat" in (records.records_dir(cfg) / "pets.md").read_text()
    assert SENTINEL not in everything_in(records.records_dir(cfg))


class RowsMem0:
    """mem0 in RAM that keeps what it is given as rows, so find and forget see it."""

    def __init__(self) -> None:
        self.rows: list[dict[str, Any]] = []

    def add(self, messages: list[dict[str, str]], **kw: Any) -> dict[str, Any]:
        self.rows.append({"id": f"m{len(self.rows) + 1}", "memory": " ".join(m["content"] for m in messages)})
        return {"results": []}

    def get_all(self, **kw: Any) -> dict[str, Any]:
        return {"results": list(self.rows)}

    def delete(self, memory_id: str) -> dict[str, Any]:
        self.rows = [r for r in self.rows if r["id"] != memory_id]
        return {"message": "ok"}


class ForgetsAfterRead:
    """The transcripts as the index reads them: the forget runs right after a conversation is read."""

    def __init__(self, tx: Transcripts, memory: Memory, token: str) -> None:
        self.tx, self.memory, self.token = tx, memory, token
        self.forgot: dict[str, Any] = {}

    def conversations(self, day: str) -> Any:
        return self.tx.conversations(day)

    def conv_lines(self, conv: str) -> Any:
        lines = self.tx.conv_lines(conv)
        if not self.forgot:
            self.forgot = self.memory.forget_apply(self.token)
        return lines


def test_a_forget_between_the_indexs_read_and_its_add_is_not_undone(tmp_path: Path) -> None:
    fake = RowsMem0()
    index = OwnerMemory(Mem0Config(enabled=True, home=tmp_path / "memory" / "mem0"), factory=lambda _c: fake)
    memory, tx, cfg = build(tmp_path, index)
    preview = memory.forget_preview(SENTINEL)
    assert preview["ok"] and preview["layers"]["transcripts"] == 2
    reader = ForgetsAfterRead(tx, memory, preview["token"])

    index.ingest_day(reader, DAY)

    assert reader.forgot.get("ok") is True                        # the forget ran between read and add
    assert all(SENTINEL not in r["memory"] for r in fake.rows)
    # control: with nothing forgotten in between, the same day does go in (from the redacted words)
    index.ingest_day(tx, DAY)
    assert fake.rows and all(SENTINEL not in r["memory"] for r in fake.rows)
