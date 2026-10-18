"""second_brain.py: the owner's markdown vault — capture, search, filing, context packs, workflow prompts."""

from __future__ import annotations

import json
import threading
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
from pathlib import Path

import pytest

from eve_bridge import second_brain as sb
from eve_bridge.second_brain import (
    PARA,
    SECOND_BRAIN_DEFAULT,
    SECOND_BRAIN_TOOL_NAMES,
    SECOND_BRAIN_TOOLS,
    WORKFLOWS,
    Include,
    PackDef,
    active_projects,
    capture,
    classify_capture,
    compile_pack,
    configured,
    dispatch,
    ensure_vault,
    file_note,
    inbox,
    load_pack,
    parse_pack,
    read_note,
    search,
    slugify,
    todos,
    workflow_prompt,
)

NOW = datetime(2026, 9, 21, 18, 30)

DECK_PACK = """# a pack in the deck's shape
name: daily-plan
description: What today needs
budget_tokens: 5000
include:
  - 00-vision/README.md
  - 02-todos/master.md
  - path: 08-journals/daily/*.md
    select: modified_desc
    count: 3
  - path: "03-projects/**/*.md"
    select: filename_desc
    count: 5
exclude:
  - 09-archive/**
  - "**/drafts/**"
rules:
  prefer_files:
    - 02-todos/master.md
  max_per_glob: 7
"""


@pytest.fixture
def vault(tmp_path: Path) -> Path:
    root = tmp_path / "vault"
    ensure_vault(root)
    return root


# ---- skeleton ------------------------------------------------------------------------------------

def test_ensure_vault_creates_skeleton_and_is_idempotent(tmp_path: Path) -> None:
    root = tmp_path / "vault"
    created = ensure_vault(root)
    for name, _ in PARA:
        assert (root / name).is_dir()
    assert "CLAUDE.md" in created
    assert "02-todos/master.md" in created
    assert "00-vision/README.md" in created
    assert ".obsidian/app.json" in created
    for wf in WORKFLOWS:
        assert f"06-processes/{wf}.md" in created
        assert f"06-processes/packs/{wf}.pack" in created
    guide = (root / "CLAUDE.md").read_text()
    for name, _ in PARA:
        assert f"`{name}/`" in guide
    master = root / "02-todos" / "master.md"
    assert all(f"## {p}" in master.read_text() for p in ("P0", "P1", "P2", "P3"))
    vision = (root / "00-vision" / "README.md").read_text()
    for heading in ("Mission", "Identity", "Principles", "10-year vision", "This year", "This quarter"):
        assert f"## {heading}" in vision
    assert (root / "08-journals" / "daily").is_dir()
    # idempotent: an edited file survives and nothing is reported created
    master.write_text(master.read_text() + "- [ ] keep me (added 2026-09-21)\n")
    assert ensure_vault(root) == []
    assert "keep me" in master.read_text()


def test_ensure_vault_inside_existing_obsidian_vault_adds_no_second_obsidian(tmp_path: Path) -> None:
    (tmp_path / ".obsidian").mkdir()
    root = tmp_path / "Second Brain"
    created = ensure_vault(root)
    assert ".obsidian/app.json" not in created
    assert not (root / ".obsidian").exists()


def test_default_is_off_and_configured_reads_env(tmp_path: Path) -> None:
    assert SECOND_BRAIN_DEFAULT is False
    assert configured({}).enabled is False
    assert configured({}).root == sb.DEFAULT_VAULT
    cfg = configured({"EVE_SECOND_BRAIN": "1", "EVE_VAULT": str(tmp_path / "v")})
    assert cfg.enabled is True and cfg.root == tmp_path / "v"
    assert configured({"EVE_SECOND_BRAIN": "on"}).enabled is True
    assert configured({"EVE_SECOND_BRAIN": "0"}).enabled is False


# ---- classify and capture ------------------------------------------------------------------------

@pytest.mark.parametrize("text, kind, cleaned", [
    ("todo: buy milk", "todo", "buy milk"),
    ("Task: call the dentist", "todo", "call the dentist"),
    ("- [ ] fix the bike", "todo", "fix the bike"),
    ("P0 renew passport", "todo", "P0 renew passport"),
    ("p3 someday learn the cello", "todo", "p3 someday learn the cello"),
    ("journal: felt sharp this morning", "journal", "felt sharp this morning"),
    ("Dear diary, long day", "journal", "long day"),
    ("Today I finally shipped the relay", "journal", "Today I finally shipped the relay"),
    ("idea: a robot that waters plants", "idea", "a robot that waters plants"),
    ("note: the pasta place is on Castro", "note", "the pasta place is on Castro"),
    ("the pasta place is on Castro", "note", "the pasta place is on Castro"),
])
def test_classify_capture_rules(text: str, kind: str, cleaned: str) -> None:
    assert classify_capture(text) == (kind, cleaned)


