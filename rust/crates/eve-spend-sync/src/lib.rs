//! The providers' own figures, beside eve's meter (eve-spend). Port of `spend_sync.py`.
//!
//! Eve's meter prices what eve itself calls. The providers know more: another key on the same account (another
//! tool's OpenRouter key, the owner's own scripts) and anything eve cannot price. So the daemon asks them, at start
//! and then hourly, off the event loop, and keeps what they said per day in `<spend dir>/providers.json`. Nothing
//! here ever touches the meter's ledger: the dashboard shows the two side by side.
//!
//! * **OpenRouter** (always, with `OPENROUTER_API_KEY`): `GET /api/v1/key` gives eve's key's own
//!   `usage_daily` / `usage_monthly` (OpenRouter's docs: the current UTC day, the current UTC month), and
//!   `GET /api/v1/credits` gives the whole account's `total_usage` (every key). The account's spend on a day is
//!   the change of `total_usage` since the day before's last snapshot; a day with no snapshot the day before is
//!   counted from its own first snapshot and marked partial.
//! * **OpenAI**, only with `EVE_OPENAI_ADMIN_KEY` (an Admin key: eve's own key is refused, it lacks the
//!   `api.usage.read` scope): `GET https://api.openai.com/v1/organization/costs?start_time=<unix>&bucket_width=1d`
//!   → `data[].start_time` (UTC day buckets) and `data[].results[].amount.value` in dollars, paged by
//!   `next_page`.
//! * **Anthropic**, only with `EVE_ANTHROPIC_ADMIN_KEY` (an Admin API key, `sk-ant-admin…`): `GET
//!   https://api.anthropic.com/v1/organizations/cost_report?starting_at=<RFC 3339>&bucket_width=1d` with
//!   `x-api-key` and `anthropic-version: 2023-06-01` → `data[].starting_at` and `data[].results[].amount`,
//!   a decimal string in cents ("123.45" is $1.23), paged by `next_page`.
//!
//! A failure is one log line per provider and reason, and the provider's row says it could not be read; it never
//! breaks anything else. Keys are sent in headers only and never logged or stored.
//!
//! The Python functions raise Python exceptions in a few corners (a state file whose provider row is not an
//! object, a bucket timestamp past year 9999). The port names those exceptions by their Python type name
//! ([`PyError`]), because the type name is what Python logs and stores as the row's `error`.

mod py;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, NaiveDate};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tokio::sync::watch;

pub use py::{py_dumps_sorted, py_loads, py_round, urlencode};
use py::{iter_items, local_date, os_error_name, py_float_from_str, py_str, py_strip, truthy, utc_date, Neumaier};

pub const STATE_FILE: &str = "providers.json";
pub const SYNC_SECS: f64 = 3600.0;
pub const TIMEOUT_SECS: f64 = 20.0;
pub const KEEP_DAYS: usize = 400;
/// How far back the OpenAI and Anthropic cost reports are read each sync.
pub const LOOKBACK_DAYS: i64 = 31;
pub const MAX_PAGES: usize = 5;
pub const OPENROUTER_KEY_URL: &str = "https://openrouter.ai/api/v1/key";
pub const OPENROUTER_CREDITS_URL: &str = "https://openrouter.ai/api/v1/credits";
pub const OPENAI_COSTS_URL: &str = "https://api.openai.com/v1/organization/costs";
pub const ANTHROPIC_COST_URL: &str = "https://api.anthropic.com/v1/organizations/cost_report";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// An environment to read keys from (Python's `Mapping[str, str]`); `None` means the process environment.
pub type Env = HashMap<String, String>;

/// The state: one JSON object per provider, as `providers.json` holds it.
pub type State = Map<String, Value>;

static WARNED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// One short reason, safe to log and to show: never a key, never a body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SyncError(pub String);

/// Any other exception the Python code would raise, by its Python type name (`TypeError`, `ValueError`, ...).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PyError(pub String);

impl PyError {
    fn new(name: &str) -> Self {
        PyError(name.to_string())
    }
}

