//! Jev uses the app the way it is meant to be used, and says whether it did what it is for.
//!
//! Owner, 2026-09-24: "Can u use jev as the verification layer instead of claude", after an expense splitter passed
//! the phone check and still "doesn't work". The scripted check (app_check) fills every field and taps every
//! button blindly: it finds crashes, a page that never saves, text that does not read in the dark. It cannot tell
//! whether adding an expense shows who owes whom.
//!
//! So the division of labour is: Claude (the maker) writes and repairs the code, and with the page it writes 2-4
//! JOURNEYS, the app's core purpose as a person would walk it: steps ("tap 'Add expense'", "type '30' into
//! 'Amount'", "choose 'Sam' in 'Paid by'") and the literal text that must be on screen afterwards. Jev (TypeSafe's
//! hosted typed-decision model, eve-jev) is the verifier: it does each step on the real page and judges each
//! expectation. Code decides from Jev's numbers; a journey that does not go through becomes a problem in the same
//! CheckReport, so the repair loop sends it back to Claude like any other.
//!
//! HOW JEV IS ASKED (eve-typed-ask's protocol guide, "ask each model natively"). Jev is a literal reader, weak at
//! counting and indirection, and loses accuracy in a big state. So:
//!   - one step is ONE request: a `choice` over the controls on screen (only the kinds the step can act on: a
//!     tap never picks a field) plus an explicit "none", and beside it an absolute `noul`, "is the control the step
//!     names in the list at all". The choice alone never acts: a relative pick always names something.
//!   - the controls are in the state too, labels only, because a noul is answered against the state (typed_ask:
//!     with the list only in the choice, `present` could not see it). The state is the step, a short screen
//!     summary and the rendered controls ("Amount (number field, e.g. 24.50)"). Nothing else.
//!   - after the steps, ONE request with one `noul` per expectation over the screen's text lines, trimmed.
//!     Expectations are literal text by contract (the maker is told why), never counts or comparisons.
//!   - the cut-offs are fitted on real apps and scored on a held-out half (JOURNEY_STEP_GATES, EXPECT_SHOWN
//!     below, with how they were measured), never carried over from the click path's.
//!
//! WHAT LEAVES THE MAC: each journey starts from an EMPTY app (no saved data), so what Jev sees is the app's own
//! labels and screen text plus the journey's example values. An edit's prompt shows Claude a sample of the owner's
//! data, which a journey could copy: apps_maker.without_owner_data keeps any journey holding a word of it (one the
//! app's page does not hold) from being walked, and sends it back as a problem. Jev off (no key, a timeout, the
//! network) never blocks a build: the journeys are reported "not run (why)" and the check stands.
//!
//! The page is behind the [`Page`] trait: Python drives Playwright, the Rust daemon drives Chromium over CDP
//! (app_check's port implements the trait). The page scripts below are the same bytes either way.

/// Python `str` character classes from CPython 3.13's tables (public for apps_maker's owner-data words).
pub mod py;
mod pychars;

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use eve_typed_ask::{StepAnswer, StepGates};
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};

pub const KINDS: [&str; 3] = ["tap", "type", "choose"];
pub const MIN_JOURNEYS: usize = 1;
pub const MAX_JOURNEYS: usize = 4;
pub const MAX_STEPS: usize = 12;
pub const MAX_EXPECT: usize = 5;
/// options per step: Jev's step eval ran 26 at 234 ms p50; a busy screen has ~30
pub const MAX_CONTROLS: usize = 48;
/// screen text lines per expectation request: a month calendar is ~60 lines of single numbers, and at 60 the
/// result under it was cut (calibration, 2026-09-24)
pub const MAX_LINES: usize = 150;
/// what is read off the page; Jev gets a shortlist of it (relevant_lines)
pub const MAX_TEXT_CHARS: usize = 5000;
/// the lines one expectation request carries
pub const MAX_EXPECT_LINES: usize = 30;
pub const MAX_LINE_CHARS: usize = 120;
pub const SUMMARY_CHARS: usize = 300;
/// after an action: a render, a toast, a view change
pub const STEP_WAIT_MS: u64 = 350;
/// every journey of one check, run side by side
pub const JOURNEY_TIMEOUT_S: f64 = 90.0;
/// a verification call: not the click path's 3 s, and never a build blocker
pub const JEV_TIMEOUT_S: f64 = 6.0;
/// one read of the page: an app caught in an endless loop never answers it
pub const EVAL_TIMEOUT_S: f64 = 5.0;
/// how long an action waits for its control to be usable
pub const LOCATE_MS: u64 = 1500;
pub const STEP_NONE: &str = "none";

/// What each kind of step may act on. Code decides the kind (a tap never types), Jev decides which one.
pub const ACTS_ON: [(&str, &[&str]); 3] = [
    ("tap", &["button", "tab", "checkbox", "radio", "link", "item", "main", "back"]),
    ("type", &["field"]),
    ("choose", &["select", "radio", "tab", "button", "checkbox", "item"]),
];

/// `ACTS_ON[do]`: the control kinds a step of this kind may act on (none for an unknown kind).
pub fn acts_on(step_do: &str) -> &'static [&'static str] {
    ACTS_ON.iter().find(|(k, _)| *k == step_do).map(|(_, v)| *v).unwrap_or(&[])
}

// ---- the journeys contract ------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Step {
    /// tap | type | choose
    pub do_: String,
    /// the visible label of the control
    pub target: String,
    /// the text to type, or the option to pick
    pub value: String,
}

impl Step {
    pub fn new(do_: impl Into<String>, target: impl Into<String>, value: impl Into<String>) -> Self {
        Self { do_: do_.into(), target: target.into(), value: value.into() }
    }

    /// `Step(do, target)`: a step with no value (a tap).
    pub fn tap(target: impl Into<String>) -> Self {
        Self::new("tap", target, "")
    }

    /// The step as Jev reads it, and as a person reads a failure.
    pub fn words(&self) -> String {
        match self.do_.as_str() {
            "type" => format!("type \"{}\" into \"{}\"", self.value, self.target),
            "choose" => format!("choose \"{}\" in \"{}\"", self.value, self.target),
            _ => format!("tap \"{}\"", self.target),
        }
    }

