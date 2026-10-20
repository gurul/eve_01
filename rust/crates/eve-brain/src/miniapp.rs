//! A port of `miniapp.py`, the whole module: eve's Telegram Mini App, the apps eve makes, opened from the chat.
//!
//! Owner, 2026-09-24: "mini apps are to replace websites, like i want to build a habit tracker, it should do it". A
//! Mini App is a web page Telegram opens over the chat. Its home lists what eve built and asks "What should I
//! build?" (apps_maker). It had a Claude chat tab too, removed the same day ("why does chat still exist in apps"):
//! the apps are the point.
//!
//! * **Served by the daemon** on 127.0.0.1 only. Telegram needs a public HTTPS address, so a Cloudflare quick tunnel
//!   (`cloudflared tunnel --url`: no account, no domain) fronts it. Its address changes each start.
//! * **The way in.** The menu button stays the / commands (owner: "i want both"); a message pinned at the top of the
//!   chat carries an **Open eve** button, edited to the new address every start and marked offline on stop.
//!   `/apps` sends the same button, and a finished build sends the app's picture with an Open button.
//! * **Only the owners.** Every API call from the home page carries Telegram's signed `initData`; the server
//!   checks its HMAC with the bot token (Telegram's WebAppData scheme), its age, and that the user is in
//!   `EVE_TELEGRAM_OWNER`. Without a valid signature nothing is answered.
//! * **An app is fenced in.** An app is model-written code (plus any library it pins from jsdelivr), so it never
//!   gets the owner's initData. It is served sandboxed (`APP_CSP`: an opaque origin, no browser storage) and
//!   opened with an app token (`app_token`: signed with the bot token, bound to that app's slug, 24 hours) that
//!   answers for its own load and save and nothing else; its Content-Security-Policy lets it connect to those two
//!   addresses only. The page itself needs the token too: without one, `/apps/<slug>/` sends the phone to the
//!   home page (`/?open=<slug>`), which opens the app with a fresh token when Telegram signed the visit.
//! * **Claude** is `claude-opus-5-5` (`EVE_MINIAPP_MODEL`) on OpenRouter with the owner's `OPENROUTER_API_KEY`.
//!   Adaptive thinking is always on for this model; effort (`EVE_MINIAPP_MAKE_EFFORT`, default high) is the depth
//!   control.
//! * **Spend is tracked, not capped.** Owner, 2026-09-24: "no claude limit, just track spend". Each build's cost is
//!   computed from its usage at the model's list price and added to a per-day ledger, and a line in the chat marks
//!   each of `SPEND_ALERTS_USD` the day crosses. `EVE_MINIAPP_DAILY_USD` set above 0 turns that total into a daily
//!   cap again.
//! * **Everything eve spends is on the page.** The header's "$X spent today" is ALL of eve's spend today (the daily
//!   meter, eve_spend), not only the builds; tapping it opens the Spending view (`/api/spend`, owner only,
//!   aggregates only).
//! * **Apps can be changed, undone, renamed and deleted** from the home page's "…" sheet. A delete moves the app to
//!   eve's `apps/.trash` on the Mac, and Undo on the page (or restore_app in the chat) brings it back.
//! * **Each app has its own change chat** at `/?chat=<slug>` (`/api/apps/<slug>/history`, owner only).
//! * **The Lights, Music and TV remote cards** go through `/api/lights`, `/api/spotify` and `/api/firetv` (owner
//!   only). `/api/me` lists the lights' names and rooms, never an address.
//!
//! Off unless `EVE_MINIAPP=1` with the Telegram door configured and `OPENROUTER_API_KEY` set.
//!
//! Python's `asyncio.Task.cancel()` on `MiniApp.run` is [`MiniApp::cancel`] here: `run` then does what its
//! `finally` did (the tunnel stopped, the server closed, the pinned button marked offline) and returns. A `run`
//! future dropped without it (its task aborted) does the same in a task of its own.

use std::collections::HashMap;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Weak;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::FutureExt;
use hmac::{Hmac, Mac};
use indexmap::IndexMap;
use sha2::Sha256;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[allow(unused_imports)]
use crate::prelude::*;

use crate::apps_maker::{AppInfo, AppMaker, AppStore, Made};

// py: miniapp.py:75 `log`: the `tracing` macros, with Python's log lines.

/// py: miniapp.py:77 `DEFAULT_MODEL`
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

/// py: miniapp.py:78 `EFFORTS`
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// py: miniapp.py:79 `DEFAULT_MAKE_EFFORT`
/// building an app is real work: think it through
pub const DEFAULT_MAKE_EFFORT: &str = "high";

/// py: miniapp.py:84 `PRICES`
///
/// $ per million tokens: eve_pricing::ANTHROPIC_RATES, grounded on the Claude pricing page (2026-09-24):
/// claude-opus-5-5 input 4, output 20, cache reads 0.20, a 5-minute cache write 5, a 1-hour one 8. For the optional
/// daily cap a model not listed costs as the most expensive listed one, so an unknown model is never under-counted
/// there; the daily spend meter (eve_spend) records it unpriced instead of guessing.
pub static PRICES: LazyLock<Value> = LazyLock::new(|| {
    let mut out = Obj::new();
    for (name, r) in eve_pricing::ANTHROPIC_RATES.iter() {
        out.insert(
            (*name).to_string(),
            json!({"in": r.input, "out": r.output, "cache_read": r.cache_read, "cache_write": r.cache_write_5m,
                   "cache_write_1h": r.cache_write_1h}),
        );
    }
    Value::Object(out)
});

/// py: miniapp.py:89 `_WORST`
pub(crate) static WORST: LazyLock<Value> = LazyLock::new(|| {
    json!({"in": 10.0, "out": 50.0, "cache_read": 1.0, "cache_write": 12.5, "cache_write_1h": 20.0})
});

/// py: miniapp.py:90 `INIT_DATA_MAX_AGE_SECS`
/// a Mini App left open all day still works; an old leaked string does not
pub const INIT_DATA_MAX_AGE_SECS: f64 = 86400.0;

/// py: miniapp.py:91 `MAX_BODY_BYTES`
pub const MAX_BODY_BYTES: usize = 2000000;

/// py: miniapp.py:92 `APP_ROUTE_RE`
/// Python's `$` (also before a final newline) is the `(?:\n?\z)`.
pub static APP_ROUTE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^/apps/([a-z0-9][a-z0-9-]{0,47})/(index\.html)?(?:\n?\z)"#).expect("APP_ROUTE_RE"));

/// py: miniapp.py:93 `API_APP_RE`
pub static API_APP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^/api/apps/([a-z0-9][a-z0-9-]{0,47})/(load|save|report|delete|rename|revert|restore|history)(?:\n?\z)"#)
        .expect("API_APP_RE")
});

/// py: miniapp.py:95 `APP_TOKEN_SECS`
/// an app left open all day still saves; as long as initData lives
pub const APP_TOKEN_SECS: f64 = 86400.0;

/// py: miniapp.py:96 `CLOSE_WAIT_SECS`
/// shutdown waits this long for open connections (a build's stream) to go
pub const CLOSE_WAIT_SECS: f64 = 2.0;

/// py: miniapp.py:97 `SPEND_ALERTS_USD`
pub const SPEND_ALERTS_USD: &[f64] = &[5.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0];

/// py: miniapp.py:102 `APP_CSP`
///
/// How an app page is served. The sandbox (no allow-same-origin) gives it an opaque origin: it cannot read the
/// home page's storage or call the API as the home page, and browser storage throws (eve.save is the storage).
/// It may run inline code and pinned jsdelivr libraries, and connect to its own load and save only. The same
/// sandbox is what app_check serves apps with, so the check sees what the phone sees.
pub const APP_CSP: &str = "{sandbox}; default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' https://telegram.org/js/telegram-web-app.js {origin}/eve.js https://cdn.jsdelivr.net/npm/; style-src 'unsafe-inline' https://cdn.jsdelivr.net/npm/; img-src data: blob: https://cdn.jsdelivr.net/npm/; font-src data: https://cdn.jsdelivr.net/npm/; media-src data: blob:; worker-src blob:; connect-src {origin}/api/apps/{slug}/ https://cdn.jsdelivr.net/npm/; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";

/// py: miniapp.py:109 `TUNNEL_URL_RE`
pub static TUNNEL_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https://[a-z0-9-]+\.trycloudflare\.com"#).expect("TUNNEL_URL_RE"));

/// py: miniapp.py:110 `TUNNEL_START_SECS`
pub const TUNNEL_START_SECS: f64 = 45.0;

/// py: miniapp.py:111 `TUNNEL_RETRY_SECS`
pub const TUNNEL_RETRY_SECS: f64 = 10.0;

/// py: miniapp.py:112 `PAGE_PATH`: the home page, embedded so both daemons serve the same bytes.
pub const PAGE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/miniapp_page.html");

/// The home page's bytes (`PAGE_PATH.read_bytes()`).
pub static PAGE: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/miniapp_page.html"));

/// py: miniapp.py:115 `IRO_PATH`
///
/// iro.js 5.5.2 (MPL-2.0, github.com/jaames/iro.js), the Lights card's colour wheel. Vendored from the npm tarball
/// (sha512 matched the registry) and served from here, never a CDN: the home page holds the owner's initData.
pub const IRO_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/miniapp_iro.min.js");

/// The colour wheel's bytes (`IRO_PATH.read_bytes()`).
pub static IRO: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/miniapp_iro.min.js"));

/// py: miniapp.py:116 `CAP_LINE`
pub const CAP_LINE: &str = "Today's Claude budget is used up (${cap:.2f}). It resets at midnight.";

/// py: miniapp.py:117 `LEDGER_DAYS`
/// how many days of spend the ledger keeps
pub const LEDGER_DAYS: i64 = 400;

/// py: miniapp.py:118 `CORS`
/// an app page's load and save come from an opaque origin
pub const CORS: &[(&str, &str)] = &[("Access-Control-Allow-Origin", "*")];

/// py: miniapp.py:119 `PROGRESS_SECS`
/// a build's progress event at least this often (it also keeps the tunnel open)
pub const PROGRESS_SECS: f64 = 2.0;

// ---- Python's behaviors this module leans on ------------------------------------------------------------------

/// `str.strip()`: Python's whitespace, which also counts the separators U+001C..U+001F.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// Python `repr()` of a str.
fn py_str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
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
            c if (c as u32) < 0x20 || (0x7f..0xa1).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `repr()` of a JSON value as `json.loads` made it.
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_str_repr(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
        other => py_str(other),
    }
}

/// Python `str()` of a JSON value as `json.loads` made it.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(u)) => u.to_string(),
            _ => eve_agentcard::py::float_repr(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python truthiness.
fn truthy(v: Option<&Value>) -> bool {
    eve_agentcard::py::truthy(v)
}

/// `str(v or "")`.
fn str_or_empty(v: Option<&Value>) -> String {
    if truthy(v) {
        v.map(py_str).unwrap_or_default()
    } else {
        String::new()
    }
}

/// The first `n` characters (`s[:n]`).
fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python `int(str)` for the ASCII forms: whitespace around, a sign, `_` only between digits. A number too big
/// for i64 saturates (Python's int would hold it, and is only ever compared with bounds far below).
fn py_int_str(raw: &str) -> Option<i64> {
    let s = py_strip(raw);
    let (neg, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    if !digits.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    // Python's int has no ceiling: a number past i64 stays past every bound it is compared with
    let n: i64 = clean.parse().unwrap_or(i64::MAX);
    Some(if neg { -n } else { n })
}

/// Python `int(v)` of a JSON value (an int as it is, a float toward zero, a str parsed, a bool 0 or 1); None where
/// Python raises.
fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else {
                let f = n.as_f64()?;
                if f.is_finite() && f.abs() < 9.2e18 {
                    Some(f.trunc() as i64)
                } else {
                    None
                }
            }
        }
        Value::String(s) => py_int_str(s),
        _ => None,
    }
}

/// `int(v or 0)`: what a usage field counts (0 for nothing, and for a value Python could not count either).
fn count(v: Option<&Value>) -> i64 {
    match v {
        Some(v) if truthy(Some(v)) => py_int(v).unwrap_or(0),
        _ => 0,
    }
}

/// Python `float(v)` of a JSON value; None where Python raises.
fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => eve_agentcard::py::parse_float(s),
        _ => None,
    }
}

/// `round(x, n)`.
fn py_round(x: f64, digits: usize) -> f64 {
    eve_spend_sync::py_round(x, digits)
}

/// `json.dumps(obj)`: Python's separators, its float repr and `ensure_ascii`.
fn dumps(v: &Value) -> String {
    eve_agentcard::py::dumps(v, false, true)
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

/// `hmac.compare_digest` on two ASCII strings.
fn same_digest(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `urllib.parse.unquote_plus` (UTF-8, errors="replace").
fn unquote_plus(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A query string that `strict_parsing` refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BadQuery;

/// `urllib.parse.parse_qsl(qs, keep_blank_values=..., strict_parsing=...)` (separator "&").
fn parse_qsl(qs: &str, keep_blank_values: bool, strict_parsing: bool) -> Result<Vec<(String, String)>, BadQuery> {
    let mut out = Vec::new();
    if qs.is_empty() {
        return Ok(out);
    }
    for piece in qs.split('&') {
        if piece.is_empty() && !strict_parsing {
            continue;
        }
        let (name, value) = match piece.split_once('=') {
            Some((n, v)) => (n, v),
            None => {
                if strict_parsing {
                    return Err(BadQuery);
                }
                if keep_blank_values {
                    (piece, "")
                } else {
                    continue;
                }
            }
        };
        if !value.is_empty() || keep_blank_values {
            out.push((unquote_plus(name), unquote_plus(value)));
        }
    }
    Ok(out)
}

/// `dict(pairs)`: a later key's value wins, in the first key's place.
fn qs_dict(pairs: Vec<(String, String)>) -> IndexMap<String, String> {
    let mut out = IndexMap::new();
    for (k, v) in pairs {
        out.insert(k, v);
    }
    out
}

/// `Path.with_name(name)`.
fn with_name(path: &Path, name: &str) -> PathBuf {
    path.with_file_name(name)
}

/// The local date as `date.today().isoformat()`.
fn local_today() -> String {
    chrono::Local::now().date_naive().format("%Y-%m-%d").to_string()
}

/// A panic's message, as a log line names an exception.
fn panic_name(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".into()
    }
}

// ---- config -------------------------------------------------------------------------------------

/// py: miniapp.py:125 `class MiniAppConfig:`
#[derive(Clone)]
pub struct MiniAppConfig {
    pub enabled: bool,
    pub token: String,
    pub owner_ids: Vec<i64>,
    pub model: String,
    /// 0: no cap, spend is only tracked
    pub daily_usd: f64,
    pub make_effort: String,
    pub ledger_path: PathBuf,
}

impl Default for MiniAppConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token: String::new(),
            owner_ids: Vec::new(),
            model: DEFAULT_MODEL.to_string(),
            daily_usd: 0.0,
            make_effort: DEFAULT_MAKE_EFFORT.to_string(),
            ledger_path: home().join(".config").join("eve-bridge").join("miniapp-spend.json"),
        }
    }
}

impl MiniAppConfig {
    /// `user in cfg.owner_ids`.
    pub fn is_owner(&self, user: i64) -> bool {
        self.owner_ids.contains(&user)
    }
}