/// What one provider's sync can fail with: Python's `except SyncError` and `except Exception` branches.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("{0}")]
    Sync(#[from] SyncError),
    #[error("{0}")]
    Other(#[from] PyError),
}

fn sync_err(reason: &str) -> ProviderError {
    ProviderError::Sync(SyncError(reason.to_string()))
}

fn type_err(name: &str) -> ProviderError {
    ProviderError::Other(PyError::new(name))
}

/// What a request can end in, as `urllib.request.urlopen` would raise it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// `HTTPError`: the server answered outside 2xx.
    Http(u16),
    /// `URLError`: no answer; the reason's Python type name (`ConnectionRefusedError`, `gaierror`, ...).
    Unreachable(String),
    /// `TimeoutError` / `OSError` while reading.
    TimedOut,
    /// Anything else the opener raised, by type name; it is not a `SyncError`.
    Raised(String),
}

/// Python's `opener`: sends one GET and returns the body bytes. The seam tests replace.
pub trait Opener: Send + Sync {
    fn open(&self, url: &str, headers: &[(String, String)], timeout: Duration) -> Result<Vec<u8>, OpenError>;
}

/// The default opener: a real HTTPS GET (`urllib.request.urlopen`).
#[derive(Debug, Default, Clone, Copy)]
pub struct HttpOpener;

fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs_f64(TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new())
    })
}

/// The Python exception name behind a failed connection (`URLError.reason`).
fn connect_reason(e: &reqwest::Error) -> String {
    let mut source: Option<&dyn std::error::Error> = std::error::Error::source(e);
    let mut text = String::new();
    while let Some(err) = source {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::ConnectionRefused => return "ConnectionRefusedError".into(),
                std::io::ErrorKind::ConnectionReset => return "ConnectionResetError".into(),
                std::io::ErrorKind::ConnectionAborted => return "ConnectionAbortedError".into(),
                std::io::ErrorKind::TimedOut => return "TimeoutError".into(),
                _ => {}
            }
        }
        text.push_str(&err.to_string().to_lowercase());
        text.push(' ');
        source = err.source();
    }
    if text.contains("dns") || text.contains("lookup") || text.contains("resolve") {
        "gaierror".into()
    } else if text.contains("certificate") {
        "SSLCertVerificationError".into()
    } else if text.contains("tls") || text.contains("ssl") || text.contains("handshake") {
        "SSLError".into()
    } else {
        "OSError".into()
    }
}

/// The lower-cased text of an error's whole source chain.
fn chain_text(e: &reqwest::Error) -> String {
    let mut text = e.to_string().to_lowercase();
    let mut source: Option<&dyn std::error::Error> = std::error::Error::source(e);
    while let Some(err) = source {
        text.push(' ');
        text.push_str(&err.to_string().to_lowercase());
        source = err.source();
    }
    text
}

/// A failure after the connection was made, as urllib lets it through unwrapped: `getresponse()` and `read()`
/// sit outside urllib's `URLError` wrapping, so a reset or a timeout there is a bare `OSError` ("timed out"),
/// a garbled status line is http.client's `BadStatusLine` and a short body its `IncompleteRead` (neither is an
/// `OSError`, so Python records the type name).
fn after_connect(e: &reqwest::Error, reading_body: bool) -> OpenError {
    if e.is_timeout() {
        return OpenError::TimedOut;
    }
    let text = chain_text(e);
    if !reading_body && text.contains("parse") {
        OpenError::Raised("BadStatusLine".into())
    } else if reading_body && (text.contains("end of file") || text.contains("incomplete")) {
        OpenError::Raised("IncompleteRead".into())
    } else {
        OpenError::TimedOut
    }
}

