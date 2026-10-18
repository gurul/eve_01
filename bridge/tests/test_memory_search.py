"""memory_index.py and the three search switches: the ranked index, its fallback, forget reaching it, the time
resolver across the 04:00 line, the code-side retry and the Telegram brain's three-memory-reads rule."""

from __future__ import annotations

import asyncio
import json
import re
import sys
from datetime import date, datetime
from pathlib import Path
from typing import Any, Optional, Sequence

import pytest

from eve_bridge import memory_index as mi
from eve_bridge.memory import MEMORY_SEARCH, Memory, memory_search_tool
from eve_bridge.recall import RecallConfig
from eve_bridge.transcripts import TranscriptConfig, Transcripts

TZ = datetime(2026, 9, 21, 12, 0).astimezone().tzinfo
SENTINEL = "quokka-sentinel-9Z"
SYNONYMS = {"bicycle": "bike", "cycle": "bike", "stole": "theft", "stolen": "theft"}


def at(month: int, day: int, hh: int, mm: int = 0) -> datetime:
    return datetime(2026, month, day, hh, mm, tzinfo=TZ)


class FakeEmbedder:
    """Bag of words (one dimension per word, no collisions), with a few synonyms folded together: meaning without a network."""

    def __init__(self) -> None:
        self.calls = 0
        self.texts = 0
        self.vocab: dict[str, int] = {}
        self.fail = False

    def __call__(self, texts: Sequence[str], timeout: float) -> list[list[float]]:
        if self.fail:
            raise RuntimeError("embeddings down")
        self.calls += 1
        self.texts += len(texts)
        out = []
        for t in texts:
            v = [0.0] * 8192
            for w in re.findall(r"\w+", t.lower()):
                if w in mi.STOPWORDS or w in ("owner", "eve"):
                    continue
                w = SYNONYMS.get(w.rstrip("s"), w.rstrip("s"))
                v[self.vocab.setdefault(w, len(self.vocab) % 8192)] += 1.0
            out.append(v)
        return out


class Clock:
    def __init__(self, now: datetime) -> None:
        self.now = now

    def __call__(self) -> datetime:
        return self.now


def make_store(tmp_path: Path, now: datetime) -> tuple[Transcripts, Clock]:
    clock = Clock(now)
    tr = Transcripts(TranscriptConfig(enabled=True, root=tmp_path / "mem" / "transcripts"), wall=clock)
    assert tr.enabled
    return tr, clock


def say(tr: Transcripts, conv: str, who: str, text: str, when: datetime, ch: str = "telegram") -> None:
    assert tr.append(ch, conv, who, "say", text, now=when)


def seed_lines(tr: Transcripts) -> None:
    say(tr, "t-1", "owner", "Carla, my landlord, says the rent includes water.", at(9, 15, 10))
    say(tr, "t-1", "eve", "Good to know, so only internet is extra.", at(9, 15, 10, 1))
    say(tr, "t-2", "owner", "Someone stole my bicycle from outside Kellner Hall.", at(9, 19, 9))
    say(tr, "t-2", "eve", "I'm sorry. Look for a used one under $150.", at(9, 19, 9, 1))
    say(tr, "v-3", "owner", "Squatted 245 today.", at(9, 20, 18), ch="voice")
    say(tr, "t-4", "owner", "Ordered pad thai again.", at(9, 24, 12))


def index_for(tr: Transcripts, embed: Optional[FakeEmbedder] = None) -> mi.MemoryIndex:
    idx = mi.MemoryIndex(mi.index_path(tr.root.parent), tr, embed if embed is not None else FakeEmbedder())
    assert idx.ok
    tr.ranker = idx
    return idx


# ---- the switches ---------------------------------------------------------------------------------------------

def test_switches_are_off_unless_set() -> None:
    assert not mi.switch("hybrid", {})
    assert mi.switch("hybrid", {mi.HYBRID_ENV: "1"}) and mi.switch(mi.RETRY_ENV, {mi.RETRY_ENV: "on"})
    assert not mi.switch("timeargs", {mi.TIMEARGS_ENV: "0"})


