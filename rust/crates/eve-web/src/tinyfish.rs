//! TinyFish: eve's web search and hosted page reads, free on every plan (docs.tinyfish.ai, 2026-09-29).
//!
//! Two calls, both with `X-API-Key: $TINYFISH_API_KEY`, both through `watch.http_request` ([`Ctx::http`]) so its
//! guards hold:
//!
//! * Search (`GET api.search.tinyfish.ai?query=`): ranked results as title, snippet and link; no page text. Free
//!   at any wallet balance; 30 requests a minute per key (HTTP 429 past it).
//! * Fetch (`POST api.fetch.tinyfish.ai`): up to 10 pages read on TinyFish's side as markdown or cleaned HTML,
//!   each with its own error code when it fails; `ttl` 0 is a live read, a positive `ttl` accepts a cached copy
//!   that young.
//!
//! Used by the web reader (web_reader: search, then read the top pages), eve's routed web search
//! (search_router sends a one-fact or one-page lookup here, the brain's and the web reader's alike), the watcher's
//! hosted reader for sites that refuse this Mac (watch), and the tutor's practice references (learning/search).
//! Every TinyFish refusal (a bad key, its rate limit, its outage) carries `host`, so the watcher never mistakes it
//! for the watched site refusing; a page's own refusal, which Fetch reports per URL, does not.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::watch::FetchError;
use crate::Ctx;

pub const SEARCH_URL: &str = "https://api.search.tinyfish.ai";
pub const FETCH_URL: &str = "https://api.fetch.tinyfish.ai";
pub const SEARCH_HOST: &str = "api.search.tinyfish.ai";
pub const FETCH_HOST: &str = "api.fetch.tinyfish.ai";
/// Fetch's limit per request.
pub const MAX_URLS: usize = 10;
/// `search`'s keyword defaults: `limit=10, timeout=15.0`.
pub const SEARCH_LIMIT: usize = 10;
pub const SEARCH_TIMEOUT_SECS: f64 = 15.0;
/// `fetch`'s keyword defaults: `fmt="markdown", ttl=None, timeout=30.0`.
pub const FETCH_FORMAT: &str = "markdown";
pub const FETCH_TIMEOUT_SECS: f64 = 30.0;

/// Fetch's per-URL error codes that are the page's own answer rather than TinyFish failing. As an HTTP status, so
/// the watcher's ladder reads them as it read a site's 401/403/404 before.
pub const PAGE_REFUSALS: [(&str, u16); 3] = [("bot_blocked", 403), ("login_required", 401), ("page_not_found", 404)];

/// `PAGE_REFUSALS.get(code)`.
pub fn page_refusal(code: &str) -> Option<u16> {
    PAGE_REFUSALS.iter().find(|(c, _)| *c == code).map(|(_, s)| *s)
}

/// One ranked result: `{"url", "title", "snippet"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub url: String,
    pub title: String,
    pub snippet: String,
}

/// `fetch`'s answer: ({url: result}, {url: error}), keyed by the URL as asked, each entry TinyFish's own object.
pub type Fetched = (IndexMap<String, Map<String, Value>>, IndexMap<String, Map<String, Value>>);

/// `TINYFISH_API_KEY`, stripped; "" when unset.
pub fn api_key(ctx: &Ctx) -> String {
    let _ = ctx;
    todo!("tinyfish.api_key")
}

/// One TinyFish request: its JSON object. FetchError carrying `host` (prefixed "TinyFish: " when the request
/// itself failed).
pub fn call(ctx: &Ctx, url: &str, host: &str, data: Option<Vec<u8>>, timeout: f64) -> Result<Map<String, Value>, FetchError> {
    let _ = (ctx, url, host, data, timeout);
    todo!("tinyfish._call")
}

/// Ranked results, at most `limit` (default 10, timeout 15 s). Only http(s) links are kept.
pub fn search(ctx: &Ctx, query: &str, limit: usize, timeout: f64) -> Result<Vec<SearchResult>, FetchError> {
    let _ = (ctx, query, limit, timeout);
    todo!("tinyfish.search")
}

/// Read `urls` (at most MAX_URLS) on TinyFish's side (`fmt` "markdown" by default, timeout 30 s). A result has
/// `text` (markdown or cleaned HTML, never raw: scripts are stripped) and `title`; an error has `error`
/// (TinyFish's code) and, for an HTTP error, `status`. FetchError when the whole request fails.
pub fn fetch(ctx: &Ctx, urls: &[String], fmt: &str, ttl: Option<i64>, timeout: f64) -> Result<Fetched, FetchError> {
    let _ = (ctx, urls, fmt, ttl, timeout);
    todo!("tinyfish.fetch")
}
