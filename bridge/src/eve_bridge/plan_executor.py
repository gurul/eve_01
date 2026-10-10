"""Walking a plan: every step grounded on a fresh snapshot, no planner turn between steps, none at the end.

`run_plan(plan, …)` takes a plan_contract.Plan and the same senses/effectors the lane uses (fast_lane's
module docstring has the protocol) and returns a PlanResult. It never raises for a model or sense failure.

Per step:
  open_app / open_url   the helpers' own calls; their sentence ("opened … (frontmost after …)") is the oracle
  click                 fast_lane.run_delegate, one step: the exact label first when the planner read one in
                        the outline (the keyword gate usually decides it, Jev only has to agree), then the
                        description. Every gate of the lane applies — sensitive labels, dialogs, System
                        Settings values, the hit-test, the frontmost pid.
  type                  the field is the only editable one; or the one the step names (or the plan just clicked)
                        with Jev agreeing; or Jev's pick among them under the same cut-offs. Then the lane's
                        focus_and_type, which pastes only when the focus really is that field
  press_key             one key. Return submits only a search the human dictated; anything else asks first.
  checkpoint            stop and hand back: the planner said it could not see past this point

Before a click or a type, effectors that know how (the browser page) dismiss a cookie/consent notice with a
button that refuses or closes; the sensitive table keeps any "accept"/"agree" button out of reach.

What happened to a step is one word from a closed set (cua's action-result contract, trycua/cua, MIT):
confirmed (a readback saw it), unverifiable (input was applied, nothing could be read back),
suspected_noop (input was applied and the screen did not change), refused (no input was applied).
`unverifiable` is never reported as success: the closing sentence says what could not be confirmed.

Who gets control when a plan stops:
  needs_human   a consequential step — the plan's flag, a sensitive label, Jev's risky noul, a Return outside
                a dictated search, composed text headed for a terminal. Zero input was applied for that step.
                The caller asks the human and calls run_plan again with `start` and `approved`.
  checkpoint /
  partial / none  the planner, with the ledger of what was applied. `none` guarantees zero input overall.

Nothing here decides a task is DONE on a model's word: `complete` means every step applied input that was
not a suspected no-op, every `expect` that was given held, and the plan's `success` did not read false.
"""

from __future__ import annotations

import re
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Mapping, Optional, Sequence

from .decider import render_option
from .fast_lane import editable_pool, is_search_field, jev_context, run_delegate
from .plan_contract import Expect, Plan, PlanStep, url_matches
from .typed_ask import JEV_STEP_GATES, StepAnswer, StepGates

STATUSES = ("complete", "partial", "none", "needs_human", "checkpoint", "unavailable")
EXPECT_WAIT_SECS = 2.5
EXPECT_POLL_SECS = 0.4
MAX_WALL_SECS = 30.0
# Typing composed text into a shell is running a command the human never said: ask before the first key.
SHELL_APPS = frozenset({"terminal", "warp", "iterm", "iterm2", "ghostty", "kitty", "alacritty", "wezterm"})


@dataclass
class Entry:
    index: int                     # 1-based, the plan's numbering
    step: str                      # PlanStep.describe()
    effect: str                    # confirmed | unverifiable | suspected_noop | refused
    how: str = ""                  # keyword | keyword+jev | jev | code | helper
    note: str = ""                 # the lane's own line, or why nothing was applied
    ms: float = 0.0

    def to_dict(self) -> dict[str, Any]:
        return {"index": self.index, "step": self.step, "effect": self.effect, "how": self.how,
                "note": self.note[:200], "ms": round(self.ms, 1)}


@dataclass
class PlanResult:
    status: str                    # STATUSES
    next_index: int = 0            # 0-based index of the step to resume from (needs_human, checkpoint, partial)
    reason: str = ""
    confirm: str = ""              # needs_human: what the human is asked about
    sentence: str = ""             # complete: what eve says
    ledger: list[Entry] = field(default_factory=list)
    log_lines: list[str] = field(default_factory=list)
    ms: float = 0.0

    @property
    def applied(self) -> list[Entry]:
        return [e for e in self.ledger if e.effect != "refused"]

    def to_dict(self) -> dict[str, Any]:
        return {"status": self.status, "next_index": self.next_index, "reason": self.reason,
                "confirm": self.confirm, "sentence": self.sentence, "ms": round(self.ms, 1),
                "ledger": [e.to_dict() for e in self.ledger]}


