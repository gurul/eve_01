"""The chief's desk: the one gate every message it would send the owner goes through, and the briefs it composes.

Pull comes before push (design principle 6, "protect attention"). The owner reads what eve is doing when they
ask: the first-message brief rides the reply to their first message of the day, ``/jobs`` answers with no model
call. What the chief would send on its own goes through ``Desk.offer``, which answers with one route and the rule
that decided it (``ROUTES``, ``RULES``):

* ``now``     send it.
* ``batch``   do not send it now. It waits for the next pull (the brief, ``/jobs``) or, when the route carries
              ``at``, is offered again then. Every doubt lands here: an error, a rule that cannot be read, a
              mandate that does not parse (consent.py's fail-closed rule, consent.py:1-13).
* ``silent``  shadow: it would have been sent; it is logged as a would-push and not sent.
* ``drop``    it no longer matters: the card is finished, or the owner already acted on it.

Who may interrupt, by the owner's decisions of 2026-09-29:

* **Not unprompted, sent:** a reply, a receipt, and the Go for a job the owner started. A Go waits out quiet
  hours; replies and receipts do not (a receipt belongs to work the owner started).
* **A reminder the owner asked for** is exempt from the daily budget and from the shadow week, but it still waits
  for quiet hours to end and for a breakpoint: a task ending, a relay turn ending, a wake session ending, or 90 s
  after the owner's last message. If no breakpoint comes within 45 minutes, it is sent then anyway (owner,
  2026-10-06: it used to move to the next brief, so a "remind me tonight" arrived the next morning).
* **Unprompted** (a nudge, the 80% budget warning, the evening close, the weekly sheet): only a firm commitment
  due within 2 hours may nudge; outside quiet hours; at a breakpoint; within 2 a day; after its backoff. ``EVE_CHIEF_PUSH`` decides
  what happens when all of that passes: ``shadow`` (the default until the E5 shadow week passes) logs it as
  ``silent``, ``on`` sends it, ``off`` batches every one. In shadow the budget counts the would-pushes, so the
  shadow week measures the policy as it would run.
* **Backoff.** Each ignored nudge doubles the gap (4 h, 8 h, 16 h); after 3 ignored nudges the card appears only
  on the weekly sheet. The owner's own action on a card cancels its queued nudges and reminders (``acted``).
* **A decision never expires into an act.** ``on_expiry`` takes the choice that does not act, or none.

The budget warning was first sent as a receipt, which skipped quiet hours and the push mode; it is not one of the
kinds the owner's decisions exempt, so it is ``warn``, unprompted (reviewer, 2026-09-29). A problem with the
chief's own files is not offered here at all: it rides the reply to the owner's next message (chief.Chief.heard).

Quiet hours (22:30-08:00) and the daily budget (2) are the defaults; ``mandate.toml`` in the chief folder may set
``quiet_start``, ``quiet_end`` and ``pushes_per_day``. A mandate that does not parse falls back to the strictest
settings (the default quiet hours and no unprompted pushes) and the owner is told once.

Every verdict is one line in ``attention.jsonl`` (ids and rule words, never text), which is also where the day's
push count lives, so it survives a restart. A verdict to push that cannot be logged is not sent: an unlogged
push could break the budget. There was no quiet-hours code anywhere in eve before this (design appendix B).

The brief, the evening close and the weekly sheet are composed by code from the ledger, so they cannot invent a
card, a deadline or a result.
"""

from __future__ import annotations

import dataclasses
import json
import logging
import math
import os
import threading
import time
import tomllib
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Callable, Mapping, Optional, Sequence

from . import chief_card
from .chief_card import Card, Nudges
from .chief_ledger import Ledger, chief_dir

log = logging.getLogger(__name__)

