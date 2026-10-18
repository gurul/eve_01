"""The chief of staff's record: the Card that holds every job the owner hands eve and every commitment they make.

The owner asked for a chief of staff (2026-09-29): work with more than one step is never delegated without a
card, and every card starts with eve's backbrief ("I intend to …; done when …; up to $X"). eve writes the
brief; the owner only corrects it. The idea is Marquet's "I intend to" (Turn the Ship Around!, 2012) and the
army's backbrief (FM 6-0, 2022, "Backbrief"): the one who will do the work says it back before starting.

A card is one of two kinds (``KINDS``):

* ``job``         eve does the phases: research, assess, act, watch (``PHASE_KINDS_LIVE``).
* ``commitment``  the owner acts; eve holds the cue ("remind me Thursday to …") and has no phases.

What code decides here, never a model:

* **The parse is strict and whole.** ``parse`` takes the brain's ``take_on`` arguments and returns a Card or a
  ``Refusal`` naming the first problem. One bad phase, one unknown check, one date that is not a date refuses
  the whole card, so a card is never partly made (plan_contract.parse_plan, plan_contract.py:94-136, does the
  same for a plan). Text
  fields are cut to their caps, as plan_contract's ``_text`` does.
* **The door is a floor in code and add-only.** A phase is ``one_way`` when task_router's ``CONSEQUENTIAL``
  wording matches its goal (task_router.py:70), when a Composio call in it has consequential slugs
  (composio_tools.consequential_slugs, composio_tools.py:150-161), or when its executor cannot stop and ask.
  Current act executors can stop and ask through the live runtime callback, so routine acts have no
  executor-wide one-way floor. Dangerous goals and model-raised doors still require Go. Loading a stored
  phase reapplies the goal floor and preserves any higher stored door (``_phase_from``, add-only).
  The brain may raise a door to ``one_way``; it can never lower one, and a revision keeps the higher door of
  each phase (plan_contract's add-only ``consequential``, plan_contract.py:13-15).
* **``grounded``** says whether the owner's own words carry the consequential verb of a one-way goal. A Go
  shows it when it is False: the verb came from eve's plan, not from the owner.
* **The words stay in the transcript.** A card keeps ``source.said_ref`` (the transcript day and time), never a
  copy of what was said, so forget reaches them where they live (memory.py).
* **``rev`` counts changes to what the owner agreed to**: the take_on fields. An approval is bound to (card,
  phase, rev) and a new rev cancels it. Progress (status, a phase's status and result, what was spent, the
  nudges) is state: it is logged in the ledger's events and does not bump rev, so a yes is never cancelled by
  its own dispatch. (The design said "every change is rev+1"; that would void each Go at the moment it is used.)

Only live names are in these sets: a phase kind or a check that is not built yet is not named anywhere, not even
as a refusal (the owner's rule for prompts and tool enums, 2026-09-09). ``Live`` lets a caller narrow the sets it
offers; ``parse`` refuses anything outside the set it was given.

The result every executor returns (``PhaseResult``) is defined here, beside ``Phase``, because a card carries
its phases' results; chief_receipt.py re-exports it and holds the oracles that read it.
"""

from __future__ import annotations

import dataclasses
import json
import math
import re
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Iterable, Mapping, Optional, Sequence

from .task_router import CONSEQUENTIAL

KINDS = ("job", "commitment")
STATUSES = ("proposed", "active", "waiting", "unverified", "done", "dropped")
TERMINAL = ("done", "dropped")
WAITING_FOR = ("question", "go", "budget", "mac_slot")
PHASE_KINDS_LIVE = ("research", "assess", "act", "watch")
PHASE_STATUSES = ("queued", "running", "ok", "failed", "refused", "skipped")
DOORS = ("two_way", "one_way")
# Every current executor can ask: Mac acts are forced through Codex with the chief's live owner callback.
# Keep the floor for any future executor that cannot ask; routine acts no longer need a preliminary Go.
ACT_CANNOT_ASK: tuple[str, ...] = ()
DONE_KINDS_LIVE = ("read_links", "cited_pick", "ui_seen", "watch_armed", "file_exists", "guru_says_done")
RESULT_STATUSES = ("done", "handed_on", "needs_guru", "refused", "failed", "over_budget")
EVIDENCE_KINDS = ("links", "ui_text", "watch_row", "file", "owner")
NUDGE_LEVELS = ("push", "sheet")

# The take_on arguments, all of them: anything else refuses the card.
TAKE_ON_KEYS = ("kind", "title", "purpose", "end_state", "done_checks", "never", "phases", "deadline", "cue",
                "firm", "question")
