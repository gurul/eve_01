"""memory.py: the facade, the one tool set, forget across every store, and the one-time move."""

from __future__ import annotations

import hashlib
import json
import logging
import os
import stat
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Callable, Optional

import pytest

from eve_bridge import memory as mem
from eve_bridge.memory import Memory, enabled, forget_markdown, migrate
from eve_bridge.recall import RecallConfig
from eve_bridge.transcripts import FORGOTTEN, TranscriptConfig, Transcripts

TZ = datetime(2026, 9, 21, 12, 0).astimezone().tzinfo
SENTINEL = "zebra-sentinel-7Q"


def at(day: int, hh: int, mm: int = 0) -> datetime:
    return datetime(2026, 9, day, hh, mm, tzinfo=TZ)


# ---- fakes for the stores other leaves own -----------------------------------------------------

class FakeRecords:
    """records.py as the contract names it, kept in memory, with a real starred.md so the move is checkable."""

    def __init__(self, lines: Optional[list[str]] = None) -> None:
        self.lines = list(lines or [])
        self.calls: list[tuple] = []
        self.fail = False

    def _check(self) -> None:
        if self.fail:
            raise RuntimeError("records broke")

    def profile(self, cfg: RecallConfig) -> str:
        self._check()
        return "PROFILE"

    def profile_for_voice(self, cfg: RecallConfig) -> str:
        self._check()
        return "VOICE PROFILE"

    def star(self, cfg: RecallConfig, text: str, when: Optional[datetime] = None) -> Optional[str]:
        self._check()
        self.calls.append(("star", text, when))
        cfg.records_dir.mkdir(parents=True, exist_ok=True)
        path = cfg.records_dir / "starred.md"
        existing = path.read_text() if path.exists() else ""
        if text.lower() in existing.lower():
            return None
        line = f"- {text} ({(when or datetime.now()):%Y-%m-%d})"
        path.write_text(existing + line + "\n")
        return line

    def RecordsReader(self, cfg: RecallConfig, recall: Any) -> Any:   # noqa: N802 - the contract's class name
        outer = self

        class Reader:
            def search(self, query: str) -> dict:
                outer._check()
                words = query.lower().split()
                hits = [{"id": "pets", "line": ln} for ln in outer.lines if all(w in ln.lower() for w in words)]
                return {"ok": True, "hits": hits, "recalled": []}

            def get(self, rid: str) -> dict:
                outer._check()
                return {"ok": True, "record": f"record {rid}"} if rid == "pets" else \
                    {"ok": False, "reason": f"no record {rid!r}"}

        return Reader()

    def forget_lines(self, cfg: RecallConfig, match: Callable[[str], bool], dry_run: bool = False) -> int:
        self._check()
        hit = [ln for ln in self.lines if match(ln)]
        self.calls.append(("forget_lines", dry_run))
        if not dry_run:
            self.lines = [ln for ln in self.lines if not match(ln)]
        return len(hit)

    def squash_history(self, repo: Path) -> bool:
        self.calls.append(("squash", repo))
        return True

    def seal(self, repo: Path) -> None:
        self.calls.append(("seal", repo))


class FakeIndex:
    def __init__(self, rows: Optional[list[dict]] = None) -> None:
        self.rows = list(rows or [])
        self.forgot: list[str] = []

    def search(self, query: str) -> list[dict]:
        return [{"text": r["text"], "day": r["day"], "channel": r["channel"]}
                for r in self.rows if query.split()[0].lower() in r["text"].lower()]

    def find(self, match: Callable[[str], bool]) -> list[dict]:
        return [{"id": r["id"], "text": r["text"]} for r in self.rows if match(r["text"])]

    def forget(self, ids: list[str]) -> int:
        self.forgot.extend(ids)
        before = len(self.rows)
        self.rows = [r for r in self.rows if r["id"] not in ids]
        return before - len(self.rows)


class Clock:
    def __init__(self, t: float = 1000.0) -> None:
        self.t = t

    def __call__(self) -> float:
        return self.t


class Wall:
    def __init__(self, now: datetime) -> None:
        self.t = now

    def __call__(self) -> datetime:
        return self.t


@pytest.fixture()
def fake_records(monkeypatch: pytest.MonkeyPatch) -> FakeRecords:
    rec = FakeRecords([f"Has a cat called {SENTINEL} (said 2026-09-20).", "Likes tea (said 2026-09-19)."])
    monkeypatch.setattr(mem, "_records_mod", lambda: rec)
    return rec


def build(tmp_path: Path, index: Optional[FakeIndex] = None, clock: Optional[Clock] = None,
          now: datetime = at(21, 12)) -> tuple[Memory, Transcripts, RecallConfig, Wall]:
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    wall = Wall(now)
    tx = Transcripts(TranscriptConfig(enabled=True, root=cfg.transcripts_dir), cfg.archive_dir, wall=wall)
    return Memory(cfg, tx, index, clock=clock or Clock()), tx, cfg, wall


