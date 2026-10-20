//! What eve spends, every day, on every model it calls. Port of `eve_bridge/spend.py`.
//!
//! Owner, 2026-09-24: "build in a dashboard that tracks costs (ik some of them are other keys but can u track
//! pricing, i want to see how much i am spending everyday)". Before this, only the app builder kept a ledger
//! (miniapp.SpendLedger), the computer agent wrote a bill into its run log, and everything else — the chat, the
//! voice, thinking, search, the camera, memory — spent without a trace.
//!
//! This module is the one meter. Every place eve spends money calls [`record`] (or one of the `record_*` helpers
//! that price a provider's usage object), and one line goes into a per-day JSONL file under
//! `~/.config/eve-bridge/spend/` (`EVE_SPEND_DIR` moves it), named by the local date:
//!
//! ```text
//! {"t":1790000000,"p":"openrouter","m":"gpt-6-luna","f":"chat","usd":0.00042,"src":"reported","tok":{"in":3100,"cached":2900,"out":80}}
//! ```
//!
//! * `p` is `openrouter` for every model call eve makes since 2026-10-08 (owner: one key, docs/openrouter.md), the
//!   Anthropic and embedding ones included; `openai` is left only for the desk voice's Live sessions
//!   ([`record_live`]), which still run on OpenAI until their rebuild. Older rows keep the provider they were
//!   written with. `m` is the call site's model name ("gpt-6-luna"), so pricing reads it whatever the prefix on
//!   the wire.
//! * `src` is `reported` when the provider's reply carried its own cost (OpenRouter's `usage.cost`), `priced` when
//!   eve priced the tokens at eve-pricing's grounded rates.
//! * A model with no rate is recorded with `usd: null` and `unpriced: true` (logged once per model): a wrong figure
//!   is worse than a blank one, so nothing is guessed. The dashboard counts those calls separately.
//! * Tiny records, never a prompt, never an answer: provider, model, feature, dollars, token counts, an optional
//!   short note, and the chief's card id when the call was made for one. Nothing else leaves the call site.
//! * [`job`]`("c7")` tags every row written inside it with `"job": "c7"` (chief design 3.2, 2026-09-29: a context
//!   value, so no call site has to pass a card id; [`carry`] / [`to_thread`] carry it into worker threads, and
//!   [`in_job`] scopes it over an async task). [`job_total`] sums one card's rows: what the chief's budget reads.
//! * [`record`] never fails and is thread-safe: a failed write is one log line, never a failed turn.
//!
//! The reader ([`day_rows`], [`totals`], [`summary`]) aggregates by day, provider, feature and model for the Mini
//! App's Spending view and the `/spend` command. The providers' own figures (OpenRouter, and OpenAI and Anthropic
//! when an admin key is set) are kept by spend_sync beside this ledger, never mixed into it.

use std::borrow::Borrow;
use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::future::Future;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone};
use indexmap::IndexMap;
use parking_lot::{Mutex, RwLock};
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};

pub const DEFAULT_DIR: &str = "~/.config/eve-bridge/spend";
/// Older day files are left alone; the reader never looks further back than this.
pub const KEEP_DAYS: i64 = 400;

// The features, in plain words: what the owner sees in the split. One name per kind of work.
pub const CHAT: &str = "chat";
pub const THINKING: &str = "thinking";
pub const VOICE: &str = "voice";
pub const TASKS: &str = "browser & Mac tasks";
pub const SEARCH: &str = "search";
pub const JEV: &str = "jev";
pub const APPS: &str = "app builder";
pub const MEMORY: &str = "memory";
pub const CAMERA: &str = "camera";
pub const EXPLORING: &str = "exploring";
pub const TRANSCRIPTION: &str = "transcription";
pub const ROOM_NOTES: &str = "room notes";
pub const LESSONS: &str = "lessons";
pub const CODEX: &str = "codex";
pub const WATCH: &str = "watching";
pub const MEET: &str = "meet notes";
pub const CALLS: &str = "phone calls";
pub const CHIEF: &str = "chief of staff";

static JOB_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^c[1-9]\d{0,8}$").expect("job id regex"));

static LOCK: Mutex<()> = Mutex::new(());
static DIR_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);
static WARNED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

// ---- the ledger's folder --------------------------------------------------------------------------------------

/// Point the ledger somewhere else (tests), or back to the default with `None`.
pub fn set_dir(path: Option<PathBuf>) {
    *DIR_OVERRIDE.write() = path;
}

/// `set_dir`'s folder, else `EVE_SPEND_DIR`, else `~/.config/eve-bridge/spend`.
pub fn spend_dir() -> PathBuf {
    if let Some(p) = DIR_OVERRIDE.read().clone() {
        return p;
    }
    spend_dir_from(std::env::var("EVE_SPEND_DIR").ok().as_deref())
}

