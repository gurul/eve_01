"""``eve-bridge memory forget|dream|reindex``: the memory folder from a terminal, against a temporary store.
Counts are printed, never the matched words."""

from __future__ import annotations

from datetime import datetime
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import cli, records
from eve_bridge import dream as dream_mod
from eve_bridge import memory as memory_mod
from eve_bridge.memory import Memory
from eve_bridge.recall import RecallConfig
from eve_bridge.transcripts import TranscriptConfig, Transcripts

SECRET = "the spare key is under the blue pot"


@pytest.fixture(autouse=True)
def _no_env_file(monkeypatch: pytest.MonkeyPatch) -> None:
    """cli.main reads the owner's env file; these tests must see only what they set."""
    monkeypatch.setattr(cli, "load_env_file", lambda *a, **kw: None)


def _memory(tmp_path: Path, index: Any = None) -> Memory:
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    t = Transcripts(TranscriptConfig(enabled=True, root=cfg.transcripts_dir), archive=cfg.archive_dir)
    return Memory(cfg, t, index)


def _run(monkeypatch: pytest.MonkeyPatch, memory: Memory, argv: list[str], answer: str = "") -> int:
    monkeypatch.setattr(cli, "_open_memory_store", lambda: (memory.cfg, memory))
    monkeypatch.setattr("builtins.input", lambda prompt="": answer)
    return cli.main(["memory", *argv])


def test_forget_previews_asks_and_forgets_only_on_yes(tmp_path: Path, monkeypatch, capsys) -> None:
    memory = _memory(tmp_path)
    t = memory.transcripts
    assert t.append("telegram", t.new_conv("telegram"), "owner", "say", SECRET)

    assert _run(monkeypatch, memory, ["forget", "blue pot"], answer="n") == 0
    out = capsys.readouterr().out
    assert "1 matching lines" in out and "nothing changed" in out and SECRET not in out
    assert t.find("blue pot")                                      # a no changes nothing

    assert _run(monkeypatch, memory, ["forget", "blue pot"], answer="y") == 0
    out = capsys.readouterr().out
    assert "lines forgotten" in out and SECRET not in out
    assert t.find("blue pot") == []

    assert _run(monkeypatch, memory, ["forget", "blue pot"]) == 0
    assert "nothing in memory matches" in capsys.readouterr().out


def test_forget_with_memory_off_says_so(monkeypatch, capsys) -> None:
    for name in ("EVE_MEMORY", "EVE_RECORDS"):
        monkeypatch.delenv(name, raising=False)
    assert cli.main(["memory", "forget", "anything"]) == 1
    assert "memory: off" in capsys.readouterr().err


def test_the_store_must_be_set_up_by_the_daemon_first(tmp_path: Path, monkeypatch, capsys) -> None:
    monkeypatch.setenv("EVE_MEMORY", "1")
    monkeypatch.setenv("EVE_MEMORY_DIR", str(tmp_path / "memory"))
    assert cli._open_memory_store() is None
    assert "not set up yet" in capsys.readouterr().err
    (tmp_path / "memory").mkdir()
    (tmp_path / "memory" / memory_mod.MIGRATED_FILE).write_text("{}")
    opened = cli._open_memory_store()                               # the control: set up, it opens
    assert opened is not None and opened[0].store == tmp_path / "memory"


def test_dream_one_day(tmp_path: Path, monkeypatch, capsys) -> None:
    memory = _memory(tmp_path)
    days: list[str] = []

    class FakeDreamer:
        def __init__(self, cfg, transcripts, client, index) -> None:
            pass

        def due_nights(self, now):
            return ["2026-09-21"]

        async def dream(self, day):
            days.append(day)
            return dream_mod.DreamReport(day=day, dreamt=True, changed=2)

        _summary = staticmethod(dream_mod.Dreamer._summary)

    monkeypatch.setattr(dream_mod, "Dreamer", FakeDreamer)
    monkeypatch.setattr(records, "make_client", lambda environ=None: object())
    assert _run(monkeypatch, memory, ["dream", "--day", "2026-09-20"]) == 0
    assert _run(monkeypatch, memory, ["dream"]) == 0
    assert days == ["2026-09-20", "2026-09-21"]
    assert "2 records changed" in capsys.readouterr().out
    assert _run(monkeypatch, memory, ["dream", "--day", "yesterday"]) == 2
    monkeypatch.setattr(records, "make_client", lambda environ=None: None)
    assert _run(monkeypatch, memory, ["dream"]) == 1
    assert "no model client" in capsys.readouterr().err


def test_reindex_clears_the_ledger_and_reads_every_day(tmp_path: Path, monkeypatch, capsys) -> None:
    calls: list[Any] = []

    class FakeIndex:
        _mem = object()

        def ingest_day(self, transcripts, day):
            calls.append(day)
            return 3

        def tidy(self):
            return 1

    memory = _memory(tmp_path, FakeIndex())
    t = memory.transcripts
    for day in ("2026-09-20", "2026-09-21"):
        when = datetime.strptime(f"{day} 12:00", "%Y-%m-%d %H:%M").astimezone()
        assert t.append("voice", t.new_conv("voice"), "owner", "say", "hello", now=when)
    ledger = memory.cfg.mem0_dir / "ingested_convs"
    ledger.parent.mkdir(parents=True)
    ledger.write_text("v-1\n")
    assert _run(monkeypatch, memory, ["reindex"]) == 0
    assert calls == ["2026-09-20", "2026-09-21"] and not ledger.exists()
    assert "2 days read, 6 memories added, 1 duplicates removed" in capsys.readouterr().out
    assert _run(monkeypatch, _memory(tmp_path / "other"), ["reindex"]) == 1        # the index is off
