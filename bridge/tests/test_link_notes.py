"""link_notes.py and the owner's edits in second_brain.py: the Notes app over a vault in tmp_path. Every action, the
off state, frontmatter kept on a save, a stale revision refused, delete and restore, and paths that leave the vault.
The words here are made up."""

from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import link_notes, second_brain


def vault(tmp_path: Path) -> Path:
    root = tmp_path / "vault"
    second_brain.ensure_vault(root)
    (root / "01-inbox" / "tea.md").write_text("---\ncreated: 2026-10-01\nsource: telegram\n---\n# Tea\n\n- [ ] buy oolong\n"
                                               "- [x] boil water\n", encoding="utf-8")
    (root / "03-projects" / "garden").mkdir()
    (root / "03-projects" / "garden" / "beds.md").write_text("# Beds\nRaised beds by the fence.\n", encoding="utf-8")
    os.utime(root / "01-inbox" / "tea.md", (2_000_000_000, 2_000_000_000))       # the newest
    return root


def panel(root: Path, enabled: bool = True) -> link_notes.NotesPanel:
    return link_notes.NotesPanel(lambda: second_brain.VaultConfig(enabled=enabled, root=root))


def run(p: link_notes.NotesPanel, body: dict[str, Any]) -> dict[str, Any]:
    out = asyncio.run(p.panel(body))
    json.dumps(out)                                                       # always JSON, as link_chat sends it
    return out


def test_off_or_missing_vault_is_unavailable_and_says_so(tmp_path: Path) -> None:
    root = vault(tmp_path)
    for p in (panel(root, enabled=False), panel(tmp_path / "nowhere")):
        assert p.available is False
        assert run(p, {"action": "list"}) == {"ok": False, "line": link_notes.OFF_LINE}
    assert panel(root).available is True


def test_list_is_every_note_newest_first_with_folders(tmp_path: Path) -> None:
    root = vault(tmp_path)
    out = run(panel(root), {})
    assert out["ok"] is True
    first = out["notes"][0]
    assert first == {"path": "01-inbox/tea.md", "folder": "01-inbox", "title": "Tea", "preview": "buy oolong",
                     "modified": 2_000_000_000}
    paths = [n["path"] for n in out["notes"]]
    assert "03-projects/garden/beds.md" in paths and "CLAUDE.md" in paths
    assert not any(p.startswith(".") or "/." in p for p in paths)
    assert "03-projects/garden" in out["folders"] and out["folders"][:2] == ["00-vision", "01-inbox"]


def test_a_captures_preview_is_not_its_title_again(tmp_path: Path) -> None:
    root = vault(tmp_path)
    (root / "01-inbox" / "cap.md").write_text("---\nkind: note\n---\nThe blue cafe is good\n\nGo on a Tuesday\n", encoding="utf-8")
    (root / "01-inbox" / "one.md").write_text("Only a line\n", encoding="utf-8")
    rows = {n["path"]: n for n in run(panel(root), {"action": "list"})["notes"]}
    assert rows["01-inbox/cap.md"]["title"] == "The blue cafe is good"
    assert rows["01-inbox/cap.md"]["preview"] == "Go on a Tuesday"
    assert rows["01-inbox/one.md"]["preview"] == ""
    assert rows["03-projects/garden/beds.md"]["preview"] == "Raised beds by the fence."


