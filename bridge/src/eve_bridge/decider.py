"""A typed-decision model behind the fast lane's `model` mode, asked as ONE `choice` question.

`Decider(predict)` wraps any callable with the System One signature — `predict(state,
questions) -> {"answers": {qid: {...}}, "usage": {...}}` — and `choose()` asks it exactly
one thing: which of these options advances the objective now. It hands back a `Choice`
that the lane gates on `p_top` and `margin` (from the probabilities), never on the answer's
`confidence` (a normalized entropy, not comparable across option counts). Every judgement
that matters — done, risk, repeat, dialog — is a code oracle in fast_lane.py; the model
only ranks.

The one backend the worker loads is TypeSafe's hosted Jev (jev.load builds a Decider over
jev.make_predict). The seam was written first for a local encoder with a 256-token decision
head, which is why the wrapper still respects a head budget BEFORE predict: every option
costs at most 48 tokens, and `choose` trims the last non-reserved option until the head
fits, counting what it dropped in `dropped_for_budget`; a state that fills the sequence
(usage.input_tokens == max_len) is flagged `overflow`. jev.py sets both budgets past any
real menu, so for Jev the trim never drops an option. The reserved options "reobserve" and
"abstain" are never dropped — the caller adds them, the gate keys on them.

How an option is worded decides more than the style (select fixtures, 73 cases,
2026-09-21): "click radio button: Week" with the full context lines scored top-1 0.315;
`render_option` — "Week (radio button)", "Month (radio button, selected)" — with a
"title: …" context scored 0.603. The lane, the planner's outline and the evals all render
through `render_option`.

A lock serializes every predict, because the worker loads on one thread and serves on
another.
"""

from __future__ import annotations

import math
import re
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Mapping, Optional, Sequence

STYLES = ("jev", "compact", "hinted")
RESERVED = ("reobserve", "abstain")
QUESTION_ID = "pick"
OPTION_TOKEN_CAP = 48          # a marker token + at most 48 option tokens per option in the head
HEAD_RESERVE = 16              # head tokens kept free past the options, or every option gets cut
TIE_TOLERANCE = 1e-4
STOP_WORDS = frozenset({"the", "and", "for", "with", "into", "onto", "from", "this", "that", "please",
                        "now", "then", "one", "all", "any", "its", "your", "our"})
INSTRUCTIONS = {
    "jev": "Which single element should be acted on next to accomplish the goal? Goal: {goal}",
    "compact": "which one action advances the goal now? reobserve if the screen is changing; abstain if none does",
    "hinted": "which one action advances the goal now? reobserve if the screen is changing; abstain if none does",
}
# The warm-up menu (jev.load): 12 options and a ~250-token state, the shape a real step takes.
WARMUP_OBJECTIVE = "switch to week view"
WARMUP_OPTIONS = {str(i + 1): f"click {role}: {label}" for i, (role, label) in enumerate((
    ("radio button", "Day"), ("radio button", "Week"), ("radio button", "Month"), ("radio button", "Year"),
    ("button", "Today"), ("button", "previous month"), ("button", "next month"), ("button", "Add Event"),
    ("search field", "Search"), ("button", "Inspector")))}
WARMUP_OPTIONS.update({"reobserve": "the screen is still changing, look again",
                       "abstain": "none of these advances the objective"})
WARMUP_CONTEXT = ("title: September 2026; focused: radio button Month; text: " + " ".join(
    f"{day} {n}" for n, day in enumerate(("Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
                                          "Sunday") * 6, start=1)))


@dataclass(frozen=True)
class Choice:
    """One answer. `id` is a key of the options, or "" when the answer was rejected (see `error`)."""

    id: str
    p_top: float = 0.0
    margin: float = 0.0
    confidence: float = 0.0            # the model's normalized entropy — LOGGING ONLY
    probabilities: dict[str, float] = field(default_factory=dict)
    ms: float = 0.0
    input_tokens: int = 0
    k: int = 0
    overflow: bool = False             # the state was right-truncated (usage.input_tokens == max_len)
    dropped_for_budget: int = 0        # options removed by the head-budget trim before predict
    error: str = ""


