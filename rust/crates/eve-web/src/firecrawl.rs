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

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::watch::FetchError;
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
    let _ = ctx;
    todo!("firecrawl.api_key")
}

/// EVE_FIRECRAWL_USD: one Firecrawl credit's price on the owner's plan ($0 free, about $0.004 Hobby, $0.005 an
/// extra credit, firecrawl.dev/pricing 2026-09-26). Unset, unreadable or outside 0..=1: None, recorded unpriced.
pub fn usd_per_credit(ctx: &Ctx) -> Option<f64> {
    let _ = ctx;
    todo!("firecrawl.usd_per_credit")
}

/// EVE_FIRECRAWL_PER_DAY: Firecrawl calls a local day (default 5; 0 stops them all; at most 10000).
pub fn per_day(ctx: &Ctx) -> i64 {
    let _ = ctx;
    todo!("firecrawl.per_day")
}

/// `_day(now)`: the local day of `now` (epoch seconds; None is now) as YYYY-MM-DD.
pub fn day(now: Option<f64>) -> String {
    let _ = now;
    todo!("firecrawl._day")
}

/// `_count_path()`.
pub fn count_path(count: &DayCount) -> PathBuf {
    let _ = count;
    todo!("firecrawl._count_path")
}

/// `_load(day)`: once a process per day, take the day's count saved by an earlier process (a restart must not hand
/// out a fresh cap). Under the lock. An unreadable file counts as nothing saved.
pub(crate) fn load(count: &DayCount, state: &mut DayState, day: &str) {
    let _ = (count, state, day);
    todo!("firecrawl._load")
}

/// `_save(day)`: under the lock. Best effort: a failed write only loses the count across a restart.
pub(crate) fn save(count: &DayCount, state: &DayState, day: &str) {
    let _ = (count, state, day);
    todo!("firecrawl._save")
}

/// Firecrawl calls left today.
pub fn left(ctx: &Ctx, now: Option<f64>) -> i64 {
    let _ = (ctx, now);
    todo!("firecrawl.left")
}

/// Count one Firecrawl call against today's cap: true when it may be sent, false when the day is spent.
pub fn take(ctx: &Ctx, now: Option<f64>) -> bool {
    let _ = (ctx, now);
    todo!("firecrawl.take")
}

/// One Firecrawl call into the daily spend meter: its credits at `price` a credit, or unpriced. The note is
/// `"{credits} credits"` with Python's int/float text, or `"? credits"`.
pub fn meter(ctx: &Ctx, what: &str, credits: Option<&Number>, feature: &str, price: Option<f64>) {
    let _ = (ctx, what, credits, feature, price);
    todo!("firecrawl.meter")
}

/// `_post`: a Firecrawl POST through watch.http_request, so the watcher's guards hold, once today's cap allows it
/// (every Firecrawl call passes here, so the cap counts them all).
pub fn post(ctx: &Ctx, url: &str, body: &Value, timeout: f64) -> Result<Map<String, Value>, FetchError> {
    let _ = (ctx, url, body, timeout);
    todo!("firecrawl._post")
}

/// `_tool(item)`: an Alexandria tool as the model sees it, or None when it is not one eve can call (no provider
/// or capability, or a required option that is only an id another tool must look up first).
pub fn tool(item: &Value) -> Option<Tool> {
    let _ = item;
    todo!("firecrawl._tool")
}

/// One Firecrawl search that reads its results (defaults: `limit=RESULTS, timeout=SEARCH_TIMEOUT_SECS,
/// tools=False`). `tools`: also ask Alexandria for data tools that fit (their full contracts).
pub fn search(ctx: &Ctx, query: &str, limit: usize, timeout: f64, tools: bool) -> Result<Found, FetchError> {
    let _ = (ctx, query, limit, timeout, tools);
    todo!("firecrawl.search")
}

/// Run one Alexandria tool (default timeout TOOL_TIMEOUT_SECS). FetchError when it fails, including a provider
/// whose terms the owner has not accepted (eve never accepts them).
pub fn run_tool(ctx: &Ctx, call: &ToolCall, timeout: f64) -> Result<Ran, FetchError> {
    let _ = (ctx, call, timeout);
    todo!("firecrawl.run_tool")
}

/// The call the model asked for, checked against what the search offered: a listed tool, only its declared
/// options, every required one present, plain values. None for anything else (the pages then answer). A reply
/// with no JSON object, or no reply at all, is Err: Python lets `_json_object`'s / `_reply_text`'s FetchError out.
pub fn pick_tool(payload: &Value, tools: &[Tool]) -> Result<Option<ToolCall>, FetchError> {
    let _ = (payload, tools);
    todo!("firecrawl.pick_tool")
}