/// py: miniapp.py:135 `__repr__`: never the token.
impl fmt::Debug for MiniAppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let owners: HashSet<i64> = self.owner_ids.iter().copied().collect();
        write!(
            f,
            "MiniAppConfig(enabled={}, owners={}, model={}, make_effort={}, daily_usd={})",
            if self.enabled { "True" } else { "False" },
            owners.len(),
            py_str_repr(&self.model),
            py_str_repr(&self.make_effort),
            eve_agentcard::py::float_repr(self.daily_usd)
        )
    }
}

impl fmt::Display for MiniAppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// py: miniapp.py:140 `def configured(environ: Any = None) -> MiniAppConfig:`
pub fn configured(environ: Option<&Environ>) -> MiniAppConfig {
    let wanted = env_get(environ, "EVE_MINIAPP").filter(|v| !v.is_empty()).unwrap_or_else(|| "0".into());
    if !matches!(py_strip(&wanted).to_lowercase().as_str(), "1" | "true" | "yes" | "on") {
        return MiniAppConfig::default();
    }
    let tg = crate::telegram::configured(environ);
    let key = eve_openrouter::available(environ);
    if !(tg.enabled && key) {
        let missing: Vec<&str> = [("the Telegram door", tg.enabled), (eve_openrouter::KEY_ENV, key)]
            .iter()
            .filter(|(_, ok)| !ok)
            .map(|(n, _)| *n)
            .collect();
        tracing::warn!("miniapp: asked for (EVE_MINIAPP=1) but off: {} not set", missing.join(" and "));
        return MiniAppConfig::default();
    }
    let raw_cap = env_get(environ, "EVE_MINIAPP_DAILY_USD").filter(|v| !v.is_empty());
    let cap = match raw_cap {
        None => 0.0,
        Some(raw) => match eve_agentcard::py::parse_float(&raw) {
            Some(c) => c,
            None => {
                tracing::warn!("miniapp: EVE_MINIAPP_DAILY_USD is not a number; spend is tracked with no cap");
                0.0
            }
        },
    };
    let effort_raw =
        env_get(environ, "EVE_MINIAPP_MAKE_EFFORT").filter(|v| !v.is_empty()).unwrap_or_else(|| DEFAULT_MAKE_EFFORT.into());
    let mut make_effort = py_strip(&effort_raw).to_lowercase();
    if !EFFORTS.contains(&make_effort.as_str()) {
        make_effort = DEFAULT_MAKE_EFFORT.to_string();
    }
    let model_raw = env_get(environ, "EVE_MINIAPP_MODEL").filter(|v| !v.is_empty()).unwrap_or_else(|| DEFAULT_MODEL.into());
    MiniAppConfig {
        enabled: true,
        token: tg.token,
        owner_ids: tg.owner_ids,
        model: py_strip(&model_raw).to_string(),
        // max(0.0, cap): NaN is not above 0.0, so it is 0.0 too
        daily_usd: if cap > 0.0 { cap } else { 0.0 },
        make_effort,
        ..MiniAppConfig::default()
    }
}

// ---- who is asking: Telegram's signed initData ----------------------------------------------------

/// py: miniapp.py:170 `def phone_report(body: dict[str, Any]) -> str:`
///
/// One log line from an app's own report (eve.js): which Telegram it runs in, whether Telegram's native
/// bridge is there (without it the MainButton and BackButton do nothing), and the page's errors. The only way to
/// see what an app does on the real phone (owner, 2026-09-24: "the expense splitter doesn't work"). Every field
/// is cut short and stripped of line breaks: it is text the page sent.
pub fn phone_report(body: &Obj) -> String {
    let clip = |v: Option<&Value>, n: usize| -> String {
        let s = str_or_empty(v);
        // str.split(): Python's whitespace, the separators U+001C..U+001F included
        let words: Vec<&str> =
            s.split(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)).filter(|w| !w.is_empty()).collect();
        cut(&words.join(" "), n)
    };
    let errors: Vec<Value> = match body.get("errors") {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let parts = [
        format!("platform={}", clip(body.get("platform"), 16)),
        format!("version={}", clip(body.get("version"), 8)),
        format!("bridge={}", clip(body.get("bridge"), 12)),
        format!("main_button={}", clip(body.get("main_button"), 40)),
        format!("errors={}", errors.len()),
    ];
    let tail: String = errors.iter().take(5).map(|e| format!(" | {}", clip(Some(e), 240))).collect();
    parts.join(", ") + &tail
}

/// py: miniapp.py:185 `def check_init_data(init_data: str, token: str, owner_ids: frozenset[int], *, now: Optional[float] = None, max_age: float = INIT_DATA_MAX_AGE_SECS) -> Optional[int]:`
///
/// The owner's user id when ``init_data`` is signed by this bot, fresh, and from an owner; else None.
///
/// Telegram's scheme: secret = HMAC-SHA256(key="WebAppData", msg=bot token); the hash is
/// HMAC-SHA256(key=secret, msg=every other field as key=value, sorted by key, joined by newlines).
pub fn check_init_data(init_data: &str, token: &str, owner_ids: &[i64], now: Option<f64>, max_age: f64) -> Option<i64> {
    if init_data.is_empty() || token.is_empty() {
        return None;
    }
    let mut fields = qs_dict(parse_qsl(init_data, true, true).ok()?);
    let got = fields.shift_remove("hash").unwrap_or_default();
    if got.is_empty() {
        return None;
    }
    let mut keys: Vec<&String> = fields.keys().collect();
    keys.sort();
    let check = keys.iter().map(|k| format!("{k}={}", fields[*k])).collect::<Vec<_>>().join("\n");
    let secret = hmac_sha256(b"WebAppData", token.as_bytes());
    let want = hex::encode(hmac_sha256(&secret, check.as_bytes()));
    // compare_digest refuses a non-ASCII string outright; it is no signature either way
    if !got.is_ascii() || !same_digest(&want, &got) {
        return None;
    }
    let auth_date = py_int_str(fields.get("auth_date").map(String::as_str).unwrap_or("0"))?;
    let user: Value = serde_json::from_str(fields.get("user").map(String::as_str).unwrap_or("{}")).ok()?;
    let user_id = match user.as_object() {
        Some(o) => match o.get("id") {
            None => 0,
            Some(id) => py_int(id)?,
        },
        None => return None,
    };
    let clock = now.unwrap_or_else(wall);
    let auth = auth_date as f64;
    if auth_date == 0 || clock - auth > max_age || auth - clock > 300.0 {
        return None;
    }
    owner_ids.contains(&user_id).then_some(user_id)
}

/// py: miniapp.py:216 `def _app_key(token: str) -> bytes:`
pub(crate) fn app_key(token: &str) -> Vec<u8> {
    hmac_sha256(b"eve-app-token", token.as_bytes())
}

/// py: miniapp.py:220 `def app_token(slug: str, token: str, *, now: Optional[float] = None) -> str:`
///
/// The key to one app: ``<expiry>.<signature>``, signed with a key derived from the bot token and bound to
/// ``slug``. It opens that app's page and answers its load and save, nothing else, until it expires.
pub fn app_token(slug: &str, token: &str, now: Option<f64>) -> String {
    let exp = now.unwrap_or_else(wall).trunc() as i64 + APP_TOKEN_SECS as i64;
    let sig = hex::encode(hmac_sha256(&app_key(token), format!("{slug}.{exp}").as_bytes()));
    format!("{exp}.{}", &sig[..40])
}

/// py: miniapp.py:227 `def check_app_token(tok: str, slug: str, token: str, *, now: Optional[float] = None) -> bool:`
pub fn check_app_token(tok: &str, slug: &str, token: &str, now: Option<f64>) -> bool {
    let (exp, sig) = tok.split_once('.').unwrap_or((tok, ""));
    if token.is_empty() || exp.is_empty() || !exp.bytes().all(|b| b.is_ascii_digit()) || sig.chars().count() != 40 {
        return false;
    }
    let want = hex::encode(hmac_sha256(&app_key(token), format!("{slug}.{exp}").as_bytes()));
    let clock = now.unwrap_or_else(wall);
    let Ok(expiry) = exp.parse::<f64>() else { return false };
    sig.is_ascii() && same_digest(&want[..40], sig) && clock <= expiry && expiry <= clock + APP_TOKEN_SECS + 300.0
}

/// py: miniapp.py:236 `def app_csp(origin: str, slug: str) -> str:`
pub fn app_csp(origin: &str, slug: &str) -> String {
    let origin = origin.trim_end_matches('/');
    // str.format fills every field from the template in one pass: a value is never read as a field
    let mut out = String::with_capacity(APP_CSP.len() + 64);
    let mut rest = APP_CSP;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail.find('}').map(|e| e + 1).unwrap_or(tail.len());
        match &tail[..end] {
            "{sandbox}" => out.push_str(eve_app_check::SANDBOX),
            "{origin}" => out.push_str(origin),
            "{slug}" => out.push_str(slug),
            other => out.push_str(other),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

// ---- what it costs: a per-day ledger --------------------------------------------------------------

/// py: miniapp.py:244 `def cost_usd(model: str, usage: Any) -> float:`
pub fn cost_usd(model: &str, usage: &Value) -> f64 {
    let p = PRICES.get(model).unwrap_or(&WORST);
    let rate = |name: &str| p.get(name).and_then(Value::as_f64).unwrap_or(0.0);
    let n = |name: &str| count(usage.get(name)) as f64;
    // A cache write costs by how long it lives: 5 minutes at 1.25x input, 1 hour at 2x (the maker's prompt is
    // cached for an hour). usage.cache_creation splits the writes; without it every write is a 5-minute one.
    let split = usage.get("cache_creation").filter(|s| !s.is_null());
    let hour = match split {
        Some(s) => count(s.get("ephemeral_1h_input_tokens")),
        None => 0,
    } as f64;
    let writes = n("cache_creation_input_tokens");
    (n("input_tokens") * rate("in")
        + n("output_tokens") * rate("out")
        + n("cache_read_input_tokens") * rate("cache_read")
        + (writes - hour).max(0.0) * rate("cache_write")
        + hour * rate("cache_write_1h"))
        / 1_000_000.0
}

/// `on_cross(mark, total)`.
pub type OnCross = Arc<dyn Fn(f64, f64) + Send + Sync>;

/// py: miniapp.py:264 `class SpendLedger:`
///
/// Dollars spent per day (local date), kept in one small JSON file (``{"days": {"2026-09-24": 1.83}}``) so
/// a restart loses nothing. It only records: ``cap`` is None unless the owner set one (a cap above 0), and
/// ``over()`` is the only thing that can refuse work.
pub struct SpendLedger {
    pub path: PathBuf,
    pub(crate) today: StrFn,
    pub cap: Option<f64>,
    /// `on_cross(mark, total)`: a spend mark crossed today (MiniApp._spend_note); set after construction
    pub on_cross: Mutex<Option<OnCross>>,
    /// LEDGER_DAYS (a test keeps fewer)
    pub(crate) keep: i64,
    /// one add at a time: Python's single loop never ran two at once
    lock: Mutex<()>,
}

impl SpendLedger {
    /// py: miniapp.py:269 `def __init__(self, path: Path, cap_usd: Optional[float] = None, today: Callable[[], str] = lambda: _dt.date.today().isoformat()):`
    pub fn new(path: &Path, cap_usd: Option<f64>, today: Option<StrFn>) -> Self {
        Self {
            path: path.to_path_buf(),
            today: today.unwrap_or_else(|| Arc::new(local_today)),
            cap: cap_usd.filter(|c| *c > 0.0),
            on_cross: Mutex::new(None),
            keep: LEDGER_DAYS,
            lock: Mutex::new(()),
        }
    }

    /// The same ledger keeping only the newest `days` days (Python's tests patch LEDGER_DAYS).
    pub fn keep_days(mut self, days: i64) -> Self {
        self.keep = days;
        self
    }

    /// py: miniapp.py:276 `def days(self) -> dict[str, float]:`
    ///
    /// Every recorded day's total, by date. A file from the capped version (``{"date", "usd"}``) is read
    /// as its one day.
    pub fn days(&self) -> HashMap<String, f64> {
        self.days_in_order().into_iter().collect()
    }

    fn days_in_order(&self) -> IndexMap<String, f64> {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return IndexMap::new() };
        let Ok(Value::Object(mut data)) = serde_json::from_str::<Value>(&text) else { return IndexMap::new() };
        if !data.contains_key("days") && data.contains_key("date") {
            let day = py_str(&data["date"]);
            let usd = data.get("usd").cloned().unwrap_or(json!(0.0));
            let mut days = Obj::new();
            days.insert(day, usd);
            data = Obj::new();
            data.insert("days".into(), Value::Object(days));
        }
        let mut out = IndexMap::new();
        if let Some(Value::Object(raw)) = data.get("days") {
            for (day, usd) in raw {
                if let Some(f) = py_float(usd) {
                    out.insert(day.clone(), f);
                }
            }
        }
        out
    }

    /// py: miniapp.py:296 `def spent(self) -> float:`
    pub fn spent(&self) -> f64 {
        self.days_in_order().get(&(self.today)()).copied().unwrap_or(0.0)
    }

    /// py: miniapp.py:299 `def left(self) -> Optional[float]:`
    ///
    /// What the cap leaves today, or None when there is no cap.
    pub fn left(&self) -> Option<f64> {
        self.cap.map(|cap| (cap - self.spent()).max(0.0))
    }

    /// py: miniapp.py:303 `def over(self) -> bool:`
    pub fn over(&self) -> bool {
        self.cap.is_some_and(|cap| self.spent() >= cap)
    }

    /// py: miniapp.py:306 `def add(self, usd: float) -> float:`
    ///
    /// Record ``usd`` against today; today's new total.
    pub fn add(&self, usd: f64) -> f64 {
        let (crossed, total) = {
            let _one = self.lock.lock();
            let mut days = self.days_in_order();
            let today = (self.today)();
            let before = days.get(&today).copied().unwrap_or(0.0);
            // max(0.0, usd): NaN is not above 0.0
            let add = if usd > 0.0 { usd } else { 0.0 };
            let total = py_round(before + add, 6);
            days.insert(today.clone(), total);
            let mut sorted: Vec<(String, f64)> = days.into_iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            let keep_from = sorted.len().saturating_sub(self.keep.max(0) as usize);
            let keep = &sorted[keep_from..];
            let mut body = Obj::new();
            for (day, v) in keep {
                body.insert(day.clone(), serde_json::Number::from_f64(*v).map(Value::Number).unwrap_or(Value::Null));
            }
            let text = dumps(&json!({"days": body}));
            if let Err(e) = self.write(&text) {
                tracing::warn!("miniapp: the spend ledger was not written ({e})");
            }
            let total = keep.iter().find(|(d, _)| *d == today).map(|(_, v)| *v).unwrap_or(total);
            let crossed: Vec<f64> = SPEND_ALERTS_USD.iter().copied().filter(|m| before < *m && *m <= total).collect();
            (crossed, total)
        };
        let on_cross = self.on_cross.lock().clone();
        if let (Some(mark), Some(on_cross)) = (crossed.last(), on_cross) {
            let mark = *mark;
            if let Err(p) = std::panic::catch_unwind(AssertUnwindSafe(|| on_cross(mark, total))) {
                // a note that fails never loses the record
                tracing::warn!("miniapp: the spend note failed ({})", panic_name(p.as_ref()));
            }
        }
        total
    }

    fn write(&self, text: &str) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)
    }
}

