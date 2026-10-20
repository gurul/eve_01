//! Watching: prices, stocks and ticket releases, checked on a schedule, told over Telegram.
//!
//! The owner texts "tell me when AAPL drops below 300", "watch this page and tell me when it's under $80", or "let
//! me know when tickets for that show in Seattle go on sale", and eve keeps checking and texts them when it
//! happens (owner, 2026-09-25). Four kinds of watch:
//!
//! * `quote`    — a stock, fund, index or crypto pair by its Yahoo Finance symbol (AAPL, VOO, BTC-USD): the
//!                chart endpoint's `regularMarketPrice`. No key, no model call.
//! * `page`     — a URL. The price and whether it can be bought come from the page's own structured data first
//!                (JSON-LD offers, product/og price meta tags, itemprop price), with no model call. Only a page
//!                with none of it goes to a cheap model once, over its visible text, and the model's JSON is
//!                checked. A phrase condition ("appears") is decided by code on the visible text. A page that
//!                refuses automated reads (401/403, as most ticket sites do) falls back to a search check.
//! * `search`   — no URL yet ("tickets for <artist> in <city> on sale"): one OpenRouter call with its web search
//!                server tool, asked for JSON (on sale or not, the lowest price, a link).
//! * `ticketmaster` — an artist, team or show on Ticketmaster's Discovery API, for on-sale alerts. Offered only
//!                when `TICKETMASTER_API_KEY` is set.
//!
//! Everything that leaves the Mac goes through ONE `RateLimiter`: a global token bucket, a minimum gap between
//! two requests to the same host, a per-host backoff after a 429 or 503 that honours Retry-After and doubles on a
//! repeat (capped), and per-kind floors on how often a watch may be checked. Model calls (a page's fallback, a
//! search) have their own daily cap, so a watch list can never run up a bill. Checks run one at a time.
//!
//! Conditions fire on the edge and re-arm (`evaluate`): "below 300" texts once when the price crosses under 300,
//! and again only after it has been back above. Nothing fires on a watch's first reading; `watch_add` returns
//! that reading, so the chat can say "it's already under 300" at once.
//!
//! Watches live in one JSON file (`~/.config/eve-bridge/watches.json`), written atomically, so a restart
//! keeps them and their state. Page text is data: it only ever reaches a model that answers with JSON and has no
//! tools, and what comes back is validated before anything is texted. URLs to private, loopback or link-local
//! addresses are refused, redirects included.
//!
//! Three halves, as elsewhere: a pure core (`RateLimiter`, `structured`, `evaluate`, `Watch`) the tests drive with
//! a fake clock; the blocking network calls (`http_request`, `openrouter`), lent so tests fake them; and
//! `Watcher`, the loop and the chat tools. It ships ON: nothing is fetched until the owner adds a watch.
//! `EVE_WATCH=0` turns it off.
//!
//! watch.py is one 2900-line module; here it is a directory, one file per section:
//!
//! | Python section                                   | Rust                 |
//! |--------------------------------------------------|----------------------|
//! | constants, `FetchError`                          | this file            |
//! | config                                           | [`config`]           |
//! | the rate limiter                                 | [`limiter`]          |
//! | a reading and reading pages                      | [`page`]             |
//! | the network (`_ip_public` … `_exchange`)         | [`net`]              |
//! | the Chrome-like read (curl_cffi)                 | [`tls`]              |
//! | `fetch_hosted`                                   | [`hosted`]           |
//! | overlays, the render and its guard proxy         | [`render`]           |
//! | `openrouter`, asking a model                     | [`model`]            |
//! | a watch and its conditions                       | [`rules`]            |
//! | the chat tools' schemas and instructions         | [`tools`]            |
//! | the watcher: store, reads, checks, loop          | [`watcher`]          |
//! | the watcher: chat tools                          | [`chat`]             |
//! | Ticketmaster's pure rules                        | [`ticketmaster`]     |

pub mod chat;
pub mod config;
pub mod hosted;
pub mod limiter;
pub mod model;
pub mod net;
pub mod page;
pub mod render;
pub mod rules;
pub mod ticketmaster;
pub mod tls;
pub mod tools;
pub mod watcher;

