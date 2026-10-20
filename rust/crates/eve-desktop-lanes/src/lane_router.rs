//! The router: the fast lane before the planner's first turn. Port of `lane_router.py`.
//!
//! A planner turn is 3.5 s (logged median) and a task needs at least two: one to act, one to say so. The paired
//! A/B of 2026-09-21 showed why putting the lane UNDER the planner did not pay: the planner spends a turn to call
//! the helper, so a two-click task went from 13.1 s to 16.9 s. The lane pays when it runs INSTEAD of the planner.
//! That is only safe when the human's own words leave nothing to interpret, and this module is the code that
//! decides that:
//!
//! ```text
//! "switch to week view"            → one control, Week, accounts for every word → click it
//! "year view and then next year"   → two clauses, each fully accounted for     → two clicks
//! "open notes"                     → names another installed app               → planner
//! "find the email from Sam"        → words no control accounts for             → planner
//! "what is on my calendar"         → a question                                → planner
//! ```
//!
//! The rule, all of it code (fast_lane::uncovered_tokens does the word arithmetic):
//!
//! 1. Refuse a question, an empty goal, a goal over MAX_GOAL_TOKENS words, and a goal whose words name an
//!    installed app other than the frontmost one ("open notes" is a launch, not a click on a folder called Notes).
//! 2. Try the whole goal as one step with `require_cover`: the keyword gate must pick exactly one control, and that
//!    control's label and role, the app's name and GENERIC_WORDS must account for every word. "privacy and
//!    security" is one row, so the whole goal is tried before it is split.
//! 3. Only if that escalates with zero input (no match, a tie, uncovered words) and the goal has 2–MAX_CLAUSES
//!    clauses, run the clauses as a script, each under the same rule.
//!
//! Every gate of fast_lane still applies to every step. The router never types and never presses a key.
//!
//! What the caller gets is a RouteResult. `complete` means every clause was done and each click was seen to change
//! something, so the task can end on `sentence` with no planner call. `partial` means something was clicked: the
//! planner starts from there and is told what. `none` and `refused` cost one AX snapshot (0.04–0.43 s measured)
//! and the planner runs as it always did.

use std::sync::LazyLock;

use serde_json::Value;

use crate::fast_lane::{Effectors, ScriptResult, Senses, Thresholds, MAX_SCRIPT_STEPS};
use crate::Clock;

/// Words a request carries that no control has to account for: what kind of thing to click and where it is. Kept
/// small; every word here is a word the router will NOT ask a label to explain.
pub const GENERIC_WORDS: &[&str] = &[
    "button", "tab", "tabs", "menu", "row", "item", "link", "option", "icon", "pane", "panel", "section", "mode",
    "page", "sidebar", "settings", "setting", "toolbar", "window", "screen", "list",
];
pub const QUESTION_WORDS: &[&str] = &[
    "what", "whats", "when", "where", "who", "whose", "why", "how", "which", "is", "are", "was", "were", "does",
    "do", "did", "tell", "read", "summarize", "summarise", "explain", "describe", "count", "list",
];
/// Python: `^(?:hey eve[,\s]*|eve[,\s]*|ok(?:ay)?[,\s]*|please\s+|can you\s+|could you\s+|would you\s+|will you\s+|
/// i want you to\s+|i need you to\s+|go ahead and\s+)+`, IGNORECASE.
pub static POLITE_PREFIX: LazyLock<regex::Regex> = LazyLock::new(|| todo!("lane_router: POLITE_PREFIX"));
/// Python: `\s*(?:[,;]|\band then\b|\bafter that\b|\bthen\b|\band\b)\s*`, IGNORECASE.
pub static CLAUSE_SPLIT: LazyLock<regex::Regex> = LazyLock::new(|| todo!("lane_router: CLAUSE_SPLIT"));
/// Python: `reason=([a-z_0-9]+)`.
pub static ESCALATE_REASON: LazyLock<regex::Regex> = LazyLock::new(|| todo!("lane_router: ESCALATE_REASON"));
pub const RETRY_AS_CLAUSES: &[&str] = &["no_match", "ambiguous_target", "uncovered"];
pub const MAX_GOAL_TOKENS: usize = 10;
pub const MAX_CLAUSES: usize = if MAX_SCRIPT_STEPS < 4 { MAX_SCRIPT_STEPS } else { 4 };
/// The planner's own limit for a final sentence (computer_agent rule 10).
pub const MAX_SPOKEN_CHARS: usize = 110;
pub const APP_DIRS: [&str; 4] =
    ["/Applications", "/System/Applications", "/System/Applications/Utilities", "~/Applications"];