fn fetch(url: &str, headers: &[(String, String)], timeout: Duration) -> Result<Vec<u8>, OpenError> {
    let mut req = http_client().get(url).timeout(timeout);
    for (name, value) in headers {
        req = req.header(name.as_str(), value.as_str());
    }
    let response = match req.send() {
        Ok(r) => r,
        Err(e) if e.is_builder() => return Err(OpenError::Raised("ValueError".into())),
        Err(e) if e.is_timeout() && e.is_connect() => return Err(OpenError::Unreachable("TimeoutError".into())),
        Err(e) if e.is_timeout() => return Err(OpenError::TimedOut),
        // urllib gives up on a redirect loop with an HTTPError carrying the redirect's code
        Err(e) if e.is_redirect() => return Err(OpenError::Http(e.status().map(|s| s.as_u16()).unwrap_or(302))),
        Err(e) if e.is_connect() => return Err(OpenError::Unreachable(connect_reason(&e))),
        // connected, then the exchange failed: past urllib's URLError wrapping
        Err(e) => return Err(after_connect(&e, false)),
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(OpenError::Http(status));
    }
    response.bytes().map(|b| b.to_vec()).map_err(|e| after_connect(&e, true))
}

impl Opener for HttpOpener {
    fn open(&self, url: &str, headers: &[(String, String)], timeout: Duration) -> Result<Vec<u8>, OpenError> {
        // The blocking client must never run on an async worker thread, so each request runs on a plain thread
        // of its own: `sync_once` is then safe to call from anywhere, as Python's is.
        std::thread::scope(|s| s.spawn(|| fetch(url, headers, timeout)).join())
            .unwrap_or_else(|_| Err(OpenError::Raised("RuntimeError".into())))
    }
}

fn once(key: &str, msg: impl FnOnce() -> String) -> bool {
    let fresh = WARNED.lock().insert(key.to_string());
    if fresh {
        tracing::warn!("{}", msg());
    }
    fresh
}

/// Python's `ValueError` / `TypeError` family, which `get_json` reads as an unreadable reply.
const UNREADABLE_ERRORS: [&str; 5] = ["ValueError", "TypeError", "UnicodeDecodeError", "JSONDecodeError", "UnicodeError"];

/// Python's `TimeoutError` / `OSError` family, which `get_json` reads as a timeout.
const OS_ERRORS: [&str; 10] = [
    "OSError", "TimeoutError", "ConnectionError", "ConnectionResetError", "ConnectionRefusedError",
    "ConnectionAbortedError", "BrokenPipeError", "SSLError", "RemoteDisconnected", "timeout",
];

/// `get_json`: one GET with `Accept: application/json` and `User-Agent: eve-spend`; the reply must be a JSON object.
pub fn get_json(
    url: &str,
    headers: &[(&str, &str)],
    opener: Option<&dyn Opener>,
    timeout: Option<Duration>,
) -> Result<Map<String, Value>, ProviderError> {
    // `{**headers, "Accept": ..., "User-Agent": ...}`: the two fixed ones win over a caller's of the same name.
    let mut all: Vec<(String, String)> = Vec::new();
    for (name, value) in headers.iter().copied().chain([("Accept", "application/json"), ("User-Agent", "eve-spend")]) {
        match all.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = value.to_string(),
            None => all.push((name.to_string(), value.to_string())),
        }
    }
    let timeout = timeout.unwrap_or(Duration::from_secs_f64(TIMEOUT_SECS));
    let default = HttpOpener;
    let send: &dyn Opener = opener.unwrap_or(&default);
    let raw = match send.open(url, &all, timeout) {
        Ok(raw) => raw,
        Err(OpenError::Http(code)) => return Err(sync_err(&format!("HTTP {code}"))),
        Err(OpenError::Unreachable(reason)) => return Err(sync_err(&format!("unreachable ({reason})"))),
        Err(OpenError::TimedOut) => return Err(sync_err("timed out")),
        Err(OpenError::Raised(name)) if UNREADABLE_ERRORS.contains(&name.as_str()) => {
            return Err(sync_err("unreadable reply"))
        }
        Err(OpenError::Raised(name)) if OS_ERRORS.contains(&name.as_str()) => return Err(sync_err("timed out")),
        Err(OpenError::Raised(name)) => return Err(type_err(&name)),
    };
    let text = String::from_utf8(raw).map_err(|_| sync_err("unreadable reply"))?;
    match py_loads(&text) {
        Some(Value::Object(body)) => Ok(body),
        _ => Err(sync_err("unreadable reply")),
    }
}

