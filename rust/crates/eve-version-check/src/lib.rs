//! GitHub-Release-based version check for the bridge.
//!
//! Port of `eve_bridge/version_check.py`.
//!
//! One HTTP call per day to `https://api.github.com/repos/.../releases/latest`,
//! cached to disk. The daemon polls every 24 h; the `check-update` CLI command
//! forces a refresh. `EVE_BRIDGE_NO_UPDATE_CHECK=1` disables both paths.
//!
//! Firmware version detection is intentionally out of scope here: the stick's
//! status ack doesn't carry a fw version field, and the upstream firmware repo
//! has no releases/tags to compare against. Would require an upstream PR.

pub mod py;

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub const GITHUB_API: &str = "https://api.github.com/repos/SnowWarri0r/eve-bridge/releases/latest";
/// `24 * 3600`.
pub const CACHE_TTL_SECS: f64 = 86400.0;
/// `f"eve-bridge/{__version__}"`.
pub static USER_AGENT: LazyLock<String> = LazyLock::new(|| format!("eve-bridge/{}", eve_meta::VERSION));
pub const HTTP_TIMEOUT_SECS: f64 = 3.0;

/// `time.time()`.
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `Path.home()`: `$HOME` when it is set (an empty one reads as `/`, as
/// `os.path.expanduser` gives), else the password database.
fn home() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) if h.is_empty() => PathBuf::from("/"),
        Some(h) => PathBuf::from(h),
        None => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
    }
}

/// `sys.platform` for the platforms the Python branches on.
fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else {
        "linux"
    }
}

/// `_cache_path` with its inputs explicit: `platform` is a `sys.platform`
/// value, `xdg_cache_home` the raw `XDG_CACHE_HOME` (an empty value falls
/// back, as Python's `or` does).
pub fn cache_path_for(platform: &str, home: &Path, xdg_cache_home: Option<&str>) -> PathBuf {
    let base = match platform {
        "darwin" => home.join("Library").join("Caches"),
        "win32" => home.join("AppData").join("Local"),
        _ => match xdg_cache_home {
            Some(x) if !x.is_empty() => PathBuf::from(x),
            _ => home.join(".cache"),
        },
    };
    base.join("eve-bridge").join("update_check.json")
}

/// Per-platform cache file for the last GitHub query result.
pub fn cache_path() -> PathBuf {
    let xdg = std::env::var_os("XDG_CACHE_HOME").map(|v| v.to_string_lossy().into_owned());
    cache_path_for(platform(), &home(), xdg.as_deref())
}

/// `_disabled`: exactly `EVE_BRIDGE_NO_UPDATE_CHECK=1`.
pub fn disabled() -> bool {
    std::env::var_os("EVE_BRIDGE_NO_UPDATE_CHECK").is_some_and(|v| v == "1")
}

/// One snapshot of the GitHub releases/latest endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateInfo {
    pub current: String,
    /// `None` on first-fetch failure.
    pub latest: Option<String>,
    /// Unix seconds.
    pub fetched_at: f64,
}

impl UpdateInfo {
    pub fn has_update(&self) -> bool {
        match &self.latest {
            Some(latest) if !latest.is_empty() => is_newer(latest, &self.current),
            _ => false,
        }
    }

    pub fn is_fresh(&self) -> bool {
        (now() - self.fetched_at) < CACHE_TTL_SECS
    }
}

// Semver-ish comparison: split on dots and non-digits, compare numerically when
// both parts parse as ints else lexically. Treats "0.1.0" < "0.1.1" < "0.2.0".
// Pre-release suffixes (-rc1, -alpha) compare lexically and are considered older
// than the bare version (e.g. "0.2.0" > "0.2.0-rc1") via the empty-after-dash
// trick. Good enough for our v-prefixed Git tags.

/// One element of the tuple `_parts` builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// A dot-separated piece made of digits, as its decimal digits with the
    /// leading zeros removed (`int(p)`, unbounded like Python's int).
    Int(String),
    /// Any other dot-separated piece, compared lexically.
    Str(String),
    /// The final `(is_release, pre)` element: release = `(True, "")`,
    /// pre-release = `(False, pre)`.
    Pre(bool, String),
}

