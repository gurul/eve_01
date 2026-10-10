"""The owner's notes in the Eve Link app: a Notes app beside Music, Lights and Remote (docs/app-chat.md, "Work").

Owner, 2026-10-10: "an app where I can look at my notes and interact with them change delete updates", "essentially
like a mini notion", "make sure it's mobile native". The notes are the second brain (second_brain.py): the markdown
vault the owner texts eve into and keeps open in Obsidian. The app draws it natively; this is its panel, reached as
the other cards are: ``POST /api/link/controls/notes`` with one JSON body, answered as JSON
(link_chat.LinkChat._controls).

Bodies:

* ``{"action": "list"}`` (or no body): ``{"ok", "notes": [{path, folder, title, preview, modified}], "folders"}``,
  every note newest first (the archive too), and the folders a note can be started in or moved to.
* ``{"action": "read", "path"}``: ``{"ok", "path", "title", "body", "front", "revision", "modified", "editable",
  "undo_id"?}``. ``body`` is the text under the frontmatter, ``front`` the frontmatter's ``key: value`` lines (shown,
  never edited from the phone). A note too long to save in one request is ``editable: false``.
* ``{"action": "save", "path", "revision", "body"}``: the body replaced, the frontmatter kept (``write_note``);
  ``{"ok", "path", "revision", "undo_id", "changed"}``. A revision that is not the file's now is refused: the note
  changed on the Mac since the phone read it.
* ``{"action": "undo", "path", "revision", "undo_id"}``: the save taken back (``undo_note``).
* ``{"action": "create", "folder", "title", "body"?}``: a new page, ``# <title>`` on top (``create_note``).
* ``{"action": "delete", "path", "revision"}``: into the vault's hidden trash (``trash_note``); ``{"ok",
  "trash_id"}``. ``{"action": "restore", "trash_id"}`` puts it back. Nothing is unlinked.
* ``{"action": "move", "path", "into"}``: into another folder of ``folders`` (``move_note``).
* ``{"action": "search", "query"}``: ``{"ok", "hits": [{path, title, snippet}]}``, the brain's own keyword search.

``available`` is false while the second brain is off (EVE_SECOND_BRAIN) or the vault is not there; the
controls listing says so and the app shows no Notes. Counts and file names go to the log, never a note's words.
"""

from __future__ import annotations

import asyncio
import logging
from pathlib import Path
from typing import Any, Callable

from . import second_brain

log = logging.getLogger(__name__)

ACTIONS = ("list", "read", "save", "undo", "create", "delete", "restore", "move", "search")
MAX_BODY_CHARS = 40_000        # a save is one request, and link_chat takes 64,000 bytes of JSON (MAX_BODY_BYTES)
MAX_READ_CHARS = 400_000       # a page, not a database dump
MAX_QUERY_CHARS = 200
MAX_PATH_CHARS = 400
MAX_ID_CHARS = 64
OFF_LINE = "Your notes are off on this Mac (EVE_SECOND_BRAIN)."
CHANGED_LINE = "This note changed on the Mac. Open it again to see the new version."
TOO_LONG_LINE = "This note is too long to save from the phone. Edit it on the Mac."
NOT_A_NOTE = "That note isn't there any more."


