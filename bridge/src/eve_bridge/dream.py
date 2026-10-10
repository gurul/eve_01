"""The nightly dream: once a day, eve rereads what was said and fixes what it knows.

The owner asked for it on 2026-09-23: "dream every night as well so that memories
can be fixed and updated". Until then a fact reached the records only through a
chain of per-conversation distils, a curate pass and a reconcile loop, and a
wrong fact stayed wrong until the owner edited the file by hand. Memory is now
three stores (owner, 2026-09-23): the transcripts hold every word, the records
hold what is true, mem0 finds things by meaning. The dream is the one step that
turns the first into the other two.

### What one night is

For one transcript day, in a worker thread, never on the event loop:

1. ``records.reconcile_day`` — ONE model call reads the current records, the
   profile, the owner's stars and the whole day's transcript, and returns the
   records that change, a new profile, and a journal (``days/<day>.md``). Where
   the day contradicts a record, the record gets a dated correction (with
   ``EVE_MEMORY_VALIDITY=1``: the old fact is closed by its ``valid_to``,
   at most ``records.MAX_CLOSES_PER_NIGHT`` a night, or the night is refused and
   dreamt again). That is the "fixed and updated".
2. ``records.consolidate`` — ONE model call over all records merges two ids for
   the same thing and ages out a fact a newer one replaced.
3. ``index.ingest_day`` + ``index.tidy`` — the day's conversations go into mem0,
   and exact duplicates and ★ candidates come out.
4. The day goes into ``records/.dreamt`` (one day per line) and a ``## Dream``
   section with the counts is appended to the journal, then committed, so every
   night ends with its whole journal in the records' history.

### Why the dream is the only automatic writer of records

The records keep one rule from their first day (2026-09-20): the brains read,
they never write. A model that promotes its own mid-conversation conclusions into
what it "knows" launders a guess into a fact. So nothing that talks writes a
record. The owner's own "remember that" goes to ``starred.md``, and the dream —
reading the owner's actual words, a day later, with the current records as input
so hand edits survive — is the only thing that changes a record by itself.

### Why after 04:00, and why at 04:30

A transcript day ends at 04:00, not midnight, so a conversation at 01:00 belongs
to the evening it continued. A night is dreamt only once its day is over: never
the current transcript day. The just-ended day waits until 04:30 local time
(``DREAM_HOUR``), half an hour past the rollover, so a conversation that ran over
04:00 has closed. Older nights are due at any time: that is the catch-up after
the Mac slept through 04:30. Catch-up runs oldest first, at most
``MAX_NIGHTS_PER_WAKE`` a wake, so a week away does not become one long burst of
model calls.

### Failure is silence

The dream never breaks a conversation and never raises out of ``loop``. When the
reconcile call fails, nothing is written and the night is not marked: the next
wake tries again, and the warning is logged once per night. A night that fails
``MAX_TRIES`` times in one run of the daemon is set aside until the next start,
so one bad night cannot hold up the nights after it. When consolidate or the
index fails, the night is still marked: both are retried by the next night's
dream anyway (consolidate reads all records; the index ingests each conversation
it has not seen). A day with nothing said is marked with no model call. Logs
carry days, counts and seconds, never a word that was said.

### A forget during a dream

The model call runs for minutes with the records unlocked. A forget that lands meanwhile would be undone
by the night's write — the model answered from the words before the forget — and the commit would put
them back into the history the forget had just squashed. So the night pins the forget generation before
it reads the day, and reconcile and consolidate write nothing when it moved: the night counts as a failed
reconcile and is dreamt again next wake, from what is left. mem0 guards its own adds the same way.
"""

from __future__ import annotations

import asyncio
import logging
import os
import re
import threading
import time
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from pathlib import Path
from typing import TYPE_CHECKING, Any, Callable, Optional

from . import records
from .recall import RecallConfig

if TYPE_CHECKING:
    from .mem0_memory import OwnerMemory
    from .transcripts import Transcripts

log = logging.getLogger(__name__)

DREAM_HOUR = 4.5                   # 04:30 local: half an hour after the 04:00 day rollover
MAX_NIGHTS_PER_WAKE = 3            # catch-up is oldest first, at most this many nights a wake
MAX_TRIES = 3                      # failed reconciles of one night before it waits for the next daemon start
FIRST_WAKE_SECS = 120.0            # the first look after start: soon, but not in the middle of start-up
DREAMT_FILE = ".dreamt"            # <records>/.dreamt: one dreamt day per line
DAYS_SUBDIR = "days"               # <records>/days/<day>.md: the journal reconcile_day writes
REPORT_HEADING = "## Dream"
_DAY = re.compile(r"^\d{4}-\d{2}-\d{2}$")


