"""Jev uses the app the way it is meant to be used, and says whether it did what it is for.

Owner, 2026-09-24: "Can u use jev as the verification layer instead of claude", after an expense splitter passed
the phone check and still "doesn't work". The scripted check (app_check.py) fills every field and taps every
button blindly: it finds crashes, a page that never saves, text that does not read in the dark. It cannot tell
whether adding an expense shows who owes whom.

So the division of labour is: Claude (the maker) writes and repairs the code, and with the page it writes 2-4
JOURNEYS, the app's core purpose as a person would walk it: steps ("tap 'Add expense'", "type '30' into
'Amount'", "choose 'Sam' in 'Paid by'") and the literal text that must be on screen afterwards. Jev (TypeSafe's
hosted typed-decision model, jev.py) is the verifier: it does each step on the real page and judges each
expectation. Code decides from Jev's numbers; a journey that does not go through becomes a problem in the same
CheckReport, so the repair loop sends it back to Claude like any other.

HOW JEV IS ASKED (typed_ask.py's protocol guide, "ask each model natively"). Jev is a literal reader, weak at
counting and indirection, and loses accuracy in a big state. So:
  - one step is ONE request: a `choice` over the controls on screen (only the kinds the step can act on: a
    tap never picks a field) plus an explicit "none", and beside it an absolute `noul`, "is the control the step
    names in the list at all". The choice alone never acts: a relative pick always names something.
  - the controls are in the state too, labels only, because a noul is answered against the state (typed_ask:
    with the list only in the choice, `present` could not see it). The state is the step, a short screen
    summary and the rendered controls ("Amount (number field, e.g. 24.50)"). Nothing else.
  - after the steps, ONE request with one `noul` per expectation over the screen's text lines, trimmed.
    Expectations are literal text by contract (the maker is told why), never counts or comparisons.
  - the cut-offs are fitted on real apps and scored on a held-out half (JOURNEY_STEP_GATES, EXPECT_SHOWN
    below, with how they were measured), never carried over from the click path's.

WHAT LEAVES THE MAC: each journey starts from an EMPTY app (no saved data), so what Jev sees is the app's own
labels and screen text plus the journey's example values. An edit's prompt shows Claude a sample of the owner's
data, which a journey could copy: apps_maker.without_owner_data keeps any journey holding a word of it (one the
app's page does not hold) from being walked, and sends it back as a problem. Jev off (no key, a timeout, the
network) never blocks a build: the journeys are reported "not run (why)" and the check stands.
"""

from __future__ import annotations

import asyncio
import json
import logging
import math
import os
import re
import statistics
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Mapping, Optional

from . import jev
from .pricing import estimate_jev_cost
from .typed_ask import StepAnswer, StepGates, _choice, _noul

log = logging.getLogger(__name__)

KINDS = ("tap", "type", "choose")
MIN_JOURNEYS, MAX_JOURNEYS = 1, 4
MAX_STEPS = 12
MAX_EXPECT = 5
MAX_CONTROLS = 48          # options per step: Jev's step eval ran 26 at 234 ms p50; a busy screen has ~30
MAX_LINES = 150            # screen text lines per expectation request: a month calendar is ~60 lines of
                           # single numbers, and at 60 the result under it was cut (calibration, 2026-09-24)
MAX_TEXT_CHARS = 5000      # what is read off the page; Jev gets a shortlist of it (relevant_lines)
MAX_EXPECT_LINES = 30      # the lines one expectation request carries
MAX_LINE_CHARS = 120
SUMMARY_CHARS = 300
STEP_WAIT_MS = 350         # after an action: a render, a toast, a view change
JOURNEY_TIMEOUT_S = 90.0   # every journey of one check, run side by side
JEV_TIMEOUT_S = 6.0        # a verification call: not the click path's 3 s, and never a build blocker
EVAL_TIMEOUT_S = 5.0       # one read of the page: an app caught in an endless loop never answers it
LOCATE_MS = 1500           # how long an action waits for its control to be usable
STEP_NONE = "none"

# What each kind of step may act on. Code decides the kind (a tap never types), Jev decides which one.
ACTS_ON = {
    "tap": frozenset({"button", "tab", "checkbox", "radio", "link", "item", "main", "back"}),
    "type": frozenset({"field"}),
    "choose": frozenset({"select", "radio", "tab", "button", "checkbox", "item"}),
}

# ---- the journeys contract ------------------------------------------------------------------------------


@dataclass(frozen=True)
class Step:
    do: str                   # tap | type | choose
    target: str               # the visible label of the control
    value: str = ""           # the text to type, or the option to pick

    def words(self) -> str:
        """The step as Jev reads it, and as a person reads a failure."""
        if self.do == "type":
            return f'type "{self.value}" into "{self.target}"'
        if self.do == "choose":
            return f'choose "{self.value}" in "{self.target}"'
        return f'tap "{self.target}"'

    def as_dict(self) -> dict[str, str]:
        d = {"do": self.do, "target": self.target}
        if self.do != "tap":
            d["value"] = self.value
        return d


@dataclass(frozen=True)
class Journey:
    name: str
    steps: tuple[Step, ...]
    expect: tuple[str, ...]

    def as_dict(self) -> dict[str, Any]:
        return {"name": self.name, "steps": [s.as_dict() for s in self.steps], "expect": list(self.expect)}


def _text(v: Any, limit: int) -> str:
    return " ".join(str(v).split())[:limit] if isinstance(v, (str, int, float)) and not isinstance(v, bool) else ""