def seed(tx: Transcripts) -> None:
    tx.append("voice", "v-1758448800000-aaaa", "owner", "say", f"my cat {SENTINEL} is asleep", now=at(21, 9))
    tx.append("voice", "v-1758448800000-aaaa", "eve", "say", "Sweet dreams to the cat.", now=at(21, 9, 1))
    tx.append("telegram", "t-1758459600000-bbbb", "owner", "say", "book the dentist", now=at(21, 11))
    tx.append("telegram", "t-1758459600000-bbbb", "eve", "say", "Booked for Friday.", now=at(21, 11, 1))


def write(path: Path, text: str, mode: int = 0o600) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    os.chmod(path, mode)
    return path


# ---- the switch --------------------------------------------------------------------------------

def test_enabled_is_off_by_default_and_on_with_memory_or_the_legacy_records_switch() -> None:
    assert enabled({}) is False
    assert enabled({"EVE_MEMORY": "0"}) is False
    assert enabled({"EVE_MEMORY": "1"}) is True
    assert enabled({"EVE_MEMORY": "on"}) is True
    assert enabled({"EVE_RECORDS": "1"}) is True


# ---- the tools ---------------------------------------------------------------------------------

def test_tools_are_strict_in_a_stable_order_and_voice_adds_recent_conversation(tmp_path: Path) -> None:
    m, *_ = build(tmp_path)
    text = [t["name"] for t in m.tools()]
    voice = [t["name"] for t in m.tools(voice=True)]
    assert text == ["memory_search", "memory_read", "forget_preview", "forget_apply"]
    assert voice == text + ["recent_conversation"]
    params = {t["name"]: t["parameters"] for t in m.tools(voice=True)}
    assert set(params["memory_search"]["properties"]) == {"query", "days_back"}
    assert params["memory_search"]["properties"]["days_back"]["type"] == ["integer", "null"]
    assert set(params["memory_read"]["properties"]) == {"ref"}
    assert set(params["forget_preview"]["properties"]) == {"query"}
    assert set(params["forget_apply"]["properties"]) == {"token"}
    assert params["recent_conversation"]["properties"] == {}
    for t in m.tools(voice=True):
        assert t["type"] == "function" and t["strict"] is True
        p = t["parameters"]
        assert p["additionalProperties"] is False and sorted(p["required"]) == sorted(p["properties"])


def test_tools_are_copies_a_caller_cannot_corrupt(tmp_path: Path) -> None:
    m, *_ = build(tmp_path)
    m.tools()[0]["description"] = "changed"
    assert m.tools()[0]["description"] != "changed"


def test_forget_tool_descriptions_require_an_explicit_ask_and_a_confirmation() -> None:
    assert "explicitly asks" in mem.FORGET_PREVIEW["description"]
    assert "confirms in a new message" in mem.FORGET_APPLY["description"]


# ---- the prompt pieces -------------------------------------------------------------------------

def test_profile_voice_profile_and_star_go_to_records(tmp_path: Path, fake_records: FakeRecords) -> None:
    m, _tx, cfg, _ = build(tmp_path)
    assert m.profile() == "PROFILE"
    assert m.profile_for_voice() == "VOICE PROFILE"
    assert m.star("the spare key is under the mat")
    assert (cfg.records_dir / "starred.md").read_text().startswith("- the spare key is under the mat (")


def test_a_broken_records_store_is_silence_not_a_crash(tmp_path: Path, fake_records: FakeRecords) -> None:
    fake_records.fail = True
    m, *_ = build(tmp_path)
    assert m.profile() == "" and m.profile_for_voice() == "" and m.star("anything at all") is None


def test_today_is_the_transcripts_today_block(tmp_path: Path, fake_records: FakeRecords) -> None:
    m, tx, _, _ = build(tmp_path)
    seed(tx)
    block = m.today(4000)
    assert "book the dentist" in block and SENTINEL in block
    hidden = m.today(4000, exclude_conv="t-1758459600000-bbbb")
    assert "book the dentist" not in hidden and SENTINEL in hidden       # control: the other conv stays
    assert "(spoken)" in m.today(4000, style="voice")


# ---- memory_search / memory_read / recent_conversation ------------------------------------------

