"""Debrief (eve_bridge/debrief.py, docs/debrief.md): eve searches and reads the owner's debrief store."""

from __future__ import annotations

import asyncio
import datetime as dt
import json
import re
from pathlib import Path

import pytest

import eve_bridge.debrief as app_mod
from eve_bridge import app_registry, tool_gate

# A stand-in for the store's search script: it echoes its argv as JSON, so a test sees exactly what eve passed.
STUB = """import json, sys
args = sys.argv[1:]
if "fail" in args:
    sys.exit(3)
if "slow" in args:
    import time; time.sleep(5)
print(json.dumps(args))
print("progress", file=sys.stderr)
"""


@pytest.fixture()
def store(tmp_path: Path) -> Path:
    (tmp_path / ".system").mkdir()
    (tmp_path / app_mod.SCRIPT).write_text(STUB)
    return tmp_path


def on(store: Path) -> dict[str, str]:
    return {"EVE_DEBRIEF": "1", "EVE_DEBRIEF_DIR": str(store)}


def argv(result: dict) -> list[str]:
    assert result["ok"], result
    return json.loads(result["text"])


def test_off_by_default_on_with_its_switch_and_a_store(store: Path, tmp_path: Path) -> None:
    assert app_mod.make({}) is None
    assert app_mod.make({"EVE_DEBRIEF": "0", "EVE_DEBRIEF_DIR": str(store)}) is None
    assert app_mod.make(on(store)) is not None
    assert app_mod.make({"EVE_DEBRIEF": "1", "EVE_DEBRIEF_DIR": str(tmp_path / "none")}) is None   # no script


def test_the_store_is_found_from_its_variables() -> None:
    assert app_mod.store_dir({"EVE_DEBRIEF_DIR": "/a", "DEBRIEF_DIR": "/b"}) == Path("/a")
    assert app_mod.store_dir({"DEBRIEF_DIR": "/b"}) == Path("/b")
    assert app_mod.store_dir({}) == Path(app_mod.DEFAULT_DIR).expanduser()


def test_search_passes_the_query_after_a_double_dash(store: Path) -> None:
    hub = app_mod.make(on(store))
    got = json.loads(asyncio.run(hub.handle("debrief_search", {"query": "  -voice   pe ", "n": None}))["text"])
    assert got == ["--dir", str(store), "search", "-n", str(app_mod.DEFAULT_N), "--", "-voice pe"]
    got = json.loads(asyncio.run(hub.handle("debrief_search", {"query": "x", "n": 99}))["text"])
    assert got[got.index("-n") + 1] == str(app_mod.MAX_N)
    assert asyncio.run(hub.handle("debrief_search", {"query": "  ", "n": None}))["ok"] is False


def test_get_takes_only_ids_and_reads_each_on_its_own(store: Path) -> None:
    hub = app_mod.make(on(store))
    text = asyncio.run(hub.handle("debrief_get", {"ids": [757, 744, 757]}))["text"]
    assert [json.loads(x)[-2:] for x in text.split("\n\n")] == [["get", "757"], ["get", "744"]]
    assert len(asyncio.run(hub.handle("debrief_get", {"ids": list(range(20))}))["text"].split("\n\n")) == app_mod.MAX_IDS
    for bad in ([], ["1; rm"], [True], [-1], "757"):
        assert asyncio.run(hub.handle("debrief_get", {"ids": bad}))["ok"] is False