def test_slugify() -> None:
    assert slugify("The Pasta Place on Castro Street is great, truly") == "the-pasta-place-on-castro-street"
    assert slugify("Café — naïve résumé!") == "cafe-naive-resume"
    assert len(slugify("a" * 100)) <= 48
    assert slugify("???") == "note"


def test_capture_note_writes_inbox_file_with_frontmatter(vault: Path) -> None:
    res = capture(vault, "The pasta place on Castro is called Doppio", now=NOW)
    assert res.kind == "note"
    assert res.path == "01-inbox/2026-09-21-1830-the-pasta-place-on-castro-is.md"
    text = (vault / res.path).read_text()
    assert text.startswith("---\ncreated: 2026-09-21T18:30\nsource: telegram\nkind: note\nstatus: inbox\n---\n")
    assert text.rstrip().endswith("The pasta place on Castro is called Doppio")
    assert res.title.startswith("The pasta place")
    # a second capture with the same slug at the same minute does not overwrite
    res2 = capture(vault, "The pasta place on Castro is called Doppio", now=NOW)
    assert res2.path == "01-inbox/2026-09-21-1830-the-pasta-place-on-castro-is-2.md"
    assert (vault / res.path).exists() and (vault / res2.path).exists()


def test_capture_idea_is_an_inbox_note_of_kind_idea(vault: Path) -> None:
    res = capture(vault, "a robot that waters plants", kind="idea", now=NOW)
    assert res.path.startswith("01-inbox/") and "kind: idea" in (vault / res.path).read_text()


def test_capture_todo_priorities(vault: Path) -> None:
    capture(vault, "buy milk", kind="todo", now=NOW)
    capture(vault, "p0 renew passport", kind="todo", now=NOW)
    capture(vault, "P1 book the dentist", kind="todo", now=NOW)
    capture(vault, "p3 learn the cello", kind="todo", now=NOW)
    capture(vault, "walk more", kind="todo", now=NOW)
    text = (vault / "02-todos" / "master.md").read_text()
    assert "- [ ] renew passport (added 2026-09-21)" in text
    assert "p0 renew" not in text.lower()
    open_items = todos(vault)
    assert open_items == {"P0": ["renew passport (added 2026-09-21)"],
                          "P1": ["book the dentist (added 2026-09-21)"],
                          "P2": ["buy milk (added 2026-09-21)", "walk more (added 2026-09-21)"],
                          "P3": ["learn the cello (added 2026-09-21)"]}
    # sections stay separated by a blank line so the file stays readable
    assert "\n\n## P1" in text and "\n\n## P2" in text


def test_capture_journal_appends_to_todays_page(vault: Path) -> None:
    res = capture(vault, "felt sharp this morning", kind="journal", now=NOW)
    assert res.path == "08-journals/daily/2026-09-21.md"
    capture(vault, "then the afternoon dragged", kind="journal", now=datetime(2026, 9, 21, 16, 5))
    text = (vault / res.path).read_text()
    assert text == "# 2026-09-21\n\n- 18:30 felt sharp this morning\n- 16:05 then the afternoon dragged\n"


def test_capture_rejects_empty_and_unknown_kind(vault: Path) -> None:
    with pytest.raises(ValueError):
        capture(vault, "   ")
    with pytest.raises(ValueError):
        capture(vault, "x", kind="poem")


# ---- reading -------------------------------------------------------------------------------------

def test_search_scores_filename_over_body_and_skips_archive(vault: Path) -> None:
    (vault / "05-resources" / "pasta.md").write_text("# Pasta\nA list of places.\n")
    (vault / "05-resources" / "dinner.md").write_text("# Dinner\nWe had pasta at Doppio, pasta was great.\n")
    (vault / "09-archive" / "05-resources").mkdir(parents=True)
    (vault / "09-archive" / "05-resources" / "old-pasta.md").write_text("pasta pasta pasta pasta\n")
    hits = search(vault, "pasta")
    assert [h.path for h in hits][:2] == ["05-resources/pasta.md", "05-resources/dinner.md"]
    assert hits[0].score == 3 + 1 and hits[1].score == 1
    assert hits[1].snippet == "We had pasta at Doppio, pasta was great."
    assert all(not h.path.startswith("09-archive") for h in hits)
    archived = search(vault, "pasta", include_archive=True)
    assert "09-archive/05-resources/old-pasta.md" in [h.path for h in archived]
    assert search(vault, "") == []
    assert len(search(vault, "pasta", limit=1)) == 1


def test_search_snippet_is_clipped(vault: Path) -> None:
    (vault / "05-resources" / "long.md").write_text("word " * 200 + "\n")
    hit = search(vault, "word")[0]
    assert len(hit.snippet) <= 200


