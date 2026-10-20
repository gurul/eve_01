//! watch.py's `Watcher`: the watch list, its store, its readers, one check start to end, and the loop. The chat
//! tools are in chat.rs (the same type, a second `impl` block).
//!
//! Python's watcher lives on one asyncio loop: the loop's checks and the chat's tool calls interleave only at an
//! `await`, and each sees the other's changes to a watch at once. Here a watch is a [`WatchRef`] (shared, behind a
//! short lock that is never held across an await), so the chat's pause, resume and set_end still land while a
//! check is in flight, and the check reads them back after each await as Python's does. `_lock` (one check at a
//! time) is a tokio mutex; `_wake` a tokio Notify.
//!
//! Python catches any `Exception` a reader or a rule raises (an answer shaped as nobody expected) and makes it the
//! watch's error, never the tick's. A Rust reader returns a [`FetchError`] where Python's would raise one; what
//! stands for the other exceptions is a panic, caught at the same three places (`_run_one`, `add`, `run`).

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use base64::Engine as _;
use futures::future::BoxFuture;
use futures::FutureExt as _;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest as _, Sha256};
use tracing::{error, info, warn};

use super::config::WatchConfig;
use super::limiter::{LimiterOptions, RateLimiter};
use super::model::{model_options, model_reading, PAGE_SYSTEM, SEARCH_SYSTEM, VISION_SYSTEM};
use super::net::{AddressRules, HttpOptions, Resolve};
use super::page::{main_text, phrase_on, price, structured, visible_text, Reading};
use super::render::RENDER_TIMEOUT_SECS;
use super::rules::{self, stretch, Watch};
use super::ticketmaster::{pick_attraction, sports, ticketmaster_reading, tm_events, tribute};
use super::tls::tls_worth;
use super::{
    floor_secs, not_stated, FetchError, MAX_PENDING_CHARS, MAX_WATCHES, OPENROUTER_HOST, PAGE_TEXT_CHARS,
    PENDING_RETRY_SECS, JITTER, QUOTE_HEADERS, QUOTE_URL, TELL_AFTER_ERRORS, TM_API, TM_EVENT_URL, TM_HOST,
};
use crate::{Ask, Clock, Ctx, Http};

/// `notify(text, about=)`: text the owner. `Ok(false)` (telegram's tell_owner returning False) or an Err is a send
/// that did not go. `about` is always given: the line the chat's history keeps instead of `text` (the text itself
/// when the caller had none; Python passes `about` only to a notify that takes it).
pub type Notify = Arc<dyn Fn(String, String) -> BoxFuture<'static, anyhow::Result<bool>> + Send + Sync>;
/// `sleep(seconds)` (asyncio.sleep; a test's no-op).
pub type Sleep = Arc<dyn Fn(f64) -> BoxFuture<'static, ()> + Send + Sync>;
/// `rng.random()`: a float in [0, 1).
pub type Rng = Box<dyn FnMut() -> f64 + Send>;
/// A page read by URL alone, (status, HTML): `tls=` (tls_request) and `crawl=` (fetch_hosted).
pub type PageFn = Arc<dyn Fn(&str) -> Result<(u16, String), FetchError> + Send + Sync>;
/// `render=` (watch::render): (rendered HTML, JPEG of the first screen).
pub type RenderFn = Arc<dyn Fn(&str) -> Result<(String, Vec<u8>), FetchError> + Send + Sync>;
/// `evaluate` (watch::evaluate), the rule a check folds its reading in with. Python's tests monkeypatch the module
/// function to one that raises; this is that seam.
pub type Evaluate = Arc<dyn Fn(&mut Watch, &Reading, bool) -> Option<String> + Send + Sync>;
/// One watch on the list, shared by the loop and the chat (Python compares watches by identity: `Arc::ptr_eq`).
pub type WatchRef = Arc<parking_lot::Mutex<Watch>>;

/// `Watcher(config, ...)`'s keyword arguments. [`WatcherDeps::new`] is Python's defaults: no notify,
/// `fetch=http_request` (ctx.http), `ask=openrouter`, `clock=time.time`, `sleep=asyncio.sleep`, a fresh
/// `random.Random()`, the limiter from the config, `offload=True`, `resolve=_public`, `render=render`, no tls, no
/// crawl.
#[derive(Clone)]
pub struct WatcherDeps {
    pub ctx: Ctx,
    pub notify: Option<Notify>,
    pub fetch: Arc<dyn Http>,
    pub ask: Ask,
    pub clock: Clock,
    pub sleep: Sleep,
    /// None: a fresh random generator.
    pub rng: Option<Arc<parking_lot::Mutex<Rng>>>,
    /// None: one built from the config with `clock`.
    pub limiter: Option<Arc<parking_lot::Mutex<RateLimiter>>>,
    /// False in tests: the fakes run on the loop.
    pub offload: bool,
    /// check_url's public-address test (a fake in tests)
    pub resolve: Resolve,
    /// headless Chromium (a fake in tests)
    pub render: RenderFn,
    /// the Chrome-like read (tls_request; a fake in tests); used only when the config's `tls` is on
    pub tls: Option<PageFn>,
    /// the hosted reader (fetch_hosted; a fake in tests)
    pub crawl: Option<PageFn>,
    /// the rule (watch::evaluate; a test's raising one)
    pub evaluate: Evaluate,
}

impl WatcherDeps {
    pub fn new(ctx: Ctx) -> Self {
        let ask = super::model::openrouter_ask(&ctx);
        WatcherDeps {
            notify: None,
            fetch: ctx.http.clone(),
            ask,
            clock: crate::wall_clock(),
            sleep: Arc::new(|secs: f64| -> BoxFuture<'static, ()> { Box::pin(tokio::time::sleep(seconds(secs))) }),
            rng: None,
            limiter: None,
            offload: true,
            resolve: Arc::new(|host: &str| super::net::public(host, &AddressRules::system())),
            render: Arc::new(|url: &str| super::render::render(url, RENDER_TIMEOUT_SECS, None, None)),
            tls: None,
            crawl: None,
            evaluate: Arc::new(rules::evaluate),
            ctx,
        }
    }
}

/// A duration from seconds that may be negative, NaN or huge (asyncio.sleep's and wait_for's tolerance).
fn seconds(secs: f64) -> Duration {
    if secs.is_finite() && secs > 0.0 {
        Duration::try_from_secs_f64(secs).unwrap_or(Duration::MAX)
    } else if secs == f64::INFINITY {
        Duration::MAX
    } else {
        Duration::ZERO
    }
}

/// The watcher's own mutable state (`watches`, `_resend_at`, `_crawls`, `_last_id`, `load_problem`).
#[derive(Default)]
pub(crate) struct WatcherState {
    pub(crate) watches: Vec<WatchRef>,
    /// pending alerts are not resent before this
    pub(crate) resend_at: f64,
    /// local day -> hosted reads (a day stepped back onto keeps its count)
    pub(crate) crawls: HashMap<String, i64>,
    /// the highest wN ever handed out (saved): an id is never reused
    pub(crate) last_id: i64,
    /// said to the owner at the first tick, when the file was unreadable
    pub(crate) load_problem: String,
}

/// The watch list, its loop and its chat tools. Everything that reaches the network is lent (`fetch`:
/// http_request's shape; `ask`: openrouter's) so the tests need none, and so is the clock.
pub struct Watcher {
    pub config: WatchConfig,
    pub(crate) deps: WatcherDeps,
    pub limiter: Arc<parking_lot::Mutex<RateLimiter>>,
    pub(crate) rng: Arc<parking_lot::Mutex<Rng>>,
    pub(crate) state: parking_lot::Mutex<WatcherState>,
    /// one check at a time: the loop's or a first check's
    pub(crate) lock: tokio::sync::Mutex<()>,
    pub(crate) wake: tokio::sync::Notify,
}

/// `Watcher._missing`: the reading lacks what the condition needs.
pub fn missing(w: &Watch, r: &Reading) -> bool {
    match w.condition.as_str() {
        "appears" => r.hit.is_none(),
        "available" => r.available.is_none(),
        "change" => r.value.is_none() && r.available.is_none(),
        _ => r.value.is_none(),
    }
}

/// `Watcher._merge`: `first`'s facts, with `then` filling what it lacks (`then`'s note and source win).
pub fn merge(first: &Reading, then: &Reading) -> Reading {
    fn or(a: &str, b: &str) -> String {
        if a.is_empty() { b } else { a }.to_string()
    }
    Reading {
        value: first.value.or(then.value),
        currency: or(&first.currency, &then.currency),
        available: first.available.or(then.available),
        hit: first.hit.or(then.hit),
        note: or(&then.note, &first.note),
        url: or(&first.url, &then.url),
        source: or(&then.source, &first.source),
        name: or(&first.name, &then.name),
        blocked: false,
    }
}

/// What a caught panic said, for the log line Python's `log.exception` leaves.
pub(crate) fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "a panic".to_string()
    }
}

