//! Updates' Composio sources: what needs the owner in the apps connected through Composio (docs/updates.md,
//! Sources). A port of `updates_composio.py`.
//!
//! Owner, 2026-10-16: "make sure updates is also linked to all the composio stuff". So the Updates pane also reads,
//! one source each: Slack (direct messages and mentions of the owner in the last 3 days that the owner has not
//! answered), Notion (pages edited in the last 14 days that hold an open to-do, mention the owner, or are assigned to
//! the owner and still open), Linear (open issues assigned to the owner, due within 14 days or updated in the last
//! 7), and the owner's OTHER Gmail and Google Calendar accounts (composio_tools.OTHER_ACCOUNT_TOOLKITS: the work inbox
//! of the last 7 days without promotions and social, and the work calendar of the next 7 days).
//!
//! How a read goes:
//!
//! * `Reader` is one gather's view of Composio: the owner's connected accounts are listed once, lazily
//!   (`ComposioRead::connected_accounts`), and every source reads through `Reader::call`.
//! * `Reader::call` runs only a slug in `READ_SLUGS`, an allow-list of exact slugs that only read, and checks each one
//!   again with composio_tools' own `consequential_slugs` and `is_read_only` before it reaches `read_tool` (which
//!   refuses a writing slug a third time). Nothing here can send, change or delete.
//! * A source is **off** when its toolkit has no account the owner connected (a sign-in started and not finished
//!   counts as none). It **failed** when Composio could not be read, or when its only connection has expired or
//!   failed: an expired connection is a lost source, not an empty one, so `Updates::rebuild` keeps its old cards and
//!   tries again.
//! * Each item is `updates::item`, clipped like the others, with a stable id (`REF_PREFIX`) and, when the app gave
//!   one, its own `link` (`safe_link`: https on that app's host only). The link is never shown to the model.
//!
//! Logs carry counts, kinds and exception types only, never a title, a sender, an id or an alias.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use chrono::{DateTime, Datelike, Duration, FixedOffset, Local, NaiveDate, TimeZone, Timelike, Utc};
use futures::future::BoxFuture;
use indexmap::IndexMap;
use regex::{Captures, Regex};
use serde_json::{json, Value};

use eve_composio_tools::{consequential_slugs, is_read_only, MULTI_EXECUTE, OTHER_ACCOUNT_TOOLKITS, PRIMARY_ALIAS};
use eve_google_rest::pytime::isoformat;
use eve_memory_sources::{py_str, py_str_or_empty};
use eve_transcripts::{py, Dt};

use crate::seams::{ComposioRead, Obj};
use crate::updates::{event_item, item, one_line, one_line_str, Item, ReadError, MAIL_QUERY};
use crate::{env_get, monotonic, Env, Mono};

pub const ENV_SWITCH: &str = "EVE_UPDATES_COMPOSIO";

/// The source kinds, each one app.
pub const KINDS: [&str; 5] = ["slack", "notion", "linear", "work_email", "work_calendar"];
/// The ref prefix of each kind (`updates::REF_KINDS`).
pub const REF_PREFIX: [(&str, &str); 5] = [
    ("slack", "slack"),
    ("notion", "notion"),
    ("linear", "linear"),
    ("work_email", "wgmail"),
    ("work_calendar", "wcal"),
];
/// The Composio toolkit each kind reads.
pub const TOOLKIT: [(&str, &str); 5] = [
    ("slack", "slack"),
    ("notion", "notion"),
    ("linear", "linear"),
    ("work_email", "gmail"),
    ("work_calendar", "googlecalendar"),
];
/// The time limit of each source (updates::gather), in seconds. Notion reads a page's blocks one call at a time.
pub const LIMITS: [(&str, f64); 5] =
    [("slack", 30.0), ("notion", 45.0), ("linear", 25.0), ("work_email", 45.0), ("work_calendar", 30.0)];

/// Every slug this module may run. Each one only reads (tests check them against composio_tools.consequential_slugs).
pub const READ_SLUGS: [&str; 7] = [
    "SLACK_SEARCH_MESSAGES",
    "NOTION_GET_ABOUT_ME",
    "NOTION_SEARCH_NOTION_PAGE",
    "NOTION_FETCH_BLOCK_CONTENTS",
    "LINEAR_LIST_LINEAR_ISSUES",
    "GMAIL_FETCH_EMAILS",
    "GOOGLECALENDAR_EVENTS_LIST",
];

/// The hosts an item's link may point at, by kind (the app's own pages; a subdomain counts).
pub const LINK_HOSTS: [(&str, &[&str]); 5] = [
    ("slack", &["slack.com"]),
    ("notion", &["notion.so", "notion.com", "notion.site"]),
    ("linear", &["linear.app"]),
    ("work_email", &["google.com"]),
    ("work_calendar", &["google.com"]),
];

pub const SLACK_DAYS: i64 = 3;
/// per search; the owner's own messages: 100
pub const SLACK_COUNT: i64 = 50;
/// conversations
pub const SLACK_MAX: usize = 15;
pub const NOTION_DAYS: i64 = 14;
/// pages read from the search, the most recently edited first
pub const NOTION_SEARCH: i64 = 50;
/// pages whose blocks are read
pub const NOTION_PAGES: usize = 10;
pub const NOTION_MAX: usize = 15;
pub const NOTION_BLOCKS: i64 = 100;
pub const LINEAR_FIRST: i64 = 100;
pub const LINEAR_DUE_DAYS: i64 = 14;
pub const LINEAR_UPDATED_DAYS: i64 = 7;
pub const LINEAR_MAX: usize = 15;
pub const WORK_MAIL_MAX: usize = 30;
pub const WORK_CALENDAR_DAYS: i64 = 7;
/// accounts read per kind
pub const MAX_ACCOUNTS: usize = 3;

pub const ACTIVE: &str = "ACTIVE";
/// a connection that was made and no longer works
pub const LOST_STATES: [&str; 3] = ["EXPIRED", "FAILED", "INACTIVE"];
pub const DONE_WORDS: [&str; 13] = [
    "done",
    "complete",
    "completed",
    "closed",
    "canceled",
    "cancelled",
    "duplicate",
    "shipped",
    "resolved",
    "archived",
    "won't do",
    "wont do",
    "not doing",
];
pub const PRIORITY: [(i64, &str); 4] = [(1, "Urgent"), (2, "High"), (3, "Medium"), (4, "Low")];

pub static SLACK_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z0-9]{3,32}\n?\z").expect("SLACK_ID"));
pub static SLACK_TS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{9,12}\.\d{1,8}\n?\z").expect("SLACK_TS"));
pub static SAFE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.:-]{1,128}\n?\z").expect("SAFE_ID"));
pub static ALIAS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-z0-9_-]+").expect("ALIAS"));

