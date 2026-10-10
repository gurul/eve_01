"""dream.py: the nightly dream — which nights are due, one night's steps, failure rules, the loop."""

from __future__ import annotations

import asyncio
import logging
import threading
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any, Optional

import pytest

from eve_bridge import dream as dream_mod
from eve_bridge import records
from eve_bridge.dream import DREAM_HOUR, MAX_NIGHTS_PER_WAKE, MAX_TRIES, Dreamer
from eve_bridge.recall import RecallConfig
from eve_bridge.transcripts import TranscriptConfig, Transcripts

TZ = datetime(2026, 9, 21, 12, 0).astimezone().tzinfo
SENTINEL = "zebra-sentinel-7Q"


def at(day: int, hh: int, mm: int = 0) -> datetime:
    return datetime(2026, 9, day, hh, mm, tzinfo=TZ)


class Clock:
    def __init__(self, now: datetime) -> None:
        self.t = now

    def __call__(self) -> datetime:
        return self.t


@dataclass
class FakeChange:
    changed: int
    created: int
    corrections: int
    journal: Path


class Calls:
    """Stands in for records.reconcile_day / records.consolidate and the mem0 index, and logs the order."""

    def __init__(self, cfg: RecallConfig, fail_days: Optional[set[str]] = None) -> None:
        self.cfg = cfg
        self.order: list[str] = []
        self.fail_days = fail_days if fail_days is not None else set()
        self.texts: dict[str, str] = {}
        self.threads: list[str] = []

    def reconcile_day(self, cfg: RecallConfig, client: Any, day: str, day_text: str) -> Optional[FakeChange]:
        self.order.append(f"reconcile {day}")
        self.threads.append(threading.current_thread().name)
        self.texts[day] = day_text
        if day in self.fail_days:
            return None
        journal = Path(cfg.records_dir) / "days" / f"{day}.md"
        journal.parent.mkdir(parents=True, exist_ok=True)
        journal.write_text(f"# {day}\n\n## Happened\n\n- a thing\n", encoding="utf-8")
        return FakeChange(changed=2, created=1, corrections=1, journal=journal)

    def consolidate(self, cfg: RecallConfig, client: Any) -> tuple[int, int]:
        self.order.append("consolidate")
        return (3, 1)


class FakeIndex:
    def __init__(self, calls: Calls, fail: bool = False) -> None:
        self.calls = calls
        self.fail = fail

    def ingest_day(self, transcripts: Transcripts, day: str) -> int:
        self.calls.order.append(f"ingest {day}")
        if self.fail:
            raise RuntimeError("index down")
        return 4

    def tidy(self) -> int:
        self.calls.order.append("tidy")
        return 1


def setup(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, now: datetime = at(23, 12), *,
          fail_days: Optional[set[str]] = None, index: str = "fake") -> tuple[Dreamer, Transcripts, Calls, Clock]:
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    clock = Clock(now)
    t = Transcripts(TranscriptConfig(enabled=True, root=cfg.transcripts_dir), wall=clock)
    assert t.enabled
    calls = Calls(cfg, fail_days)
    monkeypatch.setattr(records, "reconcile_day", calls.reconcile_day, raising=False)
    monkeypatch.setattr(records, "consolidate", calls.consolidate, raising=False)
    idx = FakeIndex(calls) if index == "fake" else FakeIndex(calls, fail=True) if index == "failing" else None
    return Dreamer(cfg, t, client=object(), index=idx, wall=clock), t, calls, clock


def say(t: Transcripts, when: datetime, text: str = "hello there") -> None:
    assert t.append("voice", "v-1", "owner", "say", text, now=when)


def dreamt_lines(d: Dreamer) -> list[str]:
    return d.dreamt_path.read_text().splitlines() if d.dreamt_path.exists() else []


# ---- which nights are due ---------------------------------------------------------------------

def test_constants() -> None:
    assert DREAM_HOUR == 4.5 and MAX_NIGHTS_PER_WAKE == 3


