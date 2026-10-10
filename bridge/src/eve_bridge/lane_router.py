"""The router: the fast lane before the planner's first turn.

A planner turn is 3.5 s (logged median) and a task needs at least two: one to act, one to
say so. The paired A/B of 2026-09-21 showed why putting the lane UNDER the planner did not
pay: the planner spends a turn to call the helper, so a two-click task went from 13.1 s to
16.9 s. The lane pays when it runs INSTEAD of the planner. That is only safe when the human's
own words leave nothing to interpret, and this module is the code that decides that:

    "switch to week view"            → one control, Week, accounts for every word → click it
    "year view and then next year"   → two clauses, each fully accounted for     → two clicks
    "open notes"                     → names another installed app               → planner
    "find the email from Sam"        → words no control accounts for             → planner
    "what is on my calendar"         → a question                                → planner

The rule, all of it code (fast_lane.uncovered_tokens does the word arithmetic):

1. Refuse a question, an empty goal, a goal over MAX_GOAL_TOKENS words, and a goal whose words
   name an installed app other than the frontmost one ("open notes" is a launch, not a click
   on a folder called Notes).
2. Try the whole goal as one step with `require_cover`: the keyword gate must pick exactly one
   control, and that control's label and role, the app's name and GENERIC_WORDS must account
   for every word. "privacy and security" is one row, so the whole goal is tried before it is
   split.
3. Only if that escalates with zero input (no match, a tie, uncovered words) and the goal has
   2–MAX_CLAUSES clauses, run the clauses as a script, each under the same rule.

Every gate of fast_lane.py still applies to every step: a sensitive label, a dialog, a System
Settings value, a hit-test miss all stop the lane with zero input, and the planner — with its
ask_user contract — takes over. The router never types and never presses a key.

What the caller gets is a RouteResult. `complete` means every clause was done and each click
was seen to change something (the AX diff or the screen diff said True; unknown is not proof),
so the task can end on `sentence` with no planner call. `partial` means something was clicked:
the planner starts from there and is told what. `none` and `refused` cost one AX snapshot
(0.04–0.43 s measured) and the planner runs as it always did.
"""

from __future__ import annotations

import os
import re
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Iterable, Optional, Sequence

from .ax_candidates import objective_tokens
from .fast_lane import MAX_SCRIPT_STEPS, ScriptResult, Thresholds, run_script

# Words a request carries that no control has to account for: what kind of thing to click and
# where it is. Kept small; every word here is a word the router will NOT ask a label to explain.
GENERIC_WORDS = frozenset({"button", "tab", "tabs", "menu", "row", "item", "link", "option", "icon", "pane", "panel",
                           "section", "mode", "page", "sidebar", "settings", "setting", "toolbar", "window",
                           "screen", "list"})
QUESTION_WORDS = frozenset({"what", "whats", "when", "where", "who", "whose", "why", "how", "which", "is", "are",
                            "was", "were", "does", "do", "did", "tell", "read", "summarize", "summarise", "explain",
                            "describe", "count", "list"})
POLITE_PREFIX = re.compile(r"^(?:hey eve[,\s]*|eve[,\s]*|ok(?:ay)?[,\s]*|please\s+|can you\s+|could you\s+|"
                           r"would you\s+|will you\s+|i want you to\s+|i need you to\s+|go ahead and\s+)+", re.IGNORECASE)
CLAUSE_SPLIT = re.compile(r"\s*(?:[,;]|\band then\b|\bafter that\b|\bthen\b|\band\b)\s*", re.IGNORECASE)
ESCALATE_REASON = re.compile(r"reason=([a-z_0-9]+)")
RETRY_AS_CLAUSES = frozenset({"no_match", "ambiguous_target", "uncovered"})
MAX_GOAL_TOKENS = 10
MAX_CLAUSES = min(4, MAX_SCRIPT_STEPS)
MAX_SPOKEN_CHARS = 110             # the planner's own limit for a final sentence (computer_agent rule 10)
APP_DIRS = ("/Applications", "/System/Applications", "/System/Applications/Utilities", "~/Applications")
STATUSES = ("complete", "partial", "none", "refused")


@dataclass
class RouteResult:
    status: str                                    # complete | partial | none | refused
    reason: str = ""                               # why it is not complete, in one token
    line: str = ""                                 # the script's own line, "" when refused
    clicked: list[str] = field(default_factory=list)      # labels clicked, in order
    already: list[str] = field(default_factory=list)      # labels found already selected
    sentence: str = ""                             # what to say when complete
    log_lines: list[str] = field(default_factory=list)
    ms: float = 0.0

    def to_dict(self) -> dict[str, Any]:
        return {"status": self.status, "reason": self.reason, "line": self.line, "clicked": list(self.clicked),
                "already": list(self.already), "sentence": self.sentence, "log": list(self.log_lines),
                "ms": round(self.ms, 1)}


def normalise(goal: str) -> str:
    """The goal on one line, without the politeness in front of it."""
    text = " ".join(str(goal or "").split())
    return POLITE_PREFIX.sub("", text).strip()


def split_clauses(goal: str) -> list[str]:
    """"year view and then next year" → ["year view", "next year"]. Clauses with no matchable word go."""
    return [c for c in (part.strip() for part in CLAUSE_SPLIT.split(normalise(goal))) if objective_tokens(c)]