def test_memory_search_returns_three_dated_groups(tmp_path: Path, fake_records: FakeRecords) -> None:
    index = FakeIndex([{"id": "m1", "text": f"Owner's cat is {SENTINEL}", "day": "2026-09-20", "channel": "voice"}])
    m, tx, cfg, _ = build(tmp_path, index)
    seed(tx)
    write(cfg.transcripts_dir / "meetings" / "2026-09-20" / "1400-standup.md",
          f"---\ntitle: standup\n---\n# Standup\n- the cat {SENTINEL} came up\n")
    out = m.handle_tool("memory_search", {"query": SENTINEL, "days_back": None})
    assert out["ok"] is True
    assert [h["id"] for h in out["hits"]] == ["pets"]
    assert out["recalled"] == [f"(2026-09-20, voice) Owner's cat is {SENTINEL}"]
    said = {(h["day"], h["ch"]) for h in out["said"]}
    assert ("2026-09-21", "voice") in said and ("2026-09-20", "meeting") in said
    voice_hit = next(h for h in out["said"] if h["ch"] == "voice")
    assert voice_hit["time"] == "09:00" and voice_hit["after"].startswith("eve:")
    assert "note" not in out


def test_hidden_words_leave_every_search_group_and_no_store_changes(tmp_path: Path) -> None:
    from eve_bridge import records as real

    index = FakeIndex([{"id": "m1", "text": f"Owner's cat is {SENTINEL}", "day": "2026-09-20", "channel": "voice"},
                       {"id": "m2", "text": f"{SENTINEL} likes the sofa", "day": "2026-09-19", "channel": "app"}])
    m, tx, cfg, _ = build(tmp_path, index)
    seed(tx)
    tx.append("telegram", "t-1758459600000-bbbb", "owner", "say", f"{SENTINEL} took the sofa again", now=at(21, 11, 5))
    cfg.records_dir.mkdir(parents=True)
    (cfg.records_dir / "pets.md").write_text(real.render_record(real.Record(
        "pets", "preference", ["cat"], [f"Has a cat called {SENTINEL}.", f"{SENTINEL} sleeps on the sofa."])))
    out = m.search(SENTINEL)
    # positive control: each group has the line about to be hidden
    assert len(out["hits"]) == 2 and len(out["recalled"]) == 2
    assert any("asleep" in h["text"] for h in out["said"])
    files = {p: p.read_bytes() for p in [*cfg.transcripts_dir.rglob("*"), cfg.records_dir / "pets.md"] if p.is_file()}
    rows = [dict(r) for r in index.rows]

    assert m.hide("cat") is not None
    assert m.hidden() == ["cat"]
    out = m.search(SENTINEL)
    assert [h["line"] for h in out["hits"]] == [f"{SENTINEL} sleeps on the sofa."]
    assert out["recalled"] == [f"(2026-09-19, app) {SENTINEL} likes the sofa"]
    assert [h["text"] for h in out["said"]] == [f"{SENTINEL} took the sofa again"]
    assert "note" not in out
    assert {p: p.read_bytes() for p in files} == files and index.rows == rows and index.forgot == []
    assert m.read("pets")["record"].count(SENTINEL) == 1                    # read leaves the hidden fact out too

    assert m.unhide("cat") is True and m.hidden() == []
    assert len(m.search(SENTINEL)["hits"]) == 2


def test_hide_on_a_broken_records_store_is_none_not_a_crash(tmp_path: Path, fake_records: FakeRecords) -> None:
    m, tx, _, _ = build(tmp_path, FakeIndex())
    seed(tx)
    assert m.hide("cat") is None and m.unhide("cat") is False and m.hidden() == []
    assert m.search(SENTINEL)["ok"] is True                                  # FakeRecords has no hidden list


def test_memory_search_with_nothing_found_says_so_and_a_wordless_query_is_refused(
        tmp_path: Path, fake_records: FakeRecords) -> None:
    m, tx, _, _ = build(tmp_path, FakeIndex())
    seed(tx)
    empty = m.handle_tool("memory_search", {"query": "nonexistent-thing"})
    assert empty["ok"] is True and empty["note"] == "nothing matched"
    assert m.handle_tool("memory_search", {"query": "  a  "})["ok"] is False


def test_memory_search_days_back_limits_the_said_group(tmp_path: Path, fake_records: FakeRecords) -> None:
    m, tx, _, wall = build(tmp_path)
    tx.append("voice", "v-1757844000000-cccc", "owner", "say", "the old umbrella story", now=at(14, 12))
    assert m.handle_tool("memory_search", {"query": "umbrella", "days_back": 30})["said"]
    assert m.handle_tool("memory_search", {"query": "umbrella", "days_back": 2})["said"] == []


def test_memory_search_survives_a_broken_records_store(tmp_path: Path, fake_records: FakeRecords) -> None:
    fake_records.fail = True
    m, tx, _, _ = build(tmp_path)
    seed(tx)
    out = m.handle_tool("memory_search", {"query": "dentist"})
    assert out["ok"] is True and out["hits"] == [] and out["said"]