def test_read_note_refuses_escapes_and_obsidian(vault: Path) -> None:
    (vault.parent / "secret.md").write_text("not yours\n")
    assert read_note(vault, "../secret.md")["ok"] is False
    assert read_note(vault, ".obsidian/app.json")["ok"] is False
    assert read_note(vault, "/etc/hosts")["ok"] is False
    assert read_note(vault, "")["ok"] is False
    assert read_note(vault, "05-resources/missing.md") == {"ok": False, "reason": "no such note"}
    res = read_note(vault, "02-todos/master.md")
    assert res["ok"] is True and res["path"] == "02-todos/master.md" and "## P0" in res["text"]
    (vault / "05-resources" / "big.md").write_text("x" * 100)
    clipped = read_note(vault, "05-resources/big.md", max_chars=10)
    assert clipped["clipped"] is True and len(clipped["text"]) == 10 and clipped["chars"] == 100


def test_inbox_oldest_first(vault: Path) -> None:
    capture(vault, "second", now=datetime(2026, 9, 21, 12, 0))
    capture(vault, "first", now=datetime(2026, 9, 20, 9, 0))
    items = inbox(vault)
    assert [i.title for i in items] == ["first", "second"]
    assert items[0].created == "2026-09-20T09:00"


def test_active_projects_excludes_done_and_archived(vault: Path) -> None:
    p = vault / "03-projects"
    (p / "eve").mkdir()
    (p / "eve" / "README.md").write_text("---\nstatus: active\n---\n# eve\n")
    (p / "garden").mkdir()
    (p / "garden" / "README.md").write_text("---\nstatus: done\n---\n")
    (p / "thesis.md").write_text("---\nstatus: archived\n---\n")
    (p / "move-house.md").write_text("# Move house\n")
    (p / "notes.txt").write_text("not a note")
    assert active_projects(vault) == ["eve", "move-house"]


# ---- filing --------------------------------------------------------------------------------------

def test_file_note_moves_updates_status_and_handles_clashes(vault: Path) -> None:
    res = capture(vault, "a thought about eve", now=NOW)
    new = file_note(vault, res.path, "03-projects/eve")
    assert new == "03-projects/eve/2026-09-21-1830-a-thought-about-eve.md"
    assert not (vault / res.path).exists()
    text = (vault / new).read_text()
    assert "status: filed" in text and "status: inbox" not in text
    assert text.rstrip().endswith("a thought about eve")
    # clash: same name already there → -2
    again = capture(vault, "a thought about eve", now=NOW)
    new2 = file_note(vault, again.path, "03-projects/eve")
    assert new2 == "03-projects/eve/2026-09-21-1830-a-thought-about-eve-2.md"


def test_a_symlinked_project_folder_is_not_a_way_out_of_the_vault(vault: Path, tmp_path: Path) -> None:
    # _destination did not resolve 03-projects/<x>: through a symlink, create/move/file wrote outside the vault,
    # then _rel raised and the caller heard "failed" after the file had already gone
    outside = tmp_path / "outside"
    outside.mkdir()
    (vault / sb.PROJECTS_DIR / "away").symlink_to(outside, target_is_directory=True)
    with pytest.raises(ValueError):
        sb.create_note(vault, f"{sb.PROJECTS_DIR}/away", "Escape")
    note = vault / sb.INBOX_DIR / "n.md"
    note.write_text("# N\n", encoding="utf-8")
    with pytest.raises(ValueError):
        sb.move_note(vault, f"{sb.INBOX_DIR}/n.md", f"{sb.PROJECTS_DIR}/away")
    with pytest.raises(ValueError):
        sb.file_note(vault, f"{sb.INBOX_DIR}/n.md", f"{sb.PROJECTS_DIR}/away")
    assert list(outside.iterdir()) == [] and note.is_file()
    made = sb.create_note(vault, f"{sb.PROJECTS_DIR}/home", "Stays")       # positive control: a real folder
    assert made["path"] == f"{sb.PROJECTS_DIR}/home/stays.md"


def test_file_note_rejects_unknown_folder_and_escapes(vault: Path) -> None:
    res = capture(vault, "x", now=NOW)
    for bad in ("10-nope", "03-projects/../..", "05-resources/sub/deeper", "", "../elsewhere", "03-projects/.hidden"):
        with pytest.raises(ValueError):
            file_note(vault, res.path, bad)
    with pytest.raises(ValueError):
        file_note(vault, "../outside.md", "05-resources")
    with pytest.raises(ValueError):
        file_note(vault, "01-inbox/missing.md", "05-resources")
    assert (vault / res.path).exists()


# ---- context packs -------------------------------------------------------------------------------

def test_parse_pack_on_the_decks_example() -> None:
    pack = parse_pack(DECK_PACK)
    assert pack.name == "daily-plan"
    assert pack.description == "What today needs"
    assert pack.budget_tokens == 5000
    assert pack.include == [
        Include(path="00-vision/README.md"),
        Include(path="02-todos/master.md"),
        Include(path="08-journals/daily/*.md", select="modified_desc", count=3),
        Include(path="03-projects/**/*.md", select="filename_desc", count=5),
    ]
    assert pack.exclude == ["09-archive/**", "**/drafts/**"]
    assert pack.prefer_files == ["02-todos/master.md"]
    assert pack.max_per_glob == 7


