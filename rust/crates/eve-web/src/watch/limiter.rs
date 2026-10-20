//! watch.py's rate limiter: one gate for every request the watcher sends.

use std::collections::HashMap;
use std::sync::Arc;

use super::{BACKOFF_BASE_SECS, BACKOFF_MAX_SECS, DEFAULT_BURST, DEFAULT_HOST_GAP_SECS, DEFAULT_MODEL_CALLS_PER_DAY, DEFAULT_RATE_PER_MIN};
use crate::Clock;

/// `day(t)`: the day a timestamp falls on (default: the local date, YYYY-MM-DD).
pub type DayFn = Arc<dyn Fn(f64) -> String + Send + Sync>;

/// `RateLimiter(...)`'s keyword arguments. `Default` is Python's defaults: `clock=time.time`, `day` the local
/// date.
#[derive(Clone)]
pub struct LimiterOptions {
    pub rate_per_min: f64,
    pub burst: i64,
    pub host_gap_secs: f64,
    pub backoff_base: f64,
    pub backoff_max: f64,
    pub model_calls_per_day: i64,
    pub clock: Clock,
    pub day: DayFn,
}

impl Default for LimiterOptions {
    fn default() -> Self {
        LimiterOptions {
            rate_per_min: DEFAULT_RATE_PER_MIN,
            burst: DEFAULT_BURST,
            host_gap_secs: DEFAULT_HOST_GAP_SECS,
            backoff_base: BACKOFF_BASE_SECS,
            backoff_max: BACKOFF_MAX_SECS,
            model_calls_per_day: DEFAULT_MODEL_CALLS_PER_DAY,
            clock: crate::wall_clock(),
            day: Arc::new(local_day),
        }
    }
}

/// `datetime.fromtimestamp(t).strftime("%Y-%m-%d")`.
pub fn local_day(t: f64) -> String {
    let _ = t;
    todo!("watch.RateLimiter: the local day")
}

/// One gate for every request the watcher sends. Pure: the clock is lent, nothing sleeps here.
///
/// `ready_in(host)` says how long until a request to `host` may go (0: now) without spending anything;
/// `take(host)` spends it. The wait is the longest of three: the global bucket's next token, the host's gap
/// since its last request, and the host's backoff. `penalize` starts or doubles a host's backoff (at least
/// the server's Retry-After, never past `backoff_max`); `clear` ends it on the next success.
///
/// Model calls are counted per local day, apart from requests: `model_ok` / `model_used`.
pub struct RateLimiter {
    /// tokens per second
    pub rate: f64,
    pub burst: i64,
    pub host_gap: f64,
    pub backoff_base: f64,
    pub backoff_max: f64,
    pub model_cap: i64,
    pub(crate) clock: Clock,
    pub(crate) day: DayFn,
    pub(crate) tokens: f64,
    pub(crate) refilled: f64,
    /// host -> when its last request went
    pub(crate) last: HashMap<String, f64>,
    /// host -> backed off until
    pub(crate) until: HashMap<String, f64>,
    /// host -> 429/503s in a row
    pub(crate) strikes: HashMap<String, i64>,
    pub(crate) model_day: String,
    pub(crate) model_n: i64,
    /// day -> model calls, for the days left behind
    pub(crate) model_seen: HashMap<String, i64>,
}

impl RateLimiter {
    pub fn new(opts: LimiterOptions) -> Self {
        let _ = opts;
        todo!("watch.RateLimiter.__init__")
    }

    /// `_day(t)`: the lent day function.
    pub fn day_of(&self, t: f64) -> String {
        (self.day)(t)
    }

    /// `_refill`: a clock that steps back must not be refilled twice over the same stretch; the refill mark only
    /// ever moves forward.
    pub(crate) fn refill(&mut self) {
        todo!("watch.RateLimiter._refill")
    }

    pub fn ready_in(&mut self, host: &str) -> f64 {
        let _ = host;
        todo!("watch.RateLimiter.ready_in")
    }

    /// May go below 0 when forced: the debt is repaid first.
    pub fn take(&mut self, host: &str) {
        let _ = host;
        todo!("watch.RateLimiter.take")
    }

    /// The host said slow down (429, 503): back off, and return for how long. A later refusal never shortens a
    /// hold still in force.
    pub fn penalize(&mut self, host: &str, retry_after: Option<f64>) -> f64 {
        let _ = (host, retry_after);
        todo!("watch.RateLimiter.penalize")
    }

    pub fn clear(&mut self, host: &str) {
        let _ = host;
        todo!("watch.RateLimiter.clear")
    }

    pub fn backed_off(&self, host: &str) -> f64 {
        let _ = host;
        todo!("watch.RateLimiter.backed_off")
    }

    /// `_roll_day`: a day left is remembered, so a clock or time zone that steps back onto it finds its calls spent.
    pub(crate) fn roll_day(&mut self) {
        todo!("watch.RateLimiter._roll_day")
    }

    pub fn model_ok(&mut self) -> bool {
        todo!("watch.RateLimiter.model_ok")
    }

    pub fn model_used(&mut self) {
        todo!("watch.RateLimiter.model_used")
    }

    /// `model_calls_today` (a property that rolls the day).
    pub fn model_calls_today(&mut self) -> i64 {
        todo!("watch.RateLimiter.model_calls_today")
    }
}
