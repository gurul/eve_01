"""Sources under a reply: which memory lines a turn's answer leaned on (docs/app-chat.md, "Sources").

ChatGPT's memory article: "When ChatGPT uses relevant personal context, Sources may appear below the response."
eve does the same in the Eve Link app. While a turn runs, every memory read the brain makes (``memory_search``,
``memory_read``) is looked at after it returns, and what it found is kept as a short list of sources: a record's
fact line, a recalled meaning, or a line that was said. When the turn's reply is written to the transcript the list
rides on that one line as ``src``; ``/api/link/messages`` gives it to the app as ``sources`` (link_chat._row), and
the app shows "Sources · N" under the reply.

The model never sees any of this: the tool result it is handed is the one the Memory returned, untouched, and the
history is the chat log's words. Only the transcript line and the app's row carry the sources.

A source is ``{"kind": "fact" | "remembered" | "said", "text": str, "when"?: str}``, at most MAX_SOURCES of them,
each text at most MAX_TEXT_CHARS."""

from __future__ import annotations

import re
from typing import Any, Optional

KINDS = ("fact", "remembered", "said")
MAX_SOURCES = 6
MAX_TEXT_CHARS = 200
MAX_WHEN_CHARS = 40

# memory._recalled's "(day time, channel) text"
_RECALLED = re.compile(r"^\((?P<tag>[^()]{1,80})\)\s*(?P<text>.+)$", re.S)
_WHO = {"owner": "You", "eve": "eve", "claude": "Claude", "notes": "Notes"}


def _clip(text: Any, cap: int) -> str:
    s = " ".join(str(text or "").split())
    return s if len(s) <= cap else s[: cap - 1].rstrip() + "…"


def _source(kind: str, text: Any, when: Any = "") -> Optional[dict[str, str]]:
    body = _clip(text, MAX_TEXT_CHARS)
    if kind not in KINDS or not body:
        return None
    out = {"kind": kind, "text": body}
    w = _clip(when, MAX_WHEN_CHARS)
    if w:
        out["when"] = w
    return out


def _said(hit: dict[str, Any]) -> Optional[dict[str, str]]:
    who = str(hit.get("who") or "")
    text = str(hit.get("text") or "")
    if who:
        text = f"{_WHO.get(who, who)}: {text}"
    when = " ".join(p for p in (str(hit.get("day") or ""), str(hit.get("time") or "")) if p)
    return _source("said", text, when)


def _hit(hit: Any) -> Optional[dict[str, str]]:
    """One entry of a search's ``hits``: a record's fact ({id, line, src?}) or, from a bare transcript search, a
    line said ({day, time, who, text})."""
    if not isinstance(hit, dict):
        return None
    if "line" in hit:
        rid, line = str(hit.get("id") or ""), str(hit.get("line") or "")
        return _source("fact", f"{rid}: {line}" if rid else line, hit.get("src") or "")
    if "text" in hit:
        return _said(hit)
    return None


def _recalled(row: Any) -> Optional[dict[str, str]]:
    if isinstance(row, dict):
        return _source("remembered", row.get("text"), row.get("src") or row.get("day") or "")
    m = _RECALLED.match(str(row or "").strip())
    if m is None:
        return _source("remembered", row)
    return _source("remembered", m.group("text"), m.group("tag"))


def from_result(name: str, result: Any) -> list[dict[str, str]]:
    """What one memory read found, as sources, in the order shown (taken in turn from the facts, the meanings and
    the lines said, so one kind never crowds out the rest). [] for anything else, a failed read, or nothing found."""
    if not isinstance(result, dict) or result.get("ok") is False:
        return []
    if name == "memory_read":
        if isinstance(result.get("record"), str):
            src = _source("fact", result["record"])
        elif isinstance(result.get("text"), str):
            src = _source("said", result["text"], result.get("day") or "")
        else:
            src = None
        return [src] if src else []
    if name != "memory_search":
        return []
    groups = [
        [s for s in (_hit(h) for h in _list(result.get("hits"))) if s],
        [s for s in (_recalled(r) for r in _list(result.get("recalled"))) if s],
        [s for s in (_said(h) for h in _list(result.get("said")) if isinstance(h, dict)) if s],
    ]
    out: list[dict[str, str]] = []
    i = 0
    while len(out) < MAX_SOURCES and any(i < len(g) for g in groups):
        for g in groups:
            if i < len(g) and len(out) < MAX_SOURCES:
                out.append(g[i])
        i += 1
    return out


def _list(raw: Any) -> list[Any]:
    return list(raw) if isinstance(raw, list) else []


def add(kept: list[dict[str, str]], found: list[dict[str, str]]) -> None:
    """``found`` onto a turn's ``kept`` sources: no duplicate (same kind and text), at most MAX_SOURCES."""
    seen = {(s["kind"], s["text"]) for s in kept}
    for s in found:
        if len(kept) >= MAX_SOURCES:
            return
        key = (s["kind"], s["text"])
        if key not in seen:
            seen.add(key)
            kept.append(s)


def sanitize(raw: Any) -> list[dict[str, str]]:
    """A transcript line's ``src`` as the app may show it: well-formed entries only, string fields, capped."""
    if not isinstance(raw, list):
        return []
    out: list[dict[str, str]] = []
    for s in raw:
        if len(out) >= MAX_SOURCES:
            break
        if not isinstance(s, dict) or s.get("kind") not in KINDS or not isinstance(s.get("text"), str):
            continue
        when = s.get("when")
        clean = _source(s["kind"], s["text"], when if isinstance(when, str) else "")
        if clean:
            out.append(clean)
    return out


def mentions(raw: Any, pred: Any) -> bool:
    """Whether any of a line's sources holds words ``pred`` matches (transcripts' forget scrubs them)."""
    return isinstance(raw, list) and any(isinstance(s, dict) and pred(str(s.get("text") or "")) for s in raw)
