"""Reactions on the owner's messages in the Eve Link app: Telegram's receipts, in the app (docs/app-chat.md).

Owner, 2026-10-09: "can you build in reactions for when it get image or takes not or sees text and things like that
like telegram". In Telegram eve puts a reaction on the owner's own message instead of a one-line acknowledgement
(TelegramInlet._receipt): 👀 an image is being read, ✍ a note was kept, 🏆 a fact was starred, 👍 it was typed to
Claude or Codex. An app turn had no Telegram message to put it on (message id 0), so the receipt fell back to a line.

This module gives an app turn's owner message an id a reaction can land on, and keeps the reactions:

* ``mint(key)``: the int the inlet uses as that message's ``Inbound.message_id``. Always negative, so it can never be
  a Telegram message id (those are positive) and is never sent to Telegram. ``key`` is the app turn's id
  (link_chat: the app's ``client_id`` when it is a plain id, else a random one), which the owner's transcript line
  keeps as ``mid``; ``/api/link/messages`` gives that row ``rid`` and its ``reaction``.
* ``set(message_id, emoji)``: Telegram's semantics exactly. A message holds at most one reaction from eve, a new
  one replaces the old, and "" takes it off. An id this store did not mint (or has since let go of) raises
  ``KeyError``, and the inlet then says the receipt's line instead, as it does when Telegram refuses a reaction.
* ``version``: changes on every set, so a long poll can tell the app has an old picture. It starts at the wall
  clock in milliseconds, so a restart never reuses a version an app is still holding.

Kept in ``path`` (JSON, 0600, the newest ``MAX_KEPT`` reactions) when given, so history shows them after a restart;
in memory only without one. A ``TRANSIENT`` reaction (👀, "reading this now") is never kept in the file: after a
crash nothing could take it off. Every listener in ``listeners`` is called after a change (link_chat wakes its long polls),
and one that raises is ignored. Logs carry the emoji and counts, never what was said.
"""
from __future__ import annotations

import json
import logging
import os
import re
import threading
import time
import unicodedata
from pathlib import Path
from typing import Any, Callable, Optional

log = logging.getLogger(__name__)

MAX_KEPT = 200                 # reactions kept (and sent to the app on every poll); the oldest go first
MAX_MINTED = 512               # message ids remembered; the oldest go first
MAX_EMOJI_CHARS = 16           # UTF-16 units: one emoji, a ZWJ sequence with modifiers, fits in this
KEY_RE = re.compile(r"^[A-Za-z0-9_-]{1,64}$")
FILE_NAME = "link-reactions.json"
# Reactions that mean "eve is working on this now" (👀: an image is being read). They live in memory only: after a
# crash or a forced kill the turn that would take one off is gone, and the ids that could reach it are not kept, so a
# saved one would stay on the message for good (review, 2026-10-09). Never written to the file, dropped when read.
TRANSIENT = frozenset({"\U0001F440"})
# An emoji's characters: symbols (So), skin-tone modifiers (Sk), the emoji variation selector (Mn), a keycap (Me), the
# zero-width joiner (Cf). Letters, digits, punctuation, spaces and control characters are not among them.
_EMOJI_CATEGORIES = ("So", "Sk", "Mn", "Me", "Cf")


def clean(emoji: Any) -> str:
    """``emoji`` when it is a short emoji string, else "". Anything ASCII, a space or a control character makes it
    "": a reaction is never words (and never markup) wherever it is drawn."""
    # counted in UTF-16 units, as the app's page counts it (reactions.ts MAX_REACTION_CHARS)
    if not isinstance(emoji, str) or not 0 < len(emoji.encode("utf-16-le")) // 2 <= MAX_EMOJI_CHARS:
        return ""
    for ch in emoji:
        if ord(ch) < 0x2000 or unicodedata.category(ch) not in _EMOJI_CATEGORIES:
            return ""
    return emoji


