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

use std::collections::HashSet;
use std::sync::LazyLock;

use serde_json::{json, Value};

use crate::ax_candidates::objective_tokens;
use crate::fast_lane::{
    try_run_script, Effectors, LaneStatus, ScriptOptions, ScriptResult, Senses, Thresholds, MAX_SCRIPT_STEPS,
};
use crate::{casefold, error_kind, py_collapse, py_is_space, py_len, py_strip, py_take, Clock};

/// Words a request carries that no control has to account for: what kind of thing to click and where it is. Kept
/// small; every word here is a word the router will NOT ask a label to explain.
pub const GENERIC_WORDS: &[&str] = &[
    "button", "tab", "tabs", "menu", "row", "item", "link", "option", "icon", "pane", "panel", "section", "mode",
    "page", "sidebar", "settings", "setting", "toolbar", "window", "screen", "list",
];
pub const QUESTION_WORDS: &[&str] = &[
    "what",
    "whats",
    "when",
    "where",
    "who",
    "whose",
    "why",
    "how",
    "which",
    "is",
    "are",
    "was",
    "were",
    "does",
    "do",
    "did",
    "tell",
    "read",
    "summarize",
    "summarise",
    "explain",
    "describe",
    "count",
    "list",
];
/// Python: `^(?:hey eve[,\s]*|eve[,\s]*|ok(?:ay)?[,\s]*|please\s+|can you\s+|could you\s+|would you\s+|will you\s+|
/// i want you to\s+|i need you to\s+|go ahead and\s+)+`, IGNORECASE.
pub static POLITE_PREFIX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        r"(?i)^(?:hey eve[,\s]*|eve[,\s]*|ok(?:ay)?[,\s]*|please\s+|can you\s+|could you\s+|",
        r"would you\s+|will you\s+|i want you to\s+|i need you to\s+|go ahead and\s+)+"
    ))
    .expect("POLITE_PREFIX")
});
/// Python: `\s*(?:[,;]|\band then\b|\bafter that\b|\bthen\b|\band\b)\s*`, IGNORECASE.
pub static CLAUSE_SPLIT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\s*(?:[,;]|\band then\b|\bafter that\b|\bthen\b|\band\b)\s*").expect("CLAUSE_SPLIT")
});
/// Python: `reason=([a-z_0-9]+)`.
pub static ESCALATE_REASON: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"reason=([a-z_0-9]+)").expect("ESCALATE_REASON"));
/// Python `re.findall(r"[a-z']+", …)` (is_question's first word).
static QUESTION_WORD: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[a-z']+").expect("QUESTION_WORD"));
/// Python `re.findall(r"[a-z0-9]+", …)` (names_other_app's words).
static NAME_WORD: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[a-z0-9]+").expect("NAME_WORD"));
pub const RETRY_AS_CLAUSES: &[&str] = &["no_match", "ambiguous_target", "uncovered"];
pub const MAX_GOAL_TOKENS: usize = 10;
pub const MAX_CLAUSES: usize = if MAX_SCRIPT_STEPS < 4 { MAX_SCRIPT_STEPS } else { 4 };
/// The planner's own limit for a final sentence (computer_agent rule 10).
pub const MAX_SPOKEN_CHARS: usize = 110;
pub const APP_DIRS: [&str; 4] = [
    "/Applications",
    "/System/Applications",
    "/System/Applications/Utilities",
    "~/Applications",
];
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
        json!({
            "status": self.status,
            "reason": self.reason,
            "line": self.line,
            "clicked": self.clicked,
            "already": self.already,
            "sentence": self.sentence,
            "log": self.log_lines,
            "ms": (self.ms * 10.0).round() / 10.0,
        })
    }
}

/// The goal on one line, without the politeness in front of it.
pub fn normalise(goal: &str) -> String {
    let text = py_collapse(goal);
    py_strip(&POLITE_PREFIX.replace(&text, "")).to_string()
}

/// "year view and then next year" → ["year view", "next year"]. Clauses with no matchable word go.
pub fn split_clauses(goal: &str) -> Vec<String> {
    CLAUSE_SPLIT
        .split(&normalise(goal))
        .map(|part| py_strip(part).to_string())
        .filter(|c| !objective_tokens(c).is_empty())
        .collect()
}

/// A question, not a request: a question word first, or a "?" — unless the request began politely ("can you
/// …?"), where the "?" is manners. One rule for the lane (lane_router) and the classifier (task_router).
pub fn is_question(text: &str, polite: bool) -> bool {
    let folded = casefold(text);
    let first_is_question = QUESTION_WORD
        .find(&folded)
        .is_some_and(|m| QUESTION_WORDS.contains(&m.as_str().replace('\'', "").as_str()));
    first_is_question || (text.contains('?') && !polite)
}

/// "" when the goal may be tried, else question | empty | too_long.
pub fn refuse_reason(goal: &str) -> String {
    let text = normalise(goal);
    let tokens = objective_tokens(&text);
    if tokens.is_empty() {
        return "empty".into();
    }
    if is_question(&text, false) {
        return "question".into();
    }
    if tokens.len() > MAX_GOAL_TOKENS {
        return "too_long".into();
    }
    String::new()
}

/// Case-folded names of the .app bundles in the usual places: a folded view of task_router::installed_apps, the
/// one scanner and the one cache, so the two routers can never disagree about what is installed.
pub fn installed_app_names() -> Vec<String> {
    fold_names(crate::task_router::installed_apps())
}

/// Case-folded and without repeats, in first-seen order (Python's frozenset of folded names).
fn fold_names(names: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let folded = casefold(&n);
        if !out.contains(&folded) {
            out.push(folded);
        }
    }
    out
}