    pub fn as_dict(&self) -> Value {
        let mut d = Map::new();
        d.insert("do".into(), Value::String(self.do_.clone()));
        d.insert("target".into(), Value::String(self.target.clone()));
        if self.do_ != "tap" {
            d.insert("value".into(), Value::String(self.value.clone()));
        }
        Value::Object(d)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Journey {
    pub name: String,
    pub steps: Vec<Step>,
    pub expect: Vec<String>,
}

impl Journey {
    pub fn as_dict(&self) -> Value {
        json!({"name": self.name, "steps": self.steps.iter().map(Step::as_dict).collect::<Vec<_>>(),
               "expect": self.expect})
    }
}

/// `_text`: a str, int or float (never a bool) as one line of at most `limit` characters; anything else "".
fn text_of(v: Option<&Value>, limit: usize) -> String {
    match v {
        Some(Value::String(s)) => py::take(&eve_typed_ask::fold_spaces(s), limit),
        Some(n @ Value::Number(_)) => py::take(&eve_typed_ask::fold_spaces(&eve_decider::py_repr(n)), limit),
        _ => String::new(),
    }
}

/// The journeys in `value` (the parsed ```journeys block), and what was wrong with the ones dropped. A
/// journey with a step that cannot be done as written, or with nothing to expect, is dropped whole: half a
/// journey would verify something the maker did not mean.
pub fn parse_journeys(value: &Value) -> (Vec<Journey>, Vec<String>) {
    let Value::Array(items) = value else {
        return (Vec::new(), vec!["The journeys block is not a JSON list.".into()]);
    };
    let mut out: Vec<Journey> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    for (i, j) in items.iter().enumerate() {
        let n = i + 1;
        if out.len() >= MAX_JOURNEYS {
            problems.push(format!("Only {MAX_JOURNEYS} journeys are run; journey {n} and later were left out."));
            break;
        }
        let Value::Object(j) = j else {
            problems.push(format!("Journey {n} is not an object."));
            continue;
        };
        let name = Some(text_of(j.get("name"), 80)).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("Journey {n}"));
        let raw_steps: &[Value] = match j.get("steps") {
            Some(Value::Array(a)) => a,
            _ => &[],
        };
        let mut steps: Vec<Step> = Vec::new();
        let mut bad = String::new();
        for (k, s) in raw_steps.iter().take(MAX_STEPS).enumerate() {
            let k = k + 1;
            let (do_, target, val) = match s {
                Value::Object(s) => (
                    text_of(s.get("do"), 10).to_lowercase(),
                    text_of(s.get("target"), 80),
                    text_of(s.get("value"), 120),
                ),
                _ => (String::new(), String::new(), String::new()),
            };
            if !KINDS.contains(&do_.as_str()) || target.is_empty() || (do_ != "tap" && val.is_empty()) {
                bad = format!(
                    "Journey \"{name}\" step {k} is not a tap, type or choose with a target{}.",
                    if do_ == "type" || do_ == "choose" { " and a value" } else { "" }
                );
                break;
            }
            let value = if do_ != "tap" { val } else { String::new() };
            steps.push(Step::new(do_, target, value));
        }
        let raw_expect: &[Value] = match j.get("expect") {
            Some(Value::Array(a)) => a,
            _ => &[],
        };
        let expect: Vec<String> =
            raw_expect.iter().map(|x| text_of(Some(x), 160)).filter(|e| !e.is_empty()).take(MAX_EXPECT).collect();
        if !bad.is_empty() {
            problems.push(bad);
        } else if steps.is_empty() {
            problems.push(format!("Journey \"{name}\" has no steps."));
        } else if expect.is_empty() {
            problems.push(format!("Journey \"{name}\" expects nothing on screen afterwards."));
        } else {
            out.push(Journey { name, steps, expect });
        }
    }
    if out.len() < MIN_JOURNEYS {
        // an empty block would switch the verifier off for this app for good: the build keeps the journeys it
        // had, and the round is told
        problems.push("The journeys block holds no journey that can be walked; write 2 to 4.".into());
    }
    (out, problems)
}

static BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)```journeys[ \t]*\n(.*?)```").expect("block regex"));

/// The ```journeys block of a maker answer: (journeys, problems). (None, []) when the answer has no block, so
/// the journeys already in hand stay; ([], problems) when it has one that does not parse or holds none that can
/// be walked (the journeys in hand stay then too, and the problem goes to the repair round).
pub fn extract_journeys(text: &str) -> (Option<Vec<Journey>>, Vec<String>) {
    let Some(last) = BLOCK.captures_iter(text).last() else {
        return (None, Vec::new());
    };
    let body = last.get(1).map(|m| m.as_str()).unwrap_or("");
    match eve_ipc::pyjson::loads(body) {
        Ok(value) => {
            let (found, problems) = parse_journeys(&value);
            (Some(found), problems)
        }
        // RecursionError is not a ValueError in Python and would raise; both read as a block that does not parse
        Err(e) => (Some(Vec::new()), vec![format!("The journeys block is not valid JSON ({}).", py::take(&e.to_string(), 80))]),
    }
}

pub fn journeys_json(journeys: &[Journey]) -> Value {
    Value::Array(journeys.iter().map(Journey::as_dict).collect())
}

// ---- what one step sees ----------------------------------------------------------------------------------

/// Every labelled control on screen, in document order, tagged data-journey-id for the action. A field's label
/// is what a person reads next to it, its placeholder the example apart. A list row that is tapped (the cursor
/// says so) and holds no control of its own is an "item". Telegram's MainButton and BackButton are controls too:
/// the app's primary action often lives there.
pub const COLLECT_JS: &str = r##"
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
"##;

/// The text a person can read on the page now, line by line. innerText leaves out what is display:none. Telegram
/// draws the MainButton under the page, so its text is on the screen too ("Clear 1 checked" is an app's result).
pub const TEXT_JS: &str = r##"
() => { const w = window.Telegram && window.Telegram.WebApp, b = w && w.MainButton;
  return ((document.body && document.body.innerText) || "") + (b && b.isVisible && b.text ? "\n" + b.text : ""); }
"##;

pub const PRESS_JS: &str = r##"
(name) => { const b = window.Telegram && window.Telegram.WebApp && window.Telegram.WebApp[name];
  if (!b || !b.isVisible || b.isActive === false || !b._cbs || !b._cbs.length) return false;
  b._cbs.slice().forEach((f) => f()); return true; }
"##;

/// The blur `type` sends after filling a field, so the app's change handler runs.
pub const BLUR_JS: &str = "(el) => el.blur()";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Control {
    /// data-journey-id, or "main" / "back" for Telegram's own buttons
    pub id: String,
    pub kind: String,
    pub label: String,
    pub type_: String,
    pub placeholder: String,
    pub value: String,
    pub options: Vec<String>,
    pub on: bool,
}