class NotesPanel:
    """The Notes app's ``panel(body)``, over ``config()`` -> second_brain.VaultConfig (read per call)."""

    def __init__(self, config: Callable[[], second_brain.VaultConfig] = second_brain.configured) -> None:
        self._config = config

    def _root(self) -> Path | None:
        try:
            cfg = self._config()
        except Exception:  # noqa: BLE001
            return None
        return cfg.root if cfg.enabled and Path(cfg.root).is_dir() else None

    @property
    def available(self) -> bool:
        return self._root() is not None

    async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
        root = self._root()
        if root is None:
            return {"ok": False, "line": OFF_LINE}
        action = str(body.get("action") or "list")
        if action not in ACTIONS:
            return {"ok": False, "line": "That isn't something Notes does."}
        try:
            out = await asyncio.to_thread(self._do, root, action, body)
        except ValueError as e:
            return {"ok": False, "line": _owner_line(str(e))}
        except (OSError, UnicodeDecodeError) as e:
            log.warning("link notes: %s failed (%s)", action, type(e).__name__)
            return {"ok": False, "line": "That didn't work on the Mac just now."}
        out.setdefault("line", "")
        return out

    def _do(self, root: Path, action: str, body: dict[str, Any]) -> dict[str, Any]:
        sb = second_brain
        if action == "list":
            notes = sb.list_notes(root)
            log.info("link notes: list → %d", len(notes))
            return {"ok": True, "notes": notes, "folders": sb.note_folders(root)}
        if action == "search":
            query = " ".join(str(body.get("query") or "").split())[:MAX_QUERY_CHARS]
            if not query:
                return {"ok": False, "line": "Type a word or two to search for."}
            hits = sb.search(root, query, limit=30, include_archive=True)
            log.info("link notes: search → %d", len(hits))
            return {"ok": True, "hits": [{"path": h.path, "title": h.title, "snippet": h.snippet} for h in hits],
                    "line": "" if hits else "No note has those words."}
        if action == "create":
            text = str(body.get("body") or "")
            if len(text) > MAX_BODY_CHARS:
                return {"ok": False, "line": TOO_LONG_LINE}
            return sb.create_note(root, str(body.get("folder") or sb.INBOX_DIR)[:MAX_PATH_CHARS],
                                  str(body.get("title") or ""), text)
        if action == "restore":
            out = sb.restore_note(root, _short(body.get("trash_id"), MAX_ID_CHARS))
            return {**out, "line": "Restored."}
        path = _short(body.get("path"), MAX_PATH_CHARS)
        if not path:
            return {"ok": False, "line": "Which note?"}
        if action == "read":
            return _read(root, path)
        if action == "move":
            out = sb.move_note(root, path, _short(body.get("into"), MAX_PATH_CHARS))
            return {**out, "line": "Moved." if out.get("changed") else ""}
        revision = _short(body.get("revision"), MAX_ID_CHARS)
        if action == "delete":
            return {**sb.trash_note(root, path, revision), "line": "Deleted."}
        if action == "undo":
            out = sb.undo_note(root, path, _short(body.get("undo_id"), MAX_ID_CHARS * 2), revision)
            return {**out, "line": "Undone."}
        text = body.get("body")
        if not isinstance(text, str):
            return {"ok": False, "line": "Nothing to save."}
        if len(text) > MAX_BODY_CHARS:
            return {"ok": False, "line": TOO_LONG_LINE}
        return sb.write_note(root, path, revision, text)


def _read(root: Path, rel: str) -> dict[str, Any]:
    sb = second_brain
    path = sb._resolve(root, rel)
    if not path.is_file() or path.suffix != ".md":
        raise ValueError("no such markdown note")
    text = path.read_text(encoding="utf-8", errors="replace")
    meta, body = sb.split_frontmatter(text)
    rel = sb._rel(root, path)
    clipped = len(body) > MAX_READ_CHARS
    return {"ok": True, "path": rel, "title": sb.title_of(text, path.stem), "body": body[:MAX_READ_CHARS],
            "front": [f"{k}: {v}" for k, v in meta.items()], "revision": sb._revision(text),
            "modified": int(path.stat().st_mtime), "editable": not clipped and len(body) <= MAX_BODY_CHARS,
            "undo_id": sb._latest_edit(root, rel, text)}


def _short(value: Any, limit: int) -> str:
    return str(value or "").strip()[:limit]


def _owner_line(reason: str) -> str:
    """second_brain's reasons, written for an agent, said to the owner."""
    if reason.startswith("note changed"):
        return CHANGED_LINE
    if reason.startswith(("no such", "nothing to restore")):
        return NOT_A_NOTE if reason.startswith("no such") else "There's nothing to restore."
    if reason.startswith("not a vault folder"):
        return "That isn't one of your note folders."
    if reason.startswith("use an undo_id") or reason.startswith("invalid note history"):
        return "That change can't be undone any more."
    if reason.startswith(("path ", "hidden paths")):
        return NOT_A_NOTE
    return "That didn't work: " + reason