/// py: miniapp.py:326 `def _error_line(e: Exception) -> str:`
///
/// The Claude API's errors as the anthropic SDK raised them: a refused key (401), a rate limit (429), any other
/// status, and a request that never got an answer.
pub(crate) fn error_line(e: &anyhow::Error) -> String {
    for cause in e.chain() {
        if let Some(err) = cause.downcast_ref::<eve_openrouter::Error>() {
            return match err {
                eve_openrouter::Error::Status { status: 401, .. } => {
                    "The OpenRouter key was refused. Check OPENROUTER_API_KEY on the Mac.".into()
                }
                eve_openrouter::Error::Status { status: 429, .. } => {
                    "Claude is rate-limited right now. Try again in a minute.".into()
                }
                eve_openrouter::Error::Status { status, .. } => format!("Claude returned an error ({status})."),
                eve_openrouter::Error::Http(_) => "Claude could not be reached. Check the Mac's internet connection.".into(),
                _ => "Something went wrong answering that.".into(),
            };
        }
        if let Some(err) = cause.downcast_ref::<reqwest::Error>() {
            return match err.status().map(|s| s.as_u16()) {
                Some(401) => "The OpenRouter key was refused. Check OPENROUTER_API_KEY on the Mac.".into(),
                Some(429) => "Claude is rate-limited right now. Try again in a minute.".into(),
                Some(status) => format!("Claude returned an error ({status})."),
                None => "Claude could not be reached. Check the Mac's internet connection.".into(),
            };
        }
        if cause.downcast_ref::<HttpError>().is_some() {
            return "Claude could not be reached. Check the Mac's internet connection.".into();
        }
    }
    "Something went wrong answering that.".into()
}

// ---- what the store and the maker answer --------------------------------------------------------------------
//
// AppStore raises KeyError (no such app), ValueError (an empty name, too much data) and FileExistsError (a restore
// under a taken address) in Python. These read either form the store's port gives them: the value itself, or a
// Result whose error names which it was.

/// Which of the store's errors a call raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoreFail {
    /// KeyError
    Missing,
    /// ValueError
    Invalid,
    /// FileExistsError
    Exists,
    /// anything else (Python let it propagate)
    Other(String),
}

fn classify(text: &str) -> StoreFail {
    // An error enum's Debug names its variant first (`Missing("slug")`): that decides before any word in it does.
    let variant: String = text.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    match variant.as_str() {
        "Missing" | "NotFound" | "NoSuchApp" | "KeyError" | "Gone" => return StoreFail::Missing,
        "Invalid" | "ValueError" | "TooLarge" | "TooBig" | "Empty" => return StoreFail::Invalid,
        "Exists" | "FileExistsError" | "Taken" => return StoreFail::Exists,
        "Io" | "Os" | "OSError" | "Disk" => return StoreFail::Other(text.to_string()),
        _ => {}
    }
    let t = text.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| t.contains(w));
    if any(&["exist"]) {
        StoreFail::Exists
    } else if any(&["keyerror"]) {
        StoreFail::Missing
    } else if any(&["valueerror", "value", "invalid", "too large", "toolarge", "too big", "too much", "empty", "needs a name"]) {
        StoreFail::Invalid
    } else if any(&["key", "missing", "notfound", "not found", "no such", "gone", "nothing", "no earlier", "unknown"]) {
        StoreFail::Missing
    } else {
        StoreFail::Other(text.to_string())
    }
}

/// A store call's answer, or which error it raised.
pub(crate) trait StoreAnswer<T> {
    fn answer(self) -> Result<T, StoreFail>;
}

macro_rules! plain_answer {
    ($($t:ty),*) => {
        $(impl StoreAnswer<$t> for $t {
            fn answer(self) -> Result<$t, StoreFail> {
                Ok(self)
            }
        })*
    };
}

plain_answer!(AppInfo, (), Value, i64, Vec<Obj>);

impl<T, E: fmt::Debug + fmt::Display> StoreAnswer<T> for Result<T, E> {
    fn answer(self) -> Result<T, StoreFail> {
        self.map_err(|e| classify(&format!("{e:?} {e}")))
    }
}

/// A build's answer, or the exception it raised.
pub(crate) trait MadeAnswer {
    fn made(self) -> anyhow::Result<Made>;
}

impl MadeAnswer for Made {
    fn made(self) -> anyhow::Result<Made> {
        Ok(self)
    }
}

impl<E: Into<anyhow::Error>> MadeAnswer for Result<Made, E> {
    fn made(self) -> anyhow::Result<Made> {
        self.map_err(Into::into)
    }
}

/// `apps_maker.DEFAULT_ROOT`: `~/.config/eve-bridge/apps`.
pub fn default_apps_root() -> PathBuf {
    home().join(".config").join("eve-bridge").join("apps")
}

// ---- the HTTP server (127.0.0.1; the tunnel is its only public door) ------------------------------

/// A connection's read half, buffered (Python's `asyncio.StreamReader`).
pub type ConnRead = Box<dyn AsyncBufRead + Unpin + Send>;
/// A connection's write half (Python's `asyncio.StreamWriter`).
pub type ConnWrite = Box<dyn AsyncWrite + Unpin + Send>;

/// `asyncio.StreamReader`'s default limit: a request head longer than this is refused.
const HEAD_LIMIT: usize = 64 * 1024;

/// py: miniapp.py:344 `class MiniAppServer:`
pub struct MiniAppServer {
    pub cfg: MiniAppConfig,
    pub ledger: Arc<SpendLedger>,
    pub(crate) page: Option<Vec<u8>>,
    /// apps_maker.AppStore / AppMaker; None: no apps
    pub store: Option<Arc<AppStore>>,
    /// apps_maker.AppStore / AppMaker; None: no apps
    pub maker: Option<Arc<AppMaker>>,
    /// one build at a time per owner
    pub building: Mutex<HashSet<i64>>,
    /// The apps being changed right now, by slug. MiniApp shares this set with the chat door
    /// (apps_maker.ChatMaker.building), so neither door deletes, renames or undoes an app under a build.
    pub changing: Arc<Mutex<HashSet<String>>>,
    /// The build running per owner, for a page opened again mid-build (/api/apps "building"), and where a
    /// build's result goes when the page that started it went away (a locked phone): the chat.
    pub builds: Mutex<HashMap<i64, Obj>>,
    pub notify: Option<BuildNotify>,
    /// the tunnel's address: the origin app pages are bound to
    pub public_url: Mutex<String>,
    /// phone_call.PhoneCalls: "Call eve", or None
    pub calls: Mutex<Option<Arc<eve_phone_call::PhoneCalls>>>,
    /// stick_link.LinkToken: the Eve Link app's, or None
    pub stick: Mutex<Option<Arc<eve_stick_link::LinkToken>>>,
    /// link_chat.LinkChat: Eve Link's chat, or None
    pub link: Mutex<Option<Arc<eve_link::link_chat::LinkChat>>>,
    /// lights.Lights: the Lights card, or None (no lights)
    pub lights: Mutex<Option<Arc<dyn eve_link::seams::Hub>>>,
    /// spotify.Spotify: the Music card, or None (no login)
    pub spotify: Mutex<Option<Arc<dyn eve_link::seams::Hub>>>,
    /// firetv.FireTV: the TV remote card, or None (no TV)
    pub firetv: Mutex<Option<Arc<dyn eve_link::seams::Hub>>>,
    /// pika.Pika: the car, or None (EVE_PIKA off, no pika.json)
    pub pika: Mutex<Option<Arc<dyn PanelCard>>>,
    /// obsbot_panel.ObsbotPanel: the webcam, or None (no OBSBOT Center)
    pub obsbot: Mutex<Option<Arc<dyn PanelCard>>>,
    /// the other cards Eve Link asks for by name (`getattr(server, name)`: "memory", "notes", an app's slot)
    pub cards: Mutex<HashMap<String, Arc<dyn eve_link::seams::Hub>>>,
    pub(crate) conns: Mutex<Vec<Job>>,
    /// the listening server's accept loop
    pub(crate) server: Mutex<Option<Job>>,
    pub port: Mutex<i64>,
}

/// Takes a build off the books however `_make` ends (Python's `finally`), a cancelled connection included.
struct BuildGuard {
    server: Arc<MiniAppServer>,
    user: i64,
    slug: String,
}

impl Drop for BuildGuard {
    fn drop(&mut self) {
        self.server.building.lock().remove(&self.user);
        self.server.builds.lock().remove(&self.user);
        self.server.changing.lock().remove(&self.slug);
    }
}

impl MiniAppServer {
    /// py: miniapp.py:345 `def __init__(self, cfg: MiniAppConfig, ledger: SpendLedger, page: Optional[bytes] = None, store: Any = None, maker: Any = None, changing: Optional[set[str]] = None, notify: Optional[Callable[[int, dict[str, Any]], Awaitable[Any]]] = None) -> None:`
    pub fn new(
        cfg: &MiniAppConfig,
        ledger: Arc<SpendLedger>,
        page: Option<&[u8]>,
        store: Option<Arc<AppStore>>,
        maker: Option<Arc<AppMaker>>,
        changing: Option<Arc<Mutex<HashSet<String>>>>,
        notify: Option<BuildNotify>,
    ) -> Arc<Self> {
        Arc::new(Self {
            cfg: cfg.clone(),
            ledger,
            page: page.map(<[u8]>::to_vec),
            store,
            maker,
            building: Mutex::new(HashSet::new()),
            changing: changing.unwrap_or_default(),
            builds: Mutex::new(HashMap::new()),
            notify,
            public_url: Mutex::new(String::new()),
            calls: Mutex::new(None),
            stick: Mutex::new(None),
            link: Mutex::new(None),
            lights: Mutex::new(None),
            spotify: Mutex::new(None),
            firetv: Mutex::new(None),
            pika: Mutex::new(None),
            obsbot: Mutex::new(None),
            cards: Mutex::new(HashMap::new()),
            conns: Mutex::new(Vec::new()),
            server: Mutex::new(None),
            port: Mutex::new(0),
        })
    }

    /// py: miniapp.py:373 `def page(self) -> bytes:`
    pub fn page(self: &Arc<Self>) -> Vec<u8> {
        self.page.clone().unwrap_or_else(|| PAGE.to_vec())
    }

    /// py: miniapp.py:378 `def spend(self) -> dict[str, Any]:`
    ///
    /// What the page shows in its header: ALL of eve's spend today (the meter: chat, voice, search,
    /// builds, everything), the app builder's own share, and its cap (None: none).
    pub fn spend(self: &Arc<Self>) -> Obj {
        self.spend_now()
    }

    fn spend_now(&self) -> Obj {
        let mut out = Obj::new();
        out.insert("spent_today".into(), num(py_round(eve_spend::today_total(None), 4)));
        out.insert("apps_today".into(), num(py_round(self.ledger.spent(), 4)));
        out.insert("cap".into(), self.ledger.cap.map(num).unwrap_or(Value::Null));
        out
    }

    /// py: miniapp.py:384 `def spending(self) -> dict[str, Any]:`
    ///
    /// The Spending view: eve's own meter (today split by provider, feature and model, yesterday, the last
    /// 30 days, this month) and each provider's own figure where one can be read (spend_sync). Aggregates
    /// only: no prompt, no answer, no key ever leaves in it.
    pub fn spending(self: &Arc<Self>) -> Obj {
        let mut out = match serde_json::to_value(eve_spend::summary(None, 30, None)) {
            Ok(Value::Object(o)) => o,
            _ => Obj::new(),
        };
        out.insert("providers".into(), Value::Object(eve_spend_sync::reported(None, None, None)));
        out.insert("cap".into(), self.ledger.cap.map(num).unwrap_or(Value::Null));
        out.insert("apps_today".into(), num(py_round(self.ledger.spent(), 4)));
        out
    }

    /// py: miniapp.py:393 `def open_url(self, slug: str) -> str:`
    ///
    /// An app's page with a fresh app token, relative to the home page.
    pub fn open_url(self: &Arc<Self>, slug: &str) -> String {
        self.open_url_now(slug)
    }

    fn open_url_now(&self, slug: &str) -> String {
        format!("apps/{slug}/?t={}", app_token(slug, &self.cfg.token, None))
    }

    /// py: miniapp.py:397 `def app_link(self, slug: str, headers: dict[str, str]) -> str:`
    ///
    /// An app's page with a fresh app token at this server's public origin (the tunnel's): what Eve Link opens
    /// (link_chat, ``POST /api/link/apps/<slug>/open``). The token is the one ``check_app_token`` takes.
    pub fn app_link(self: &Arc<Self>, slug: &str, headers: &HashMap<String, String>) -> String {
        self.app_link_now(slug, headers)
    }

    fn app_link_now(&self, slug: &str, headers: &HashMap<String, String>) -> String {
        format!("{}/{}", self.origin_now(headers), self.open_url_now(slug))
    }