impl Control {
    /// `Control(id, kind, label)` with the defaults for the rest.
    pub fn new(id: impl Into<String>, kind: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), kind: kind.into(), label: label.into(), ..Self::default() }
    }

    /// The option text Jev reads: the label, then what kind of thing it is, in a few words.
    pub fn render(&self) -> String {
        match self.kind.as_str() {
            "main" => return format!("{} (Telegram's main button at the bottom of the screen)", self.label),
            "back" => return "Back (Telegram's back arrow)".into(),
            "field" => {
                let kind = match self.type_.as_str() {
                    "number" => "number field",
                    "date" => "date field",
                    "time" => "time field",
                    "textarea" => "text box",
                    "search" => "search field",
                    "email" => "email field",
                    _ => "text field",
                };
                let extra = if !self.placeholder.is_empty() && self.placeholder != self.label {
                    let p = self.placeholder.strip_prefix("e.g.").unwrap_or(&self.placeholder);
                    format!(", e.g. {}", py::strip(p))
                } else {
                    String::new()
                };
                return format!("{} ({kind}{extra})", self.label);
            }
            "select" => {
                let shown: Vec<&str> = self.options.iter().take(8).map(String::as_str).collect();
                let more = if self.options.len() > 8 { "…" } else { "" };
                return format!("{} (dropdown: {}{more})", self.label, shown.join(", "));
            }
            _ => {}
        }
        let word = match self.kind.as_str() {
            "item" => "list item",
            "radio" => "option",
            "tab" => "tab",
            "checkbox" => "checkbox",
            "link" => "link",
            _ => "button",
        };
        format!("{} ({word}{})", self.label, if self.on { ", selected" } else { "" })
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Screen {
    pub controls: Vec<Control>,
    pub heading: String,
    pub lines: Vec<String>,
}

impl Screen {
    /// A few words of what is on screen: the heading, then the first lines, short.
    pub fn summary(&self) -> String {
        let parts: Vec<&str> = std::iter::once(self.heading.as_str())
            .chain(self.lines.iter().take(8).map(String::as_str))
            .filter(|x| !x.is_empty())
            .collect();
        py::take(&parts.join(" | "), SUMMARY_CHARS)
    }
}

/// The page text as short, distinct, non-empty lines, in order: the state an expectation is judged on.
pub fn screen_lines(text: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let mut size = 0usize;
    for raw in py::splitlines(text) {
        let line = py::take(&eve_typed_ask::fold_spaces(raw), MAX_LINE_CHARS);
        if !line.is_empty() && !seen.contains(&line) {
            seen.insert(line.clone());
            size += py::len(&line) + 1;
            out.push(line);
        }
        if out.len() >= MAX_LINES || size >= MAX_TEXT_CHARS {
            break;
        }
    }
    out
}

const STOP: [&str; 21] = [
    "a", "an", "the", "of", "to", "in", "on", "for", "and", "or", "at", "is", "by", "with", "your", "you", "it", "this",
    "that", "no", "not",
];

/// `re.findall(r"[^\W\d_]+|\d+", text)`: runs of letters, or runs of decimal digits.
fn letter_or_digit_runs(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let take_while = |pred: fn(char) -> bool, from: usize| {
            let mut j = from;
            while j < chars.len() && pred(chars[j]) {
                j += 1;
            }
            j
        };
        if py::is_letter(c) {
            let j = take_while(py::is_letter, i);
            out.push(chars[i..j].iter().collect());
            i = j;
        } else if py::is_decimal(c) {
            let j = take_while(py::is_decimal, i);
            out.push(chars[i..j].iter().collect());
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// `_words`: the words and numbers of `text` that say something (no stop words, no single letters).
fn words_of(text: &str) -> HashSet<String> {
    letter_or_digit_runs(&py::casefold(text))
        .into_iter()
        .filter(|w| {
            !STOP.contains(&w.as_str()) && (w.chars().count() > 1 || w.chars().all(py::is_digit))
        })
        .collect()
}

/// The screen lines worth judging `expects` against: each line that shares a word or a number with one of
/// them, with the line before and after it (a result split over two lines), in screen order. Jev suffers
/// context rot: on the whole screen (a month calendar is 60 lines of single numbers) it read 0.28 for
/// "average mood · Good" that was plainly there, and 0.12 for a MainButton text at the end (calibration,
/// 2026-09-24). A near miss stays in: "$15.00" is shortlisted for an expected "$25.00" by its words, and Jev
/// must say no to it. With nothing in common, the first lines go, so the answer is an informed no.
pub fn relevant_lines<L: AsRef<str>, E: AsRef<str>>(lines: &[L], expects: &[E], limit: usize) -> Vec<String> {
    let want: HashSet<String> = expects.iter().flat_map(|e| words_of(e.as_ref())).collect();
    let score: Vec<usize> = lines.iter().map(|l| words_of(l.as_ref()).intersection(&want).count()).collect();
    let mut hits: Vec<usize> = (0..lines.len()).filter(|&i| score[i] > 0).collect();
    hits.sort_by_key(|&i| (std::cmp::Reverse(score[i]), i));
    let mut keep: HashSet<usize> = HashSet::new();
    for i in hits {
        let near: Vec<usize> = [i.checked_sub(1), Some(i), Some(i + 1)]
            .into_iter()
            .flatten()
            .filter(|&j| j < lines.len())
            .collect();
        let grown = keep.len() + near.iter().filter(|j| !keep.contains(j)).count();
        if grown > limit {
            break;
        }
        keep.extend(near);
    }
    if keep.is_empty() {
        return lines.iter().take(limit).map(|l| l.as_ref().to_string()).collect();
    }
    let mut kept: Vec<usize> = keep.into_iter().collect();
    kept.sort_unstable();
    kept.into_iter().map(|i| lines[i].as_ref().to_string()).collect()
}

/// `\d(?:[\d,.]*\d)?` and `[^\W\d_]+` (when `letters`), in order, as `re.findall` returns them.
fn number_and_word_matches(text: &str, letters: bool) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if py::is_decimal(c) {
            // greedy over [\d,.]*, then back to the last digit
            let mut j = i + 1;
            let mut last = i;
            while j < chars.len() && (py::is_decimal(chars[j]) || chars[j] == ',' || chars[j] == '.') {
                if py::is_decimal(chars[j]) {
                    last = j;
                }
                j += 1;
            }
            out.push(chars[i..=last].iter().collect());
            i = last + 1;
        } else if letters && py::is_letter(c) {
            let mut j = i + 1;
            while j < chars.len() && py::is_letter(chars[j]) {
                j += 1;
            }
            out.push(chars[i..j].iter().collect());
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// A number written one way: no thousands commas, no trailing decimal zeros, no leading zeros.
fn one_way(raw: &str) -> String {
    let n = raw.replace(',', "");
    let n = if n.contains('.') { n.trim_end_matches('0').trim_end_matches('.').to_string() } else { n };
    let n = n.trim_start_matches('0');
    if n.is_empty() {
        "0".into()
    } else {
        n.to_string()
    }
}

/// The numbers in `text`, written one way: no thousands commas, no trailing decimal zeros, no leading zeros
/// ("$1,234.50" -> 1234.5, "30.00" -> 30, "09" -> 9).
fn numbers_of(text: &str) -> HashSet<String> {
    number_and_word_matches(text, false).iter().map(|raw| one_way(raw)).collect()
}

/// Code's veto on a "shown": every number the expectation holds is a number on the screen. Jev is weak at
/// counting (typed_ask's guide), and read 0.90 for "1 book" on a screen with no 1 on it (calibration,
/// 2026-09-24); this cannot make a missing result pass, only stop a number Jev did not really see.
pub fn numbers_shown<L: AsRef<str>>(expect: &str, lines: &[L]) -> bool {
    let screen: Vec<&str> = lines.iter().map(AsRef::as_ref).collect();
    numbers_of(expect).is_subset(&numbers_of(&screen.join(" ")))
}

/// `text` as words and numbers, in order: case, punctuation, emoji and line breaks dropped, numbers
/// written one way (as numbers_of does).
fn tokens(text: &str) -> Vec<String> {
    number_and_word_matches(&py::casefold(text), true)
        .into_iter()
        .map(|w| if w.chars().next().is_some_and(py::is_digit) { one_way(&w) } else { w })
        .collect()
}

/// Code's second veto: the expectation's words and numbers stand on the screen in its order, next to each
/// other (line breaks, case, punctuation and number format aside). Jev reads a bag of words too kindly when
/// they stay and the meaning goes: a live build passed "1-day streak" over "Streak ended · best 1 day" at
/// 0.56 to 0.62 across ten asks, and "Sam owes Alex ended · best $16.20" read up to 0.89 for "Sam owes Alex
/// $16.20" (calibration, 2026-09-24). Expectations are literal text by contract, so this never fails one the
/// screen really shows.
pub fn words_shown<L: AsRef<str>>(expect: &str, lines: &[L]) -> bool {
    let want = tokens(expect);
    if want.is_empty() {
        return true;
    }
    let screen: Vec<&str> = lines.iter().map(AsRef::as_ref).collect();
    let have = tokens(&screen.join("\n"));
    have.windows(want.len()).any(|w| w == want.as_slice())
}

/// Python's truthiness of a decoded JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// `str(v)` of a decoded JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => eve_decider::py_repr(other),
    }
}

/// `str(m.get(key) or default)`.
fn str_or(m: &Map<String, Value>, key: &str, default: &str) -> String {
    match m.get(key) {
        Some(v) if truthy(v) => py_str(v),
        _ => default.to_string(),
    }
}

/// `tuple(str(o) for o in v or ())`: a list's items, a string's characters, an object's keys.
fn str_items(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().map(py_str).collect(),
        Some(Value::String(s)) => s.chars().map(String::from).collect(),
        Some(Value::Object(m)) => m.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// The screen as `COLLECT_JS` (or `{"text": TEXT_JS}`) read it.
pub fn screen_from(raw: &Value) -> Screen {
    let empty = Map::new();
    let raw = raw.as_object().unwrap_or(&empty);
    let mut controls: Vec<Control> = match raw.get("controls") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_object)
            .map(|c| Control {
                id: py_str(c.get("id").unwrap_or(&Value::Null)),
                kind: str_or(c, "kind", "button"),
                label: str_or(c, "label", ""),
                type_: str_or(c, "type", ""),
                placeholder: str_or(c, "placeholder", ""),
                value: str_or(c, "value", ""),
                options: str_items(c.get("options")),
                on: c.get("on").is_some_and(truthy),
            })
            .collect(),
        _ => Vec::new(),
    };
    if let Some(main) = raw.get("main").filter(|v| truthy(v)) {
        controls.push(Control::new("main", "main", py_str(main)));
    }
    if raw.get("back").is_some_and(truthy) {
        controls.push(Control::new("back", "back", "Back"));
    }
    Screen { controls, heading: str_or(raw, "heading", ""), lines: screen_lines(&str_or(raw, "text", "")) }
}

// ---- asking Jev ------------------------------------------------------------------------------------------

pub const STEP_ABOUT: &str = concat!(
    "One step of a test of a small phone app. The controls are the labelled buttons, fields, tabs and ",
    "list items on the app's screen now, each with what kind of control it is."
);
pub const EXPECT_ABOUT: &str = "The text on a small phone app's screen, line by line, after a test used the app.";

/// What does not make a label another label. A list row's label is its whole text, so a row named "Weekly
/// groceries" reads "Weekly groceries Thu, Sep 24 · $12.50" (calibration: 15 of the 340 real steps named a row or a
/// button by the words before such details).
pub const SAME_LABEL: &str = concat!(
    "A leading icon, emoji or \"+\", letter case, and details after the name (a count, a date, an amount or ",
    "a status, as a list row shows) do not matter."
);

/// The one request for one step. The label is IN the instructions: Jev is literal, and "the step's label"
/// is one indirection too many (the single right field of a form read 0.61 that way).
pub fn step_questions(step: &Step, options: &IndexMap<String, String>) -> Value {
    let mut criteria = Map::new();
    for (k, v) in options {
        criteria.insert(k.clone(), Value::String(v.clone()));
    }
    criteria.insert(STEP_NONE.into(), Value::String(format!("no control here is labelled \"{}\"", step.target)));
    json!({
        "target": {"type": "choice", "instructions": format!(
            "Which control is labelled \"{}\"? {SAME_LABEL} A control that only shares a word with it, or \
             does something related, is not it.", step.target), "criteria": criteria},
        "present": {"type": "noul", "instructions": format!(
            "Is a control labelled \"{}\" in the list of controls? {SAME_LABEL} Answer no when no control \
             carries this label, even when a similar or related control is there.", step.target)},
    })
}

pub fn expect_questions<E: AsRef<str>>(expects: &[E]) -> Value {
    let mut out = Map::new();
    for (i, e) in expects.iter().enumerate() {
        out.insert(
            format!("e{}", i + 1),
            json!({"type": "noul", "instructions": format!(
                "Does the screen text show: {} ? Answer yes when this text is on the screen, allowing only a different \
                 letter case, spacing, line break or number format of the same amount. Answer no when a name, number or \
                 amount differs, or when it is not there.", e.as_ref())}),
        );
    }
    Value::Object(out)
}

pub fn expect_state<L: AsRef<str>, E: AsRef<str>>(lines: &[L], expects: &[E]) -> Value {
    json!({"about": EXPECT_ABOUT, "screen": relevant_lines(lines, expects, MAX_EXPECT_LINES)})
}

pub fn step_state(step: &Step, screen: &Screen, options: &IndexMap<String, String>) -> Value {
    json!({"about": STEP_ABOUT, "screen": screen.summary(), "step": step.words(),
           "controls": options.values().collect::<Vec<_>>()})
}

/// The controls a step of this kind can act on, by id, at most MAX_CONTROLS, Telegram's buttons kept.
pub fn offered(step: &Step, screen: &Screen) -> IndexMap<String, Control> {
    let kinds = acts_on(&step.do_);
    let mut fits: Vec<&Control> = screen.controls.iter().filter(|c| kinds.contains(&c.kind.as_str())).collect();
    if fits.len() > MAX_CONTROLS {
        let is_tg = |c: &&Control| c.kind == "main" || c.kind == "back";
        let tg: Vec<&Control> = fits.iter().copied().filter(is_tg).collect();
        let mut rest: Vec<&Control> =
            fits.iter().copied().filter(|c| !is_tg(c)).take(MAX_CONTROLS.saturating_sub(tg.len())).collect();
        rest.extend(tg);
        fits = rest;
    }
    fits.into_iter().enumerate().map(|(n, c)| (format!("c{}", n + 1), c.clone())).collect()
}

/// A step answer as typed_ask's StepAnswer, so its gates and its fitter are the click path's code (the cut-offs
/// are this question's own). `already_done` and `risky` are not asked here: 0.
pub fn read_step(answers: &Map<String, Value>, options: &IndexMap<String, String>, ms: f64) -> StepAnswer {
    let (key, p, probs) = eve_typed_ask::choice(answers.get("target"));
    let k = options.len() + 1;
    if key != STEP_NONE && !options.contains_key(&key) {
        return StepAnswer {
            ms,
            k,
            error: format!("target {} is not an option", eve_typed_ask::py_repr_str(&key)),
            ..StepAnswer::new("", 0.0, 0.0, 0.0, 0.0, 0.0)
        };
    }
    let runner_up = probs.iter().filter(|(name, _)| **name != key).map(|(_, v)| *v).fold(None, |m: Option<f64>, v| {
        Some(match m {
            Some(m) if m >= v => m,
            _ => v,
        })
    });
    let margin = (p - runner_up.unwrap_or(0.0)).max(0.0);
    StepAnswer {
        ms,
        k,
        ..StepAnswer::new(&key, p, margin, eve_typed_ask::noul(answers.get("present")), 0.0, 0.0)
    }
}

/// Fitted 2026-09-24 by tools/journey_eval.py on the 17 apps built overnight (11 kinds), with journeys the maker's
/// own prompt wrote for them (Claude, $2.25; 63 journeys), walked in app_check's sandbox with a code oracle acting
/// and every step the oracle could not place read by hand (15 of 349: all rows or buttons named by the words before
/// their details). Split by KIND: fitted on 6 (baby log, grocery list, monthly budget, pomodoro, trip splitter,
/// workout log), zero wrong acts allowed (typed_ask.fit_step_gates); held out 5 (flashcards, habit tracker, mood
/// journal, reading list, water tracker). Held-out steps, 367: 127 real, 120 with the label swapped for another
/// app's, 120 with the right control removed from the screen:
///   under these gates    acted 127, right 124, wrong 3   precision 97.6%  recall 97.6%  abstained on 237 of 240
///   the choice alone     acted 138, right 127, wrong 11  precision 92.0%  recall 100%
/// The 3 wrong: a habit's row removed, and its check toggle "Drink water: done today" taken for "Drink water".
/// 1,388 requests over OpenRouter: 244 ms p50, 357 ms p90, 684 input tokens on average ($0.00003 a request).
pub const JOURNEY_STEP_GATES: StepGates =
    StepGates { present: 0.8, p_target: 0.9, margin: 0.3, already_done: 2.0, risky_max: 2.0 };

/// Refitted 2026-09-24 (review round 3) after a live build let "1-day streak" pass over "Streak ended · best 1 day"
/// (Jev 0.56 to 0.62 over ten asks, then cut at 0.4). The calibration had no negative where the words and numbers
/// stay and the meaning goes, so journey_eval.flips now adds them: two names or numbers trading places ("swap") and
/// the phrase broken up around its number ("reword"). Same apps, same split, zero false "shown" allowed on the
/// calibration kinds (74 shown, 181 not): the lowest such cut on Jev's noul is 0.6 (0.4 passed 12 rewords there).
/// Held-out, 181 expectations (54 shown; 127 not: 27 a number changed, 25 another app's, 51 the first screen, 19
/// rewords, 4 swaps, 1 hand-labelled):
///   noul >= 0.6, numbers_shown, words_shown   53 of 54 shown, 0 false   precision 100%  recall 98.1%
///   noul >= 0.6 alone                         53 of 54 shown, 6 false (5 rewords, 1 first screen)
/// The one missed is "Tracking since" (Jev 0.47, plainly there). Beside it, the plain normalized-substring check the
/// labels were drawn with scores 54 of 54 and 0 false on this set: the positives are that oracle's (plus hand
/// labels for number formats), so words_shown agrees with them largely by construction, and on this data Jev adds
/// no catch the code does not make. The unread set (journey_eval.py score, 6 apps the maker built with its own
/// journeys, 166 expectations with 25 rewords and 9 swaps): 45 of 45 shown, 0 of 121 absent passed; without
/// words_shown, 0.6 would pass 11 of the 25 rewords. Jev: 1,457 requests, 243 ms p50, 356 ms p90, 673 input tokens each.
pub const EXPECT_SHOWN: f64 = 0.6;

/// Jev could not answer (no key, the network, a timeout, a wrong envelope): the journeys are not run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct JevDown(pub String);

/// The Jev requests of one check: eve_jev::Meter's counts plus each request's milliseconds, for p50/p90.
#[derive(Debug, Default)]
pub struct JevCalls {
    pub meter: eve_jev::Meter,
    ms: Mutex<Vec<f64>>,
}

impl JevCalls {
    pub fn add(&self, payload: &Value, ms: f64, error: bool) {
        self.meter.add(payload, ms, error);
        self.ms.lock().push(ms);
    }

    /// Each request's milliseconds, in the order they were added.
    pub fn ms(&self) -> Vec<f64> {
        self.ms.lock().clone()
    }

    pub fn as_dict(&self) -> Map<String, Value> {
        let snap = self.meter.snapshot();
        let mut ms = self.ms();
        ms.sort_by(f64::total_cmp);
        let (p50, p90) = if ms.is_empty() {
            (0, 0)
        } else {
            let n = ms.len();
            let median = if n % 2 == 1 { ms[n / 2] } else { (ms[n / 2 - 1] + ms[n / 2]) / 2.0 };
            let at = ((0.9 * n as f64).ceil() as usize).saturating_sub(1).min(n - 1);
            (py::round_int(median), py::round_int(ms[at]))
        };
        let mut out = Map::new();
        out.insert("calls".into(), json!(snap.calls));
        out.insert("input_tokens".into(), json!(snap.input_tokens));
        out.insert("ms".into(), json!(snap.ms));
        out.insert("errors".into(), json!(snap.errors));
        out.insert("usd".into(), json!(py::round_to(eve_pricing::estimate_jev_cost(snap.input_tokens), 6)));
        out.insert("p50_ms".into(), json!(p50));
        out.insert("p90_ms".into(), json!(p90));
        out
    }
}

/// `predict(state, questions) -> envelope`, blocking (jev's predict is): the verifier runs it on a thread.
pub type Predict = Arc<dyn Fn(&Value, &Value) -> anyhow::Result<Value> + Send + Sync>;

/// `str(e)[:160] or type(e).__name__` for a predict failure.
fn down_reason(e: &anyhow::Error) -> String {
    let text = py::take(&e.to_string(), 160);
    if !text.is_empty() {
        return text;
    }
    if let Some(p) = e.downcast_ref::<eve_typed_ask::PredictError>() {
        return p.kind.clone();
    }
    if let Some(p) = e.downcast_ref::<eve_jev::PredictFailure>() {
        return p.kind().to_string();
    }
    "Exception".into()
}

/// Jev behind the two questions a journey asks. Every request runs on a blocking thread (jev's predict is
/// blocking), is timed into `calls`, and fails with JevDown on any failure: a verifier that cannot answer never
/// fails an app, it stops the journeys.
pub struct Verifier {
    predict: Predict,
    pub step_gates: StepGates,
    pub expect_shown: f64,
    pub calls: JevCalls,
    /// every request and its answer, for calibration (journey_eval); None: not kept
    record: Option<Mutex<Vec<Value>>>,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verifier")
            .field("step_gates", &self.step_gates)
            .field("expect_shown", &self.expect_shown)
            .field("calls", &self.calls)
            .finish_non_exhaustive()
    }
}

