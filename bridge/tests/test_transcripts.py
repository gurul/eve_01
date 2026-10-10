"""transcripts.py: the local per-day transcript store — guards, day boundary, blocks, search, forget."""

from __future__ import annotations

import json
import logging
import os
import socket
import stat
import subprocess
import threading
from datetime import datetime, timedelta
from pathlib import Path

import pytest

from eve_bridge import transcripts as tx
from eve_bridge.transcripts import (
    FORGOTTEN,
    OUTSIDE_LEFT_OUT,
    ConvInfo,
    TranscriptConfig,
    Transcripts,
    configured,
    match,
    refusal,
)

TZ = datetime(2026, 9, 21, 12, 0).astimezone().tzinfo
SENTINEL = "zebra-sentinel-7Q"


def at(day: int, hh: int, mm: int = 0, ss: int = 0) -> datetime:
    return datetime(2026, 9, day, hh, mm, ss, tzinfo=TZ)


class Clock:
    def __init__(self, now: datetime) -> None:
        self.t = now

    def __call__(self) -> datetime:
        return self.t


def store(tmp_path: Path, now: datetime = at(21, 12), archive: Path | None = None) -> tuple[Transcripts, Clock]:
    clock = Clock(now)
    return Transcripts(TranscriptConfig(enabled=True, root=tmp_path / "tx"), archive, wall=clock), clock


def raw_lines(t: Transcripts, day: str) -> list[dict]:
    return [json.loads(r) for r in t.path_of(day).read_text().splitlines() if r.strip()]


# ---- config ----------------------------------------------------------------------------------

def test_configured_is_off_by_default_and_on_with_memory_or_the_legacy_records_switch() -> None:
    off = configured({})
    assert off.enabled is False and tx.MEMORY_DEFAULT is False
    assert off.root == Path("~/.config/eve-bridge/memory/transcripts").expanduser()
    assert configured({"EVE_MEMORY": "1"}).enabled is True
    assert configured({"EVE_RECORDS": "1"}).enabled is True       # the older switch still turns it on
    assert configured({"EVE_MEMORY": "0", "EVE_RECORDS": "0"}).enabled is False
    assert configured({"EVE_MEMORY_DIR": "/x/mem"}).root == Path("/x/mem/transcripts")
    assert (tx.DAY_START_HOUR, tx.VOICE_CHARS, tx.BACKEND_CHARS) == (4, 4000, 16000)


def test_a_disabled_store_writes_and_reads_nothing(tmp_path: Path) -> None:
    t = Transcripts(TranscriptConfig(enabled=False, root=tmp_path / "tx"))
    assert t.append("voice", "v-1", "owner", "say", "hi") is False
    assert not (tmp_path / "tx").exists() and t.days() == [] and t.today_block(1000) == ""


# ---- day boundary and files ------------------------------------------------------------------

def test_the_day_starts_at_four(tmp_path: Path) -> None:
    t, _ = store(tmp_path)
    assert t.day_of(at(22, 3, 59)) == "2026-09-21"
    assert t.day_of(at(22, 4, 0)) == "2026-09-22"
    t.append("voice", "v-1", "owner", "say", "late", now=at(22, 3, 59))
    t.append("voice", "v-1", "owner", "say", "early", now=at(22, 4, 0))
    assert [ln["text"] for ln in t.lines("2026-09-21")] == ["late"]
    assert [ln["text"] for ln in t.lines("2026-09-22")] == ["early"]
    assert t.days() == ["2026-09-21", "2026-09-22"]


def test_modes_are_private_whatever_the_umask(tmp_path: Path) -> None:
    old = os.umask(0o022)
    try:
        t, _ = store(tmp_path)
        t.append("telegram", "t-1", "owner", "say", "hello")
    finally:
        os.umask(old)
    assert stat.S_IMODE(os.stat(t.root).st_mode) == 0o700
    assert stat.S_IMODE(os.stat(t.path_of("2026-09-21")).st_mode) == 0o600
    assert (t.root / tx.NEVER_INDEX).exists()