def parse_journeys(value: Any) -> tuple[list[Journey], list[str]]:
    """The journeys in ``value`` (the parsed ```journeys block), and what was wrong with the ones dropped. A
    journey with a step that cannot be done as written, or with nothing to expect, is dropped whole: half a
    journey would verify something the maker did not mean."""
    if not isinstance(value, list):
        return [], ["The journeys block is not a JSON list."]
    out: list[Journey] = []
    problems: list[str] = []
    for n, j in enumerate(value, 1):
        if len(out) >= MAX_JOURNEYS:
            problems.append(f"Only {MAX_JOURNEYS} journeys are run; journey {n} and later were left out.")
            break
        if not isinstance(j, dict):
            problems.append(f"Journey {n} is not an object.")
            continue
        name = _text(j.get("name"), 80) or f"Journey {n}"
        raw_steps = j.get("steps") if isinstance(j.get("steps"), list) else []
        steps: list[Step] = []
        bad = ""
        for k, s in enumerate(raw_steps[:MAX_STEPS], 1):
            do = _text(s.get("do"), 10).lower() if isinstance(s, dict) else ""
            target = _text(s.get("target"), 80) if isinstance(s, dict) else ""
            val = _text(s.get("value"), 120) if isinstance(s, dict) else ""
            if do not in KINDS or not target or (do != "tap" and not val):
                bad = (f'Journey "{name}" step {k} is not a tap, type or choose with a target'
                       + (" and a value" if do in ("type", "choose") else "") + ".")
                break
            steps.append(Step(do, target, val if do != "tap" else ""))
        expect = [e for e in (_text(x, 160) for x in (j.get("expect") if isinstance(j.get("expect"), list) else []))
                  if e][:MAX_EXPECT]
        if bad:
            problems.append(bad)
        elif not steps:
            problems.append(f'Journey "{name}" has no steps.')
        elif not expect:
            problems.append(f'Journey "{name}" expects nothing on screen afterwards.')
        else:
            out.append(Journey(name, tuple(steps), tuple(expect)))
    if len(out) < MIN_JOURNEYS:
        # an empty block would switch the verifier off for this app for good: the build keeps the journeys it
        # had, and the round is told
        problems.append("The journeys block holds no journey that can be walked; write 2 to 4.")
    return out, problems


_BLOCK = re.compile(r"```journeys[ \t]*\n(.*?)```", re.S | re.I)


def extract_journeys(text: str) -> tuple[Optional[list[Journey]], list[str]]:
    """The ```journeys block of a maker answer: (journeys, problems). (None, []) when the answer has no block, so
    the journeys already in hand stay; ([], problems) when it has one that does not parse or holds none that can
    be walked (the journeys in hand stay then too, and the problem goes to the repair round)."""
    blocks = _BLOCK.findall(text or "")
    if not blocks:
        return None, []
    try:
        value = json.loads(blocks[-1])
    except ValueError as e:
        return [], [f"The journeys block is not valid JSON ({str(e)[:80]})."]
    return parse_journeys(value)


def journeys_json(journeys: list[Journey]) -> list[dict[str, Any]]:
    return [j.as_dict() for j in journeys]


# ---- what one step sees ----------------------------------------------------------------------------------