def objective_tokens(text: str) -> set[str]:
    """The words of an objective worth matching: ≥ 3 chars, case-folded, stop-words removed."""
    return {w for w in re.findall(r"[a-z0-9]+", text.casefold()) if len(w) >= 3 and w not in STOP_WORDS}


OPTION_LABEL_CHARS = 40


def render_option(c: Any) -> str:
    """The option text the model sees for a control: `label (role)` or `label (role, value)`,
    e.g. "Week (radio button)", "Month (radio button, selected)". The one place the lane and
    the eval render a candidate (the module docstring has the numbers behind the wording)."""
    label = " ".join(str(getattr(c, "label", "") or "").split())[:OPTION_LABEL_CHARS]
    role = " ".join(str(getattr(c, "role", "") or "").split())
    value = " ".join(str(getattr(c, "value", "") or "").split())
    inner = f"{role}, {value}" if value else role
    return f"{label} ({inner})" if inner else label


def _hint_keys(objective: str, options: Mapping[str, str]) -> list[str]:
    words = objective_tokens(objective)
    if not words:
        return []
    return [k for k, text in options.items() if k not in RESERVED and words & objective_tokens(text)]


def render_instructions(style: str, objective: str, options: Mapping[str, str]) -> str:
    """The instruction sentence of the one `choice` question, per style."""
    if style not in STYLES:
        raise ValueError(f"style must be one of {STYLES}, not {style!r}")
    text = INSTRUCTIONS[style].format(goal=" ".join(objective.split()))
    if style == "hinted":
        keys = _hint_keys(objective, options)
        if keys:
            text += " matches: " + ", ".join(keys)
    return text


def render_question(style: str, objective: str, options: Mapping[str, str]) -> dict[str, Any]:
    """The question dict for `choose`, so the eval can print exactly what the model was asked."""
    return {"type": "choice", "instructions": render_instructions(style, objective, options),
            "criteria": dict(options)}


def word_count(text: str) -> int:
    """The token estimate used when no tokenizer is injected (tests, fakes)."""
    return len(text.split())


def instruction_tokens(style: str, objective: str = "", options: Optional[Mapping[str, str]] = None,
                       tokenize: Optional[Callable[[str], int]] = None) -> int:
    """Tokens the head spends before the options: "choice question: <instructions>"."""
    count = tokenize or word_count
    return count("choice question: " + render_instructions(style, objective, options or {}))


def option_tokens(text: str, tokenize: Optional[Callable[[str], int]] = None) -> int:
    """Tokens one option costs in the head: a marker token plus at most 48 text tokens."""
    count = tokenize or word_count
    return 1 + min(OPTION_TOKEN_CAP, count(" " + text))


def trim_options(options: Mapping[str, str], budget: int,
                 tokenize: Optional[Callable[[str], int]] = None) -> tuple[dict[str, str], int]:
    """Drop the LAST non-reserved options until the rendered options fit `budget` tokens.

    Options arrive in rank order, so the least likely go first. The reserved two are
    never dropped, and at least one real option always stays (the caller sees the count
    in dropped_for_budget). Returns
    (kept options in their original order, dropped count).
    """
    kept = dict(options)
    costs = {k: option_tokens(v, tokenize) for k, v in kept.items()}
    dropped = 0
    real = [k for k in kept if k not in RESERVED]
    while sum(costs[k] for k in kept) > budget and len(real) > 1:
        gone = real.pop()
        del kept[gone]
        dropped += 1
    return kept, dropped