/// installed_app_names over other folders or another `listdir` (uncached, as Python's non-default call).
pub fn installed_app_names_in(dirs: &[&str], listdir: &dyn Fn(&str) -> std::io::Result<Vec<String>>) -> Vec<String> {
    fold_names(crate::task_router::installed_apps_in(dirs, listdir))
}

/// The installed app the goal names, when it is not the frontmost one; "" otherwise. An app is named when every
/// word of its name is a word of the goal ("system settings", "notes"). `apps` are case-folded names.
pub fn names_other_app(goal: &str, frontmost_app: &str, apps: &[String]) -> String {
    let normalised = casefold(&normalise(goal));
    let words: HashSet<&str> = NAME_WORD.find_iter(&normalised).map(|m| m.as_str()).collect();
    let front = py_collapse(&casefold(frontmost_app));
    let mut best = "";
    for name in apps {
        let parts: Vec<&str> = NAME_WORD.find_iter(name).map(|m| m.as_str()).collect();
        if parts.is_empty() || *name == front || (parts.len() == 1 && parts[0].len() < 3) {
            continue;
        }
        if parts.iter().all(|p| words.contains(p)) && py_len(name) > py_len(best) {
            best = name;
        }
    }
    best.to_string()
}

/// What eve says when the lane finished the task: what was clicked, never a guess at more.
pub fn spoken_sentence(clicked: &[String], already: &[String]) -> String {
    fn short(label: &str) -> String {
        let text = py_collapse(label);
        if py_len(&text) <= 32 {
            text
        } else {
            format!("{}…", py_take(&text, 31).trim_end_matches(py_is_space))
        }
    }
    let said = if !clicked.is_empty() {
        let names: Vec<String> = clicked.iter().map(|c| short(c)).collect();
        format!("Done. I clicked {}.", names.join(", then "))
    } else if let Some(last) = already.last() {
        format!("{} is already selected.", short(last))
    } else {
        "Done.".to_string()
    };
    if py_len(&said) <= MAX_SPOKEN_CHARS {
        said
    } else {
        format!(
            "{}…",
            py_take(&said, MAX_SPOKEN_CHARS - 1).trim_end_matches(py_is_space)
        )
    }
}

/// Python `_reason_of`.
pub fn reason_of(script: &ScriptResult) -> String {
    if script.complete() {
        return String::new();
    }
    let Some(last) = script.results.last() else {
        return "unavailable".into();
    };
    match last.status {
        LaneStatus::Escalate => ESCALATE_REASON
            .captures(&last.line)
            .and_then(|c| c.get(1))
            .map_or_else(|| "escalate".to_string(), |m| m.as_str().to_string()),
        // it clicked; nothing confirmed the click did anything
        LaneStatus::Stopped => "unverified".into(),
        // confirm | unavailable (done never ends a script short)
        status => status.as_str().to_string(),
    }
}

/// Python `_from_script`.
pub fn from_script(script: &ScriptResult, ms: f64, log_lines: Vec<String>) -> RouteResult {
    let clicked = script.labels.clone();
    let already = script.already.clone();
    let status = if script.status == "complete" || script.status == "partial" {
        script.status.as_str()
    } else {
        "none"
    };
    RouteResult {
        status: status.to_string(),
        reason: reason_of(script),
        line: script.line.clone(),
        sentence: if status == "complete" {
            spoken_sentence(&clicked, &already)
        } else {
            String::new()
        },
        clicked,
        already,
        log_lines,
        ms,
    }
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
    let clock = opts.clock;
    let t0 = clock();
    let text = normalise(goal);
    let why = refuse_reason(&text);
    if !why.is_empty() {
        return RouteResult {
            reason: why,
            ms: (clock() - t0) * 1000.0,
            ..RouteResult::new("refused")
        };
    }
    let apps = match opts.apps {
        Some(apps) => apps,
        None => installed_app_names(),
    };
    let other = names_other_app(&text, &opts.frontmost_app, &apps);
    if !other.is_empty() {
        return RouteResult {
            reason: format!("names_app:{other}"),
            ms: (clock() - t0) * 1000.0,
            ..RouteResult::new("refused")
        };
    }
    let common = || ScriptOptions {
        decide: "keyword".into(),
        require_cover: true,
        cover_extra: GENERIC_WORDS.iter().map(|w| w.to_string()).collect(),
        approve: None,
        dry_run: opts.dry_run,
        thresholds: opts.thresholds,
        clock,
    };
    // Python raised a failed snapshot out of route, and Helpers.lane_first answered RouteResult("none",
    // reason=f"error:{type(e).__name__}"): the same result, made here.
    let failed = |e: anyhow::Error| RouteResult {
        reason: format!("error:{}", error_kind(&e)),
        ms: (clock() - t0) * 1000.0,
        ..RouteResult::new("none")
    };
    let mut log_lines: Vec<String> = Vec::new();
    let whole = match try_run_script(std::slice::from_ref(&text), senses, effectors, None, common()) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    log_lines.extend(whole.log_lines.iter().cloned());
    let result = from_script(&whole, (clock() - t0) * 1000.0, log_lines.clone());
    if result.status != "none" || !RETRY_AS_CLAUSES.contains(&result.reason.as_str()) {
        return result;
    }
    let clauses = split_clauses(&text);
    if !(2..=MAX_CLAUSES).contains(&clauses.len()) {
        return result;
    }
    let script = match try_run_script(&clauses, senses, effectors, None, common()) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    log_lines.extend(script.log_lines.iter().cloned());
    from_script(&script, (clock() - t0) * 1000.0, log_lines)
}
