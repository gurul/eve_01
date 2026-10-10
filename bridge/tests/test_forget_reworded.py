"""Forget takes what the dream wrote from a forgotten line in other words, and eve's answer to the line.

Found by the memory eval's holdout (2026-10-08): with every switch off, "Honestly I failed my driving test back in
July." was forgotten, yet the records kept "failing the driving test in July" and the transcripts kept "Okay, no
driving then."; "a counselor on Tuesdays at 5" left a record named counseling-tuesday-5.
"""

from __future__ import annotations

from datetime import datetime
from pathlib import Path

from eve_bridge.memory import Memory, reworded
from eve_bridge.recall import RecallConfig
from eve_bridge.transcripts import FORGOTTEN, TranscriptConfig, Transcripts

TZ = datetime(2026, 8, 14, 12, 0).astimezone().tzinfo
SAID = "Honestly I failed my driving test back in July."


def at(hh: int, mm: int = 0) -> datetime:
    return datetime(2026, 8, 14, hh, mm, tzinfo=TZ)


def test_reworded_takes_the_dreams_words_and_leaves_what_shares_one_word() -> None:
    pred = reworded(SAID)
    assert pred is not None
    assert pred("- Avoid driving for now due to failing the driving test in July (active 2026-08-14).")
    assert not pred("- 2026-08-14 20:43 — Owner confirmed/accepted: no driving.")
    assert not pred("Uncle offered to let them drive his car.")
    counselor = reworded("I've started seeing a counselor on Tuesdays at 5.")
    assert counselor is not None
    assert counselor("- counseling-tuesday-5 (routine): counselor, counseling, therapy")
    assert not counselor("- Considering intramural volleyball on Tuesday nights.")


def test_reworded_is_off_for_a_short_or_quoted_query() -> None:
    assert reworded("failed") is None
    assert reworded("driving test") is None
    assert reworded(f'"{SAID}"') is None
    assert reworded("") is None


def build(tmp_path: Path) -> tuple[Memory, Transcripts, RecallConfig]:
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    tx = Transcripts(TranscriptConfig(enabled=True, root=cfg.transcripts_dir), cfg.archive_dir, wall=lambda: at(23))
    return Memory(cfg, tx, None), tx, cfg


def test_forget_takes_eves_answer_and_nothing_after_it(tmp_path: Path) -> None:
    _, tx, _ = build(tmp_path)
    tx.append("voice", "v-1", "owner", "say", SAID, now=at(20, 41))
    tx.append("voice", "v-1", "eve", "say", "Okay, no driving then.", now=at(20, 42))
    tx.append("voice", "v-1", "owner", "say", "What's the capital of Australia?", now=at(20, 43))
    tx.append("voice", "v-1", "eve", "say", "Canberra.", now=at(20, 44))
    tx.append("voice", "v-2", "eve", "say", "Morning.", now=at(21, 0))
    before = [ln["text"] for ln in tx.lines("2026-08-14")]
    assert tx.redact(SAID, dry_run=True) == 2
    assert [ln["text"] for ln in tx.lines("2026-08-14")] == before          # the dry run wrote nothing
    assert tx.redact(SAID) == 2
    assert [ln["text"] for ln in tx.lines("2026-08-14")] == [
        FORGOTTEN, FORGOTTEN, "What's the capital of Australia?", "Canberra.", "Morning."]


def test_eves_own_line_that_matches_does_not_take_the_next_one(tmp_path: Path) -> None:
    _, tx, _ = build(tmp_path)
    tx.append("voice", "v-1", "eve", "say", "Your zebra-sentinel code is set.", now=at(20, 0))
    tx.append("voice", "v-1", "eve", "say", "Anything else?", now=at(20, 1))
    assert tx.redact("zebra-sentinel") == 1
    assert [ln["text"] for ln in tx.lines("2026-08-14")] == [FORGOTTEN, "Anything else?"]


def test_forget_apply_removes_a_record_fact_in_other_words(tmp_path: Path) -> None:
    mem, tx, cfg = build(tmp_path)
    tx.append("voice", "v-1", "owner", "say", SAID, now=at(20, 41))
    cfg.records_dir.mkdir(parents=True)
    profile = cfg.records_dir / "profile.md"
    profile.write_text("---\nid: profile\n---\n"
                       "- Avoid driving for now due to failing the driving test in July (active 2026-08-14).\n"
                       "- Likes tea.\n", encoding="utf-8")
    assert "failing the driving test" in profile.read_text()                  # the positive control
    prev = mem.forget_preview(SAID)
    assert prev["layers"]["records"] == 1 and prev["layers"]["transcripts"] == 1
    done = mem.forget_apply(prev["token"])
    assert done["ok"] and done["layers"]["records"] == 1
    left = profile.read_text()
    assert "driving test" not in left and "Likes tea." in left