def test_parse_pack_rejects_nonsense() -> None:
    with pytest.raises(ValueError):
        parse_pack("description: no name\n")
    with pytest.raises(ValueError):
        parse_pack("name: x\ninclude:\n  - path: a/*.md\n    select: random\n")


def test_builtin_packs_parse_and_load(vault: Path) -> None:
    for wf in WORKFLOWS:
        pack = load_pack(vault, wf)
        assert pack.name == wf and pack.include and pack.budget_tokens > 0
    with pytest.raises(ValueError):
        load_pack(vault, "nope")
    with pytest.raises(ValueError):
        load_pack(vault, "../CLAUDE")


def test_compile_pack_orders_selects_and_excludes(vault: Path) -> None:
    daily = vault / "08-journals" / "daily"
    for day in ("2026-09-18", "2026-09-19", "2026-09-20", "2026-09-21"):
        (daily / f"{day}.md").write_text(f"# {day}\n- 09:00 line\n")
    (vault / "03-projects" / "eve").mkdir()
    (vault / "03-projects" / "eve" / "README.md").write_text("# eve\n")
    (vault / "03-projects" / "eve" / "drafts").mkdir()
    (vault / "03-projects" / "eve" / "drafts" / "wip.md").write_text("# wip\n")
    (vault / "09-archive" / "03-projects").mkdir(parents=True)
    (vault / "09-archive" / "03-projects" / "gone.md").write_text("# gone\n")
    pack = parse_pack(DECK_PACK.replace("select: modified_desc", "select: filename_desc"))
    out = compile_pack(vault, pack, now=NOW)
    assert out.files[0] == "02-todos/master.md"          # preferred first
    assert out.files[1] == "00-vision/README.md"
    journals = [f for f in out.files if f.startswith("08-journals")]
    assert journals == ["08-journals/daily/2026-09-21.md", "08-journals/daily/2026-09-20.md",
                        "08-journals/daily/2026-09-19.md"]   # newest three by name
    assert "03-projects/eve/README.md" in out.files
    assert all("drafts" not in f and not f.startswith("09-archive") for f in out.files)
    assert out.dropped == []
    assert out.xml.startswith('<context pack="daily-plan" compiled="2026-09-21">\n<file path="02-todos/master.md">\n')
    assert out.xml.rstrip().endswith("</file>\n</context>")
    assert out.xml.count("<file ") == len(out.files)
    assert out.tokens == len(out.xml) // 4


def test_compile_pack_respects_budget_and_reports_drops(vault: Path) -> None:
    (vault / "05-resources" / "a-big.md").write_text("A" * 4000 + "\n")
    (vault / "05-resources" / "b-big.md").write_text("B" * 4000 + "\n")
    (vault / "05-resources" / "c-small.md").write_text("small\n")
    pack = PackDef(name="t", description="", budget_tokens=1200,
                   include=[Include(path="05-resources/*.md", select="filename_desc")],
                   exclude=[], prefer_files=["05-resources/a-big.md"], max_per_glob=10)
    out = compile_pack(vault, pack, now=NOW)
    # filename_desc gives c, b, a; preference moves a to the front; the budget (~1200 tokens = 4800 chars)
    # fits one big file, so the tail — b — is dropped and c (small) survives ahead of it.
    assert out.files == ["05-resources/a-big.md", "05-resources/c-small.md"]
    assert out.dropped == ["05-resources/b-big.md"]
    assert out.tokens <= 1200
    assert "BBBB" not in out.xml and "small" in out.xml
    tiny = compile_pack(vault, PackDef(name="t", description="", budget_tokens=10, include=pack.include,
                                       exclude=[], prefer_files=[], max_per_glob=10))
    assert tiny.files == [] and len(tiny.dropped) == 3 and tiny.xml.endswith("</context>")


def test_compile_pack_modified_desc_and_count_and_max_per_glob(vault: Path) -> None:
    folder = vault / "05-resources"
    import os
    import time
    base = time.time() - 1000
    for i, name in enumerate(("old", "mid", "new")):
        p = folder / f"{name}.md"
        p.write_text(f"# {name}\n")
        os.utime(p, (base + i * 100, base + i * 100))
    pack = PackDef(name="t", description="", budget_tokens=9999,
                   include=[Include(path="05-resources/*.md", select="modified_desc", count=2)],
                   exclude=[], prefer_files=[], max_per_glob=10)
    assert compile_pack(vault, pack).files == ["05-resources/new.md", "05-resources/mid.md"]
    capped = PackDef(name="t", description="", budget_tokens=9999,
                     include=[Include(path="05-resources/*.md", select="modified_desc", count=5)],
                     exclude=[], prefer_files=[], max_per_glob=1)
    assert compile_pack(vault, capped).files == ["05-resources/new.md"]


