//! The chief of staff's ledger: the cards on disk, their events, and what is due. Port of `chief_ledger.py`.
//!
//! Everything lives in one folder, `~/.config/eve-bridge/chief/` (`EVE_CHIEF_DIR` moves it; the tests point it
//! at a temporary folder), never in the repo, mode 600 for the files and 700 for the folder:
//!
//! ```text
//! cards.json     the cards and `last_id`, written atomically
//! events.jsonl   append-only: one line per created, revised, state, asked, answered, approved, dispatched,
//!                result, checked, pushed, would_push, critic, reflected, retried, reopened or redacted event
//! ```
//!
//! The store is the Watcher's (watch.py:2033-2092), for the same reasons:
//!
//! * **Atomic.** A temp file in the same folder, then a rename (`os.replace`). A crash or a failed rename
//!   mid-write leaves the old file whole; the change is not taken in memory either, and the caller gets
//!   [`LedgerError`].
//! * **An unreadable file is moved aside,** as `cards.json.bad-<ts>`, and the owner is told once
//!   ([`Ledger::take_problem`]). So is a file that parses but is not the ledger's shape (a list, a string, `cards`
//!   that is not a list). When single cards do not load (a hand-edit's typo, a number too big), the rest load,
//!   the whole file is COPIED aside first and the owner is told which ids: the next save writes only what
//!   loaded, and without the copy it would erase the rest for good (reviewer, 2026-09-29). A hand-edit's typo
//!   never costs a card.
//! * **Ids are never reused.** `last_id` is saved; a bad file's highest `c<N>` and every id in the events log
//!   count too, so an id the log already speaks of is never handed out again, even after the cards file is lost.
//!
//! The events log is the audit log's shape (audit.py): one JSON object per line, appended, never rewritten
//! except by [`Ledger::redact`]. [`Ledger::event`] returns whether the line reached the disk (flushed and
//! fsynced): a dispatch or a push is logged BEFORE the act, and a caller that gets false does not act. That is
//! what lets a restart tell "dispatched, no result" from "never sent" (design rule 13: an act with a dispatch line
//! and no result never runs again without a new yes). Event lines carry ids, numbers and short code words, never
//! what the owner said.
//!
//! State changes (status, a phase's status or result, what was spent) go through [`Ledger::update`] and
//! [`Ledger::update_phase`], which only touch the state fields. What the owner agreed to changes only through
//! [`Ledger::revise`] (chief_card::revise: rev+1, the door add-only, approvals cancelled).
//!
//! Forget does not reach this folder yet: memory.NOT_COVERED names it, and `redact` is the helper a forget layer
//! will call.
//!
//! Python's keyword arguments become typed values here: the state fields are a [`StateUpdate`] and a
//! [`PhaseUpdate`], an event's fields a slice of `(name, value)` pairs, and the event kind an [`EventKind`].

mod py;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};

use eve_chief_card::{
    self as chief_card, Approval, Budget, Card, Live, Nudges, Phase, PhaseResult, Refusal, Spent, PHASE_STATUSES,
    STATUSES, WAITING_FOR,
};

pub const DEFAULT_DIR: &str = "~/.config/eve-bridge/chief";
pub const CARDS_FILE: &str = "cards.json";
pub const EVENTS_FILE: &str = "events.jsonl";
pub const EVENT_KINDS: &[&str] = &[
    "created", "revised", "state", "asked", "answered", "approved", "dispatched", "result", "checked", "pushed",
    "would_push", "critic", "reflected", "not_reflected", "retried", "reopened", "redacted", "replanned",
    "would_replan", "not_replanned",
];
pub const STATE_FIELDS: &[&str] = &["status", "waiting_for", "spent", "budget", "nudges"];
pub const PHASE_STATE_FIELDS: &[&str] = &["status", "executor", "approval", "result"];
/// The store's cap; the oldest finished cards give way first.
pub const MAX_CARDS: usize = 500;
/// for_turn: the line the brain sees every turn.
pub const TURN_LINE_CHARS: usize = 300;
pub const MAX_EVENT_TEXT: usize = 200;

static ID_IN_TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""c([1-9]\d{0,8})""#).expect("_ID_IN_TEXT"));
// Python's `\s` is str.isspace, which also takes U+001C..U+001F.
static ID_IN_EVENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""id":[\s\x1c-\x1f]*"c([1-9]\d{0,8})""#).expect("ids in events"));
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z0-9]+").expect("_WORD"));
const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "about", "your", "have", "will", "what", "when",
    "then", "them", "they", "just", "some", "please", "eve", "need", "want",
];

fn waiting_word(w: &str) -> Option<&'static str> {
    match w {
        "go" => Some("Go"),
        "question" => Some("answer"),
        "budget" => Some("budget raise"),
        _ => None,
    }
}

fn doing(d: &str) -> Option<&'static str> {
    match d {
        "research" => Some("researching"),
        "assess" => Some("weighing options"),
        "act" => Some("acting"),
        "watch" => Some("setting a watch"),
        _ => None,
    }
}

// ---- errors ------------------------------------------------------------------------------------

/// The ledger could not save a change. Nothing changed, on disk or in memory; the caller falls back.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LedgerError(pub String);

/// What a ledger call raises in Python: a ValueError (a bad field or a finished card), a KeyError (no such card
/// or phase), or a [`LedgerError`] (the save failed).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Value(String),
    #[error("{0}")]
    Key(String),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

// ---- the folder --------------------------------------------------------------------------------