@dataclass
class DreamReport:
    """What one night did. Counts only: a report never carries what was said."""

    day: str
    dreamt: bool = False            # marked in .dreamt
    empty: bool = False             # nothing was said: marked with no model call
    changed: int = 0                # records the reconcile changed
    created: int = 0                # of those, new records
    corrections: int = 0            # dated corrections the day forced
    closed: int = 0                 # facts closed by a valid_to (EVE_MEMORY_VALIDITY)
    consolidated: int = 0           # records consolidate rewrote
    removed: int = 0                # records consolidate merged away
    ingested: int = 0               # memories the index added
    tidied: int = 0                 # memories the index removed
    failed: list[str] = field(default_factory=list)   # steps that failed: read, reconcile, consolidate, index, mark
    secs: float = 0.0
    journal: Optional[Path] = None  # the day's journal the counts were appended to


def _local(now: datetime) -> datetime:
    """The same moment on this Mac's wall clock. A naive datetime is taken as local already."""
    return now.astimezone() if now.tzinfo is not None else now


def _count(value: Any) -> int:
    try:
        return max(0, int(value))
    except (TypeError, ValueError):
        return 0


class Dreamer:
    """The nightly pass. ``client`` is the records' reconcile client (``records.OpenAIReconcileClient`` in the
    daemon, a fake in tests); ``index`` is the mem0 index or None; ``wall`` is the local clock."""

    def __init__(self, cfg: RecallConfig, transcripts: "Transcripts", client: Any, index: Optional["OwnerMemory"],
                 wall: Callable[[], datetime] = datetime.now) -> None:
        self.cfg = cfg
        self.transcripts = transcripts
        self.client = client
        self.index = index
        self.wall = wall
        self._night_lock = threading.Lock()          # one night at a time, whoever asks
        self._tries: dict[str, int] = {}              # failed reconciles per night, this run of the daemon
        self._loop_warned = False

    # -- the ledger ---------------------------------------------------------------------------

    @property
    def dreamt_path(self) -> Path:
        return Path(self.cfg.records_dir) / DREAMT_FILE

    def dreamt(self) -> set[str]:
        """Every day already dreamt. A missing or unreadable ledger is an empty one."""
        try:
            raw = self.dreamt_path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return set()
        return {s for s in (ln.strip() for ln in raw.splitlines()) if _DAY.match(s)}

    def _mark(self, day: str) -> bool:
        """Append the day to the ledger: one write of one whole line on an O_APPEND descriptor."""
        if day in self.dreamt():
            return True
        folder = Path(self.cfg.records_dir)
        try:
            folder.mkdir(mode=0o700, parents=True, exist_ok=True)
            fd = os.open(self.dreamt_path, os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC,
                         0o600)
            try:
                os.write(fd, f"{day}\n".encode("ascii"))
            finally:
                os.close(fd)
        except OSError as e:
            log.warning("dream: could not mark %s dreamt (%s) — it is dreamt again next wake", day, type(e).__name__)
            return False
        return True

    # -- which nights ---------------------------------------------------------------------------

    def due_nights(self, now: datetime) -> list[str]:
        """Transcript days before the current one, not yet dreamt, oldest first, at most MAX_NIGHTS_PER_WAKE.

        The just-ended calendar day waits until DREAM_HOUR; any older night is due at once (catch-up). A night
        that failed MAX_TRIES times in this run waits for the next daemon start."""
        local = _local(now)
        current = self.transcripts.day_of(local)
        yesterday = (local.date() - timedelta(days=1)).strftime("%Y-%m-%d")
        past_hour = local.hour + local.minute / 60.0 >= DREAM_HOUR
        done = self.dreamt()
        due: list[str] = []
        for day in self.transcripts.days():
            if day >= current or day in done or self._tries.get(day, 0) >= MAX_TRIES:
                continue
            if day >= yesterday and not past_hour:
                continue
            due.append(day)
            if len(due) >= MAX_NIGHTS_PER_WAKE:
                break
        return due

    # -- one night -----------------------------------------------------------------------------

    async def dream(self, day: str) -> DreamReport:
        """Dream one night, in a worker thread. Never raises: a failure is in ``report.failed``."""
        try:
            return await asyncio.to_thread(self._night, day)
        except Exception as e:  # noqa: BLE001 - the thread already guards each step; this is the last net
            log.warning("dream: %s stopped (%s)", day, type(e).__name__)
            return DreamReport(day=day, failed=["dream"])

    def _night(self, day: str) -> DreamReport:
        with self._night_lock:
            started = time.monotonic()
            report = DreamReport(day=day)
            # Pinned before the day is read: a forget from here on makes the night's writes drop, since
            # the model would answer from words that are forgotten by then (verification/Eve/Forget.lean).
            with records.forgets_pinned(self.cfg):
                self._run(day, report)
            report.secs = round(time.monotonic() - started, 1)
            if report.dreamt and not report.empty:
                self._append_report(day, report)
                if report.journal is not None:
                    # The counts land after reconcile's and consolidate's commits: one more, so the night
                    # ends with its whole journal in the records' history, never as an uncommitted edit.
                    records.commit(Path(self.cfg.records_dir), f"dream: {day} report")
            if report.dreamt:
                log.info("dream: %s — %s", day, "nothing said, no call" if report.empty else self._summary(report))
            return report

    def _run(self, day: str, report: DreamReport) -> None:
        if not _DAY.match(day or "") or not self.transcripts.enabled:
            report.failed.append("read")           # a night that was never read is never marked
            return
        text = self.transcripts.day_text(day)
        if not text.strip():
            report.empty = True
            report.dreamt = self._mark(day)
            if not report.dreamt:
                report.failed.append("mark")
            return

        try:
            change = records.reconcile_day(self.cfg, self.client, day, text)
        except Exception as e:  # noqa: BLE001 - reconcile_day promises None on failure; hold it to that
            log.debug("dream: reconcile of %s raised %s", day, type(e).__name__)
            change = None
        if change is None:
            tries = self._tries.get(day, 0) + 1
            self._tries[day] = tries
            report.failed.append("reconcile")
            if tries == 1:
                log.warning("dream: could not reconcile %s — nothing written, it is tried again next wake", day)
            elif tries >= MAX_TRIES:
                log.warning("dream: %s failed %d times — it waits for the next start", day, tries)
            return
        self._tries.pop(day, None)
        report.changed = _count(getattr(change, "changed", 0))
        report.created = _count(getattr(change, "created", 0))
        report.corrections = _count(getattr(change, "corrections", 0))
        report.closed = _count(getattr(change, "closed", 0))
        journal = getattr(change, "journal", None)
        report.journal = Path(journal) if isinstance(journal, (str, Path)) and str(journal) else None

        try:
            merged = records.consolidate(self.cfg, self.client)
            report.consolidated, report.removed = _count(merged[0]), _count(merged[1])
        except Exception as e:  # noqa: BLE001
            report.failed.append("consolidate")
            log.warning("dream: consolidate failed after %s (%s) — the next night tries again", day, type(e).__name__)

        if self.index is not None:
            try:
                report.ingested = _count(self.index.ingest_day(self.transcripts, day))
                report.tidied = _count(self.index.tidy())
            except Exception as e:  # noqa: BLE001
                report.failed.append("index")
                log.warning("dream: the index missed %s (%s) — the next night tries again", day, type(e).__name__)

        report.dreamt = self._mark(day)
        if not report.dreamt:
            report.failed.append("mark")

    @staticmethod
    def _summary(report: DreamReport) -> str:
        closed = f", {report.closed} closed" if report.closed else ""
        text = (f"{report.changed} records changed ({report.created} new, {report.corrections} corrections{closed}); "
                f"consolidate {report.consolidated} changed, {report.removed} removed; "
                f"index {report.ingested} added, {report.tidied} tidied; {report.secs:.1f} s")
        if report.failed:
            text += f"; failed: {', '.join(report.failed)}"
        return text

    def _append_report(self, day: str, report: DreamReport) -> None:
        """The counts, under the journal reconcile_day just wrote. No journal, no section."""
        journal = report.journal or Path(self.cfg.records_dir) / DAYS_SUBDIR / f"{day}.md"
        if not journal.is_file():
            report.journal = None
            return
        lines = [
            "", REPORT_HEADING, "",
            f"- records: {report.changed} changed, {report.created} new, {report.corrections} corrections"
            + (f", {report.closed} closed" if report.closed else ""),
            f"- consolidated: {report.consolidated} changed, {report.removed} removed",
            f"- index: {report.ingested} added, {report.tidied} tidied" if self.index is not None
            else "- index: off",
            f"- took {report.secs:.1f} s",
        ]
        if report.failed:
            lines.append(f"- failed: {', '.join(report.failed)}")
        try:
            with open(journal, "r+", encoding="utf-8") as f:
                body = f.read()
                f.write(("" if body.endswith("\n") or not body else "\n") + "\n".join(lines) + "\n")
            report.journal = journal
        except (OSError, UnicodeDecodeError) as e:
            report.journal = None
            log.warning("dream: could not add the counts to the %s journal (%s)", day, type(e).__name__)

    # -- the loop ------------------------------------------------------------------------------

    async def loop(self, shutdown: asyncio.Event, interval_secs: float = 1800.0) -> None:
        """Wake soon after start, then every ``interval_secs``: dream whatever nights are due. Never raises."""
        wait = min(FIRST_WAKE_SECS, interval_secs)
        while not shutdown.is_set():
            try:
                await asyncio.wait_for(shutdown.wait(), timeout=wait)
                return
            except asyncio.TimeoutError:
                pass
            wait = interval_secs
            try:
                for day in self.due_nights(self.wall()):
                    if shutdown.is_set():
                        return
                    await self.dream(day)
            except asyncio.CancelledError:
                raise
            except Exception as e:  # noqa: BLE001 - the loop outlives any one wake
                if not self._loop_warned:
                    self._loop_warned = True
                    log.warning("dream: a wake failed (%s) — the next wake tries again", type(e).__name__)