# Every labelled control on screen, in document order, tagged data-journey-id for the action. A field's label
# is what a person reads next to it, its placeholder the example apart. A list row that is tapped (the cursor
# says so) and holds no control of its own is an "item". Telegram's MainButton and BackButton are controls too:
# the app's primary action often lives there.
COLLECT_JS = r"""
() => {
  const shown = (el) => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el);
    return r.width > 0 && r.height > 0 && s.visibility !== "hidden" && s.display !== "none"
      && !el.closest("[hidden],[aria-hidden=true],[inert]"); };
  const squash = (t, n) => (t || "").trim().replace(/\s+/g, " ").slice(0, n || 70);
  const labelled = (el) => {
    const aria = el.getAttribute("aria-label"); if (aria && aria.trim()) return aria;
    const by = el.getAttribute("aria-labelledby");
    if (by) { const t = by.split(/\s+/).map((i) => document.getElementById(i)).filter(Boolean).map((e) => e.textContent).join(" ");
      if (t.trim()) return t; }
    if (el.id) { const l = document.querySelector(`label[for="${CSS.escape(el.id)}"]`); if (l && l.textContent.trim()) return l.textContent; }
    const wrap = el.closest("label");
    if (wrap) { const c = wrap.cloneNode(true); c.querySelectorAll("input,select,textarea").forEach((x) => x.remove());
      if (c.textContent.trim()) return c.textContent; }
    return "";
  };
  const fieldSel = "input:not([type]),input[type=text],input[type=number],input[type=date],input[type=time],"
    + "input[type=datetime-local],input[type=month],input[type=email],input[type=url],input[type=tel],"
    + "input[type=search],textarea,select";
  const pressSel = "button,[role=button],[role=tab],[role=checkbox],[role=switch],[role=radio],[role=menuitem],"
    + "[role=option],input[type=checkbox],input[type=radio],input[type=submit],input[type=button],a[href],summary,"
    + "[onclick],label:has(input[type=checkbox],input[type=radio])";
  // a tag from the last screen may sit on an element now hidden: it would win the next step's locator
  document.querySelectorAll("[data-journey-id]").forEach((e) => e.removeAttribute("data-journey-id"));
  const out = [], seen = new Set();
  const kindOf = (el) => {
    const t = el.tagName, type = (el.getAttribute("type") || "").toLowerCase(), role = el.getAttribute("role") || "";
    if (t === "SELECT") return "select";
    if (t === "TEXTAREA" || (t === "INPUT" && !["checkbox", "radio", "submit", "button"].includes(type))) return "field";
    if (role === "tab") return "tab";
    if (type === "checkbox" || role === "checkbox" || role === "switch" || (t === "LABEL" && el.querySelector("input[type=checkbox]"))) return "checkbox";
    if (type === "radio" || role === "radio" || role === "option" || (t === "LABEL" && el.querySelector("input[type=radio]"))) return "radio";
    if (t === "A") return "link";
    return "button";
  };
  const on = (el) => el.getAttribute("aria-selected") === "true" || el.getAttribute("aria-pressed") === "true"
    || el.getAttribute("aria-checked") === "true" || el.checked === true
    || (el.hasAttribute("aria-current") && el.getAttribute("aria-current") !== "false")
    || /(^|\s)(active|selected|current|is-active|is-selected)(\s|$)/.test(el.getAttribute("class") || "");
  for (const el of document.querySelectorAll(fieldSel + "," + pressSel)) {
    if (seen.has(el) || !shown(el) || el.disabled) continue;
    if (el.tagName === "INPUT" && el.closest("label") && seen.has(el.closest("label"))) continue;
    if (el.closest("button,a[href],[role=button]") && el.closest("button,a[href],[role=button]") !== el) continue;
    seen.add(el);
    const kind = kindOf(el);
    const isField = kind === "field" || kind === "select";
    const label = isField ? squash(labelled(el) || el.getAttribute("placeholder") || el.getAttribute("name"))
      : squash(el.getAttribute("aria-label") || el.innerText || el.textContent || el.value || el.getAttribute("title"));
    if (!label) continue;
    const c = { kind, label, type: (el.getAttribute("type") || (el.tagName === "TEXTAREA" ? "textarea" : "")).toLowerCase(),
      placeholder: isField ? squash(el.getAttribute("placeholder"), 40) : "", value: "", options: [], on: false };
    if (kind === "field") c.value = squash(el.value, 40);
    if (kind === "select") { c.options = Array.from(el.options).slice(0, 12).map((o) => squash(o.textContent, 30));
      c.value = el.selectedIndex >= 0 ? squash(el.options[el.selectedIndex].textContent, 30) : ""; }
    if (!isField) c.on = on(el);
    el.setAttribute("data-journey-id", String(out.length));
    c.id = String(out.length);
    out.push(c);
  }
  // rows tapped by a listener: the outermost element with a pointer cursor that holds no control and sits in none
  const tagged = Array.from(document.querySelectorAll("[data-journey-id]"));
  for (const el of document.querySelectorAll("body *")) {
    if (out.length >= 120) break;
    if (getComputedStyle(el).cursor !== "pointer" || !shown(el)) continue;
    if (el.parentElement && getComputedStyle(el.parentElement).cursor === "pointer") continue;
    if (el.closest("[data-journey-id]") || tagged.some((t) => el.contains(t))) continue;
    const label = squash(el.innerText || el.getAttribute("aria-label"));
    if (!label) continue;
    el.setAttribute("data-journey-id", String(out.length));
    out.push({ id: String(out.length), kind: "item", label, type: "", placeholder: "", value: "", options: [], on: false });
  }
  const w = window.Telegram && window.Telegram.WebApp;
  const main = w && w.MainButton && w.MainButton.isVisible && w.MainButton.isActive !== false && w.MainButton._cbs
    && w.MainButton._cbs.length ? squash(w.MainButton.text || "MainButton") : "";
  const back = !!(w && w.BackButton && w.BackButton.isVisible && w.BackButton._cbs && w.BackButton._cbs.length);
  const h = Array.from(document.querySelectorAll("h1,h2,[role=heading]")).find(shown);
  return { controls: out, heading: h ? squash(h.textContent, 60) : "", main, back,
           text: (document.body && document.body.innerText || "") };
}
"""

# The text a person can read on the page now, line by line. innerText leaves out what is display:none. Telegram
# draws the MainButton under the page, so its text is on the screen too ("Clear 1 checked" is an app's result).
TEXT_JS = r"""
() => { const w = window.Telegram && window.Telegram.WebApp, b = w && w.MainButton;
  return ((document.body && document.body.innerText) || "") + (b && b.isVisible && b.text ? "\n" + b.text : ""); }
"""

PRESS_JS = r"""
(name) => { const b = window.Telegram && window.Telegram.WebApp && window.Telegram.WebApp[name];
  if (!b || !b.isVisible || b.isActive === false || !b._cbs || !b._cbs.length) return false;
  b._cbs.slice().forEach((f) => f()); return true; }
"""


@dataclass(frozen=True)
class Control:
    id: str                   # data-journey-id, or "main" / "back" for Telegram's own buttons
    kind: str
    label: str
    type: str = ""
    placeholder: str = ""
    value: str = ""
    options: tuple[str, ...] = ()
    on: bool = False

    def render(self) -> str:
        """The option text Jev reads: the label, then what kind of thing it is, in a few words."""
        if self.kind == "main":
            return f"{self.label} (Telegram's main button at the bottom of the screen)"
        if self.kind == "back":
            return "Back (Telegram's back arrow)"
        if self.kind == "field":
            kind = {"number": "number field", "date": "date field", "time": "time field", "textarea": "text box",
                    "search": "search field", "email": "email field"}.get(self.type, "text field")
            extra = f", e.g. {self.placeholder.removeprefix('e.g.').strip()}" if self.placeholder and \
                self.placeholder != self.label else ""
            return f"{self.label} ({kind}{extra})"
        if self.kind == "select":
            opts = ", ".join(self.options[:8]) + ("…" if len(self.options) > 8 else "")
            return f"{self.label} (dropdown: {opts})"
        word = {"item": "list item", "radio": "option", "tab": "tab", "checkbox": "checkbox", "link": "link"}.get(
            self.kind, "button")
        return f"{self.label} ({word}{', selected' if self.on else ''})"


@dataclass(frozen=True)
class Screen:
    controls: tuple[Control, ...]
    heading: str
    lines: tuple[str, ...]

    def summary(self) -> str:
        """A few words of what is on screen: the heading, then the first lines, short."""
        text = " | ".join(x for x in ((self.heading,) + self.lines[:8]) if x)
        return text[:SUMMARY_CHARS]