/// `int(p)` for an all-digits piece, as normalized digits.
fn int_digits(p: &str) -> String {
    let t = p.trim_start_matches('0');
    if t.is_empty() { "0".to_string() } else { t.to_string() }
}

/// `_parts`: the comparison key of a version string.
pub fn parts(v: &str) -> Vec<Part> {
    let v = py::strip(v);
    let v = v.strip_prefix('v').unwrap_or(v);
    // Split version on '-' so pre-release suffixes compare separately.
    let (base, pre) = v.split_once('-').unwrap_or((v, ""));
    let mut out: Vec<Part> = base
        .split('.')
        .map(|p| {
            if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) {
                Part::Int(int_digits(p))
            } else {
                Part::Str(p.to_string())
            }
        })
        .collect();
    // Empty pre sorts AFTER any non-empty pre, so "0.2.0" > "0.2.0-rc1":
    // release = (True,), pre-release = (False, pre).
    out.push(if pre.is_empty() { Part::Pre(true, String::new()) } else { Part::Pre(false, pre.to_string()) });
    out
}

/// Python's `>` between two tuple elements; `None` where it raises
/// `TypeError` (an int against a str, or either against the tuple).
fn part_cmp(a: &Part, b: &Part) -> Option<Ordering> {
    match (a, b) {
        (Part::Int(x), Part::Int(y)) => Some(x.len().cmp(&y.len()).then_with(|| x.cmp(y))),
        (Part::Str(x), Part::Str(y)) => Some(x.cmp(y)),
        (Part::Pre(xr, xp), Part::Pre(yr, yp)) => Some(xr.cmp(yr).then_with(|| xp.cmp(yp))),
        _ => None,
    }
}

/// `tuple > tuple` with Python's rules: the first pair that is not `==`
/// decides, and comparing unorderable types raises.
fn tuple_gt(a: &[Part], b: &[Part]) -> Option<bool> {
    for (x, y) in a.iter().zip(b) {
        if x != y {
            return part_cmp(x, y).map(|o| o == Ordering::Greater);
        }
    }
    Some(a.len() > b.len())
}

/// True iff candidate > baseline. Robust against missing/garbage strings.
pub fn is_newer(candidate: &str, baseline: &str) -> bool {
    if candidate.is_empty() || baseline.is_empty() {
        return false;
    }
    tuple_gt(&parts(candidate), &parts(baseline)).unwrap_or(false)
}

static CLIENT: LazyLock<Option<reqwest::blocking::Client>> = LazyLock::new(|| {
    let timeout = Duration::from_secs_f64(HTTP_TIMEOUT_SECS);
    match reqwest::blocking::Client::builder()
        .connect_timeout(timeout)
        .timeout(timeout)
        .build()
    {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::debug!("update check: GitHub fetch failed: {}", e);
            None
        }
    }
});

/// The `tag_name` of a releases/latest body, if it is a string.
pub fn tag_from_body(body: &[u8]) -> Option<String> {
    let payload: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("update check: GitHub fetch failed: {}", e);
            return None;
        }
    };
    match payload.get("tag_name") {
        Some(Value::String(tag)) => Some(tag.clone()),
        _ => None,
    }
}

/// Network call. Returns the tag_name of the latest release, or `None` on
/// failure. Blocking: call it off the async runtime (see [`check_async`]).
pub fn fetch_latest() -> Option<String> {
    let client = CLIENT.as_ref()?;
    let resp = client
        .get(GITHUB_API)
        .header(reqwest::header::USER_AGENT, USER_AGENT.as_str())
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send();
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("update check: GitHub fetch failed: {}", e);
            return None;
        }
    };
    // urllib raises HTTPError for any final status outside 200..300.
    let status = resp.status();
    if !status.is_success() {
        tracing::debug!(
            "update check: GitHub fetch failed: HTTP Error {}: {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        );
        return None;
    }
    match resp.bytes() {
        Ok(body) => tag_from_body(&body),
        Err(e) => {
            tracing::debug!("update check: GitHub fetch failed: {}", e);
            None
        }
    }
}