# ---- workflows -----------------------------------------------------------------------------------

def test_workflow_prompt_for_every_name_has_sop_and_context(vault: Path) -> None:
    capture(vault, "buy milk", kind="todo", now=NOW)
    capture(vault, "felt sharp", kind="journal", now=NOW)
    capture(vault, "an inbox thought", now=NOW)
    (vault / "03-projects" / "eve").mkdir()
    (vault / "03-projects" / "eve" / "README.md").write_text("# eve\nthe desk robot\n")
    for wf in WORKFLOWS:
        sop = (vault / "06-processes" / f"{wf}.md").read_text().rstrip()
        prompt = workflow_prompt(vault, wf, extra="", now=NOW)
        assert prompt.startswith(sop)
        assert "Today is Monday 2026-09-21." in prompt
        assert "Active projects: eve." in prompt
        assert f'<context pack="{wf}"' in prompt and "</context>" in prompt
        if wf != "distill-chat":
            assert prompt.rstrip().endswith("</context>")
    daily = workflow_prompt(vault, "daily-plan", now=NOW)
    assert '<file path="02-todos/master.md">' in daily and "buy milk" in daily
    assert '<file path="08-journals/daily/2026-09-21.md">' in daily and "felt sharp" in daily
    triage = workflow_prompt(vault, "triage-inbox", now=NOW)
    assert "an inbox thought" in triage and '<file path="03-projects/eve/README.md">' in triage
    distilled = workflow_prompt(vault, "distill-chat", extra="A: let's ship it\nB: agreed", now=NOW)
    assert distilled.rstrip().endswith("<transcript>\nA: let's ship it\nB: agreed\n</transcript>")
    empty = workflow_prompt(vault, "distill-chat", now=NOW)
    assert "no transcript was supplied" in empty
    with pytest.raises(ValueError):
        workflow_prompt(vault, "make-coffee")


# ---- the tools -----------------------------------------------------------------------------------

def test_every_tool_is_strict_named_and_dispatched(vault: Path) -> None:
    assert len(SECOND_BRAIN_TOOLS) == 9
    for tool in SECOND_BRAIN_TOOLS:
        assert tool["type"] == "function" and tool["strict"] is True
        params = tool["parameters"]
        assert params["type"] == "object" and params["additionalProperties"] is False
        assert sorted(params["required"]) == sorted(params["properties"])   # strict mode: every property required
        assert tool["name"] in SECOND_BRAIN_TOOL_NAMES
        res = dispatch(vault, tool["name"], {})
        assert isinstance(res, dict) and "ok" in res
        json.dumps(res)
    assert set(SECOND_BRAIN_TOOL_NAMES) == {"capture_note", "search_notes", "read_note", "list_inbox", "file_note",
                                            "list_todos", "second_brain_workflow", "edit_note", "undo_note"}


def test_dispatch_never_raises_and_returns_ok_false_for_bad_input(vault: Path) -> None:
    assert dispatch(vault, "capture_note", {"text": ""})["ok"] is False
    assert dispatch(vault, "capture_note", {"text": "x", "kind": "poem"})["ok"] is False
    assert dispatch(vault, "read_note", {"path": "../x.md"})["ok"] is False
    assert dispatch(vault, "read_note", {"path": ".obsidian/app.json"})["ok"] is False
    assert dispatch(vault, "file_note", {"path": "01-inbox/nope.md", "into": "05-resources"})["ok"] is False
    assert dispatch(vault, "second_brain_workflow", {"name": "nope", "extra": None})["ok"] is False
    assert dispatch(vault, "no_such_tool", {})["ok"] is False
    assert dispatch(vault, "search_notes", None)["ok"] is True  # type: ignore[arg-type]
    missing = vault / "not-there"
    assert dispatch(missing, "list_inbox", {}) == {"ok": True, "items": [], "count": 0}
    assert dispatch(missing, "list_todos", {})["todos"] == {"P0": [], "P1": [], "P2": [], "P3": []}
    assert not missing.exists()   # reading never creates a vault; only capture and the workflows do