    /// py: miniapp.py:402 `async def start(self, port: int = 0) -> int:`
    ///
    /// Listen on 127.0.0.1:`port` (0: any free port) and serve; the port it listens on. An error is the bind
    /// failing (Python's `start_server` raising).
    pub async fn start(self: &Arc<Self>, port: i64) -> anyhow::Result<i64> {
        let port = u16::try_from(port).map_err(|_| anyhow::anyhow!("port must be 0-65535"))?;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        let bound = i64::from(listener.local_addr()?.port());
        *self.port.lock() = bound;
        let me = Arc::downgrade(self);
        let accept = Job::spawn("miniapp-server", async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::warn!("miniapp: accept failed ({e})");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let Some(server) = me.upgrade() else { return };
                let conn = {
                    let server = server.clone();
                    Job::spawn("miniapp-conn", async move {
                        let (read, write) = stream.into_split();
                        let reader: ConnRead = Box::new(tokio::io::BufReader::new(read));
                        let writer: ConnWrite = Box::new(write);
                        server.handle(reader, writer).await;
                    })
                };
                let mut conns = server.conns.lock();
                conns.retain(|j| !j.done());
                conns.push(conn);
            }
        });
        *self.server.lock() = Some(accept);
        Ok(bound)
    }

    /// py: miniapp.py:407 `async def close(self) -> None:`
    ///
    /// Stop serving. Open connections are cut, and the wait for them is bounded: a build's stream stays
    /// open for minutes, and a daemon restart must not wait for it until launchd kills it before the pinned
    /// button says offline.
    pub async fn close(self: &Arc<Self>) {
        let pika = self.pika.lock().clone();
        if let Some(pika) = pika {
            // the car stops and its slot is let go
            let _ = AssertUnwindSafe(pika.close()).catch_unwind().await;
        }
        let obsbot = self.obsbot.lock().clone();
        if let Some(obsbot) = obsbot {
            // the gimbal stops turning and the picture ends
            let _ = AssertUnwindSafe(obsbot.close()).catch_unwind().await;
        }
        let link = self.link.lock().clone();
        if let Some(link) = link {
            // its streams end with [DONE], not a cut: they write it before their connections are cancelled
            link.close();
            let _ = AssertUnwindSafe(link.settle(eve_link::link_chat::CLOSE_SETTLE_SECS)).catch_unwind().await;
        }
        let accept = self.server.lock().take();
        if let Some(accept) = accept {
            accept.cancel();
            let conns: Vec<Job> = std::mem::take(&mut *self.conns.lock());
            for task in &conns {
                task.cancel();
            }
            let all = async {
                accept.join().await;
                for task in &conns {
                    task.join().await;
                }
            };
            let _ = tokio::time::timeout(Duration::from_secs_f64(CLOSE_WAIT_SECS), all).await;
        }
    }

    /// py: miniapp.py:428 `async def _handle(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:`
    ///
    /// One connection: served, then closed. A connection that breaks, a head that never ends, or a request
    /// Python's parsing raised on is closed without an answer.
    pub(crate) async fn handle(self: &Arc<Self>, mut reader: ConnRead, mut writer: ConnWrite) {
        self.serve(&mut reader, &mut writer).await;
        let _ = writer.shutdown().await;
    }

    /// py: miniapp.py:442 `def _origin(self, headers: dict[str, str]) -> str:`
    ///
    /// This server's public origin: the tunnel's, else (local use, tests) what the Host header names.
    pub(crate) fn origin(self: &Arc<Self>, headers: &HashMap<String, String>) -> String {
        self.origin_now(headers)
    }

    fn origin_now(&self, headers: &HashMap<String, String>) -> String {
        static HOST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9.-]+(:\d{1,5})?$").expect("HOST"));
        let public = self.public_url.lock().clone();
        if !public.is_empty() {
            return public.trim_end_matches('/').to_string();
        }
        let host = headers.get("host").map(String::as_str).unwrap_or("");
        if HOST.is_match(host) {
            format!("http://{host}")
        } else {
            "http://127.0.0.1".into()
        }
    }

    /// py: miniapp.py:449 `async def _serve(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:`
    pub(crate) async fn serve(self: &Arc<Self>, reader: &mut ConnRead, writer: &mut ConnWrite) {
        let head = match tokio::time::timeout(Duration::from_secs(15), read_head(reader)).await {
            Ok(Ok(head)) => head,
            _ => return,
        };
        // latin-1: one character per byte
        let text: String = head.iter().map(|b| char::from(*b)).collect();
        let lines: Vec<&str> = text.split("\r\n").collect();
        let mut first = lines[0].splitn(3, ' ');
        let method = first.next().unwrap_or("").to_string();
        let target = first.next().unwrap_or("").to_string();
        let mut headers: HashMap<String, String> = HashMap::new();
        for ln in lines[1..].iter().filter(|ln| !ln.is_empty()) {
            let (k, v) = ln.split_once(':').unwrap_or((ln, ""));
            headers.insert(py_strip(k).to_lowercase(), py_strip(v).to_string());
        }
        let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
        let (path, query) = (path.to_string(), query.to_string());
        let get = method == "GET";
        if get && (path == "/" || path == "/index.html") {
            self.send(writer, 200, &self.page(), "text/html; charset=utf-8", None).await;
            return;
        }
        if get && path == "/iro.js" {
            self.send(writer, 200, IRO, "application/javascript; charset=utf-8", None).await;
            return;
        }
        if get && path == "/healthz" {
            self.send(writer, 200, b"ok", "text/plain", None).await;
            return;
        }
        if get && path == "/eve.js" && self.store.is_some() {
            self.send(writer, 200, crate::apps_maker::EVE_JS.as_bytes(), "application/javascript; charset=utf-8", None)
                .await;
            return;
        }
        if get && path == "/stick" {
            // Eve Link's pairing page (stick_link): static, the token stays in the fragment.
            let extra: IndexMap<String, String> =
                eve_stick_link::PAIR_HEADERS.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
            self.send(writer, 200, eve_stick_link::PAIR_PAGE.as_bytes(), "text/html; charset=utf-8", Some(&extra))
                .await;
            return;
        }
        if get && path == "/api/stick" {
            // The Eve Link app's call (stick_link): the same call as "Call eve", proven by the link token.
            // Browsers send an Origin on every WebSocket and the app sends none: a page cannot use this door.
            let calls = self.calls.lock().clone();
            let stick = self.stick.lock().clone();
            match (calls, stick) {
                (Some(calls), Some(stick)) if eve_phone_call::is_upgrade(&headers) => {
                    if headers.get("origin").is_some_and(|o| !o.is_empty()) {
                        self.send(writer, 403, b"the Eve Link app only", "text/plain", None).await;
                    } else {
                        let check: eve_phone_call::TokenCheck = Arc::new(move |t: &str| stick.check(t));
                        let (r, w) = take_conn(reader, writer);
                        if let Err(e) = calls.serve(r, w, &headers, Some(check)).await {
                            tracing::warn!("miniapp: the Eve Link call ended on an error ({e})");
                        }
                    }
                }
                _ => self.send(writer, 404, b"not found", "text/plain", None).await,
            }
            return;
        }
        if path.starts_with("/api/link/") {
            // Eve Link's chat (link_chat): the app's native code only, proven by the link token as a Bearer;
            // any Origin is refused there, as on /api/stick. It answers its own errors (401, 403, 503).
            let link = self.link.lock().clone();
            match link {
                None => self.send(writer, 404, b"not found", "text/plain", None).await,
                Some(link) => {
                    let stick = self.stick.lock().clone();
                    let check = stick.map(|s| move |t: &str| s.check(t));
                    let check_ref: Option<&(dyn Fn(&str) -> bool + Send + Sync)> =
                        check.as_ref().map(|c| c as &(dyn Fn(&str) -> bool + Send + Sync));
                    let apps: Arc<dyn eve_link::seams::MiniApp> = self.clone();
                    if let Err(e) = link.serve(&method, &path, &query, &headers, reader, writer, check_ref, Some(apps)).await
                    {
                        tracing::debug!("miniapp: an Eve Link request broke off ({e})");
                    }
                }
            }
            return;
        }
        if get && path == "/api/call" {
            // "Call eve" (phone_call): a WebSocket, from the home page only; the owner is checked on it.
            let origin = headers.get("origin").cloned().unwrap_or_default();
            let calls = self.calls.lock().clone();
            match calls {
                Some(calls) if eve_phone_call::is_upgrade(&headers) => {
                    let public = !self.public_url.lock().is_empty();
                    if origin.is_empty() || origin == "null" || (public && origin != self.origin_now(&headers)) {
                        self.send(writer, 403, b"only eve's home page can call", "text/plain", None).await;
                    } else {
                        let (r, w) = take_conn(reader, writer);
                        if let Err(e) = calls.serve(r, w, &headers, None).await {
                            tracing::warn!("miniapp: the call ended on an error ({e})");
                        }
                    }
                }
                _ => self.send(writer, 404, b"not found", "text/plain", None).await,
            }
            return;
        }
        if get && self.store.is_some() {
            if let Some(route) = APP_ROUTE_RE.captures(&path) {
                let slug = route[1].to_string();
                self.app_page(writer, &slug, &query, &headers).await;
                return;
            }
        }
        let api_app = API_APP_RE.captures(&path).map(|c| (c[1].to_string(), c[2].to_string()));
        let app_io = api_app.as_ref().is_some_and(|(_, a)| matches!(a.as_str(), "load" | "save" | "report"));
        if method == "OPTIONS" && app_io {
            // A sandboxed app's eve.js sends a CORS-simple request (no preflight), but answer one anyway.
            let mut extra = cors();
            extra.insert("Access-Control-Allow-Methods".into(), "POST".into());
            extra.insert("Access-Control-Allow-Headers".into(), "content-type".into());
            extra.insert("Access-Control-Max-Age".into(), "600".into());
            self.send(writer, 204, b"", "text/plain", Some(&extra)).await;
            return;
        }
        let known = matches!(
            path.as_str(),
            "/api/me" | "/api/spend" | "/api/apps" | "/api/make" | "/api/lights" | "/api/spotify" | "/api/firetv"
        ) || api_app.is_some();
        if method != "POST" || !known {
            self.send(writer, 404, b"not found", "text/plain", None).await;
            return;
        }
        let size = match headers.get("content-length").filter(|v| !v.is_empty()) {
            None => 0,
            // int() raising here closed the connection without an answer
            Some(raw) => match py_int_str(raw) {
                Some(n) => n,
                None => return,
            },
        };
        if size <= 0 || size > MAX_BODY_BYTES as i64 {
            self.send(writer, 413, b"too large", "text/plain", None).await;
            return;
        }
        let mut raw = vec![0u8; size as usize];
        match tokio::time::timeout(Duration::from_secs(30), reader.read_exact(&mut raw)).await {
            Err(_) => {
                self.send(writer, 400, b"bad request", "text/plain", None).await;
                return;
            }
            // the body ended early: the connection is closed without an answer
            Ok(Err(_)) => return,
            Ok(Ok(_)) => {}
        }
        let body = match eve_agentcard::py::json_loads_bytes(&raw) {
            Some(Value::Object(body)) => body,
            _ => {
                self.send(writer, 400, b"bad request", "text/plain", None).await;
                return;
            }
        };
        if body.contains_key("t") {
            // An app's own load or save, with the app token its page was opened with; that token opens nothing else.
            let tok = py_str(&body["t"]);
            match &api_app {
                Some((slug, action))
                    if app_io && self.store.is_some() && check_app_token(&tok, slug, &self.cfg.token, None) =>
                {
                    let (status, obj) = self.app_io(slug, action, &body);
                    self.json(writer, status, &obj, Some(&cors())).await;
                }
                _ => {
                    let obj = obj_of(json!({"error": "This app can only reach its own data."}));
                    self.json(writer, 403, &obj, Some(&cors())).await;
                }
            }
            return;
        }
        let origin = headers.get("origin").cloned().unwrap_or_default();
        let public = !self.public_url.lock().is_empty();
        if origin == "null" || (!origin.is_empty() && public && origin != self.origin_now(&headers)) {
            // A sandboxed app page (Origin: null) or another site: only the home page may call the owner API.
            self.json(writer, 403, &obj_of(json!({"error": "Only eve's home page can do that."})), None).await;
            return;
        }
        let init_data = str_or_empty(body.get("initData"));
        let Some(user) = check_init_data(&init_data, &self.cfg.token, &self.cfg.owner_ids, None, INIT_DATA_MAX_AGE_SECS)
        else {
            self.json(writer, 403, &obj_of(json!({"error": "Only eve's owner can use this."})), None).await;
            return;
        };
        let body_value = Value::Object(body.clone());
        match path.as_str() {
            "/api/me" => {
                let lights = self.lights.lock().clone();
                let mut out = Obj::new();
                out.insert("model".into(), json!(self.cfg.model));
                out.insert("apps".into(), json!(self.store.is_some()));
                out.extend(self.spend_now());
                out.insert("lights".into(), Value::Array(lights.map(|l| l.listing()).unwrap_or_default()));
                out.insert("spotify".into(), json!(self.spotify.lock().is_some()));
                out.insert("firetv".into(), json!(self.firetv.lock().is_some()));
                self.json(writer, 200, &out, None).await;
            }
            "/api/lights" | "/api/spotify" | "/api/firetv" => {
                // The Lights, Music and TV remote cards: owner only, like everything above; an app's token never
                // reaches here.
                let (hub, line) = match path.as_str() {
                    "/api/lights" => (self.lights.lock().clone(), "eve has no lights on this computer."),
                    "/api/spotify" => (self.spotify.lock().clone(), "eve has no Spotify login on this computer."),
                    _ => (self.firetv.lock().clone(), "eve has no TV on this computer."),
                };
                let Some(hub) = hub else {
                    self.json(writer, 404, &obj_of(json!({"ok": false, "line": line})), None).await;
                    return;
                };
                match hub.panel(&body_value).await {
                    Ok(answer) => self.json_value(writer, 200, &answer, None).await,
                    // Python's exception ended the connection without an answer
                    Err(e) => tracing::error!("miniapp: {path} failed ({e})"),
                }
            }
            "/api/spend" => {
                // ~30 small file reads
                let me = self.clone();
                let out = tokio::task::spawn_blocking(move || me.spending()).await.unwrap_or_default();
                self.json(writer, 200, &out, None).await;
            }
            _ => {
                let api_value = match &api_app {
                    Some((slug, action)) => json!([slug, action]),
                    None => Value::Null,
                };
                self.apps_api(writer, &path, &api_value, &body, user).await;
            }
        }
    }

    /// py: miniapp.py:588 `async def _app_page(self, writer: asyncio.StreamWriter, slug: str, query: str, headers: dict[str, str]) -> None:`
    ///
    /// One app's page, for its app token only, served sandboxed (APP_CSP). Without a valid token (an Open
    /// button from before tokens, a link someone saw) the answer is the same for every slug, so it says
    /// nothing about which apps exist: the home page, which opens the app when Telegram signed the visit.
    pub(crate) async fn app_page(self: &Arc<Self>, writer: &mut ConnWrite, slug: &str, query: &str, headers: &HashMap<String, String>) {
        let tok = qs_dict(parse_qsl(query, false, false).unwrap_or_default()).get("t").cloned().unwrap_or_default();
        let html = match &self.store {
            Some(store) if check_app_token(&tok, slug, &self.cfg.token, None) => store.html(slug),
            _ => None,
        };
        let Some(html) = html else {
            tracing::info!("apps: {slug} asked for without a valid app token; sent to the home page");
            let mut extra = IndexMap::new();
            extra.insert("Location".to_string(), format!("/?open={slug}"));
            self.send(writer, 302, b"", "text/plain", Some(&extra)).await;
            return;
        };
        tracing::info!("apps: {slug} opened");
        let mut extra = IndexMap::new();
        extra.insert("Content-Security-Policy".to_string(), app_csp(&self.origin_now(headers), slug));
        extra.insert("Referrer-Policy".to_string(), "no-referrer".to_string());
        extra.insert("X-Content-Type-Options".to_string(), "nosniff".to_string());
        let page = crate::apps_maker::serve_app_html(&html);
        self.send(writer, 200, page.as_bytes(), "text/html; charset=utf-8", Some(&extra)).await;
    }

    /// py: miniapp.py:605 `def _building_now(self, user: int) -> list[dict[str, Any]]:`
    ///
    /// The builds running now: this owner's own from the page, and apps the chat door is changing.
    pub(crate) fn building_now(self: &Arc<Self>, user: i64) -> Vec<Obj> {
        self.building_now_for(user)
    }

    fn building_now_for(&self, user: i64) -> Vec<Obj> {
        let mut out = Vec::new();
        let mine = self.builds.lock().get(&user).cloned();
        if let Some(mine) = &mine {
            let started = mine.get("started").and_then(Value::as_f64).unwrap_or_else(wall);
            let mut row = Obj::new();
            for key in ["app", "request", "stage"] {
                row.insert(key.into(), mine.get(key).cloned().unwrap_or(Value::Null));
            }
            row.insert("secs".into(), json!((wall() - started).round_ties_even() as i64));
            out.push(row);
        }
        let mine_app = mine.as_ref().and_then(|m| m.get("app")).and_then(Value::as_str).map(str::to_string);
        let mut slugs: Vec<String> = self.changing.lock().iter().cloned().collect();
        slugs.sort();
        for slug in slugs {
            let other = mine_app.as_deref() != Some(slug.as_str()) || mine.is_none();
            if other && self.store.as_ref().is_some_and(|s| s.info(&slug).is_some()) {
                out.push(obj_of(json!({"app": slug, "request": "", "stage": "", "secs": null})));
            }
        }
        out
    }

    /// py: miniapp.py:617 `async def _apps_api(self, writer: asyncio.StreamWriter, path: str, api_app: Any, body: dict[str, Any], user: int) -> None:`
    ///
    /// `api_app` is the matched `/api/apps/<slug>/<action>` as `[slug, action]`, or null.
    pub(crate) async fn apps_api(self: &Arc<Self>, writer: &mut ConnWrite, path: &str, api_app: &Value, body: &Obj, user: i64) {
        let Some(store) = self.store.clone() else {
            self.json(writer, 404, &obj_of(json!({"error": "Apps are off."})), None).await;
            return;
        };
        let api = api_app.as_array().and_then(|a| Some((a.first()?.as_str()?.to_string(), a.get(1)?.as_str()?.to_string())));
        if path == "/api/apps" {
            let apps: Vec<Value> = store
                .list()
                .iter()
                .map(|a| {
                    let mut row = a.as_dict();
                    row.insert("open".into(), json!(self.open_url_now(&a.slug)));
                    Value::Object(row)
                })
                .collect();
            let building: Vec<Value> = self.building_now_for(user).into_iter().map(Value::Object).collect();
            self.json(writer, 200, &obj_of(json!({"apps": apps, "building": building})), None).await;
            return;
        }
        if let Some((slug, action)) = &api {
            let (status, obj) = if action == "history" { self.history(slug, user) } else { self.app_action(slug, action, body) };
            self.json(writer, status, &obj, None).await;
            return;
        }
        // /api/make: {request} makes a new app; {app: slug, request} changes that one. Progress as SSE.
        let request_text = cut(py_strip(&str_or_empty(body.get("request"))), 4000);
        if request_text.is_empty() || self.maker.is_none() {
            self.json(writer, 400, &obj_of(json!({"error": "Say what the app should do."})), None).await;
            return;
        }
        let slug = str_or_empty(body.get("app"));
        if !slug.is_empty() && store.info(&slug).is_none() {
            self.json(writer, 404, &obj_of(json!({"error": "There is no such app.", "gone": true})), None).await;
            return;
        }
        if self.ledger.over() {
            let line = CAP_LINE.replace("{cap:.2f}", &format!("{:.2}", self.ledger.cap.unwrap_or(0.0)));
            self.json(writer, 429, &obj_of(json!({"error": line})), None).await;
            return;
        }
        let refused = {
            let mut building = self.building.lock();
            if building.contains(&user) {
                Some(409_i64)
            } else {
                let mut changing = self.changing.lock();
                if !slug.is_empty() && changing.contains(&slug) {
                    Some(410)
                } else {
                    building.insert(user);
                    self.builds.lock().insert(
                        user,
                        obj_of(json!({"app": slug, "request": cut(&request_text, 80), "started": wall(),
                                      "stage": "thinking"})),
                    );
                    if !slug.is_empty() {
                        changing.insert(slug.clone());
                    }
                    None
                }
            }
        };
        match refused {
            Some(409) => {
                let building: Vec<Value> = self.building_now_for(user).into_iter().map(Value::Object).collect();
                let obj = obj_of(json!({"error": "Still working on the last one.", "building": building}));
                self.json(writer, 409, &obj, None).await;
                return;
            }
            Some(_) => {
                self.json(writer, 409, &obj_of(json!({"error": "That app is being changed right now."})), None).await;
                return;
            }
            None => {}
        }
        let _books = BuildGuard { server: self.clone(), user, slug: slug.clone() };
        self.make(writer, &request_text, &slug, user).await;
    }

    /// py: miniapp.py:663 `def _history(self, slug: str, user: int) -> tuple[int, dict[str, Any]]:`
    ///
    /// One app's change chat: the app (with a fresh Open address), what was asked of it and how each build
    /// went (AppStore.thread: when, the request, version, the check's line, the journeys, what may still be
    /// wrong, cost), and the change running on it now, if any (``stage`` and ``secs`` only for this owner's
    /// own build from the page; a change from the chat door has neither).
    pub(crate) fn history(self: &Arc<Self>, slug: &str, user: i64) -> (i64, Obj) {
        let gone = || (404, obj_of(json!({"error": "There is no such app.", "gone": true})));
        let Some(store) = &self.store else { return gone() };
        let Some(info) = store.info(slug) else { return gone() };
        let thread: Vec<Obj> = match store.thread(slug).answer() {
            Ok(t) => t,
            Err(StoreFail::Other(e)) => {
                tracing::warn!("apps: {slug}'s change chat could not be read ({e})");
                return gone();
            }
            Err(_) => return gone(),
        };
        let running = self
            .building_now_for(user)
            .into_iter()
            .find(|b| b.get("app").and_then(Value::as_str) == Some(slug))
            .map(Value::Object)
            .unwrap_or(Value::Null);
        let mut app = info.as_dict();
        app.insert("open".into(), json!(self.open_url_now(slug)));
        let mut out = Obj::new();
        out.insert("app".into(), Value::Object(app));
        out.insert("thread".into(), Value::Array(thread.into_iter().map(Value::Object).collect()));
        out.insert("building".into(), running);
        out.extend(self.spend_now());
        (200, out)
    }

    /// py: miniapp.py:680 `def _app_io(self, slug: str, action: str, body: dict[str, Any]) -> tuple[int, dict[str, Any]]:`
    pub(crate) fn app_io(self: &Arc<Self>, slug: &str, action: &str, body: &Obj) -> (i64, Obj) {
        let missing = || (404, obj_of(json!({"error": "There is no such app."})));
        let Some(store) = &self.store else { return missing() };
        if action == "report" {
            tracing::info!("apps: {slug} on the phone: {}", phone_report(body));
            return (200, obj_of(json!({"ok": true})));
        }
        let failed = |fail: StoreFail| match fail {
            StoreFail::Invalid => (413, obj_of(json!({"error": "That is too much data for one app (1 MB)."}))),
            StoreFail::Other(e) => {
                tracing::warn!("apps: {slug}'s data could not be reached ({e})");
                missing()
            }
            _ => missing(),
        };
        if action == "load" {
            return match store.load_data(slug).answer() {
                Ok(data) => {
                    tracing::info!("apps: {slug} loaded ({})", if data.is_null() { "no data yet" } else { "has data" });
                    (200, obj_of(json!({"data": data})))
                }
                Err(fail) => failed(fail),
            };
        }
        let data = body.get("data").cloned().unwrap_or(Value::Null);
        match store.save_data(slug, &data).answer() {
            Ok(size) => {
                tracing::info!("apps: {slug} saved {size} bytes");
                (200, obj_of(json!({"ok": true, "bytes": size})))
            }
            Err(fail) => failed(fail),
        }
    }

    /// py: miniapp.py:698 `def _app_action(self, slug: str, action: str, body: dict[str, Any]) -> tuple[int, dict[str, Any]]:`
    ///
    /// One app's load, save, delete, rename, revert or restore. Delete, rename and revert wait while the app
    /// is being changed: the build would otherwise save over a rename or bring a deleted app back.
    pub(crate) fn app_action(self: &Arc<Self>, slug: &str, action: &str, body: &Obj) -> (i64, Obj) {
        let missing = || (404, obj_of(json!({"error": "There is no such app."})));
        if matches!(action, "load" | "save" | "report") {
            return self.app_io(slug, action, body);
        }
        if !matches!(action, "delete" | "rename" | "revert" | "restore") {
            // Every action has its own branch: one without (an owner-signed report did) once fell through to
            // Undo below and reverted the app.
            return (400, obj_of(json!({"error": "That isn't something an app can do."})));
        }
        if matches!(action, "delete" | "rename" | "revert") && self.changing.lock().contains(slug) {
            return (409, obj_of(json!({"error": "That app is being changed right now. Try again when it is done."})));
        }
        let Some(store) = &self.store else { return missing() };
        let other = |what: &str, e: String| {
            tracing::warn!("apps: {slug} could not be {what} ({e})");
            missing()
        };
        match action {
            "delete" => {
                let info = store.info(slug);
                match store.delete(slug).answer() {
                    Ok(()) => {}
                    Err(StoreFail::Other(e)) => return other("deleted", e),
                    Err(_) => return missing(),
                }
                tracing::info!("apps: {slug} moved to the trash from the Mini App");
                let deleted = info.map(|i| i.title).unwrap_or_else(|| slug.to_string());
                (200, obj_of(json!({"ok": true, "deleted": deleted})))
            }
            "restore" => match store.restore(slug).answer() {
                Ok(info) => {
                    tracing::info!("apps: {slug} restored from the trash from the Mini App");
                    (200, obj_of(json!({"ok": true, "app": info.as_dict()})))
                }
                Err(StoreFail::Exists) => (
                    409,
                    obj_of(json!({"error": "A newer app has that address now, so this one can't come back under it. \
                                             Rename or delete the newer one first."})),
                ),
                Err(StoreFail::Other(e)) => other("restored", e),
                Err(_) => missing(),
            },
            "rename" => {
                let title = str_or_empty(body.get("title"));
                match store.rename(slug, &title).answer() {
                    Ok(info) => (200, obj_of(json!({"ok": true, "app": info.as_dict()}))),
                    Err(StoreFail::Invalid) => (400, obj_of(json!({"error": "An app needs a name."}))),
                    Err(StoreFail::Other(e)) => other("renamed", e),
                    Err(_) => missing(),
                }
            }
            _ => {
                let Some(now) = store.info(slug) else { return missing() };
                // Undo is a pop, so a second tap sent before the first answered must not pop a second version: the
                // page says how many versions it saw, and a count that no longer matches is refused.
                if let Some(Value::Number(seen)) = body.get("versions") {
                    let differs = match (seen.as_i64(), seen.as_u64()) {
                        (Some(i), _) => Some(i != now.versions),
                        (None, Some(_)) => Some(true),
                        _ => None,
                    };
                    if differs == Some(true) {
                        return (409, obj_of(json!({"error": "That was already undone.", "app": now.as_dict()})));
                    }
                }
                match store.revert(slug).answer() {
                    Ok(info) => {
                        tracing::info!("apps: {slug} undone from the Mini App");
                        (200, obj_of(json!({"ok": true, "app": info.as_dict()})))
                    }
                    Err(StoreFail::Other(e)) => other("undone", e),
                    Err(_) => (409, obj_of(json!({"error": "There is no earlier version to go back to."}))),
                }
            }
        }
    }

    /// py: miniapp.py:747 `async def _make(self, writer: asyncio.StreamWriter, request_text: str, slug: str, user: int = 0) -> None:`
    ///
    /// Build or change one app, streaming ``{"s": stage, "p": n}`` (apps_maker's progress: thinking, writing
    /// with the characters so far, testing, fixing with the number of problems, saving) at least every
    /// PROGRESS_SECS and at once on a new stage, then one done or error event. A phone that goes away does not
    /// stop the build: it finishes and saves, and its result goes to the chat instead (``notify``).
    pub(crate) async fn make(self: &Arc<Self>, writer: &mut ConnWrite, request_text: &str, slug: &str, user: i64) {
        let Some(maker) = self.maker.clone() else { return };
        let head = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\n\
                     Cache-Control: no-store\r\nX-Accel-Buffering: no\r\nConnection: close\r\n\r\n";
        if writer.write_all(head).await.is_err() || writer.flush().await.is_err() {
            return;
        }
        let state = Arc::new(Mutex::new(("thinking".to_string(), 0_i64)));
        let wake = Arc::new(Flag::new());
        let done = AtomicBool::new(false);
        // the page stopped listening (locked phone, closed app)
        let mut gone = false;

        let on_progress: crate::apps_maker::MakerProgress = {
            let state = state.clone();
            let wake = wake.clone();
            let me = Arc::downgrade(self);
            Arc::new(move |stage: &str, n: i64| {
                let new_stage = {
                    let mut s = state.lock();
                    let new_stage = s.0 != stage;
                    *s = (stage.to_string(), n);
                    new_stage
                };
                if let Some(me) = me.upgrade() {
                    if let Some(build) = me.builds.lock().get_mut(&user) {
                        build.insert("stage".into(), json!(stage));
                    }
                }
                if new_stage {
                    wake.set();
                }
            })
        };

        let build = async {
            let run = async {
                if slug.is_empty() {
                    maker.make(request_text, Some(on_progress.clone())).await.made()
                } else {
                    maker.edit(slug, request_text, Some(on_progress.clone())).await.made()
                }
            };
            let out = match AssertUnwindSafe(run).catch_unwind().await {
                Ok(result) => result,
                Err(p) => Err(anyhow::anyhow!("{}", panic_name(p.as_ref()))),
            };
            done.store(true, Ordering::SeqCst);
            wake.set();
            out
        };
        let ticker = async {
            let mut broke = false;
            while !done.load(Ordering::SeqCst) {
                wake.clear();
                let (s, p) = state.lock().clone();
                let line = format!("data: {}\n\n", dumps(&json!({"s": s, "p": p})));
                if writer.write_all(line.as_bytes()).await.is_err() || writer.flush().await.is_err() {
                    broke = true;
                    break;
                }
                let _ = tokio::time::timeout(Duration::from_secs_f64(PROGRESS_SECS), wake.wait()).await;
            }
            broke
        };
        let (made, broke) = tokio::join!(build, ticker);
        gone |= broke;
        let mut reason = String::new();
        let made = match made {
            // what the build raised (Made.error: Python's make and edit raised it), told in plain words
            Ok(made) if made.error.is_some() => {
                let e = made.error.as_ref().expect("checked");
                tracing::warn!("apps: making failed ({})", error_kind(e));
                reason = error_line(e);
                None
            }
            Ok(made) => Some(made),
            Err(e) => {
                tracing::warn!("apps: making failed ({})", error_kind(&e));
                reason = error_line(&e);
                None
            }
        };
        let mut out = Obj::new();
        match &made {
            Some(made) if made.ok && made.app.is_some() => {
                let app = made.app.as_ref().expect("checked");
                let tested = made.check.as_ref().is_some_and(|c| c.skipped.is_empty());
                let check = made.check.as_ref().map(|c| c.summary()).unwrap_or_else(|| "not tested".into());
                tracing::info!("apps: {} phone check {check}", app.slug);
                let first: Vec<String> = made.issues.iter().take(2).cloned().collect();
                out.insert("done".into(), json!(true));
                out.insert("app".into(), Value::Object(app.as_dict()));
                out.insert("url".into(), json!(self.open_url_now(&app.slug)));
                out.insert("check".into(), json!(check));
                out.insert("tested".into(), json!(tested));
                out.insert("rounds".into(), json!(made.rounds));
                out.insert("problems".into(), json!(made.issues.len()));
                out.insert("issues".into(), json!(crate::apps_maker::owner_issues(made.check.as_ref(), &first, 300)));
                out.insert("changed".into(), json!(!slug.is_empty()));
                out.insert("usd".into(), num(py_round(made.usd, 4)));
                out.extend(self.spend_now());
            }
            _ => {
                let error = match &made {
                    Some(made) => {
                        format!("I couldn't {} it: {}.", if slug.is_empty() { "build" } else { "change" }, made.reason)
                    }
                    None => reason,
                };
                out.insert("error".into(), json!(error));
                out.insert("changed".into(), json!(!slug.is_empty()));
                out.extend(self.spend_now());
                if !slug.is_empty() {
                    // a change that failed still leads to its change chat
                    out.insert("slug".into(), json!(slug));
                }
            }
        }
        if !gone {
            let line = format!("data: {}\n\n", dumps(&Value::Object(out.clone())));
            if writer.write_all(line.as_bytes()).await.is_ok() && writer.flush().await.is_ok() {
                return;
            }
        }
        if let (Some(notify), true) = (&self.notify, user != 0) {
            let _ = AssertUnwindSafe(notify(user, out)).catch_unwind().await;
        }
    }

    /// py: miniapp.py:822 `async def _send(self, writer: asyncio.StreamWriter, status: int, body: bytes, ctype: str, extra: Optional[dict[str, str]] = None) -> None:`
    ///
    /// `extra` is the extra headers in order. A connection that broke is let go: it is closed after.
    pub(crate) async fn send(self: &Arc<Self>, writer: &mut ConnWrite, status: i64, body: &[u8], ctype: &str, extra: Option<&IndexMap<String, String>>) {
        let reason = match status {
            200 => "OK",
            204 => "No Content",
            302 => "Found",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            409 => "Conflict",
            413 => "Payload Too Large",
            429 => "Too Many Requests",
            _ => "OK",
        };
        let more: String = extra.map(|e| e.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect()).unwrap_or_default();
        let mut out = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{more}Connection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        if writer.write_all(&out).await.is_ok() {
            let _ = writer.flush().await;
        }
    }

    /// py: miniapp.py:832 `async def _json(self, writer: asyncio.StreamWriter, status: int, obj: dict[str, Any], extra: Optional[dict[str, str]] = None) -> None:`
    pub(crate) async fn json(self: &Arc<Self>, writer: &mut ConnWrite, status: i64, obj: &Obj, extra: Option<&IndexMap<String, String>>) {
        self.json_value(writer, status, &Value::Object(obj.clone()), extra).await;
    }

    async fn json_value(self: &Arc<Self>, writer: &mut ConnWrite, status: i64, obj: &Value, extra: Option<&IndexMap<String, String>>) {
        self.send(writer, status, dumps(obj).as_bytes(), "application/json", extra).await;
    }
}