def test_tool_schema_all_off_is_the_old_one_and_each_switch_shapes_it() -> None:
    assert memory_search_tool() is MEMORY_SEARCH
    ranked = memory_search_tool(hybrid=True)
    assert "Every word of the query" not in ranked["description"] and "best match first" in ranked["description"]
    timed = memory_search_tool(timeargs=True)
    params = timed["parameters"]
    assert params["required"] == ["query", "days_back", "from", "to"]
    assert params["properties"]["from"]["type"] == ["string", "null"]
    assert "Every word of the query" in timed["description"] and "pass it as from and to" in timed["description"]
    assert "search once more with other words" in memory_search_tool(retry=True)["description"]
    # the cap is stated only where code enforces it: the Telegram brain, not voice
    assert "at most three memory calls" in memory_search_tool(retry=True)["description"]
    assert "at most three memory calls" not in memory_search_tool(retry=True, capped=False)["description"]
    # fixed per switch set: the cached prefix does not move between calls
    assert json.dumps(memory_search_tool(True, True, True)) == json.dumps(memory_search_tool(True, True, True))


# ---- the time resolver ------------------------------------------------------------------------------------------

def d(s: str) -> date:
    return date.fromisoformat(s)


def test_resolver_uses_the_owners_day_across_the_0400_line() -> None:
    before = at(9, 10, 2, 30)            # 02:30 on the 10th: still the owner's 9th
    after = at(9, 10, 4, 30)
    assert mi.owner_day(before) == d("2026-09-09") and mi.owner_day(after) == d("2026-09-10")
    assert mi.resolve_when("yesterday", before) == (d("2026-09-08"), d("2026-09-08"))
    assert mi.resolve_when("yesterday", after) == (d("2026-09-09"), d("2026-09-09"))
    assert mi.resolve_when("today", before) == (d("2026-09-09"), d("2026-09-09"))
    assert mi.resolve_when("last night", after) == (d("2026-09-09"), d("2026-09-09"))
    assert mi.resolve_when("3 days ago", before) == (d("2026-09-06"), d("2026-09-06"))


def test_resolver_weeks_months_and_dates() -> None:
    now = at(9, 23, 20)                   # Wednesday 2026-09-23
    assert date(2026, 9, 23).weekday() == 2
    assert mi.resolve_when("last week", now) == (d("2026-09-14"), d("2026-09-20"))
    assert mi.resolve_when("this week", now) == (d("2026-09-21"), d("2026-09-23"))
    assert mi.resolve_when("two weeks ago", now) == (d("2026-09-07"), d("2026-09-13"))
    assert mi.resolve_when("in May", now) == (d("2026-05-01"), d("2026-05-31"))
    assert mi.resolve_when("in December", now) == (d("2025-12-01"), d("2025-12-31"))   # never in the future
    assert mi.resolve_when("this month", now) == (d("2026-09-01"), d("2026-09-23"))
    assert mi.resolve_when("last month", now) == (d("2026-08-01"), d("2026-08-31"))
    assert mi.resolve_when("early August", now) == (d("2026-08-01"), d("2026-08-10"))
    assert mi.resolve_when("August 6", now) == (d("2026-08-06"), d("2026-08-06"))
    assert mi.resolve_when("6th of August", now) == (d("2026-08-06"), d("2026-08-06"))
    assert mi.resolve_when("since August 6", now) == (d("2026-08-06"), d("2026-09-23"))
    assert mi.resolve_when("2026-09-03", now) == (d("2026-09-03"), d("2026-09-03"))
    assert mi.resolve_when("last Tuesday", now) == (d("2026-09-22"), d("2026-09-22"))
    assert mi.resolve_when("on Wednesday", now) == (d("2026-09-23"), d("2026-09-23"))
    assert mi.resolve_when("last Wednesday", now) == (d("2026-09-16"), d("2026-09-16"))
    assert mi.resolve_when("last weekend", now) == (d("2026-09-19"), d("2026-09-20"))
    assert mi.resolve_when("a while back", now) is None and mi.resolve_when("", now) is None