/// `_num`: a JSON number (never a bool) as a float.
fn num(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

fn float_value(x: f64) -> Value {
    serde_json::Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null)
}

/// `row = state.setdefault(name, {})`: the row, or the Python exception the first use of a non-dict row raises.
fn setdefault_obj<'a>(map: &'a mut Map<String, Value>, key: &str) -> &'a mut Value {
    map.entry(key.to_string()).or_insert_with(|| Value::Object(Map::new()))
}

/// `row.setdefault(key, {})` on `row`: `AttributeError` when `row` is not a dict.
fn row_setdefault<'a>(row: &'a mut Value, key: &str) -> Result<&'a mut Value, ProviderError> {
    match row {
        Value::Object(m) => Ok(setdefault_obj(m, key)),
        _ => Err(type_err("AttributeError")),
    }
}

/// `row[key] = value` on `row`: `TypeError` when `row` is not a dict.
fn row_set(row: &mut Value, key: &str, value: Value) -> Result<(), ProviderError> {
    match row {
        Value::Object(m) => {
            m.insert(key.to_string(), value);
            Ok(())
        }
        _ => Err(type_err("TypeError")),
    }
}

/// `row.pop("error", None)`.
fn row_pop_error(row: &mut Value) -> Result<(), ProviderError> {
    match row {
        Value::Object(m) => {
            m.shift_remove("error");
            Ok(())
        }
        Value::Array(_) => Err(type_err("TypeError")),
        _ => Err(type_err("AttributeError")),
    }
}

/// `row.setdefault("days", {}).update(days)`.
fn merge_days(state: &mut State, name: &str, days: Days, now: f64) -> Result<(), ProviderError> {
    let row = setdefault_obj(state, name);
    match row_setdefault(row, "days")? {
        Value::Object(existing) => {
            for (day, usd) in days {
                existing.insert(day, float_value(usd));
            }
        }
        _ => return Err(type_err("AttributeError")),
    }
    row_set(row, "at", json!(now as i64))?;
    row_pop_error(row)
}

// ---- the three providers ---------------------------------------------------------------------------------

/// OpenRouter: the key's own day and month (UTC) and the account's `total_usage` snapshot for the local day.
pub fn sync_openrouter(state: &mut State, key: &str, now: f64, opener: Option<&dyn Opener>) -> Result<(), ProviderError> {
    let bearer = format!("Bearer {key}");
    let auth = [("Authorization", bearer.as_str())];
    let data = get_json(OPENROUTER_KEY_URL, &auth, opener, None)?.shift_remove("data");
    let credits = get_json(OPENROUTER_CREDITS_URL, &auth, opener, None)?.shift_remove("data");
    let (Some(Value::Object(data)), Some(Value::Object(credits))) = (data, credits) else {
        return Err(sync_err("unreadable reply"));
    };
    let row = setdefault_obj(state, "openrouter");
    let utc = utc_date(now)?.format("%Y-%m-%d").to_string();
    let (daily, monthly) = (num(data.get("usage_daily")), num(data.get("usage_monthly")));
    if let Some(daily) = daily {
        row_set(row_setdefault(row, "key_days")?, &utc, float_value(py_round(daily, 6)))?;
    }
    if let Some(monthly) = monthly {
        row_set(row, "key_month", json!({"month": &utc[..7], "usd": float_value(py_round(monthly, 6))}))?;
    }
    if let Some(total) = num(credits.get("total_usage")) {
        let local = local_date(now)?.format("%Y-%m-%d").to_string();
        let day = row_setdefault(row_setdefault(row, "account_days")?, &local)?;
        let total = float_value(py_round(total, 6));
        if let Value::Object(m) = day {
            m.entry("first".to_string()).or_insert_with(|| total.clone());
        } else {
            return Err(type_err("AttributeError"));
        }
        row_set(day, "last", total)?;
    }
    row_set(row, "at", json!(now as i64))?;
    row_pop_error(row)
}