def test_dispatch_round_trip(vault: Path) -> None:
    saved = dispatch(vault, "capture_note", {"text": "idea: a robot that waters plants", "kind": None})
    assert saved["ok"] and saved["kind"] == "idea" and saved["path"].startswith("01-inbox/")
    assert saved["line"] == f"Saved to {saved['path']}"
    todo = dispatch(vault, "capture_note", {"text": "todo: p1 water the plants", "kind": None})
    assert todo["ok"] and todo["kind"] == "todo" and "todo list" in todo["line"]
    assert dispatch(vault, "list_todos", {})["todos"]["P1"] == ["water the plants (added %s)" % datetime.now().strftime("%Y-%m-%d")]
    forced = dispatch(vault, "capture_note", {"text": "this reads like a note but is a journal line", "kind": "journal"})
    assert forced["kind"] == "journal" and forced["path"].startswith("08-journals/daily/")
    found = dispatch(vault, "search_notes", {"query": "waters plants"})
    assert found["ok"] and found["hits"][0]["path"] == saved["path"]
    note = dispatch(vault, "read_note", {"path": saved["path"]})
    assert note["ok"] and "waters plants" in note["text"]
    listed = dispatch(vault, "list_inbox", {})
    assert listed["count"] == 1 and listed["items"][0]["path"] == saved["path"]
    moved = dispatch(vault, "file_note", {"path": saved["path"], "into": "04-areas/garden"})
    assert moved["ok"] and moved["path"].startswith("04-areas/garden/")
    wf = dispatch(vault, "second_brain_workflow", {"name": "daily-plan", "extra": None})
    assert wf["ok"] and "calendar" in wf["note"]
    review = dispatch(vault, "second_brain_workflow", {"name": "weekly-review", "extra": None})
    assert review["ok"] and review["note"] == "hand this prompt to think_hard"
    assert "water the plants" in wf["prompt"] and wf["tokens"] == len(wf["prompt"]) // 4
    json.dumps([saved, todo, forced, found, note, listed, moved, wf])


def test_dispatch_clips_long_notes(vault: Path) -> None:
    (vault / "05-resources" / "huge.md").write_text("z" * 10000)
    res = dispatch(vault, "read_note", {"path": "05-resources/huge.md"})
    assert res["ok"] and res["clipped"] is True and len(res["text"]) < 5000


def test_instructions_block_mentions_the_tools_and_workflows() -> None:
    for name in ("capture_note", "search_notes", "read_note", "list_todos", "second_brain_workflow", "think_hard",
                 "edit_note", "undo_note"):
        assert name in sb.INSTRUCTIONS_BLOCK
    for phrase in ("plan my day", "weekly review", "triage my inbox", "distill this"):
        assert phrase in sb.INSTRUCTIONS_BLOCK


def _edit(root: Path, path: str, old: str, new: str) -> dict:
    note = dispatch(root, "read_note", {"path": path})
    assert note["ok"]
    return dispatch(root, "edit_note", {"path": path, "revision": note["revision"],
                                        "old_text": old, "new_text": new})


def test_edit_shopping_list_keeps_one_file_and_exact_frontmatter(vault: Path) -> None:
    path = capture(vault, "# Shopping\n- [ ] wipes\n- [ ] bathroom mat", now=NOW).path
    file = vault / path
    original = file.read_text().replace("kind: note", "kind: note\ntags:\n  - home\ncustom: 'keep this'")
    file.write_text(original)
    before_paths = sorted(p.relative_to(vault) for p in vault.rglob("*.md"))
    result = _edit(vault, path, "", "- [ ] alcohol wipes for electronics")
    assert result["ok"] and result["path"] == path and result["changed"]
    assert file.read_text() == original + "- [ ] alcohol wipes for electronics\n"
    assert sorted(p.relative_to(vault) for p in vault.rglob("*.md")) == before_paths
    assert _edit(vault, path, "- [ ] wipes", "- [x] wipes")["ok"]
    assert _edit(vault, path, "- [ ] bathroom mat\n", "")["ok"]
    assert file.read_text() == original.replace("- [ ] wipes", "- [x] wipes").replace(
        "- [ ] bathroom mat\n", "") + "- [ ] alcohol wipes for electronics\n"
    assert [h.path for h in search(vault, "electronics")] == [path]


def test_todo_completion_reopen_and_priority_change(vault: Path) -> None:
    capture(vault, "p2 buy milk", kind="todo", now=NOW)
    capture(vault, "p1 call Sam", kind="todo", now=NOW)
    line = "- [ ] buy milk (added 2026-09-21)"
    assert _edit(vault, sb.TODOS_FILE, line, line.replace("[ ]", "[x]"))["ok"]
    assert todos(vault)["P2"] == []
    assert _edit(vault, sb.TODOS_FILE, line.replace("[ ]", "[x]"), line)["ok"]
    assert todos(vault)["P2"] == ["buy milk (added 2026-09-21)"]
    old = (vault / sb.TODOS_FILE).read_text()
    new = old.replace(line + "\n", "").replace("## P0\n", "## P0\n" + line + "\n")
    assert _edit(vault, sb.TODOS_FILE, old, new)["ok"]
    assert todos(vault)["P0"] == ["buy milk (added 2026-09-21)"]
    assert todos(vault)["P2"] == []
    assert todos(vault)["P1"] == ["call Sam (added 2026-09-21)"]