impl Verifier {
    /// `Verifier(predict)`: the fitted gates and cut-off, nothing recorded.
    pub fn new(predict: Predict) -> Self {
        Self::with(predict, JOURNEY_STEP_GATES, EXPECT_SHOWN, false)
    }

    /// `Verifier(predict, step_gates=, expect_shown=, record=[] if record else None)`.
    pub fn with(predict: Predict, step_gates: StepGates, expect_shown: f64, record: bool) -> Self {
        Self { predict, step_gates, expect_shown, calls: JevCalls::default(), record: record.then(|| Mutex::new(Vec::new())) }
    }

    /// The recorded requests (`{"state", "questions", "answers", "ms"}`), when recording.
    pub fn record(&self) -> Option<Vec<Value>> {
        self.record.as_ref().map(|r| r.lock().clone())
    }

    async fn ask(&self, state: Value, questions: Value) -> Result<(Map<String, Value>, f64), JevDown> {
        // Asked a second time before it counts as down: jev's make_predict retries only a 429 or a 5xx, and one
        // read timeout among the 30-40 requests of a check would otherwise drop every journey.
        let state = Arc::new(state);
        let questions = Arc::new(questions);
        let mut attempt = 1;
        let (result, ms) = loop {
            let t0 = Instant::now();
            let (predict, s, q) = (self.predict.clone(), state.clone(), questions.clone());
            let outcome = match tokio::task::spawn_blocking(move || predict(&s, &q)).await {
                Ok(r) => r,
                Err(join) => Err(anyhow::anyhow!("{join}")),
            };
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            match outcome {
                Ok(result) => break (result, ms),
                Err(e) => {
                    self.calls.add(&json!({}), ms, true);
                    if attempt == 2 {
                        return Err(JevDown(down_reason(&e)));
                    }
                    attempt += 1;
                }
            }
        };
        self.calls.add(&result, ms, false);
        let answers = match result.get("answers") {
            Some(Value::Object(a)) if questions.as_object().is_some_and(|q| q.keys().all(|k| a.contains_key(k))) => a.clone(),
            _ => return Err(JevDown("Jev answered without the questions asked".into())),
        };
        if let Some(record) = &self.record {
            record.lock().push(json!({"state": *state, "questions": *questions, "answers": answers,
                                      "ms": py::round_int(ms)}));
        }
        Ok((answers, ms))
    }