/// The folder for a given `EVE_SPEND_DIR` value (blank means the default), `~` expanded.
pub fn spend_dir_from(env_value: Option<&str>) -> PathBuf {
    let raw = env_value.unwrap_or("").trim();
    expanduser(if raw.is_empty() { DEFAULT_DIR } else { raw })
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from).or_else(dirs::home_dir)
}

/// `Path.expanduser` for `~` and `~/...` (the forms eve uses).
fn expanduser(raw: &str) -> PathBuf {
    if raw == "~" {
        return home().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(h) = home() {
            return h.join(rest);
        }
    }
    PathBuf::from(raw)
}

// ---- the chief's card tag ---------------------------------------------------------------------------------------
//
// Python keeps it in a contextvar, which follows a task and is copied into `asyncio.to_thread`. Rust has two
// homes for it: a thread-local (set by the `job` guard, for synchronous code) and a tokio task-local (set by
// `in_job`, which follows an async task across worker threads). Each set takes a fresh sequence number, so the
// innermost set wins whichever home it lives in, exactly as nested `with spend.job(...)` blocks do.

#[derive(Clone, Debug)]
struct Tag {
    seq: u64,
    id: String,
}

static SEQ: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static THREAD_JOB: RefCell<Option<Tag>> = const { RefCell::new(None) };
}

tokio::task_local! {
    static TASK_JOB: Tag;
}

/// The tag a card id sets: itself when it is `c<N>`, else "" (tags nothing). Python's `$` also matches before a
/// trailing newline, so "c7\n" is accepted as it is there.
fn clean_card_id(card_id: &str) -> String {
    let core = card_id.strip_suffix('\n').unwrap_or(card_id);
    if JOB_ID.is_match(card_id) || JOB_ID.is_match(core) {
        card_id.to_string()
    } else {
        String::new()
    }
}

fn new_tag(card_id: &str) -> Tag {
    Tag { seq: SEQ.fetch_add(1, Ordering::Relaxed), id: clean_card_id(card_id) }
}