def test_read_splits_the_frontmatter_off_and_save_keeps_it(tmp_path: Path) -> None:
    root = vault(tmp_path)
    p = panel(root)
    note = run(p, {"action": "read", "path": "01-inbox/tea.md"})
    assert note["ok"] and note["title"] == "Tea" and note["editable"] is True
    assert note["front"] == ["created: 2026-10-01", "source: telegram"]
    assert note["body"].startswith("# Tea\n") and "---" not in note["body"]
    body = note["body"].replace("- [ ] buy oolong", "- [x] buy oolong")
    saved = run(p, {"action": "save", "path": "01-inbox/tea.md", "revision": note["revision"], "body": body})
    assert saved["ok"] and saved["changed"] is True and saved["undo_id"]
    text = (root / "01-inbox" / "tea.md").read_text(encoding="utf-8")
    assert text.startswith("---\ncreated: 2026-10-01\nsource: telegram\n---\n# Tea") and "- [x] buy oolong" in text
    # the same body again changes nothing
    again = run(p, {"action": "save", "path": "01-inbox/tea.md", "revision": saved["revision"], "body": body})
    assert again["ok"] and again["changed"] is False
    # and undo takes the save back
    undone = run(p, {"action": "undo", "path": "01-inbox/tea.md", "revision": saved["revision"],
                     "undo_id": saved["undo_id"]})
    assert undone["ok"] and "- [ ] buy oolong" in (root / "01-inbox" / "tea.md").read_text(encoding="utf-8")


def test_a_stale_revision_is_refused_and_nothing_is_written(tmp_path: Path) -> None:
    root = vault(tmp_path)
    p = panel(root)
    note = run(p, {"action": "read", "path": "01-inbox/tea.md"})
    (root / "01-inbox" / "tea.md").write_text("# Tea\nchanged in Obsidian\n", encoding="utf-8")
    for body in ({"action": "save", "path": "01-inbox/tea.md", "revision": note["revision"], "body": "mine"},
                 {"action": "delete", "path": "01-inbox/tea.md", "revision": note["revision"]},
                 {"action": "save", "path": "01-inbox/tea.md", "revision": "", "body": "mine"}):
        assert run(p, body) == {"ok": False, "line": link_notes.CHANGED_LINE}, body
    assert (root / "01-inbox" / "tea.md").read_text(encoding="utf-8") == "# Tea\nchanged in Obsidian\n"


def test_create_names_the_page_from_its_title_and_never_overwrites(tmp_path: Path) -> None:
    root = vault(tmp_path)
    p = panel(root)
    a = run(p, {"action": "create", "folder": "03-projects/garden", "title": "Seed list", "body": "- tomatoes"})
    b = run(p, {"action": "create", "folder": "03-projects/garden", "title": "Seed list"})
    assert a["path"] == "03-projects/garden/seed-list.md" and b["path"] == "03-projects/garden/seed-list-2.md"
    assert (root / a["path"]).read_text(encoding="utf-8") == "# Seed list\n\n- tomatoes\n"
    assert (root / b["path"]).read_text(encoding="utf-8") == "# Seed list\n"
    assert run(p, {"action": "create", "title": ""})["path"] == "01-inbox/untitled.md"      # the inbox by default
    for folder in ("../outside", ".eve-trash", "/tmp", "03-projects/a/b", "nope"):
        out = run(p, {"action": "create", "folder": folder, "title": "x"})
        assert out == {"ok": False, "line": "That isn't one of your note folders."}, folder
    assert not (tmp_path / "outside").exists()


def test_delete_goes_to_the_hidden_trash_and_restore_puts_it_back(tmp_path: Path) -> None:
    root = vault(tmp_path)
    p = panel(root)
    note = run(p, {"action": "read", "path": "01-inbox/tea.md"})
    gone = run(p, {"action": "delete", "path": "01-inbox/tea.md", "revision": note["revision"]})
    assert gone["ok"] and gone["trash_id"]
    assert not (root / "01-inbox" / "tea.md").exists()
    assert "01-inbox/tea.md" not in [n["path"] for n in run(p, {"action": "list"})["notes"]]
    assert run(p, {"action": "search", "query": "oolong"})["hits"] == []                # the trash is not searched
    assert run(p, {"action": "read", "path": f".eve-trash/{gone['trash_id']}/tea.md"})["ok"] is False
    # a new note took the name meanwhile: it keeps it, and the restored one is -2
    (root / "01-inbox" / "tea.md").write_text("# Other tea\n", encoding="utf-8")
    back = run(p, {"action": "restore", "trash_id": gone["trash_id"]})
    assert back["ok"] and back["path"] == "01-inbox/tea-2.md"
    assert "buy oolong" in (root / "01-inbox" / "tea-2.md").read_text(encoding="utf-8")
    assert run(p, {"action": "restore", "trash_id": gone["trash_id"]}) == {"ok": False,
                                                                           "line": "There's nothing to restore."}
    assert run(p, {"action": "restore", "trash_id": "../../x"})["ok"] is False


