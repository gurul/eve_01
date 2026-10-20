//! eve's browser on Steel: the self-hosted Steel browser API on this Mac, with no API key.
//!
//! Owner, 2026-10-13: "replace browser with steel for eve" — "local steel". Steel (github.com/steel-dev/steel-browser,
//! Apache-2.0) is a browser API: it runs Chrome and hands out a Chrome DevTools websocket per session. The browser lane
//! already attaches to a Chrome over CDP (attach mode, own mode), so Steel is one more endpoint for the same lane:
//! `EVE_BROWSER_STEEL=1` and the lane connects to a Steel session instead of eve's own Chrome. The plan executor, the
//! gate and the tab rules are unchanged (browser lane).
//!
//! ```text
//!     web task ──▶ BrowserLane._ensure ──▶ ensure_server()  (start steel-local.sh when nothing answers /v1/health)
//!                                      ──▶ start_session()  (POST /v1/sessions → websocketUrl ws://127.0.0.1:3000/)
//!                                      ──▶ CDP connect(websocketUrl)
//! ```
//!
//! What this needs on the Mac (docs/steel.md):
//! - a checkout of steel-browser with its API built (`EVE_STEEL_DIR`, default `~/Documents/personal/steel-browser`)
//!   and its `steel-local.sh`, which binds 127.0.0.1 only and moves Chrome's DevTools port off 9222;
//! - Homebrew's `node@22` (Node 26 cannot build steel-browser's duckdb);
//! - the local timeout patch: upstream reports `timeout: 0` and Steel's CLI reads that as expired. eve itself never
//!   reads the timeout, so the lane works without the patch; only the `steel` CLI needs it.
//!
//! Sign-ins: the server is started with `CHROME_USER_DATA_DIR` pointing at `~/.config/eve-bridge/steel-profile`
//! (`EVE_STEEL_PROFILE` moves it), so cookies stay between sessions and server restarts. Steel starts one session at
//! a time: a new session (from eve or from the `steel` CLI) replaces the running one.
//!
//! Errors: the fallible calls return `anyhow::Result`. A [`SteelError`] inside it is the Python `SteelError` (callers
//! `downcast_ref::<SteelError>()` where Python caught it); anything else is what Python let escape (an `OSError` from
//! the launch or the profile directory, a `ValueError` from a malformed URL).

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Map, Value};

pub const DEFAULT_URL: &str = "http://127.0.0.1:3000";
pub const DEFAULT_DIR: &str = "~/Documents/personal/steel-browser";
pub const DEFAULT_PROFILE: &str = "~/.config/eve-bridge/steel-profile";
pub const LAUNCHER: &str = "steel-local.sh";
pub const LOG_FILE: &str = "~/Library/Logs/eve-steel.log";
/// A cold start: node, then Chrome; measured about 3 s on this Mac, 2026-10-13.
pub const START_SECS: f64 = 40.0;
pub const HTTP_TIMEOUT: f64 = 10.0;
pub const LOOPBACK: [&str; 3] = ["127.0.0.1", "localhost", "::1"];

/// Steel cannot be reached or started. The message says what to do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SteelError(pub String);

/// A malformed URL, raised where Python's `urlsplit` raises `ValueError`. Not a [`SteelError`]: Python lets it escape.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UrlError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteelConfig {
    pub url: String,
    pub steel_dir: PathBuf,
    pub profile: PathBuf,
}

impl Default for SteelConfig {
    fn default() -> Self {
        SteelConfig {
            url: DEFAULT_URL.to_string(),
            steel_dir: py_path(DEFAULT_DIR),
            profile: py_path(DEFAULT_PROFILE),
        }
    }
}

/// `EVE_STEEL_URL` (default http://127.0.0.1:3000; loopback only), `EVE_STEEL_DIR` (the steel-browser
/// checkout eve starts the server from) and `EVE_STEEL_PROFILE` (the Chrome user data directory).
pub fn configured() -> SteelConfig {
    configured_from(|k| std::env::var(k).ok())
}

