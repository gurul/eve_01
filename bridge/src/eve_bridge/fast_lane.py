"""The fast lane: a narrow run of clicks on labelled controls, decided without the planner.

`run_delegate(objective, ...)` is what the `delegate` helper calls from exec_py. Per
step it takes an accessibility snapshot (ax_candidates), builds a bounded menu in
code, picks the option that advances the objective (the keyword gate in code, or a
typed-decision model — see `decide` below), gates the answer in code, acts through injected effectors, settles, and
checks `done_when`. The model only ranks: every judgement that can cost the human
something — sensitive labels, dialogs, System Settings values, return into a message,
a hit-test miss, a focus change, a repeat, a stall — is a code oracle here, and the
lane can only ADD stops to the planner's ask_user contract, never remove one.

Measured cost floor of a step on this Mac (PLAN §0): AX snapshot 0.04-0.43 s, a keyword
decide well under a millisecond (a hosted Jev answer ~240 ms p50), hit-test ≤ 24 ms, settle 0.33-1.0 s — against a replaced planner turn of
3.56 s API + 1.3 s exec. The honest promise is fewer failed-click recovery turns first.

The decide step is two oracles in order. First the keyword gate (`keyword_pick`): when
exactly one offered click option shares the most objective tokens (label + value, via
ax_candidates.objective_tokens) and that count is above zero, it is the pick and the
model is not asked — on the select fixtures this code-only rule is 0.90 precise at 0.66
coverage, better than the model on its own. Otherwise the model ranks the menu, with each
click option worded by decider.render_option ("Week (radio button)") and a context of the
window title (and the focused control): the wording and the context the select fixtures
scored best (decider.py has the numbers). Every gate after the pick (sensitive, dialog, System Settings, return, repeat,
hit-test) applies to a keyword pick exactly as to a model pick; Step.via says which one
decided.

Senses protocol (the adapter lives in desktop_helpers.Helpers.delegate):

    snapshot() -> Snapshot            the frontmost app's focused window, a fresh seq each call
    text_visible(s: str) -> bool      fast OCR plus AX labels/values, right now
    focused() -> Optional[Candidate]  AXFocusedUIElement re-read now
    frontmost_pid() -> int            0 when unknown
    screen_changed() -> Optional[bool]  the helpers' thumbnail diff since the step began; None = unknown

Effectors protocol:

    click_candidate(c) -> str         re-hit-tests the centre; a sentence starting "refused:" means
                                      the element is gone, in another app, or its title differs
    focus_and_type(c, text) -> str    click, then paste only if the focused element is c; "refused:" otherwise
    press(key) -> str                 one key, never a combo
    settle(secs) -> float             wait for the screen to stop changing, up to secs; returns the wait

Result line (exact templates; values single-line, quotes escaped; the payload states are
none | pending | applied and a step applies its payload at most once):

    done: matched="{done_when}" via={ax|ocr|title}; steps={n}; last={action}; text={state}; key={state}
    stopped: reason={one_step|dry_run}; steps={n}; last={action}; pick={id role "label"}; text=…; key=…
    escalate: app="{app}"; reason={no_window|no_candidate|dialog_open|truncated|model_invalid|abstain|
              stalled|stalled_2|ambiguous_target|no_match|uncovered|hit_test_failed|focus_changed|
              step_cap|jev_error|jev_veto|jev_none}; top=[…≤3]; steps={n}; last={action}; text=…; key=…
    confirm: "{label}" needs the human's yes; app="{app}"; action={click|type|press}; steps={n};
             last={action}; text=…; key=…
    unavailable: {reason}

`last` is the last APPLIED input or none; `steps` counts steps that applied input; a
confirm guarantees the blocked action did not run; unavailable guarantees zero input.

Who decides is `decide` (DECIDE_MODES, default "keyword"): the keyword gate alone, where a tie
escalates `ambiguous_target` and zero shared words escalates `no_match`, and no model is loaded
or asked; "model", the gate first and the decider (decider.Decider over hosted Jev, jev.py) on
the rest; or "jev", the gate proposes and Jev's step asker can refuse (run_delegate). Keyword mode clicks only: a
call with `text` or `key` answers unavailable, because the gate has never been measured on
which field to type into.

`run_script(objectives, …)` is the unit both callers above the lane use: an ordered list of
objectives, one keyword-decided click each, a fresh snapshot per step, stopping at the first
step that does not act. The planner sends exact labels in order (`delegate(steps=[…])`), so
each step's objective is its own label and the gate cannot confuse step 2's control with step
1's; the router (lane_router.py) sends the human's own clauses, with `require_cover`. Its line:

    script: {complete|partial|none}; applied=[{action}, …]; step {i}/{n} {that step's own line}

`complete` means every step either applied input that the Accessibility tree or the screen
diff saw change something, or found its control already selected. Anything less is partial
(something was applied) or none (nothing was), and the caller hands the rest to the planner.
"""

from __future__ import annotations

import re
import time
from dataclasses import dataclass, field, replace
from typing import Any, Callable, Optional, Sequence

from .ax_candidates import (
    Candidate,
    Snapshot,
    describe,
    needs_confirmation,
    objective_tokens,
    pressable,
    rank_candidates,
)
from .decider import RESERVED, Choice, render_option
from .typed_ask import JEV_STEP_GATES, StepAnswer, StepGates