/// `CORS` as the headers a reply carries.
fn cors() -> IndexMap<String, String> {
    CORS.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// A JSON number (null for NaN or an infinity, which JSON has no room for).
fn num(x: f64) -> Value {
    serde_json::Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null)
}

fn obj_of(v: Value) -> Obj {
    match v {
        Value::Object(o) => o,
        _ => Obj::new(),
    }
}

/// The exception's class name, as the log lines name it.
fn error_kind(e: &anyhow::Error) -> String {
    for cause in e.chain() {
        if let Some(err) = cause.downcast_ref::<eve_openrouter::Error>() {
            return match err {
                // the anthropic SDK's class for each status (_client.py `_make_status_error`)
                eve_openrouter::Error::Status { status, .. } => match *status {
                    400 => "BadRequestError",
                    401 => "AuthenticationError",
                    403 => "PermissionDeniedError",
                    404 => "NotFoundError",
                    409 => "ConflictError",
                    413 => "RequestTooLargeError",
                    422 => "UnprocessableEntityError",
                    429 => "RateLimitError",
                    529 => "OverloadedError",
                    s if s >= 500 => "InternalServerError",
                    _ => "APIStatusError",
                }
                .into(),
                eve_openrouter::Error::Http(e) if e.is_timeout() => "APITimeoutError".into(),
                eve_openrouter::Error::Http(_) => "APIConnectionError".into(),
                eve_openrouter::Error::NoKey(_) => "NoKey".into(),
                _ => "Error".into(),
            };
        }
        if let Some(err) = cause.downcast_ref::<HttpError>() {
            return err.kind.clone();
        }
        if cause.downcast_ref::<std::io::Error>().is_some() {
            return "OSError".into();
        }
    }
    "Exception".into()
}

