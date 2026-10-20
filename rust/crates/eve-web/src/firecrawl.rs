//! Firecrawl: a paid web search that reads its results, and the Alexandria data tools beside it.
//!
//! eve's web search routes here (search_router) only when a request needs live structured data that a data
//! service holds: a fare, a price from a data service, an official record. Everything else is cheaper elsewhere:
//! TinyFish reads pages for free, Perplexity synthesises many sources. Two calls, both `Authorization: Bearer
//! $FIRECRAWL_API_KEY`, both through watch.http_request ([`Ctx::http`]) so its guards hold, both failing with
//! [`FetchError`]:
//!
//! * Search (`POST /v2/search`) with the request as its query, which also reads the top few result pages as
//!   markdown in the same call (`scrapeOptions`): 1 credit per 10 results plus about 1 per page read
//!   (docs.firecrawl.dev, 2026-09-26). Pages up to an hour old may come from Firecrawl's cache (`maxAge`): fast,
//!   and fresh enough for a lookup. With `tools`, the same search also asks Firecrawl's Alexandria catalogue for
//!   data tools that fit (Google Flights and Skyscanner fares, government records, ...), with their full contracts.
//! * A tool run (`POST /v2/scrape` with `alexandria`; firecrawl-cli 1.24.6): one tool, the options the model
//!   picked. A provider whose terms the owner has not accepted refuses; eve never accepts them (the owner's call).
//!
//! The model sees the tools beside the pages and may pick ONE with its options instead of answering (web_reader.
//! answer_from's `tools`); `pick_tool` checks that pick against what the search offered: a listed tool, only its
//! declared options, every required one present, plain values.
//!
//! Spend: every call is recorded under provider "firecrawl" with its credits. `EVE_FIRECRAWL_USD` is one
//! credit's price on the owner's plan; unset, the line is recorded unpriced rather than guessed.
//!
//! Daily cap (2026-09-29): `EVE_FIRECRAWL_PER_DAY` Firecrawl calls a local day (default 5), counted HERE and
//! nowhere else ([`DayCount`], [`Ctx::firecrawl_calls`]), so the brain's searches and the web reader's tasks share
//! it: every call (a search, a data tool run) takes one in `post` before it is sent, and a call past the cap fails
//! without reaching Firecrawl. search_router also reads `left()` before routing to Firecrawl, and sends a spent
//! day's request to the many-source search with the reason logged.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::LazyLock;

use chrono::{Local, TimeZone};
use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::watch::{FetchError, HttpOptions};
use crate::web_reader::Page;
use crate::Ctx;

pub const SEARCH_URL: &str = "https://api.firecrawl.dev/v2/search";
/// Alexandria tools run through scrape (firecrawl-cli 1.24.6).
pub const TOOL_URL: &str = "https://api.firecrawl.dev/v2/scrape";
pub const HOST: &str = "api.firecrawl.dev";
/// Alexandria tools shown to the model.
pub const MAX_TOOLS: usize = 4;
pub const TOOL_TIMEOUT_SECS: f64 = 45.0;
/// pages read per search: ~4-5 credits (a 4th page costs a credit and 2-3 s uncached)
pub const RESULTS: usize = 3;
/// an hour of Firecrawl's cache is fresh enough for a lookup
pub const MAX_AGE_MS: i64 = 3_600_000;
pub const SEARCH_TIMEOUT_SECS: f64 = 30.0;
/// Firecrawl calls a local day, the brain's and the web reader's together
pub const DEFAULT_PER_DAY: i64 = 5;
/// beside the spend ledger: {"day": "YYYY-MM-DD", "calls": n}; a restart keeps the day's count
pub const COUNT_FILE: &str = "firecrawl-calls.json";

/// `_NEEDS_LOOKUP`: an option NAME that is only an id another tool must look up first.
pub const NEEDS_LOOKUP: &str = r"(?i)(?:^|_)ids?$";

/// One declared option of an Alexandria tool, as the model sees it (`{"name", "type", "about", "required"}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOption {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub about: String,
    pub required: bool,
}

/// An Alexandria tool as the model sees it (`_tool`'s dict: provider, capability, name, about, options).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub provider: String,
    pub capability: String,
    pub name: String,
    pub about: String,
    pub options: Vec<ToolOption>,
}

