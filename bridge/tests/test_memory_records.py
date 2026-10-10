"""The record switches (memory plan items 2 and 3): validity on fact lines, and a source pointer on every fact.

EVE_MEMORY_VALIDITY and EVE_MEMORY_SOURCES, both off by default. Old record files must load, search and
render exactly as before; the guard holds a night's closures over the limit open (the night is still written); a
forget follows src pointers, with the switch on or off."""

from __future__ import annotations

import hashlib
import json
from datetime import datetime
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import records
from eve_bridge.mem0_memory import Mem0Config, OwnerMemory
from eve_bridge.memory import Memory
from eve_bridge.recall import RecallConfig
from eve_bridge.records import (
    Fact,
    Record,
    consolidate,
    forgets_pinned,
    load_records,
    parse_fact,
    parse_record,
    profile,
    reconcile_day,
    render_fact,
    render_record,
    search,
)
from eve_bridge.transcripts import TranscriptConfig, Transcripts

DAY = "2026-09-20"
DAY_TEXT = ("2026-09-20 12:01 texted Owner: rent goes up to 1180 from next month\n"
            "2026-09-20 12:02 texted eve: Noted, 1180 from October.")
OLD_RECORD = ("---\nid: home\ntype: place\naliases: [apartment, rent]\nupdated: 2026-09-01\n---\n"
              "- Rent is $1,050 a month (said 2026-09-01).\n"
              "- (until 2026-09-08) Lease ends August 31 (dated by 2026-09-08 13:01)\n"
              "- Owner's {curly} notes stay as written\n")
# The plain prompts as they were before the switches existed (sha256), so switching nothing changes nothing.
PLAIN_RECONCILE_SHA = "288bd17fbb0564981217a631e39d76aa759d62d4716221ff3a1c492a97d139df"
PLAIN_CONSOLIDATE_SHA = "f791010a10b0831d07d4c18638174c899f18b0379604f64228a34fc7462edec1"