PUSH_MODES = ("off", "shadow", "on")
DEFAULT_PUSH_MODE = "shadow"
QUIET_START = "22:30"
QUIET_END = "08:00"
PUSHES_PER_DAY = 2
STRICT_PUSHES_PER_DAY = 0            # a mandate that does not parse: no unprompted pushes
MAX_PUSHES_PER_DAY = 10
MANDATE_FILE = "mandate.toml"
MANDATE_KEYS = ("quiet_start", "quiet_end", "pushes_per_day")
ATTENTION_FILE = "attention.jsonl"
EVENING_CLOSE_AT = "21:00"
WEEKLY_SHEET_DAY = 6                 # Sunday (datetime.weekday)
WEEKLY_SHEET_AT = "18:00"

ITEM_KINDS = ("reply", "receipt", "go", "reminder", "nudge", "warn", "close", "sheet")
UNPROMPTED = ("nudge", "warn", "close", "sheet")
ROUTES = ("now", "batch", "silent", "drop")
RULES = ("reply", "receipt", "owner_started_go", "asked_reminder", "urgent_firm", "budget_warning", "evening_close",
         "weekly_sheet",
         "quiet_hours", "push_budget", "await_breakpoint", "reminder_waited", "backoff", "sheet_only",
         "not_urgent", "owner_acted", "card_closed", "push_off", "error")
BREAKPOINTS = ("task_end", "relay_end", "wake_end", "idle")
BREAKPOINT_WINDOW_SECS = 120.0       # an item offered this soon after a breakpoint is "at" it
IDLE_SECS = 90.0                     # the owner's last message this long ago is a breakpoint ("idle")
REMINDER_WAIT_SECS = chief_card.REMINDER_WAIT_MIN * 60.0   # a reminder waits this long for a breakpoint, then is sent
URGENT_SECS = 2 * 3600.0             # a firm commitment due within this may nudge
NUDGE_GAP_SECS = 4 * 3600.0          # the gap after the first ignored nudge; it doubles with each one
MAX_IGNORED = 3                      # then only the weekly sheet
STALE_DAYS = 7                       # the weekly sheet: an open card untouched this long
BRIEF_LINES = 3


@dataclass(frozen=True)
class Mandate:
    quiet_start: str = QUIET_START
    quiet_end: str = QUIET_END
    pushes_per_day: int = PUSHES_PER_DAY


STRICT = Mandate(pushes_per_day=STRICT_PUSHES_PER_DAY)


@dataclass(frozen=True)
class Item:
    """Something the chief would send. ``queued_at``: when it was first offered (the 45-minute wait and the
    owner's own action count from here). ``deadline_at``: a nudge's firm deadline."""
    kind: str                                  # ITEM_KINDS
    card_id: str = ""
    queued_at: float = 0.0
    deadline_at: Optional[float] = None
    firm: bool = False
    nudges: Nudges = Nudges()
    card_open: bool = True


@dataclass(frozen=True)
class Route:
    route: str                                 # ROUTES
    rule: str                                  # RULES
    at: Optional[float] = None                 # batch: offer it again then; None: the next pull


@dataclass(frozen=True)
class Choice:
    label: str
    acts: bool                                 # choosing it starts an act


@dataclass(frozen=True)
class Sheet:
    text: str
    ids: tuple[str, ...]                       # each gets Keep and Kill


def push_mode(environ: Optional[Mapping[str, str]] = None) -> str:
    """EVE_CHIEF_PUSH = off | shadow (the default) | on. 1/true/yes mean on, 0/false/no mean off."""
    env = os.environ if environ is None else environ
    raw = (env.get("EVE_CHIEF_PUSH") or DEFAULT_PUSH_MODE).strip().lower()
    raw = {"1": "on", "true": "on", "yes": "on", "0": "off", "false": "off", "no": "off"}.get(raw, raw)
    if raw not in PUSH_MODES:
        log.warning("chief: EVE_CHIEF_PUSH=%r is not one of %s; %s", raw, PUSH_MODES, DEFAULT_PUSH_MODE)
        return DEFAULT_PUSH_MODE
    return raw