def _holds(expect: Optional[Expect], senses: Any, effectors: Any, frontmost_app: Callable[[], str],
           clock: Callable[[], float], wait: float = EXPECT_WAIT_SECS) -> Optional[bool]:
    """True/False once the expectation can be read, None when there is none or reading it failed.
    Polls up to `wait`: a page that is still loading is not a failed step."""
    if expect is None:
        return None
    needle = expect.value.casefold()
    t0 = clock()
    while True:
        try:
            if expect.kind == "app_frontmost":
                seen = needle in (frontmost_app() or "").casefold()
            else:
                snap = senses.snapshot()
                if expect.kind == "url_is":
                    getter = getattr(senses, "current_url", None)
                    seen = bool(getter and url_matches(getter(), expect.value))
                else:
                    seen = (needle in (snap.title or "").casefold() if expect.kind == "title_contains"
                            else bool(senses.text_visible(expect.value)))
        except Exception:  # noqa: BLE001 — an oracle that cannot read is unknown, never a no
            return False if expect.kind == "url_is" else None
        if seen:
            return True
        if clock() - t0 >= wait:
            return False
        try:
            effectors.settle(EXPECT_POLL_SECS)
        except Exception:  # noqa: BLE001
            return False


def _confirm_label(line: str, fallback: str) -> str:
    """The label inside the lane's `confirm: "{label}" needs the human's yes` line."""
    if line.startswith('confirm: "'):
        rest = line[len('confirm: "'):]
        end = rest.find('" needs the human')
        if end > 0:
            return rest[:end].replace('\\"', '"')
    return fallback


def _reason(line: str) -> str:
    """The `reason=` of the lane's escalate line, without what follows it (a dialog's quoted text)."""
    if "reason=" not in line:
        return ""
    return line.split("reason=", 1)[1].split(";", 1)[0].split(":", 1)[0].split(" (", 1)[0].strip()


def _say(plan: Plan, ledger: Sequence[Entry]) -> str:
    unsure = [e for e in ledger if e.effect == "unverifiable"]
    if not unsure and plan.final_say:
        return plan.final_say
    done = [e.step for e in ledger if e.effect == "confirmed"]
    if not unsure:
        return "Done." if not done else f"Done: {done[-1]}."
    if len(unsure) == len(ledger):
        return f"I did it, but I couldn't confirm it worked: {unsure[-1].step}."
    return f"I did it; the one thing I couldn't confirm was: {unsure[-1].step}."