fn current_tag() -> Option<Tag> {
    let thread = THREAD_JOB.with(|t| t.borrow().clone());
    let task = TASK_JOB.try_with(|t| t.clone()).ok();
    match (thread, task) {
        (Some(a), Some(b)) => Some(if a.seq > b.seq { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// Restores the tag that was current before [`job`] when dropped (`_JOB.reset(token)`).
#[must_use = "the job tag lasts only while the guard is alive"]
pub struct JobGuard {
    previous: Option<Tag>,
    // A thread-local guard must be dropped on the thread that made it.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        THREAD_JOB.with(|t| *t.borrow_mut() = previous);
    }
}

/// Every spend row written while the guard lives, on this thread or a thread it hands off to with [`carry`] /
/// [`to_thread`], carries `"job": card_id`. A card id that is not `c<N>` tags nothing. For async code that awaits
/// inside the block, use [`in_job`]: a thread-local guard does not follow a task across worker threads.
pub fn job(card_id: &str) -> JobGuard {
    let tag = new_tag(card_id);
    let previous = THREAD_JOB.with(|t| t.borrow_mut().replace(tag));
    JobGuard { previous, _not_send: std::marker::PhantomData }
}

/// Run `fut` with every spend row it writes tagged `"job": card_id` (the async form of [`job`]).
pub async fn in_job<F: Future>(card_id: &str, fut: F) -> F::Output {
    TASK_JOB.scope(new_tag(card_id), fut).await
}

/// The card the current call is made for ("c7"), or "".
pub fn current_job() -> String {
    current_tag().map(|t| t.id).unwrap_or_default()
}

/// `contextvars.copy_context().run`: wrap `f` so it runs with the caller's job tag on whatever thread runs it.
pub fn carry<F, R>(f: F) -> impl FnOnce() -> R + Send + 'static
where
    F: FnOnce() -> R + Send + 'static,
{
    let tag = current_tag();
    move || {
        let previous = THREAD_JOB.with(|t| std::mem::replace(&mut *t.borrow_mut(), tag));
        // Restores the worker thread's own tag even if `f` panics.
        let _restore = JobGuard { previous, _not_send: std::marker::PhantomData };
        f()
    }
}

/// `asyncio.to_thread`: run blocking `f` on tokio's blocking pool, carrying the job tag.
pub async fn to_thread<F, R>(f: F) -> R
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    match tokio::task::spawn_blocking(carry(f)).await {
        Ok(out) => out,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

// ---- small Python-shaped helpers --------------------------------------------------------------------------------

/// Log once per key for the life of the process. Returns whether it logged.
fn once(key: &str, msg: impl FnOnce() -> String) -> bool {
    let fresh = WARNED.lock().insert(key.to_string());
    if fresh {
        tracing::warn!("{}", msg());
    }
    fresh
}

/// `_num`: a positive JSON integer (never a bool or a float), else 0. Integers past i64 saturate.
fn num(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i.max(0)
            } else if n.as_u64().is_some() {
                i64::MAX
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// Python truthiness of a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str(value)` for a JSON value as Python would print the decoded object.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(u)) => u.to_string(),
            _ => py_float_repr(n.as_f64().unwrap_or(0.0)),
        },
        other => other.to_string(),
    }
}

/// A JSON number, or a bool as Python's `isinstance(x, (int, float))` sees it (True is 1).
fn int_or_float_with_bool(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Python's `round(x, n)` (correctly rounded, ties to even on the exact value).
fn py_round(x: f64, digits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// Python's `repr(float)`: the shortest round-trip digits, scientific below 1e-4 and from 1e16.
pub fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let sci = format!("{:e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if x.is_sign_negative() { "-" } else { "" };
    if !(-4..16).contains(&exp) {
        let (head, tail) = digits.split_at(1);
        let mant = if tail.is_empty() { head.to_string() } else { format!("{head}.{tail}") };
        let esign = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{mant}e{esign}{:02}", exp.abs());
    }
    let n = digits.len() as i32;
    if exp < 0 {
        format!("{sign}0.{}{}", "0".repeat((-exp - 1) as usize), digits)
    } else if exp + 1 >= n {
        format!("{sign}{}{}.0", digits, "0".repeat((exp + 1 - n) as usize))
    } else {
        let (a, b) = digits.split_at((exp + 1) as usize);
        format!("{sign}{a}.{b}")
    }
}

/// A JSON string as Python's `json.dumps` writes it (`ensure_ascii`: everything outside space..~ escaped).
fn py_json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// Python's `str.isspace` for `str.split()`: Unicode whitespace plus the ASCII separators 0x1c..0x1f.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The Python exception name an OS error would raise, for the one log line a failed write leaves.
fn os_error_name(e: &std::io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EEXIST) => "FileExistsError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::EACCES) | Some(libc::EPERM) => "PermissionError",
        _ => "OSError",
    }
}

/// The local calendar day of an epoch time (`datetime.fromtimestamp(ts).date()`).
fn local_day(ts: f64) -> Option<NaiveDate> {
    if !ts.is_finite() {
        return None;
    }
    let secs = ts.floor();
    if secs < i64::MIN as f64 || secs > i64::MAX as f64 {
        return None;
    }
    let nanos = ((ts - secs) * 1e9).round().clamp(0.0, 999_999_999.0) as u32;
    // `datetime` covers years 1 to 9999; past that Python raises and nothing is written.
    Local
        .timestamp_opt(secs as i64, nanos)
        .earliest()
        .map(|d| d.date_naive())
        .filter(|d| (1..=9999).contains(&d.year()))
}

fn now_epoch() -> f64 {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    d.as_secs_f64()
}

// ---- the writer -------------------------------------------------------------------------------------------------

/// The optional parts of [`record`]: token counts, `source` ("reported" or anything else for "priced"), a short
/// note, and the epoch time (default now).
#[derive(Debug, Clone, Default)]
pub struct RecordOpts {
    pub tokens: Vec<(String, Value)>,
    pub source: String,
    pub note: String,
    pub now: Option<f64>,
}

impl RecordOpts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tokens<K: Into<String>>(mut self, tokens: impl IntoIterator<Item = (K, Value)>) -> Self {
        self.tokens = tokens.into_iter().map(|(k, v)| (k.into(), v)).collect();
        self
    }

    pub fn source(mut self, source: &str) -> Self {
        self.source = source.to_string();
        self
    }

    pub fn note(mut self, note: &str) -> Self {
        self.note = note.to_string();
        self
    }

    pub fn now(mut self, now: f64) -> Self {
        self.now = Some(now);
        self
    }
}

#[derive(Debug)]
enum WriteError {
    Value(&'static str),
    Io(std::io::Error),
}

/// The ledger: the module-level functions use the configured folder; tests and tools can point one elsewhere.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    dir: Option<PathBuf>,
}

impl Ledger {
    /// A ledger in `dir`.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: Some(dir.into()) }
    }

    /// This ledger's folder ([`spend_dir`] unless it was made with [`Ledger::at`]).
    pub fn dir(&self) -> PathBuf {
        self.dir.clone().unwrap_or_else(spend_dir)
    }

    /// One spend line in today's ledger. `usd` `None` means "not priced" (flagged, never guessed). Never fails.
    pub fn record(&self, provider: &str, model: &str, feature: &str, usd: Option<f64>, opts: RecordOpts) {
        if let Err(e) = self.try_record(provider, model, feature, usd, &opts) {
            let name = match &e {
                WriteError::Value(n) => *n,
                WriteError::Io(io) => os_error_name(io),
            };
            once("write", || format!("spend: a spend line was not written ({name})"));
        }
    }

    fn try_record(
        &self,
        provider: &str,
        model: &str,
        feature: &str,
        usd: Option<f64>,
        opts: &RecordOpts,
    ) -> Result<(), WriteError> {
        let ts = opts.now.unwrap_or_else(now_epoch);
        if ts.is_nan() {
            return Err(WriteError::Value("ValueError"));
        }
        if ts.is_infinite() {
            return Err(WriteError::Value("OverflowError"));
        }
        let p = truncate_chars(if provider.is_empty() { "?" } else { provider }, 32);
        let m = truncate_chars(if model.is_empty() { "?" } else { model }, 64);
        let f = truncate_chars(if feature.is_empty() { "other" } else { feature }, 32);

        let mut line = String::with_capacity(160);
        line.push_str(&format!("{{\"t\":{},\"p\":", ts.trunc() as i64));
        py_json_str(&p, &mut line);
        line.push_str(",\"m\":");
        py_json_str(&m, &mut line);
        line.push_str(",\"f\":");
        py_json_str(&f, &mut line);
        match usd {
            Some(x) if !x.is_nan() && x >= 0.0 => {
                line.push_str(",\"usd\":");
                line.push_str(&py_float_repr(py_round(x, 8)));
            }
            _ => {
                line.push_str(",\"usd\":null,\"unpriced\":true");
                if opts.note.is_empty() {
                    once(&format!("unpriced:{p}:{m}"), || {
                        format!("spend: no price for {p} model {m} ({f}); recorded unpriced")
                    });
                }
            }
        }
        line.push_str(if opts.source == "reported" { ",\"src\":\"reported\"" } else { ",\"src\":\"priced\"" });
        // As a dict: a repeated key keeps its first place and its last value; then only positive counts stay.
        let mut given: IndexMap<&str, &Value> = IndexMap::new();
        for (k, v) in &opts.tokens {
            given.insert(k.as_str(), v);
        }
        let tok: IndexMap<&str, i64> =
            given.into_iter().map(|(k, v)| (k, num(Some(v)))).filter(|(_, n)| *n != 0).collect();
        if !tok.is_empty() {
            line.push_str(",\"tok\":{");
            for (i, (k, n)) in tok.iter().enumerate() {
                if i > 0 {
                    line.push(',');
                }
                py_json_str(k, &mut line);
                line.push_str(&format!(":{n}"));
            }
            line.push('}');
        }
        if !opts.note.is_empty() {
            let joined = opts.note.split(py_isspace).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
            line.push_str(",\"note\":");
            py_json_str(&truncate_chars(&joined, 80), &mut line);
        }
        let tag = current_job();
        if !tag.is_empty() {
            line.push_str(",\"job\":");
            py_json_str(&tag, &mut line);
        }
        line.push_str("}\n");

        let day = local_day(ts).ok_or(WriteError::Value("OverflowError"))?;
        let folder = self.dir();
        let _held = LOCK.lock();
        std::fs::create_dir_all(&folder).map_err(WriteError::Io)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(folder.join(format!("{}.jsonl", day.format("%Y-%m-%d"))))
            .map_err(WriteError::Io)?;
        file.write_all(line.as_bytes()).map_err(WriteError::Io)?;
        Ok(())
    }

    /// One Responses API reply (its JSON; an SDK object through [`sdk_dump`]). Priced at
    /// `eve_pricing::OPENAI_RATES`, plus any hosted web searches it made; a reply without usage is recorded
    /// unpriced. Never fails.
    pub fn record_response(&self, feature: &str, response: &Value, model: &str, provider: &str, searches: Option<i64>) {
        let body = as_dict(response);
        let usage = body.get("usage").and_then(Value::as_object);
        let name = model_name(model, &body);
        let calls = match searches {
            Some(n) => n.max(0),
            None => web_search_calls(body.get("output")),
        };
        let Some(usage) = usage else {
            self.record(provider, &name, feature, None, RecordOpts::new().note("no usage in the reply"));
            return;
        };
        let usage_value = Value::Object(usage.clone());
        let t = eve_pricing::responses_tokens(&usage_value);
        let mut tokens = vec![
            ("in".to_string(), Value::from(t.input)),
            ("cached".to_string(), Value::from(t.cached)),
            ("out".to_string(), Value::from(t.out)),
        ];
        if let Some(cost) = int_or_float_with_bool(usage.get("cost")) {
            self.record(provider, &name, feature, Some(cost), RecordOpts::new().tokens(tokens).source("reported"));
            return;
        }
        let mut usd = eve_pricing::estimate_openai_cost(&name, &usage_value);
        if let Some(u) = usd.as_mut() {
            if calls != 0 {
                *u += calls as f64 * eve_pricing::OPENAI_WEB_SEARCH_PER_CALL;
            }
        }
        if calls != 0 {
            tokens.push(("searches".to_string(), Value::from(calls)));
        }
        self.record(provider, &name, feature, usd, RecordOpts::new().tokens(tokens));
    }

    /// One Chat Completions reply (OpenRouter's or OpenAI's): the provider's `usage.cost` when it is there, else
    /// the tokens at `eve_pricing::OPENAI_RATES` (an OpenAI model behind a prefix), else unpriced.
    pub fn record_chat_completion(&self, feature: &str, payload: &Value, model: &str, provider: &str) {
        let body = as_dict(payload);
        let empty = Map::new();
        let usage = body.get("usage").and_then(Value::as_object).unwrap_or(&empty);
        let name = model_name(model, &body);
        let details = usage.get("prompt_tokens_details").and_then(Value::as_object).unwrap_or(&empty);
        let (tin, cached, tout) =
            (num(usage.get("prompt_tokens")), num(details.get("cached_tokens")), num(usage.get("completion_tokens")));
        let tokens = vec![
            ("in".to_string(), Value::from(tin)),
            ("cached".to_string(), Value::from(cached)),
            ("out".to_string(), Value::from(tout)),
        ];
        if let Some(cost) = int_or_float_with_bool(usage.get("cost")) {
            self.record(provider, &name, feature, Some(cost), RecordOpts::new().tokens(tokens).source("reported"));
            return;
        }
        let usd = if usage.is_empty() {
            None
        } else {
            eve_pricing::estimate_openai_cost(
                &name,
                &serde_json::json!({"input_tokens": tin, "output_tokens": tout,
                                    "input_tokens_details": {"cached_tokens": cached}}),
            )
        };
        self.record(provider, &name, feature, usd, RecordOpts::new().tokens(tokens));
    }

    /// One Messages API usage: OpenRouter's reported cost when it is there, else `eve_pricing::ANTHROPIC_RATES`.
    /// Returns the dollars recorded (`None`: unpriced).
    pub fn record_anthropic(&self, feature: &str, model: &str, usage: &Value, provider: &str) -> Option<f64> {
        let get = |name: &str| usage.as_object().and_then(|o| o.get(name)).cloned().unwrap_or(Value::Null);
        let reported = reported_cost(usage);
        let usd = reported.or_else(|| eve_pricing::estimate_anthropic_cost(model, usage));
        self.record(
            provider,
            model,
            feature,
            usd,
            RecordOpts::new()
                .tokens([
                    ("in", get("input_tokens")),
                    ("out", get("output_tokens")),
                    ("cache_read", get("cache_read_input_tokens")),
                    ("cache_write", get("cache_creation_input_tokens")),
                ])
                .source(if reported.is_some() { "reported" } else { "priced" }),
        );
        usd
    }

    /// One embeddings call: OpenRouter's reported cost when it is there, else the tokens at the listed rate.
    pub fn record_embedding(&self, feature: &str, model: &str, response: &Value, provider: &str) {
        let usage = usage_of(response);
        let mut tokens = num(usage.get("prompt_tokens"));
        if tokens == 0 {
            tokens = num(usage.get("total_tokens"));
        }
        let usage_value = Value::Object(usage);
        let reported = reported_cost(&usage_value);
        let usd = match reported {
            Some(r) => Some(r),
            None if truthy(&usage_value) => eve_pricing::estimate_embedding_cost(model, tokens),
            None => None,
        };
        self.record(
            provider,
            model,
            feature,
            usd,
            RecordOpts::new()
                .tokens([("in", Value::from(tokens))])
                .source(if reported.is_some() { "reported" } else { "priced" }),
        );
    }

    /// One transcription: OpenRouter's reported cost when it is there, else its token usage, else its audio
    /// seconds at the per-minute rate.
    pub fn record_transcription(&self, feature: &str, model: &str, response: &Value, seconds: f64, provider: &str) {
        let usage = usage_of(response);
        let usage_value = Value::Object(usage);
        let reported = reported_cost(&usage_value);
        let usd = reported.or_else(|| eve_pricing::estimate_transcribe_cost(model, Some(&usage_value), seconds));
        let get = |name: &str| usage_value.get(name).cloned().unwrap_or(Value::Null);
        self.record(
            provider,
            model,
            feature,
            usd,
            RecordOpts::new()
                .tokens([("in", get("input_tokens")), ("out", get("output_tokens"))])
                .source(if reported.is_some() { "reported" } else { "priced" }),
        );
    }

    /// A Live voice session: its billed seconds at the per-minute rate. Still OpenAI's (the desk voice is not on
    /// OpenRouter yet, docs/openrouter.md).
    pub fn record_live(&self, feature: &str, model: &str, seconds: f64, note: &str) {
        let usd = eve_pricing::estimate_live_cost(model, seconds);
        // `int(round(seconds or 0))`: Python rounds half to even.
        let secs = if seconds.is_finite() { seconds.round_ties_even() as i64 } else { 0 };
        self.record("openai", model, feature, usd, RecordOpts::new().tokens([("secs", Value::from(secs))]).note(note));
    }

    /// Every line of one local day in this ledger.
    pub fn day_rows(&self, day: &str) -> Vec<Map<String, Value>> {
        day_rows(day, Some(&self.dir()))
    }
}

