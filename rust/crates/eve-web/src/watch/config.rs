//! watch.py's config section: `WatchConfig` and `configured`.

use std::path::PathBuf;

use super::{
    DEFAULT_BURST, DEFAULT_HOSTED_CALLS_PER_DAY, DEFAULT_HOST_GAP_SECS, DEFAULT_MODEL, DEFAULT_MODEL_CALLS_PER_DAY,
    DEFAULT_PATH, DEFAULT_RATE_PER_MIN, WATCH_DEFAULT,
};
use super::rules::py;
use super::tls::tls_available;
use crate::websearch::{self, SearchConfig};
use crate::{tinyfish, Ctx};

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

/// `Path(p).expanduser()`: a leading `~` is the home folder (`$HOME`, else the password database), `~name` that
/// user's; a user the system does not know leaves the path as it is.
pub fn expand_user(path: &str) -> PathBuf {
    if !path.starts_with('~') {
        return PathBuf::from(path);
    }
    let i = path[1..].find('/').map(|j| j + 1).unwrap_or(path.len());
    let home = if i == 1 {
        match std::env::var_os("HOME") {
            Some(h) => Some(h.to_string_lossy().into_owned()),
            None => passwd_home(None),
        }
    } else {
        passwd_home(Some(&path[1..i]))
    };
    let Some(home) = home else {
        return PathBuf::from(path);
    };
    let joined = format!("{}{}", home.trim_end_matches('/'), &path[i..]);
    PathBuf::from(if joined.is_empty() { "/".to_string() } else { joined })
}

