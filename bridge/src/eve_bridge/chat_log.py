"""The chat log: the last messages of each Telegram chat, kept on this Mac so a quiet spell or a restart
does not wipe what the chat said.

Until 2026-10-08 the text brain's history lived in RAM only. Ten quiet minutes (``_close_chat``) or a
restart emptied it, and with memory off (the default, ``transcripts.MEMORY_DEFAULT``) nothing else held the
words. Asked "before that", the model had nothing earlier to read and repeated the newest message as if it
were the one before. Work handed to an agent (a computer task, Codex, Claude Code) was never in the history
either, so "did you tell the agent?" had nothing to answer from.

This log is the fix, on every install, memory on or off:

- what it keeps: per chat, the owner's and eve's messages, and one short note per hand-off to an agent and
  its outcome (``who="event"``), each with its time and the run (``boot``) that wrote it;
- where: ``~/.config/eve-bridge/chat/log.json`` (``EVE_CHAT_LOG_FILE`` moves it), the folder 0700
  and the file 0600, refused in a git work tree or a synced folder (the transcripts' guards);
- how long: the newest ``EVE_CHAT_LOG_LINES`` (300) lines per chat, none older than
  ``EVE_CHAT_LOG_DAYS`` (7) days, pruned on every write and at start;
- how it is written: the whole file, atomically (a 0600 temp file in the same folder, fsync, rename), so a
  crash leaves the old log or the new one, never half of one;
- how it is forgotten: ``forget`` drops every line that matches (memory.Memory.forget_apply calls it beside
  the transcripts, in the daemon and in ``eve-bridge memory forget``), and ``clear`` empties a chat. A
  process that finds the file rewritten by another (the CLI's forget while the daemon runs) reads it again
  before its next read or write, so a forget is never undone by the daemon's next save;
- seeded once from the transcripts: an install that had memory on before the log existed starts with an empty
  log, so "before that" had nothing from before its first boot though the transcripts held it (live, 2026-10-08
  evening). ``seed`` copies a chat's earlier Telegram-channel say/image lines (the Eve Link app's turns ride
  that channel too) into the log once, oldest first, inside the log's own bounds, and marks the chat seeded in
  the file so it never runs twice. The seeded lines are ordinary lines (forget reaches them like any other);
- which owner message a recall answer quoted: an eve line can carry ``quotes``, the seq of the owner line it
  quoted (telegram.py tags it when the turn called recent_messages), so "before that" after it is a lookup;
- off: ``EVE_CHAT_LOG=0``. Then no file is written or read, and the chat behaves as before: its history
  is RAM only and is cleared after ten quiet minutes.

The model reads it two ways (telegram.py): a window of the newest lines is its history every turn, with a gap
marker where time passed, and the ``recent_messages`` tool returns earlier lines word for word.

Logs never carry what was said: counts and reasons only. Nothing here leaves the machine.
"""

from __future__ import annotations

import json
import logging
import os
import re
import secrets
import threading
import time
from dataclasses import asdict, dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any, Callable, Optional

from . import tool_gate, transcripts

log = logging.getLogger(__name__)

CHAT_LOG_DEFAULT = True                     # on for every install (2026-10-08); EVE_CHAT_LOG=0 turns it off
DEFAULT_FILE = "~/.config/eve-bridge/chat/log.json"
MAX_LINES = 300                             # per chat
MAX_DAYS = 7.0
TEXT_CHARS = 4000                           # one message on disk, at most
NOTE_CHARS = 200                            # one hand-off note, at most
VERSION = 1
WHO = ("user", "eve", "event")
SEED_BOOT = "seed"                          # the run name of a line copied from the transcripts (``seed``)
SEED_CHANNEL = "telegram"                   # the transcript channel this chat's lines are on (the app's too)
SEED_KINDS = ("say", "image")               # the transcript kinds a chat's history shows
SEED_WHO = {"owner": "user", "eve": "eve"}

_LINK = re.compile(r"https?://\S+|www\.\S+", re.I)


def _off(raw: Any) -> bool:
    return str(raw or "").strip().lower() in ("0", "false", "no", "off")


def _number(raw: Any, default: float, low: float) -> float:
    try:
        value = float(str(raw).strip())
    except (TypeError, ValueError):
        return default
    return value if value >= low else default


@dataclass(frozen=True)
class ChatLogConfig:
    enabled: bool = CHAT_LOG_DEFAULT
    path: Path = Path(DEFAULT_FILE).expanduser()
    max_lines: int = MAX_LINES
    max_days: float = MAX_DAYS