def test_the_current_transcript_day_is_never_due_and_yesterday_waits_for_the_dream_hour(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, _, _ = setup(tmp_path, monkeypatch)
    say(t, at(22, 21))                       # the 22nd, evening
    say(t, at(23, 2))                        # 02:00 on the 23rd still belongs to the 22nd
    say(t, at(23, 9))                        # the 23rd
    assert t.days() == ["2026-09-22", "2026-09-23"]
    # 03:59 on the 23rd: the transcript day is still the 22nd, so nothing is over yet.
    assert d.due_nights(at(23, 3, 59)) == []
    # 04:00: the 22nd has ended, but the dream waits for 04:30.
    assert d.due_nights(at(23, 4, 0)) == []
    assert d.due_nights(at(23, 4, 29)) == []
    # 04:30: the 22nd is due; the 23rd (today) is not.
    assert d.due_nights(at(23, 4, 30)) == ["2026-09-22"]
    assert d.due_nights(at(23, 23, 0)) == ["2026-09-22"]
    # Naive local times give the same answer.
    assert d.due_nights(datetime(2026, 9, 23, 4, 29)) == []
    assert d.due_nights(datetime(2026, 9, 23, 4, 31)) == ["2026-09-22"]


def test_older_nights_are_due_at_any_hour_and_dreamt_nights_never(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, _, _ = setup(tmp_path, monkeypatch)
    for day in (19, 20, 21, 22):
        say(t, at(day, 12))
    # 02:00 on the 23rd: the 22nd is still the current transcript day; the 21st and older are overdue.
    assert d.due_nights(at(23, 2)) == ["2026-09-19", "2026-09-20", "2026-09-21"]
    # 04:10 on the 23rd: the 22nd is over but waits for 04:30; the cap keeps the three oldest.
    assert d.due_nights(at(23, 4, 10)) == ["2026-09-19", "2026-09-20", "2026-09-21"]
    d.dreamt_path.parent.mkdir(parents=True, exist_ok=True)
    d.dreamt_path.write_text("2026-09-19\n2026-09-21\nnot-a-day\n")
    assert d.dreamt() == {"2026-09-19", "2026-09-21"}
    assert d.due_nights(at(23, 4, 10)) == ["2026-09-20"]
    assert d.due_nights(at(23, 5)) == ["2026-09-20", "2026-09-22"]


def test_no_transcripts_means_nothing_due(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, _, _, _ = setup(tmp_path, monkeypatch)
    assert d.due_nights(at(23, 12)) == []


# ---- one night --------------------------------------------------------------------------------

def test_one_night_runs_its_steps_in_order_in_a_worker_thread_and_marks_the_day(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, calls, _ = setup(tmp_path, monkeypatch)
    say(t, at(22, 10), f"my favourite is {SENTINEL}")
    report = asyncio.run(d.dream("2026-09-22"))
    assert calls.order == ["reconcile 2026-09-22", "consolidate", "ingest 2026-09-22", "tidy"]
    assert calls.threads and calls.threads[0] != threading.main_thread().name
    assert SENTINEL in calls.texts["2026-09-22"]             # the model reads the day's own words
    assert (report.dreamt, report.empty, report.failed) == (True, False, [])
    assert (report.changed, report.created, report.corrections) == (2, 1, 1)
    assert (report.consolidated, report.removed, report.ingested, report.tidied) == (3, 1, 4, 1)
    assert dreamt_lines(d) == ["2026-09-22"]
    assert d.due_nights(at(23, 12)) == []


def test_the_dream_section_is_appended_to_the_journal_with_counts_only(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, _, _ = setup(tmp_path, monkeypatch)
    say(t, at(22, 10), f"remember {SENTINEL}")
    report = asyncio.run(d.dream("2026-09-22"))
    journal = d.cfg.records_dir / "days" / "2026-09-22.md"
    assert report.journal == journal
    text = journal.read_text()
    assert text.startswith("# 2026-09-22\n\n## Happened\n\n- a thing\n")   # what reconcile wrote is kept
    section = text.split("## Dream", 1)[1]
    assert text.count("## Dream") == 1
    assert "2 changed, 1 new, 1 corrections" in section
    assert "3 changed, 1 removed" in section
    assert "4 added, 1 tidied" in section
    assert SENTINEL not in text


def test_an_empty_day_is_marked_with_no_model_call(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, calls, _ = setup(tmp_path, monkeypatch)
    assert t.close("voice", "v-1", now=at(22, 10))           # a close marker, nothing said
    assert t.days() == ["2026-09-22"] and t.day_text("2026-09-22") == ""
    report = asyncio.run(d.dream("2026-09-22"))
    assert calls.order == []
    assert (report.dreamt, report.empty) == (True, True)
    assert dreamt_lines(d) == ["2026-09-22"]
    assert not (d.cfg.records_dir / "days").exists()


def test_a_failed_reconcile_is_not_marked_and_is_retried_and_marked_on_the_next_wake(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture) -> None:
    d, t, calls, _ = setup(tmp_path, monkeypatch, fail_days={"2026-09-22"})
    say(t, at(22, 10), SENTINEL)
    with caplog.at_level(logging.DEBUG, logger="eve_bridge.dream"):
        first = asyncio.run(d.dream("2026-09-22"))
    assert (first.dreamt, first.failed) == (False, ["reconcile"])
    assert calls.order == ["reconcile 2026-09-22"]            # nothing after a failed reconcile
    assert dreamt_lines(d) == []
    assert d.due_nights(at(23, 12)) == ["2026-09-22"]         # still due
    assert sum("could not reconcile" in r.message for r in caplog.records) == 1
    calls.fail_days.clear()
    second = asyncio.run(d.dream("2026-09-22"))
    assert second.dreamt and second.failed == []
    assert dreamt_lines(d) == ["2026-09-22"]
    assert SENTINEL not in caplog.text


def test_a_failure_is_logged_once_per_night_and_a_night_that_keeps_failing_steps_aside(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture) -> None:
    d, t, _, _ = setup(tmp_path, monkeypatch, fail_days={"2026-09-19"})
    for day in (19, 20, 21, 22):
        say(t, at(day, 12))
    with caplog.at_level(logging.WARNING, logger="eve_bridge.dream"):
        for _ in range(MAX_TRIES):
            assert d.due_nights(at(23, 2))[0] == "2026-09-19"
            asyncio.run(d.dream("2026-09-19"))
    assert sum("could not reconcile" in r.message for r in caplog.records) == 1
    # Set aside for this run, so the nights after it are no longer held up by it.
    assert d.due_nights(at(23, 2)) == ["2026-09-20", "2026-09-21"]
    assert "2026-09-19" not in d.dreamt()


def test_a_raising_reconcile_counts_as_a_failure(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, _, _ = setup(tmp_path, monkeypatch)

    def boom(*_: Any) -> None:
        raise RuntimeError("model down")

    monkeypatch.setattr(records, "reconcile_day", boom, raising=False)
    say(t, at(22, 10))
    report = asyncio.run(d.dream("2026-09-22"))
    assert (report.dreamt, report.failed) == (False, ["reconcile"])


def test_consolidate_or_index_failures_still_mark_the_night(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, calls, _ = setup(tmp_path, monkeypatch, index="failing")

    def bad_consolidate(*_: Any) -> tuple[int, int]:
        calls.order.append("consolidate")
        raise RuntimeError("too big")

    monkeypatch.setattr(records, "consolidate", bad_consolidate, raising=False)
    say(t, at(22, 10))
    report = asyncio.run(d.dream("2026-09-22"))
    assert calls.order == ["reconcile 2026-09-22", "consolidate", "ingest 2026-09-22"]
    assert report.dreamt and report.failed == ["consolidate", "index"]
    assert dreamt_lines(d) == ["2026-09-22"]
    assert "- failed: consolidate, index" in (d.cfg.records_dir / "days" / "2026-09-22.md").read_text()


def test_the_dream_works_with_no_index(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, calls, _ = setup(tmp_path, monkeypatch, index="none")
    assert d.index is None
    say(t, at(22, 10))
    report = asyncio.run(d.dream("2026-09-22"))
    assert calls.order == ["reconcile 2026-09-22", "consolidate"]
    assert report.dreamt and report.failed == [] and report.ingested == 0
    assert "- index: off" in (d.cfg.records_dir / "days" / "2026-09-22.md").read_text()


def test_dreaming_a_night_twice_keeps_one_ledger_line(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, _, _ = setup(tmp_path, monkeypatch)
    say(t, at(22, 10))
    asyncio.run(d.dream("2026-09-22"))
    asyncio.run(d.dream("2026-09-22"))
    assert dreamt_lines(d) == ["2026-09-22"]
    assert oct(d.dreamt_path.stat().st_mode & 0o777) == "0o600"


def test_a_bad_day_string_is_refused_without_a_call(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, _, calls, _ = setup(tmp_path, monkeypatch)
    report = asyncio.run(d.dream("../etc"))
    assert calls.order == [] and report.failed == ["read"]
    assert not report.dreamt and not d.dreamt_path.exists()


def test_a_disabled_transcript_store_never_marks_a_night(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, _, calls, _ = setup(tmp_path, monkeypatch)
    d.transcripts = Transcripts(TranscriptConfig(enabled=False, root=tmp_path / "off"))
    report = asyncio.run(d.dream("2026-09-22"))       # it would read as an empty day; it must not be marked as one
    assert calls.order == [] and report.failed == ["read"] and not d.dreamt_path.exists()
    assert d.due_nights(at(23, 12)) == []


# ---- the loop ---------------------------------------------------------------------------------

async def _run_loop(d: Dreamer, until: Any, interval: float = 0.01, limit: float = 5.0) -> None:
    shutdown = asyncio.Event()
    task = asyncio.create_task(d.loop(shutdown, interval_secs=interval))
    deadline = asyncio.get_running_loop().time() + limit
    while not until() and asyncio.get_running_loop().time() < deadline:
        await asyncio.sleep(0.01)
    shutdown.set()
    await asyncio.wait_for(task, timeout=2.0)


def test_the_loop_catches_up_missed_nights_oldest_first_three_a_wake(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, calls, _ = setup(tmp_path, monkeypatch, now=at(23, 12))
    for day in (18, 19, 20, 21, 22):
        say(t, at(day, 12))
    say(t, at(23, 9))                                        # today: never dreamt
    woke: list[list[str]] = []
    real = d.due_nights

    def recording(now: datetime) -> list[str]:
        due = real(now)
        woke.append(due)
        return due

    monkeypatch.setattr(d, "due_nights", recording)
    asyncio.run(_run_loop(d, lambda: len(dreamt_lines(d)) >= 5))
    assert woke[0] == ["2026-09-18", "2026-09-19", "2026-09-20"]      # the cap, oldest first
    assert woke[1] == ["2026-09-21", "2026-09-22"]
    assert dreamt_lines(d) == ["2026-09-18", "2026-09-19", "2026-09-20", "2026-09-21", "2026-09-22"]
    reconciled = [c for c in calls.order if c.startswith("reconcile")]
    assert reconciled == [f"reconcile 2026-09-{n}" for n in (18, 19, 20, 21, 22)]


def test_the_loop_stops_promptly_on_shutdown(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, calls, _ = setup(tmp_path, monkeypatch)
    say(t, at(22, 10))

    async def go() -> float:
        shutdown = asyncio.Event()
        task = asyncio.create_task(d.loop(shutdown, interval_secs=3600.0))
        await asyncio.sleep(0.05)
        started = asyncio.get_running_loop().time()
        shutdown.set()
        await asyncio.wait_for(task, timeout=1.0)
        return asyncio.get_running_loop().time() - started

    assert asyncio.run(go()) < 0.5
    assert calls.order == []                                  # it never woke: the first wake is FIRST_WAKE_SECS away
    assert dream_mod.FIRST_WAKE_SECS > 0.05


def test_the_loop_does_not_start_a_night_after_shutdown(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, calls, _ = setup(tmp_path, monkeypatch)
    for day in (19, 20, 21):
        say(t, at(day, 12))
    shutdown = asyncio.Event()

    def reconcile_then_stop(cfg: RecallConfig, client: Any, day: str, text: str) -> Optional[FakeChange]:
        shutdown.set()
        return Calls.reconcile_day(calls, cfg, client, day, text)

    monkeypatch.setattr(records, "reconcile_day", reconcile_then_stop, raising=False)

    async def go() -> None:
        await asyncio.wait_for(d.loop(shutdown, interval_secs=0.01), timeout=2.0)

    asyncio.run(go())
    assert [c for c in calls.order if c.startswith("reconcile")] == ["reconcile 2026-09-19"]


def test_nothing_raises_out_of_the_loop_when_a_step_throws(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture) -> None:
    d, t, calls, _ = setup(tmp_path, monkeypatch, index="failing")
    say(t, at(21, 12))
    say(t, at(22, 12))

    def boom(*_: Any) -> None:
        raise RuntimeError("everything is down")

    monkeypatch.setattr(records, "consolidate", boom, raising=False)
    wakes = {"n": 0}
    real = d.due_nights

    def flaky(now: datetime) -> list[str]:
        wakes["n"] += 1
        if wakes["n"] == 1:
            raise OSError("disk gone")                       # the wake itself throws
        return real(now)

    monkeypatch.setattr(d, "due_nights", flaky)
    with caplog.at_level(logging.WARNING, logger="eve_bridge.dream"):
        asyncio.run(_run_loop(d, lambda: len(dreamt_lines(d)) >= 2))
    assert wakes["n"] >= 2
    assert dreamt_lines(d) == ["2026-09-21", "2026-09-22"]    # consolidate and index failed; the nights still count
    assert sum("a wake failed" in r.message for r in caplog.records) == 1


def test_the_loop_survives_a_dream_that_raises(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d, t, _, _ = setup(tmp_path, monkeypatch)
    say(t, at(22, 12))

    def broken(day: str) -> Any:
        raise RuntimeError("thread died")

    monkeypatch.setattr(d, "_night", broken)
    report = asyncio.run(d.dream("2026-09-22"))
    assert report.failed == ["dream"] and not report.dreamt


# ---- the night ends committed (L6) -------------------------------------------------------------

def test_the_night_ends_with_the_report_committed(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """The counts are appended after reconcile's and consolidate's commits; one more commit puts the whole
    journal in history. A night with no journal commits nothing more."""
    d, t, _, _ = setup(tmp_path, monkeypatch)
    commits: list[tuple[Path, str]] = []
    monkeypatch.setattr(records, "commit", lambda repo, message: commits.append((Path(repo), message)) or "abc1234")
    say(t, at(22, 21))
    report = asyncio.run(d.dream("2026-09-22"))
    assert report.dreamt and report.journal is not None
    assert "## Dream" in report.journal.read_text(encoding="utf-8")
    assert commits == [(Path(d.cfg.records_dir), "dream: 2026-09-22 report")]

    empty = asyncio.run(d.dream("2026-09-20"))           # nothing said: no call, no journal, no commit
    assert empty.empty and len(commits) == 1


def test_the_report_commit_is_real_history(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    import shutil
    import subprocess
    if shutil.which("git") is None:
        pytest.skip("git is not installed")
    d, t, _, _ = setup(tmp_path, monkeypatch)
    assert records.ensure_repo(Path(d.cfg.records_dir))
    say(t, at(22, 21))
    asyncio.run(d.dream("2026-09-22"))
    status = subprocess.run(["git", "status", "--porcelain"], cwd=d.cfg.records_dir, capture_output=True,
                            text=True, check=True).stdout
    assert status == ""                                   # nothing of the night is left uncommitted
    log_out = subprocess.run(["git", "log", "--format=%s"], cwd=d.cfg.records_dir, capture_output=True,
                             text=True, check=True).stdout
    assert "dream: 2026-09-22 report" in log_out
