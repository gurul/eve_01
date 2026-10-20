//! Walking a plan: every step grounded on a fresh snapshot, no planner turn between steps, none at the end. Port of
//! `plan_executor.py`; its module docstring is the contract and is kept here in substance.
//!
//! [`run_plan`] takes an eve_plan_contract::Plan and the same senses/effectors the lane uses (fast_lane's module
//! docs have the protocol) and returns a PlanResult. It never fails for a model or sense failure.
//!
//! Per step:
//!
//! ```text
//! open_app / open_url   the helpers' own calls; their sentence ("opened … (frontmost after …)") is the oracle
//! click                 fast_lane::run_delegate, one step: the exact label first when the planner read one in
//!                       the outline (the keyword gate usually decides it, Jev only has to agree), then the
//!                       description. Every gate of the lane applies.
//! type                  the field is the only editable one; or the one the step names (or the plan just clicked)
//!                       with Jev agreeing; or Jev's pick among them under the same cut-offs. Then the lane's
//!                       focus_and_type, which pastes only when the focus really is that field
//! press_key             one key. Return submits only a search the human dictated; anything else asks first.
//! fill                  the effector's fill_from_vault (browser_lane's page; vault): code finds the form's
//!                       fields and types a vault item's values. No model picks the field or sees a value; an
//!                       effector without a vault (the Mac's) refuses it.
//! checkpoint            stop and hand back: the planner said it could not see past this point
//! ```
//!
//! Before a click or a type, effectors that know how (the browser page) dismiss a cookie/consent notice with a
//! button that refuses or closes; the sensitive table keeps any "accept"/"agree" button out of reach.
//!
//! What happened to a step is one word from a closed set (cua's action-result contract, trycua/cua, MIT):
//! confirmed (a readback saw it), unverifiable (input was applied, nothing could be read back), suspected_noop
//! (input was applied and the screen did not change), refused (no input was applied). `unverifiable` is never
//! reported as success: the closing sentence says what could not be confirmed.
//!
//! Who gets control when a plan stops:
//!
//! ```text
//! needs_human   a consequential step — the plan's flag, a sensitive label, Jev's risky noul, a Return outside
//!               a dictated search, composed text headed for a terminal. Zero input was applied for that step.
//!               The caller asks the human and calls run_plan again with `start` and `approved`.
//! checkpoint /
//! partial / none  the planner, with the ledger of what was applied. `none` guarantees zero input overall.
//! ```
//!
//! Nothing here decides a task is DONE on a model's word: `complete` means every step applied input that was not
//! a suspected no-op, every `expect` that was given held, and the plan's `success` did not read false.
//!
//! A browser search is stricter. Its check is code's, derived from the request (plan_contract.request_check, the
//! `url_query` kind), and it must be READ TRUE at the end — and have been read false before step 1. So such a plan
//! ends `partial` with every step applied (next_index == len(steps)) when:
//!
//! ```text
//! success_already_held   the check already held before step 1 (read once, without waiting), so it proves
//!                        nothing. A resumed plan (start > 0) cannot look again: the caller hands back the
//!                        first call's `success_rejected`.
//! the plan ran but …     it did not hold at the end.
//! ```
//!
//! Every other plan — on the Mac, or a browser plan with no derived check — keeps the planner's check as given:
//! none at all is not a failure, and an app_frontmost or text_visible check that is already true is ordinary once
//! `planned_for` has brought the app back.

use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::{json, Value};

use eve_plan_contract::{bare_host, url_matches, url_query_matches, Expect, Plan, PlanStep, CODE_EXPECT_KINDS};
use eve_typed_ask::{StepGates, JEV_STEP_GATES};

use crate::ax_candidates::Candidate;
use crate::fast_lane::{
    editable_pool, is_search_field, jev_context, run_delegate, DelegateOptions, DelegateResult, Effectors, LaneStatus,
    Senses, StepAsk,
};
use crate::{error_kind, error_message, Clock};

pub const STATUSES: [&str; 6] = ["complete", "partial", "none", "needs_human", "checkpoint", "unavailable"];
pub const EXPECT_WAIT_SECS: f64 = 2.5;
pub const EXPECT_POLL_SECS: f64 = 0.4;
pub const MAX_WALL_SECS: f64 = 30.0;
/// Typing composed text into a shell is running a command the human never said: ask before the first key.
pub const SHELL_APPS: &[&str] = &["terminal", "warp", "iterm", "iterm2", "ghostty", "kitty", "alacritty", "wezterm"];