def _hhmm(value: Any) -> str:
    if not isinstance(value, str):
        raise ValueError("not a time")
    return datetime.strptime(value.strip(), "%H:%M").strftime("%H:%M")


def load_mandate(path: Path) -> tuple[Mandate, str]:
    """The mandate and a problem line ("" when fine). No file: the defaults. A file that does not parse: STRICT."""
    try:
        raw = tomllib.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return Mandate(), ""
    except (OSError, ValueError) as e:
        return STRICT, f"I couldn't read {path.name} ({type(e).__name__}), so I'm sending nothing unprompted."
    try:
        extra = sorted(set(raw) - set(MANDATE_KEYS))
        if extra:
            raise ValueError(f"unknown setting {extra[0]!r}")
        n = raw.get("pushes_per_day", PUSHES_PER_DAY)
        if isinstance(n, bool) or not isinstance(n, int) or not 0 <= n <= MAX_PUSHES_PER_DAY:
            raise ValueError(f"pushes_per_day must be 0 to {MAX_PUSHES_PER_DAY}")
        start, end = _hhmm(raw.get("quiet_start", QUIET_START)), _hhmm(raw.get("quiet_end", QUIET_END))
        if start == end:
            raise ValueError("quiet hours are empty")
        return Mandate(start, end, n), ""
    except ValueError as e:
        return STRICT, f"{path.name} has a problem ({e}), so I'm sending nothing unprompted until it's fixed."