pub use config::{configured, WatchConfig};
pub use hosted::fetch_hosted;
pub use limiter::{LimiterOptions, RateLimiter};
pub use model::{json_object, model_reading, openrouter, plain_link, reply_text};
pub use net::{check_url, http_request, HttpOptions};
pub use page::{main_text, microdata, phrase_on, structured, visible_text, Reading};
pub use render::render;
pub use rules::{describe, evaluate, money, now_line, plain_note, quote_reading, Watch};
pub use ticketmaster::{pick_attraction, ticketmaster_reading};
pub use tls::{tls_available, tls_request, tls_worth};
pub use tools::{INSTRUCTIONS_BLOCK, TICKETMASTER_LINE, TOOLS, TOOL_NAMES};
pub use watcher::{make_watcher, Notify, Watcher, WatcherDeps};

pub const WATCH_DEFAULT: bool = true;
pub const DEFAULT_PATH: &str = "~/.config/eve-bridge/watches.json";
pub const KINDS: [&str; 4] = ["quote", "page", "search", "ticketmaster"];
pub const CONDITIONS: [&str; 7] = ["below", "above", "drop_pct", "rise_pct", "change", "available", "appears"];
pub const PRICE_CONDITIONS: [&str; 4] = ["below", "above", "drop_pct", "rise_pct"];
// How often a watch may be checked, at least, and by default. A quote is one small JSON read; a page is a whole
// page from someone's shop; a search costs money.
// Ticketmaster's Discovery API allows 5000 calls a day at 5 a second (developer.ticketmaster.com, 2026-09-25).
/// `FLOOR_SECS`: "browser" is a page that needs headless Chromium (a render is seconds of CPU); "hosted" a hosted
/// read through TinyFish (free, but someone else's service).
pub const FLOOR_SECS: [(&str, i64); 6] =
    [("quote", 60), ("page", 300), ("search", 3600), ("ticketmaster", 300), ("browser", 900), ("hosted", 3600)];
pub const DEFAULT_EVERY_SECS: [(&str, i64); 4] =
    [("quote", 900), ("page", 1800), ("search", 6 * 3600), ("ticketmaster", 1800)];
pub const MAX_EVERY_SECS: i64 = 7 * 24 * 3600;
/// a watch window or a pause, at most a year
pub const MAX_PERIOD_HOURS: f64 = 24.0 * 366.0;
pub const MAX_WATCHES: usize = 25;
/// ±10% on each next check, so watches on one host never march in step
pub const JITTER: f64 = 0.1;
/// a refused alert is sent again no sooner than this
pub const PENDING_RETRY_SECS: f64 = 120.0;
/// alerts waiting on one watch, at most (the newest kept)
pub const MAX_PENDING_CHARS: usize = 3500;
/// consecutive failures before the owner hears that a watch cannot be read
pub const TELL_AFTER_ERRORS: i64 = 4;
/// a failing watch is checked at most this many times its interval apart
pub const MAX_ERROR_STRETCH: i64 = 8;

// The limiter's defaults (EVE_WATCH_RATE, _BURST, _HOST_GAP, _MODEL_CALLS).
/// requests leaving the Mac, over all hosts
pub const DEFAULT_RATE_PER_MIN: f64 = 12.0;
/// how many may go back to back after a quiet spell
pub const DEFAULT_BURST: i64 = 4;
/// between two requests to one host
pub const DEFAULT_HOST_GAP_SECS: f64 = 20.0;
/// a host's first backoff after a 429/503 with no Retry-After
pub const BACKOFF_BASE_SECS: f64 = 60.0;
pub const BACKOFF_MAX_SECS: f64 = 3600.0;
/// page fallbacks and searches together
pub const DEFAULT_MODEL_CALLS_PER_DAY: i64 = 48;
/// TinyFish Fetch is free (docs.tinyfish.ai, 2026-09-29); a bound, not a budget
pub const DEFAULT_HOSTED_CALLS_PER_DAY: i64 = 100;
/// watch_add waits this long for the limiter before queueing the first check
pub const FIRST_CHECK_WAIT_SECS: f64 = 25.0;