def run_plan(plan: Plan, *, senses: Any, effectors: Any, asker: Optional[Callable[..., StepAnswer]],
             open_app: Callable[[str], str], open_url: Callable[[str], str], frontmost_app: Callable[[], str],
             start: int = 0, approved: Optional[Mapping[int, str]] = None, decide: str = "jev",
             dry_run: bool = False, planned_for: str = "",
             step_gates: StepGates = JEV_STEP_GATES, clock: Callable[[], float] = time.perf_counter) -> PlanResult:
    """Walk `plan.steps[start:]`. `approved` maps a 0-based step index the human has said yes to onto what
    they were asked about (PlanResult.confirm): for a sensitive control that is its label, which the lane
    lets through exactly once (fast_lane's `approve`).

    `planned_for` is the app whose window the planner was shown. Planning takes seconds and the human keeps
    using the Mac meanwhile (live, 2026-09-21: Finder was in front by the time the plan arrived), so when a
    plan opens no app of its own, that app is brought back to the front before the first step."""
    t_start = clock()
    out = PlanResult("none", next_index=start)
    ok = {int(i): str(label) for i, label in (approved or {}).items()}
    if decide == "jev" and asker is None:
        decide = "keyword"                                   # no hosted model configured: the gate alone decides
        out.log_lines.append("plan: jev is not configured; the keyword gate decides alone")
    typed: Optional[tuple[bool, str, str]] = None            # (into a search field, text_source, text) of the last type
    clicked = False                                          # the step before this one was a click that applied
    first = plan.steps[start] if start < len(plan.steps) else None
    if (planned_for and not dry_run and first is not None and first.kind not in ("open_app", "open_url")
            and planned_for.casefold() != (frontmost_app() or "").casefold()):
        try:
            out.log_lines.append(f"plan: {planned_for} is no longer in front — " + str(open_app(planned_for)))
        except Exception as e:  # noqa: BLE001 — the step's own gates decide what happens next
            out.log_lines.append(f"plan: could not bring {planned_for} back ({type(e).__name__})")

    def stop(status: str, index: int, reason: str = "", confirm: str = "") -> PlanResult:
        if status == "partial" and not out.applied:
            status = "none"                                   # zero input overall: the caller starts clean
        out.status, out.next_index, out.reason, out.confirm = status, index, reason, confirm
        out.ms = (clock() - t_start) * 1000.0
        out.log_lines.append(f"plan {status}: step {min(index + 1, len(plan.steps))}/{len(plan.steps)}"
                             + (f" {reason}" if reason else "") + (f' confirm="{confirm}"' if confirm else ""))
        return out

    for i in range(start, len(plan.steps)):
        step = plan.steps[i]
        t0 = clock()
        if clock() - t_start > MAX_WALL_SECS:
            return stop("partial", i, "out_of_time")
        if step.kind == "checkpoint":
            return stop("checkpoint", i + 1, "the planner asked to look again here")
        if step.consequential and i not in ok:
            return stop("needs_human", i, "the plan marked this step consequential", step.describe())

        if step.kind in ("click", "type") and not dry_run:
            _clear_consent(effectors, i, out)
        entry = Entry(i + 1, step.describe(), "refused")
        if dry_run and step.kind != "click":
            entry.note = "dry_run"
            out.ledger.append(entry)
            continue

        if step.kind in ("open_app", "open_url"):
            try:
                said = (open_app if step.kind == "open_app" else open_url)(step.target)
            except Exception as e:  # noqa: BLE001 — no such app, a refused URL: nothing was opened
                entry.note = f"{type(e).__name__}: {e}"
                out.ledger.append(entry)
                return stop("partial", i, f"{step.kind}_failed")
            entry.how, entry.note = "helper", str(said)
            entry.effect = "confirmed" if str(said).startswith("opened ") and " but " not in str(said) else "unverifiable"

        elif step.kind == "click":
            _grant(effectors, ok.get(i, ""))
            objectives = list(dict.fromkeys(o for o in (step.label_hint, step.target) if o))
            result = None
            for objective in objectives:
                result = run_delegate(objective, senses=senses, effectors=effectors, decider=None, decide=decide,
                                      asker=asker, max_steps=1, skip_if_selected=True, dry_run=dry_run,
                                      approve=ok.get(i) or None,
                                      step_gates=step_gates, clock=clock)
                out.log_lines.extend(result.log_lines)
                if result.status in ("stopped", "done", "confirm"):
                    break
            assert result is not None
            entry.note = result.line
            last = result.steps[-1] if result.steps else None
            entry.how = last.via if last is not None else "code"
            if result.status == "confirm" and i in ok and ok[i] != _confirm_label(result.line, ""):
                # The human already said yes to THIS step (the plan had flagged it); the lane now names the
                # control. Their yes covers it: one question a step, not two — at the effector's own gate too.
                _grant(effectors, _confirm_label(result.line, ""))
                result = run_delegate(objective, senses=senses, effectors=effectors, decider=None, decide=decide,
                                      asker=asker, max_steps=1, skip_if_selected=True, dry_run=dry_run,
                                      approve=_confirm_label(result.line, "") or None, step_gates=step_gates,
                                      clock=clock)
                out.log_lines.extend(result.log_lines)
                entry.note = result.line
                last = result.steps[-1] if result.steps else None
                entry.how = last.via if last is not None else "code"
            _grant(effectors, "")                            # a yes covers one step, never the next
            if result.status == "confirm":
                out.ledger.append(entry)
                return stop("needs_human", i, "a consequential control", _confirm_label(result.line, step.target))
            if result.status == "done":
                entry.effect = "confirmed"                       # already in the state the step asks for
            elif result.status == "stopped" and last is not None and last.verdict == "act":
                entry.effect = ("confirmed" if last.changed is True else
                                "suspected_noop" if last.changed is False else "unverifiable")
            elif result.status == "stopped" and dry_run:
                entry.note = "dry_run: " + result.line
                out.ledger.append(entry)
                continue
            else:
                out.ledger.append(entry)
                return stop("partial", i, _reason(result.line) or result.status)

        elif step.kind == "type":
            app = (frontmost_app() or "").casefold()
            if step.text_source == "composed" and app in SHELL_APPS and i not in ok:
                return stop("needs_human", i, "composed text into a shell", f'type "{step.text}" into {app}')
            field_c, how, why = _pick_field(step, senses, asker, step_gates, after_click=clicked)
            if field_c is None:
                entry.note = why
                out.ledger.append(entry)
                return stop("needs_human" if why == "risky" else "partial", i, why,
                            step.describe() if why == "risky" else "")
            try:
                said = effectors.focus_and_type(field_c, step.text)
            except Exception as e:  # noqa: BLE001 — an effector that raised applied nothing we can vouch for
                said = f"refused: {type(e).__name__}: {e}"
            entry.how, entry.note = how, str(said)
            if str(said).startswith("refused:"):
                out.ledger.append(entry)
                return stop("partial", i, "focus_changed")
            entry.effect = "unverifiable"                        # pasted; only an `expect` can confirm it
            typed = (is_search_field(field_c), step.text_source, step.text)

        elif step.kind == "press_key":
            submit = step.key in ("return",)
            dictated_search = typed is not None and typed[0] and typed[1] == "utterance"
            if submit and not dictated_search and i not in ok:
                what = f'press Return to submit "{typed[2]}"' if typed else "press Return"
                return stop("needs_human", i, "return outside a dictated search", what)
            try:
                said = effectors.press(step.key)
            except Exception as e:  # noqa: BLE001
                entry.note = f"{type(e).__name__}: {e}"
                out.ledger.append(entry)
                return stop("partial", i, "press_failed")
            entry.how, entry.note, entry.effect = "code", str(said), "unverifiable"
            typed = None

        if entry.effect != "refused" and step.kind != "click":
            try:
                effectors.settle(0.6)
            except Exception:  # noqa: BLE001 — a failed settle is a shorter wait
                pass
        held = _holds(step.expect, senses, effectors, frontmost_app, clock) if entry.effect != "refused" else None
        if held is True:
            entry.effect = "confirmed"
        entry.ms = (clock() - t0) * 1000.0
        out.ledger.append(entry)
        out.log_lines.append(f"plan step {i + 1}: {entry.step} -> {entry.effect}" + (f" ({entry.how})" if entry.how else ""))
        clicked = step.kind == "click" and entry.effect != "refused"
        if held is False:
            return stop("partial", i + 1, f'expected {step.expect.kind} "{step.expect.value}" and did not see it')
        if entry.effect == "suspected_noop":
            return stop("partial", i + 1, "the screen did not change")

    if dry_run:
        return stop("complete", len(plan.steps), "dry_run")
    final = _holds(plan.success, senses, effectors, frontmost_app, clock)
    if final is False:
        return stop("partial", len(plan.steps), f'the plan ran but "{plan.success.value}" is not on screen')
    if final is True:
        for e in out.ledger[-1:]:
            e.effect = "confirmed"
    out.sentence = _say(plan, out.ledger)
    return stop("complete", len(plan.steps))