def test_a_line_has_the_envelope_and_text_is_capped(tmp_path: Path) -> None:
    t, _ = store(tmp_path)
    conv = t.new_conv("voice")
    assert t.append("voice", conv, "eve", "tool", "x" * 30000, tool="web_search", now=at(21, 9, 5))
    (line,) = raw_lines(t, "2026-09-21")
    assert set(line) == {"ts", "ch", "conv", "who", "kind", "text", "tool"}
    assert line["ts"].startswith("2026-09-21T09:05:00") and line["ts"][19] in "+-"
    assert len(line["text"]) == tx.MAX_TEXT_CHARS and line["tool"] == "web_search"
    # nonsense is refused, not written
    assert t.append("fax", conv, "owner", "say", "x") is False
    assert t.append("voice", conv, "stranger", "say", "x") is False
    assert t.stats.rejected == 2 and len(raw_lines(t, "2026-09-21")) == 1


def test_conversation_ids_are_unique_and_carry_the_channel(tmp_path: Path) -> None:
    t, _ = store(tmp_path)
    ids = {t.new_conv("voice") for _ in range(2000)} | {t.new_conv("telegram") for _ in range(2000)}
    assert len(ids) == 4000
    assert all(tx._CONV.match(i) for i in ids)
    assert {i[0] for i in ids} == {"v", "t"}


# ---- guards ----------------------------------------------------------------------------------

def test_a_root_inside_a_git_work_tree_is_refused_with_one_warning(tmp_path: Path,
                                                                    caplog: pytest.LogCaptureFixture) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    caplog.set_level(logging.WARNING, logger="eve_bridge.transcripts")
    t = Transcripts(TranscriptConfig(enabled=True, root=repo / "deep" / "tx"))
    assert t.enabled is False
    assert t.append("voice", "v-1", "owner", "say", "hi") is False
    assert not (repo / "deep").exists()
    warnings = [r for r in caplog.records if r.levelno == logging.WARNING]
    assert len(warnings) == 1 and "git work tree" in warnings[0].getMessage()
    # positive control: the same shape of root with no .git writes
    plain = tmp_path / "plain"
    plain.mkdir()
    ok = Transcripts(TranscriptConfig(enabled=True, root=plain / "deep" / "tx"))
    assert ok.enabled is True and ok.append("voice", "v-1", "owner", "say", "hi") is True