def screen_lines(text: str) -> tuple[str, ...]:
    """The page text as short, distinct, non-empty lines, in order: the state an expectation is judged on."""
    seen: set[str] = set()
    out: list[str] = []
    size = 0
    for raw in str(text or "").splitlines():
        line = " ".join(raw.split())[:MAX_LINE_CHARS]
        if line and line not in seen:
            seen.add(line)
            out.append(line)
            size += len(line) + 1
        if len(out) >= MAX_LINES or size >= MAX_TEXT_CHARS:
            break
    return tuple(out)


_STOP = frozenset({"a", "an", "the", "of", "to", "in", "on", "for", "and", "or", "at", "is", "by", "with", "your",
                   "you", "it", "this", "that", "no", "not"})


def _words(text: str) -> set[str]:
    return {w for w in re.findall(r"[^\W\d_]+|\d+", text.casefold()) if w not in _STOP and (len(w) > 1 or w.isdigit())}


def relevant_lines(lines: tuple[str, ...], expects: tuple[str, ...], limit: int = MAX_EXPECT_LINES) -> tuple[str, ...]:
    """The screen lines worth judging ``expects`` against: each line that shares a word or a number with one of
    them, with the line before and after it (a result split over two lines), in screen order. Jev suffers
    context rot: on the whole screen (a month calendar is 60 lines of single numbers) it read 0.28 for
    "average mood · Good" that was plainly there, and 0.12 for a MainButton text at the end (calibration,
    2026-09-24). A near miss stays in: "$15.00" is shortlisted for an expected "$25.00" by its words, and Jev
    must say no to it. With nothing in common, the first lines go, so the answer is an informed no."""
    want = set().union(*(_words(e) for e in expects)) if expects else set()
    score = [len(_words(line) & want) for line in lines]
    hits = sorted((i for i, n in enumerate(score) if n), key=lambda i: (-score[i], i))
    keep: set[int] = set()
    for i in hits:
        near = {j for j in (i - 1, i, i + 1) if 0 <= j < len(lines)}
        if len(keep | near) > limit:
            break
        keep |= near
    if not keep:
        return lines[:limit]
    return tuple(lines[i] for i in sorted(keep))


_NUMBER = re.compile(r"\d(?:[\d,.]*\d)?")


def _numbers(text: str) -> set[str]:
    """The numbers in ``text``, written one way: no thousands commas, no trailing decimal zeros, no leading zeros
    ("$1,234.50" -> 1234.5, "30.00" -> 30, "09" -> 9)."""
    out = set()
    for raw in _NUMBER.findall(text):
        n = raw.replace(",", "")
        if "." in n:
            n = n.rstrip("0").rstrip(".")
        out.add(n.lstrip("0") or "0")
    return out


def numbers_shown(expect: str, lines: tuple[str, ...]) -> bool:
    """Code's veto on a "shown": every number the expectation holds is a number on the screen. Jev is weak at
    counting (typed_ask's guide), and read 0.90 for "1 book" on a screen with no 1 on it (calibration,
    2026-09-24); this cannot make a missing result pass, only stop a number Jev did not really see."""
    return _numbers(expect) <= _numbers(" ".join(lines))


_TOKEN = re.compile(r"\d(?:[\d,.]*\d)?|[^\W\d_]+")


def _tokens(text: str) -> list[str]:
    """``text`` as words and numbers, in order: case, punctuation, emoji and line breaks dropped, numbers
    written one way (as _numbers does)."""
    out = []
    for w in _TOKEN.findall(text.casefold()):
        if w[0].isdigit():
            n = w.replace(",", "")
            w = (n.rstrip("0").rstrip(".") if "." in n else n).lstrip("0") or "0"
        out.append(w)
    return out


def words_shown(expect: str, lines: tuple[str, ...]) -> bool:
    """Code's second veto: the expectation's words and numbers stand on the screen in its order, next to each
    other (line breaks, case, punctuation and number format aside). Jev reads a bag of words too kindly when
    they stay and the meaning goes: a live build passed "1-day streak" over "Streak ended · best 1 day" at
    0.56 to 0.62 across ten asks, and "Sam owes Alex ended · best $16.20" read up to 0.89 for "Sam owes Alex
    $16.20" (calibration, 2026-09-24). Expectations are literal text by contract, so this never fails one the
    screen really shows."""
    want = _tokens(expect)
    if not want:
        return True
    have = _tokens("\n".join(lines))
    return any(have[i:i + len(want)] == want for i in range(len(have) - len(want) + 1))


def screen_from(raw: Mapping[str, Any]) -> Screen:
    controls = [Control(id=str(c.get("id")), kind=str(c.get("kind") or "button"), label=str(c.get("label") or ""),
                        type=str(c.get("type") or ""), placeholder=str(c.get("placeholder") or ""),
                        value=str(c.get("value") or ""), options=tuple(str(o) for o in c.get("options") or ()),
                        on=bool(c.get("on"))) for c in raw.get("controls") or []]
    if raw.get("main"):
        controls.append(Control("main", "main", str(raw["main"])))
    if raw.get("back"):
        controls.append(Control("back", "back", "Back"))
    return Screen(tuple(controls), str(raw.get("heading") or ""), screen_lines(str(raw.get("text") or "")))


# ---- asking Jev ------------------------------------------------------------------------------------------

STEP_ABOUT = ("One step of a test of a small phone app. The controls are the labelled buttons, fields, tabs and "
              "list items on the app's screen now, each with what kind of control it is.")
EXPECT_ABOUT = "The text on a small phone app's screen, line by line, after a test used the app."


# What does not make a label another label. A list row's label is its whole text, so a row named "Weekly groceries"
# reads "Weekly groceries Thu, Sep 24 · $12.50" (calibration: 15 of the 340 real steps named a row or a button
# by the words before such details).
SAME_LABEL = ('A leading icon, emoji or "+", letter case, and details after the name (a count, a date, an amount or '
              "a status, as a list row shows) do not matter.")