PHASE_KEYS = ("do", "goal", "door")
CHECK_KEYS = ("kind", "arg")
# A check that needs a phase of this kind on the card; the rest (file_exists, guru_says_done) need none.
CHECK_NEEDS_PHASE = {"read_links": "research", "cited_pick": "assess", "ui_seen": "act", "watch_armed": "watch"}
# The owner's own checks: what a commitment may be closed on.
COMMITMENT_CHECKS = ("file_exists", "guru_says_done")

MAX_PHASES = 6
MAX_NEVER = 5
MAX_CHECKS = 4
MAX_TITLE_CHARS = 40
MAX_LINE_CHARS = 160            # purpose, end_state, a goal, a next action
MAX_NEVER_CHARS = 80
MAX_QUESTION_CHARS = 200
MAX_UI_TEXT_CHARS = 80
MIN_UI_WORDS = 2                # ui_seen: one word ("order") is on too many screens to prove anything
MAX_PATH_CHARS = 240
MAX_READ_LINKS = 20
MAX_EVIDENCE = 40
MAX_EVIDENCE_REF_CHARS = 2000
MAX_RESULT_TEXT_CHARS = 2000

DEADLINE_FORMATS = ("%Y-%m-%d %H:%M", "%Y-%m-%d")
CUE_FORMAT = "%Y-%m-%d %H:%M"

# The owner's defaults (2026-09-29): a card that only reads and weighs gets $0.50 and 20 minutes; one with an act
# gets $1 and 30 minutes. A raise is one tap; the warning line goes out at 80%.
DEFAULT_BUDGET_READ_USD = 0.50
DEFAULT_BUDGET_READ_MIN = 20
DEFAULT_BUDGET_ACT_USD = 1.00
DEFAULT_BUDGET_ACT_MIN = 30
BUDGET_WARN_AT = 0.8

_ID = re.compile(r"^c([1-9]\d{0,8})$")
_WATCH_ID = re.compile(r"^w[1-9]\d{0,8}$")


class CardError(ValueError):
    """A stored card that does not load. The ledger skips it and says so; it never half-loads one."""


@dataclass(frozen=True)
class Refusal:
    """Why a take_on or a revision was refused: one line, for the brain to fix and for the log."""
    reason: str


@dataclass(frozen=True)
class Live:
    """The positive sets a caller offers. ``parse`` refuses anything outside them."""
    phases: tuple[str, ...] = PHASE_KINDS_LIVE
    checks: tuple[str, ...] = DONE_KINDS_LIVE


LIVE = Live()


@dataclass(frozen=True)
class Check:
    kind: str                       # DONE_KINDS_LIVE
    arg: str = ""                   # read_links: the count; ui_seen: the text; file_exists: the path; else ""

    def describe(self) -> str:
        if self.kind == "read_links":
            return f"read {self.arg} or more pages"
        if self.kind == "cited_pick":
            return "the pick cites a page I read"
        if self.kind == "ui_seen":
            return f"'{self.arg}' seen on screen"
        if self.kind == "watch_armed":
            return "the watch is set"
        if self.kind == "file_exists":
            return f"{self.arg} exists"
        return "you say it is done"


@dataclass(frozen=True)
class Evidence:
    kind: str                       # EVIDENCE_KINDS
    ref: str                        # a URL, the UI-state text, a watch id, a path, "done"


@dataclass(frozen=True)
class PhaseResult:
    """The one shape every executor adapter returns. ``text`` is the executor's own words: data, never evidence."""
    status: str                     # RESULT_STATUSES
    evidence: tuple[Evidence, ...] = ()
    text: str = ""
    usd: float = 0.0
    secs: float = 0.0


@dataclass(frozen=True)
class Approval:
    token: str                      # single use, bound to (card, n, rev)
    rev: int
    at: float
    what: str = ""                  # a digest of the act as the Go showed it (chief.act_digest); "" matches nothing


@dataclass(frozen=True)
class Phase:
    n: int                          # 1-based
    do: str                         # PHASE_KINDS_LIVE
    goal: str
    door: str = "two_way"           # DOORS: a code floor, add-only
    grounded: bool = True           # a one-way goal's verb is in the owner's words
    executor: str = ""              # set by code at dispatch
    status: str = "queued"          # PHASE_STATUSES
    approval: Optional[Approval] = None
    result: Optional[PhaseResult] = None


@dataclass(frozen=True)
class Cue:
    at: Optional[str]               # "YYYY-MM-DD HH:MM" or None
    next_action: str


