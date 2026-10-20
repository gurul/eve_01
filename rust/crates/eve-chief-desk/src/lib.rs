//! The chief's desk: the one gate every message it would send the owner goes through, and the briefs it
//! composes. Port of `chief_desk.py`.
//!
//! Pull comes before push (design principle 6, "protect attention"). The owner reads what eve is doing when they
//! ask: the first-message brief rides the reply to their first message of the day, `/jobs` answers with no model
//! call. What the chief would send on its own goes through [`Desk::offer`], which answers with one route and the
//! rule that decided it ([`ROUTES`], [`RULES`]):
//!
//! * `now`     send it.
//! * `batch`   do not send it now. It waits for the next pull (the brief, `/jobs`) or, when the route carries
//!   `at`, is offered again then. Every doubt lands here: an error, a rule that cannot be read, a mandate that
//!   does not parse (consent.py's fail-closed rule).
//! * `silent`  shadow: it would have been sent; it is logged as a would-push and not sent.
//! * `drop`    it no longer matters: the card is finished, or the owner already acted on it.
//!
//! Who may interrupt, by the owner's decisions of 2026-09-29:
//!
//! * **Not unprompted, sent:** a reply, a receipt, and the Go for a job the owner started. A Go waits out quiet
//!   hours; replies and receipts do not (a receipt belongs to work the owner started).
//! * **A reminder the owner asked for** is exempt from the daily budget and from the shadow week, but it still
//!   waits for quiet hours to end and for a breakpoint: a task ending, a relay turn ending, a wake session
//!   ending, or 90 s after the owner's last message. If no breakpoint comes within 45 minutes, it is sent then
//!   anyway (owner, 2026-10-06: it used to move to the next brief, so a "remind me tonight" arrived the next
//!   morning).
//! * **Unprompted** (a nudge, the 80% budget warning, the evening close, the weekly sheet): only a firm
//!   commitment due within 2 hours may nudge; outside quiet hours; at a breakpoint; within 2 a day; after its
//!   backoff. `EVE_CHIEF_PUSH` decides what happens when all of that passes: `shadow` (the default until the E5
//!   shadow week passes) logs it as `silent`, `on` sends it, `off` batches every one. In shadow the budget counts
//!   the would-pushes, so the shadow week measures the policy as it would run.
//! * **Backoff.** Each ignored nudge doubles the gap (4 h, 8 h, 16 h); after 3 ignored nudges the card appears
//!   only on the weekly sheet. The owner's own action on a card cancels its queued nudges and reminders
//!   (`acted`).
//! * **A decision never expires into an act.** [`on_expiry`] takes the choice that does not act, or none.
//!
//! The budget warning was first sent as a receipt, which skipped quiet hours and the push mode; it is not one of
//! the kinds the owner's decisions exempt, so it is `warn`, unprompted (reviewer, 2026-09-29). A problem with the
//! chief's own files is not offered here at all: it rides the reply to the owner's next message.
//!
//! Quiet hours (22:30-08:00) and the daily budget (2) are the defaults; `mandate.toml` in the chief folder may
//! set `quiet_start`, `quiet_end` and `pushes_per_day`. A mandate that does not parse falls back to the
//! strictest settings (the default quiet hours and no unprompted pushes) and the owner is told once.
//!
//! Every verdict is one line in `attention.jsonl` (ids and rule words, never text), which is also where the
//! day's push count lives, so it survives a restart. A verdict to push that cannot be logged is not sent: an
//! unlogged push could break the budget.
//!
//! The brief, the evening close and the weekly sheet are composed by code from the ledger, so they cannot
//! invent a card, a deadline or a result.

mod py;

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use chrono::{NaiveDate, TimeDelta, Timelike};
use eve_chief_card::{at_time, Card, Nudges, REMINDER_WAIT_MIN};
use eve_chief_ledger::{chief_dir, Clock, JsonlRows, Ledger};
use parking_lot::Mutex;
use serde_json::{Map, Value};
use tracing::warn;

pub const PUSH_MODES: &[&str] = &["off", "shadow", "on"];
pub const DEFAULT_PUSH_MODE: &str = "shadow";
pub const QUIET_START: &str = "22:30";
pub const QUIET_END: &str = "08:00";
pub const PUSHES_PER_DAY: i64 = 2;
/// A mandate that does not parse: no unprompted pushes.
pub const STRICT_PUSHES_PER_DAY: i64 = 0;
pub const MAX_PUSHES_PER_DAY: i64 = 10;
pub const MANDATE_FILE: &str = "mandate.toml";
pub const MANDATE_KEYS: &[&str] = &["quiet_start", "quiet_end", "pushes_per_day"];
pub const ATTENTION_FILE: &str = "attention.jsonl";
pub const EVENING_CLOSE_AT: &str = "21:00";
/// Sunday (`datetime.weekday`).
pub const WEEKLY_SHEET_DAY: u32 = 6;
pub const WEEKLY_SHEET_AT: &str = "18:00";