def test_move_keeps_the_note_as_it_is(tmp_path: Path) -> None:
    root = vault(tmp_path)
    p = panel(root)
    before = (root / "01-inbox" / "tea.md").read_text(encoding="utf-8")
    out = run(p, {"action": "move", "path": "01-inbox/tea.md", "into": "09-archive"})
    assert out == {"ok": True, "path": "09-archive/tea.md", "changed": True, "line": "Moved."}
    assert (root / "09-archive" / "tea.md").read_text(encoding="utf-8") == before          # no status: filed
    assert run(p, {"action": "move", "path": "09-archive/tea.md", "into": "09-archive"})["changed"] is False
    assert run(p, {"action": "move", "path": "09-archive/tea.md", "into": "../x"})["ok"] is False


def test_paths_that_leave_the_vault_or_are_hidden_are_refused(tmp_path: Path) -> None:
    root = vault(tmp_path)
    (tmp_path / "secret.md").write_text("# no\n", encoding="utf-8")
    p = panel(root)
    for path in ("../secret.md", "/etc/hosts", "~/x.md", ".obsidian/app.json", ".eve-history/x.md", ""):
        for action in ("read", "save", "delete", "move"):
            out = run(p, {"action": action, "path": path, "revision": "r", "body": "x", "into": "01-inbox"})
            assert out["ok"] is False, (action, path)
    assert (tmp_path / "secret.md").read_text(encoding="utf-8") == "# no\n"
    assert run(p, {"action": "drop table"}) == {"ok": False, "line": "That isn't something Notes does."}


def test_a_note_too_long_for_one_request_is_read_only(tmp_path: Path) -> None:
    root = vault(tmp_path)
    (root / "05-resources" / "big.md").write_text("# Big\n" + "x" * (link_notes.MAX_BODY_CHARS + 10), encoding="utf-8")
    p = panel(root)
    note = run(p, {"action": "read", "path": "05-resources/big.md"})
    assert note["ok"] and note["editable"] is False
    out = run(p, {"action": "save", "path": "05-resources/big.md", "revision": note["revision"],
                  "body": "y" * (link_notes.MAX_BODY_CHARS + 1)})
    assert out == {"ok": False, "line": link_notes.TOO_LONG_LINE}


def test_search_finds_words_and_says_so_when_none_match(tmp_path: Path) -> None:
    root = vault(tmp_path)
    p = panel(root)
    out = run(p, {"action": "search", "query": "fence"})
    assert out["ok"] and out["hits"][0]["path"] == "03-projects/garden/beds.md"
    assert run(p, {"action": "search", "query": "zebra"}) == {"ok": True, "hits": [], "line": "No note has those words."}
    assert run(p, {"action": "search", "query": "  "})["ok"] is False


def test_an_emptied_body_leaves_the_frontmatter(tmp_path: Path) -> None:
    root = vault(tmp_path)
    note = second_brain.read_note(root, "01-inbox/tea.md")
    out = second_brain.write_note(root, "01-inbox/tea.md", note["revision"], "")
    assert out["changed"] is True
    assert (root / "01-inbox" / "tea.md").read_text(encoding="utf-8") == "---\ncreated: 2026-10-01\nsource: telegram\n---\n"


@pytest.mark.parametrize("body", ["a\r\nb\r\n", "a\rb"])
def test_a_save_writes_unix_newlines(tmp_path: Path, body: str) -> None:
    root = vault(tmp_path)
    note = second_brain.read_note(root, "03-projects/garden/beds.md")
    second_brain.write_note(root, "03-projects/garden/beds.md", note["revision"], body)
    assert (root / "03-projects" / "garden" / "beds.md").read_text(encoding="utf-8") == "a\nb\n"