/// `str(model or body.get("model") or "")`.
fn model_name(model: &str, body: &Map<String, Value>) -> String {
    if !model.is_empty() {
        return model.to_string();
    }
    match body.get("model") {
        Some(v) if truthy(v) => py_str(v),
        _ => String::new(),
    }
}

/// `_as_dict`: a reply as a JSON object, or empty.
fn as_dict(obj: &Value) -> Map<String, Value> {
    obj.as_object().cloned().unwrap_or_default()
}

/// An SDK object as `model_dump(exclude_none=True)` gives it: its JSON form with every null field dropped, at any
/// depth. Pass the result to the `record_*` helpers.
pub fn sdk_dump<T: Serialize + ?Sized>(obj: &T) -> Value {
    fn strip(v: Value) -> Value {
        match v {
            Value::Object(m) => {
                Value::Object(m.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k, strip(v))).collect())
            }
            Value::Array(a) => Value::Array(a.into_iter().map(strip).collect()),
            other => other,
        }
    }
    serde_json::to_value(obj).map(strip).unwrap_or_else(|_| Value::Object(Map::new()))
}

/// `_usage_of`: a reply's `usage` as an object, or empty.
fn usage_of(reply: &Value) -> Map<String, Value> {
    reply.get("usage").and_then(Value::as_object).cloned().unwrap_or_default()
}