/// Hand the connection's two halves to a door that keeps them (a WebSocket call); what is left behind is closed.
fn take_conn(reader: &mut ConnRead, writer: &mut ConnWrite) -> (ConnRead, ConnWrite) {
    let r = std::mem::replace(reader, Box::new(tokio::io::empty()));
    let w = std::mem::replace(writer, Box::new(tokio::io::sink()));
    (r, w)
}

/// `reader.readuntil(b"\r\n\r\n")`: the request head with its blank line, within `HEAD_LIMIT`.
async fn read_head(reader: &mut ConnRead) -> std::io::Result<Vec<u8>> {
    let mut head = Vec::new();
    loop {
        let room = HEAD_LIMIT.saturating_sub(head.len());
        if room == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "the request head is too long"));
        }
        let mut limited = (&mut *reader).take(room as u64);
        let n = limited.read_until(b'\n', &mut head).await?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the request head ended early"));
        }
        if head.ends_with(b"\r\n\r\n") {
            return Ok(head[..head.len() - 4].to_vec());
        }
    }
}

/// The Mini App server as Eve Link's door reads it (`getattr(server, name)`, `server.store`, `server.app_link`).
impl eve_link::seams::MiniApp for MiniAppServer {
    fn hub(&self, name: &str) -> Option<Arc<dyn eve_link::seams::Hub>> {
        match name {
            "lights" => self.lights.lock().clone(),
            "spotify" => self.spotify.lock().clone(),
            "firetv" => self.firetv.lock().clone(),
            "pika" => self.pika.lock().clone().map(|p| p as Arc<dyn eve_link::seams::Hub>),
            "obsbot" => self.obsbot.lock().clone().map(|p| p as Arc<dyn eve_link::seams::Hub>),
            other => self.cards.lock().get(other).cloned(),
        }
    }
    fn app_store(&self) -> Option<Arc<dyn eve_link::seams::AppStoreView>> {
        self.store.clone().map(|s| s as Arc<dyn eve_link::seams::AppStoreView>)
    }
    fn app_link(&self, slug: &str, headers: &eve_link::seams::Headers) -> String {
        self.app_link_now(slug, headers)
    }
}

// ---- the public door: a Cloudflare quick tunnel -----------------------------------------------------

/// py: miniapp.py:839 `def cloudflared_bin() -> Optional[str]:`
pub fn cloudflared_bin() -> Option<String> {
    if let Some(v) = std::env::var("EVE_CLOUDFLARED").ok().filter(|v| !v.is_empty()) {
        return Some(v);
    }
    if let Ok(p) = which::which("cloudflared") {
        return Some(p.to_string_lossy().into_owned());
    }
    ["/opt/homebrew/bin/cloudflared", "/usr/local/bin/cloudflared"]
        .iter()
        .find(|p| Path::new(p).exists())
        .map(|p| (*p).to_string())
}