    /// The control the step means, or None; Jev's answer; the controls it was offered.
    pub async fn pick(
        &self,
        step: &Step,
        screen: &Screen,
    ) -> Result<(Option<Control>, StepAnswer, IndexMap<String, Control>), JevDown> {
        let by_id = offered(step, screen);
        if by_id.is_empty() {
            return Ok((None, StepAnswer::new(STEP_NONE, 1.0, 1.0, 0.0, 0.0, 0.0), by_id));
        }
        let options: IndexMap<String, String> = by_id.iter().map(|(k, c)| (k.clone(), c.render())).collect();
        let (answers, ms) = self.ask(step_state(step, screen, &options), step_questions(step, &options)).await?;
        let a = read_step(&answers, &options, ms);
        let chosen = self.step_gates.decide(&a);
        Ok((by_id.get(&chosen).cloned(), a, by_id))
    }

    /// Jev's noul for each expectation, in order.
    pub async fn shown<L: AsRef<str>, E: AsRef<str>>(&self, lines: &[L], expects: &[E]) -> Result<Vec<f64>, JevDown> {
        let (answers, _) = self.ask(expect_state(lines, expects), expect_questions(expects)).await?;
        Ok((1..=expects.len()).map(|i| eve_typed_ask::noul(answers.get(&format!("e{i}")))).collect())
    }
}