def test_edit_refuses_stale_ambiguous_missing_or_frontmatter_matches(vault: Path) -> None:
    path = capture(vault, "milk\nmilk\nkeep me", now=NOW).path
    before = (vault / path).read_text()
    stale = read_note(vault, path)["revision"]
    for old in ("milk", "not present", "status: inbox"):
        result = _edit(vault, path, old, "oops")
        assert not result["ok"] and "exactly once" in result["reason"]
        assert (vault / path).read_text() == before
    assert _edit(vault, path, "milk\nmilk", "oat milk")["ok"]  # positive control: context disambiguates
    current = (vault / path).read_text()
    for revision in (stale, ""):
        result = dispatch(vault, "edit_note", {"path": path, "revision": revision,
                                              "old_text": "keep me", "new_text": "lost"})
        assert not result["ok"] and "read_note" in result["reason"]
        assert (vault / path).read_text() == current


def test_undo_restores_bytes_and_walks_history_without_toggling(vault: Path) -> None:
    path = capture(vault, "original", now=NOW).path
    original = (vault / path).read_bytes()
    first = _edit(vault, path, "original", "second")
    second = _edit(vault, path, "second", "original")  # deliberately return to a previous state
    assert first["ok"] and second["ok"]
    for expected, change in ((b"second\n", second), (b"original\n", first)):
        note = read_note(vault, path)
        assert note["undo_id"] == change["undo_id"]
        result = dispatch(vault, "undo_note", {"path": path, "undo_id": note["undo_id"],
                                              "revision": note["revision"]})
        assert result["ok"] and (vault / path).read_bytes().endswith(expected)
    assert (vault / path).read_bytes() == original
    assert read_note(vault, path)["undo_id"] is None
    assert len(list((vault / sb.HISTORY_DIR).rglob("*.undone"))) == 2


def test_undo_refuses_later_obsidian_changes_and_wrong_note(vault: Path) -> None:
    path = capture(vault, "original", now=NOW).path
    change = _edit(vault, path, "original", "second")
    other = capture(vault, "second", now=NOW).path
    assert not dispatch(vault, "undo_note", {"path": other, "undo_id": change["undo_id"],
                                            "revision": read_note(vault, other)["revision"]})["ok"]
    (vault / path).write_text("a later manual edit\n")
    for revision in (change["revision"], read_note(vault, path)["revision"]):
        result = dispatch(vault, "undo_note", {"path": path, "undo_id": change["undo_id"], "revision": revision})
        assert not result["ok"] and "note changed" in result["reason"]
        assert (vault / path).read_text() == "a later manual edit\n"
    assert read_note(vault, path)["undo_id"] is None


def test_edit_paths_history_and_hidden_search(vault: Path, tmp_path: Path) -> None:
    outside = tmp_path / "outside.md"
    outside.write_text("outside")
    (vault / "link.md").symlink_to(outside)
    hidden = vault / ".obsidian" / "hidden.md"
    hidden.write_text("hidden")
    for path in ("../outside.md", str(outside), "link.md", ".obsidian/hidden.md", "02-todos/nope.md"):
        for name in ("edit_note", "undo_note"):
            assert not dispatch(vault, name, {"path": path, "revision": "x", "undo_id": "../x",
                                              "old_text": "", "new_text": "oops"})["ok"]
    assert outside.read_text() == "outside" and hidden.read_text() == "hidden"
    (vault / "link.md").unlink()
    path = capture(vault, "# Test\nsecretcobalt", now=NOW).path
    assert [h.path for h in search(vault, "secretcobalt")] == [path]  # positive control
    assert _edit(vault, path, "secretcobalt", "replacement")["ok"]
    assert search(vault, "secretcobalt") == []
    record = next((vault / sb.HISTORY_DIR).rglob("*.json"))
    assert "secretcobalt" in record.read_text()
    assert not read_note(vault, record.relative_to(vault).as_posix())["ok"]
    pack = PackDef(name="all", description="", budget_tokens=100000, include=[Include("**/*")])
    assert "secretcobalt" not in compile_pack(vault, pack).xml


def test_history_symlink_and_failed_backup_leave_note_unchanged(vault: Path, tmp_path: Path, monkeypatch) -> None:
    path = capture(vault, "keep", now=NOW).path
    revision = read_note(vault, path)["revision"]
    before = (vault / path).read_bytes()
    external = tmp_path / "external"
    external.mkdir()
    (vault / sb.HISTORY_DIR).symlink_to(external, target_is_directory=True)
    assert not dispatch(vault, "edit_note", {"path": path, "revision": revision,
                                           "old_text": "keep", "new_text": "lose"})["ok"]
    assert (vault / path).read_bytes() == before and list(external.iterdir()) == []
    (vault / sb.HISTORY_DIR).unlink()

    def fail_write(*args):
        raise OSError("disk full")

    monkeypatch.setattr(sb, "_atomic_text", fail_write)
    assert not _edit(vault, path, "keep", "lose")["ok"]
    assert (vault / path).read_bytes() == before


