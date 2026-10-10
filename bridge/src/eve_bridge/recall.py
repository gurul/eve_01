"""What eve carries from one conversation into the next: where memory lives, and the opening brief.

eve already remembers what it SEES: diary.py keeps a memory stream of looks. It remembered nothing of
what was SAID, and every conversation opened from nothing (owner request 2026-09-11). This module is the
cheapest part of the fix: the place memory lives on disk, and the one clause a conversation opens with.

### Where the memory lives

One folder, ``~/.config/eve-bridge/memory`` (``EVE_MEMORY_DIR`` overrides). The owner asked for
fewer stores (owner, 2026-09-23: "simplify the memory system — too many stores"), so there are three and an
archive, each a property of ``RecallConfig``:

    transcripts/   every word, written the moment it is said (transcripts.py): the source
    records/       what is true about the owner, rewritten nightly by the dream (records.py, dream.py)
    mem0/          a search by meaning, rebuilt from the transcripts (mem0_memory.py)
    archive/       the retired debrief store, moved here once (memory.migrate), read-only, still searched

The retired store's own location is still read, once, as the source of that move:
``EVE_DEBRIEF_DIR`` or ``~/.config/eve-bridge/debrief`` (``legacy_store``), and the old mem0
home ``~/.config/eve-bridge/mem0`` (``legacy_mem0``).

### Why the opening brief does not search

A conversation opens on a recency question, and similarity has no clock (measured 2026-09-11: asked "what
did we talk about yesterday", a semantic search returned a note about opening lines). So the brief reads
the newest things only: when the transcripts last saw a word said, and, when that was before today, the
title of the dream's journal for that day. Search is left to the ``memory_search`` tool, mid-conversation.

### The two rules that keep it from being creepy

1. **At most one clause.** ``opening_brief`` returns one short sentence or nothing. A robot that recites a
   dossier at you is worse company than one that forgot.
2. **What eve said is quotable; what eve saw is not.** This module reads only the spoken memory.

Silence is a valid answer, and it is the right one with memory off and on the first ever conversation: the
brief is "", the instructions are byte-identical to a memory-less session, and eve greets exactly as it
always did. It never announces that it has no memories.
"""

from __future__ import annotations

import logging
import os
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import TYPE_CHECKING, Any, Optional

if TYPE_CHECKING:
    from .transcripts import Transcripts

log = logging.getLogger(__name__)

DEFAULT_STORE = "~/.config/eve-bridge/memory"
DEFAULT_NOTES = "~/.config/eve-bridge/notes"
LEGACY_STORE = "~/.config/eve-bridge/debrief"     # the retired debrief store: read once, by the migration
LEGACY_MEM0 = "~/.config/eve-bridge/mem0"         # the retired mem0 home: read once, by the migration
MAX_BRIEF_CHARS = 240
RECENT_SECS = 900                                      # a talk this recent is the same conversation to a person
FORGOTTEN = "(forgotten)"                              # a journal title forget redacted: never quoted


@dataclass(frozen=True)
class RecallConfig:
    store: Path
    notes: Path

    # The simplified memory (owner, 2026-09-23): three stores and an archive under one root.
    @property
    def transcripts_dir(self) -> Path:
        return self.store / "transcripts"

    @property
    def records_dir(self) -> Path:
        return self.store / "records"

    @property
    def mem0_dir(self) -> Path:
        return self.store / "mem0"

    @property
    def archive_dir(self) -> Path:
        return self.store / "archive"


def configured(environ: Any = None) -> RecallConfig:
    env = os.environ if environ is None else environ
    store = (env.get("EVE_MEMORY_DIR") or "").strip() or DEFAULT_STORE
    notes = (env.get("EVE_NOTES_DIR") or "").strip() or DEFAULT_NOTES
    return RecallConfig(store=Path(store).expanduser(), notes=Path(notes).expanduser())


def legacy_store(environ: Any = None) -> Path:
    """Where the retired debrief store is: the source ``memory.migrate`` moves into the archive."""
    env = os.environ if environ is None else environ
    return Path((env.get("EVE_DEBRIEF_DIR") or "").strip() or LEGACY_STORE).expanduser()


def legacy_mem0() -> Path:
    """Where the retired mem0 index lived, before it moved under the memory root."""
    return Path(LEGACY_MEM0).expanduser()


# ---- the brief ---------------------------------------------------------------------------------

def _local(when: datetime) -> datetime:
    """An aware local datetime. A naive one is taken as local already."""
    return when.astimezone()


def _gap_clause(then: datetime, now: datetime) -> str:
    """"earlier today" / "yesterday" / "N days ago", or "" when unknowable or too recent to mention.

    A clock that has gone backwards is the same answer as no clock: say nothing about time."""
    secs = (now - then).total_seconds()
    if secs < 0:
        return ""
    days = (now.date() - then.date()).days
    if days <= 0:
        return "" if secs < RECENT_SECS else "earlier today"
    if days == 1:
        return "yesterday"
    if days <= 6:
        return f"{days} days ago"
    if days <= 13:
        return "last week"
    return "a while ago"


def _journal_title(cfg: RecallConfig, day: str) -> str:
    """The dream journal's title for `day`, or "" when that day has no journal (yet) or it was forgotten."""
    from . import records

    latest = records.latest_day(cfg)
    if latest is None or latest[0] != day:
        return ""
    title = " ".join(str(latest[1] or "").split()).rstrip(".")
    if not title or title == day or FORGOTTEN in title or title.endswith("?"):
        return ""
    return title


def opening_brief(cfg: RecallConfig, transcripts: Optional["Transcripts"],
                  now: Optional[datetime] = None) -> str:
    """One short sentence for the top of a conversation, or "".

    The gap comes first because it is what a person notices. When the last talk was before today and the
    dream has written that day's journal, its title rides along: "You last talked yesterday, about the
    trip." Never a question, never longer than MAX_BRIEF_CHARS. "" when memory is off (no transcripts)."""
    if transcripts is None or not getattr(transcripts, "enabled", False):
        return ""
    try:
        last = transcripts.last_turn_at()
        if last is None:
            return ""
        then, when = _local(last), _local(now or datetime.now())
        gap = _gap_clause(then, when)
        if not gap:
            return ""
        title = ""
        if then.date() < when.date():
            title = _journal_title(cfg, transcripts.day_of(then))
    except Exception as e:  # noqa: BLE001 - a brief is a nicety: never a failed conversation
        log.debug("recall: brief unavailable (%s)", type(e).__name__)
        return ""
    brief = f"You last talked {gap}, about: {title}." if title else f"You last talked {gap}."
    if len(brief) > MAX_BRIEF_CHARS:
        brief = brief[:MAX_BRIEF_CHARS - 1].rsplit(" ", 1)[0].rstrip(",;:") + "."
    return brief
