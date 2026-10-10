"""recall.py: where memory lives, and the one clause a conversation opens with.

The brief reads the transcripts' last turn and, when that was before today, the dream journal's title for
that day. Everything here is a file read against a temporary store. The refusals matter most: memory off,
nothing said yet, a clock gone backwards, a journal from another day, a forgotten title.
"""

from __future__ import annotations

import os
from datetime import datetime, timedelta
from pathlib import Path

import pytest

from eve_bridge import recall as recall_mod
from eve_bridge.recall import (
    MAX_BRIEF_CHARS,
    RecallConfig,
    configured,
    legacy_mem0,
    legacy_store,
    opening_brief,
)
from eve_bridge.transcripts import TranscriptConfig, Transcripts
from eve_bridge.voice_agent import VoiceConfig, memory_block, session_config

TZ = datetime(2026, 9, 11, 12, 0).astimezone().tzinfo
NOW = datetime(2026, 9, 11, 21, 30, tzinfo=TZ)


def _cfg(tmp_path: Path) -> RecallConfig:
    return RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")


def _store(cfg: RecallConfig, enabled: bool = True) -> Transcripts:
    return Transcripts(TranscriptConfig(enabled=enabled, root=cfg.transcripts_dir), wall=lambda: NOW)


def _said(t: Transcripts, when: datetime, ch: str = "voice") -> None:
    assert t.append(ch, f"{ch[0]}-1726000000000-abcd", "owner", "say", "hello there", now=when)


def _journal(cfg: RecallConfig, day: str, title: str) -> Path:
    path = cfg.records_dir / "days" / f"{day}.md"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"---\nday: {day}\ntitle: {title}\n---\n\n# {title}\n", encoding="utf-8")
    return path


# ---- nothing to say --------------------------------------------------------------------

def test_memory_off_says_nothing(tmp_path: Path) -> None:
    """Memory off: no brief, so the prompt is byte-identical to a memory-less session."""
    cfg = _cfg(tmp_path)
    on = _store(cfg)
    _said(on, NOW - timedelta(days=1))
    assert opening_brief(cfg, on, NOW) == "You last talked yesterday."        # the control
    assert opening_brief(cfg, None, NOW) == ""
    assert opening_brief(cfg, _store(cfg, enabled=False), NOW) == ""


def test_an_empty_or_missing_store_says_nothing(tmp_path: Path) -> None:
    """The first ever conversation must sound exactly like today. A robot that announces it has no
    memories is narrating its own plumbing."""
    assert opening_brief(_cfg(tmp_path), _store(_cfg(tmp_path)), NOW) == ""
    cfg = RecallConfig(store=tmp_path / "nope" / "memory", notes=tmp_path / "nope" / "notes")
    assert opening_brief(cfg, _store(cfg), NOW) == ""


# ---- the gap -----------------------------------------------------------------------------

def test_the_gap_is_named_the_way_a_person_would(tmp_path: Path) -> None:
    for i, (delta, expected) in enumerate((
        (timedelta(hours=4), "earlier today"),
        (timedelta(days=1), "yesterday"),
        (timedelta(days=3), "3 days ago"),
        (timedelta(days=9), "last week"),
        (timedelta(days=40), "a while ago"),
    )):
        cfg = RecallConfig(store=tmp_path / str(i), notes=tmp_path / "notes")
        t = _store(cfg)
        _said(t, NOW - delta)
        assert opening_brief(cfg, t, NOW) == f"You last talked {expected}.", expected


def test_a_conversation_minutes_ago_is_not_worth_mentioning(tmp_path: Path) -> None:
    """Back-to-back conversations are one conversation to a person."""
    cfg = _cfg(tmp_path)
    t = _store(cfg)
    _said(t, NOW - timedelta(minutes=2))
    assert opening_brief(cfg, t, NOW) == ""


def test_a_future_last_turn_is_silence_not_an_error(tmp_path: Path) -> None:
    """A clock that went backwards, or a restored backup."""
    cfg = _cfg(tmp_path)
    t = _store(cfg)
    _said(t, NOW + timedelta(days=2))
    assert opening_brief(cfg, t, NOW) == ""


def test_either_channel_counts_but_a_bus_line_does_not(tmp_path: Path) -> None:
    """A text is talking too; a line sent over the memory bus is not the owner talking."""
    cfg = _cfg(tmp_path)
    t = _store(cfg)
    _said(t, NOW - timedelta(days=3), ch="telegram")
    assert opening_brief(cfg, t, NOW) == "You last talked 3 days ago."
    assert t.append("bus", "b-1726000000000-abcd", "system", "tool", "a line from the bus", tool="remember",
                    now=NOW - timedelta(hours=5))
    fresh = _store(cfg)                                   # a fresh reader scans the files, not the cache
    assert opening_brief(cfg, fresh, NOW) == "You last talked 3 days ago."


def test_a_broken_store_is_silence(tmp_path: Path) -> None:
    class Broken:
        enabled = True

        def last_turn_at(self):
            raise OSError("disk gone")

    assert opening_brief(_cfg(tmp_path), Broken(), NOW) == ""


# ---- the journal title ------------------------------------------------------------------