/// The UTC midnight `LOOKBACK_DAYS - 1` days before `now`'s UTC day, or Python's `OverflowError` before year 1.
fn lookback_start(now: f64) -> Result<NaiveDate, ProviderError> {
    let start = utc_date(now)? - ChronoDuration::days(LOOKBACK_DAYS - 1);
    if start < NaiveDate::from_ymd_opt(1, 1, 1).unwrap_or(NaiveDate::MIN) {
        return Err(type_err("OverflowError"));
    }
    Ok(start)
}

/// One page loop over a cost report: `MAX_PAGES` at most, following `next_page` while `has_more`.
fn read_pages(
    url: &str,
    mut params: Vec<(String, Value)>,
    headers: &[(&str, &str)],
    opener: Option<&dyn Opener>,
    mut each_page: impl FnMut(&Map<String, Value>) -> Result<(), ProviderError>,
) -> Result<(), ProviderError> {
    for _ in 0..MAX_PAGES {
        let body = get_json(&format!("{url}?{}", urlencode(&params)), headers, opener, None)?;
        each_page(&body)?;
        let next = body.get("next_page");
        if !truthy(body.get("has_more")) || !truthy(next) {
            break;
        }
        let next = next.cloned().unwrap_or(Value::Null);
        match params.iter_mut().find(|(k, _)| k == "page") {
            Some(slot) => slot.1 = next,
            None => params.push(("page".to_string(), next)),
        }
    }
    Ok(())
}

/// `days[day] = round(days.get(day, 0.0) + usd, 6)`.
fn add_day(days: &mut Days, day: String, usd: f64) {
    let before = days.get(&day).copied().unwrap_or(0.0);
    days.insert(day, py_round(before + usd, 6));
}

/// One sync's dollars per day, in the order the report gave them.
type Days = IndexMap<String, f64>;

/// OpenAI's organization cost report (an Admin key): dollars per UTC day for the last `LOOKBACK_DAYS` days.
pub fn sync_openai(state: &mut State, admin_key: &str, now: f64, opener: Option<&dyn Opener>) -> Result<(), ProviderError> {
    let start = lookback_start(now)?;
    let start_ts = start.and_hms_opt(0, 0, 0).map(|d| d.and_utc().timestamp()).unwrap_or(0);
    let params = vec![
        ("start_time".to_string(), json!(start_ts)),
        ("bucket_width".to_string(), json!("1d")),
        ("limit".to_string(), json!(LOOKBACK_DAYS)),
    ];
    let bearer = format!("Bearer {admin_key}");
    let mut days = Days::new();
    read_pages(OPENAI_COSTS_URL, params, &[("Authorization", bearer.as_str())], opener, |body| {
        for bucket in iter_items(body.get("data"))? {
            let Value::Object(bucket) = bucket else { continue };
            let Some(start_time) = num(bucket.get("start_time")) else { continue };
            let day = utc_date(start_time)?.format("%Y-%m-%d").to_string();
            let mut usd = 0.0;
            for result in iter_items(bucket.get("results"))? {
                let amount = match result {
                    Value::Object(r) => r.get("amount"),
                    _ => None,
                };
                if let Some(Value::Object(amount)) = amount {
                    if py_str(amount.get("currency"), "usd").to_lowercase() == "usd" {
                        usd += num(amount.get("value")).unwrap_or(0.0);
                    }
                }
            }
            add_day(&mut days, day, usd);
        }
        Ok(())
    })?;
    merge_days(state, "openai", days, now)
}