/// The ledger's folder: `EVE_CHIEF_DIR` (stripped; empty is unset) or [`DEFAULT_DIR`], `~` expanded.
/// `environ` None reads the process environment.
pub fn chief_dir(environ: Option<&HashMap<String, String>>) -> PathBuf {
    let raw = match environ {
        Some(env) => env.get("EVE_CHIEF_DIR").cloned().unwrap_or_default(),
        None => std::env::var_os("EVE_CHIEF_DIR").map(|v| v.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    let raw = py::strip(&raw);
    py::expanduser(if raw.is_empty() { DEFAULT_DIR } else { raw })
}

fn content_words(text: &str) -> HashSet<String> {
    let folded = py::casefold(text);
    WORD.find_iter(&folded)
        .map(|m| m.as_str())
        .filter(|w| w.len() >= 4 && !STOP.contains(w))
        .map(str::to_string)
        .collect()
}

/// The local date of an epoch time, ISO; "" where Python's `fromtimestamp` raises (not finite, out of range).
fn day(ts: f64) -> String {
    py::local_day(ts).unwrap_or_default()
}

/// One event field: a number, bool or null as it is; anything else as its text, whitespace squashed and cut to
/// [`MAX_EVENT_TEXT`] characters (a dict or a list inside a list as Python's `str()` writes it).
fn scalar(value: &Value) -> Value {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        other => Value::String(py::take_chars(&py::squash(&py::str_value(other)), MAX_EVENT_TEXT).to_string()),
    }
}

/// The digits of a `c<N>` id as a number; `card_number` reads Unicode digits as Python's `int()` does.
fn id_number(digits: &str) -> u64 {
    chief_card::card_number(&format!("c{digits}"))
}

// ---- events ------------------------------------------------------------------------------------

/// The kinds of line events.jsonl holds ([`EVENT_KINDS`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    Created,
    Revised,
    State,
    Asked,
    Answered,
    Approved,
    Dispatched,
    Result,
    Checked,
    Pushed,
    WouldPush,
    Critic,
    Reflected,
    NotReflected,
    Retried,
    Reopened,
    Redacted,
    Replanned,
    WouldReplan,
    NotReplanned,
}

impl EventKind {
    pub const ALL: [EventKind; 20] = [
        EventKind::Created,
        EventKind::Revised,
        EventKind::State,
        EventKind::Asked,
        EventKind::Answered,
        EventKind::Approved,
        EventKind::Dispatched,
        EventKind::Result,
        EventKind::Checked,
        EventKind::Pushed,
        EventKind::WouldPush,
        EventKind::Critic,
        EventKind::Reflected,
        EventKind::NotReflected,
        EventKind::Retried,
        EventKind::Reopened,
        EventKind::Redacted,
        EventKind::Replanned,
        EventKind::WouldReplan,
        EventKind::NotReplanned,
    ];

    /// The code word written as the line's `e`.
    pub fn as_str(self) -> &'static str {
        EVENT_KINDS[self as usize]
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EventKind {
    type Err = Error;

    /// Python's `ValueError("unknown event kind 'x'")` for a word outside [`EVENT_KINDS`].
    fn from_str(s: &str) -> Result<Self, Error> {
        EVENT_KINDS
            .iter()
            .position(|k| *k == s)
            .map(|i| EventKind::ALL[i])
            .ok_or_else(|| Error::Value(format!("unknown event kind {}", py::repr_str(s))))
    }
}

#[derive(Default)]
struct RowsState {
    key: Option<(u64, u64)>,
    offset: u64,
    rows: Vec<Map<String, Value>>,
}

/// The dict rows of an append-only JSONL file, parsed once each.
///
/// The chief reads its events (and the desk its attention log) many times a tick, and parsing the whole file
/// each time grew with every line ever written. This keeps the rows parsed so far and the byte offset they end
/// at, and reads only what was appended since. A file that was replaced (another inode, as [`Ledger::redact`]
/// does with a rename) or shrank is read afresh. A last line without its newline yet (a write in flight) is
/// parsed for this answer but read again next time. A torn or non-dict line is skipped.
#[derive(Default)]
pub struct JsonlRows {
    state: Mutex<RowsState>,
    parsed: AtomicUsize,
}

impl JsonlRows {
    pub fn new() -> Self {
        Self::default()
    }

    fn parse(&self, chunk: &[u8]) -> Vec<Map<String, Value>> {
        let mut out = Vec::new();
        for line in chunk.split(|b| *b == b'\n') {
            if py::bytes_blank(line) {
                continue;
            }
            self.parsed.fetch_add(1, Ordering::Relaxed);
            if let Ok(Value::Object(row)) = py::loads(&String::from_utf8_lossy(line), py::NonFinite::Null) {
                out.push(row);
            }
        }
        out
    }

    /// The lines parsed so far, over this reader's life (what the D6 test counts).
    #[cfg(test)]
    fn parsed_lines(&self) -> usize {
        self.parsed.load(Ordering::Relaxed)
    }

