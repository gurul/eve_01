//! watch.py's config section: `WatchConfig` and `configured`.

use std::path::PathBuf;

use super::{
    DEFAULT_BURST, DEFAULT_HOSTED_CALLS_PER_DAY, DEFAULT_HOST_GAP_SECS, DEFAULT_MODEL, DEFAULT_MODEL_CALLS_PER_DAY,
    DEFAULT_PATH, DEFAULT_RATE_PER_MIN, WATCH_DEFAULT,
};
use crate::websearch::SearchConfig;
use crate::Ctx;

/// The watcher's settings. `Default` is Python's bare `WatchConfig()` (path `DEFAULT_PATH` expanded).
#[derive(Debug, Clone, PartialEq)]
pub struct WatchConfig {
    pub enabled: bool,
    pub path: PathBuf,
    pub rate_per_min: f64,
    pub burst: i64,
    pub host_gap_secs: f64,
    pub model_calls_per_day: i64,
    pub model: String,
    /// TICKETMASTER_API_KEY is set (read again at each call)
    pub ticketmaster: bool,
    /// a headless Chromium is there: render JS pages, read screenshots
    pub browser: bool,
    /// the Chrome-like reader is there: retry a refused page as Chrome would
    pub tls: bool,
    /// TINYFISH_API_KEY is set (read again at each call)
    pub hosted: bool,
    pub hosted_calls_per_day: i64,
    /// reads a rendered page's screenshot (image input)
    pub vision_model: String,
    pub search: SearchConfig,
}

impl Default for WatchConfig {
    fn default() -> Self {
        WatchConfig {
            enabled: WATCH_DEFAULT,
            path: expand_user(DEFAULT_PATH),
            rate_per_min: DEFAULT_RATE_PER_MIN,
            burst: DEFAULT_BURST,
            host_gap_secs: DEFAULT_HOST_GAP_SECS,
            model_calls_per_day: DEFAULT_MODEL_CALLS_PER_DAY,
            model: DEFAULT_MODEL.to_string(),
            ticketmaster: false,
            browser: false,
            tls: false,
            hosted: false,
            hosted_calls_per_day: DEFAULT_HOSTED_CALLS_PER_DAY,
            vision_model: DEFAULT_MODEL.to_string(),
            search: SearchConfig::default(),
        }
    }
}

/// `Path(p).expanduser()`: a leading `~` is the home folder.
pub fn expand_user(path: &str) -> PathBuf {
    let _ = path;
    todo!("watch: Path.expanduser")
}

/// `_number(env, name, default, lo, hi)`: a float setting, clamped; unset is the default, unreadable is the default
/// with a warning.
pub fn number(ctx: &Ctx, name: &str, default: f64, lo: f64, hi: f64) -> f64 {
    let _ = (ctx, name, default, lo, hi);
    todo!("watch._number")
}

/// `_browser_on`: EVE_WATCH_BROWSER = 1 | 0 | auto (the default: on when a headless Chromium is installed;
/// Python's "Playwright is importable", see PARITY).
pub fn browser_on(ctx: &Ctx) -> bool {
    let _ = ctx;
    todo!("watch._browser_on")
}

/// `_tls_on`: EVE_WATCH_TLS = 1 | 0 | auto (the default: on when the Chrome-like reader is available).
pub fn tls_on(ctx: &Ctx) -> bool {
    let _ = ctx;
    todo!("watch._tls_on")
}

/// `EVE_WATCH` (on unless 0/false/off), `EVE_WATCH_FILE`, the limiter's `_RATE` (per minute, 1-600), `_BURST`
/// (1-50), `_HOST_GAP` (seconds, 0-3600) and `_MODEL_CALLS` (per day, 0-10000), `_MODEL` for the page fallback
/// and searches, `_VISION_MODEL` for screenshots, `_BROWSER` for headless Chromium, `_TLS` for the Chrome-like
/// retry, and `_HOSTED` (with `TINYFISH_API_KEY`) and `_HOSTED_CALLS` (per day) for the hosted reader.
pub fn configured(ctx: &Ctx) -> WatchConfig {
    let _ = ctx;
    todo!("watch.configured")
}