# The shipped default for EVE_FAST_LANE. The holdout eval (tools/fastlane_eval.py
# --check-default, gate G6) decides it: enabled only if gated top-1 ≥ 0.80, coverage ≥ 0.70,
# real ≥ keyword + 0.10 on overlap:false cases and the cost-weighted score > 0. The last run
# (holdout, 82 cases, 2026-09-21) was of a local checkpoint since removed from the lane: gated
# top-1 72.3%, coverage 79.3% — two of the four conditions failed, so the lane ships off. The
# keyword gate alone decided 54.9% of holdout at 88.9%.
FAST_LANE_DEFAULT: bool = False
# Who decides a step. "keyword": the keyword gate alone — a tie or zero shared words escalates,
# no model is asked or loaded. "model": the gate first, the decider on the rest (EVE_DECIDER=jev;
# desktop_worker.start_fast_lane). "jev": the gate proposes and Jev's step asker can refuse. Sliced by
# decision path on holdout (2026-09-21): keyword picks 88.9% right (n=45) against 29.7% for the model
# picks of that run, so the default keeps any model out of the click path.
DECIDE_MODES = ("keyword", "model", "jev")
DEFAULT_DECIDE = "keyword"
# The shipped default for EVE_LANE_FIRST (lane_router.py: the lane runs before the planner's
# first turn). tools/fastlane_eval.py --router runs the real router over the fixtures and
# --check-default asserts this constant equals its decision. The bar was fixed before the first
# run: precision ≥ 0.90 on ≥ 10 engaged holdout cases, zero sensitive clicks. Measured 2026-09-21:
# holdout 23 engaged of 82, 23 right (Wilson 95% 85.7–100%), select 26 of 73, 26 right; no click on
# any abstain-expected case, no sensitive click; it engages on about a third of the click cases and
# declines the rest (no match, a tie, uncovered words, a confirm) for one AX snapshot each.
# Two cautions the numbers do not remove: the holdout had been sliced by overlap and distractor
# before the cover rule was written, and the fixture goals were authored as lane objectives, not
# transcribed from speech. tools/fastlane_eval.py --live-route is the check on a real desktop.
LANE_FIRST_DEFAULT: bool = True
MAX_SCRIPT_STEPS = 6
# "jev" mode's menu. Jev reads 255 options, and on select the right control is on a 25-menu in 60 of 69
# cases against 58 on the 10-menu `model` mode offers (tools/jev_step_eval.py, 2026-09-21).
JEV_MAX_OPTIONS = 27               # 25 real options + the two reserved
DEFAULT_STYLE = "jev"              # how `model` mode words its question: the style Jev scored best with (jev.py)
SYSTEM_SETTINGS_BUNDLE = "com.apple.systempreferences"
SYSTEM_SETTINGS_NAV_ROLES = frozenset({"row", "cell", "tab", "link"})
# In System Settings every `button` looks alike to the Accessibility tree: "Dark" (a value) and
# "Language & Region" (a door to another pane) are both role button, no value, no actions. The
# paired A/B of 2026-09-21 lost three lane runs of four to that: the lane answered confirm for a
# pane it was only asked to open. So the doors are named here, a closed set: the sub-panes of
# General. Opening one changes no setting; every control inside it is still a value and still
# confirms. "Transfer or Reset" and "Device Management" are left out on purpose, and "Login Items &
# Extensions" would be pointless here: the sensitive-label table catches "login" first, as it
# catches the "Privacy & Security" row. This set never outranks that table.
SYSTEM_SETTINGS_PANE_BUTTONS = frozenset(name.casefold() for name in (
    "About", "Software Update", "Storage", "AppleCare & Warranty", "AirDrop & Continuity",
    "AutoFill & Passwords", "Date & Time", "Language & Region", "Sharing", "Startup Disk", "Time Machine"))
SEARCH_FIELD_WORDS = re.compile(r"\b(search|address|url|go to)\b", re.IGNORECASE)
RETURN_KEYS = frozenset({"return", "enter"})
REOBSERVE_TEXT = "the screen is still changing, look again"
ABSTAIN_TEXT = "none of these advances the objective"
MAX_TYPE_OPTIONS = 3
MAX_TOP = 3
MAX_LOG_CHARS = 80
MAX_RECENT = 3
STATUSES = ("done", "stopped", "escalate", "confirm", "unavailable")


@dataclass(frozen=True)
class Thresholds:
    # p_min / margin_min are the eval's (tools/fastlane_eval.py, 2026-09-21, select fixtures, 73
    # cases, measured on the local checkpoint the lane used then): the highest grid pair with
    # coverage ≥ 0.70 on select — 0.60 / 0.15 covered 79.5% at 81.0% gated top-1 there. They gate model picks only; a keyword-gate pick carries p_top 1.0.
    # The gate escalates ambiguous_target below them.
    p_min: float = 0.60
    margin_min: float = 0.15
    max_options: int = 12          # click + type + press + reobserve + abstain, total
    max_steps: int = 4             # clamped to [1, 6] by run_delegate
    settle_secs: float = 1.0


@dataclass
class Step:
    n: int
    snapshot_seq: int
    k: int
    dropped_cap: int
    dropped_budget: int
    chosen: str
    description: str
    p_top: float
    margin: float
    confidence: float
    verdict: str                   # act | reobserve | escalate | confirm | stopped
    reason: str
    changed: Optional[bool]
    snapshot_ms: float
    node_count: int
    truncated: bool
    decide_ms: float
    act_ms: float
    settle_ms: float
    via: str = "model"              # keyword | model: which oracle picked (see the module docstring)
    kind: str = ""                  # click | type | press, "" for a reserved answer
    offered: tuple[str, ...] = ()   # the option texts the menu carried, in menu order
    label: str = ""                 # the picked control's full label, "" for a press or a reserved answer


@dataclass
class DelegateResult:
    status: str                    # done | stopped | escalate | confirm | unavailable
    line: str
    steps: list[Step] = field(default_factory=list)
    log_lines: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class MenuItem:
    """One real option: what the lane would do if the model picks its key."""

    key: str
    kind: str                      # click | type | press
    text: str                      # the option text the model sees
    candidate: Optional[Candidate] = None
    payload: str = ""              # the text to type, or the key to press


# ---- formatting ------------------------------------------------------------------------

