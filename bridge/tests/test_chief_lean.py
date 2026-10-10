"""The chief's Lean model (verification/Eve/Chief.lean), replayed against the Python chief: gate G8.3 of the chief
design (2026-09-29), ``PYTEST tests/test_chief_lean.py && echo CHIEF_LEAN_REPLAY_OK``.

The model's decision rules are transliterated here one to one (``MPhase`` … ``step_with``, each citing its Lean
definition), with the constants read out of the .lean file itself, so a change to either side shows up as a
disagreement. The transliteration is first held to the model's own kernel-checked traces (``desk_job_closes``,
``timeout_is_no``, … ``no_act_at_night``): what Lean proved of the model, the transliteration must reproduce.

Then seeded random traces drive the real ``Chief`` (chief.py) with fakes the test controls: when each executor
answers and how (a web read with four links, a failed one; an options sheet, a reply that is not one; a Mac act
that saw "Order confirmed" on screen, one whose only claim is its own sentence, one that failed), the owner's
answers and code words (Yes, No, silence, a hold word, "go c1", change, drop, done, reopen, It happened, It
didn't, Raise), the clock (quiet-hour edges), and restarts (a new Chief on the same folder). Every ledger line the
code writes becomes the model's event, and after every move the two must agree:

- **dispatch**: the code starts a step exactly when the model's ``dispatch`` of the first unfinished phase would
  (canGo, fits, quiet hours), and it is the same step;
- **approval**: every yes the code records (by id or to a Go) the model's ``fGo`` / ``fAnswer`` also grants, and
  the token is there on both sides or on neither; every Go the code asks the model's ``fAsk`` allows;
- **proof**: every ``checked`` verdict equals ``verdictOf`` of the evidence the fakes actually produced (the test
  knows it: it wrote them), and the card's status (active, unverified, done, dropped), revision, running step,
  each phase's state, attempts and retries, and the lessons kept per (executor, phase kind) are equal;
- the model's ``Spec`` (all seven properties) holds after every event.

Positive control (design 9 P8): a variant of the code whose act adapter takes the executor's own sentence for a
screen state (``chief_receipt.from_agent`` patched) closes a card done on the executor's word; the replay finds the
disagreement at that check, and the same variant agrees instead with the model's ``naiveVerdict`` oracle, whose
end state breaks DoneNeedsEvidence, as ``naive_violates`` proves in Lean.

Where the model and the code differ in shape and not in the properties, the replay maps one onto the other and
says so here (checked 2026-09-29 in chief.py):

1. A failed one-way act: the model puts it back to waiting for a Go on an active card (Chief.lean ``fReflect``);
   the code leaves it failed, closes the card unverified at the next tick, and Reopen then asks a new Go
   (chief.py ``_advance``, ``reopen``). Both never run it again without a new yes. The replay lets a code
   ``failed`` one-way phase stand for a model ``queued`` one.
2. "go c1" on an act in doubt: the code approves it by id ("say go c1 to do it again", chief.py ``approve``); the
   model's ``fGo`` only approves a queued or asked phase, so the replay feeds it as It didn't, then go. The code
   keeps offering It happened / It didn't on that act until it runs again; the replay does not press them then.
3. A retry: the code also re-checks the budget before it re-queues (chief.py ``_reflect``); the model leaves that to
   ``dispatch``. The replay passes the reflection's word AND the model's own ``fits`` as ``reflect``'s ``retry``.
4. Spend: the model's ``result`` cost is what the code charged (the card's spend before and after, in whole
   micro-dollars and seconds, rounded up: the budget thresholds are whole numbers, so ``x <= T`` iff
   ``ceil(x) <= T``).
5. A change on an unverified card: ``Chief.change`` makes a new revision and keeps it unverified (chief_card.revise
   refuses only done or dropped cards); the model's ``revise`` changes only an active card. Nothing can be approved
   on an unverified card on either side; the replay compares revisions up to the count of such changes. No door
   calls ``Chief.change`` yet (the chat's "change c12" shows the card, telegram.py ``verb == "change"``); the
   replay drives it because the model has ``revise`` and the method is the chief's API for it.
6. The desk: the model's ``atBreak`` is realized by a breakpoint at the moment of the offer on a Desk reloaded from
   the same folder (so every offer also checks that the day's count survives a reload); ``busy`` and ``restart`` are
   the absence of one. With pushes off the model drops an unprompted message where the code batches it
   (``push_off``); in shadow the code logs a would-push only where pushes on would have sent one, and counts it
   against the day's budget, so it is stricter than the model's ``silent``. Neither ever sends it.

Not generated, each for a stated reason: a Mac act left running while the clock passes its 10-minute ceiling (the
code stops it and holds it in doubt, ``_overdue``; the model has no stop event); "reopen" on a dropped card (the
code allows the typed word, the model's ``reopen`` only takes done or unverified; the Reopen button only exists on
those); the model's ``raise`` (a door raised before a step first ran: the code raises one only through a change
that re-plans, and dangerous goals keep their code door floor); and Reopen while a step of a closed card is
still marked running (see
``test_a_card_closed_during_a_web_step_then_reopened_moves_on``: found as a strict xfail, fixed in on_phase_result).
"""

from __future__ import annotations

import asyncio
import json
import math
import random
import re
from collections import Counter
from dataclasses import dataclass, replace
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Callable, Optional

import pytest

from eve_bridge import chief, chief_receipt, chief_reflect, spend
from eve_bridge.chief import Chief
from eve_bridge.chief_desk import PUSHES_PER_DAY, QUIET_END, QUIET_START, Desk, Item, Mandate, in_quiet
from eve_bridge.chief_ledger import Ledger
from eve_bridge.chief_reflect import Memory, Reflector

LEAN = Path(__file__).resolve().parents[2] / "verification" / "Eve" / "Chief.lean"
LINKS = ["https://desks.example.com/a", "https://desks.example.com/b", "https://shop.example.org/c",
         "https://review.example.net/d"]
SAID = "find me a standing desk under $600 and order it"
DESK_JOB = {
    "kind": "job", "title": "Standing desk", "purpose": "a desk to stand at",
    "end_state": "an order confirmation for one desk under $600",
    "done_checks": [{"kind": "read_links", "arg": "3"}, {"kind": "cited_pick", "arg": None},
                    {"kind": "ui_seen", "arg": "order confirmed"}],
    "never": ["over $600"],
    "phases": [{"do": "research", "goal": "standing desks under $600 with reviews", "door": None},
               {"do": "assess", "goal": "pick one", "door": None},
               {"do": "act", "goal": "order the picked desk", "door": None}],
    "deadline": None, "cue": None, "firm": None, "question": None}


# ---- the constants, read from the .lean file ----------------------------------------------------------------

def _nat(expr: str) -> int:
    """A Lean Nat literal of the form ``22 * 60 + 30``: digits, ``*`` and ``+`` only."""
    if not re.fullmatch(r"[0-9 *+]+", expr):
        raise ValueError(expr)
    return sum(math.prod(int(f) for f in term.split("*")) for term in expr.split("+"))