def test_clipped_reads_support_targeted_edits_without_truncation(vault: Path) -> None:
    body = "visible passage\n" + "z" * 10000 + "\nkeep this tail\n"
    path = capture(vault, body, now=NOW).path
    before = (vault / path).read_text()
    assert dispatch(vault, "read_note", {"path": path})["clipped"]
    assert _edit(vault, path, "visible passage", "changed passage")["ok"]
    assert (vault / path).read_text() == before.replace("visible passage", "changed passage")


def test_two_simultaneous_edits_cannot_overwrite_each_other(vault: Path) -> None:
    path = capture(vault, "keep", now=NOW).path
    revision = read_note(vault, path)["revision"]
    def append(text):
        return dispatch(vault, "edit_note", {"path": path, "revision": revision, "old_text": "", "new_text": text})
    with ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(append, ["one", "two"]))
    assert sum(result["ok"] for result in results) == 1
    text = (vault / path).read_text()
    assert text.endswith("keep\none\n") or text.endswith("keep\ntwo\n")


def test_todo_capture_and_edit_cannot_silently_lose_a_change(vault: Path, monkeypatch) -> None:
    capture(vault, "first", kind="todo", now=NOW)
    path = sb.TODOS_FILE
    revision = read_note(vault, path)["revision"]
    real_append_under = sb._append_under
    edit_result: list = []
    workers: list[threading.Thread] = []

    def edit():
        edit_result.append(dispatch(vault, "edit_note", {"path": path, "revision": revision,
                                                         "old_text": "", "new_text": "edited line"}))

    def slow_append_under(*args):
        # An edit lands between the capture's read and its write.
        worker = threading.Thread(target=edit)
        workers.append(worker)
        worker.start()
        worker.join(timeout=0.3)
        return real_append_under(*args)

    monkeypatch.setattr(sb, "_append_under", slow_append_under)
    capture(vault, "second", kind="todo", now=NOW)
    workers[0].join(timeout=5)
    text = (vault / path).read_text()
    assert "second (added 2026-09-21)" in text
    # Either both changes survive, or the edit was refused as stale; never "ok" and lost.
    assert edit_result[0]["ok"] is False or "edited line" in text


def test_failed_note_write_keeps_original_and_does_not_offer_failed_undo(vault: Path, monkeypatch) -> None:
    path = capture(vault, "keep", now=NOW).path
    original = (vault / path).read_bytes()
    real_replace = sb.os.replace

    def fail_note_replace(source, destination):
        if destination == vault / path:
            raise OSError("cannot replace note")
        return real_replace(source, destination)

    monkeypatch.setattr(sb.os, "replace", fail_note_replace)
    result = _edit(vault, path, "keep", "changed")
    assert not result["ok"] and (vault / path).read_bytes() == original
    assert len(list((vault / sb.HISTORY_DIR).rglob("*.failed"))) == 1
    assert not list((vault / sb.INBOX_DIR).glob(".eve-*"))
    # Even if a later manual edit matches the failed write, it must not gain an undo entry.
    (vault / path).write_text(original.decode().replace("keep", "changed"))
    assert read_note(vault, path)["undo_id"] is None


def test_edit_rechecks_after_backup_when_an_external_editor_changes_note(vault: Path, monkeypatch) -> None:
    path = capture(vault, "keep", now=NOW).path
    real_write = sb._atomic_text

    def external_change_after_backup(destination, text):
        real_write(destination, text)
        if destination.suffix == ".json":
            (vault / path).write_text("edited in Obsidian\n")

    monkeypatch.setattr(sb, "_atomic_text", external_change_after_backup)
    result = _edit(vault, path, "keep", "changed")
    assert not result["ok"] and "note changed" in result["reason"]
    assert (vault / path).read_text() == "edited in Obsidian\n"


def test_the_daily_plan_is_written_in_one_pass_with_the_calendar(vault: Path) -> None:
    """2026-09-24 08:44: "plan my day" went vault → think_hard (20.5 s at high effort) and came back as "a
    suggested plan … with no calendar checked": the pack never reads the calendar and the second model has
    no tools to. The brain that holds the calendar tools writes the plan itself, from the calendar and the pack."""
    wf = dispatch(vault, "second_brain_workflow", {"name": "daily-plan", "extra": None})
    assert "think_hard" not in wf["note"] and "calendar" in wf["note"]
    assert "calendar" in wf["prompt"].split("<context")[0]
    tool = next(t for t in sb.SECOND_BRAIN_TOOLS if t["name"] == "second_brain_workflow")
    assert "think_hard" not in tool["description"]
    sentence = next(x for x in sb.INSTRUCTIONS_BLOCK.replace("\n", " ").split(". ") if "plan my day" in x)
    assert "think_hard" not in sentence and "calendar" in sentence