pub const STATUSES: [&str; 4] = ["complete", "partial", "none", "refused"];

#[derive(Debug, Clone, PartialEq)]
pub struct RouteResult {
    /// complete | partial | none | refused
    pub status: String,
    /// why it is not complete, in one token
    pub reason: String,
    /// the script's own line, "" when refused
    pub line: String,
    /// labels clicked, in order
    pub clicked: Vec<String>,
    /// labels found already selected
    pub already: Vec<String>,
    /// what to say when complete
    pub sentence: String,
    pub log_lines: Vec<String>,
    pub ms: f64,
}

impl RouteResult {
    /// `RouteResult(status)` with every other field at its default.
    pub fn new(status: &str) -> Self {
        RouteResult {
            status: status.to_string(),
            reason: String::new(),
            line: String::new(),
            clicked: Vec::new(),
            already: Vec::new(),
            sentence: String::new(),
            log_lines: Vec::new(),
            ms: 0.0,
        }
    }

    /// `{"status", "reason", "line", "clicked", "already", "sentence", "log", "ms"}` (ms rounded to 0.1).
    pub fn to_dict(&self) -> Value {
        todo!("lane_router: RouteResult.to_dict")
    }
}

/// The goal on one line, without the politeness in front of it.
pub fn normalise(goal: &str) -> String {
    todo!("lane_router: normalise")
}

/// "year view and then next year" → ["year view", "next year"]. Clauses with no matchable word go.
pub fn split_clauses(goal: &str) -> Vec<String> {
    todo!("lane_router: split_clauses")
}

/// A question, not a request: a question word first, or a "?" — unless the request began politely ("can you
/// …?"), where the "?" is manners. One rule for the lane (lane_router) and the classifier (task_router).
pub fn is_question(text: &str, polite: bool) -> bool {
    todo!("lane_router: is_question")
}

/// "" when the goal may be tried, else question | empty | too_long.
pub fn refuse_reason(goal: &str) -> String {
    todo!("lane_router: refuse_reason")
}

/// Case-folded names of the .app bundles in the usual places: a folded view of task_router::installed_apps, the
/// one scanner and the one cache, so the two routers can never disagree about what is installed.
pub fn installed_app_names() -> Vec<String> {
    todo!("lane_router: installed_app_names")
}

/// installed_app_names over other folders or another `listdir` (uncached, as Python's non-default call).
pub fn installed_app_names_in(
    dirs: &[&str],
    listdir: &dyn Fn(&str) -> std::io::Result<Vec<String>>,
) -> Vec<String> {
    todo!("lane_router: installed_app_names (dirs, listdir)")
}

/// The installed app the goal names, when it is not the frontmost one; "" otherwise. An app is named when every
/// word of its name is a word of the goal ("system settings", "notes"). `apps` are case-folded names.
pub fn names_other_app(goal: &str, frontmost_app: &str, apps: &[String]) -> String {
    todo!("lane_router: names_other_app")
}

/// What eve says when the lane finished the task: what was clicked, never a guess at more.
pub fn spoken_sentence(clicked: &[String], already: &[String]) -> String {
    todo!("lane_router: spoken_sentence")
}

/// Python `_reason_of`.
pub fn reason_of(script: &ScriptResult) -> String {
    todo!("lane_router: _reason_of")
}

/// Python `_from_script`.
pub fn from_script(script: &ScriptResult, ms: f64, log_lines: Vec<String>) -> RouteResult {
    todo!("lane_router: _from_script")
}

/// route's keyword arguments, with Python's defaults (`Default`). `apps` None scans installed_app_names().
pub struct RouteOptions<'a> {
    pub frontmost_app: String,
    pub apps: Option<Vec<String>>,
    pub dry_run: bool,
    pub thresholds: Thresholds,
    pub clock: Clock<'a>,
}

impl Default for RouteOptions<'_> {
    fn default() -> Self {
        RouteOptions {
            frontmost_app: String::new(),
            apps: None,
            dry_run: false,
            thresholds: Thresholds::default(),
            clock: &crate::perf_counter,
        }
    }
}

/// Try to finish `goal` in the lane, before any planner call (the module docs have the rule).
pub fn route(goal: &str, senses: &dyn Senses, effectors: &dyn Effectors, opts: RouteOptions<'_>) -> RouteResult {
    todo!("lane_router: route")
}