/// py: miniapp.py:845 `class QuickTunnel:`
///
/// ``cloudflared tunnel --url http://127.0.0.1:<port>``: prints its trycloudflare.com address on stderr.
pub struct QuickTunnel {
    pub binary: String,
    /// the running cloudflared's process id (Python's `proc.pid`)
    pub(crate) pid: Mutex<Option<u32>>,
    /// its exit code once it exits (Python's `proc.returncode`; a signal is its negative)
    pub(crate) exit: Mutex<Option<tokio::sync::watch::Receiver<Option<i64>>>>,
    pub url: Mutex<String>,
    pub(crate) drain: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl QuickTunnel {
    /// py: miniapp.py:848 `def __init__(self, binary: str) -> None:`
    pub fn new(binary: &str) -> Arc<Self> {
        Arc::new(Self {
            binary: binary.to_string(),
            pid: Mutex::new(None),
            exit: Mutex::new(None),
            url: Mutex::new(String::new()),
            drain: Mutex::new(None),
        })
    }

    /// py: miniapp.py:854 `async def start(self, port: int, timeout: float = TUNNEL_START_SECS) -> str:`
    ///
    /// The tunnel's address; an error when cloudflared did not start or gave none in `timeout` seconds.
    pub async fn start(&self, port: i64, timeout: f64) -> anyhow::Result<String> {
        let mut child = tokio::process::Command::new(&self.binary)
            .args(["tunnel", "--no-autoupdate", "--url", &format!("http://127.0.0.1:{port}")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow::anyhow!("cloudflared has no stderr"))?;
        *self.pid.lock() = child.id();
        let (tx, rx) = tokio::sync::watch::channel(None);
        *self.exit.lock() = Some(rx);
        tokio::spawn(async move {
            let code = match child.wait().await {
                Ok(status) => exit_code(status),
                Err(_) => -1,
            };
            let _ = tx.send(Some(code));
        });
        let mut lines = tokio::io::BufReader::new(stderr);
        let deadline = monotonic() + timeout;
        while monotonic() < deadline {
            let mut line = Vec::new();
            let wait = Duration::from_secs_f64((deadline - monotonic()).max(0.1));
            match tokio::time::timeout(wait, lines.read_until(b'\n', &mut line)).await {
                Err(_) => break,
                Ok(Err(_)) | Ok(Ok(0)) => break,
                Ok(Ok(_)) => {}
            }
            if let Some(m) = TUNNEL_URL_RE.find(&String::from_utf8_lossy(&line)) {
                let url = m.as_str().to_string();
                *self.url.lock() = url.clone();
                let drain = tokio::spawn(async move {
                    // keep the pipe from filling; the lines are noise
                    let mut sink = Vec::new();
                    loop {
                        sink.clear();
                        match lines.read_until(b'\n', &mut sink).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                });
                *self.drain.lock() = Some(drain);
                return Ok(url);
            }
        }
        self.stop().await;
        Err(anyhow::anyhow!("cloudflared gave no address"))
    }

    /// py: miniapp.py:875 `async def _drain_stderr(self) -> None:`
    ///
    /// The drain runs from `start` (it owns the pipe); this waits for it to end (the pipe closed).
    pub(crate) async fn drain_stderr(&self) {
        let drain = self.drain.lock().take();
        if let Some(drain) = drain {
            let _ = drain.await;
        }
    }

    /// py: miniapp.py:880 `async def wait(self) -> int:`
    pub async fn wait(&self) -> i64 {
        let rx = self.exit.lock().clone();
        let Some(mut rx) = rx else { return -1 };
        let code = rx.wait_for(Option::is_some).await.map(|c| c.unwrap_or(-1)).unwrap_or(-1);
        code
    }

    /// py: miniapp.py:884 `async def stop(self) -> None:`
    pub async fn stop(&self) {
        if let Some(drain) = self.drain.lock().take() {
            drain.abort();
        }
        let rx = self.exit.lock().clone();
        let pid = *self.pid.lock();
        let (Some(mut rx), Some(pid)) = (rx, pid) else { return };
        if rx.borrow().is_some() {
            return;
        }
        signal(pid, Signal::Term);
        let exited = tokio::time::timeout(Duration::from_secs(5), rx.wait_for(Option::is_some)).await;
        if exited.is_err() {
            signal(pid, Signal::Kill);
        }
    }
}

#[async_trait::async_trait]
impl Tunnel for QuickTunnel {
    async fn start(&self, port: i64, timeout: f64) -> anyhow::Result<String> {
        QuickTunnel::start(self, port, timeout).await
    }
    async fn wait(&self) -> i64 {
        QuickTunnel::wait(self).await
    }
    async fn stop(&self) {
        QuickTunnel::stop(self).await
    }
}

enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal(pid: u32, which: Signal) {
    let sig = match which {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: kill(2) with a process id this tunnel started and has not seen exit.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

#[cfg(not(unix))]
fn signal(pid: u32, _which: Signal) {
    let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).status();
}

#[cfg(unix)]
fn exit_code(status: std::process::ExitStatus) -> i64 {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => i64::from(code),
        (None, Some(sig)) => -i64::from(sig),
        _ => -1,
    }
}

#[cfg(not(unix))]
fn exit_code(status: std::process::ExitStatus) -> i64 {
    status.code().map(i64::from).unwrap_or(-1)
}

// ---- the way in: a pinned Open button (the menu button stays the / commands) -------------------------
//
// Owner, 2026-09-24: "menu disappeared, all that's left is ask claude, i want both". Telegram's menu button is
// either the / command list or one Mini App, never both, so it stays the commands, and the Mini App is one tap
// away in a message pinned at the top of the chat (plus /apps, and the Open button a build sends). The tunnel's
// address changes every start, so the pinned message's button is edited to the new one each time.

/// py: miniapp.py:903 `PIN_TEXT`
pub const PIN_TEXT: &str = "eve: your apps.";

/// py: miniapp.py:904 `PIN_OFFLINE_TEXT`
pub const PIN_OFFLINE_TEXT: &str = "eve's apps are offline (the Mac is asleep or restarting). This button comes back by itself.";

/// py: miniapp.py:905 `OPEN_TEXT`
pub const OPEN_TEXT: &str = "Open eve";

/// A Bot API call that failed, by the name Python's log line gave it (never its URL: the URL is the token).
fn bot_error_kind(e: &anyhow::Error) -> String {
    for cause in e.chain() {
        if let Some(err) = cause.downcast_ref::<reqwest::Error>() {
            return if err.is_timeout() {
                "ReadTimeout".into()
            } else if err.is_connect() {
                "ConnectError".into()
            } else if err.is_decode() {
                "JSONDecodeError".into()
            } else {
                "HTTPError".into()
            };
        }
        if let Some(err) = cause.downcast_ref::<HttpError>() {
            return err.kind.clone();
        }
    }
    "Exception".into()
}

/// py: miniapp.py:908 `def bot_caller(token: str, environ: Any = None) -> Bot:`
///
/// One Bot API call as ``await bot(method, data)`` -> Telegram's JSON reply (``{"ok": ...}``). With
/// EVE_TELEGRAM=app (telegram's app-only mode) nothing reaches Telegram: every call is refused here.
pub fn bot_caller(token: &str, environ: Option<&Environ>) -> Bot {
    // the request URL is the token: never in a log line
    crate::telegram::hide_token(token);
    let mode = env_get(environ, "EVE_TELEGRAM").unwrap_or_default();
    if py_strip(&mode).to_lowercase() == "app" {
        return Arc::new(|_method: String, _data: Obj| {
            async { Ok(json!({"ok": false, "description": "Telegram is off (EVE_TELEGRAM=app)"})) }.boxed()
        });
    }
    let token = token.to_string();
    let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().unwrap_or_default();
    Arc::new(move |method: String, data: Obj| {
        let url = format!("https://api.telegram.org/bot{token}/{method}");
        let client = client.clone();
        async move {
            let reply = client.post(url).json(&Value::Object(data)).send().await?;
            let bytes = reply.bytes().await?;
            eve_agentcard::py::json_loads_bytes(&bytes).ok_or_else(|| anyhow::anyhow!("JSONDecodeError"))
        }
        .boxed()
    })
}

/// py: miniapp.py:930 `def open_button(url: str, stick_url: str = "") -> dict[str, Any]:`
///
/// The pinned message's buttons: Open, and once Eve Link is paired, its tap-to-update link (stick_link),
/// a plain URL button because it opens a page that hands off to the app, not a Mini App.
pub fn open_button(url: &str, stick_url: &str) -> Obj {
    let mut rows = vec![json!([{"text": OPEN_TEXT, "web_app": {"url": url}}])];
    if !stick_url.is_empty() {
        rows.push(json!([{"text": eve_stick_link::UPDATE_TEXT, "url": stick_url}]));
    }
    obj_of(json!({"inline_keyboard": rows}))
}

/// py: miniapp.py:941 `class Pins:`
///
/// The pinned Open message per owner chat: {chat id: message id} in a small JSON file.
pub struct Pins {
    pub path: PathBuf,
    pub(crate) bot: Bot,
}

impl Pins {
    /// py: miniapp.py:944 `def __init__(self, path: Path, bot: Bot) -> None:`
    pub fn new(path: &Path, bot: Bot) -> Self {
        Self { path: path.to_path_buf(), bot }
    }

    /// py: miniapp.py:947 `def _load(self) -> dict[str, int]:`
    pub(crate) fn load(&self) -> IndexMap<String, i64> {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return IndexMap::new() };
        let Ok(Value::Object(raw)) = serde_json::from_str::<Value>(&text) else { return IndexMap::new() };
        let mut out = IndexMap::new();
        for (k, v) in raw {
            match py_int(&v) {
                Some(mid) => {
                    out.insert(k, mid);
                }
                None => return IndexMap::new(),
            }
        }
        out
    }

    /// py: miniapp.py:953 `def _save(self, pins: dict[str, int]) -> None:`
    pub(crate) fn save(&self, pins: &IndexMap<String, i64>) {
        let body: Obj = pins.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        let write = || -> std::io::Result<()> {
            if let Some(parent) = self.path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::write(&self.path, dumps(&Value::Object(body)))
        };
        if let Err(e) = write() {
            tracing::warn!("miniapp: the pinned message's id was not kept ({e})");
        }
    }

    /// py: miniapp.py:957 `async def _ok(self, method: str, data: dict) -> Any:`
    ///
    /// Telegram's reply when the call went through (or there was nothing to change), else None.
    pub(crate) async fn ok(&self, method: &str, data: &Obj) -> Option<Value> {
        let reply = match AssertUnwindSafe((self.bot)(method.to_string(), data.clone())).catch_unwind().await {
            Ok(Ok(reply)) => reply,
            Ok(Err(e)) => {
                // a failed call is logged, never fatal
                tracing::warn!("miniapp: {method} failed ({})", bot_error_kind(&e));
                return None;
            }
            Err(p) => {
                tracing::warn!("miniapp: {method} failed ({})", panic_name(p.as_ref()));
                return None;
            }
        };
        if let Value::Object(o) = &reply {
            let description = o.get("description").map(py_str).unwrap_or_default();
            if truthy(o.get("ok")) || description.contains("not modified") {
                return Some(reply);
            }
        }
        let said = match &reply {
            Value::Object(o) => o.get("description").map(py_str).unwrap_or_else(|| "None".into()),
            other => py_str(other),
        };
        tracing::info!("miniapp: {method} refused ({})", cut(&said, 120));
        None
    }

    /// py: miniapp.py:969 `async def show(self, chat_id: int, url: str, stick_url: str = "") -> None:`
    ///
    /// The / menu on the menu button, and the pinned message's button pointing at ``url`` (and Eve Link's
    /// update button at ``stick_url``, when it is paired).
    pub async fn show(&self, chat_id: i64, url: &str, stick_url: &str) {
        self.ok("setChatMenuButton", &obj_of(json!({"chat_id": chat_id, "menu_button": {"type": "commands"}}))).await;
        let mut pins = self.load();
        let old = pins.get(&chat_id.to_string()).copied().unwrap_or(0);
        if old != 0 {
            let edit = json!({"chat_id": chat_id, "message_id": old, "text": PIN_TEXT,
                              "reply_markup": open_button(url, stick_url)});
            if self.ok("editMessageText", &obj_of(edit)).await.is_some_and(|r| truthy(Some(&r))) {
                return;
            }
        }
        let send = json!({"chat_id": chat_id, "text": PIN_TEXT, "disable_notification": true,
                          "reply_markup": open_button(url, stick_url)});
        let Some(sent) = self.ok("sendMessage", &obj_of(send)).await.filter(|r| truthy(Some(r))) else { return };
        let Some(mid) = sent.get("result").and_then(|r| r.get("message_id")).and_then(py_int) else {
            tracing::warn!("miniapp: sendMessage answered without a message id");
            return;
        };
        let pin = json!({"chat_id": chat_id, "message_id": mid, "disable_notification": true});
        self.ok("pinChatMessage", &obj_of(pin)).await;
        pins.insert(chat_id.to_string(), mid);
        self.save(&pins);
    }

    /// py: miniapp.py:987 `async def offline(self, chat_id: int) -> None:`
    ///
    /// The daemon is stopping: the pinned button would lead nowhere, so it says so instead.
    pub async fn offline(&self, chat_id: i64) {
        let old = self.load().get(&chat_id.to_string()).copied().unwrap_or(0);
        if old != 0 {
            let edit = json!({"chat_id": chat_id, "message_id": old, "text": PIN_OFFLINE_TEXT});
            self.ok("editMessageText", &obj_of(edit)).await;
        }
    }
}

// ---- the whole thing, for the daemon's life ---------------------------------------------------------

// py: miniapp.py:996 `_DEFAULT`: `Lent::FromEnv` (the phone check by default).

/// py: miniapp.py:999 `class MiniApp:`
///
/// The server, the tunnel and the pinned button, plus the app maker both doors share.
///
/// ``check`` is the phone check each build goes through (app_check.check_app; None turns it off) and
/// ``context`` the owner-context line each build prompt carries (apps_maker.owner_context: date, time zone,
/// units). ``changing`` is the set of app slugs being changed right now, shared with the chat door.
pub struct MiniApp {
    pub cfg: MiniAppConfig,
    pub ledger: Arc<SpendLedger>,
    pub store: Arc<AppStore>,
    pub maker: Arc<AppMaker>,
    pub changing: Arc<Mutex<HashSet<String>>>,
    pub server: Arc<MiniAppServer>,
    pub(crate) tunnel_factory: Option<TunnelFactory>,
    pub(crate) bot: Bot,
    pub pins: Pins,
    pub url: Mutex<String>,
    pub(crate) notes: Mutex<Vec<Job>>,
    /// `task.cancel()` on `run`
    pub(crate) stopping: Flag,
    pub(crate) me: Weak<MiniApp>,
}

/// What `run`'s `finally` does, done once: by `run` itself, or by a task of its own when `run` was dropped.
struct RunCleanup {
    app: Arc<MiniApp>,
    announcer: Option<Job>,
    tunnel: Arc<Mutex<Option<Arc<dyn Tunnel>>>>,
}

impl RunCleanup {
    fn take(&mut self) -> (Option<Job>, Option<Arc<dyn Tunnel>>) {
        (self.announcer.take(), self.tunnel.lock().take())
    }

    async fn finish(app: Arc<MiniApp>, announcer: Option<Job>, tunnel: Option<Arc<dyn Tunnel>>) {
        if let Some(announcer) = announcer {
            announcer.cancel();
        }
        if let Some(tunnel) = tunnel {
            tunnel.stop().await;
        }
        app.server.close().await;
        // the pinned button says offline, not a dead link
        let _ = AssertUnwindSafe(app.point_pins("")).catch_unwind().await;
    }
}

impl Drop for RunCleanup {
    fn drop(&mut self) {
        let (announcer, tunnel) = self.take();
        if announcer.is_none() && tunnel.is_none() {
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(RunCleanup::finish(self.app.clone(), announcer, tunnel));
        }
    }
}

impl MiniApp {
    /// py: miniapp.py:1006 `def __init__(self, cfg: MiniAppConfig, *, tunnel_factory: Optional[Callable[[], Any]] = None, bot: Optional[Bot] = None, pins_path: Optional[Path] = None, generate: Any = None, apps_root: Optional[Path] = None, check: Any = _DEFAULT, context: Optional[Callable[[], str]] = None) -> None:`
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: &MiniAppConfig,
        tunnel_factory: Option<TunnelFactory>,
        bot: Option<Bot>,
        pins_path: Option<&Path>,
        generate: Option<crate::apps_maker::Generate>,
        apps_root: Option<&Path>,
        check: Lent<crate::apps_maker::Check>,
        context: Option<StrFn>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me: &Weak<MiniApp>| {
            let ledger = Arc::new(SpendLedger::new(&cfg.ledger_path, Some(cfg.daily_usd), None));
            let root = apps_root.map(Path::to_path_buf).unwrap_or_else(default_apps_root);
            let store = Arc::new(AppStore::new(&root));
            let generate =
                generate.unwrap_or_else(|| crate::apps_maker::claude_generate(&cfg.model, &cfg.make_effort, None));
            let model = cfg.model.clone();
            let cost: crate::apps_maker::CostFn = Arc::new(move |usage: &Value| meter_build_for(&model, usage));
            let spend: crate::apps_maker::SpendFn = {
                let ledger = ledger.clone();
                Arc::new(move |usd: f64| ledger.add(usd))
            };
            let context = context.unwrap_or_else(|| Arc::new(|| crate::apps_maker::owner_context(None, None)));
            let check = match check {
                Lent::FromEnv => Some(phone_check()),
                Lent::Given(check) => check,
            };
            let maker = AppMaker::new(store.clone(), generate, cost, spend, Some(context), check, 2);
            let changing: Arc<Mutex<HashSet<String>>> = Arc::default();
            let notify: BuildNotify = {
                let me = me.clone();
                Arc::new(move |user: i64, out: Obj| {
                    let me = me.clone();
                    async move {
                        if let Some(app) = me.upgrade() {
                            app.build_result(user, &out).await;
                        }
                    }
                    .boxed()
                })
            };
            let server = MiniAppServer::new(
                cfg,
                ledger.clone(),
                None,
                Some(store.clone()),
                Some(maker.clone()),
                Some(changing.clone()),
                Some(notify),
            );
            let tunnel_factory = tunnel_factory.or_else(|| {
                cloudflared_bin().map(|binary| {
                    Arc::new(move || QuickTunnel::new(&binary) as Arc<dyn Tunnel>) as TunnelFactory
                })
            });
            let bot = bot.unwrap_or_else(|| bot_caller(&cfg.token, None));
            let pins_path = pins_path.map(Path::to_path_buf).unwrap_or_else(|| with_name(&cfg.ledger_path, "miniapp-pin.json"));
            let pins = Pins::new(&pins_path, bot.clone());
            *server.stick.lock() =
                Some(Arc::new(eve_stick_link::LinkToken::new(with_name(&cfg.ledger_path, "stick-link.json"))));
            let on_cross: OnCross = {
                let me = me.clone();
                Arc::new(move |mark: f64, total: f64| {
                    if let Some(app) = me.upgrade() {
                        app.spend_note(mark, total);
                    }
                })
            };
            *ledger.on_cross.lock() = Some(on_cross);
            MiniApp {
                cfg: cfg.clone(),
                ledger,
                store,
                maker,
                changing,
                server,
                tunnel_factory,
                bot,
                pins,
                url: Mutex::new(String::new()),
                notes: Mutex::new(Vec::new()),
                stopping: Flag::new(),
                me: me.clone(),
            }
        })
    }

    /// py: miniapp.py:1035 `def _meter_build(self, usage: Any) -> float:`
    ///
    /// One build round's cost: into the daily spend meter (eve_spend, feature "app builder", unpriced for a
    /// model eve_pricing does not know), and returned at the cap's never-under-counted price for this ledger.
    pub(crate) fn meter_build(self: &Arc<Self>, usage: &Value) -> f64 {
        meter_build_for(&self.cfg.model, usage)
    }

    /// py: miniapp.py:1041 `def app_url(self, slug: str) -> str:`
    ///
    /// An Open button's address for one app right now ("" while the tunnel is down). It goes through the
    /// home page, which gets Telegram's signed initData and opens the app with an app token: the app never
    /// sees the initData, which answers for the whole API.
    pub fn app_url(self: &Arc<Self>, slug: &str) -> String {
        self.app_url_now(slug)
    }

    fn app_url_now(&self, slug: &str) -> String {
        let url = self.url.lock().clone();
        if url.is_empty() {
            String::new()
        } else {
            format!("{url}/?open={slug}")
        }
    }

    /// py: miniapp.py:1047 `def chat_url(self, slug: str) -> str:`
    ///
    /// A Change button's address: the app's change chat on the home page ("" while the tunnel is down).
    pub fn chat_url(self: &Arc<Self>, slug: &str) -> String {
        self.chat_url_now(slug)
    }

    fn chat_url_now(&self, slug: &str) -> String {
        let url = self.url.lock().clone();
        if url.is_empty() {
            String::new()
        } else {
            format!("{url}/?chat={slug}")
        }
    }

    /// py: miniapp.py:1051 `async def _say(self, text: str, buttons: Optional[list[tuple[str, str]]] = None) -> None:`
    pub(crate) async fn say(self: &Arc<Self>, text: &str, buttons: Option<&[(String, String)]>) {
        let mut owners = self.cfg.owner_ids.clone();
        owners.sort();
        owners.dedup();
        // a private chat's id is the owner's user id
        for chat in owners {
            let mut data = obj_of(json!({"chat_id": chat, "text": text}));
            if let Some(buttons) = buttons.filter(|b| !b.is_empty()) {
                // one per row, as telegram.send_web_apps lays them out
                let rows: Vec<Value> =
                    buttons.iter().map(|(label, url)| json!([{"text": label, "web_app": {"url": url}}])).collect();
                data.insert("reply_markup".into(), json!({"inline_keyboard": rows}));
            }
            self.pins.ok("sendMessage", &data).await;
        }
    }

    /// py: miniapp.py:1059 `async def _build_result(self, user: int, out: dict[str, Any]) -> None:`
    ///
    /// A build from the page finished after the page went away (a locked phone): its result comes to the
    /// chat, as a build from the chat does, so it is never lost.
    pub async fn build_result(self: &Arc<Self>, user: i64, out: &Obj) {
        let changed = truthy(out.get("changed"));
        if truthy(out.get("done")) {
            let app = match out.get("app") {
                Some(Value::Object(a)) if !a.is_empty() => a.clone(),
                _ => Obj::new(),
            };
            let title = if truthy(app.get("title")) { py_str(&app["title"]) } else { "Your app".to_string() };
            let icon = str_or_empty(app.get("icon"));
            let mut text = py_strip(&format!("{icon} {title} is {}.", if changed { "updated" } else { "ready" })).to_string();
            if let Some(Value::Array(issues)) = out.get("issues").filter(|v| truthy(Some(v))) {
                if let Some(first) = issues.first() {
                    text.push_str(&format!("\nIt may still have a problem: {}", cut(&py_str(first), 240)));
                }
            } else if let Some(issues) = out.get("issues").filter(|v| truthy(Some(v))) {
                // a non-list `issues` reads as its first character in Python (str(x[0]))
                let first = match issues {
                    Value::String(s) => s.chars().next().map(String::from).unwrap_or_default(),
                    other => py_str(other),
                };
                text.push_str(&format!("\nIt may still have a problem: {}", cut(&first, 240)));
            }
            let slug = str_or_empty(app.get("slug"));
            let url = self.app_url_now(&slug);
            let mut buttons: Vec<(String, String)> = Vec::new();
            if !url.is_empty() {
                buttons.push((format!("Open {title}"), url.clone()));
                if changed {
                    // back to its change chat, to keep refining
                    buttons.push(("Change".into(), self.chat_url_now(&slug)));
                }
            }
            self.say(&text, Some(&buttons)).await;
        } else {
            let slug = str_or_empty(out.get("slug"));
            let chat = if !slug.is_empty() && changed && self.store.info(&slug).is_some() {
                self.chat_url_now(&slug)
            } else {
                String::new()
            };
            let error = out.get("error").filter(|v| truthy(Some(v))).map(py_str);
            let text = error.unwrap_or_else(|| "The app build did not finish.".to_string());
            let buttons = vec![("Change".to_string(), chat.clone())];
            self.say(&text, if chat.is_empty() { None } else { Some(&buttons) }).await;
        }
        let _ = user;
    }

