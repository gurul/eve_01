//! One Alexandria government-index query and bounded reads of its official sources.
//!
//! A port of `bridge/src/eve_bridge/america_index.py`.
//!
//! The index returns evidence for Eve's existing answer model, never an answer or a law's effective date. This
//! module does not discover tools, retry, accept terms, or switch providers. [`eve_web::firecrawl::run_tool`]
//! owns the shared paid-call cap.
//!
//! Python's module globals the tests monkeypatch (`firecrawl.run_tool`, `firecrawl.meter`, `watch.http_request`,
//! `time.monotonic`) are seams on [`GovernmentIndex`]: a [`Runner`], a [`Meter`], a [`Fetch`], a [`Clock`] and the
//! [`Ctx`] the defaults use.

use std::sync::{Arc, LazyLock};

use chrono::{DateTime, Datelike, SecondsFormat, Utc};
use eve_web::firecrawl::{self, ToolCall};
use eve_web::watch::{self, net::UrlGuard, HttpOptions};
use eve_web::{Clock, Ctx};
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Number, Value};

pub const MAX_RESULTS: usize = 5;
pub const MAX_SOURCES: usize = 3;
pub const MAX_EXCERPT: usize = 2000;
pub const MAX_PAGE_TEXT: usize = 6000;
pub const MAX_QUERY_SECS: f64 = 55.0;
pub const MAX_READ_SECS: f64 = 8.0;

const UNRESERVED: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~";
const SESSION_KEYS: [&str; 11] = [
    "jsessionid",
    "phpsessid",
    "session",
    "sessionid",
    "session_id",
    "sessiontoken",
    "session_token",
    "access_token",
    "auth_token",
    "cfid",
    "cftoken",
];
const TRACKING_KEYS: [&str; 8] = ["gclid", "dclid", "fbclid", "msclkid", "mc_cid", "mc_eid", "_ga", "_gl"];

/// `_PATH_SESSION`: `(?:;|%3b)(?:jsessionid|phpsessid|sessionid)(?:=|%3d)[^/;?#]*` under `re.I`. Python's
/// Unicode IGNORECASE also lets `i` match U+0130 and U+0131 and `s` match U+017F, which Rust's `(?i)` does not,
/// so each letter is spelled as the class Python matches.
static PATH_SESSION: LazyLock<Regex> = LazyLock::new(|| {
    let word = |w: &str| -> String {
        w.chars()
            .map(|c| match c {
                'i' => "[iI\u{130}\u{131}]".to_string(),
                's' => "[sS\u{17f}]".to_string(),
                c => format!("[{c}{}]", c.to_ascii_uppercase()),
            })
            .collect()
    };
    let pattern = format!(
        "(?:;|%3[bB])(?:{}|{}|{})(?:=|%3[dD])[^/;?#]*",
        word("jsessionid"),
        word("phpsessid"),
        word("sessionid")
    );
    Regex::new(&pattern).expect("the session pattern compiles")
});

/// A failed lookup, with static prose safe to show the owner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct IndexFailure(pub String);

impl IndexFailure {
    fn new(why: &str) -> Self {
        IndexFailure(why.to_string())
    }
}

/// `GovernmentIndex(timeout_secs=...)`'s ValueError.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Government index timeout must be finite and positive.")]
pub struct InvalidTimeout;

/// `runner(request, timeout=...)`: the Alexandria call (`firecrawl.run_tool`). It gets the request dict and
/// returns the provider's free-form result (`{"data": ..., "credits": n}`); any error is an unavailable index.
pub type Runner = Arc<dyn Fn(&Value, f64) -> anyhow::Result<Value> + Send + Sync>;
/// `fetch(url, timeout=...)`: one source read, (status, body text).
pub type Fetch = Arc<dyn Fn(&str, f64) -> anyhow::Result<(u16, String)> + Send + Sync>;
/// `firecrawl.meter(what, credits, feature, price)`.
pub type Meter = Arc<dyn Fn(&str, Option<&Number>, &str, Option<f64>) -> anyhow::Result<()> + Send + Sync>;

/// One official source in the evidence (the Python dict, keys in its order).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub title: String,
    pub url: String,
    pub currency: String,
    pub excerpt: String,
    pub page_text: String,
    pub read_status: String,
    pub text_truncated: bool,
    pub indexed_at: Option<String>,
    pub fetched_at: Option<String>,
}

/// `query`'s answer (the Python dict, keys in its order).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub ok: bool,
    pub status: String,
    pub provider: String,
    pub question: String,
    pub jurisdiction: String,
    pub retrieved_at: String,
    pub legal_advice: bool,
    pub sources: Vec<Source>,
}