@dataclass(frozen=True)
class Budget:
    usd: float
    wall_min: int


@dataclass(frozen=True)
class Spent:
    usd: float = 0.0
    wall_s: float = 0.0


@dataclass(frozen=True)
class Source:
    door: str = "telegram"          # where the owner said it
    said_ref: str = ""              # transcript day + time; never the words


@dataclass(frozen=True)
class Nudges:
    sent: int = 0
    ignored: int = 0
    next_at: Optional[float] = None
    level: str = "push"             # NUDGE_LEVELS: after 3 ignored nudges, only the weekly sheet


@dataclass(frozen=True)
class Card:
    id: str                         # "c<N>", never reused; "" until the ledger files it
    rev: int
    kind: str
    title: str
    purpose: str
    end_state: str
    done_checks: tuple[Check, ...]
    never: tuple[str, ...] = ()
    phases: tuple[Phase, ...] = ()
    deadline: Optional[str] = None
    cue: Optional[Cue] = None
    firm: bool = False
    question: str = ""
    budget: Budget = Budget(DEFAULT_BUDGET_READ_USD, DEFAULT_BUDGET_READ_MIN)
    spent: Spent = Spent()
    status: str = "active"
    waiting_for: Optional[str] = None
    source: Source = Source()
    nudges: Nudges = Nudges()
    created: float = 0.0
    updated: float = 0.0

    @property
    def terminal(self) -> bool:
        return self.status in TERMINAL

    def phase(self, n: int) -> Optional[Phase]:
        return next((p for p in self.phases if p.n == n), None)


# ---- the door ----------------------------------------------------------------------------------

def verbs(text: str) -> frozenset[str]:
    """The consequential words in a text (task_router.CONSEQUENTIAL), folded."""
    return frozenset(" ".join(m.group(0).casefold().split()) for m in CONSEQUENTIAL.finditer(text or ""))


def door(goal: str, *, raised: bool = False, slugs: Sequence[str] = (), can_ask: bool = True) -> str:
    """The code floor for one phase. ``raised``: the brain (or, later, a critic) asked for one_way; it can only
    raise. ``slugs``: composio_tools.consequential_slugs for a call the phase will make. ``can_ask``: False when
    the executor cannot stop and ask the owner."""
    if raised or verbs(goal) or any(str(s).strip() for s in slugs) or not can_ask:
        return "one_way"
    return "two_way"


def higher(a: str, b: str) -> str:
    return "one_way" if "one_way" in (a, b) else "two_way"


def grounded(goal: str, said: str) -> bool:
    """A one-way goal is grounded when every consequential word in it is also in the owner's words. A goal with
    no such word (raised by the brain) is not grounded: nothing the owner said names the act."""
    words = verbs(goal)
    return bool(words) and words <= verbs(said)


# ---- the parse ---------------------------------------------------------------------------------

class _Refused(Exception):
    pass


def _line(value: Any, limit: int) -> str:
    return " ".join(value.split())[:limit] if isinstance(value, str) else ""


def _need(value: Any, limit: int, what: str) -> str:
    text = _line(value, limit)
    if not text:
        raise _Refused(f"{what} is empty")
    return text


def _when(raw: Any, formats: Iterable[str], what: str) -> Optional[str]:
    if raw is None or raw == "":
        return None
    if not isinstance(raw, str):
        raise _Refused(f"{what} must be a string or null")
    text = " ".join(raw.replace("T", " ").split())
    for fmt in formats:
        try:
            return datetime.strptime(text, fmt).strftime(fmt)
        except ValueError:
            continue
    raise _Refused(f"{what} {raw!r} is not a date in the form {' or '.join(formats)}")


# An asked reminder waits this long for a breakpoint, then is sent anyway (chief_desk.REMINDER_WAIT_SECS reads it;
# the backbrief promises it).
REMINDER_WAIT_MIN = 45


def at_time(stamp: Optional[str]) -> Optional[datetime]:
    """A deadline or cue string as a local datetime; a date alone is the end of that day."""
    if not stamp:
        return None
    for fmt in DEADLINE_FORMATS:
        try:
            when = datetime.strptime(stamp, fmt)
        except ValueError:
            continue
        return when.replace(hour=23, minute=59) if fmt == "%Y-%m-%d" else when
    return None