/// Python's `round(x, 1)`: correctly rounded on the exact binary value, ties to even.
fn round1(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.1}").parse().unwrap_or(x)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// 1-based, the plan's numbering
    pub index: usize,
    /// PlanStep::describe()
    pub step: String,
    /// confirmed | unverifiable | suspected_noop | refused
    pub effect: String,
    /// keyword | keyword+jev | jev | code | helper
    pub how: String,
    /// the lane's own line, or why nothing was applied
    pub note: String,
    pub ms: f64,
}

impl Entry {
    /// `Entry(index, step, effect)` with every other field at its default.
    pub fn new(index: usize, step: &str, effect: &str) -> Self {
        Entry {
            index,
            step: step.to_string(),
            effect: effect.to_string(),
            how: String::new(),
            note: String::new(),
            ms: 0.0,
        }
    }

    /// `{"index", "step", "effect", "how", "note" (≤ 200 chars), "ms" (rounded to 0.1)}`.
    pub fn to_dict(&self) -> Value {
        json!({"index": self.index, "step": self.step, "effect": self.effect, "how": self.how,
               "note": crate::py_take(&self.note, 200), "ms": round1(self.ms)})
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanResult {
    /// STATUSES
    pub status: String,
    /// 0-based index of the step to resume from (needs_human, checkpoint, partial)
    pub next_index: usize,
    pub reason: String,
    /// needs_human: what the human is asked about
    pub confirm: String,
    /// complete: what eve says
    pub sentence: String,
    pub ledger: Vec<Entry>,
    pub log_lines: Vec<String>,
    pub ms: f64,
    /// the plan's success check already held before step 1 (pass it back on resume)
    pub success_rejected: bool,
}

impl PlanResult {
    /// `PlanResult(status)` with every other field at its default.
    pub fn new(status: &str) -> Self {
        PlanResult {
            status: status.to_string(),
            next_index: 0,
            reason: String::new(),
            confirm: String::new(),
            sentence: String::new(),
            ledger: Vec::new(),
            log_lines: Vec::new(),
            ms: 0.0,
            success_rejected: false,
        }
    }

    /// The entries whose effect is not "refused".
    pub fn applied(&self) -> Vec<&Entry> {
        self.ledger.iter().filter(|e| e.effect != "refused").collect()
    }

    /// `{"status", "next_index", "reason", "confirm", "sentence", "ms", "success_rejected", "ledger"}`.
    pub fn to_dict(&self) -> Value {
        json!({"status": self.status, "next_index": self.next_index, "reason": self.reason,
               "confirm": self.confirm, "sentence": self.sentence, "ms": round1(self.ms),
               "success_rejected": self.success_rejected,
               "ledger": self.ledger.iter().map(Entry::to_dict).collect::<Vec<_>>()})
    }
}

/// One read of an expectation: Ok(seen), or Err when the oracle could not read.
fn read_once(
    expect: &Expect,
    needle: &str,
    senses: &dyn Senses,
    frontmost_app: &dyn Fn() -> String,
) -> anyhow::Result<bool> {
    if expect.kind == "app_frontmost" {
        return Ok(crate::casefold(&frontmost_app()).contains(needle));
    }
    let snap = senses.snapshot()?;
    if expect.kind == "url_is" || expect.kind == "url_query" {
        let Some(url) = senses.current_url() else {
            return Ok(false);
        };
        let url = url?;
        return Ok(if expect.kind == "url_is" {
            url_matches(&url, &expect.value)
        } else {
            url_query_matches(&url, expect)
        });
    }
    if expect.kind == "title_contains" {
        return Ok(crate::casefold(&snap.title).contains(needle));
    }
    senses.text_visible(&expect.value)
}

/// Python `_holds`: True/False once the expectation can be read, None when there is none or reading it failed (a
/// url check that cannot be read is False). Polls up to `wait`: a page that is still loading is not a failed step.
pub fn holds(
    expect: Option<&Expect>,
    senses: &dyn Senses,
    effectors: &dyn Effectors,
    frontmost_app: &dyn Fn() -> String,
    clock: Clock<'_>,
    wait: f64,
) -> Option<bool> {
    let expect = expect?;
    let needle = crate::casefold(&expect.value);
    let t0 = clock();
    loop {
        let seen = match read_once(expect, &needle, senses, frontmost_app) {
            Ok(seen) => seen,
            // an oracle that cannot read is unknown, never a no
            Err(_) => return if expect.kind == "url_is" || expect.kind == "url_query" { Some(false) } else { None },
        };
        if seen {
            return Some(true);
        }
        if clock() - t0 >= wait {
            return Some(false);
        }
        if effectors.settle(EXPECT_POLL_SECS).is_err() {
            return Some(false);
        }
    }
}

/// Python `_confirm_label`: the label inside the lane's `confirm: "{label}" needs the human's yes` line.
pub fn confirm_label(line: &str, fallback: &str) -> String {
    if let Some(rest) = line.strip_prefix("confirm: \"") {
        if let Some(end) = rest.find("\" needs the human") {
            if end > 0 {
                return rest[..end].replace("\\\"", "\"");
            }
        }
    }
    fallback.to_string()
}

/// Python `_reason`: the `reason=` of the lane's escalate line, without what follows it (a dialog's quoted text).
pub fn reason(line: &str) -> String {
    let Some((_, after)) = line.split_once("reason=") else {
        return String::new();
    };
    let cut = |s: &'_ str, sep: &str| -> String { s.split_once(sep).map_or(s, |(head, _)| head).to_string() };
    let r = cut(after, ";");
    let r = cut(&r, ":");
    let r = cut(&r, " (");
    crate::py_strip(&r).to_string()
}

/// Python `_say`: the closing sentence.
pub fn say(plan: &Plan, ledger: &[Entry]) -> String {
    let unsure: Vec<&Entry> = ledger.iter().filter(|e| e.effect == "unverifiable").collect();
    if unsure.is_empty() && !plan.final_say.is_empty() {
        return plan.final_say.clone();
    }
    let done: Vec<&str> = ledger.iter().filter(|e| e.effect == "confirmed").map(|e| e.step.as_str()).collect();
    if let Some(last) = unsure.last() {
        if unsure.len() == ledger.len() {
            return format!("I did it, but I couldn't confirm it worked: {}.", last.step);
        }
        return format!("I did it; the one thing I couldn't confirm was: {}.", last.step);
    }
    match done.last() {
        None => "Done.".to_string(),
        Some(step) => format!("Done: {step}."),
    }
}

/// run_plan's keyword arguments, with Python's defaults (`Default`).
pub struct PlanOptions<'a> {
    pub start: usize,
    /// 0-based step index the human said yes to → what they were asked about (PlanResult.confirm)
    pub approved: HashMap<i64, String>,
    pub decide: String,
    pub dry_run: bool,
    pub planned_for: String,
    pub success_rejected: bool,
    pub step_gates: StepGates,
    pub clock: Clock<'a>,
}

impl Default for PlanOptions<'_> {
    fn default() -> Self {
        PlanOptions {
            start: 0,
            approved: HashMap::new(),
            decide: "jev".to_string(),
            dry_run: false,
            planned_for: String::new(),
            success_rejected: false,
            step_gates: JEV_STEP_GATES,
            clock: &crate::perf_counter,
        }
    }
}