/// (the verifier, "") on this Mac's Jev route, or (None, why not). EVE_APP_JOURNEYS=0 turns it off.
pub fn from_env<E: eve_jev::Env + ?Sized>(env: &E) -> (Option<Verifier>, String) {
    let switch = env.var("EVE_APP_JOURNEYS").filter(|s| !s.is_empty()).unwrap_or_else(|| "1".into());
    if ["0", "false", "no", "off"].contains(&py::strip(&switch).to_lowercase().as_str()) {
        return (None, "turned off (EVE_APP_JOURNEYS=0)".into());
    }
    let (url, key, model) = match eve_jev::route_config(env, "") {
        Ok(r) => r,
        Err(e) => return (None, format!("Jev is not configured: {e}")),
    };
    let seconds = if env.var("EVE_JEV_TIMEOUT").is_some_and(|s| !s.is_empty()) {
        eve_jev::timeout_from_env(env).max(JEV_TIMEOUT_S)
    } else {
        JEV_TIMEOUT_S
    };
    let predict = eve_jev::make_predict(&url, &key, &model, eve_jev::PredictOptions { timeout_s: seconds, ..Default::default() });
    (Some(Verifier::new(Arc::new(predict.anyhow_fn()))), String::new())
}

/// `from_env()` on `os.environ`.
pub fn from_process_env() -> (Option<Verifier>, String) {
    from_env(&eve_jev::ProcessEnv)
}

// ---- the page --------------------------------------------------------------------------------------------

/// The page a journey is walked on: the few Playwright calls jev_verify makes. A `selector` names elements; an
/// action acts on the FIRST element it matches (Playwright's `locator(selector).first`). `timeout_ms` bounds
/// finding the element and waiting for it to be usable, as Playwright's `timeout=` does.
///
/// An error that Python's code names by class (`f"{type(e).__name__}: ..."`) should be a [`PageError`] carrying
/// that name ("TimeoutError", "Error"); any other error reads as "Error".
#[async_trait]
pub trait Page: Send + Sync {
    /// `page.evaluate(js)` / `page.evaluate(js, arg)`: the script's JSON result.
    async fn evaluate(&self, js: &str, arg: Option<Value>) -> anyhow::Result<Value>;
    /// `page.locator(selector).first.count()`: how many elements match (only 0 matters).
    async fn count(&self, selector: &str) -> anyhow::Result<usize>;
    /// `locator.fill(value, timeout=)`.
    async fn fill(&self, selector: &str, value: &str, timeout_ms: u64) -> anyhow::Result<()>;
    /// `locator.evaluate(js, timeout=)`: `js` takes the element.
    async fn evaluate_on(&self, selector: &str, js: &str, timeout_ms: u64) -> anyhow::Result<Value>;
    /// `locator.select_option(label=, timeout=)`.
    async fn select_option(&self, selector: &str, label: Option<&str>, timeout_ms: u64) -> anyhow::Result<()>;
    /// `locator.scroll_into_view_if_needed(timeout=)`.
    async fn scroll_into_view_if_needed(&self, selector: &str, timeout_ms: u64) -> anyhow::Result<()>;
    /// `locator.click(timeout=)`.
    async fn click(&self, selector: &str, timeout_ms: u64) -> anyhow::Result<()>;
    /// `page.wait_for_timeout(ms)`.
    async fn wait_for_timeout(&self, ms: u64);
}

/// A page error with the class name Python's message shows (`TimeoutError`, `Error`, `LookupError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct PageError {
    pub kind: String,
    pub message: String,
}

impl PageError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind: kind.into(), message: message.into() }
    }
}

/// `type(e).__name__` for an action's failure.
fn error_kind(e: &anyhow::Error) -> String {
    e.downcast_ref::<PageError>().map(|p| p.kind.clone()).unwrap_or_else(|| "Error".into())
}

// ---- walking a journey on a page ---------------------------------------------------------------------------

/// One expectation as judged: `{text, score, shown}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExpectResult {
    pub text: String,
    pub score: f64,
    pub shown: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JourneyResult {
    pub name: String,
    /// passed | step (failed at `step`) | expect (an expectation not shown)
    pub status: String,
    /// how many steps the journey has
    pub steps: usize,
    /// the 1-based step it failed at
    pub step: usize,
    pub reason: String,
    /// what was on screen when it failed
    pub evidence: String,
    pub expects: Vec<ExpectResult>,
    /// uncaught errors while it ran
    pub errors: Vec<String>,
    /// the step it failed at, as the owner reads it ('tap "Add expense"')
    pub step_words: String,
    /// its last screen, when it passed (app_check: the owner's picture)
    pub screenshot: Option<Vec<u8>>,
    /// what the app had saved at its end (the data that picture shows); Null: none
    pub data: Value,
}

impl JourneyResult {
    /// `JourneyResult(name, status)` with the defaults for the rest.
    pub fn new(name: impl Into<String>, status: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: status.into(),
            steps: 0,
            step: 0,
            reason: String::new(),
            evidence: String::new(),
            expects: Vec::new(),
            errors: Vec::new(),
            step_words: String::new(),
            screenshot: None,
            data: Value::Null,
        }
    }

    pub fn passed(&self) -> bool {
        self.status == "passed"
    }

    /// The problem as the repair round reads it: what was done, what went wrong, what was on screen.
    pub fn issue(&self) -> String {
        let errors = if self.errors.is_empty() { String::new() } else { format!(" While it ran: {}", self.errors.join(" ")) };
        if self.status == "step" {
            return format!(
                "Journey \"{}\" failed at step {} of {}: {} On screen: {}{errors} Fix the app if it lacks this or it does \
                 not work; fix the journey only if the app is right and the journey names it wrongly.",
                self.name, self.step, self.steps, self.reason, self.evidence
            );
        }
        let missing: Vec<String> = self.expects.iter().filter(|e| !e.shown).map(|e| format!("\"{}\"", e.text)).collect();
        format!(
            "Journey \"{}\": after all {} steps the screen does not show {}. The screen showed: {}{errors} Fix the app if \
             it does not do this; fix the journey only if the app shows the right result in other words.",
            self.name,
            self.steps,
            missing.join("; "),
            self.evidence
        )
    }

    /// The same problem in a line the owner reads in the change chat and the Telegram caption: what could
    /// not be done, in which journey. No control lists, no screen dump, no instructions to Claude.
    pub fn owner_line(&self) -> String {
        if self.status == "step" {
            let what = if self.step_words.is_empty() {
                self.reason.split(':').next().unwrap_or("").to_string()
            } else {
                self.step_words.clone()
            };
            return if what.is_empty() {
                format!("\"{}\" did not go through.", self.name)
            } else {
                format!("Couldn't {what} in \"{}\".", self.name)
            };
        }
        let missing: Vec<String> = self.expects.iter().filter(|e| !e.shown).map(|e| format!("\"{}\"", e.text)).collect();
        format!("After \"{}\", the screen didn't show {}.", self.name, missing.join(", "))
    }

    pub fn as_dict(&self) -> Value {
        json!({"name": self.name, "status": self.status, "steps": self.steps, "step": self.step,
               "reason": self.reason, "evidence": self.evidence, "expects": self.expects, "errors": self.errors})
    }
}

fn evidence(screen: &Screen) -> String {
    evidence_within(screen, 400)
}