def home_path(raw: str, home: Optional[Path] = None) -> Optional[Path]:
    """A path below the home folder, resolved; None for anything outside it (``..``, another root, a symlink out)
    and for the home folder itself, which always exists and so proves nothing (reviewer, 2026-09-29)."""
    base = (home or Path.home()).resolve()
    text = raw.strip()
    if not text:
        return None
    path = Path(text.replace("~", str(base), 1) if text.startswith("~") else text)
    if not path.is_absolute():
        path = base / path
    try:
        resolved = path.resolve()
    except (OSError, RuntimeError):
        return None
    return resolved if base in resolved.parents else None


def _check(raw: Any, index: int, live: Live, home: Optional[Path]) -> Check:
    where = f"done check {index}"
    if not isinstance(raw, dict):
        raise _Refused(f"{where}: not an object")
    extra = sorted(set(raw) - set(CHECK_KEYS))
    if extra:
        raise _Refused(f"{where}: unknown field {extra[0]!r}")
    kind = raw.get("kind")
    if kind not in live.checks:
        raise _Refused(f"{where}: kind must be one of {live.checks}, not {kind!r}")
    arg = raw.get("arg")
    if kind == "read_links":
        n = arg if isinstance(arg, int) and not isinstance(arg, bool) else (
            int(arg) if isinstance(arg, str) and arg.strip().isdigit() else 0)
        if not 1 <= n <= MAX_READ_LINKS:
            raise _Refused(f"{where}: read_links needs a count from 1 to {MAX_READ_LINKS}")
        return Check("read_links", str(n))
    if kind == "ui_seen":
        text = _need(arg, MAX_UI_TEXT_CHARS, f"{where}: ui_seen text")
        if len(text.split()) < MIN_UI_WORDS:
            raise _Refused(f"{where}: ui_seen needs at least two words that will be on the screen when it worked")
        return Check("ui_seen", text)
    if kind == "file_exists":
        text = _need(arg, MAX_PATH_CHARS, f"{where}: file_exists path")
        if home_path(text, home) is None:
            raise _Refused(f"{where}: file_exists takes the path of a file in the home folder")
        return Check("file_exists", text)
    return Check(str(kind), "")


def _phase(raw: Any, n: int, live: Live, said: str) -> Phase:
    where = f"phase {n}"
    if not isinstance(raw, dict):
        raise _Refused(f"{where}: not an object")
    extra = sorted(set(raw) - set(PHASE_KEYS))
    if extra:
        raise _Refused(f"{where}: unknown field {extra[0]!r}")
    do = raw.get("do")
    if do not in live.phases:
        raise _Refused(f"{where}: do must be one of {live.phases}, not {do!r}")
    goal = _need(raw.get("goal"), MAX_LINE_CHARS, f"{where}: goal")
    asked = raw.get("door")
    if asked not in (None, "", *DOORS):
        raise _Refused(f"{where}: door must be one of {DOORS} or null, not {asked!r}")
    raised = asked == "one_way"
    d = door(goal, raised=raised, can_ask=do not in ACT_CANNOT_ASK)
    # grounded flags a consequential word that came from eve's plan; an act one-way only by its executor, with
    # no such word and not raised, has nothing to flag (it is the owner's step, as it was when it was two-way)
    g = True if d == "two_way" or not (raised or verbs(goal)) else grounded(goal, said)
    return Phase(n=n, do=str(do), goal=goal, door=d, grounded=g)


def default_budget(phases: Sequence[Phase]) -> Budget:
    if any(p.do == "act" for p in phases):
        return Budget(DEFAULT_BUDGET_ACT_USD, DEFAULT_BUDGET_ACT_MIN)
    return Budget(DEFAULT_BUDGET_READ_USD, DEFAULT_BUDGET_READ_MIN)