def _q(value: Any) -> str:
    """A value as one line inside double quotes: backslashes and quotes escaped, newlines spaced."""
    text = " ".join(str(value).split()).replace(";", ",")   # ';' separates fields: a label may not forge one
    return text.replace("\\", "\\\\").replace('"', '\\"')


def describe_pick(c: Optional[Candidate]) -> str:
    """`7 radio button "Week"` — the id, role and full label of a candidate."""
    if c is None:
        return "none"
    return f'{c.id} {c.role} "{_q(c.label)}"'


def _action_text(item: MenuItem) -> str:
    """The `last=` form of an applied input: never the typed text, only its length."""
    if item.kind == "click":
        assert item.candidate is not None
        return f"click {item.candidate.role}: {_q(item.candidate.label)}"
    if item.kind == "type":
        assert item.candidate is not None
        return f"type {len(item.payload)} chars into {item.candidate.role}: {_q(item.candidate.label)}"
    return f"press {item.payload}"


def format_line(status: str, *, steps: int = 0, last: str = "none", text_state: str = "none",
                key_state: str = "none", app: str = "", reason: str = "", done_when: str = "", via: str = "",
                pick: Optional[Candidate] = None, top: tuple[Candidate, ...] = (), label: str = "",
                action: str = "") -> str:
    """The one result sentence, exactly per the module docstring's templates."""
    if status not in STATUSES:
        raise ValueError(f"unknown status {status!r}")
    tail = f"steps={steps}; last={last}; text={text_state}; key={key_state}"
    if status == "unavailable":
        return f"unavailable: {reason}"
    if status == "done":
        return f'done: matched="{_q(done_when)}" via={via}; {tail}'
    if status == "stopped":
        return f"stopped: reason={reason}; steps={steps}; last={last}; pick={describe_pick(pick)}; " \
               f"text={text_state}; key={key_state}"
    if status == "escalate":
        tops = ", ".join(describe_pick(c) for c in top[:MAX_TOP])
        return f'escalate: app="{_q(app)}"; reason={reason}; top=[{tops}]; {tail}'
    return f'confirm: "{_q(label)}" needs the human\'s yes; app="{_q(app)}"; action={action}; {tail}'


def _clip(text: str, limit: int = MAX_LOG_CHARS) -> str:
    text = " ".join(text.split())
    return text if len(text) <= limit else text[:limit - 1].rstrip() + "…"


# ---- the pure gate (step 5) --------------------------------------------------------------

def gate(choice: Choice, item: Optional[MenuItem], *, thresholds: Thresholds, approve: Optional[str],
         approve_used: bool, search_typed: bool, dialog_open: bool, reobserve_count: int) -> tuple[str, str]:
    """(verdict, reason) for one model answer. Pure: every rule is a code judgement.

    Verdicts: "escalate" (model_invalid | abstain | stalled | ambiguous_target), "reobserve",
    "confirm" (sensitive | return), "act". The order is the plan's: an invalid answer first,
    then the reserved answers, then the probability gate, then what the pick would do.
    """
    if not choice.id or choice.error:
        return "escalate", "model_invalid"
    if choice.id == "abstain":
        return "escalate", "abstain"
    if choice.id == "reobserve":
        return ("reobserve", "") if reobserve_count < 1 else ("escalate", "stalled")
    if choice.p_top < thresholds.p_min or choice.margin < thresholds.margin_min:
        return "escalate", "ambiguous_target"
    if item is None or item.key != choice.id:
        return "escalate", "model_invalid"
    if item.kind in ("click", "type"):
        assert item.candidate is not None
        label = item.candidate.label
        approved = approve is not None and not approve_used and label.casefold() == approve.casefold()
        if needs_confirmation(item.candidate) and not approved:   # sensitive, or an account change on a page
            return "confirm", "sensitive"
        return "act", ""
    key = item.payload.casefold()
    if key in RETURN_KEYS and not (search_typed and not dialog_open):
        return "confirm", "return"
    return "act", ""


# ---- helpers ---------------------------------------------------------------------------

def is_search_field(c: Candidate) -> bool:
    if c.role == "search field":
        return True
    return c.role in ("text field", "combo box") and SEARCH_FIELD_WORDS.search(c.label) is not None


def _same_control(a: Candidate, b: Candidate) -> bool:
    return a.role == b.role and a.label == b.label and a.frame == b.frame


def editable_pool(snapshot: Snapshot, allow_page_links: bool = False) -> list[Candidate]:
    """Fields the lane may type into: never secure, never in a dialog, and page-content fields
    (a web form) only when the caller allowed page content."""
    return [c for c in snapshot.elements if c.editable and c.enabled and c.label.strip() and not c.secure
            and not c.in_dialog and (allow_page_links or not c.in_content)]


def _shares_token(c: Candidate, tokens: list[str]) -> bool:
    hay = f"{c.label} {c.value}".casefold()
    return any(t in hay for t in tokens)


def lane_context(snapshot: Snapshot) -> str:
    """What the model is told besides the goal and the app: the window title, and the focused
    control when there is one — nothing else (the full context lines cost accuracy, see the
    module docstring). The eval passes the same string."""
    parts = [f"title: {' '.join(snapshot.title.split())}"]
    f = snapshot.focused
    if f is not None and (f.label or f.role):
        parts.append(f"focused: {f.role} {' '.join(f.label.split())}".rstrip())
    return "; ".join(parts)


def jev_context(snapshot: Snapshot) -> str:
    """What a HOSTED model is told besides the step, the app and the menu: the focused control, and
    never the window title — the one string most likely to be a document's name, a message's subject
    or a chat's. Measured on select (2026-09-21): top-1 58/69 without the title against 57/69 with it,
    and the gate-then-veto path identical (49 right, 2 wrong), so the title bought nothing."""
    f = snapshot.focused
    if f is not None and (f.label or f.role):
        return f"focused: {f.role} {' '.join(f.label.split())}".rstrip()
    return ""