def _time(value: Any) -> Optional[float]:
    """A log row's time, or None: a hand-edited row ("soon", 1e999, a list) is skipped, never a crash
    (reviewer, 2026-09-29: ``float("soon")`` raised in Desk.__init__ and the chief could not be built)."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    try:
        t = float(value)
        datetime.fromtimestamp(t)
    except (OverflowError, OSError, ValueError):
        return None
    return t if math.isfinite(t) and t >= 0 else None


def _minutes(hhmm: str) -> int:
    h, m = hhmm.split(":")
    return int(h) * 60 + int(m)


def in_quiet(mandate: Mandate, ts: float) -> bool:
    t = datetime.fromtimestamp(ts)
    now, start, end = t.hour * 60 + t.minute, _minutes(mandate.quiet_start), _minutes(mandate.quiet_end)
    return start <= now < end if start < end else (now >= start or now < end)


def quiet_ends(mandate: Mandate, ts: float) -> float:
    """When the quiet hours holding ``ts`` end (``ts`` itself when it is not in them)."""
    if not in_quiet(mandate, ts):
        return ts
    t = datetime.fromtimestamp(ts)
    h, m = mandate.quiet_end.split(":")
    end = t.replace(hour=int(h), minute=int(m), second=0, microsecond=0)
    if end <= t:
        end += timedelta(days=1)
    return end.timestamp()


def on_expiry(choices: Sequence[Choice]) -> Optional[Choice]:
    """A decision that expired (a timeout, a hold word, silence) takes the first choice that does not act, or
    none at all: it never expires into an act (design rule 7; consent.decision's "" is never a yes)."""
    return next((c for c in choices if not c.acts), None)


class Desk:
    """The attention gate. Lent a clock so the tests drive the hours."""

    def __init__(self, folder: Optional[Path] = None, *, mode: Optional[str] = None,
                 mandate: Optional[Mandate] = None, clock: Callable[[], float] = time.time,
                 environ: Optional[Mapping[str, str]] = None) -> None:
        self.dir = Path(folder).expanduser() if folder is not None else chief_dir(environ)
        self._clock = clock
        self._lock = threading.RLock()
        self.mode = mode if mode in PUSH_MODES else push_mode(environ)
        if mandate is not None:
            self.mandate, self._problem = mandate, ""
        else:
            self.mandate, self._problem = load_mandate(self.dir / MANDATE_FILE)
            if self._problem:
                log.warning("chief: %s", self._problem)
        self._break_at: Optional[float] = None
        self._owner_said_at: Optional[float] = None
        self._acted: dict[str, float] = {}
        for row in self._rows():
            t = _time(row.get("t"))
            if row.get("kind") == "acted" and isinstance(row.get("id"), str) and t is not None:
                self._acted[row["id"]] = max(self._acted.get(row["id"], 0.0), t)

    @property
    def path(self) -> Path:
        return self.dir / ATTENTION_FILE

    def take_problem(self) -> str:
        with self._lock:
            problem, self._problem = self._problem, ""
            return problem

    # -- the log --
    def _append(self, row: dict[str, Any]) -> bool:
        line = json.dumps(row, separators=(",", ":")) + "\n"
        with self._lock:
            try:
                self.dir.mkdir(mode=0o700, parents=True, exist_ok=True)
                fd = os.open(self.path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
                try:
                    os.write(fd, line.encode("utf-8"))
                    os.fsync(fd)
                finally:
                    os.close(fd)
                return True
            except OSError as e:
                log.warning("chief: an attention line was not written (%s)", type(e).__name__)
                return False

    def _rows(self) -> list[dict[str, Any]]:
        try:
            lines = self.path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            return []
        out = []
        for line in lines:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if isinstance(row, dict):
                out.append(row)
        return out

    def pushes_today(self, now: Optional[float] = None) -> int:
        """Unprompted pushes today (sent, or would-sent in shadow), read from the log: a restart keeps the count."""
        day = datetime.fromtimestamp(self._clock() if now is None else now).date()
        n = 0
        for row in self._rows():
            t = _time(row.get("t"))
            if (row.get("unprompted") and row.get("route") in ("now", "silent") and t is not None
                    and datetime.fromtimestamp(t).date() == day):
                n += 1
        return n

    # -- the moments --
    def breakpoint(self, kind: str, now: Optional[float] = None) -> None:
        """A task ended, a relay turn ended, a wake session ended, or the owner went quiet (``BREAKPOINTS``)."""
        if kind not in BREAKPOINTS:
            raise ValueError(f"unknown breakpoint {kind!r}")
        with self._lock:
            self._break_at = self._clock() if now is None else now

    def owner_said(self, now: Optional[float] = None) -> None:
        """The owner wrote: IDLE_SECS later is a breakpoint."""
        with self._lock:
            self._owner_said_at = self._clock() if now is None else now

    def at_breakpoint(self, now: float) -> bool:
        """A breakpoint happened within BREAKPOINT_WINDOW_SECS, or the owner's last message is IDLE_SECS old
        (and not much older: a message from hours ago is not a moment)."""
        with self._lock:
            broke = self._break_at is not None and 0 <= now - self._break_at <= BREAKPOINT_WINDOW_SECS
            said = self._owner_said_at
            idle = said is not None and IDLE_SECS <= now - said <= IDLE_SECS + BREAKPOINT_WINDOW_SECS
            return broke or idle

    def acted(self, card_id: str, now: Optional[float] = None) -> None:
        """The owner acted on this card themselves: its queued nudges and reminders are dropped."""
        t = self._clock() if now is None else now
        with self._lock:
            self._acted[card_id] = max(self._acted.get(card_id, 0.0), t)
        self._append({"t": round(t, 3), "kind": "acted", "id": card_id})

    # -- backoff --
    @staticmethod
    def sent(nudges: Nudges, now: float) -> Nudges:
        """A nudge went out: the next one waits the current gap."""
        return dataclasses.replace(nudges, sent=nudges.sent + 1, next_at=now + NUDGE_GAP_SECS * 2 ** nudges.ignored)

    @staticmethod
    def ignored(nudges: Nudges, now: float) -> Nudges:
        """A nudge was ignored: the gap doubles; at MAX_IGNORED the card goes to the weekly sheet only."""
        n = nudges.ignored + 1
        return dataclasses.replace(nudges, ignored=n, next_at=now + NUDGE_GAP_SECS * 2 ** (n - 1),
                                   level="sheet" if n >= MAX_IGNORED else nudges.level)

    # -- the gate --
    def offer(self, item: Item, now: Optional[float] = None) -> Route:
        """One verdict for one item, logged. Never raises: an error is ``batch``."""
        t = self._clock() if now is None else now
        try:
            route = self._decide(item, t)
        except Exception as e:  # noqa: BLE001 — a gate that breaks holds the message, it never sends it
            log.warning("chief: the desk could not decide (%s); batched", type(e).__name__)
            route = Route("batch", "error")
        unprompted = getattr(item, "kind", "") in UNPROMPTED
        row = {"t": round(t, 3), "kind": str(getattr(item, "kind", ""))[:16],
               "id": str(getattr(item, "card_id", ""))[:16], "route": route.route, "rule": route.rule,
               "mode": self.mode, "unprompted": unprompted}
        if not self._append(row) and unprompted and route.route in ("now", "silent"):
            return Route("batch", "error")                 # an uncounted push could break the budget
        return route

    def _decide(self, item: Item, now: float) -> Route:
        if item.kind not in ITEM_KINDS:
            raise ValueError(f"unknown item kind {item.kind!r}")
        if item.kind in ("go", "reminder", "nudge") and not item.card_open:
            return Route("drop", "card_closed")
        if item.kind in ("reminder", "nudge") and item.card_id and self._acted.get(item.card_id, -1.0) >= item.queued_at:
            return Route("drop", "owner_acted")
        if item.kind == "reply":
            return Route("now", "reply")
        if item.kind == "receipt":
            return Route("now", "receipt")
        quiet = in_quiet(self.mandate, now)
        if item.kind == "go":
            return Route("batch", "quiet_hours", quiet_ends(self.mandate, now)) if quiet else Route(
                "now", "owner_started_go")
        if item.kind == "reminder":
            if quiet:
                return Route("batch", "quiet_hours", quiet_ends(self.mandate, now))
            if self.at_breakpoint(now):
                return Route("now", "asked_reminder")
            if now - item.queued_at >= REMINDER_WAIT_SECS:
                return Route("now", "reminder_waited")
            return Route("batch", "await_breakpoint", item.queued_at + REMINDER_WAIT_SECS)
        # unprompted: a nudge, the evening close, the weekly sheet
        if self.mode == "off":
            return Route("batch", "push_off")
        if item.kind == "nudge":
            if item.nudges.level == "sheet" or item.nudges.ignored >= MAX_IGNORED:
                return Route("batch", "sheet_only")
            if not (item.firm and item.deadline_at is not None and item.deadline_at - now <= URGENT_SECS):
                return Route("batch", "not_urgent")
            if item.nudges.next_at is not None and item.nudges.next_at > now:
                return Route("batch", "backoff", item.nudges.next_at)
        if quiet:
            return Route("batch", "quiet_hours", quiet_ends(self.mandate, now))
        if self.pushes_today(now) >= self.mandate.pushes_per_day:
            return Route("batch", "push_budget")
        if not self.at_breakpoint(now):
            return Route("batch", "await_breakpoint")
        rule = {"nudge": "urgent_firm", "warn": "budget_warning", "close": "evening_close",
                "sheet": "weekly_sheet"}[item.kind]
        return Route("silent" if self.mode == "shadow" else "now", rule)


# ---- composed by code from the ledger -------------------------------------------------------------

def _due_at(card: Card) -> Optional[float]:
    stamps = [chief_card.at_time(card.deadline), chief_card.at_time(card.cue.at) if card.cue else None]
    times = [s.timestamp() for s in stamps if s is not None]
    return min(times) if times else None


def _waiting_word(card: Card) -> str:
    if card.waiting_for == "go":
        pending = next((p for p in card.phases if p.door == "one_way" and p.status == "queued"), None)
        return f"{card.id} your Go ({pending.goal if pending else card.title})"
    if card.waiting_for == "question":
        return f"{card.id} your answer ({card.title})"
    if card.waiting_for == "budget":
        return f"{card.id} a budget raise ({card.title})"
    return ""


def _fit(lines: list[str], limit: int = 200) -> list[str]:
    return [line if len(line) <= limit else line[:limit - 1].rsplit(" ", 1)[0] + "…" for line in lines]


def first_brief(ledger: Ledger, now: float) -> str:
    """After the owner's first message of the day: today's one thing, what waits on them, what closed since
    yesterday, what is proposed. At most BRIEF_LINES lines; "" when there is nothing."""
    cards = ledger.all()
    open_ = [c for c in cards if not c.terminal and c.status != "proposed"]
    dated = sorted(((_due_at(c), c) for c in open_ if _due_at(c) is not None), key=lambda x: x[0] or 0.0)
    lines: list[str] = []
    if dated:
        c = dated[0][1]
        when = c.deadline or (c.cue.at if c.cue else "")
        lines.append(f"Today's one thing: {c.title} ({c.id}), due {when}.")
    waiting = [w for w in (_waiting_word(c) for c in open_ if c.status == "waiting") if w]
    if waiting:
        lines.append("Waiting on you: " + "; ".join(waiting) + ".")
    closed = [c for c in cards if c.terminal and c.updated >= now - 86400]
    if closed:
        lines.append("Closed since yesterday: " + ", ".join(f"{c.title} ({c.id}, {c.status})" for c in closed) + ".")
    proposed = [c for c in cards if c.status == "proposed"]
    if proposed:
        lines.append("Proposed: " + ", ".join(f"{c.title} ({c.id})" for c in proposed) + ".")
    return "\n".join(_fit(lines[:BRIEF_LINES]))


def evening_close(ledger: Ledger, now: float) -> str:
    """What closed today with its checks, what slipped, and one choice about tomorrow."""
    today = datetime.fromtimestamp(now).date()
    cards = ledger.all()
    lines: list[str] = []
    closed = [c for c in cards if c.status == "done" and datetime.fromtimestamp(c.updated).date() == today]
    if closed:
        lines.append("Closed today: " + "; ".join(
            f"{c.title} ({c.id}: " + ", ".join(k.describe() for k in c.done_checks) + ")" for c in closed) + ".")
    open_ = [c for c in cards if not c.terminal and c.status != "proposed"]
    slipped = [c for c in open_ if (_due_at(c) or float("inf")) < now]
    if slipped:
        lines.append("Slipped: " + ", ".join(f"{c.title} ({c.id}, due {c.deadline or (c.cue.at if c.cue else '')})"
                                             for c in slipped) + ".")
    tomorrow = today + timedelta(days=1)
    nxt = sorted((c for c in open_ if (d := _due_at(c)) is not None and datetime.fromtimestamp(d).date() == tomorrow),
                 key=lambda c: _due_at(c) or 0.0)
    if nxt:
        lines.append(f"Tomorrow first: {nxt[0].title} ({nxt[0].id})?")
    return "\n".join(_fit(lines))


def weekly_sheet(ledger: Ledger, now: float) -> Sheet:
    """Stale cards, cards with no next action, reminders ignored 3 times, unverified cards; each with Keep and
    Kill. Empty text when there is nothing."""
    rows: list[tuple[str, str]] = []
    for c in ledger.open():
        if c.status == "unverified":
            why = "not confirmed"
        elif c.nudges.level == "sheet" or c.nudges.ignored >= MAX_IGNORED:
            why = f"reminder ignored {c.nudges.ignored} times"
        elif c.kind == "job" and not any(p.status in ("queued", "running") for p in c.phases):
            why = "no next action"
        elif c.updated <= now - STALE_DAYS * 86400:
            why = f"untouched for {int((now - c.updated) // 86400)} days"
        else:
            continue
        rows.append((c.id, f"{c.id} {c.title}: {why}"))
    return Sheet("\n".join(_fit([r[1] for r in rows])), tuple(r[0] for r in rows))