/// [`configured`] over any environment (Python's `environ` argument).
pub fn configured_from<F: Fn(&str) -> Option<String>>(env: F) -> SteelConfig {
    let get = |k: &str| env(k).unwrap_or_default();
    let raw_url = get("EVE_STEEL_URL");
    let url = raw_url.trim().trim_end_matches('/');
    let url = if url.is_empty() { DEFAULT_URL } else { url };
    let dir = get("EVE_STEEL_DIR");
    let dir = if dir.trim().is_empty() { DEFAULT_DIR } else { dir.trim() };
    let profile = get("EVE_STEEL_PROFILE");
    let profile = if profile.trim().is_empty() { DEFAULT_PROFILE } else { profile.trim() };
    SteelConfig { url: url.to_string(), steel_dir: py_path(dir), profile: py_path(profile) }
}

// ---- paths -------------------------------------------------------------------------------------------------------

/// `os.path.expanduser`: `~` from `$HOME` (else the password database), `~user` from the password database; an
/// unknown user leaves the path unchanged.
pub fn expanduser(path: &str) -> String {
    if !path.starts_with('~') {
        return path.to_string();
    }
    let i = path[1..].find('/').map(|j| j + 1).unwrap_or(path.len());
    let home = if i == 1 {
        match std::env::var_os("HOME") {
            Some(h) => Some(h.to_string_lossy().into_owned()),
            None => dirs::home_dir().map(|h| h.to_string_lossy().into_owned()),
        }
    } else {
        user_home(&path[1..i])
    };
    let Some(home) = home else { return path.to_string() };
    let joined = format!("{}{}", home.trim_end_matches('/'), &path[i..]);
    if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    }
}