def test_resolve_args_from_alone_both_and_unreadable() -> None:
    now = at(9, 23, 20)
    assert mi.resolve_args("last week", None, now) == ((d("2026-09-14"), d("2026-09-20")), False)
    assert mi.resolve_args("2026-09-01", "2026-09-05", now) == ((d("2026-09-01"), d("2026-09-05")), False)
    assert mi.resolve_args(None, None, now) == (None, False)
    assert mi.resolve_args("someday", None, now) == (None, True)
    assert mi.widen((d("2026-09-14"), d("2026-09-20")), d("2026-09-23")) == (d("2026-09-07"), d("2026-09-23"))


# ---- units -------------------------------------------------------------------------------------------------------

def test_units_are_rounds_an_owner_line_with_eves_reply() -> None:
    from eve_bridge.transcripts import speaker

    rows = [
        {"ts": "2026-09-01T10:00:00", "ch": "telegram", "conv": "a", "who": "eve", "kind": "say", "text": "Morning."},
        {"ts": "2026-09-01T10:01:00", "ch": "telegram", "conv": "a", "who": "owner", "kind": "say", "text": "Hi"},
        {"ts": "2026-09-01T10:02:00", "ch": "telegram", "conv": "a", "who": "eve", "kind": "say", "text": "Hey!"},
        {"ts": "2026-09-01T10:02:30", "ch": "telegram", "conv": "a", "who": "eve", "kind": "tool",
         "text": "found 3 results", "tool": "web_search"},
        {"ts": "2026-09-01T10:03:00", "ch": "telegram", "conv": "a", "who": "owner", "kind": "say",
         "text": "(forgotten)"},
        {"ts": "2026-09-01T10:04:00", "ch": "telegram", "conv": "a", "who": "system", "kind": "close", "text": ""},
        {"ts": "2026-09-01T10:05:00", "ch": "bus", "conv": "b", "who": "owner", "kind": "say", "text": "Tracker: 5"},
    ]
    units = mi.units_of("2026-09-01", rows, speaker)
    assert [u.head for u in units] == ["eve: Morning.", "Owner: Hi", "Owner: Tracker: 5"]
    assert units[1].reply == ["eve: Hey!", "eve (web search): found 3 results"]
    assert units[1].before == "eve: Morning." and units[2].ch == "bus"


# ---- the ranked search -------------------------------------------------------------------------------------------

def test_hybrid_finds_what_keyword_and_misses(tmp_path: Path) -> None:
    tr, _clock = make_store(tmp_path, at(9, 24, 20))
    seed_lines(tr)
    assert tr.search("landlord name")["hits"] == []                       # the control: keyword AND misses it
    idx = index_for(tr)
    assert idx.ensure_built()
    got = tr.search("landlord name")
    assert got["ranked"] is True
    assert got["hits"][0]["text"].startswith("Carla, my landlord")
    assert got["hits"][0]["after"].startswith("eve: Good to know")       # the reply rides along
    # by meaning alone: no word of the query is in the line
    got = tr.search("bike theft")
    assert got["hits"] and got["hits"][0]["text"].startswith("Someone stole my bicycle")
    assert len(tr.search("today")["hits"]) <= mi.TOP_K
    idx.close()


def test_hybrid_respects_the_range_and_the_char_cap(tmp_path: Path) -> None:
    tr, _clock = make_store(tmp_path, at(9, 24, 20))
    seed_lines(tr)
    for i in range(30):
        say(tr, f"t-x{i}", "owner", f"Bike note {i}: " + "spokes and gears " * 20, at(9, 23, 8, i))
    idx = index_for(tr)
    idx.ensure_built()
    got = tr.search("bicycle", since="2026-09-19", until="2026-09-19")
    assert [h["day"] for h in got["hits"]] == ["2026-09-19"]
    wide = tr.search("bike")
    assert len(json.dumps(wide, ensure_ascii=False)) <= 3000 and wide["truncated"] is True
    idx.close()