def is_question(text: str, *, polite: bool = False) -> bool:
    """A question, not a request: a question word first, or a "?" — unless the request began politely ("can
    you …?"), where the "?" is manners. One rule for the lane (lane_router) and the classifier (task_router)."""
    first = re.findall(r"[a-z']+", text.casefold())
    return (bool(first) and first[0].replace("'", "") in QUESTION_WORDS) or ("?" in text and not polite)


def refuse_reason(goal: str) -> str:
    """"" when the goal may be tried, else question | empty | too_long."""
    text = normalise(goal)
    tokens = objective_tokens(text)
    if not tokens:
        return "empty"
    if is_question(text):
        return "question"
    if len(tokens) > MAX_GOAL_TOKENS:
        return "too_long"
    return ""


def installed_app_names(dirs: Iterable[str] = APP_DIRS, listdir: Callable[[str], list[str]] = os.listdir,
                        ) -> frozenset[str]:
    """Case-folded names of the .app bundles in the usual places: a folded view of task_router.installed_apps,
    the one scanner and the one cache, so the two routers can never disagree about what is installed."""
    from . import task_router  # task_router imports this module at its top

    if tuple(dirs) == tuple(APP_DIRS) and listdir is os.listdir:
        return frozenset(n.casefold() for n in task_router.installed_apps())
    return frozenset(n.casefold() for n in task_router.installed_apps(tuple(dirs), listdir=listdir))


def names_other_app(goal: str, frontmost_app: str, apps: Iterable[str]) -> str:
    """The installed app the goal names, when it is not the frontmost one; "" otherwise. An app is
    named when every word of its name is a word of the goal ("system settings", "notes")."""
    words = set(re.findall(r"[a-z0-9]+", normalise(goal).casefold()))
    front = " ".join(str(frontmost_app or "").casefold().split())
    best = ""
    for name in apps:
        parts = re.findall(r"[a-z0-9]+", name)
        if not parts or name == front or (len(parts) == 1 and len(parts[0]) < 3):
            continue
        if all(p in words for p in parts) and len(name) > len(best):
            best = name
    return best


def spoken_sentence(clicked: Sequence[str], already: Sequence[str] = ()) -> str:
    """What eve says when the lane finished the task: what was clicked, never a guess at more."""
    def short(label: str) -> str:
        text = " ".join(label.split())
        return text if len(text) <= 32 else text[:31].rstrip() + "…"

    if clicked:
        said = "Done. I clicked " + ", then ".join(short(c) for c in clicked) + "."
    elif already:
        said = f"{short(already[-1])} is already selected."
    else:
        said = "Done."
    return said if len(said) <= MAX_SPOKEN_CHARS else said[:MAX_SPOKEN_CHARS - 1].rstrip() + "…"


def _reason_of(script: ScriptResult) -> str:
    if script.complete:
        return ""
    last = script.results[-1] if script.results else None
    if last is None:
        return "unavailable"
    if last.status == "escalate":
        m = ESCALATE_REASON.search(last.line)
        return m.group(1) if m else "escalate"
    if last.status == "stopped":
        return "unverified"                        # it clicked; nothing confirmed the click did anything
    return last.status                             # confirm | unavailable


def _from_script(script: ScriptResult, ms: float, log_lines: list[str]) -> RouteResult:
    clicked = list(script.labels)
    already = list(script.already)
    status = script.status if script.status in ("complete", "partial") else "none"
    return RouteResult(status=status, reason=_reason_of(script), line=script.line, clicked=clicked, already=already,
                       sentence=spoken_sentence(clicked, already) if status == "complete" else "",
                       log_lines=log_lines, ms=ms)


def route(goal: str, *, senses: Any, effectors: Any, frontmost_app: str = "", apps: Optional[Iterable[str]] = None,
          dry_run: bool = False, thresholds: Thresholds = Thresholds(),
          clock: Callable[[], float] = time.perf_counter) -> RouteResult:
    """Try to finish `goal` in the lane, before any planner call (the module docstring has the rule)."""
    t0 = clock()
    text = normalise(goal)
    why = refuse_reason(text)
    if why:
        return RouteResult("refused", reason=why, ms=(clock() - t0) * 1000.0)
    other = names_other_app(text, frontmost_app, installed_app_names() if apps is None else apps)
    if other:
        return RouteResult("refused", reason=f"names_app:{other}", ms=(clock() - t0) * 1000.0)
    common = dict(senses=senses, effectors=effectors, decide="keyword", require_cover=True,
                  cover_extra=tuple(GENERIC_WORDS), dry_run=dry_run, thresholds=thresholds, clock=clock)
    log_lines: list[str] = []
    whole = run_script([text], **common)
    log_lines.extend(whole.log_lines)
    result = _from_script(whole, (clock() - t0) * 1000.0, log_lines)
    if result.status != "none" or result.reason not in RETRY_AS_CLAUSES:
        return result
    clauses = split_clauses(text)
    if not 2 <= len(clauses) <= MAX_CLAUSES:
        return result
    script = run_script(clauses, **common)
    log_lines.extend(script.log_lines)
    return _from_script(script, (clock() - t0) * 1000.0, log_lines)