/// Anthropic's cost report (an Admin API key): cents per UTC day, kept in dollars.
pub fn sync_anthropic(
    state: &mut State,
    admin_key: &str,
    now: f64,
    opener: Option<&dyn Opener>,
) -> Result<(), ProviderError> {
    let start = lookback_start(now)?;
    let params = vec![
        ("starting_at".to_string(), json!(format!("{}T00:00:00Z", start.format("%Y-%m-%d")))),
        ("bucket_width".to_string(), json!("1d")),
        ("limit".to_string(), json!(LOOKBACK_DAYS)),
    ];
    let headers = [("x-api-key", admin_key), ("anthropic-version", ANTHROPIC_VERSION)];
    let mut days = Days::new();
    read_pages(ANTHROPIC_COST_URL, params, &headers, opener, |body| {
        for bucket in iter_items(body.get("data"))? {
            let Value::Object(bucket) = bucket else { continue };
            let Some(Value::String(starting_at)) = bucket.get("starting_at") else { continue };
            let day: String = starting_at.chars().take(10).collect();
            let mut cents = 0.0;
            for result in iter_items(bucket.get("results"))? {
                let Value::Object(result) = result else { continue };
                if py_str(result.get("currency"), "USD").to_uppercase() != "USD" {
                    continue;
                }
                // lowest currency unit: cents
                let amount = result.get("amount");
                let value = if !truthy(amount) {
                    Some(0.0)
                } else {
                    match amount {
                        Some(Value::String(s)) => py_float_from_str(s),
                        Some(Value::Number(n)) => n.as_f64(),
                        Some(Value::Bool(true)) => Some(1.0),
                        _ => None,
                    }
                };
                if let Some(v) = value {
                    cents += v;
                }
            }
            add_day(&mut days, day, cents / 100.0);
        }
        Ok(())
    })?;
    merge_days(state, "anthropic", days, now)
}

// ---- the state file ----------------------------------------------------------------------------------------

/// `<spend dir>/providers.json`, the spend dir being eve-spend's (`EVE_SPEND_DIR`, `~/.config/eve-bridge/spend`).
pub fn state_path() -> PathBuf {
    state_path_in(&eve_spend::spend_dir())
}

/// `providers.json` in a given spend folder.
pub fn state_path_in(folder: &Path) -> PathBuf {
    folder.join(STATE_FILE)
}

/// The state at the default path; `{}` when missing, unreadable or not an object.
pub fn load_state() -> State {
    load_state_from(&state_path())
}

/// The state at `path`; `{}` when missing, unreadable or not an object.
pub fn load_state_from(path: &Path) -> State {
    let Ok(raw) = std::fs::read(path) else { return Map::new() };
    let Ok(text) = String::from_utf8(raw) else { return Map::new() };
    match py_loads(&text) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// Keep each day map to its newest `KEEP_DAYS` days (sorted by day).
fn prune(state: &mut State) {
    for row in state.values_mut() {
        let Value::Object(row) = row else { continue };
        for name in ["key_days", "account_days", "days"] {
            if let Some(Value::Object(days)) = row.get_mut(name) {
                if days.len() > KEEP_DAYS {
                    let mut items: Vec<(String, Value)> = std::mem::take(days).into_iter().collect();
                    items.sort_by(|a, b| a.0.cmp(&b.0));
                    let keep = items.split_off(items.len() - KEEP_DAYS);
                    *days = keep.into_iter().collect();
                }
            }
        }
    }
}

/// Write the state at the default path (`json.dumps(state, sort_keys=True)`, through a `.tmp` and a rename).
pub fn save_state(state: &State) -> std::io::Result<()> {
    save_state_to(&state_path(), state)
}

/// Write the state at `path`, atomically: `providers.tmp` beside it, then a rename.
pub fn save_state_to(path: &Path, state: &State) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, py_dumps_sorted(&Value::Object(state.clone())))?;
    std::fs::rename(&tmp, path)
}

