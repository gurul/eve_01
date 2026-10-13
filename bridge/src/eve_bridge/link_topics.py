"""Horizontal History: eve's one conversation cut into topics the app shows as cards, and the ones the owner saved.

Owner, 2026-10-13: past topics "organized as swipeable horizontal cards. Users can bookmark important topics and
quickly jump back into them" — "for our eve app", and "remember occams razor". eve has one continuous
conversation, so a topic is simply a run of messages with no pause longer than ``GAP_SECS``. Its title is its first
owner message. No model is asked anything.

Bookmarks live in ``~/.config/eve-bridge/chat/bookmarks.json`` (0600), keyed by the topic's first message id, so a
saved topic stays saved while it grows.
"""

from __future__ import annotations

import json
import os
import re
import tempfile
import time
from pathlib import Path
from typing import Any, Iterable, Optional

GAP_SECS = 45 * 60            # a pause longer than this starts a new topic
DAYS = 30                     # transcript days read for the cards
TITLE_CHARS = 60
PREVIEW_CHARS = 120
DEFAULT_LIMIT = 30
MAX_LIMIT = 60
BOOKMARKS_FILE = "~/.config/eve-bridge/chat/bookmarks.json"
ID_RE = re.compile(r"^\d{4}-\d{2}-\d{2}:\d{1,7}$")    # a message id: <day>:<line index>
# what makes a line read badly on a card: markdown marks, an OpenUI program, a component action's context
_PROGRAM = re.compile(r"```[\s\S]*?(```|$)|\n\n\[\"User clicked: [\s\S]*$")
_MARKS = re.compile(r"[*_`#>|~\[\]]+")


def plain(text: str) -> str:
    return " ".join(_MARKS.sub(" ", _PROGRAM.sub(" ", text)).split())


def clip(text: str, n: int) -> str:
    if len(text) <= n:
        return text
    cut = text[: n - 1].rsplit(" ", 1)[0] if " " in text[: n - 1] else text[: n - 1]
    return cut.rstrip(" ,.;:-") + "…"


def segment(rows: Iterable[dict[str, Any]], gap: float = GAP_SECS) -> list[dict[str, Any]]:
    """Rows as link_chat._row makes them, oldest first → topics, oldest first. A run with no owner message (eve's
    own reminders alone) is not a topic."""
    runs: list[list[dict[str, Any]]] = []
    for row in rows:
        if runs and row["t"] - runs[-1][-1]["t"] <= gap:
            runs[-1].append(row)
        else:
            runs.append([row])
    out = []
    for run in runs:
        first_owner = next((plain(r["text"]) for r in run if r["role"] == "owner" and plain(r["text"])), None)
        if first_owner is None:
            continue
        out.append({"id": run[0]["id"], "first_id": run[0]["id"], "last_id": run[-1]["id"],
                    "started_at": run[0]["t"], "ended_at": run[-1]["t"], "count": len(run),
                    "title": clip(first_owner, TITLE_CHARS),
                    "preview": clip(next((plain(r["text"]) for r in reversed(run)
                                          if r["role"] == "eve" and plain(r["text"])), ""), PREVIEW_CHARS)})
    return out


def read_rows(store: Any, days: int = DAYS) -> list[dict[str, Any]]:
    """Every shown message of the last ``days`` transcript days, oldest first. Blocking: call it in a thread."""
    from .link_chat import _row

    rows: list[dict[str, Any]] = []
    for day in store.days()[-days:]:
        rows.extend(r for r in (_row(day, i, ln) for i, ln in enumerate(store.lines(day))) if r is not None)
    return rows


class Bookmarks:
    def __init__(self, path: str | Path = BOOKMARKS_FILE) -> None:
        self.path = Path(path).expanduser()

    def load(self) -> dict[str, float]:
        try:
            items = json.loads(self.path.read_text()).get("items", {})
        except (OSError, ValueError, AttributeError):
            return {}
        return {k: float(v) for k, v in items.items() if isinstance(k, str) and ID_RE.match(k)
                and isinstance(v, (int, float))}

    def set(self, topic_id: str, on: bool, now: Optional[float] = None) -> None:
        items = self.load()
        if on:
            items.setdefault(topic_id, time.time() if now is None else now)
        else:
            items.pop(topic_id, None)
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        fd, tmp = tempfile.mkstemp(dir=self.path.parent, prefix=".bookmarks.")
        with os.fdopen(fd, "w") as f:
            json.dump({"version": 1, "items": items}, f)
        os.chmod(tmp, 0o600)
        os.replace(tmp, self.path)


def view(store: Any, bookmarks: Bookmarks, limit: int = DEFAULT_LIMIT) -> dict[str, Any]:
    """``GET /api/link/topics``: the newest ``limit`` topics, and the saved ones (newest bookmark first), each with
    ``bookmarked``. A saved topic whose first message was forgotten drops out. Blocking: call it in a thread."""
    found = segment(read_rows(store))
    saved = bookmarks.load()
    for t in found:
        t["bookmarked"] = t["id"] in saved
    by_id = {t["id"]: t for t in found}
    kept = sorted((i for i in saved if i in by_id), key=lambda i: saved[i], reverse=True)
    return {"topics": list(reversed(found))[:limit], "bookmarked": [by_id[i] for i in kept]}


def known(store: Any, topic_id: str) -> bool:
    return any(t["id"] == topic_id for t in segment(read_rows(store)))