/// Python run_plan's inner `stop`: the closing status, the resume point and the closing log line.
fn stop(
    out: &mut PlanResult,
    status: &str,
    index: usize,
    why: &str,
    confirm: &str,
    total: usize,
    elapsed_ms: f64,
) -> PlanResult {
    let status = if status == "partial" && out.applied().is_empty() {
        "none" // zero input overall: the caller starts clean
    } else {
        status
    };
    out.status = status.to_string();
    out.next_index = index;
    out.reason = why.to_string();
    out.confirm = confirm.to_string();
    out.ms = elapsed_ms;
    let mut line = format!("plan {status}: step {}/{total}", (index + 1).min(total));
    if !why.is_empty() {
        line.push_str(&format!(" {why}"));
    }
    if !confirm.is_empty() {
        line.push_str(&format!(" confirm=\"{confirm}\""));
    }
    out.log_lines.push(line);
    std::mem::replace(out, PlanResult::new(""))
}

/// The lane on one click objective: max_steps=1, skip_if_selected, the plan's decide mode and gates.
#[allow(clippy::too_many_arguments)]
fn delegate_once(
    objective: &str,
    senses: &dyn Senses,
    effectors: &dyn Effectors,
    asker: Option<&dyn StepAsk>,
    decide: &str,
    dry_run: bool,
    approve: &str,
    step_gates: StepGates,
    clock: Clock<'_>,
) -> DelegateResult {
    run_delegate(
        objective,
        senses,
        effectors,
        None,
        DelegateOptions {
            decide: decide.to_string(),
            asker,
            max_steps: 1,
            skip_if_selected: true,
            dry_run,
            approve: (!approve.is_empty()).then(|| approve.to_string()),
            step_gates,
            clock,
            ..DelegateOptions::default()
        },
    )
}