impl Evidence {
    /// The dict America hands the model.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// `str.isspace()`: Rust's White_Space plus the four C0 separators Python also counts.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Two ASCII hex digits at `bytes[at..at + 2]`.
fn hex_pair(bytes: &[u8], at: usize) -> bool {
    bytes.len() >= at + 2 && bytes[at].is_ascii_hexdigit() && bytes[at + 1].is_ascii_hexdigit()
}

/// `_BAD_PERCENT.search(value)`: a `%` not followed by two hex digits.
fn bad_percent(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.iter().enumerate().any(|(i, &b)| {
        b == b'%' && !hex_pair(bytes, i + 1)
    })
}

fn hex_value(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        _ => b - b'A' + 10,
    }
}

/// `_unquote_impl(ascii_run)`: `%XX` to its byte, any other `%` kept.
fn unquote_bytes(run: &str) -> Vec<u8> {
    let bytes = run.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && hex_pair(bytes, i + 1) {
            out.push(hex_value(bytes[i + 1]) * 16 + hex_value(bytes[i + 2]));
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// `urllib.parse.unquote_plus(s)` (UTF-8, errors="replace"): `+` is a space; each ASCII run is unquoted and
/// decoded on its own, non-ASCII text is kept as written.
fn unquote_plus(s: &str) -> String {
    let s = s.replace('+', " ");
    if !s.contains('%') {
        return s;
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while !rest.is_empty() {
        let ascii_end = rest.find(|c: char| !c.is_ascii()).unwrap_or(rest.len());
        let (ascii, tail) = rest.split_at(ascii_end);
        out.push_str(&String::from_utf8_lossy(&unquote_bytes(ascii)));
        let other_end = tail.find(|c: char| c.is_ascii()).unwrap_or(tail.len());
        out.push_str(&tail[..other_end]);
        rest = &tail[other_end..];
    }
    out
}

/// `parse_qsl(qs, keep_blank_values=True, max_num_fields=100)`; Err is its "Max number of fields exceeded".
fn parse_qsl(qs: &str) -> Result<Vec<(String, String)>, ()> {
    if qs.is_empty() {
        return Ok(Vec::new());
    }
    if 1 + qs.matches('&').count() > 100 {
        return Err(());
    }
    Ok(qs
        .split('&')
        .filter(|field| !field.is_empty())
        .map(|field| {
            let (name, value) = field.split_once('=').unwrap_or((field, ""));
            (unquote_plus(name), unquote_plus(value))
        })
        .collect())
}

/// `urllib.parse.quote_plus(s, safe="")`.
fn quote_plus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b' ' => out.push('+'),
            b if b.is_ascii_alphanumeric() || b"_.-~".contains(&b) => out.push(b as char),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `urllib.parse.urlencode(pairs)`.
fn urlencode(pairs: &[(String, String)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v))).collect::<Vec<_>>().join("&")
}

/// `_HOST.fullmatch(label)`: `[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?`.
fn host_label(label: &str) -> bool {
    let b = label.as_bytes();
    let edge = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    !b.is_empty()
        && b.len() <= 63
        && edge(b[0])
        && edge(b[b.len() - 1])
        && b.iter().all(|&c| edge(c) || c == b'-')
}

fn bad_char(c: char) -> bool {
    (c as u32) < 32 || c as u32 == 127 || c == '\\'
}

/// The path with each `%XX` of an unreserved character decoded and every other one uppercased. None for an
/// escape of a control character or a backslash (Python's ValueError).
fn normalize_percents(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = String::with_capacity(path.len());
    let mut last = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && hex_pair(bytes, i + 1) {
            out.push_str(&path[last..i]);
            let character = char::from(hex_value(bytes[i + 1]) * 16 + hex_value(bytes[i + 2]));
            if bad_char(character) {
                return None;
            }
            if UNRESERVED.contains(character) {
                out.push(character);
            } else {
                out.push_str(&path[i..i + 3].to_ascii_uppercase());
            }
            i += 3;
            last = i;
            continue;
        }
        i += 1;
    }
    out.push_str(&path[last..]);
    Some(out)
}