def test_memory_read_takes_a_day_a_time_range_a_start_or_a_record_id(
        tmp_path: Path, fake_records: FakeRecords) -> None:
    m, tx, _, _ = build(tmp_path)
    seed(tx)
    day = m.handle_tool("memory_read", {"ref": "2026-09-21"})
    assert day["ok"] and SENTINEL in day["text"] and "dentist" in day["text"]
    ranged = m.handle_tool("memory_read", {"ref": "2026-09-21 10:30-11:30"})
    assert "dentist" in ranged["text"] and SENTINEL not in ranged["text"]
    from_start = m.handle_tool("memory_read", {"ref": "2026-09-21 10:00"})
    assert "dentist" in from_start["text"] and SENTINEL not in from_start["text"]
    assert m.handle_tool("memory_read", {"ref": "2026-09-01"})["ok"] is False
    assert m.handle_tool("memory_read", {"ref": "pets"}) == {"ok": True, "record": "record pets"}
    assert m.handle_tool("memory_read", {"ref": "nope"})["ok"] is False
    assert m.handle_tool("memory_read", {"ref": " "})["ok"] is False


def test_recent_conversation_reads_only_the_other_channel_since_the_session_opened(
        tmp_path: Path, fake_records: FakeRecords) -> None:
    m, tx, _, _ = build(tmp_path)
    seed(tx)
    out = m.handle_tool("recent_conversation", {}, since=at(21, 8), channel="voice")
    assert "book the dentist" in out["text"] and SENTINEL not in out["text"]
    later = m.handle_tool("recent_conversation", {}, since=at(21, 11, 0), channel="voice")
    assert "dentist" not in later["text"] and "Booked for Friday" in later["text"]
    assert m.handle_tool("recent_conversation", {}, since=None, channel="voice")["ok"] is False
    nothing = m.handle_tool("recent_conversation", {}, since=at(21, 11, 30), channel="voice")
    assert nothing["ok"] is True and nothing["text"] == ""


def test_recent_conversation_keeps_the_newest_within_its_budget(tmp_path: Path, fake_records: FakeRecords) -> None:
    m, tx, _, wall = build(tmp_path, now=at(21, 23))
    for i in range(80):
        tx.append("telegram", "t-1758459600000-bbbb", "owner", "say", f"line {i:03d} " + "x" * 100,
                  now=at(21, 12) + timedelta(minutes=i))
    out = m.handle_tool("recent_conversation", {}, since=at(21, 8), channel="voice")
    assert len(out["text"]) <= mem.RECENT_CHARS and "line 079" in out["text"] and "line 000" not in out["text"]
    assert out["omitted"] > 0


def test_unknown_tools_and_crashing_tools_answer_instead_of_raising(
        tmp_path: Path, fake_records: FakeRecords, monkeypatch: pytest.MonkeyPatch) -> None:
    m, *_ = build(tmp_path)
    assert m.handle_tool("memory_write", {})["ok"] is False
    monkeypatch.setattr(m, "search", lambda *a, **k: 1 / 0)
    assert m.handle_tool("memory_search", {"query": "x y"}) == {"ok": False,
                                                                 "reason": "memory is unavailable right now"}
    assert m.handle_tool("memory_read", "not a dict")["ok"] is False   # type: ignore[arg-type]


# ---- forget ------------------------------------------------------------------------------------

def forget_fixture(tmp_path: Path) -> tuple[Memory, Transcripts, RecallConfig, FakeIndex, Clock, dict[str, Path]]:
    index = FakeIndex([{"id": "m1", "text": f"cat is {SENTINEL}", "day": "2026-09-20", "channel": "voice"},
                       {"id": "m2", "text": "likes tea", "day": "2026-09-19", "channel": "telegram"}])
    clock = Clock()
    m, tx, cfg, _ = build(tmp_path, index, clock)
    seed(tx)
    files = {
        "archive": write(cfg.archive_dir / "sessions" / "2026-09-20" / "0900-a1.md",
                         f"---\ntitle: about {SENTINEL}\n---\n# Talk\n- the cat {SENTINEL}\n- tea\n", 0o640),
        "day": write(cfg.archive_dir / "2026-09-20-cats.md", f"# Day\n- {SENTINEL} again\n- other\n"),
        "meeting": write(cfg.transcripts_dir / "meetings" / "2026-09-20" / "1400-standup.md",
                         f"# Standup\n- {SENTINEL} mentioned\n- budget\n"),
        "archive_git": write(cfg.archive_dir / ".git" / "objects" / "note.md", f"- {SENTINEL} in git\n"),
    }
    (cfg.records_dir / ".git").mkdir(parents=True)
    return m, tx, cfg, index, clock, files