    /// py: miniapp.py:1080 `def _spend_note(self, mark: float, total: float) -> None:`
    ///
    /// Today's Claude spend passed ``mark``: a line in the chat. Tracking, not a limit (owner: "no claude
    /// limit, just track spend"), so a runaway (a leaked key, a loop) is seen the same day.
    pub(crate) fn spend_note(self: &Arc<Self>, mark: f64, total: f64) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let text = format!(
            "Claude has cost ${total:.2} today (chat and app builds), past ${:.0}. Nothing is capped; \
             this is only so you know.",
            mark.round_ties_even()
        );
        let me = self.clone();
        let task = Job::spawn("miniapp-spend-note", async move {
            me.say(&text, None).await;
        });
        let mut notes = self.notes.lock();
        notes.retain(|j| !j.done());
        notes.push(task);
    }

    /// py: miniapp.py:1092 `def stick_url(self) -> str:`
    ///
    /// Eve Link's tap-to-update link at the tunnel's address now; "" before pairing or with no tunnel.
    pub fn stick_url(self: &Arc<Self>) -> String {
        self.stick_url_now()
    }

    fn stick_url_now(&self) -> String {
        let stick = self.server.stick.lock().clone();
        let token = stick.map(|s| s.current()).unwrap_or_default();
        let url = self.url.lock().clone();
        if !token.is_empty() && !url.is_empty() {
            eve_stick_link::pair_url(&url, &token)
        } else {
            String::new()
        }
    }

    /// py: miniapp.py:1099 `async def stick_link(self, rotate: bool = False) -> str:`
    ///
    /// The owner's /stick: the link token (a new one with ``rotate``), and the pinned message's Update
    /// button pointing at it. Returns the pairing link, or "" while the tunnel is down.
    pub async fn stick_link(self: &Arc<Self>, rotate: bool) -> String {
        let url = self.url.lock().clone();
        let stick = self.server.stick.lock().clone();
        let Some(stick) = stick.filter(|_| !url.is_empty()) else { return String::new() };
        let made = if rotate { stick.rotate() } else { stick.ensure() };
        if let Err(e) = made {
            tracing::warn!("miniapp: Eve Link's link token could not be written ({e})");
            return String::new();
        }
        self.point_pins(&format!("{url}/")).await;
        self.stick_url_now()
    }

    /// py: miniapp.py:1111 `async def _point_pins(self, url: str) -> None:`
    pub(crate) async fn point_pins(self: &Arc<Self>, url: &str) {
        let stick = if url.is_empty() { String::new() } else { self.stick_url_now() };
        let mut owners = self.cfg.owner_ids.clone();
        owners.sort();
        owners.dedup();
        // a private chat's id is the owner's user id
        for chat in owners {
            if url.is_empty() {
                self.pins.offline(chat).await;
            } else {
                self.pins.show(chat, url, &stick).await;
            }
        }
    }

    /// py: miniapp.py:1116 `async def _announce_loop(self, every: float = 30.0, clock: Callable[[], float] = time.monotonic) -> None:`
    ///
    /// Eve Link's rendezvous (stick_link.publish_address): the address goes out when it changes, when the token
    /// does, and again every REPOST_SECS, so a phone that missed the restart finds it.
    pub(crate) async fn announce_loop(self: &Arc<Self>, every: f64, clock: Clock) {
        let base = eve_stick_link::rendezvous_base();
        if base.is_empty() {
            tracing::info!("miniapp: Eve Link's rendezvous is off (EVE_LINK_RENDEZVOUS)");
            return;
        }
        let mut sent = (String::new(), String::new());
        let mut sent_at = 0.0;
        loop {
            // Rendezvous.lean (2026-10-04): record the address that was POSTED, read before the await; self.url can
            // change while the post is in flight (a tunnel restart), and recording the new one left the topic on the
            // dead address until REPOST_SECS. And nothing raised here may end this fire-and-forget task.
            let stick = self.server.stick.lock().clone();
            let token = stick.map(|s| s.current()).unwrap_or_default();
            let url = self.url.lock().clone();
            let now = clock();
            if !url.is_empty()
                && !token.is_empty()
                && ((url.clone(), token.clone()) != sent || now - sent_at >= eve_stick_link::REPOST_SECS)
            {
                let (u, t, b) = (url.clone(), token.clone(), base.clone());
                match tokio::task::spawn_blocking(move || eve_stick_link::publish_address(&u, &t, &b)).await {
                    Ok(true) => {
                        sent = (url, token);
                        sent_at = now;
                    }
                    Ok(false) => {}
                    Err(e) => tracing::warn!("miniapp: the rendezvous post failed ({e}); trying again"),
                }
            }
            tokio::time::sleep(Duration::from_secs_f64(every.max(0.0))).await;
        }
    }

    /// `task.cancel()` on [`MiniApp::run`]: it stops the tunnel, closes the server, marks the pinned button
    /// offline, and returns.
    pub fn cancel(&self) {
        self.stopping.set();
    }

    /// py: miniapp.py:1141 `async def run(self) -> None:`
    pub async fn run(self: &Arc<Self>) {
        let Some(factory) = self.tunnel_factory.clone() else {
            tracing::warn!("miniapp: cloudflared is not installed (brew install cloudflared); the Mini App is off");
            return;
        };
        let port = match self.server.start(0).await {
            Ok(port) => port,
            Err(e) => {
                tracing::error!("miniapp: the server did not start ({e})");
                return;
            }
        };
        let cap = match self.ledger.cap {
            Some(cap) => format!("a ${cap:.2} daily cap"),
            None => "spend tracked, no cap".into(),
        };
        tracing::info!(
            "miniapp: serving on 127.0.0.1:{port} ({}, build effort {}; {cap})",
            self.cfg.model,
            self.cfg.make_effort
        );
        let me = self.clone();
        let announcer =
            Job::spawn("miniapp-rendezvous", async move { me.announce_loop(30.0, Arc::new(monotonic)).await });
        let current: Arc<Mutex<Option<Arc<dyn Tunnel>>>> = Arc::default();
        let mut cleanup = RunCleanup { app: self.clone(), announcer: Some(announcer), tunnel: current.clone() };
        let tunnels = async {
            loop {
                let tunnel = factory();
                *current.lock() = Some(tunnel.clone());
                let url = match tunnel.start(port, TUNNEL_START_SECS).await {
                    Ok(url) => url,
                    Err(e) => {
                        // no address this time: try again shortly
                        tracing::warn!(
                            "miniapp: the tunnel did not start ({e}); retrying in {:.0} s",
                            TUNNEL_RETRY_SECS
                        );
                        tokio::time::sleep(Duration::from_secs_f64(TUNNEL_RETRY_SECS)).await;
                        continue;
                    }
                };
                *self.url.lock() = url.clone();
                *self.server.public_url.lock() = url.clone();
                tracing::info!("miniapp: live at {url}; the pinned Open button points there");
                self.point_pins(&format!("{url}/")).await;
                let code = tunnel.wait().await;
                tracing::warn!("miniapp: the tunnel exited ({code}); starting a new one");
                self.url.lock().clear();
                tokio::time::sleep(Duration::from_secs_f64(TUNNEL_RETRY_SECS)).await;
            }
        };
        tokio::select! {
            _ = self.stopping.wait() => {}
            _ = tunnels => {}
        }
        let (announcer, tunnel) = cleanup.take();
        RunCleanup::finish(self.clone(), announcer, tunnel).await;
    }
}

/// `MiniApp._meter_build` for one model: the round into the daily meter, and its cost at the cap's price.
pub fn meter_build_for(model: &str, usage: &Value) -> f64 {
    eve_spend::record_anthropic(eve_spend::APPS, model, usage, "openrouter");
    cost_usd(model, usage)
}

/// app_check.check_app as a build's phone check.
fn phone_check() -> crate::apps_maker::Check {
    Arc::new(|html: String, opts: eve_app_check::CheckOptions| {
        async move { eve_app_check::check_app(&html, opts).await }.boxed()
    })
}

/// py: miniapp.py:902 `Bot = Callable[[str, dict], Awaitable[Any]]`: a Bot API call (method, data) → its result; an
/// error is the call failing.
pub type Bot = Arc<dyn Fn(String, Obj) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;
/// `notify(user, build)`: a build finished (MiniApp._build_result).
pub type BuildNotify = Arc<dyn Fn(i64, Obj) -> BoxFuture<'static, ()> + Send + Sync>;
/// `tunnel_factory()`: a tunnel to start (QuickTunnel(binary), or a test's fake).
pub type TunnelFactory = Arc<dyn Fn() -> Arc<dyn Tunnel> + Send + Sync>;

/// QuickTunnel as MiniApp drives it: `start(port) -> url`, `wait() -> exit code`, `stop()`.
#[async_trait::async_trait]
pub trait Tunnel: Send + Sync {
    async fn start(&self, port: i64, timeout: f64) -> anyhow::Result<String>;
    async fn wait(&self) -> i64;
    async fn stop(&self);
}

/// The live Mini App as the chat's app tools use it (apps_maker.ChatMaker's `app`).
#[async_trait::async_trait]
impl MakerHost for MiniApp {
    fn url(&self) -> String {
        self.url.lock().clone()
    }
    fn maker(&self) -> Arc<AppMaker> {
        self.maker.clone()
    }
    fn store(&self) -> Arc<AppStore> {
        self.store.clone()
    }
    fn app_url(&self, slug: &str) -> String {
        self.app_url_now(slug)
    }
    fn chat_url(&self, slug: &str) -> String {
        self.chat_url_now(slug)
    }
    async fn stick_link(&self, rotate: bool) -> String {
        match self.me.upgrade() {
            Some(me) => MiniApp::stick_link(&me, rotate).await,
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_qsl_strict_refuses_a_field_without_an_equals_sign() {
        assert_eq!(parse_qsl("a=1&&b=2", true, true), Err(BadQuery));
        assert_eq!(parse_qsl("not a query string at all&&&", true, true), Err(BadQuery));
        assert_eq!(parse_qsl("a=1&b=", true, true).unwrap(), vec![("a".into(), "1".into()), ("b".into(), "".into())]);
        assert_eq!(parse_qsl("t=x%20y+z&u", false, false).unwrap(), vec![("t".into(), "x y z".into())]);
        assert_eq!(unquote_plus("%E2%9C%85%zz%"), "✅%zz%");
    }

    #[test]
    fn py_int_reads_what_python_int_reads() {
        assert_eq!(py_int_str(" 12 "), Some(12));
        assert_eq!(py_int_str("1_000"), Some(1000));
        assert_eq!(py_int_str("1__0"), None);
        assert_eq!(py_int_str("-3"), Some(-3));
        assert_eq!(py_int_str("x"), None);
        assert_eq!(py_int(&json!(3.9)), Some(3));
        assert_eq!(py_int(&json!("42")), Some(42));
        assert_eq!(py_int(&json!(null)), None);
    }

    #[test]
    fn the_csp_fills_each_field_once() {
        let csp = app_csp("http://127.0.0.1:9/", "habit-tracker");
        assert!(csp.starts_with(eve_app_check::SANDBOX));
        assert!(csp.contains("connect-src http://127.0.0.1:9/api/apps/habit-tracker/ "));
        assert!(csp.contains("http://127.0.0.1:9/eve.js "));
        assert!(!csp.contains('{'));
        let odd = app_csp("http://{slug}", "x");
        assert!(odd.contains("http://{slug}/eve.js"));
    }

    #[test]
    fn the_config_never_shows_its_token() {
        let cfg = MiniAppConfig { enabled: true, token: "123456:SECRET".into(), owner_ids: vec![4242], ..Default::default() };
        let shown = format!("{cfg:?}");
        assert_eq!(
            shown,
            "MiniAppConfig(enabled=True, owners=1, model='claude-opus-5-5', make_effort='high', daily_usd=0.0)"
        );
    }

    #[test]
    fn the_api_errors_read_as_python_named_them() {
        let status = |code: u16| -> anyhow::Error {
            eve_openrouter::Error::Status { status: code, body: String::new(), headers: Box::default() }.into()
        };
        assert_eq!(error_line(&status(401)), "The OpenRouter key was refused. Check OPENROUTER_API_KEY on the Mac.");
        assert_eq!(error_line(&status(429)), "Claude is rate-limited right now. Try again in a minute.");
        assert_eq!(error_line(&status(500)), "Claude returned an error (500).");
        let down: anyhow::Error = HttpError { kind: "ConnectError".into(), message: "down".into() }.into();
        assert_eq!(error_line(&down), "Claude could not be reached. Check the Mac's internet connection.");
        assert_eq!(error_line(&anyhow::anyhow!("down")), "Something went wrong answering that.");
        assert_eq!(error_kind(&status(429)), "RateLimitError");
    }

    #[test]
    fn store_errors_are_told_apart_by_what_they_say() {
        assert_eq!(classify("KeyError(\"habit-tracker\")"), StoreFail::Missing);
        assert_eq!(classify("NoSuchApp no such app"), StoreFail::Missing);
        assert_eq!(classify("ValueError: too much data"), StoreFail::Invalid);
        assert_eq!(classify("FileExistsError taken"), StoreFail::Exists);
        assert_eq!(classify("Io(permission denied)"), StoreFail::Other("Io(permission denied)".into()));
        assert_eq!(classify("Missing(\"empty-value-exists\") 'empty-value-exists'"), StoreFail::Missing);
        assert_eq!(classify("Invalid(\"An app needs a name\") An app needs a name"), StoreFail::Invalid);
        let r: Result<i64, String> = Err("KeyError".into());
        assert_eq!(r.answer(), Err(StoreFail::Missing));
        assert_eq!(7_i64.answer(), Ok(7));
    }

    /// test_miniapp.py::test_an_owner_signed_report_never_undoes_the_app, the half that calls `_app_action`
    /// itself: an action with no branch of its own is refused before any store call.
    #[test]
    fn an_unknown_action_is_refused_before_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = MiniAppConfig { enabled: true, token: "t".into(), ledger_path: dir.path().join("s.json"), ..Default::default() };
        let srv = MiniAppServer::new(&cfg, Arc::new(SpendLedger::new(&cfg.ledger_path, None, None)), Some(b"x"), None, None, None, None);
        let (status, body) = srv.app_action("habit-tracker", "unknown", &Obj::new());
        assert_eq!(status, 400);
        assert_eq!(body["error"], "That isn't something an app can do.");
        srv.changing.lock().insert("habit-tracker".into());
        assert_eq!(srv.app_action("habit-tracker", "delete", &Obj::new()).0, 409);
        assert_eq!(srv.app_action("habit-tracker", "restore", &Obj::new()).0, 404);
    }

    /// test_miniapp.py::test_an_app_build_round_is_metered_and_still_costs_the_cap_price.
    #[test]
    fn an_app_build_round_is_metered_and_still_costs_the_cap_price() {
        let dir = tempfile::tempdir().unwrap();
        let _g = crate::miniapp::tests::SPEND.lock().unwrap_or_else(|e| e.into_inner());
        eve_spend::set_dir(Some(dir.path().join("spend")));
        let usage = json!({"input_tokens": 1_000_000, "output_tokens": 1_000_000});
        assert!((meter_build_for("claude-opus-5-5", &usage) - 24.0).abs() < 1e-9);
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let rows = eve_spend::day_rows(&today, Some(&dir.path().join("spend")));
        assert_eq!(rows[0]["p"], "openrouter");
        assert_eq!(rows[0]["f"], "app builder");
        assert!((rows[0]["usd"].as_f64().unwrap() - 24.0).abs() < 1e-9);
        // the cap never under-counts; the meter never guesses
        assert!(meter_build_for("claude-future-9", &usage) > 0.0);
        let rows = eve_spend::day_rows(&today, Some(&dir.path().join("spend")));
        assert!(rows.last().unwrap()["usd"].is_null());
        eve_spend::set_dir(None);
    }

    pub(crate) static SPEND: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