/// The toolkit has no connected account: the source is off (`ReadError::Off`). Python's `SourceOff`.
pub fn source_off(message: impl Into<String>) -> ReadError {
    ReadError::off(message)
}

/// Composio could not be read for this source, or its connection has expired: the source failed (and is lost).
/// Python's `ReadFailed`.
pub fn read_failed(message: impl Into<String>) -> ReadError {
    ReadError::failed("ReadFailed", message)
}

/// A slug off the allow-list, or one composio_tools says writes. Python's `PermissionError`.
pub fn permission(message: impl Into<String>) -> ReadError {
    ReadError::failed("PermissionError", message)
}

/// On unless EVE_UPDATES_COMPOSIO is 0/false/no/off.
pub fn enabled(environ: Option<&Env>) -> bool {
    let raw = env_get(environ, ENV_SWITCH).filter(|v| !v.is_empty()).unwrap_or_else(|| "1".to_string());
    !["0", "false", "no", "off"].contains(&py::strip(&raw).to_lowercase().as_str())
}

/// A bridge these sources can read through: full eve's ComposioBridge once its client is made.
pub fn usable(bridge: Option<&Arc<dyn ComposioRead>>) -> bool {
    bridge.is_some_and(|b| b.can_read())
}

static IPV_FUTURE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^v[a-fA-F0-9]+\..+\z").expect("IPV_FUTURE"));

/// `url` when it is https on one of `kind`'s hosts (LINK_HOSTS; any kind's for None), with no user part, in
/// printable ASCII; else None.
pub fn safe_link(url: Option<&Value>, kind: Option<&str>) -> Option<String> {
    let Some(Value::String(url)) = url else { return None };
    // ASCII first, so the length in bytes is Python's len()
    if !url.is_ascii() || url.is_empty() || url.len() > 2048 {
        return None;
    }
    if !url.bytes().all(|b| (0x20..0x7f).contains(&b)) || url.contains(' ') || url.contains('\\') {
        return None;
    }
    let parts = urlsplit(url)?;
    if parts.scheme != "https" || parts.netloc.contains('@') {
        return None;
    }
    // parts.port: None or 443. A port that is not a number, or is past 65535, raised ValueError in Python
    // (outside its try); here it is no link (PARITY.md, Differences).
    match parts.port {
        None => {}
        Some(p) if p.bytes().all(|b| b.is_ascii_digit()) => {
            let n = p.trim_start_matches('0');
            if n.len() > 5 || n.parse::<u32>().unwrap_or(0) > 65535 || n.parse::<u32>().unwrap_or(0) != 443 {
                return None;
            }
        }
        Some(_) => return None,
    }
    let host = parts.hostname.to_lowercase();
    let hosts: Vec<&str> = match kind {
        Some(k) => LINK_HOSTS.iter().find(|(name, _)| *name == k).map(|(_, hs)| hs.to_vec()).unwrap_or_default(),
        None => LINK_HOSTS.iter().flat_map(|(_, hs)| hs.iter().copied()).collect(),
    };
    hosts.iter().any(|h| host == *h || host.ends_with(&format!(".{h}"))).then(|| url.clone())
}

/// What `safe_link` reads of `urllib.parse.urlsplit(url)`: the scheme, the netloc, and the hostname and port of
/// `_hostinfo` (the port as written, None when empty).
struct Split {
    scheme: String,
    netloc: String,
    hostname: String,
    port: Option<String>,
}

/// `urllib.parse.urlsplit` (Python 3.13) as far as `safe_link` needs it, for printable ASCII with no space. None
/// where it raises ValueError (an unbalanced bracket, a bracketed host that is no IPv6 or IPvFuture address) and for
/// an IPv6 host, which is never one of LINK_HOSTS.
fn urlsplit(url: &str) -> Option<Split> {
    let scheme_chars = |c: char| c.is_ascii_alphanumeric() || "+-.".contains(c);
    let mut scheme = String::new();
    let mut rest = url;
    if let Some(i) = url.find(':') {
        if i > 0 && url.as_bytes()[0].is_ascii_alphabetic() && url[..i].chars().all(scheme_chars) {
            scheme = url[..i].to_ascii_lowercase();
            rest = &url[i + 1..];
        }
    }
    let mut netloc = "";
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = &after[..end];
    }
    let (open, close) = (netloc.contains('['), netloc.contains(']'));
    if open != close {
        return None; // "Invalid IPv6 URL"
    }
    let hostinfo = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let (hostname, port) = if open {
        let bracketed = netloc.split_once('[').map_or("", |(_, b)| b);
        let bracketed = bracketed.split_once(']').map_or(bracketed, |(b, _)| b);
        // _check_bracketed_host: an IPvFuture address passes; an IPv6 one passes too but can be no app's host
        if !bracketed.starts_with('v') || !IPV_FUTURE.is_match(bracketed) {
            return None;
        }
        let inner = hostinfo.split_once('[').map_or("", |(_, b)| b);
        let (host, after) = inner.split_once(']').unwrap_or((inner, ""));
        (host, after.split_once(':').map_or("", |(_, p)| p))
    } else {
        hostinfo.split_once(':').unwrap_or((hostinfo, ""))
    };
    Some(Split {
        scheme,
        netloc: netloc.to_string(),
        hostname: hostname.to_string(),
        port: (!port.is_empty()).then(|| port.to_string()),
    })
}

/// `row` with its `link` when `url` passes `safe_link` for `kind`. Python's `_linked`.
pub fn linked(mut row: Item, url: Option<&Value>, kind: &str) -> Item {
    if let Some(link) = safe_link(url, Some(kind)) {
        if !link.is_empty() {
            row.insert("link".into(), Value::String(link));
        }
    }
    row
}

/// An account's alias as it goes in a ref (`wgmail:<alias>/<thread>`): lowercase letters, digits, - and _.
pub fn alias_word(alias: &str) -> String {
    let word = ALIAS.replace_all(&py::strip(alias).to_lowercase(), "-").into_owned();
    let word = py::head(word.trim_matches('-'), 32).to_string();
    if word.is_empty() {
        "other".to_string()
    } else {
        word
    }
}

/// `str.capitalize()`: the first character in title case, the rest lower case.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else { return String::new() };
    let head: String = match first {
        // the characters whose title case is not their upper case
        'Ǆ' | 'ǅ' | 'ǆ' => "ǅ".into(),
        'Ǉ' | 'ǈ' | 'ǉ' => "ǈ".into(),
        'Ǌ' | 'ǋ' | 'ǌ' => "ǋ".into(),
        'Ǳ' | 'ǲ' | 'ǳ' => "ǲ".into(),
        'ß' => "Ss".into(),
        'ﬀ' => "Ff".into(),
        'ﬁ' => "Fi".into(),
        'ﬂ' => "Fl".into(),
        'ﬃ' => "Ffi".into(),
        'ﬄ' => "Ffl".into(),
        'ﬅ' | 'ﬆ' => "St".into(),
        'ŉ' => "ʼN".into(),
        'և' => "Եւ".into(),
        c => c.to_uppercase().collect(),
    };
    head + &chars.as_str().to_lowercase()
}