def test_a_failure_or_a_timeout_is_a_reason(store: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    hub = app_mod.make(on(store))
    assert asyncio.run(hub.handle("debrief_search", {"query": "fail", "n": 1}))["ok"] is False
    monkeypatch.setattr(app_mod, "MEANING_SECS", 0.3)
    monkeypatch.setattr(app_mod, "KEYWORD_SECS", 0.3)
    out = asyncio.run(hub.handle("debrief_search", {"query": "slow", "n": 1}))
    assert out["ok"] is False and "longer" in out["reason"]


def test_long_output_is_clipped() -> None:
    assert len(app_mod._clip("x" * (app_mod.MAX_OUT_CHARS + 50))) < app_mod.MAX_OUT_CHARS + 20


def test_an_unknown_tool_is_refused(store: Path) -> None:
    assert asyncio.run(app_mod.make(on(store)).handle("debrief_explode", {}))["ok"] is False


def test_its_tools_are_its_own_and_strict(store: Path) -> None:
    hub = app_mod.make(on(store))
    names = tuple(t["name"] for t in hub.tools())
    assert names == app_mod.TOOL_NAMES == ("debrief_search", "debrief_get", "debrief_day", "debrief_list", "debrief_when")
    for tool in hub.tools():
        params = tool["parameters"]
        assert params["additionalProperties"] is False and set(params["required"]) == set(params["properties"])
    named = set(re.findall(r"\bdebrief_[a-z_]+", hub.instructions()))
    assert named and named <= set(app_mod.TOOL_NAMES)


def test_it_is_registered_as_reads_that_taint() -> None:
    app = app_registry.get("debrief")
    assert app is not None and app_registry.check(app) == []
    assert app.switch == "EVE_DEBRIEF" and not app.default_on and not app.acting and not app.clean
    assert app.policies() == {"debrief_search": "read", "debrief_get": "read", "debrief_day": "read",
                              "debrief_list": "read", "debrief_when": "read"}
    for name in app_mod.TOOL_NAMES:      # a note can quote outside words: never clean, never asks
        assert name not in tool_gate.CLEAN and tool_gate.policy(name) == "read"


@pytest.mark.skipif(not (app_mod.store_dir() / app_mod.SCRIPT).is_file(), reason="no debrief store on this Mac")
def test_the_real_store_answers_a_search() -> None:
    hub = app_mod.Debrief(app_mod.store_dir())
    out = asyncio.run(hub.handle("debrief_search", {"query": "eve", "n": 2}))
    assert out["ok"] and re.search(r"^\[\d+\]", out["text"], re.M)


def test_the_phone_sheet_is_a_status_line(store: Path) -> None:
    (store / "2026-11-08-a.md").write_text("x")
    hub = app_mod.make(on(store))
    sheet = asyncio.run(hub.panel({"action": "status"}))
    assert app_registry.check_panel(sheet) == [] and sheet["status"] == "On" and "1 notes" in sheet["line"]
    assert asyncio.run(hub.panel({"action": "power", "on": True}))["ok"] is False


# ---- debrief_day: a day is read by date, never ranked (2026-11-09) ----------------------------------------------

TODAY = dt.date(2026, 11, 9)
INDEX = """# Index

## Debriefs (newest first)

| Date | Debrief | TL;DR | Key entities |
|---|---|---|---|
| 2026-11-08 | [Matcher day](2026-11-08-matcher.md) | Built the spoken-name matcher. Late addendum: the debrief app. | `[[eve]]` |
| 2026-11-06 | [Report day](2026-11-06-report.md) | Wrote the unit report. | `[[report]]` |
"""


@pytest.fixture()
def days(store: Path) -> app_mod.Debrief:
    (store / "INDEX.md").write_text(INDEX)
    (store / "2026-11-08-matcher.md").write_text("# 2026-11-08 matcher\nThe full day note.")
    (store / "2026-11-06-report.md").write_text("# 2026-11-06 report\nThe report note.")
    (store / "sessions" / "2026-11-07").mkdir(parents=True)
    (store / "sessions" / "2026-11-07" / "0900-abc.md").write_text("an unverified draft")
    return app_mod.make(on(store))


def test_yesterday_reads_that_days_summary_and_note(days: app_mod.Debrief) -> None:
    out = days.day("yesterday", None, today=TODAY)
    assert out["ok"] and "2026-11-08" in out["text"] and "Today is 2026-11-09" in out["text"]
    text = out["text"]
    assert text.index("spoken-name matcher") < text.index("The full day note.")   # the summary leads
    assert "[[eve]]" not in text.split("Summary:")[1].split("\n")[0]                # entities cell left out
    assert days.day("2026-11-08", None, today=TODAY)["text"] == text


def test_latest_is_the_newest_day_with_a_note(days: app_mod.Debrief) -> None:
    assert "Debrief for 2026-11-08" in days.day("latest", None, today=TODAY)["text"]


def test_a_day_without_notes_says_so_and_names_the_nearest(days: app_mod.Debrief) -> None:
    out = days.day("2026-11-07", None, today=TODAY)     # only a sessions/ draft that day: never read
    assert out["ok"] and "No debrief notes for 2026-11-07" in out["text"]
    assert "2026-11-06, 2026-11-08" in out["text"] and "draft" not in out["text"]


def test_a_range_answers_summaries_newest_first(days: app_mod.Debrief) -> None:
    out = days.day("2026-11-02", "today", today=TODAY)
    text = out["text"]
    assert out["ok"] and "2 days with notes" in text
    assert text.index("2026-11-08 (Sunday)") < text.index("2026-11-06 (Friday)")
    assert "The full day note." not in text                 # a range is rows only
    assert days.day("today", "2026-11-02", today=TODAY)["text"] == text      # reversed ends are swapped


def test_bad_dates_and_long_ranges_are_reasons(days: app_mod.Debrief) -> None:
    for bad in ("2026-11-31", "next tuesday", "../INDEX", "", None, 20261108):
        assert days.day(bad, None, today=TODAY)["ok"] is False
    assert days.day("2026-11-08", "next week", today=TODAY)["ok"] is False
    assert days.day("2026-08-01", "2026-11-09", today=TODAY)["ok"] is False


def test_a_symlinked_note_is_never_read(store: Path, tmp_path: Path) -> None:
    outside = tmp_path.parent / f"{tmp_path.name}-secret.md"
    outside.write_text("SECRET")
    (store / "2026-11-05-link.md").symlink_to(outside)
    out = app_mod.make(on(store)).day("2026-11-05", None, today=TODAY)
    assert "SECRET" not in out["text"]


def test_day_goes_through_handle(days: app_mod.Debrief) -> None:
    out = asyncio.run(days.handle("debrief_day", {"date": "2026-11-06", "until": None}))
    assert out["ok"] and "The report note." in out["text"]


@pytest.mark.parametrize(("word", "first", "last"), [
    ("today", "2026-11-09", "2026-11-09"),
    ("Yesterday", "2026-11-08", "2026-11-08"),
    ("day before yesterday", "2026-11-07", "2026-11-07"),
    ("thursday", "2026-11-05", "2026-11-05"),
    ("last Monday", "2026-11-02", "2026-11-02"),        # a weekday is never today
    ("on sunday", "2026-11-08", "2026-11-08"),
    ("this week", "2026-11-09", "2026-11-09"),
    ("last week", "2026-11-02", "2026-11-08"),
    ("weekend", "2026-11-07", "2026-11-08"),
    ("last month", "2026-10-01", "2026-10-31"),
    ("this month", "2026-11-01", "2026-11-09"),
    ("2026-11-03", "2026-11-03", "2026-11-03"),
])
def test_time_words_become_dates_here(word: str, first: str, last: str) -> None:
    got = app_mod.resolve_span(word, TODAY)
    assert got is not None and (got[0].isoformat(), got[1].isoformat()) == (first, last)


def test_a_range_names_its_empty_days_and_skips_template_rows(days: app_mod.Debrief, store: Path) -> None:
    (store / "INDEX.md").write_text(INDEX + "| 2026-11-04 | [Example row](2026-11-04-x.md) | A template. | x |\n")
    text = days.day("last week", None, today=TODAY)["text"]
    assert "No notes (no coding sessions): Mon Nov 2, Tue Nov 3, Wed Nov 4, Thu Nov 5, Sat Nov 7." in text
    assert "A template." not in text


def test_a_long_day_keeps_an_outline_of_every_part(store: Path) -> None:
    body = "# day\n## TL;DR\nx\n" + "y" * (app_mod.MAX_OUT_CHARS + 100) + "\n## Late-day addendum — evening work\nz"
    (store / "2026-11-08-long.md").write_text(body)
    text = app_mod.make(on(store)).day("yesterday", None, today=TODAY)["text"]
    assert "Its parts, in order: TL;DR; Late-day addendum — evening work." in text
    assert "not shown" in text


def test_when_counts_mentions_per_day_newest_first(days: app_mod.Debrief, store: Path) -> None:
    (store / "2026-11-06-report.md").write_text("The Voice PE, the voice-pe and VoicePE.")
    (store / "2026-11-08-matcher.md").write_text("voice pe again")
    text = days.when("Voice PE", today=TODAY)["text"]
    assert "on 2 days" in text and text.index("2026-11-08 (Sun): 1") < text.index("2026-11-06 (Fri): 3")
    assert "First mentioned 2026-11-06" in text
    assert "No debrief note mentions" in days.when("kubernetes", today=TODAY)["text"]
    assert days.when(" ", today=TODAY)["ok"] is False
    assert asyncio.run(days.handle("debrief_when", {"term": "voice pe"}))["ok"]


# ---- the search and get as the model meets them (the swarm's findings, 2026-11-09) ------------------------------

# A store script that answers like the real one: search lines, get headers. Its behaviour is set by files in the
# store: ``hang`` (the meaning search never answers), ``semoff`` (Ollama refused), ``moved`` (ids reindexed).
FAKE = r"""import sys, os, time
args = sys.argv[1:]
root = args[args.index("--dir") + 1]
cmd = args[args.index("--dir") + 2]
flag = lambda n: os.path.exists(os.path.join(root, n))
if cmd == "search":
    if flag("hang") and "--keyword" not in args:
        time.sleep(30)
    if flag("semoff"):
        print("(semantic off: refused; keyword only)", file=sys.stderr)
    print("[7] 2026-11-08 (1d) 2026-11-08-a.md §TL;DR — ## TL;DR the day…")
    print("[9]~ index INDEX.md §Systems / repos — …")
    print("(2 hits)" + (" keyword" if "--keyword" in args else ""))
elif cmd == "get":
    i = args[-1]
    sec = "Open threads" if (i == "7" and flag("moved")) else {"7": "TL;DR", "9": "Systems / repos"}.get(i, "X")
    name = "INDEX.md" if i == "9" else "2026-11-08-a.md"
    body = "\n".join(f"- `[[thing{n}]]` filler words {'kokoro now opt-in' if n == 150 else ''} " + "z" * 60
                     for n in range(300)) if i == "9" else "the TL;DR text"
    print(f"[{i}] {name} §{sec}\n\n{body}")
"""


@pytest.fixture()
def real(tmp_path: Path) -> tuple[Path, app_mod.Debrief]:
    (tmp_path / ".system").mkdir()
    (tmp_path / app_mod.SCRIPT).write_text(FAKE)
    return tmp_path, app_mod.make(on(tmp_path))


def test_a_long_hit_is_cut_to_the_entries_that_name_the_search(real: tuple[Path, app_mod.Debrief]) -> None:
    _, hub = real
    asyncio.run(hub.handle("debrief_search", {"query": "kokoro decision", "n": 6}))
    text = asyncio.run(hub.handle("debrief_get", {"ids": [9, 7]}))["text"]
    assert "kokoro now opt-in" in text and "the TL;DR text" in text    # each id keeps its share
    assert "thing3]]" not in text


def test_an_id_that_moved_since_the_search_is_refused(real: tuple[Path, app_mod.Debrief]) -> None:
    root, hub = real
    asyncio.run(hub.handle("debrief_search", {"query": "day", "n": 6}))
    (root / "moved").write_text("")
    text = asyncio.run(hub.handle("debrief_get", {"ids": [7, 9]}))["text"]
    assert "[7] changed since the search" in text and "§Systems / repos" in text


def test_a_hung_meaning_search_falls_back_to_words(real: tuple[Path, app_mod.Debrief],
                                                   monkeypatch: pytest.MonkeyPatch) -> None:
    root, hub = real
    (root / "hang").write_text("")
    monkeypatch.setattr(app_mod, "MEANING_SECS", 0.5)
    out = asyncio.run(hub.handle("debrief_search", {"query": "day", "n": 6}))
    assert out["ok"] and "keyword" in out["text"] and "did not answer in time" in out["text"]


def test_a_refused_meaning_search_is_said(real: tuple[Path, app_mod.Debrief]) -> None:
    root, hub = real
    (root / "semoff").write_text("")
    assert "meaning search is off" in asyncio.run(hub.handle("debrief_search", {"query": "day", "n": 6}))["text"]


TODO = """# Todo

## The rules

- [ ] a rule example that is not open work

## Open

- [ ] `[[voice-pe]]`: hear the soft limiter on the board.
  — opened 2026-11-07
- [ ] `[[circles]]`: check the map migration.
  — opened 2026-10-29

## Closed (recent)

- [x] done thing
"""
STARS = """# Highlights

### ★ A real lesson about voice

Body one.

### ★ Verify the mechanism

*(example)* A template entry.
"""


def test_open_threads_and_stars_are_lists(store: Path) -> None:
    (store / "TODO.md").write_text(TODO)
    (store / "HIGHLIGHTS.md").write_text(STARS)
    hub = app_mod.make(on(store))
    every = hub.read_list("open", None)["text"]
    assert "2 open threads (of 2); all of them:" in every and "rule example" not in every and "done" not in every
    one = hub.read_list("open", "voice pe")["text"]
    assert "soft limiter" in one and "map migration" not in one
    assert "No open threads match" in hub.read_list("open", "kubernetes")["text"]
    stars = hub.read_list("stars", None)["text"]
    assert "A real lesson" in stars and "template" not in stars
    assert hub.read_list("todos", None)["ok"] is False
    assert asyncio.run(hub.handle("debrief_list", {"kind": "open", "topic": None}))["ok"]


def test_instructions_name_only_markers_the_script_prints() -> None:
    assert "~" in app_mod.INSTRUCTIONS and "⊘" in app_mod.INSTRUCTIONS
    assert "newest first" not in app_mod.TOOLS[0]["description"]
