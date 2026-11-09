"""Debrief (eve_bridge/debrief.py, docs/debrief.md): eve searches and reads the owner's debrief store."""

from __future__ import annotations

import asyncio
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
    assert names == app_mod.TOOL_NAMES == ("debrief_search", "debrief_get", "debrief_day", "debrief_status", "debrief_list",
                                             "debrief_when")
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
                              "debrief_status": "read", "debrief_list": "read", "debrief_when": "read"}
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


# ---- the table questions go to the store's script (2026-11-09) -------------------------------------------------

def test_table_questions_pass_their_words_to_the_script(store: Path) -> None:
    hub = app_mod.make(on(store))

    def call(name: str, args: dict) -> list[str]:
        return argv(asyncio.run(hub.handle(name, args)))[2:]

    assert call("debrief_day", {"date": " last  week ", "until": None}) == ["day", "--", "last week"]
    assert call("debrief_day", {"date": "2026-11-02", "until": "today"}) == ["day", "--until", "today", "--",
                                                                              "2026-11-02"]
    assert call("debrief_when", {"term": "-voice pe"}) == ["when", "--", "-voice pe"]
    assert call("debrief_status", {"name": "circles"}) == ["status", "--", "circles"]
    assert call("debrief_list", {"kind": "open", "topic": "voice pe"}) == ["todos", "-n", "12", "--", "voice", "pe"]
    assert call("debrief_list", {"kind": "open", "topic": None}) == ["todos", "-n", "12"]
    assert call("debrief_list", {"kind": "stars", "topic": None}) == ["stars"]
    for name, args in (("debrief_day", {"date": " ", "until": None}), ("debrief_when", {"term": "x"}),
                       ("debrief_list", {"kind": "todos", "topic": None})):
        assert asyncio.run(hub.handle(name, args))["ok"] is False


OLD = """import sys
print("usage: debrief-search ... invalid choice: 'day'", file=sys.stderr)
sys.exit(2)
"""
BAD = """import sys
print("date must be YYYY-MM-DD, today, yesterday")
sys.exit(2)
"""


@pytest.mark.parametrize(("script", "said"), [(OLD, "update claude-debrief"), (BAD, "date must be")])
def test_an_old_script_or_a_bad_word_is_a_reason(tmp_path: Path, script: str, said: str) -> None:
    (tmp_path / ".system").mkdir()
    (tmp_path / app_mod.SCRIPT).write_text(script)
    out = asyncio.run(app_mod.make(on(tmp_path)).handle("debrief_day", {"date": "x y", "until": None}))
    assert out["ok"] is False and said in out["reason"]


def test_a_long_table_answer_is_clipped_with_a_pointer(tmp_path: Path) -> None:
    (tmp_path / ".system").mkdir()
    (tmp_path / app_mod.SCRIPT).write_text(f"print('x' * {app_mod.MAX_OUT_CHARS + 500})")
    text = asyncio.run(app_mod.make(on(tmp_path)).handle("debrief_status", {"name": "eve"}))["text"]
    assert len(text) < app_mod.MAX_OUT_CHARS + 200 and "debrief_search finds a part" in text


@pytest.mark.skipif(not (app_mod.store_dir() / app_mod.SCRIPT).is_file(), reason="no debrief store on this Mac")
def test_the_real_script_answers_the_table_questions(tmp_path: Path) -> None:
    """The store's own script over a small made-up store: eve's calls are ones it understands."""
    (tmp_path / ".system").mkdir()
    (tmp_path / app_mod.SCRIPT).write_text((app_mod.store_dir() / app_mod.SCRIPT).read_text())
    if "def cmd_day" not in (tmp_path / app_mod.SCRIPT).read_text():
        pytest.skip("this Mac's debrief script predates the tables")
    import datetime as dt
    day = (dt.date.today() - dt.timedelta(days=1)).isoformat()
    (tmp_path / f"{day}-a.md").write_text(f"# {day}\n\n## What happened\n\nTuned the [[voice-pe]] ring.\n")
    (tmp_path / "INDEX.md").write_text("# Index\n\n## Glossary\n\n- `[[voice-pe]]` — the desk speaker, ring tuned.\n")
    (tmp_path / "TODO.md").write_text("# Todo\n\n## Open\n\n- [ ] `[[voice-pe]]`: hear the ring.\n")
    hub = app_mod.make(on(tmp_path))

    def ask(name: str, args: dict) -> dict:
        return asyncio.run(hub.handle(name, args))

    assert "Tuned the [[voice-pe]] ring" in ask("debrief_day", {"date": "yesterday", "until": None})["text"]
    assert day in ask("debrief_when", {"term": "voice pe"})["text"]
    assert "the desk speaker, ring tuned" in ask("debrief_status", {"name": "voice pe"})["text"]
    assert "hear the ring" in ask("debrief_list", {"kind": "open", "topic": "voice pe"})["text"]


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


def test_instructions_name_only_markers_the_script_prints() -> None:
    assert "~" in app_mod.INSTRUCTIONS and "⊘" in app_mod.INSTRUCTIONS
    assert "newest first" not in app_mod.TOOLS[0]["description"]