def _finite_unit(value: Any) -> Optional[float]:
    """`value` as a float in [0, 1], or None when it is not one."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    f = float(value)
    if not math.isfinite(f) or f < 0.0 or f > 1.0:
        return None
    return f


class Decider:
    """A typed-decision model behind an injectable `predict(state, questions) -> result dict`.

    `available` turns True once a warm-up answer came back (jev.load sets it)."""

    def __init__(
        self,
        predict: Callable[[Any, dict[str, Any]], dict[str, Any]],
        *,
        tokenize: Optional[Callable[[str], int]] = None,
        max_len: int = 1024,
        head_max_len: int = 256,
        clock: Callable[[], float] = time.perf_counter,
        style: str = "compact",
    ) -> None:
        if style not in STYLES:
            raise ValueError(f"style must be one of {STYLES}, not {style!r}")
        self._predict = predict
        self._tokenize = tokenize
        self.max_len = int(max_len)
        self.head_max_len = int(head_max_len)
        self._clock = clock
        self.style = style
        self.available = False          # True only after a loader's warm-up (jev.load)
        self.load_ms = 0.0
        self.warm_ms = 0.0
        self._lock = threading.Lock()

    # -- the one question --
    def choose(self, objective: str, *, app: str, context: str, options: Mapping[str, str],
               recent: Sequence[str] = ()) -> Choice:
        """Which option advances the objective now. Never raises for a model failure: see `error`."""
        if not options or any(not isinstance(k, str) or not isinstance(v, str) for k, v in options.items()):
            raise ValueError("options must be a non-empty {str: str} mapping")
        if any(key not in options for key in RESERVED):
            raise ValueError(f"options must include the reserved keys {RESERVED}")
        if len(options) == len(RESERVED):
            raise ValueError("options must include at least one real option")
        head = instruction_tokens(self.style, objective, options, self._tokenize)
        budget = self.head_max_len - HEAD_RESERVE - head
        kept, dropped = trim_options(options, budget, self._tokenize)
        question = render_question(self.style, objective, kept)
        state = {"goal": objective, "app": app, "context": context, "recent": list(recent)}
        t0 = self._clock()
        try:
            with self._lock:
                result = self._predict(state, {QUESTION_ID: question})
        except Exception as e:  # noqa: BLE001 — the lane must see "unavailable", never a traceback
            return Choice("", ms=(self._clock() - t0) * 1000.0, k=len(kept), dropped_for_budget=dropped,
                          error=f"predict raised {type(e).__name__}: {e}"[:200])
        ms = (self._clock() - t0) * 1000.0
        return self._parse_choice(result, kept, ms, dropped)

    def _parse_choice(self, result: Any, options: dict[str, str], ms: float, dropped: int) -> Choice:
        k = len(options)

        def rejected(why: str, **extra: Any) -> Choice:
            return Choice("", ms=ms, k=k, dropped_for_budget=dropped, error=why, **extra)

        if not isinstance(result, dict):
            return rejected("predict returned no dict")
        usage = result.get("usage") if isinstance(result.get("usage"), dict) else {}
        tokens = usage.get("input_tokens")
        input_tokens = int(tokens) if isinstance(tokens, int) and not isinstance(tokens, bool) else 0
        overflow = input_tokens >= self.max_len
        answers = result.get("answers")
        answer = answers.get(QUESTION_ID) if isinstance(answers, dict) else None
        if not isinstance(answer, dict):
            return rejected("predict returned no answer", input_tokens=input_tokens, overflow=overflow)
        chosen = answer.get("choice")
        probs = answer.get("probabilities")
        if not isinstance(chosen, str) or chosen not in options:
            return rejected(f"choice {chosen!r} is not an option", input_tokens=input_tokens, overflow=overflow)
        if not isinstance(probs, dict) or set(probs) != set(options):
            return rejected("probabilities do not cover the options", input_tokens=input_tokens,
                            overflow=overflow)
        clean: dict[str, float] = {}
        for key in options:                       # option order, for a stable log line
            value = _finite_unit(probs[key])
            if value is None:
                return rejected(f"probability of {key!r} is not in [0, 1]", input_tokens=input_tokens,
                                overflow=overflow)
            clean[key] = value
        ranked = sorted(clean.values(), reverse=True)
        p_top = clean[chosen]
        if p_top < ranked[0] - TIE_TOLERANCE:
            return rejected("choice is not the most probable option", probabilities=clean,
                            input_tokens=input_tokens, overflow=overflow)
        margin = p_top - (ranked[1] if len(ranked) > 1 else 0.0)
        confidence = _finite_unit(answer.get("confidence"))
        return Choice(chosen, p_top=p_top, margin=max(0.0, margin),
                      confidence=confidence if confidence is not None else 0.0, probabilities=clean,
                      ms=ms, input_tokens=input_tokens, k=k, overflow=overflow, dropped_for_budget=dropped)
