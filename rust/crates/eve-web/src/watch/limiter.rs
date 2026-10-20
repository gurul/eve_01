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
    // Python raises for a time it cannot place; a clock that far off has no day, and every such time shares one.
    super::rules::py::fromtimestamp(t).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
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
        let burst = opts.burst.max(1);
        let refilled = (opts.clock)();
        RateLimiter {
            // max(1e-6, rate): a NaN rate is the floor too, as Python's max keeps its first argument
            rate: (if opts.rate_per_min > 1e-6 { opts.rate_per_min } else { 1e-6 }) / 60.0,
            burst,
            host_gap: if opts.host_gap_secs > 0.0 { opts.host_gap_secs } else { 0.0 },
            backoff_base: opts.backoff_base,
            backoff_max: opts.backoff_max,
            model_cap: opts.model_calls_per_day.max(0),
            clock: opts.clock,
            day: opts.day,
            tokens: burst as f64,
            refilled,
            last: HashMap::new(),
            until: HashMap::new(),
            strikes: HashMap::new(),
            model_day: String::new(),
            model_n: 0,
            model_seen: HashMap::new(),
        }
    }

    /// `_day(t)`: the lent day function.
    pub fn day_of(&self, t: f64) -> String {
        (self.day)(t)
    }

    /// `_refill`: a clock that steps back must not be refilled twice over the same stretch; the refill mark only
    /// ever moves forward.
    pub(crate) fn refill(&mut self) {
        let now = (self.clock)();
        self.tokens = py_min(self.burst as f64, self.tokens + py_max(0.0, now - self.refilled) * self.rate);
        // PyrateLimiter's GCRA keeps max(state, now) for the same reason
        self.refilled = py_max(self.refilled, now);
    }

    pub fn ready_in(&mut self, host: &str) -> f64 {
        self.refill();
        let now = (self.clock)();
        let bucket = if self.tokens >= 1.0 { 0.0 } else { (1.0 - self.tokens) / self.rate };
        let gap = py_max(0.0, self.last.get(host).copied().unwrap_or(f64::NEG_INFINITY) + self.host_gap - now);
        let backoff = py_max(0.0, self.until.get(host).copied().unwrap_or(0.0) - now);
        py_max(py_max(bucket, gap), backoff)
    }

    /// May go below 0 when forced: the debt is repaid first.
    pub fn take(&mut self, host: &str) {
        self.refill();
        self.tokens -= 1.0;
        self.last.insert(host.to_string(), (self.clock)());
    }

    /// The host said slow down (429, 503): back off, and return for how long. A later refusal never shortens a
    /// hold still in force.
    pub fn penalize(&mut self, host: &str, retry_after: Option<f64>) -> f64 {
        let strikes = self.strikes.get(host).copied().unwrap_or(0) + 1;
        self.strikes.insert(host.to_string(), strikes);
        // the exponent is capped: 2**1024 overflowed a float
        let mut secs = py_min(self.backoff_max, self.backoff_base * 2.0_f64.powi((strikes - 1).min(30) as i32));
        if let Some(ra) = retry_after {
            if ra > 0.0 {
                secs = py_max(secs, py_min(self.backoff_max, ra));
            }
        }
        // the server's earlier Retry-After stands
        let held = self.until.get(host).copied().unwrap_or(0.0);
        self.until.insert(host.to_string(), py_max(held, (self.clock)() + secs));
        secs
    }

    pub fn clear(&mut self, host: &str) {
        self.strikes.remove(host);
        self.until.remove(host);
    }

    pub fn backed_off(&self, host: &str) -> f64 {
        py_max(0.0, self.until.get(host).copied().unwrap_or(0.0) - (self.clock)())
    }

    /// `_roll_day`: a day left is remembered, so a clock or time zone that steps back onto it finds its calls spent.
    pub(crate) fn roll_day(&mut self) {
        let today = (self.day)((self.clock)());
        if today != self.model_day {
            self.model_seen.insert(std::mem::take(&mut self.model_day), self.model_n);
            self.model_n = self.model_seen.remove(&today).unwrap_or(0);
            self.model_day = today;
        }
    }

    pub fn model_ok(&mut self) -> bool {
        self.roll_day();
        self.model_n < self.model_cap
    }

    pub fn model_used(&mut self) {
        self.roll_day();
        self.model_n += 1;
    }

    /// `model_calls_today` (a property that rolls the day).
    pub fn model_calls_today(&mut self) -> i64 {
        self.roll_day();
        self.model_n
    }

    /// The bucket's tokens now (the tests read `_tokens`).
    pub fn tokens(&self) -> f64 {
        self.tokens
    }
}

/// Python's `max(a, b)`: `a` unless `b > a` (so a NaN second argument loses).
fn py_max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

/// Python's `min(a, b)`: `a` unless `b < a`.
fn py_min(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}