    /// Every dict row of the file at `path`, oldest first; none when it cannot be read.
    pub fn rows(&self, path: &Path) -> Vec<Map<String, Value>> {
        let mut st = self.state.lock();
        let read = (|| -> io::Result<Vec<u8>> {
            let mut f = fs::File::open(path)?;
            let meta = f.metadata()?;
            let key = (meta.dev(), meta.ino());
            if Some(key) != st.key || meta.len() < st.offset {
                *st = RowsState { key: Some(key), ..RowsState::default() };
            }
            f.seek(SeekFrom::Start(st.offset))?;
            let mut data = Vec::new();
            f.read_to_end(&mut data)?;
            Ok(data)
        })();
        let data = match read {
            Ok(data) => data,
            Err(_) => {
                *st = RowsState::default();
                return Vec::new();
            }
        };
        let end = data.iter().rposition(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0);
        if end > 0 {
            let fresh = self.parse(&data[..end]);
            st.rows.extend(fresh);
            st.offset += end as u64;
        }
        let tail = if end < data.len() && !py::bytes_blank(&data[end..]) { self.parse(&data[end..]) } else { Vec::new() };
        let mut out = st.rows.clone();
        out.extend(tail);
        out
    }
}

// ---- state changes -----------------------------------------------------------------------------

/// The state fields `update` may change ([`STATE_FIELDS`]); a field left None is left alone.
/// `waiting_for: Some(None)` sets it to None.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StateUpdate {
    pub status: Option<String>,
    pub waiting_for: Option<Option<String>>,
    pub spent: Option<Spent>,
    pub budget: Option<Budget>,
    pub nudges: Option<Nudges>,
}

impl StateUpdate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(mut self, status: &str) -> Self {
        self.status = Some(status.to_string());
        self
    }

    pub fn waiting_for(mut self, waiting_for: Option<&str>) -> Self {
        self.waiting_for = Some(waiting_for.map(str::to_string));
        self
    }

    pub fn spent(mut self, spent: Spent) -> Self {
        self.spent = Some(spent);
        self
    }

    pub fn budget(mut self, budget: Budget) -> Self {
        self.budget = Some(budget);
        self
    }

    pub fn nudges(mut self, nudges: Nudges) -> Self {
        self.nudges = Some(nudges);
        self
    }
}

/// The phase state fields `update_phase` may change ([`PHASE_STATE_FIELDS`]); a field left None is left alone.
/// `approval: Some(None)` and `result: Some(None)` clear them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhaseUpdate {
    pub status: Option<String>,
    pub executor: Option<String>,
    pub approval: Option<Option<Approval>>,
    pub result: Option<Option<PhaseResult>>,
}

impl PhaseUpdate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(mut self, status: &str) -> Self {
        self.status = Some(status.to_string());
        self
    }

    pub fn executor(mut self, executor: &str) -> Self {
        self.executor = Some(executor.to_string());
        self
    }

    pub fn approval(mut self, approval: Option<Approval>) -> Self {
        self.approval = Some(approval);
        self
    }

    pub fn result(mut self, result: Option<PhaseResult>) -> Self {
        self.result = Some(result);
        self
    }

    fn apply(&self, phase: &Phase) -> Phase {
        let mut p = phase.clone();
        if let Some(s) = &self.status {
            p.status = s.clone();
        }
        if let Some(e) = &self.executor {
            p.executor = e.clone();
        }
        if let Some(a) = &self.approval {
            p.approval = a.clone();
        }
        if let Some(r) = &self.result {
            p.result = r.clone();
        }
        p
    }
}

// ---- the ledger --------------------------------------------------------------------------------

/// What the ledger reads the time with (`time.time` by default; the tests pass a fixed clock).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// `os.replace`; the tests swap it for one that fails.
type ReplaceFn = fn(&Path, &Path) -> io::Result<()>;

fn os_replace(src: &Path, dst: &Path) -> io::Result<()> {
    fs::rename(src, dst)
}

#[derive(Default)]
struct State {
    cards: IndexMap<String, Card>,
    last_id: u64,
    problem: String,
}

/// The cards and their events. Thread-safe: the chief's loop and the Telegram turn share one.
pub struct Ledger {
    dir: PathBuf,
    clock: Clock,
    state: Mutex<State>,
    /// events.jsonl, parsed once per line (`events`).
    event_rows: JsonlRows,
    replace: Mutex<ReplaceFn>,
}

impl fmt::Debug for Ledger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ledger").field("dir", &self.dir).finish_non_exhaustive()
    }
}

impl Ledger {
    /// `Ledger(folder)`: the folder (`~` expanded), or [`chief_dir`] when None, read at once.
    pub fn new(folder: Option<&Path>) -> Ledger {
        Self::with_clock(folder, Arc::new(py::time_now))
    }