def test_the_journal_title_of_the_last_day_rides_along(tmp_path: Path) -> None:
    cfg = _cfg(tmp_path)
    t = _store(cfg)
    _said(t, NOW - timedelta(days=1))
    _journal(cfg, "2026-09-10", "Planning the weekend trip")
    assert opening_brief(cfg, t, NOW) == "You last talked yesterday, about: Planning the weekend trip."


def test_a_journal_from_another_day_is_not_quoted(tmp_path: Path) -> None:
    """The dream has not run for the last day yet: the newest journal is an older day, and saying the
    last talk was about it would be wrong."""
    cfg = _cfg(tmp_path)
    t = _store(cfg)
    _said(t, NOW - timedelta(days=1))
    _journal(cfg, "2026-09-08", "Something older")
    assert opening_brief(cfg, t, NOW) == "You last talked yesterday."


def test_a_talk_earlier_today_never_carries_a_title(tmp_path: Path) -> None:
    cfg = _cfg(tmp_path)
    t = _store(cfg)
    _said(t, NOW - timedelta(hours=4))
    _journal(cfg, "2026-09-11", "Today so far")
    assert opening_brief(cfg, t, NOW) == "You last talked earlier today."


@pytest.mark.parametrize("title", ["(forgotten)", "2026-09-10", "Did we fix the servo?"])
def test_a_forgotten_bare_or_questioning_title_is_left_out(tmp_path: Path, title: str) -> None:
    cfg = _cfg(tmp_path)
    t = _store(cfg)
    _said(t, NOW - timedelta(days=1))
    _journal(cfg, "2026-09-10", title)
    assert opening_brief(cfg, t, NOW) == "You last talked yesterday."


def test_a_long_brief_is_cut_to_length_on_a_word(tmp_path: Path) -> None:
    cfg = _cfg(tmp_path)
    t = _store(cfg)
    _said(t, NOW - timedelta(days=1))
    _journal(cfg, "2026-09-10", "a very long day about the servos " * 12)
    brief = opening_brief(cfg, t, NOW)
    assert len(brief) <= MAX_BRIEF_CHARS
    assert brief.startswith("You last talked yesterday, about: a very long day")
    assert brief.endswith(".") and not brief.endswith(" .")


def test_the_stamp_file_is_gone() -> None:
    """The last-conversation stamp is retired: the transcripts know when the owner last talked."""
    for name in ("note_conversation_time", "STAMP_FILE", "_newest_eve_note", "_newest_daily"):
        assert not hasattr(recall_mod, name), name
    assert not hasattr(RecallConfig, "stamp_path") and not hasattr(RecallConfig, "sessions_dir")


# ---- how it reaches the model -----------------------------------------------------------

def test_no_memory_leaves_the_prompt_byte_identical() -> None:
    """The safety property of the whole phase: an empty brief cannot change how eve behaves."""
    plain = session_config(VoiceConfig())
    assert session_config(VoiceConfig(), "") == plain
    assert session_config(VoiceConfig(), "   ") == plain
    assert memory_block("") == "" and memory_block("  ") == ""


def test_a_brief_reaches_the_prompt_with_the_rules_that_keep_it_short() -> None:
    cfg = session_config(VoiceConfig(), "You last talked yesterday, about: the servo order")
    text = cfg["instructions"]
    assert "You last talked yesterday" in text
    assert "the servo order" in text
    for rule in ("one clause", "never as a separate announcement", "what they said",
                 "never ask them to confirm"):
        assert rule in text, rule


def test_captions_mode_still_gets_its_own_instructions_after_the_memory() -> None:
    """Order matters: the caption rules are last, so they are the final thing the model reads."""
    text = session_config(VoiceConfig(output="captions"), "You last talked yesterday.")["instructions"]
    assert text.index("You last talked yesterday") < text.index("17 characters")


# ---- where it lives ----------------------------------------------------------------------

def test_configured_reads_the_memory_dir_and_expands_the_paths() -> None:
    cfg = configured({"EVE_MEMORY_DIR": "~/x/memory", "EVE_NOTES_DIR": "~/x/notes"})
    assert cfg.store.is_absolute() and cfg.store.name == "memory" and "~" not in str(cfg.store)
    assert cfg.transcripts_dir == cfg.store / "transcripts" and cfg.records_dir == cfg.store / "records"
    assert cfg.mem0_dir == cfg.store / "mem0" and cfg.archive_dir == cfg.store / "archive"
    default = configured({})
    assert default.store == Path(os.path.expanduser("~/.config/eve-bridge/memory"))
    assert default.notes.name == "notes"
    # the retired store's variable no longer moves the memory root: it only names the migration's source
    assert configured({"EVE_DEBRIEF_DIR": "~/x/debrief"}).store == default.store


def test_the_legacy_locations_are_the_migrations_source() -> None:
    assert legacy_store({}) == Path(os.path.expanduser("~/.config/eve-bridge/debrief"))
    assert legacy_store({"EVE_DEBRIEF_DIR": "~/x/debrief"}) == Path(os.path.expanduser("~/x/debrief"))
    assert legacy_mem0() == Path(os.path.expanduser("~/.config/eve-bridge/mem0"))