def test_forget_preview_counts_every_store_and_never_returns_the_words(
        tmp_path: Path, fake_records: FakeRecords) -> None:
    m, tx, cfg, index, _, files = forget_fixture(tmp_path)
    out = m.handle_tool("forget_preview", {"query": SENTINEL})
    assert out["ok"] is True
    assert out["layers"] == {"transcripts": 2, "records": 1, "index": 1, "archive": 2, "meetings": 1}  # + the answer
    assert out["total"] == 7 and out["expires_in"] == 600 and out["token"]
    # no chat log attached: it is named as not covered, never claimed (review, 2026-10-08)
    assert out["not_covered"] == ["claude-mem", "vault", "chief ledger", "telegram chat log"]
    assert SENTINEL.lower() not in json.dumps(out).lower()
    # a preview changes nothing (control: the words are still everywhere)
    assert SENTINEL in files["archive"].read_text() and SENTINEL in files["meeting"].read_text()
    assert any(SENTINEL in ln["text"] for ln in tx.lines("2026-09-21"))
    assert fake_records.calls == [("forget_lines", True)] and index.forgot == []


def test_forget_preview_with_no_match_mints_no_token_and_a_wordless_query_is_refused(
        tmp_path: Path, fake_records: FakeRecords) -> None:
    m, *_ = forget_fixture(tmp_path)
    none = m.forget_preview("nothing-like-this-anywhere")
    assert none["ok"] is True and none["total"] == 0 and none["token"] == ""
    assert m.forget_preview(" x ")["ok"] is False


def test_forget_apply_removes_the_words_from_every_store_and_squashes_history(
        tmp_path: Path, fake_records: FakeRecords) -> None:
    m, tx, cfg, index, _, files = forget_fixture(tmp_path)
    token = m.forget_preview(SENTINEL)["token"]
    out = m.handle_tool("forget_apply", {"token": token})
    assert out["ok"] is True and out["total"] == 7
    assert out["layers"] == {"transcripts": 2, "records": 1, "index": 1, "archive": 2, "meetings": 1}
    assert sorted(out["history_squashed"]) == ["archive", "records"]
    assert ("squash", cfg.records_dir) in fake_records.calls and ("squash", cfg.archive_dir) in fake_records.calls
    # transcripts: the envelope stays, the words go
    rows = tx.lines("2026-09-21")
    assert len(rows) == 4 and rows[0]["text"] == FORGOTTEN
    assert SENTINEL not in tx.path_of("2026-09-21").read_text()
    assert all(SENTINEL not in ln for ln in fake_records.lines) and len(fake_records.lines) == 1
    assert index.forgot == ["m1"] and [r["id"] for r in index.rows] == ["m2"]
    # markdown: matching body lines gone, other lines and the frontmatter kept, mode kept
    archived = files["archive"].read_text()
    assert archived == f"---\ntitle: about {SENTINEL}\n---\n# Talk\n- tea\n"
    assert stat.S_IMODE(files["archive"].stat().st_mode) == 0o640
    assert files["day"].read_text() == "# Day\n- other\n"
    assert files["meeting"].read_text() == "# Standup\n- budget\n"
    assert SENTINEL in files["archive_git"].read_text()      # inside .git is git's to rewrite (squash), not ours
    assert m.forget_preview(SENTINEL)["layers"]["archive"] == 0


def test_a_forget_token_works_once_expires_and_belongs_to_one_memory(
        tmp_path: Path, fake_records: FakeRecords) -> None:
    m, tx, cfg, index, clock, _ = forget_fixture(tmp_path)
    token = m.forget_preview(SENTINEL)["token"]
    other, *_ = build(tmp_path / "other")
    assert other.forget_apply(token)["ok"] is False
    clock.t += 601
    assert m.forget_apply(token)["ok"] is False                    # expired
    assert any(SENTINEL in ln["text"] for ln in tx.lines("2026-09-21"))
    fresh = m.forget_preview(SENTINEL)["token"]
    assert fresh != token
    assert m.forget_apply(fresh)["ok"] is True
    assert m.forget_apply(fresh)["ok"] is False                    # single use
    assert m.forget_apply("")["ok"] is False


def test_forget_apply_does_not_squash_when_nothing_was_forgotten(
        tmp_path: Path, fake_records: FakeRecords) -> None:
    m, tx, cfg, index, clock, files = forget_fixture(tmp_path)
    token = m.forget_preview(SENTINEL)["token"]
    # everything the preview counted vanished before the confirmation
    for p in (files["archive"], files["day"], files["meeting"]):
        p.unlink()
    tx.redact(SENTINEL)
    fake_records.lines = []
    index.rows = []
    out = m.forget_apply(token)
    assert out["ok"] is True and out["total"] == 0 and out["history_squashed"] == []