class Reactions:
    """The app's reactions, by message key. Safe to call from any thread."""

    def __init__(self, path: Optional[Path] = None, *, clock: Callable[[], float] = time.time) -> None:
        self._lock = threading.Lock()
        self._path: Optional[Path] = None
        self._by_key: dict[str, str] = {}          # key -> emoji, oldest set first
        self._minted: dict[int, str] = {}          # message id -> key, oldest first
        self._next = -1
        self.version = int(clock() * 1000)
        self.listeners: list[Callable[[str], None]] = []
        if path is not None:
            self.attach(path)

    # -- where they are kept --
    def attach(self, path: Path) -> None:
        """Keep the reactions in ``path`` from now on, loading what it holds (once; a second call is ignored)."""
        with self._lock:
            if self._path is not None:
                return
            self._path = Path(path)
            loaded = self._load(self._path)
            loaded.update(self._by_key)                 # a reaction set before the file was found wins
            self._by_key = dict(list(loaded.items())[-MAX_KEPT:])

    @staticmethod
    def _load(path: Path) -> dict[str, str]:
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
        except FileNotFoundError:
            return {}
        except (OSError, ValueError) as e:
            log.warning("link reactions: %s not read (%s); starting empty", path.name, type(e).__name__)
            return {}
        if not isinstance(raw, dict):
            return {}
        out: dict[str, str] = {}
        for key, emoji in raw.items():
            if isinstance(key, str) and KEY_RE.match(key) and clean(emoji) and emoji not in TRANSIENT:
                out[key] = emoji
        return out

    def _save(self) -> None:
        """Write the file (under the lock). Atomic: a crash leaves the old file or the new one, never half."""
        if self._path is None:
            return
        tmp = self._path.with_name(self._path.name + ".tmp")
        try:
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                json.dump({k: e for k, e in self._by_key.items() if e not in TRANSIENT}, f, ensure_ascii=False)
            os.replace(tmp, self._path)
        except OSError as e:
            log.warning("link reactions: not saved (%s); kept in memory", type(e).__name__)

    # -- ids --
    def mint(self, key: str) -> int:
        """The negative message id for the app message ``key`` (the same id for the same key while remembered)."""
        if not KEY_RE.match(key or ""):
            raise ValueError("bad message key")
        with self._lock:
            for mid, k in self._minted.items():
                if k == key:
                    return mid
            mid, self._next = self._next, self._next - 1
            self._minted[mid] = key
            while len(self._minted) > MAX_MINTED:
                del self._minted[next(iter(self._minted))]
            return mid

    @staticmethod
    def is_app(message_id: int) -> bool:
        """An id this kind of store mints (negative), never a Telegram message's."""
        return message_id < 0

    # -- reactions --
    def set(self, message_id: int, emoji: str) -> None:
        """Put ``emoji`` on the message (replacing eve's reaction there), or take it off with "". KeyError: not a
        message this store knows; ValueError: not an emoji."""
        if emoji and not clean(emoji):
            raise ValueError("not an emoji")
        with self._lock:
            key = self._minted.get(message_id)
            if key is None:
                raise KeyError("no such app message")
            had = self._by_key.pop(key, "")
            if emoji:
                self._by_key[key] = emoji               # newest last
                while len(self._by_key) > MAX_KEPT:
                    del self._by_key[next(iter(self._by_key))]
            if had == emoji:
                return                                  # no change: nobody to wake
            self.version += 1
            self._save()
        log.info("link reactions: %s", emoji or "cleared")
        for listener in list(self.listeners):
            try:
                listener(key)
            except Exception as e:  # noqa: BLE001 — a listener never costs the reaction
                log.warning("link reactions: a listener failed (%s)", type(e).__name__)

    def get(self, key: str) -> str:
        with self._lock:
            return self._by_key.get(key, "")

    def snapshot(self) -> dict[str, str]:
        """Every reaction kept, by message key (what ``/api/link/messages`` sends as ``reactions``)."""
        with self._lock:
            return dict(self._by_key)