/// Walk `plan.steps[start..]`. `approved` maps a 0-based step index the human has said yes to onto what they were
/// asked about (PlanResult.confirm): for a sensitive control that is its label, which the lane lets through
/// exactly once (fast_lane's `approve`).
///
/// `planned_for` is the app whose window the planner was shown. Planning takes seconds and the human keeps using
/// the Mac meanwhile (live, 2026-09-21: Finder was in front by the time the plan arrived), so when a plan opens no
/// app of its own, that app is brought back to the front before the first step.
///
/// `success_rejected` is the first call's PlanResult.success_rejected, handed back on a resume: only the call that
/// starts at step 1 can read the success check before anything was done. It counts only for a plan whose check is
/// code's (url_query).
#[allow(clippy::too_many_arguments)]
pub fn run_plan(
    plan: &Plan,
    senses: &dyn Senses,
    effectors: &dyn Effectors,
    asker: Option<&dyn StepAsk>,
    open_app: &dyn Fn(&str) -> anyhow::Result<String>,
    open_url: &dyn Fn(&str) -> anyhow::Result<String>,
    frontmost_app: &dyn Fn() -> String,
    opts: PlanOptions<'_>,
) -> PlanResult {
    let PlanOptions { start, approved, mut decide, dry_run, planned_for, success_rejected, step_gates, clock } = opts;
    let t_start = clock();
    let total = plan.steps.len();
    let mut success = plan.success.clone();
    let strict = success.as_ref().is_some_and(|s| CODE_EXPECT_KINDS.contains(&s.kind.as_str()));
    let mut out = PlanResult::new("none");
    out.next_index = start;
    out.success_rejected = success_rejected && strict;
    let ok: HashMap<usize, String> =
        approved.into_iter().filter_map(|(i, label)| usize::try_from(i).ok().map(|i| (i, label))).collect();
    if decide == "jev" && asker.is_none() {
        decide = "keyword".to_string(); // no hosted model configured: the gate alone decides
        out.log_lines.push("plan: jev is not configured; the keyword gate decides alone".to_string());
    }
    // (into a search field, text_source, text) of the last type
    let mut typed: Option<(bool, String, String)> = None;
    // the step before this one was a click that applied
    let mut clicked = false;
    let first = plan.steps.get(start);
    if !planned_for.is_empty()
        && !dry_run
        && first.is_some_and(|f| f.kind != "open_app" && f.kind != "open_url")
        && crate::casefold(&planned_for) != crate::casefold(&frontmost_app())
    {
        match open_app(&planned_for) {
            Ok(said) => out.log_lines.push(format!("plan: {planned_for} is no longer in front — {said}")),
            // the step's own gates decide what happens next
            Err(e) => out.log_lines.push(format!("plan: could not bring {planned_for} back ({})", error_kind(&e))),
        }
    }
    if strict && start == 0 && !dry_run && !plan.steps.is_empty() {
        if let Some(check) = success.as_mut() {
            if check.here.is_empty() {
                // The page open before step 1: with no site named, a search on it counts (url_query_matches).
                // An unreadable page adds nothing.
                let here = match senses.current_url() {
                    Some(Ok(url)) => bare_host(&url),
                    _ => String::new(),
                };
                if !here.is_empty() {
                    check.here = here;
                }
            }
        }
        // Read once, no polling: a check that is true before any input says nothing about what the plan does.
        if holds(success.as_ref(), senses, effectors, frontmost_app, clock, 0.0) == Some(true) {
            out.success_rejected = true;
            let check = success.as_ref().map(|s| (s.kind.as_str(), s.value.as_str())).unwrap_or_default();
            out.log_lines.push(format!(
                "plan: the success check {} \"{}\" already holds before step 1; it cannot confirm the plan",
                check.0, check.1
            ));
        }
    }
    let elapsed = |clock: Clock<'_>| (clock() - t_start) * 1000.0;

    for i in start..total {
        let step = &plan.steps[i];
        let t0 = clock();
        if clock() - t_start > MAX_WALL_SECS {
            return stop(&mut out, "partial", i, "out_of_time", "", total, elapsed(clock));
        }
        if step.kind == "checkpoint" {
            return stop(&mut out, "checkpoint", i + 1, "the planner asked to look again here", "", total, elapsed(clock));
        }
        if step.consequential && !ok.contains_key(&i) {
            let what = step.describe();
            return stop(&mut out, "needs_human", i, "the plan marked this step consequential", &what, total, elapsed(clock));
        }

        if matches!(step.kind.as_str(), "click" | "type" | "fill") && !dry_run {
            clear_consent(effectors, i, &mut out);
        }
        let mut entry = Entry::new(i + 1, &step.describe(), "refused");
        if dry_run && step.kind != "click" {
            entry.note = "dry_run".to_string();
            out.ledger.push(entry);
            continue;
        }

        match step.kind.as_str() {
            "open_app" | "open_url" => {
                let opened = if step.kind == "open_app" { open_app(&step.target) } else { open_url(&step.target) };
                let said = match opened {
                    Ok(said) => said,
                    // no such app, a refused URL: nothing was opened
                    Err(e) => {
                        entry.note = format!("{}: {}", error_kind(&e), error_message(&e));
                        out.ledger.push(entry);
                        let why = format!("{}_failed", step.kind);
                        return stop(&mut out, "partial", i, &why, "", total, elapsed(clock));
                    }
                };
                entry.how = "helper".to_string();
                entry.effect =
                    if said.starts_with("opened ") && !said.contains(" but ") { "confirmed" } else { "unverifiable" }
                        .to_string();
                entry.note = said;
            }
            "click" => {
                let approved_here = ok.get(&i).cloned().unwrap_or_default();
                grant(effectors, &approved_here);
                let mut objectives: Vec<&str> = Vec::new();
                for o in [step.label_hint.as_str(), step.target.as_str()] {
                    if !o.is_empty() && !objectives.contains(&o) {
                        objectives.push(o);
                    }
                }
                let mut result: Option<DelegateResult> = None;
                let mut objective = "";
                for o in objectives {
                    objective = o;
                    let r = delegate_once(o, senses, effectors, asker, &decide, dry_run, &approved_here, step_gates, clock);
                    out.log_lines.extend(r.log_lines.iter().cloned());
                    let settled = matches!(r.status, LaneStatus::Stopped | LaneStatus::Done | LaneStatus::Confirm);
                    result = Some(r);
                    if settled {
                        break;
                    }
                }
                let Some(mut result) = result else {
                    // parse_plan never lets a click without a target through (Python asserts here)
                    grant(effectors, "");
                    entry.note = "the step names no control".to_string();
                    out.ledger.push(entry);
                    return stop(&mut out, "partial", i, "no_target", "", total, elapsed(clock));
                };
                entry.note = result.line.clone();
                entry.how = result.steps.last().map_or_else(|| "code".to_string(), |last| last.via.clone());
                if result.status == LaneStatus::Confirm
                    && ok.get(&i).is_some_and(|said| *said != confirm_label(&result.line, ""))
                {
                    // The human already said yes to THIS step (the plan had flagged it); the lane now names the
                    // control. Their yes covers it: one question a step, not two — at the effector's own gate too.
                    let label = confirm_label(&result.line, "");
                    grant(effectors, &label);
                    result = delegate_once(objective, senses, effectors, asker, &decide, dry_run, &label, step_gates, clock);
                    out.log_lines.extend(result.log_lines.iter().cloned());
                    entry.note = result.line.clone();
                    entry.how = result.steps.last().map_or_else(|| "code".to_string(), |last| last.via.clone());
                }
                grant(effectors, ""); // a yes covers one step, never the next
                let last = result.steps.last();
                if result.status == LaneStatus::Confirm {
                    out.ledger.push(entry);
                    let label = confirm_label(&result.line, &step.target);
                    return stop(&mut out, "needs_human", i, "a consequential control", &label, total, elapsed(clock));
                }
                if result.status == LaneStatus::Done {
                    entry.effect = "confirmed".to_string(); // already in the state the step asks for
                } else if let (LaneStatus::Stopped, Some(last)) = (result.status, last.filter(|l| l.verdict == "act")) {
                    entry.effect = match last.changed {
                        Some(true) => "confirmed",
                        Some(false) => "suspected_noop",
                        None => "unverifiable",
                    }
                    .to_string();
                } else if result.status == LaneStatus::Stopped && dry_run {
                    entry.note = format!("dry_run: {}", result.line);
                    out.ledger.push(entry);
                    continue;
                } else {
                    out.ledger.push(entry);
                    let why = reason(&result.line);
                    let why = if why.is_empty() { result.status.as_str().to_string() } else { why };
                    return stop(&mut out, "partial", i, &why, "", total, elapsed(clock));
                }
            }
            "type" => {
                let app = crate::casefold(&frontmost_app());
                if step.text_source == "composed" && SHELL_APPS.contains(&app.as_str()) && !ok.contains_key(&i) {
                    let what = format!("type \"{}\" into {app}", step.text);
                    return stop(&mut out, "needs_human", i, "composed text into a shell", &what, total, elapsed(clock));
                }
                let (field, how, why) = pick_field(step, senses, asker, &step_gates, clicked);
                let Some(field) = field else {
                    entry.note = why.clone();
                    out.ledger.push(entry);
                    let risky = why == "risky";
                    let what = if risky { step.describe() } else { String::new() };
                    return stop(&mut out, if risky { "needs_human" } else { "partial" }, i, &why, &what, total, elapsed(clock));
                };
                // an effector that failed applied nothing we can vouch for
                let said = effectors
                    .focus_and_type(&field, &step.text)
                    .unwrap_or_else(|e| format!("refused: {}: {}", error_kind(&e), error_message(&e)));
                entry.how = how;
                entry.note = said;
                if entry.note.starts_with("refused:") {
                    out.ledger.push(entry);
                    return stop(&mut out, "partial", i, "focus_changed", "", total, elapsed(clock));
                }
                entry.effect = "unverifiable".to_string(); // pasted; only an `expect` can confirm it
                typed = Some((is_search_field(&field), step.text_source.clone(), step.text.clone()));
            }
            "fill" => {
                let said = match effectors.fill_from_vault(&step.text) {
                    None => "refused: the vault fills only in the browser".to_string(),
                    Some(Ok(said)) => said,
                    // an effector that failed applied nothing we can vouch for
                    Some(Err(e)) => format!("refused: {}", error_kind(&e)),
                };
                entry.how = "code".to_string();
                entry.note = said;
                if !entry.note.starts_with("filled ") {
                    out.ledger.push(entry);
                    return stop(&mut out, "partial", i, "fill_refused", "", total, elapsed(clock));
                }
                entry.effect = if entry.note.contains(" but ") { "unverifiable" } else { "confirmed" }.to_string();
                typed = None;
            }
            "press_key" => {
                let submit = step.key == "return";
                let dictated_search = typed.as_ref().is_some_and(|t| t.0 && t.1 == "utterance");
                if submit && !dictated_search && !ok.contains_key(&i) {
                    let what = match &typed {
                        Some(t) => format!("press Return to submit \"{}\"", t.2),
                        None => "press Return".to_string(),
                    };
                    return stop(&mut out, "needs_human", i, "return outside a dictated search", &what, total, elapsed(clock));
                }
                match effectors.press(&step.key) {
                    Ok(said) => {
                        entry.how = "code".to_string();
                        entry.note = said;
                        entry.effect = "unverifiable".to_string();
                    }
                    Err(e) => {
                        entry.note = format!("{}: {}", error_kind(&e), error_message(&e));
                        out.ledger.push(entry);
                        return stop(&mut out, "partial", i, "press_failed", "", total, elapsed(clock));
                    }
                }
                typed = None;
            }
            _ => {}
        }

        if entry.effect != "refused" && step.kind != "click" {
            // a failed settle is a shorter wait
            let _ = effectors.settle(0.6);
        }
        let held = if entry.effect != "refused" {
            holds(step.expect.as_ref(), senses, effectors, frontmost_app, clock, EXPECT_WAIT_SECS)
        } else {
            None
        };
        if held == Some(true) {
            entry.effect = "confirmed".to_string();
        }
        entry.ms = (clock() - t0) * 1000.0;
        let line = if entry.how.is_empty() {
            format!("plan step {}: {} -> {}", i + 1, entry.step, entry.effect)
        } else {
            format!("plan step {}: {} -> {} ({})", i + 1, entry.step, entry.effect, entry.how)
        };
        let effect = entry.effect.clone();
        out.ledger.push(entry);
        out.log_lines.push(line);
        clicked = step.kind == "click" && effect != "refused";
        if held == Some(false) {
            let (kind, value) = step.expect.as_ref().map(|e| (e.kind.as_str(), e.value.as_str())).unwrap_or_default();
            let why = format!("expected {kind} \"{value}\" and did not see it");
            return stop(&mut out, "partial", i + 1, &why, "", total, elapsed(clock));
        }
        if effect == "suspected_noop" {
            return stop(&mut out, "partial", i + 1, "the screen did not change", "", total, elapsed(clock));
        }
    }

    if dry_run {
        return stop(&mut out, "complete", total, "dry_run", "", total, elapsed(clock));
    }
    if out.success_rejected {
        return stop(&mut out, "partial", total, "success_already_held", "", total, elapsed(clock));
    }
    let last = holds(success.as_ref(), senses, effectors, frontmost_app, clock, EXPECT_WAIT_SECS);
    if last == Some(false) {
        let value = success.as_ref().map(|s| s.value.as_str()).unwrap_or_default();
        let what = if strict { format!("the search for \"{value}\"") } else { format!("\"{value}\"") };
        let why = format!("the plan ran but {what} is not on screen");
        return stop(&mut out, "partial", total, &why, "", total, elapsed(clock));
    }
    if last == Some(true) {
        if let Some(e) = out.ledger.last_mut() {
            e.effect = "confirmed".to_string();
        }
    }
    out.sentence = say(plan, &out.ledger);
    stop(&mut out, "complete", total, "", "", total, elapsed(clock))
}

