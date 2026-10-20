//! The chief of staff's record: the Card that holds every job the owner hands eve and every commitment they make.
//! Port of `chief_card.py`.
//!
//! The owner asked for a chief of staff (2026-09-29): work with more than one step is never delegated without a
//! card, and every card starts with eve's backbrief ("I intend to …; done when …; up to $X"). eve writes the
//! brief; the owner only corrects it. The idea is Marquet's "I intend to" (Turn the Ship Around!, 2012) and the
//! army's backbrief (FM 6-0, 2022, "Backbrief"): the one who will do the work says it back before starting.
//!
//! A card is one of two kinds ([`KINDS`]):
//!
//! * `job`         eve does the phases: research, assess, act, watch ([`PHASE_KINDS_LIVE`]).
//! * `commitment`  the owner acts; eve holds the cue ("remind me Thursday to …") and has no phases.
//!
//! What code decides here, never a model:
//!
//! * **The parse is strict and whole.** [`parse`] takes the brain's `take_on` arguments and returns a Card or a
//!   [`Refusal`] naming the first problem. One bad phase, one unknown check, one date that is not a date refuses
//!   the whole card, so a card is never partly made (plan_contract.parse_plan does the same for a plan). Text
//!   fields are cut to their caps, as plan_contract's `_text` does.
//! * **The door is a floor in code and add-only.** A phase is `one_way` when task_router's `CONSEQUENTIAL`
//!   wording matches its goal, when a Composio call in it has consequential slugs
//!   (composio_tools.consequential_slugs), or when its executor cannot stop and ask. Current act executors can
//!   stop and ask through the live runtime callback, so routine acts have no executor-wide one-way floor.
//!   Dangerous goals and model-raised doors still require Go. Loading a stored phase reapplies the goal floor and
//!   preserves any higher stored door (add-only). The brain may raise a door to `one_way`; it can never lower
//!   one, and a revision keeps the higher door of each phase (plan_contract's add-only `consequential`).
//! * **`grounded`** says whether the owner's own words carry the consequential verb of a one-way goal. A Go
//!   shows it when it is false: the verb came from eve's plan, not from the owner.
//! * **The words stay in the transcript.** A card keeps `source.said_ref` (the transcript day and time), never a
//!   copy of what was said, so forget reaches them where they live (memory.py).
//! * **`rev` counts changes to what the owner agreed to**: the take_on fields. An approval is bound to (card,
//!   phase, rev) and a new rev cancels it. Progress (status, a phase's status and result, what was spent, the
//!   nudges) is state: it is logged in the ledger's events and does not bump rev, so a yes is never cancelled by
//!   its own dispatch. (The design said "every change is rev+1"; that would void each Go at the moment it is
//!   used.)
//!
//! Only live names are in these sets: a phase kind or a check that is not built yet is not named anywhere, not
//! even as a refusal (the owner's rule for prompts and tool enums, 2026-09-09). [`Live`] lets a caller narrow the
//! sets it offers; [`parse`] refuses anything outside the set it was given.
//!
//! The result every executor returns ([`PhaseResult`]) is defined here, beside [`Phase`], because a card carries
//! its phases' results; chief_receipt re-exports it and holds the oracles that read it.
//!
//! Python's `Card | Refusal` is `Result<Card, ParseError>` here: [`ParseError::Refusal`] is the Refusal, and
//! [`ParseError::ValueError`] is the one exception Python's parse lets out (a done check's `int()` or `lstat` on
//! an argument it cannot read), so a caller can still tell the two apart.

mod py;

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use chrono::{Duration, NaiveDate, NaiveDateTime, Timelike};
use regex::Regex;
use serde_json::{json, Map, Value};

use eve_desktop_lanes::task_router::CONSEQUENTIAL;

pub const KINDS: &[&str] = &["job", "commitment"];
pub const STATUSES: &[&str] = &["proposed", "active", "waiting", "unverified", "done", "dropped"];
pub const TERMINAL: &[&str] = &["done", "dropped"];
pub const WAITING_FOR: &[&str] = &["question", "go", "budget", "mac_slot"];
pub const PHASE_KINDS_LIVE: &[&str] = &["research", "assess", "act", "watch"];
pub const PHASE_STATUSES: &[&str] = &["queued", "running", "ok", "failed", "refused", "skipped"];
pub const DOORS: &[&str] = &["two_way", "one_way"];
/// Every current executor can ask: Mac acts are forced through Codex with the chief's live owner callback.
/// Keep the floor for any future executor that cannot ask; routine acts no longer need a preliminary Go.
pub const ACT_CANNOT_ASK: &[&str] = &[];
pub const DONE_KINDS_LIVE: &[&str] =
    &["read_links", "cited_pick", "ui_seen", "watch_armed", "file_exists", "guru_says_done"];
pub const RESULT_STATUSES: &[&str] = &["done", "handed_on", "needs_guru", "refused", "failed", "over_budget"];
pub const EVIDENCE_KINDS: &[&str] = &["links", "ui_text", "watch_row", "file", "owner"];
pub const NUDGE_LEVELS: &[&str] = &["push", "sheet"];

/// The take_on arguments, all of them: anything else refuses the card.
pub const TAKE_ON_KEYS: &[&str] =
    &["kind", "title", "purpose", "end_state", "done_checks", "never", "phases", "deadline", "cue", "firm", "question"];
pub const PHASE_KEYS: &[&str] = &["do", "goal", "door"];
pub const CHECK_KEYS: &[&str] = &["kind", "arg"];
/// A check that needs a phase of this kind on the card; the rest (file_exists, guru_says_done) need none.
pub const CHECK_NEEDS_PHASE: &[(&str, &str)] =
    &[("read_links", "research"), ("cited_pick", "assess"), ("ui_seen", "act"), ("watch_armed", "watch")];
/// The owner's own checks: what a commitment may be closed on.
pub const COMMITMENT_CHECKS: &[&str] = &["file_exists", "guru_says_done"];

pub const MAX_PHASES: usize = 6;
pub const MAX_NEVER: usize = 5;
pub const MAX_CHECKS: usize = 4;
pub const MAX_TITLE_CHARS: usize = 40;
/// purpose, end_state, a goal, a next action
pub const MAX_LINE_CHARS: usize = 160;
pub const MAX_NEVER_CHARS: usize = 80;
pub const MAX_QUESTION_CHARS: usize = 200;
pub const MAX_UI_TEXT_CHARS: usize = 80;
/// ui_seen: one word ("order") is on too many screens to prove anything
pub const MIN_UI_WORDS: usize = 2;
pub const MAX_PATH_CHARS: usize = 240;
pub const MAX_READ_LINKS: u64 = 20;
pub const MAX_EVIDENCE: usize = 40;
pub const MAX_EVIDENCE_REF_CHARS: usize = 2000;
pub const MAX_RESULT_TEXT_CHARS: usize = 2000;

pub const DEADLINE_FORMATS: &[&str] = &["%Y-%m-%d %H:%M", "%Y-%m-%d"];
pub const CUE_FORMAT: &str = "%Y-%m-%d %H:%M";