fn evidence_within(screen: &Screen, limit: usize) -> String {
    let text = screen.lines.join(" | ");
    if text.is_empty() {
        return "(a blank screen)".into();
    }
    let more = if py::len(&text) > limit { "…" } else { "" };
    format!("\"{}{more}\"", py::take(&text, limit))
}

/// `_norm`: casefolded, every run of what is not a word character or `$€£₹.,:%-` one space, ends trimmed.
fn norm(s: &str) -> String {
    let keep = |c: char| py::is_word(c) || "$€£₹.,:%-".contains(c);
    let folded = py::casefold(s);
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in folded.chars() {
        if keep(c) {
            cur.push(c);
        } else if !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words.join(" ")
}

/// The select option a choose step means: the same text, else the one option that holds it.
pub fn option_for(control: &Control, value: &str) -> Option<String> {
    let want = norm(value);
    if let Some(exact) = control.options.iter().find(|o| norm(o) == want) {
        return Some(exact.clone());
    }
    let near: Vec<&String> = control.options.iter().filter(|o| !want.is_empty() && norm(o).contains(&want)).collect();
    if near.len() == 1 {
        Some(near[0].clone())
    } else {
        None
    }
}

/// The page did not answer a read within EVAL_TIMEOUT_S: the app is stuck (an endless loop). The journey
/// fails at that step in seconds, instead of every journey waiting out JOURNEY_TIMEOUT_S.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Hung(pub String);

async fn bounded<T>(fut: impl Future<Output = anyhow::Result<T>>, eval_s: f64, extra_s: f64) -> anyhow::Result<T> {
    let secs = eval_s + extra_s;
    match tokio::time::timeout(Duration::from_secs_f64(secs.max(0.0)), fut).await {
        Ok(r) => r,
        Err(_) => Err(Hung(format!("the page did not answer for {secs:.0} s")).into()),
    }
}

pub async fn read_screen(page: &dyn Page) -> anyhow::Result<Screen> {
    read_screen_within(page, EVAL_TIMEOUT_S).await
}

/// `read_screen` with the page's answer bounded by `eval_s` seconds instead of EVAL_TIMEOUT_S.
pub async fn read_screen_within(page: &dyn Page, eval_s: f64) -> anyhow::Result<Screen> {
    Ok(screen_from(&bounded(page.evaluate(COLLECT_JS, None), eval_s, 0.0).await?))
}

/// `control` (read on `before`) on the screen as it is `now`: the same kind and label, and the same
/// place among the controls that share them ("Delete" in the second row). An app that re-renders its view (a
/// clock, a running timer, a "2 min ago" label) replaces the node Jev chose while Jev was answering.
pub fn refind(control: &Control, before: &Screen, now: &Screen) -> Option<Control> {
    let same = |c: &&Control| c.kind == control.kind && c.label == control.label;
    let nth = before.controls.iter().filter(same).position(|c| c == control).unwrap_or(0);
    let now_same: Vec<&Control> = now.controls.iter().filter(same).collect();
    if nth < now_same.len() {
        return Some(now_same[nth].clone());
    }
    if now_same.len() == 1 {
        Some(now_same[0].clone())
    } else {
        None
    }
}

async fn do_step(page: &dyn Page, selector: &str, step: &Step, control: &Control) -> anyhow::Result<()> {
    if step.do_ == "type" {
        page.fill(selector, &step.value, LOCATE_MS).await?;
        page.evaluate_on(selector, BLUR_JS, LOCATE_MS).await?;
    } else if step.do_ == "choose" && control.kind == "select" {
        let label = option_for(control, &step.value);
        page.select_option(selector, label.as_deref(), LOCATE_MS).await?;
    } else {
        page.scroll_into_view_if_needed(selector, LOCATE_MS).await?;
        page.click(selector, LOCATE_MS).await?;
    }
    Ok(())
}

/// Do the step on the control: "" when done, else why it could not be done. With the `screen` the
/// control was chosen on, a control that is gone from the page (re-rendered meanwhile) is read again and
/// used once more, without asking Jev again: only a control still missing then is the app's problem.
/// Fails with [`Hung`] when the page stops answering, and with the page's own error when it breaks.
pub async fn act(page: &dyn Page, step: &Step, control: &Control, screen: Option<&Screen>) -> anyhow::Result<String> {
    act_within(page, step, control, screen, EVAL_TIMEOUT_S).await
}

/// `act` with the page's answers bounded by `eval_s` seconds instead of EVAL_TIMEOUT_S.
pub async fn act_within(
    page: &dyn Page,
    step: &Step,
    control: &Control,
    screen: Option<&Screen>,
    eval_s: f64,
) -> anyhow::Result<String> {
    if control.kind == "main" || control.kind == "back" {
        let name = if control.kind == "main" { "MainButton" } else { "BackButton" };
        let pressed = bounded(page.evaluate(PRESS_JS, Some(Value::String(name.into()))), eval_s, 0.0).await?;
        return Ok(if truthy(&pressed) { String::new() } else { format!("Telegram's {name} could not be pressed") });
    }
    if step.do_ == "choose" && control.kind == "select" && option_for(control, &step.value).is_none() {
        let shown: Vec<&str> = control.options.iter().take(8).map(String::as_str).collect();
        return Ok(format!("\"{}\" has no option \"{}\" (it has: {}).", control.label, step.value, shown.join(", ")));
    }
    let mut control = control.clone();
    let mut retried = false;
    loop {
        let selector = format!("[data-journey-id='{}']", control.id);
        let attempt: anyhow::Result<()> = async {
            if bounded(page.count(&selector), eval_s, 0.0).await? == 0 {
                return Err(PageError::new("LookupError", "it is no longer on the page").into());
            }
            // a locator's timeout bounds finding the control, not the page running the event it sends (the
            // blur after typing runs the app's change handler)
            bounded(do_step(page, &selector, step, &control), eval_s, 2.0 * LOCATE_MS as f64 / 1000.0).await
        }
        .await;
        let e = match attempt {
            Ok(()) => return Ok(String::new()),
            Err(e) if e.is::<Hung>() => return Err(e),
            Err(e) => e, // covered, detached, not editable: the step did not happen
        };
        if let Some(before) = screen {
            if !retried && bounded(page.count(&selector), eval_s, 0.0).await? == 0 {
                retried = true;
                let now = read_screen_within(page, eval_s).await?;
                if let Some(again) = refind(&control, before, &now) {
                    control = again;
                    continue;
                }
                return Ok(format!("\"{}\" went away when the screen changed, before it could be used.", control.label));
            }
        }
        let text = e.to_string();
        let first = py::take(py::splitlines(&text).first().copied().unwrap_or(""), 100);
        return Ok(format!("\"{}\" could not be used ({}: {first}).", control.label, error_kind(&e)));
    }
}

fn listed(by_id: &IndexMap<String, Control>) -> String {
    let labels: Vec<String> = by_id.values().take(14).map(|c| format!("\"{}\"", c.label)).collect();
    if labels.is_empty() {
        "none".into()
    } else {
        labels.join(", ")
    }
}

fn not_found(step: &Step, target: &str, answer: &StepAnswer, by_id: &IndexMap<String, Control>, gates: &StepGates) -> String {
    let kinds = if step.do_ == "type" { "fields" } else { "controls" };
    if answer.target == STEP_NONE || answer.present < gates.present {
        format!("could not find \"{target}\" to {} (the {kinds} on screen: {}).", step.do_, listed(by_id))
    } else {
        format!("could not tell which control is \"{target}\" (the {kinds} on screen: {}).", listed(by_id))
    }
}

/// How a journey is walked: where the page's uncaught errors land, who hears which step is being done, and how
/// long a read of the page may take.
pub struct WalkOptions<'a> {
    /// the list the page's uncaught errors land in (app_check's session)
    pub errors: Option<&'a Mutex<Vec<String>>>,
    /// names what is being done, so an error says during which step
    pub on_step: Option<&'a (dyn Fn(&str) + Send + Sync)>,
    /// EVAL_TIMEOUT_S
    pub eval_timeout_s: f64,
}

