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

use crate::firecrawl::{iter_list, or_str, py_split_join, py_str, py_strip, take_chars};
use crate::watch::{FetchError, HttpOptions};
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
    py_strip(&ctx.get("TINYFISH_API_KEY")).to_string()
}

/// One TinyFish request: its JSON object. FetchError carrying `host` (prefixed "TinyFish: " when the request
/// itself failed).
pub fn call(ctx: &Ctx, url: &str, host: &str, data: Option<Vec<u8>>, timeout: f64) -> Result<Map<String, Value>, FetchError> {
    let key = api_key(ctx);
    if key.is_empty() {
        return Err(FetchError::with_host("TinyFish is not set up on this computer (TINYFISH_API_KEY)", host));
    }
    let mut headers = vec![("X-API-Key".to_string(), key), ("Accept".to_string(), "application/json".to_string())];
    if data.is_some() {
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
    }
    let opts = HttpOptions { data, headers, timeout, ..HttpOptions::default() };
    let (_, text) = ctx.http.request(url, &opts).map_err(|e| FetchError {
        reason: format!("TinyFish: {}", e.reason),
        status: e.status,
        retry_after: e.retry_after,
        host: host.to_string(),
    })?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(FetchError::with_host("TinyFish sent an unreadable answer", host)),
    }
}

/// `urllib.parse.urlencode({"query": q})`'s value: quote_plus (letters, digits and `_.-~` kept, a space as `+`,
/// every other UTF-8 byte as `%XX`).
fn quote_plus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `urllib.parse.urlsplit(url).scheme`: leading C0 controls and spaces stripped, tabs and newlines removed, then
/// the letters before the first ":" when they make a scheme, lowercased; "" otherwise.
pub(crate) fn url_scheme(url: &str) -> String {
    let cleaned: String =
        url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let Some(i) = cleaned.find(':') else { return String::new() };
    let scheme = &cleaned[..i];
    let mut chars = scheme.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return String::new(),
    }
    if chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        scheme.to_ascii_lowercase()
    } else {
        String::new()
    }
}

/// Ranked results, at most `limit` (default 10, timeout 15 s). Only http(s) links are kept.
pub fn search(ctx: &Ctx, query: &str, limit: usize, timeout: f64) -> Result<Vec<SearchResult>, FetchError> {
    let q = take_chars(&py_split_join(query), 500);
    let payload = call(ctx, &format!("{SEARCH_URL}?query={}", quote_plus(&q)), SEARCH_HOST, None, timeout)?;
    let mut out = Vec::new();
    for item in iter_list(payload.get("results")) {
        let Some(item) = item.as_object() else { continue };
        let Some(Value::String(url)) = item.get("url") else { continue };
        if !matches!(url_scheme(url).as_str(), "http" | "https") {
            continue; // a result is only ever a web page to read
        }
        out.push(SearchResult {
            url: url.clone(),
            title: take_chars(&or_str(item.get("title")), 200),
            snippet: or_str(item.get("snippet")),
        });
        // Appended, then counted, as Python does: a limit of 0 still keeps the first result.
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// Read `urls` (at most MAX_URLS) on TinyFish's side (`fmt` "markdown" by default, timeout 30 s). A result has
/// `text` (markdown or cleaned HTML, never raw: scripts are stripped) and `title`; an error has `error`
/// (TinyFish's code) and, for an HTTP error, `status`. FetchError when the whole request fails.
pub fn fetch(ctx: &Ctx, urls: &[String], fmt: &str, ttl: Option<i64>, timeout: f64) -> Result<Fetched, FetchError> {
    let mut body = Map::new();
    body.insert("urls".into(), Value::from(urls.iter().take(MAX_URLS).cloned().collect::<Vec<_>>()));
    body.insert("format".into(), Value::from(fmt));
    body.insert("per_url_timeout_ms".into(), Value::from(((timeout - 5.0).clamp(1.0, 110.0) * 1000.0) as i64));
    if let Some(ttl) = ttl {
        body.insert("ttl".into(), Value::from(ttl));
    }
    let data = eve_jev::py_dumps(&Value::Object(body)).into_bytes();
    let payload = call(ctx, FETCH_URL, FETCH_HOST, Some(data), timeout)?;
    let keyed = |list: Option<&Value>| -> IndexMap<String, Map<String, Value>> {
        let mut out = IndexMap::new();
        for r in iter_list(list) {
            let Value::Object(r) = r else { continue };
            match r.get("url") {
                Some(u) if crate::firecrawl::truthy(u) => {
                    out.insert(py_str(u), r.clone());
                }
                _ => {}
            }
        }
        out
    };
    Ok((keyed(payload.get("results")), keyed(payload.get("errors"))))
}