/// Python `_grant`: tell an effector with its own sensitive-label gate (browser_lane's page, desktop_helpers'
/// adapter) which label the human's yes covers for THIS click, normalised as they compare it; "" for none. Before
/// this the page kept the first approval of a run as given — the step's description ("click button labelled Send
/// message"), never a label — so an approved sensitive click was refused at the page and read as hit_test_failed
/// (browser_model_eval smoke run, contact_form, 2026-09-24).
pub fn grant(effectors: &dyn Effectors, approved: &str) {
    effectors.grant(&norm(approved));
}

/// Python `_clear_consent`: before a click or a type, an effector that can dismiss a cookie/consent notice does,
/// with a button that refuses or closes, never one that agrees. What it pressed goes in the ledger like any applied
/// input. Effectors without the ability (the Mac's) are left alone.
pub fn clear_consent(effectors: &dyn Effectors, i: usize, out: &mut PlanResult) {
    let said = match effectors.clear_consent() {
        None => return,
        Some(Ok(said)) => said,
        // the step's own dialog gate decides what happens next
        Some(Err(e)) => format!("refused: {}", error_kind(&e)),
    };
    if said.is_empty() {
        return;
    }
    out.log_lines.push(format!("plan step {}: {said}", i + 1));
    if !said.starts_with("refused") {
        let mut entry = Entry::new(i + 1, "dismiss the cookie notice", "confirmed");
        entry.how = "code".to_string();
        entry.note = said;
        out.ledger.push(entry);
    }
}

