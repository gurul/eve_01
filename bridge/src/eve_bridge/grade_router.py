"""How much model an app turn gets: Jev grades the owner's message instant or think.

Why (2026-10-08): the owner moved to texting eve in Eve Link and asked that "jev routes request to best grade so
that we have instantaneous when we need it and the snappy stuff we have already built in", with thinking done by
gpt-6.1-sol. Measured the same day on one easy message: gpt-6-luna at low effort answered in 1.0-1.4 s,
gpt-6.1-sol in 2.7-3.6 s with one run at 12.9 s; on a hard money question sol took 11.8 s at medium effort and
16.8 s at high. Sol on every turn would make easy chat two to three times slower; luna on every turn answers hard
questions without thinking. So each turn is graded:

* ``instant``: today's chat turn (the brain's model, gpt-6-luna at low effort), with every fast path before it
  (quick answers by code, reflexes, the fast lane).
* ``think``: the turn runs on the think model at the think effort (EVE_APP_THINK_MODEL at
  EVE_APP_THINK_EFFORT, as the explicit think grade does); the model spends reasoning tokens only as the turn
  needs them (the probe: 0 to 20 on a trivial question).

Quick answers by code still answer first in both grades: a grade is only asked for a turn that reaches the model.

Jev is asked the way toolset_router.py asks it: the state is the message, the owner's previous message and one line
about what the text is; one absolute Noul ("must the assistant think it through ...") written as the job in the
owner's words, naming no wrong thing (the owner's rule for prompts). Code owns the rule: think when the Noul clears
the fitted gate. An error, a timeout or a missing key is ``instant``: today's behaviour. It is a separate call from
the toolset router's, run beside it and the prompt's building, so it usually adds no wall time (when the toolset
router is off or keywords-only, the turn can wait up to TIMEOUT_S for it) and leaves that router's fitted gates as
they were.

Fitting (tools/grade_eval.py): live Jev runs once over the tuning set (grade_tuning.json, 220 cases, writer A;
answers cached in grade_tuning_answers.json), and the HIGHEST gate on GRID is kept whose think recall on the tuning
set's think-only cases is >= MIN_TUNING_RECALL. Then live Jev runs ONCE over the blind holdout (grade_holdout.json,
100 cases, writer B, sha256 committed in 4b76703 before this module existed) and is scored against the bar below.
Nothing is refitted after the holdout.

PRE-REGISTERED BAR (written 2026-10-08, before any Jev answer on either set existed):
  (1) think recall: think-only holdout cases graded think        >= 0.90
  (2) over-think: instant-only holdout cases graded think        <= 0.15
  (3) Jev latency p95                                            <= 800 ms
  (4) errors and timeouts                                        <= 2% of cases
SHIPPED is True only if all four hold. Ambiguous cases (both acceptable) count in no gate.

``EVE_GRADE_ROUTER`` = auto (the default: on when SHIPPED) | on (a live trial before or despite the bar) |
off (every auto turn is instant). Jev is a third party: with it on, the owner's app message and the previous one
leave the Mac on every auto app turn, as they already do for the toolset router.
"""

from __future__ import annotations

import json
import logging
import os
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Mapping, Optional

log = logging.getLogger(__name__)

Predict = Callable[[Any, dict[str, Any]], dict[str, Any]]

INSTANT, THINK = "instant", "think"
GRADES = (INSTANT, THINK)
MODES = ("auto", "on", "off")
TIMEOUT_S = 0.8                      # the toolset router's budget: past it the turn is instant, today's behaviour
GRID = tuple(round(0.05 * i, 2) for i in range(1, 20))      # 0.05 .. 0.95
MIN_TUNING_RECALL = 0.95
DEFAULT_GATE = 0.5                   # used only until grade_gate.json exists
GATE_FILE = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "grade_gate.json"
SHIPPED = True                       # holdout 2026-10-08: recall 0.943, over-think 0.0, p95 482 ms, errors 0 (passed)