/// `pwd.getpwnam(name).pw_dir` (`pwd.getpwuid(os.getuid())` for None); None when there is no such user.
#[cfg(unix)]
fn passwd_home(name: Option<&str>) -> Option<String> {
    use std::ffi::{CStr, CString};

    let cname = match name {
        Some(n) => Some(CString::new(n).ok()?),
        None => None,
    };
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: an all-zero passwd is a valid out-parameter; getpw*_r writes only into `pwd` and `buf`.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call and `buf.len()` is its size.
    let rc = unsafe {
        match &cname {
            Some(c) => libc::getpwnam_r(c.as_ptr(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result),
            None => libc::getpwuid_r(libc::getuid(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result),
        }
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: pw_dir points into `buf`, NUL-terminated by getpw*_r.
    Some(unsafe { CStr::from_ptr(pwd.pw_dir) }.to_string_lossy().into_owned())
}

#[cfg(not(unix))]
fn passwd_home(name: Option<&str>) -> Option<String> {
    match name {
        None => dirs::home_dir().map(|h| h.to_string_lossy().into_owned()),
        Some(_) => None,
    }
}

/// The settings whose Python default is an int, so the warning shows "4", not "4.0".
const INT_SETTINGS: [&str; 3] = ["EVE_WATCH_BURST", "EVE_WATCH_MODEL_CALLS", "EVE_WATCH_HOSTED_CALLS"];

/// Python's `float(text)` of a stripped string: a decimal or exponent literal, inf / infinity / nan in any case,
/// single underscores between digits.
fn py_float(text: &str) -> Option<f64> {
    let b = text.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if c == b'_' {
            let digit_before = i > 0 && b[i - 1].is_ascii_digit();
            let digit_after = b.get(i + 1).is_some_and(u8::is_ascii_digit);
            if !(digit_before && digit_after) {
                return None;
            }
        }
    }
    text.replace('_', "").parse::<f64>().ok()
}

/// `_number(env, name, default, lo, hi)`: a float setting, clamped; unset is the default, unreadable is the default
/// with a warning.
pub fn number(ctx: &Ctx, name: &str, default: f64, lo: f64, hi: f64) -> f64 {
    let raw = ctx.get(name);
    let raw = py::strip(&raw);
    if raw.is_empty() {
        return default;
    }
    let Some(value) = py_float(raw) else {
        let shown = if INT_SETTINGS.contains(&name) && default == default.trunc() {
            format!("{}", default as i64)
        } else {
            py::float_repr(default)
        };
        tracing::warn!("watch: {}={} is not a number; using {}", name, py::repr_str(raw), shown);
        return default;
    };
    // max(lo, min(hi, value)), as Python's min and max compare (a NaN lands on hi)
    let capped = if value < hi { value } else { hi };
    if capped > lo {
        capped
    } else {
        lo
    }
}

/// A switch's words for off.
fn off(raw: &str) -> bool {
    ["0", "false", "no", "off"].contains(&raw)
}

/// A switch's words for on.
fn on(raw: &str) -> bool {
    ["1", "true", "yes", "on"].contains(&raw)
}

/// `env.get(name) or default`, stripped and lowercased.
fn switch(ctx: &Ctx, name: &str, default: &str) -> String {
    let raw = ctx.get(name);
    py::strip(if raw.is_empty() { default } else { &raw }).to_lowercase()
}

/// `_browser_on`: EVE_WATCH_BROWSER = 1 | 0 | auto (the default: on when a headless Chromium is installed;
/// Python's "Playwright is importable", see PARITY).
pub fn browser_on(ctx: &Ctx) -> bool {
    let raw = switch(ctx, "EVE_WATCH_BROWSER", "auto");
    if off(&raw) {
        return false;
    }
    let have = eve_browser_lane::cdp::find_chromium(true).is_some();
    if on(&raw) && !have {
        tracing::warn!(
            "watch: EVE_WATCH_BROWSER is on but no headless Chromium is installed \
             (python -m playwright install chromium); off"
        );
    }
    have
}

/// `_tls_on`: EVE_WATCH_TLS = 1 | 0 | auto (the default: on when the Chrome-like reader is available).
pub fn tls_on(ctx: &Ctx) -> bool {
    let raw = switch(ctx, "EVE_WATCH_TLS", "auto");
    if off(&raw) {
        return false;
    }
    let have = tls_available();
    if on(&raw) && !have {
        tracing::warn!("watch: EVE_WATCH_TLS is on but curl_cffi is not installed (pip install -e \".[tls]\"); off");
    }
    have
}

/// `EVE_WATCH` (on unless 0/false/off), `EVE_WATCH_FILE`, the limiter's `_RATE` (per minute, 1-600), `_BURST`
/// (1-50), `_HOST_GAP` (seconds, 0-3600) and `_MODEL_CALLS` (per day, 0-10000), `_MODEL` for the page fallback
/// and searches, `_VISION_MODEL` for screenshots, `_BROWSER` for headless Chromium, `_TLS` for the Chrome-like
/// retry, and `_HOSTED` (with `TINYFISH_API_KEY`) and `_HOSTED_CALLS` (per day) for the hosted reader.
pub fn configured(ctx: &Ctx) -> WatchConfig {
    let enabled = on(&switch(ctx, "EVE_WATCH", if WATCH_DEFAULT { "1" } else { "0" }));
    // Any OpenRouter model id, openai/* included: every model call is on the owner's OpenRouter key since
    // 2026-10-08 (docs/openrouter.md); before that an OpenAI model was refused here.
    let or_default = |name: &str| {
        let raw = ctx.get(name);
        py::strip(if raw.is_empty() { DEFAULT_MODEL } else { &raw }).to_string()
    };
    let model = or_default("EVE_WATCH_MODEL");
    let vision_model = or_default("EVE_WATCH_VISION_MODEL");
    let file = ctx.get("EVE_WATCH_FILE");
    WatchConfig {
        enabled,
        path: expand_user(if file.is_empty() { DEFAULT_PATH } else { &file }),
        rate_per_min: number(ctx, "EVE_WATCH_RATE", DEFAULT_RATE_PER_MIN, 1.0, 600.0),
        burst: number(ctx, "EVE_WATCH_BURST", DEFAULT_BURST as f64, 1.0, 50.0) as i64,
        host_gap_secs: number(ctx, "EVE_WATCH_HOST_GAP", DEFAULT_HOST_GAP_SECS, 0.0, 3600.0),
        model_calls_per_day: number(ctx, "EVE_WATCH_MODEL_CALLS", DEFAULT_MODEL_CALLS_PER_DAY as f64, 0.0, 10000.0)
            as i64,
        model,
        ticketmaster: !py::strip(&ctx.get("TICKETMASTER_API_KEY")).is_empty(),
        browser: browser_on(ctx),
        tls: tls_on(ctx),
        vision_model,
        search: websearch::configured(ctx),
        hosted: !tinyfish::api_key(ctx).is_empty() && !off(&switch(ctx, "EVE_WATCH_HOSTED", "1")),
        hosted_calls_per_day: number(
            ctx,
            "EVE_WATCH_HOSTED_CALLS",
            DEFAULT_HOSTED_CALLS_PER_DAY as f64,
            0.0,
            10000.0,
        ) as i64,
    }
}