    /// `Ledger(folder, clock=clock)`.
    pub fn with_clock(folder: Option<&Path>, clock: Clock) -> Ledger {
        let dir = match folder {
            Some(f) => py::expanduser(&f.to_string_lossy()),
            None => chief_dir(None),
        };
        let led = Ledger {
            dir,
            clock,
            state: Mutex::new(State::default()),
            event_rows: JsonlRows::new(),
            replace: Mutex::new(os_replace),
        };
        {
            let mut st = led.state.lock();
            let cards = led.load(&mut st);
            st.cards = cards;
        }
        led
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn cards_path(&self) -> PathBuf {
        self.dir.join(CARDS_FILE)
    }

    pub fn events_path(&self) -> PathBuf {
        self.dir.join(EVENTS_FILE)
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    #[cfg(test)]
    fn set_replace(&self, f: ReplaceFn) {
        *self.replace.lock() = f;
    }

    // -- the store --

    fn ids_in_events(&self) -> u64 {
        let Ok(bytes) = fs::read(self.events_path()) else {
            return 0;
        };
        let text = String::from_utf8_lossy(&bytes);
        ID_IN_EVENTS.captures_iter(&text).map(|c| id_number(&c[1])).max().unwrap_or(0)
    }

    /// The cards file kept as cards.json.bad-<ts>: moved (nothing in it is used) or copied (some cards did not
    /// load; the next save would erase them). Returns where it is, or the file itself when that failed.
    fn aside(&self, st: &mut State, text: &str, move_: bool) -> PathBuf {
        let path = self.cards_path();
        let stamp = self.now().trunc();
        let aside = self.dir.join(format!("{CARDS_FILE}.bad-{}", py_int(stamp)));
        let highest = ID_IN_TEXT.captures_iter(text).map(|c| id_number(&c[1])).max().unwrap_or(0);
        st.last_id = st.last_id.max(highest);
        let done = if move_ {
            fs::rename(&path, &aside)
        } else {
            fs::copy(&path, &aside).and_then(|_| py::chmod_600(&aside))
        };
        match done {
            Ok(()) => aside,
            Err(_) => path,
        }
    }

    fn read_lossy(path: &Path) -> String {
        fs::read(path).map(|b| py::universal_newlines(&String::from_utf8_lossy(&b))).unwrap_or_default()
    }

    fn load(&self, st: &mut State) -> IndexMap<String, Card> {
        let mut cards: IndexMap<String, Card> = IndexMap::new();
        let path = self.cards_path();
        let mut raw: Option<Value> = None;
        let mut bad = String::new();
        match fs::read(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => bad = py::os_error_name(&e).to_string(),
            Ok(bytes) => match String::from_utf8(bytes) {
                Err(_) => bad = "UnicodeDecodeError".into(),
                Ok(text) => match py::loads(&py::universal_newlines(&text), py::NonFinite::Marker) {
                    Err(_) => bad = "JSONDecodeError".into(),
                    Ok(v) => {
                        let shaped = py::as_dict(&v)
                            .map(|d| matches!(d.get("cards"), None | Some(Value::Array(_))))
                            .unwrap_or(false);
                        if !shaped {
                            bad = "not a list of cards".into();
                        }
                        raw = Some(v);
                    }
                },
            },
        }
        if !bad.is_empty() {
            // nothing in it is used, so it is moved aside whole
            let text = Self::read_lossy(&path);
            let aside = self.aside(st, &text, true);
            let name = file_name(&aside);
            tracing::warn!(
                "chief: {} is unreadable ({}); kept as {}, starting with no cards",
                path.display(),
                bad,
                name
            );
            st.problem = format!(
                "I couldn't read my list of jobs, so I'm starting with none. The old file is kept as {} in {}.",
                name,
                parent_text(&aside)
            );
            raw = None;
        }
        if let Some(raw) = raw.as_ref().and_then(py::as_dict) {
            // an int (not a float, not a bool) in 1..10**9
            if let Some(last) = raw.get("last_id").and_then(Value::as_u64) {
                if 0 < last && last < 1_000_000_000 {
                    st.last_id = last;
                }
            }
            let mut skipped: Vec<String> = Vec::new();
            let items: &[Value] = match raw.get("cards") {
                Some(Value::Array(a)) => a,
                _ => &[],
            };
            for (i, item) in items.iter().enumerate() {
                match chief_card::from_json(item) {
                    Err(_) => {
                        // any card that does not load is set aside, never the whole list
                        let cid = py::as_dict(item).and_then(|d| d.get("id")).and_then(Value::as_str);
                        skipped.push(match cid {
                            Some(c) if chief_card::card_number(c) != 0 => c.to_string(),
                            _ => format!("#{}", i + 1),
                        });
                    }
                    Ok(card) => {
                        if !cards.contains_key(&card.id) {
                            // the first of two same ids wins
                            st.last_id = st.last_id.max(chief_card::card_number(&card.id));
                            cards.insert(card.id.clone(), card);
                        }
                    }
                }
            }
            if !skipped.is_empty() {
                let text = Self::read_lossy(&path);
                let aside = self.aside(st, &text, false);
                let name = file_name(&aside);
                let first: Vec<&str> = skipped.iter().take(10).map(String::as_str).collect();
                tracing::warn!(
                    "chief: {} stored card(s) did not load ({}); the file is kept as {}",
                    skipped.len(),
                    first.join(", "),
                    name
                );
                let n = skipped.len();
                st.problem = format!(
                    "{n} job{} in my list didn't load ({}), so I set {} aside. The whole file is kept as {} in {}.",
                    if n != 1 { "s" } else { "" },
                    first.join(", "),
                    if n != 1 { "them" } else { "it" },
                    name,
                    parent_text(&aside)
                );
            }
        }
        st.last_id = st.last_id.max(self.ids_in_events());
        cards
    }

    /// Atomic, as the Watcher saves. LedgerError, and the old file stays whole.
    fn save(&self, cards: &IndexMap<String, Card>, last_id: u64) -> Result<(), LedgerError> {
        let path = self.cards_path();
        let replace = *self.replace.lock();
        let written = (|| -> io::Result<()> {
            py::mkdir_700(&self.dir)?;
            let mut tmp = tempfile::Builder::new().prefix(".cards-").suffix(".json").tempfile_in(&self.dir)?;
            let body = json!({
                "version": 1,
                "last_id": last_id,
                "cards": cards.values().map(chief_card::to_json).collect::<Vec<_>>(),
            });
            tmp.write_all(py::dumps_indent1(&body).as_bytes())?;
            tmp.flush()?;
            let tmp = tmp.into_temp_path();
            py::chmod_600(&tmp)?;
            replace(&tmp, &path)?;
            // renamed: nothing is left to remove
            tmp.keep().map_err(|e| e.error)?;
            Ok(())
        })();
        written.map_err(|e| {
            let name = py::os_error_name(&e);
            tracing::warn!("chief: could not save the cards ({name})");
            LedgerError(format!("could not save the cards ({name})"))
        })
    }

    fn commit(&self, st: &mut State, cards: IndexMap<String, Card>, last_id: Option<u64>) -> Result<(), LedgerError> {
        let last = last_id.unwrap_or(st.last_id);
        self.save(&cards, last)?;
        st.cards = cards;
        st.last_id = last;
        Ok(())
    }

    /// What went wrong at load, said once.
    pub fn take_problem(&self) -> String {
        std::mem::take(&mut self.state.lock().problem)
    }

    // -- events --

    fn event_line(&self, kind: EventKind, cid: &str, fields: &[(&str, Value)]) -> String {
        let mut row = Map::new();
        row.insert("t".into(), json!(py::round_ndigits(self.now(), 3)));
        row.insert("e".into(), json!(kind.as_str()));
        row.insert("id".into(), json!(cid));
        for (key, value) in fields {
            if row.contains_key(*key) {
                continue;
            }
            let v = match value {
                Value::Array(items) => Value::Array(items.iter().take(20).map(scalar).collect()),
                other => scalar(other),
            };
            row.insert((*key).to_string(), v);
        }
        py::dumps_compact(&Value::Object(row)) + "\n"
    }

    /// Append one line; the caller holds the lock.
    fn append(&self, line: &str) -> bool {
        let written = (|| -> io::Result<()> {
            py::mkdir_700(&self.dir)?;
            let mut f = OpenOptions::new().append(true).create(true).mode(0o600).open(self.events_path())?;
            f.write_all(line.as_bytes())?;
            py::fsync(&f)
        })();
        match written {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("chief: an event line was not written ({})", py::os_error_name(&e));
                false
            }
        }
    }

    fn event_locked(&self, _held: &mut State, kind: EventKind, cid: &str, fields: &[(&str, Value)]) -> bool {
        let line = self.event_line(kind, cid, fields);
        self.append(&line)
    }

    /// One line appended to events.jsonl, flushed to disk. True when it is there: a caller logs a dispatch or a
    /// push BEFORE the act, and does not act on false. Never fails for IO. A field that is a list keeps its
    /// first 20 items; every text is squashed and cut to [`MAX_EVENT_TEXT`]; `t`, `e` and `id` are the line's own.
    pub fn event(&self, kind: EventKind, cid: &str, fields: &[(&str, Value)]) -> bool {
        let line = self.event_line(kind, cid, fields);
        let _held = self.state.lock();
        self.append(&line)
    }

    /// The events, oldest first (one card's with `cid`). A torn or foreign line is skipped.
    pub fn events(&self, cid: Option<&str>) -> Vec<Map<String, Value>> {
        self.event_rows
            .rows(&self.events_path())
            .into_iter()
            .filter(|row| {
                matches!(row.get("e"), Some(Value::String(e)) if EVENT_KINDS.contains(&e.as_str()))
                    && cid.is_none_or(|c| matches!(row.get("id"), Some(Value::String(i)) if i == c))
            })
            .collect()
    }

    // -- cards --

    pub fn get(&self, cid: &str) -> Option<Card> {
        self.state.lock().cards.get(cid).cloned()
    }

    /// Every card, by number.
    pub fn all(&self) -> Vec<Card> {
        let mut out: Vec<Card> = self.state.lock().cards.values().cloned().collect();
        out.sort_by_key(|c| chief_card::card_number(&c.id));
        out
    }

    /// The cards that are not done or dropped, oldest first.
    pub fn open(&self) -> Vec<Card> {
        self.all().into_iter().filter(|c| !c.terminal()).collect()
    }

    /// `ledger.new(card, why=...)`: file a parsed card under the next id (never one used before) and log
    /// `created`. Error::Value when the card already has an id. `why` is "take_on" in Python's default.
    pub fn new_card(&self, card: Card, why: &str) -> Result<Card, Error> {
        if !card.id.is_empty() {
            return Err(Error::Value("a new card has no id yet".into()));
        }
        let mut st = self.state.lock();
        let now = self.now();
        let mut n = st.last_id + 1;
        while st.cards.contains_key(&format!("c{n}")) {
            n += 1;
        }
        let filed = Card { id: format!("c{n}"), created: now, updated: now, ..card };
        let mut cards = st.cards.clone();
        cards.insert(filed.id.clone(), filed.clone());
        let trimmed = Self::trim(cards);
        self.commit(&mut st, trimmed, Some(n))?;
        let phases: Vec<Value> = filed.phases.iter().map(|p| json!(p.r#do)).collect();
        let doors: Vec<Value> = filed.phases.iter().map(|p| json!(p.door)).collect();
        self.event_locked(
            &mut st,
            EventKind::Created,
            &filed.id,
            &[
                ("rev", json!(filed.rev)),
                ("kind", json!(filed.kind)),
                ("status", json!(filed.status)),
                ("phases", Value::Array(phases)),
                ("doors", Value::Array(doors)),
                ("why", json!(why)),
            ],
        );
        Ok(filed)
    }

    fn trim(cards: IndexMap<String, Card>) -> IndexMap<String, Card> {
        if cards.len() <= MAX_CARDS {
            return cards;
        }
        let mut finished: Vec<&Card> = cards.values().filter(|c| c.terminal()).collect();
        finished.sort_by(|a, b| a.updated.partial_cmp(&b.updated).unwrap_or(std::cmp::Ordering::Equal));
        let gone: Vec<String> = finished.iter().take(cards.len() - MAX_CARDS).map(|c| c.id.clone()).collect();
        let mut out = cards;
        for id in gone {
            out.shift_remove(&id);
        }
        out
    }

    fn put(&self, st: &mut State, card: Card) -> Result<Card, LedgerError> {
        let mut cards = st.cards.clone();
        cards.insert(card.id.clone(), card.clone());
        self.commit(st, cards, None)?;
        Ok(card)
    }

    /// The owner's correction as a new revision (chief_card::revise), saved and logged with its reason.
    /// Ok(Err(Refusal)) is Python's returned Refusal; Error::Value is the ValueError chief_card's parse lets out.
    pub fn revise(
        &self,
        cid: &str,
        patch: &Map<String, Value>,
        why: &str,
        said: &str,
        live: &Live,
        home: Option<&Path>,
    ) -> Result<Result<Card, Refusal>, Error> {
        let mut st = self.state.lock();
        let Some(card) = st.cards.get(cid).cloned() else {
            return Ok(Err(Refusal::new(format!("there is no card {cid}"))));
        };
        let new = match chief_card::revise(&card, patch, said, live, self.now(), home) {
            Ok(c) => c,
            Err(chief_card::ParseError::Refusal(r)) => return Ok(Err(r)),
            Err(chief_card::ParseError::ValueError(e)) => return Err(Error::Value(e.0)),
        };
        let new = self.put(&mut st, new)?;
        let doors: Vec<Value> = new.phases.iter().map(|p| json!(p.door)).collect();
        self.event_locked(
            &mut st,
            EventKind::Revised,
            cid,
            &[("rev", json!(new.rev)), ("why", json!(why)), ("doors", Value::Array(doors))],
        );
        Ok(Ok(new))
    }

    /// The replanner's new phases (chief_replan.apply), as one new revision, under the lock, or not at all: the
    /// card must still be exactly `expect` (what the replan was asked about; an owner's drop or change meanwhile
    /// wins). rev+1 cancels every approval, as the owner's own revision does. None when it changed.
    pub fn replan(
        &self,
        cid: &str,
        expect: &Card,
        phases: Vec<Phase>,
        event_fields: &[(&str, Value)],
    ) -> Result<Option<Card>, LedgerError> {
        let mut st = self.state.lock();
        let card = match st.cards.get(cid) {
            Some(c) if c == expect && !c.terminal() => c.clone(),
            _ => return Ok(None),
        };
        let phases = phases.into_iter().map(|p| Phase { approval: None, ..p }).collect();
        let next = Card { rev: card.rev + 1, updated: self.now(), phases, ..card };
        let new = self.put(&mut st, next)?;
        let mut fields: Vec<(&str, Value)> = vec![("rev", json!(new.rev))];
        fields.extend(event_fields.iter().cloned());
        self.event_locked(&mut st, EventKind::Replanned, cid, &fields);
        Ok(Some(new))
    }

    /// A done or dropped card back as a new revision (chief_card::reopen), logged. Error::Key when there is no
    /// such card.
    pub fn reopen(&self, cid: &str, why: &str) -> Result<Card, Error> {
        let mut st = self.state.lock();
        let card = Self::need(&st, cid)?;
        let new = self.put(&mut st, chief_card::reopen(&card, self.now()))?;
        self.event_locked(&mut st, EventKind::Reopened, cid, &[("rev", json!(new.rev)), ("why", json!(why))]);
        Ok(new)
    }

    fn need(st: &State, cid: &str) -> Result<Card, Error> {
        st.cards.get(cid).cloned().ok_or_else(|| Error::Key(cid.to_string()))
    }

    /// Change the card's state ([`STATE_FIELDS`] only), saved and logged. A done or dropped card is reopened,
    /// never moved back by a state change.
    pub fn update(&self, cid: &str, why: &str, fields: StateUpdate) -> Result<Card, Error> {
        if let Some(s) = &fields.status {
            if !STATUSES.contains(&s.as_str()) {
                return Err(Error::Value(format!("unknown status {}", py::repr_str(s))));
            }
        }
        if let Some(Some(w)) = &fields.waiting_for {
            if !WAITING_FOR.contains(&w.as_str()) {
                return Err(Error::Value(format!("unknown waiting_for {}", py::repr_str(w))));
            }
        }
        let mut st = self.state.lock();
        let card = Self::need(&st, cid)?;
        if card.terminal() && fields.status.as_ref().is_some_and(|s| *s != card.status) {
            return Err(Error::Value(format!("{cid} is {}: reopen it instead", card.status)));
        }
        let mut next = Card { updated: self.now(), ..card };
        if let Some(s) = fields.status {
            next.status = s;
        }
        if let Some(w) = fields.waiting_for {
            next.waiting_for = w;
        }
        if let Some(s) = fields.spent {
            next.spent = s;
        }
        if let Some(b) = fields.budget {
            next.budget = b;
        }
        if let Some(n) = fields.nudges {
            next.nudges = n;
        }
        let new = self.put(&mut st, next)?;
        self.event_locked(
            &mut st,
            EventKind::State,
            cid,
            &[
                ("rev", json!(new.rev)),
                ("status", json!(new.status)),
                ("waiting_for", json!(new.waiting_for)),
                ("why", json!(why)),
            ],
        );
        Ok(new)
    }

    fn check_phase_fields(fields: &PhaseUpdate) -> Result<(), Error> {
        if let Some(s) = &fields.status {
            if !PHASE_STATUSES.contains(&s.as_str()) {
                return Err(Error::Value(format!("unknown phase status {}", py::repr_str(s))));
            }
        }
        Ok(())
    }

    fn update_phase_locked(
        &self,
        st: &mut State,
        cid: &str,
        n: usize,
        why: &str,
        fields: &PhaseUpdate,
    ) -> Result<Card, Error> {
        let card = Self::need(st, cid)?;
        if card.terminal() {
            return Err(Error::Value(format!("{cid} is {}: reopen it instead", card.status)));
        }
        if card.phase(n).is_none() {
            return Err(Error::Key(format!("{cid} has no phase {n}")));
        }
        let phases = card.phases.iter().map(|p| if p.n == n { fields.apply(p) } else { p.clone() }).collect();
        let new = self.put(st, Card { phases, updated: self.now(), ..card })?;
        let phase_status = new.phase(n).map(|p| p.status.clone()).unwrap_or_default();
        self.event_locked(
            st,
            EventKind::State,
            cid,
            &[("rev", json!(new.rev)), ("phase", json!(n)), ("phase_status", json!(phase_status)), ("why", json!(why))],
        );
        Ok(new)
    }

    /// Change one phase's state ([`PHASE_STATE_FIELDS`] only), saved and logged. A done or dropped card's phases
    /// do not change (Error::Value, as `update`): a step never starts on a finished card.
    pub fn update_phase(&self, cid: &str, n: usize, why: &str, fields: PhaseUpdate) -> Result<Card, Error> {
        Self::check_phase_fields(&fields)?;
        let mut st = self.state.lock();
        self.update_phase_locked(&mut st, cid, n, why, &fields)
    }

    /// Start one phase, all under the lock, or not at all: the card is open and still at `rev`, the phase is
    /// queued, and a one-way phase holds the approval `token` for this rev whose digest is `what` (the act as it
    /// would run now). Then the `dispatched` line is written (fsynced) and the phase set running with its approval
    /// spent. None when any of that does not hold or the line did not reach the disk; the caller then starts
    /// nothing. LedgerError when the line is written and the save fails (the caller logs a result).
    /// The reviewer (2026-09-29) found the dispatcher moving a card from a copy read before an await: a drop or a
    /// change made meanwhile still dispatched the old yes.
    #[allow(clippy::too_many_arguments)]
    pub fn claim(
        &self,
        cid: &str,
        n: usize,
        rev: i64,
        token: &str,
        what: &str,
        executor: &str,
        event_fields: &[(&str, Value)],
    ) -> Result<Option<Card>, Error> {
        let mut st = self.state.lock();
        let Some(card) = st.cards.get(cid).cloned() else {
            return Ok(None);
        };
        let Some(phase) = card.phase(n) else {
            return Ok(None);
        };
        if card.terminal() || card.rev != rev || phase.status != "queued" {
            return Ok(None);
        }
        if phase.door == "one_way" {
            let ok = match &phase.approval {
                Some(a) => !token.is_empty() && a.token == token && a.rev == card.rev && !a.what.is_empty() && a.what == what,
                None => false,
            };
            if !ok {
                return Ok(None);
            }
        }
        let mut fields: Vec<(&str, Value)> = vec![
            ("phase", json!(n)),
            ("rev", json!(rev)),
            ("token", json!(token)),
            ("executor", json!(executor)),
        ];
        fields.extend(event_fields.iter().cloned());
        if !self.event_locked(&mut st, EventKind::Dispatched, cid, &fields) {
            return Ok(None);
        }
        let running = PhaseUpdate::new().status("running").executor(executor).approval(None);
        self.update_phase_locked(&mut st, cid, n, "dispatched", &running).map(Some)
    }

    // -- reading it back --

    /// Open cards whose cue time or next nudge has come, earliest first.
    pub fn due(&self, now: f64) -> Vec<Card> {
        let mut out: Vec<(f64, Card)> = Vec::new();
        for c in self.open() {
            if c.status == "proposed" {
                continue;
            }
            let mut times = Vec::new();
            let cue_at = c.cue.as_ref().and_then(|cue| chief_card::at_time(cue.at.as_deref()));
            if let Some(at) = cue_at {
                if c.nudges.sent == 0 {
                    if let Some(ts) = py::naive_timestamp(&at) {
                        times.push(ts);
                    }
                }
            }
            if let Some(next) = c.nudges.next_at {
                times.push(next);
            }
            let ready = times.into_iter().filter(|t| *t <= now).fold(None, |m: Option<f64>, t| {
                Some(match m {
                    Some(x) if x <= t => x,
                    _ => t,
                })
            });
            if let Some(t) = ready {
                out.push((t, c));
            }
        }
        out.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| chief_card::card_number(&a.1.id).cmp(&chief_card::card_number(&b.1.id)))
        });
        out.into_iter().map(|(_, c)| c).collect()
    }