fn env_get(env: Option<&Env>, name: &str) -> String {
    match env {
        Some(e) => e.get(name).cloned().unwrap_or_default(),
        None => std::env::var(name).unwrap_or_default(),
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

type Job = fn(&mut State, &str, f64, Option<&dyn Opener>) -> Result<(), ProviderError>;

const JOBS: [(&str, &str, Job); 3] = [
    ("openrouter", "OPENROUTER_API_KEY", sync_openrouter),
    ("openai", "EVE_OPENAI_ADMIN_KEY", sync_openai),
    ("anthropic", "EVE_ANTHROPIC_ADMIN_KEY", sync_anthropic),
];

/// Ask every provider eve has a key for, keep what they said, and return the state, at the default path.
pub fn sync_once(env: Option<&Env>, now: Option<f64>, opener: Option<&dyn Opener>) -> Result<State, PyError> {
    sync_once_in(&eve_spend::spend_dir(), env, now, opener)
}

/// `sync_once` against `providers.json` in `folder`.
///
/// It fails only where Python's raises despite its "never raises": a state file holding a provider row that is
/// not an object makes recording that provider's error a `TypeError`, and nothing is saved.
pub fn sync_once_in(
    folder: &Path,
    env: Option<&Env>,
    now: Option<f64>,
    opener: Option<&dyn Opener>,
) -> Result<State, PyError> {
    let now = now.unwrap_or_else(now_secs);
    let path = state_path_in(folder);
    let mut state = load_state_from(&path);
    for (name, key_name, job) in JOBS {
        let key = env_get(env, key_name);
        let key = py_strip(&key);
        if key.is_empty() {
            continue;
        }
        let error = match job(&mut state, key, now, opener) {
            Ok(()) => continue,
            Err(ProviderError::Sync(e)) => {
                once(&format!("{name}:{e}"), || {
                    format!("spend: {name}'s own figures could not be read ({e}); tried again next hour")
                });
                e.0
            }
            // one provider's surprise never stops the others
            Err(ProviderError::Other(e)) => {
                once(&format!("{name}:{e}"), || format!("spend: {name}'s own figures could not be read ({e})"));
                e.0
            }
        };
        if row_set(setdefault_obj(&mut state, name), "error", json!(error)).is_err() {
            return Err(PyError::new("TypeError"));
        }
    }
    prune(&mut state);
    if let Err(e) = save_state_to(&path, &state) {
        let name = os_error_name(&e);
        once("save", || format!("spend: the providers' figures could not be saved ({name})"));
    }
    Ok(state)
}

/// What the loop runs each round (the seam tests replace; the daemon's is `sync_once`).
pub type SyncFn = Arc<dyn Fn(Option<&Env>) -> Result<(), PyError> + Send + Sync>;

/// Python's `loop`: at start, then every `interval` until shutdown, one sync on a worker thread. Never fails.
/// `shutdown` is the daemon's stop signal (true once set), Python's `asyncio.Event`.
pub async fn run_loop(shutdown: watch::Receiver<bool>, interval: Duration, env: Option<Env>) {
    let sync: SyncFn = Arc::new(|env: Option<&Env>| sync_once(env, None, None).map(|_| ()));
    run_loop_with(shutdown, interval, env, sync).await
}

/// `run_loop` with the per-round sync given.
pub async fn run_loop_with(mut shutdown: watch::Receiver<bool>, interval: Duration, env: Option<Env>, sync: SyncFn) {
    let env = Arc::new(env);
    while !*shutdown.borrow() {
        let (job, env_now) = (sync.clone(), env.clone());
        let failed = match tokio::task::spawn_blocking(eve_spend::carry(move || job(env_now.as_ref().as_ref()))).await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e.0),
            Err(_) => Some("RuntimeError".to_string()),
        };
        if let Some(name) = failed {
            once(&format!("loop:{name}"), || format!("spend: a provider sync failed ({name})"));
        }
        let sender_gone = matches!(tokio::time::timeout(interval, shutdown.wait_for(|stop| *stop)).await, Ok(Err(_)));
        if sender_gone {
            // nothing can stop the loop early any more: wait out the interval
            tokio::time::sleep(interval).await;
        }
    }
}

// ---- what the dashboard shows ------------------------------------------------------------------------------

fn status_of(row: &Map<String, Value>) -> &'static str {
    if truthy(row.get("error")) {
        "error"
    } else if truthy(row.get("at")) {
        "ok"
    } else {
        "pending"
    }
}

fn obj<'a>(value: Option<&'a Value>, empty: &'a Map<String, Value>) -> &'a Map<String, Value> {
    match value {
        Some(Value::Object(m)) => m,
        _ => empty,
    }
}

fn raw(value: Option<&Value>) -> Value {
    value.cloned().unwrap_or(Value::Null)
}