/// OpenAI's hosted web_search calls in a Responses output: $10 per 1,000, on top of the tokens.
fn web_search_calls(output: Option<&Value>) -> i64 {
    output
        .and_then(Value::as_array)
        .map(|items| items.iter().filter(|i| i.get("type").and_then(Value::as_str) == Some("web_search_call")).count())
        .unwrap_or(0) as i64
}

/// OpenRouter's own figure for a call (`usage.cost`, dollars), when the reply carries one.
fn reported_cost(usage: &Value) -> Option<f64> {
    match usage.get("cost") {
        Some(Value::Number(n)) => n.as_f64().filter(|c| *c >= 0.0),
        _ => None,
    }
}

// ---- the module-level meter (Python's functions) -----------------------------------------------------------------

/// One spend line in today's ledger, in the configured folder. Never fails.
pub fn record(provider: &str, model: &str, feature: &str, usd: Option<f64>, opts: RecordOpts) {
    Ledger::default().record(provider, model, feature, usd, opts)
}

/// See [`Ledger::record_response`]. Python's defaults: `model=""`, `provider="openrouter"`, `searches=None`.
pub fn record_response(feature: &str, response: &Value, model: &str, provider: &str, searches: Option<i64>) {
    Ledger::default().record_response(feature, response, model, provider, searches)
}

