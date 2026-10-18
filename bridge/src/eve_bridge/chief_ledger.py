"""The chief of staff's ledger: the cards on disk, their events, and what is due.

Everything lives in one folder, ``~/.config/eve-bridge/chief/`` (``EVE_CHIEF_DIR`` moves it; the tests
point it at a temporary folder), never in the repo, mode 600 for the files and 700 for the folder:

    cards.json     the cards and ``last_id``, written atomically
    events.jsonl   append-only: one line per created, revised, state, asked, answered, approved, dispatched,
                   result, checked, pushed, would_push, critic, reflected, retried, reopened or redacted event

The store is the Watcher's (watch.py:2033-2092), for the same reasons:

* **Atomic.** A temp file in the same folder, then ``os.replace``. A crash or a failed rename mid-write leaves
  the old file whole; the change is not taken in memory either, and the caller gets ``LedgerError``.
* **An unreadable file is moved aside,** as ``cards.json.bad-<ts>``, and the owner is told once
  (``take_problem``). So is a file that parses but is not the ledger's shape (a list, a string, ``cards`` that is
  not a list). When single cards do not load (a hand-edit's typo, a number too big), the rest load, the whole
  file is COPIED aside first and the owner is told which ids: the next save writes only what loaded, and without
  the copy it would erase the rest for good (reviewer, 2026-09-29). A hand-edit's typo never costs a card.
* **Ids are never reused.** ``last_id`` is saved; a bad file's highest ``c<N>`` and every id in the events log
  count too, so an id the log already speaks of is never handed out again, even after the cards file is lost.

The events log is the audit log's shape (audit.py): one JSON object per line, appended, never rewritten except by
``redact``. ``event`` returns whether the line reached the disk (flushed and fsynced): a dispatch or a push is
logged BEFORE the act, and a caller that gets False does not act. That is what lets a restart tell "dispatched,
no result" from "never sent" (design rule 13: an act with a dispatch line and no result never runs again without a
new yes). Event lines carry ids, numbers and short code words, never what the owner said.

State changes (status, a phase's status or result, what was spent) go through ``update`` and ``update_phase``,
which only touch the state fields. What the owner agreed to changes only through ``revise`` (chief_card.revise:
rev+1, the door add-only, approvals cancelled).

Forget does not reach this folder yet: memory.NOT_COVERED names it, and ``redact`` is the helper a forget layer
will call.
"""

from __future__ import annotations

import dataclasses
import json
import logging
import os
import re
import shutil
import tempfile
import threading
import time
from datetime import datetime
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Optional

from . import chief_card
from .chief_card import (
    LIVE,
    PHASE_STATUSES,
    STATUSES,
    WAITING_FOR,
    Approval,
    Budget,
    Card,
    Live,
    Nudges,
    Phase,
    PhaseResult,
    Refusal,
    Spent,
)

log = logging.getLogger(__name__)

DEFAULT_DIR = "~/.config/eve-bridge/chief"
CARDS_FILE = "cards.json"
EVENTS_FILE = "events.jsonl"
EVENT_KINDS = ("created", "revised", "state", "asked", "answered", "approved", "dispatched", "result", "checked",
               "pushed", "would_push", "critic", "reflected", "not_reflected", "retried", "reopened",
               "redacted", "replanned", "would_replan", "not_replanned")
STATE_FIELDS = ("status", "waiting_for", "spent", "budget", "nudges")
PHASE_STATE_FIELDS = ("status", "executor", "approval", "result")
MAX_CARDS = 500                      # the store's cap; the oldest finished cards give way first
TURN_LINE_CHARS = 300                # for_turn: the line the brain sees every turn
MAX_EVENT_TEXT = 200
_ID_IN_TEXT = re.compile(r'"c([1-9]\d{0,8})"')
_WORD = re.compile(r"[a-z0-9]+")
_STOP = frozenset({"the", "and", "for", "with", "that", "this", "from", "into", "about", "your", "have", "will",
                   "what", "when", "then", "them", "they", "just", "some", "please", "eve", "need", "want"})
_WAITING_WORDS = {"go": "Go", "question": "answer", "budget": "budget raise"}
_DOING = {"research": "researching", "assess": "weighing options", "act": "acting", "watch": "setting a watch"}