def configured(environ: Any = None) -> ChatLogConfig:
    """``EVE_CHAT_LOG=0`` turns the log off (it is on by default). ``EVE_CHAT_LOG_FILE`` moves the file,
    ``EVE_CHAT_LOG_LINES`` and ``EVE_CHAT_LOG_DAYS`` bound it."""
    env = os.environ if environ is None else environ
    raw = env.get("EVE_CHAT_LOG")
    enabled = CHAT_LOG_DEFAULT if raw is None or not str(raw).strip() else not _off(raw)
    path = Path((env.get("EVE_CHAT_LOG_FILE") or "").strip() or DEFAULT_FILE).expanduser()
    return ChatLogConfig(enabled=enabled, path=path,
                         max_lines=int(_number(env.get("EVE_CHAT_LOG_LINES"), MAX_LINES, 1)),
                         max_days=_number(env.get("EVE_CHAT_LOG_DAYS"), MAX_DAYS, 0.01))


def note_text(text: Any, cap: int = NOTE_CHARS) -> str:
    """A hand-off note's words: one line, links taken out (the owner has them in the chat; a page's link is not
    eve's to repeat), at most ``cap`` characters. Used for anything an agent or a page wrote."""
    flat = " ".join(_LINK.sub("(link)", str(text or "")).split())
    return flat if len(flat) <= cap else flat[:cap - 1].rstrip() + "…"


@dataclass
class Line:
    seq: int                                  # this line's number in the log, never reused
    t: float                                  # wall clock, seconds
    chat: int
    who: str                                  # "user", "eve" or "event" (a hand-off note)
    text: str
    kind: str = ""                            # the transcript kind it was written as ("" when not written there)
    conv: str = ""                            # the transcript conversation it was written in ("" when none)
    boot: str = ""                            # the run of the daemon that wrote it
    key: str = ""                             # a note's thread: a later note with the same key replaces it
    quotes: int = 0                           # an eve line: the seq of the owner line it quoted (0: none known)
    outside: bool = False                     # eve wrote it from words it did not write (tool_gate.py)
    extra: dict[str, Any] = field(default_factory=dict)