/// `search`'s answer: `{"pages", "tools", "credits"}`. `credits` keeps Firecrawl's number as sent (an int), None
/// when it sent none.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub pages: Vec<Page>,
    pub tools: Vec<Tool>,
    pub credits: Option<Number>,
}

/// The call `pick_tool` accepted. Python's dict carries `name` beside the call and pops it before `run_tool`;
/// [`ToolCall::alexandria`] is the dict sent (`{"provider", "capability", "options"}`).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub provider: String,
    pub capability: String,
    pub options: Map<String, Value>,
    pub name: String,
}

impl ToolCall {
    /// `{"provider", "capability", "options"}`, in that order: the entry of `"alexandria": [call]`.
    pub fn alexandria(&self) -> Value {
        let mut m = Map::new();
        m.insert("provider".into(), Value::String(self.provider.clone()));
        m.insert("capability".into(), Value::String(self.capability.clone()));
        m.insert("options".into(), Value::Object(self.options.clone()));
        Value::Object(m)
    }
}

/// `run_tool`'s answer: `{"data": its result, "credits": n}`; `credits` an int or a float as Firecrawl sent it.
#[derive(Debug, Clone, PartialEq)]
pub struct Ran {
    pub data: Value,
    pub credits: Option<Number>,
}

/// `_CALLS` (local day -> calls sent; a daemon lives for days), `_LOADED` (days read back from disk in this
/// process) and `_CALLS_LOCK`. `path` None is `spend.spend_dir() / COUNT_FILE`, resolved at each use as Python's
/// `_count_path()` is.
#[derive(Debug, Default)]
pub struct DayCount {
    pub path: Option<PathBuf>,
    pub(crate) state: Mutex<DayState>,
}

#[derive(Debug, Default)]
pub(crate) struct DayState {
    pub(crate) calls: HashMap<String, i64>,
    pub(crate) loaded: HashSet<String>,
}

impl DayCount {
    /// A count kept in `path` (tests).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        DayCount { path: Some(path.into()), state: Mutex::default() }
    }
}

/// `FIRECRAWL_API_KEY`, stripped; "" when unset.
pub fn api_key(ctx: &Ctx) -> String {
    py_strip(&ctx.get("FIRECRAWL_API_KEY")).to_string()
}

/// EVE_FIRECRAWL_USD: one Firecrawl credit's price on the owner's plan ($0 free, about $0.004 Hobby, $0.005 an
/// extra credit, firecrawl.dev/pricing 2026-09-26). Unset, unreadable or outside 0..=1: None, recorded unpriced.
pub fn usd_per_credit(ctx: &Ctx) -> Option<f64> {
    let raw = py_strip(&ctx.get("EVE_FIRECRAWL_USD")).to_string();
    if raw.is_empty() {
        return None;
    }
    let Some(usd) = py_float(&raw) else {
        tracing::warn!(
            "firecrawl: EVE_FIRECRAWL_USD={} is not a number; Firecrawl calls recorded unpriced",
            py_repr_str(&raw)
        );
        return None;
    };
    // NaN fails both comparisons, as in Python.
    if (0.0..=1.0).contains(&usd) {
        Some(usd)
    } else {
        None
    }
}

/// EVE_FIRECRAWL_PER_DAY: Firecrawl calls a local day (default 5; 0 stops them all; at most 10000).
pub fn per_day(ctx: &Ctx) -> i64 {
    let raw = py_strip(&ctx.get("EVE_FIRECRAWL_PER_DAY")).to_string();
    if raw.is_empty() {
        return DEFAULT_PER_DAY;
    }
    match py_float(&raw).and_then(py_int_of_float) {
        Some(n) => n.clamp(0, 10000),
        None => {
            tracing::warn!(
                "firecrawl: EVE_FIRECRAWL_PER_DAY={} is not a number; {} a day",
                py_repr_str(&raw),
                DEFAULT_PER_DAY
            );
            DEFAULT_PER_DAY
        }
    }
}

/// `_day(now)`: the local day of `now` (epoch seconds; None is now) as YYYY-MM-DD.
pub fn day(now: Option<f64>) -> String {
    let local = match now {
        None => Local::now(),
        Some(t) if t.is_finite() => {
            let secs = t.floor();
            let nanos = ((t - secs) * 1e9).round().clamp(0.0, 999_999_999.0) as u32;
            Local.timestamp_opt(secs as i64, nanos).earliest().unwrap_or_else(Local::now)
        }
        Some(_) => Local::now(),
    };
    local.format("%Y-%m-%d").to_string()
}