pub const TIMEOUT_SECS: f64 = 20.0;
pub const MAX_BYTES: usize = 3 * 1024 * 1024;
/// of a page's visible text, to the model
pub const PAGE_TEXT_CHARS: usize = 12000;
pub const NOTE_CHARS: usize = 200;
/// past this a "price" is a parsing accident (or an attack), not a price
pub const MAX_PRICE: f64 = 1e12;
/// A browser's user agent: Yahoo answers a bare client with 429 (measured 2026-09-25) and shops refuse one.
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36";
/// `{symbol}` is filled by the reader.
pub const QUOTE_URL: &str = "https://query2.finance.yahoo.com/v8/finance/chart/{symbol}?range=1d&interval=1d";
/// Yahoo answers a full Chrome user agent from a non-Chrome TLS stack with 429 (a fingerprint that does not match
/// its claim) and the bare token with 200 (measured with curl, 2026-09-25). So quotes say only "Mozilla/5.0".
pub const QUOTE_HEADERS: [(&str, &str); 2] = [("User-Agent", "Mozilla/5.0"), ("Accept", "application/json")];
pub const TM_API: &str = "https://app.ticketmaster.com/discovery/v2";
pub const TM_HOST: &str = "app.ticketmaster.com";
pub const TM_EVENT_URL: &str = r"(?i)^https?://(?:www\.)?ticketmaster\.[a-z.]+/(?:.*?/)?event/([A-Za-z0-9]{6,40})(?:[/?#]|$)";
pub const OPENROUTER_URL: &str = crate::websearch::URL;
pub const OPENROUTER_HOST: &str = "openrouter.ai";
/// watch text, search and page screenshots; independent of web synthesis
pub const DEFAULT_MODEL: &str = "deepseek/deepseek-v4.1-flash";

/// schema.org availability, as the last path segment. What can be bought or booked now counts as available.
pub const AVAILABLE: [&str; 7] =
    ["instock", "limitedavailability", "onlineonly", "instoreonly", "preorder", "presale", "backorder"];
pub const UNAVAILABLE: [&str; 3] = ["outofstock", "soldout", "discontinued"];
pub const CURRENCY_SIGNS: [(&str, &str); 7] =
    [("USD", "$"), ("EUR", "€"), ("GBP", "£"), ("INR", "₹"), ("JPY", "¥"), ("CAD", "CA$"), ("AUD", "A$")];

pub const OFF_REASON: &str = "watching is off on this computer (EVE_WATCH)";
/// else: the page does not show a price
pub const NOT_STATED: [(&str, &str); 2] =
    [("available", "the page does not say whether it is available"), ("appears", "the page could not be read")];
/// `NOT_STATED.get(condition, ...)`'s default.
pub const NO_PRICE: &str = "the page does not show a price";

/// `FLOOR_SECS[kind_or_via]` (0 for a name not in it).
pub fn floor_secs(name: &str) -> i64 {
    FLOOR_SECS.iter().find(|(k, _)| *k == name).map(|(_, v)| *v).unwrap_or(0)
}

/// `DEFAULT_EVERY_SECS[kind]` (0 for a kind not in it).
pub fn default_every_secs(kind: &str) -> i64 {
    DEFAULT_EVERY_SECS.iter().find(|(k, _)| *k == kind).map(|(_, v)| *v).unwrap_or(0)
}

/// `NOT_STATED.get(condition, "the page does not show a price")`.
pub fn not_stated(condition: &str) -> &'static str {
    NOT_STATED.iter().find(|(k, _)| *k == condition).map(|(_, v)| *v).unwrap_or(NO_PRICE)
}

/// A request that did not give a usable answer. `status` is the HTTP code (0: none reached us); `host` is who
/// refused, when not the watch's own host (a model call, TinyFish, Firecrawl).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{reason}")]
pub struct FetchError {
    pub reason: String,
    pub status: u16,
    pub retry_after: Option<f64>,
    pub host: String,
}

impl FetchError {
    /// `FetchError(reason)`.
    pub fn new(reason: impl Into<String>) -> Self {
        FetchError { reason: reason.into(), status: 0, retry_after: None, host: String::new() }
    }

    /// `FetchError(reason, status)`.
    pub fn with_status(reason: impl Into<String>, status: u16) -> Self {
        FetchError { status, ..FetchError::new(reason) }
    }

    /// `FetchError(reason, status, retry_after)`.
    pub fn with_retry(reason: impl Into<String>, status: u16, retry_after: Option<f64>) -> Self {
        FetchError { status, retry_after, ..FetchError::new(reason) }
    }

    /// `FetchError(reason, host=host)`.
    pub fn with_host(reason: impl Into<String>, host: impl Into<String>) -> Self {
        FetchError { host: host.into(), ..FetchError::new(reason) }
    }

    /// This error, `host` set (`e.host = OPENROUTER_HOST`).
    pub fn at(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }
}