class ChatLog:
    """The log, loaded once and kept in RAM; every change rewrites the file atomically. Thread-safe; never
    raises from a write (a disk that refuses costs the persistence, never the chat)."""

    def __init__(self, path: Path, *, max_lines: int = MAX_LINES, max_days: float = MAX_DAYS,
                 wall: Callable[[], float] = time.time) -> None:
        self.path = Path(path).expanduser()
        self.max_lines = max(1, int(max_lines))
        self.max_days = float(max_days)
        self._wall = wall
        self._lock = threading.Lock()
        self._lines: list[Line] = []
        self._seq = 0
        self._seeded: set[int] = set()            # chats already seeded from the transcripts (kept in the file)
        self.boot = secrets.token_hex(4)          # this run; a line from another run was before a restart
        self.generation = 0                       # moves on forget and clear: a reader's cached window is stale
        self.refused = ""                         # why the folder may not hold the log, or ""
        self._failed = 0
        self._stamp: Optional[tuple[int, int, int]] = None   # the file as this process last read or wrote it
        self._load()

    # -- reading --
    def lines(self, chat: int) -> list[Line]:
        """This chat's lines, oldest first, within the bounds."""
        with self._lock:
            self._refresh()
            self._prune()
            return [ln for ln in self._lines if ln.chat == chat]

    def count(self, pred: Callable[[str], bool]) -> int:
        with self._lock:
            self._refresh()
            return sum(1 for ln in self._lines if pred(ln.text))

    # -- writing --
    def append(self, chat: int, who: str, text: str, *, kind: str = "", conv: str = "", key: str = "",
               at: Optional[float] = None, quotes: int = 0, outside: bool = False) -> Optional[Line]:
        """One line → the line kept (None for empty text or an unknown speaker). A note whose ``key`` is the
        chat's newest line's key replaces that line, so a run of one agent's answers is one note. ``outside``: the
        line carries words eve did not write; it is kept so, and marked in tool_gate.MARKS on every load, so a
        restart does not launder it into eve's own words (v3 safety review)."""
        if who not in WHO:
            return None
        text = str(text or "").strip()
        if not text:
            return None
        if len(text) > TEXT_CHARS:
            text = text[:TEXT_CHARS - 1] + "…"
        with self._lock:
            self._refresh()
            now = self._wall() if at is None else float(at)
            mine = [ln for ln in self._lines if ln.chat == chat]
            if key and mine and mine[-1].key == key:
                last = mine[-1]
                last.text, last.t, last.boot = text, now, self.boot
                last.outside = last.outside or outside
                line = last
            else:
                self._seq += 1
                line = Line(self._seq, now, chat, who, text, kind, conv or "", self.boot, key, int(quotes or 0),
                            bool(outside))
                self._lines.append(line)
            self._prune()
            self._save()
            return line

    def seed(self, chat: int, store: Any) -> int:
        """Copy this chat's earlier lines from the transcripts (``store``: transcripts.Transcripts) into the log, once
        → lines copied. Only what the log does not have: lines older than the chat's oldest kept line, from
        conversations the log has no line of; the Telegram channel's say/image lines of the owner and eve (the
        Eve Link app's turns are on that channel); none forgotten; within max_days, and only as many as fit
        under max_lines beside the chat's lines, the newest of them. They go in before every kept line, oldest
        first, as run ``SEED_BOOT``. The chat is marked seeded in the file, so this never runs twice, even after a
        forget empties the chat. Never raises."""
        if store is None or not getattr(store, "enabled", False):
            return 0
        with self._lock:
            self._refresh()
            if chat in self._seeded:
                return 0
        cutoff = self._wall() - self.max_days * 86400.0
        rows: list[tuple[float, str, str, str, str]] = []
        try:
            first = store.day_of(datetime.fromtimestamp(cutoff).astimezone())
            for day in store.days():
                if day < first:
                    continue
                for row in store.lines(day):
                    who = SEED_WHO.get(row.get("who"))
                    text = str(row.get("text") or "").strip()
                    if (row.get("ch") != SEED_CHANNEL or row.get("kind") not in SEED_KINDS or who is None
                            or not text or text == transcripts.FORGOTTEN):
                        continue
                    at = transcripts._parse_ts(row.get("ts", ""))
                    if at is not None and at.timestamp() >= cutoff:
                        rows.append((at.timestamp(), who, text, str(row["kind"]), str(row.get("conv") or ""),
                                     bool(row.get("out"))))
        except Exception as e:  # noqa: BLE001 — a transcript that cannot be read costs the seed, never the chat
            log.warning("chat log: not seeded from the transcripts (%s)", type(e).__name__)
            return 0
        rows.sort(key=lambda r: r[0])
        with self._lock:
            self._refresh()
            if chat in self._seeded:
                return 0
            mine = [ln for ln in self._lines if ln.chat == chat]
            oldest = min((ln.t for ln in mine), default=float("inf"))
            convs = {ln.conv for ln in mine if ln.conv}
            picked = [r for r in rows if r[0] < oldest and r[4] not in convs]
            room = max(0, self.max_lines - len(mine))
            picked = picked[len(picked) - room:] if room else []
            # numbered before every kept line: the log's order is its seq order. Every number moves up by the count,
            # so none is reused; a reader's cached window is stale (generation)
            k = len(picked)
            for ln in self._lines:
                ln.seq += k
                if ln.quotes:                    # the owner line it quoted moved up by k too (0: none known)
                    ln.quotes += k
            new = [Line(i + 1, t, chat, who, text[:TEXT_CHARS], kind, conv, SEED_BOOT, outside=out)
                   for i, (t, who, text, kind, conv, out) in enumerate(picked)]
            self._lines = new + self._lines
            self._seq += k
            self._seeded.add(chat)
            self.generation += 1
            self._prune()
            self._save()
        log.info("chat log: %d lines seeded from the transcripts", k)
        return k

    def forget(self, pred: Callable[[str], bool]) -> int:
        """Drop every line whose text matches → lines dropped. The file is rewritten at once."""
        with self._lock:
            self._refresh()
            before = len(self._lines)
            self._lines = [ln for ln in self._lines if not pred(ln.text)]
            dropped = before - len(self._lines)
            if dropped:
                self.generation += 1
                self._save()
        if dropped:
            log.info("chat log: forgot %d lines", dropped)
        return dropped

    def clear(self, chat: Optional[int] = None) -> int:
        """Empty one chat (or all of them) → lines dropped."""
        with self._lock:
            self._refresh()
            before = len(self._lines)
            self._lines = [] if chat is None else [ln for ln in self._lines if ln.chat != chat]
            dropped = before - len(self._lines)
            if dropped:
                self.generation += 1
                self._save()
        return dropped

    # -- bounds --
    def _prune(self) -> None:
        """Older than max_days, or past max_lines in its chat: gone (from RAM here, from disk on the next save)."""
        cutoff = self._wall() - self.max_days * 86400.0
        kept = [ln for ln in self._lines if ln.t >= cutoff]
        per_chat: dict[int, int] = {}
        for ln in reversed(kept):
            per_chat[ln.chat] = per_chat.get(ln.chat, 0) + 1
        if any(n > self.max_lines for n in per_chat.values()):
            seen: dict[int, int] = {}
            out: list[Line] = []
            for ln in reversed(kept):
                seen[ln.chat] = seen.get(ln.chat, 0) + 1
                if seen[ln.chat] <= self.max_lines:
                    out.append(ln)
            kept = list(reversed(out))
        if len(kept) != len(self._lines):
            self._lines = kept

    # -- disk --
    def _file_stamp(self) -> Optional[tuple[int, int, int]]:
        try:
            st = self.path.stat()
        except OSError:
            return None
        return (st.st_ino, st.st_mtime_ns, st.st_size)

    def _refresh(self) -> None:
        """Under the lock: when another process rewrote the file since this one last read or wrote it (the CLI's
        ``memory forget`` while the daemon runs), read it again, so the next save does not bring back what that
        process removed (review, 2026-10-08). One stat per call; nothing when the file is unchanged."""
        if self.refused:
            return
        stamp = self._file_stamp()
        if stamp == self._stamp:
            return
        seq, self._lines, self._seeded = self._seq, [], set()
        self._read()
        self._seq = max(seq, self._seq)           # a number is never reused, even one the other process dropped
        self.generation += 1                      # a reader's cached window is stale
        self._prune()

    def _load(self) -> None:
        self.refused = transcripts.refusal(self.path.parent) if self.path.parent.exists() else ""
        if self.refused:
            log.warning("chat log: not kept on disk — %s", self.refused)
            return
        with self._lock:
            self._read()
            self._prune()
        log.info("chat log: %d lines loaded", len(self._lines))

    def _read(self) -> None:
        """The file into self._lines (appended, sorted by seq) and its stamp remembered. Never raises."""
        self._stamp = self._file_stamp()
        try:
            raw = self.path.read_bytes()
        except FileNotFoundError:
            return
        except OSError as e:
            log.warning("chat log: could not be read (%s); starting empty", type(e).__name__)
            return
        try:
            doc = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            log.warning("chat log: the file is not readable JSON; starting empty")
            return
        rows = doc.get("lines") if isinstance(doc, dict) else None
        seeded = doc.get("seeded") if isinstance(doc, dict) else None
        for chat in seeded if isinstance(seeded, list) else []:
            if isinstance(chat, int) and not isinstance(chat, bool):
                self._seeded.add(chat)
        for row in rows if isinstance(rows, list) else []:
            line = _parse(row)
            if line is not None:
                self._lines.append(line)
        self._lines.sort(key=lambda ln: ln.seq)
        self._seq = max((ln.seq for ln in self._lines), default=0)

    def _save(self) -> None:
        if self.refused:
            return
        folder = self.path.parent
        if not folder.exists():
            why = transcripts._prepare(folder)
            if why:
                self.refused = why
                log.warning("chat log: not kept on disk — %s", why)
                return
        data = json.dumps({"version": VERSION, "seeded": sorted(self._seeded),
                           "lines": [asdict(ln) for ln in self._lines]},
                          ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        tmp = folder / f".{self.path.name}.{os.getpid()}.{secrets.token_hex(4)}.tmp"
        try:
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
            try:
                os.fchmod(fd, 0o600)
                view = memoryview(data)
                while view:
                    view = view[os.write(fd, view):]
                os.fsync(fd)
            finally:
                os.close(fd)
            os.replace(tmp, self.path)
            self._stamp = self._file_stamp()
        except OSError as e:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            self._failed += 1
            if self._failed == 1 or self._failed % 100 == 0:
                log.warning("chat log: not written (%s); %d failed writes", type(e).__name__, self._failed)


def _parse(row: Any) -> Optional[Line]:
    if not isinstance(row, dict):
        return None
    try:
        line = Line(seq=int(row["seq"]), t=float(row["t"]), chat=int(row["chat"]), who=str(row["who"]),
                    text=str(row["text"]), kind=str(row.get("kind") or ""), conv=str(row.get("conv") or ""),
                    boot=str(row.get("boot") or ""), key=str(row.get("key") or ""),
                    quotes=int(row.get("quotes") or 0), outside=bool(row.get("outside")))
    except (KeyError, TypeError, ValueError):
        return None
    if line.outside:
        tool_gate.MARKS.add(line.text)              # outside words read back from the file: marked again
    return line if line.who in WHO and line.text else None


def open_configured(environ: Any = None) -> Optional[ChatLog]:
    """The daemon's log, or None when ``EVE_CHAT_LOG=0``."""
    cfg = configured(environ)
    if not cfg.enabled:
        log.info("chat log: off (EVE_CHAT_LOG=0) — the chat's history is RAM only and clears when it goes quiet")
        return None
    return ChatLog(cfg.path, max_lines=cfg.max_lines, max_days=cfg.max_days)