/// `_count_path()`.
pub fn count_path(count: &DayCount) -> PathBuf {
    match &count.path {
        Some(p) => p.clone(),
        None => eve_spend::spend_dir().join(COUNT_FILE),
    }
}

/// `_load(day)`: once a process per day, take the day's count saved by an earlier process (a restart must not hand
/// out a fresh cap). Under the lock. An unreadable file counts as nothing saved.
pub(crate) fn load(count: &DayCount, state: &mut DayState, day: &str) {
    if state.loaded.contains(day) {
        return;
    }
    state.loaded.insert(day.to_string());
    let Ok(text) = std::fs::read_to_string(count_path(count)) else {
        return;
    };
    let Ok(Value::Object(saved)) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    if saved.get("day").and_then(Value::as_str) != Some(day) {
        return;
    }
    // isinstance(saved["calls"], int): a JSON integer (or a bool, which Python counts as an int).
    let calls = match saved.get("calls") {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64().unwrap_or(i64::MAX),
        Some(Value::Bool(b)) => i64::from(*b),
        _ => return,
    };
    let held = state.calls.get(day).copied().unwrap_or(0);
    state.calls.insert(day.to_string(), held.max(calls));
}

/// `_save(day)`: under the lock. Best effort: a failed write only loses the count across a restart.
pub(crate) fn save(count: &DayCount, state: &DayState, day: &str) {
    let path = count_path(count);
    let written = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        let calls = state.calls.get(day).copied().unwrap_or(0);
        // json.dumps({"day": day, "calls": n}): Python's separators.
        std::fs::write(&tmp, format!("{{\"day\": {}, \"calls\": {}}}", eve_jev::py_dumps(&Value::from(day)), calls))?;
        std::fs::rename(&tmp, &path)
    })();
    if let Err(e) = written {
        tracing::warn!("firecrawl: could not save today's call count ({})", os_error_name(&e));
    }
}

/// Firecrawl calls left today.
pub fn left(ctx: &Ctx, now: Option<f64>) -> i64 {
    let day = day(now);
    let count = &ctx.firecrawl_calls;
    let mut state = count.state.lock();
    load(count, &mut state, &day);
    (per_day(ctx) - state.calls.get(&day).copied().unwrap_or(0)).max(0)
}

/// Count one Firecrawl call against today's cap: true when it may be sent, false when the day is spent.
pub fn take(ctx: &Ctx, now: Option<f64>) -> bool {
    let day = day(now);
    let count = &ctx.firecrawl_calls;
    let mut state = count.state.lock();
    load(count, &mut state, &day);
    let used = state.calls.get(&day).copied().unwrap_or(0);
    if used >= per_day(ctx) {
        return false;
    }
    state.calls.insert(day.clone(), used + 1);
    let mut days: Vec<String> = state.calls.keys().cloned().collect();
    days.sort();
    let drop = days.len().saturating_sub(7);
    for old in &days[..drop] {
        state.calls.remove(old);
    }
    save(count, &state, &day);
    true
}

/// One Firecrawl call into the daily spend meter: its credits at `price` a credit, or unpriced. The note is
/// `"{credits} credits"` with Python's int/float text, or `"? credits"`.
pub fn meter(ctx: &Ctx, what: &str, credits: Option<&Number>, feature: &str, price: Option<f64>) {
    let amount = credits.and_then(Number::as_f64);
    let usd = match (amount, price) {
        (Some(c), Some(p)) if c != 0.0 => Some(c * p),
        _ => None,
    };
    let note = format!("{} credits", credits.map(number_text).unwrap_or_else(|| "?".to_string()));
    ctx.ledger.record("firecrawl", what, feature, usd, eve_spend::RecordOpts::new().note(&note));
}