/// Python `_norm`: whitespace collapsed, case-folded.
pub fn norm(s: &str) -> String {
    crate::casefold(&crate::py_collapse(s))
}

/// `re.search(r"(?<![a-z0-9])" + re.escape(label) + r"(?![a-z0-9])", said)`: `label` occurs in `said` with no
/// ASCII letter or digit right before or right after it.
fn contains_as_words(said: &str, label: &str) -> bool {
    let alnum = |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    said.char_indices().any(|(at, _)| {
        said[at..].starts_with(label)
            && !alnum(said[..at].chars().next_back())
            && !alnum(said[at + label.len()..].chars().next())
    })
}

/// `_named_field`, as the index of the field in `pool`.
fn named_field_index(step: &PlanStep, pool: &[Candidate]) -> Option<usize> {
    let hint = norm(&step.label_hint);
    if !hint.is_empty() {
        let exact: Vec<usize> = (0..pool.len()).filter(|&k| norm(&pool[k].label) == hint).collect();
        if exact.len() == 1 {
            return Some(exact[0]);
        }
    }
    let said = format!("{hint} {}", norm(&step.target));
    let hits: Vec<(usize, usize)> = pool
        .iter()
        .enumerate()
        .filter_map(|(k, c)| {
            let label = norm(&c.label);
            (!label.is_empty() && contains_as_words(&said, &label)).then(|| (k, crate::py_len(&label)))
        })
        .collect();
    let longest = hits.iter().map(|&(_, n)| n).max()?;
    let best: Vec<usize> = hits.iter().filter(|&&(_, n)| n == longest).map(|&(k, _)| k).collect();
    if best.len() == 1 {
        Some(best[0])
    } else {
        None
    }
}