class LedgerError(RuntimeError):
    """The ledger could not save a change. Nothing changed, on disk or in memory; the caller falls back."""


def chief_dir(environ: Optional[Mapping[str, str]] = None) -> Path:
    env = os.environ if environ is None else environ
    raw = (env.get("EVE_CHIEF_DIR") or "").strip()
    return Path(raw or DEFAULT_DIR).expanduser()


def _content_words(text: str) -> set[str]:
    return {w for w in _WORD.findall((text or "").casefold()) if len(w) >= 4 and w not in _STOP}


def _day(ts: float) -> str:
    return datetime.fromtimestamp(ts).date().isoformat()


def _scalar(value: Any) -> Any:
    if value is None or isinstance(value, (bool, int, float)):
        return value
    return " ".join(str(value).split())[:MAX_EVENT_TEXT]


class JsonlRows:
    """The dict rows of an append-only JSONL file, parsed once each.

    The chief reads its events (and the desk its attention log) many times a tick, and parsing the whole
    file each time grew with every line ever written. This keeps the rows parsed so far and the byte offset
    they end at, and reads only what was appended since. A file that was replaced (another inode, as
    `Ledger.redact` does with os.replace) or shrank is read afresh. A last line without its newline yet (a
    write in flight) is parsed for this answer but read again next time. A torn or non-dict line is skipped."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._key: Optional[tuple[int, int]] = None
        self._offset = 0
        self._rows: list[dict[str, Any]] = []

    @staticmethod
    def _parse(chunk: bytes) -> list[dict[str, Any]]:
        out = []
        for line in chunk.split(b"\n"):
            if not line.strip():
                continue
            try:
                row = json.loads(line.decode("utf-8", errors="replace"))
            except ValueError:
                continue
            if isinstance(row, dict):
                out.append(row)
        return out

    def rows(self, path: Path) -> list[dict[str, Any]]:
        with self._lock:
            try:
                with open(path, "rb") as f:
                    st = os.fstat(f.fileno())
                    key = (st.st_dev, st.st_ino)
                    if key != self._key or st.st_size < self._offset:
                        self._key, self._offset, self._rows = key, 0, []
                    f.seek(self._offset)
                    data = f.read()
            except OSError:
                self._key, self._offset, self._rows = None, 0, []
                return []
            end = data.rfind(b"\n") + 1
            if end:
                self._rows.extend(self._parse(data[:end]))
                self._offset += end
            tail = self._parse(data[end:]) if end < len(data) and data[end:].strip() else []
            return [dict(r) for r in self._rows] + tail


class Ledger:
    """The cards and their events. Thread-safe: the chief's loop and the Telegram turn share one."""

    def __init__(self, folder: Optional[Path] = None, *, clock: Callable[[], float] = time.time) -> None:
        self.dir = Path(folder).expanduser() if folder is not None else chief_dir()
        self._clock = clock
        self._lock = threading.RLock()
        self._last_id = 0
        self._problem = ""
        self._event_rows = JsonlRows()          # events.jsonl, parsed once per line (events())
        self._cards: dict[str, Card] = self._load()

    @property
    def cards_path(self) -> Path:
        return self.dir / CARDS_FILE

    @property
    def events_path(self) -> Path:
        return self.dir / EVENTS_FILE

    # -- the store --
    def _ids_in_events(self) -> int:
        try:
            text = self.events_path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return 0
        return max((int(x) for x in re.findall(r'"id":\s*"c([1-9]\d{0,8})"', text)), default=0)

    def _aside(self, text: str, *, move: bool) -> Path:
        """The cards file kept as cards.json.bad-<ts>: moved (nothing in it is used) or copied (some cards did
        not load; the next save would erase them). Returns where it is, or the file itself when that failed."""
        path = self.cards_path
        aside = path.with_name(f"{path.name}.bad-{int(self._clock())}")
        self._last_id = max(self._last_id, max((int(x) for x in _ID_IN_TEXT.findall(text)), default=0))
        try:
            if move:
                path.rename(aside)
            else:
                shutil.copyfile(path, aside)
                os.chmod(aside, 0o600)
        except OSError:
            return path
        return aside

    def _load(self) -> dict[str, Card]:
        cards: dict[str, Card] = {}
        path = self.cards_path
        raw: Any = None
        bad = ""
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
            if not (isinstance(raw, dict) and isinstance(raw.get("cards", []), list)):
                bad = "not a list of cards"
        except FileNotFoundError:
            raw = None
        except (OSError, ValueError) as e:                  # unreadable, undecodable or not JSON
            bad = type(e).__name__
        if bad:
            # nothing in it is used, so it is moved aside whole
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                text = ""
            aside = self._aside(text, move=True)
            log.warning("chief: %s is unreadable (%s); kept as %s, starting with no cards", path, bad, aside.name)
            self._problem = (f"I couldn't read my list of jobs, so I'm starting with none. The old file is kept as "
                             f"{aside.name} in {aside.parent}.")
            raw = None
        if isinstance(raw, dict):
            last = raw.get("last_id")
            if isinstance(last, int) and not isinstance(last, bool) and 0 < last < 10**9:
                self._last_id = last
            skipped: list[str] = []
            for i, item in enumerate(raw.get("cards") or []):
                try:
                    card = chief_card.from_json(item)
                except Exception:  # noqa: BLE001 — any card that does not load is set aside, never the whole list
                    cid = item.get("id") if isinstance(item, dict) else None
                    skipped.append(cid if isinstance(cid, str) and chief_card.card_number(cid) else f"#{i + 1}")
                    continue
                if card.id not in cards:                     # the first of two same ids wins
                    cards[card.id] = card
                    self._last_id = max(self._last_id, chief_card.card_number(card.id))
            if skipped:
                try:
                    text = path.read_text(encoding="utf-8", errors="replace")
                except OSError:
                    text = ""
                aside = self._aside(text, move=False)
                log.warning("chief: %d stored card(s) did not load (%s); the file is kept as %s", len(skipped),
                            ", ".join(skipped[:10]), aside.name)
                n = len(skipped)
                self._problem = (f"{n} job{'s' if n != 1 else ''} in my list didn't load "
                                 f"({', '.join(skipped[:10])}), so I set {'them' if n != 1 else 'it'} aside. The whole "
                                 f"file is kept as {aside.name} in {aside.parent}.")
        self._last_id = max(self._last_id, self._ids_in_events())
        return cards

    def _save(self, cards: Mapping[str, Card], last_id: int) -> None:
        """Atomic, as the Watcher saves. Raises LedgerError, and the old file stays whole."""
        path = self.cards_path
        try:
            self.dir.mkdir(mode=0o700, parents=True, exist_ok=True)
            fd, tmp = tempfile.mkstemp(prefix=".cards-", suffix=".json", dir=str(self.dir))
            try:
                with os.fdopen(fd, "w", encoding="utf-8") as f:
                    json.dump({"version": 1, "last_id": last_id,
                               "cards": [chief_card.to_json(c) for c in cards.values()]}, f, indent=1)
                os.chmod(tmp, 0o600)
                os.replace(tmp, path)
            except BaseException:
                try:
                    os.unlink(tmp)
                except OSError:
                    pass
                raise
        except OSError as e:
            log.warning("chief: could not save the cards (%s)", type(e).__name__)
            raise LedgerError(f"could not save the cards ({type(e).__name__})") from e

    def _commit(self, cards: dict[str, Card], last_id: Optional[int] = None) -> None:
        last = self._last_id if last_id is None else last_id
        self._save(cards, last)
        self._cards, self._last_id = cards, last

    def take_problem(self) -> str:
        """What went wrong at load, said once."""
        with self._lock:
            problem, self._problem = self._problem, ""
            return problem

    # -- events --
    def event(self, kind: str, cid: str, /, **fields: Any) -> bool:
        """One line appended to events.jsonl, flushed to disk. True when it is there: a caller logs a dispatch
        or a push BEFORE the act, and does not act on False. Never raises for IO."""
        if kind not in EVENT_KINDS:
            raise ValueError(f"unknown event kind {kind!r}")
        row: dict[str, Any] = {"t": round(self._clock(), 3), "e": kind, "id": str(cid)}
        for key, value in fields.items():
            if key in row:
                continue
            row[key] = [_scalar(v) for v in value][:20] if isinstance(value, (list, tuple)) else _scalar(value)
        line = json.dumps(row, separators=(",", ":"), ensure_ascii=False) + "\n"
        with self._lock:
            try:
                self.dir.mkdir(mode=0o700, parents=True, exist_ok=True)
                fd = os.open(self.events_path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
                try:
                    os.write(fd, line.encode("utf-8"))
                    os.fsync(fd)
                finally:
                    os.close(fd)
                return True
            except OSError as e:
                log.warning("chief: an event line was not written (%s)", type(e).__name__)
                return False

    def events(self, cid: Optional[str] = None) -> list[dict[str, Any]]:
        """The events, oldest first (one card's with ``cid``). A torn or foreign line is skipped."""
        return [row for row in self._event_rows.rows(self.events_path)
                if row.get("e") in EVENT_KINDS and (cid is None or row.get("id") == cid)]

    # -- cards --
    def get(self, cid: str) -> Optional[Card]:
        with self._lock:
            return self._cards.get(cid)

    def all(self) -> list[Card]:
        with self._lock:
            return sorted(self._cards.values(), key=lambda c: chief_card.card_number(c.id))

    def open(self) -> list[Card]:
        """The cards that are not done or dropped, oldest first."""
        return [c for c in self.all() if not c.terminal]

    def new(self, card: Card, *, why: str = "take_on") -> Card:
        """File a parsed card under the next id (never one used before) and log ``created``."""
        if card.id:
            raise ValueError("a new card has no id yet")
        with self._lock:
            now = self._clock()
            n = self._last_id + 1
            while f"c{n}" in self._cards:
                n += 1
            filed = dataclasses.replace(card, id=f"c{n}", created=now, updated=now)
            cards = dict(self._cards)
            cards[filed.id] = filed
            self._commit(self._trim(cards), n)
            self.event("created", filed.id, rev=filed.rev, kind=filed.kind, status=filed.status,
                       phases=[p.do for p in filed.phases], doors=[p.door for p in filed.phases], why=why)
            return filed

    def _trim(self, cards: dict[str, Card]) -> dict[str, Card]:
        if len(cards) <= MAX_CARDS:
            return cards
        finished = sorted((c for c in cards.values() if c.terminal), key=lambda c: c.updated)
        out = dict(cards)
        for c in finished[:len(cards) - MAX_CARDS]:
            out.pop(c.id)
        return out

    def _put(self, card: Card) -> Card:
        cards = dict(self._cards)
        cards[card.id] = card
        self._commit(cards)
        return card

    def revise(self, cid: str, patch: Mapping[str, Any], why: str, *, said: str, live: Live = LIVE,
               home: Optional[Path] = None) -> Card | Refusal:
        """The owner's correction as a new revision (chief_card.revise), saved and logged with its reason."""
        with self._lock:
            card = self._cards.get(cid)
            if card is None:
                return Refusal(f"there is no card {cid}")
            new = chief_card.revise(card, patch, said=said, live=live, now=self._clock(), home=home)
            if isinstance(new, Refusal):
                return new
            self._put(new)
            self.event("revised", cid, rev=new.rev, why=why, doors=[p.door for p in new.phases])
            return new

    def replan(self, cid: str, *, expect: Card, phases: tuple[Phase, ...], **event_fields: Any) -> Optional[Card]:
        """The replanner's new phases (chief_replan.apply), as one new revision, under the lock, or not at all:
        the card must still be exactly ``expect`` (what the replan was asked about; an owner's drop or change
        meanwhile wins). rev+1 cancels every approval, as the owner's own revision does. None when it changed."""
        with self._lock:
            card = self._cards.get(cid)
            if card is None or card != expect or card.terminal:
                return None
            new = self._put(dataclasses.replace(card, rev=card.rev + 1, updated=self._clock(),
                                                phases=tuple(dataclasses.replace(p, approval=None) for p in phases)))
            self.event("replanned", cid, rev=new.rev, **event_fields)
            return new

    def reopen(self, cid: str, *, why: str) -> Card:
        with self._lock:
            card = self._need(cid)
            new = self._put(chief_card.reopen(card, now=self._clock()))
            self.event("reopened", cid, rev=new.rev, why=why)
            return new

    def _need(self, cid: str) -> Card:
        card = self._cards.get(cid)
        if card is None:
            raise KeyError(cid)
        return card

    def update(self, cid: str, *, why: str, **fields: Any) -> Card:
        """Change the card's state (``STATE_FIELDS`` only), saved and logged. A done or dropped card is reopened,
        never moved back by a state change."""
        bad = sorted(set(fields) - set(STATE_FIELDS))
        if bad:
            raise ValueError(f"not a state field: {bad[0]}")
        if "status" in fields and fields["status"] not in STATUSES:
            raise ValueError(f"unknown status {fields['status']!r}")
        if "waiting_for" in fields and fields["waiting_for"] not in (None, *WAITING_FOR):
            raise ValueError(f"unknown waiting_for {fields['waiting_for']!r}")
        for name, kind in (("spent", Spent), ("budget", Budget), ("nudges", Nudges)):
            if name in fields and not isinstance(fields[name], kind):
                raise ValueError(f"{name} must be a {kind.__name__}")
        with self._lock:
            card = self._need(cid)
            if card.terminal and fields.get("status", card.status) != card.status:
                raise ValueError(f"{cid} is {card.status}: reopen it instead")
            new = self._put(dataclasses.replace(card, updated=self._clock(), **fields))
            self.event("state", cid, rev=new.rev, status=new.status, waiting_for=new.waiting_for, why=why)
            return new

    def update_phase(self, cid: str, n: int, *, why: str, **fields: Any) -> Card:
        """Change one phase's state (``PHASE_STATE_FIELDS`` only), saved and logged. A done or dropped card's
        phases do not change (ValueError, as ``update``): a step never starts on a finished card."""
        bad = sorted(set(fields) - set(PHASE_STATE_FIELDS))
        if bad:
            raise ValueError(f"not a phase state field: {bad[0]}")
        if "status" in fields and fields["status"] not in PHASE_STATUSES:
            raise ValueError(f"unknown phase status {fields['status']!r}")
        if fields.get("approval") is not None and not isinstance(fields["approval"], Approval):
            raise ValueError("approval must be an Approval or None")
        if fields.get("result") is not None and not isinstance(fields["result"], PhaseResult):
            raise ValueError("result must be a PhaseResult or None")
        with self._lock:
            card = self._need(cid)
            if card.terminal:
                raise ValueError(f"{cid} is {card.status}: reopen it instead")
            phase: Optional[Phase] = card.phase(n)
            if phase is None:
                raise KeyError(f"{cid} has no phase {n}")
            phases = tuple(dataclasses.replace(p, **fields) if p.n == n else p for p in card.phases)
            new = self._put(dataclasses.replace(card, phases=phases, updated=self._clock()))
            self.event("state", cid, rev=new.rev, phase=n, phase_status=new.phase(n).status, why=why)
            return new

    def claim(self, cid: str, n: int, *, rev: int, token: str, what: str, executor: str,
              **event_fields: Any) -> Optional[Card]:
        """Start one phase, all under the lock, or not at all: the card is open and still at ``rev``, the phase is
        queued, and a one-way phase holds the approval ``token`` for this rev whose digest is ``what`` (the act
        as it would run now). Then the ``dispatched`` line is written (fsynced) and the phase set running with
        its approval spent. None when any of that does not hold or the line did not reach the disk; the caller
        then starts nothing. LedgerError when the line is written and the save fails (the caller logs a result).
        The reviewer (2026-09-29) found the dispatcher moving a card from a copy read before an await: a drop or
        a change made meanwhile still dispatched the old yes."""
        with self._lock:
            card = self._cards.get(cid)
            phase = card.phase(n) if card is not None else None
            if card is None or phase is None or card.terminal or card.rev != rev or phase.status != "queued":
                return None
            if phase.door == "one_way":
                a = phase.approval
                if not token or a is None or a.token != token or a.rev != card.rev or not a.what or a.what != what:
                    return None
            if not self.event("dispatched", cid, phase=n, rev=rev, token=token, executor=executor, **event_fields):
                return None
            return self.update_phase(cid, n, why="dispatched", status="running", executor=executor, approval=None)

    # -- reading it back --
    def due(self, now: float) -> list[Card]:
        """Open cards whose cue time or next nudge has come, earliest first."""
        out: list[tuple[float, Card]] = []
        for c in self.open():
            if c.status == "proposed":
                continue
            times = []
            cue_at = chief_card.at_time(c.cue.at) if c.cue else None
            if cue_at is not None and c.nudges.sent == 0:
                times.append(cue_at.timestamp())
            if c.nudges.next_at is not None:
                times.append(c.nudges.next_at)
            ready = [t for t in times if t <= now]
            if ready:
                out.append((min(ready), c))
        return [c for _, c in sorted(out, key=lambda x: (x[0], chief_card.card_number(x[1].id)))]

    def for_turn(self, now: float) -> str:
        """The line the brain sees every turn, at most TURN_LINE_CHARS; "" when nothing is open.
        "Open jobs: 2 (c12 waiting on your Go: order the picked desk; c13 researching: desks). Due today: …" """
        today = _day(now)
        cards = self.open()
        jobs = [c for c in cards if c.kind == "job" and c.status != "proposed"]
        parts: list[str] = []
        if jobs:
            parts.append(f"Open jobs: {len(jobs)} (" + "; ".join(self._describe(c) for c in jobs) + ").")
        due = [c for c in cards if c.kind == "commitment" and c.status != "proposed"
               and today in ((c.deadline or "")[:10], ((c.cue.at if c.cue else None) or "")[:10])]
        if due:
            parts.append("Due today: " + ", ".join(f"{c.title} ({c.id})" for c in due) + ".")
        proposed = sum(1 for c in cards if c.status == "proposed")
        if proposed:
            parts.append(f"Proposed: {proposed}.")
        line = " ".join(parts)
        if len(line) > TURN_LINE_CHARS:
            line = line[:TURN_LINE_CHARS - 1].rsplit(" ", 1)[0] + "…"
        return line

    @staticmethod
    def _describe(c: Card) -> str:
        if c.status == "waiting" and c.waiting_for == "go":
            pending = next((p for p in c.phases if p.door == "one_way" and p.status == "queued"), None)
            return f"{c.id} waiting on your Go: {pending.goal if pending else c.title}"
        if c.status == "waiting" and c.waiting_for in _WAITING_WORDS:
            return f"{c.id} waiting on your {_WAITING_WORDS[c.waiting_for]}: {c.title}"
        if c.status == "waiting":
            return f"{c.id} waiting for the Mac: {c.title}"
        if c.status == "unverified":
            return f"{c.id} unverified: {c.title}"
        current = next((p for p in c.phases if p.status in ("queued", "running")), None)
        return f"{c.id} {_DOING.get(current.do, 'working') if current else 'working'}: {c.title}"

    def matching(self, words: str) -> list[Card]:
        """Open cards the owner's words are about: they share two content words with the card's title, purpose
        and next action (one, when the card has only one). The owner's own action cancels queued nudges."""
        said = _content_words(words)
        out = []
        for c in self.open():
            mine = _content_words(" ".join([c.title, c.purpose, c.cue.next_action if c.cue else ""]))
            if mine and len(said & mine) >= min(2, len(mine)):
                out.append(c)
        return out

    def redact(self, ids: Iterable[str]) -> int:
        """Remove these cards and their event lines (a forget layer's helper). Ids stay used. Returns the count
        of cards removed."""
        gone = {i for i in ids if isinstance(i, str)}
        with self._lock:
            removed = [i for i in gone if i in self._cards]
            if removed:
                self._commit({k: v for k, v in self._cards.items() if k not in gone})
            try:
                lines = self.events_path.read_text(encoding="utf-8", errors="replace").splitlines(keepends=True)
            except FileNotFoundError:
                lines = []
            except OSError as e:
                raise LedgerError(f"could not read the events ({type(e).__name__})") from e
            keep = []
            for line in lines:
                try:
                    row = json.loads(line)
                except ValueError:
                    keep.append(line)
                    continue
                if not (isinstance(row, dict) and row.get("id") in gone):
                    keep.append(line)
            if len(keep) != len(lines):
                tmp = ""
                try:
                    fd, tmp = tempfile.mkstemp(prefix=".events-", suffix=".jsonl", dir=str(self.dir))
                    with os.fdopen(fd, "w", encoding="utf-8") as f:
                        f.writelines(keep)
                    os.chmod(tmp, 0o600)
                    os.replace(tmp, self.events_path)
                except OSError as e:
                    if tmp:
                        try:
                            os.unlink(tmp)
                        except OSError:
                            pass
                    raise LedgerError(f"could not rewrite the events ({type(e).__name__})") from e
            self.event("redacted", "", count=len(removed))
            return len(removed)
