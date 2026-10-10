"""eve's memory in the Eve Link app: a Memory card beside Music, Lights and Remote (docs/app-chat.md, "Work").

Owner, 2026-10-09: "make memories accessible through app". The card is the chat's own memory tools (memory.py:
``search``, ``forget_preview``, ``forget_apply``, and the profile the brain reads), reached the way the other cards
are: ``POST /api/link/controls/memory`` with one JSON body, answered as JSON (link_chat.LinkChat._controls). Nothing
new is stored; nothing is forgotten without the two-step the chat has (a preview that counts, then the token).

Bodies:

* ``{"action": "profile"}`` (or no body): ``{"ok", "profile"}``, what eve knows about the owner (the one-pager).
* ``{"action": "search", "query"}``: ``{"ok", "hits", "recalled", "said", "note"?}`` as ``Memory.search`` returns
  them: record facts ``[{id, line, src?}]``, recalled meanings ``[str]``, and the words said
  ``[{day, time, ch, who, text}]`` (the line before and after are left out: a phone screen).
* ``{"action": "forget", "query"}``: ``Memory.forget_preview``: ``{"ok", "total", "layers", "token", "line"}``;
  the token is good once for ten minutes. Counts, never the words.
* ``{"action": "apply", "token"}``: ``Memory.forget_apply``: ``{"ok", "total", "layers", "line"}``.
* ``{"action": "remember", "text"}``: ``Memory.star``, the chat's "remember that": one claim kept for good, on the
  profile from the next message (owner, 2026-10-09: "add all of these to memories"). ``{"ok", "line"}``.
* ``{"action": "hide", "text"}``: ``Memory.hide``, ChatGPT's "Don't mention this again": every line the words match
  is left out of the profile and every search until unhidden; nothing is deleted. ``{"ok", "line"}``.
* ``{"action": "unhide", "text"}``: ``Memory.unhide``, the hidden line taken back. ``{"ok", "line"}``.
* ``{"action": "hidden"}``: ``{"ok", "hidden": [str]}``, what is hidden, oldest first.

``available`` is false while memory is off (EVE_MEMORY) or not built yet; the controls listing says so and the
app shows no card. The memory object is read at each call (``getter``): the daemon builds it after the server.
"""

from __future__ import annotations

import asyncio
import logging
from typing import Any, Callable, Optional

log = logging.getLogger(__name__)

MAX_QUERY_CHARS = 200
MAX_TOKEN_CHARS = 64
MAX_REMEMBER_CHARS = 500
MAX_HIDE_CHARS = 500
OFF_LINE = "eve's memory is off on this Mac (EVE_MEMORY)."
ACTIONS = ("profile", "search", "forget", "apply", "remember", "hide", "unhide", "hidden")


class MemoryPanel:
    """The Memory card's ``panel(body)``, over ``getter()`` -> memory.Memory or None."""

    def __init__(self, getter: Callable[[], Optional[Any]]) -> None:
        self._getter = getter

    @property
    def available(self) -> bool:
        try:
            return self._getter() is not None
        except Exception:  # noqa: BLE001
            return False

    async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
        memory = self._getter()
        if memory is None:
            return {"ok": False, "line": OFF_LINE}
        action = str(body.get("action") or "profile")
        if action not in ACTIONS:
            return {"ok": False, "line": "That isn't something the Memory card does."}
        if action == "profile":
            text = await asyncio.to_thread(memory.profile)
            return {"ok": True, "profile": text or "", "line": "" if text else "eve has no profile of you yet."}
        if action == "apply":
            token = str(body.get("token") or "").strip()[:MAX_TOKEN_CHARS]
            out = await asyncio.to_thread(memory.forget_apply, token)
            return self._forgotten(out)
        if action == "remember":
            text = " ".join(str(body.get("text") or "").split())[:MAX_REMEMBER_CHARS]
            if len(text) < 4:
                return {"ok": False, "line": "Type what to remember."}
            kept = await asyncio.to_thread(memory.star, text)
            log.info("link memory: remember → %s", "kept" if kept else "refused")
            return {"ok": bool(kept), "line": "Remembered." if kept else "That couldn't be kept just now."}
        if action == "hidden":
            rows = await asyncio.to_thread(memory.hidden)
            log.info("link memory: hidden → %d", len(rows))
            return {"ok": True, "hidden": [str(r) for r in rows], "line": ""}
        if action in ("hide", "unhide"):
            text = " ".join(str(body.get("text") or "").split())[:MAX_HIDE_CHARS]
            if not text:
                return {"ok": False, "line": "Nothing to hide." if action == "hide" else "Nothing to unhide."}
            if action == "hide":
                kept = await asyncio.to_thread(memory.hide, text)
                log.info("link memory: hide → %s", "kept" if kept else "refused")
                return {"ok": bool(kept),
                        "line": "eve won't bring that up again." if kept else "That couldn't be hidden just now."}
            gone = await asyncio.to_thread(memory.unhide, text)
            log.info("link memory: unhide → %s", "done" if gone else "none")
            return {"ok": bool(gone), "line": "eve can bring that up again." if gone else "That wasn't hidden."}
        query = " ".join(str(body.get("query") or "").split())[:MAX_QUERY_CHARS]
        if not query:
            return {"ok": False, "line": "Type a word or two to search for."}
        if action == "search":
            out = await asyncio.to_thread(memory.search, query)
            if not out.get("ok", True):
                return {"ok": False, "line": str(out.get("reason") or "That search didn't work.")}
            said = [{k: h.get(k, "") for k in ("day", "time", "ch", "who", "text")}
                    for h in out.get("said") or [] if isinstance(h, dict)]
            result: dict[str, Any] = {"ok": True, "hits": list(out.get("hits") or []),
                                      "recalled": [str(r) for r in out.get("recalled") or []], "said": said,
                                      "line": str(out.get("note") or "")}
            log.info("link memory: search → %d hits, %d recalled, %d said", len(result["hits"]),
                     len(result["recalled"]), len(said))
            return result
        out = await asyncio.to_thread(memory.forget_preview, query)
        if not out.get("ok"):
            return {"ok": False, "line": str(out.get("reason") or "That didn't work.")}
        total = int(out.get("total") or 0)
        line = ("nothing in memory matches" if not total
                else f"{total} {'thing' if total == 1 else 'things'} match, in {_layers(out.get('layers'))}")
        log.info("link memory: forget preview → %d", total)
        return {"ok": True, "total": total, "layers": dict(out.get("layers") or {}),
                "token": str(out.get("token") or ""), "expires_in": int(out.get("expires_in") or 0), "line": line}

    @staticmethod
    def _forgotten(out: dict[str, Any]) -> dict[str, Any]:
        total = int(out.get("total") or 0)
        if not out.get("ok"):
            return {"ok": False, "total": total, "layers": dict(out.get("layers") or {}),
                    "line": str(out.get("reason") or "That didn't work.")}
        log.info("link memory: forgot %d", total)
        return {"ok": True, "total": total, "layers": dict(out.get("layers") or {}),
                "line": f"Forgot {total} {'thing' if total == 1 else 'things'}." if total else "Nothing to forget."}


def _layers(layers: Any) -> str:
    if not isinstance(layers, dict):
        return "memory"
    names = {"records": "facts", "index": "meanings", "transcripts": "what was said", "chat_log": "the chat",
             "archive": "notes", "meetings": "meetings"}
    parts = [names.get(k, k) for k, v in layers.items() if v]
    return ", ".join(parts) if parts else "memory"