/// `_post`: a Firecrawl POST through watch.http_request, so the watcher's guards hold, once today's cap allows it
/// (every Firecrawl call passes here, so the cap counts them all).
pub fn post(ctx: &Ctx, url: &str, body: &Value, timeout: f64) -> Result<Map<String, Value>, FetchError> {
    let key = api_key(ctx);
    if key.is_empty() {
        return Err(FetchError::with_host("Firecrawl is not set up on this computer (FIRECRAWL_API_KEY)", HOST));
    }
    if !take(ctx, None) {
        let n = per_day(ctx);
        tracing::info!("firecrawl: today's {} calls are used up (EVE_FIRECRAWL_PER_DAY); not sent", n);
        return Err(FetchError::with_host(
            format!("Firecrawl: today's {n} calls are used up (EVE_FIRECRAWL_PER_DAY)"),
            HOST,
        ));
    }
    let opts = HttpOptions {
        data: Some(eve_jev::py_dumps(body).into_bytes()),
        timeout,
        headers: vec![
            ("Authorization".to_string(), format!("Bearer {key}")),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
        ],
        ..HttpOptions::default()
    };
    let (_, text) = ctx.http.request(url, &opts).map_err(|e| FetchError {
        reason: format!("Firecrawl: {}", e.reason),
        status: e.status,
        retry_after: e.retry_after,
        host: HOST.to_string(),
    })?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Ok(Map::new()),
        Err(_) => Err(FetchError::with_host("Firecrawl sent an unreadable answer", HOST)),
    }
}

static NEEDS_LOOKUP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(NEEDS_LOOKUP).expect("needs-lookup regex"));

/// `_NEEDS_LOOKUP.search(name)`. Python's `$` also matches before one newline that ends the string.
fn needs_lookup(name: &str) -> bool {
    NEEDS_LOOKUP_RE.is_match(name.strip_suffix('\n').unwrap_or(name))
}

/// `_tool(item)`: an Alexandria tool as the model sees it, or None when it is not one eve can call (no provider
/// or capability, or a required option that is only an id another tool must look up first).
pub fn tool(item: &Value) -> Option<Tool> {
    let item = item.as_object()?;
    let provider = item.get("provider").filter(|v| truthy(v))?;
    let capability = item.get("capability").filter(|v| truthy(v))?;
    let mut options = Vec::new();
    for o in iter_list(item.get("options")) {
        let Some(o) = o.as_object() else { continue };
        let Some(name) = o.get("name").filter(|v| truthy(v)) else { continue };
        options.push(ToolOption {
            name: py_str(name),
            kind: or_str(o.get("type")),
            about: take_chars(&or_str(o.get("about")), 300),
            required: o.get("required") == Some(&Value::Bool(true)),
        });
    }
    // A required option that is only an id another tool must look up first (Skyscanner's search_flights wants a
    // numeric destination_entity_id) cannot be filled from the request in one step: the answer model picked it
    // anyway and it failed with invalid_option (live, 2026-09-29). Not offered. Judged by the option's NAME: Google
    // Flights' origin says "IATA code or Google city id" and takes "SFO", so an "id" in the text is no sign.
    if options.iter().any(|o| o.required && needs_lookup(&o.name)) {
        return None;
    }
    Some(Tool {
        provider: py_str(provider),
        capability: py_str(capability),
        name: take_chars(&or_str(item.get("name")), 120),
        about: take_chars(&or_str(item.get("description")), 600),
        options,
    })
}

/// One Firecrawl search that reads its results (defaults: `limit=RESULTS, timeout=SEARCH_TIMEOUT_SECS,
/// tools=False`). `tools`: also ask Alexandria for data tools that fit (their full contracts).
pub fn search(ctx: &Ctx, query: &str, limit: usize, timeout: f64, tools: bool) -> Result<Found, FetchError> {
    let mut sources = vec![serde_json::json!({"type": "web"})];
    if tools {
        sources.push(serde_json::json!({"type": "alexandria"}));
    }
    let mut body = Map::new();
    body.insert("query".into(), Value::String(take_chars(&py_split_join(query), 500)));
    body.insert("limit".into(), Value::from(limit));
    body.insert("sources".into(), Value::Array(sources));
    body.insert("timeout".into(), Value::from(((timeout - 5.0) * 1000.0) as i64));
    body.insert(
        "scrapeOptions".into(),
        serde_json::json!({"formats": ["markdown"], "onlyMainContent": true, "maxAge": MAX_AGE_MS}),
    );
    if tools {
        body.insert("toolDetail".into(), Value::String("full".into()));
    }
    let payload = post(ctx, SEARCH_URL, &Value::Object(body), timeout)?;
    let data = payload.get("data");
    let web = match data {
        Some(Value::Object(d)) => d.get("web"),
        Some(v @ Value::Array(_)) => Some(v),
        _ => None,
    };
    let mut pages = Vec::new();
    for item in iter_list(web) {
        let Some(item) = item.as_object() else { continue };
        let source_url = match item.get("metadata") {
            Some(Value::Object(m)) => m.get("sourceURL"),
            _ => None,
        };
        let url = first_truthy(&[item.get("url"), source_url]);
        let md = first_truthy(&[item.get("markdown"), item.get("description")]);
        if !url.is_empty() && !py_strip(&md).is_empty() {
            pages.push(Page { url, title: take_chars(&or_str(item.get("title")), 200), markdown: md });
        }
    }
    let listed = match data {
        Some(Value::Object(d)) => d.get("tools"),
        _ => None,
    };
    let found: Vec<Tool> = iter_list(listed).iter().filter_map(tool).take(MAX_TOOLS).collect();
    let credits = match payload.get("creditsUsed") {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => Some(n.clone()),
        _ => None,
    };
    Ok(Found { pages, tools: found, credits })
}