def _grant(effectors: Any, approved: str) -> None:
    """Tell an effector with its own sensitive-label gate (browser_lane's page, desktop_helpers' adapter: an
    `approve` attribute) which label the human's yes covers for THIS click, normalised as they compare it;
    "" for none. Before this the page kept the first approval of a run as given — the step's description
    ("click button labelled Send message"), never a label — so an approved sensitive click was refused at the
    page and read as hit_test_failed (browser_model_eval smoke run, contact_form, 2026-09-24)."""
    if hasattr(effectors, "approve"):
        effectors.approve = " ".join(str(approved or "").split()).casefold()


def _clear_consent(effectors: Any, i: int, out: PlanResult) -> None:
    """Before a click or a type: an effector that can dismiss a cookie/consent notice (browser_lane's page,
    CONSENT_TEXT) does, with a button that refuses or closes, never one that agrees. What it pressed goes in
    the ledger like any applied input. Effectors without the ability (the Mac's) are left alone."""
    clear = getattr(effectors, "clear_consent", None)
    if not callable(clear):
        return
    try:
        said = str(clear() or "")
    except Exception as e:  # noqa: BLE001 — the step's own dialog gate decides what happens next
        said = f"refused: {type(e).__name__}"
    if not said:
        return
    out.log_lines.append(f"plan step {i + 1}: {said}")
    if not said.startswith("refused"):
        out.ledger.append(Entry(i + 1, "dismiss the cookie notice", "confirmed", how="code", note=said))