@pytest.fixture(autouse=True)
def switches_off(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(records.VALIDITY_ENV, raising=False)
    monkeypatch.delenv(records.SOURCES_ENV, raising=False)


def cfg_at(tmp_path: Path) -> RecallConfig:
    store = tmp_path / "memory"
    store.mkdir(exist_ok=True)
    return RecallConfig(store=store, notes=tmp_path / "notes")


def put(cfg: RecallConfig, name: str, text: str) -> Path:
    folder = records.records_dir(cfg)
    folder.mkdir(parents=True, exist_ok=True)
    (folder / name).write_text(text, encoding="utf-8")
    return folder / name


class FakeClient:
    def __init__(self, *results: Any) -> None:
        self.results = list(results)
        self.bodies: list[str] = []

    def _next(self, body: str) -> str:
        self.bodies.append(body)
        return json.dumps(self.results.pop(0))

    def reconcile(self, body: str) -> str:
        return self._next(body)

    def consolidate(self, body: str) -> str:
        return self._next(body)


def day_result(records_out: list[dict[str, Any]], life: list[str] | None = None) -> dict[str, Any]:
    return {"records": records_out, "forget": [],
            "profile": {"life_context": life or ["Lives in the home record."], "acting": [], "talking": []},
            "journal": {"title": "Rent", "happened": [], "learned": [], "open": [], "plans": [],
                        "corrections": []}}


def fact(text: str, said: str = DAY, valid_from: str = DAY, valid_to: str = "", src: str = "") -> dict[str, str]:
    return {"text": text, "said": said, "valid_from": valid_from, "valid_to": valid_to, "src": src}


# ---- old files, unchanged ---------------------------------------------------------------------------------

def test_an_old_record_file_parses_and_renders_back_byte_for_byte(tmp_path: Path) -> None:
    rec = parse_record(OLD_RECORD)
    assert rec is not None and render_record(rec) == OLD_RECORD
    for line in rec.facts:
        f = parse_fact(line)
        assert f == Fact(text=line) and render_fact(f) == line       # no meta: all text, braces of the owner's kept
    cfg = cfg_at(tmp_path)
    put(cfg, "home.md", OLD_RECORD)
    assert render_record(load_records(cfg)["home"]) == OLD_RECORD


def test_an_old_record_searches_as_before_with_no_src_and_no_label(tmp_path: Path) -> None:
    rec = parse_record(OLD_RECORD)
    hits = search({"home": rec}, "rent lease")
    assert {h["line"] for h in hits} == set(rec.facts)
    assert all(set(h) == {"id", "line"} for h in hits)


def test_with_both_switches_off_the_prompts_and_schemas_are_the_plain_ones() -> None:
    assert hashlib.sha256(records.RECONCILE_PROMPT.encode()).hexdigest() == PLAIN_RECONCILE_SHA
    assert hashlib.sha256(records.CONSOLIDATE_PROMPT.encode()).hexdigest() == PLAIN_CONSOLIDATE_SHA
    assert records.reconcile_prompt() == records.RECONCILE_PROMPT
    assert records.reconcile_schema() is records.RECONCILE_SCHEMA
    assert records.consolidate_schema() is records.CONSOLIDATE_SCHEMA
    assert records.RECONCILE_SCHEMA["properties"]["records"]["items"]["properties"]["facts"]["items"] == \
        {"type": "string"}


def test_each_switch_adds_its_own_fields_to_the_fact_schema_and_prompt() -> None:
    def fields(v: bool, s: bool) -> list[str]:
        item = records.reconcile_schema(v, s)["properties"]["records"]["items"]["properties"]["facts"]["items"]
        return item["required"]

    assert fields(True, False) == ["text", "said", "valid_from", "valid_to"]
    assert fields(False, True) == ["text", "src"]
    assert fields(True, True) == ["text", "said", "valid_from", "valid_to", "src"]
    assert "valid_to" in records.reconcile_prompt(True, False) and "src" not in records.reconcile_prompt(True, False)
    assert "src:" in records.reconcile_prompt(False, True) and "valid_to" not in records.reconcile_prompt(False, True)
    assert "past tense" in records.reconcile_prompt(True, False)
    assert "valid_to" in records.consolidate_prompt(True, False)
    # each combination is one fixed string: the prompt cache keeps holding it
    assert records.reconcile_prompt(True, True) == records.reconcile_prompt(True, True)


def test_the_meta_round_trips_and_only_known_keys_count() -> None:
    f = Fact("Rent is $1,180 a month.", said=DAY, valid_from="2026-10-01", src="2026-09-20 12:01")
    line = render_fact(f)
    assert line == "Rent is $1,180 a month. {said: 2026-09-20; valid_from: 2026-10-01; src: 2026-09-20 12:01}"
    assert parse_fact(line) == f
    assert parse_fact("Likes {jazz: and blues}") == Fact("Likes {jazz: and blues}")


# ---- validity ----------------------------------------------------------------------------------------------

def test_validity_closes_the_old_fact_hides_it_from_the_profile_and_labels_it_in_search(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(records.VALIDITY_ENV, "1")
    cfg = cfg_at(tmp_path)
    put(cfg, "home.md", "---\nid: home\ntype: place\naliases: [apartment, rent]\n---\n"
                        "- Rent is $1,050 a month. {said: 2026-09-01; valid_from: 2026-09-01}\n")
    old = fact("Rent is $1,050 a month.", said="2026-09-01", valid_from="2026-09-01", valid_to=DAY)
    new = fact("Rent is $1,180 a month.", valid_from="2026-10-01")
    life = ["Rent is $1,050 a month. {said: 2026-09-01; valid_to: 2026-09-20}", "Rent is $1,180 a month."]
    change = reconcile_day(cfg, FakeClient(day_result([{"id": "home", "type": "place",
                                                        "aliases": ["apartment", "rent"],
                                                        "facts": [old, new]}], life)), DAY, DAY_TEXT)
    assert change is not None and change.closed == 1
    rec = load_records(cfg)["home"]
    assert rec.facts[0] == "Rent is $1,050 a month. {said: 2026-09-01; valid_from: 2026-09-01; valid_to: 2026-09-20}"
    page = profile(cfg)
    assert "$1,180" in page and "$1,050" not in page and "{" not in page
    hits = search(load_records(cfg), "rent")
    assert hits[0]["line"].startswith("Rent is $1,180")                 # on a tie, the open fact first
    assert hits[1]["line"] == "(until 2026-09-20) Rent is $1,050 a month. (said 2026-09-01)"


def test_the_guard_writes_a_busy_night_with_the_closures_over_the_limit_held_open(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(records.VALIDITY_ENV, "1")
    n = records.MAX_CLOSES_PER_NIGHT
    cfg = cfg_at(tmp_path)
    facts_before = [f"Fact number {i}. {{said: 2026-09-01}}" for i in range(n + 2)]
    put(cfg, "many.md", render_record(Record("many", "preference", ["many"], facts_before)))

    def closing(k: int) -> dict[str, Any]:
        out = [fact(f"Fact number {i}.", said="2026-09-01", valid_from="", valid_to=DAY if i < k else "")
               for i in range(n + 2)]
        return day_result([{"id": "many", "type": "preference", "aliases": ["many"], "facts": out}])

    change = reconcile_day(cfg, FakeClient(closing(n + 2)), DAY, DAY_TEXT)      # two over: written, not refused
    assert change is not None and change.closed == n                          # so the night is never retried
    facts = [parse_fact(f) for f in load_records(cfg)["many"].facts]
    assert [bool(f.valid_to) for f in facts] == [True] * n + [False, False]    # the first n close, the rest stay open
    journal = (records.records_dir(cfg) / "days" / f"{DAY}.md").read_text()
    assert f"No longer true from {DAY}: Fact number {n}." in journal            # held, but written down, dated
    assert f"No longer true from {DAY}: Fact number {n + 1}." in journal
    assert "Fact number 0." not in journal                                     # control: a real closure is no correction


def test_a_valid_to_set_ahead_counts_against_the_night(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(records.VALIDITY_ENV, "1")
    n = records.MAX_CLOSES_PER_NIGHT
    cfg = cfg_at(tmp_path)
    put(cfg, "many.md", render_record(Record("many", "preference", ["many"],
                                             [f"Fact number {i}. {{said: 2026-09-01}}" for i in range(n + 1)])))
    later = "2026-12-31"                                                       # after the night's day
    out = [fact(f"Fact number {i}.", said="2026-09-01", valid_from="", valid_to=later) for i in range(n + 1)]
    change = reconcile_day(cfg, FakeClient(day_result([{"id": "many", "type": "preference", "aliases": ["many"],
                                                        "facts": out}])), DAY, DAY_TEXT)
    assert change is not None and change.closed == n
    assert sum(1 for f in load_records(cfg)["many"].facts if parse_fact(f).valid_to) == n


def test_reconcile_and_consolidate_of_one_night_share_the_closure_budget(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(records.VALIDITY_ENV, "1")
    n = records.MAX_CLOSES_PER_NIGHT
    cfg = cfg_at(tmp_path)
    put(cfg, "a.md", render_record(Record("a", "preference", ["a"], [f"A{i}. {{said: 2026-09-01}}" for i in range(n)])))
    put(cfg, "b.md", render_record(Record("b", "preference", ["b"], ["B0. {said: 2026-09-01}", "B1."])))
    close_a = day_result([{"id": "a", "type": "preference", "aliases": ["a"],
                           "facts": [fact(f"A{i}.", said="2026-09-01", valid_from="", valid_to=DAY)
                                     for i in range(n)]}])
    close_b = {"records": [{"id": "b", "type": "preference", "aliases": ["b"],
                            "facts": [fact("B0.", said="2026-09-01", valid_from="", valid_to=DAY), "B1."]}],
               "merged": []}
    before_b = (records.records_dir(cfg) / "b.md").read_text()
    with forgets_pinned(cfg):
        assert reconcile_day(cfg, FakeClient(close_a), DAY, DAY_TEXT).closed == n
        assert consolidate(cfg, FakeClient(close_b)) == (0, 0)                        # the night's budget is spent
    assert (records.records_dir(cfg) / "b.md").read_text() == before_b
    assert consolidate(cfg, FakeClient(close_b)) == (1, 0)                            # a new night: allowed


def test_with_validity_off_a_valid_to_still_loads_and_the_profile_shows_open_lines_only(tmp_path: Path) -> None:
    cfg = cfg_at(tmp_path)
    put(cfg, "profile.md", "---\nid: profile\nupdated: 2026-09-20\n---\n# Page\n"
                           "- Old job. {valid_to: 2026-01-01}\n- New job. {valid_from: 2026-01-01}\n- Plain line.\n")
    page = profile(cfg)
    assert "Old job" not in page and "- New job." in page and "- Plain line." in page


# ---- sources -----------------------------------------------------------------------------------------------

def test_sources_keep_a_src_that_points_at_a_real_line_and_drop_one_that_does_not(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(records.SOURCES_ENV, "1")
    cfg = cfg_at(tmp_path)
    out = day_result([{"id": "home", "type": "place", "aliases": ["rent"],
                       "facts": [{"text": "Rent goes to 1180 (said 2026-09-20).", "src": "2026-09-20 12:01"},
                                 {"text": "Landlord is kind (said 2026-09-20).", "src": "2026-09-20 23:59"},
                                 {"text": "Pays by check (said 2026-09-20).", "src": "noon"}]}])
    assert reconcile_day(cfg, FakeClient(out), DAY, DAY_TEXT) is not None
    facts = load_records(cfg)["home"].facts
    assert facts == ["Rent goes to 1180 (said 2026-09-20). {src: 2026-09-20 12:01}",
                     "Landlord is kind (said 2026-09-20).", "Pays by check (said 2026-09-20)."]
    hits = search(load_records(cfg), "rent 1180")
    assert hits[0]["src"] == "2026-09-20 12:01"
    assert hits[0]["line"] == "Rent goes to 1180 (said 2026-09-20). (source 2026-09-20 12:01)"
    # a src a record already carries survives a later night that does not repeat the line
    later = day_result([{"id": "home", "type": "place", "aliases": ["rent"],
                         "facts": [{"text": "Rent goes to 1180 (said 2026-09-20).", "src": "2026-09-20 12:01"},
                                   {"text": "Rent paid (said 2026-09-21).", "src": "2026-09-21 09:00"}]}])
    assert reconcile_day(cfg, FakeClient(later), "2026-09-21", "2026-09-21 09:00 texted Owner: paid rent") is not None
    assert all("{src:" in f for f in load_records(cfg)["home"].facts)


def _memory(tmp_path: Path) -> tuple[Memory, RecallConfig, Transcripts]:
    cfg = cfg_at(tmp_path)
    when = datetime(2026, 9, 20, 12, 1).astimezone()
    tx = Transcripts(TranscriptConfig(enabled=True, root=cfg.transcripts_dir), cfg.archive_dir, wall=lambda: when)
    tx.append("telegram", "t-1789930000000-abcd", "owner", "say", "my locker code is 3141", now=when)
    stamp = f"{when:%Y-%m-%d %H:%M}"
    put(cfg, "gym.md", "---\nid: gym\ntype: place\naliases: [gym]\n---\n"
                       f"- Keeps a combination at the gym. {{src: {stamp}}}\n- Goes on Mondays.\n")
    return Memory(cfg, tx, None), cfg, tx


def test_forget_follows_the_src_pointer_to_a_fact_in_other_words(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(records.SOURCES_ENV, "1")
    m, cfg, tx = _memory(tmp_path)
    prev = m.forget_preview("locker code 3141")
    assert prev["layers"]["transcripts"] == 1 and prev["layers"]["records"] == 1
    done = m.forget_apply(prev["token"])
    assert done["ok"] and done["layers"]["records"] == 1
    assert load_records(cfg)["gym"].facts == ["Goes on Mondays."]


def test_with_sources_off_forget_still_follows_a_src_already_written(tmp_path: Path) -> None:
    m, cfg, _ = _memory(tmp_path)                                     # a fact written while the switch was on
    prev = m.forget_preview("locker code 3141")
    assert prev["layers"]["records"] == 1
    m.forget_apply(prev["token"])
    assert load_records(cfg)["gym"].facts == ["Goes on Mondays."]     # control: the fact with no src stays


# ---- mem0: one unit per owner line, src in metadata ----------------------------------------------------------

class FakeMem0:
    def __init__(self) -> None:
        self.added: list[tuple[list[dict[str, str]], dict[str, Any]]] = []
        self.rows: list[dict[str, Any]] = []

    def add(self, messages: list[dict[str, str]], **kw: Any) -> dict[str, Any]:
        self.added.append((messages, kw))
        return {"results": []}

    def search(self, query: str, **kw: Any) -> dict[str, Any]:
        return {"results": self.rows}

    def get_all(self, **kw: Any) -> dict[str, Any]:
        return {"results": list(self.rows)}

    def delete(self, memory_id: str) -> None:
        self.rows = [r for r in self.rows if r["id"] != memory_id]


def _tx_day(tmp_path: Path) -> Transcripts:
    cfg = cfg_at(tmp_path)
    t0 = datetime(2026, 9, 20, 12, 0).astimezone()
    tx = Transcripts(TranscriptConfig(enabled=True, root=cfg.transcripts_dir), cfg.archive_dir, wall=lambda: t0)
    conv = "t-1789930000000-abcd"
    for mm, who, text in ((0, "owner", "I moved to Quarry Street"), (1, "eve", "Which unit?"),
                          (2, "owner", "Unit 5"), (3, "eve", "Got it.")):
        tx.append("telegram", conv, who, "say", text, now=t0.replace(minute=mm))
    return tx


def test_with_sources_on_mem0_gets_one_add_per_owner_line_with_its_src(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(records.SOURCES_ENV, "1")
    tx = _tx_day(tmp_path)
    fake = FakeMem0()
    idx = OwnerMemory(Mem0Config(enabled=True, home=tmp_path / "mem0"), factory=lambda _c: fake)
    assert idx.ingest_day(tx, DAY) == 1
    assert [m for m, _ in fake.added] == [
        [{"role": "user", "content": "I moved to Quarry Street"}],
        [{"role": "assistant", "content": "Which unit?"}, {"role": "user", "content": "Unit 5"}]]
    assert [kw["metadata"]["src"] for _, kw in fake.added] == ["2026-09-20 12:00", "2026-09-20 12:02"]
    # the second unit also holds eve's line before the owner's: it points at that line too
    assert "src_before" not in fake.added[0][1]["metadata"]
    assert fake.added[1][1]["metadata"]["src_before"] == "2026-09-20 12:01"


def test_a_forget_of_eves_line_reaches_a_memory_made_from_its_unit(tmp_path: Path) -> None:
    fake = FakeMem0()
    fake.rows = [{"id": "m1", "memory": "Lives in unit 5", "score": 0.9,
                  "metadata": {"day": DAY, "src": "2026-09-20 12:02", "src_before": "2026-09-20 12:01"}},
                 {"id": "m2", "memory": "Likes tea", "score": 0.9, "metadata": {"day": DAY, "src": "2026-09-20 13:00"}}]
    idx = OwnerMemory(Mem0Config(enabled=True, home=tmp_path / "mem0"), factory=lambda _c: fake)
    assert idx.forget_matching(lambda t: "src: 2026-09-20 12:01" in t) == 1
    assert [r["id"] for r in fake.rows] == ["m2"]                       # control: the other memory stays


def test_with_sources_off_mem0_ingest_is_one_chunk_without_src(tmp_path: Path) -> None:
    tx = _tx_day(tmp_path)
    fake = FakeMem0()
    idx = OwnerMemory(Mem0Config(enabled=True, home=tmp_path / "mem0"), factory=lambda _c: fake)
    assert idx.ingest_day(tx, DAY) == 1
    assert len(fake.added) == 1 and "src" not in fake.added[0][1]["metadata"]


def test_mem0_search_shows_src_and_a_forget_reaches_a_memory_by_its_src(tmp_path: Path) -> None:
    fake = FakeMem0()
    fake.rows = [{"id": "m1", "memory": "Lives on Quarry Street", "score": 0.9,
                  "metadata": {"day": DAY, "channel": "telegram", "src": "2026-09-20 12:00"}},
                 {"id": "m2", "memory": "Likes tea", "score": 0.9, "metadata": {"day": DAY}}]
    idx = OwnerMemory(Mem0Config(enabled=True, home=tmp_path / "mem0"), factory=lambda _c: fake)
    rows = idx.search("where do I live")
    assert rows[0]["src"] == "2026-09-20 12:00" and "src" not in rows[1]
    assert Memory._recalled(rows[0]) == "(2026-09-20 12:00, telegram) Lives on Quarry Street"
    assert idx.forget_matching(lambda t: "src: 2026-09-20 12:00" in t) == 1
    assert [r["id"] for r in fake.rows] == ["m2"]