/// An official HTTPS citation, without sessions, tracking or fragments.
///
/// This is a syntax/domain check. The production reader separately checks and pins public DNS addresses before
/// every connection, including redirects. Meaningful query parameters and the order of repeated parameters
/// survive.
pub fn canonical_url(value: &str) -> Option<String> {
    let length = value.chars().count();
    if !(1..=2000).contains(&length)
        || value.chars().any(|c| py_isspace(c) || (c as u32) < 32 || c as u32 == 127)
        || value.contains('\\')
        || bad_percent(value)
    {
        return None;
    }
    let parts = watch::net::urlsplit(value).ok()?;
    if parts.scheme != "https" || parts.username().is_some() || parts.password().is_some() {
        return None;
    }
    if !matches!(parts.port().ok()?, None | Some(443)) {
        return None;
    }
    let lowered = parts.hostname().unwrap_or_default().to_lowercase();
    let host = lowered.strip_suffix('.').unwrap_or(&lowered);
    if host.chars().count() > 253
        || !(host.ends_with(".gov") || host.ends_with(".mil"))
        || !host.split('.').all(host_label)
    {
        return None;
    }
    let raw_path = if parts.path.is_empty() { "/" } else { parts.path.as_str() };
    let path = normalize_percents(&PATH_SESSION.replace_all(raw_path, ""))?;
    let mut query = Vec::new();
    for (key, item) in parse_qsl(&parts.query).ok()? {
        if key.chars().chain(item.chars()).any(bad_char) {
            return None;
        }
        let lowered = key.to_lowercase();
        if lowered.starts_with("utm_")
            || lowered.starts_with("aspsessionid")
            || SESSION_KEYS.contains(&lowered.as_str())
            || TRACKING_KEYS.contains(&lowered.as_str())
        {
            continue;
        }
        query.push((key, item));
    }
    // sorted(..., key=pair[0]) is stable: repeated keys keep their order.
    query.sort_by(|a, b| a.0.cmp(&b.0));
    let query = urlencode(&query);
    // urlunsplit(("https", host, path, query, "")).
    let mut canonical = format!("https://{host}");
    if !path.starts_with('/') {
        canonical.push('/');
    }
    canonical.push_str(&path);
    if !query.is_empty() {
        canonical.push('?');
        canonical.push_str(&query);
    }
    (canonical.chars().count() <= 2000).then_some(canonical)
}

/// watch.http_request's additional guard, applied to initial URLs and redirects.
pub fn official_guard(url: &str) -> String {
    if canonical_url(url).is_some() {
        String::new()
    } else {
        "Only official HTTPS government source links may be read.".to_string()
    }
}

/// [`official_guard`] as `http_request`'s `url_guard`.
pub fn official_url_guard() -> UrlGuard {
    Arc::new(official_guard)
}

/// `_fetch`: the production reader. No provider headers or cookies are passed to a source host.
pub fn fetch(ctx: &Ctx, url: &str, timeout: f64) -> anyhow::Result<(u16, String)> {
    let opts = HttpOptions { timeout, url_guard: Some(official_url_guard()), ..HttpOptions::default() };
    Ok(ctx.http.request(url, &opts)?)
}

