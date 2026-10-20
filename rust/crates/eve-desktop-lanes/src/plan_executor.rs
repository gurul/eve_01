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

use serde_json::Value;

use eve_plan_contract::{Expect, Plan, PlanStep};
use eve_typed_ask::{StepGates, JEV_STEP_GATES};

use crate::ax_candidates::Candidate;
use crate::fast_lane::{Effectors, Senses, StepAsk};
use crate::Clock;

pub const STATUSES: [&str; 6] = ["complete", "partial", "none", "needs_human", "checkpoint", "unavailable"];
pub const EXPECT_WAIT_SECS: f64 = 2.5;
pub const EXPECT_POLL_SECS: f64 = 0.4;
pub const MAX_WALL_SECS: f64 = 30.0;
/// Typing composed text into a shell is running a command the human never said: ask before the first key.
pub const SHELL_APPS: &[&str] = &["terminal", "warp", "iterm", "iterm2", "ghostty", "kitty", "alacritty", "wezterm"];

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
        todo!("plan_executor: Entry.to_dict")
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
        todo!("plan_executor: PlanResult.applied")
    }

    /// `{"status", "next_index", "reason", "confirm", "sentence", "ms", "success_rejected", "ledger"}`.
    pub fn to_dict(&self) -> Value {
        todo!("plan_executor: PlanResult.to_dict")
    }
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
    todo!("plan_executor: _holds")
}

/// Python `_confirm_label`: the label inside the lane's `confirm: "{label}" needs the human's yes` line.
pub fn confirm_label(line: &str, fallback: &str) -> String {
    todo!("plan_executor: _confirm_label")
}

/// Python `_reason`: the `reason=` of the lane's escalate line, without what follows it.
pub fn reason(line: &str) -> String {
    todo!("plan_executor: _reason")
}

/// Python `_say`: the closing sentence.
pub fn say(plan: &Plan, ledger: &[Entry]) -> String {
    todo!("plan_executor: _say")
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

/// Walk `plan.steps[start..]`. `approved` maps a 0-based step index the human has said yes to onto what they were
/// asked about (PlanResult.confirm): for a sensitive control that is its label, which the lane lets through
/// exactly once (fast_lane's `approve`).
///
/// `planned_for` is the app whose window the planner was shown. Planning takes seconds and the human keeps using
/// the Mac meanwhile, so when a plan opens no app of its own, that app is brought back to the front before the
/// first step.
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
    todo!("plan_executor: run_plan")
}

/// Python `_grant`: tell an effector with its own sensitive-label gate which label the human's yes covers for THIS
/// click, normalised as they compare it; "" for none.
pub fn grant(effectors: &dyn Effectors, approved: &str) {
    todo!("plan_executor: _grant")
}

/// Python `_clear_consent`: before a click or a type, an effector that can dismiss a cookie/consent notice does,
/// with a button that refuses or closes, never one that agrees. What it pressed goes in the ledger like any applied
/// input. Effectors without the ability (the Mac's) are left alone.
pub fn clear_consent(effectors: &dyn Effectors, i: usize, out: &mut PlanResult) {
    todo!("plan_executor: _clear_consent")
}

/// Python `_norm`: whitespace collapsed, case-folded.
pub fn norm(s: &str) -> String {
    todo!("plan_executor: _norm")
}

/// Python `_named_field`: the one field the step names, or None. A label equal to the step's label_hint, else the
/// longest field label that appears as whole words in the hint or the target. Two fields with the winning label
/// name nothing.
pub fn named_field(step: &PlanStep, pool: &[Candidate]) -> Option<Candidate> {
    todo!("plan_executor: _named_field")
}

/// Python `_pick_field`: (the editable candidate to type into, who picked it, "" | why not). A lone field is
/// code's pick. Among several, code PROPOSES the field the step names or, right after the plan clicked a field, the
/// focused one; Jev is asked as on a click and can REFUSE. Without a proposal it is Jev's pick under the step
/// cut-offs.
pub fn pick_field(
    step: &PlanStep,
    senses: &dyn Senses,
    asker: Option<&dyn StepAsk>,
    gates: &StepGates,
    after_click: bool,
) -> (Option<Candidate>, String, String) {
    todo!("plan_executor: _pick_field")
}