ABOUT = ("A message the owner texted their personal assistant from their phone. When a previous message is given, "
         "the new one may follow it: judge what the new message asks for, reading the previous one only as context.")

NOUL = ("To answer this well, must the assistant think it through before replying: plan several steps, work out "
        "money, dates or schedules under several constraints, weigh options with trade-offs, debug or write code, "
        "write something careful for another person to read, or give advice where a quick wrong answer would cost "
        "the owner? No when a short reply right away is the right answer: small talk, thanks, a greeting, a quick "
        "fact, or a simple request to the assistant's own tools such as music, lights, a timer, a reminder, today's "
        "calendar or opening an app.")


def questions() -> dict[str, Any]:
    return {THINK: {"type": "noul", "instructions": NOUL}}


def state(text: str, previous: str = "") -> dict[str, str]:
    return {"about": ABOUT, "previous": " ".join(str(previous or "").split())[:500],
            "request": " ".join(str(text or "").split())[:500]}


@dataclass(frozen=True)
class GradeAnswer:
    noul: float = 0.0                # Jev's Noul for THINK; 0.0 when absent
    ms: float = 0.0
    error: str = ""

    def to_json(self) -> dict[str, Any]:
        return {"noul": round(self.noul, 4), "ms": round(self.ms, 1), "error": self.error}

    @staticmethod
    def from_json(raw: Mapping[str, Any]) -> "GradeAnswer":
        return GradeAnswer(float(raw.get("noul") or 0.0), float(raw.get("ms") or 0.0), str(raw.get("error") or ""))


def ask(predict: Predict, text: str, previous: str, clock: Callable[[], float]) -> GradeAnswer:
    """Jev's Noul for one message. A failure is an answer with ``error`` set: it grades instant."""
    t0 = clock()
    try:
        answers = predict(state(text, previous), questions()).get("answers") or {}
    except Exception as e:  # noqa: BLE001 — a model that fails abstains: the turn is instant, as before
        return GradeAnswer(0.0, (clock() - t0) * 1000.0, f"{type(e).__name__}: {e}"[:160])
    a = answers.get(THINK)
    v = a.get("noul") if isinstance(a, dict) else None
    noul = float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else 0.0
    return GradeAnswer(noul, (clock() - t0) * 1000.0)


def decide(a: GradeAnswer, gate: float, budget_ms: float = float("inf")) -> str:
    """THINK when the Noul clears the gate; INSTANT on an error or an answer slower than the turn waits."""
    if a.error or a.ms > budget_ms:
        return INSTANT
    return THINK if a.noul >= gate else INSTANT


def fit(answers: list[GradeAnswer], acceptable: list[list[str]]) -> tuple[float, float]:
    """(gate, tuning think recall): the highest GRID gate whose recall on think-only cases is >= MIN_TUNING_RECALL;
    the lowest GRID gate when none reaches it (then the recall says by how much it missed)."""
    think_only = [a for a, ok in zip(answers, acceptable, strict=True) if ok == [THINK]]
    if not think_only:
        raise ValueError("the tuning set has no think-only case")
    best = (GRID[0], 0.0)
    for gate in GRID:
        rec = sum(decide(a, gate) == THINK for a in think_only) / len(think_only)
        if rec >= MIN_TUNING_RECALL:
            best = (gate, rec)
    if best[1] == 0.0:
        best = (GRID[0], sum(decide(a, GRID[0]) == THINK for a in think_only) / len(think_only))
    return best


def bar(grades: list[str], acceptable: list[list[str]], answers: list[GradeAnswer]) -> dict[str, Any]:
    """The pre-registered bar, exactly (the module's text)."""
    think_only = [g for g, ok in zip(grades, acceptable, strict=True) if ok == [THINK]]
    instant_only = [g for g, ok in zip(grades, acceptable, strict=True) if ok == [INSTANT]]
    recall = sum(g == THINK for g in think_only) / len(think_only) if think_only else 0.0
    over = sum(g == THINK for g in instant_only) / len(instant_only) if instant_only else 1.0
    ms = sorted(a.ms for a in answers if not a.error)
    p95 = ms[min(len(ms) - 1, int(0.95 * len(ms)))] if ms else float("inf")
    errors = sum(1 for a in answers if a.error) / len(answers) if answers else 1.0
    out = {"n": len(grades), "think_only": len(think_only), "instant_only": len(instant_only),
           "recall": round(recall, 3), "1": recall >= 0.90, "over_think": round(over, 3), "2": over <= 0.15,
           "p95_ms": round(p95, 1), "3": p95 <= 800.0, "errors": round(errors, 3), "4": errors <= 0.02}
    out["shipped"] = all(out[k] for k in ("1", "2", "3", "4"))
    return out