def step_questions(step: Step, options: Mapping[str, str]) -> dict[str, Any]:
    """The one request for one step. The label is IN the instructions: Jev is literal, and "the step's label"
    is one indirection too many (the single right field of a form read 0.61 that way)."""
    criteria = dict(options)
    criteria[STEP_NONE] = f'no control here is labelled "{step.target}"'
    return {
        "target": {"type": "choice", "instructions": (
            f'Which control is labelled "{step.target}"? {SAME_LABEL} A control that only shares a word with it, or '
            "does something related, is not it."), "criteria": criteria},
        "present": {"type": "noul", "instructions": (
            f'Is a control labelled "{step.target}" in the list of controls? {SAME_LABEL} Answer no when no control '
            "carries this label, even when a similar or related control is there.")},
    }


def expect_questions(expects: list[str]) -> dict[str, Any]:
    return {f"e{i}": {"type": "noul", "instructions": (
        f"Does the screen text show: {e} ? Answer yes when this text is on the screen, allowing only a different "
        "letter case, spacing, line break or number format of the same amount. Answer no when a name, number or "
        "amount differs, or when it is not there.")} for i, e in enumerate(expects, 1)}


def expect_state(lines: tuple[str, ...], expects: tuple[str, ...]) -> dict[str, Any]:
    return {"about": EXPECT_ABOUT, "screen": list(relevant_lines(lines, expects))}


def step_state(step: Step, screen: Screen, options: Mapping[str, str]) -> dict[str, Any]:
    return {"about": STEP_ABOUT, "screen": screen.summary(), "step": step.words(), "controls": list(options.values())}


def offered(step: Step, screen: Screen) -> dict[str, Control]:
    """The controls a step of this kind can act on, by id, at most MAX_CONTROLS, Telegram's buttons kept."""
    fits = [c for c in screen.controls if c.kind in ACTS_ON[step.do]]
    if len(fits) > MAX_CONTROLS:
        tg = [c for c in fits if c.kind in ("main", "back")]
        fits = [c for c in fits if c.kind not in ("main", "back")][:MAX_CONTROLS - len(tg)] + tg
    return {f"c{n}": c for n, c in enumerate(fits, 1)}


def read_step(answers: Mapping[str, Any], options: Mapping[str, str], ms: float) -> StepAnswer:
    """A step answer as typed_ask.StepAnswer, so its gates and its fitter are the click path's code (the cut-offs
    are this question's own). `already_done` and `risky` are not asked here: 0."""
    key, p, probs = _choice(answers.get("target"))
    k = len(options) + 1
    if key != STEP_NONE and key not in options:
        return StepAnswer("", 0.0, 0.0, 0.0, 0.0, 0.0, ms, k, f"target {key!r} is not an option")
    others = sorted((v for name, v in probs.items() if name != key), reverse=True)
    return StepAnswer(target=key, p_target=p, margin=max(0.0, p - (others[0] if others else 0.0)),
                      present=_noul(answers.get("present")), already_done=0.0, risky=0.0, ms=ms, k=k)


# Fitted 2026-09-24 by tools/journey_eval.py on the 17 apps built overnight (11 kinds), with journeys the maker's own
# prompt wrote for them (Claude, $2.25; 63 journeys), walked in app_check's sandbox with a code oracle acting and
# every step the oracle could not place read by hand (15 of 349: all rows or buttons named by the words before
# their details). Split by KIND: fitted on 6 (baby log, grocery list, monthly budget, pomodoro, trip splitter,
# workout log), zero wrong acts allowed (typed_ask.fit_step_gates); held out 5 (flashcards, habit tracker, mood
# journal, reading list, water tracker). Held-out steps, 367: 127 real, 120 with the label swapped for another
# app's, 120 with the right control removed from the screen:
#   under these gates    acted 127, right 124, wrong 3   precision 97.6%  recall 97.6%  abstained on 237 of 240
#   the choice alone     acted 138, right 127, wrong 11  precision 92.0%  recall 100%
# The 3 wrong: a habit's row removed, and its check toggle "Drink water: done today" taken for "Drink water".
# 1,388 requests over OpenRouter: 244 ms p50, 357 ms p90, 684 input tokens on average ($0.00003 a request).
JOURNEY_STEP_GATES = StepGates(present=0.8, p_target=0.9, margin=0.3, already_done=2.0, risky_max=2.0)
# Refitted 2026-09-24 (review round 3) after a live build let "1-day streak" pass over "Streak ended · best 1 day"
# (Jev 0.56 to 0.62 over ten asks, then cut at 0.4). The calibration had no negative where the words and numbers
# stay and the meaning goes, so journey_eval.flips now adds them: two names or numbers trading places ("swap") and
# the phrase broken up around its number ("reword"). Same apps, same split, zero false "shown" allowed on the
# calibration kinds (74 shown, 181 not): the lowest such cut on Jev's noul is 0.6 (0.4 passed 12 rewords there).
# Held-out, 181 expectations (54 shown; 127 not: 27 a number changed, 25 another app's, 51 the first screen, 19
# rewords, 4 swaps, 1 hand-labelled):
#   noul >= 0.6, numbers_shown, words_shown   53 of 54 shown, 0 false   precision 100%  recall 98.1%
#   noul >= 0.6 alone                         53 of 54 shown, 6 false (5 rewords, 1 first screen)
# The one missed is "Tracking since" (Jev 0.47, plainly there). Beside it, the plain normalized-substring check the
# labels were drawn with scores 54 of 54 and 0 false on this set: the positives are that oracle's (plus hand
# labels for number formats), so words_shown agrees with them largely by construction, and on this data Jev adds
# no catch the code does not make. The unread set (journey_eval.py score, 6 apps the maker built with its own
# journeys, 166 expectations with 25 rewords and 9 swaps): 45 of 45 shown, 0 of 121 absent passed; without
# words_shown, 0.6 would pass 11 of the 25 rewords. Jev: 1,457 requests, 243 ms p50, 356 ms p90, 673 input tokens each.
EXPECT_SHOWN = 0.6