def test_falls_back_to_keyword_and_when_embeddings_or_index_are_down(tmp_path: Path) -> None:
    tr, _clock = make_store(tmp_path, at(9, 24, 20))
    seed_lines(tr)
    keyword = tr.search("landlord")
    emb = FakeEmbedder()
    idx = index_for(tr, emb)
    assert idx.search("landlord", "2026-09-01", "2026-09-30") is None      # not built yet: no ranking
    idx.ensure_built()
    emb.fail = True                                                        # the query's vector fails
    got = tr.search("landlord")
    assert "ranked" not in got and got["hits"] == keyword["hits"]
    assert tr.search("landlord name")["hits"] == []                        # and it is the AND, as before
    idx.close()
    assert tr.search("landlord")["hits"] == keyword["hits"]                # a closed index: the same


def test_a_new_line_is_searchable_at_once_and_reaches_the_index_off_the_loop(tmp_path: Path,
                                                                           monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(mi, "DEBOUNCE_SECS", 0.05)
    monkeypatch.setattr(mi, "EMBED_IDLE_SECS", 0.1)
    tr, _clock = make_store(tmp_path, at(9, 24, 20))
    seed_lines(tr)
    emb = FakeEmbedder()
    idx = index_for(tr, emb)
    tr.listeners.append(idx.notify)
    idx.start()
    for _ in range(100):
        if idx._built:
            break
        asyncio.run(asyncio.sleep(0.02))
    assert idx._built
    say(tr, "t-5", "owner", "The new apartment wifi password is on the fridge.", at(9, 24, 19))
    got = tr.search("wifi password")
    assert got["ranked"] and got["hits"][0]["text"].startswith("The new apartment wifi")
    for _ in range(100):                                                    # the worker embeds it
        if idx.stats()["vectors"] == idx.stats()["units"]:
            break
        asyncio.run(asyncio.sleep(0.02))
    assert idx.stats()["vectors"] == idx.stats()["units"] == 5
    idx.close()


def test_a_growing_round_leaves_no_vectors_behind_and_the_worker_embeds_it_once_quiet(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    tr, _clock = make_store(tmp_path, at(9, 21, 12))
    say(tr, "t-1", "owner", "start", at(9, 21, 10))
    emb = FakeEmbedder()
    idx = index_for(tr, emb)
    idx.ensure_built()
    day = tr.day_of(at(9, 21, 10))
    for i in range(40):                                                     # each refresh embeds the grown round
        say(tr, "t-1", "eve", f"reply part {i}", at(9, 21, 10, 1))
        idx.refresh([day], embed=True)
    assert idx.stats()["units"] == 1 and idx.stats()["vectors"] == 1         # was 41: one per version of the round
    idx.close()
    # the worker: the words at once, the vector only after EMBED_IDLE_SECS with no new line
    monkeypatch.setattr(mi, "DEBOUNCE_SECS", 0.02)
    monkeypatch.setattr(mi, "EMBED_IDLE_SECS", 0.5)
    tr2, _c = make_store(tmp_path / "w", at(9, 21, 12))
    say(tr2, "t-1", "owner", "start", at(9, 21, 10))
    emb2 = FakeEmbedder()
    idx2 = index_for(tr2, emb2)
    tr2.listeners.append(idx2.notify)
    idx2.start()
    for _ in range(100):
        if idx2._built:
            break
        asyncio.run(asyncio.sleep(0.02))
    first = emb2.texts
    for i in range(5):
        say(tr2, "t-1", "eve", f"reply part {i}", at(9, 21, 10, 1))
        asyncio.run(asyncio.sleep(0.1))
    assert emb2.texts == first                                              # still talking: nothing embedded
    for _ in range(100):
        if idx2.stats()["vectors"] == idx2.stats()["units"] and emb2.texts > first:
            break
        asyncio.run(asyncio.sleep(0.05))
    assert emb2.texts == first + 1 and idx2.stats()["vectors"] == 1          # one embedding, of the whole round
    idx2.close()


def test_bm25_candidates_come_from_the_searched_days(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(mi, "FTS_CANDIDATES", 5)
    monkeypatch.setattr(mi, "MIN_COS", 2.0)                                 # found by words only: BM25 decides
    tr, _clock = make_store(tmp_path, at(9, 24, 20))
    for i in range(20):                                                     # outside the range, the word many times
        say(tr, f"t-o{i}", "owner", "rent rent rent", at(8, 1 + i, 9))
    say(tr, "t-in", "owner", "The rent went up again this month.", at(9, 20, 9))
    idx = index_for(tr)
    idx.ensure_built()
    got = idx.search("rent", "2026-09-20", "2026-09-20")
    assert got is not None and [h["day"] for h in got["hits"]] == ["2026-09-20"]
    idx.close()


def test_vectors_are_kept_by_text_so_a_rebuild_embeds_nothing_twice(tmp_path: Path) -> None:
    tr, _clock = make_store(tmp_path, at(9, 24, 20))
    seed_lines(tr)
    emb = FakeEmbedder()
    idx = index_for(tr, emb)
    idx.ensure_built()
    first = emb.texts
    with idx._lock:
        idx._db.executescript("DELETE FROM fts; DELETE FROM units; DELETE FROM days;")   # the eval's seed shape
    idx.ensure_built()
    assert emb.texts == first and idx.stats()["units"] == 4
    idx.close()


# ---- the Memory: forget, ranges, retry ---------------------------------------------------------------------------

def memory(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, now: datetime, *, hybrid: bool = False,
           timeargs: bool = False, retry: bool = False, emb: Optional[FakeEmbedder] = None) -> Memory:
    for name, on in (("hybrid", hybrid), ("timeargs", timeargs), ("retry", retry)):
        monkeypatch.setenv(mi.SWITCHES[name], "1" if on else "0")
    fake = emb if emb is not None else FakeEmbedder()
    monkeypatch.setattr(mi, "openai_embedder", lambda: fake)
    store = tmp_path / "mem"
    tr, _clock = make_store(tmp_path, now)
    seed_lines(tr)
    mem = Memory(RecallConfig(store=store, notes=store / "notes"), tr, None)
    if mem.ranked is not None:
        mem.ranked.ensure_built()
    return mem


def test_forget_reaches_the_index_rows_and_vectors(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    mem = memory(tmp_path, monkeypatch, at(9, 24, 20), hybrid=True)
    assert mem.ranked is not None
    say(mem.transcripts, "t-9", "owner", f"My locker code is {SENTINEL}.", at(9, 24, 18))
    say(mem.transcripts, "t-9", "eve", "Got it.", at(9, 24, 18, 1))
    mem.ranked.ensure_built()
    path = mi.index_path(mem.cfg.store)
    assert SENTINEL.encode() in path.read_bytes()                         # the positive control
    assert mem.search(SENTINEL)["said"]
    prev = mem.forget_preview(SENTINEL)
    assert prev["layers"]["index"] == 1
    done = mem.forget_apply(prev["token"])
    assert done["layers"]["index"] == 1 and done["layers"]["transcripts"] == 2
    assert SENTINEL.encode() not in path.read_bytes()
    assert SENTINEL.lower().encode() not in path.read_bytes().lower()
    with mem.ranked._lock:
        orphans = mem.ranked._db.execute(
            "SELECT COUNT(*) FROM vectors WHERE hash NOT IN (SELECT hash FROM units)").fetchone()[0]
    assert orphans == 0                                                    # no vector outlives its round
    assert not mem.search(SENTINEL)["said"]
    assert not mem.search("Got it")["said"]                                 # eve's answer to it went too
    mem.ranked.close()


def test_forget_reaches_words_past_the_first_2000_chars_of_a_round(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    mem = memory(tmp_path, monkeypatch, at(9, 24, 20), hybrid=True)
    assert mem.ranked is not None
    say(mem.transcripts, "t-9", "owner", "Tell me a long story please.", at(9, 24, 18))
    say(mem.transcripts, "t-9", "eve", ("filler words here " * 130) + f" the secret is {SENTINEL}", at(9, 24, 18, 1))
    mem.ranked.ensure_built()
    path = mi.index_path(mem.cfg.store)
    assert SENTINEL.encode() in path.read_bytes()                         # the positive control: the reply is stored
    prev = mem.forget_preview(SENTINEL)
    assert prev["layers"]["index"] == 1                                    # counted past the FTS body's clip
    done = mem.forget_apply(prev["token"])
    assert done["ok"] and done["layers"]["index"] == 1
    assert SENTINEL.lower().encode() not in path.read_bytes().lower()
    mem.ranked.close()


def test_forget_cleans_an_index_left_on_disk_with_hybrid_off(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    on = memory(tmp_path, monkeypatch, at(9, 24, 20), hybrid=True)
    assert on.ranked is not None
    say(on.transcripts, "t-9", "owner", f"My locker code is {SENTINEL}.", at(9, 24, 18))
    on.ranked.ensure_built()
    on.ranked.close()
    path = mi.index_path(on.cfg.store)
    assert SENTINEL.encode() in path.read_bytes()                          # the positive control: the file holds it
    monkeypatch.setenv(mi.HYBRID_ENV, "0")
    off = Memory(on.cfg, on.transcripts, None)
    assert off.ranked is None
    prev = off.forget_preview(SENTINEL)
    assert prev["layers"]["index"] == 1                                    # the switch is off, the file is counted
    done = off.forget_apply(prev["token"])
    assert done["ok"] and done["layers"]["index"] == 1
    assert SENTINEL.lower().encode() not in path.read_bytes().lower()


def test_a_forget_that_cannot_clean_the_index_says_so(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(mi, "COMPACT_WAITS", (0.0, 0.0))
    mem = memory(tmp_path, monkeypatch, at(9, 24, 20), hybrid=True)
    assert mem.ranked is not None
    say(mem.transcripts, "t-9", "owner", f"My locker code is {SENTINEL}.", at(9, 24, 18))
    mem.ranked.ensure_built()
    tries = []

    def busy() -> None:
        tries.append(1)
        raise mi.sqlite3.OperationalError("database is locked")

    monkeypatch.setattr(mem.ranked, "_compact", busy)
    prev = mem.forget_preview(SENTINEL)
    done = mem.forget_apply(prev["token"])
    assert len(tries) == 2                                                 # tried again before giving up
    assert done["ok"] is False and done["failed"] == ["index"] and "index" in done["reason"]
    mem.ranked.close()


def test_only_the_daemon_starts_the_index_worker(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    mem = memory(tmp_path, monkeypatch, at(9, 24, 20), hybrid=True)
    assert mem.ranked is not None and mem.ranked._worker is None           # the CLI and the eval: no worker
    # Review D10: a search before the index is built must not start the build-and-embed worker either.
    mem.ranked._built = False                                              # as for a missing index file
    assert mem.ranked.search("rent", "2026-09-01", "2026-09-24") is None   # not built yet: the keyword AND
    assert mem.ranked._worker is None
    mem.ranked.close()
    daemon = Memory(mem.cfg, mem.transcripts, None, background=True)
    assert daemon.ranked is not None and daemon.ranked._worker is not None
    daemon.ranked.close()


def test_a_range_limits_the_words_and_widens_once(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    mem = memory(tmp_path, monkeypatch, at(9, 24, 20), timeargs=True)
    got = mem.handle_tool("memory_search", {"query": "squatted", "days_back": None, "from": "4 days ago",
                                            "to": None})
    assert got["range"] == {"from": "2026-09-20", "to": "2026-09-20"} and "widened" not in got
    assert [h["day"] for h in got["said"]] == ["2026-09-20"]
    got = mem.handle_tool("memory_search", {"query": "squatted", "days_back": None, "from": "yesterday",
                                            "to": None})
    assert got["widened"] == {"from": "2026-09-16", "to": "2026-09-24"} and got["said"]
    got = mem.handle_tool("memory_search", {"query": "squat weight", "days_back": None, "from": "4 days ago",
                                            "to": None})
    assert got["fewer_words"] is True and "widened" not in got                 # AND misses "weight"; the range
    assert [h["text"] for h in got["said"]] == ["Squatted 245 today."]        # keeps half the words narrow
    got = mem.handle_tool("memory_search", {"query": "squatted", "days_back": None, "from": "a while back",
                                            "to": None})
    assert got["time_note"] and got["said"]
    off = memory(tmp_path / "off", monkeypatch, at(9, 24, 20))
    got = off.handle_tool("memory_search", {"query": "squatted", "days_back": None, "from": "yesterday"})
    assert "range" not in got and got["said"]                              # switch off: from/to are not read


def test_loose_search_ranks_the_rarer_word_first(tmp_path: Path) -> None:
    tr, _clock = make_store(tmp_path, at(9, 24, 20))
    say(tr, "t-r", "owner", "The studio rent is $1,180 a month.", at(9, 2, 9))
    for i in range(6):
        say(tr, f"t-n{i}", "owner", f"New playlist number {i}.", at(9, 10 + i, 9))
    assert tr.search("new rent")["hits"] == []                                 # the control: no line has both
    got = tr.search("new rent", loose=True)
    assert got["hits"][0]["text"].startswith("The studio rent")               # older, but the rarer word
    assert len(got["hits"]) == 7


def test_retry_searches_again_wider_and_looser(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    mem = memory(tmp_path, monkeypatch, at(9, 24, 20), retry=True)
    say(mem.transcripts, "t-old", "owner", "The garage door code changed to 4471.", at(3, 2, 9))
    got = mem.search("garage door code")                                    # 206 days back: past the 30
    assert got["said"] and got["searched_again"].startswith("the last 365 days")
    got = mem.search("landlord name")                                       # AND misses; half the words find it
    assert got["said"][0]["text"].startswith("Carla") and "fewer words" in got["searched_again"]
    got = mem.search("zeppelin hangar")
    assert not got["said"] and "search once more with other words" in got["note"]
    off = memory(tmp_path / "off", monkeypatch, at(9, 24, 20))
    assert not off.search("landlord name")["said"] and "searched_again" not in off.search("landlord name")


# ---- the Telegram brain: at most three memory reads a turn -------------------------------------------------------

def _reply(text: str) -> dict[str, Any]:
    return {"output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}]}


def _search_call(i: int) -> dict[str, Any]:
    return {"output": [{"type": "function_call", "name": "memory_search", "call_id": f"c{i}",
                        "arguments": json.dumps({"query": "squatted", "days_back": None})}]}


def _turns(mem: Memory, scripts: list[list[dict[str, Any]]], monkeypatch: pytest.MonkeyPatch) -> list[list[bool]]:
    """Each script as one Telegram turn through the real _think; → per turn, whether each memory call ran."""
    from eve_bridge import telegram as tg

    script: list[dict[str, Any]] = []
    calls: list[dict[str, Any]] = []

    async def create(payload: dict[str, Any]) -> dict[str, Any]:
        return script.pop(0) if script else _reply("done")

    inlet = tg.TelegramInlet(tg.TelegramConfig(), object(), create, memory=mem, toolset_mode="off", brief=lambda: "")

    async def tool(name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:
        r = await tg.TelegramInlet._memory_tool(inlet, name, args)
        calls.append(r)
        return r

    inlet._tool = tool
    monkeypatch.setattr(tg, "MAX_TOOL_ROUNDS", 6)
    out = []
    for turn in scripts:
        script[:] = turn
        calls.clear()
        asyncio.run(inlet._think([tg.message_item("user", "how much did I squat?")], 0))
        out.append([r.get("ok") is not False for r in calls])
        if out[-1] and not out[-1][-1]:
            assert calls[-1]["reason"] == tg.MEMORY_CALLS_SPENT
    return out


def test_telegram_allows_three_memory_reads_a_turn_with_retry_on(tmp_path: Path,
                                                                  monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge import spend

    spend.set_dir(tmp_path / "spend")
    four = [_search_call(i) for i in range(4)] + [_reply("done")]
    try:
        on = memory(tmp_path / "on", monkeypatch, at(9, 24, 20), retry=True)
        # the fourth read is refused; the next turn starts with three again
        assert _turns(on, [four, [_search_call(9), _reply("done")]], monkeypatch) == [
            [True, True, True, False], [True]]
        off = memory(tmp_path / "off", monkeypatch, at(9, 24, 20))
        assert _turns(off, [list(four)], monkeypatch) == [[True, True, True, True]]     # switch off: no cap
    finally:
        spend.set_dir(None)


def test_voice_is_not_told_of_a_cap_it_does_not_have(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    mem = memory(tmp_path, monkeypatch, at(9, 24, 20), retry=True)
    assert "at most three memory calls" in mem.tools()[0]["description"]
    assert "at most three memory calls" not in mem.tools(voice=True)[0]["description"]


def test_two_telegram_turns_at_once_keep_their_own_memory_count(tmp_path: Path,
                                                                monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge import spend
    from eve_bridge import telegram as tg

    spend.set_dir(tmp_path / "spend")
    try:
        mem = memory(tmp_path, monkeypatch, at(9, 24, 20), retry=True)
        inlet = tg.TelegramInlet(tg.TelegramConfig(), object(), None, memory=mem, toolset_mode="off",
                                 brief=lambda: "")
        results: dict[str, list[bool]] = {"a": [], "b": []}

        async def turn(key: str, gate: asyncio.Event, other: asyncio.Event) -> None:
            tg._TURN_MEMORY_CALLS.set([0])                                  # what _think does as a turn starts
            for i in range(3):
                r = await tg.TelegramInlet._memory_tool(inlet, "memory_search", {"query": "squatted"})
                results[key].append(r.get("ok") is not False)
                if i == 0:
                    other.set()
                    await gate.wait()                                       # the other turn starts mid-way

        async def both() -> None:
            ga, gb = asyncio.Event(), asyncio.Event()
            await asyncio.gather(turn("a", ga, gb), turn("b", gb, ga))

        asyncio.run(both())
        assert results == {"a": [True, True, True], "b": [True, True, True]}   # neither reset nor spent the other's
    finally:
        spend.set_dir(None)


def test_eval_features_flag_sets_every_switch(monkeypatch: pytest.MonkeyPatch) -> None:
    import importlib.util
    import os

    tool = Path(__file__).resolve().parents[1] / "tools" / "memory_eval.py"
    spec = importlib.util.spec_from_file_location("memory_eval_features", tool)
    assert spec and spec.loader
    ev = importlib.util.module_from_spec(spec)
    monkeypatch.setitem(sys.modules, "memory_eval_features", ev)
    spec.loader.exec_module(ev)
    assert ev.parse_features("") == [] and ev.parse_features("retry,hybrid") == ["hybrid", "retry"]
    assert ev.parse_features("all")[:3] == ["hybrid", "timeargs", "retry"]
    with pytest.raises(SystemExit):
        ev.parse_features("telepathy")
    for var in mi.SWITCHES.values():
        monkeypatch.setenv(var, "1")
    ev.set_features(["timeargs"])
    assert [os.environ[v] for v in mi.SWITCHES.values()] == ["0", "1", "0"]
    for var in mi.SWITCHES.values():
        monkeypatch.delenv(var)