def load_gate(path: Path = GATE_FILE) -> float:
    try:
        return float(json.loads(path.read_text(encoding="utf-8"))["gate"])
    except Exception:  # noqa: BLE001
        return DEFAULT_GATE


def mode(environ: Optional[Mapping[str, str]] = None) -> str:
    env = os.environ if environ is None else environ
    raw = (env.get("EVE_GRADE_ROUTER") or "auto").strip().lower()
    if raw not in MODES:
        log.warning("grade router: EVE_GRADE_ROUTER=%r is not one of %s; auto", raw, MODES)
        raw = "auto"
    return raw


@dataclass(frozen=True)
class Grade:
    grade: str = INSTANT
    noul: float = 0.0
    ms: float = 0.0
    asked: bool = False
    error: str = ""

    def log_line(self) -> str:
        """Numbers only, never the message."""
        if not self.asked:
            return f"{self.grade}; jev off"
        return f"{self.grade} (noul {self.noul:.2f}); jev {self.ms:.0f} ms" + (f" ({self.error})" if self.error else "")


_JEV: dict[str, Any] = {}
_JEV_LOCK = threading.Lock()


def make_grader(environ: Optional[Mapping[str, str]] = None) -> Optional[Callable[[str, str], GradeAnswer]]:
    """Jev over EVE_JEV_ROUTE, made once a process, or None (off, auto while not SHIPPED, or no Jev route)."""
    from . import jev

    env = os.environ if environ is None else environ
    m = mode(env)
    if m == "off" or (m == "auto" and not SHIPPED):
        return None
    try:
        url, key, model = jev.route_config(env)
    except Exception:  # noqa: BLE001
        return None
    with _JEV_LOCK:
        cached = _JEV.get("grader")
        if cached is None or _JEV.get("route") != (url, model):
            predict = jev.make_predict(url, key, model, timeout_s=TIMEOUT_S)
            cached = lambda text, previous="": ask(predict, text, previous, time.perf_counter)  # noqa: E731
            _JEV.update(grader=cached, route=(url, model))
        return cached


def grade_turn(text: str, previous: str = "", grader: Optional[Callable[[str, str], GradeAnswer]] = None,
               gate: Optional[float] = None, timeout_s: float = TIMEOUT_S) -> Grade:
    """The grade of one auto turn. Blocking (call it with asyncio.to_thread). Jev runs in a worker waited on for
    ``timeout_s`` at most; a grader that is None, raises, errs or is slower grades instant, today's behaviour."""
    if grader is None:
        return Grade()
    box: dict[str, GradeAnswer] = {}
    done = threading.Event()
    t0 = time.perf_counter()

    def run() -> None:
        try:
            box["a"] = grader(text, previous)
        except Exception as e:  # noqa: BLE001
            box["a"] = GradeAnswer(error=type(e).__name__)
        finally:
            done.set()

    threading.Thread(target=run, name="grade-router", daemon=True).start()
    if not done.wait(max(0.0, timeout_s)):
        return Grade(INSTANT, 0.0, (time.perf_counter() - t0) * 1000.0, True, "timeout")
    a = box.get("a")
    if not isinstance(a, GradeAnswer):
        return Grade(INSTANT, 0.0, (time.perf_counter() - t0) * 1000.0, True, "not an answer")
    g = decide(a, load_gate() if gate is None else gate)
    return Grade(g, a.noul, a.ms, True, a.error)