#[cfg(unix)]
fn user_home(name: &str) -> Option<String> {
    use std::ffi::{CStr, CString};
    let cname = CString::new(name).ok()?;
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16384];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call, and the buffer outlives the read of pw_dir below.
    let rc = unsafe { libc::getpwnam_r(cname.as_ptr(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: getpwnam_r succeeded, so pw_dir points at a NUL-terminated string inside `buf`.
    Some(unsafe { CStr::from_ptr(pwd.pw_dir) }.to_string_lossy().into_owned())
}

#[cfg(not(unix))]
fn user_home(_name: &str) -> Option<String> {
    None
}

/// `Path(s).expanduser()`: expanded, then normalised the way pathlib spells it (no doubled or trailing slashes, no
/// `.` parts), so `str(path)` matches Python's in argv, env and messages.
pub fn py_path(s: &str) -> PathBuf {
    let expanded = expanduser(s);
    let p = Path::new(&expanded);
    let mut out = PathBuf::new();
    for c in p.components() {
        if c != Component::CurDir {
            out.push(c.as_os_str());
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    // pathlib keeps exactly two leading slashes (POSIX leaves them implementation-defined).
    if expanded.starts_with("//") && !expanded.starts_with("///") {
        let mut os = OsString::from("/");
        os.push(out.as_os_str());
        return PathBuf::from(os);
    }
    out
}

// ---- urlsplit ----------------------------------------------------------------------------------------------------

/// The netloc of `urllib.parse.urlsplit(url)`, with its `ValueError`s.
fn netloc(url: &str) -> Result<String, UrlError> {
    let url: String = url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut rest: &str = &url;
    if let Some(i) = url.find(':') {
        let first = url.chars().next().unwrap_or(' ');
        if i > 0
            && first.is_ascii_alphabetic()
            && url[..i].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            rest = &url[i + 1..];
        }
    }
    if !rest.starts_with("//") {
        return Ok(String::new());
    }
    let after = &rest[2..];
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let netloc = &after[..end];
    let (open, close) = (netloc.contains('['), netloc.contains(']'));
    if open != close {
        return Err(UrlError("Invalid IPv6 URL".into()));
    }
    if open && close {
        check_bracketed_netloc(netloc)?;
    }
    Ok(netloc.to_string())
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), UrlError> {
    let host_port = netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(netloc);
    let host = match host_port.split_once('[') {
        Some((before, bracketed)) => {
            if !before.is_empty() {
                return Err(UrlError("Invalid IPv6 URL".into()));
            }
            let (host, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !port.is_empty() && !port.starts_with(':') {
                return Err(UrlError("Invalid IPv6 URL".into()));
            }
            host
        }
        None => host_port.split_once(':').map(|(h, _)| h).unwrap_or(host_port),
    };
    if let Some(v) = host.strip_prefix('v') {
        let ok = v.split_once('.').is_some_and(|(hex, tail)| {
            !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) && !tail.is_empty() && !tail.contains('\n')
        });
        if !ok {
            return Err(UrlError("IPvFuture address is invalid".into()));
        }
        return Ok(());
    }
    let addr = host.split_once('%').map(|(a, _)| a).unwrap_or(host);
    if addr.parse::<std::net::Ipv6Addr>().is_ok() {
        Ok(())
    } else if addr.parse::<std::net::Ipv4Addr>().is_ok() {
        Err(UrlError("An IPv4 address cannot be in brackets".into()))
    } else {
        Err(UrlError(format!("'{host}' does not appear to be an IPv4 or IPv6 address")))
    }
}

/// `SplitResult._hostinfo`: the raw hostname and port strings.
fn hostinfo(netloc: &str) -> (String, Option<String>) {
    let hostinfo = netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(netloc);
    let (host, port) = match hostinfo.split_once('[') {
        Some((_, bracketed)) => {
            let (h, p) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            (h, p.split_once(':').map(|(_, p)| p).unwrap_or(""))
        }
        None => hostinfo.split_once(':').unwrap_or((hostinfo, "")),
    };
    (host.to_string(), (!port.is_empty()).then(|| port.to_string()))
}

/// `urlsplit(url).hostname`: lowercased (a `%zone` keeps its case), `None` when empty.
pub fn hostname(url: &str) -> Result<Option<String>, UrlError> {
    let (host, _) = hostinfo(&netloc(url)?);
    if host.is_empty() {
        return Ok(None);
    }
    Ok(Some(match host.split_once('%') {
        Some((h, zone)) => format!("{}%{}", h.to_lowercase(), zone),
        None => host.to_lowercase(),
    }))
}

/// `urlsplit(url).port`.
pub fn port(url: &str) -> Result<Option<u16>, UrlError> {
    let (_, port) = hostinfo(&netloc(url)?);
    let Some(p) = port else { return Ok(None) };
    if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
        return Err(UrlError(format!("Port could not be cast to integer value as {}", py_repr_str(&p))));
    }
    match p.trim_start_matches('0').parse::<u32>() {
        Ok(n) if n <= 65535 => Ok(Some(n as u16)),
        _ if p.trim_start_matches('0').is_empty() => Ok(Some(0)),
        _ => Err(UrlError("Port out of range 0-65535".into())),
    }
}

fn loopback(url: &str) -> Result<bool, UrlError> {
    Ok(LOOPBACK.contains(&hostname(url)?.unwrap_or_default().as_str()))
}

// ---- HTTP --------------------------------------------------------------------------------------------------------

/// What Python's callers catch around a request (`OSError`, `ValueError`, `URLError`): the server is unreachable,
/// answered an error status, or answered something that is not JSON.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HttpError(pub String);

/// The seam over Steel's HTTP API (Python's `http` argument). `timeout` is in seconds.
#[async_trait]
pub trait Http: Send + Sync {
    async fn request(&self, method: &str, url: &str, body: Option<&Value>, timeout: f64)
        -> Result<Map<String, Value>, HttpError>;
}

/// Python's `_request`: JSON in, JSON out, `Content-Type: application/json` on every request, `{}` for an empty
/// reply. Loopback only, so no proxy.
pub struct DefaultHttp;

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder().no_proxy().build().unwrap_or_else(|_| reqwest::Client::new())
});