class JevDown(RuntimeError):
    """Jev could not answer (no key, the network, a timeout, a wrong envelope): the journeys are not run."""


@dataclass
class JevCalls:
    """The Jev requests of one check: jev.Meter's counts plus each request's milliseconds, for p50/p90."""

    meter: jev.Meter = field(default_factory=jev.Meter)
    ms: list[float] = field(default_factory=list)

    def add(self, payload: Any, ms: float, *, error: bool = False) -> None:
        self.meter.add(payload, ms, error=error)
        self.ms.append(ms)

    def as_dict(self) -> dict[str, Any]:
        snap = self.meter.snapshot()
        ms = sorted(self.ms)
        p90 = ms[min(len(ms) - 1, math.ceil(0.9 * len(ms)) - 1)] if ms else 0.0
        return {**snap, "usd": round(estimate_jev_cost(snap["input_tokens"]), 6),
                "p50_ms": round(statistics.median(ms)) if ms else 0, "p90_ms": round(p90)}


Predict = Callable[[Any, dict[str, Any]], dict[str, Any]]


class Verifier:
    """Jev behind the two questions a journey asks. Every request runs in a thread (jev's predict is blocking),
    is timed into ``calls``, and raises JevDown on any failure: a verifier that cannot answer never fails an
    app, it stops the journeys."""

    def __init__(self, predict: Predict, *, step_gates: StepGates = JOURNEY_STEP_GATES,
                 expect_shown: float = EXPECT_SHOWN, record: Optional[list[dict[str, Any]]] = None) -> None:
        self._predict = predict
        self.step_gates, self.expect_shown = step_gates, expect_shown
        self.calls = JevCalls()
        self.record = record                    # every request and its answer, for calibration (journey_eval)

    async def _ask(self, state: dict[str, Any], questions: dict[str, Any]) -> tuple[dict[str, Any], float]:
        # Asked a second time before it counts as down: jev.make_predict retries only a 429 or a 5xx, and one
        # read timeout among the 30-40 requests of a check would otherwise drop every journey.
        for attempt in (1, 2):
            t0 = time.perf_counter()
            try:
                result = await asyncio.to_thread(self._predict, state, questions)
                break
            except Exception as e:  # noqa: BLE001 — JevError, a timeout, anything: the verifier is down
                self.calls.add({}, (time.perf_counter() - t0) * 1000.0, error=True)
                if attempt == 2:
                    raise JevDown(str(e)[:160] or type(e).__name__) from None
        ms = (time.perf_counter() - t0) * 1000.0
        self.calls.add(result, ms)
        answers = result.get("answers") if isinstance(result, dict) else None
        if not isinstance(answers, dict) or not all(q in answers for q in questions):
            raise JevDown("Jev answered without the questions asked")
        if self.record is not None:
            self.record.append({"state": state, "questions": questions, "answers": answers, "ms": round(ms)})
        return answers, ms

    async def pick(self, step: Step, screen: Screen) -> tuple[Optional[Control], StepAnswer, dict[str, Control]]:
        """The control the step means, or None; Jev's answer; the controls it was offered."""
        by_id = offered(step, screen)
        if not by_id:
            return None, StepAnswer(STEP_NONE, 1.0, 1.0, 0.0, 0.0, 0.0), by_id
        options = {k: c.render() for k, c in by_id.items()}
        answers, ms = await self._ask(step_state(step, screen, options), step_questions(step, options))
        a = read_step(answers, options, ms)
        chosen = self.step_gates.decide(a)
        return by_id.get(chosen), a, by_id

    async def shown(self, lines: tuple[str, ...], expects: tuple[str, ...]) -> list[float]:
        """Jev's noul for each expectation, in order."""
        answers, _ = await self._ask(expect_state(lines, expects), expect_questions(list(expects)))
        return [_noul(answers.get(f"e{i}")) for i in range(1, len(expects) + 1)]


def from_env(environ: Optional[Mapping[str, str]] = None) -> tuple[Optional[Verifier], str]:
    """(the verifier, "") on this Mac's Jev route, or (None, why not). EVE_APP_JOURNEYS=0 turns it off."""
    env = os.environ if environ is None else environ
    if (env.get("EVE_APP_JOURNEYS") or "1").strip().lower() in ("0", "false", "no", "off"):
        return None, "turned off (EVE_APP_JOURNEYS=0)"
    try:
        url, key, model = jev.route_config(env)
    except jev.JevError as e:
        return None, f"Jev is not configured: {e}"
    seconds = max(jev.timeout_from_env(env), JEV_TIMEOUT_S) if env.get("EVE_JEV_TIMEOUT") else JEV_TIMEOUT_S
    return Verifier(jev.make_predict(url, key, model, timeout_s=seconds)), ""


# ---- walking a journey on a page ---------------------------------------------------------------------------