def _fields(args: Mapping[str, Any], *, said: str, live: Live, home: Optional[Path]) -> dict[str, Any]:
    """The take_on arguments as Card fields, or _Refused."""
    if not isinstance(args, Mapping):
        raise _Refused("take_on arguments must be an object")
    extra = sorted(set(args) - set(TAKE_ON_KEYS))
    if extra:
        raise _Refused(f"unknown field {extra[0]!r}")
    kind = args.get("kind")
    if kind not in KINDS:
        raise _Refused(f"kind must be one of {KINDS}, not {kind!r}")
    title = _need(args.get("title"), MAX_TITLE_CHARS, "title")
    raw_phases = args.get("phases")
    raw_phases = [] if raw_phases is None else raw_phases
    if not isinstance(raw_phases, list):
        raise _Refused("phases must be a list")
    if kind == "commitment" and raw_phases:
        raise _Refused("a commitment has no phases: the owner does it")
    if kind == "job" and not raw_phases:
        raise _Refused("a job needs at least one phase")
    if len(raw_phases) > MAX_PHASES:
        raise _Refused(f"at most {MAX_PHASES} phases")
    phases = tuple(_phase(p, i, live, said) for i, p in enumerate(raw_phases, 1))
    raw_checks = args.get("done_checks")
    raw_checks = [] if raw_checks is None else raw_checks
    if not isinstance(raw_checks, list):
        raise _Refused("done_checks must be a list")
    if len(raw_checks) > MAX_CHECKS:
        raise _Refused(f"at most {MAX_CHECKS} done checks")
    checks = tuple(_check(c, i, live, home) for i, c in enumerate(raw_checks, 1))
    if not checks:
        if "guru_says_done" not in live.checks:
            raise _Refused("a card needs at least one done check")
        checks = (Check("guru_says_done"),)
    kinds = {p.do for p in phases}
    for c in checks:
        if kind == "commitment" and c.kind not in COMMITMENT_CHECKS:
            raise _Refused(f"a commitment is closed only by {tuple(k for k in COMMITMENT_CHECKS if k in live.checks)}")
        need = CHECK_NEEDS_PHASE.get(c.kind)
        if need and need not in kinds:
            raise _Refused(f"the {c.kind} check needs a {need} phase")
    raw_never = args.get("never")
    raw_never = [] if raw_never is None else raw_never
    if not isinstance(raw_never, list) or not all(isinstance(x, str) for x in raw_never):
        raise _Refused("never must be a list of strings")
    never = tuple(t for t in (_line(x, MAX_NEVER_CHARS) for x in raw_never) if t)[:MAX_NEVER]
    cue_raw = args.get("cue")
    cue: Optional[Cue] = None
    if cue_raw is not None:
        if kind == "job":
            raise _Refused("a job has no cue: its phases are its next actions")
        if not isinstance(cue_raw, dict) or sorted(set(cue_raw) - {"at", "next_action"}):
            raise _Refused("cue must be an object with at and next_action")
        cue = Cue(at=_when(cue_raw.get("at"), (CUE_FORMAT,), "cue.at"),
                  next_action=_need(cue_raw.get("next_action"), MAX_LINE_CHARS, "cue.next_action"))
    if kind == "commitment" and cue is None:
        raise _Refused("a commitment needs a cue with its next action")
    firm = args.get("firm")
    if firm not in (None, True, False):
        raise _Refused("firm must be true, false or null")
    question = args.get("question")
    if question is not None and not isinstance(question, str):
        raise _Refused("question must be a string or null")
    return {"kind": str(kind), "title": title, "purpose": _line(args.get("purpose"), MAX_LINE_CHARS),
            "end_state": _line(args.get("end_state"), MAX_LINE_CHARS), "done_checks": checks, "never": never,
            "phases": phases, "deadline": _when(args.get("deadline"), DEADLINE_FORMATS, "deadline"), "cue": cue,
            "firm": bool(firm), "question": _line(question, MAX_QUESTION_CHARS)}


def parse(args: Mapping[str, Any], *, said: str, live: Live = LIVE, door_name: str = "telegram",
          said_ref: str = "", now: float = 0.0, home: Optional[Path] = None) -> Card | Refusal:
    """The brain's take_on arguments as a Card (id "", rev 1), or a Refusal. ``said`` is the owner's words in this
    turn, taken by code from the turn's message, never from the brain; a card is made only in a turn that has
    them. The words are used for ``grounded`` and dropped: the card keeps ``said_ref``."""
    if not _line(said, 10_000):
        return Refusal("a card is made only in a turn that carries the owner's words")
    try:
        f = _fields(args, said=said, live=live, home=home)
    except _Refused as e:
        return Refusal(str(e))
    card = Card(id="", rev=1, budget=default_budget(f["phases"]), source=Source(door_name, said_ref[:40]),
                created=now, updated=now, **f)
    blocked = missing(card)
    return dataclasses.replace(card, status="waiting", waiting_for="question") if blocked else card


def missing(card: Card) -> list[str]:
    """What blocks a card from starting: a missing end state, or the one question whose answer changes the
    outcome. Nothing else may block one (design principle 2)."""
    out: list[str] = []
    if not card.end_state:
        out.append("end_state")
    if card.question:
        out.append("question")
    return out