// The owner's defaults (2026-09-29): a card that only reads and weighs gets $0.50 and 20 minutes; one with an act
// gets $1 and 30 minutes. A raise is one tap; the warning line goes out at 80%.
pub const DEFAULT_BUDGET_READ_USD: f64 = 0.50;
pub const DEFAULT_BUDGET_READ_MIN: i64 = 20;
pub const DEFAULT_BUDGET_ACT_USD: f64 = 1.00;
pub const DEFAULT_BUDGET_ACT_MIN: i64 = 30;
pub const BUDGET_WARN_AT: f64 = 0.8;

/// An asked reminder waits this long for a breakpoint, then is sent anyway (chief_desk.REMINDER_WAIT_SECS reads
/// it; the backbrief promises it).
pub const REMINDER_WAIT_MIN: i64 = 45;

// Python's `$` (no MULTILINE) also matches before a final "\n", hence the `\n?`.
static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^c([1-9]\d{0,8})\n?$").expect("_ID"));
static WATCH_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^w[1-9]\d{0,8}\n?$").expect("_WATCH_ID"));

// ---- errors ------------------------------------------------------------------------------------

/// A stored card that does not load. The ledger skips it and says so; it never half-loads one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CardError(pub String);

/// Why a take_on or a revision was refused: one line, for the brain to fix and for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub reason: String,
}

impl Refusal {
    pub fn new(reason: impl Into<String>) -> Self {
        Refusal { reason: reason.into() }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

/// Python's ValueError, raised where chief_card.py lets one out: `int()` on a digit string that is not decimal
/// (a superscript) or too long, and `lstat` on a path with a NUL in it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ValueError(pub String);

/// What `parse` and `revise` return instead of a Card.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The Python `Refusal`: the reason goes back to the brain.
    #[error("{0}")]
    Refusal(Refusal),
    /// The exception Python's parse raises rather than returns ([`ValueError`]).
    #[error("{0}")]
    ValueError(ValueError),
}

impl ParseError {
    /// The Refusal, when it is one.
    pub fn refusal(&self) -> Option<&Refusal> {
        match self {
            ParseError::Refusal(r) => Some(r),
            ParseError::ValueError(_) => None,
        }
    }
}

impl From<Refusal> for ParseError {
    fn from(r: Refusal) -> Self {
        ParseError::Refusal(r)
    }
}

impl From<ValueError> for ParseError {
    fn from(e: ValueError) -> Self {
        ParseError::ValueError(e)
    }
}

fn refused(reason: impl Into<String>) -> ParseError {
    ParseError::Refusal(Refusal::new(reason))
}

// ---- the record --------------------------------------------------------------------------------

/// The positive sets a caller offers. `parse` refuses anything outside them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    pub phases: Vec<String>,
    pub checks: Vec<String>,
}

impl Live {
    pub fn new(phases: &[&str], checks: &[&str]) -> Self {
        Live {
            phases: phases.iter().map(|s| s.to_string()).collect(),
            checks: checks.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn has_phase(&self, v: Option<&Value>) -> bool {
        matches!(v, Some(Value::String(s)) if self.phases.iter().any(|p| p == s))
    }

    fn has_check(&self, v: Option<&Value>) -> bool {
        matches!(v, Some(Value::String(s)) if self.checks.iter().any(|p| p == s))
    }
}

impl Default for Live {
    fn default() -> Self {
        Live::new(PHASE_KINDS_LIVE, DONE_KINDS_LIVE)
    }
}

pub static LIVE: LazyLock<Live> = LazyLock::new(Live::default);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// DONE_KINDS_LIVE
    pub kind: String,
    /// read_links: the count; ui_seen: the text; file_exists: the path; else ""
    pub arg: String,
}

impl Check {
    pub fn new(kind: &str, arg: &str) -> Self {
        Check { kind: kind.to_string(), arg: arg.to_string() }
    }