static TM_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(TM_EVENT_URL).expect("TM_EVENT_URL"));
static HEX16: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9A-Fa-f]{16}$").expect("hex id"));
static SEPARATORS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[-_/]+").expect("separators"));
static NOT_WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:\d+|tickets?|event)$").expect("not words"));
static IDS_IN_TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""w(\d{1,9})""#).expect("ids"));

impl Watcher {
    /// `watcher.notify = ...`: who the alerts are texted through, set after the watcher is made (the daemon lends
    /// the text door's tell_owner once the door exists).
    pub fn set_notify(&mut self, notify: Option<Notify>) {
        self.deps.notify = notify;
    }

    /// `Watcher(config, ...)`: the list loaded from `config.path`.
    pub fn new(config: WatchConfig, deps: WatcherDeps) -> Self {
        let mut deps = deps;
        if !config.tls {
            deps.tls = None;
        }
        let limiter = deps.limiter.clone().unwrap_or_else(|| {
            Arc::new(parking_lot::Mutex::new(RateLimiter::new(LimiterOptions {
                rate_per_min: config.rate_per_min,
                burst: config.burst,
                host_gap_secs: config.host_gap_secs,
                model_calls_per_day: config.model_calls_per_day,
                clock: deps.clock.clone(),
                ..LimiterOptions::default()
            })))
        });
        let rng = deps.rng.clone().unwrap_or_else(|| {
            let fresh: Rng = Box::new(rand::random::<f64>);
            Arc::new(parking_lot::Mutex::new(fresh))
        });
        let watcher = Watcher {
            config,
            deps,
            limiter,
            rng,
            state: parking_lot::Mutex::new(WatcherState::default()),
            lock: tokio::sync::Mutex::new(()),
            wake: tokio::sync::Notify::new(),
        };
        let watches = watcher.load();
        watcher.state.lock().watches = watches;
        watcher
    }

    /// The watches, in order (the shared handles).
    pub fn watches(&self) -> Vec<WatchRef> {
        self.state.lock().watches.clone()
    }

    /// A copy of every watch, in order.
    pub fn snapshot(&self) -> Vec<Watch> {
        self.watches().iter().map(|w| w.lock().clone()).collect()
    }

    /// `w.watches.append(watch)`: a watch put on the list as it is, unchecked (Python's tests append to the public
    /// list). Returns its shared handle.
    pub fn push(&self, w: Watch) -> WatchRef {
        let w: WatchRef = Arc::new(parking_lot::Mutex::new(w));
        self.state.lock().watches.push(w.clone());
        w
    }

    /// `load_problem`: what the owner is told at the first tick when the file was unreadable ("" when nothing).
    pub fn load_problem(&self) -> String {
        self.state.lock().load_problem.clone()
    }

    /// `self._clock()`.
    pub(crate) fn now(&self) -> f64 {
        (self.deps.clock)()
    }

    /// `self._wake.set()`. Python clears the event after each tick, so a set while no wait is pending is lost:
    /// `notify_waiters` stores no permit, the same.
    pub(crate) fn wake_up(&self) {
        self.wake.notify_waiters();
    }