    /// The line the brain sees every turn, at most [`TURN_LINE_CHARS`]; "" when nothing is open.
    /// "Open jobs: 2 (c12 waiting on your Go: order the picked desk; c13 researching: desks). Due today: …"
    pub fn for_turn(&self, now: f64) -> String {
        let today = day(now);
        let cards = self.open();
        let jobs: Vec<&Card> = cards.iter().filter(|c| c.kind == "job" && c.status != "proposed").collect();
        let mut parts: Vec<String> = Vec::new();
        if !jobs.is_empty() {
            let described: Vec<String> = jobs.iter().map(|c| Self::describe(c)).collect();
            parts.push(format!("Open jobs: {} ({}).", jobs.len(), described.join("; ")));
        }
        let due: Vec<&Card> = cards
            .iter()
            .filter(|c| {
                // `today` is "" only where Python's fromtimestamp raises: then nothing is due today
                c.kind == "commitment" && c.status != "proposed" && !today.is_empty() && {
                    let deadline = py::take_chars(c.deadline.as_deref().unwrap_or(""), 10);
                    let cue = py::take_chars(c.cue.as_ref().and_then(|q| q.at.as_deref()).unwrap_or(""), 10);
                    today == deadline || today == cue
                }
            })
            .collect();
        if !due.is_empty() {
            let named: Vec<String> = due.iter().map(|c| format!("{} ({})", c.title, c.id)).collect();
            parts.push(format!("Due today: {}.", named.join(", ")));
        }
        let proposed = cards.iter().filter(|c| c.status == "proposed").count();
        if proposed > 0 {
            parts.push(format!("Proposed: {proposed}."));
        }
        let line = parts.join(" ");
        if py::len(&line) > TURN_LINE_CHARS {
            let cut = py::take_chars(&line, TURN_LINE_CHARS - 1);
            let kept = match cut.rfind(' ') {
                Some(i) => &cut[..i],
                None => cut,
            };
            return format!("{kept}…");
        }
        line
    }