pub const ITEM_KINDS: &[&str] = &["reply", "receipt", "go", "reminder", "nudge", "warn", "close", "sheet"];
pub const UNPROMPTED: &[&str] = &["nudge", "warn", "close", "sheet"];
pub const ROUTES: &[&str] = &["now", "batch", "silent", "drop"];
pub const RULES: &[&str] = &[
    "reply",
    "receipt",
    "owner_started_go",
    "asked_reminder",
    "urgent_firm",
    "budget_warning",
    "evening_close",
    "weekly_sheet",
    "quiet_hours",
    "push_budget",
    "await_breakpoint",
    "reminder_waited",
    "backoff",
    "sheet_only",
    "not_urgent",
    "owner_acted",
    "card_closed",
    "push_off",
    "error",
];
pub const BREAKPOINTS: &[&str] = &["task_end", "relay_end", "wake_end", "idle"];
/// An item offered this soon after a breakpoint is "at" it.
pub const BREAKPOINT_WINDOW_SECS: f64 = 120.0;
/// The owner's last message this long ago is a breakpoint ("idle").
pub const IDLE_SECS: f64 = 90.0;
/// A reminder waits this long for a breakpoint, then is sent.
pub const REMINDER_WAIT_SECS: f64 = REMINDER_WAIT_MIN as f64 * 60.0;
/// A firm commitment due within this may nudge.
pub const URGENT_SECS: f64 = 2.0 * 3600.0;
/// The gap after the first ignored nudge; it doubles with each one.
pub const NUDGE_GAP_SECS: f64 = 4.0 * 3600.0;
/// Then only the weekly sheet.
pub const MAX_IGNORED: i64 = 3;
/// The weekly sheet: an open card untouched this long.
pub const STALE_DAYS: i64 = 7;
pub const BRIEF_LINES: usize = 3;

/// The exception Python raises where the desk cannot go on: `name` is its type (`ValueError`, `OverflowError`,
/// `OSError`), `message` its text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DeskError {
    pub name: &'static str,
    pub message: String,
}

impl DeskError {
    pub fn new(name: &'static str, message: impl Into<String>) -> Self {
        DeskError { name, message: message.into() }
    }

