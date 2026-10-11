"""records.py: typed, git-tracked records the brains read, the owner stars into, and only the dream writes."""

from __future__ import annotations

import inspect
import json
import os
import stat
import subprocess
import sys
import time
import types
from datetime import datetime
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import mem0_memory, records
from eve_bridge.mem0_memory import Mem0Config, OwnerMemory
from eve_bridge.recall import RecallConfig
from eve_bridge.records import (
    DayChange,
    Record,
    RecordsReader,
    apply_day,
    consolidate,
    forget_lines,
    latest_day,
    load_records,
    parse_record,
    profile,
    profile_for_voice,
    reconcile_day,
    render_record,
    search,
    squash_history,
    star,
    stars,
)

DAY = "2026-09-20"
DAY_TEXT = ("12:01 texted Owner: ramen for lunch now, pasta is over\n"
            "12:02 texted eve: Noted, ramen it is.")
DINING = Record(id="dining", type="preference", aliases=["food", "lunch", "restaurants", "takeout"],
                facts=["Loves pasta (said 2026-09-15).", "Usual lunch spot is [[cafe-nero]] (said 2026-09-18)."],
                updated="2026-09-18")
CAFE = Record(id="cafe-nero", type="place", aliases=["cafe", "coffee place", "nero"],
              facts=["The coffee place on the main street (said 2026-09-18)."])


def cfg_at(tmp_path: Path) -> RecallConfig:
    store = tmp_path / "memory"
    store.mkdir()
    return RecallConfig(store=store, notes=tmp_path / "notes")


def rdir(cfg: RecallConfig) -> Path:
    return records.records_dir(cfg)


def seed(cfg: RecallConfig, *recs: Record) -> None:
    rdir(cfg).mkdir(parents=True, exist_ok=True)
    for rec in recs:
        (rdir(cfg) / f"{rec.id}.md").write_text(render_record(rec), encoding="utf-8")


def git(cfg: RecallConfig, *args: str) -> str:
    return subprocess.run(["git", "-C", str(rdir(cfg)), *args], capture_output=True, text=True).stdout


class FakeClient:
    """The dream's client: reconcile(body) and consolidate(body) → JSON text, recording each body."""

    def __init__(self, *results: Any) -> None:
        self.results = list(results)
        self.bodies: list[tuple[str, str]] = []

    def _next(self, method: str, body: str) -> str:
        self.bodies.append((method, body))
        result = self.results.pop(0)
        if isinstance(result, Exception):
            raise result
        return result if isinstance(result, str) else json.dumps(result)

    def reconcile(self, body: str) -> str:
        return self._next("reconcile", body)

    def consolidate(self, body: str) -> str:
        return self._next("consolidate", body)


def result(**over: Any) -> dict[str, Any]:
    base = {"records": [{"id": "dining", "type": "preference", "aliases": ["food", "lunch", "ramen"],
                         "facts": ["Now prefers ramen for lunch (changed 2026-09-20; was pasta).",
                                   "Usual lunch spot is [[cafe-nero]] (said 2026-09-18)."]},
                        {"id": "ramen-bar", "type": "place", "aliases": ["ramen place"],
                         "facts": ["Where lunch comes from now (said 2026-09-20)."]}],
            "forget": [],
            "profile": {"life_context": ["Works on a desk robot called eve."],
                        "acting": ["Order lunch without asking; ask before anything over $50."],
                        "talking": ["Short texts, no lists."]},
            "journal": {"title": "Ramen replaces pasta", "happened": ["12:01 lunch talk by text"],
                        "learned": ["Prefers ramen now"], "open": ["Find the ramen bar's hours"],
                        "corrections": ["Lunch: pasta became ramen (2026-09-20)"]}}
    base.update(over)
    return base


# ---- the file format --------------------------------------------------------------------------

def test_a_record_round_trips_and_a_hand_edited_one_still_parses() -> None:
    assert parse_record(render_record(DINING)) == DINING
    assert DINING.links == ["cafe-nero"]
    hand = "---\nid: dining\nType: Preference\naliases: [ food , 'lunch' ]\n---\n\nsome prose\n\n* Loves pasta.\n- Hates kale\n"
    rec = parse_record(hand)
    assert rec is not None and rec.aliases == ["food", "lunch"] and rec.facts == ["Loves pasta.", "Hates kale"]
    assert rec.type == "preference"
    for bad in ("", "no frontmatter", "---\nid: Has Spaces\n---\n- x", "---\ntype: person\n---\n- no id"):
        assert parse_record(bad) is None