/// Run one Alexandria tool (default timeout TOOL_TIMEOUT_SECS). FetchError when it fails, including a provider
/// whose terms the owner has not accepted (eve never accepts them).
pub fn run_tool(ctx: &Ctx, call: &ToolCall, timeout: f64) -> Result<Ran, FetchError> {
    let reply = run_tool_reply(ctx, call, timeout)?;
    let credits = match reply.get("credits") {
        Some(Value::Number(n)) => Some(n.clone()),
        Some(Value::Bool(b)) => Some(Number::from(i64::from(*b))),
        _ => None,
    };
    Ok(Ran { data: reply.get("data").cloned().unwrap_or(Value::Null), credits })
}

/// `run_tool`'s answer exactly as Python returns it: `{"data": its result, "credits": n}`, where `credits` is the
/// `creditsCost` Firecrawl sent when it is a JSON number or bool (Python's `isinstance(credits, (int, float))`
/// keeps a bool), else null. A caller that checks the type of `credits` itself, as america_index does, reads this.
pub fn run_tool_reply(ctx: &Ctx, call: &ToolCall, timeout: f64) -> Result<Value, FetchError> {
    let mut body = Map::new();
    body.insert("alexandria".into(), Value::Array(vec![call.alexandria()]));
    body.insert("timeout".into(), Value::from(((timeout - 5.0) * 1000.0) as i64));
    let payload = post(ctx, TOOL_URL, &Value::Object(body), timeout)?;
    let items = match payload.get("data") {
        Some(Value::Object(d)) => iter_list(d.get("alexandria")),
        _ => Vec::new(),
    };
    let empty = Map::new();
    let item = items.first().and_then(|v| v.as_object()).unwrap_or(&empty);
    let success = payload.get("success").map(truthy).unwrap_or(true);
    let failed_item = item.get("error").map(truthy).unwrap_or(false);
    if !success || failed_item || !item.contains_key("data") {
        let why = [payload.get("code"), item.get("error"), payload.get("error")]
            .into_iter()
            .flatten()
            .find(|v| truthy(v))
            .map(py_str)
            .unwrap_or_else(|| "no data".to_string());
        return Err(FetchError::with_host(
            format!("Alexandria {}/{}: {}", call.provider, call.capability, take_chars(&why, 120)),
            HOST,
        ));
    }
    let credits = match item.get("creditsCost") {
        Some(v @ (Value::Number(_) | Value::Bool(_))) => v.clone(),
        _ => Value::Null,
    };
    let mut reply = Map::new();
    reply.insert("data".into(), item["data"].clone());
    reply.insert("credits".into(), credits);
    Ok(Value::Object(reply))
}