    pub fn describe(&self) -> String {
        match self.kind.as_str() {
            "read_links" => format!("read {} or more pages", self.arg),
            "cited_pick" => "the pick cites a page I read".to_string(),
            "ui_seen" => format!("'{}' seen on screen", self.arg),
            "watch_armed" => "the watch is set".to_string(),
            "file_exists" => format!("{} exists", self.arg),
            _ => "you say it is done".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// EVIDENCE_KINDS
    pub kind: String,
    /// a URL, the UI-state text, a watch id, a path, "done"
    pub r#ref: String,
}

impl Evidence {
    pub fn new(kind: &str, r#ref: &str) -> Self {
        Evidence { kind: kind.to_string(), r#ref: r#ref.to_string() }
    }
}

/// The one shape every executor adapter returns. `text` is the executor's own words: data, never evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseResult {
    /// RESULT_STATUSES
    pub status: String,
    pub evidence: Vec<Evidence>,
    pub text: String,
    pub usd: f64,
    pub secs: f64,
}

impl PhaseResult {
    /// `PhaseResult(status, evidence)` with Python's defaults for the rest.
    pub fn new(status: &str, evidence: Vec<Evidence>) -> Self {
        PhaseResult { status: status.to_string(), evidence, text: String::new(), usd: 0.0, secs: 0.0 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    /// single use, bound to (card, n, rev)
    pub token: String,
    pub rev: i64,
    pub at: f64,
    /// a digest of the act as the Go showed it (chief.act_digest); "" matches nothing
    pub what: String,
}

impl Approval {
    pub fn new(token: &str, rev: i64, at: f64) -> Self {
        Approval { token: token.to_string(), rev, at, what: String::new() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Phase {
    /// 1-based
    pub n: usize,
    /// PHASE_KINDS_LIVE
    pub r#do: String,
    pub goal: String,
    /// DOORS: a code floor, add-only
    pub door: String,
    /// a one-way goal's verb is in the owner's words
    pub grounded: bool,
    /// set by code at dispatch
    pub executor: String,
    /// PHASE_STATUSES
    pub status: String,
    pub approval: Option<Approval>,
    pub result: Option<PhaseResult>,
}

impl Phase {
    /// `Phase(n, do, goal)` with Python's defaults for the rest.
    pub fn new(n: usize, r#do: &str, goal: &str) -> Self {
        Phase {
            n,
            r#do: r#do.to_string(),
            goal: goal.to_string(),
            door: "two_way".to_string(),
            grounded: true,
            executor: String::new(),
            status: "queued".to_string(),
            approval: None,
            result: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    /// "YYYY-MM-DD HH:MM" or None
    pub at: Option<String>,
    pub next_action: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    pub usd: f64,
    pub wall_min: i64,
}

impl Default for Budget {
    fn default() -> Self {
        Budget { usd: DEFAULT_BUDGET_READ_USD, wall_min: DEFAULT_BUDGET_READ_MIN }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Spent {
    pub usd: f64,
    pub wall_s: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// where the owner said it
    pub door: String,
    /// transcript day + time; never the words
    pub said_ref: String,
}

impl Default for Source {
    fn default() -> Self {
        Source { door: "telegram".to_string(), said_ref: String::new() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Nudges {
    pub sent: i64,
    pub ignored: i64,
    pub next_at: Option<f64>,
    /// NUDGE_LEVELS: after 3 ignored nudges, only the weekly sheet
    pub level: String,
}

impl Default for Nudges {
    fn default() -> Self {
        Nudges { sent: 0, ignored: 0, next_at: None, level: "push".to_string() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// "c<N>", never reused; "" until the ledger files it
    pub id: String,
    pub rev: i64,
    pub kind: String,
    pub title: String,
    pub purpose: String,
    pub end_state: String,
    pub done_checks: Vec<Check>,
    pub never: Vec<String>,
    pub phases: Vec<Phase>,
    pub deadline: Option<String>,
    pub cue: Option<Cue>,
    pub firm: bool,
    pub question: String,
    pub budget: Budget,
    pub spent: Spent,
    pub status: String,
    pub waiting_for: Option<String>,
    pub source: Source,
    pub nudges: Nudges,
    pub created: f64,
    pub updated: f64,
}

impl Card {
    /// `Card(id, rev, kind, title, purpose, end_state, done_checks)` with Python's defaults for the rest.
    pub fn new(id: &str, rev: i64, kind: &str, title: &str, purpose: &str, end_state: &str, done_checks: Vec<Check>) -> Self {
        Card {
            id: id.to_string(),
            rev,
            kind: kind.to_string(),
            title: title.to_string(),
            purpose: purpose.to_string(),
            end_state: end_state.to_string(),
            done_checks,
            never: Vec::new(),
            phases: Vec::new(),
            deadline: None,
            cue: None,
            firm: false,
            question: String::new(),
            budget: Budget::default(),
            spent: Spent::default(),
            status: "active".to_string(),
            waiting_for: None,
            source: Source::default(),
            nudges: Nudges::default(),
            created: 0.0,
            updated: 0.0,
        }
    }

    pub fn terminal(&self) -> bool {
        TERMINAL.contains(&self.status.as_str())
    }

    pub fn phase(&self, n: usize) -> Option<&Phase> {
        self.phases.iter().find(|p| p.n == n)
    }
}

// ---- the door ----------------------------------------------------------------------------------

/// The consequential words in a text (task_router.CONSEQUENTIAL), folded.
pub fn verbs(text: &str) -> BTreeSet<String> {
    CONSEQUENTIAL.find_iter(text).map(|m| py::squash(&py::casefold(m.as_str()))).collect()
}

/// The code floor for one phase. `raised`: the brain (or, later, a critic) asked for one_way; it can only
/// raise. `slugs`: composio_tools.consequential_slugs for a call the phase will make. `can_ask`: false when
/// the executor cannot stop and ask the owner.
pub fn door<S: AsRef<str>>(goal: &str, raised: bool, slugs: &[S], can_ask: bool) -> &'static str {
    if raised || !verbs(goal).is_empty() || slugs.iter().any(|s| !py::strip(s.as_ref()).is_empty()) || !can_ask {
        return "one_way";
    }
    "two_way"
}

/// `door(goal)` with Python's defaults: not raised, no slugs, an executor that can ask.
pub fn goal_door(goal: &str) -> &'static str {
    door::<&str>(goal, false, &[], true)
}

pub fn higher(a: &str, b: &str) -> &'static str {
    if a == "one_way" || b == "one_way" {
        "one_way"
    } else {
        "two_way"
    }
}

/// A one-way goal is grounded when every consequential word in it is also in the owner's words. A goal with
/// no such word (raised by the brain) is not grounded: nothing the owner said names the act.
pub fn grounded(goal: &str, said: &str) -> bool {
    let words = verbs(goal);
    !words.is_empty() && words.is_subset(&verbs(said))
}

// ---- the parse ---------------------------------------------------------------------------------

/// `" ".join(value.split())[:limit]` for a string, "" for anything else.
fn line(value: Option<&Value>, limit: usize) -> String {
    match value {
        Some(Value::String(s)) => py::take_chars(&py::squash(s), limit).to_string(),
        _ => String::new(),
    }
}

fn need(value: Option<&Value>, limit: usize, what: &str) -> Result<String, ParseError> {
    let text = line(value, limit);
    if text.is_empty() {
        return Err(refused(format!("{what} is empty")));
    }
    Ok(text)
}

/// The patterns CPython 3.13's `_strptime` builds for the two formats (`\s+` is str.isspace, which counts the
/// four ASCII separators too), matched at the start as `re.match` does.
static STRPTIME_MINUTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?P<Y>\d\d\d\d)-(?P<m>1[0-2]|0[1-9]|[1-9])-(?P<d>3[0-1]|[1-2]\d|0[1-9]|[1-9]| [1-9])",
        r"[\s\x1c-\x1f]+(?P<H>2[0-3]|[0-1]\d|\d| \d):(?P<M>[0-5]\d|\d)"
    ))
    .expect("strptime %Y-%m-%d %H:%M")
});
static STRPTIME_DAY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<Y>\d\d\d\d)-(?P<m>1[0-2]|0[1-9]|[1-9])-(?P<d>3[0-1]|[1-2]\d|0[1-9]|[1-9]| [1-9])")
        .expect("strptime %Y-%m-%d")
});

/// `datetime.strptime(text, fmt)` for the two formats here; None where Python raises ValueError (no match,
/// unconverted data left, a day the month does not have, year 0).
fn strptime(text: &str, fmt: &str) -> Option<NaiveDateTime> {
    let pattern = match fmt {
        "%Y-%m-%d %H:%M" => &*STRPTIME_MINUTE,
        "%Y-%m-%d" => &*STRPTIME_DAY,
        _ => return None,
    };
    let m = pattern.captures(text)?;
    if m.get(0)?.end() != text.len() {
        return None; // "unconverted data remains"
    }
    let num = |name: &str| m.name(name).map(|g| py::digits_value(g.as_str()));
    let year = num("Y")?;
    if year < 1 {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(year as i32, num("m")? as u32, num("d")? as u32)?;
    let (hour, minute) = if fmt == "%Y-%m-%d" { (0, 0) } else { (num("H")? as u32, num("M")? as u32) };
    date.and_hms_opt(hour, minute, 0)
}

/// `when.strftime(fmt)` for the two formats here (the year zero-padded to four, as CPython on macOS writes it).
fn strftime(when: &NaiveDateTime, fmt: &str) -> String {
    use chrono::Datelike;
    let day = format!("{:04}-{:02}-{:02}", when.year(), when.month(), when.day());
    if fmt == "%Y-%m-%d" {
        day
    } else {
        format!("{day} {:02}:{:02}", when.hour(), when.minute())
    }
}

fn when(raw: Option<&Value>, formats: &[&str], what: &str) -> Result<Option<String>, ParseError> {
    let raw = match raw {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) if s.is_empty() => return Ok(None),
        Some(Value::String(s)) => s,
        Some(_) => return Err(refused(format!("{what} must be a string or null"))),
    };
    let text = py::squash(&raw.replace('T', " "));
    for fmt in formats {
        if let Some(at) = strptime(&text, fmt) {
            return Ok(Some(strftime(&at, fmt)));
        }
    }
    Err(refused(format!("{what} {} is not a date in the form {}", py::repr_str(raw), formats.join(" or "))))
}

/// A deadline or cue string as a local datetime; a date alone is the end of that day.
pub fn at_time(stamp: Option<&str>) -> Option<NaiveDateTime> {
    let stamp = stamp.filter(|s| !s.is_empty())?;
    for fmt in DEADLINE_FORMATS {
        if let Some(at) = strptime(stamp, fmt) {
            return if *fmt == "%Y-%m-%d" { at.date().and_hms_opt(23, 59, 0) } else { Some(at) };
        }
    }
    None
}

/// A path below the home folder, resolved; None for anything outside it (`..`, another root, a symlink out)
/// and for the home folder itself, which always exists and so proves nothing (reviewer, 2026-09-29). `home`
/// None is the account's home folder (`Path.home()`). Err is Python's ValueError for a NUL in the path.
pub fn home_path(raw: &str, home: Option<&Path>) -> Result<Option<PathBuf>, ValueError> {
    let base_raw = match home {
        Some(h) => h.to_string_lossy().into_owned(),
        None => py::home_dir(),
    };
    let base = py::realpath(&base_raw).map_err(ValueError)?;
    let text = py::strip(raw);
    if text.is_empty() {
        return Ok(None);
    }
    let path = if let Some(rest) = text.strip_prefix('~') { format!("{base}{rest}") } else { text.to_string() };
    let path = if path.starts_with('/') { path } else { format!("{base}/{path}") };
    let resolved = py::realpath(&path).map_err(ValueError)?;
    // `base in resolved.parents`: a proper ancestor, lexically
    let below = if base == "/" {
        resolved != "/"
    } else {
        resolved.strip_prefix(base.as_str()).is_some_and(|tail| tail.starts_with('/'))
    };
    Ok(below.then(|| PathBuf::from(resolved)))
}

fn object<'a>(raw: &'a Value, where_: &str) -> Result<&'a Map<String, Value>, ParseError> {
    raw.as_object().ok_or_else(|| refused(format!("{where_}: not an object")))
}

/// `sorted(set(raw) - set(keys))[0]`, the first unknown key.
fn first_extra<'a>(raw: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a String> {
    raw.keys().filter(|k| !keys.contains(&k.as_str())).min()
}

fn check(raw: &Value, index: usize, live: &Live, home: Option<&Path>) -> Result<Check, ParseError> {
    let where_ = format!("done check {index}");
    let raw = object(raw, &where_)?;
    if let Some(extra) = first_extra(raw, CHECK_KEYS) {
        return Err(refused(format!("{where_}: unknown field {}", py::repr_str(extra))));
    }
    let kind = raw.get("kind");
    if !live.has_check(kind) {
        return Err(refused(format!(
            "{where_}: kind must be one of {}, not {}",
            py::tuple_repr(&live.checks),
            py::repr(kind)
        )));
    }
    let kind = kind.and_then(Value::as_str).unwrap_or_default();
    let arg = raw.get("arg");
    match kind {
        "read_links" => {
            let n = match arg {
                Some(Value::Number(n)) => n.as_u64().or_else(|| n.as_i64().map(|_| 0)).unwrap_or(0),
                Some(Value::String(s)) if py::isdigit(py::strip(s)) => py::int_of_digits(s).map_err(ValueError)?,
                _ => 0,
            };
            if !(1..=MAX_READ_LINKS).contains(&n) {
                return Err(refused(format!("{where_}: read_links needs a count from 1 to {MAX_READ_LINKS}")));
            }
            Ok(Check::new("read_links", &n.to_string()))
        }
        "ui_seen" => {
            let text = need(arg, MAX_UI_TEXT_CHARS, &format!("{where_}: ui_seen text"))?;
            if py::split(&text).count() < MIN_UI_WORDS {
                return Err(refused(format!(
                    "{where_}: ui_seen needs at least two words that will be on the screen when it worked"
                )));
            }
            Ok(Check::new("ui_seen", &text))
        }
        "file_exists" => {
            let text = need(arg, MAX_PATH_CHARS, &format!("{where_}: file_exists path"))?;
            if home_path(&text, home)?.is_none() {
                return Err(refused(format!("{where_}: file_exists takes the path of a file in the home folder")));
            }
            Ok(Check::new("file_exists", &text))
        }
        other => Ok(Check::new(other, "")),
    }
}

fn phase(raw: &Value, n: usize, live: &Live, said: &str) -> Result<Phase, ParseError> {
    let where_ = format!("phase {n}");
    let raw = object(raw, &where_)?;
    if let Some(extra) = first_extra(raw, PHASE_KEYS) {
        return Err(refused(format!("{where_}: unknown field {}", py::repr_str(extra))));
    }
    let r#do = raw.get("do");
    if !live.has_phase(r#do) {
        return Err(refused(format!(
            "{where_}: do must be one of {}, not {}",
            py::tuple_repr(&live.phases),
            py::repr(r#do)
        )));
    }
    let r#do = r#do.and_then(Value::as_str).unwrap_or_default();
    let goal = need(raw.get("goal"), MAX_LINE_CHARS, &format!("{where_}: goal"))?;
    let asked = raw.get("door");
    let asked_ok = match asked {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty() || DOORS.contains(&s.as_str()),
        Some(_) => false,
    };
    if !asked_ok {
        return Err(refused(format!(
            "{where_}: door must be one of {} or null, not {}",
            py::tuple_repr(DOORS),
            py::repr(asked)
        )));
    }
    let raised = asked.and_then(Value::as_str) == Some("one_way");
    let d = door::<&str>(&goal, raised, &[], !ACT_CANNOT_ASK.contains(&r#do));
    // grounded flags a consequential word that came from eve's plan; an act one-way only by its executor, with
    // no such word and not raised, has nothing to flag (it is the owner's step, as it was when it was two-way)
    let g = d == "two_way" || (!raised && verbs(&goal).is_empty()) || grounded(&goal, said);
    Ok(Phase { door: d.to_string(), grounded: g, ..Phase::new(n, r#do, &goal) })
}

pub fn default_budget(phases: &[Phase]) -> Budget {
    if phases.iter().any(|p| p.r#do == "act") {
        return Budget { usd: DEFAULT_BUDGET_ACT_USD, wall_min: DEFAULT_BUDGET_ACT_MIN };
    }
    Budget { usd: DEFAULT_BUDGET_READ_USD, wall_min: DEFAULT_BUDGET_READ_MIN }
}

/// The take_on arguments as Card fields.
struct Fields {
    kind: String,
    title: String,
    purpose: String,
    end_state: String,
    done_checks: Vec<Check>,
    never: Vec<String>,
    phases: Vec<Phase>,
    deadline: Option<String>,
    cue: Option<Cue>,
    firm: bool,
    question: String,
}

/// `x if x is not None else []` for a list argument; Err when it is there and not a list.
fn list<'a>(v: Option<&'a Value>, what: &str) -> Result<&'a [Value], ParseError> {
    match v {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(a)) => Ok(a),
        Some(_) => Err(refused(format!("{what} must be a list"))),
    }
}

fn fields(args: &Value, said: &str, live: &Live, home: Option<&Path>) -> Result<Fields, ParseError> {
    let args = args.as_object().ok_or_else(|| refused("take_on arguments must be an object"))?;
    if let Some(extra) = first_extra(args, TAKE_ON_KEYS) {
        return Err(refused(format!("unknown field {}", py::repr_str(extra))));
    }
    let kind = match args.get("kind") {
        Some(Value::String(k)) if KINDS.contains(&k.as_str()) => k.clone(),
        other => {
            return Err(refused(format!("kind must be one of {}, not {}", py::tuple_repr(KINDS), py::repr(other))))
        }
    };
    let title = need(args.get("title"), MAX_TITLE_CHARS, "title")?;
    let raw_phases = list(args.get("phases"), "phases")?;
    if kind == "commitment" && !raw_phases.is_empty() {
        return Err(refused("a commitment has no phases: the owner does it"));
    }
    if kind == "job" && raw_phases.is_empty() {
        return Err(refused("a job needs at least one phase"));
    }
    if raw_phases.len() > MAX_PHASES {
        return Err(refused(format!("at most {MAX_PHASES} phases")));
    }
    let phases = raw_phases
        .iter()
        .enumerate()
        .map(|(i, p)| phase(p, i + 1, live, said))
        .collect::<Result<Vec<_>, _>>()?;
    let raw_checks = list(args.get("done_checks"), "done_checks")?;
    if raw_checks.len() > MAX_CHECKS {
        return Err(refused(format!("at most {MAX_CHECKS} done checks")));
    }
    let mut checks = raw_checks
        .iter()
        .enumerate()
        .map(|(i, c)| check(c, i + 1, live, home))
        .collect::<Result<Vec<_>, _>>()?;
    if checks.is_empty() {
        if !live.checks.iter().any(|c| c == "guru_says_done") {
            return Err(refused("a card needs at least one done check"));
        }
        checks = vec![Check::new("guru_says_done", "")];
    }
    for c in &checks {
        if kind == "commitment" && !COMMITMENT_CHECKS.contains(&c.kind.as_str()) {
            let offered: Vec<&str> =
                COMMITMENT_CHECKS.iter().copied().filter(|k| live.checks.iter().any(|c| c == k)).collect();
            return Err(refused(format!("a commitment is closed only by {}", py::tuple_repr(&offered))));
        }
        if let Some(&(_, need_)) = CHECK_NEEDS_PHASE.iter().find(|(k, _)| *k == c.kind) {
            if !phases.iter().any(|p| p.r#do == need_) {
                return Err(refused(format!("the {} check needs a {need_} phase", c.kind)));
            }
        }
    }
    let raw_never = match args.get("never") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(a)) if a.iter().all(Value::is_string) => a.as_slice(),
        Some(_) => return Err(refused("never must be a list of strings")),
    };
    let never: Vec<String> = raw_never
        .iter()
        .map(|x| line(Some(x), MAX_NEVER_CHARS))
        .filter(|t| !t.is_empty())
        .take(MAX_NEVER)
        .collect();
    let mut cue = None;
    match args.get("cue") {
        None | Some(Value::Null) => {}
        Some(cue_raw) => {
            if kind == "job" {
                return Err(refused("a job has no cue: its phases are its next actions"));
            }
            let cue_obj = match cue_raw.as_object() {
                Some(o) if first_extra(o, &["at", "next_action"]).is_none() => o,
                _ => return Err(refused("cue must be an object with at and next_action")),
            };
            let at = when(cue_obj.get("at"), &[CUE_FORMAT], "cue.at")?;
            let next_action = need(cue_obj.get("next_action"), MAX_LINE_CHARS, "cue.next_action")?;
            cue = Some(Cue { at, next_action });
        }
    }
    if kind == "commitment" && cue.is_none() {
        return Err(refused("a commitment needs a cue with its next action"));
    }
    // `firm not in (None, True, False)`: Python's 1 == True and 0 == False let a number 1 or 0 through
    let firm = args.get("firm");
    let firm_ok = match firm {
        None | Some(Value::Null) | Some(Value::Bool(_)) => true,
        Some(Value::Number(n)) => matches!(n.as_f64(), Some(f) if f == 0.0 || f == 1.0),
        Some(_) => false,
    };
    if !firm_ok {
        return Err(refused("firm must be true, false or null"));
    }
    let question = args.get("question");
    if !matches!(question, None | Some(Value::Null) | Some(Value::String(_))) {
        return Err(refused("question must be a string or null"));
    }
    let purpose = line(args.get("purpose"), MAX_LINE_CHARS);
    let end_state = line(args.get("end_state"), MAX_LINE_CHARS);
    let deadline = when(args.get("deadline"), DEADLINE_FORMATS, "deadline")?;
    Ok(Fields {
        kind,
        title,
        purpose,
        end_state,
        done_checks: checks,
        never,
        phases,
        deadline,
        cue,
        firm: py::truthy(firm),
        question: line(question, MAX_QUESTION_CHARS),
    })
}

/// `parse`'s keyword arguments, with Python's defaults.
#[derive(Debug, Clone)]
pub struct ParseOptions<'a> {
    pub live: &'a Live,
    pub door_name: &'a str,
    pub said_ref: &'a str,
    pub now: f64,
    pub home: Option<&'a Path>,
}

impl Default for ParseOptions<'_> {
    fn default() -> Self {
        ParseOptions { live: &LIVE, door_name: "telegram", said_ref: "", now: 0.0, home: None }
    }
}

/// The brain's take_on arguments as a Card (id "", rev 1), or a Refusal. `said` is the owner's words in this
/// turn, taken by code from the turn's message, never from the brain; a card is made only in a turn that has
/// them. The words are used for `grounded` and dropped: the card keeps `said_ref`.
pub fn parse(args: &Value, said: &str, opts: &ParseOptions<'_>) -> Result<Card, ParseError> {
    if py::split(said).next().is_none() {
        return Err(refused("a card is made only in a turn that carries the owner's words"));
    }
    let f = fields(args, said, opts.live, opts.home)?;
    let budget = default_budget(&f.phases);
    let card = Card {
        id: String::new(),
        rev: 1,
        kind: f.kind,
        title: f.title,
        purpose: f.purpose,
        end_state: f.end_state,
        done_checks: f.done_checks,
        never: f.never,
        phases: f.phases,
        deadline: f.deadline,
        cue: f.cue,
        firm: f.firm,
        question: f.question,
        budget,
        spent: Spent::default(),
        status: "active".to_string(),
        waiting_for: None,
        source: Source { door: opts.door_name.to_string(), said_ref: py::take_chars(opts.said_ref, 40).to_string() },
        nudges: Nudges::default(),
        created: opts.now,
        updated: opts.now,
    };
    if missing(&card).is_empty() {
        return Ok(card);
    }
    Ok(Card { status: "waiting".to_string(), waiting_for: Some("question".to_string()), ..card })
}

/// What blocks a card from starting: a missing end state, or the one question whose answer changes the
/// outcome. Nothing else may block one (design principle 2).
pub fn missing(card: &Card) -> Vec<&'static str> {
    let mut out = Vec::new();
    if card.end_state.is_empty() {
        out.push("end_state");
    }
    if !card.question.is_empty() {
        out.push("question");
    }
    out
}

/// Python's `max(a, b)`: the first unless the second is greater.
fn py_max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

/// The owner's correction: the take_on fields in `patch` replace the card's, as one new revision (rev+1).
/// The kind never changes; a finished card is reopened, not revised. Each phase keeps the higher of its old and
/// new door (add-only), phases that already ran keep their state (a phase is matched by its kind and goal, so
/// one added, removed or moved never resets another), and every pending approval is cancelled.
pub fn revise(
    card: &Card,
    patch: &Map<String, Value>,
    said: &str,
    live: &Live,
    now: f64,
    home: Option<&Path>,
) -> Result<Card, ParseError> {
    if card.terminal() {
        return Err(refused(format!("{} is {}; reopen it first", card.id, card.status)));
    }
    if let Some(k) = patch.get("kind") {
        if k.as_str() != Some(card.kind.as_str()) {
            return Err(refused("a revision cannot change the kind"));
        }
    }
    let mut merged = to_args(card);
    for (k, v) in patch {
        merged.insert(k.clone(), v.clone());
    }
    let opts =
        ParseOptions { live, door_name: &card.source.door, said_ref: &card.source.said_ref, now, home };
    let new = parse(&Value::Object(merged), said, &opts)?;
    // A phase is the same phase by what it is (its kind and goal), never by its number: matched by number, a
    // phase inserted at the front put every later one "in a new place", and a phase that had already run went
    // back to queued with its result gone, so an act that ran could become eligible again.
    let mut unclaimed: Vec<&Phase> = card.phases.iter().collect();
    let mut phases = Vec::with_capacity(new.phases.len());
    for p in &new.phases {
        if let Some(i) = unclaimed.iter().position(|o| o.r#do == p.r#do && o.goal == p.goal) {
            // the same phase: its state stays, its approval does not
            let same = unclaimed.remove(i);
            if same.status == "running" && same.n != p.n {
                // its result comes back by number (chief.on_phase_result) and would land on another phase
                return Err(refused(format!(
                    "step {} is running; change the card's steps once it finishes",
                    same.n
                )));
            }
            let d = higher(&same.door, &p.door);
            let g = if same.door == d { same.grounded } else { grounded(&p.goal, said) };
            phases.push(Phase { n: p.n, door: d.to_string(), grounded: g, approval: None, ..same.clone() });
            continue;
        }
        let Some(old) = card.phase(p.n) else {
            phases.push(p.clone());
            continue;
        };
        // a new phase in this place: it inherits the higher door
        let d = higher(&old.door, &p.door);
        let g = if p.door == d { p.grounded } else { grounded(&p.goal, said) };
        phases.push(Phase { door: d.to_string(), grounded: g, ..p.clone() });
    }
    let (status, waiting) = if !missing(&new).is_empty() {
        ("waiting".to_string(), Some("question".to_string()))
    } else if card.status == "waiting" && card.waiting_for.as_deref() == Some("question") {
        ("active".to_string(), None)
    } else {
        (card.status.clone(), card.waiting_for.clone())
    };
    let floor = default_budget(&new.phases);
    let budget = Budget {
        usd: py_max(card.budget.usd, floor.usd),
        wall_min: card.budget.wall_min.max(floor.wall_min),
    };
    Ok(Card {
        id: card.id.clone(),
        rev: card.rev.saturating_add(1),
        phases,
        budget,
        spent: card.spent,
        nudges: card.nudges.clone(),
        status,
        waiting_for: waiting,
        created: card.created,
        updated: now,
        ..new
    })
}

/// A done or dropped card comes back as a new revision, active; its history stays in the events log.
pub fn reopen(card: &Card, now: f64) -> Card {
    Card {
        rev: card.rev.saturating_add(1),
        status: "active".to_string(),
        waiting_for: None,
        updated: now,
        phases: card.phases.iter().map(|p| Phase { approval: None, ..p.clone() }).collect(),
        ..card.clone()
    }
}

/// The card's take_on fields, the shape `parse` takes (a revision merges a patch into these).
pub fn to_args(card: &Card) -> Map<String, Value> {
    let value = json!({
        "kind": card.kind, "title": card.title, "purpose": card.purpose, "end_state": card.end_state,
        "done_checks": card.done_checks.iter().map(|c| json!({"kind": c.kind, "arg": c.arg})).collect::<Vec<_>>(),
        "never": card.never,
        "phases": card.phases.iter().map(|p| json!({"do": p.r#do, "goal": p.goal, "door": p.door})).collect::<Vec<_>>(),
        "deadline": card.deadline,
        "cue": card.cue.as_ref().map(|c| json!({"at": c.at, "next_action": c.next_action})),
        "firm": card.firm,
        "question": if card.question.is_empty() { Value::Null } else { Value::String(card.question.clone()) },
    });
    match value {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

// ---- the backbrief -----------------------------------------------------------------------------

fn money(usd: f64) -> String {
    if usd < 10.0 {
        format!("${}", py::fixed(usd, 2)).replace(".00", "")
    } else {
        format!("${}", py::fixed(usd, 0))
    }
}

// A research or watch goal is often a noun phrase ("standing desks under $600"): it gets its kind's verb, unless it
// already opens with a verb of its own.
const KIND_VERB: &[(&str, &str)] = &[("research", "research"), ("watch", "watch")];
const OPENING_VERBS: &[&str] = &[
    "research", "find", "look", "search", "compare", "read", "check", "watch", "track", "see", "get", "learn", "tell",
    "keep",
];

fn step(p: &Phase) -> String {
    let first = py::casefold(p.goal.split(' ').next().unwrap_or_default());
    match KIND_VERB.iter().find(|(k, _)| *k == p.r#do) {
        Some((_, verb)) if !OPENING_VERBS.contains(&first.as_str()) => format!("{verb} {}", p.goal),
        _ => p.goal.clone(),
    }
}

/// What eve says back before it starts, composed by code from the card (nothing here can be invented):
/// "On it: standing desk. I intend to …, then … after your Go. Done when …. Up to $1 and 30 min."
pub fn backbrief(card: &Card) -> String {
    if card.kind == "commitment" {
        let due = card.deadline.as_deref().filter(|d| !d.is_empty()).map(|d| format!(", due {d}")).unwrap_or_default();
        let mut when = " I'll keep it on your list.".to_string();
        if let Some(at) = card.cue.as_ref().and_then(|c| c.at.as_deref()).filter(|a| !a.is_empty()) {
            let latest = at_time(Some(at))
                .and_then(|due_at| due_at.checked_add_signed(Duration::minutes(REMINDER_WAIT_MIN)))
                .map(|t| format!(", {} at the latest", t.format("%H:%M")))
                .unwrap_or_default();
            when = format!(" I'll remind you at your first break after {at}{latest}.");
        }
        return format!("Holding it: {}{due}.{when}", card.title);
    }
    let steps: Vec<String> = card
        .phases
        .iter()
        .map(|p| step(p) + if p.door == "one_way" { " after your Go" } else { "" })
        .collect();
    let plan = steps.join(", then ");
    let mut lines = vec![format!("On it: {}. I intend to {plan}.", card.title)];
    if !card.end_state.is_empty() {
        lines.push(format!("Done when {}.", card.end_state.trim_end_matches('.')));
    }
    if !card.never.is_empty() {
        lines.push(format!("Never: {}.", card.never.join("; ")));
    }
    lines.push(format!("Up to {} and {} min.", money(card.budget.usd), card.budget.wall_min));
    if !card.question.is_empty() {
        lines.push(card.question.clone());
    }
    lines.join(" ")
}

// ---- stored form -------------------------------------------------------------------------------

fn evidence_json(e: &Evidence) -> Value {
    json!({"kind": e.kind, "ref": e.r#ref})
}

fn result_json(r: &PhaseResult) -> Value {
    json!({"status": r.status, "evidence": r.evidence.iter().map(evidence_json).collect::<Vec<_>>(),
           "text": r.text, "usd": r.usd, "secs": r.secs})
}

fn phase_json(p: &Phase) -> Value {
    json!({
        "n": p.n, "do": p.r#do, "goal": p.goal, "door": p.door, "grounded": p.grounded, "executor": p.executor,
        "status": p.status,
        "approval": p.approval.as_ref().map(|a| json!({"token": a.token, "rev": a.rev, "at": a.at, "what": a.what})),
        "result": p.result.as_ref().map(result_json),
    })
}

/// The card as plain JSON values (tuples become lists), the form `from_json` reads back; the keys in the
/// dataclass field order `dataclasses.asdict` writes.
pub fn to_json(card: &Card) -> Value {
    json!({
        "id": card.id, "rev": card.rev, "kind": card.kind, "title": card.title, "purpose": card.purpose,
        "end_state": card.end_state,
        "done_checks": card.done_checks.iter().map(|c| json!({"kind": c.kind, "arg": c.arg})).collect::<Vec<_>>(),
        "never": card.never,
        "phases": card.phases.iter().map(phase_json).collect::<Vec<_>>(),
        "deadline": card.deadline,
        "cue": card.cue.as_ref().map(|c| json!({"at": c.at, "next_action": c.next_action})),
        "firm": card.firm, "question": card.question,
        "budget": {"usd": card.budget.usd, "wall_min": card.budget.wall_min},
        "spent": {"usd": card.spent.usd, "wall_s": card.spent.wall_s},
        "status": card.status, "waiting_for": card.waiting_for,
        "source": {"door": card.source.door, "said_ref": card.source.said_ref},
        "nudges": {"sent": card.nudges.sent, "ignored": card.nudges.ignored, "next_at": card.nudges.next_at,
                   "level": card.nudges.level},
        "created": card.created, "updated": card.updated,
    })
}

/// A finite number >= 0. Infinity is refused too: a hand-edit's 1e999 reads as inf, and int(inf) raises
/// OverflowError, which is not a CardError (reviewer, 2026-09-29).
fn num(value: Option<&Value>, what: &str) -> Result<f64, CardError> {
    let number = match value {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        _ => return Err(CardError(format!("{what} is not a number >= 0.0"))),
    };
    if !number.is_finite() || number < 0.0 {
        return Err(CardError(format!("{what} is not a finite number >= 0.0")));
    }
    Ok(number)
}

/// `int(float)`: truncated toward zero.
fn int_of(x: f64) -> i64 {
    x.trunc() as i64
}

fn string(value: Option<&Value>, what: &str, limit: usize, empty: bool) -> Result<String, CardError> {
    match value {
        Some(Value::String(s)) if py::len(s) <= limit && (empty || !s.is_empty()) => Ok(s.clone()),
        _ => Err(CardError(format!("{what} is not a string"))),
    }
}

/// `raw.get(key, default)`: the default only when the key is missing (a null is None, not the default).
fn get_or<'a>(raw: &'a Map<String, Value>, key: &str, default: &'a Value) -> &'a Value {
    raw.get(key).unwrap_or(default)
}

static EMPTY_STR: Value = Value::String(String::new());
static ZERO: LazyLock<Value> = LazyLock::new(|| json!(0.0));
static EMPTY_OBJECT: LazyLock<Map<String, Value>> = LazyLock::new(Map::new);

fn opt_string(value: Option<&Value>, what: &str, limit: usize) -> Result<Option<String>, CardError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        v => string(v, what, limit, true).map(Some),
    }
}

fn result_from_opt(raw: Option<&Value>) -> Result<Option<PhaseResult>, CardError> {
    let raw = match raw {
        None | Some(Value::Null) => return Ok(None),
        Some(r) => r,
    };
    let obj = match raw.as_object() {
        Some(o) if matches!(o.get("status"), Some(Value::String(s)) if RESULT_STATUSES.contains(&s.as_str())) => o,
        _ => return Err(CardError("a phase result has no known status".into())),
    };
    // `raw.get("evidence") or []`: any falsy value is no evidence
    let ev: &[Value] = match obj.get("evidence") {
        e if !py::truthy(e) => &[],
        Some(Value::Array(a)) if a.len() <= MAX_EVIDENCE => a,
        _ => return Err(CardError("evidence is not a list".into())),
    };
    let mut items = Vec::with_capacity(ev.len());
    for e in ev {
        let kind = match e.as_object().and_then(|o| o.get("kind")) {
            Some(Value::String(k)) if EVIDENCE_KINDS.contains(&k.as_str()) => k.clone(),
            _ => return Err(CardError("evidence has no known kind".into())),
        };
        let r#ref = string(e.get("ref"), "evidence.ref", MAX_EVIDENCE_REF_CHARS, true)?;
        items.push(Evidence { kind, r#ref });
    }
    Ok(Some(PhaseResult {
        status: obj["status"].as_str().unwrap_or_default().to_string(),
        evidence: items,
        text: string(Some(get_or(obj, "text", &EMPTY_STR)), "result.text", MAX_RESULT_TEXT_CHARS, true)?,
        usd: num(Some(get_or(obj, "usd", &ZERO)), "result.usd")?,
        secs: num(Some(get_or(obj, "secs", &ZERO)), "result.secs")?,
    }))
}

/// An executor adapter's result, checked: a closed status, closed evidence kinds, capped text.
pub fn result_from(raw: &Value) -> Result<PhaseResult, CardError> {
    result_from_opt(Some(raw))?.ok_or_else(|| CardError("no result".into()))
}

/// `isinstance(v, int) and not isinstance(v, bool)`, as an i64.
fn int_value(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

fn phase_from(raw: &Value) -> Result<Phase, CardError> {
    let raw = raw.as_object().ok_or_else(|| CardError("a phase is not an object".into()))?;
    let n = match int_value(raw.get("n")) {
        Some(n) if (1..=MAX_PHASES as i64).contains(&n) => n as usize,
        _ => return Err(CardError("a phase has no number".into())),
    };
    let one_of = |key: &str, set: &[&str]| matches!(raw.get(key), Some(Value::String(s)) if set.contains(&s.as_str()));
    if !one_of("do", PHASE_KINDS_LIVE) || !one_of("door", DOORS) || !one_of("status", PHASE_STATUSES) {
        return Err(CardError(format!("phase {n} has an unknown kind, door or status")));
    }
    let text = |key: &str| raw.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    let (r#do, stored_door, status) = (text("do"), text("door"), text("status"));
    let approval = match raw.get("approval") {
        None | Some(Value::Null) => None,
        Some(ap) => {
            let ap = ap.as_object().ok_or_else(|| CardError(format!("phase {n} has a bad approval")))?;
            let rev = int_value(ap.get("rev")).ok_or_else(|| CardError(format!("phase {n} has a bad approval")))?;
            Some(Approval {
                token: string(ap.get("token"), "approval.token", 128, false)?,
                rev,
                at: num(ap.get("at"), "approval.at")?,
                what: string(Some(get_or(ap, "what", &EMPTY_STR)), "approval.what", 64, true)?,
            })
        }
    };
    let goal = string(raw.get("goal"), "goal", MAX_LINE_CHARS, false)?;
    // Preserve an existing one-way door, and never load a dangerous goal below the code's current floor.
    let floor = door::<&str>(&goal, false, &[], !ACT_CANNOT_ASK.contains(&r#do.as_str()));
    let door_ = higher(&stored_door, floor);
    Ok(Phase {
        n,
        r#do,
        goal,
        door: door_.to_string(),
        grounded: py::truthy(raw.get("grounded")),
        executor: string(Some(get_or(raw, "executor", &EMPTY_STR)), "executor", 64, true)?,
        status,
        approval,
        result: result_from_opt(raw.get("result"))?,
    })
}

/// A stored card, checked field by field; CardError on anything off, so a hand-edit never half-loads.
pub fn from_json(raw: &Value) -> Result<Card, CardError> {
    let raw = raw.as_object().ok_or_else(|| CardError("a card is not an object".into()))?;
    let cid = match raw.get("id") {
        Some(Value::String(s)) if ID.is_match(s) => s.clone(),
        _ => return Err(CardError("a card has no id".into())),
    };
    let rev = match int_value(raw.get("rev")) {
        Some(r) if r >= 1 => r,
        _ => return Err(CardError(format!("{cid}: bad rev"))),
    };
    let in_set = |key: &str, set: &[&str]| matches!(raw.get(key), Some(Value::String(s)) if set.contains(&s.as_str()));
    if !in_set("kind", KINDS) || !in_set("status", STATUSES) {
        return Err(CardError(format!("{cid}: unknown kind or status")));
    }
    let waiting_for = match raw.get("waiting_for") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if WAITING_FOR.contains(&s.as_str()) => Some(s.clone()),
        Some(_) => return Err(CardError(format!("{cid}: unknown waiting_for"))),
    };
    let checks = match raw.get("done_checks") {
        Some(Value::Array(a)) if !a.is_empty() => a,
        _ => return Err(CardError(format!("{cid}: no done checks"))),
    };
    let mut done_checks = Vec::with_capacity(checks.len());
    for c in checks {
        let obj = match c.as_object() {
            Some(o) if matches!(o.get("kind"), Some(Value::String(k)) if DONE_KINDS_LIVE.contains(&k.as_str())) => o,
            _ => return Err(CardError(format!("{cid}: unknown done check"))),
        };
        let arg = string(Some(get_or(obj, "arg", &EMPTY_STR)), "check.arg", MAX_PATH_CHARS, true)?;
        done_checks.push(Check { kind: obj["kind"].as_str().unwrap_or_default().to_string(), arg });
    }
    let phases = match raw.get("phases") {
        Some(Value::Array(a)) if a.len() <= MAX_PHASES => a,
        _ => return Err(CardError(format!("{cid}: bad phases"))),
    };
    let never = match raw.get("never") {
        Some(Value::Array(a)) => a,
        _ => return Err(CardError(format!("{cid}: bad never"))),
    };
    let cue = match raw.get("cue") {
        None | Some(Value::Null) => None,
        Some(Value::Object(c)) => Some(Cue {
            at: opt_string(c.get("at"), "cue.at", 16)?,
            next_action: string(c.get("next_action"), "next_action", MAX_LINE_CHARS, true)?,
        }),
        Some(_) => return Err(CardError(format!("{cid}: bad cue"))),
    };
    let sub = |key: &str| match raw.get(key) {
        Some(Value::Object(o)) => o,
        _ => &*EMPTY_OBJECT,
    };
    let (b, s, src, nd) = (sub("budget"), sub("spent"), sub("source"), sub("nudges"));
    let level = match nd.get("level") {
        None => "push".to_string(),
        Some(Value::String(l)) if NUDGE_LEVELS.contains(&l.as_str()) => l.clone(),
        Some(_) => return Err(CardError(format!("{cid}: bad nudge level"))),
    };
    let next_at = nd.get("next_at");
    let zero_int = json!(0);
    let telegram = json!("telegram");
    // the fields in the order Python evaluates the Card(...) call, so the first problem named is the same
    let title = string(raw.get("title"), "title", MAX_TITLE_CHARS, false)?;
    let purpose = string(Some(get_or(raw, "purpose", &EMPTY_STR)), "purpose", MAX_LINE_CHARS, true)?;
    let end_state = string(Some(get_or(raw, "end_state", &EMPTY_STR)), "end_state", MAX_LINE_CHARS, true)?;
    let never = never.iter().map(|x| string(Some(x), "never", MAX_NEVER_CHARS, true)).collect::<Result<Vec<_>, _>>()?;
    let phases = phases.iter().map(phase_from).collect::<Result<Vec<_>, _>>()?;
    let deadline = opt_string(raw.get("deadline"), "deadline", 16)?;
    let firm = py::truthy(raw.get("firm"));
    let question = string(Some(get_or(raw, "question", &EMPTY_STR)), "question", MAX_QUESTION_CHARS, true)?;
    let budget = Budget { usd: num(b.get("usd"), "budget.usd")?, wall_min: int_of(num(b.get("wall_min"), "budget.wall_min")?) };
    let spent = Spent {
        usd: num(Some(get_or(s, "usd", &ZERO)), "spent.usd")?,
        wall_s: num(Some(get_or(s, "wall_s", &ZERO)), "spent.wall_s")?,
    };
    let source = Source {
        door: string(Some(get_or(src, "door", &telegram)), "source.door", 32, true)?,
        said_ref: string(Some(get_or(src, "said_ref", &EMPTY_STR)), "said_ref", 40, true)?,
    };
    let nudges = Nudges {
        sent: int_of(num(Some(get_or(nd, "sent", &zero_int)), "nudges.sent")?),
        ignored: int_of(num(Some(get_or(nd, "ignored", &zero_int)), "nudges.ignored")?),
        next_at: match next_at {
            None | Some(Value::Null) => None,
            v => Some(num(v, "nudges.next_at")?),
        },
        level,
    };
    let created = num(Some(get_or(raw, "created", &ZERO)), "created")?;
    let updated = num(Some(get_or(raw, "updated", &ZERO)), "updated")?;
    Ok(Card {
        id: cid,
        rev,
        kind: raw["kind"].as_str().unwrap_or_default().to_string(),
        title,
        purpose,
        end_state,
        done_checks,
        never,
        phases,
        deadline,
        cue,
        firm,
        question,
        budget,
        spent,
        status: raw["status"].as_str().unwrap_or_default().to_string(),
        waiting_for,
        source,
        nudges,
        created,
        updated,
    })
}

/// The N of "c<N>", or 0.
pub fn card_number(cid: &str) -> u64 {
    ID.captures(cid).and_then(|m| m.get(1)).map(|g| py::digits_value(g.as_str())).unwrap_or(0)
}

pub fn is_watch_id(text: &str) -> bool {
    WATCH_ID.is_match(text)
}