def test_forget_apply_is_not_ok_when_mem0_could_not_be_opened(
        tmp_path: Path, fake_records: FakeRecords) -> None:
    """mem0 in its retry window (or with no key) answered a forget with 0, and the index read as done."""
    from eve_bridge.mem0_memory import Mem0Config, OwnerMemory

    def broken(_c: Mem0Config) -> Any:
        raise ImportError("no mem0")

    index = OwnerMemory(Mem0Config(enabled=True, home=tmp_path / "mem0"), factory=broken)
    m, *_ = build(tmp_path, index)                     # type: ignore[arg-type]
    out = m.forget_apply(m.forget_preview(SENTINEL)["token"])
    assert out["ok"] is False and out["failed"] == ["index"]


def test_forget_apply_is_not_ok_when_a_markdown_file_could_not_be_rewritten(
        tmp_path: Path, fake_records: FakeRecords, monkeypatch: pytest.MonkeyPatch) -> None:
    """The matched lines were counted before the rewrite and its OSError swallowed: ok, with the words on disk."""
    m, _tx, _cfg, _index, _, files = forget_fixture(tmp_path)
    token = m.forget_preview(SENTINEL)["token"]
    real = mem._atomic_rewrite

    def refuse_meetings(path: Path, text: str) -> None:
        if path == files["meeting"]:
            raise PermissionError("read-only")
        real(path, text)
    monkeypatch.setattr(mem, "_atomic_rewrite", refuse_meetings)
    out = m.forget_apply(token)
    assert out["ok"] is False and out["failed"] == ["meetings"]
    assert out["layers"]["meetings"] == 0 and out["layers"]["archive"] == 2   # only what really went is counted
    assert SENTINEL in files["meeting"].read_text() and files["day"].read_text() == "# Day\n- other\n"


def test_forget_markdown_never_touches_frontmatter_symlinks_or_git(tmp_path: Path) -> None:
    folder = tmp_path / "md"
    note = write(folder / "a.md", f"---\nid: {SENTINEL}\n---\n{SENTINEL} body\nkeep\n")
    outside = write(tmp_path / "outside.md", f"{SENTINEL} outside\n")
    (folder / "link.md").symlink_to(outside)
    write(folder / ".git" / "x.md", f"{SENTINEL}\n")
    write(folder / "plain.txt", f"{SENTINEL}\n")
    pred = mem.match(SENTINEL)
    assert forget_markdown(folder, pred, dry_run=True) == 1
    assert SENTINEL in note.read_text().split("---\n", 2)[2]         # control: dry run changed nothing
    assert forget_markdown(folder, pred) == 1
    assert note.read_text() == f"---\nid: {SENTINEL}\n---\nkeep\n"
    assert SENTINEL in outside.read_text() and SENTINEL in (folder / "plain.txt").read_text()
    assert forget_markdown(tmp_path / "missing", pred) == 0


def test_logs_never_carry_what_was_said(tmp_path: Path, fake_records: FakeRecords,
                                        caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.DEBUG)
    m, *_ = forget_fixture(tmp_path)
    m.handle_tool("memory_search", {"query": SENTINEL})
    m.forget_apply(m.forget_preview(SENTINEL)["token"])
    assert "forgot 7 lines" in caplog.text                          # control: the logs did run
    assert SENTINEL.lower() not in caplog.text.lower()


# ---- the one-time move -------------------------------------------------------------------------

HIGHLIGHTS = """# HIGHLIGHTS

> **Template.** This file is seeded with an example.

## 2026-01-01 — Example outage

- ★ Retries need an idempotency key *(example)*
- ★ Somebody else's lesson (2026-01-01)

## From talking

- ★ The dog is called Pepper (2026-09-11)
- ★ Prefers mornings for calls (2026-09-12)
- ★ (candidate) maybe likes jazz
- ★ Old example line *(example)*
"""


def old_layout(tmp_path: Path) -> tuple[Path, Path]:
    legacy = tmp_path / "debrief"
    old_mem0 = tmp_path / "mem0-old"
    write(legacy / "sessions" / "2026-09-20" / "0900-a1.md", "---\nsource: eve-voice\n---\n- talk\n")
    write(legacy / "sessions" / "2026-09-21" / "1000-b2.md", "- another talk\n")
    write(legacy / "2026-09-20-a-day.md", "# the day\n")
    write(legacy / "INDEX.md", "# INDEX\n")
    write(legacy / "HIGHLIGHTS.md", HIGHLIGHTS)
    write(legacy / "records" / "profile.md", "---\nupdated: 2026-09-20\n---\nprofile\n")
    write(legacy / "records" / "pets.md", "---\nid: pets\n---\n- a dog\n")
    write(legacy / ".git" / "HEAD", "ref: refs/heads/main\n")
    write(legacy / ".git" / "objects" / "ab" / "cdef", "blob\n")
    write(legacy / ".gitignore", "*\n")
    write(legacy / "notes" / "2026-09-19" / "1400-standup.md", "# notes\n")
    write(legacy / "notes" / "2026-09-19" / "1400-standup.transcript.md", "words\n")
    write(legacy / "notes" / "README.md", "not a date\n")
    write(old_mem0 / "qdrant" / "collection" / "data.bin", "vectors\n")
    write(old_mem0 / "history.db", "sqlite\n")
    write(old_mem0 / "ingested", "a\n")
    return legacy, old_mem0