    pub fn value(message: impl Into<String>) -> Self {
        Self::new("ValueError", message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mandate {
    pub quiet_start: String,
    pub quiet_end: String,
    pub pushes_per_day: i64,
}

impl Default for Mandate {
    fn default() -> Self {
        Mandate { quiet_start: QUIET_START.into(), quiet_end: QUIET_END.into(), pushes_per_day: PUSHES_PER_DAY }
    }
}

/// The default quiet hours and no unprompted pushes: what a mandate that does not parse falls back to.
pub static STRICT: LazyLock<Mandate> =
    LazyLock::new(|| Mandate { pushes_per_day: STRICT_PUSHES_PER_DAY, ..Mandate::default() });

/// Something the chief would send. `queued_at`: when it was first offered (the 45-minute wait and the owner's
/// own action count from here). `deadline_at`: a nudge's firm deadline.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// [`ITEM_KINDS`]
    pub kind: String,
    pub card_id: String,
    pub queued_at: f64,
    pub deadline_at: Option<f64>,
    pub firm: bool,
    pub nudges: Nudges,
    pub card_open: bool,
}

impl Item {
    /// `Item(kind, card_id)` with Python's defaults for the rest.
    pub fn new(kind: &str, card_id: &str) -> Self {
        Item {
            kind: kind.to_string(),
            card_id: card_id.to_string(),
            queued_at: 0.0,
            deadline_at: None,
            firm: false,
            nudges: Nudges::default(),
            card_open: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Route {
    /// [`ROUTES`]
    pub route: &'static str,
    /// [`RULES`]
    pub rule: &'static str,
    /// batch: offer it again then; None: the next pull
    pub at: Option<f64>,
}

impl Route {
    pub fn new(route: &'static str, rule: &'static str) -> Self {
        Route { route, rule, at: None }
    }

    pub fn at(route: &'static str, rule: &'static str, at: f64) -> Self {
        Route { route, rule, at: Some(at) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    /// choosing it starts an act
    pub acts: bool,
}

impl Choice {
    pub fn new(label: &str, acts: bool) -> Self {
        Choice { label: label.to_string(), acts }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sheet {
    pub text: String,
    /// each gets Keep and Kill
    pub ids: Vec<String>,
}

fn env_get(environ: Option<&HashMap<String, String>>, key: &str) -> Option<String> {
    match environ {
        Some(env) => env.get(key).cloned(),
        None => std::env::var_os(key).map(|v| v.to_string_lossy().into_owned()),
    }
}

/// EVE_CHIEF_PUSH = off | shadow (the default) | on. 1/true/yes mean on, 0/false/no mean off. `environ` None
/// reads the process environment.
pub fn push_mode(environ: Option<&HashMap<String, String>>) -> String {
    let given = env_get(environ, "EVE_CHIEF_PUSH").filter(|v| !v.is_empty());
    let raw = py::strip(given.as_deref().unwrap_or(DEFAULT_PUSH_MODE)).to_lowercase();
    let raw = match raw.as_str() {
        "1" | "true" | "yes" => "on".to_string(),
        "0" | "false" | "no" => "off".to_string(),
        _ => raw,
    };
    if !PUSH_MODES.contains(&raw.as_str()) {
        warn!(
            "chief: EVE_CHIEF_PUSH={} is not one of ('off', 'shadow', 'on'); {}",
            py::repr_str(&raw),
            DEFAULT_PUSH_MODE
        );
        return DEFAULT_PUSH_MODE.to_string();
    }
    raw
}

/// `_hhmm`: a mandate's time as `HH:MM`; Err is the ValueError text.
fn hhmm(value: Option<&toml::Value>, default: &str) -> Result<String, DeskError> {
    match value {
        None => py::strptime_hhmm(py::strip(default)),
        Some(toml::Value::String(s)) => py::strptime_hhmm(py::strip(s)),
        Some(_) => Err(DeskError::value("not a time")),
    }
}

/// The mandate and a problem line ("" when fine). No file: the defaults. A file that does not parse: STRICT.
pub fn load_mandate(path: &Path) -> (Mandate, String) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let unreadable =
        |why: &str| (STRICT.clone(), format!("I couldn't read {name} ({why}), so I'm sending nothing unprompted."));
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return (Mandate::default(), String::new()),
        Err(e) => return unreadable(py::os_error_name(&e)),
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return unreadable("UnicodeDecodeError");
    };
    let text = py::universal_newlines(&text);
    let raw = match text.parse::<toml::Table>() {
        Ok(t) if !py::toml_1_1_only(&text) => t,
        _ => return unreadable("TOMLDecodeError"),
    };
    let checked = (|| -> Result<Mandate, DeskError> {
        let mut extra: Vec<&String> = raw.keys().filter(|k| !MANDATE_KEYS.contains(&k.as_str())).collect();
        extra.sort();
        if let Some(first) = extra.first() {
            return Err(DeskError::value(format!("unknown setting {}", py::repr_str(first))));
        }
        let n = match raw.get("pushes_per_day") {
            None => PUSHES_PER_DAY,
            Some(toml::Value::Integer(n)) if (0..=MAX_PUSHES_PER_DAY).contains(n) => *n,
            Some(_) => return Err(DeskError::value(format!("pushes_per_day must be 0 to {MAX_PUSHES_PER_DAY}"))),
        };
        let start = hhmm(raw.get("quiet_start"), QUIET_START)?;
        let end = hhmm(raw.get("quiet_end"), QUIET_END)?;
        if start == end {
            return Err(DeskError::value("quiet hours are empty"));
        }
        Ok(Mandate { quiet_start: start, quiet_end: end, pushes_per_day: n })
    })();
    match checked {
        Ok(m) => (m, String::new()),
        Err(e) => (
            STRICT.clone(),
            format!("{name} has a problem ({}), so I'm sending nothing unprompted until it's fixed.", e.message),
        ),
    }
}

/// A log row's time, or None: a hand-edited row ("soon", 1e999, a list) is skipped, never a crash (reviewer,
/// 2026-09-29: `float("soon")` raised in Desk.__init__ and the chief could not be built).
fn row_time(value: Option<&Value>) -> Option<f64> {
    let t = match value {
        Some(Value::Number(n)) => n.as_f64()?,
        _ => return None,
    };
    py::fromtimestamp(t).ok()?;
    (t.is_finite() && t >= 0.0).then_some(t)
}

/// `_minutes`: `h, m = hhmm.split(":")` and `int(h) * 60 + int(m)`.
fn minutes_of(hhmm: &str) -> Result<i64, DeskError> {
    let (h, m) = split_hm(hhmm)?;
    Ok(h * 60 + m)
}

fn split_hm(hhmm: &str) -> Result<(i64, i64), DeskError> {
    let parts: Vec<&str> = hhmm.split(':').collect();
    if parts.len() != 2 {
        let how = if parts.len() < 2 { "not enough values to unpack (expected 2, got 1)".to_string() } else {
            "too many values to unpack (expected 2)".to_string()
        };
        return Err(DeskError::value(how));
    }
    Ok((py::int_of_str(parts[0])?, py::int_of_str(parts[1])?))
}

/// Whether `ts` falls in the mandate's quiet hours (a window may cross midnight). Err where Python raises: a
/// time `fromtimestamp` cannot read, or a mandate time that is not `H:M`.
pub fn in_quiet(mandate: &Mandate, ts: f64) -> Result<bool, DeskError> {
    let t = py::fromtimestamp(ts)?;
    let now = i64::from(t.hour()) * 60 + i64::from(t.minute());
    let start = minutes_of(&mandate.quiet_start)?;
    let end = minutes_of(&mandate.quiet_end)?;
    Ok(if start < end { start <= now && now < end } else { now >= start || now < end })
}

/// When the quiet hours holding `ts` end (`ts` itself when it is not in them).
pub fn quiet_ends(mandate: &Mandate, ts: f64) -> Result<f64, DeskError> {
    if !in_quiet(mandate, ts)? {
        return Ok(ts);
    }
    let t = py::fromtimestamp(ts)?;
    let (h, m) = split_hm(&mandate.quiet_end)?;
    let mut end = py::replace_hm(&t, h, m)?;
    if end <= t {
        end = py::plus_day(&end)?;
    }
    py::naive_timestamp(&end).ok_or_else(|| DeskError::new("OverflowError", "timestamp out of range"))
}

/// A decision that expired (a timeout, a hold word, silence) takes the first choice that does not act, or none
/// at all: it never expires into an act (design rule 7; consent.decision's "" is never a yes).
pub fn on_expiry(choices: &[Choice]) -> Option<Choice> {
    choices.iter().find(|c| !c.acts).cloned()
}

/// What `Desk::new` takes besides the folder: Python's keyword arguments. `mode` not one of [`PUSH_MODES`] (or
/// None) reads `EVE_CHIEF_PUSH`; `mandate` None reads `mandate.toml`; `clock` None is `time.time`; `environ`
/// None is the process environment.
#[derive(Default, Clone)]
pub struct DeskOptions {
    pub mode: Option<String>,
    pub mandate: Option<Mandate>,
    pub clock: Option<Clock>,
    pub environ: Option<HashMap<String, String>>,
}

#[derive(Default)]
struct State {
    break_at: Option<f64>,
    owner_said_at: Option<f64>,
    acted: HashMap<String, f64>,
    problem: String,
}

/// The attention gate. Lent a clock so the tests drive the hours. Thread-safe.
pub struct Desk {
    dir: PathBuf,
    clock: Clock,
    state: Mutex<State>,
    write: Mutex<()>,
    /// attention.jsonl, parsed once per line (pushes_today)
    attention_rows: JsonlRows,
    pub mode: String,
    pub mandate: Mandate,
    /// the test seam for "the gate itself broke" (Python's monkeypatched at_breakpoint)
    #[cfg(test)]
    break_probe_panics: std::sync::atomic::AtomicBool,
}

impl fmt::Debug for Desk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Desk").field("dir", &self.dir).field("mode", &self.mode).field("mandate", &self.mandate).finish()
    }
}

/// One attention line as `json.dumps(row, separators=(",", ":"))` writes it.
enum Field<'a> {
    Num(f64),
    Str(&'a str),
    Bool(bool),
}

fn dumps_row(fields: &[(&str, Field<'_>)]) -> String {
    let body: Vec<String> = fields
        .iter()
        .map(|(k, v)| {
            let v = match v {
                Field::Num(x) => py::float_repr(*x),
                Field::Str(s) => py::json_str(s),
                Field::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
            };
            format!("{}:{v}", py::json_str(k))
        })
        .collect();
    format!("{{{}}}\n", body.join(","))
}

impl Desk {
    /// `Desk(folder, mode=, mandate=, clock=, environ=)`: the folder (`~` expanded), or `chief_dir(environ)`
    /// when None.
    pub fn new(folder: Option<&Path>, opts: DeskOptions) -> Desk {
        let environ = opts.environ.as_ref();
        let dir = match folder {
            Some(f) => py::expanduser(&f.to_string_lossy()),
            None => chief_dir(environ),
        };
        let mode = match opts.mode {
            Some(m) if PUSH_MODES.contains(&m.as_str()) => m,
            _ => push_mode(environ),
        };
        let (mandate, problem) = match opts.mandate {
            Some(m) => (m, String::new()),
            None => {
                let (m, problem) = load_mandate(&dir.join(MANDATE_FILE));
                if !problem.is_empty() {
                    warn!("chief: {problem}");
                }
                (m, problem)
            }
        };
        let desk = Desk {
            dir,
            clock: opts.clock.unwrap_or_else(|| Arc::new(py::time_now)),
            state: Mutex::new(State { problem, ..State::default() }),
            write: Mutex::new(()),
            attention_rows: JsonlRows::new(),
            mode,
            mandate,
            #[cfg(test)]
            break_probe_panics: std::sync::atomic::AtomicBool::new(false),
        };
        let mut acted: HashMap<String, f64> = HashMap::new();
        for row in desk.rows() {
            let t = row_time(row.get("t"));
            if let (Some("acted"), Some(Value::String(id)), Some(t)) =
                (row.get("kind").and_then(Value::as_str), row.get("id"), t)
            {
                let was = acted.get(id).copied().unwrap_or(0.0);
                acted.insert(id.clone(), was.max(t));
            }
        }
        desk.state.lock().acted = acted;
        desk
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(ATTENTION_FILE)
    }

    fn now(&self, now: Option<f64>) -> f64 {
        now.unwrap_or_else(|| (self.clock)())
    }

    /// The mandate's problem line, once ("" after).
    pub fn take_problem(&self) -> String {
        std::mem::take(&mut self.state.lock().problem)
    }

    // -- the log --
    fn append(&self, line: &str) -> bool {
        let _w = self.write.lock();
        let wrote = (|| -> io::Result<()> {
            py::mkdir_700(&self.dir)?;
            let mut f = fs::OpenOptions::new().append(true).create(true).mode(0o600).open(self.path())?;
            f.write_all(line.as_bytes())?;
            py::fsync(&f)
        })();
        match wrote {
            Ok(()) => true,
            Err(e) => {
                warn!("chief: an attention line was not written ({})", py::os_error_name(&e));
                false
            }
        }
    }

    fn rows(&self) -> Vec<Map<String, Value>> {
        self.attention_rows.rows(&self.path())
    }

    /// Unprompted pushes today (sent, or would-sent in shadow), read from the log: a restart keeps the count.
    /// Err where `fromtimestamp(now)` raises.
    pub fn pushes_today(&self, now: Option<f64>) -> Result<usize, DeskError> {
        let day = py::fromtimestamp(self.now(now))?.date();
        let mut n = 0;
        for row in self.rows() {
            let route = row.get("route").and_then(Value::as_str);
            if !(py::truthy(row.get("unprompted")) && matches!(route, Some("now") | Some("silent"))) {
                continue;
            }
            if let Some(t) = row_time(row.get("t")) {
                if py::fromtimestamp(t).map(|d| d.date()).ok() == Some(day) {
                    n += 1;
                }
            }
        }
        Ok(n)
    }

    // -- the moments --
    /// A task ended, a relay turn ended, a wake session ended, or the owner went quiet ([`BREAKPOINTS`]).
    pub fn breakpoint(&self, kind: &str, now: Option<f64>) -> Result<(), DeskError> {
        if !BREAKPOINTS.contains(&kind) {
            return Err(DeskError::value(format!("unknown breakpoint {}", py::repr_str(kind))));
        }
        let t = self.now(now);
        self.state.lock().break_at = Some(t);
        Ok(())
    }

    /// The owner wrote: IDLE_SECS later is a breakpoint.
    pub fn owner_said(&self, now: Option<f64>) {
        let t = self.now(now);
        self.state.lock().owner_said_at = Some(t);
    }

    /// A breakpoint happened within BREAKPOINT_WINDOW_SECS, or the owner's last message is IDLE_SECS old (and
    /// not much older: a message from hours ago is not a moment).
    pub fn at_breakpoint(&self, now: f64) -> bool {
        #[cfg(test)]
        if self.break_probe_panics.load(std::sync::atomic::Ordering::Relaxed) {
            panic!("clock broke");
        }
        let st = self.state.lock();
        let broke = st.break_at.is_some_and(|b| 0.0 <= now - b && now - b <= BREAKPOINT_WINDOW_SECS);
        let idle =
            st.owner_said_at.is_some_and(|s| IDLE_SECS <= now - s && now - s <= IDLE_SECS + BREAKPOINT_WINDOW_SECS);
        broke || idle
    }

    /// The owner acted on this card themselves: its queued nudges and reminders are dropped.
    pub fn acted(&self, card_id: &str, now: Option<f64>) {
        let t = self.now(now);
        {
            let mut st = self.state.lock();
            let was = st.acted.get(card_id).copied().unwrap_or(0.0);
            st.acted.insert(card_id.to_string(), was.max(t));
        }
        self.append(&dumps_row(&[
            ("t", Field::Num(py::round_ndigits(t, 3))),
            ("kind", Field::Str("acted")),
            ("id", Field::Str(card_id)),
        ]));
    }

    // -- backoff --
    /// A nudge went out: the next one waits the current gap.
    pub fn sent(nudges: &Nudges, now: f64) -> Nudges {
        Nudges {
            sent: nudges.sent.saturating_add(1),
            next_at: Some(now + NUDGE_GAP_SECS * 2f64.powf(nudges.ignored as f64)),
            ..nudges.clone()
        }
    }

    /// A nudge was ignored: the gap doubles; at MAX_IGNORED the card goes to the weekly sheet only.
    pub fn ignored(nudges: &Nudges, now: f64) -> Nudges {
        let n = nudges.ignored.saturating_add(1);
        Nudges {
            ignored: n,
            next_at: Some(now + NUDGE_GAP_SECS * 2f64.powf((n - 1) as f64)),
            level: if n >= MAX_IGNORED { "sheet".to_string() } else { nudges.level.clone() },
            ..nudges.clone()
        }
    }

    // -- the gate --
    /// One verdict for one item, logged. Never fails: an error (or a panic) is `batch`.
    pub fn offer(&self, item: &Item, now: Option<f64>) -> Route {
        let t = self.now(now);
        // a gate that breaks holds the message, it never sends it
        let route = match panic::catch_unwind(AssertUnwindSafe(|| self.decide(item, t))) {
            Ok(Ok(route)) => route,
            Ok(Err(e)) => {
                warn!("chief: the desk could not decide ({}); batched", e.name);
                Route::new("batch", "error")
            }
            Err(_) => {
                warn!("chief: the desk could not decide (panic); batched");
                Route::new("batch", "error")
            }
        };
        let unprompted = UNPROMPTED.contains(&item.kind.as_str());
        let line = dumps_row(&[
            ("t", Field::Num(py::round_ndigits(t, 3))),
            ("kind", Field::Str(py::take_chars(&item.kind, 16))),
            ("id", Field::Str(py::take_chars(&item.card_id, 16))),
            ("route", Field::Str(route.route)),
            ("rule", Field::Str(route.rule)),
            ("mode", Field::Str(&self.mode)),
            ("unprompted", Field::Bool(unprompted)),
        ]);
        if !self.append(&line) && unprompted && matches!(route.route, "now" | "silent") {
            return Route::new("batch", "error"); // an uncounted push could break the budget
        }
        route
    }

    fn decide(&self, item: &Item, now: f64) -> Result<Route, DeskError> {
        let kind = item.kind.as_str();
        if !ITEM_KINDS.contains(&kind) {
            return Err(DeskError::value(format!("unknown item kind {}", py::repr_str(kind))));
        }
        if matches!(kind, "go" | "reminder" | "nudge") && !item.card_open {
            return Ok(Route::new("drop", "card_closed"));
        }
        if matches!(kind, "reminder" | "nudge") && !item.card_id.is_empty() {
            let acted = self.state.lock().acted.get(&item.card_id).copied().unwrap_or(-1.0);
            if acted >= item.queued_at {
                return Ok(Route::new("drop", "owner_acted"));
            }
        }
        if kind == "reply" {
            return Ok(Route::new("now", "reply"));
        }
        if kind == "receipt" {
            return Ok(Route::new("now", "receipt"));
        }
        let quiet = in_quiet(&self.mandate, now)?;
        if kind == "go" {
            return Ok(if quiet {
                Route::at("batch", "quiet_hours", quiet_ends(&self.mandate, now)?)
            } else {
                Route::new("now", "owner_started_go")
            });
        }
        if kind == "reminder" {
            if quiet {
                return Ok(Route::at("batch", "quiet_hours", quiet_ends(&self.mandate, now)?));
            }
            if self.at_breakpoint(now) {
                return Ok(Route::new("now", "asked_reminder"));
            }
            if now - item.queued_at >= REMINDER_WAIT_SECS {
                return Ok(Route::new("now", "reminder_waited"));
            }
            return Ok(Route::at("batch", "await_breakpoint", item.queued_at + REMINDER_WAIT_SECS));
        }
        // unprompted: a nudge, the budget warning, the evening close, the weekly sheet
        if self.mode == "off" {
            return Ok(Route::new("batch", "push_off"));
        }
        if kind == "nudge" {
            if item.nudges.level == "sheet" || item.nudges.ignored >= MAX_IGNORED {
                return Ok(Route::new("batch", "sheet_only"));
            }
            let urgent = item.firm && item.deadline_at.is_some_and(|d| d - now <= URGENT_SECS);
            if !urgent {
                return Ok(Route::new("batch", "not_urgent"));
            }
            if let Some(next_at) = item.nudges.next_at {
                if next_at > now {
                    return Ok(Route::at("batch", "backoff", next_at));
                }
            }
        }
        if quiet {
            return Ok(Route::at("batch", "quiet_hours", quiet_ends(&self.mandate, now)?));
        }
        if (self.pushes_today(Some(now))? as i64) >= self.mandate.pushes_per_day {
            return Ok(Route::new("batch", "push_budget"));
        }
        if !self.at_breakpoint(now) {
            return Ok(Route::new("batch", "await_breakpoint"));
        }
        let rule = match kind {
            "nudge" => "urgent_firm",
            "warn" => "budget_warning",
            "close" => "evening_close",
            _ => "weekly_sheet",
        };
        Ok(Route::new(if self.mode == "shadow" { "silent" } else { "now" }, rule))
    }
}

// ---- composed by code from the ledger -------------------------------------------------------------

fn due_at(card: &Card) -> Option<f64> {
    let stamps = [at_time(card.deadline.as_deref()), card.cue.as_ref().and_then(|c| at_time(c.at.as_deref()))];
    stamps.iter().flatten().filter_map(py::naive_timestamp).reduce(f64::min)
}

/// `c.deadline or (c.cue.at if c.cue else "")`, as an f-string writes it.
fn due_word(card: &Card) -> String {
    match (&card.deadline, &card.cue) {
        (Some(d), _) if !d.is_empty() => d.clone(),
        (_, Some(cue)) => cue.at.clone().unwrap_or_else(|| "None".to_string()),
        _ => String::new(),
    }
}

fn waiting_word(card: &Card) -> String {
    match card.waiting_for.as_deref() {
        Some("go") => {
            let pending = card.phases.iter().find(|p| p.door == "one_way" && p.status == "queued");
            format!("{} your Go ({})", card.id, pending.map(|p| p.goal.as_str()).unwrap_or(&card.title))
        }
        Some("question") => format!("{} your answer ({})", card.id, card.title),
        Some("budget") => format!("{} a budget raise ({})", card.id, card.title),
        _ => String::new(),
    }
}

fn fit(lines: &[String], limit: usize) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            if py::len(line) <= limit {
                return line.clone();
            }
            let head = py::take_chars(line, limit - 1);
            let cut = head.rsplit_once(' ').map(|(a, _)| a).unwrap_or(head);
            format!("{cut}…")
        })
        .collect()
}

fn by_due(a: &f64, b: &f64) -> std::cmp::Ordering {
    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
}

/// After the owner's first message of the day: today's one thing, what waits on them, what closed since
/// yesterday, what is proposed. At most BRIEF_LINES lines; "" when there is nothing.
pub fn first_brief(ledger: &Ledger, now: f64) -> String {
    let cards = ledger.all();
    let open: Vec<&Card> = cards.iter().filter(|c| !c.terminal() && c.status != "proposed").collect();
    let mut dated: Vec<(f64, &Card)> = open.iter().filter_map(|c| due_at(c).map(|d| (d, *c))).collect();
    dated.sort_by(|a, b| by_due(&a.0, &b.0));
    let mut lines: Vec<String> = Vec::new();
    if let Some((_, c)) = dated.first() {
        lines.push(format!("Today's one thing: {} ({}), due {}.", c.title, c.id, due_word(c)));
    }
    let waiting: Vec<String> =
        open.iter().filter(|c| c.status == "waiting").map(|c| waiting_word(c)).filter(|w| !w.is_empty()).collect();
    if !waiting.is_empty() {
        lines.push(format!("Waiting on you: {}.", waiting.join("; ")));
    }
    let closed: Vec<String> = cards
        .iter()
        .filter(|c| c.terminal() && c.updated >= now - 86400.0)
        .map(|c| format!("{} ({}, {})", c.title, c.id, c.status))
        .collect();
    if !closed.is_empty() {
        lines.push(format!("Closed since yesterday: {}.", closed.join(", ")));
    }
    let proposed: Vec<String> =
        cards.iter().filter(|c| c.status == "proposed").map(|c| format!("{} ({})", c.title, c.id)).collect();
    if !proposed.is_empty() {
        lines.push(format!("Proposed: {}.", proposed.join(", ")));
    }
    lines.truncate(BRIEF_LINES);
    fit(&lines, 200).join("\n")
}

fn local_date(ts: f64) -> Result<NaiveDate, DeskError> {
    Ok(py::fromtimestamp(ts)?.date())
}

/// What closed today with its checks, what slipped, and one choice about tomorrow. Err where Python raises (a
/// time `fromtimestamp` cannot read).
pub fn evening_close(ledger: &Ledger, now: f64) -> Result<String, DeskError> {
    let today = local_date(now)?;
    let cards = ledger.all();
    let mut lines: Vec<String> = Vec::new();
    let mut closed: Vec<String> = Vec::new();
    for c in cards.iter().filter(|c| c.status == "done") {
        if local_date(c.updated)? == today {
            let checks: Vec<String> = c.done_checks.iter().map(|k| k.describe()).collect();
            closed.push(format!("{} ({}: {})", c.title, c.id, checks.join(", ")));
        }
    }
    if !closed.is_empty() {
        lines.push(format!("Closed today: {}.", closed.join("; ")));
    }
    let open: Vec<&Card> = cards.iter().filter(|c| !c.terminal() && c.status != "proposed").collect();
    // `_due_at(c) or float("inf")`: a due time of exactly 0.0 is falsy in Python
    let slipped: Vec<String> = open
        .iter()
        .filter(|c| due_at(c).filter(|d| *d != 0.0).unwrap_or(f64::INFINITY) < now)
        .map(|c| format!("{} ({}, due {})", c.title, c.id, due_word(c)))
        .collect();
    if !slipped.is_empty() {
        lines.push(format!("Slipped: {}.", slipped.join(", ")));
    }
    let tomorrow = today
        .checked_add_signed(TimeDelta::days(1))
        .filter(|d| chrono::Datelike::year(d) <= 9999)
        .ok_or_else(|| DeskError::new("OverflowError", "date value out of range"))?;
    let mut next: Vec<(f64, &Card)> = Vec::new();
    for c in &open {
        if let Some(d) = due_at(c) {
            if local_date(d)? == tomorrow {
                next.push((d, c));
            }
        }
    }
    next.sort_by(|a, b| by_due(&a.0, &b.0));
    if let Some((_, c)) = next.first() {
        lines.push(format!("Tomorrow first: {} ({})?", c.title, c.id));
    }
    Ok(fit(&lines, 200).join("\n"))
}

/// Stale cards, cards with no next action, reminders ignored 3 times, unverified cards; each with Keep and
/// Kill. Empty text when there is nothing.
pub fn weekly_sheet(ledger: &Ledger, now: f64) -> Sheet {
    let mut rows: Vec<(String, String)> = Vec::new();
    for c in ledger.open() {
        let why = if c.status == "unverified" {
            "not confirmed".to_string()
        } else if c.nudges.level == "sheet" || c.nudges.ignored >= MAX_IGNORED {
            format!("reminder ignored {} times", c.nudges.ignored)
        } else if c.kind == "job" && !c.phases.iter().any(|p| p.status == "queued" || p.status == "running") {
            "no next action".to_string()
        } else if c.updated <= now - (STALE_DAYS * 86400) as f64 {
            format!("untouched for {} days", ((now - c.updated) / 86400.0).floor() as i64)
        } else {
            continue;
        };
        rows.push((c.id.clone(), format!("{} {}: {why}", c.id, c.title)));
    }
    let texts: Vec<String> = rows.iter().map(|r| r.1.clone()).collect();
    Sheet { text: fit(&texts, 200).join("\n"), ids: rows.into_iter().map(|r| r.0).collect() }
}

#[cfg(test)]
mod tests;