def _norm(s: str) -> str:
    return " ".join(str(s or "").split()).casefold()


def _named_field(step: PlanStep, pool: Sequence[Any]) -> Optional[Any]:
    """The one field the step names, or None. A label equal to the step's label_hint, else the longest field
    label that appears as whole words in the hint or the target ("the focused Full name text field" names
    "Full name"; "the Name for the booking field" names that one, not a field called "Name"). Two fields with
    the winning label name nothing."""
    hint = _norm(step.label_hint)
    if hint:
        exact = [c for c in pool if _norm(c.label) == hint]
        if len(exact) == 1:
            return exact[0]
    said = f"{hint} {_norm(step.target)}"
    hits = [c for c in pool if _norm(c.label) and re.search(
        r"(?<![a-z0-9])" + re.escape(_norm(c.label)) + r"(?![a-z0-9])", said)]
    if not hits:
        return None
    longest = max(len(_norm(c.label)) for c in hits)
    best = [c for c in hits if len(_norm(c.label)) == longest]
    return best[0] if len(best) == 1 else None


def _pick_field(step: PlanStep, senses: Any, asker: Optional[Callable[..., StepAnswer]], gates: StepGates,
                after_click: bool = False) -> tuple[Any, str, str]:
    """(the editable candidate to type into, who picked it, "" | why not). A lone field is code's pick.
    Among several, code PROPOSES the field the step names (its label, _named_field) or, right after the plan
    clicked a field, the focused one; Jev is asked as on a click and can REFUSE: the proposal is typed into
    when Jev's top field is the same one. Without a proposal it is Jev's pick under the step cut-offs — the
    wording was measured on pressable controls, not fields, so a pick that does not clear them types nothing.
    Eval 2026-09-24 (tools/browser_model_eval.py, contact_form): Jev alone never cleared the cut-offs among
    three labelled fields, even right after the plan had clicked the field, so every model's form fill
    stopped at ambiguous_field."""
    try:
        snap = senses.snapshot()
    except Exception as e:  # noqa: BLE001
        return None, "", f"no_window ({type(e).__name__})"
    if snap.dialog_text:
        return None, "", "dialog_open"
    pool = editable_pool(snap)
    if not pool:
        return None, "", "no_field"
    if len(pool) == 1:
        return pool[0], "code", ""
    proposal = _named_field(step, pool)
    focused = snap.focused
    if proposal is None and after_click and focused is not None:
        proposal = next((c for c in pool if c.id == focused.id), None)
    if asker is None:
        return (proposal, "code", "") if proposal is not None else (None, "", "ambiguous_field")
    by_key = {str(n): c for n, c in enumerate(pool, start=1)}
    answer = asker(f"type into {step.target}", app=snap.app, context=jev_context(snap),
                   options={k: render_option(c) for k, c in by_key.items()}, recent=())
    if answer.error or not answer.target:
        return None, "jev", "ambiguous_field"
    if answer.risky >= gates.risky_max:
        return None, "jev", "risky"
    if proposal is not None and by_key.get(answer.target) is proposal:
        return proposal, "code+jev", ""
    pick = gates.decide(answer)
    if pick == "confirm":
        return None, "jev", "risky"
    if pick in by_key:
        return by_key[pick], "jev", ""
    return None, "jev", "ambiguous_field"