def digests(*roots: Path) -> list[str]:
    out = []
    for root in roots:
        if not root.exists():
            continue
        for base, _dirs, files in os.walk(root):
            for f in files:
                p = Path(base) / f
                if p.name == mem.MIGRATED_FILE:
                    continue
                out.append(hashlib.sha256(p.read_bytes()).hexdigest())
    return sorted(out)


def file_count(*roots: Path) -> int:
    return sum(len(files) for root in roots if root.exists() for _b, _d, files in os.walk(root))


def test_migrate_moves_the_old_layout_into_the_new_root_and_loses_nothing(
        tmp_path: Path, fake_records: FakeRecords) -> None:
    legacy, old_mem0 = old_layout(tmp_path)
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    before_count = file_count(legacy, old_mem0)
    before = digests(legacy, old_mem0)
    assert before_count == 16                                        # control: the fixture is what we think

    out = migrate(cfg, legacy, old_mem0)

    assert out["ok"] is True and out["already"] is False
    assert out["records"] == 2 and out["mem0"] == 3 and out["meetings"] == 2 and out["stars"] == 2
    assert out["archive"] == 9 and out["skipped"] == 0 and out["failed"] == 0
    # every original file is somewhere under the new root, and the only new file is starred.md
    after_count = file_count(legacy, old_mem0, cfg.store) - 1        # minus .migrated
    assert after_count == before_count + 1
    after = digests(legacy, old_mem0, cfg.store)
    for d in before:
        assert d in after
        after.remove(d)
    assert len(after) == 1                                           # starred.md
    assert file_count(legacy, old_mem0) == 0 and legacy.is_dir()     # emptied, never deleted
    # where things went
    assert (cfg.records_dir / "pets.md").read_text() == "---\nid: pets\n---\n- a dog\n"
    assert (cfg.mem0_dir / "history.db").exists() and (cfg.mem0_dir / "qdrant" / "collection" / "data.bin").exists()
    assert (cfg.transcripts_dir / "meetings" / "2026-09-19" / "1400-standup.md").exists()
    for rel in ("sessions/2026-09-20/0900-a1.md", "2026-09-20-a-day.md", "INDEX.md", "HIGHLIGHTS.md",
                ".git/HEAD", ".gitignore", "notes/README.md"):
        assert (cfg.archive_dir / rel).exists(), rel
    # only the owner's own stars, dated, never the template's or a candidate
    starred = (cfg.records_dir / "starred.md").read_text()
    assert starred == "- The dog is called Pepper (2026-09-11)\n- Prefers mornings for calls (2026-09-12)\n"
    assert ("seal", cfg.archive_dir) in fake_records.calls
    assert stat.S_IMODE(cfg.store.stat().st_mode) == 0o700
    marker = json.loads((cfg.store / mem.MIGRATED_FILE).read_text())
    assert marker["records"] == 2 and marker["archive"] == 9 and "at" in marker


def test_migrate_is_idempotent(tmp_path: Path, fake_records: FakeRecords) -> None:
    legacy, old_mem0 = old_layout(tmp_path)
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    migrate(cfg, legacy, old_mem0)
    snapshot = digests(cfg.store)
    write(legacy / "INDEX.md", "# came back\n")                      # something new appears in the old place
    again = migrate(cfg, legacy, old_mem0)
    assert again["already"] is True and again["records"] == 2
    assert digests(cfg.store) == snapshot and (legacy / "INDEX.md").exists()


def test_migrate_skips_an_existing_target_and_keeps_both(tmp_path: Path, fake_records: FakeRecords,
                                                         caplog: pytest.LogCaptureFixture) -> None:
    legacy, old_mem0 = old_layout(tmp_path)
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    write(cfg.mem0_dir / "history.db", "the new one\n")
    write(cfg.archive_dir / "INDEX.md", "already archived\n")
    before = file_count(legacy, old_mem0, cfg.store)
    caplog.set_level(logging.INFO)
    out = migrate(cfg, legacy, old_mem0)
    assert out["skipped"] == 2 and out["mem0"] == 0 and out["ok"] is True
    assert (cfg.mem0_dir / "history.db").read_text() == "the new one\n"
    assert (old_mem0 / "history.db").read_text() == "sqlite\n"       # left where it was, not lost
    assert (legacy / "INDEX.md").read_text() == "# INDEX\n"
    assert (cfg.archive_dir / "INDEX.md").read_text() == "already archived\n"
    assert file_count(legacy, old_mem0, cfg.store) - 1 == before + 1  # minus .migrated, plus starred.md
    assert "skipped" in caplog.text and "INDEX" not in caplog.text and "Pepper" not in caplog.text