impl Default for WalkOptions<'_> {
    fn default() -> Self {
        Self { errors: None, on_step: None, eval_timeout_s: EVAL_TIMEOUT_S }
    }
}

/// One journey on an app just opened with no data. Fails with [`JevDown`] when Jev stops answering, and with
/// the page's own error when the page breaks (a crashed tab: app_check makes that the journey's failure).
///
/// A "choose" on anything but a <select> (a custom dropdown, chips, a radio group) is two picks: the control
/// labelled with the target is opened, then the option labelled with the value is tapped on the screen that
/// follows. A radio group has no control of its own (its label is a legend), so then the value is picked
/// straight away.
pub async fn walk(page: &dyn Page, journey: &Journey, verifier: &Verifier, options: WalkOptions<'_>) -> anyhow::Result<JourneyResult> {
    let mut res = JourneyResult::new(&journey.name, "passed");
    res.steps = journey.steps.len();
    let mut n = 0usize;
    let mut judging = false;
    let outcome = walk_steps(page, journey, verifier, &options, &mut res, &mut n, &mut judging).await;
    if let Err(e) = outcome {
        match e.downcast::<Hung>() {
            Ok(hung) => {
                let after = if judging { "after its last step".to_string() } else { format!("at step {n}") };
                res.status = "step".into();
                res.step = n.max(1);
                res.expects = Vec::new();
                let doing = if res.step_words.is_empty() { "opening it" } else { res.step_words.as_str() };
                res.reason = format!(
                    "{doing}: the app stopped responding {after} ({hung}: an endless loop, or work that never yields)."
                );
                res.evidence = "(the page stopped answering)".into();
            }
            Err(other) => return Err(other),
        }
    }
    res.errors = match options.errors {
        Some(errors) => errors
            .lock()
            .iter()
            .filter(|e| e.starts_with("Uncaught error") || e.starts_with("Console error"))
            .take(3)
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    Ok(res)
}

async fn walk_steps(
    page: &dyn Page,
    journey: &Journey,
    verifier: &Verifier,
    options: &WalkOptions<'_>,
    res: &mut JourneyResult,
    n: &mut usize,
    judging: &mut bool,
) -> anyhow::Result<()> {
    let gates = verifier.step_gates;
    let eval_s = options.eval_timeout_s;
    for (i, step) in journey.steps.iter().enumerate() {
        *n = i + 1;
        if let Some(on_step) = options.on_step {
            on_step(&format!("the journey \"{}\", step {} ({})", journey.name, *n, step.words()));
        }
        res.step_words = step.words();
        let mut screen = read_screen_within(page, eval_s).await?;
        let (mut control, answer, by_id) = verifier.pick(step, &screen).await?;
        let mut doing = step.clone();
        if step.do_ == "choose" && control.as_ref().is_none_or(|c| c.kind != "select") {
            let opened = control.clone();
            if let Some(opened) = &opened {
                let failed = act_within(page, &Step::tap(&opened.label), opened, Some(&screen), eval_s).await?;
                if !failed.is_empty() {
                    res.status = "step".into();
                    res.step = *n;
                    res.reason = format!("{}: {failed}", step.words());
                    res.evidence = evidence(&read_screen_within(page, eval_s).await?);
                    break;
                }
                page.wait_for_timeout(STEP_WAIT_MS).await;
                screen = read_screen_within(page, eval_s).await?;
            }
            doing = Step::tap(&step.value);
            let (value_control, value_answer, value_ids) = verifier.pick(&doing, &screen).await?;
            control = value_control;
            if control.is_none() {
                let why = match &opened {
                    None => not_found(step, &step.target, &answer, &by_id, &gates),
                    Some(opened) => format!(
                        "opened \"{}\" but {}",
                        opened.label,
                        not_found(&doing, &step.value, &value_answer, &value_ids, &gates).replace(" to tap ", " to pick ")
                    ),
                };
                res.status = "step".into();
                res.step = *n;
                res.reason = format!("{}: {why}", step.words());
                res.evidence = evidence(&screen);
                break;
            }
        } else if control.is_none() {
            res.status = "step".into();
            res.step = *n;
            res.reason = format!("{}: {}", step.words(), not_found(step, &step.target, &answer, &by_id, &gates));
            res.evidence = evidence(&screen);
            break;
        }
        let Some(control) = control else { break };
        let failed = act_within(page, &doing, &control, Some(&screen), eval_s).await?;
        page.wait_for_timeout(STEP_WAIT_MS).await;
        if !failed.is_empty() {
            let screen = read_screen_within(page, eval_s).await?;
            res.status = "step".into();
            res.step = *n;
            res.reason = format!("{}: {failed}", step.words());
            res.evidence = evidence(&screen);
            break;
        }
    }
    if res.status == "passed" {
        *judging = true;
        let text = bounded(page.evaluate(TEXT_JS, None), eval_s, 0.0).await?;
        let screen = screen_from(&json!({"text": text}));
        let scores = verifier.shown(&screen.lines, &journey.expect).await?;
        res.expects = journey
            .expect
            .iter()
            .zip(scores)
            .map(|(e, s)| ExpectResult {
                text: e.clone(),
                score: py::round_to(s, 3),
                shown: s >= verifier.expect_shown && numbers_shown(e, &screen.lines) && words_shown(e, &screen.lines),
            })
            .collect();
        if !res.expects.iter().all(|e| e.shown) {
            // the evidence is the screen as a person saw it, not Jev's shortlist: "No expenses yet" shares no
            // word with "Alex owes Sam $15.00" and is exactly what the repair round needs to read
            res.status = "expect".into();
            res.evidence = evidence(&screen);
        }
    }
    Ok(())
}

/// Every journey of one check: the results of the ones that finished, why the rest did not run (if any),
/// and what Jev cost.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct JourneyRun {
    pub results: Vec<JourneyResult>,
    pub total: usize,
    pub not_run: String,
    /// JevCalls::as_dict of the check; an empty object until the journeys ran
    pub jev: Map<String, Value>,
    pub secs: f64,
}

impl JourneyRun {
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|r| r.passed()).count()
    }

    pub fn summary(&self) -> String {
        if self.total == 0 {
            return String::new();
        }
        if !self.not_run.is_empty() && self.results.is_empty() {
            return format!("journeys not run ({})", self.not_run);
        }
        if !self.not_run.is_empty() {
            return format!(
                "journeys {}/{} ({} not run: {})",
                self.passed(),
                self.total,
                self.total as i64 - self.results.len() as i64,
                self.not_run
            );
        }
        format!("journeys {}/{}", self.passed(), self.total)
    }

    /// A journey that finished and failed is a problem even when others could not run: Jev going down
    /// later never hides breakage it already saw.
    pub fn issues(&self) -> Vec<String> {
        self.results.iter().filter(|r| !r.passed()).map(JourneyResult::issue).collect()
    }

    /// Each issue() by the short line the owner reads instead (build_record, the Telegram caption).
    pub fn owner_lines(&self) -> IndexMap<String, String> {
        self.results.iter().filter(|r| !r.passed()).map(|r| (r.issue(), r.owner_line())).collect()
    }

    pub fn as_dict(&self) -> Value {
        json!({"total": self.total, "passed": self.passed(), "ran": self.results.len(), "not_run": self.not_run,
               "secs": py::round_to(self.secs, 1), "jev": self.jev,
               "results": self.results.iter().map(JourneyResult::as_dict).collect::<Vec<_>>()})
    }
}

#[cfg(test)]
mod tests;