/// An alias as a label ("work" → "Work"), "Other" for none: `alias.strip().capitalize() or "Other"`.
fn label_of(alias: &str) -> String {
    let label = capitalize(py::strip(alias));
    if label.is_empty() {
        "Other".to_string()
    } else {
        label
    }
}

// ---- the reader ------------------------------------------------------------------------------------------------

/// One gather's reads through `bridge`. The connected accounts are listed once, by the first source that asks; the
/// others wait for that answer.
pub struct Reader {
    pub bridge: Arc<dyn ComposioRead>,
    /// the listing: None until asked; then the accounts, or the failing call's exception type name
    accounts: tokio::sync::Mutex<Option<Result<Vec<Obj>, String>>>,
    /// the slugs `call` may run: READ_SLUGS (a test widens it, as Python's monkeypatched the module's set)
    allowed: Vec<String>,
    /// the slugs run, in order (tests)
    pub calls: parking_lot::Mutex<Vec<String>>,
}

impl Reader {
    pub fn new(bridge: Arc<dyn ComposioRead>) -> Self {
        Self::with_allowed(bridge, &READ_SLUGS)
    }

    /// A reader whose allow-list is `allowed` instead of READ_SLUGS. Only tests use it: the second guard
    /// (`is_read_only`, `consequential_slugs`) still refuses a writing slug that got onto the list.
    pub fn with_allowed(bridge: Arc<dyn ComposioRead>, allowed: &[&str]) -> Self {
        Reader {
            bridge,
            accounts: tokio::sync::Mutex::new(None),
            allowed: allowed.iter().map(|s| s.to_string()).collect(),
            calls: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// The connected accounts (mappings only), listed once. A failed listing fails every source that asks:
    /// "the connected accounts could not be listed (<type>)".
    pub async fn accounts(&self) -> Result<Vec<Obj>, ReadError> {
        let mut held = self.accounts.lock().await;
        if held.is_none() {
            *held = Some(match self.bridge.connected_accounts().await {
                Ok(got) => Ok(got
                    .into_iter()
                    .filter_map(|a| match a {
                        Value::Object(o) => Some(o),
                        _ => None,
                    })
                    .collect()),
                Err(e) => Err(e.type_name().to_string()), // the type only
            });
        }
        match held.as_ref() {
            Some(Ok(rows)) => Ok(rows.clone()),
            Some(Err(kind)) => Err(read_failed(format!("the connected accounts could not be listed ({kind})"))),
            None => Ok(Vec::new()),
        }
    }

    /// `[(alias, id)]` of the accounts `kind` reads, at most MAX_ACCOUNTS. For the work kinds only accounts aliased
    /// other than the owner's own (composio_tools.PRIMARY_ALIAS): the personal one is read directly. Off when there
    /// is none, failed when the only ones have expired.
    pub async fn accounts_for(&self, kind: &str) -> Result<Vec<(String, String)>, ReadError> {
        let Some(toolkit) = lookup(&TOOLKIT, kind) else {
            return Err(ReadError::failed("KeyError", format!("'{kind}'")));
        };
        let mut rows: Vec<Obj> = self
            .accounts()
            .await?
            .into_iter()
            .filter(|a| str_of(a.get("toolkit")).to_lowercase() == toolkit && a.get("id").is_some_and(py::truthy))
            .collect();
        if OTHER_ACCOUNT_TOOLKITS.contains(&toolkit) {
            rows.retain(|a| {
                let alias = py::strip(&str_of(a.get("alias"))).to_lowercase();
                !alias.is_empty() && alias != PRIMARY_ALIAS
            });
        }
        let live: Vec<(String, String)> = rows
            .iter()
            .filter(|a| str_of(a.get("status")) == ACTIVE)
            .map(|a| (str_of(a.get("alias")), py_str(a.get("id").unwrap_or(&Value::Null))))
            .collect();
        if !live.is_empty() {
            return Ok(live.into_iter().take(MAX_ACCOUNTS).collect());
        }
        if rows.iter().any(|a| LOST_STATES.contains(&str_of(a.get("status")).to_uppercase().as_str())) {
            return Err(read_failed(format!("the {toolkit} connection has expired")));
        }
        Err(source_off(format!("{toolkit} is not connected")))
    }

    /// Run one allowed read → its `data`. A permission error for a slug off the allow-list or one composio_tools says
    /// writes; failed when the call failed or sent no data.
    pub async fn call(&self, slug: &str, arguments: &Obj, account_id: &str) -> Result<Obj, ReadError> {
        if !self.allowed.iter().any(|s| s == slug) {
            return Err(permission(format!("{slug} is not one of Updates' reads")));
        }
        let tools = json!({"tools": [{"tool_slug": slug, "arguments": Value::Object(arguments.clone())}]});
        if !is_read_only(slug) || !consequential_slugs(MULTI_EXECUTE, &tools).is_empty() {
            return Err(permission(format!("{slug} is not a read")));
        }
        self.calls.lock().push(slug.to_string());
        let got = self.bridge.read_tool(slug, arguments, account_id).await;
        let Value::Object(got) = got else { return Err(read_failed(format!("{slug} failed"))) };
        if got.get("ok") == Some(&Value::Bool(false)) {
            return Err(read_failed(format!("{slug} failed")));
        }
        match got.get("data") {
            Some(Value::Object(data)) => Ok(data.clone()),
            _ => Err(read_failed(format!("{slug} sent no data"))),
        }
    }
}

// ---- small readers ---------------------------------------------------------------------------------------------

/// `table[key]` of one of the (kind, value) tables above.
fn lookup<T: Copy>(table: &[(&str, T)], key: &str) -> Option<T> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// `str(value or "")` of a mapping's value.
fn str_of(value: Option<&Value>) -> String {
    value.map(py_str_or_empty).unwrap_or_default()
}

/// `value or default`, Python's truthiness.
fn or<'a>(value: Option<&'a Value>, default: &'a Value) -> &'a Value {
    match value {
        Some(v) if py::truthy(v) => v,
        _ => default,
    }
}

/// `_one_line(mapping.get(key))`.
fn line_of(value: Option<&Value>) -> String {
    value.map(one_line).unwrap_or_default()
}

fn text(value: impl Into<String>) -> Value {
    Value::String(value.into())
}

/// A mapping's value when it is a mapping.
fn mapping(value: Option<&Value>) -> Option<&Obj> {
    match value {
        Some(Value::Object(o)) => Some(o),
        _ => None,
    }
}

/// `for x in value or []`: a list's items. A mapping's keys and a string's characters are never mappings, so every
/// loop here skips them all; a number or True is Python's TypeError, which fails the source.
fn py_iter(value: Option<&Value>) -> Result<&[Value], ReadError> {
    match value {
        Some(Value::Array(items)) => Ok(items),
        Some(v) if py::truthy(v) => match v {
            Value::Bool(_) => Err(not_iterable("bool")),
            Value::Number(n) if n.is_f64() => Err(not_iterable("float")),
            Value::Number(_) => Err(not_iterable("int")),
            _ => Ok(&[]),
        },
        _ => Ok(&[]),
    }
}

fn not_iterable(kind: &str) -> ReadError {
    ReadError::failed("TypeError", format!("'{kind}' object is not iterable"))
}

/// `float(text)` for digits and a dot (what `_SLACK_TS` and `_iso`'s pattern hold): any Unicode decimal digit
/// counts, and whitespace around it is ignored, as in Python.
fn digits_float(text: &str) -> f64 {
    let ascii: String = py::strip(text)
        .chars()
        .map(|c| {
            if c.is_ascii_digit() || c == '.' {
                c
            } else {
                py::int(&c.to_string()).and_then(|d| char::from_digit(d as u32, 10)).unwrap_or(c)
            }
        })
        .collect();
    ascii.parse().unwrap_or(0.0)
}

/// `datetime.timestamp()`: seconds since the epoch, to the microsecond.
fn timestamp(when: &Dt) -> f64 {
    when.timestamp_micros() as f64 / 1e6
}

/// `when.isoformat(timespec="seconds")`.
fn iso_seconds(when: &Dt) -> String {
    isoformat(&when.with_nanosecond(0).unwrap_or(*when))
}

/// `date.isoformat()`.
fn date_iso(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

static EPOCH_TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{9,12}(?:\.\d+)?\z").expect("EPOCH_TEXT"));
static DAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}\z").expect("DAY"));

/// `datetime.fromtimestamp(t, tz=timezone.utc).astimezone().isoformat(timespec="seconds")`, with the errors Python
/// raises for a time it cannot hold (OverflowError, OSError, ValueError), which fail the source.
fn from_timestamp(t: f64) -> Result<String, ReadError> {
    // _PyTime_ObjectToTimeval, ROUND_HALF_EVEN: the microseconds can carry into the second
    let mut whole = t.trunc();
    let micro = ((t - whole) * 1e6).round_ties_even();
    if micro >= 1e6 {
        whole += 1.0;
    } else if micro < 0.0 {
        whole -= 1.0;
    }
    if !(-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&whole) {
        return Err(ReadError::failed("OverflowError", "timestamp out of range for platform time_t"));
    }
    let secs = whole as i64;
    let Some(utc) = DateTime::<Utc>::from_timestamp(secs, 0) else {
        return Err(if secs.unsigned_abs() > 67_767_976_233_532_799 {
            ReadError::failed("OSError", "[Errno 84] Value too large to be stored in data type")
        } else {
            ReadError::failed("ValueError", "year is out of range")
        });
    };
    if !(1..=9999).contains(&utc.year()) {
        return Err(ReadError::failed("ValueError", format!("year {} is out of range", utc.year())));
    }
    let local = utc.with_timezone(&Local).fixed_offset();
    if !(1..=9999).contains(&local.year()) {
        return Err(ReadError::failed("OverflowError", "date value out of range"));
    }
    Ok(isoformat(&local))
}

/// `iso` with Python's errors: a time past what a datetime holds fails the source.
pub fn try_iso(value: &Value) -> Result<String, ReadError> {
    if let Value::Number(n) = value {
        return from_timestamp(n.as_f64().unwrap_or(0.0));
    }
    let text = one_line(value);
    if EPOCH_TEXT.is_match(&text) {
        return from_timestamp(digits_float(&text));
    }
    Ok(py::head(&text, 40).to_string())
}

/// A timestamp as ISO 8601: Slack's `ts` (epoch seconds, a number or a digit string), or an ISO string as it is (at
/// most 40 characters). Python's `_iso`. A time no datetime holds (Python raised) is "" here; the sources call
/// [`try_iso`], which fails like Python.
pub fn iso(value: &Value) -> String {
    try_iso(value).unwrap_or_default()
}

/// An ISO time, aware (a naive one read as UTC), or None. Python's `_parse`.
pub fn parse(value: &Value) -> Option<Dt> {
    let text = one_line(value);
    if text.is_empty() {
        return None;
    }
    let (naive, offset) = py::fromisoformat(&text.replace('Z', "+00:00"))?;
    let offset = offset.unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC"));
    offset.from_local_datetime(&naive).single()
}

/// The first ten characters when they are a YYYY-MM-DD date. Python's `_day`.
pub fn day(value: &Value) -> Option<String> {
    let text = one_line(value);
    let text = py::head(&text, 10);
    DAY.is_match(text).then(|| text.to_string())
}

// ---- Slack -----------------------------------------------------------------------------------------------------

pub static SLACK_USER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<@([A-Z0-9]+)(?:\|[^>]*)?>").expect("SLACK_USER"));
pub static SLACK_CHANNEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<#[A-Z0-9]+\|([^>]*)>").expect("SLACK_CHANNEL"));
pub static SLACK_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<((?:https?|mailto):[^>|]+)(?:\|([^>]*))?>").expect("SLACK_LINK"));
pub static SLACK_SPECIAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<!(here|channel|everyone)[^>]*>").expect("SLACK_SPECIAL"));

/// Slack's markup as plain words: the owner's mention as "@you", another's as "@someone", a channel by its name, a
/// link by its label (or "(link)").
pub fn slack_text(text: &Value, owner: Option<&str>) -> String {
    let s = py_str_or_empty(text);
    let s = SLACK_USER.replace_all(&s, |c: &Captures| {
        if owner.is_some_and(|o| !o.is_empty() && &c[1] == o) {
            "@you".to_string()
        } else {
            "@someone".to_string()
        }
    });
    let s = SLACK_CHANNEL.replace_all(&s, |c: &Captures| format!("#{}", &c[1]));
    let s = SLACK_LINK.replace_all(&s, |c: &Captures| {
        c.get(2).map(|m| m.as_str()).filter(|l| !l.is_empty()).unwrap_or("(link)").to_string()
    });
    let s = SLACK_SPECIAL.replace_all(&s, |c: &Captures| format!("@{}", &c[1]));
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// A search answer's `messages.matches`, mappings only. Python's `_matches`.
pub fn matches(data: &Obj) -> Vec<Obj> {
    match mapping(data.get("messages")).and_then(|m| m.get("matches")) {
        Some(Value::Array(got)) => got
            .iter()
            .filter_map(|m| match m {
                Value::Object(o) => Some(o.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A match with a well-formed channel id and ts. Python's `_slack_ok`.
pub fn slack_ok(m: &Obj) -> bool {
    let id_ok = matches!(mapping(m.get("channel")).and_then(|ch| ch.get("id")), Some(Value::String(id)) if SLACK_ID.is_match(id));
    id_ok && matches!(m.get("ts"), Some(Value::String(ts)) if SLACK_TS.is_match(ts))
}

/// A well-formed match's channel id and ts (`slack_ok` holds).
fn channel_ts(m: &Obj) -> (String, String) {
    let ch = mapping(m.get("channel")).map(|c| str_of(c.get("id"))).unwrap_or_default();
    (ch, str_of(m.get("ts")))
}

/// `m["user"] == owner`.
fn user_is(m: &Obj, owner: &str) -> bool {
    matches!(m.get("user"), Some(Value::String(u)) if u == owner)
}

/// Direct messages and mentions of the owner from the last SLACK_DAYS days that the owner has not answered since
/// (no message of theirs in that conversation after it), one item per conversation, newest first:
/// `slack:<channel>/<ts of its newest message>`.
pub async fn slack_items(reader: &Reader, now: Dt) -> Result<Vec<Item>, ReadError> {
    let since = now - Duration::days(SLACK_DAYS);
    // Slack's after: leaves its own day out
    let after = date_iso(since.date_naive().pred_opt().unwrap_or(NaiveDate::MIN));
    let since_ts = timestamp(&since);
    let someone = text("someone");
    let mut out: Vec<Item> = Vec::new();
    for (_alias, account) in reader.accounts_for("slack").await? {
        let search = |query: String, count: i64| -> Obj {
            let mut args = Obj::new();
            args.insert("count".into(), json!(count));
            args.insert("sort".into(), text("timestamp"));
            args.insert("sort_dir".into(), text("desc"));
            args.insert("query".into(), Value::String(query));
            args
        };
        let mine = matches(&reader.call("SLACK_SEARCH_MESSAGES", &search("from:me".into(), 100), &account).await?);
        let owner: Option<String> = mine.iter().find_map(|m| match m.get("user") {
            Some(Value::String(u)) if !u.is_empty() => Some(u.clone()),
            _ => None,
        });
        let mut last_mine: HashMap<String, f64> = HashMap::new();
        for m in mine.iter().filter(|m| slack_ok(m)) {
            let (ch, ts) = channel_ts(m);
            let ts = digits_float(&ts);
            let entry = last_mine.entry(ch).or_insert(0.0);
            *entry = entry.max(ts);
        }
        let dms = search(format!("is:dm after:{after}"), SLACK_COUNT);
        let mut found: Vec<Obj> = matches(&reader.call("SLACK_SEARCH_MESSAGES", &dms, &account).await?)
            .into_iter()
            .filter(|m| {
                mapping(m.get("channel")).is_some_and(|ch| {
                    ch.get("is_im").is_some_and(py::truthy) || ch.get("is_mpim").is_some_and(py::truthy)
                })
            })
            .collect();
        if let Some(owner) = owner.as_deref().filter(|o| SLACK_ID.is_match(o)) {
            let query = format!("<@{owner}> after:{after}");
            found.extend(matches(&reader.call("SLACK_SEARCH_MESSAGES", &search(query, SLACK_COUNT), &account).await?));
        }
        let mut convs: IndexMap<String, Vec<Obj>> = IndexMap::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for m in found {
            if !slack_ok(&m) || owner.as_deref().is_some_and(|o| user_is(&m, o)) {
                continue;
            }
            let (ch, ts) = channel_ts(&m);
            let at = digits_float(&ts);
            if seen.contains(&(ch.clone(), ts.clone()))
                || at < since_ts
                || last_mine.get(&ch).copied().unwrap_or(0.0) > at
            {
                continue;
            }
            seen.insert((ch.clone(), ts));
            convs.entry(ch).or_default().push(m);
        }
        for (ch, mut msgs) in convs {
            msgs.sort_by(|a, b| digits_float(&str_of(a.get("ts"))).total_cmp(&digits_float(&str_of(b.get("ts")))));
            let newest = msgs.last().cloned().unwrap_or_default();
            let channel = mapping(newest.get("channel")).cloned().unwrap_or_default();
            let who = one_line(or(newest.get("username"), &someone));
            let title = if channel.get("is_im").is_some_and(py::truthy) {
                format!("Slack: direct message from {who}")
            } else if channel.get("is_mpim").is_some_and(py::truthy) {
                format!("Slack: group message from {who}")
            } else {
                let name = line_of(channel.get("name"));
                let name = if name.is_empty() { "a channel".to_string() } else { name };
                format!("Slack: mention in #{name} from {who}")
            };
            let n = msgs.len();
            let mut lines = vec![format!("{n} message{} the owner has not answered.", if n != 1 { "s" } else { "" })];
            for m in &msgs[n.saturating_sub(5)..] {
                let name = one_line(or(m.get("username"), &someone));
                let said = slack_text(m.get("text").unwrap_or(&Value::Null), owner.as_deref());
                lines.push(format!("{name}: {said}"));
            }
            let ts = str_of(newest.get("ts"));
            let when = try_iso(&text(ts.clone()))?;
            let row = item(
                &format!("slack:{ch}/{ts}"),
                "slack",
                &text(title),
                &text(lines.join("\n")),
                &text(who),
                &text(when),
            );
            out.push(linked(row, newest.get("permalink"), "slack"));
        }
    }
    let when_of = |r: &Item| str_of(r.get("when"));
    out.sort_by_key(|r| std::cmp::Reverse(when_of(r)));
    out.truncate(SLACK_MAX);
    Ok(out)
}

// ---- Notion ----------------------------------------------------------------------------------------------------

/// A rich-text list's plain text. Python's `_plain`.
pub fn plain(rich: Option<&Value>) -> String {
    let Some(Value::Array(rich)) = rich else { return String::new() };
    rich.iter()
        .filter_map(|r| match r {
            Value::Object(r) => Some(str_of(r.get("plain_text"))),
            _ => None,
        })
        .collect()
}

/// A rich-text list mentions the user `owner`. Python's `_mentions`.
pub fn mentions(rich: Option<&Value>, owner: Option<&str>) -> bool {
    let (Some(owner), Some(Value::Array(rich))) = (owner.filter(|o| !o.is_empty()), rich) else { return false };
    rich.iter().any(|r| {
        let user = match r {
            Value::Object(r) => mapping(r.get("mention")).and_then(|m| mapping(m.get("user"))),
            _ => None,
        };
        user.is_some_and(|u| matches!(u.get("id"), Some(Value::String(id)) if id == owner))
    })
}

/// `notion_page` with Python's errors: a `people` property that is a number or true is a TypeError.
pub fn try_notion_page(page: &Obj, owner: Option<&str>) -> Result<Obj, ReadError> {
    let owner = owner.filter(|o| !o.is_empty());
    let empty = Obj::new();
    let props = mapping(page.get("properties")).unwrap_or(&empty);
    let (mut title, mut status, mut due, mut assigned, mut done) = (String::new(), String::new(), None, false, false);
    for (name, prop) in props {
        let Value::Object(prop) = prop else { continue };
        let kind = prop.get("type");
        let is = |k: &str| matches!(kind, Some(Value::String(s)) if s == k);
        let name = py::strip(name).to_lowercase();
        if is("title") {
            if title.is_empty() {
                title = plain(prop.get("title"));
            }
        } else if is("people") && owner.is_some() {
            if !assigned {
                for p in py_iter(prop.get("people"))? {
                    if matches!(p, Value::Object(p) if matches!(p.get("id"), Some(Value::String(id)) if Some(id.as_str()) == owner))
                    {
                        assigned = true;
                        break;
                    }
                }
            }
        } else if (is("status") || is("select")) && (name == "status" || name == "state") {
            let key = if is("status") { "status" } else { "select" };
            if let Some(value) = mapping(prop.get(key)) {
                if value.get("name").is_some_and(py::truthy) {
                    status = line_of(value.get("name"));
                    done = done || DONE_WORDS.contains(&status.to_lowercase().as_str());
                }
            }
        } else if is("checkbox") && ["done", "complete", "completed"].contains(&name.as_str()) {
            done = done || prop.get("checkbox") == Some(&Value::Bool(true));
        } else if is("date") && due.is_none() {
            due = mapping(prop.get("date")).and_then(|v| day(v.get("start").unwrap_or(&Value::Null)));
        }
    }
    let mut out = Obj::new();
    out.insert("title".into(), Value::String(title));
    out.insert("status".into(), Value::String(status));
    out.insert("due".into(), due.map_or(Value::Null, Value::String));
    out.insert("assigned".into(), Value::Bool(assigned));
    out.insert("done".into(), Value::Bool(done));
    Ok(out)
}

/// What a page's properties say: `{"title", "status", "due", "assigned", "done"}`: its title, whether it is assigned
/// to the owner (a people property holding them), its status (a status or select property named Status or State),
/// whether that reads as done (or a checkbox named Done is ticked), and its first date. A page Python could not read
/// (a TypeError) reads as nothing here; `notion_items` calls [`try_notion_page`], which fails like Python.
pub fn notion_page(page: &Obj, owner: Option<&str>) -> Obj {
    try_notion_page(page, owner).unwrap_or_else(|_| {
        let mut out = Obj::new();
        out.insert("title".into(), text(""));
        out.insert("status".into(), text(""));
        out.insert("due".into(), Value::Null);
        out.insert("assigned".into(), Value::Bool(false));
        out.insert("done".into(), Value::Bool(false));
        out
    })
}

/// `notion_blocks` with Python's errors: `results` that is a number or true is a TypeError.
pub fn try_notion_blocks(data: &Obj, owner: Option<&str>) -> Result<(Vec<String>, Vec<String>), ReadError> {
    let (mut todos, mut found) = (Vec::new(), Vec::new());
    for block in py_iter(data.get("results"))? {
        let Value::Object(block) = block else { continue };
        if block.get("archived").is_some_and(py::truthy) || block.get("in_trash").is_some_and(py::truthy) {
            continue;
        }
        let kind = match block.get("type") {
            Some(Value::String(k)) => k.as_str(),
            _ => continue,
        };
        let Some(body) = mapping(block.get(kind)) else { continue };
        let rich = body.get("rich_text");
        let words = plain(rich);
        if kind == "to_do" && body.get("checked") == Some(&Value::Bool(false)) && !py::strip(&words).is_empty() {
            todos.push(one_line_str(&words));
        } else if mentions(rich, owner) {
            found.push(one_line_str(&words));
        }
    }
    Ok((todos, found))
}

/// (open to-dos, lines that mention the owner) among a page's first-level blocks. Blocks Python could not read (a
/// TypeError) are none here; `notion_items` calls [`try_notion_blocks`], which fails like Python.
pub fn notion_blocks(data: &Obj, owner: Option<&str>) -> (Vec<String>, Vec<String>) {
    try_notion_blocks(data, owner).unwrap_or_default()
}

/// Pages edited in the last NOTION_DAYS days that hold an open to-do, mention the owner, or are assigned to the
/// owner and not done; most recently edited first: `notion:<page>`. The blocks of at most NOTION_PAGES pages are
/// read, and none after `deadline` on `clock`.
pub async fn notion_items(
    reader: &Reader,
    now: Dt,
    deadline: Option<f64>,
    clock: &Mono,
) -> Result<Vec<Item>, ReadError> {
    let since = now - Duration::days(NOTION_DAYS);
    let mut out: Vec<Item> = Vec::new();
    for (_alias, account) in reader.accounts_for("notion").await? {
        let about = reader.call("NOTION_GET_ABOUT_ME", &Obj::new(), &account).await?;
        let owner: Option<String> = mapping(about.get("bot"))
            .and_then(|bot| mapping(bot.get("owner")))
            .and_then(|o| mapping(o.get("user")))
            .and_then(|u| match u.get("id") {
                Some(Value::String(id)) => Some(id.clone()),
                _ => None,
            });
        let mut args = Obj::new();
        args.insert("query".into(), text(""));
        args.insert("timestamp".into(), text("last_edited_time"));
        args.insert("direction".into(), text("descending"));
        args.insert("page_size".into(), json!(NOTION_SEARCH));
        args.insert("filter_value".into(), text("page"));
        args.insert("filter_property".into(), text("object"));
        let found = reader.call("NOTION_SEARCH_NOTION_PAGE", &args, &account).await?;
        let pages: Vec<&Obj> = py_iter(found.get("results"))?
            .iter()
            .filter_map(|p| match p {
                Value::Object(p) => Some(p),
                _ => None,
            })
            .filter(|p| {
                p.get("object") == Some(&text("page"))
                    && !p.get("archived").is_some_and(py::truthy)
                    && !p.get("in_trash").is_some_and(py::truthy)
                    && matches!(p.get("id"), Some(Value::String(id)) if SAFE_ID.is_match(id))
                    && parse(p.get("last_edited_time").unwrap_or(&Value::Null)).unwrap_or(since) > since
            })
            .collect();
        let mut read = 0usize;
        for page in pages {
            let facts = try_notion_page(page, owner.as_deref())?;
            if facts.get("done") == Some(&Value::Bool(true)) {
                continue;
            }
            let id = str_of(page.get("id"));
            let (mut todos, mut found) = (Vec::new(), Vec::new());
            if read < NOTION_PAGES && deadline.is_none_or(|d| clock() < d) {
                read += 1;
                let mut args = Obj::new();
                args.insert("block_id".into(), text(id.clone()));
                args.insert("page_size".into(), json!(NOTION_BLOCKS));
                let blocks = reader.call("NOTION_FETCH_BLOCK_CONTENTS", &args, &account).await?;
                (todos, found) = try_notion_blocks(&blocks, owner.as_deref())?;
            }
            let assigned = facts.get("assigned") == Some(&Value::Bool(true));
            if todos.is_empty() && found.is_empty() && !assigned {
                continue;
            }
            let status = str_of(facts.get("status"));
            let mut lines = Vec::new();
            if assigned {
                let tail = if status.is_empty() { String::new() } else { format!(" Status: {status}.") };
                lines.push(format!("Assigned to the owner.{tail}"));
            } else if !status.is_empty() {
                lines.push(format!("Status: {status}."));
            }
            if let Some(Value::String(due)) = facts.get("due") {
                lines.push(format!("Date: {due}."));
            }
            lines.extend(todos.iter().map(|t| format!("- [ ] {t}")));
            lines.extend(found.iter().map(|m| format!("Mentions the owner: {m}")));
            let title = str_of(facts.get("title"));
            let title = format!("Notion: {}", if title.is_empty() { "Untitled" } else { &title });
            let when = try_iso(page.get("last_edited_time").unwrap_or(&Value::Null))?;
            let row =
                item(&format!("notion:{id}"), "notion", &text(title), &text(lines.join("\n")), &text(""), &text(when));
            out.push(linked(row, page.get("url"), "notion"));
            if out.len() >= NOTION_MAX {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

// ---- Linear ----------------------------------------------------------------------------------------------------

/// Open issues assigned to the owner that are due within LINEAR_DUE_DAYS days (or overdue) or were updated in the
/// last LINEAR_UPDATED_DAYS days; the nearest due date first, then the latest update: `linear:<issue id>`.
pub async fn linear_items(reader: &Reader, now: Dt) -> Result<Vec<Item>, ReadError> {
    let due_by = date_iso((now + Duration::days(LINEAR_DUE_DAYS)).date_naive());
    let updated_since = now - Duration::days(LINEAR_UPDATED_DAYS);
    let mut picked: Vec<((bool, String, f64), Item)> = Vec::new();
    for (_alias, account) in reader.accounts_for("linear").await? {
        let mut args = Obj::new();
        args.insert("assignee_id".into(), text("me"));
        args.insert("first".into(), json!(LINEAR_FIRST));
        let data = reader.call("LINEAR_LIST_LINEAR_ISSUES", &args, &account).await?;
        for issue in py_iter(data.get("issues"))? {
            let Value::Object(issue) = issue else { continue };
            let id = match issue.get("id") {
                Some(Value::String(id)) if SAFE_ID.is_match(id) => id.clone(),
                _ => continue,
            };
            let state = line_of(mapping(issue.get("state")).and_then(|s| s.get("name")));
            if DONE_WORDS.contains(&state.to_lowercase().as_str()) {
                continue;
            }
            let due = day(issue.get("dueDate").unwrap_or(&Value::Null));
            let updated = parse(issue.get("updatedAt").unwrap_or(&Value::Null));
            if !(due.as_deref().is_some_and(|d| d <= due_by.as_str()) || updated.is_some_and(|u| u >= updated_since)) {
                continue;
            }
            let ident = line_of(issue.get("identifier"));
            let mut lines = vec![if state.is_empty() { String::new() } else { format!("State: {state}.") }];
            // isinstance(priority, int): True is 1
            let priority = match issue.get("priority") {
                Some(Value::Bool(b)) => Some(i64::from(*b)),
                Some(Value::Number(n)) => n.as_i64(),
                _ => None,
            };
            if let Some(label) = priority.and_then(|p| lookup_int(&PRIORITY, p)) {
                lines.push(format!("Priority: {label}."));
            }
            if let Some(d) = &due {
                lines.push(format!("Due: {d}."));
            }
            for (key, label) in [("project", "Project"), ("team", "Team")] {
                if let Some(value) = mapping(issue.get(key)) {
                    if value.get("name").is_some_and(py::truthy) {
                        lines.push(format!("{label}: {}.", line_of(value.get("name"))));
                    }
                }
            }
            if let Some(u) = &updated {
                lines.push(format!("Updated: {}.", date_iso(u.date_naive())));
            }
            if let Some(description) = issue.get("description").filter(|d| py::truthy(d)) {
                lines.push(py_str(description));
            }
            let title = if ident.is_empty() {
                format!("Linear: {}", py_str(issue.get("title").unwrap_or(&Value::Null)))
            } else {
                format!("Linear {ident}: {}", line_of(issue.get("title")))
            };
            let body: Vec<String> = lines.into_iter().filter(|x| !x.is_empty()).collect();
            let when = due.clone().unwrap_or_else(|| updated.as_ref().map(iso_seconds).unwrap_or_default());
            let row =
                item(&format!("linear:{id}"), "linear", &text(title), &text(body.join("\n")), &text(""), &text(when));
            let key = (due.is_none(), due.clone().unwrap_or_default(), -updated.as_ref().map_or(0.0, timestamp));
            picked.push((key, linked(row, issue.get("url"), "linear")));
        }
    }
    picked.sort_by(|(a, _), (b, _)| {
        a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.partial_cmp(&b.2).unwrap_or(Ordering::Equal))
    });
    Ok(picked.into_iter().take(LINEAR_MAX).map(|(_, row)| row).collect())
}

fn lookup_int(table: &[(i64, &'static str)], key: i64) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

// ---- the work Gmail and Calendar -------------------------------------------------------------------------------

/// A message's `labelIds` as strings. Python's `_labels`.
pub fn labels(m: &Obj) -> Vec<String> {
    match m.get("labelIds") {
        Some(Value::Array(got)) => got.iter().map(py_str).collect(),
        _ => Vec::new(),
    }
}

/// Each other Gmail account's inbox of the last 7 days without promotions and social (updates::MAIL_QUERY), one
/// item per thread with its newest message: `wgmail:<account>/<thread>`. A thread the owner wrote in after that
/// message (the account's sent mail of the same 7 days) says so.
pub async fn work_email_items(reader: &Reader, now: Dt) -> Result<Vec<Item>, ReadError> {
    let _ = now;
    let mut out: Vec<Item> = Vec::new();
    for (alias, account) in reader.accounts_for("work_email").await? {
        let word = alias_word(&alias);
        let label = label_of(&alias);
        let fetch = |query: &str, max: i64, verbose: bool| -> Obj {
            let mut args = Obj::new();
            args.insert("query".into(), text(query));
            args.insert("max_results".into(), json!(max));
            args.insert("verbose".into(), Value::Bool(verbose));
            args.insert("include_payload".into(), Value::Bool(false));
            args
        };
        let inbox = reader.call("GMAIL_FETCH_EMAILS", &fetch(MAIL_QUERY, WORK_MAIL_MAX as i64, true), &account).await?;
        let sent = reader.call("GMAIL_FETCH_EMAILS", &fetch("in:sent newer_than:7d", 50, false), &account).await?;
        let mut replied: HashMap<String, String> = HashMap::new();
        for m in py_iter(sent.get("messages"))? {
            let Value::Object(m) = m else { continue };
            if let Some(Value::String(thread)) = m.get("threadId") {
                let when = line_of(m.get("messageTimestamp"));
                let entry = replied.entry(thread.clone()).or_default();
                if when > *entry {
                    *entry = when;
                }
            }
        }
        let mut threads: IndexMap<String, &Obj> = IndexMap::new();
        for m in py_iter(inbox.get("messages"))? {
            let Value::Object(m) = m else { continue };
            let thread = match m.get("threadId") {
                Some(Value::String(t)) if SAFE_ID.is_match(t) => t.clone(),
                _ => continue,
            };
            if labels(m).iter().any(|l| l == "SENT") {
                continue;
            }
            let newer = match threads.get(&thread) {
                None => true,
                Some(old) => line_of(m.get("messageTimestamp")) > line_of(old.get("messageTimestamp")),
            };
            if newer {
                threads.insert(thread, m);
            }
        }
        let mut ordered: Vec<(String, &Obj)> = threads.into_iter().collect();
        ordered.sort_by_key(|(_, m)| std::cmp::Reverse(line_of(m.get("messageTimestamp"))));
        let empty = Obj::new();
        let blank = text("");
        for (thread, m) in ordered {
            let preview = mapping(m.get("preview")).unwrap_or(&empty);
            let body_text = py_str_or_empty(or(m.get("messageText"), or(preview.get("body"), &blank)));
            let when = line_of(m.get("messageTimestamp"));
            let mut notes = Vec::new();
            let mine = replied.get(&thread).cloned().unwrap_or_default();
            if mine > when {
                notes.push(format!("The owner already replied after this message, on {}.", py::head(&mine, 10)));
            }
            let body = if notes.is_empty() {
                body_text
            } else {
                notes.push(body_text);
                notes.join("\n")
            };
            let subject = line_of(m.get("subject"));
            let subject = if subject.is_empty() { "(no subject)".to_string() } else { subject };
            let row = item(
                &format!("wgmail:{word}/{thread}"),
                "work_email",
                &text(format!("{label} email: {subject}")),
                &text(body),
                m.get("sender").unwrap_or(&Value::Null),
                &text(try_iso(&text(when))?),
            );
            out.push(linked(row, m.get("display_url"), "work_email"));
        }
    }
    out.truncate(WORK_MAIL_MAX);
    Ok(out)
}

/// Each other calendar account's events of the next 7 days that the owner has not declined, soonest first, as
/// updates::calendar_items reads the owner's own (an unanswered invitation is marked): `wcal:<account>/<event>`.
pub async fn work_calendar_items(reader: &Reader, now: Dt) -> Result<Vec<Item>, ReadError> {
    let mut out: Vec<Item> = Vec::new();
    let end = now + Duration::days(WORK_CALENDAR_DAYS);
    for (alias, account) in reader.accounts_for("work_calendar").await? {
        let word = alias_word(&alias);
        let label = label_of(&alias);
        let mut args = Obj::new();
        args.insert("calendarId".into(), text("primary"));
        args.insert("timeMin".into(), text(isoformat(&now)));
        args.insert("timeMax".into(), text(isoformat(&end)));
        args.insert("singleEvents".into(), Value::Bool(true));
        args.insert("orderBy".into(), text("startTime"));
        args.insert("maxResults".into(), json!(50));
        let data = reader.call("GOOGLECALENDAR_EVENTS_LIST", &args, &account).await?;
        let mut seen: HashSet<String> = HashSet::new();
        for ev in py_iter(data.get("items"))? {
            let Value::Object(ev) = ev else { continue };
            let id = match ev.get("id") {
                Some(Value::String(id)) if SAFE_ID.is_match(id) && !seen.contains(id) => id.clone(),
                _ => continue,
            };
            if let Some(row) =
                event_item(ev, &format!("wcal:{word}/{id}"), "work_calendar", &format!("{label} calendar: "))
            {
                seen.insert(id);
                out.push(linked(row, ev.get("htmlLink"), "work_calendar"));
            }
        }
    }
    Ok(out)
}

/// The kinds READERS holds a read for (Python's `READERS` dict, by key): every one of KINDS.
pub const READERS: [&str; 5] = KINDS;

/// `READERS[kind](reader, now)`: one source's read with its defaults (Notion with no deadline).
pub async fn read_kind(kind: &str, reader: &Reader, now: Dt) -> Result<Vec<Item>, ReadError> {
    match kind {
        "slack" => slack_items(reader, now).await,
        "notion" => {
            let clock: Mono = Arc::new(monotonic);
            notion_items(reader, now, None, &clock).await
        }
        "linear" => linear_items(reader, now).await,
        "work_email" => work_email_items(reader, now).await,
        "work_calendar" => work_calendar_items(reader, now).await,
        other => Err(ReadError::failed("KeyError", format!("'{other}'"))),
    }
}

/// One source's read, as updates::gather runs it.
pub type ReadFn = Box<dyn FnOnce() -> BoxFuture<'static, Result<Vec<Item>, ReadError>> + Send>;

/// `(kind, read)` for each of KINDS, as updates::gather runs them; `read` is None (the source is off) without a
/// bridge these sources can read through. One Reader serves them all, so the accounts are listed once. Notion's
/// deadline is `mono() + limits["notion"] * 0.7`, read when its read starts.
pub fn reads(
    bridge: Option<Arc<dyn ComposioRead>>,
    now: Dt,
    limits: &HashMap<String, f64>,
    mono: Mono,
) -> Vec<(&'static str, Option<ReadFn>)> {
    let Some(bridge) = bridge.filter(|b| usable(Some(b))) else {
        return KINDS.iter().map(|kind| (*kind, None)).collect();
    };
    let reader = Arc::new(Reader::new(bridge));
    let notion_limit = limits.get("notion").copied().unwrap_or_else(|| lookup(&LIMITS, "notion").unwrap_or(45.0));
    KINDS
        .iter()
        .map(|kind| {
            let kind: &'static str = kind;
            let reader = reader.clone();
            let mono = mono.clone();
            let read: ReadFn = Box::new(move || {
                Box::pin(async move {
                    if kind == "notion" {
                        let deadline = mono() + notion_limit * 0.7;
                        notion_items(&reader, now, Some(deadline), &mono).await
                    } else {
                        read_kind(kind, &reader, now).await
                    }
                })
            });
            (kind, Some(read))
        })
        .collect()
}