    /// `_call(fn, ...)`: off the loop when `offload` (asyncio.to_thread), else right here. A panic in the call
    /// goes on unwinding in the caller, as an exception raised in the thread is re-raised by the await.
    pub(crate) async fn call<T, F>(&self, f: F) -> T
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        if !self.deps.offload {
            return f();
        }
        match tokio::task::spawn_blocking(f).await {
            Ok(v) => v,
            Err(e) => match e.try_into_panic() {
                Ok(p) => std::panic::resume_unwind(p),
                Err(e) => std::panic::resume_unwind(Box::new(format!("watch: an offloaded call did not finish ({e})"))),
            },
        }
    }

    /// `self._fetch(url, headers=...)`, off the loop as configured.
    async fn fetch(&self, url: String, opts: HttpOptions) -> Result<(u16, String), FetchError> {
        let fetch = self.deps.fetch.clone();
        self.call(move || fetch.request(&url, &opts)).await
    }

    /// `self._ask(body)`, off the loop as configured.
    async fn ask(&self, body: Value) -> Result<Value, FetchError> {
        let ask = self.deps.ask.clone();
        self.call(move || ask(&body)).await
    }

    /// `self._from_html(w, page)` off the loop (parsing someone else's markup is CPU work).
    async fn html_reading_off(&self, w: &WatchRef, page: String) -> Reading {
        let snap = w.lock().clone();
        self.call(move || Watcher::from_html(&snap, &page)).await
    }

    /// `main_text(page)` off the loop.
    async fn main_text_off(&self, page: String) -> String {
        self.call(move || main_text(&page)).await
    }

    // -- the store --

    /// `_load`: the list in the file. An unreadable file is moved aside (`<name>.bad-<epoch>`), the owner told at
    /// the first tick, and ids go on from the highest one it names; a bad entry is skipped; the first of two same
    /// ids wins; at most MAX_WATCHES.
    pub(crate) fn load(&self) -> Vec<WatchRef> {
        let path = &self.config.path;
        let raw = match std::fs::read(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => Err(os_error_name(&e).to_string()),
            Ok(bytes) => match String::from_utf8(bytes) {
                Err(_) => Err("UnicodeDecodeError".to_string()),
                Ok(text) => pyjson::loads(&text).map_err(|_| "JSONDecodeError".to_string()),
            },
        };
        let raw = match raw {
            Ok(v) => v,
            Err(kind) => {
                // A typo in a hand-edited file must not cost every watch at the next save (re-verification,
                // 2026-09-25): the file is moved aside, the owner is told at the first tick, and ids go on from the
                // highest one it names.
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let mut aside = path.with_file_name(format!("{name}.bad-{}", self.now() as i64));
                let moved = std::fs::read(path).and_then(|bytes| {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    std::fs::rename(path, &aside)?;
                    Ok(text)
                });
                match moved {
                    Ok(text) => {
                        let last = IDS_IN_TEXT
                            .captures_iter(&text)
                            .filter_map(|c| c[1].parse::<i64>().ok())
                            .max()
                            .unwrap_or(0);
                        self.state.lock().last_id = last;
                    }
                    Err(_) => aside = path.clone(),
                }
                let aside_name = aside.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let aside_parent = aside.parent().map(|p| p.display().to_string()).unwrap_or_default();
                warn!(
                    "watch: {} is unreadable ({}); kept as {}, starting with no watches",
                    path.display(),
                    kind,
                    aside_name
                );
                self.state.lock().load_problem = format!(
                    "I couldn't read my watch list, so I'm starting with no watches. The old file is kept as \
                     {aside_name} in {aside_parent}."
                );
                return Vec::new();
            }
        };
        let (items, last) = match &raw {
            Value::Object(m) => (m.get("watches"), m.get("last_id")),
            _ => (None, None),
        };
        let last_id = match last {
            Some(Value::Number(n)) => n.as_i64().filter(|v| (1..1_000_000_000).contains(v)).unwrap_or(0),
            _ => 0,
        };
        self.state.lock().last_id = last_id;
        let mut watches: Vec<WatchRef> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        if let Some(Value::Array(items)) = items {
            for x in items {
                // one entry the loader cannot make sense of is one entry skipped
                let w = std::panic::catch_unwind(AssertUnwindSafe(|| Watch::from_json(x))).ok().flatten();
                if let Some(w) = w {
                    if seen.insert(w.id.clone()) {
                        // the first of two same ids wins
                        watches.push(Arc::new(parking_lot::Mutex::new(w)));
                        if watches.len() >= MAX_WATCHES {
                            break;
                        }
                    }
                }
            }
        }
        watches
    }

    /// Atomic: a temp file (`.watches-*.json`, mode 0600) in the same folder, then a rename. A crash mid-write never
    /// leaves half a list. `{"version": 1, "last_id", "watches"}` with Python's `indent=1` and `ensure_ascii`.
    pub fn save(&self) {
        let (last_id, watches) = {
            let st = self.state.lock();
            (st.last_id, st.watches.clone())
        };
        let list: Vec<Value> = watches.iter().map(|w| w.lock().to_json()).collect();
        let doc = json!({"version": 1, "last_id": last_id, "watches": list});
        let text = pyjson::dumps_indent1(&doc);
        if let Err(e) = write_atomic(&self.config.path, &text) {
            warn!("watch: could not save the watch list ({})", os_error_name(&e));
        }
    }

    // -- where a watch's request goes --

    /// The host a watch's request goes to (the limiter's key).
    pub fn host(&self, w: &Watch) -> String {
        if w.kind == "quote" {
            return "finance.yahoo.com".to_string();
        }
        if w.kind == "search" || w.via == "search" {
            return OPENROUTER_HOST.to_string();
        }
        if w.kind == "ticketmaster" {
            return TM_HOST.to_string();
        }
        url_hostname(&w.target).to_lowercase()
    }

    /// `_next(w, stretch)`: now + every_secs × stretch × (1 ± JITTER).
    pub(crate) fn next(&self, w: &Watch, stretch: f64) -> f64 {
        let r = (self.rng.lock())();
        let wobble = 1.0 + JITTER * (2.0 * r - 1.0);
        self.now() + w.every_secs as f64 * stretch * wobble
    }

    // -- reading one watch --

    /// One reading, or FetchError. The caller has already been let through the limiter for `host(w)`.
    pub async fn read(&self, w: &WatchRef) -> Result<Reading, FetchError> {
        let (kind, via) = {
            let g = w.lock();
            (g.kind.clone(), g.via.clone())
        };
        let mut via = via;
        if kind == "page" && via == "hosted" {
            if self.crawl_on() {
                return self.read_crawled(w).await;
            }
            // TinyFish was turned off (the key removed) after this page needed it: the next rung, not an error
            // forever. Still forward on the ladder (WatchScheduler.lean, `crawlOff`).
            let mut g = w.lock();
            info!("watch: {} needed TinyFish, which is off now; watching it by search", g.id);
            g.via = "search".to_string();
            g.every_secs = g.every_secs.max(floor_secs("search"));
            via = g.via.clone();
        }
        if kind == "quote" {
            return self.read_quote(w).await;
        }
        if kind == "search" || via == "search" {
            return self.read_search(w).await;
        }
        if kind == "ticketmaster" {
            return self.read_ticketmaster(w).await;
        }
        self.read_page(w).await
    }

    /// `_read_quote`.
    pub(crate) async fn read_quote(&self, w: &WatchRef) -> Result<Reading, FetchError> {
        let target = w.lock().target.clone();
        let symbol = quote(&py_strip(&target).to_uppercase(), "-.=^");
        let opts = HttpOptions {
            headers: QUOTE_HEADERS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..HttpOptions::default()
        };
        let (_, body) = self.fetch(QUOTE_URL.replace("{symbol}", &symbol), opts).await?;
        let unreadable = || FetchError::new("the quote service sent an unreadable answer");
        let payload = pyjson::loads(&body).map_err(|_| unreadable())?;
        let chart = payload.as_object().and_then(|m| m.get("chart")).ok_or_else(unreadable)?;
        let chart = chart.as_object().ok_or_else(unreadable)?;
        if chart.get("error").is_some_and(truthy) {
            return Err(FetchError::with_status(format!("no such symbol on Yahoo Finance: {target}"), 404));
        }
        let meta = match chart.get("result") {
            Some(Value::Array(a)) => a.first().and_then(|x| x.as_object()).and_then(|x| x.get("meta")),
            _ => None,
        };
        let meta = meta.and_then(Value::as_object).ok_or_else(unreadable)?;
        let found = price(meta.get("regularMarketPrice").unwrap_or(&Value::Null), "");
        let Some(found) = found else {
            return Err(FetchError::new(format!("no price for {target}")));
        };
        Ok(rules::quote_reading(meta, found, self.now()))
    }

    /// `_model_budget`: OpenRouter's hold, then today's model cap.
    pub(crate) fn model_budget(&self) -> Result<(), FetchError> {
        let mut lim = self.limiter.lock();
        let held = lim.backed_off(OPENROUTER_HOST);
        if held > 0.0 {
            // OpenRouter asked to wait: a page's model call waits it out too, not only the searches gated on it
            return Err(FetchError::with_host(
                format!("the model service asked me to wait {held:.0} s"),
                OPENROUTER_HOST,
            ));
        }
        if !lim.model_ok() {
            return Err(FetchError::new(format!(
                "today's model checks are used up ({}; EVE_WATCH_MODEL_CALLS)",
                lim.model_cap
            )));
        }
        Ok(())
    }

    /// `_read_page`: the cheapest reader that answers, in order: the page's own structured data (free), its text by
    /// a cheap model (one call, skipped while the text is unchanged), then the page as a browser renders it.
    pub(crate) async fn read_page(&self, w: &WatchRef) -> Result<Reading, FetchError> {
        let (via, condition) = {
            let g = w.lock();
            (g.via.clone(), g.condition.clone())
        };
        if via == "browser" {
            let seen = self.read_rendered(w, &Reading::default()).await?;
            if missing(&w.lock(), &seen) {
                return Err(FetchError::new(not_stated(&condition)));
            }
            return Ok(seen);
        }
        let page = self.fetch_page(w).await?;
        // Parsing a page is CPU work on up to MAX_BYTES of someone else's markup: never on the event loop.
        let mut r = self.html_reading_off(w, page.clone()).await;
        if !missing(&w.lock(), &r) {
            return Ok(r);
        }
        if condition != "appears" {
            let text = self.main_text_off(page).await;
            let asked = self.ask_text(w, &text).await?;
            r = merge(&r, &asked);
            if !missing(&w.lock(), &r) {
                return Ok(r);
            }
        }
        if self.config.browser {
            let rendered = self.read_rendered(w, &r).await?;
            let mut g = w.lock();
            if !missing(&g, &rendered) {
                g.via = "browser".to_string(); // this page needs the browser: go straight there from now on
                g.every_secs = g.every_secs.max(floor_secs("browser"));
                info!("watch: {} needs a browser; rendered from now on", g.id);
                return Ok(rendered);
            }
        }
        // Every reader ran and none found what the condition needs: a failed read, counted like any other, so
        // the owner hears "I can't read X" rather than "no reading yet" forever (review, 2026-09-25).
        Err(FetchError::new(not_stated(&condition)))
    }

    /// `_crawl_on`.
    pub(crate) fn crawl_on(&self) -> bool {
        self.config.hosted && self.deps.crawl.is_some()
    }

    /// `_read_crawled`: a page every local reader was refused, as TinyFish fetched it. Capped per day.
    pub(crate) async fn read_crawled(&self, w: &WatchRef) -> Result<Reading, FetchError> {
        let crawl = match (&self.deps.crawl, self.crawl_on()) {
            (Some(c), true) => c.clone(),
            _ => return Err(FetchError::new("TinyFish is not set up on this computer (TINYFISH_API_KEY)")),
        };
        let fetch_host = crate::tinyfish::FETCH_HOST;
        let held = self.limiter.lock().backed_off(fetch_host);
        if held > 0.0 {
            return Err(FetchError::with_host(format!("TinyFish asked me to wait {held:.0} s"), fetch_host));
        }
        let day = self.limiter.lock().day_of(self.now());
        {
            let mut st = self.state.lock();
            if st.crawls.get(&day).copied().unwrap_or(0) >= self.config.hosted_calls_per_day {
                return Err(FetchError::with_host(
                    format!(
                        "today's TinyFish reads are used up ({}; EVE_WATCH_HOSTED_CALLS)",
                        self.config.hosted_calls_per_day
                    ),
                    fetch_host,
                ));
            }
            *st.crawls.entry(day).or_insert(0) += 1;
            let mut days: Vec<String> = st.crawls.keys().cloned().collect();
            days.sort();
            let keep = days.len().saturating_sub(7);
            for old in &days[..keep] {
                st.crawls.remove(old);
            }
        }
        let target = w.lock().target.clone();
        let (_, page) = self.call(move || crawl(&target)).await?;
        self.deps.ctx.ledger.record(
            "tinyfish",
            "fetch",
            eve_spend::WATCH,
            Some(0.0),
            eve_spend::RecordOpts::new().note("free"),
        );
        let mut r = self.html_reading_off(w, page.clone()).await;
        let appears = w.lock().condition == "appears";
        if missing(&w.lock(), &r) && !appears {
            let text = self.main_text_off(page).await;
            let asked = self.ask_text(w, &text).await?;
            r = merge(&r, &asked);
        }
        let condition = w.lock().condition.clone();
        if missing(&w.lock(), &r) {
            return Err(FetchError::new(not_stated(&condition)));
        }
        if r.source != "text" && r.source != "model" {
            r.source = "hosted".to_string();
        }
        Ok(r)
    }

    /// `_fetch_page`: a plain read, else (refused or never answered) the Chrome-like read, which a page that
    /// needed it once (`via == "tls"`) goes straight to.
    pub(crate) async fn fetch_page(&self, w: &WatchRef) -> Result<String, FetchError> {
        let tls = if self.config.tls { self.deps.tls.clone() } else { None };
        let (via, target, id) = {
            let g = w.lock();
            (g.via.clone(), g.target.clone(), g.id.clone())
        };
        let Some(tls) = tls else {
            return self.fetch(target, HttpOptions::default()).await.map(|(_, page)| page);
        };
        if via != "tls" {
            match self.fetch(target.clone(), HttpOptions::default()).await {
                Ok((_, page)) => return Ok(page),
                Err(e) if !tls_worth(&e) => return Err(e),
                Err(e) => info!("watch: {} refused a plain read ({}); reading it as Chrome would", id, e.reason),
            }
        }
        let (_, page) = self.call(move || tls(&target)).await?;
        let mut g = w.lock();
        if g.via != "tls" {
            g.via = "tls".to_string(); // straight to the Chrome-like read from now on
            info!("watch: {} reads as Chrome would; from now on", g.id);
        }
        Ok(page)
    }

    /// `_from_html`.
    pub(crate) fn from_html(w: &Watch, page: &str) -> Reading {
        let mut r = structured(page);
        if w.condition == "appears" {
            r.hit = Some(phrase_on(&visible_text(page), &w.text));
            r.source = "text".to_string();
        }
        r
    }

    /// `_ask_text`: the page's text to the cheap model, once per distinct text (a sha256 digest, 24 hex
    /// characters): an unchanged page reuses the last answer and costs nothing.
    pub(crate) async fn ask_text(&self, w: &WatchRef, text: &str) -> Result<Reading, FetchError> {
        let head: String = text.chars().take(PAGE_TEXT_CHARS).collect();
        let digest = hex::encode(Sha256::digest(head.as_bytes()))[..24].to_string();
        let (label, target) = {
            let g = w.lock();
            if digest == g.digest && !g.cached.is_empty() {
                let c = &g.cached;
                return Ok(Reading {
                    value: c.get("value").and_then(Value::as_f64),
                    currency: c.get("currency").and_then(Value::as_str).unwrap_or("").to_string(),
                    available: c.get("available").and_then(Value::as_bool),
                    note: c.get("note").and_then(Value::as_str).unwrap_or("").to_string(),
                    source: "model".to_string(),
                    ..Reading::default()
                });
            }
            (g.label.clone(), g.target.clone())
        };
        self.model_budget()?;
        let model = self.config.model.clone();
        let mut body = Map::new();
        body.insert("model".into(), json!(model));
        body.extend(model_options(&model));
        body.insert("max_tokens".into(), json!(200));
        body.insert("usage".into(), json!({"include": true}));
        body.insert(
            "messages".into(),
            json!([
                {"role": "system", "content": PAGE_SYSTEM},
                {"role": "user", "content": format!("Watching: {label}\nPage: {target}\n\nPage text:\n{head}")},
            ]),
        );
        self.limiter.lock().model_used();
        let payload = self.ask(Value::Object(body)).await?;
        self.deps.ctx.ledger.record_chat_completion(eve_spend::WATCH, &payload, &model, "openrouter");
        let m = model_reading(&payload, "model")?;
        let mut g = w.lock();
        g.digest = digest;
        let mut cached = Map::new();
        cached.insert("value".into(), m.value.map_or(Value::Null, |v| json!(v)));
        cached.insert("currency".into(), json!(m.currency));
        cached.insert("available".into(), m.available.map_or(Value::Null, Value::Bool));
        cached.insert("note".into(), json!(m.note));
        g.cached = cached;
        Ok(m)
    }

    /// `_read_rendered`: the page in a headless browser: structured data from the rendered DOM, else the
    /// screenshot to the vision model.
    pub(crate) async fn read_rendered(&self, w: &WatchRef, before: &Reading) -> Result<Reading, FetchError> {
        let render = self.deps.render.clone();
        let target = w.lock().target.clone();
        let (html_page, shot) = self.call(move || render(&target)).await?;
        let mut r = merge(&self.html_reading_off(w, html_page).await, before);
        let (condition, label, target) = {
            let g = w.lock();
            (g.condition.clone(), g.label.clone(), g.target.clone())
        };
        if !missing(&w.lock(), &r) || condition == "appears" {
            if r.source != "text" {
                r.source = "browser".to_string();
            }
            return Ok(r);
        }
        self.model_budget()?;
        let image = format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&shot));
        let model = self.config.vision_model.clone();
        let mut body = Map::new();
        body.insert("model".into(), json!(model));
        body.extend(model_options(&model));
        body.insert("max_tokens".into(), json!(200));
        body.insert("usage".into(), json!({"include": true}));
        body.insert(
            "messages".into(),
            json!([
                {"role": "system", "content": VISION_SYSTEM},
                {"role": "user", "content": [
                    {"type": "text", "text": format!("Watching: {label}\nPage: {target}")},
                    {"type": "image_url", "image_url": {"url": image}},
                ]},
            ]),
        );
        self.limiter.lock().model_used();
        let payload = self.ask(Value::Object(body)).await?;
        self.deps.ctx.ledger.record_chat_completion(eve_spend::WATCH, &payload, &model, "openrouter");
        let seen = model_reading(&payload, "vision")?;
        if seen.blocked {
            // A bot check shown to the browser too (Target's "press & hold", 2026-09-25): a 403 in all but name,
            // so the watch moves on to search rather than reading nothing on every check.
            return Err(FetchError::with_status("the site shows a browser a bot check", 403));
        }
        Ok(merge(&r, &seen))
    }

    /// `_read_search`.
    pub(crate) async fn read_search(&self, w: &WatchRef) -> Result<Reading, FetchError> {
        self.model_budget()?;
        let question = {
            let g = w.lock();
            if g.kind == "search" {
                g.target.clone()
            } else {
                format!("{} ({})", g.label, g.target)
            }
        };
        let engine = crate::websearch::openrouter_engine(&self.config.search.engine).unwrap_or("perplexity");
        let today = local_today(self.now());
        let model = self.config.model.clone();
        let mut body = Map::new();
        body.insert("model".into(), json!(model));
        body.extend(model_options(&model));
        body.insert("max_tokens".into(), json!(300));
        body.insert("usage".into(), json!({"include": true}));
        body.insert(
            "tools".into(),
            json!([{"type": "openrouter:web_search",
                    "parameters": {"engine": engine, "max_results": 5, "max_uses": 2}}]),
        );
        let user: String = py_split(&question).join(" ").chars().take(500).collect();
        body.insert(
            "messages".into(),
            json!([
                {"role": "system", "content": format!("{SEARCH_SYSTEM} Today is {today}.")},
                {"role": "user", "content": user},
            ]),
        );
        self.limiter.lock().model_used();
        let payload = self.ask(Value::Object(body)).await?;
        self.deps.ctx.ledger.record_chat_completion(eve_spend::WATCH, &payload, &model, "openrouter");
        model_reading(&payload, "search")
    }

    /// `_tm_get(path, query)`: one Discovery API call (TICKETMASTER_API_KEY from the ctx).
    pub(crate) async fn tm_get(&self, path: &str, query: &[(String, String)]) -> Result<Map<String, Value>, FetchError> {
        let key = py_strip(&self.deps.ctx.get("TICKETMASTER_API_KEY")).to_string();
        if key.is_empty() {
            return Err(FetchError::new("Ticketmaster is not set up on this computer (TICKETMASTER_API_KEY)"));
        }
        let mut pairs: Vec<(String, String)> = vec![("apikey".to_string(), key)];
        for (k, v) in query {
            match pairs.iter_mut().find(|(pk, _)| pk == k) {
                Some(slot) => slot.1 = v.clone(),
                None => pairs.push((k.clone(), v.clone())),
            }
        }
        let url = format!("{TM_API}/{path}?{}", urlencode(&pairs));
        let opts = HttpOptions {
            headers: vec![("Accept".to_string(), "application/json".to_string())],
            ..HttpOptions::default()
        };
        let (_, body) = self.fetch(url, opts).await?;
        match pyjson::loads(&body) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(FetchError::new("Ticketmaster sent an unreadable answer")),
        }
    }

    /// `_read_ticketmaster`: events for the watch (an event link, an attraction resolved once, or a keyword
    /// search with tribute acts dropped), then the pure rule (ticketmaster_reading).
    ///
    /// * a link: the hex id in a ticketmaster.com link is NOT the API's event id (the docs' own sample pairs id
    ///   vvG1VZKS5pr1qy with url .../event/0E0050681F51BA4C), so the link's words are searched and the event whose
    ///   url ends in that id is kept; any other id is fetched as /events/<id>;
    /// * words: resolved once to an attraction (an exact name or alias, never a tribute act; the one with the most
    ///   upcoming events), then that attraction's events, so "Taylor Swift" does not match "Taylor Swift Tribute
    ///   Night"; the attraction id is kept on the watch;
    /// * words with no such attraction: a keyword search, tribute acts dropped.
    pub(crate) async fn read_ticketmaster(&self, w: &WatchRef) -> Result<Reading, FetchError> {
        let (target, label, city) = {
            let g = w.lock();
            (g.target.clone(), g.label.clone(), g.city.clone())
        };
        let link = TM_LINK.captures(&target).map(|c| c[1].to_string());
        let city: Vec<(String, String)> =
            if city.is_empty() { Vec::new() } else { vec![("city".to_string(), city)] };
        let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
        if let Some(id) = link.as_deref().filter(|id| !HEX16.is_match(id)) {
            let event = self.tm_get(&format!("events/{id}.json"), &[]).await?;
            return Ok(ticketmaster_reading(&[event], self.now()));
        }
        if let Some(id) = link.as_deref() {
            let path = url_path(&target);
            let before = path.split("/event/").next().unwrap_or("");
            let spaced = SEPARATORS.replace_all(before, " ");
            let words: Vec<&str> = py_split(py_strip(&spaced)).into_iter().filter(|x| !NOT_WORDS.is_match(x)).collect();
            let words: String = words.join(" ").chars().take(120).collect();
            let keyword = if words.is_empty() { label.clone() } else { words };
            let payload = self
                .tm_get("events.json", &[pair("keyword", &keyword), pair("size", "50"), pair("sort", "date,asc")])
                .await?;
            let want = id.to_uppercase();
            let events: Vec<Map<String, Value>> = tm_events(&payload)
                .into_iter()
                .filter(|e| py_str(e.get("url")).trim_end_matches('/').to_uppercase().ends_with(&want))
                .collect();
            if events.is_empty() {
                return Err(FetchError::new(
                    "Ticketmaster's API does not list that event link; watch the artist instead",
                ));
            }
            return Ok(ticketmaster_reading(&events, self.now()));
        }
        let (wref, ref_at) = {
            let g = w.lock();
            (g.ref_.clone(), g.ref_at)
        };
        if wref.is_empty() || (wref == "-" && self.now() - ref_at > 86400.0) {
            // "-": looked, none matched; search by keyword. Looked up again a day later: a new act reaches
            // Ticketmaster's attraction data some time after its first listing.
            let found = self.tm_get("attractions.json", &[pair("keyword", &target), pair("size", "10")]).await?;
            let picked = pick_attraction(&target, &found);
            let now = self.now();
            {
                let mut g = w.lock();
                g.ref_ = if picked.is_empty() { "-".to_string() } else { picked };
                g.ref_at = now;
            }
            self.limiter.lock().take(TM_HOST); // the lookup's own request, counted
        }
        let wref = w.lock().ref_.clone();
        let events: Vec<Map<String, Value>> = if wref != "-" {
            let mut q = vec![pair("attractionId", &wref), pair("size", "50"), pair("sort", "date,asc")];
            q.extend(city);
            let payload = self.tm_get("events.json", &q).await?;
            tm_events(&payload)
                .into_iter()
                .filter(|e| {
                    let attractions = e
                        .get("_embedded")
                        .and_then(Value::as_object)
                        .and_then(|m| m.get("attractions"))
                        .and_then(Value::as_array);
                    attractions.is_some_and(|list| {
                        list.iter().filter_map(Value::as_object).any(|a| a.get("id") == Some(&Value::String(wref.clone())))
                    })
                })
                .collect()
        } else {
            let mut q = vec![pair("keyword", &target), pair("size", "50"), pair("sort", "date,asc")];
            q.extend(city);
            let payload = self.tm_get("events.json", &q).await?;
            tm_events(&payload)
                .into_iter()
                .filter(|e| !tribute(&py_str(e.get("name")), &target, sports(e)))
                .collect()
        };
        Ok(ticketmaster_reading(&events, self.now()))
    }

    // -- one check, start to end --

    /// Read `w` (the limiter already let it through), fold the reading in, and return the alert, if any. A
    /// failure is counted and backs the watch off; the FetchError is returned as the reason. A refusal (401/403)
    /// of a page moves it down the ladder ("" -> tls -> browser -> hosted -> search) and is no error.
    pub async fn check(&self, w: &WatchRef, shown: bool) -> Result<Option<String>, FetchError> {
        let host = self.host(&w.lock());
        let now = self.now();
        w.lock().last_checked = now;
        let read = async {
            let reading = self.read(w).await?;
            let currency = w.lock().currency.clone();
            if reading.value.is_some()
                && !reading.currency.is_empty()
                && !currency.is_empty()
                && reading.currency != currency
            {
                // Prices in another currency than the watch's: a failed read, counted and told, never compared
                // with a mark set in the other currency, and never dropped in silence (re-verification, 2026-09-25)
                return Err(FetchError::new(format!(
                    "the page now shows prices in {}, not {}",
                    reading.currency, currency
                )));
            }
            Ok(reading)
        };
        let reading = match read.await {
            Ok(r) => r,
            Err(e) => {
                {
                    let mut g = w.lock();
                    if matches!(e.status, 401 | 403) && g.kind == "page" && g.via != "search" && e.host.is_empty() {
                        // A site that refuses a plain read may still show a browser the page; one that refuses the
                        // browser too (most ticket sites) is watched by search from then on. Only the site's own
                        // refusal moves a watch down the ladder: OpenRouter's (e.host) says nothing about the page. A
                        // phrase cannot be watched by search, so an "appears" watch stops at the browser and counts
                        // the refusal.
                        let via = g.via.as_str();
                        let nxt = if self.config.browser && matches!(via, "" | "tls") {
                            "browser"
                        } else if self.crawl_on() && matches!(via, "" | "tls" | "browser") {
                            "hosted"
                        } else {
                            "search"
                        };
                        if !(nxt == "search" && g.condition == "appears") {
                            info!("watch: {} refused a read (HTTP {}); trying it by {}", host, e.status, nxt);
                            g.via = nxt.to_string();
                            g.every_secs = g.every_secs.max(floor_secs(nxt));
                            g.next_at = now; // due now; the limiter decides when it goes
                            return Ok(None);
                        }
                    }
                }
                if matches!(e.status, 429 | 503) || (e.status >= 500 && e.retry_after.is_some_and(|r| r != 0.0)) {
                    let refused = if e.host.is_empty() { host.clone() } else { e.host.clone() };
                    let secs = self.limiter.lock().penalize(&refused, e.retry_after);
                    info!("watch: {} said slow down (HTTP {}); backing off {:.0} s", refused, e.status, secs);
                }
                let mut g = w.lock();
                g.errors += 1;
                g.last_error = e.reason.clone();
                g.next_at = self.next(&g, stretch(g.errors));
                return Err(e);
            }
        };
        self.limiter.lock().clear(&host);
        // The rule runs before the streak is cleared: a rule that raises is this check's error and adds to the
        // streak, where clearing first left the count at 1 forever (re-verification, 2026-09-25).
        let alert = {
            let mut g = w.lock();
            (self.deps.evaluate)(&mut g, &reading, shown)
        };
        let (errors, told, label) = {
            let g = w.lock();
            (g.errors, g.told_error, g.label.clone())
        };
        if errors != 0 && told && self.deps.notify.is_some() && self.kept(w) {
            w.lock().told_error = false;
            self.tell(&format!("I can read {label} again."), "").await;
        }
        let mut g = w.lock();
        g.errors = 0;
        g.last_error.clear();
        g.told_error = false;
        if alert.as_deref().is_some_and(|a| !a.is_empty()) {
            g.last_fired = now;
        }
        g.next_at = self.next(&g, 1.0);
        Ok(alert)
    }

    /// `_tell(text, about)`: text the owner; whether it went.
    ///
    /// `notify` returning false (telegram.TelegramInlet.tell_owner) or failing is a send that did not go. `about`
    /// is the line the chat's history keeps instead of `text`: an alert can carry words from the web (a search's
    /// note, an event's name), and the tool-using brain must never read those as its own (re-verification,
    /// 2026-09-25).
    pub(crate) async fn tell(&self, text: &str, about: &str) -> bool {
        let Some(notify) = self.deps.notify.clone() else {
            warn!("watch: an alert with no chat to send it to");
            return false;
        };
        let about = if about.is_empty() { text } else { about };
        // a failed send never ends the loop
        match AssertUnwindSafe(notify(text.to_string(), about.to_string())).catch_unwind().await {
            Ok(Ok(sent)) => sent,
            Ok(Err(e)) => {
                warn!("watch: could not text an alert ({e})");
                false
            }
            Err(p) => {
                warn!("watch: could not text an alert ({})", panic_text(p.as_ref()));
                false
            }
        }
    }

    /// `_kept(w)`: `w` itself is still on the list (by identity). A watch removed while its check was in flight is
    /// not, and nothing more is texted about it.
    pub fn kept(&self, w: &WatchRef) -> bool {
        self.state.lock().watches.iter().any(|x| Arc::ptr_eq(x, w))
    }

    /// `_run_one(w)`: one background check under the lock; a reader or a rule that fails is this watch's error,
    /// never the tick's; an alert that could not be sent is kept on the watch (`pending`).
    pub async fn run_one(&self, w: &WatchRef) {
        let alert = {
            let _one = self.lock.lock().await;
            if !self.kept(w) || w.lock().paused {
                return;
            }
            let host = self.host(&w.lock());
            let held = self.limiter.lock().backed_off(&host);
            if held > 0.0 {
                // a refusal landed on this host while the check waited for the lock
                w.lock().next_at = self.now() + held + 0.5;
                return;
            }
            let alert = match AssertUnwindSafe(self.check(w, false)).catch_unwind().await {
                Ok(Ok(alert)) => alert,
                Ok(Err(e)) => {
                    self.failed(w, &e.reason).await;
                    None
                }
                Err(p) => {
                    // A reader or a rule tripped on an answer shaped as it did not expect. It is this watch's
                    // error, never the tick's: raised out of tick, next_at never moved, and the same watch was first
                    // in line on every tick while the watches after it were never checked.
                    let reason = {
                        let mut g = w.lock();
                        error!("watch: {}: the check raised ({})", g.id, panic_text(p.as_ref()));
                        g.errors += 1;
                        g.last_error = "an answer I could not make sense of".to_string();
                        g.next_at = self.next(&g, stretch(g.errors));
                        g.last_error.clone()
                    };
                    self.failed(w, &reason).await;
                    None
                }
            };
            self.save();
            alert
        };
        let Some(alert) = alert.filter(|a| !a.is_empty()) else {
            return;
        };
        if !self.kept(w) {
            return;
        }
        let (id, condition, about) = {
            let g = w.lock();
            (g.id.clone(), g.condition.clone(), rules::alert_about(&g))
        };
        info!("watch: {} fired ({})", id, condition);
        if !self.tell(&alert, &about).await {
            // evaluate has disarmed the watch: a lost send would lose this alert for good (Lean verifier,
            // 2026-09-25). It is kept on the watch, after any alert still waiting (never over it), and sent again
            // once Telegram takes messages.
            {
                let mut g = w.lock();
                let joined = if g.pending.is_empty() { alert } else { format!("{}\n\n{}", g.pending, alert) };
                g.pending = last_chars(&joined, MAX_PENDING_CHARS);
            }
            self.save();
        }
    }

    /// `_run_one`'s failure path: logged, and the owner told once the streak reaches TELL_AFTER_ERRORS.
    async fn failed(&self, w: &WatchRef, reason: &str) {
        let (id, errors, told, label) = {
            let g = w.lock();
            (g.id.clone(), g.errors, g.told_error, g.label.clone())
        };
        info!("watch: {} could not be read ({}); error {}", id, reason, errors);
        if errors >= TELL_AFTER_ERRORS && !told && self.kept(w) {
            // told only once it went: a notice Telegram refused is tried again on the next failure
            let went =
                self.tell(&format!("I can't read {label} right now ({reason}). I'll keep trying, less often."), "").await;
            w.lock().told_error = went;
        }
    }

    /// Check every due watch the limiter lets through; push the rest back. Returns seconds until the next watch is
    /// due (the loop sleeps that long, or until woken).
    pub async fn tick(&self) -> f64 {
        let problem = self.load_problem();
        if !problem.is_empty() && self.tell(&problem, "").await {
            self.state.lock().load_problem.clear();
        }
        let resend_at = self.state.lock().resend_at;
        if self.now() >= resend_at {
            let waiting: Vec<WatchRef> = self.watches().into_iter().filter(|w| !w.lock().pending.is_empty()).collect();
            for w in waiting {
                if !self.kept(&w) {
                    continue; // removed while an earlier resend was in flight
                }
                let (pending, about) = {
                    let g = w.lock();
                    (g.pending.clone(), rules::alert_about(&g))
                };
                if self.tell(&pending, &about).await {
                    w.lock().pending.clear();
                    self.save();
                } else {
                    // Telegram is still refusing
                    self.state.lock().resend_at = self.now() + PENDING_RETRY_SECS;
                    break;
                }
            }
        }
        let now = self.now();
        let ended: Vec<WatchRef> = self
            .watches()
            .into_iter()
            .filter(|w| {
                let g = w.lock();
                g.ends_at != 0.0 && now >= g.ends_at
            })
            .collect();
        for w in ended {
            // the time the owner gave it is up: removed, and said once (a lost send is not retried: it is gone)
            let was_there = {
                let mut st = self.state.lock();
                let before = st.watches.len();
                st.watches.retain(|x| !Arc::ptr_eq(x, &w));
                st.watches.len() != before
            };
            if !was_there {
                continue;
            }
            self.save();
            let (id, label) = {
                let g = w.lock();
                (g.id.clone(), g.label.clone())
            };
            self.tell(
                &format!("I've stopped watching {label}: the time you gave it is up."),
                &format!("(I texted that watch {id}, {label}, ended: its time was up.)"),
            )
            .await;
        }
        let resumed: Vec<WatchRef> = self
            .watches()
            .into_iter()
            .filter(|w| {
                let g = w.lock();
                g.paused && g.paused_until != 0.0 && now >= g.paused_until
            })
            .collect();
        for w in resumed {
            {
                // the pause ran out: checked right away
                let mut g = w.lock();
                g.paused = false;
                g.paused_until = 0.0;
                g.next_at = now;
            }
            self.save();
        }
        let mut order: Vec<(f64, WatchRef)> = self.watches().into_iter().map(|w| {
            let t = w.lock().next_at;
            (t, w)
        }).collect();
        order.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut now = now;
        for (_, w) in order {
            let (next_at, paused, host) = {
                let g = w.lock();
                (g.next_at, g.paused, self.host(&g))
            };
            if next_at > now || paused {
                continue;
            }
            let wait = self.limiter.lock().ready_in(&host);
            if wait > 0.0 {
                w.lock().next_at = now + wait + 0.5;
                continue;
            }
            self.limiter.lock().take(&host);
            self.run_one(&w).await;
            now = self.now();
        }
        let mut times: Vec<f64> = Vec::new();
        let watches = self.watches();
        for w in &watches {
            let g = w.lock();
            let t = if g.paused { g.paused_until } else { g.next_at };
            if t != 0.0 {
                times.push(t);
            }
        }
        for w in &watches {
            let g = w.lock();
            if g.ends_at != 0.0 {
                times.push(g.ends_at);
            }
        }
        if times.is_empty() {
            return 300.0;
        }
        let first = times.iter().copied().fold(f64::INFINITY, |a, b| if b < a { b } else { a });
        (first - self.now()).max(1.0)
    }

    /// The loop: tick, then wait for the next due time (at most 300 s) or a wake. Never returns.
    pub async fn run(&self) {
        info!(
            "watch: {} watch(es); {} requests/min, burst {}, {} s per host, {} model checks/day",
            self.state.lock().watches.len(),
            py_g(self.config.rate_per_min),
            self.config.burst,
            py_g(self.config.host_gap_secs),
            self.config.model_calls_per_day
        );
        loop {
            let wait = match AssertUnwindSafe(self.tick()).catch_unwind().await {
                Ok(wait) => wait,
                Err(p) => {
                    // one bad tick never ends the watching
                    error!("watch: a tick failed ({})", panic_text(p.as_ref()));
                    60.0
                }
            };
            let woken = self.wake.notified();
            let _ = tokio::time::timeout(seconds(wait.min(300.0)), woken).await;
        }
    }
}