def _overlap(c: Candidate, tokens: set[str]) -> int:
    """How many objective tokens the control's label and value carry (the keyword gate's score)."""
    return len(tokens & set(objective_tokens(f"{c.label} {c.value}")))


def keyword_verdict(items: Sequence[MenuItem], objective: str) -> tuple[Optional[MenuItem], str]:
    """The keyword gate, with its reason: (the pick, "") or (None, "tie" | "no_match").

    The pick is the one click option sharing the most objective tokens with its label and value
    (ax_candidates.objective_tokens on both sides), when that count is above zero and no other
    click option ties it."""
    tokens = set(objective_tokens(objective))
    if not tokens:
        return None, "no_match"
    best: Optional[MenuItem] = None
    best_score = 0
    tied = False
    for it in items:
        if it.kind != "click" or it.candidate is None:
            continue
        score = _overlap(it.candidate, tokens)
        if score > best_score:
            best, best_score, tied = it, score, False
        elif score == best_score and score > 0:
            tied = True
    if best_score == 0:
        return None, "no_match"
    if tied:
        return None, "tie"
    return best, ""


def keyword_pick(items: Sequence[MenuItem], objective: str) -> Optional[MenuItem]:
    """The keyword gate's pick, or None on a tie or on zero overlap (see keyword_verdict)."""
    return keyword_verdict(items, objective)[0]


def uncovered_tokens(objective: str, c: Candidate, app: str = "", extra: Sequence[str] = ()) -> list[str]:
    """The objective's words that the picked control does not account for.

    A word is accounted for by the control's label, by its role ("the week button"), by the
    frontmost app's name ("in calendar …") or by `extra` (the caller's generic words). The value
    is left out on purpose: "is week selected" must not read as covered by a selected Week.
    An empty list is what lets the router act on a human's own words without a planner: the
    request says nothing the click does not do."""
    have = set(objective_tokens(f"{c.label} {c.role} {app}")) | {str(w).casefold() for w in extra}
    return [t for t in objective_tokens(objective) if t not in have]


def _ax_changed(before: Snapshot, after: Snapshot, chosen: Optional[Candidate]) -> Optional[bool]:
    """The AX diff: the chosen control's value flipped, the focus moved, the title changed, the
    element count or truncation changed. None when nothing in the tree says either way."""
    if chosen is not None:
        for c in after.elements:
            if _same_control(c, chosen) and c.value != chosen.value:
                return True

    def focus_key(s: Snapshot) -> tuple[str, str, tuple[int, ...]]:
        f = s.focused
        return (f.role, f.label, f.frame) if f is not None else ("", "", ())
    if focus_key(before) != focus_key(after):
        return True
    if before.title != after.title:
        return True
    if len(before.elements) != len(after.elements) or before.truncated != after.truncated:
        return True
    return None


def _jev_verdict(answer: StepAnswer, keyword: Optional[MenuItem], gates: StepGates,
                 options: dict[str, str]) -> tuple[str, Choice, str]:
    """(via, choice, stop) for one Jev answer. `stop` is "" to go on to the code gate, "confirm", or an
    escalation reason: jev_error (no usable answer — nothing is pressed on a guess), jev_veto (the
    keyword gate had a pick and Jev's top control is another one, without clearing its own bar),
    jev_none (no pick from either)."""
    empty = Choice("", k=len(options))
    if answer.error or not answer.target:
        return "jev", empty, "jev_error"
    risky = answer.risky >= gates.risky_max          # the pick is still worked out: the human is asked about IT
    if keyword is not None and answer.target == keyword.key:
        return "keyword+jev", Choice(keyword.key, p_top=1.0, margin=1.0, confidence=answer.present,
                                     probabilities={k: (1.0 if k == keyword.key else 0.0) for k in options},
                                     k=len(options)), "confirm" if risky else ""
    pick = replace(gates, risky_max=2.0).decide(answer)
    if pick in options and pick not in RESERVED:
        return "jev", Choice(pick, p_top=answer.p_target, margin=answer.margin, confidence=answer.present,
                             probabilities={k: (answer.p_target if k == pick else 0.0) for k in options},
                             k=len(options)), "confirm" if risky else ""
    if risky:
        return "jev", empty, "confirm"
    return "jev", empty, "jev_veto" if keyword is not None else "jev_none"


def _valid_key(key: Optional[str]) -> tuple[Optional[str], str]:
    """(normalised key, error). Combos (a "+", "command", "cmd") are never offered."""
    if key is None:
        return None, ""
    k = " ".join(str(key).split()).casefold()
    if not k:
        return None, ""
    if "+" in k or "command" in k or "cmd" in k or " " in k:
        return None, "key combos are not supported"
    return k, ""


# ---- the loop --------------------------------------------------------------------------