/// `_load_cache` against an explicit path.
pub fn load_cache_at(path: &Path) -> Option<UpdateInfo> {
    if !path.exists() {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let data: Value = serde_json::from_slice(&bytes).ok()?;
    // data["current"] and data["fetched_at"] raise on a non-dict or a missing
    // key; both make the cache unusable.
    let obj = data.as_object()?;
    let current = py::str_of(obj.get("current")?);
    let latest = match obj.get("latest") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        // Only a hand-edited file holds anything else; Python would carry it
        // and crash later in has_update. Treat the cache as unusable instead.
        Some(_) => return None,
    };
    let fetched_at = py::float_of(obj.get("fetched_at")?)?;
    Some(UpdateInfo { current, latest, fetched_at })
}

/// `_load_cache` at [`cache_path`].
pub fn load_cache() -> Option<UpdateInfo> {
    load_cache_at(&cache_path())
}

/// The bytes `json.dump` writes for the cache, with Python's default
/// separators and `ensure_ascii`.
pub fn cache_json(info: &UpdateInfo) -> String {
    let latest = match &info.latest {
        Some(l) => py::json_str(l),
        None => "null".to_string(),
    };
    format!(
        "{{\"current\": {}, \"latest\": {}, \"fetched_at\": {}}}",
        py::json_str(&info.current),
        latest,
        py::json_float(info.fetched_at)
    )
}

/// `_save_cache` against an explicit path.
pub fn save_cache_at(info: &UpdateInfo, path: &Path) {
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, cache_json(info))
    })();
    if let Err(e) = result {
        tracing::debug!("update check: cache write failed: {}", e);
    }
}

/// `_save_cache` at [`cache_path`].
pub fn save_cache(info: &UpdateInfo) {
    save_cache_at(info, &cache_path())
}

/// `check` with every input explicit: whether the check is disabled, the
/// installed version, the cache file, and the network call.
pub fn check_with<F>(force: bool, disabled: bool, current: &str, cache: &Path, fetch: F) -> UpdateInfo
where
    F: FnOnce() -> Option<String>,
{
    let current = current.to_string();
    if disabled {
        return UpdateInfo { current, latest: None, fetched_at: now() };
    }

    if !force {
        if let Some(cached) = load_cache_at(cache) {
            if cached.is_fresh() && cached.current == current {
                return cached;
            }
        }
    }

    let latest = fetch();
    let info = UpdateInfo { current, latest, fetched_at: now() };
    if info.latest.is_some() {
        save_cache_at(&info, cache);
    }
    info
}

/// Return current update state.
///
/// With `force == false` (the default), reuse a fresh cache if available.
/// With `force == true`, always hit the network. Returns a stale-cache or
/// current-only UpdateInfo if disabled or the network call fails.
///
/// Blocking (the network call is synchronous, like Python's urllib); from
/// async code use [`check_async`], the port of `asyncio.to_thread(check, ...)`.
pub fn check(force: bool) -> UpdateInfo {
    check_with(force, disabled(), eve_meta::VERSION, &cache_path(), fetch_latest)
}

/// [`check`] on the blocking pool, as the daemon's `asyncio.to_thread` call.
pub async fn check_async(force: bool) -> UpdateInfo {
    match tokio::task::spawn_blocking(move || check(force)).await {
        Ok(info) => info,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        // The runtime is shutting down: report what is installed, nothing fetched.
        Err(_) => UpdateInfo { current: eve_meta::VERSION.to_string(), latest: None, fetched_at: now() },
    }
}

// ---- CLI helper ----

/// Human-friendly multi-line summary for `eve-bridge check-update`.
pub fn render_cli(info: &UpdateInfo) -> String {
    let mut lines = vec![format!("Installed:   {}", info.current)];
    let Some(latest) = &info.latest else {
        lines.push("Latest:      (could not reach github.com — try again later)".to_string());
        return lines.join("\n");
    };
    lines.push(format!("Latest:      {latest}"));
    if info.has_update() {
        lines.push(String::new());
        lines.push(format!("Update available: {} → {}", info.current, latest));
        lines.push("Pull with:        git pull && pip install -e .".to_string());
        lines.push("Then restart:     eve-bridge install --service  (or kickstart the daemon)".to_string());
    } else {
        lines.push("You're on the latest release.".to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests;