@dataclass
class JourneyResult:
    name: str
    status: str                     # passed | step (failed at `step`) | expect (an expectation not shown)
    steps: int = 0                  # how many steps the journey has
    step: int = 0                   # the 1-based step it failed at
    reason: str = ""
    evidence: str = ""              # what was on screen when it failed
    expects: list[dict[str, Any]] = field(default_factory=list)   # {text, score, shown}
    errors: list[str] = field(default_factory=list)               # uncaught errors while it ran
    step_words: str = ""            # the step it failed at, as the owner reads it ('tap "Add expense"')
    screenshot: Optional[bytes] = None   # its last screen, when it passed (app_check: the owner's picture)
    data: Any = None                # what the app had saved at its end (the data that picture shows)

    @property
    def passed(self) -> bool:
        return self.status == "passed"

    def issue(self) -> str:
        """The problem as the repair round reads it: what was done, what went wrong, what was on screen."""
        errors = f" While it ran: {' '.join(self.errors)}" if self.errors else ""
        if self.status == "step":
            return (f'Journey "{self.name}" failed at step {self.step} of {self.steps}: {self.reason} '
                    f"On screen: {self.evidence}{errors} Fix the app if it lacks this or it does not work; fix the "
                    "journey only if the app is right and the journey names it wrongly.")
        missing = "; ".join(f'"{e["text"]}"' for e in self.expects if not e["shown"])
        return (f'Journey "{self.name}": after all {self.steps} steps the screen does not show {missing}. '
                f"The screen showed: {self.evidence}{errors} Fix the app if it does not do this; fix the journey "
                "only if the app shows the right result in other words.")

    def owner_line(self) -> str:
        """The same problem in a line the owner reads in the change chat and the Telegram caption: what could
        not be done, in which journey. No control lists, no screen dump, no instructions to Claude."""
        if self.status == "step":
            what = self.step_words or self.reason.split(":", 1)[0]
            return f'Couldn\'t {what} in "{self.name}".' if what else f'"{self.name}" did not go through.'
        missing = ", ".join(f'"{e["text"]}"' for e in self.expects if not e["shown"])
        return f'After "{self.name}", the screen didn\'t show {missing}.'

    def as_dict(self) -> dict[str, Any]:
        return {"name": self.name, "status": self.status, "steps": self.steps, "step": self.step,
                "reason": self.reason, "evidence": self.evidence, "expects": self.expects, "errors": self.errors}


def _evidence(screen: Screen, limit: int = 400) -> str:
    text = " | ".join(screen.lines)
    return f'"{text[:limit]}{"…" if len(text) > limit else ""}"' if text else "(a blank screen)"


def _norm(s: str) -> str:
    return " ".join(re.sub(r"[^\w$€£₹.,:%-]+", " ", s.casefold()).split())


def _option_for(control: Control, value: str) -> Optional[str]:
    """The select option a choose step means: the same text, else the one option that holds it."""
    want = _norm(value)
    exact = [o for o in control.options if _norm(o) == want]
    if exact:
        return exact[0]
    near = [o for o in control.options if want and want in _norm(o)]
    return near[0] if len(near) == 1 else None


class Hung(RuntimeError):
    """The page did not answer a read within EVAL_TIMEOUT_S: the app is stuck (an endless loop). The journey
    fails at that step in seconds, instead of every journey waiting out JOURNEY_TIMEOUT_S."""


async def _bounded(awaitable: Any, extra_s: float = 0.0) -> Any:
    try:
        return await asyncio.wait_for(awaitable, EVAL_TIMEOUT_S + extra_s)
    except asyncio.TimeoutError:
        raise Hung(f"the page did not answer for {EVAL_TIMEOUT_S + extra_s:.0f} s") from None


async def read_screen(page: Any) -> Screen:
    return screen_from(await _bounded(page.evaluate(COLLECT_JS)))


def refind(control: Control, before: Screen, now: Screen) -> Optional[Control]:
    """``control`` (read on ``before``) on the screen as it is ``now``: the same kind and label, and the same
    place among the controls that share them ("Delete" in the second row). An app that re-renders its view (a
    clock, a running timer, a "2 min ago" label) replaces the node Jev chose while Jev was answering."""
    same = lambda c: c.kind == control.kind and c.label == control.label      # noqa: E731
    try:
        nth = [c for c in before.controls if same(c)].index(control)
    except ValueError:
        nth = 0
    now_same = [c for c in now.controls if same(c)]
    if nth < len(now_same):
        return now_same[nth]
    return now_same[0] if len(now_same) == 1 else None


async def _do(loc: Any, step: Step, control: Control) -> None:
    if step.do == "type":
        await loc.fill(step.value, timeout=LOCATE_MS)
        await loc.evaluate("(el) => el.blur()", timeout=LOCATE_MS)
    elif step.do == "choose" and control.kind == "select":
        await loc.select_option(label=_option_for(control, step.value), timeout=LOCATE_MS)
    else:
        await loc.scroll_into_view_if_needed(timeout=LOCATE_MS)
        await loc.click(timeout=LOCATE_MS)


async def act(page: Any, step: Step, control: Control, screen: Optional[Screen] = None) -> str:
    """Do the step on the control: "" when done, else why it could not be done. With the ``screen`` the
    control was chosen on, a control that is gone from the page (re-rendered meanwhile) is read again and
    used once more, without asking Jev again: only a control still missing then is the app's problem."""
    if control.kind in ("main", "back"):
        name = "MainButton" if control.kind == "main" else "BackButton"
        return "" if await _bounded(page.evaluate(PRESS_JS, name)) else f"Telegram's {name} could not be pressed"
    if step.do == "choose" and control.kind == "select" and _option_for(control, step.value) is None:
        return f'"{control.label}" has no option "{step.value}" (it has: {", ".join(control.options[:8])}).'
    retried = False
    while True:
        loc = page.locator(f"[data-journey-id='{control.id}']").first
        try:
            if await _bounded(loc.count()) == 0:
                raise LookupError("it is no longer on the page")
            # a locator's timeout bounds finding the control, not the page running the event it sends (the
            # blur after typing runs the app's change handler)
            await _bounded(_do(loc, step, control), 2 * LOCATE_MS / 1000)
            return ""
        except Hung:
            raise
        except Exception as e:  # noqa: BLE001 — covered, detached, not editable: the step did not happen
            if screen is not None and not retried and await _bounded(loc.count()) == 0:
                retried = True
                again = refind(control, screen, await read_screen(page))
                if again is not None:
                    control = again
                    continue
                return f'"{control.label}" went away when the screen changed, before it could be used.'
            return f'"{control.label}" could not be used ({type(e).__name__}: {str(e).splitlines()[0][:100]}).'