class _Run:
    """One run_delegate call: the state the templates need, and the step log."""

    def __init__(self, objective: str, text: Optional[str], key: Optional[str], done_when: Optional[str],
                 approve: Optional[str], thresholds: Thresholds, clock: Callable[[], float]) -> None:
        self.objective = objective
        self.text = text
        self.key = key
        self.done_when = done_when
        self.approve = approve
        self.approve_used = False
        self.thresholds = thresholds
        self.clock = clock
        self.steps: list[Step] = []
        self.log_lines: list[str] = []
        self.applied = 0
        self.last = "none"
        self.recent: list[str] = []
        self.search_typed = False
        self.text_state = "pending" if text else "none"
        self.key_state = "pending" if key else "none"
        self.previous: Optional[Candidate] = None      # last step's chosen control, never re-offered
        self.unchanged_streak = 0
        self.reobserves = 0
        self.app = ""
        self.top: tuple[Candidate, ...] = ()

    def finish(self, status: str, line: str) -> DelegateResult:
        self.log_lines.append(_clip(f"delegate {line}"))
        return DelegateResult(status, line, self.steps, self.log_lines)

    def escalate(self, reason: str) -> DelegateResult:
        line = format_line("escalate", app=self.app, reason=reason, top=self.top, steps=self.applied,
                           last=self.last, text_state=self.text_state, key_state=self.key_state)
        return self.finish("escalate", line)

    def confirm(self, label: str, action: str) -> DelegateResult:
        line = format_line("confirm", label=label, app=self.app, action=action, steps=self.applied,
                           last=self.last, text_state=self.text_state, key_state=self.key_state)
        return self.finish("confirm", line)

    def done(self, via: str) -> DelegateResult:
        line = format_line("done", done_when=self.done_when or "", via=via, steps=self.applied, last=self.last,
                           text_state=self.text_state, key_state=self.key_state)
        return self.finish("done", line)

    def stopped(self, reason: str, pick: Optional[Candidate]) -> DelegateResult:
        line = format_line("stopped", reason=reason, steps=self.applied, last=self.last, pick=pick,
                           text_state=self.text_state, key_state=self.key_state)
        return self.finish("stopped", line)


def _done_check(run: _Run, senses: Any, snapshot: Snapshot, *, title0: str, visible0: bool,
                allow_now: bool) -> str:
    """Which oracle says done_when is met: "ax", "ocr", "title", or "". The ocr and title rules
    need the marker to have been absent at step 0; the ax rule (a control with that label is
    selected/checked) may fire at once, which is how a run from the target state returns done."""
    marker = run.done_when
    if not marker:
        return ""
    needle = marker.casefold().strip()
    for c in snapshot.elements:
        if c.label.casefold().strip() == needle and c.value in ("selected", "checked"):
            return "ax"
    if not allow_now:
        return ""
    if needle in snapshot.title.casefold() and needle not in title0.casefold():
        return "title"
    if not visible0:
        try:
            if senses.text_visible(marker):
                return "ocr"
        except Exception:  # noqa: BLE001 — a failed OCR read is "not visible", never a crash mid-run
            return ""
    return ""


def _timed_snapshot(run: _Run, senses: Any) -> tuple[Snapshot, float]:
    t0 = run.clock()
    snap = senses.snapshot()
    return snap, (run.clock() - t0) * 1000.0


def _build_menu(run: _Run, snapshot: Snapshot, *, allow_page_links: bool) -> tuple[list[MenuItem], int,
                                                                                  list[Candidate]]:
    """(items, dropped_by_cap, blocked). `blocked` are the objective-relevant controls the lane
    withholds — sensitive labels without approval, and non-navigation roles in System Settings —
    so the caller can answer confirm for the best of them when nothing offered matches."""
    th = run.thresholds
    tokens = objective_tokens(run.objective)
    type_items: list[MenuItem] = []
    press_items: list[MenuItem] = []
    if run.text_state == "pending":
        fields, _ = rank_candidates(snapshot, run.objective, max_out=MAX_TYPE_OPTIONS, kinds="editable",
                                    allow_page_links=allow_page_links)
        for c in fields:
            if run.previous is not None and _same_control(c, run.previous):
                continue
            type_items.append(MenuItem("", "type", describe(c, verb="type into"), c, run.text or ""))
    if run.key_state == "pending" and run.text_state != "pending":
        press_items.append(MenuItem("", "press", f"press {run.key}", None, run.key or ""))
    cap = max(1, th.max_options - len(RESERVED) - len(type_items) - len(press_items))
    pool, _ = rank_candidates(snapshot, run.objective, max_out=len(snapshot.elements) or 1, kinds="pressable",
                              allow_page_links=allow_page_links, allow_sensitive=True)
    settings = snapshot.bundle == SYSTEM_SETTINGS_BUNDLE
    offered: list[Candidate] = []
    blocked: list[Candidate] = []
    for c in pool:
        if run.previous is not None and _same_control(c, run.previous):
            continue
        approved = (run.approve is not None and not run.approve_used
                    and c.label.casefold() == run.approve.casefold())
        if needs_confirmation(c) and not approved:
            blocked.append(c)
            continue
        if settings and c.role not in SYSTEM_SETTINGS_NAV_ROLES and c.label.casefold() != "back" and not (
                c.role == "button" and " ".join(c.label.split()).casefold() in SYSTEM_SETTINGS_PANE_BUTTONS):
            blocked.append(c)
            continue
        offered.append(c)
    dropped_cap = max(0, len(offered) - cap)
    items: list[MenuItem] = []
    for c in offered[:cap]:
        items.append(MenuItem("", "click", render_option(c), c))
    items.extend(type_items)
    items.extend(press_items)
    items = [MenuItem(str(i + 1), it.kind, it.text, it.candidate, it.payload) for i, it in enumerate(items)]
    run.top = tuple(it.candidate for it in items if it.kind == "click" and it.candidate is not None)[:MAX_TOP]
    token_set = set(tokens)
    blocked_matching = sorted((c for c in blocked if _overlap(c, token_set) > 0),
                              key=lambda c: -_overlap(c, token_set))
    return items, dropped_cap, blocked_matching