/// See [`Ledger::record_chat_completion`]. Python's defaults: `model=""`, `provider="openrouter"`.
pub fn record_chat_completion(feature: &str, payload: &Value, model: &str, provider: &str) {
    Ledger::default().record_chat_completion(feature, payload, model, provider)
}

/// See [`Ledger::record_anthropic`]. Python's default: `provider="openrouter"`.
pub fn record_anthropic(feature: &str, model: &str, usage: &Value, provider: &str) -> Option<f64> {
    Ledger::default().record_anthropic(feature, model, usage, provider)
}

/// See [`Ledger::record_embedding`]. Python's default: `provider="openrouter"`.
pub fn record_embedding(feature: &str, model: &str, response: &Value, provider: &str) {
    Ledger::default().record_embedding(feature, model, response, provider)
}

/// See [`Ledger::record_transcription`]. Python's defaults: `seconds=0.0`, `provider="openrouter"`.
pub fn record_transcription(feature: &str, model: &str, response: &Value, seconds: f64, provider: &str) {
    Ledger::default().record_transcription(feature, model, response, seconds, provider)
}

/// See [`Ledger::record_live`]. Python's default: `note=""`.
pub fn record_live(feature: &str, model: &str, seconds: f64, note: &str) {
    Ledger::default().record_live(feature, model, seconds, note)
}

// ---- the reader -------------------------------------------------------------------------------------------------