#[async_trait]
impl Http for DefaultHttp {
    async fn request(&self, method: &str, url: &str, body: Option<&Value>, timeout: f64)
        -> Result<Map<String, Value>, HttpError> {
        request(method, url, body, timeout).await
    }
}

async fn request(method: &str, url: &str, body: Option<&Value>, timeout: f64) -> Result<Map<String, Value>, HttpError> {
    let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| HttpError(e.to_string()))?;
    let mut req = CLIENT
        .request(m, url)
        .header("Content-Type", "application/json")
        .timeout(Duration::from_secs_f64(timeout.max(0.0)));
    if let Some(b) = body {
        req = req.body(py_dumps(b));
    }
    let resp = req.send().await.map_err(|e| HttpError(format!("<urlopen error {e}>")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(HttpError(format!(
            "HTTP Error {}: {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        )));
    }
    let raw = resp.bytes().await.map_err(|e| HttpError(e.to_string()))?;
    if raw.is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(&raw) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(other) => Err(HttpError(format!("Steel answered {}, not a JSON object", py_str(&other)))),
        Err(e) => Err(HttpError(e.to_string())),
    }
}

// ---- launching -----------------------------------------------------------------------------------------------------

/// One server start, as Python hands it to `subprocess.Popen`.
#[derive(Debug)]
pub struct Launch {
    pub argv: Vec<String>,
    /// The whole environment the server gets: the daemon's, plus HOST, PORT and CHROME_USER_DATA_DIR.
    pub env: Vec<(OsString, OsString)>,
    pub cwd: PathBuf,
    /// The log file, opened for append; stdout and stderr both go to it, stdin is /dev/null.
    pub out: File,
    /// Its own session: the server outlives a daemon restart, like eve's own Chrome does.
    pub start_new_session: bool,
}

impl Launch {
    pub fn env_var(&self, key: &str) -> Option<&OsStr> {
        self.env.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.as_os_str())
    }
}

/// The seam over `subprocess.Popen`.
pub trait Spawn: Send + Sync {
    fn spawn(&self, launch: Launch) -> io::Result<()>;
}

/// Starts the process for real, in its own session, and reaps it in the background when it exits (as Python's
/// discarded `Popen` is reaped), without ever waiting on it.
pub struct ProcessSpawn;

impl Spawn for ProcessSpawn {
    fn spawn(&self, launch: Launch) -> io::Result<()> {
        let (prog, args) = launch
            .argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
        let mut cmd = std::process::Command::new(prog);
        cmd.args(args)
            .env_clear()
            .envs(launch.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&launch.cwd)
            .stdin(std::process::Stdio::null())
            .stderr(launch.out.try_clone()?)
            .stdout(launch.out);
        #[cfg(unix)]
        if launch.start_new_session {
            use std::os::unix::process::CommandExt;
            // SAFETY: setsid is async-signal-safe and touches no memory of the parent.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = cmd.spawn()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// The seam over `time.monotonic` and `time.sleep`.
#[async_trait]
pub trait Clock: Send + Sync {
    fn now(&self) -> f64;
    async fn sleep(&self, secs: f64);
}

pub struct SystemClock;

static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

#[async_trait]
impl Clock for SystemClock {
    fn now(&self) -> f64 {
        EPOCH.elapsed().as_secs_f64()
    }
    async fn sleep(&self, secs: f64) {
        tokio::time::sleep(Duration::from_secs_f64(secs.max(0.0))).await;
    }
}

/// Python's keyword arguments to `ensure_server` (`http`, `popen`, `sleep`/`clock`, `wait_secs`) plus the log file,
/// which Python fixes at `LOG_FILE`.
pub struct Deps<'a> {
    pub http: &'a dyn Http,
    pub spawn: &'a dyn Spawn,
    pub clock: &'a dyn Clock,
    pub wait_secs: f64,
    pub log_file: PathBuf,
}

impl Default for Deps<'static> {
    fn default() -> Self {
        Deps { http: &DefaultHttp, spawn: &ProcessSpawn, clock: &SystemClock, wait_secs: START_SECS, log_file: py_path(LOG_FILE) }
    }
}

impl<'a> Deps<'a> {
    /// The defaults with another HTTP client (Python's `start_session(cfg, http=...)`).
    pub fn with_http(http: &'a dyn Http) -> Deps<'a> {
        Deps { http, ..Deps::default() }
    }
}

// ---- the API ---------------------------------------------------------------------------------------------------------

pub async fn healthy(cfg: &SteelConfig, http: &dyn Http) -> bool {
    match http.request("GET", &format!("{}/v1/health", cfg.url), None, 2.0).await {
        Ok(m) => m.get("status") == Some(&Value::String("ok".into())),
        Err(_) => false,
    }
}

pub fn launch_command(cfg: &SteelConfig) -> Vec<String> {
    vec![cfg.steel_dir.join(LAUNCHER).to_string_lossy().into_owned()]
}

/// `Path.mkdir(parents=True, exist_ok=True, mode=mode)`: parents get the default mode, the leaf gets `mode` (less
/// the umask); an existing directory is fine and keeps its mode.
fn mkdir_leaf_mode(path: &Path, mode: u32) -> io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    match b.create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            match b.create(path) {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
                r => r,
            }
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

/// Steel is answering, or start it and wait until it is. Fails with [`SteelError`] when it cannot be.
pub async fn ensure_server(cfg: &SteelConfig) -> anyhow::Result<()> {
    ensure_server_with(cfg, &Deps::default()).await
}

pub async fn ensure_server_with(cfg: &SteelConfig, deps: &Deps<'_>) -> anyhow::Result<()> {
    if !loopback(&cfg.url)? {
        return Err(SteelError(format!(
            "EVE_STEEL_URL must be on this Mac (127.0.0.1 or localhost), not {}",
            cfg.url
        ))
        .into());
    }
    if healthy(cfg, deps.http).await {
        return Ok(());
    }
    let launcher = cfg.steel_dir.join(LAUNCHER);
    if !launcher.is_file() {
        return Err(SteelError(format!(
            "Steel is not running and {} is missing: see docs/steel.md",
            launcher.display()
        ))
        .into());
    }
    mkdir_leaf_mode(&cfg.profile, 0o700)?;
    let port = match port(&cfg.url)? {
        Some(p) if p != 0 => p,
        _ => 3000,
    };
    let mut env: Vec<(OsString, OsString)> =
        std::env::vars_os().filter(|(k, _)| !matches!(k.to_str(), Some("HOST" | "PORT" | "CHROME_USER_DATA_DIR"))).collect();
    env.push(("HOST".into(), "127.0.0.1".into()));
    env.push(("PORT".into(), port.to_string().into()));
    env.push(("CHROME_USER_DATA_DIR".into(), cfg.profile.clone().into_os_string()));
    let logfile = &deps.log_file;
    if let Some(parent) = logfile.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let out = std::fs::OpenOptions::new().append(true).create(true).open(logfile)?;
    // Its own session: the server outlives a daemon restart, like eve's own Chrome does.
    deps.spawn.spawn(Launch {
        argv: launch_command(cfg),
        env,
        cwd: cfg.steel_dir.clone(),
        out,
        start_new_session: true,
    })?;
    tracing::info!("steel: starting the local server ({}, profile {})", cfg.url, cfg.profile.display());
    let deadline = deps.clock.now() + deps.wait_secs;
    while deps.clock.now() < deadline {
        deps.clock.sleep(0.5).await;
        if healthy(cfg, deps.http).await {
            tracing::info!("steel: up at {}", cfg.url);
            return Ok(());
        }
    }
    Err(SteelError(format!("Steel did not answer within {:.0} s: see {}", deps.wait_secs, logfile.display())).into())
}

/// A fresh Steel session; returns its CDP websocket URL. Starts the server first when needed.
/// `visible` opens Chrome in a window on the Mac (signing in: `eve-bridge steel-profile`).
pub async fn start_session(cfg: &SteelConfig, visible: bool) -> anyhow::Result<String> {
    start_session_with(cfg, &Deps::default(), visible).await
}

pub async fn start_session_with(cfg: &SteelConfig, deps: &Deps<'_>, visible: bool) -> anyhow::Result<String> {
    ensure_server_with(cfg, deps).await?;
    let mut body = Map::new();
    body.insert("blockAds".into(), Value::Bool(true));
    if visible {
        body.insert("headless".into(), Value::Bool(false));
    }
    let session = deps
        .http
        .request("POST", &format!("{}/v1/sessions", cfg.url), Some(&Value::Object(body)), HTTP_TIMEOUT)
        .await
        .map_err(|e| SteelError(format!("Steel refused a new session: {e}")))?;
    let ws = or_default(session.get("websocketUrl"), "");
    if !(ws.starts_with("ws://") || ws.starts_with("wss://")) || !loopback(&ws)? {
        let shown = if ws.is_empty() { "none" } else { ws.as_str() };
        return Err(SteelError(format!("Steel answered without a local websocket URL ({shown})")).into());
    }
    let id: String = or_default(session.get("id"), "?").chars().take(8).collect();
    tracing::info!("steel: session {}", id);
    Ok(ws)
}

/// End the running session: Chrome closes and writes the profile. Quiet when Steel is gone.
pub async fn release(cfg: &SteelConfig) {
    release_with(cfg, &DefaultHttp).await
}

pub async fn release_with(cfg: &SteelConfig, http: &dyn Http) {
    let _ = http
        .request("POST", &format!("{}/v1/sessions/release", cfg.url), Some(&Value::Object(Map::new())), HTTP_TIMEOUT)
        .await;
}

// ---- Python value spelling ---------------------------------------------------------------------------------------

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str(value or default)`.
fn or_default(v: Option<&Value>, default: &str) -> String {
    match v {
        Some(v) if truthy(v) => py_str(v),
        _ => default.to_string(),
    }
}

fn py_float(f: f64) -> String {
    if f.is_nan() {
        "nan".into()
    } else if f.is_infinite() {
        if f > 0.0 { "inf".into() } else { "-inf".into() }
    } else if f.fract() == 0.0 && f.abs() < 1e16 {
        format!("{f:.1}")
    } else {
        let s = format!("{f:?}");
        match s.split_once('e') {
            Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e:0>2}"),
            Some((m, e)) => format!("{m}e-{:0>2}", &e[1..]),
            None => s,
        }
    }
}

fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
        other => py_str(other),
    }
}

/// `str(value)` for a parsed JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) if n.is_f64() => py_float(n.as_f64().unwrap_or(0.0)),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `json.dumps(value)`: `", "` and `": "` separators, non-ASCII escaped.
pub fn py_dumps(v: &Value) -> String {
    fn esc(s: &str, out: &mut String) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                    let mut buf = [0u16; 2];
                    for u in c.encode_utf16(&mut buf) {
                        out.push_str(&format!("\\u{:04x}", u));
                    }
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }
    fn go(v: &Value, out: &mut String) {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) if n.is_f64() => {
                let f = n.as_f64().unwrap_or(0.0);
                out.push_str(&match py_float(f).as_str() {
                    "nan" => "NaN".to_string(),
                    "inf" => "Infinity".to_string(),
                    "-inf" => "-Infinity".to_string(),
                    s => s.to_string(),
                });
            }
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::String(s) => esc(s, out),
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    go(x, out);
                }
                out.push(']');
            }
            Value::Object(o) => {
                out.push('{');
                for (i, (k, x)) in o.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    esc(k, out);
                    out.push_str(": ");
                    go(x, out);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    go(v, &mut out);
    out
}

#[cfg(test)]
mod tests;