def revise(card: Card, patch: Mapping[str, Any], *, said: str, live: Live = LIVE, now: float = 0.0,
           home: Optional[Path] = None) -> Card | Refusal:
    """The owner's correction: the take_on fields in ``patch`` replace the card's, as one new revision (rev+1).
    The kind never changes; a finished card is reopened, not revised. Each phase keeps the higher of its old and
    new door (add-only), phases that already ran keep their state (a phase is matched by its kind and goal, so
    one added, removed or moved never resets another), and every pending approval is cancelled."""
    if card.terminal:
        return Refusal(f"{card.id} is {card.status}; reopen it first")
    if "kind" in patch and patch["kind"] != card.kind:
        return Refusal("a revision cannot change the kind")
    merged = {**to_args(card), **dict(patch)}
    new = parse(merged, said=said, live=live, door_name=card.source.door, said_ref=card.source.said_ref, now=now,
                home=home)
    if isinstance(new, Refusal):
        return new
    # A phase is the same phase by what it is (its kind and goal), never by its number: matched by number, a
    # phase inserted at the front put every later one "in a new place", and a phase that had already run went
    # back to queued with its result gone, so an act that ran could become eligible again.
    unclaimed = list(card.phases)
    phases = []
    for p in new.phases:
        same = next((o for o in unclaimed if o.do == p.do and o.goal == p.goal), None)
        if same is not None:                             # the same phase: its state stays, its approval does not
            unclaimed.remove(same)
            if same.status == "running" and same.n != p.n:
                # its result comes back by number (chief.on_phase_result) and would land on another phase
                return Refusal(f"step {same.n} is running; change the card's steps once it finishes")
            d = higher(same.door, p.door)
            g = same.grounded if same.door == d else grounded(p.goal, said)
            phases.append(dataclasses.replace(same, n=p.n, door=d, grounded=g, approval=None))
            continue
        old = card.phase(p.n)
        if old is None:
            phases.append(p)
            continue
        d = higher(old.door, p.door)                     # a new phase in this place: it inherits the higher door
        g = p.grounded if p.door == d else grounded(p.goal, said)
        phases.append(dataclasses.replace(p, door=d, grounded=g))
    if missing(new):
        status, waiting = "waiting", "question"
    elif card.status == "waiting" and card.waiting_for == "question":
        status, waiting = "active", None
    else:
        status, waiting = card.status, card.waiting_for
    floor = default_budget(new.phases)
    budget = Budget(max(card.budget.usd, floor.usd), max(card.budget.wall_min, floor.wall_min))
    return dataclasses.replace(new, id=card.id, rev=card.rev + 1, phases=tuple(phases), budget=budget,
                               spent=card.spent, nudges=card.nudges, status=status, waiting_for=waiting,
                               created=card.created, updated=now)


def reopen(card: Card, *, now: float = 0.0) -> Card:
    """A done or dropped card comes back as a new revision, active; its history stays in the events log."""
    return dataclasses.replace(card, rev=card.rev + 1, status="active", waiting_for=None, updated=now,
                               phases=tuple(dataclasses.replace(p, approval=None) for p in card.phases))


def to_args(card: Card) -> dict[str, Any]:
    """The card's take_on fields, the shape ``parse`` takes (a revision merges a patch into these)."""
    return {"kind": card.kind, "title": card.title, "purpose": card.purpose, "end_state": card.end_state,
            "done_checks": [{"kind": c.kind, "arg": c.arg} for c in card.done_checks], "never": list(card.never),
            "phases": [{"do": p.do, "goal": p.goal, "door": p.door} for p in card.phases],
            "deadline": card.deadline,
            "cue": {"at": card.cue.at, "next_action": card.cue.next_action} if card.cue else None,
            "firm": card.firm, "question": card.question or None}


# ---- the backbrief -----------------------------------------------------------------------------

def _money(usd: float) -> str:
    return f"${usd:.2f}".replace(".00", "") if usd < 10 else f"${usd:.0f}"


# A research or watch goal is often a noun phrase ("standing desks under $600"): it gets its kind's verb, unless it
# already opens with a verb of its own.
_KIND_VERB = {"research": "research", "watch": "watch"}
_OPENING_VERBS = frozenset({"research", "find", "look", "search", "compare", "read", "check", "watch", "track",
                            "see", "get", "learn", "tell", "keep"})


def _step(p: Phase) -> str:
    first = p.goal.split(" ", 1)[0].casefold()
    verb = _KIND_VERB.get(p.do)
    return p.goal if not verb or first in _OPENING_VERBS else f"{verb} {p.goal}"