/// Python `_named_field`: the one field the step names, or None. A label equal to the step's label_hint, else the
/// longest field label that appears as whole words in the hint or the target ("the focused Full name text field"
/// names "Full name"; "the Name for the booking field" names that one, not a field called "Name"). Two fields with
/// the winning label name nothing.
pub fn named_field(step: &PlanStep, pool: &[Candidate]) -> Option<Candidate> {
    named_field_index(step, pool).map(|k| pool[k].clone())
}

/// Python `_pick_field`: (the editable candidate to type into, who picked it, "" | why not). A lone field is
/// code's pick. Among several, code PROPOSES the field the step names (its label, named_field) or, right after the
/// plan clicked a field, the focused one; Jev is asked as on a click and can REFUSE: the proposal is typed into when
/// Jev's top field is the same one. Without a proposal it is Jev's pick under the step cut-offs — the wording was
/// measured on pressable controls, not fields, so a pick that does not clear them types nothing. Eval 2026-09-24
/// (tools/browser_model_eval.py, contact_form): Jev alone never cleared the cut-offs among three labelled fields,
/// even right after the plan had clicked the field, so every model's form fill stopped at ambiguous_field.
pub fn pick_field(
    step: &PlanStep,
    senses: &dyn Senses,
    asker: Option<&dyn StepAsk>,
    gates: &StepGates,
    after_click: bool,
) -> (Option<Candidate>, String, String) {
    let refuse = |how: &str, why: &str| (None, how.to_string(), why.to_string());
    let snap = match senses.snapshot() {
        Ok(snap) => snap,
        Err(e) => return refuse("", &format!("no_window ({})", error_kind(&e))),
    };
    if !snap.dialog_text.is_empty() {
        return refuse("", "dialog_open");
    }
    let pool = editable_pool(&snap, false);
    if pool.is_empty() {
        return refuse("", "no_field");
    }
    if pool.len() == 1 {
        return (Some(pool[0].clone()), "code".to_string(), String::new());
    }
    let mut proposal = named_field_index(step, &pool);
    if proposal.is_none() && after_click {
        if let Some(focused) = &snap.focused {
            proposal = pool.iter().position(|c| c.id == focused.id);
        }
    }
    let Some(asker) = asker else {
        return match proposal {
            Some(k) => (Some(pool[k].clone()), "code".to_string(), String::new()),
            None => refuse("", "ambiguous_field"),
        };
    };
    let by_key: IndexMap<String, usize> = (0..pool.len()).map(|k| ((k + 1).to_string(), k)).collect();
    let options: IndexMap<String, String> =
        by_key.iter().map(|(key, &k)| (key.clone(), eve_decider::render_option(&pool[k]))).collect();
    let answer = asker.ask(&format!("type into {}", step.target), &snap.app, &jev_context(&snap), &options, &[]);
    if !answer.error.is_empty() || answer.target.is_empty() {
        return refuse("jev", "ambiguous_field");
    }
    if answer.risky >= gates.risky_max {
        return refuse("jev", "risky");
    }
    if let Some(k) = proposal {
        if by_key.get(&answer.target) == Some(&k) {
            return (Some(pool[k].clone()), "code+jev".to_string(), String::new());
        }
    }
    let pick = gates.decide(&answer);
    if pick == "confirm" {
        return refuse("jev", "risky");
    }
    match by_key.get(&pick) {
        Some(&k) => (Some(pool[k].clone()), "jev".to_string(), String::new()),
        None => refuse("jev", "ambiguous_field"),
    }
}