def test_migrate_with_an_existing_records_folder_archives_the_old_one(
        tmp_path: Path, fake_records: FakeRecords) -> None:
    legacy, old_mem0 = old_layout(tmp_path)
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    write(cfg.records_dir / "profile.md", "new profile\n")
    migrate(cfg, legacy, old_mem0)
    assert (cfg.records_dir / "profile.md").read_text() == "new profile\n"
    assert (cfg.archive_dir / "records" / "pets.md").exists()


def test_migrate_with_nothing_to_move_just_marks_it_done(tmp_path: Path, fake_records: FakeRecords) -> None:
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    out = migrate(cfg, tmp_path / "no-debrief", tmp_path / "no-mem0")
    assert out["ok"] is True and sum(out[k] for k in ("records", "mem0", "meetings", "archive", "stars")) == 0
    assert (cfg.store / mem.MIGRATED_FILE).exists()
    assert fake_records.calls == []                                  # no archive repo: nothing to seal


def test_migrate_refuses_overlapping_folders(tmp_path: Path, fake_records: FakeRecords) -> None:
    legacy, old_mem0 = old_layout(tmp_path)
    same = RecallConfig(store=legacy, notes=tmp_path / "notes")
    inside = RecallConfig(store=legacy / "memory", notes=tmp_path / "notes")
    count = file_count(legacy)
    assert migrate(same, legacy, old_mem0)["ok"] is False
    assert migrate(inside, legacy, old_mem0)["ok"] is False
    assert file_count(legacy) == count and not (legacy / mem.MIGRATED_FILE).exists()


def test_a_failed_move_writes_no_marker_so_the_next_start_retries(
        tmp_path: Path, fake_records: FakeRecords, monkeypatch: pytest.MonkeyPatch) -> None:
    legacy, old_mem0 = old_layout(tmp_path)
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    real_rename = os.rename

    def flaky(src: Any, dst: Any) -> None:
        if Path(src).name == "sessions":
            raise OSError(18, "cross-device link")
        real_rename(src, dst)

    monkeypatch.setattr(mem.os, "rename", flaky)
    first = migrate(cfg, legacy, old_mem0)
    assert first["ok"] is False and first["failed"] == 1
    assert not (cfg.store / mem.MIGRATED_FILE).exists() and (legacy / "sessions").is_dir()
    monkeypatch.setattr(mem.os, "rename", real_rename)
    second = migrate(cfg, legacy, old_mem0)
    assert second["ok"] is True and second["archive"] == 2 and (cfg.archive_dir / "sessions").is_dir()
    assert (cfg.store / mem.MIGRATED_FILE).exists()
    assert (cfg.records_dir / "starred.md").read_text().count("Pepper") == 1   # stars are not doubled


def test_a_star_that_fails_to_carry_keeps_highlights_in_place_for_the_retry(
        tmp_path: Path, fake_records: FakeRecords, monkeypatch: pytest.MonkeyPatch) -> None:
    legacy, old_mem0 = old_layout(tmp_path)
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")

    def broken(*_a: Any, **_k: Any) -> None:
        raise OSError("disk")

    monkeypatch.setattr(fake_records, "star", broken)
    first = migrate(cfg, legacy, old_mem0)
    assert first["ok"] is False and first["failed"] == 2
    assert (legacy / "HIGHLIGHTS.md").exists() and not (cfg.archive_dir / "HIGHLIGHTS.md").exists()
    assert (cfg.archive_dir / "INDEX.md").exists()                   # control: the rest moved
    monkeypatch.undo()
    monkeypatch.setattr(mem, "_records_mod", lambda: fake_records)
    second = migrate(cfg, legacy, old_mem0)
    assert second["ok"] is True and second["stars"] == 2
    assert (cfg.archive_dir / "HIGHLIGHTS.md").exists()
    assert "Pepper" in (cfg.records_dir / "starred.md").read_text()


def test_a_legacy_mem0_that_already_is_the_new_one_is_left_alone(tmp_path: Path, fake_records: FakeRecords) -> None:
    cfg = RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    write(cfg.mem0_dir / "history.db", "sqlite\n")
    out = migrate(cfg, tmp_path / "no-debrief", cfg.mem0_dir)
    assert out["ok"] is True and out["skipped"] == 0 and out["mem0"] == 0
    assert (cfg.mem0_dir / "history.db").read_text() == "sqlite\n"