def lean_constants(text: str) -> dict[str, int]:
    out = {}
    for name in ("QUIET_FROM", "QUIET_TO", "PUSH_BUDGET", "MAX_RETRIES", "OMEGA"):
        m = re.search(rf"^def {name} : Nat := ([0-9 *+]+?)\s*(?:--.*)?$", text, re.M)
        assert m, f"{name} is not in {LEAN.name}"
        out[name] = _nat(m.group(1))
    return out


K = lean_constants(LEAN.read_text(encoding="utf-8"))


def _hhmm(text: str) -> int:
    h, m = text.split(":")
    return int(h) * 60 + int(m)


def test_the_models_constants_are_the_codes() -> None:
    """Chief.lean:109-112 against chief_desk and chief_reflect."""
    assert K == {"QUIET_FROM": _hhmm(QUIET_START), "QUIET_TO": _hhmm(QUIET_END), "PUSH_BUDGET": PUSHES_PER_DAY,
                 "MAX_RETRIES": chief_reflect.MAX_RETRIES, "OMEGA": chief_reflect.OMEGA}
    assert K == {"QUIET_FROM": 1350, "QUIET_TO": 480, "PUSH_BUDGET": 2, "MAX_RETRIES": 2, "OMEGA": 3}


def test_the_models_quiet_hours_are_the_codes_for_every_minute() -> None:
    """Chief.lean:115 ``quiet`` against chief_desk.in_quiet, all 1440 minutes of a day."""
    for m in range(1440):
        ts = datetime(2026, 9, 29, m // 60, m % 60).timestamp()
        assert quiet(m) == in_quiet(Mandate(), ts), m


# ---- the model, transliterated (Chief.lean:107-515) ----------------------------------------------------------

Cost = tuple[int, int]            # (micro-dollars, seconds): Chief.lean:190 ``Cost``
ZERO: Cost = (0, 0)
VERDICT_OF = {"observed": "confirmed", "contradicted": "failed", "executorText": "unverifiable",
              "missing": "unverifiable"}                                   # Chief.lean:361 ``verdictOf``
NAIVE_VERDICT = {**VERDICT_OF, "executorText": "confirmed"}               # Chief.lean:978 ``naiveVerdict``


def quiet(m: int) -> bool:                                                 # Chief.lean:115
    return K["QUIET_FROM"] <= m or m < K["QUIET_TO"]


@dataclass(frozen=True)
class MPhase:                                                              # Chief.lean:224
    kind: str
    door: str                     # "two" | "one"
    st: str = "queued"            # queued | asked | running | ok | failed | doubt
    token: Optional[int] = None
    ask_rev: Optional[int] = None
    attempts: int = 0
    retries: int = 0
    yeses: int = 0
    ow_acts: int = 0
    last_yes: Optional[int] = None
    stale: bool = False
    doubt: bool = False
    replay: bool = False


@dataclass(frozen=True)
class MCard:                                                               # Chief.lean:240
    cap: Cost
    rev: int = 1
    status: str = "active"        # active | unverified | done | dropped
    phases: tuple[MPhase, ...] = ()
    checks: tuple[tuple[str, bool], ...] = ()
    by_owner: bool = False
    running: Optional[int] = None
    held: Cost = ZERO
    spent: Cost = ZERO
    over: Cost = ZERO


@dataclass(frozen=True)
class MDesk:                                                               # Chief.lean:253
    mode: str = "shadow"
    clock: int = 0
    at_break: bool = False
    waited: bool = False
    pushes: int = 0
    route: str = "batch"


@dataclass(frozen=True)
class MS:                                                                  # Chief.lean:262
    card: MCard
    desk: MDesk = MDesk()
    lessons: tuple[int, ...] = ()
    quiet_act: bool = False
    bad_push: bool = False
    sent_today: int = 0


def start(phases: list[tuple[str, str]], checks: int, cap: Cost, mode: str) -> MS:        # Chief.lean:279
    return MS(MCard(cap=cap, phases=tuple(MPhase(k, d) for k, d in phases), checks=(("pending", False),) * checks),
              MDesk(mode=mode))


def upd(f: Callable[[Any], Any], n: int, xs: tuple) -> tuple:              # Chief.lean:285
    return tuple(f(x) if i == n else x for i, x in enumerate(xs))


def _at(xs: tuple, n: int) -> Any:
    return xs[n] if 0 <= n < len(xs) else None


def approve(r: int, p: MPhase) -> MPhase:                                  # Chief.lean:293
    return replace(p, st="queued", ask_rev=None, token=r, yeses=p.yeses + 1, last_yes=r, doubt=False)


def f_ask(r: int, p: MPhase) -> MPhase:                                    # Chief.lean:297
    return replace(p, st="asked", ask_rev=r) if p.door == "one" and p.st == "queued" and p.token is None else p


def f_answer(r: int, a: str, p: MPhase) -> MPhase:                         # Chief.lean:302
    if p.st != "asked":
        return p
    if a == "yes" and p.ask_rev == r and p.door == "one":
        return approve(r, p)
    return replace(p, st="queued", ask_rev=None)


def f_go(r: int, p: MPhase) -> MPhase:                                     # Chief.lean:309
    return approve(r, p) if p.door == "one" and p.st in ("queued", "asked") else p


def f_clear(p: MPhase) -> MPhase:                                          # Chief.lean:313
    return replace(p, token=None, ask_rev=None, st="queued" if p.st == "asked" else p.st)


def f_raise(p: MPhase) -> MPhase:                                          # Chief.lean:317
    return replace(p, door="one") if p.st == "queued" and p.attempts == 0 else p


def can_go(r: int, p: MPhase) -> bool:                                     # Chief.lean:320
    return p.st == "queued" and (p.door == "two" or p.token == r)


def quiet_bars(p: MPhase) -> bool:                                         # Chief.lean:323
    return p.door == "one" or p.kind == "act"


def f_dispatch(r: int, p: MPhase) -> MPhase:                               # Chief.lean:326
    if not can_go(r, p):
        return p
    one = p.door == "one"
    return replace(p, st="running", token=None, attempts=p.attempts + 1, ow_acts=p.ow_acts + (1 if one else 0),
                   stale=p.stale or (one and p.last_yes != r), replay=p.replay or (one and p.doubt))


def f_result(good: bool, p: MPhase) -> MPhase:                             # Chief.lean:334
    return replace(p, st="ok" if good else "failed") if p.st == "running" else p


def f_reflect(retry: bool, p: MPhase) -> MPhase:                           # Chief.lean:339
    if p.st != "failed":
        return p
    if p.door == "one":
        return replace(p, st="queued")
    if retry and p.retries < K["MAX_RETRIES"]:
        return replace(p, st="queued", retries=p.retries + 1)
    return p


def f_requeue(p: MPhase) -> MPhase:                                        # Chief.lean:346
    return replace(p, st="queued") if p.st in ("ok", "failed") else p


def f_restart(p: MPhase) -> MPhase:                                        # Chief.lean:350
    if p.st == "running":
        return replace(p, st="doubt", doubt=True) if p.door == "one" else replace(p, st="queued")
    if p.st == "asked":
        return replace(p, st="queued", ask_rev=None)
    return p


def f_happened(p: MPhase) -> MPhase:                                       # Chief.lean:355
    return replace(p, st="ok") if p.st == "doubt" else p


def f_didnt(p: MPhase) -> MPhase:                                          # Chief.lean:356
    return replace(p, st="queued") if p.st == "doubt" else p


def fits(c: MCard, k: Cost) -> bool:                                       # Chief.lean:367
    return all(c.spent[i] + c.held[i] + k[i] <= c.cap[i] for i in (0, 1))


def charge(c: MCard, k: Cost) -> MCard:                                    # Chief.lean:371
    return replace(c, running=None, held=ZERO, spent=(c.spent[0] + k[0], c.spent[1] + k[1]),
                   over=(c.over[0] + max(0, k[0] - c.held[0]), c.over[1] + max(0, k[1] - c.held[1])))


def on_phase(c: MCard, n: int, f: Callable[[MPhase], MPhase]) -> MCard:   # Chief.lean:378
    return replace(c, phases=upd(f, n, c.phases))


def all_confirmed(checks: tuple) -> bool:                                  # Chief.lean:380
    return bool(checks) and all(v == "confirmed" for v, _ in checks)


def card_step(vf: dict[str, str], ceil: Callable[[str], Cost], q: bool, c: MCard, e: tuple) -> MCard:  # :381
    kind, active = e[0], c.status == "active"
    if kind == "ask":
        return on_phase(c, e[1], lambda p: f_ask(c.rev, p)) if active and not q else c
    if kind == "answer":
        return on_phase(c, e[1], lambda p: f_answer(c.rev, e[2], p)) if active else c
    if kind == "goId":
        return on_phase(c, e[1], lambda p: f_go(c.rev, p)) if active else c
    if kind == "revise":
        return replace(c, rev=c.rev + 1, phases=tuple(map(f_clear, c.phases))) if active else c
    if kind == "raise":
        return on_phase(c, e[1], f_raise) if active else c
    if kind == "dispatch":
        p = _at(c.phases, e[1])
        if p is not None and active and c.running is None and can_go(c.rev, p) and fits(c, ceil(p.kind)) \
                and not (q and quiet_bars(p)):
            return replace(c, phases=upd(lambda x: f_dispatch(c.rev, x), e[1], c.phases), running=e[1],
                           held=ceil(p.kind))
        return c
    if kind == "result":
        return on_phase(charge(c, e[3]), e[1], lambda p: f_result(e[2], p)) if c.running == e[1] else c
    if kind == "reflect":
        return on_phase(c, e[1], lambda p: f_reflect(e[3], p))
    if kind == "requeue":
        return on_phase(c, e[1], f_requeue) if active else c
    if kind == "restart":
        c2 = c if c.running is None else charge(c, e[1])
        return replace(c2, phases=tuple(map(f_restart, c2.phases)))
    if kind == "happened":
        return on_phase(c, e[1], f_happened)
    if kind == "didnt":
        return on_phase(c, e[1], f_didnt)
    if kind == "oracle":
        return replace(c, checks=upd(lambda _: (vf[e[2]], e[2] == "observed"), e[1], c.checks)) if active else c
    if kind == "close":
        if active and c.running is None:
            return replace(c, status="done" if all_confirmed(c.checks) else "unverified")
        return c
    if kind == "ownerDone":
        return replace(c, status="done", by_owner=True) if c.status in ("active", "unverified") else c
    if kind == "reopen":
        if c.status in ("done", "unverified"):
            return replace(c, status="active", rev=c.rev + 1, checks=tuple(("pending", False) for _ in c.checks),
                           by_owner=False, phases=tuple(map(f_clear, c.phases)))
        return c
    if kind == "drop":
        return replace(c, status="dropped", phases=tuple(map(f_clear, c.phases))) if c.status != "dropped" else c
    if kind == "raiseBudget":
        return replace(c, cap=(c.cap[0] + e[1][0], c.cap[1] + e[1][1]))
    assert kind in ("clock", "newDay", "breakpoint", "waited", "busy", "offer"), e
    return c


def interrupting(k: str) -> bool:                                          # Chief.lean:424
    return k in ("unprompted", "reminder")


def offer(d: MDesk, k: str) -> MDesk:                                      # Chief.lean:432
    if k in ("reply", "receipt"):
        return replace(d, route="now")
    if k == "reminder":
        return replace(d, route="now" if not quiet(d.clock) and (d.at_break or d.waited) else "batch")
    if d.mode == "off":
        return replace(d, route="drop")
    if d.mode == "shadow":
        return replace(d, route="silent")
    if not quiet(d.clock) and d.at_break and d.pushes < K["PUSH_BUDGET"]:
        return replace(d, route="now", pushes=d.pushes + 1)
    return replace(d, route="batch")


def desk_step(d: MDesk, e: tuple) -> MDesk:                                # Chief.lean:444
    return {"clock": lambda: replace(d, clock=e[1] % 1440) if e[0] == "clock" else d,
            "newDay": lambda: replace(d, pushes=0), "breakpoint": lambda: replace(d, at_break=True),
            "waited": lambda: replace(d, waited=True),
            "busy": lambda: replace(d, at_break=False), "restart": lambda: replace(d, at_break=False),
            "offer": lambda: offer(d, e[1])}.get(e[0], lambda: d)()


def add_lesson(k: int, xs: tuple[int, ...]) -> tuple[int, ...]:            # Chief.lean:455
    ys = list(xs) + [k]
    if K["OMEGA"] < ys.count(k):
        ys.remove(k)                                                       # List.erase: the first (oldest) one
    return tuple(ys)


def lesson_step(c: MCard, xs: tuple[int, ...], e: tuple) -> tuple[int, ...]:   # Chief.lean:459
    if e[0] == "reflect":
        p = _at(c.phases, e[1])
        if p is not None and p.st == "failed":
            return add_lesson(e[2], xs)
    return xs


def act_started(c: MCard, c2: MCard, e: tuple) -> bool:                    # Chief.lean:468
    if e[0] == "dispatch":
        p = _at(c.phases, e[1])
        return c2.running == e[1] and c.running is None and p is not None and quiet_bars(p)
    if e[0] == "ask":
        before, after = _at(c.phases, e[1]), _at(c2.phases, e[1])
        return after is not None and after.st == "asked" and (before is None or before.st != "asked")
    return False


def pushed_against_rule(d: MDesk, d2: MDesk, e: tuple) -> bool:            # Chief.lean:474
    return e[0] == "offer" and d2.route == "now" and interrupting(e[1]) and (
        quiet(d.clock) or (not d.at_break and not (e[1] == "reminder" and d.waited))
        or (e[1] == "unprompted" and d.mode != "on"))


def sent_now(d2: MDesk, e: tuple) -> int:                                  # Chief.lean:479
    return 1 if e[0] == "offer" and e[1] == "unprompted" and d2.route == "now" else 0


def step_with(vf: dict[str, str], ceil: Callable[[str], Cost], s: MS, e: tuple) -> MS:   # Chief.lean:483
    q = quiet(s.desk.clock)
    c2 = card_step(vf, ceil, q, s.card, e)
    d2 = desk_step(s.desk, e)
    return MS(c2, d2, lesson_step(s.card, s.lessons, e), s.quiet_act or (q and act_started(s.card, c2, e)),
              s.bad_push or pushed_against_rule(s.desk, d2, e),
              0 if e[0] == "newDay" else s.sent_today + sent_now(d2, e))


def run_model(ceil: Callable[[str], Cost], s: MS, events: list[tuple], vf: dict[str, str] = VERDICT_OF) -> MS:
    for e in events:
        s = step_with(vf, ceil, s, e)
    return s


def done_needs_evidence(s: MS) -> bool:                                    # Chief.lean:498
    return s.card.status != "done" or s.card.by_owner or (bool(s.card.checks) and all(o for _, o in s.card.checks))


def spec(s: MS) -> dict[str, bool]:                                        # Chief.lean:498-515
    c, ps = s.card, s.card.phases
    return {
        "DoneNeedsEvidence": done_needs_evidence(s),
        "OneYesOneAct": all(p.ow_acts <= p.yeses and not p.stale for p in ps),
        "BudgetBeforeDispatch": all(c.spent[i] + c.held[i] <= c.cap[i] + c.over[i] for i in (0, 1)),
        "PushBudgetQuiet": (not s.bad_push and not s.quiet_act and s.sent_today <= K["PUSH_BUDGET"]
                            and s.desk.pushes == s.sent_today),
        "NoActReplayAfterRestart": all(not p.replay for p in ps),
        "OneWayNeverAutoRetried": all((p.door != "one" or (p.attempts <= p.yeses and p.retries == 0))
                                      and p.retries <= K["MAX_RETRIES"] for p in ps),
        "LessonsBounded": all(s.lessons.count(k) <= K["OMEGA"] for k in s.lessons),
    }


# ---- the transliteration against Lean's own worked traces (Chief.lean:975-1050) --------------------------------

def ceil0(_: str) -> Cost:                                                 # Chief.lean:984
    return (10, 600)


ORDER_DESK = [("research", "two"), ("assess", "two"), ("act", "one")]      # Chief.lean:987


def order_desk(mode: str = "shadow", phases: Optional[list] = None, checks: int = 3) -> MS:
    return start(phases or ORDER_DESK, checks, (100, 1800), mode)


def test_the_transliteration_reproduces_every_worked_trace_lean_proved() -> None:
    routine = order_desk(phases=[("act", "two")], checks=1)
    dangerous = order_desk(phases=[("act", "one")], checks=1)
    assert run_model(ceil0, routine, [("clock", 600), ("dispatch", 0)]).card.running == 0
    assert run_model(ceil0, dangerous, [("clock", 600), ("dispatch", 0)]).card.running is None
    one = [("research", "two")]
    naive = run_model(ceil0, order_desk(phases=one, checks=1),
                      [("clock", 600), ("dispatch", 0), ("result", 0, True, (3, 60)), ("oracle", 0, "executorText"),
                       ("close",)], NAIVE_VERDICT)
    assert naive.card.status == "done" and not done_needs_evidence(naive)                    # naive_violates
    word = run_model(ceil0, order_desk(phases=one, checks=1),
                     [("clock", 600), ("dispatch", 0), ("result", 0, True, (3, 60)), ("oracle", 0, "executorText"),
                      ("close",)])
    assert word.card.status == "unverified"                                                  # executor_word_is_unverified
    job = run_model(ceil0, order_desk(), [
        ("clock", 600), ("dispatch", 0), ("result", 0, True, (3, 90)), ("oracle", 0, "observed"),
        ("dispatch", 1), ("result", 1, True, (2, 20)), ("oracle", 1, "observed"), ("ask", 2), ("answer", 2, "yes"),
        ("dispatch", 2), ("result", 2, True, (0, 500)), ("oracle", 2, "observed"), ("close",)])
    assert job.card.status == "done" and [p.attempts for p in job.card.phases] == [1, 1, 1] \
        and job.card.spent == (5, 610)                                                       # desk_job_closes
    assert run_model(ceil0, order_desk(), [("clock", 600), ("ask", 2), ("answer", 2, "timeout"),
                                           ("dispatch", 2)]).card.running is None           # timeout_is_no
    assert run_model(ceil0, order_desk(), [("clock", 600), ("ask", 2), ("answer", 2, "yes"), ("revise",),
                                           ("dispatch", 2)]).card.running is None           # revision_cancels_the_yes
    pre = [("clock", 600), ("ask", 2), ("answer", 2, "yes"), ("dispatch", 2), ("restart", (0, 30)), ("didnt", 2),
           ("dispatch", 2)]
    assert run_model(ceil0, order_desk(), pre).card.running is None                          # restart_needs_a_new_yes
    assert run_model(ceil0, order_desk(), pre + [("ask", 2), ("answer", 2, "yes"), ("dispatch", 2)]).card.running == 2
    assert run_model(ceil0, order_desk(), [
        ("clock", 600), ("ask", 2), ("answer", 2, "yes"), ("dispatch", 2), ("result", 2, False, (0, 30)),
        ("reflect", 2, 7, True), ("dispatch", 2)]).card.running is None                     # one_way_failure_waits_for_a_go
    tries = [("dispatch", 0), ("result", 0, False, (1, 10)), ("reflect", 0, 5, True)]
    three = run_model(lambda _: (1, 10), order_desk(), [("clock", 600)] + tries * 3
                      + [("dispatch", 0), ("reflect", 0, 5, True)])
    assert three.card.phases[0].attempts == 3 and three.card.running is None \
        and three.lessons == (5, 5, 5)                                                       # two_way_three_trials
    pushed = run_model(ceil0, order_desk("on"), [
        ("clock", 600), ("breakpoint",), ("offer", "unprompted"), ("offer", "unprompted"), ("restart", (0, 0)),
        ("breakpoint",), ("offer", "unprompted")])
    assert pushed.desk.route == "batch" and pushed.desk.pushes == 2                         # third_push_is_batched
    assert run_model(ceil0, order_desk(), [("clock", 1380), ("breakpoint",),
                                           ("offer", "reminder")]).desk.route == "batch"
    assert run_model(ceil0, order_desk(), [("clock", 1185), ("waited",), ("offer", "reminder")]).desk.route == "now"
    assert run_model(ceil0, order_desk(), [("clock", 1140), ("offer", "reminder")]).desk.route == "batch"
    assert run_model(ceil0, order_desk(), [("clock", 1380), ("waited",),
                                           ("offer", "reminder")]).desk.route == "batch"   # waited_reminder_goes
    assert run_model(ceil0, order_desk(), [("clock", 600), ("ask", 2), ("answer", 2, "yes"), ("clock", 1380),
                                           ("dispatch", 2)]).card.running is None           # no_act_at_night
    for s in (job, word, three, pushed):
        assert all(spec(s).values()), spec(s)
    assert not spec(naive)["DoneNeedsEvidence"]


# ---- the replay against the Python chief ---------------------------------------------------------------------

class Disagreement(AssertionError):
    """The code and the model did not agree."""


def micro(usd: float) -> int:
    return math.ceil(round(usd * 1e6, 3))


def secs_of(s: float) -> int:
    return math.ceil(round(s, 3))


def ceilings(kind: str) -> Cost:
    """chief.CEILINGS in the model's units (the web read's for research: the fakes' router always says web)."""
    c = chief.CEILINGS[kind]
    return micro(c.usd), secs_of(c.secs)


def ui_final(outcome: str) -> tuple[str, list[dict[str, str]], bool]:
    """An act's (final sentence, UI states, ok) for each outcome the fakes produce."""
    return {"good": ("I ordered it.", [{"state": "Thank you! Order confirmed #123"}], True),
            "word": ("I placed the order. Order confirmed.", [], True),
            "bad": ("The payment did not go through.", [{"state": "Payment declined"}], False)}[outcome]


async def settle(rounds: int = 60) -> None:
    for _ in range(rounds):
        await asyncio.sleep(0)


class Replay:
    """One card (the standing-desk job of design 4.1) driven through the real Chief, and the model beside it."""

    def __init__(self, folder: Path, *, vf: dict[str, str] = VERDICT_OF, check_spec: bool = True) -> None:
        self.folder, self.vf, self.check_spec = folder, vf, check_spec
        # this card's spend rows in its own folder: every replay's card is c1, and _resume reads rows by id and
        # date; never the owner's ledger, even run outside pytest's fixtures
        spend.set_dir(folder / "spend")
        self.t = datetime(2026, 9, 29, 10, 0)
        self.futs: dict[str, asyncio.Future[Any]] = {}
        self.ask_fut: Optional[asyncio.Future[str]] = None
        self.act_running = False
        self.latest: dict[int, Optional[str]] = {1: None, 2: None, 3: None}   # what each phase last returned
        self.retry_word, self.reflect_error = True, False
        self.keys: dict[tuple[str, str], int] = {}
        self.seen = 0
        self.rev_offset = 0                     # revisions the code made on a card the model does not revise (doc 5)
        self.fed: list[tuple] = []
        self.stats: Counter[str] = Counter()
        self.chief = self._make()
        self.m: MS

    # -- the fakes --
    def clock(self) -> float:
        return self.t.timestamp()

    def _make(self) -> Chief:
        reflector = Reflector(Memory(self.folder, clock=self.clock), ask=self._reflect, clock=self.clock,
                              offload=False)
        return Chief(ledger=Ledger(self.folder, clock=self.clock),
                     desk=Desk(self.folder, clock=self.clock, mode="shadow"), reflector=reflector,
                     research=self._research, route_research=lambda goal: "web", start_task=self._start_task,
                     mac_free=lambda: True, create=self._create, ask=self._ask, notify=self._notify,
                     clock=self.clock, offload=False, home=self.folder,
                     # Chief.lean has no replan step; the replanner's rules are replayed in test_chief_replan.py
                     environ={"EVE_CHIEF_REPLAN": "off"})

    async def _research(self, query: str) -> dict[str, Any]:
        fut = self.futs["research"] = asyncio.get_running_loop().create_future()
        return await fut

    async def _create(self, payload: dict[str, Any]) -> dict[str, Any]:
        fut = self.futs["assess"] = asyncio.get_running_loop().create_future()
        return await fut

    def _start_task(self, goal: str, *, card_id: str, n: int, floor: Optional[str]) -> dict[str, Any]:
        assert floor == "codex", "an act runs where it can stop and ask"
        self.act_running = True
        return {"ok": True}

    async def _ask(self, text: str) -> str:
        fut = self.ask_fut = asyncio.get_running_loop().create_future()
        return await fut

    async def _notify(self, text: str, about: str) -> bool:
        return True

    def _reflect(self, body: dict[str, Any]) -> dict[str, Any]:
        if self.reflect_error:
            raise RuntimeError("the reflection model is down")
        lesson = {"lesson": "Use the store's own search with the price cap.", "retry": self.retry_word}
        return {"choices": [{"message": {"content": json.dumps(lesson)}}],
                "usage": {"prompt_tokens": 300, "completion_tokens": 40, "cost": 0.0}}

    # -- reading the code --
    def card(self) -> Any:
        return self.chief.ledger.get("c1")

    def events(self) -> list[dict[str, Any]]:
        return self.chief.ledger.events("c1")

    def count(self, kind: str, n: int) -> int:
        return sum(1 for e in self.events() if e.get("e") == kind and e.get("phase") == n)

    def asking(self, n: int, rev: int) -> bool:
        """A Go for phase n is out at this revision and nothing answered it yet."""
        if self.ask_fut is None or self.ask_fut.done():
            return False
        last = None
        for e in self.events():
            if e.get("e") == "asked":
                last = e
            elif e.get("e") == "approved" and last is not None and e.get("phase") == last.get("phase"):
                last = None
        return last is not None and last.get("phase") == n and last.get("rev") == rev

    def truth(self, check: int) -> str:
        """What the oracle for this check could read, from what the fakes returned (the test wrote it)."""
        research, assess, act = self.latest[1], self.latest[2], self.latest[3]
        if check == 0:
            return {None: "missing", "good": "observed", "bad": "contradicted"}[research]
        if check == 1:
            if assess is None:
                return "missing"
            return "observed" if assess == "good" and research == "good" else "contradicted"
        return {None: "missing", "good": "observed", "bad": "contradicted", "word": "executorText",
                "owner": "missing"}[act]

    # -- the model --
    def feed(self, e: tuple) -> None:
        self.m = step_with(self.vf, ceilings, self.m, e)
        self.fed.append(e)
        if self.check_spec:
            broken = [k for k, v in spec(self.m).items() if not v]
            self.expect(not broken, f"the model's Spec fails after {e}: {broken}")

    def expect(self, ok: bool, why: str) -> None:
        if not ok:
            raise Disagreement(f"{why}\nmodel events so far: {self.fed}")

    def would_dispatch(self) -> Optional[int]:
        c = self.m.card
        cand = next((i for i, p in enumerate(c.phases) if p.st != "ok"), None)
        if cand is None:
            return None
        after = card_step(self.vf, ceilings, quiet(self.m.desk.clock), c, ("dispatch", cand))
        return cand if after.running == cand and c.running is None else None

    def charged(self) -> Cost:
        card = self.card()
        k = (micro(card.spent.usd) - self.m.card.spent[0], secs_of(card.spent.wall_s) - self.m.card.spent[1])
        self.expect(k[0] >= 0 and k[1] >= 0, f"the code's spend went down: {card.spent}")
        return k

    def sync(self) -> None:
        """Every ledger line the code wrote since the last sync, as the model's events, in order."""
        events = self.events()
        new, self.seen = events[self.seen:], len(events)
        owner_close = False
        for e in new:
            kind, n = e.get("e"), e.get("phase")
            i = n - 1 if isinstance(n, int) else -1
            if kind == "dispatched":
                self.feed(("dispatch", i))
                self.expect(self.m.card.running == i, f"the code started step {n}; the model does not")
                self.stats[f"dispatch_{self.m.card.phases[i].kind}"] += 1
            elif kind == "asked":
                self.feed(("ask", i))
                self.expect(self.m.card.phases[i].st == "asked", f"the code asked a Go for step {n}; the model "
                                                                 f"does not (quiet: {quiet(self.m.desk.clock)})")
                self.stats["asked"] += 1
            elif kind == "answered":
                a = e.get("answer")
                if a in ("happened", "didnt"):
                    self.feed((a, i))
                    self.stats[a] += 1
                else:
                    self.feed(("answer", i, a))
                    self.stats[f"answer_{a}"] += 1
            elif kind == "approved":
                p = self.m.card.phases[i]
                if e.get("via") == "id":
                    if p.st == "doubt":                    # "go c1" on an act in doubt: It didn't, then go (doc 2)
                        self.feed(("didnt", i))
                        self.stats["go_from_doubt"] += 1
                    self.feed(("goId", i))
                    self.stats["go_by_id"] += 1
                self.expect(self.m.card.phases[i].token == self.m.card.rev,
                            f"the code recorded a yes for step {n} ({e.get('via')}); the model grants none")
            elif kind == "result" and e.get("by") != "owner":
                self.expect(e.get("started") is not False, f"a step did not start: {e}")
                stale = bool(e.get("stale"))
                self.feed(("result", i, e.get("status") == "done", ZERO if stale else self.charged()))
                self.stats["stale_result" if stale else f"result_{e.get('status')}"] += 1
            elif kind == "checked":
                if e.get("by") == "owner":
                    self.feed(("ownerDone",))
                    owner_close = True
                    continue
                ci = e["check"]
                ev = self.truth(ci)
                self.feed(("oracle", ci, ev))
                self.expect(e.get("outcome") == self.vf[ev], f"check {ci} ({e.get('kind')}): the code says "
                                                             f"{e.get('outcome')}, the model {self.vf[ev]} on {ev}")
                self.stats[f"check_{ev}"] += 1
            elif kind == "reflected":
                key = self.keys.setdefault((str(e.get("executor")), str(e.get("do"))), len(self.keys))
                retry = self.retry_word and fits(self.m.card, ceilings(str(e.get("do"))))     # doc 3
                self.feed(("reflect", i, key, retry))
                self.stats["reflected"] += 1
            elif kind == "retried":
                self.stats["retried"] += 1
            elif kind == "revised":
                if self.m.card.status != "active":             # "change c1" on an unverified card (doc 5)
                    self.rev_offset += 1
                    self.stats["revised_closed"] += 1
                self.feed(("revise",))
            elif kind == "reopened":
                self.feed(("reopen",))
                self.stats["reopened"] += 1
            elif kind == "state":
                why = e.get("why")
                if why == "closed":
                    if owner_close:
                        owner_close = False
                    else:
                        self.feed(("close",))
                    self.stats[f"closed_{self.m.card.status}"] += 1
                elif why == "dropped by the owner":
                    self.feed(("drop",))
                    self.stats["dropped"] += 1
                elif why == "reopened" and n is not None and e.get("phase_status") == "queued":
                    self.feed(("requeue", i))
                elif why == "budget raised by the owner":
                    card = self.card()
                    self.feed(("raiseBudget", (micro(card.budget.usd) - self.m.card.cap[0],
                                               card.budget.wall_min * 60 - self.m.card.cap[1])))
                    self.stats["raised"] += 1
                elif why == "over budget":
                    self.stats["over_budget"] += 1
                elif str(why).endswith("may have done it"):
                    self.stats["doubt"] += 1

    def compare(self) -> None:
        """The projection both sides must agree on."""
        card, m = self.card(), self.m.card
        self.expect(m.rev + self.rev_offset == card.rev,
                    f"revision: code {card.rev}, model {m.rev} (+{self.rev_offset})")
        status = {"active": "active", "waiting": "active"}.get(card.status, card.status)
        self.expect(m.status == status, f"status: code {card.status}, model {m.status}")
        for p in card.phases:
            mp = m.phases[p.n - 1]
            self.expect(mp.attempts == self.count("dispatched", p.n),
                        f"step {p.n} attempts: code {self.count('dispatched', p.n)}, model {mp.attempts}")
            self.expect(mp.retries == self.count("retried", p.n),
                        f"step {p.n} retries: code {self.count('retried', p.n)}, model {mp.retries}")
        if status == "active":
            running = [p.n - 1 for p in card.phases if p.status == "running"]
            self.expect((running[0] if running else None) == m.running, f"running: code {running}, model {m.running}")
            doubtful = self.chief._doubtful(card)
            for p in card.phases:
                mp = m.phases[p.n - 1]
                if p.status == "queued":
                    in_doubt = doubtful is not None and doubtful.n == p.n and p.approval is None   # doc 2
                    code = "doubt" if in_doubt else \
                        "asked" if self.asking(p.n, card.rev) else "queued"
                else:
                    code = {"refused": "failed", "skipped": "queued"}.get(p.status, p.status)
                same = code == mp.st or (code == "failed" and mp.st == "queued" and mp.door == "one")   # doc 1
                self.expect(same, f"step {p.n}: code {p.status} ({code}), model {mp.st}")
                a = p.approval
                has = a is not None and a.rev == card.rev and a.token not in self.chief._used
                self.expect(has == (mp.token is not None), f"step {p.n} yes: code {has}, model {mp.token}")
        kept = self.chief.reflector.memory.count()
        model = {key: self.m.lessons.count(i) for key, i in self.keys.items()}
        self.expect({k: v for k, v in kept.items() if v} == {k: v for k, v in model.items() if v},
                    f"lessons per key: code {kept}, model {model}")

    # -- the moves --
    async def begin(self) -> None:
        out = await self.chief.handle("take_on", DESK_JOB, said=SAID, said_ref="2026-09-29 10:00")
        assert out["ok"], out
        card = self.card()
        doors = {"two_way": "two", "one_way": "one"}
        self.m = start([(p.do, doors[p.door]) for p in card.phases], len(card.done_checks),
                       (micro(card.budget.usd), card.budget.wall_min * 60), "shadow")
        self.feed(("clock", self.t.hour * 60 + self.t.minute))
        self.sync()
        self.compare()

    async def tick(self) -> None:
        would = self.would_dispatch()
        before = len([e for e in self.events() if e.get("e") == "dispatched"])
        await self.chief.tick()
        await settle()
        started = [e.get("phase") for e in self.events() if e.get("e") == "dispatched"][before:]
        self.sync()
        self.expect(started == ([would + 1] if would is not None else []),
                    f"dispatch: the code started {started}, the model would start "
                    f"{[would + 1] if would is not None else []}")
        self.compare()

    def running(self) -> Optional[str]:
        """The executor the fakes are holding a step on, if any."""
        for kind in ("research", "assess"):
            fut = self.futs.get(kind)
            if fut is not None and not fut.done():
                return kind
        return "act" if self.act_running else None

    async def finish(self, outcome: str, *, usd: float = 0.01, secs: float = 60.0, retry: bool = True,
                     reflect_error: bool = False) -> None:
        kind = self.running()
        assert kind is not None
        n = {"research": 1, "assess": 2, "act": 3}[kind]
        card = self.card()
        phase = card.phase(n)
        accepted = not card.terminal and phase is not None and phase.status == "running"
        self.retry_word, self.reflect_error = retry, reflect_error
        if accepted:
            self.latest[n] = outcome
        if kind == "research":
            out = ({"ok": True, "answer": "Desk A is $549; Desk B is $499.", "sources": [{"url": u} for u in LINKS],
                    "cost_usd": usd} if outcome == "good" else {"ok": False, "reason": "the search failed"})
            self.futs["research"].set_result(out)
        elif kind == "assess":
            text = json.dumps({"options": [{"name": "Desk A", "price": 549, "why": "sturdy", "link": LINKS[0]}],
                               "pick": 0, "change": "a sale"}) if outcome == "good" else "not the options sheet"
            self.futs["assess"].set_result({"output": [{"type": "message", "content": [
                {"type": "output_text", "text": text}]}], "usage": {"input_tokens": 1000, "output_tokens": 200}})
        else:
            self.act_running = False
            final, ui, ok = ui_final(outcome)
            await self.chief.on_agent_result("c1", 3, final, ui, ok=ok, secs=secs)
        await settle()
        self.sync()
        self.compare()

    async def answer(self, word: str) -> None:
        text = {"yes": "Yes", "no": "No", "timeout": "no (no answer within 180 seconds)", "hold": "maybe later"}[word]
        assert self.ask_fut is not None and not self.ask_fut.done()
        self.ask_fut.set_result(text)
        await settle()
        self.sync()
        self.compare()

    async def word(self, name: str) -> None:
        """An owner's code word or button, as the door calls it."""
        c = self.chief
        if name == "go":
            c.approve("c1")
        elif name == "revise":
            c.change("c1", {"title": f"Standing desk r{self.card().rev + 1}"}, said="make it a standing desk")
        elif name == "drop":
            c.drop("c1")
        elif name == "done":
            await c.done("c1")
        elif name == "reopen":
            c.reopen("c1")
        elif name == "happened":
            if (await c.happened("c1"))["ok"]:
                self.latest[3] = "owner"                    # the owner's word: a result, but no screen state
        elif name == "didnt":
            c.didnt("c1")
        elif name == "raise":
            c.raise_budget("c1")
        await settle()
        self.sync()
        self.compare()

    async def move_clock(self, minute: int) -> None:
        """The next time the clock reads ``minute``: time never goes back (a held step waits for its time)."""
        t = self.t.replace(hour=minute // 60, minute=minute % 60)
        if t <= self.t:
            t += timedelta(days=1)
        if t.date() != self.t.date():
            self.feed(("newDay",))
        self.t = t
        self.feed(("clock", minute))
        self.compare()

    async def restart(self) -> None:
        """The daemon dies and launchd starts it again: the old chief's work is gone, a new one reads the folder."""
        old = self.chief
        for task in list(old._tasks):
            task.cancel()
        await asyncio.gather(*old._tasks, return_exceptions=True)
        self.futs.clear()
        self.ask_fut = None
        self.act_running = False
        self.chief = self._make()
        self.feed(("restart", ZERO))
        self.stats["restart"] += 1
        await self.tick()


MINUTES = (600, 1380, 30, 480, 479, 1349, 1350, 610)


async def random_trace(r: Replay, rng: random.Random, moves: int) -> None:
    await r.begin()
    for _ in range(moves):
        card = r.card()
        options: list[tuple[str, int]] = [("tick", 30), ("restart", 3)]
        if r.running():
            options.append(("finish", 30))
        if r.ask_fut is not None and not r.ask_fut.done():
            options.append(("answer", 18))
        if not card.terminal:
            options += [("go", 4), ("revise", 2), ("drop", 1), ("done", 1), ("raise", 4 if card.waiting_for ==
                                                                              "budget" else 1)]
            doubtful = r.chief._doubtful(card)
            if doubtful is not None and doubtful.approval is None:                              # doc 2
                options += [("happened", 5), ("didnt", 5)]
        if card.status in ("done", "unverified") and not any(p.status == "running" for p in card.phases):
            options.append(("reopen", 6))
        if r.running() != "act":
            options.append(("clock", 6))
        move = rng.choices([o for o, _ in options], [w for _, w in options])[0]
        if move == "tick":
            await r.tick()
        elif move == "restart":
            await r.restart()
        elif move == "finish":
            kind = r.running()
            outcome = rng.choices(["good", "bad", "word"], [60, 25, 15 if kind == "act" else 0])[0]
            await r.finish(outcome, usd=rng.choice((0.01, 0.2, 0.45)), secs=rng.choice((30, 240, 700, 1300)),
                           retry=rng.random() < 0.75, reflect_error=rng.random() < 0.1)
        elif move == "answer":
            await r.answer(rng.choices(["yes", "no", "timeout", "hold"], [70, 10, 10, 10])[0])
        elif move == "clock":
            await r.move_clock(rng.choice(MINUTES))
        else:
            await r.word(move)


SEEDS = range(240)
MOVES = 40


@pytest.mark.parametrize("seed", SEEDS)
def test_generated_traces_the_code_and_the_model_agree(tmp_path: Path, seed: int) -> None:
    r = Replay(tmp_path)
    asyncio.run(random_trace(r, random.Random(seed), MOVES))
    _COVERAGE.update(r.stats)
    _RAN.add(seed)


_COVERAGE: Counter[str] = Counter()
_RAN: set[int] = set()


def test_the_generated_traces_reached_every_part_of_the_machine(tmp_path: Path) -> None:
    """A positive control on the generator: the traces above went through every rule they are meant to test.
    Runs after them (pytest keeps file order); when they did not all run here, it runs them itself."""
    if _RAN != set(SEEDS):
        _COVERAGE.clear()
        for seed in SEEDS:
            folder = tmp_path / str(seed)
            folder.mkdir()
            r = Replay(folder)
            asyncio.run(random_trace(r, random.Random(seed), MOVES))
            _COVERAGE.update(r.stats)
    need = ["dispatch_research", "dispatch_assess", "dispatch_act", "asked", "answer_yes", "answer_no",
            "answer_timeout", "answer_hold", "go_by_id", "result_done", "result_failed", "result_handed_on",
            "stale_result", "check_observed", "check_contradicted", "check_executorText", "check_missing",
            "reflected", "retried", "closed_done", "closed_unverified", "dropped", "reopened", "restart", "doubt",
            "happened", "didnt", "over_budget", "raised", "go_from_doubt", "revised_closed"]
    missing = [k for k in need if not _COVERAGE[k]]
    assert not missing, (missing, dict(_COVERAGE))


def test_the_whole_job_of_design_4_1_agrees_step_by_step(tmp_path: Path) -> None:
    r = Replay(tmp_path)

    async def main() -> None:
        await r.begin()
        await r.tick()                                  # research
        await r.finish("good")
        await r.tick()                                  # assess
        await r.finish("good")
        await r.tick()                                  # the Go is asked
        await r.answer("yes")
        await r.tick()                                  # the act, on codex
        await r.finish("good", secs=240)
        await r.tick()                                  # closes

    asyncio.run(main())
    assert r.card().status == "done" and r.m.card.status == "done" and not r.m.card.by_owner
    assert [p.attempts for p in r.m.card.phases] == [1, 1, 1]


def test_night_holds_the_go_and_the_act_on_both_sides(tmp_path: Path) -> None:
    r = Replay(tmp_path)

    async def main() -> None:
        await r.begin()
        await r.tick()
        await r.finish("good")
        await r.tick()
        await r.finish("good")
        await r.move_clock(1380)                        # 23:00
        await r.tick()                                  # no Go asked
        assert r.ask_fut is None
        await r.move_clock(610)                         # 10:10 the next day
        await r.tick()
        await r.answer("yes")
        await r.move_clock(1350)                        # 22:30: quiet again
        await r.tick()                                  # the approved act waits
        assert r.m.card.running is None and not r.act_running
        await r.move_clock(480)                         # 08:00
        await r.tick()
        assert r.act_running and r.m.card.running == 2

    asyncio.run(main())


def _executor_word_trace(r: Replay) -> Any:
    async def main() -> None:
        await r.begin()
        await r.tick()
        await r.finish("good")
        await r.tick()
        await r.finish("good")
        await r.tick()
        await r.answer("yes")
        await r.tick()
        await r.finish("word")                          # the executor says "Order confirmed."; no screen showed it
        await r.tick()
    return main()


def test_the_code_on_the_executors_word_agrees_the_card_is_unverified(tmp_path: Path) -> None:
    r = Replay(tmp_path)
    asyncio.run(_executor_word_trace(r))
    assert r.card().status == "unverified" and r.m.card.status == "unverified"


def _trust_the_executor(monkeypatch: pytest.MonkeyPatch) -> None:
    """The variant: the act adapter takes the executor's own sentence for a screen state."""
    real = chief_receipt.from_agent

    def trusting(final: str, ui_evidence: Any, **kw: Any) -> Any:
        return real(final, [{"state": final}, *(ui_evidence or [])], **kw)

    monkeypatch.setattr(chief_receipt, "from_agent", trusting)


def test_positive_control_a_variant_that_closes_on_the_executors_word_disagrees(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    _trust_the_executor(monkeypatch)
    r = Replay(tmp_path)
    with pytest.raises(Disagreement, match=r"check 2 \(ui_seen\): the code says confirmed, the model unverifiable"):
        asyncio.run(_executor_word_trace(r))


def test_positive_control_the_variant_agrees_with_the_naive_oracle_and_breaks_done_needs_evidence(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """The same variant against Chief.lean's ``naiveVerdict``: no disagreement, the card closes done, and the
    model's end state breaks DoneNeedsEvidence, as ``naive_violates`` proves."""
    _trust_the_executor(monkeypatch)
    r = Replay(tmp_path, vf=NAIVE_VERDICT, check_spec=False)
    asyncio.run(_executor_word_trace(r))
    assert r.card().status == "done" and r.m.card.status == "done"
    assert not done_needs_evidence(r.m)


def test_a_card_closed_during_a_web_step_then_reopened_moves_on(tmp_path: Path) -> None:
    r = Replay(tmp_path)

    async def main() -> None:
        await r.begin()
        await r.tick()                                  # research starts
        await r.word("done")                            # the owner closes it meanwhile
        await r.finish("good")                          # the read ends: a stale result
        await r.word("reopen")
        await r.tick()                                  # the model runs assess; the code does not move

    asyncio.run(main())


# ---- the desk (PushBudgetQuiet) ------------------------------------------------------------------------------

DESK_KINDS = {"unprompted": ("warn", "close", "sheet"), "reminder": ("reminder",), "reply": ("reply",),
              "receipt": ("receipt",)}


def desk_replay(folder: Path, mode: str, rng: random.Random, moves: int) -> Counter[str]:
    """Offers, breakpoints, busy spells, restarts, the clock and new days, through the model's ``offer`` and a
    chief_desk.Desk reloaded from the same folder at each offer (doc 6)."""
    s = order_desk(mode)
    day = datetime(2026, 9, 29)
    minute = 600
    s = step_with(VERDICT_OF, ceil0, s, ("clock", minute))
    seen: Counter[str] = Counter()
    for _ in range(moves):
        move = rng.choices(["clock", "newDay", "breakpoint", "busy", "restart", "offer"], [8, 3, 10, 5, 3, 25])[0]
        if move == "clock":
            minute = rng.choice(MINUTES + (1200, 900, 1439, 0))
            e: tuple = ("clock", minute)
        elif move == "newDay":
            day += timedelta(days=1)
            e = ("newDay",)
        elif move == "offer":
            e = ("offer", rng.choice(list(DESK_KINDS)))
        else:
            e = (move,) if move != "restart" else ("restart", ZERO)
        before = s
        s = step_with(VERDICT_OF, ceil0, s, e)
        broken = [k for k, v in spec(s).items() if not v]
        assert not broken, (e, broken)
        if e[0] != "offer":
            continue
        now = (day + timedelta(minutes=minute)).timestamp()
        desk = Desk(folder, mode=mode, clock=lambda now=now: now)
        if before.desk.at_break:
            desk.breakpoint("task_end", now)
        kind = rng.choice(DESK_KINDS[e[1]])
        item = Item(kind, "", queued_at=now)
        route = desk.offer(item, now)
        want = s.desk.route
        seen[f"{e[1]}_{want}"] += 1
        where = f"{mode} {e[1]}/{kind} at {minute} (break {before.desk.at_break}): code {route}, model {want}"
        assert (route.route == "now") == (want == "now"), where
        if e[1] != "unprompted" or mode == "on":
            assert route.route == want, where
        elif mode == "off":
            assert want == "drop" and (route.route, route.rule) == ("batch", "push_off"), where
        else:
            assert route.route in ("silent", "batch") and want == "silent", where
        pushes = desk.pushes_today(now)
        assert pushes <= K["PUSH_BUDGET"], where
        if mode == "on":
            assert pushes == s.desk.pushes == s.sent_today, (where, pushes, s.desk.pushes)
    return seen


@pytest.mark.parametrize("mode", ["off", "shadow", "on"])
@pytest.mark.parametrize("seed", range(12))
def test_the_desk_and_the_model_agree_on_every_offer(tmp_path: Path, mode: str, seed: int) -> None:
    seen = desk_replay(tmp_path, mode, random.Random(1000 + seed), 120)
    assert sum(seen.values()) > 0


def test_the_desk_traces_reach_now_and_batch_for_each_interrupting_kind(tmp_path: Path) -> None:
    seen: Counter[str] = Counter()
    for seed in range(12):
        folder = tmp_path / str(seed)
        folder.mkdir()
        seen.update(desk_replay(folder, "on", random.Random(1000 + seed), 120))
    for key in ("unprompted_now", "unprompted_batch", "reminder_now", "reminder_batch", "reply_now", "receipt_now"):
        assert seen[key], (key, dict(seen))