/// `_utc_now()`: ISO 8601 UTC with milliseconds and a `Z`.
fn utc_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// `_indexed_at`: observed updated_at values are Unix milliseconds, not seconds/effective dates.
pub fn indexed_at(value: &Value) -> Option<String> {
    let Value::Number(n) = value else { return None };
    let ms = n.as_f64()?;
    if !ms.is_finite() || ms < 0.0 {
        return None;
    }
    // datetime.fromtimestamp(ms / 1000, utc): microseconds rounded half to even, then milliseconds truncated.
    let secs = ms / 1000.0;
    let mut whole = secs.trunc();
    let mut micros = ((secs - whole) * 1e6).round_ties_even();
    if micros >= 1e6 {
        micros -= 1e6;
        whole += 1.0;
    }
    // datetime's last second: 9999-12-31T23:59:59.
    if whole > 253_402_300_799.0 {
        return None;
    }
    let stamp = DateTime::from_timestamp(whole as i64, (micros as u32) * 1000)?;
    if stamp.year() > 9999 {
        return None;
    }
    Some(stamp.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// `_text(value, limit)`: whitespace collapsed, at most `limit` characters, and whether anything was cut.
pub fn text(value: &Value, limit: usize) -> (String, bool) {
    let Value::String(value) = value else { return (String::new(), false) };
    text_of(value, limit)
}

fn text_of(value: &str, limit: usize) -> (String, bool) {
    // Avoid parsing unlimited whitespace supplied by a malformed response.
    let raw_limit = limit * 4;
    let raw: String = value.chars().take(raw_limit).collect();
    let joined = raw.split(py_isspace).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    let joined_len = joined.chars().count();
    let cut: String = joined.chars().take(limit).collect();
    (cut, value.chars().count() > raw_limit || joined_len > limit)
}

/// `_readable_body(body)`.
pub fn readable_body(body: &str) -> bool {
    if body.len() > watch::MAX_BYTES && body.chars().count() > watch::MAX_BYTES {
        return false;
    }
    let head: String = body.chars().take(256).collect();
    let prefix = head.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    // http_request decodes bytes but does not expose MIME. Binary documents are not fresh page text; in
    // particular, main_text is not a PDF text extractor.
    if ["%PDF-", "\u{89}PNG", "\u{fffd}PNG", "GIF87a", "GIF89a", "PK\u{3}\u{4}"].iter().any(|p| prefix.starts_with(p)) {
        return false;
    }
    !prefix.chars().any(|c| (c as u32) < 32 && !matches!(c, '\t' | '\r' | '\n'))
}

/// The default runner: `firecrawl.run_tool(request, timeout=...)` through `ctx`, its answer as Python's dict.
pub fn run_tool(ctx: &Ctx, request: &Value, timeout: f64) -> anyhow::Result<Value> {
    let field = |name: &str| request.get(name).and_then(Value::as_str).unwrap_or_default().to_string();
    let call = ToolCall {
        provider: field("provider"),
        capability: field("capability"),
        options: request.get("options").and_then(Value::as_object).cloned().unwrap_or_default(),
        name: String::new(),
    };
    let ran = firecrawl::run_tool(ctx, &call, timeout)?;
    Ok(json!({"data": ran.data, "credits": ran.credits}))
}

/// Synchronous source lookup; America runs this off its event loop.
///
/// Injected runners/readers implement the same finite-timeout contracts as the production dependencies. They
/// receive only the query or canonical source URL.
#[derive(Clone)]
pub struct GovernmentIndex {
    ctx: Ctx,
    runner: Option<Runner>,
    fetch: Option<Fetch>,
    meter: Option<Meter>,
    clock: Clock,
    timeout_secs: f64,
}

impl Default for GovernmentIndex {
    /// `GovernmentIndex()`: the real process, MAX_QUERY_SECS.
    fn default() -> Self {
        GovernmentIndex {
            ctx: Ctx::global().clone(),
            runner: None,
            fetch: None,
            meter: None,
            clock: eve_web::perf_clock(),
            timeout_secs: MAX_QUERY_SECS,
        }
    }
}

impl GovernmentIndex {
    /// `GovernmentIndex(timeout_secs=...)`, capped at MAX_QUERY_SECS.
    pub fn new(timeout_secs: f64) -> Result<Self, InvalidTimeout> {
        if !timeout_secs.is_finite() || timeout_secs <= 0.0 {
            return Err(InvalidTimeout);
        }
        Ok(GovernmentIndex { timeout_secs: timeout_secs.min(MAX_QUERY_SECS), ..GovernmentIndex::default() })
    }

    /// The process services the default runner, reader and meter use (a test's environment, HTTP and ledger).
    pub fn with_ctx(mut self, ctx: Ctx) -> Self {
        self.ctx = ctx;
        self
    }

    /// `runner=`.
    pub fn with_runner(mut self, runner: Runner) -> Self {
        self.runner = Some(runner);
        self
    }

    /// `fetch=`.
    pub fn with_fetch(mut self, fetch: Fetch) -> Self {
        self.fetch = Some(fetch);
        self
    }

    /// `firecrawl.meter`.
    pub fn with_meter(mut self, meter: Meter) -> Self {
        self.meter = Some(meter);
        self
    }

    /// `time.monotonic`.
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The effective total budget in seconds.
    pub fn timeout_secs(&self) -> f64 {
        self.timeout_secs
    }

    fn run(&self, request: &Value, timeout: f64) -> anyhow::Result<Value> {
        match &self.runner {
            Some(runner) => runner(request, timeout),
            None => run_tool(&self.ctx, request, timeout),
        }
    }

    fn read(&self, url: &str, timeout: f64) -> anyhow::Result<(u16, String)> {
        match &self.fetch {
            Some(fetch) => fetch(url, timeout),
            None => fetch(&self.ctx, url, timeout),
        }
    }

    fn record(&self, credits: Option<&Number>) -> anyhow::Result<()> {
        let price = firecrawl::usd_per_credit(&self.ctx);
        let what = "alexandria firecrawl-gov-index";
        match &self.meter {
            Some(meter) => meter(what, credits, "america", price),
            None => {
                firecrawl::meter(&self.ctx, what, credits, "america", price);
                Ok(())
            }
        }
    }

    pub fn query(&self, question: &str, jurisdiction: &str) -> Result<Evidence, IndexFailure> {
        let blank = |s: &str| s.chars().all(py_isspace);
        if blank(question)
            || question.chars().count() > 4000
            || blank(jurisdiction)
            || jurisdiction.chars().count() > 100
        {
            return Err(IndexFailure::new(
                "A question and U.S. jurisdiction are required for government source research.",
            ));
        }
        let question = question.trim_matches(py_isspace);
        let jurisdiction = jurisdiction.trim_matches(py_isspace);
        let deadline = (self.clock)() + self.timeout_secs;
        let remaining = deadline - (self.clock)();
        // run_tool reserves five seconds inside its upstream timeout field.
        if remaining < 6.0 {
            return Err(IndexFailure::new("Government index lookup has no time left to start."));
        }
        let request = json!({"provider": "firecrawl-gov-index", "capability": "search", "options": {
            "query": format!("Jurisdiction: {jurisdiction}\nQuestion: {question}"),
            "top_k": MAX_RESULTS, "full_content": false,
        }});
        // Provider bodies can contain the question, keys and account details.
        let result = self
            .run(&request, remaining.min(firecrawl::TOOL_TIMEOUT_SECS))
            .map_err(|_| IndexFailure::new("Government index is unavailable. No alternate provider was called."))?;
        let Value::Object(result) = result else {
            return Err(IndexFailure::new("Government index returned an unreadable source result."));
        };
        let credits = match result.get("credits") {
            // _finite_number(credits) and credits >= 0 (a bool is not a JSON number).
            Some(Value::Number(n)) if n.as_f64().is_some_and(|c| c.is_finite() && c >= 0.0) => Some(n),
            _ => None,
        };
        self.record(credits).map_err(|_| IndexFailure::new("Government index usage could not be recorded."))?;
        let rows = match result.get("data") {
            Some(Value::Object(data)) => data.get("data").and_then(Value::as_array),
            _ => None,
        };
        let Some(rows) = rows else {
            return Err(IndexFailure::new("Government index returned an unreadable source result."));
        };
        let mut selected: IndexMap<String, Source> = IndexMap::new();
        for row in rows.iter().take(MAX_RESULTS) {
            let Value::Object(row) = row else { continue };
            let Some(url) = row.get("url").and_then(Value::as_str).and_then(canonical_url) else { continue };
            if selected.contains_key(&url) {
                // Retain the first ranked record's excerpt and matching metadata.
                continue;
            }
            if selected.len() == MAX_SOURCES {
                continue;
            }
            let field = |name: &str| row.get(name).unwrap_or(&Value::Null);
            let (title, _) = text(field("title"), 500);
            let (excerpt, truncated) = text(field("snippet"), MAX_EXCERPT);
            let title = if title.is_empty() {
                watch::net::urlsplit(&url).ok().and_then(|p| p.hostname()).unwrap_or_default()
            } else {
                title
            };
            selected.insert(
                url.clone(),
                Source {
                    title,
                    url,
                    currency: "unverified".into(),
                    excerpt,
                    page_text: String::new(),
                    read_status: "indexed_only".into(),
                    text_truncated: truncated,
                    indexed_at: indexed_at(field("updated_at")),
                    fetched_at: None,
                },
            );
        }
        let mut sources = Vec::new();
        for (_, mut source) in selected {
            let remaining = deadline - (self.clock)();
            if remaining > 0.0 {
                // A failed read is not a fresh answer, and never triggers a retry.
                if let Ok((status, body)) = self.read(&source.url, remaining.min(MAX_READ_SECS)) {
                    if (200..300).contains(&status) && readable_body(&body) {
                        let (page, truncated) = text_of(&watch::main_text(&body), MAX_PAGE_TEXT);
                        if !page.is_empty() {
                            source.page_text = page;
                            source.read_status = "live".into();
                            source.fetched_at = Some(utc_now());
                            source.text_truncated = source.text_truncated || truncated;
                        }
                    }
                }
            }
            if !source.excerpt.is_empty() || !source.page_text.is_empty() {
                sources.push(source);
            }
        }
        if sources.is_empty() {
            return Err(IndexFailure::new("Government index found no usable official sources for this question."));
        }
        Ok(Evidence {
            ok: true,
            status: "sources_found".into(),
            provider: "firecrawl_gov_index".into(),
            question: question.to_string(),
            jurisdiction: jurisdiction.to_string(),
            retrieved_at: utc_now(),
            legal_advice: false,
            sources,
        })
    }
}