/// `make_watcher(config)`: the daemon's watcher (with tls_request and fetch_hosted), or None when EVE_WATCH is off.
/// `config` None is `configured(Ctx::global())`.
pub fn make_watcher(config: Option<WatchConfig>) -> Option<Watcher> {
    let cfg = config.unwrap_or_else(|| super::config::configured(Ctx::global()));
    if !cfg.enabled {
        info!("watch: off (EVE_WATCH)");
        return None;
    }
    let mut deps = WatcherDeps::new(Ctx::global().clone());
    deps.tls = Some(Arc::new(|url: &str| super::tls::tls_request(url, super::TIMEOUT_SECS)));
    deps.crawl = Some(Arc::new(|url: &str| {
        super::hosted::fetch_hosted(Ctx::global(), url, super::hosted::HOSTED_TIMEOUT_SECS, None)
    }));
    Some(Watcher::new(cfg, deps))
}

// ---- small Python equivalents ---------------------------------------------------------------------------------

/// Python's truthiness of a JSON value.
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

/// `str(x or "")` of a JSON value (None and every falsy value are "").
pub(crate) fn py_str(v: Option<&Value>) -> String {
    match v {
        None => String::new(),
        Some(v) if !truthy(v) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(_)) => "True".to_string(),
        Some(Value::Number(n)) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (_, Some(u)) => u.to_string(),
            _ => eve_spend::py_float_repr(n.as_f64().unwrap_or(0.0)),
        },
        Some(other) => other.to_string(),
    }
}