/// Every line of one local day. A torn or foreign line is skipped.
pub fn day_rows(day: &str, folder: Option<&Path>) -> Vec<Map<String, Value>> {
    let path = match folder {
        Some(f) => f.join(format!("{day}.jsonl")),
        None => spend_dir().join(format!("{day}.jsonl")),
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    // Text mode reads with universal newlines: "\n", "\r\n" and "\r" all end a line.
    bytes
        .split(|b| *b == b'\n' || *b == b'\r')
        .filter_map(|line| std::str::from_utf8(line).ok())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .collect()
}

/// `_usd`: a row's dollars (a JSON number, never a bool), else 0.
fn row_usd(row: &Map<String, Value>) -> f64 {
    match row.get("usd") {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// What [`totals`] returns. Serializes with the same keys, in the same order, as the Python dict.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Totals {
    pub usd: f64,
    pub calls: i64,
    pub unpriced: i64,
    pub unpriced_models: Vec<String>,
    pub by_provider: IndexMap<String, f64>,
    pub by_feature: IndexMap<String, f64>,
    pub by_model: IndexMap<String, f64>,
}

/// `{"usd", "calls", "unpriced", "by_provider", "by_feature", "by_model"}` for some lines. The splits are sorted
/// by dollars, largest first; unpriced calls count in `unpriced` and in no dollar figure.
pub fn totals<I>(rows: I) -> Totals
where
    I: IntoIterator,
    I::Item: Borrow<Map<String, Value>>,
{
    let (mut total, mut calls, mut unpriced) = (0.0_f64, 0_i64, 0_i64);
    let mut splits: [IndexMap<String, f64>; 3] = Default::default();
    let mut unpriced_models: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for row in rows {
        let row = row.borrow();
        calls += 1;
        if row.get("usd").is_none_or(Value::is_null) {
            unpriced += 1;
            let part = |k: &str| row.get(k).map(py_str).unwrap_or_else(|| "?".into());
            let key = format!("{}/{}", part("p"), part("m"));
            if seen.insert(key.clone()) {
                unpriced_models.push(key);
            }
        }
        let usd = row_usd(row);
        total += usd;
        for (k, split) in ["p", "f", "m"].iter().zip(splits.iter_mut()) {
            let name = match row.get(*k) {
                Some(v) if truthy(v) => py_str(v),
                _ => "?".to_string(),
            };
            *split.entry(name).or_insert(0.0) += usd;
        }
    }
    unpriced_models.sort();

    fn ordered(split: IndexMap<String, f64>) -> IndexMap<String, f64> {
        let mut items: Vec<(String, f64)> = split.into_iter().collect();
        items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0)));
        items.into_iter().map(|(k, v)| (k, py_round(v, 6))).collect()
    }

    let [p, f, m] = splits;
    Totals {
        usd: py_round(total, 6),
        calls,
        unpriced,
        unpriced_models,
        by_provider: ordered(p),
        by_feature: ordered(f),
        by_model: ordered(m),
    }
}

fn add_days(d: NaiveDate, n: i64) -> NaiveDate {
    d.checked_add_signed(Duration::days(n)).unwrap_or(if n < 0 { NaiveDate::MIN } else { NaiveDate::MAX })
}

fn today_local() -> NaiveDate {
    Local::now().date_naive()
}

/// The dollars of every row tagged with this card, from `since` to `today` (at most KEEP_DAYS days). Unpriced
/// rows count nothing, as in [`totals`].
pub fn job_total(card_id: &str, since: NaiveDate, today: Option<NaiveDate>, folder: Option<&Path>) -> f64 {
    let today = today.unwrap_or_else(today_local);
    let base = folder.map(Path::to_path_buf).unwrap_or_else(spend_dir);
    let mut d = since.max(add_days(today, -KEEP_DAYS));
    let mut total = 0.0;
    while d <= today {
        total += day_rows(&d.format("%Y-%m-%d").to_string(), Some(&base))
            .iter()
            .filter(|r| r.get("job").and_then(Value::as_str) == Some(card_id))
            .map(row_usd)
            .sum::<f64>();
        d = add_days(d, 1);
    }
    py_round(total, 8)
}

pub fn day_total(day: &str, folder: Option<&Path>) -> f64 {
    totals(day_rows(day, folder)).usd
}

pub fn today_total(today: Option<NaiveDate>) -> f64 {
    day_total(&today.unwrap_or_else(today_local).format("%Y-%m-%d").to_string(), None)
}

/// One day's totals with its date first (`{"date": ..., **totals}`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DayTotals {
    pub date: String,
    #[serde(flatten)]
    pub totals: Totals,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DayUsd {
    pub date: String,
    pub usd: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MonthUsd {
    pub label: String,
    pub usd: f64,
}

/// What [`summary`] returns; serializes as Python's dict does.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub today: DayTotals,
    pub yesterday: DayTotals,
    pub days: Vec<DayUsd>,
    pub month: MonthUsd,
}