    /// One open card for the turn line: what it waits on, or what it is doing (Python's `_describe`).
    pub fn describe(c: &Card) -> String {
        let waiting_for = c.waiting_for.as_deref();
        if c.status == "waiting" && waiting_for == Some("go") {
            let pending = c.phases.iter().find(|p| p.door == "one_way" && p.status == "queued");
            return format!("{} waiting on your Go: {}", c.id, pending.map(|p| p.goal.as_str()).unwrap_or(&c.title));
        }
        if c.status == "waiting" {
            if let Some(word) = waiting_for.and_then(waiting_word) {
                return format!("{} waiting on your {}: {}", c.id, word, c.title);
            }
            return format!("{} waiting for the Mac: {}", c.id, c.title);
        }
        if c.status == "unverified" {
            return format!("{} unverified: {}", c.id, c.title);
        }
        let current = c.phases.iter().find(|p| p.status == "queued" || p.status == "running");
        let what = current.and_then(|p| doing(&p.r#do)).unwrap_or("working");
        format!("{} {}: {}", c.id, what, c.title)
    }

    /// Open cards the owner's words are about: they share two content words with the card's title, purpose and
    /// next action (one, when the card has only one). The owner's own action cancels queued nudges.
    pub fn matching(&self, words: &str) -> Vec<Card> {
        let said = content_words(words);
        self.open()
            .into_iter()
            .filter(|c| {
                let next_action = c.cue.as_ref().map(|q| q.next_action.as_str()).unwrap_or("");
                let mine = content_words(&[c.title.as_str(), c.purpose.as_str(), next_action].join(" "));
                !mine.is_empty() && said.intersection(&mine).count() >= mine.len().min(2)
            })
            .collect()
    }

    /// Remove these cards and their event lines (a forget layer's helper). Ids stay used. Returns the count of
    /// cards removed.
    pub fn redact<I, S>(&self, ids: I) -> Result<usize, LedgerError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let gone: HashSet<String> = ids.into_iter().map(|i| i.as_ref().to_string()).collect();
        let mut st = self.state.lock();
        let removed = gone.iter().filter(|i| st.cards.contains_key(*i)).count();
        if removed > 0 {
            let kept: IndexMap<String, Card> =
                st.cards.iter().filter(|(k, _)| !gone.contains(*k)).map(|(k, v)| (k.clone(), v.clone())).collect();
            self.commit(&mut st, kept, None)?;
        }
        let events_path = self.events_path();
        let text = match fs::read(&events_path) {
            Ok(b) => py::universal_newlines(&String::from_utf8_lossy(&b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(LedgerError(format!("could not read the events ({})", py::os_error_name(&e)))),
        };
        let lines = py::splitlines_keepends(&text);
        let keep: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|line| match py::loads(line, py::NonFinite::Null) {
                Err(_) => true,
                Ok(row) => !matches!(row.get("id"), Some(Value::String(i)) if gone.contains(i)),
            })
            .collect();
        if keep.len() != lines.len() {
            let replace = *self.replace.lock();
            let rewritten = (|| -> io::Result<()> {
                let mut tmp =
                    tempfile::Builder::new().prefix(".events-").suffix(".jsonl").tempfile_in(&self.dir)?;
                tmp.write_all(keep.concat().as_bytes())?;
                tmp.flush()?;
                let tmp = tmp.into_temp_path();
                py::chmod_600(&tmp)?;
                replace(&tmp, &events_path)?;
                tmp.keep().map_err(|e| e.error)?;
                Ok(())
            })();
            if let Err(e) = rewritten {
                return Err(LedgerError(format!("could not rewrite the events ({})", py::os_error_name(&e))));
            }
        }
        self.event_locked(&mut st, EventKind::Redacted, "", &[("count", json!(removed))]);
        Ok(removed)
    }
}

/// `int(x)` of a finite float as text (any size, as Python's int is unbounded).
fn py_int(x: f64) -> String {
    if x.is_finite() && x.abs() < 9.0e15 {
        format!("{}", x as i64)
    } else {
        format!("{x:.0}")
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `str(path.parent)`.
fn parent_text(p: &Path) -> String {
    match p.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        _ => ".".into(),
    }
}

#[cfg(test)]
mod tests;