def backbrief(card: Card) -> str:
    """What eve says back before it starts, composed by code from the card (nothing here can be invented):
    "On it: standing desk. I intend to …, then … after your Go. Done when …. Up to $1 and 30 min." """
    if card.kind == "commitment":
        due = f", due {card.deadline}" if card.deadline else ""
        cue = card.cue
        when = " I'll keep it on your list."
        if cue and cue.at:
            due_at = at_time(cue.at)
            latest = f", {(due_at + timedelta(minutes=REMINDER_WAIT_MIN)):%H:%M} at the latest" if due_at else ""
            when = f" I'll remind you at your first break after {cue.at}{latest}."
        return f"Holding it: {card.title}{due}.{when}"
    steps = [_step(p) + (" after your Go" if p.door == "one_way" else "") for p in card.phases]
    plan = steps[0] if len(steps) == 1 else ", then ".join(steps)
    lines = [f"On it: {card.title}. I intend to {plan}."]
    if card.end_state:
        lines.append(f"Done when {card.end_state.rstrip('.')}.")
    if card.never:
        lines.append("Never: " + "; ".join(card.never) + ".")
    lines.append(f"Up to {_money(card.budget.usd)} and {card.budget.wall_min} min.")
    if card.question:
        lines.append(card.question)
    return " ".join(lines)


# ---- stored form -------------------------------------------------------------------------------

def to_json(card: Card) -> dict[str, Any]:
    """The card as plain JSON values (tuples become lists), the form ``from_json`` reads back."""
    return json.loads(json.dumps(dataclasses.asdict(card)))


def _num(value: Any, what: str, *, lo: float = 0.0) -> float:
    """A finite number >= lo. Infinity is refused too: a hand-edit's 1e999 reads as inf, and int(inf) raises
    OverflowError, which is not a CardError (reviewer, 2026-09-29)."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise CardError(f"{what} is not a number >= {lo}")
    try:
        number = float(value)
    except OverflowError as e:                 # an int too big for a float
        raise CardError(f"{what} is too big") from e
    if not math.isfinite(number) or number < lo:
        raise CardError(f"{what} is not a finite number >= {lo}")
    return number


def _str(value: Any, what: str, limit: int, *, empty: bool = True) -> str:
    if not isinstance(value, str) or len(value) > limit or (not empty and not value):
        raise CardError(f"{what} is not a string")
    return value


def _opt_str(value: Any, what: str, limit: int) -> Optional[str]:
    return None if value is None else _str(value, what, limit)


def _result_from(raw: Any) -> Optional[PhaseResult]:
    if raw is None:
        return None
    if not isinstance(raw, dict) or raw.get("status") not in RESULT_STATUSES:
        raise CardError("a phase result has no known status")
    ev = raw.get("evidence") or []
    if not isinstance(ev, list) or len(ev) > MAX_EVIDENCE:
        raise CardError("evidence is not a list")
    items = []
    for e in ev:
        if not isinstance(e, dict) or e.get("kind") not in EVIDENCE_KINDS:
            raise CardError("evidence has no known kind")
        items.append(Evidence(e["kind"], _str(e.get("ref"), "evidence.ref", MAX_EVIDENCE_REF_CHARS)))
    return PhaseResult(status=raw["status"], evidence=tuple(items),
                       text=_str(raw.get("text", ""), "result.text", MAX_RESULT_TEXT_CHARS),
                       usd=_num(raw.get("usd", 0.0), "result.usd"), secs=_num(raw.get("secs", 0.0), "result.secs"))


def result_from(raw: Any) -> PhaseResult:
    """An executor adapter's result, checked: a closed status, closed evidence kinds, capped text."""
    out = _result_from(raw)
    if out is None:
        raise CardError("no result")
    return out


def _phase_from(raw: Any) -> Phase:
    if not isinstance(raw, dict):
        raise CardError("a phase is not an object")
    n = raw.get("n")
    if isinstance(n, bool) or not isinstance(n, int) or not 1 <= n <= MAX_PHASES:
        raise CardError("a phase has no number")
    if raw.get("do") not in PHASE_KINDS_LIVE or raw.get("door") not in DOORS or raw.get("status") not in PHASE_STATUSES:
        raise CardError(f"phase {n} has an unknown kind, door or status")
    ap = raw.get("approval")
    approval = None
    if ap is not None:
        if not isinstance(ap, dict) or isinstance(ap.get("rev"), bool) or not isinstance(ap.get("rev"), int):
            raise CardError(f"phase {n} has a bad approval")
        approval = Approval(_str(ap.get("token"), "approval.token", 128, empty=False), ap["rev"],
                            _num(ap.get("at"), "approval.at"), _str(ap.get("what", ""), "approval.what", 64))
    goal = _str(raw.get("goal"), "goal", MAX_LINE_CHARS, empty=False)
    # Preserve an existing one-way door, and never load a dangerous goal below the code's current floor.
    door_ = higher(raw["door"], door(goal, can_ask=raw["do"] not in ACT_CANNOT_ASK))
    return Phase(n=n, do=raw["do"], goal=goal, door=door_,
                 grounded=bool(raw.get("grounded")), executor=_str(raw.get("executor", ""), "executor", 64),
                 status=raw["status"], approval=approval, result=_result_from(raw.get("result")))