def run_delegate(objective: str, *, senses: Any, effectors: Any, decider: Any, text: Optional[str] = None,
                 key: Optional[str] = None, done_when: Optional[str] = None, approve: Optional[str] = None,
                 allow_page_links: bool = False, max_steps: int = 4, dry_run: bool = False,
                 thresholds: Thresholds = Thresholds(), clock: Callable[[], float] = time.perf_counter,
                 decide: str = DEFAULT_DECIDE, require_cover: bool = False, cover_extra: Sequence[str] = (),
                 skip_if_selected: bool = False, asker: Optional[Callable[..., StepAnswer]] = None,
                 step_gates: StepGates = JEV_STEP_GATES) -> DelegateResult:
    """The loop of the module docstring. Never raises for a model or sense failure: the status
    says what happened, and every path that could act consequentially returns confirm first.

    `decide` is who picks (DECIDE_MODES): in "keyword" mode the decider may be None. `require_cover`
    is the router's rule (lane_router.py): act only on a keyword pick that accounts for every word
    of the objective (uncovered_tokens), else escalate `uncovered` with zero input.
    `skip_if_selected` answers done with zero input when the keyword pick is already selected — a
    script step that asks for the view the window is in.

    In "jev" mode `asker` is typed_ask.ask_jev_step bound to a predict, and the decider is unused. The
    keyword gate PROPOSES and Jev can REFUSE: a gate pick is pressed only when Jev's own top control is
    the same one; otherwise the step is Jev's, under `step_gates`. On the read fixtures that took the
    click path from 40 right / 5 wrong to 43 right / 1 wrong (holdout, tools/jev_step_eval.py). Jev's
    `risky` noul can only ADD a confirm, and every code gate after the pick applies to its pick too."""
    objective = " ".join(str(objective or "").split())
    text = text if isinstance(text, str) and text != "" else None
    done_when = " ".join(str(done_when).split()) if isinstance(done_when, str) and done_when.strip() else None
    approve = " ".join(str(approve).split()) if isinstance(approve, str) and approve.strip() else None
    key, key_error = _valid_key(key)
    run = _Run(objective, text, key, done_when, approve, thresholds, clock)
    if key_error:
        return run.finish("unavailable", format_line("unavailable", reason=key_error))
    if not objective:
        return run.finish("unavailable", format_line("unavailable", reason="an objective is required"))
    if decide not in DECIDE_MODES:
        return run.finish("unavailable", format_line("unavailable", reason=f"decide must be one of {DECIDE_MODES}"))
    if decide == "model" and (decider is None or not getattr(decider, "available", False)):
        return run.finish("unavailable", format_line("unavailable", reason="the decider is not loaded"))
    if decide == "jev" and asker is None:
        return run.finish("unavailable", format_line("unavailable", reason="jev is not configured"))
    if decide in ("keyword", "jev") and (text is not None or key is not None):
        return run.finish("unavailable", format_line(
            "unavailable", reason="the keyword lane only clicks; type with type_text and press keys yourself"))
    if decide == "jev":
        # A grouped asker (typed_ask.ask_jev_step_grouped, EVE_JEV_GROUPS) reads past one 25-menu: its
        # `max_options` real controls, plus the two reserved. Any other asker keeps the 25.
        grouped = getattr(asker, "max_options", None)
        menu = int(grouped) + len(RESERVED) if isinstance(grouped, int) and grouped > 0 else JEV_MAX_OPTIONS
        if thresholds.max_options < menu:
            thresholds = replace(thresholds, max_options=menu)
            run.thresholds = thresholds
    max_steps = max(1, min(6, int(max_steps)))

    # step 0: the baseline the ocr and title oracles compare against
    try:
        snapshot, snapshot_ms = _timed_snapshot(run, senses)
    except Exception as e:  # noqa: BLE001 — no snapshot means no lane, not a traceback in exec_py
        run.app = ""
        return run.escalate(f"no_window ({type(e).__name__})")
    run.app = snapshot.app
    title0 = snapshot.title
    visible0 = False
    if done_when:
        try:
            visible0 = bool(senses.text_visible(done_when))
        except Exception:  # noqa: BLE001 — unknown baseline: the ocr oracle then needs a later sighting
            visible0 = True
    if snapshot.dialog_text:                        # a dialog is never operated, not even to say "done"
        run.app = snapshot.app
        return run.escalate(f'dialog_open: "{_q(snapshot.dialog_text)}"')
    if snapshot.truncated:
        # A visible label cannot establish uniqueness when other controls were omitted.
        return run.escalate("truncated")
    via = _done_check(run, senses, snapshot, title0=title0, visible0=visible0, allow_now=False)
    if via:
        return run.done(via)

    n = 0
    while n < max_steps:
        n += 1
        run.app = snapshot.app or run.app
        if not snapshot.elements and snapshot.node_count == 0:
            return run.escalate("no_window")
        if snapshot.dialog_text:
            return run.escalate(f'dialog_open: "{_q(snapshot.dialog_text)}"')
        if len(pressable(snapshot)) < 2 and not editable_pool(snapshot):
            snapshot, snapshot_ms = _timed_snapshot(run, senses)       # one re-snapshot
            if not snapshot.elements and snapshot.node_count == 0:
                return run.escalate("no_window")
            if snapshot.dialog_text:
                return run.escalate(f'dialog_open: "{_q(snapshot.dialog_text)}"')
            if len(pressable(snapshot)) < 2 and not editable_pool(snapshot):
                return run.escalate("no_candidate")
        items, dropped_cap, blocked = _build_menu(run, snapshot, allow_page_links=allow_page_links)
        tokens = objective_tokens(objective)
        if snapshot.truncated:
            return run.escalate("truncated")
        # A withheld control (sensitive, or a System Settings value) that matches the objective at
        # least as well as anything offered is what the human meant: confirm it, never a look-alike
        # ("delete the event" must not click "Add Event"). Only an offered option that matches
        # strictly better goes on to the pick.
        # An offered control whose label IS the objective (the plan copied it from the outline) is what was
        # meant, however many words a withheld one shares: "Your message" is the field, not "Send message"
        # (browser_model_eval contact_form, 2026-09-24). The withheld control stays withheld either way.
        token_set = set(tokens)
        offered_best = max((_overlap(it.candidate, token_set) for it in items if it.candidate is not None),
                           default=0)
        named = any(it.candidate is not None and " ".join(it.candidate.label.split()).casefold() == objective.casefold()
                    for it in items)
        if blocked and not named and _overlap(blocked[0], token_set) >= offered_best:
            return run.confirm(blocked[0].label, "click")
        if not items:
            return run.escalate("no_candidate")
        options = {it.key: it.text for it in items}
        options["reobserve"] = REOBSERVE_TEXT
        options["abstain"] = ABSTAIN_TEXT
        t0 = run.clock()
        keyword, why_not = keyword_verdict(items, objective)
        if keyword is not None and keyword.candidate is not None:
            if require_cover:
                missing = uncovered_tokens(objective, keyword.candidate, snapshot.app, cover_extra)
                if missing:
                    run.log_lines.append(_clip(f"delegate route: {describe_pick(keyword.candidate)} leaves "
                                               f"{', '.join(missing)} unexplained"))
                    return run.escalate("uncovered")
            if skip_if_selected and keyword.candidate.value == "selected":
                run.done_when = keyword.candidate.label
                return run.done("ax")
        if decide == "jev" and (keyword is not None or not require_cover):
            assert asker is not None
            real = {it.key: it.text for it in items if it.kind == "click"}
            answer = asker(objective, app=snapshot.app, context=jev_context(snapshot), options=real,
                           recent=tuple(run.recent[-MAX_RECENT:]))
            via, choice, stop = _jev_verdict(answer, keyword, step_gates, options)
            run.log_lines.append(_clip(f"delegate jev: target={answer.target or '-'} p={answer.p_target:.2f} "
                                       f"present={answer.present:.2f} risky={answer.risky:.2f} "
                                       f"{answer.ms:.0f}ms" + (f" {answer.error}" if answer.error else "")))
            if stop == "confirm":
                # Jev read the step as consequential. The human is asked about the control it would press, and
                # their yes (`approve`, once) is the only thing that lets that control through.
                named = next((it for it in items if it.key == (choice.id or answer.target)
                              and it.candidate is not None), None)
                label = named.candidate.label if named is not None and named.candidate is not None else objective
                if not (choice.id and approve is not None and not run.approve_used
                        and label.casefold() == approve.casefold()):
                    return run.confirm(label, "click")
            elif stop:
                return run.escalate(stop)
        elif keyword is not None:
            via = "keyword"
            choice = Choice(keyword.key, p_top=1.0, margin=1.0, confidence=1.0,
                            probabilities={k: (1.0 if k == keyword.key else 0.0) for k in options}, k=len(options))
        elif decide in ("keyword", "jev") or require_cover:
            # The gate abstained and nobody else is asked: a tie is ambiguous, zero shared words
            # is no match. Zero input either way; the planner takes it from here.
            return run.escalate("ambiguous_target" if why_not == "tie" else "no_match")
        else:
            via = "model"
            try:
                choice = decider.choose(objective, app=snapshot.app, context=lane_context(snapshot),
                                        options=options, recent=tuple(run.recent[-MAX_RECENT:]))
            except Exception as e:  # noqa: BLE001 — a contract error in the lane is still "the model gave nothing"
                choice = Choice("", error=f"choose raised {type(e).__name__}: {e}"[:200])
        decide_ms = (run.clock() - t0) * 1000.0
        by_key = {it.key: it for it in items}
        item = by_key.get(choice.id)
        verdict, reason = gate(choice, item, thresholds=thresholds, approve=approve, approve_used=run.approve_used,
                               search_typed=run.search_typed, dialog_open=bool(snapshot.dialog_text),
                               reobserve_count=run.reobserves)
        step = Step(n=n, snapshot_seq=snapshot.seq, k=choice.k or len(options), dropped_cap=dropped_cap,
                    dropped_budget=choice.dropped_for_budget, chosen=choice.id,
                    description=item.text if item else choice.id, p_top=choice.p_top, margin=choice.margin,
                    confidence=choice.confidence, verdict=verdict, reason=reason, changed=None,
                    snapshot_ms=round(snapshot_ms, 2), node_count=snapshot.node_count, truncated=snapshot.truncated,
                    decide_ms=round(decide_ms, 2), act_ms=0.0, settle_ms=0.0, via=via,
                    kind=item.kind if item is not None else "", offered=tuple(it.text for it in items),
                    label=item.candidate.label if item is not None and item.candidate is not None else "")
        run.steps.append(step)
        how = "keyword" if via == "keyword" else f"p {choice.p_top:.2f}"
        run.log_lines.append(_clip(f"delegate step {n}: {step.description} ({how}) {verdict}"
                                   + (f" {reason}" if reason else "")))
        if verdict == "escalate":
            if reason == "model_invalid" and choice.error:
                run.log_lines.append(_clip(f"delegate model: {choice.error}"))
            return run.escalate(reason)
        if verdict == "reobserve":
            run.reobserves += 1
            n -= 1                                                    # a look is not a step
            snapshot, snapshot_ms = _timed_snapshot(run, senses)
            continue
        assert item is not None
        if verdict == "confirm":
            label = item.candidate.label if item.candidate is not None else item.payload
            return run.confirm(label, item.kind)
        if dry_run:
            step.verdict = "stopped"
            step.reason = "dry_run"
            return run.stopped("dry_run", item.candidate)

        # execute
        try:
            front = int(senses.frontmost_pid() or 0)
        except Exception:  # noqa: BLE001 — an unreadable pid is unknown, not a mismatch
            front = 0
        if front and snapshot.pid and front != snapshot.pid:
            step.verdict, step.reason = "escalate", "focus_changed"
            return run.escalate("focus_changed")
        t0 = run.clock()
        try:
            if item.kind == "click":
                assert item.candidate is not None
                said = effectors.click_candidate(item.candidate)
            elif item.kind == "type":
                assert item.candidate is not None
                said = effectors.focus_and_type(item.candidate, item.payload)
            else:
                said = effectors.press(item.payload)
        except Exception as e:  # noqa: BLE001 — an effector that raised applied nothing we can vouch for
            said = f"refused: {type(e).__name__}: {e}"
        step.act_ms = round((run.clock() - t0) * 1000.0, 2)
        if isinstance(said, str) and said.startswith("refused:"):
            step.verdict, step.reason = "escalate", "hit_test_failed" if item.kind != "type" else "focus_changed"
            run.log_lines.append(_clip(f"delegate {said}"))
            return run.escalate(step.reason)
        run.applied += 1
        run.last = _action_text(item)
        run.recent.append(run.last)
        if item.kind == "click" and item.candidate is not None:
            if approve is not None and item.candidate.label.casefold() == approve.casefold():
                run.approve_used = True
        elif item.kind == "type":
            run.text_state = "applied"
            if item.candidate is not None and is_search_field(item.candidate):
                run.search_typed = True
        else:
            run.key_state = "applied"
        run.previous = item.candidate

        # settle, re-snapshot, diff, oracles
        t0 = run.clock()
        try:
            effectors.settle(thresholds.settle_secs)
        except Exception:  # noqa: BLE001 — a failed settle is a shorter wait, nothing more
            pass
        step.settle_ms = round((run.clock() - t0) * 1000.0, 2)
        after, snapshot_ms = _timed_snapshot(run, senses)
        changed = _ax_changed(snapshot, after, item.candidate)
        if changed is None:
            try:
                changed = senses.screen_changed()
            except Exception:  # noqa: BLE001 — unknown is not unchanged
                changed = None
        step.changed = changed
        run.unchanged_streak = run.unchanged_streak + 1 if changed is False else 0
        snapshot = after
        run.app = snapshot.app or run.app
        if snapshot.truncated:
            # Preserve the applied input, but incomplete post-action evidence cannot prove success.
            step.reason = "truncated"
            return run.escalate("truncated")
        via = _done_check(run, senses, snapshot, title0=title0, visible0=visible0, allow_now=True)
        if via:
            return run.done(via)
        if done_when is None:
            return run.stopped("one_step", item.candidate)
        if run.unchanged_streak >= 2:
            return run.escalate("stalled_2")
    return run.escalate("step_cap")