def test_synced_folders_and_symlinks_are_refused(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    home = tmp_path / "home"
    (home / "Documents").mkdir(parents=True)
    monkeypatch.setenv("HOME", str(home))
    for rel in ("Documents/eve", "Desktop/tx", "Library/Mobile Documents/x", "Library/CloudStorage/y/tx"):
        assert "syncs" in refusal(home / rel), rel
        t = Transcripts(TranscriptConfig(enabled=True, root=home / rel))
        assert t.enabled is False and t.append("voice", "v-1", "owner", "say", "x") is False
    target = tmp_path / "target"
    target.mkdir()
    link = tmp_path / "link"
    link.symlink_to(target)
    assert "symlink" in refusal(link)
    assert Transcripts(TranscriptConfig(enabled=True, root=link)).enabled is False
    # control: a private folder under the same fake home is fine
    assert refusal(home / ".config" / "tx") == ""
    assert Transcripts(TranscriptConfig(enabled=True, root=home / ".config" / "tx")).enabled is True


# ---- failure is silence ----------------------------------------------------------------------

@pytest.mark.skipif(os.geteuid() == 0, reason="root ignores directory permissions")
def test_an_unwritable_folder_counts_failures_and_raises_nothing(tmp_path: Path,
                                                                  caplog: pytest.LogCaptureFixture) -> None:
    t, _ = store(tmp_path)
    os.chmod(t.root, 0o500)
    try:
        caplog.set_level(logging.WARNING, logger="eve_bridge.transcripts")
        for _ in range(3):
            assert t.append("voice", "v-1", "owner", "say", SENTINEL) is False
    finally:
        os.chmod(t.root, 0o700)
    assert t.stats.failed == 3
    assert len([r for r in caplog.records if r.levelno == logging.WARNING]) == 1    # once an hour, not per line
    assert SENTINEL not in caplog.text


def test_a_torn_last_line_is_skipped_and_the_next_line_survives(tmp_path: Path) -> None:
    t, _ = store(tmp_path)
    t.append("voice", "v-1", "owner", "say", "whole", now=at(21, 9))
    with open(t.path_of("2026-09-21"), "a") as f:
        f.write('{"ts": "2026-09-21T09:01:00+00:00", "ch": "voice", "te')      # a crash mid-write
    assert [ln["text"] for ln in t.lines("2026-09-21")] == ["whole"]
    t.append("voice", "v-1", "eve", "say", "after", now=at(21, 9, 2))
    assert [ln["text"] for ln in t.lines("2026-09-21")] == ["whole", "after"]


def test_eight_threads_of_appends_give_whole_lines(tmp_path: Path) -> None:
    t, _ = store(tmp_path)

    def worker(n: int) -> None:
        for i in range(500):
            t.append("telegram" if n % 2 else "voice", f"v-{n}", "owner", "say", f"thread {n} line {i} " + "y" * 200)

    threads = [threading.Thread(target=worker, args=(n,)) for n in range(8)]
    for th in threads:
        th.start()
    for th in threads:
        th.join()
    assert len(raw_lines(t, "2026-09-21")) == 4000          # json.loads on every one: none torn or interleaved
    assert len(t.lines("2026-09-21")) == 4000 and t.stats.appended == 4000


def test_nothing_said_reaches_the_log(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.DEBUG)
    t, _ = store(tmp_path)
    conv = t.new_conv("voice")
    t.append("voice", conv, "owner", "say", f"my {SENTINEL} is here")
    t.close("voice", conv)
    t.today_block(4000)
    t.search(SENTINEL)
    t.find(SENTINEL)
    assert t.redact(SENTINEL) == 1
    assert "forgot 1 lines" in caplog.text                   # counts do get logged
    assert SENTINEL not in caplog.text


def test_no_network_is_touched(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    def refuse(*a: object, **k: object) -> None:
        raise AssertionError("network used")

    monkeypatch.setattr(socket, "socket", refuse)
    monkeypatch.setattr(socket, "create_connection", refuse)
    t, _ = store(tmp_path)
    t.append("voice", "v-1", "owner", "say", "the red kite", now=at(21, 9))
    assert "red kite" in t.read("2026-09-21")
    assert t.search("kite")["hits"][0]["text"] == "the red kite"
    src = Path(tx.__file__).read_text()
    for lib in ("import socket", "import http", "import urllib", "requests", "openai", "httpx"):
        assert lib not in src


# ---- today block -----------------------------------------------------------------------------

def test_today_block_renders_both_channels_and_drops_commands_relays_and_closes(tmp_path: Path) -> None:
    t, clock = store(tmp_path, now=at(21, 18))
    assert t.today_block(4000) == ""
    t.append("voice", "v-1", "owner", "say", "I moved the  meeting\nto Friday", now=at(21, 9, 5))
    t.append("voice", "v-1", "eve", "tool", "found it", tool="web_search", now=at(21, 9, 6))
    t.append("voice", "v-1", "eve", "say", "Noted.", now=at(21, 9, 6))
    t.close("voice", "v-1", now=at(21, 9, 7))
    t.append("telegram", "t-1", "owner", "command", "claude on", now=at(21, 10))
    t.append("telegram", "t-1", "owner", "relay", "fix the build", now=at(21, 10, 1))
    t.append("telegram", "t-1", "owner", "image", "a receipt", now=at(21, 10, 2))
    t.append("voice", "v-0", "owner", "say", "yesterday's line", now=at(20, 23))
    assert t.today_block(4000).splitlines() == [
        "09:05 (spoken) Owner: I moved the meeting to Friday",
        "09:06 (spoken) eve (web search): found it",
        "09:06 (spoken) eve: Noted.",
        "10:02 (texted) Owner (photo): a receipt",
    ]


def test_today_block_keeps_the_newest_lines_within_budget(tmp_path: Path) -> None:
    t, _ = store(tmp_path, now=at(21, 23))
    for i in range(300):
        t.append("telegram", "t-1", "owner" if i % 2 else "eve", "say", f"line {i:03d} " + "w" * 60,
                 now=at(21, 5) + timedelta(minutes=i))
    block = t.today_block(4000)
    assert 0 < len(block) <= 4000
    rows = block.splitlines()
    assert rows[0].startswith("(about ") and rows[0].endswith("earlier lines today: memory_search finds them)")
    assert "line 299" in rows[-1]                                           # the newest line is kept
    kept = [int(r.split("line ")[1][:3]) for r in rows[1:]]
    assert kept == list(range(kept[0], 300))                                # a contiguous newest run
    n = int(rows[0].split()[1])
    assert n % tx.HEADER_BUCKET == 0 and n >= kept[0]                      # bucketed count
    for budget in (500, 1000, 2500, 12000):
        assert len(t.today_block(budget)) <= budget


def test_today_block_is_a_stable_prefix_while_under_budget_and_between_cuts(tmp_path: Path) -> None:
    t, clock = store(tmp_path, now=at(21, 23))
    for i in range(5):
        t.append("voice", "v-1", "owner", "say", f"first {i}", now=at(21, 9, i))
    before = t.today_block(4000)
    t.append("telegram", "t-1", "owner", "say", "then a text", now=at(21, 10))
    after = t.today_block(4000)
    assert after.startswith(before) and len(after) > len(before)
    # over budget: once cut, later appends extend the block rather than shifting it
    for i in range(80):
        t.append("telegram", "t-1", "eve", "say", f"reply {i:02d} " + "z" * 40, now=at(21, 11) + timedelta(minutes=i))
    cut = t.today_block(3000)
    t.append("telegram", "t-1", "owner", "say", "one more", now=at(21, 13))
    assert t.today_block(3000).startswith(cut)


def test_exclude_conv_and_exclude_tail_remove_exactly_those_lines(tmp_path: Path) -> None:
    t, _ = store(tmp_path, now=at(21, 23))
    t.append("voice", "v-1", "owner", "say", "voice one", now=at(21, 9))
    for i in range(6):
        t.append("telegram", "t-1", "owner" if i % 2 == 0 else "eve", "say", f"text {i}", now=at(21, 10, i))
    t.append("telegram", "t-1", "eve", "tool", "a tool result", tool="memory_search", now=at(21, 10, 3))
    t.append("voice", "v-2", "owner", "say", "voice two", now=at(21, 11))
    whole = t.today_block(4000, exclude_conv="t-1")
    assert "text" not in whole and "a tool result" not in whole and "voice one" in whole and "voice two" in whole
    tail = t.today_block(4000, exclude_conv="t-1", exclude_tail=4)          # the last 4 turns are in the history
    assert [r.split(": ", 1)[1] for r in tail.splitlines()] == [
        "voice one", "text 0", "text 1", "a tool result", "voice two"]
    assert t.today_block(4000, exclude_conv="t-9") == t.today_block(4000)   # an unknown conv drops nothing


def test_voice_style_clips_lines_shorter(tmp_path: Path) -> None:
    t, _ = store(tmp_path, now=at(21, 23))
    t.append("voice", "v-1", "owner", "say", "a" * 2000, now=at(21, 9))
    text_row, = t.today_block(4000).splitlines()
    voice_row, = t.today_block(4000, style="voice").splitlines()
    assert len(voice_row) < len(text_row) and voice_row.endswith("…")


# ---- other readers ---------------------------------------------------------------------------

def test_conversations_are_read_whole_across_the_day_boundary(tmp_path: Path) -> None:
    t, clock = store(tmp_path)
    late = f"v-{int(at(22, 3, 58).timestamp() * 1000)}-abcd"
    t.append("voice", late, "owner", "say", "still up", now=at(22, 3, 58))
    t.append("voice", late, "eve", "say", "me too", now=at(22, 4, 2))
    t.close("voice", late, now=at(22, 4, 12))
    tg = f"t-{int(at(21, 20).timestamp() * 1000)}-0001"
    t.append("telegram", tg, "owner", "say", "hi", now=at(21, 20))
    convs = t.conversations("2026-09-21")
    assert [c.conv for c in convs] == [tg, late]
    info = convs[1]
    assert info == ConvInfo(conv=late, ch="voice", started=at(22, 3, 58), ended=at(22, 4, 2), closed=True)
    assert convs[0].closed is False
    assert t.conversations("2026-09-22") == []                   # filed under the day it began, once
    assert [ln["text"] for ln in t.conv_lines(late)] == ["still up", "me too", ""]


def test_last_turn_at_spans_both_channels_and_ignores_close_markers(tmp_path: Path) -> None:
    t, _ = store(tmp_path)
    assert t.last_turn_at() is None
    t.append("voice", "v-1", "owner", "say", "a", now=at(21, 9))
    t.append("telegram", "t-1", "owner", "say", "b", now=at(21, 11))
    t.close("voice", "v-1", now=at(21, 12))
    assert t.last_turn_at() == at(21, 11)
    fresh, _ = store(tmp_path)                                    # a restart reads it back from disk
    assert fresh.last_turn_at() == at(21, 11)


def test_lines_since_returns_other_channels_only(tmp_path: Path) -> None:
    t, _ = store(tmp_path, now=at(21, 12))
    t.append("telegram", "t-1", "owner", "say", "too early", now=at(21, 9))
    t.append("telegram", "t-1", "owner", "say", "sent mid-session", now=at(21, 10, 5))
    t.append("voice", "v-1", "owner", "say", "the voice itself", now=at(21, 10, 6))
    t.append("telegram", "t-1", "owner", "command", "claude off", now=at(21, 10, 7))
    got = t.lines_since(at(21, 10), exclude_ch="voice")
    assert [ln["text"] for ln in got] == ["sent mid-session"]


def test_the_cache_is_invalidated_when_the_file_grows(tmp_path: Path) -> None:
    t, _ = store(tmp_path)
    t.append("voice", "v-1", "owner", "say", "one", now=at(21, 9))
    assert len(t.lines("2026-09-21")) == 1
    assert t.lines("2026-09-21")[0] is t.lines("2026-09-21")[0]       # cached
    with open(t.path_of("2026-09-21"), "a") as f:                     # another process appends
        f.write(json.dumps({"ts": at(21, 9, 1).isoformat(), "ch": "telegram", "conv": "t-1", "who": "owner",
                            "kind": "say", "text": "two"}) + "\n")
    assert [ln["text"] for ln in t.lines("2026-09-21")] == ["one", "two"]


def test_read_returns_a_time_window_including_after_midnight(tmp_path: Path) -> None:
    t, _ = store(tmp_path, now=at(22, 3))
    t.append("voice", "v-1", "owner", "say", "morning", now=at(21, 8))
    t.append("voice", "v-1", "owner", "say", "evening", now=at(21, 22))
    t.append("voice", "v-1", "owner", "say", "past midnight", now=at(22, 1))
    assert [r.split(": ")[1] for r in t.read("2026-09-21", "21:00", "").splitlines()] == ["evening", "past midnight"]
    assert "morning" in t.read("2026-09-21") and "evening" not in t.read("2026-09-21", "", "09:00")
    long = t.read("2026-09-21", max_chars=80)
    assert len(long) <= 80 + 60 and "read again from" in long


def test_day_text_renders_the_whole_day_and_keeps_the_newest_over_budget(tmp_path: Path) -> None:
    t, _ = store(tmp_path)
    assert t.day_text("2026-09-21") == ""
    t.append("voice", "v-1", "owner", "say", "hello", now=at(21, 9))
    t.append("voice", "v-1", "eve", "tool", "sunny", tool="web_search", now=at(21, 9, 1))
    t.append("telegram", "t-1", "owner", "command", "claude on", now=at(21, 9, 2))
    t.append("telegram", "t-1", "eve", "say", "done", now=at(21, 9, 3))
    assert t.day_text("2026-09-21").splitlines() == [
        "2026-09-21 09:00 spoken Owner: hello", f"2026-09-21 09:01 spoken eve (web search): {OUTSIDE_LEFT_OUT}",
        "2026-09-21 09:03 texted eve: done"]
    assert "spoken eve (web search): sunny" in t.day_text("2026-09-21", outside=True)
    for i in range(100):
        t.append("telegram", "t-1", "owner", "say", f"n{i:03d} " + "q" * 50, now=at(21, 10) + timedelta(minutes=i))
    text = t.day_text("2026-09-21", max_chars=1000)
    rows = text.splitlines()
    assert len(text) <= 1000 and "n099" in rows[-1]
    omitted = int(rows[0].strip("(").split()[0])
    assert rows[0] == f"({omitted} earlier lines omitted)" and omitted + len(rows) - 1 == 103


def test_day_text_dates_each_line_with_its_own_calendar_date(tmp_path: Path) -> None:
    """The dream's days/2026-09-23.md filed an event of 2026-09-24 00:08 under 2026-09-23: the transcript day
    runs 04:00 to 04:00, and its lines carried only HH:MM under "Everything said on 2026-09-23". A line past
    midnight stays in its transcript day, and says the date it was actually said."""
    t, _ = store(tmp_path, now=at(22, 12))
    t.append("telegram", "t-1", "owner", "say", "evening", now=at(21, 23, 50))
    t.append("telegram", "t-1", "owner", "say", "past midnight", now=at(22, 0, 8))
    assert t.day_of(at(22, 0, 8)) == "2026-09-21"
    assert t.day_text("2026-09-21").splitlines() == [
        "2026-09-21 23:50 texted Owner: evening", "2026-09-22 00:08 texted Owner: past midnight"]


# ---- search ----------------------------------------------------------------------------------

def test_search_finds_dated_hits_newest_first_with_neighbours_from_the_same_conversation(tmp_path: Path) -> None:
    t, _ = store(tmp_path, now=at(22, 12))
    t.append("voice", "v-1", "owner", "say", "my sister lives by the harbour", now=at(20, 9))
    t.append("voice", "v-1", "eve", "say", "Good to know.", now=at(20, 9, 1))
    t.append("telegram", "t-1", "owner", "say", "unrelated interleaved line", now=at(21, 10))
    t.append("voice", "v-2", "owner", "say", "we visited my sister today", now=at(21, 10, 1))
    t.append("telegram", "t-1", "eve", "say", "another interleaved line", now=at(21, 10, 2))
    t.append("voice", "v-2", "eve", "say", "How was it?", now=at(21, 10, 3))
    res = t.search("Sister")
    assert res["ok"] and not res["truncated"]
    assert [(h["day"], h["time"]) for h in res["hits"]] == [("2026-09-21", "10:01"), ("2026-09-20", "09:00")]
    first = res["hits"][0]
    assert first["ch"] == "voice" and first["who"] == "owner"
    assert first["before"] == "" and first["after"] == "eve: How was it?"      # not the interleaved text
    assert res["hits"][1]["after"] == "eve: Good to know."
    assert t.search("sister", channel="telegram")["hits"] == []
    assert len(t.search("interleaved", channel="telegram")["hits"]) == 2
    assert t.search("sister harbour")["hits"][0]["day"] == "2026-09-20"        # every term must match
    assert t.search("sister", days_back=1)["hits"][0]["day"] == "2026-09-21"
    assert len(t.search("sister", days_back=1)["hits"]) == 1


def test_a_quoted_phrase_matches_as_written(tmp_path: Path) -> None:
    t, _ = store(tmp_path)
    t.append("voice", "v-1", "owner", "say", "the blue car is parked", now=at(21, 9))
    t.append("voice", "v-1", "owner", "say", "the car is blue", now=at(21, 9, 1))
    assert len(t.search("blue car")["hits"]) == 2
    assert [h["text"] for h in t.search('"blue car"')["hits"]] == ["the blue car is parked"]
    assert t.search("a")["ok"] is False and t.search('" "')["ok"] is False    # no usable term


def test_search_reads_meeting_notes_and_the_archive(tmp_path: Path) -> None:
    archive = tmp_path / "debrief"
    (archive / "sessions" / "2026-09-19").mkdir(parents=True)
    (archive / "sessions" / "2026-09-19" / "0930-abc.md").write_text(
        "---\nstatus: machine-draft\n---\n\n# Plans\n\n- Owner wants a trip to the lighthouse.\n")
    (archive / "2026-09-18-planning.md").write_text("# The day\n\n- Lighthouse trip first raised.\n")
    t, _ = store(tmp_path, now=at(21, 12), archive=archive)
    meet = t.root / tx.MEETINGS_SUBDIR / "2026-09-20"
    meet.mkdir(parents=True)
    (meet / "1400-standup.md").write_text(
        "---\nstatus: notes\n---\n\n# Standup\n\n## Decisions\n\n- Ship the lighthouse page Friday.\n"
        "- Budget stays flat.\n")
    t.append("telegram", "t-1", "owner", "say", "book the lighthouse tour", now=at(21, 9))
    hits = t.search("lighthouse")["hits"]
    assert [(h["day"], h["ch"]) for h in hits] == [
        ("2026-09-21", "telegram"), ("2026-09-20", "meeting"), ("2026-09-19", "note"), ("2026-09-18", "note")]
    meeting = hits[1]
    assert meeting["time"] == "14:00" and meeting["text"] == "Ship the lighthouse page Friday."
    assert meeting["before"] == "Decisions" and meeting["after"] == "Budget stays flat."
    assert hits[2]["time"] == "09:30" and hits[3]["time"] == ""
    assert [h["ch"] for h in t.search("lighthouse", channel="meeting")["hits"]] == ["meeting"]
    assert "status" not in json.dumps(t.search("notes")["hits"])               # frontmatter is not searched
    no_archive, _ = store(tmp_path, now=at(21, 12))
    assert {h["ch"] for h in no_archive.search("lighthouse")["hits"]} == {"telegram", "meeting"}


def test_the_search_result_is_capped(tmp_path: Path) -> None:
    t, _ = store(tmp_path, now=at(21, 23))
    for i in range(40):
        t.append("telegram", f"t-{i}", "owner", "say", f"penguin fact {i} " + "p" * 400, now=at(21, 5) + timedelta(minutes=i))
    res = t.search("penguin", limit=50)
    assert len(json.dumps(res, ensure_ascii=False)) <= tx.SEARCH_RESULT_CHARS
    assert res["truncated"] is True and res["hits"] and all(len(h["text"]) <= 300 for h in res["hits"])
    assert "penguin fact 39" in res["hits"][0]["text"]                          # newest first


def test_search_stops_at_its_time_budget(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    t, _ = store(tmp_path, now=at(21, 23))
    for d in (18, 19, 20, 21):
        t.append("voice", "v-1", "owner", "say", "owl", now=at(d, 9))
    monkeypatch.setattr(tx, "SEARCH_BUDGET_SECS", -1.0)
    assert t.search("owl") == {"ok": True, "hits": [], "truncated": True}


# ---- forget ----------------------------------------------------------------------------------

def test_match_is_the_shared_rule() -> None:
    assert match("") is None and match("a b ?") is None and match('""') is None
    pred = match('Blue "red  car"')
    assert pred is not None
    assert pred("a RED car and a blue one") and not pred("a red truck, blue") and not pred("red car only")
    assert match("keys?")("where are my keys")                               # punctuation is not a term


def test_find_and_redact_keep_the_envelope_and_forget_the_words(tmp_path: Path) -> None:
    t, _ = store(tmp_path)
    t.append("voice", "v-1", "owner", "say", f"my {SENTINEL} code", now=at(20, 9))
    t.append("voice", "v-1", "eve", "say", "ok", now=at(20, 9, 1))
    t.append("telegram", "t-1", "owner", "say", f"again {SENTINEL}", now=at(21, 9))
    t.close("telegram", "t-1", now=at(21, 9, 5))
    found = t.find(SENTINEL.upper())
    assert [(f["day"], f["ch"]) for f in found] == [("2026-09-20", "voice"), ("2026-09-21", "telegram")]
    assert t.search(SENTINEL)["hits"]                                         # cached before the forget
    before = raw_lines(t, "2026-09-20")
    assert t.redact(SENTINEL, dry_run=True) == 3 and raw_lines(t, "2026-09-20") == before   # counts, writes nothing
    assert t.redact(SENTINEL) == 3                                            # two lines and eve's answer to one
    after = raw_lines(t, "2026-09-20")
    assert after[0] == {**before[0], "text": FORGOTTEN} and after[1] == {**before[1], "text": FORGOTTEN}
    for day in ("2026-09-20", "2026-09-21"):
        assert SENTINEL not in t.path_of(day).read_text()
        assert stat.S_IMODE(os.stat(t.path_of(day)).st_mode) == 0o600
    assert t.search(SENTINEL)["hits"] == [] and t.find(SENTINEL) == []      # the cache did not keep it
    assert t.redact(SENTINEL) == 0 and t.find("forgotten") == []             # nothing matched twice
    assert t.redact("x") == 0 and t.find("") == []                           # no usable term
    assert not [p for p in t.root.iterdir() if p.name.endswith(".tmp")]


def test_redact_drops_a_torn_line_holding_the_words(tmp_path: Path) -> None:
    t, _ = store(tmp_path)
    t.append("voice", "v-1", "owner", "say", "kept", now=at(21, 9))
    with open(t.path_of("2026-09-21"), "a") as f:
        f.write(f'{{"ts": "2026-09-21T09:01:00", "text": "{SENTINEL} tor\n')
    t.append("voice", "v-1", "owner", "say", "also kept", now=at(21, 9, 2))
    assert t.redact(SENTINEL) == 1
    assert SENTINEL not in t.path_of("2026-09-21").read_text()
    assert [ln["text"] for ln in t.lines("2026-09-21")] == ["kept", "also kept"]


def test_redact_loses_no_concurrent_append(tmp_path: Path) -> None:
    t, _ = store(tmp_path)
    for i in range(2000):
        t.append("voice", "v-1", "owner", "say", f"{SENTINEL} {i}" if i % 3 == 0 else f"plain {i}")
    stop = threading.Event()
    written: list[int] = []

    def writer() -> None:
        i = 0
        while not stop.is_set() or i < 300:
            t.append("telegram", "t-1", "owner", "say", f"live {i}")
            written.append(i)
            i += 1

    th = threading.Thread(target=writer)
    th.start()
    removed = t.redact(SENTINEL)
    stop.set()
    th.join()
    lines = t.lines("2026-09-21")
    assert removed == 667
    assert sum(1 for ln in lines if ln["text"].startswith("live ")) == len(written)
    assert sum(1 for ln in lines if ln["text"] == FORGOTTEN) == 667
    assert len(lines) == 2000 + len(written)