/// What the Spending view and /spend show: today and yesterday in full, the last `days` days as totals (oldest
/// first, zeros included), and this month's total. Eve's own meter only; spend_sync adds the providers'.
/// Python's default `days` is 30.
pub fn summary(today: Option<NaiveDate>, days: i64, folder: Option<&Path>) -> Summary {
    let today = today.unwrap_or_else(today_local);
    let base = folder.map(Path::to_path_buf).unwrap_or_else(spend_dir);
    let iso = |d: NaiveDate| d.format("%Y-%m-%d").to_string();
    let t = totals(day_rows(&iso(today), Some(&base)));
    let y_day = add_days(today, -1);
    let y = totals(day_rows(&iso(y_day), Some(&base)));
    let mut series = Vec::new();
    for i in (0..days.clamp(1, KEEP_DAYS)).rev() {
        let d = add_days(today, -i);
        let usd = match i {
            0 => t.usd,
            1 => y.usd,
            _ => day_total(&iso(d), Some(&base)),
        };
        series.push(DayUsd { date: iso(d), usd: py_round(usd, 6) });
    }
    let mut month = 0.0;
    let mut d = today.with_day(1).unwrap_or(today);
    while d <= today {
        month += if d == today {
            t.usd
        } else if d == y_day {
            y.usd
        } else {
            day_total(&iso(d), Some(&base))
        };
        d = add_days(d, 1);
    }
    Summary {
        today: DayTotals { date: iso(today), totals: t },
        yesterday: DayTotals { date: iso(y_day), totals: y },
        days: series,
        month: MonthUsd { label: today.format("%Y-%m").to_string(), usd: py_round(month, 6) },
    }
}

/// `_money`: "?" for no figure; two decimals from ten cents (and for zero), three below.
fn money(usd: Option<f64>) -> String {
    match usd {
        None => "?".into(),
        Some(u) if u.is_nan() => "$nan".into(),
        Some(u) if u.is_infinite() => (if u > 0.0 { "$inf" } else { "$-inf" }).into(),
        Some(u) if u >= 0.1 || u == 0.0 => format!("${u:.2}"),
        Some(u) => format!("${u:.3}"),
    }
}

fn money_of(v: Option<&Value>) -> String {
    money(match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    })
}

/// The /spend answer, written by code (no model call): today, yesterday, this month, the top three features
/// today, and what OpenRouter itself says about today. `s` is a [`Summary`] as JSON (or any object of that
/// shape); `providers` is spend_sync's `reported` map.
pub fn brief(s: &Value, providers: Option<&Value>) -> String {
    let null = Value::Null;
    let today = s.get("today").unwrap_or(&null);
    let yesterday = s.get("yesterday").unwrap_or(&null);
    let month = s.get("month").unwrap_or(&null);
    let mut lines = vec![
        format!("Today: {}", money_of(today.get("usd"))),
        format!("Yesterday: {}", money_of(yesterday.get("usd"))),
        format!("This month: {}", money_of(month.get("usd"))),
    ];
    let top: Vec<String> = today
        .get("by_feature")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(name, usd)| usd.as_f64().filter(|u| *u > 0.0).map(|u| (name, u)))
                .take(3)
                .map(|(name, u)| format!("{name} {}", money(Some(u))))
                .collect()
        })
        .unwrap_or_default();
    if !top.is_empty() {
        lines.push(format!("Top today: {}", top.join(", ")));
    }
    if let Some(n) = today.get("unpriced").filter(|v| truthy(v)) {
        let n_str = py_str(n);
        // `n == 1` in Python also holds for 1.0 and True.
        let one = match n {
            Value::Bool(b) => *b,
            Value::Number(x) => x.as_f64() == Some(1.0),
            _ => false,
        };
        let plural = if one { "" } else { "s" };
        lines.push(format!(
            "{n_str} call{plural} today had no price (Codex on the ChatGPT plan, or a model without a rate); not in \
             the totals."
        ));
    }
    let orow = providers.and_then(|p| p.get("openrouter")).filter(|v| truthy(v)).unwrap_or(&null);
    match orow.get("status").and_then(Value::as_str) {
        Some("ok") => {
            let mut part = format!("OpenRouter says today (UTC): {} on eve's key", money_of(orow.get("key_today")));
            if let Some(acct) = orow.get("account_today").filter(|v| !v.is_null()) {
                part.push_str(&format!(", {} on the whole account", money_of(Some(acct))));
                if orow.get("account_partial").is_some_and(truthy) {
                    part.push_str(" so far");
                }
            }
            lines.push(format!("{part}."));
        }
        Some("error") => {
            let err = orow.get("error").map(py_str).unwrap_or_else(|| "None".into());
            lines.push(format!("OpenRouter's own figure could not be read ({err})."));
        }
        _ => {}
    }
    format!("Spending (eve's own meter)\n{}", lines.join("\n"))
}

/// [`brief`] for a typed [`Summary`].
pub fn brief_summary(s: &Summary, providers: Option<&Value>) -> String {
    brief(&serde_json::to_value(s).unwrap_or(Value::Null), providers)
}

#[cfg(test)]
mod tests;