# ---- scripts: an ordered list of objectives ----------------------------------------------

@dataclass
class ScriptResult:
    status: str                    # complete | partial | none (see the module docstring)
    line: str
    applied: list[str] = field(default_factory=list)      # the `last=` text of every applied input, in order
    labels: list[str] = field(default_factory=list)       # the label of every control clicked, in order
    already: list[str] = field(default_factory=list)      # the label of every control found already selected
    results: list[DelegateResult] = field(default_factory=list)
    log_lines: list[str] = field(default_factory=list)

    @property
    def complete(self) -> bool:
        return self.status == "complete"


def _script_line(status: str, applied: Sequence[str], index: int, total: int, last_line: str) -> str:
    done = ", ".join(applied) if applied else ""
    return f"script: {status}; applied=[{done}]; step {index}/{total} {last_line}"


def run_script(objectives: Sequence[str], *, senses: Any, effectors: Any, decider: Any = None,
               decide: str = DEFAULT_DECIDE, require_cover: bool = False, cover_extra: Sequence[str] = (),
               approve: Optional[str] = None, dry_run: bool = False, thresholds: Thresholds = Thresholds(),
               clock: Callable[[], float] = time.perf_counter) -> ScriptResult:
    """Run `objectives` in order, one step each (the module docstring has the contract).

    Each step is its own run_delegate call with max_steps=1, so it gets a fresh snapshot and every
    gate; a step whose control is already selected counts as satisfied with zero input. The script
    stops at the first step that does not act or whose click changed nothing anyone could see."""
    steps = [" ".join(str(o or "").split()) for o in objectives]
    steps = [s for s in steps if s]
    total = len(steps)
    if total == 0:
        line = _script_line("none", (), 0, 0, format_line("unavailable", reason="a script needs at least one step"))
        return ScriptResult("none", line, log_lines=[_clip(f"delegate {line}")])
    if total > MAX_SCRIPT_STEPS:
        line = _script_line("none", (), 0, total,
                            format_line("unavailable", reason=f"a script is at most {MAX_SCRIPT_STEPS} steps"))
        return ScriptResult("none", line, log_lines=[_clip(f"delegate {line}")])
    out = ScriptResult("none", "")
    verified = True
    for i, objective in enumerate(steps, start=1):
        result = run_delegate(objective, senses=senses, effectors=effectors, decider=decider, decide=decide,
                              require_cover=require_cover, cover_extra=cover_extra, approve=approve,
                              max_steps=1, dry_run=dry_run, thresholds=thresholds, clock=clock,
                              skip_if_selected=True)
        out.results.append(result)
        out.log_lines.extend(result.log_lines)
        acted = result.status == "stopped" and bool(result.steps) and result.steps[-1].verdict == "act"
        satisfied = result.status == "done"
        if acted:
            step = result.steps[-1]
            out.applied.append(step.description)
            out.labels.append(step.label or step.description)
            if step.changed is not True:          # None is unknown, not proof: the planner looks
                verified = False
        elif satisfied:
            out.already.append(result.line.split('matched="', 1)[-1].split('"', 1)[0])
        if not (acted or satisfied) or (acted and result.steps[-1].changed is False):
            status = "partial" if out.applied else "none"
            out.status, out.line = status, _script_line(status, out.applied, i, total, result.line)
            out.log_lines.append(_clip(f"delegate {out.line}", 160))
            return out
    status = "complete" if verified else "partial"
    if not out.applied and status == "partial":
        status = "none"
    out.status = status
    out.line = _script_line(status, out.applied, total, total, out.results[-1].line)
    out.log_lines.append(_clip(f"delegate {out.line}", 160))
    return out