/// Per provider: what it says today and this month, or why there is nothing (no admin key, an error). Keyed by
/// provider; each row has `"status"`: `ok`, `error`, `pending` or `no_key`. `state` defaults to the saved one.
pub fn reported(state: Option<&State>, now: Option<f64>, env: Option<&Env>) -> Map<String, Value> {
    let loaded;
    let state = match state {
        Some(s) => s,
        None => {
            loaded = load_state();
            &loaded
        }
    };
    let mut now = now.unwrap_or_else(now_secs);
    if utc_date(now).is_err() || local_date(now).is_err() {
        now = now_secs();
    }
    let utc = utc_date(now).unwrap_or_default();
    let local = local_date(now).unwrap_or_default();
    let utc_iso = utc.format("%Y-%m-%d").to_string();
    let utc_month = &utc_iso[..7];
    let empty = Map::new();
    let mut out = Map::new();

    let row = obj(state.get("openrouter"), &empty);
    if py_strip(&env_get(env, "OPENROUTER_API_KEY")).is_empty() && row.is_empty() {
        out.insert("openrouter".into(), json!({"status": "no_key"}));
    } else {
        let key_days = obj(row.get("key_days"), &empty);
        let month = obj(row.get("key_month"), &empty);
        let (account_today, partial) = account_day(row.get("account_days"), local);
        let key_month = match month.get("month") {
            Some(Value::String(m)) if m == utc_month => raw(month.get("usd")),
            _ => Value::Null,
        };
        out.insert(
            "openrouter".into(),
            json!({
                "status": status_of(row),
                "error": raw(row.get("error")), "at": raw(row.get("at")),
                "key_today": raw(key_days.get(&utc_iso)), "key_day": utc_iso,
                "key_month": key_month,
                "account_today": account_today.map(float_value).unwrap_or(Value::Null), "account_partial": partial,
            }),
        );
    }
    for (name, key_name) in [("openai", "EVE_OPENAI_ADMIN_KEY"), ("anthropic", "EVE_ANTHROPIC_ADMIN_KEY")] {
        let row = obj(state.get(name), &empty);
        if py_strip(&env_get(env, key_name)).is_empty() {
            out.insert(name.into(), json!({"status": "no_key", "key_name": key_name}));
            continue;
        }
        let days = obj(row.get("days"), &empty);
        // `sum(float(v) for ...)`: a bool counts, as `isinstance(v, (int, float))` lets it; no item at all is int 0
        let mut month = Neumaier::default();
        for (day, v) in days {
            if day.chars().take(7).collect::<String>() != utc_month {
                continue;
            }
            match v {
                Value::Number(n) => month.add(n.as_f64().unwrap_or(0.0)),
                Value::Bool(b) => month.add(if *b { 1.0 } else { 0.0 }),
                _ => {}
            }
        }
        let month = if days.is_empty() {
            Value::Null
        } else if month.count == 0 {
            json!(0)
        } else {
            float_value(py_round(month.total(), 6))
        };
        out.insert(
            name.into(),
            json!({"status": status_of(row), "error": raw(row.get("error")), "at": raw(row.get("at")),
                   "day": utc_iso, "today": raw(days.get(&utc_iso)), "month": month}),
        );
    }
    out
}

/// (the whole OpenRouter account's spend on `day`, partial?) from the total_usage snapshots.
fn account_day(days: Option<&Value>, day: NaiveDate) -> (Option<f64>, bool) {
    let Some(Value::Object(days)) = days else { return (None, false) };
    let Some(Value::Object(snap)) = days.get(&day.format("%Y-%m-%d").to_string()) else { return (None, false) };
    let Some(last) = num(snap.get("last")) else { return (None, false) };
    // `max(0.0, x)`: Python keeps the first argument unless the second is greater (so NaN and -0.0 give 0.0)
    let floor0 = |x: f64| if x > 0.0 { x } else { 0.0 };
    let before = day.pred_opt().map(|d| d.format("%Y-%m-%d").to_string());
    if let Some(Value::Object(before)) = before.and_then(|b| days.get(&b)) {
        if let Some(prev) = num(before.get("last")) {
            return (Some(py_round(floor0(last - prev), 6)), false);
        }
    }
    (num(snap.get("first")).map(|first| py_round(floor0(last - first), 6)), true)
}

#[cfg(test)]
mod tests;