def _listed(by_id: Mapping[str, Control]) -> str:
    return ", ".join(f'"{c.label}"' for c in list(by_id.values())[:14]) or "none"


def _not_found(step: Step, target: str, answer: StepAnswer, by_id: Mapping[str, Control], gates: StepGates) -> str:
    kinds = "fields" if step.do == "type" else "controls"
    return (f'could not find "{target}" to {step.do} (the {kinds} on screen: {_listed(by_id)}).'
            if answer.target == STEP_NONE or answer.present < gates.present else
            f'could not tell which control is "{target}" (the {kinds} on screen: {_listed(by_id)}).')


async def walk(page: Any, journey: Journey, verifier: Verifier, *, errors: Optional[list[str]] = None,
               on_step: Callable[[str], None] = lambda text: None) -> JourneyResult:
    """One journey on an app just opened with no data. ``errors`` is the list the page's uncaught errors land
    in (app_check's session), ``on_step`` names what is being done, so an error says during which step. Raises
    JevDown when Jev stops answering.

    A "choose" on anything but a <select> (a custom dropdown, chips, a radio group) is two picks: the control
    labelled with the target is opened, then the option labelled with the value is tapped on the screen that
    follows. A radio group has no control of its own (its label is a legend), so then the value is picked
    straight away."""
    res = JourneyResult(journey.name, "passed", steps=len(journey.steps))
    seen_errors = errors if errors is not None else []
    gates = verifier.step_gates
    n, judging = 0, False
    try:
        for n, step in enumerate(journey.steps, 1):
            on_step(f'the journey "{journey.name}", step {n} ({step.words()})')
            res.step_words = step.words()
            screen = await read_screen(page)
            control, answer, by_id = await verifier.pick(step, screen)
            doing = step
            if step.do == "choose" and (control is None or control.kind != "select"):
                opened = control
                if opened is not None:
                    failed = await act(page, Step("tap", opened.label), opened, screen)
                    if failed:
                        res.status, res.step, res.reason = "step", n, f"{step.words()}: {failed}"
                        res.evidence = _evidence(await read_screen(page))
                        break
                    await page.wait_for_timeout(STEP_WAIT_MS)
                    screen = await read_screen(page)
                doing = Step("tap", step.value)
                control, value_answer, value_ids = await verifier.pick(doing, screen)
                if control is None:
                    why = (_not_found(step, step.target, answer, by_id, gates) if opened is None else
                           f'opened "{opened.label}" but ' + _not_found(doing, step.value, value_answer, value_ids,
                                                                          gates).replace(" to tap ", " to pick "))
                    res.status, res.step, res.reason = "step", n, f"{step.words()}: {why}"
                    res.evidence = _evidence(screen)
                    break
            elif control is None:
                res.status, res.step = "step", n
                res.reason = f"{step.words()}: {_not_found(step, step.target, answer, by_id, gates)}"
                res.evidence = _evidence(screen)
                break
            failed = await act(page, doing, control, screen)
            await page.wait_for_timeout(STEP_WAIT_MS)
            if failed:
                screen = await read_screen(page)
                res.status, res.step, res.reason = "step", n, f"{step.words()}: {failed}"
                res.evidence = _evidence(screen)
                break
        if res.status == "passed":
            judging = True
            screen = screen_from({"text": await _bounded(page.evaluate(TEXT_JS))})
            scores = await verifier.shown(screen.lines, journey.expect)
            res.expects = [{"text": e, "score": round(s, 3), "shown": s >= verifier.expect_shown
                            and numbers_shown(e, screen.lines) and words_shown(e, screen.lines)}
                           for e, s in zip(journey.expect, scores, strict=True)]
            if not all(e["shown"] for e in res.expects):
                # the evidence is the screen as a person saw it, not Jev's shortlist: "No expenses yet" shares no
                # word with "Alex owes Sam $15.00" and is exactly what the repair round needs to read
                res.status, res.evidence = "expect", _evidence(screen)
    except Hung as e:
        after = "after its last step" if judging else f"at step {n}"
        res.status, res.step, res.expects = "step", max(n, 1), []
        res.reason = (f"{res.step_words or 'opening it'}: the app stopped responding {after} ({e}: an endless "
                      "loop, or work that never yields).")
        res.evidence = "(the page stopped answering)"
    res.errors = [e for e in seen_errors if e.startswith(("Uncaught error", "Console error"))][:3]
    return res


@dataclass
class JourneyRun:
    """Every journey of one check: the results of the ones that finished, why the rest did not run (if any),
    and what Jev cost."""

    results: list[JourneyResult] = field(default_factory=list)
    total: int = 0
    not_run: str = ""
    jev: dict[str, Any] = field(default_factory=dict)
    secs: float = 0.0

    @property
    def passed(self) -> int:
        return sum(1 for r in self.results if r.passed)

    def summary(self) -> str:
        if not self.total:
            return ""
        if self.not_run and not self.results:
            return f"journeys not run ({self.not_run})"
        if self.not_run:
            return f"journeys {self.passed}/{self.total} ({self.total - len(self.results)} not run: {self.not_run})"
        return f"journeys {self.passed}/{self.total}"

    def issues(self) -> list[str]:
        """A journey that finished and failed is a problem even when others could not run: Jev going down
        later never hides breakage it already saw."""
        return [r.issue() for r in self.results if not r.passed]

    def owner_lines(self) -> dict[str, str]:
        """Each issue() by the short line the owner reads instead (build_record, the Telegram caption)."""
        return {r.issue(): r.owner_line() for r in self.results if not r.passed}

    def as_dict(self) -> dict[str, Any]:
        return {"total": self.total, "passed": self.passed, "ran": len(self.results), "not_run": self.not_run,
                "secs": round(self.secs, 1), "jev": self.jev, "results": [r.as_dict() for r in self.results]}