def test_the_records_folder_is_the_configs_and_load_skips_what_is_not_a_record(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    assert rdir(cfg) == cfg.records_dir == cfg.store / "records"
    seed(cfg, DINING, CAFE)
    (rdir(cfg) / "profile.md").write_text("---\nid: profile\n---\n# hi\n")
    (rdir(cfg) / "imposter.md").write_text(render_record(DINING))
    star(cfg, "my sister lives by the sea")
    assert sorted(load_records(cfg)) == ["cafe-nero", "dining"]
    assert load_records(RecallConfig(tmp_path / "none", tmp_path / "none")) == {}


# ---- stars -----------------------------------------------------------------------------------------

def test_a_star_is_one_dated_line_in_starred_md_and_saying_it_twice_is_one(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    line = star(cfg, "  my name is Sam. ", when=datetime(2026, 9, 11, 15))
    assert line == "- My name is Sam (2026-09-11)"
    assert star(cfg, "MY NAME IS SAM!", when=datetime(2026, 9, 12, 9)) == line          # case-insensitive
    assert star(cfg, "my sister is Ana", when=datetime(2026, 9, 12, 1, 30)) == "- My sister is Ana (2026-09-11)"
    assert star(cfg, "ok") is None and star(cfg, "") is None
    text = (rdir(cfg) / "starred.md").read_text()
    assert text.count("My name is Sam") == 1 and text.startswith("# Starred")
    assert stars(cfg) == ["My name is Sam", "My sister is Ana"]                           # text only, newest last
    assert stars(cfg, limit=1) == ["My sister is Ana"]
    assert stat.S_IMODE(os.stat(rdir(cfg) / "starred.md").st_mode) == 0o600
    assert stat.S_IMODE(os.stat(rdir(cfg)).st_mode) == 0o700
    assert not (cfg.store / "HIGHLIGHTS.md").exists()


def test_a_hand_typed_or_migrated_star_line_reads_too(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    rdir(cfg).mkdir(parents=True)
    (rdir(cfg) / "starred.md").write_text("# Starred\n\n- ★ Allergic to peanuts (2026-09-01)\n- Likes jazz\n")
    assert stars(cfg) == ["Allergic to peanuts", "Likes jazz"]
    assert star(cfg, "allergic to peanuts") == "- Allergic to peanuts (2026-09-01)"


# ---- the profile -------------------------------------------------------------------------------------

def test_a_star_newer_than_the_profile_is_on_top_of_it_verbatim(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    assert profile(cfg) == ""                                                   # no stars, no page: nothing
    star(cfg, "my name is Sam", when=datetime(2026, 9, 11, 12))
    assert profile(cfg).startswith(records.FRESH_STARS_HEADING)                 # no page yet: the stars are it
    reconcile_day(cfg, FakeClient(result()), DAY, DAY_TEXT)
    prof = profile(cfg)
    assert "They asked you to remember" not in prof and "My name is Sam" not in prof   # the page has read it
    star(cfg, "my sister is Ana", when=datetime(2026, 9, 22, 12))
    prof = RecordsReader(cfg).profile()
    assert prof.startswith("---\n")                                           # the frontmatter stays on top
    assert prof.index(records.FRESH_STARS_HEADING) < prof.index("## Life context")   # stars above every section
    assert "- My sister is Ana (2026-09-22)" in prof
    assert "My name is Sam" not in prof and "## Life context" in prof
    (rdir(cfg) / "profile.md").write_text("# The owner\n" + "x\n" * 5000)
    assert 0 < len(profile(cfg)) <= records.MAX_PROFILE_CHARS


def test_the_voice_profile_drops_the_index_and_stays_small(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, DINING, CAFE)
    reconcile_day(cfg, FakeClient(result()), DAY, DAY_TEXT)
    full = profile(cfg)
    assert records.INDEX_HEADING == "## Records you can search or read by id"          # the heading it strips
    assert records.INDEX_HEADING in full and "- cafe-nero (place)" in full               # positive control
    voice = profile_for_voice(cfg)
    assert records.INDEX_HEADING not in voice and "cafe-nero (place)" not in voice
    assert "## Life context" in voice and "Short texts" in voice and "id: profile" not in voice
    star(cfg, "my name is Sam", when=datetime(2026, 9, 25, 12))
    assert profile_for_voice(cfg).startswith(records.FRESH_STARS_HEADING)
    (rdir(cfg) / "profile.md").write_text("# The owner\n" + "y\n" * 5000)
    assert 0 < len(profile_for_voice(cfg)) <= records.MAX_VOICE_PROFILE_CHARS


# ---- don't mention this again --------------------------------------------------------------------------

def _source_bytes(cfg: RecallConfig) -> dict[str, bytes]:
    """Every file in the records folder except hidden.md itself (and git's own), as bytes."""
    return {str(p.relative_to(rdir(cfg))): p.read_bytes() for p in sorted(rdir(cfg).rglob("*"))
            if p.is_file() and ".git" not in p.parts and p.name != records.HIDDEN_FILE}


def test_hide_is_one_dated_private_line_deduped_and_unhide_takes_it_back(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    assert records.hidden(cfg) == [] and records.hidden_match(cfg) is None
    line = records.hide(cfg, "  the  dentist. ", when=datetime(2026, 10, 9, 15))
    assert line == "- the dentist (2026-10-09)"
    assert records.hide(cfg, "THE DENTIST", when=datetime(2026, 10, 10, 9)) == line          # same words: one line
    assert records.hide(cfg, '"ana\'s birthday"', when=datetime(2026, 10, 10, 1)) == '- "ana\'s birthday" (2026-10-09)'
    assert records.hide(cfg, "") is None and records.hide(cfg, " . ") is None and records.hide(cfg, "a") is None
    path = rdir(cfg) / records.HIDDEN_FILE
    text = path.read_text()
    assert text.startswith("# Hidden") and text.count("dentist") == 1
    assert stat.S_IMODE(os.stat(path).st_mode) == 0o600
    assert records.hidden(cfg) == ["the dentist", '"ana\'s birthday"']
    assert records.unhide(cfg, "The Dentist.") is True
    assert records.unhide(cfg, "the dentist") is False and records.unhide(cfg, "") is False
    assert records.hidden(cfg) == ['"ana\'s birthday"']
    assert stat.S_IMODE(os.stat(path).st_mode) == 0o600
    assert "hidden" not in load_records(cfg)                                               # never a record


def test_hidden_words_leave_the_profile_and_the_searches_but_nothing_is_deleted(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, DINING, CAFE)
    reconcile_day(cfg, FakeClient(result()), DAY, DAY_TEXT)
    star(cfg, "my sister is Ana", when=datetime(2026, 9, 22, 12))
    star(cfg, "my dentist is Dr Lee", when=datetime(2026, 9, 22, 12))
    full, voice = profile(cfg), profile_for_voice(cfg)
    # positive controls: every line about to be hidden is there first
    assert "Order lunch without asking" in full and "Order lunch without asking" in voice
    assert "Dr Lee" in full and "Dr Lee" in voice and "My sister is Ana" in full
    assert search(load_records(cfg), "coffee place")[0]["id"] == "cafe-nero"
    reader = RecordsReader(cfg, FakeIndex([{"text": "Dentist is on main street", "day": "2026-09-20", "channel": ""},
                                           {"text": "Likes jazz", "day": "2026-09-19", "channel": ""}]))
    assert len(reader.search("street")["recalled"]) == 2
    before = _source_bytes(cfg)

    records.hide(cfg, "order lunch")                     # every word, any case, anywhere in the line
    records.hide(cfg, "dentist")
    records.hide(cfg, '"the coffee place on"')           # a quoted phrase as written
    full, voice = profile(cfg), profile_for_voice(cfg)
    assert "Order lunch" not in full and "Order lunch" not in voice
    assert "Dr Lee" not in full and "Dr Lee" not in voice
    assert "My sister is Ana" in full and "My sister is Ana" in voice                    # the rest stays
    assert "## Life context" in full and "Works on a desk robot" in full and "Short texts" in voice
    assert full.index(records.FRESH_STARS_HEADING) < full.index("## Life context")
    hits = search(load_records(cfg), "coffee place", hide=records.hidden_match(cfg))
    assert all("The coffee place on the main street" not in h["line"] for h in hits)
    assert search(load_records(cfg), "coffee place")[0]["id"] == "cafe-nero"            # without hide: as before
    assert reader.search("street")["recalled"] == ["(2026-09-19) Likes jazz"]          # the dentist row is left out
    assert "coffee place on the main street" not in reader.get("cafe-nero").get("record", "")
    assert _source_bytes(cfg) == before                                                  # nothing deleted anywhere
    assert "The coffee place on the main street" in (rdir(cfg) / "cafe-nero.md").read_text()

    records.unhide(cfg, "dentist")                       # unhidden: back on the next read
    assert "Dr Lee" in profile(cfg) and "Order lunch" not in profile(cfg)


def test_a_star_wholly_hidden_drops_the_fresh_heading_and_a_hide_survives_a_forget_of_other_words(
        tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    star(cfg, "my dentist is Dr Lee", when=datetime(2026, 9, 22, 12))
    assert profile(cfg).startswith(records.FRESH_STARS_HEADING)
    records.hide(cfg, "dr lee")
    assert profile(cfg) == "" and profile_for_voice(cfg) == ""
    assert forget_lines(cfg, lambda ln: "jazz" in ln) == 0
    assert records.hidden(cfg) == ["dr lee"]                                             # hidden.md is not a record
    assert forget_lines(cfg, lambda ln: "lee" in ln.lower()) == 2                        # the star and the hide
    assert records.hidden(cfg) == [] and (rdir(cfg) / records.HIDDEN_FILE).exists()


# ---- what the brains may do --------------------------------------------------------------------------

def test_search_is_keyword_over_aliases_and_lines_and_aliases_count_more() -> None:
    recs = {"dining": DINING, "cafe-nero": CAFE}
    hits = search(recs, "where do I get lunch?")
    assert hits[0]["id"] == "dining" and "lunch spot" in hits[0]["line"]
    assert search(recs, "coffee")[0]["id"] == "cafe-nero"
    assert search(recs, "main street")[0]["line"].startswith("The coffee place")
    assert search(recs, "") == [] and search(recs, "quantum") == []
    assert len(search(recs, "the", limit=1)) <= 1


def test_the_reader_re_reads_disk_so_an_owner_edit_counts_at_once(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, DINING)
    reader = RecordsReader(cfg)
    assert reader.profile() == ""
    assert reader.search("pasta")["hits"][0]["id"] == "dining"
    (rdir(cfg) / "dining.md").write_text(render_record(Record("dining", "preference", ["food"],
                                                              ["Went off pasta (said 2026-09-21)."])))
    assert reader.search("pasta")["hits"][0]["line"].startswith("Went off")
    assert reader.get("dining")["record"].startswith("---\nid: dining")
    assert reader.get("nope")["ok"] is False
    assert reader.search("quantum") == {"ok": True, "hits": [], "note": "nothing matched"}


class FakeIndex:
    def __init__(self, hits: Any) -> None:
        self.hits = hits

    def search(self, query: str) -> Any:
        if isinstance(self.hits, Exception):
            raise self.hits
        return self.hits


def test_recalled_memories_carry_their_day_and_channel(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    reader = RecordsReader(cfg, FakeIndex([{"text": "Sister lives by the sea", "day": "2026-09-20", "channel": "voice"},
                                           {"text": "Likes jazz", "day": "2026-09-19", "channel": ""},
                                           {"text": "Undated", "day": "", "channel": ""}]))
    out = reader.search("sister")
    assert out["recalled"] == ["(2026-09-20, voice) Sister lives by the sea", "(2026-09-19) Likes jazz", "Undated"]
    assert "note" not in out
    assert RecordsReader(cfg, FakeIndex(RuntimeError("boom"))).search("x")["note"] == "nothing matched"


def test_keyword_hits_still_return_when_the_meaning_search_times_out(tmp_path: Path,
                                                                    monkeypatch: pytest.MonkeyPatch) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, DINING)

    class Slow:
        def search(self, query: str, **kw: Any) -> dict[str, Any]:
            time.sleep(2.0)
            return {"results": [{"memory": "late", "score": 0.9}]}

    monkeypatch.setattr(mem0_memory, "SEARCH_TIMEOUT_SECS", 0.2)
    index = OwnerMemory(Mem0Config(enabled=True, home=tmp_path / "mem0"), factory=lambda _c: Slow())
    start = time.monotonic()
    out = RecordsReader(cfg, index).search("pasta")
    assert time.monotonic() - start < 1.5
    assert out["hits"][0]["id"] == "dining" and "recalled" not in out


def test_latest_day_is_the_newest_journal_and_its_title(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    assert latest_day(cfg) is None
    reconcile_day(cfg, FakeClient(result()), DAY, DAY_TEXT)
    reconcile_day(cfg, FakeClient(result(journal={**result()["journal"], "title": "Quiet day"})), "2026-09-21",
                  DAY_TEXT)
    (rdir(cfg) / "days" / "notes.md").write_text("# not a day\n")
    assert latest_day(cfg) == ("2026-09-21", "Quiet day")


# ---- the dream: reconcile one day ----------------------------------------------------------------------

def test_a_day_is_dreamt_into_records_a_profile_a_journal_and_one_commit(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, DINING, CAFE)
    star(cfg, "my name is Sam", when=datetime(2026, 9, 11, 12))
    client = FakeClient(result())
    change = reconcile_day(cfg, client, DAY, DAY_TEXT)
    assert isinstance(change, DayChange)
    assert (change.changed, change.created, change.corrections) == (1, 1, 1)
    assert change.journal == rdir(cfg) / "days" / f"{DAY}.md"
    method, body = client.bodies[0]
    assert method == "reconcile" and len(client.bodies) == 1                     # ONE model call
    assert "## Records now" in body and "Loves pasta" in body and "## The profile page now" in body
    assert "- My name is Sam (2026-09-11)" in body and "ramen for lunch now" in body
    recs = load_records(cfg)
    assert recs["dining"].facts[0].startswith("Now prefers ramen") and recs["dining"].updated == DAY
    assert recs["cafe-nero"] == CAFE and "ramen-bar" in recs
    prof = (rdir(cfg) / "profile.md").read_text()
    assert "updated: 2026-09-20" in prof and "Order lunch without asking" in prof
    assert "- dining (preference): food, lunch, ramen" in prof and "- cafe-nero (place)" in prof
    journal = change.journal.read_text()
    assert "title: Ramen replaces pasta" in journal and "## What happened\n- 12:01 lunch talk" in journal
    assert "## Still open\n- Find the ramen bar" in journal and "## Corrections\n- Lunch: pasta became ramen" in journal
    # git: the root is the records folder, never the memory root that holds the transcripts
    assert (rdir(cfg) / ".git").is_dir() and not (cfg.store / ".git").exists()
    log = git(cfg, "log", "--format=%s").splitlines()
    assert log[0] == f"dream: {DAY}" and log[1] == f"as found before dream: {DAY}" and len(log) == 2
    old = git(cfg, "show", "HEAD~1:dining.md")
    assert "Loves pasta" in old and "Loves pasta" not in (rdir(cfg) / "dining.md").read_text()
    assert git(cfg, "status", "--porcelain") == ""


def test_the_owner_asking_to_forget_a_record_removes_it(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, DINING, CAFE)
    change = reconcile_day(cfg, FakeClient(result(records=[], forget=["cafe-nero", "profile", "../etc", "nobody"])),
                           DAY, DAY_TEXT)
    assert change is not None and change.changed == 1
    assert sorted(load_records(cfg)) == ["dining"] and (rdir(cfg) / "profile.md").exists()
    assert "cafe-nero" in git(cfg, "log", "--all", "--format=%s", "--name-only", "--", "cafe-nero.md")


def test_a_failed_or_empty_dream_writes_nothing(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, DINING)
    for bad in (RuntimeError("503"), "not json", json.dumps(["a", "list"])):
        assert reconcile_day(cfg, FakeClient(bad), DAY, DAY_TEXT) is None
    assert load_records(cfg)["dining"] == DINING
    assert not (rdir(cfg) / "profile.md").exists() and not (rdir(cfg) / "days").exists()
    idle = FakeClient()
    assert reconcile_day(cfg, idle, DAY, "   ") is None and reconcile_day(cfg, idle, "../x", DAY_TEXT) is None
    assert idle.bodies == []                                                   # no call for an empty day


def test_apply_skips_malformed_entries_and_never_escapes_the_folder(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    change = apply_day(cfg, {"records": [
        {"id": "../escape", "type": "person", "aliases": [], "facts": ["x"]},
        {"id": "Profile", "type": "person", "aliases": [], "facts": ["x"]},
        {"id": "starred", "type": "person", "aliases": [], "facts": ["x"]},
        {"id": "empty", "type": "person", "aliases": [], "facts": []},
        {"id": "sam", "type": "not-a-type", "aliases": ["  ", "colleague"], "facts": [" Works at a shop. ", ""]},
        "junk"], "forget": ["../../etc/passwd"], "profile": {"life_context": []}, "journal": "junk"}, DAY)
    assert (change.changed, change.created, change.corrections) == (0, 1, 0)
    sam = load_records(cfg)["sam"]
    assert sam.type == "preference" and sam.aliases == ["colleague"] and sam.facts == ["Works at a shop."]
    assert sorted(p.name for p in rdir(cfg).iterdir()) == ["days", "profile.md", "sam.md"]
    assert not (tmp_path / "escape.md").exists() and not (cfg.store / "escape.md").exists()
    assert "(nothing yet)" in (rdir(cfg) / "profile.md").read_text()
    assert "# 2026-09-20" in (rdir(cfg) / "days" / f"{DAY}.md").read_text()      # an untitled day is its date


def test_an_unchanged_record_is_not_rewritten(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, CAFE)
    same = {"id": CAFE.id, "type": CAFE.type, "aliases": CAFE.aliases, "facts": CAFE.facts}
    change = reconcile_day(cfg, FakeClient(result(records=[same])), DAY, DAY_TEXT)
    assert change is not None and (change.changed, change.created) == (0, 0)
    assert load_records(cfg)["cafe-nero"].updated == ""


def test_without_git_records_are_still_written(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, DINING)
    monkeypatch.setattr(records.shutil, "which", lambda name: None)
    assert reconcile_day(cfg, FakeClient(result()), DAY, DAY_TEXT) is not None
    assert load_records(cfg)["dining"].facts[0].startswith("Now prefers ramen")
    assert not (rdir(cfg) / ".git").exists()


def test_the_prompt_asks_for_corrections_and_the_journal() -> None:
    assert "dated correction" in records.RECONCILE_PROMPT or "correction and its date" in records.RECONCILE_PROMPT
    assert "fold every real fact about them" in records.RECONCILE_PROMPT
    assert set(records.RECONCILE_SCHEMA["properties"]["journal"]["required"]) == \
        {"title", "happened", "learned", "open", "plans", "corrections"}
    assert "conversation" not in records.TYPES                    # what happened once belongs in the journal


# ---- consolidate ------------------------------------------------------------------------------------------

SAM_A = Record("sam", "person", ["colleague"], ["Works at the shop (said 2026-09-01)."])
SAM_B = Record("sam-the-colleague", "person", ["sammy"], ["Likes tea (said 2026-09-02)."])
LUNCH = Record("lunch-club", "routine", ["lunch"], ["Eats with [[sam-the-colleague]] on Fridays (said 2026-09-03)."])


def test_consolidate_merges_two_ids_rewrites_links_and_commits_once(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, SAM_A, SAM_B, LUNCH)
    reconcile_day(cfg, FakeClient(result(records=[])), DAY, DAY_TEXT)
    before = len(git(cfg, "log", "--format=%s").splitlines())
    merged = {"id": "sam", "type": "person", "aliases": ["colleague"],
              "facts": ["Works at the shop (said 2026-09-01).", "Likes tea (said 2026-09-02)."]}
    client = FakeClient({"records": [merged], "merged": [{"id": "sam-the-colleague", "into": "sam"},
                                                         {"id": "ghost", "into": "sam"}]})
    assert consolidate(cfg, client) == (2, 1)                          # sam rewritten + lunch-club's link
    assert client.bodies[0][0] == "consolidate" and "sam-the-colleague" in client.bodies[0][1]
    recs = load_records(cfg)
    assert sorted(recs) == ["lunch-club", "sam"]
    assert set(recs["sam"].aliases) >= {"colleague", "sammy", "sam-the-colleague"}      # the code joins aliases
    assert recs["lunch-club"].facts == ["Eats with [[sam]] on Fridays (said 2026-09-03)."]
    prof = (rdir(cfg) / "profile.md").read_text()
    assert "- sam (person)" in prof and "sam-the-colleague (person)" not in prof and "## Life context" in prof
    log = git(cfg, "log", "--format=%s").splitlines()
    assert log[0] == "dream: consolidate" and len(log) == before + 1


def test_consolidate_with_nothing_to_change_is_no_commit(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, SAM_A, SAM_B)
    records.ensure_repo(rdir(cfg))
    records.commit(rdir(cfg), "start")
    assert consolidate(cfg, FakeClient({"records": [], "merged": []})) == (0, 0)
    assert consolidate(cfg, FakeClient(RuntimeError("down"))) == (0, 0)
    assert git(cfg, "log", "--format=%s").splitlines() == ["start"]
    (tmp_path / "lone").mkdir()
    lone = cfg_at(tmp_path / "lone")
    seed(lone, CAFE)
    idle = FakeClient()
    assert consolidate(lone, idle) == (0, 0) and idle.bodies == []      # one one-fact record: nothing to merge


def test_consolidate_skips_records_too_big_for_one_call(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, SAM_A, Record("big", "project", [], ["x" * 1000] * 130))
    idle = FakeClient()
    assert consolidate(cfg, idle) == (0, 0) and idle.bodies == []
    assert "not consolidated" in caplog.text


# ---- forget --------------------------------------------------------------------------------------------------

def test_forget_lines_reaches_records_profile_stars_and_journals_but_never_frontmatter(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, DINING, CAFE)
    star(cfg, "the cafe owner is rude", when=datetime(2026, 9, 21, 12))
    reconcile_day(cfg, FakeClient(result(journal={**result()["journal"], "happened": ["Talked about nero"]})),
                  DAY, DAY_TEXT)
    match = lambda line: any(w in line.casefold() for w in ("nero", "cafe", "coffee place"))     # noqa: E731
    head = git(cfg, "rev-parse", "HEAD")
    n = forget_lines(cfg, match, dry_run=True)
    assert n >= 4 and git(cfg, "rev-parse", "HEAD") == head and (rdir(cfg) / "cafe-nero.md").exists()
    assert forget_lines(cfg, match) == n
    assert not (rdir(cfg) / "cafe-nero.md").exists()                    # no fact left: the record goes
    dining = (rdir(cfg) / "dining.md").read_text()
    assert dining.startswith("---\nid: dining") and "cafe-nero" not in dining and "Now prefers ramen" in dining
    for path in [rdir(cfg) / "profile.md", rdir(cfg) / "starred.md", rdir(cfg) / "days" / f"{DAY}.md"]:
        body = path.read_text()
        assert "nero" not in body.casefold().split("---", 2)[-1] and "cafe owner" not in body
    assert (rdir(cfg) / "profile.md").read_text().startswith("---\nid: profile\nupdated: ")
    assert git(cfg, "log", "-1", "--format=%s").strip() == f"forget: {n} lines"
    assert forget_lines(cfg, match) == 0


def test_forget_keeps_frontmatter_lines_but_not_what_they_say(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, DINING, CAFE)
    reconcile_day(cfg, FakeClient(result(records=[], journal={**result()["journal"], "title": "Pasta day"})),
                  DAY, DAY_TEXT)
    removed = forget_lines(cfg, lambda line: "food" in line or "pasta" in line.lower())
    dining = (rdir(cfg) / "dining.md").read_text()
    assert dining.startswith("---\nid: dining\ntype: preference\naliases: [lunch, restaurants, takeout]\n")
    assert "pasta" not in dining and "[[cafe-nero]]" in dining and parse_record(dining) is not None
    journal = (rdir(cfg) / "days" / f"{DAY}.md").read_text()
    assert journal.startswith("---\nday: ") and "title: (forgotten)" in journal and "pasta" not in journal.lower()
    assert removed == 6         # alias, fact, profile index row, journal title, heading, correction
    assert forget_lines(cfg, lambda line: "cafe-nero" in line) >= 2       # its own id: the record goes whole
    assert not (rdir(cfg) / "cafe-nero.md").exists()
    assert not (rdir(cfg) / "dining.md").exists()                   # its one fact left was the link there


def test_squash_history_leaves_one_commit_and_no_old_object(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    seed(cfg, DINING)
    reconcile_day(cfg, FakeClient(result()), DAY, DAY_TEXT)
    old_blob = git(cfg, "rev-parse", "HEAD~1:dining.md").strip()
    assert "Loves pasta" in git(cfg, "cat-file", "-p", old_blob)                  # positive control
    assert squash_history(rdir(cfg)) is True
    assert git(cfg, "log", "--all", "--format=%s").splitlines() == ["memory (history squashed by a forget)"]
    r = subprocess.run(["git", "-C", str(rdir(cfg)), "cat-file", "-e", old_blob], capture_output=True)
    assert r.returncode != 0                                                       # pruned, not merely unreachable
    assert git(cfg, "status", "--porcelain") == "" and "Now prefers ramen" in git(cfg, "show", "HEAD:dining.md")
    assert squash_history(tmp_path / "not-a-repo") is False


def test_squash_history_never_touches_a_folder_inside_another_repository(tmp_path: Path) -> None:
    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    inner = tmp_path / "inner"
    inner.mkdir()
    (inner / "a.md").write_text("x\n")
    subprocess.run(["git", "-C", str(tmp_path), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(tmp_path), "-c", "user.name=t", "-c", "user.email=t@t", "-c",
                    "commit.gpgsign=false", "commit", "-qm", "outer"], check=True)
    assert squash_history(inner) is False
    assert subprocess.run(["git", "-C", str(tmp_path), "log", "--format=%s"], capture_output=True,
                          text=True).stdout.strip() == "outer"


# ---- git: sealed, and only its own repository ------------------------------------------------------------

def test_a_records_folder_inside_another_repository_is_never_committed_to_it(tmp_path: Path) -> None:
    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    cfg = cfg_at(tmp_path)
    seed(cfg, DINING)
    assert records.ensure_repo(rdir(cfg)) is False
    assert records.commit(rdir(cfg), "x") is None
    assert reconcile_day(cfg, FakeClient(result()), DAY, DAY_TEXT) is not None      # written, no history
    status = subprocess.run(["git", "-C", str(tmp_path), "log", "--oneline"], capture_output=True, text=True)
    assert status.returncode != 0 or status.stdout == ""


def test_an_ignore_everything_file_does_not_cost_the_history(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    rdir(cfg).mkdir()
    (rdir(cfg) / ".gitignore").write_text("*\n", encoding="utf-8")
    reconcile_day(cfg, FakeClient(result()), DAY, DAY_TEXT)
    tracked = git(cfg, "ls-files")
    assert "profile.md" in tracked and "dining.md" in tracked and f"days/{DAY}.md" in tracked


def test_the_memory_store_can_never_be_pushed_anywhere(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    outside = tmp_path / "outside.git"
    subprocess.run(["git", "init", "-q", "--bare", str(outside)], check=True)
    rdir(cfg).mkdir()
    subprocess.run(["git", "-C", str(rdir(cfg)), "init", "-q"], check=True)
    subprocess.run(["git", "-C", str(rdir(cfg)), "remote", "add", "origin", str(outside)], check=True)
    assert reconcile_day(cfg, FakeClient(result()), DAY, DAY_TEXT)
    g = ["git", "-C", str(rdir(cfg))]
    assert subprocess.run(g + ["remote"], capture_output=True, text=True).stdout == ""
    for extra in ([], ["--no-verify"]):
        for target in (str(outside), outside.as_uri(), "https://example.invalid/x.git", "git@example.invalid:x.git"):
            r = subprocess.run(g + ["push", *extra, target, "HEAD:refs/heads/main"], capture_output=True, text=True)
            assert r.returncode != 0, (extra, target)
    assert subprocess.run(["git", "-C", str(outside), "rev-parse", "--verify", "-q", "refs/heads/main"]).returncode != 0
    records.ensure_repo(rdir(cfg))
    urls = subprocess.run(g + ["config", "--get-all", "url.local-only-never-pushed:.pushInsteadOf"],
                          capture_output=True, text=True).stdout.split()
    assert len(urls) == len(set(urls)) == len(records._PUSH_PREFIXES)


# ---- the client, and what is gone -------------------------------------------------------------------------

def test_the_openai_client_never_stores_and_uses_one_schema_per_call(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[dict[str, Any]] = []

    class Responses:
        def create(self, **kw: Any) -> Any:
            calls.append(kw)
            return types.SimpleNamespace(output_text='{"ok": true}')

    class OpenAI:
        def __init__(self, **kw: Any) -> None:
            self.init = kw
            self.responses = Responses()

    monkeypatch.setitem(sys.modules, "openai", types.SimpleNamespace(OpenAI=OpenAI))
    client = records.OpenAIReconcileClient(api_key="k")
    assert client.reconcile("day") == '{"ok": true}' and client.consolidate("all") == '{"ok": true}'
    assert [c["store"] for c in calls] == [False, False]
    assert calls[0]["text"]["format"]["schema"] is records.RECONCILE_SCHEMA
    assert calls[1]["text"]["format"]["schema"] is records.CONSOLIDATE_SCHEMA
    assert all(c["text"]["format"]["strict"] is True for c in calls)
    assert records.make_client(environ={}) is None
    assert isinstance(records.make_client(environ={"OPENROUTER_API_KEY": "k"}), records.OpenAIReconcileClient)


def test_the_retired_machinery_is_gone() -> None:
    assert hasattr(records, "reconcile_day") and hasattr(records, "consolidate")       # positive control
    for name in ("Reconciler", "due_days", "day_notes", "reconciled_days", "STATE_FILE", "owner_stars",
                 "make_reconciler", "RecordsConfig", "configured", "RECORDS_DEFAULT"):
        assert not hasattr(records, name), name
    src = inspect.getsource(records)
    assert "chat_" "memory" not in src and "HIGHLIGHTS" not in src and ".reconciled" not in src
    assert "import asyncio" not in src


# ---- event dates vs the transcript day ---------------------------------------------------------------

def test_the_dream_is_told_each_line_carries_its_real_date_and_plans_keep_when_apart_from_said(tmp_path: Path) -> None:
    """days/2026-09-23.md labelled a 2026-09-24 00:08 event with 2026-09-23: the body said "Everything said on
    <day>" over lines with no date, and a transcript day runs 04:00 to 04:00. And a plan kept only when it was
    said, not when it happens."""
    cfg = cfg_at(tmp_path)
    journal = {**result()["journal"],
               "happened": ["2026-09-21 00:08 planned the trip by text"],
               "plans": [{"what": "Dentist", "when": "2026-09-30 10:00", "said": "2026-09-21"},
                         {"what": "Visit the ramen bar", "when": "", "said": "2026-09-20"}]}
    client = FakeClient(result(journal=journal), {"records": [], "merged": []})
    reconcile_day(cfg, client, DAY, "2026-09-20 23:59 texted Owner: dentist on the 30th at 10\n"
                                     "2026-09-21 00:08 texted Owner: and the ramen bar some day")
    body = client.bodies[0][1]
    assert "04:00 on 2026-09-20 to 04:00 on 2026-09-21" in body
    assert "real date" in body
    schema = records.RECONCILE_SCHEMA["properties"]["journal"]
    assert "plans" in schema["required"]
    assert set(schema["properties"]["plans"]["items"]["required"]) == {"what", "when", "said"}
    assert "when" in records.RECONCILE_PROMPT and "real date" in records.RECONCILE_PROMPT
    page = (rdir(cfg) / "days" / f"{DAY}.md").read_text()
    assert "- 2026-09-21 00:08 planned the trip by text" in page
    assert "## Plans\n- Dentist (when 2026-09-30 10:00; said 2026-09-21)\n" \
           "- Visit the ramen bar (when: no date yet; said 2026-09-20)" in page