/// Python's `str.isspace` for `str.split()` / `strip()`: Unicode whitespace plus 0x1c..0x1f.
pub(crate) fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `s.split()`.
pub(crate) fn py_split(s: &str) -> Vec<&str> {
    s.split(py_space).filter(|x| !x.is_empty()).collect()
}

/// `s.strip()`.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// `s[-n:]`, in characters.
fn last_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

/// `f"{x:g}"` for the run line.
fn py_g(x: f64) -> String {
    if x.is_finite() && x == x.trunc() && x.abs() < 1e16 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// `datetime.fromtimestamp(t).strftime("%A %Y-%m-%d")`.
fn local_today(t: f64) -> String {
    use chrono::TimeZone as _;
    let secs = t.floor();
    if !secs.is_finite() || secs.abs() > 1e14 {
        return String::new();
    }
    chrono::Local
        .timestamp_opt(secs as i64, 0)
        .earliest()
        .map(|d| d.format("%A %Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// `urllib.parse.quote(s, safe=safe)`: UTF-8 bytes, letters, digits, `_.-~` and `safe` kept, the rest %XX.
pub(crate) fn quote(s: &str, safe: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if b.is_ascii_alphanumeric() || "_.-~".contains(c) || (b.is_ascii() && safe.contains(c)) {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `urllib.parse.urlencode(pairs)`: quote_plus on each key and value (a space is "+").
pub(crate) fn urlencode(pairs: &[(String, String)]) -> String {
    let plus = |s: &str| quote(s, " ").replace(' ', "+");
    pairs.iter().map(|(k, v)| format!("{}={}", plus(k), plus(v))).collect::<Vec<_>>().join("&")
}

/// urlsplit's (netloc, path): the netloc follows "//" after a scheme up to "/", "?" or "#"; the path runs to "?"
/// or "#".
fn split_url(url: &str) -> (&str, &str) {
    let rest = match url.find(':') {
        Some(i)
            if i > 0
                && url[..i].starts_with(|c: char| c.is_ascii_alphabetic())
                && url[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) =>
        {
            &url[i + 1..]
        }
        _ => url,
    };
    let (netloc, after) = match rest.strip_prefix("//") {
        Some(r) => {
            let end = r.find(['/', '?', '#']).unwrap_or(r.len());
            (&r[..end], &r[end..])
        }
        None => ("", rest),
    };
    let path_end = after.find(['?', '#']).unwrap_or(after.len());
    (netloc, &after[..path_end])
}

/// `urllib.parse.urlsplit(url).hostname or ""` (not yet lowercased).
pub(crate) fn url_hostname(url: &str) -> String {
    let (netloc, _) = split_url(url);
    let host = netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(netloc);
    let host = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    host.to_lowercase()
}

/// `urllib.parse.urlsplit(url).path`.
fn url_path(url: &str) -> &str {
    split_url(url).1
}

/// The Python exception name an OS error would raise, for the log lines.
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

/// `save`'s write: the folder made, a `.watches-*.json` temp file beside the list, mode 0600, then a rename; the
/// temp file removed when anything fails.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => std::path::PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir)?;
    let mut tmp = tempfile::Builder::new().prefix(".watches-").suffix(".json").tempfile_in(&dir)?;
    tmp.write_all(text.as_bytes())?;
    tmp.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600))?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Python's `json` as the store needs it: `json.loads` (NaN, Infinity, numbers past a float, lone surrogates) and
/// `json.dump(indent=1)` with `ensure_ascii`.
pub(crate) mod pyjson {
    use serde_json::{Map, Number, Value};

    /// `json.loads(text)`. A number serde_json cannot hold (NaN, ±Infinity, past a float) reads as null: every
    /// reader of the store treats such a number and a null alike (Watch.from_json keeps the field's default for
    /// both; last_id is 0 for both). A lone surrogate escape reads as U+FFFD.
    pub fn loads(text: &str) -> Result<Value, String> {
        let mut p = Parser { s: text.as_bytes(), i: 0, text };
        p.ws();
        let v = p.value(0)?;
        p.ws();
        if p.i != p.s.len() {
            return Err(format!("Extra data at {}", p.i));
        }
        Ok(v)
    }

    struct Parser<'a> {
        s: &'a [u8],
        i: usize,
        text: &'a str,
    }

    impl Parser<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
                self.i += 1;
            }
        }

        fn eat(&mut self, lit: &str) -> bool {
            if self.s[self.i..].starts_with(lit.as_bytes()) {
                self.i += lit.len();
                true
            } else {
                false
            }
        }

        fn value(&mut self, depth: usize) -> Result<Value, String> {
            if depth > 10_000 {
                return Err("too deep".into());
            }
            match self.s.get(self.i) {
                None => Err("Expecting value".into()),
                Some(b'{') => {
                    self.i += 1;
                    let mut m = Map::new();
                    self.ws();
                    if self.eat("}") {
                        return Ok(Value::Object(m));
                    }
                    loop {
                        self.ws();
                        if self.s.get(self.i) != Some(&b'"') {
                            return Err("Expecting property name".into());
                        }
                        let k = self.string()?;
                        self.ws();
                        if !self.eat(":") {
                            return Err("Expecting ':'".into());
                        }
                        self.ws();
                        let v = self.value(depth + 1)?;
                        m.insert(k, v);
                        self.ws();
                        if self.eat(",") {
                            continue;
                        }
                        if self.eat("}") {
                            return Ok(Value::Object(m));
                        }
                        return Err("Expecting ',' delimiter".into());
                    }
                }
                Some(b'[') => {
                    self.i += 1;
                    let mut a = Vec::new();
                    self.ws();
                    if self.eat("]") {
                        return Ok(Value::Array(a));
                    }
                    loop {
                        self.ws();
                        a.push(self.value(depth + 1)?);
                        self.ws();
                        if self.eat(",") {
                            continue;
                        }
                        if self.eat("]") {
                            return Ok(Value::Array(a));
                        }
                        return Err("Expecting ',' delimiter".into());
                    }
                }
                Some(b'"') => Ok(Value::String(self.string()?)),
                Some(b'n') if self.eat("null") => Ok(Value::Null),
                Some(b't') if self.eat("true") => Ok(Value::Bool(true)),
                Some(b'f') if self.eat("false") => Ok(Value::Bool(false)),
                Some(b'N') if self.eat("NaN") => Ok(Value::Null),
                Some(b'I') if self.eat("Infinity") => Ok(Value::Null),
                Some(b'-') if self.eat("-Infinity") => Ok(Value::Null),
                Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
                _ => Err("Expecting value".into()),
            }
        }

        fn number(&mut self) -> Result<Value, String> {
            let start = self.i;
            if self.s[self.i] == b'-' {
                self.i += 1;
            }
            match self.s.get(self.i) {
                Some(b'0') => self.i += 1,
                Some(c) if c.is_ascii_digit() => {
                    while self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
                        self.i += 1;
                    }
                }
                _ => return Err("Expecting value".into()),
            }
            let mut float = false;
            if self.s.get(self.i) == Some(&b'.') && self.s.get(self.i + 1).is_some_and(u8::is_ascii_digit) {
                float = true;
                self.i += 1;
                while self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
                    self.i += 1;
                }
            }
            if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
                let mut j = self.i + 1;
                if matches!(self.s.get(j), Some(b'+' | b'-')) {
                    j += 1;
                }
                if self.s.get(j).is_some_and(u8::is_ascii_digit) {
                    float = true;
                    while self.s.get(j).is_some_and(u8::is_ascii_digit) {
                        j += 1;
                    }
                    self.i = j;
                }
            }
            let lit = &self.text[start..self.i];
            if !float {
                if let Ok(i) = lit.parse::<i64>() {
                    return Ok(Value::Number(i.into()));
                }
                if let Ok(u) = lit.parse::<u64>() {
                    return Ok(Value::Number(u.into()));
                }
            }
            let x: f64 = lit.parse().map_err(|_| "bad number".to_string())?;
            Ok(Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null))
        }

        fn hex4(&mut self) -> Result<u32, String> {
            let h = self.text.get(self.i..self.i + 4).ok_or("Invalid \\uXXXX escape")?;
            if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("Invalid \\uXXXX escape".into());
            }
            self.i += 4;
            u32::from_str_radix(h, 16).map_err(|e| e.to_string())
        }

        fn string(&mut self) -> Result<String, String> {
            self.i += 1; // the opening quote
            let mut out = String::new();
            loop {
                let start = self.i;
                while self.i < self.s.len() && !matches!(self.s[self.i], b'"' | b'\\') && self.s[self.i] >= 0x20 {
                    self.i += 1;
                }
                out.push_str(&self.text[start..self.i]);
                match self.s.get(self.i) {
                    None => return Err("Unterminated string".into()),
                    Some(b'"') => {
                        self.i += 1;
                        return Ok(out);
                    }
                    Some(b'\\') => {
                        self.i += 1;
                        let c = *self.s.get(self.i).ok_or("Unterminated string")?;
                        self.i += 1;
                        match c {
                            b'"' => out.push('"'),
                            b'\\' => out.push('\\'),
                            b'/' => out.push('/'),
                            b'b' => out.push('\u{08}'),
                            b'f' => out.push('\u{0c}'),
                            b'n' => out.push('\n'),
                            b'r' => out.push('\r'),
                            b't' => out.push('\t'),
                            b'u' => {
                                let u = self.hex4()?;
                                if (0xd800..0xdc00).contains(&u) && self.text[self.i..].starts_with("\\u") {
                                    let save = self.i;
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if (0xdc00..0xe000).contains(&lo) {
                                        let cp = 0x10000 + ((u - 0xd800) << 10) + (lo - 0xdc00);
                                        out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                                        continue;
                                    }
                                    self.i = save;
                                }
                                out.push(char::from_u32(u).unwrap_or('\u{fffd}'));
                            }
                            _ => return Err("Invalid \\escape".into()),
                        }
                    }
                    Some(_) => return Err("Invalid control character".into()),
                }
            }
        }
    }

    /// `json.dumps(value, indent=1)` (ensure_ascii, `", "`/`": "` separators as indent leaves them, floats by
    /// `repr`).
    pub fn dumps_indent1(v: &Value) -> String {
        let mut out = String::new();
        write(v, 0, &mut out);
        out
    }

    fn write(v: &Value, level: usize, out: &mut String) {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    out.push_str(&i.to_string());
                } else if let Some(u) = n.as_u64() {
                    out.push_str(&u.to_string());
                } else {
                    out.push_str(&eve_spend::py_float_repr(n.as_f64().unwrap_or(0.0)));
                }
            }
            Value::String(s) => string(s, out),
            Value::Array(a) => {
                if a.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    newline(level + 1, out);
                    write(x, level + 1, out);
                }
                newline(level, out);
                out.push(']');
            }
            Value::Object(m) => {
                if m.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push('{');
                for (i, (k, x)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    newline(level + 1, out);
                    string(k, out);
                    out.push_str(": ");
                    write(x, level + 1, out);
                }
                newline(level, out);
                out.push('}');
            }
        }
    }

    fn newline(level: usize, out: &mut String) {
        out.push('\n');
        out.push_str(&" ".repeat(level));
    }

    fn string(s: &str, out: &mut String) {
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

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        #[test]
        fn loads_reads_what_python_reads() {
            assert_eq!(loads(r#"{"a": NaN, "b": Infinity, "c": -Infinity}"#).unwrap(), json!({"a": null, "b": null, "c": null}));
            assert_eq!(loads(&format!("{{\"n\": 1{}}}", "0".repeat(400))).unwrap(), json!({"n": null}));
            assert_eq!(loads(r#"{"s": "\ud83d\ude00 \ud800"}"#).unwrap(), json!({"s": "\u{1f600} \u{fffd}"}));
            assert_eq!(loads("[1, 2.5, -0, 1e3]").unwrap(), json!([1, 2.5, 0, 1000.0]));
            assert!(loads("{not json").is_err());
            assert!(loads("{\"a\": 1,}").is_err());
            assert!(loads("[1] x").is_err());
            assert!(loads("\"a\nb\"").is_err());
        }

        #[test]
        fn dumps_is_indent_one_and_ascii() {
            let v = json!({"version": 1, "w": [{"a": 1.0, "b": "é", "c": {}, "d": []}], "e": 1e16});
            assert_eq!(
                dumps_indent1(&v),
                "{\n \"version\": 1,\n \"w\": [\n  {\n   \"a\": 1.0,\n   \"b\": \"\\u00e9\",\n   \"c\": {},\n   \"d\": []\n  }\n ],\n \"e\": 1e+16\n}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_as_urllib_does() {
        assert_eq!(url_hostname("https://Shop.Example:8443/p?q#f"), "shop.example");
        assert_eq!(url_hostname("http://u:p@[::1]:80/x"), "::1");
        assert_eq!(url_hostname("nonsense"), "");
        assert_eq!(url_path("https://www.ticketmaster.com/big-band/event/0E00?x=1"), "/big-band/event/0E00");
        assert_eq!(quote("BRK.B ^GSPC/é", "-.=^"), "BRK.B%20^GSPC%2F%C3%A9");
        assert_eq!(
            urlencode(&[("keyword".into(), "big band & co".into()), ("sort".into(), "date,asc".into())]),
            "keyword=big+band+%26+co&sort=date%2Casc"
        );
    }

    /// The daemon spawns the loop and calls the tools from other tasks: every future is Send, the watcher Sync.
    #[allow(dead_code)]
    fn futures_are_send(w: &'static Watcher, args: &'static Map<String, Value>, item: &'static WatchRef) {
        fn send<T: Send>(_: T) {}
        fn sync<T: Sync + Send>(_: &T) {}
        sync(w);
        send(w.run());
        send(w.tick());
        send(w.add(args));
        send(w.handle("watch_list", args));
        send(w.run_one(item));
        send(w.check(item, false));
        send(w.read(item));
    }

    #[test]
    fn python_strings() {
        assert_eq!(py_str(Some(&serde_json::json!(5))), "5");
        assert_eq!(py_str(Some(&serde_json::json!(0))), "");
        assert_eq!(py_str(Some(&serde_json::json!(true))), "True");
        assert_eq!(py_str(None), "");
        assert_eq!(py_split(" a \u{1f} b\n"), vec!["a", "b"]);
        assert_eq!(last_chars("abcdé", 2), "dé");
    }
}