/// The call the model asked for, checked against what the search offered: a listed tool, only its declared
/// options, every required one present, plain values. None for anything else (the pages then answer). A reply
/// with no JSON object, or no reply at all, is Err: Python lets `_json_object`'s / `_reply_text`'s FetchError out.
pub fn pick_tool(payload: &Value, tools: &[Tool]) -> Result<Option<ToolCall>, FetchError> {
    let reply = crate::watch::json_object(&crate::watch::reply_text(payload)?)?;
    let Some(Value::Object(choice)) = reply.get("tool") else {
        return Ok(None);
    };
    let n: Option<u128> = match choice.get("id") {
        Some(Value::Number(n)) => n.as_u64().map(u128::from).or_else(|| n.as_i64().map(|_| 0)),
        // "2".strip().isdigit(): int() of it (ASCII digits; one past u128 is out of range anyway).
        Some(Value::String(s)) => {
            let t = py_strip(s);
            if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
                Some(t.parse::<u128>().unwrap_or(u128::MAX))
            } else {
                None
            }
        }
        _ => None,
    };
    let Some(n) = n else { return Ok(None) };
    if n < 1 || n > tools.len() as u128 {
        return Ok(None);
    }
    let tool = &tools[(n - 1) as usize];
    let declared: HashSet<&str> = tool.options.iter().map(|o| o.name.as_str()).collect();
    fn scalar(v: &Value) -> bool {
        matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_))
    }
    fn plain(v: &Value) -> bool {
        scalar(v) || matches!(v, Value::Array(items) if items.iter().all(scalar))
    }
    let mut options = Map::new();
    if let Some(Value::Object(raw)) = choice.get("options") {
        for (k, v) in raw {
            if declared.contains(k.as_str()) && plain(v) {
                options.insert(k.clone(), v.clone());
            }
        }
    }
    if tool.options.iter().any(|o| o.required && !options.contains_key(&o.name)) {
        return Ok(None);
    }
    Ok(Some(ToolCall {
        provider: tool.provider.clone(),
        capability: tool.capability.clone(),
        options,
        name: tool.name.clone(),
    }))
}

// ---- Python's built-ins, as these three modules use them ----------------------------------------------------

/// `str.isspace()` for one character: Unicode White_Space and the four information separators.
pub(crate) fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `s.strip()`.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// `" ".join(s.split())`.
pub(crate) fn py_split_join(s: &str) -> String {
    s.split(py_isspace).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `s[:n]`, in characters.
pub(crate) fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python's truth value of a JSON value.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str(v or "")`.
pub(crate) fn or_str(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(v) => py_str(v),
        _ => String::new(),
    }
}

/// `str(a or b or "")`.
pub(crate) fn first_truthy(vs: &[Option<&Value>]) -> String {
    vs.iter().flatten().find(|v| truthy(v)).map(|v| py_str(v)).unwrap_or_default()
}

/// `for x in (v or [])`: a list's items. Iterating any other truthy value gives keys or characters, never an
/// object, which every caller skips, so they are left out here.
pub(crate) fn iter_list(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    }
}

/// `str(n)` of a JSON number: an int's digits, a float's repr.
pub(crate) fn number_text(n: &Number) -> String {
    if n.is_i64() || n.is_u64() {
        n.to_string()
    } else {
        eve_spend::py_float_repr(n.as_f64().unwrap_or(0.0))
    }
}

/// `str(v)` of a value `json.loads` gave.
pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `repr(v)` of a value `json.loads` gave.
pub(crate) fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => number_text(n),
        Value::String(s) => py_repr_str(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter().map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `repr(s)` of a str: single quotes unless only double quotes avoid escaping one.
pub(crate) fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `float(s)` of an already stripped string: Python's literal (digits with single underscores between them, a
/// point, an exponent, inf, infinity, nan, a sign); None where Python raises ValueError.
pub(crate) fn py_float(s: &str) -> Option<f64> {
    let s = py_strip(s);
    if s.is_empty() || !s.is_ascii() {
        return None;
    }
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'_' {
            let before = i > 0 && bytes[i - 1].is_ascii_digit();
            let after = bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
            if !before || !after {
                return None;
            }
        }
    }
    let cleaned: String = s.chars().filter(|c| *c != '_').collect();
    let body = cleaned.trim_start_matches(['+', '-']);
    if body.len() + 1 < cleaned.len() {
        return None; // two signs
    }
    let lower = body.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" || lower == "nan" {
        return cleaned.parse::<f64>().ok();
    }
    // Rust accepts nothing Python refuses among digits, points and exponents; Python accepts what Rust does.
    if !body.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// `int(x)` of a float: None for NaN (Python's ValueError); infinity saturates (Python raises OverflowError: see
/// PARITY).
pub(crate) fn py_int_of_float(x: f64) -> Option<i64> {
    if x.is_nan() {
        None
    } else {
        Some(x.trunc() as i64)
    }
}

/// The Python exception name an OS error raises, for a log line.
pub(crate) fn os_error_name(e: &std::io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EEXIST) => "FileExistsError",
        Some(libc::EACCES) | Some(libc::EPERM) => "PermissionError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        _ => "OSError",
    }
}