def from_json(raw: Any) -> Card:
    """A stored card, checked field by field; CardError on anything off, so a hand-edit never half-loads."""
    if not isinstance(raw, dict):
        raise CardError("a card is not an object")
    cid = raw.get("id")
    if not isinstance(cid, str) or not _ID.match(cid):
        raise CardError("a card has no id")
    rev = raw.get("rev")
    if isinstance(rev, bool) or not isinstance(rev, int) or rev < 1:
        raise CardError(f"{cid}: bad rev")
    if raw.get("kind") not in KINDS or raw.get("status") not in STATUSES:
        raise CardError(f"{cid}: unknown kind or status")
    if raw.get("waiting_for") not in (None, *WAITING_FOR):
        raise CardError(f"{cid}: unknown waiting_for")
    checks = raw.get("done_checks")
    if not isinstance(checks, list) or not checks:
        raise CardError(f"{cid}: no done checks")
    done_checks = []
    for c in checks:
        if not isinstance(c, dict) or c.get("kind") not in DONE_KINDS_LIVE:
            raise CardError(f"{cid}: unknown done check")
        done_checks.append(Check(c["kind"], _str(c.get("arg", ""), "check.arg", MAX_PATH_CHARS)))
    phases = raw.get("phases")
    if not isinstance(phases, list) or len(phases) > MAX_PHASES:
        raise CardError(f"{cid}: bad phases")
    never = raw.get("never")
    if not isinstance(never, list):
        raise CardError(f"{cid}: bad never")
    cue_raw = raw.get("cue")
    cue = None
    if cue_raw is not None:
        if not isinstance(cue_raw, dict):
            raise CardError(f"{cid}: bad cue")
        cue = Cue(_opt_str(cue_raw.get("at"), "cue.at", 16), _str(cue_raw.get("next_action"), "next_action", MAX_LINE_CHARS))
    b, s, src, nd = (raw.get(k) if isinstance(raw.get(k), dict) else {} for k in ("budget", "spent", "source", "nudges"))
    level = nd.get("level", "push")
    if level not in NUDGE_LEVELS:
        raise CardError(f"{cid}: bad nudge level")
    next_at = nd.get("next_at")
    return Card(
        id=cid, rev=rev, kind=raw["kind"], title=_str(raw.get("title"), "title", MAX_TITLE_CHARS, empty=False),
        purpose=_str(raw.get("purpose", ""), "purpose", MAX_LINE_CHARS),
        end_state=_str(raw.get("end_state", ""), "end_state", MAX_LINE_CHARS), done_checks=tuple(done_checks),
        never=tuple(_str(x, "never", MAX_NEVER_CHARS) for x in never), phases=tuple(_phase_from(p) for p in phases),
        deadline=_opt_str(raw.get("deadline"), "deadline", 16), cue=cue, firm=bool(raw.get("firm")),
        question=_str(raw.get("question", ""), "question", MAX_QUESTION_CHARS),
        budget=Budget(_num(b.get("usd"), "budget.usd"), int(_num(b.get("wall_min"), "budget.wall_min"))),
        spent=Spent(_num(s.get("usd", 0.0), "spent.usd"), _num(s.get("wall_s", 0.0), "spent.wall_s")),
        status=raw["status"], waiting_for=raw.get("waiting_for"),
        source=Source(_str(src.get("door", "telegram"), "source.door", 32), _str(src.get("said_ref", ""), "said_ref", 40)),
        nudges=Nudges(int(_num(nd.get("sent", 0), "nudges.sent")), int(_num(nd.get("ignored", 0), "nudges.ignored")),
                      None if next_at is None else _num(next_at, "nudges.next_at"), level),
        created=_num(raw.get("created", 0.0), "created"), updated=_num(raw.get("updated", 0.0), "updated"))


def card_number(cid: str) -> int:
    """The N of "c<N>", or 0."""
    m = _ID.match(cid or "")
    return int(m.group(1)) if m else 0


def is_watch_id(text: str) -> bool:
    return bool(_WATCH_ID.match(text or ""))

