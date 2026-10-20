//! `lite_web.py`: the two doors (crate docs). Constants, helpers, `web.json`, the page's files, the context and
//! the collaborators' seams first; then HTTP, the routes, the shared door, and the two listeners.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::future::{BoxFuture, FutureExt, Shared as SharedFuture};
use indexmap::IndexMap;
use parking_lot::{Mutex, RwLock};
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{AbortHandle, JoinHandle};
use tracing::{info, warn};

use eve_brain::onboarding::{self, OnboardingError, SignInError};
use eve_web_auth::{Device, Devices, MacLogin, PairingWindow};

// ---- where -------------------------------------------------------------------------------------------------------

/// disjoint from the LAN door's
pub const LOCAL_PORTS: [u16; 9] = [48780, 48782, 48783, 48784, 48785, 48786, 48787, 48788, 48789];
pub const LAN_PORTS: [u16; 11] = [48781, 48790, 48791, 48792, 48793, 48794, 48795, 48796, 48797, 48798, 48799];
pub const LOCAL_HOSTS: [&str; 2] = ["127.0.0.1", "::1"];
/// every interface, IPv4 and IPv6
pub const LAN_HOSTS: [&str; 2] = ["0.0.0.0", "::"];
pub const LOOPBACK_NAMES: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];
pub const WEB_FILE: &str = "~/.config/eve-bridge/web.json";
/// profile.LITE_CLASSIFIED: "kept"
pub const WEB_DIR_ENV: &str = "EVE_WEB_DIR";
/// `pnpm build:web` (ios/EveLink/Web): `web_lite` beside the Python package, so both daemons serve the same files.
pub const PACKAGED_WEB_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/web_lite");

// ---- limits ------------------------------------------------------------------------------------------------------

/// to read a request's head
pub const HEAD_SECS: f64 = 15.0;
/// to read a JSON body
pub const BODY_SECS: f64 = 30.0;
/// a TLS handshake on the LAN door
pub const TLS_SECS: f64 = 10.0;
/// how long stopping waits for open connections, and a connection for its close
pub const CLOSE_SECS: f64 = 2.0;
/// an offer's answer (the hub gathers every candidate first)
pub const RTC_SECS: f64 = 30.0;
/// Settings → Apps' calls to Composio (composio_connect waits 60 s itself)
pub const APPS_SECS: f64 = 75.0;
/// the second look after Composio sent no Connect Link
pub const CONNECTED_SECS: f64 = 10.0;
/// the OpenRouter credit is read again after this ("Check again")
pub const CREDIT_TTL_SECS: f64 = 15.0;
/// a page waits this long for a fresh read; then the last one known, and it lands later
pub const CREDIT_WAIT_SECS: f64 = 4.0;
/// the LAN door's Host allowlist is read again after this
pub const NAMES_TTL_SECS: f64 = 5.0;
/// after a sign-in: the redirect and the page load before eve restarts
pub const RESTART_DELAY_SECS: f64 = 2.0;
/// open connections per door; one more is closed at once
pub const MAX_CONNECTIONS: usize = 128;
/// and from one address: one device on the Wi-Fi cannot take every slot
pub const MAX_PER_PEER: usize = 32;
pub const MAX_HEADERS: usize = 64;
pub const MAX_TARGET_CHARS: usize = 8192;
/// an SDP offer is at most 20,000 characters (lite_rtc.MAX_SDP_CHARS)
pub const MAX_JSON_BYTES: usize = 64 * 1024;
/// a LinkChat head this long without its end goes out as it is
pub const MAX_HEAD_HOLD: usize = 64 * 1024;
/// the page's files kept in memory, raw and gzipped
pub const CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;
pub const GZIP_MIN_BYTES: usize = 1024;
/// asyncio.StreamReader's default limit: a head whose end is further than this is a 431
const STREAM_LIMIT: usize = 64 * 1024;

// ---- routes (spec M4) --------------------------------------------------------------------------------------------

pub const HEALTH_PATH: &str = "/healthz";
pub const MAC_LOGIN_PATH: &str = "/api/mac/login";
pub const ONBOARD_STATE_PATH: &str = "/onboard/state";
pub const OPENROUTER_START_PATH: &str = "/onboard/openrouter/start";
pub const OPENROUTER_CALLBACK_PATH: &str = "/onboard/openrouter/callback";
pub const COMPOSIO_START_PATH: &str = "/onboard/composio/start";
pub const COMPOSIO_CALLBACK_PATH: &str = "/onboard/composio/callback";
pub const COMPOSIO_SKIP_PATH: &str = "/onboard/composio/skip";
pub const PAIR_OPEN_PATH: &str = "/api/web/pair/open";
pub const PAIR_PATH: &str = "/api/web/pair";
pub const DEVICES_PATH: &str = "/api/web/devices";
pub const REVOKE_PATH: &str = "/api/web/devices/revoke";
pub const LAN_PATH: &str = "/api/web/lan";
pub const RTC_PATH: &str = "/api/web/rtc";
pub const APPS_PATH: &str = "/api/web/apps";
pub const STATUS_PATH: &str = "/api/web/status";
pub const LOGOUT_PATH: &str = "/api/web/logout";
pub const CA_PATH: &str = "/api/web/ca.crt";
pub const MOBILECONFIG_PATH: &str = "/api/web/eve.mobileconfig";
/// → LinkChat's /api/link/
pub const LINK_PREFIX: &str = "/api/web/link/";
/// one flat folder (vite.web.config.ts)
pub static ASSET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A/assets/([A-Za-z0-9_][A-Za-z0-9._-]{0,127})\z").expect("ASSET_RE"));
pub static APP_CONNECT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A/api/web/apps/([a-z0-9][a-z0-9_]{0,63})/connect\z").expect("APP_CONNECT_RE"));
/// link_uploads.ID_RE
pub static LINK_FILE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\Afile/[0-9a-f]{32}\z").expect("LINK_FILE_RE"));

// ---- what goes out ------------------------------------------------------------------------------------------------

/// inline styles: OpenUI injects <style> elements
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: \
     blob: https:; font-src 'self' data:; media-src 'self' blob:; connect-src 'self'; object-src 'none'; base-uri \
     'none'; form-action 'self'; frame-ancestors 'none'";
pub const SECURITY_HEADERS: [(&str, &str); 6] = [
    ("Content-Security-Policy", CSP),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    ("Permissions-Policy", "microphone=(self), camera=(), geolocation=()"),
    // frame-ancestors, for a browser that predates it
    ("X-Frame-Options", "DENY"),
    // no other site's page may embed or read an answer
    ("Cross-Origin-Resource-Policy", "same-origin"),
];
/// Added to LinkChat's own answers where it did not set them. Not the CSP: a PDF it serves inline opens in the
/// browser's viewer, which a document policy can break.
pub const LINK_HEADERS: [(&str, &str); 2] = [("Referrer-Policy", "no-referrer"), ("X-Content-Type-Options", "nosniff")];
/// never reach LinkChat
pub const LINK_DROPPED: [&str; 3] = ["origin", "cookie", "authorization"];
pub const JSON_TYPE: &str = "application/json";
pub const HTML_TYPE: &str = "text/html; charset=utf-8";
pub const NO_STORE: &str = "no-store";
/// /assets/* names carry their content's hash
pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";
pub const TYPES: [(&str, &str); 22] = [
    (".html", HTML_TYPE),
    (".js", "text/javascript; charset=utf-8"),
    (".mjs", "text/javascript; charset=utf-8"),
    (".css", "text/css; charset=utf-8"),
    (".json", JSON_TYPE),
    (".map", JSON_TYPE),
    (".svg", "image/svg+xml"),
    (".png", "image/png"),
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".webp", "image/webp"),
    (".gif", "image/gif"),
    (".ico", "image/x-icon"),
    (".woff2", "font/woff2"),
    (".woff", "font/woff"),
    (".ttf", "font/ttf"),
    (".otf", "font/otf"),
    (".txt", "text/plain; charset=utf-8"),
    (".wasm", "application/wasm"),
    (".webmanifest", "application/manifest+json"),
    (".mp3", "audio/mpeg"),
    (".wav", "audio/wav"),
];
pub const GZIP_TYPES: [&str; 4] = ["text/", "application/json", "image/svg+xml", "application/manifest+json"];
pub const REASONS: [(u16, &str); 20] = [
    (200, "OK"),
    (302, "Found"),
    (303, "See Other"),
    (400, "Bad Request"),
    (401, "Unauthorized"),
    (403, "Forbidden"),
    (404, "Not Found"),
    (405, "Method Not Allowed"),
    (409, "Conflict"),
    (411, "Length Required"),
    (413, "Payload Too Large"),
    (414, "URI Too Long"),
    (415, "Unsupported Media Type"),
    (421, "Misdirected Request"),
    (429, "Too Many Requests"),
    (431, "Request Header Fields Too Large"),
    (500, "Internal Server Error"),
    (502, "Bad Gateway"),
    (503, "Service Unavailable"),
    (504, "Gateway Timeout"),
];

/// lite.py's states; any other is shown only if it reads so
pub const READY: &str = "READY";
pub const NEEDS_OPENROUTER: &str = "NEEDS_OPENROUTER";

// What a page shows: one plain line each.
pub const WRONG_HOST: &str = "That address is not one of eve's on this Mac.";
pub const NOT_FOUND: &str = "not found";
pub const WRONG_METHOD: &str = "wrong method";
pub const WRONG_ORIGIN: &str = "Only eve's own page can do that.";
pub const NO_CHUNKED: &str = "Send the body with a Content-Length.";
pub const WRONG_TYPE: &str = "Send the body as {want}.";
pub const BAD_BODY: &str = "The body must be a JSON object.";
pub const TOO_LARGE: &str = "The body is too large.";
pub const SIGNED_OUT_MAC: &str = "This browser is not signed in to eve. Open Eve from Applications on this Mac.";
pub const SIGNED_OUT_DEV: &str =
    "This device is not paired with eve. On the Mac: Settings, Phone access, Add a phone.";
pub const LOGIN_SPENT: &str =
    "That sign-in link was already used or is out of date. Open Eve from Applications again.";
pub const NO_WINDOW: &str = "No phone pairing is open on the Mac. On the Mac: Settings, Phone access, Add a phone.";
pub const WRONG_CODE: &str = "That code didn't match. Check the six digits on the Mac and try again.";
pub const TOO_MANY: &str =
    "Too many wrong tries, so that code is closed. Make a new one on the Mac: Settings, Phone access.";
pub const DEVICE_NOT_SAVED: &str = "eve could not save this device on the Mac. Check the Mac's disk, then try again.";
pub const NOT_SAVED: &str = "eve could not save that on the Mac. Check the Mac's disk, then try again.";
pub const NOT_READY: &str = "eve isn't set up yet. Finish setting it up on the Mac, then try again.";
pub const RTC_OFF: &str = "Direct links are off on this Mac, so chat goes over HTTPS and voice can't connect.";
pub const OFFER_REFUSED: &str = "eve could not answer that link offer.";
pub const RTC_SLOW: &str = "eve took too long to answer the link offer. Try again.";
pub const LAN_FAILED: &str = "eve could not start listening for phones on this Mac: every port it uses is taken. \
                              Restart the Mac, then try again.";
pub const NO_ADDRESS: &str = "This Mac has no address a phone on the Wi-Fi can reach. Join the Wi-Fi, then try again.";
pub const LAN_FLAG: &str = "on must be true or false.";
pub const DEVICE_ID: &str = "id must be a device's id, or all.";
pub const COMPOSIO_OUT: &str = "Sign in to Composio first: on the Mac, Settings, Apps.";
pub const COMPOSIO_SLOW: &str = "Composio is slow to answer. Try again in a minute.";
pub const COMPOSIO_DOWN: &str = "Composio could not be reached from this Mac. Try again in a minute.";
pub const NOT_AN_APP: &str = "eve does not connect that app.";
pub const NO_LINK: &str = "Composio didn't send a sign-in link for {name}. Try again in a minute.";
pub const ALREADY: &str = "{name} is already connected.";
pub const SIGNIN_BROKE: &str = "That sign-in did not finish on this Mac. Start it again from eve's setup page.";
pub const NO_SIGNIN: &str = "That sign-in is not available on this Mac right now. Restart eve, then try again.";
pub const TLS_OFF: &str =
    "eve's certificate could not be read on this Mac. Turn phone access off and on again in Settings.";
pub const NOT_BUILT: &str = "eve's web page is not installed on this Mac. Run eve lite's install line again.";
pub const FAILED: &str = "That didn't work on the Mac. Try again.";

static METHOD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[A-Z]{1,16}\z").expect("_METHOD"));
/// an HTTP field name (a token)
static FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[!#$%&'*+.^_`|~0-9A-Za-z-]{1,64}\z").expect("_FIELD"));
/// twice in one request is an attack, not a list
const SINGLE: [&str; 7] =
    ["host", "content-length", "content-type", "origin", "authorization", "transfer-encoding", "x-filename"];
static STATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[A-Z][A-Z_]{0,31}\z").expect("_STATE"));
/// onboarding.credit's reasons
static CREDIT_WHY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A(?:HTTP \d{3}|[a-z][a-z ]{0,39}|[A-Z][A-Za-z]{0,39})\z").expect("_CREDIT_WHY")
});
/// a run this long is a key or a code, not a word
static OPAQUE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9_\-+/=.%]{24,}").expect("_OPAQUE"));
const KINDS: [(&str, &str); 7] = [
    ("iPhone", "iPhone"),
    ("iPad", "iPad"),
    ("Android", "Android"),
    ("CrOS", "Chromebook"),
    ("Macintosh", "Mac"),
    ("Windows", "Windows PC"),
    ("Linux", "Linux computer"),
];
const BROWSERS: [(&str, &str); 8] = [
    ("EdgiOS", "Edge"),
    ("Edg/", "Edge"),
    ("OPR/", "Opera"),
    ("FxiOS", "Firefox"),
    ("Firefox/", "Firefox"),
    ("CriOS", "Chrome"),
    ("Chrome/", "Chrome"),
    ("Safari/", "Safari"),
];

// ---- small pure helpers -----------------------------------------------------------------------------------------

/// Python's `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// Seconds as a Duration: a negative or broken figure is no wait.
fn secs(s: f64) -> Duration {
    if s.is_nan() || s <= 0.0 {
        Duration::ZERO
    } else {
        Duration::try_from_secs_f64(s).unwrap_or(Duration::MAX)
    }
}

/// `time.monotonic()`, as seconds since this process first asked.
fn monotonic() -> f64 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_secs_f64() + 1.0
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `secrets.token_urlsafe(n)`.
fn token_urlsafe(n: usize) -> String {
    use base64::Engine as _;
    use rand::RngCore as _;
    let mut bytes = vec![0u8; n];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `_port`: an int (not a bool) in 1..65535, else None.
fn port_of(value: Option<&Value>) -> Option<u16> {
    match value {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => {
            n.as_i64().filter(|p| 0 < *p && *p < 65536).map(|p| p as u16)
        }
        _ => None,
    }
}

/// `_ports`: the ports to try, in order: the one this door had last time first (the address stays put), then the
/// rest.
pub fn ports(candidates: &[u16], saved: Option<u16>) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    for p in candidates {
        if !out.contains(p) {
            out.push(*p);
        }
    }
    if let Some(saved) = saved {
        if let Some(at) = out.iter().position(|p| *p == saved) {
            out.remove(at);
            out.insert(0, saved);
        }
    }
    out
}

/// `_plain`: a line for a page, at most 300 characters on one line; `fallback` when there is none, or when it
/// holds a run that reads as a key, a token or a code. No page and no JSON from here carries one, whatever raised
/// it.
pub fn plain(line: &str, fallback: &str) -> String {
    let text: String = line.split(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect();
    if text.is_empty() || OPAQUE_RE.is_match(&text) {
        fallback.to_string()
    } else {
        text
    }
}

/// What `_credit` passes the raw credit through: `credit_view`, or a stand-in (the tests' positive control).
pub type CreditView = fn(&Value) -> Option<Value>;

/// `_credit_view`: onboarding.credit's answer as the page reads it, field by field: {ok, usd, funded} and a short
/// `error` reason. A field of any other shape is left out or null, so nothing else can ride along.
pub fn credit_view(raw: &Value) -> Option<Value> {
    let raw = raw.as_object()?;
    let usd = match raw.get("usd") {
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()),
        _ => None,
    };
    let funded = match raw.get("funded") {
        Some(Value::Bool(b)) => Value::Bool(*b),
        _ => Value::Null,
    };
    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(raw.get("ok") == Some(&Value::Bool(true))));
    out.insert("usd".into(), usd.and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number));
    out.insert("funded".into(), funded);
    if let Some(Value::String(why)) = raw.get("error") {
        if CREDIT_WHY_RE.is_match(why) {
            out.insert("error".into(), Value::String(why.clone()));
        }
    }
    if raw.get("rejected") == Some(&Value::Bool(true)) {
        out.insert("rejected".into(), Value::Bool(true));
    }
    Some(Value::Object(out))
}

/// `_key_rejected`: whether the credit read says the OpenRouter key was turned down (a 401), so a new sign-in is
/// the one fix.
pub fn key_rejected(view: Option<&Value>) -> bool {
    view.and_then(|v| v.get("rejected")) == Some(&Value::Bool(true))
}

/// How the device list names a browser: "iPhone · Safari", "Android · Chrome", "This Mac · Safari". Coarse on
/// purpose: the kind of device and the browser, never a version or a model.
pub fn device_label(user_agent: Option<&str>, where_: &str) -> String {
    let ua = user_agent.unwrap_or("");
    let browser = BROWSERS.iter().find(|(mark, _)| ua.contains(mark)).map_or("", |(_, name)| name);
    let kind = if where_ == "local" {
        "This Mac"
    } else {
        KINDS.iter().find(|(mark, _)| ua.contains(mark)).map_or("A device", |(_, name)| name)
    };
    if browser.is_empty() {
        kind.to_string()
    } else {
        format!("{kind} · {browser}")
    }
}

/// `_host_name`: the Host header without its port: "kitchen-mac.local", "192.168.1.20", "localhost", "[::1]".
pub fn host_name(host: &str) -> String {
    match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => name.to_string(),
        _ => host.to_string(),
    }
}

/// `urllib.parse.unquote(s)`: percent-escapes decoded as UTF-8, a broken sequence replaced.
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() && bytes[i + 1].is_ascii_hexdigit() && bytes[i + 2].is_ascii_hexdigit() {
            let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            out.push(hex(bytes[i + 1]) * 16 + hex(bytes[i + 2]));
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `_query`: a query's fields, the first of each name; {} for one that will not parse
/// (`parse_qsl(qs, keep_blank_values=True, max_num_fields=32)`).
pub fn query(qs: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if !qs.is_empty() && 1 + qs.matches('&').count() > 32 {
        return out;
    }
    for field in qs.split('&') {
        if field.is_empty() {
            continue;
        }
        let (name, value) = field.split_once('=').unwrap_or((field, ""));
        let name = unquote(&name.replace('+', " "));
        let value = unquote(&value.replace('+', " "));
        out.entry(name).or_insert(value);
    }
    out
}

fn media(headers: &IndexMap<String, String>) -> String {
    let ctype = headers.get("content-type").map(String::as_str).unwrap_or("");
    py_strip(ctype.split(';').next().unwrap_or("")).to_lowercase()
}

/// `os.path.splitext(name)[1]`: the last component's extension, leading dots not counting.
fn extension(name: &str) -> &str {
    let base = name.rsplit('/').next().unwrap_or(name);
    let lead = base.len() - base.trim_start_matches('.').len();
    match base[lead..].rfind('.') {
        Some(at) => &base[lead + at..],
        None => "",
    }
}

/// `_content_type`.
pub fn content_type(name: &str) -> &'static str {
    let ext = extension(name).to_lowercase();
    TYPES.iter().find(|(e, _)| *e == ext).map_or("application/octet-stream", |(_, t)| t)
}

/// `_accepts_gzip`: whether Accept-Encoding takes gzip (or anything, `*`) with a weight above zero.
pub fn accepts_gzip(header: &str) -> bool {
    for part in header.split(',') {
        let part = py_strip(part);
        let (coding, params) = part.split_once(';').unwrap_or((part, ""));
        let coding = py_strip(coding).to_lowercase();
        if coding != "gzip" && coding != "*" {
            continue;
        }
        let mut weight = 1.0f64;
        for param in params.split(';') {
            let param = py_strip(param);
            let (name, value) = param.split_once('=').unwrap_or((param, ""));
            if py_strip(name).to_lowercase() == "q" {
                weight = eve_apps_mcp::py::parse_float(value).unwrap_or(0.0);
            }
        }
        return weight > 0.0;
    }
    false
}

/// `html.escape(s)`.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// `_page_html`: a short page with no script: a sign-in that did not finish, or no web page installed.
pub fn page_html(title: &str, line: &str) -> Vec<u8> {
    let (t, body) = (html_escape(title), html_escape(line));
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"light dark\">\
         <title>{t}</title><style>body{{font:16px/1.5 -apple-system,system-ui,sans-serif;max-width:32rem;\
         margin:15vh auto;padding:0 1rem}}a{{color:inherit}}</style></head>\
         <body><h1>{t}</h1><p>{body}</p><p><a href=\"/\">Back to eve</a></p></body></html>"
    )
    .into_bytes()
}

/// `url` as a QR code: an SVG data URI that scales to its box, black on white (readable on a dark page), with the
/// standard quiet zone, byte for byte what segno draws. "" (said once) when the text does not fit a QR code: the
/// page then shows the link alone.
pub fn qr_svg(url: &str) -> String {
    match crate::qr::svg_data_uri(url) {
        Some(uri) => uri,
        None => {
            static SAID: AtomicBool = AtomicBool::new(false);
            if !SAID.swap(true, Ordering::SeqCst) {
                warn!("lite web: a pairing link does not fit a QR code, so it has none");
            }
            String::new()
        }
    }
}

/// `type(e).__name__` for what a Rust error can say: the OSError family for an io::Error anywhere in the chain,
/// else ValueError.
fn error_name(e: &anyhow::Error) -> &'static str {
    match e.chain().find_map(|c| c.downcast_ref::<io::Error>()) {
        Some(io) => os_error_name(io),
        None => "ValueError",
    }
}

fn os_error_name(e: &io::Error) -> &'static str {
    use io::ErrorKind::*;
    match e.kind() {
        PermissionDenied => "PermissionError",
        NotFound => "FileNotFoundError",
        AlreadyExists => "FileExistsError",
        IsADirectory => "IsADirectoryError",
        NotADirectory => "NotADirectoryError",
        Interrupted => "InterruptedError",
        TimedOut => "TimeoutError",
        ConnectionRefused => "ConnectionRefusedError",
        ConnectionReset => "ConnectionResetError",
        ConnectionAborted => "ConnectionAbortedError",
        BrokenPipe => "BrokenPipeError",
        _ => "OSError",
    }
}

/// `os.path.expanduser`.
fn expanduser(path: &Path) -> PathBuf {
    eve_apps_mcp::py::expanduser(path)
}

// ---- the environment ---------------------------------------------------------------------------------------------

#[derive(Debug)]
enum EnvKind {
    Process,
    Map(IndexMap<String, String>),
}

/// `WebContext.environ`: the process's environment (the daemon), or a mapping (the tests). It holds the model
/// key, so it is never shown (no Debug of its values). Clones share one environment.
#[derive(Clone)]
pub struct WebEnv(Arc<Mutex<EnvKind>>);

impl std::fmt::Debug for WebEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebEnv(..)")
    }
}

impl WebEnv {
    /// `os.environ`.
    pub fn process() -> Self {
        WebEnv(Arc::new(Mutex::new(EnvKind::Process)))
    }

    /// A mapping of its own.
    pub fn map<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        WebEnv(Arc::new(Mutex::new(EnvKind::Map(pairs.into_iter().map(|(k, v)| (k.into(), v.into())).collect()))))
    }

    pub fn get(&self, key: &str) -> Option<String> {
        match &*self.0.lock() {
            EnvKind::Process => std::env::var_os(key).map(|v| v.to_string_lossy().into_owned()),
            EnvKind::Map(m) => m.get(key).cloned(),
        }
    }

    /// `environ[key] = value` (a mapping; the process's environment only for a value it can hold).
    pub fn set(&self, key: &str, value: &str) {
        let mut guard = self.0.lock();
        match &mut *guard {
            EnvKind::Process => {
                let _ = eve_envfile::Environ::set_var(&mut eve_envfile::ProcessEnv, key, value);
            }
            EnvKind::Map(m) => {
                m.insert(key.to_string(), value.to_string());
            }
        }
    }

    pub fn remove(&self, key: &str) {
        let mut guard = self.0.lock();
        match &mut *guard {
            EnvKind::Process => std::env::remove_var(key),
            EnvKind::Map(m) => {
                m.shift_remove(key);
            }
        }
    }

    /// `profile.is_lite(environ)`; Err is profile's ValueError for an EVE_PROFILE that is neither.
    fn is_lite(&self) -> Result<bool, String> {
        match &*self.0.lock() {
            EnvKind::Process => eve_profile::is_lite_in(&eve_profile::ProcessEnv).map_err(|e| e.to_string()),
            EnvKind::Map(m) => eve_profile::is_lite_in(m).map_err(|e| e.to_string()),
        }
    }

    /// `onboarding.save_openrouter_key(key, path=path, environ=self)`. Blocking.
    fn save_openrouter_key(&self, key: &str, path: Option<&Path>) -> Result<(), SignInError> {
        let mut guard = self.0.lock();
        match &mut *guard {
            EnvKind::Process => onboarding::save_openrouter_key(key, path, &mut eve_envfile::ProcessEnv),
            EnvKind::Map(m) => onboarding::save_openrouter_key(key, path, m),
        }
    }
}

// ---- the collaborators -------------------------------------------------------------------------------------------

/// link_chat.LinkChat as the doors use it: `serve` on the real stream, and `close` + `settle` when eve stops.
#[async_trait]
pub trait LinkServe: Send + Sync {
    /// One request on `/api/link/*`, its head already read; the whole answer is written to `writer`.
    #[allow(clippy::too_many_arguments)]
    async fn serve(
        &self,
        method: &str,
        path: &str,
        query: &str,
        headers: &eve_link::seams::Headers,
        reader: &mut eve_link::seams::DynRead,
        writer: &mut eve_link::seams::DynWrite,
        token_check: &(dyn for<'s> Fn(&'s str) -> bool + Send + Sync + 'static),
    ) -> io::Result<()>;
    /// The server is stopping: every turn ends, and its streams with it.
    fn close(&self);
    /// After `close`: the open streams write their [DONE] (link_chat.CLOSE_SETTLE_SECS at most).
    async fn settle(&self);
}

#[async_trait]
impl LinkServe for eve_link::link_chat::LinkChat {
    async fn serve(
        &self,
        method: &str,
        path: &str,
        query: &str,
        headers: &eve_link::seams::Headers,
        reader: &mut eve_link::seams::DynRead,
        writer: &mut eve_link::seams::DynWrite,
        token_check: &(dyn for<'s> Fn(&'s str) -> bool + Send + Sync + 'static),
    ) -> io::Result<()> {
        eve_link::link_chat::LinkChat::serve(self, method, path, query, headers, reader, writer, Some(token_check), None)
            .await
    }

    fn close(&self) {
        eve_link::link_chat::LinkChat::close(self);
    }

    async fn settle(&self) {
        eve_link::link_chat::LinkChat::settle(self, eve_link::link_chat::CLOSE_SETTLE_SECS).await;
    }
}

/// Why an offer got no answer: `Refused` is lite_rtc.RtcError (a ValueError: an offer this Mac will not answer,
/// a 400 with its line); `Failed` anything else (a 500).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RtcFailure {
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Failed(String),
}

/// lite_rtc.RtcHub as the doors use it: answer, close_device, voice_ready.
#[async_trait]
pub trait RtcDoor: Send + Sync {
    /// `{"sdp": <answer>, "voice": <bool>}` for a device's offer (`sdp` as the body gave it).
    async fn answer(&self, device_id: &str, sdp: &Value, peer_addr: &str) -> Result<Value, RtcFailure>;
    /// A revoked device: its peer closes now.
    async fn close_device(&self, device_id: &str) -> anyhow::Result<()>;
    /// Whether calls can be taken.
    fn voice_ready(&self) -> anyhow::Result<bool>;
}

#[async_trait]
impl RtcDoor for eve_lite_rtc::RtcHub {
    async fn answer(&self, device_id: &str, sdp: &Value, peer_addr: &str) -> Result<Value, RtcFailure> {
        eve_lite_rtc::RtcHub::answer(self, device_id, sdp.as_str(), peer_addr)
            .await
            .map(|a| a.to_value())
            .map_err(|e| RtcFailure::Refused(e.0))
    }

    async fn close_device(&self, device_id: &str) -> anyhow::Result<()> {
        eve_lite_rtc::RtcHub::close_device(self, device_id).await;
        Ok(())
    }

    fn voice_ready(&self) -> anyhow::Result<bool> {
        Ok(eve_lite_rtc::RtcHub::voice_ready(self))
    }
}

/// lite_tls.LocalTls as the LAN door uses it. Blocking.
pub trait TlsDoor: Send + Sync {
    /// The server context for the TLS accept: TLS 1.2 or later, the current leaf.
    fn context(&self) -> anyhow::Result<Arc<rustls::ServerConfig>>;
    /// The leaf reissued when it lacks one of these names or addresses.
    fn refresh(&self, addresses: &[String], hostname: &str) -> anyhow::Result<bool>;
    /// Every name the leaf holds: DNS names in lower case and addresses.
    fn names(&self) -> anyhow::Result<HashSet<String>>;
    /// The CA certificate as PEM.
    fn ca_pem(&self) -> anyhow::Result<Vec<u8>>;
    /// The iPhone profile that installs the CA.
    fn mobileconfig(&self) -> anyhow::Result<Vec<u8>>;
}

impl TlsDoor for eve_lite_tls::LocalTls {
    fn context(&self) -> anyhow::Result<Arc<rustls::ServerConfig>> {
        eve_lite_tls::LocalTls::context(self)
    }

    fn refresh(&self, addresses: &[String], hostname: &str) -> anyhow::Result<bool> {
        eve_lite_tls::LocalTls::refresh(self, addresses.iter().map(String::as_str), hostname)
    }

    fn names(&self) -> anyhow::Result<HashSet<String>> {
        eve_lite_tls::LocalTls::names(self)
    }

    fn ca_pem(&self) -> anyhow::Result<Vec<u8>> {
        eve_lite_tls::LocalTls::ca_pem(self)
    }

    fn mobileconfig(&self) -> anyhow::Result<Vec<u8>> {
        eve_lite_tls::LocalTls::mobileconfig(self)
    }
}

/// onboarding.OpenRouterLogin as the setup page uses it.
#[async_trait]
pub trait OpenRouterSignIn: Send + Sync {
    /// The URL to send the browser to; `callback_url` is this door's own callback.
    fn start(&self, callback_url: &str) -> Result<String, SignInError>;
    /// The key, for the callback's `code` and `state` ("" when the query has none).
    async fn finish(&self, code: &str, state: &str, error: &str) -> Result<String, OnboardingError>;
}

#[async_trait]
impl OpenRouterSignIn for onboarding::OpenRouterLogin {
    fn start(&self, callback_url: &str) -> Result<String, SignInError> {
        onboarding::OpenRouterLogin::start(self, callback_url, "")
    }

    async fn finish(&self, code: &str, state: &str, error: &str) -> Result<String, OnboardingError> {
        onboarding::OpenRouterLogin::finish(self, code.to_string(), state.to_string(), error).await
    }
}

/// onboarding.ComposioConnectLogin as the setup page uses it.
#[async_trait]
pub trait ComposioSignIn: Send + Sync {
    /// `token_path`: composio-connect.json, which onboarding writes only once a token arrived.
    fn token_path(&self) -> Option<PathBuf>;
    async fn start(&self, callback_url: &str) -> Result<String, SignInError>;
    async fn finish(&self, code: &str, state: &str, error: &str) -> Result<(), OnboardingError>;
}

#[async_trait]
impl ComposioSignIn for onboarding::ComposioConnectLogin {
    fn token_path(&self) -> Option<PathBuf> {
        Some(self.token_path.clone())
    }

    async fn start(&self, callback_url: &str) -> Result<String, SignInError> {
        onboarding::ComposioConnectLogin::start(self, callback_url).await
    }

    async fn finish(&self, code: &str, state: &str, error: &str) -> Result<(), OnboardingError> {
        onboarding::ComposioConnectLogin::finish(self, code.to_string(), state.to_string(), error).await
    }
}

/// composio_connect.ConnectBridge as Settings → Apps uses it.
#[async_trait]
pub trait AppsDoor: Send + Sync {
    /// Only a new sign-in helps (composio_connect.needs_sign_in).
    fn needs_sign_in(&self) -> anyhow::Result<bool>;
    /// {slug: connected}: `None` is every app in APPS (the bridge's default), else those toolkits.
    async fn connections(&self, toolkits: Option<&[&str]>) -> anyhow::Result<IndexMap<String, bool>>;
    /// The Connect Link for one app, or None.
    async fn connect_link(&self, toolkit: &str) -> anyhow::Result<Option<String>>;
}

#[async_trait]
impl AppsDoor for eve_composio_connect::ConnectBridge {
    fn needs_sign_in(&self) -> anyhow::Result<bool> {
        Ok(eve_composio_connect::ConnectBridge::needs_sign_in(self))
    }

    async fn connections(&self, toolkits: Option<&[&str]>) -> anyhow::Result<IndexMap<String, bool>> {
        let all = eve_composio_connect::app_slugs();
        let asked = toolkits.unwrap_or(&all);
        Ok(eve_composio_connect::ConnectBridge::connections(self, asked, eve_composio_connect::CONNECTIONS_TTL_SECS)
            .await)
    }

    async fn connect_link(&self, toolkit: &str) -> anyhow::Result<Option<String>> {
        Ok(eve_composio_connect::ConnectBridge::connect_link(self, toolkit).await)
    }
}

/// `WebContext.status`: the daemon's own state (a mapping: `state`, optionally `openrouter` and `voice`).
pub type StatusFn = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;
/// `WebContext.restart`: asks the daemon to restart, after a credential was written.
pub type RestartFn = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// `WebContext.credit`: onboarding.credit (what the key can spend).
pub type CreditFn = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;
pub type HostnameFn = Arc<dyn Fn() -> anyhow::Result<String> + Send + Sync>;
pub type AddressesFn = Arc<dyn Fn() -> anyhow::Result<Vec<String>> + Send + Sync>;
pub type PeerAllowedFn = Arc<dyn Fn(&SocketAddr) -> anyhow::Result<bool> + Send + Sync>;
pub type AlertFn = Arc<dyn Fn() -> anyhow::Result<()> + Send + Sync>;
pub type ProbeFn = Arc<dyn Fn(&SocketAddr) -> anyhow::Result<String> + Send + Sync>;

/// A status that is always `value` (a sync mapping in Python).
pub fn status_value(value: Value) -> StatusFn {
    Arc::new(move || {
        let v = value.clone();
        async move { Ok(v) }.boxed()
    })
}

/// The limits a test turns down (Python's tests patch the module constants).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub head_secs: f64,
    pub credit_wait_secs: f64,
    pub max_connections: usize,
    pub max_per_peer: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            head_secs: HEAD_SECS,
            credit_wait_secs: CREDIT_WAIT_SECS,
            max_connections: MAX_CONNECTIONS,
            max_per_peer: MAX_PER_PEER,
        }
    }
}

/// What the two doors are given (M11), read again on every request, so the daemon may set a part that comes up
/// later (`link`, `rtc`, `apps`) on the same context ([`WebContext::update`]):
///
/// * `link`: link_chat.LinkChat, the one chat door; None while eve needs its OpenRouter key (setup only).
/// * `rtc`: lite_rtc.RtcHub, or None (no WebRTC: the page chats over HTTPS, and voice is off).
/// * `devices`, `pairing`, `mac_login`: web_auth.Devices, PairingWindow and MacLogin.
/// * `tls`: lite_tls.LocalTls, the LAN door's certificate.
/// * `openrouter`, `composio_login`: onboarding.OpenRouterLogin and ComposioConnectLogin.
/// * `apps`: composio_connect.ConnectBridge once Composio is signed in, else None.
/// * `status`: the daemon's own state: `state` (lite.py's `READY` or `NEEDS_OPENROUTER`), and optionally
///   `openrouter` and `voice` as booleans. Nothing else is read from it.
/// * `restart`: asks the daemon to restart, after a credential was written.
/// * `web_dir`: the page's files; None reads EVE_WEB_DIR, else the packaged `web_lite`.
/// * `state_path`: `web.json`.
///
/// The fields after those are the daemon's defaults; tests replace them (ports 0, fakes for the network).
#[derive(Clone)]
pub struct Parts {
    pub link: Option<Arc<dyn LinkServe>>,
    pub rtc: Option<Arc<dyn RtcDoor>>,
    pub devices: Arc<Devices>,
    pub pairing: Arc<PairingWindow>,
    pub mac_login: Arc<MacLogin>,
    pub tls: Arc<dyn TlsDoor>,
    pub openrouter: Option<Arc<dyn OpenRouterSignIn>>,
    pub composio_login: Option<Arc<dyn ComposioSignIn>>,
    pub apps: Option<Arc<dyn AppsDoor>>,
    pub status: Option<StatusFn>,
    pub restart: Option<RestartFn>,
    pub web_dir: Option<PathBuf>,
    pub state_path: PathBuf,
    /// the model key, EVE_WEB_DIR
    pub environ: WebEnv,
    /// where save_openrouter_key writes; None: envfile.env_path(environ)
    pub env_path: Option<PathBuf>,
    pub credit: CreditFn,
    pub hostname: HostnameFn,
    pub addresses: AddressesFn,
    pub peer_allowed: PeerAllowedFn,
    pub alert: AlertFn,
    pub probe: ProbeFn,
    pub local_hosts: Vec<String>,
    pub local_ports: Vec<u16>,
    pub lan_hosts: Vec<String>,
    pub lan_ports: Vec<u16>,
    pub restart_delay: f64,
    pub limits: Limits,
    pub credit_view: CreditView,
}

impl Parts {
    /// The parts with the daemon's defaults for everything but the five a door cannot do without: no link, no
    /// rtc, no sign-ins, no apps, no status and no restart yet (set them on the struct).
    pub fn new(
        devices: Arc<Devices>,
        pairing: Arc<PairingWindow>,
        mac_login: Arc<MacLogin>,
        tls: Arc<dyn TlsDoor>,
        state_path: impl Into<PathBuf>,
    ) -> Self {
        Parts {
            link: None,
            rtc: None,
            devices,
            pairing,
            mac_login,
            tls,
            openrouter: None,
            composio_login: None,
            apps: None,
            status: None,
            restart: None,
            web_dir: None,
            state_path: state_path.into(),
            environ: WebEnv::process(),
            env_path: None,
            credit: Arc::new(|key: String| {
                async move { Ok(Value::Object(onboarding::credit(&key, None).await)) }.boxed()
            }),
            hostname: Arc::new(|| Ok(eve_lan_net::local_hostname())),
            addresses: Arc::new(|| Ok(eve_lan_net::private_addresses())),
            peer_allowed: Arc::new(|peer: &SocketAddr| Ok(eve_lan_net::peer_allowed(*peer))),
            alert: Arc::new(|| {
                eve_lan_net::trigger_local_network_alert();
                Ok(())
            }),
            probe: Arc::new(|peer: &SocketAddr| Ok(eve_lan_net::probe(*peer).to_string())),
            local_hosts: LOCAL_HOSTS.iter().map(|s| s.to_string()).collect(),
            local_ports: LOCAL_PORTS.to_vec(),
            lan_hosts: LAN_HOSTS.iter().map(|s| s.to_string()).collect(),
            lan_ports: LAN_PORTS.to_vec(),
            restart_delay: RESTART_DELAY_SECS,
            limits: Limits::default(),
            credit_view,
        }
    }
}

/// `WebContext`: the parts (read again on every request) and what the two doors share while the daemon runs.
pub struct WebContext {
    parts: RwLock<Parts>,
    state_path: PathBuf,
    pub shared: Shared,
}

impl std::fmt::Debug for WebContext {
    // never the environment: it holds the model key
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self.state_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        write!(f, "WebContext({name}, link {})", if self.parts.read().link.is_some() { "on" } else { "off" })
    }
}

impl WebContext {
    pub fn new(mut parts: Parts) -> Arc<Self> {
        parts.state_path = expanduser(&parts.state_path);
        let state_path = parts.state_path.clone();
        Arc::new(WebContext { shared: Shared::new(&state_path), state_path, parts: RwLock::new(parts) })
    }

    /// The parts as they are now.
    pub fn parts(&self) -> Parts {
        self.parts.read().clone()
    }

    /// Change a part: the daemon setting `link`, `rtc` or `apps` once they come up; a test its fakes.
    pub fn update(&self, change: impl FnOnce(&mut Parts)) {
        change(&mut self.parts.write());
    }

    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    /// The page's files: `web_dir`, else EVE_WEB_DIR, else the packaged `web_lite`.
    pub fn web_root(&self) -> PathBuf {
        let parts = self.parts.read();
        if let Some(dir) = &parts.web_dir {
            return expanduser(dir);
        }
        let named = parts.environ.get(WEB_DIR_ENV).unwrap_or_default();
        let named = py_strip(&named);
        if named.is_empty() {
            PathBuf::from(PACKAGED_WEB_DIR)
        } else {
            expanduser(Path::new(named))
        }
    }

    fn require_lite(&self) -> Result<(), StartError> {
        match self.parts.read().environ.is_lite() {
            Ok(true) => Ok(()),
            Ok(false) => Err(StartError::Runtime(
                "the web doors are eve lite's (EVE_PROFILE=lite); this is full eve".to_string(),
            )),
            Err(e) => Err(StartError::Value(e)),
        }
    }
}

// ---- web.json ----------------------------------------------------------------------------------------------------

/// One change `Saved::save` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    LocalPort(u16),
    LanPort(u16),
    Lan(bool),
    ComposioSkipped(bool),
}

/// What web.json holds now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SavedState {
    pub local_port: Option<u16>,
    pub lan_port: Option<u16>,
    pub lan: bool,
    pub composio_skipped: bool,
}

pub type JsonWriter = Arc<dyn Fn(&Path, &Map<String, Value>) -> io::Result<()> + Send + Sync>;

/// `_Saved`: `web.json` (0600): the port each door had last, the LAN switch, and whether setup's Composio step was
/// skipped. Read once; written whole and atomically on each change. Thread-safe.
pub struct Saved {
    pub path: PathBuf,
    state: Mutex<SavedState>,
    writer: Mutex<JsonWriter>,
}

impl Saved {
    pub fn new(path: &Path) -> Self {
        let path = expanduser(path);
        let data = eve_apps_mcp::read_json(&path);
        let state = SavedState {
            local_port: port_of(data.get("local_port")),
            lan_port: port_of(data.get("lan_port")),
            lan: data.get("lan") == Some(&Value::Bool(true)),
            composio_skipped: data.get("composio_skipped") == Some(&Value::Bool(true)),
        };
        Saved { path, state: Mutex::new(state), writer: Mutex::new(Arc::new(eve_apps_mcp::write_private_json)) }
    }

    pub fn state(&self) -> SavedState {
        *self.state.lock()
    }

    pub fn local_port(&self) -> Option<u16> {
        self.state.lock().local_port
    }

    pub fn lan_port(&self) -> Option<u16> {
        self.state.lock().lan_port
    }

    pub fn lan(&self) -> bool {
        self.state.lock().lan
    }

    pub fn composio_skipped(&self) -> bool {
        self.state.lock().composio_skipped
    }

    /// Hold a value without writing it (a test setting what web.json said at start).
    pub fn set_held(&self, change: Change) {
        apply(&mut self.state.lock(), change);
    }

    /// The writer (apps_mcp.write_private_json; a test lends a full disk).
    pub fn set_writer(&self, writer: JsonWriter) {
        *self.writer.lock() = writer;
    }

    /// Write the file with `changes`, then hold them. False (logged) when it could not be written: then nothing
    /// changed, so what the page shows is what the next start will do.
    pub fn save(&self, changes: &[Change]) -> bool {
        let mut state = self.state.lock();
        let mut next = *state;
        for change in changes {
            apply(&mut next, *change);
        }
        let mut data = Map::new();
        data.insert("version".into(), json!(1));
        data.insert("local_port".into(), next.local_port.map_or(Value::Null, |p| json!(p)));
        data.insert("lan_port".into(), next.lan_port.map_or(Value::Null, |p| json!(p)));
        data.insert("lan".into(), json!(next.lan));
        data.insert("composio_skipped".into(), json!(next.composio_skipped));
        let writer = self.writer.lock().clone();
        if let Err(e) = writer(&self.path, &data) {
            warn!("lite web: web.json was not saved ({})", os_error_name(&e));
            return false;
        }
        *state = next;
        true
    }
}

fn apply(state: &mut SavedState, change: Change) {
    match change {
        Change::LocalPort(p) => state.local_port = Some(p),
        Change::LanPort(p) => state.lan_port = Some(p),
        Change::Lan(on) => state.lan = on,
        Change::ComposioSkipped(on) => state.composio_skipped = on,
    }
}

// ---- the page's files --------------------------------------------------------------------------------------------

type Stamp = (u64, u64, i128);

struct Kept {
    stamp: Stamp,
    data: Arc<Vec<u8>>,
    zipped: Option<Arc<Vec<u8>>>,
}

/// `_Files`: the page's files, each read once and kept with its gzip until it changes on disk. Thread-safe.
#[derive(Default)]
pub struct Files {
    kept: Mutex<(HashMap<PathBuf, Kept>, usize)>,
}

impl Files {
    /// (body, gzipped) of `path`, a regular file inside `root` once every link is followed; None for anything else.
    /// Blocking.
    pub fn read(&self, path: &Path, root: &Path, compress: bool) -> Option<(Arc<Vec<u8>>, bool)> {
        use std::os::unix::fs::MetadataExt as _;
        let base = std::fs::canonicalize(root).ok()?;
        let real = std::fs::canonicalize(path).ok()?;
        if !real.starts_with(&base) {
            return None;
        }
        let st = std::fs::metadata(&real).ok()?;
        if !st.is_file() {
            return None;
        }
        let stamp: Stamp = (st.ino(), st.size(), i128::from(st.mtime()) * 1_000_000_000 + i128::from(st.mtime_nsec()));
        let kept = {
            let guard = self.kept.lock();
            guard.0.get(&real).map(|k| (k.stamp, k.data.clone(), k.zipped.clone()))
        };
        let (stamp, data, mut zipped) = match kept {
            Some(k) if k.0 == stamp => k,
            _ => (stamp, Arc::new(std::fs::read(&real).ok()?), None),
        };
        let use_zip = compress && data.len() >= GZIP_MIN_BYTES;
        if use_zip && zipped.is_none() {
            zipped = Some(Arc::new(gzip(&data)));
        }
        let size = data.len() + zipped.as_ref().map_or(0, |z| z.len());
        {
            let mut guard = self.kept.lock();
            let (map, total) = &mut *guard;
            if let Some(old) = map.remove(&real) {
                *total -= old.data.len() + old.zipped.as_ref().map_or(0, |z| z.len());
            }
            if size <= CACHE_MAX_BYTES / 4 {
                // a bigger file is read each time, never kept
                if *total + size > CACHE_MAX_BYTES {
                    map.clear();
                    *total = 0;
                }
                map.insert(real, Kept { stamp, data: data.clone(), zipped: zipped.clone() });
                *total += size;
            }
        }
        match zipped {
            Some(z) if use_zip => Some((z, true)),
            _ => Some((data, false)),
        }
    }
}

/// `gzip.compress(data, compresslevel=6, mtime=0)`.
fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut enc = flate2::GzBuilder::new().mtime(0).write(Vec::new(), flate2::Compression::new(6));
    if enc.write_all(data).is_err() {
        return Vec::new();
    }
    enc.finish().unwrap_or_default()
}

// ---- what the doors share --------------------------------------------------------------------------------------

/// The daemon's state as the pages may read it: a state word, a model key saved, calls can be taken.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Daemon {
    state: String,
    openrouter: bool,
    voice: bool,
}

type CreditTask = SharedFuture<BoxFuture<'static, Value>>;

/// `_Shared`: what the two doors share while the daemon runs: one per WebContext.
pub struct Shared {
    /// LinkChat's only token here (the LinkChat door)
    pub nonce: String,
    pub saved: Saved,
    restarting: AtomicBool,
    /// the last sign-in that did not finish, as its page said it
    error: Mutex<String>,
    /// lan_net.probe's last word on the Local Network privilege
    local_network: Mutex<String>,
    /// (SHA-256 of the key, when, credit)
    credit: Mutex<Option<(String, f64, Value)>>,
    /// the read in flight: for which key, the shared answer, and whether it is done (`task.done()`)
    credit_task: Mutex<Option<(String, CreditTask, AbortHandle)>>,
    /// device id -> the door its WebRTC peer was signaled through
    rtc_doors: Mutex<IndexMap<String, String>>,
    /// the LAN door, once one was made (LocalServer::set_lan)
    lan: Mutex<Option<LanServer>>,
    tasks: Mutex<HashMap<tokio::task::Id, AbortHandle>>,
    files: Files,
    warned: Mutex<HashSet<String>>,
    lan_lock: tokio::sync::Mutex<()>,
}

impl Shared {
    fn new(state_path: &Path) -> Self {
        Shared {
            nonce: token_urlsafe(32),
            saved: Saved::new(state_path),
            restarting: AtomicBool::new(false),
            error: Mutex::new(String::new()),
            local_network: Mutex::new("unknown".to_string()),
            credit: Mutex::new(None),
            credit_task: Mutex::new(None),
            rtc_doors: Mutex::new(IndexMap::new()),
            lan: Mutex::new(None),
            tasks: Mutex::new(HashMap::new()),
            files: Files::default(),
            warned: Mutex::new(HashSet::new()),
            lan_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn restarting(&self) -> bool {
        self.restarting.load(Ordering::SeqCst)
    }

    pub fn error(&self) -> String {
        self.error.lock().clone()
    }

    pub fn local_network(&self) -> String {
        self.local_network.lock().clone()
    }

    /// The last credit read: (SHA-256 of the key, when, the page's view).
    pub fn credit(&self) -> Option<(String, f64, Value)> {
        self.credit.lock().clone()
    }

    /// Forget the last credit read (`shared.credit = None`): the next page reads it again.
    pub fn forget_credit(&self) {
        *self.credit.lock() = None;
    }

    pub fn rtc_doors(&self) -> IndexMap<String, String> {
        self.rtc_doors.lock().clone()
    }

    /// The LAN door, once one was made.
    pub fn lan(&self) -> Option<LanServer> {
        self.lan.lock().clone()
    }

    fn warn_once(&self, key: &str) -> bool {
        self.warned.lock().insert(key.to_string())
    }

    fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.tasks.lock();
        tasks.retain(|_, h| !h.is_finished());
        let handle = tokio::spawn(fut);
        tasks.insert(handle.id(), handle.abort_handle());
    }
}

// ---- errors ------------------------------------------------------------------------------------------------------

/// `PortsTaken`: every port a door may use is taken (LOCAL_PORTS, LAN_PORTS); an OSError with errno EADDRINUSE.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("[Errno {errno}] no free port among {count}")]
pub struct PortsTaken {
    pub errno: i32,
    pub count: usize,
}

/// What starting a door can fail with: PortsTaken; the certificate not made or loaded (OSError or ValueError);
/// Python's RuntimeError (outside eve lite, a second LAN door); or profile's ValueError.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    PortsTaken(#[from] PortsTaken),
    #[error("{0}")]
    Tls(anyhow::Error),
    #[error("{0}")]
    Runtime(String),
    #[error("{0}")]
    Value(String),
}

impl StartError {
    /// `type(e).__name__`.
    pub fn name(&self) -> &'static str {
        match self {
            StartError::PortsTaken(_) => "PortsTaken",
            StartError::Tls(e) => error_name(e),
            StartError::Runtime(_) => "RuntimeError",
            StartError::Value(_) => "ValueError",
        }
    }

    /// `except (OSError, ValueError)`: what the callers catch (PortsTaken is an OSError too).
    fn is_os_or_value(&self) -> bool {
        !matches!(self, StartError::Runtime(_))
    }
}

/// How a request handler can fail: the peer went away (`Conn`: nothing more to say), or something else, which is
/// a 500 when nothing was answered yet (`type(e).__name__` only is logged).
#[derive(Debug)]
enum Failure {
    Conn(io::Error),
    Error(&'static str),
}

impl From<io::Error> for Failure {
    fn from(e: io::Error) -> Self {
        Failure::Conn(e)
    }
}

type Handled = Result<(), Failure>;

// ---- HTTP --------------------------------------------------------------------------------------------------------

/// A stream the doors answer on: plain TCP, or TLS over it.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

type BoxStream = Box<dyn Stream>;
type Reader = BufReader<ReadHalf<BoxStream>>;
type Writer = ConnWriter;

/// `_Refused`: a request refused before it reached a route: the status, and one plain line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refused(u16, String);

enum HeadError {
    Refused(Refused),
    /// asyncio.LimitOverrunError: the head is too long
    Overrun,
    /// the peer went away, or never sent the whole head in time
    Gone,
}

/// One request: `_Request`. No Debug: its headers hold the cookie.
struct Request {
    method: String,
    path: String,
    query: String,
    headers: IndexMap<String, String>,
    peername: SocketAddr,
    host: String,
    device: Option<Device>,
    token: String,
    responded: bool,
}

impl Request {
    /// The TCP peer's address, without its port.
    fn peer(&self) -> String {
        self.peername.ip().to_string()
    }

    fn head_only(&self) -> bool {
        self.method == "HEAD"
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// `_read_head`: (method, path, query, headers) of the next request; field names lowercase.
async fn read_head(reader: &mut Reader, limit_secs: f64) -> Result<(String, String, String, IndexMap<String, String>), HeadError> {
    let head = match tokio::time::timeout(secs(limit_secs), read_until_blank(reader)).await {
        Ok(Ok(Some(head))) => head,
        Ok(Ok(None)) => return Err(HeadError::Overrun),
        Ok(Err(_)) | Err(_) => return Err(HeadError::Gone),
    };
    parse_head(&head[..head.len() - 4]).map_err(HeadError::Refused)
}

/// `reader.readuntil(b"\r\n\r\n")` under asyncio's 64 KiB limit: the head with its blank line; None when the
/// limit is passed (LimitOverrunError); an error when the stream ends first (IncompleteReadError).
async fn read_until_blank(reader: &mut Reader) -> io::Result<Option<Vec<u8>>> {
    let mut head: Vec<u8> = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the head never ended"));
        }
        let from = head.len().saturating_sub(3);
        let before = head.len();
        head.extend_from_slice(chunk);
        let n = chunk.len();
        if let Some(at) = head[from..].windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + from) {
            if at > STREAM_LIMIT {
                return Ok(None);
            }
            let end = at + 4;
            reader.consume(end - before);
            head.truncate(end);
            return Ok(Some(head));
        }
        reader.consume(n);
        if head.len() + 1 - 4 > STREAM_LIMIT {
            return Ok(None);
        }
    }
}

fn parse_head(raw: &[u8]) -> Result<(String, String, String, IndexMap<String, String>), Refused> {
    let text: String = raw.iter().map(|b| *b as char).collect(); // latin-1
    let lines: Vec<&str> = text.split("\r\n").collect();
    let parts: Vec<&str> = lines[0].split(' ').collect();
    if parts.len() != 3 || !matches!(parts[2], "HTTP/1.1" | "HTTP/1.0") || !METHOD_RE.is_match(parts[0]) {
        return Err(Refused(400, "bad request line".into()));
    }
    let (method, target) = (parts[0], parts[1]);
    if target.chars().count() > MAX_TARGET_CHARS {
        return Err(Refused(414, "the address is too long".into()));
    }
    if !target.starts_with('/') || target.chars().any(|c| (c as u32) <= 0x20 || (c as u32) >= 0x7F) {
        return Err(Refused(400, "bad request target".into()));
    }
    if lines.len() - 1 > MAX_HEADERS {
        return Err(Refused(431, "too many header fields".into()));
    }
    let mut headers: IndexMap<String, String> = IndexMap::new();
    for line in &lines[1..] {
        // an obs-fold or a space before the colon lands here too
        let Some((name, value)) = line.split_once(':') else {
            return Err(Refused(400, "bad header field".into()));
        };
        if !FIELD_RE.is_match(name) {
            return Err(Refused(400, "bad header field".into()));
        }
        let name = name.to_lowercase();
        let value = value.trim_matches(|c| c == ' ' || c == '\t');
        // a bare LF, a NUL
        if value.chars().any(|c| ((c as u32) < 0x20 && c != '\t') || c as u32 == 0x7F) {
            return Err(Refused(400, "bad header field".into()));
        }
        if let Some(have) = headers.get_mut(&name) {
            if SINGLE.contains(&name.as_str()) {
                return Err(Refused(400, format!("{name} given twice")));
            }
            have.push_str(if name == "cookie" { "; " } else { ", " });
            have.push_str(value);
        } else {
            headers.insert(name, value.to_string());
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok((method.to_string(), path.to_string(), query.to_string(), headers))
}

fn reason(status: u16) -> &'static str {
    REASONS.iter().find(|(s, _)| *s == status).map_or("OK", |(_, r)| r)
}

/// `_head_bytes`; Err for a header value with a line break (Python's ValueError).
fn head_bytes(status: u16, ctype: &str, length: usize, cache: &str, headers: &[(String, String)]) -> Result<Vec<u8>, Failure> {
    let mut lines = vec![
        format!("HTTP/1.1 {status} {}", reason(status)),
        format!("Content-Type: {ctype}"),
        format!("Content-Length: {length}"),
        format!("Cache-Control: {cache}"),
    ];
    lines.extend(SECURITY_HEADERS.iter().map(|(k, v)| format!("{k}: {v}")));
    for (name, value) in headers {
        if value.contains('\r') || value.contains('\n') {
            return Err(Failure::Error("ValueError"));
        }
        lines.push(format!("{name}: {value}"));
    }
    lines.push("Connection: close".into());
    let text = lines.join("\r\n") + "\r\n\r\n";
    // .encode("latin-1"): a character past U+00FF raises
    if text.chars().any(|c| c as u32 > 0xFF) {
        return Err(Failure::Error("UnicodeEncodeError"));
    }
    Ok(text.chars().map(|c| c as u32 as u8).collect())
}

/// `_AddedHeaders`: the stream LinkChat answers on, with LINK_HEADERS added to its answer's head where it did not
/// set them. LinkChat writes each head whole and then the body; the head is held until its blank line, so a head
/// written in pieces still gets them.
struct ConnWriter {
    inner: WriteHalf<BoxStream>,
    extra: &'static [(&'static str, &'static str)],
    /// None once the head went out
    head: Option<Vec<u8>>,
    pending: Vec<u8>,
    sent: usize,
}

impl ConnWriter {
    /// Writes go straight through until `add_headers`.
    fn new(inner: WriteHalf<BoxStream>) -> Self {
        ConnWriter { inner, extra: &[], head: None, pending: Vec::new(), sent: 0 }
    }

    /// From here on, the next head written is held to its blank line and gets `extra` where it lacks them.
    fn add_headers(&mut self, extra: &'static [(&'static str, &'static str)]) {
        self.extra = extra;
        self.head = Some(Vec::new());
    }

    fn take_head(&mut self, end: usize) {
        let Some(held) = self.head.take() else { return };
        let (head, rest) = held.split_at(end);
        let have: HashSet<Vec<u8>> = head
            .split(|b| *b == b'\n')
            .skip(1)
            .map(|line| {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let name = line.split(|b| *b == b':').next().unwrap_or(line);
                name.trim_ascii().to_ascii_lowercase()
            })
            .collect();
        let mut out = head.to_vec();
        for (name, value) in self.extra {
            if !have.contains(name.to_ascii_lowercase().as_bytes()) {
                out.extend_from_slice(b"\r\n");
                out.extend_from_slice(format!("{name}: {value}").as_bytes());
            }
        }
        out.extend_from_slice(rest);
        self.pending.extend_from_slice(&out);
    }

    /// `flush()`: a head held without its end goes out as it is.
    fn release(&mut self) {
        if let Some(held) = self.head.take() {
            self.pending.extend_from_slice(&held);
        }
    }

    fn poll_pending(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.sent < self.pending.len() {
            let n = match Pin::new(&mut self.inner).poll_write(cx, &self.pending[self.sent..]) {
                Poll::Ready(Ok(n)) => n,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.sent += n;
        }
        self.pending.clear();
        self.sent = 0;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for ConnWriter {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match this.poll_pending(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }
        let Some(head) = this.head.as_mut() else {
            return Pin::new(&mut this.inner).poll_write(cx, buf);
        };
        let from = head.len().saturating_sub(3);
        head.extend_from_slice(buf);
        match head[from..].windows(4).position(|w| w == b"\r\n\r\n") {
            Some(at) => this.take_head(from + at),
            None => {
                if head.len() > MAX_HEAD_HOLD {
                    this.release();
                }
            }
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match this.poll_pending(cx) {
            Poll::Ready(Ok(())) => Pin::new(&mut this.inner).poll_flush(cx),
            other => other,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match this.poll_pending(cx) {
            Poll::Ready(Ok(())) => Pin::new(&mut this.inner).poll_shutdown(cx),
            other => other,
        }
    }
}

// ---- the routes -------------------------------------------------------------------------------------------------

const GET_HEAD: &[&str] = &["GET", "HEAD"];
const GET: &[&str] = &["GET"];
const POST: &[&str] = &["POST"];

/// Which door: the Mac's own page, or the LAN door.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Where {
    Local,
    Lan,
}

impl Where {
    pub fn as_str(self) -> &'static str {
        match self {
            Where::Local => "local",
            Where::Lan => "lan",
        }
    }
}

const LOCAL: &[Where] = &[Where::Local];
const LAN: &[Where] = &[Where::Lan];
const BOTH: &[Where] = &[Where::Local, Where::Lan];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handler {
    Index,
    Health,
    MacLogin,
    OnboardState,
    OpenRouterStart,
    OpenRouterCallback,
    ComposioStart,
    ComposioCallback,
    ComposioSkip,
    PairOpen,
    Pair,
    Devices,
    DevicesRevoke,
    Lan,
    Rtc,
    Apps,
    Status,
    Logout,
    Ca,
    MobileConfig,
    Asset,
    Connect,
    Link,
}

/// `_Route`: its methods, its doors, its handler, whether it needs the door's device cookie (the rest prove
/// themselves), and what a POST carries ("json", or "bytes": an upload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    methods: &'static [&'static str],
    doors: &'static [Where],
    handler: Handler,
    cookie: bool,
    bytes: bool,
}

const fn route(methods: &'static [&'static str], doors: &'static [Where], handler: Handler, cookie: bool) -> Route {
    Route { methods, doors, handler, cookie, bytes: false }
}

const ROUTES: [(&str, Route); 21] = [
    ("/", route(GET_HEAD, BOTH, Handler::Index, false)),
    ("/index.html", route(GET_HEAD, BOTH, Handler::Index, false)),
    (HEALTH_PATH, route(GET_HEAD, BOTH, Handler::Health, false)),
    (MAC_LOGIN_PATH, route(POST, LOCAL, Handler::MacLogin, false)),
    (ONBOARD_STATE_PATH, route(GET, LOCAL, Handler::OnboardState, true)),
    (OPENROUTER_START_PATH, route(GET, LOCAL, Handler::OpenRouterStart, true)),
    (OPENROUTER_CALLBACK_PATH, route(GET, LOCAL, Handler::OpenRouterCallback, false)),
    (COMPOSIO_START_PATH, route(GET, LOCAL, Handler::ComposioStart, true)),
    (COMPOSIO_CALLBACK_PATH, route(GET, LOCAL, Handler::ComposioCallback, false)),
    (COMPOSIO_SKIP_PATH, route(POST, LOCAL, Handler::ComposioSkip, true)),
    (PAIR_OPEN_PATH, route(POST, LOCAL, Handler::PairOpen, true)),
    (PAIR_PATH, route(POST, LAN, Handler::Pair, false)),
    (DEVICES_PATH, route(GET, LOCAL, Handler::Devices, true)),
    (REVOKE_PATH, route(POST, LOCAL, Handler::DevicesRevoke, true)),
    (LAN_PATH, route(POST, LOCAL, Handler::Lan, true)),
    (RTC_PATH, route(POST, BOTH, Handler::Rtc, true)),
    (APPS_PATH, route(GET, BOTH, Handler::Apps, true)),
    (STATUS_PATH, route(GET, BOTH, Handler::Status, true)),
    (LOGOUT_PATH, route(POST, BOTH, Handler::Logout, true)),
    (CA_PATH, route(GET_HEAD, LAN, Handler::Ca, false)),
    (MOBILECONFIG_PATH, route(GET_HEAD, LAN, Handler::MobileConfig, false)),
];
const ASSET_ROUTE: Route = route(GET_HEAD, BOTH, Handler::Asset, false);
const CONNECT_ROUTE: Route = route(POST, BOTH, Handler::Connect, true);
/// LinkChat's paths this door passes on; any other /api/web/link/ path is a 404 here
const LINK_ROUTES: [(&str, Route); 3] = [
    ("turn", route(POST, BOTH, Handler::Link, true)),
    ("messages", route(GET, BOTH, Handler::Link, true)),
    ("upload", Route { methods: POST, doors: BOTH, handler: Handler::Link, cookie: true, bytes: true }),
];
const LINK_FILE_ROUTE: Route = route(GET, BOTH, Handler::Link, true);

impl Route {
    /// The methods it takes, as `Allow` lists them.
    pub fn methods(&self) -> &'static [&'static str] {
        self.methods
    }

    /// Whether `door` has it.
    pub fn on(&self, door: Where) -> bool {
        self.doors.contains(&door)
    }

    /// Whether it needs the door's device cookie.
    pub fn cookie(&self) -> bool {
        self.cookie
    }
}

/// `route_for`: the route a path names, and its argument (an asset's name, LinkChat's rest of the path, a
/// toolkit).
pub fn route_for(path: &str) -> Option<(Route, String)> {
    if let Some((_, r)) = ROUTES.iter().find(|(p, _)| *p == path) {
        return Some((*r, String::new()));
    }
    if let Some(rest) = path.strip_prefix(LINK_PREFIX) {
        if let Some((_, r)) = LINK_ROUTES.iter().find(|(p, _)| *p == rest) {
            return Some((*r, rest.to_string()));
        }
        return LINK_FILE_RE.is_match(rest).then(|| (LINK_FILE_ROUTE, rest.to_string()));
    }
    if let Some(m) = ASSET_RE.captures(path) {
        return Some((ASSET_ROUTE, m[1].to_string()));
    }
    if let Some(m) = APP_CONNECT_RE.captures(path) {
        return Some((CONNECT_ROUTE, m[1].to_string()));
    }
    None
}

// ---- a door ------------------------------------------------------------------------------------------------------

/// `_Door`: what both listeners do: bind, take a connection, read one request, check it (crate docs), route it,
/// answer with `Connection: close`.
struct Door {
    ctx: Arc<WebContext>,
    kind: Where,
    port: AtomicU16,
    /// connections past a cap, closed at once
    refused: AtomicU64,
    /// (LanServer) connections from outside the home network, dropped unread
    dropped: AtomicU64,
    listeners: Mutex<Vec<(SocketAddr, JoinHandle<()>)>>,
    conns: Mutex<HashMap<u64, AbortHandle>>,
    next_conn: AtomicU64,
    /// open connections by the peer's address
    per_peer: Mutex<HashMap<String, usize>>,
    /// the Host allowlist, and when it was read (0: read it again)
    allowed: Mutex<(HashSet<String>, f64)>,
    /// (LanServer) this Mac's LocalHostName and private addresses, as last read
    names: Mutex<(String, Vec<String>)>,
}

impl Door {
    fn new(ctx: Arc<WebContext>, kind: Where) -> Arc<Door> {
        Arc::new(Door {
            ctx,
            kind,
            port: AtomicU16::new(0),
            refused: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            listeners: Mutex::new(Vec::new()),
            conns: Mutex::new(HashMap::new()),
            next_conn: AtomicU64::new(0),
            per_peer: Mutex::new(HashMap::new()),
            allowed: Mutex::new((HashSet::new(), 0.0)),
            names: Mutex::new((String::new(), Vec::new())),
        })
    }

    fn shared(&self) -> &Shared {
        &self.ctx.shared
    }

    fn scheme(&self) -> &'static str {
        match self.kind {
            Where::Local => "http",
            Where::Lan => "https",
        }
    }

    fn cookie_name(&self) -> &'static str {
        match self.kind {
            Where::Local => eve_web_auth::COOKIE_MAC,
            Where::Lan => eve_web_auth::COOKIE_DEV,
        }
    }

    fn signed_out(&self) -> &'static str {
        match self.kind {
            Where::Local => SIGNED_OUT_MAC,
            Where::Lan => SIGNED_OUT_DEV,
        }
    }

    fn running(&self) -> bool {
        !self.listeners.lock().is_empty()
    }

    fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    fn bound(&self) -> Vec<(String, u16)> {
        self.listeners.lock().iter().map(|(a, _)| (a.ip().to_string(), a.port())).collect()
    }

    fn hosts(&self) -> HashSet<String> {
        match self.kind {
            Where::Local => self.allowed.lock().0.clone(),
            Where::Lan => self.lan_hosts(),
        }
    }

    /// LanServer `_hosts`: the certificate's names and the Mac's current private addresses, each with this door's
    /// port; read again after NAMES_TTL_SECS.
    fn lan_hosts(&self) -> HashSet<String> {
        let now = monotonic();
        {
            let allowed = self.allowed.lock();
            if allowed.1 != 0.0 && now - allowed.1 < NAMES_TTL_SECS {
                return allowed.0.clone();
            }
        }
        let parts = self.ctx.parts();
        let mut names: HashSet<String> = HashSet::new();
        match parts.tls.names() {
            Ok(n) => names.extend(n.into_iter().map(|n| n.to_lowercase())),
            Err(e) => {
                // the type only; the addresses still count
                if self.shared().warn_once("names") {
                    warn!("lite web: the certificate's names could not be read ({})", error_name(&e));
                }
            }
        }
        if let Ok(addresses) = (parts.addresses)() {
            names.extend(addresses);
        }
        let port = self.port();
        let out: HashSet<String> =
            names.into_iter().filter(|n| !n.is_empty() && !n.contains(':')).map(|n| format!("{n}:{port}")).collect();
        *self.allowed.lock() = (out.clone(), now);
        out
    }

    // -- the listening sockets --

    fn socket(host: &str, port: u16) -> io::Result<std::net::TcpListener> {
        use socket2::{Domain, Protocol, Socket, Type};
        let ip: IpAddr = host.parse().map_err(|_| io::Error::from_raw_os_error(libc_eaddrnotavail()))?;
        let v6 = host.contains(':');
        let sock = Socket::new(if v6 { Domain::IPV6 } else { Domain::IPV4 }, Type::STREAM, Some(Protocol::TCP))?;
        sock.set_reuse_address(true)?;
        if v6 {
            // "::" is IPv6 only; 0.0.0.0 is beside it
            sock.set_only_v6(true)?;
        }
        sock.bind(&SocketAddr::new(ip, port).into())?;
        sock.listen(128)?;
        sock.set_nonblocking(true)?;
        Ok(sock.into())
    }

    /// `_bind`: listen on every host at the first port all of them can have. A host after the first that this Mac
    /// cannot have at all (no IPv6) is left out; a port taken on any host moves on to the next port. PortsTaken
    /// when no port is free.
    fn bind(self: &Arc<Self>, hosts: &[String], ports: &[u16]) -> Result<u16, PortsTaken> {
        for want in ports {
            let mut got: Vec<std::net::TcpListener> = Vec::new();
            let mut port = *want;
            let mut failed = false;
            for (i, host) in hosts.iter().enumerate() {
                match Self::socket(host, port) {
                    Ok(sock) => {
                        if let Ok(addr) = sock.local_addr() {
                            port = addr.port();
                        }
                        got.push(sock);
                    }
                    Err(e) => {
                        let skip = [libc_eaddrnotavail(), libc_eafnosupport(), libc_eprotonosupport()];
                        if i > 0 && e.raw_os_error().is_some_and(|n| skip.contains(&n)) {
                            continue;
                        }
                        failed = true;
                        break;
                    }
                }
            }
            if failed || got.is_empty() {
                continue;
            }
            let mut ready = Vec::new();
            for sock in got {
                let Ok(addr) = sock.local_addr() else { continue };
                let Ok(listener) = TcpListener::from_std(sock) else { continue };
                ready.push((addr, listener));
            }
            if ready.is_empty() {
                continue;
            }
            // the port (and the Mac page's Host allowlist, which names it) before the first connection is taken, as
            // Python sets both before its loop can run a handler
            self.port.store(port, Ordering::SeqCst);
            if self.kind == Where::Local {
                self.allowed.lock().0 = LOOPBACK_NAMES.iter().map(|name| format!("{name}:{port}")).collect();
            }
            let listeners = ready
                .into_iter()
                .map(|(addr, listener)| (addr, tokio::spawn(Arc::clone(self).accept_loop(listener))))
                .collect();
            *self.listeners.lock() = listeners;
            return Ok(port);
        }
        Err(PortsTaken { errno: libc_eaddrinuse(), count: ports.len() })
    }

    async fn accept_loop(self: Arc<Self>, listener: TcpListener) {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => self.spawn_connection(stream, peer),
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }

    /// `_close`: stop listening, cut the open connections, and wait for them a little (CLOSE_SECS).
    async fn close(&self) {
        let listeners: Vec<(SocketAddr, JoinHandle<()>)> = std::mem::take(&mut *self.listeners.lock());
        for (_, task) in &listeners {
            task.abort();
        }
        for (_, task) in listeners {
            let _ = tokio::time::timeout(secs(CLOSE_SECS), task).await;
        }
        let conns: Vec<AbortHandle> = self.conns.lock().values().cloned().collect();
        for task in &conns {
            task.abort();
        }
        let deadline = Instant::now() + secs(CLOSE_SECS);
        while Instant::now() < deadline && conns.iter().any(|c| !c.is_finished()) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    // -- one connection, one request --

    fn spawn_connection(self: &Arc<Self>, stream: TcpStream, peer: SocketAddr) {
        let id = self.next_conn.fetch_add(1, Ordering::SeqCst);
        let door = Arc::clone(self);
        let mut conns = self.conns.lock();
        let task = tokio::spawn(async move {
            door.connection(id, stream, peer).await;
        });
        conns.insert(id, task.abort_handle());
    }

    async fn connection(self: Arc<Self>, id: u64, stream: TcpStream, peername: SocketAddr) {
        let peer = peername.ip().to_string();
        let limits = self.ctx.parts().limits;
        let over = {
            let open = self.conns.lock().len();
            open > limits.max_connections || self.per_peer.lock().get(&peer).copied().unwrap_or(0) >= limits.max_per_peer
        };
        if over {
            if self.refused.fetch_add(1, Ordering::SeqCst) == 0 {
                warn!(
                    "lite web: too many connections at once ({} in all, {} from one address); new ones are closed",
                    limits.max_connections, limits.max_per_peer
                );
            }
            self.forget(id, None);
            drop(stream);
            return;
        }
        *self.per_peer.lock().entry(peer.clone()).or_insert(0) += 1;
        let guard = ConnGuard { door: Arc::clone(&self), id, peer };
        // LanServer: the peer filter and the TLS handshake
        if let Some(io) = self.admit(stream, &peername).await {
            let (rd, wr) = tokio::io::split(io);
            let mut wr = ConnWriter::new(wr);
            let mut reader = BufReader::new(rd);
            match self.serve_one(&mut reader, &mut wr, peername).await {
                Ok(()) | Err(Failure::Conn(_)) => {}
                Err(Failure::Error(name)) => warn!("lite web: a request failed ({name})"),
            }
            // the counts first, as Python's finally has them: a client that saw the close finds its slot free
            drop(guard);
            let _ = tokio::time::timeout(secs(CLOSE_SECS), wr.shutdown()).await;
            // what the peer sent and nobody read (a refused body) is read off first, as asyncio's buffer had it, so
            // the close is a FIN and not a reset that could beat the answer to the client
            let drain = async {
                let mut buf = [0u8; 8192];
                let mut left = 2 * STREAM_LIMIT;
                while left > 0 {
                    match reader.read(&mut buf).await {
                        Ok(n) if n > 0 => left = left.saturating_sub(n),
                        _ => break,
                    }
                }
            };
            let _ = tokio::time::timeout(secs(CLOSE_SECS), drain).await;
        }
    }

    fn forget(&self, id: u64, peer: Option<&str>) {
        self.conns.lock().remove(&id);
        if let Some(peer) = peer {
            let mut per_peer = self.per_peer.lock();
            let left = per_peer.get(peer).copied().unwrap_or(1).saturating_sub(1);
            if left > 0 {
                per_peer.insert(peer.to_string(), left);
            } else {
                per_peer.remove(peer);
            }
        }
    }

    /// `_admit`: for the LAN door, drop a peer from outside the home network before a byte is read, then TLS.
    async fn admit(&self, stream: TcpStream, peername: &SocketAddr) -> Option<BoxStream> {
        if self.kind == Where::Local {
            return Some(Box::new(stream));
        }
        let parts = self.ctx.parts();
        let allowed = (parts.peer_allowed)(peername).unwrap_or(false); // a peer that cannot be judged is not let in
        if !allowed {
            if self.dropped.fetch_add(1, Ordering::SeqCst) == 0 {
                warn!("lite web: the LAN door dropped a connection from outside this network (said once)");
            }
            return None;
        }
        let config = match parts.tls.context() {
            Ok(c) => c,
            Err(e) => {
                if self.shared().warn_once("tls") {
                    warn!("lite web: the LAN certificate could not be loaded ({}); no phone can connect", error_name(&e));
                }
                return None;
            }
        };
        let acceptor = tokio_rustls::TlsAcceptor::from(config);
        match tokio::time::timeout(secs(TLS_SECS), acceptor.accept(stream)).await {
            Ok(Ok(tls)) => Some(Box::new(tls)),
            // the peer went away, or never spoke TLS: nothing to say
            _ => None,
        }
    }

    async fn serve_one(&self, reader: &mut Reader, writer: &mut Writer, peername: SocketAddr) -> Handled {
        let head_secs = self.ctx.parts().limits.head_secs;
        let (method, path, query, headers) = match read_head(reader, head_secs).await {
            Ok(head) => head,
            Err(HeadError::Refused(Refused(status, line))) => {
                let body = dumps(&json!({"ok": false, "error": line}));
                return send(writer, status, body.as_bytes(), JSON_TYPE, &[], NO_STORE, false).await;
            }
            Err(HeadError::Overrun) => {
                let body = dumps(&json!({"ok": false, "error": "the head is too long"}));
                return send(writer, 431, body.as_bytes(), JSON_TYPE, &[], NO_STORE, false).await;
            }
            Err(HeadError::Gone) => return Ok(()),
        };
        let mut req = Request {
            method,
            path,
            query,
            headers,
            peername,
            host: String::new(),
            device: None,
            token: String::new(),
            responded: false,
        };
        match self.handle(&mut req, reader, writer).await {
            Ok(()) => Ok(()),
            Err(Failure::Conn(e)) => Err(Failure::Conn(e)),
            Err(Failure::Error(name)) => {
                warn!("lite web: a request failed ({name})");
                if !req.responded {
                    self.error(&mut req, writer, 500, FAILED, &[]).await?;
                }
                Ok(())
            }
        }
    }

    async fn handle(&self, req: &mut Request, reader: &mut Reader, writer: &mut Writer) -> Handled {
        let host = py_strip(req.header("host").unwrap_or("")).to_lowercase();
        if !self.hosts().contains(&host) {
            return self.error(req, writer, 421, WRONG_HOST, &[]).await;
        }
        req.host = host.clone();
        let Some((route, arg)) = route_for(&req.path).filter(|(r, _)| r.on(self.kind)) else {
            return self.error(req, writer, 404, NOT_FOUND, &[]).await;
        };
        if !route.methods.contains(&req.method.as_str()) {
            let mut allow: Vec<&str> = route.methods.to_vec();
            allow.sort_unstable();
            return self.error(req, writer, 405, WRONG_METHOD, &[("Allow".into(), allow.join(", "))]).await;
        }
        if !GET_HEAD.contains(&req.method.as_str()) {
            let origin = py_strip(req.header("origin").unwrap_or("")).to_lowercase();
            if origin != format!("{}://{host}", self.scheme()) {
                return self.error(req, writer, 403, WRONG_ORIGIN, &[]).await;
            }
            if req.headers.contains_key("transfer-encoding") {
                return self.error(req, writer, 411, NO_CHUNKED, &[]).await;
            }
            let want = if route.bytes { "application/octet-stream" } else { JSON_TYPE };
            if media(&req.headers) != want {
                return self.error(req, writer, 415, &WRONG_TYPE.replace("{want}", want), &[]).await;
            }
        }
        if route.cookie && !self.signed_in(req, writer).await? {
            return Ok(());
        }
        match route.handler {
            Handler::Index => self.static_file(req, writer, "index.html").await,
            Handler::Asset => self.static_file(req, writer, &format!("assets/{arg}")).await,
            Handler::Health => self.health(req, writer).await,
            Handler::Rtc => self.rtc(req, reader, writer).await,
            Handler::Link => self.link(req, reader, writer, &arg).await,
            Handler::Apps => self.apps(req, writer).await,
            Handler::Connect => self.connect(req, reader, writer, &arg).await,
            Handler::Status => self.status_route(req, writer).await,
            Handler::Logout => self.logout(req, reader, writer).await,
            Handler::MacLogin => self.mac_login(req, reader, writer).await,
            Handler::OnboardState => self.onboard_state(req, writer).await,
            Handler::OpenRouterStart => self.openrouter_start(req, writer).await,
            Handler::OpenRouterCallback => self.openrouter_callback(req, writer).await,
            Handler::ComposioStart => self.composio_start(req, writer).await,
            Handler::ComposioCallback => self.composio_callback(req, writer).await,
            Handler::ComposioSkip => self.composio_skip(req, reader, writer).await,
            Handler::PairOpen => self.pair_open(req, reader, writer).await,
            Handler::Devices => self.devices(req, writer).await,
            Handler::DevicesRevoke => self.devices_revoke(req, reader, writer).await,
            Handler::Lan => self.lan_route(req, reader, writer).await,
            Handler::Pair => self.pair(req, reader, writer).await,
            Handler::Ca => self.download(req, writer, false).await,
            Handler::MobileConfig => self.download(req, writer, true).await,
        }
    }

    /// `_signed_in`: the request's device, from this door's cookie; false after answering 401 (and clearing a
    /// cookie the devices no longer know).
    async fn signed_in(&self, req: &mut Request, writer: &mut Writer) -> Result<bool, Failure> {
        let token = eve_web_auth::cookie_value(req.header("cookie"), self.cookie_name());
        let device = self.check(&token).await;
        let Some(device) = device else {
            let mut headers = Vec::new();
            if !token.is_empty() {
                headers.push(("Set-Cookie".to_string(), clear_cookie(self.cookie_name())?));
            }
            self.error(req, writer, 401, self.signed_out(), &headers).await?;
            return Ok(false);
        };
        req.device = Some(device);
        req.token = token;
        Ok(true)
    }

    async fn check(&self, token: &str) -> Option<Device> {
        if token.is_empty() {
            return None;
        }
        let devices = self.ctx.parts().devices;
        let (token, door) = (token.to_string(), self.kind.as_str());
        tokio::task::spawn_blocking(move || devices.check(&token, Some(door))).await.ok().flatten()
    }

    // -- answering --

    async fn json(&self, req: &mut Request, writer: &mut Writer, status: u16, obj: &Value, headers: &[(String, String)]) -> Handled {
        req.responded = true;
        send(writer, status, dumps(obj).as_bytes(), JSON_TYPE, headers, NO_STORE, req.head_only()).await
    }

    async fn error(&self, req: &mut Request, writer: &mut Writer, status: u16, line: &str, headers: &[(String, String)]) -> Handled {
        self.json(req, writer, status, &json!({"ok": false, "error": line}), headers).await
    }

    async fn page(&self, req: &mut Request, writer: &mut Writer, status: u16, title: &str, line: &str) -> Handled {
        req.responded = true;
        send(writer, status, &page_html(title, line), HTML_TYPE, &[], NO_STORE, req.head_only()).await
    }

    async fn redirect(&self, req: &mut Request, writer: &mut Writer, status: u16, location: &str) -> Handled {
        req.responded = true;
        let headers = [("Location".to_string(), location.to_string())];
        send(writer, status, b"", "text/plain; charset=utf-8", &headers, NO_STORE, false).await
    }

    /// `_body`: the request's JSON object ({} for no body); None once a refusal is answered.
    async fn body(&self, req: &mut Request, reader: &mut Reader, writer: &mut Writer) -> Result<Option<Map<String, Value>>, Failure> {
        let raw = py_strip(req.header("content-length").unwrap_or("")).to_string();
        let raw = if raw.is_empty() { "0".to_string() } else { raw };
        // "²".isdigit() is True too, and int() of it raises
        if !raw.bytes().all(|b| b.is_ascii_digit()) {
            self.error(req, writer, 400, BAD_BODY, &[]).await?;
            return Ok(None);
        }
        let size: usize = raw.trim_start_matches('0').parse().unwrap_or(if raw.trim_start_matches('0').is_empty() { 0 } else { usize::MAX });
        if size > MAX_JSON_BYTES {
            self.error(req, writer, 413, TOO_LARGE, &[]).await?;
            return Ok(None);
        }
        if size == 0 {
            return Ok(Some(Map::new()));
        }
        let mut data = vec![0u8; size];
        match tokio::time::timeout(secs(BODY_SECS), reader.read_exact(&mut data)).await {
            Ok(Ok(_)) => {}
            _ => {
                self.error(req, writer, 400, BAD_BODY, &[]).await?;
                return Ok(None);
            }
        }
        match eve_apps_mcp::py::json_loads_bytes(&data) {
            Ok(Value::Object(m)) => Ok(Some(m)),
            _ => {
                self.error(req, writer, 400, BAD_BODY, &[]).await?;
                Ok(None)
            }
        }
    }

    /// `_cookie_again`: the device's cookie set again, so it lasts MAX_AGE from this visit, not from the pairing.
    fn cookie_again(&self, req: &Request) -> Result<(String, String), Failure> {
        Ok(("Set-Cookie".to_string(), set_cookie(self.cookie_name(), &req.token)?))
    }

    // -- the daemon, the key, the apps --

    /// `_daemon`: the daemon's state, read field by field: nothing else in its mapping reaches a page.
    async fn daemon(&self) -> Daemon {
        let parts = self.ctx.parts();
        let raw = match &parts.status {
            Some(status) => match status().await {
                Ok(v) => v,
                Err(e) => {
                    if self.shared().warn_once("status") {
                        warn!("lite web: the daemon's status could not be read ({})", error_name(&e));
                    }
                    Value::Object(Map::new())
                }
            },
            None => Value::Object(Map::new()),
        };
        let raw = raw.as_object().cloned().unwrap_or_default();
        let key = parts.environ.get(eve_openrouter::KEY_ENV).unwrap_or_default();
        let keyed = !py_strip(&key).is_empty() || raw.get("openrouter") == Some(&Value::Bool(true));
        let state = match raw.get("state") {
            Some(Value::String(s)) if STATE_RE.is_match(s) => s.clone(),
            _ => (if keyed { READY } else { NEEDS_OPENROUTER }).to_string(),
        };
        let voice = match raw.get("voice") {
            Some(Value::Bool(b)) => *b,
            // a hub that cannot say has no voice
            _ => parts.rtc.as_ref().is_some_and(|rtc| rtc.voice_ready().unwrap_or(false)),
        };
        Daemon { state, openrouter: keyed, voice }
    }

    /// `_credit`: what the saved key can spend (onboarding.credit, through credit_view), read at most every
    /// CREDIT_TTL_SECS and shared by every page asking at once. A page waits CREDIT_WAIT_SECS at most: past that it
    /// gets the last figure known (or null), and the read lands for the next page. None without a key.
    async fn credit(&self) -> Option<Value> {
        let parts = self.ctx.parts();
        let key = parts.environ.get(eve_openrouter::KEY_ENV).unwrap_or_default();
        let key = py_strip(&key).to_string();
        if key.is_empty() {
            return None;
        }
        let shared = self.shared();
        let digest = hex::encode(Sha256::digest(key.as_bytes()));
        let kept = shared.credit.lock().clone().filter(|(d, _, _)| *d == digest);
        if let Some((_, when, view)) = &kept {
            if monotonic() - when < CREDIT_TTL_SECS {
                return Some(view.clone());
            }
        }
        let task = {
            let mut slot = shared.credit_task.lock();
            match &*slot {
                Some((for_, task, running)) if *for_ == digest && !running.is_finished() => task.clone(),
                _ => {
                    let ctx = Arc::clone(&self.ctx);
                    let (credit, view) = (parts.credit.clone(), parts.credit_view);
                    let d = digest.clone();
                    let handle = tokio::spawn(async move {
                        let raw = match credit(key).await {
                            Ok(raw) => raw,
                            // onboarding.credit never fails; a stand-in might
                            Err(e) => json!({"ok": false, "usd": null, "funded": null, "error": error_name(&e)}),
                        };
                        let view = view(&raw).unwrap_or_else(|| json!({"ok": false, "usd": null, "funded": null}));
                        *ctx.shared.credit.lock() = Some((d, monotonic(), view.clone()));
                        view
                    });
                    let running = handle.abort_handle();
                    let task: CreditTask = async move {
                        handle.await.unwrap_or_else(|_| json!({"ok": false, "usd": null, "funded": null}))
                    }
                    .boxed()
                    .shared();
                    *slot = Some((digest, task.clone(), running));
                    task
                }
            }
        };
        match tokio::time::timeout(secs(parts.limits.credit_wait_secs), task).await {
            Ok(view) => Some(view),
            Err(_) => kept.map(|(_, _, view)| view),
        }
    }

    /// `_composio_signed_in`: composio-connect.json exists: onboarding writes it only once a token arrived.
    fn composio_signed_in(&self) -> bool {
        let parts = self.ctx.parts();
        parts.composio_login.as_ref().and_then(|l| l.token_path()).is_some_and(|p| p.is_file())
    }

    /// `_composio_live`: signed in, and not one the bridge says only a new sign-in helps. No bridge yet (eve
    /// restarting into it) counts as live.
    fn composio_live(&self) -> bool {
        if !self.composio_signed_in() {
            return false;
        }
        match &self.ctx.parts().apps {
            None => true,
            // a bridge that cannot say is taken as signed in
            Some(bridge) => !bridge.needs_sign_in().unwrap_or(false),
        }
    }

    fn lan_on(&self) -> bool {
        self.shared().lan().is_some_and(|lan| lan.running())
    }

    /// `GET /api/web/status`: booleans, words and the credit figure. Built field by field, so no key, token or
    /// code can reach it.
    async fn status(&self) -> Value {
        let daemon = self.daemon().await;
        let credit = if daemon.openrouter && !self.shared().restarting() { self.credit().await } else { None };
        json!({
            "state": daemon.state, "openrouter": daemon.openrouter, "openrouter_rejected": key_rejected(credit.as_ref()),
            "composio": self.composio_live(), "voice": daemon.voice, "lan": self.lan_on(),
            "local_network": self.shared().local_network(), "restarting": self.shared().restarting(),
            "credit": credit.unwrap_or(Value::Null),
        })
    }

    /// `_close_peers`: those devices' WebRTC peers close now, with their requests and calls.
    async fn close_peers(&self, device_ids: &[String]) {
        let rtc = self.ctx.parts().rtc;
        for device_id in device_ids {
            self.shared().rtc_doors.lock().shift_remove(device_id);
            let Some(rtc) = &rtc else { continue };
            if let Err(e) = rtc.close_device(device_id).await {
                // the type only; the revoke itself stands
                warn!("lite web: a revoked device's link did not close ({})", error_name(&e));
            }
        }
    }

    async fn revoke_ids(&self, id_or_all: &str) -> io::Result<Vec<String>> {
        let devices = self.ctx.parts().devices;
        let target = id_or_all.to_string();
        let gone = tokio::task::spawn_blocking(move || devices.revoke(&target))
            .await
            .map_err(|_| io::Error::other("the revoke did not run"))??;
        self.close_peers(&gone).await;
        Ok(gone)
    }

    /// `_probe_soon`: ask in the background whether macOS lets eve reach the device (lan_net.probe), for the status
    /// line.
    fn probe_soon(&self, peername: SocketAddr) {
        let probe = self.ctx.parts().probe;
        let ctx = Arc::clone(&self.ctx);
        self.shared().spawn(async move {
            let word = tokio::task::spawn_blocking(move || probe(&peername))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_else(|| "unknown".to_string());
            if word == "ok" || word == "blocked" {
                *ctx.shared.local_network.lock() = word;
            }
        });
    }

    // -- sign in: a browser gets its cookie --

    /// `_sign_in`: answer {ok} with this door's cookie: the browser's own device when its cookie is still good
    /// (opening Eve again, scanning again), else a new one.
    async fn sign_in(&self, req: &mut Request, writer: &mut Writer) -> Handled {
        let mut token = eve_web_auth::cookie_value(req.header("cookie"), self.cookie_name());
        if self.check(&token).await.is_none() {
            let devices = self.ctx.parts().devices;
            let label = device_label(req.header("user-agent"), self.kind.as_str());
            let host = host_name(&req.host);
            let door = self.kind.as_str();
            let added = tokio::task::spawn_blocking(move || devices.add(&label, Some(&host), door))
                .await
                .map_err(|_| io::Error::other("the device was not added"))
                .and_then(|r| r);
            match added {
                Ok((_, t)) => token = t,
                Err(e) => {
                    warn!("lite web: a device could not be saved ({})", os_error_name(&e));
                    return self.error(req, writer, 500, DEVICE_NOT_SAVED, &[]).await;
                }
            }
            info!("lite web: {} signed in", if self.kind == Where::Local { "the Mac's browser" } else { "a device paired" });
        }
        let cookie = set_cookie(self.cookie_name(), &token)?;
        self.json(req, writer, 200, &json!({"ok": true}), &[("Set-Cookie".into(), cookie)]).await
    }

    // -- the routes both doors have --

    async fn static_file(&self, req: &mut Request, writer: &mut Writer, rel: &str) -> Handled {
        let root = self.ctx.web_root();
        let ctype = content_type(rel);
        let squeezable = GZIP_TYPES.iter().any(|t| ctype.starts_with(t));
        let compress = squeezable && accepts_gzip(req.header("accept-encoding").unwrap_or(""));
        let ctx = Arc::clone(&self.ctx);
        let path = root.join(rel);
        let found = tokio::task::spawn_blocking(move || ctx.shared.files.read(&path, &root, compress)).await.ok().flatten();
        let page = rel == "index.html";
        let Some((body, zipped)) = found else {
            if page {
                if self.shared().warn_once("not-built") {
                    warn!("lite web: the web page is not installed (no index.html where it is looked for)");
                }
                return self.page(req, writer, 503, "eve", NOT_BUILT).await;
            }
            return self.error(req, writer, 404, NOT_FOUND, &[]).await;
        };
        let mut headers: Vec<(String, String)> = Vec::new();
        if squeezable {
            headers.push(("Vary".into(), "Accept-Encoding".into()));
        }
        if zipped {
            headers.push(("Content-Encoding".into(), "gzip".into()));
        }
        req.responded = true;
        send(writer, 200, &body, ctype, &headers, if page { NO_STORE } else { IMMUTABLE }, req.head_only()).await
    }

    async fn health(&self, req: &mut Request, writer: &mut Writer) -> Handled {
        let daemon = self.daemon().await;
        self.json(req, writer, 200, &json!({"ok": true, "state": daemon.state}), &[]).await
    }

    async fn rtc(&self, req: &mut Request, reader: &mut Reader, writer: &mut Writer) -> Handled {
        let Some(body) = self.body(req, reader, writer).await? else { return Ok(()) };
        let Some(rtc) = self.ctx.parts().rtc else {
            return self.error(req, writer, 503, RTC_OFF, &[]).await;
        };
        let device_id = req.device.as_ref().map(|d| d.id.clone()).unwrap_or_default();
        let sdp = body.get("sdp").cloned().unwrap_or(Value::Null);
        let answer = match tokio::time::timeout(secs(RTC_SECS), rtc.answer(&device_id, &sdp, &req.peer())).await {
            Err(_) => return self.error(req, writer, 504, RTC_SLOW, &[]).await,
            // lite_rtc.RtcError: an offer this Mac will not answer
            Ok(Err(RtcFailure::Refused(line))) => return self.error(req, writer, 400, &plain(&line, OFFER_REFUSED), &[]).await,
            Ok(Err(RtcFailure::Failed(_))) => return Err(Failure::Error("Exception")),
            Ok(Ok(answer)) => answer,
        };
        let Some(Value::String(sdp)) = answer.get("sdp") else {
            return self.error(req, writer, 500, OFFER_REFUSED, &[]).await;
        };
        let sdp = sdp.clone();
        self.shared().rtc_doors.lock().insert(device_id, self.kind.as_str().to_string());
        if self.kind == Where::Lan {
            self.probe_soon(req.peername);
        }
        let voice = answer.get("voice") == Some(&Value::Bool(true));
        self.json(req, writer, 200, &json!({"sdp": sdp, "voice": voice}), &[]).await
    }

    /// `_link`: LinkChat, unchanged, on the real stream: Origin and the cookie removed, the nonce as its Bearer
    /// token.
    async fn link(&self, req: &mut Request, reader: &mut Reader, writer: &mut Writer, rest: &str) -> Handled {
        let Some(link) = self.ctx.parts().link else {
            return self.error(req, writer, 503, NOT_READY, &[]).await;
        };
        let mut headers: eve_link::seams::Headers = req
            .headers
            .iter()
            .filter(|(name, _)| !LINK_DROPPED.contains(&name.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        headers.insert("authorization".into(), format!("Bearer {}", self.shared().nonce));
        let nonce = self.shared().nonce.clone();
        let check = move |token: &str| eve_lite_rtc::py::compare_digest(token.as_bytes(), nonce.as_bytes());
        req.responded = true;
        writer.add_headers(&LINK_HEADERS);
        let served = link.serve(&req.method, &format!("/api/link/{rest}"), &req.query, &headers, reader, writer, &check).await;
        writer.release();
        let flushed = writer.flush().await;
        served?;
        flushed?;
        Ok(())
    }

    /// `_apps`: Settings → Apps: {composio, apps: [{toolkit, name, connected}]} in composio_connect.APPS's order.
    async fn apps(&self, req: &mut Request, writer: &mut Writer) -> Handled {
        let bridge = self.ctx.parts().apps;
        if !self.composio_live() {
            return self.json(req, writer, 200, &json!({"composio": false, "apps": []}), &[]).await;
        }
        let mut connected: IndexMap<String, bool> = IndexMap::new();
        if let Some(bridge) = bridge {
            match tokio::time::timeout(secs(APPS_SECS), bridge.connections(None)).await {
                Err(_) => return self.error(req, writer, 504, COMPOSIO_SLOW, &[]).await,
                Ok(Err(e)) => return Err(Failure::Error(error_name(&e))),
                Ok(Ok(c)) => connected = c,
            }
        }
        let apps: Vec<Value> = eve_composio_connect::APPS
            .iter()
            .map(|(slug, name)| json!({"toolkit": slug, "name": name, "connected": connected.get(*slug) == Some(&true)}))
            .collect();
        self.json(req, writer, 200, &json!({"composio": true, "apps": apps}), &[]).await
    }

    /// `_connect`: the Connect Link for one app: {ok, url}, `https` on composio.dev only (composio_connect.link_ok).
    async fn connect(&self, req: &mut Request, reader: &mut Reader, writer: &mut Writer, toolkit: &str) -> Handled {
        if self.body(req, reader, writer).await?.is_none() {
            return Ok(());
        }
        let slug = match eve_composio_connect::toolkit_slug_str(toolkit) {
            Some(slug) if !eve_composio_tools::WEB_TOOLKITS.contains(&slug.as_str()) => slug,
            _ => return self.error(req, writer, 404, NOT_AN_APP, &[]).await,
        };
        let bridge = self.ctx.parts().apps;
        if !self.composio_live() {
            return self.error(req, writer, 409, COMPOSIO_OUT, &[]).await;
        }
        let Some(bridge) = bridge else {
            // signed in, and eve restarting into the bridge
            return self.error(req, writer, 503, COMPOSIO_DOWN, &[]).await;
        };
        let name = eve_composio_connect::APPS.iter().find(|(s, _)| *s == slug).map_or(slug.clone(), |(_, n)| n.to_string());
        let url = match tokio::time::timeout(secs(APPS_SECS), bridge.connect_link(&slug)).await {
            Err(_) => return self.error(req, writer, 504, COMPOSIO_SLOW, &[]).await,
            Ok(Err(e)) => return Err(Failure::Error(error_name(&e))),
            Ok(Ok(url)) => url,
        };
        if let Some(url) = url.filter(|u| eve_composio_connect::link_ok_str(u)) {
            info!("lite web: a Connect Link went to a page");
            return self.json(req, writer, 200, &json!({"ok": true, "url": url}), &[]).await;
        }
        // no link: most often the app is connected already (connect_link remembered it)
        let known = match tokio::time::timeout(secs(CONNECTED_SECS), bridge.connections(Some(&[slug.as_str()]))).await {
            Ok(Ok(known)) => known,
            _ => IndexMap::new(),
        };
        if known.get(&slug) == Some(&true) {
            let line = ALREADY.replace("{name}", &name);
            return self.json(req, writer, 409, &json!({"ok": false, "error": line, "connected": true}), &[]).await;
        }
        self.error(req, writer, 502, &NO_LINK.replace("{name}", &name), &[]).await
    }

    async fn status_route(&self, req: &mut Request, writer: &mut Writer) -> Handled {
        let status = self.status().await;
        let again = self.cookie_again(req)?;
        self.json(req, writer, 200, &status, &[again]).await
    }

    /// `_logout`: this browser signs out: its device is revoked (its peer closes) and its cookie cleared.
    async fn logout(&self, req: &mut Request, reader: &mut Reader, writer: &mut Writer) -> Handled {
        if self.body(req, reader, writer).await?.is_none() {
            return Ok(());
        }
        let me = req.device.as_ref().map(|d| d.id.clone()).unwrap_or_default();
        if let Err(e) = self.revoke_ids(&me).await {
            warn!("lite web: a sign-out was not saved ({})", os_error_name(&e));
            return self.error(req, writer, 500, NOT_SAVED, &[]).await;
        }
        let cleared = clear_cookie(self.cookie_name())?;
        self.json(req, writer, 200, &json!({"ok": true}), &[("Set-Cookie".into(), cleared)]).await
    }

    // -- the routes only the Mac's page has --

    /// `_mac_login`: `eve-bridge web open`'s one-time code, from the page's #login= fragment: eve_mac for this
    /// browser.
    async fn mac_login(&self, req: &mut Request, reader: &mut Reader, writer: &mut Writer) -> Handled {
        let Some(body) = self.body(req, reader, writer).await? else { return Ok(()) };
        if !self.ctx.parts().mac_login.consume(body.get("code").and_then(Value::as_str)) {
            return self.error(req, writer, 403, LOGIN_SPENT, &[]).await;
        }
        self.sign_in(req, writer).await
    }

    /// `_onboard_state`: the setup page's state: booleans, the credit figure and one plain line.
    async fn onboard_state(&self, req: &mut Request, writer: &mut Writer) -> Handled {
        let daemon = self.daemon().await;
        let restarting = self.shared().restarting();
        let credit = if daemon.openrouter && !restarting { self.credit().await } else { None };
        let error = self.shared().error();
        let state = json!({
            "openrouter": daemon.openrouter, "openrouter_rejected": key_rejected(credit.as_ref()),
            "credit": credit.unwrap_or(Value::Null), "composio": self.composio_signed_in(),
            "composio_skipped": self.shared().saved.composio_skipped(), "restarting": restarting,
            "error": if error.is_empty() { String::new() } else { plain(&error, SIGNIN_BROKE) },
        });
        let again = self.cookie_again(req)?;
        self.json(req, writer, 200, &state, &[again]).await
    }

    async fn openrouter_start(&self, req: &mut Request, writer: &mut Writer) -> Handled {
        let Some(login) = self.ctx.parts().openrouter else {
            return self.page(req, writer, 503, "eve", NO_SIGNIN).await;
        };
        let url = match login.start(&format!("http://{}{OPENROUTER_CALLBACK_PATH}", req.host)) {
            Ok(url) => url,
            Err(e) => {
                warn!("lite web: the OpenRouter sign-in did not start ({})", sign_in_error_name(&e));
                return self.page(req, writer, 500, "eve", SIGNIN_BROKE).await;
            }
        };
        self.shared().error.lock().clear();
        self.redirect(req, writer, 302, &url).await
    }

    async fn composio_start(&self, req: &mut Request, writer: &mut Writer) -> Handled {
        let Some(login) = self.ctx.parts().composio_login else {
            return self.page(req, writer, 503, "eve", NO_SIGNIN).await;
        };
        let url = match login.start(&format!("http://{}{COMPOSIO_CALLBACK_PATH}", req.host)).await {
            Ok(url) => url,
            Err(SignInError::Onboarding(e)) => return self.signin_failed(req, writer, "Composio", e.status, &e.message).await,
            Err(e) => {
                warn!("lite web: the Composio sign-in did not start ({})", sign_in_error_name(&e));
                return self.signin_failed(req, writer, "Composio", 500, SIGNIN_BROKE).await;
            }
        };
        self.shared().error.lock().clear();
        self.redirect(req, writer, 302, &url).await
    }

    /// `_openrouter_callback`: OpenRouter's redirect back, checked by its single-use state alone: the key is
    /// exchanged and saved.
    async fn openrouter_callback(&self, req: &mut Request, writer: &mut Writer) -> Handled {
        let parts = self.ctx.parts();
        let Some(login) = parts.openrouter.clone() else {
            return self.page(req, writer, 503, "eve", NO_SIGNIN).await;
        };
        let q = query(&req.query);
        let field = |name: &str| q.get(name).cloned().unwrap_or_default();
        let key = match login.finish(&field("code"), &field("state"), &field("error")).await {
            Ok(key) => key,
            Err(e) => return self.signin_failed(req, writer, "OpenRouter", e.status, &e.message).await,
        };
        let (environ, env_path) = (parts.environ.clone(), parts.env_path.clone());
        let saved = tokio::task::spawn_blocking(move || environ.save_openrouter_key(&key, env_path.as_deref())).await;
        match saved {
            Ok(Ok(())) => {}
            Ok(Err(SignInError::Onboarding(e))) => {
                return self.signin_failed(req, writer, "OpenRouter", e.status, &e.message).await;
            }
            Ok(Err(e)) => {
                // the type only: never the key
                warn!("lite web: the OpenRouter sign-in failed ({})", sign_in_error_name(&e));
                return self.signin_failed(req, writer, "OpenRouter", 500, SIGNIN_BROKE).await;
            }
            Err(_) => {
                warn!("lite web: the OpenRouter sign-in failed (RuntimeError)");
                return self.signin_failed(req, writer, "OpenRouter", 500, SIGNIN_BROKE).await;
            }
        }
        self.shared().forget_credit();
        self.signed_up(req, writer, "OpenRouter").await
    }

    /// `_composio_callback`: Composio's redirect back, checked by its single-use state alone: the token is
    /// exchanged and saved.
    async fn composio_callback(&self, req: &mut Request, writer: &mut Writer) -> Handled {
        let Some(login) = self.ctx.parts().composio_login else {
            return self.page(req, writer, 503, "eve", NO_SIGNIN).await;
        };
        let q = query(&req.query);
        let field = |name: &str| q.get(name).cloned().unwrap_or_default();
        if let Err(e) = login.finish(&field("code"), &field("state"), &field("error")).await {
            return self.signin_failed(req, writer, "Composio", e.status, &e.message).await;
        }
        self.signed_up(req, writer, "Composio").await
    }

    /// `_signed_up`: a credential is saved: back to the setup page (the code leaves the address bar), then a
    /// restart into it.
    async fn signed_up(&self, req: &mut Request, writer: &mut Writer, provider: &str) -> Handled {
        self.shared().error.lock().clear();
        info!("lite web: {provider} signed in; eve restarts");
        self.redirect(req, writer, 303, "/").await?;
        self.restart_soon();
        Ok(())
    }

    /// `_signin_failed`: the provider's status, a short page with the line, and the same line on the setup page.
    async fn signin_failed(&self, req: &mut Request, writer: &mut Writer, provider: &str, status: i64, line: &str) -> Handled {
        let text = plain(line, SIGNIN_BROKE);
        *self.shared().error.lock() = text.clone();
        info!("lite web: the {provider} sign-in did not finish (HTTP {status})");
        let code = if (400..600).contains(&status) { status as u16 } else { 500 };
        self.page(req, writer, code, "Sign-in did not finish", &text).await
    }

    /// `_restart_soon`: ask the daemon to restart in `restart_delay` seconds, once: the page has loaded by then and
    /// shows that eve is restarting (`restarting`) until the new one answers.
    fn restart_soon(&self) {
        if self.shared().restarting.swap(true, Ordering::SeqCst) {
            return;
        }
        let parts = self.ctx.parts();
        let (delay, restart) = (parts.restart_delay, parts.restart.clone());
        let ctx = Arc::clone(&self.ctx);
        self.shared().spawn(async move {
            tokio::time::sleep(secs(delay.max(0.0))).await;
            let Some(restart) = restart else { return };
            if let Err(e) = restart().await {
                warn!("lite web: eve did not restart ({}); the sign-in counts from the next start", restart_error_name(&e));
                ctx.shared.restarting.store(false, Ordering::SeqCst);
            }
        });
    }

    async fn composio_skip(&self, req: &mut Request, reader: &mut Reader, writer: &mut Writer) -> Handled {
        if self.body(req, reader, writer).await?.is_none() {
            return Ok(());
        }
        let ctx = Arc::clone(&self.ctx);
        let saved = tokio::task::spawn_blocking(move || ctx.shared.saved.save(&[Change::ComposioSkipped(true)]))
            .await
            .unwrap_or(false);
        if !saved {
            return self.error(req, writer, 500, NOT_SAVED, &[]).await;
        }
        self.json(req, writer, 200, &json!({"ok": true}), &[]).await
    }

    /// `_pair_open`: Add a phone: the LAN door on, the Local Network alert, a fresh window, its links as QR codes.
    async fn pair_open(&self, req: &mut Request, reader: &mut Reader, writer: &mut Writer) -> Handled {
        if self.body(req, reader, writer).await?.is_none() {
            return Ok(());
        }
        let was_on = self.lan_on();
        let opened = async {
            let lan = self.set_lan(true).await?.ok_or_else(|| StartError::Runtime("no LAN door".into()))?;
            // a door that was already open names the Mac as it is now: a new address reissues the certificate
            let names = if was_on { lan.0.refresh_names().await? } else { lan.0.names.lock().clone() };
            Ok::<_, StartError>((lan, names))
        }
        .await;
        let (lan, (hostname, addresses)) = match opened {
            Ok(v) => v,
            Err(StartError::PortsTaken(e)) => {
                let _ = e;
                warn!("lite web: the LAN door did not open (PortsTaken)");
                return self.error(req, writer, 503, LAN_FAILED, &[]).await;
            }
            Err(e) if e.is_os_or_value() => {
                // the certificate could not be made or loaded
                warn!("lite web: the LAN certificate is not usable ({})", e.name());
                return self.error(req, writer, 503, TLS_OFF, &[]).await;
            }
            Err(_) => return Err(Failure::Error("RuntimeError")),
        };
        let parts = self.ctx.parts();
        let alert = parts.alert.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(e) = alert() {
                // the type only; a hook that fails must not fail the request
                info!("lite web: trigger_local_network_alert failed ({})", error_name(&e));
            }
        })
        .await;
        let (secret, code) = parts.pairing.open();
        let port = lan.port();
        let mut links: Vec<(&str, &str, String)> = Vec::new();
        if !hostname.is_empty() {
            links.push(("apple", "iPhone or iPad", format!("https://{}.local:{port}/#p={secret}", hostname.to_lowercase())));
        }
        if let Some(first) = addresses.first() {
            links.push(("other", "Android or other", format!("https://{first}:{port}/#p={secret}")));
        }
        if links.is_empty() {
            parts.pairing.close();
            return self.error(req, writer, 503, NO_ADDRESS, &[]).await;
        }
        let urls: Vec<String> = links.iter().map(|(_, _, u)| u.clone()).collect();
        let codes = tokio::task::spawn_blocking(move || urls.iter().map(|u| qr_svg(u)).collect::<Vec<_>>())
            .await
            .map_err(|_| Failure::Error("RuntimeError"))?;
        let mut payload = Map::new();
        payload.insert("ok".into(), json!(true));
        payload.insert("code".into(), json!(code));
        let mut shown = Vec::new();
        for ((kind, label, url), qr) in links.iter().zip(codes) {
            payload.insert(format!("url_{kind}"), json!(url));
            payload.insert(format!("qr_{kind}"), json!(qr));
            shown.push(json!({"label": label, "url": url, "qr": qr}));
        }
        let left = parts.pairing.remaining();
        payload.insert("expires".into(), json!((now_epoch() + left) as i64));
        payload.insert("expires_in".into(), json!(left as i64));
        payload.insert("links".into(), Value::Array(shown));
        info!("lite web: a pairing window opened ({} links)", links.len());
        self.json(req, writer, 200, &Value::Object(payload), &[]).await
    }

    async fn devices(&self, req: &mut Request, writer: &mut Writer) -> Handled {
        let devices = self.ctx.parts().devices;
        let rows = tokio::task::spawn_blocking(move || devices.list()).await.map_err(|_| Failure::Error("RuntimeError"))?;
        let me = req.device.as_ref().map(|d| d.id.clone()).unwrap_or_default();
        let listed: Vec<Value> = rows
            .iter()
            .map(|row| {
                let mut v = serde_json::to_value(row).unwrap_or_else(|_| json!({}));
                if let Some(m) = v.as_object_mut() {
                    m.insert("current".into(), json!(row.id == me));
                }
                v
            })
            .collect();
        self.json(req, writer, 200, &json!({"devices": listed}), &[]).await
    }

    async fn devices_revoke(&self, req: &mut Request, reader: &mut Reader, writer: &mut Writer) -> Handled {
        let Some(body) = self.body(req, reader, writer).await? else { return Ok(()) };
        let target = match body.get("id") {
            Some(Value::String(t)) if t == "all" || eve_web_auth::DEVICE_ID.is_match(t) => t.clone(),
            _ => return self.error(req, writer, 400, DEVICE_ID, &[]).await,
        };
        match self.revoke_ids(&target).await {
            Ok(gone) => self.json(req, writer, 200, &json!({"ok": true, "revoked": gone}), &[]).await,
            Err(e) => {
                warn!("lite web: a revoke was not saved ({})", os_error_name(&e));
                self.error(req, writer, 500, NOT_SAVED, &[]).await
            }
        }
    }

    async fn lan_route(&self, req: &mut Request, reader: &mut Reader, writer: &mut Writer) -> Handled {
        let Some(body) = self.body(req, reader, writer).await? else { return Ok(()) };
        let Some(Value::Bool(on)) = body.get("on").cloned() else {
            return self.error(req, writer, 400, LAN_FLAG, &[]).await;
        };
        match self.set_lan(on).await {
            Ok(_) => {}
            Err(StartError::PortsTaken(_)) => {
                warn!("lite web: the LAN door did not open (PortsTaken)");
                return self.error(req, writer, 503, LAN_FAILED, &[]).await;
            }
            Err(e) if e.is_os_or_value() => {
                warn!("lite web: the LAN certificate is not usable ({})", e.name());
                return self.error(req, writer, 503, TLS_OFF, &[]).await;
            }
            Err(_) => return Err(Failure::Error("RuntimeError")),
        }
        if self.shared().saved.lan() != on {
            // the door is as asked for now, but web.json still says otherwise: the next start would undo it
            return self.error(req, writer, 500, NOT_SAVED, &[]).await;
        }
        let lan = self.lan_on();
        self.json(req, writer, 200, &json!({"ok": true, "lan": lan}), &[]).await
    }

    /// LocalServer `set_lan`: turn the LAN door on (start it if it is not running) or off, and remember it in
    /// web.json. Off also closes a pairing window and the WebRTC peers of the devices that came through it.
    async fn set_lan(&self, on: bool) -> Result<Option<LanServer>, StartError> {
        let shared = self.shared();
        let _held = shared.lan_lock.lock().await;
        let lan = shared.lan();
        if on {
            let lan = match lan {
                Some(lan) => lan,
                None => {
                    let lan = LanServer::new(Arc::clone(&self.ctx));
                    *shared.lan.lock() = Some(lan.clone());
                    lan
                }
            };
            if !lan.running() {
                lan.start().await?;
            }
            if !shared.saved.lan() {
                let ctx = Arc::clone(&self.ctx);
                let _ = tokio::task::spawn_blocking(move || ctx.shared.saved.save(&[Change::Lan(true)])).await;
            }
            return Ok(Some(lan));
        }
        if let Some(lan) = lan.filter(|l| l.running()) {
            lan.stop().await;
        }
        self.ctx.parts().pairing.close();
        if shared.saved.lan() {
            let ctx = Arc::clone(&self.ctx);
            let _ = tokio::task::spawn_blocking(move || ctx.shared.saved.save(&[Change::Lan(false)])).await;
        }
        let lan_ids: Vec<String> =
            shared.rtc_doors.lock().iter().filter(|(_, door)| *door == "lan").map(|(dev, _)| dev.clone()).collect();
        self.close_peers(&lan_ids).await;
        Ok(None)
    }

    // -- the routes only the LAN door has --

    /// `_pair`: a phone or laptop pairs with the window's secret (the QR code's #p=) or its 6-digit code: eve_dev.
    async fn pair(&self, req: &mut Request, reader: &mut Reader, writer: &mut Writer) -> Handled {
        let Some(body) = self.body(req, reader, writer).await? else { return Ok(()) };
        let window = self.ctx.parts().pairing;
        let left = window.remaining();
        if left <= 0.0 {
            return self.error(req, writer, 403, NO_WINDOW, &[]).await;
        }
        if !window.pair(body.get("secret"), body.get("code")) {
            if window.is_open() {
                return self.error(req, writer, 403, WRONG_CODE, &[]).await;
            }
            // that try closed it, or it ran out meanwhile
            let (status, line) = if left > 1.0 { (429, TOO_MANY) } else { (403, NO_WINDOW) };
            return self.error(req, writer, status, line, &[]).await;
        }
        self.sign_in(req, writer).await?;
        self.probe_soon(req.peername);
        Ok(())
    }

    /// `_download`: the CA certificate, alone or in an iPhone profile: lite_tls encodes both from the certificate,
    /// never a key.
    async fn download(&self, req: &mut Request, writer: &mut Writer, profile: bool) -> Handled {
        let tls = self.ctx.parts().tls;
        let made = tokio::task::spawn_blocking(move || if profile { tls.mobileconfig() } else { tls.ca_pem() }).await;
        let data = match made {
            Ok(Ok(data)) => data,
            Ok(Err(e)) => {
                warn!("lite web: the CA download could not be made ({})", error_name(&e));
                return self.error(req, writer, 503, TLS_OFF, &[]).await;
            }
            Err(_) => {
                warn!("lite web: the CA download could not be made (RuntimeError)");
                return self.error(req, writer, 503, TLS_OFF, &[]).await;
            }
        };
        let (ctype, filename) = if profile {
            ("application/x-apple-aspen-config", "eve-lite.mobileconfig")
        } else {
            ("application/x-x509-ca-cert", "eve-lite-ca.crt")
        };
        req.responded = true;
        let headers = [("Content-Disposition".to_string(), format!("attachment; filename=\"{filename}\""))];
        send(writer, 200, &data, ctype, &headers, NO_STORE, req.head_only()).await
    }

    /// LanServer `refresh_names`: this Mac's LocalHostName and private addresses now, and the certificate reissued
    /// when it lacks one (a new name or address; lite_tls.LocalTls.refresh).
    async fn refresh_names(&self) -> Result<(String, Vec<String>), StartError> {
        let parts = self.ctx.parts();
        let (hostname_fn, addresses_fn, tls) = (parts.hostname.clone(), parts.addresses.clone(), parts.tls.clone());
        let refreshed = tokio::task::spawn_blocking(move || {
            // no name: the IP link alone; no address: the .local link alone
            let hostname = hostname_fn().unwrap_or_default();
            let addresses = addresses_fn().unwrap_or_default();
            tls.refresh(&addresses, &hostname).map(|_| (hostname, addresses))
        })
        .await
        .map_err(|_| StartError::Runtime("the names were not read".into()))?;
        let (hostname, addresses) = refreshed.map_err(StartError::Tls)?;
        *self.names.lock() = (hostname.clone(), addresses.clone());
        self.allowed.lock().1 = 0.0;
        Ok((hostname, addresses))
    }
}

/// The connection's place in the door's counts, given back when the connection ends, however it ends.
struct ConnGuard {
    door: Arc<Door>,
    id: u64,
    peer: String,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.door.forget(self.id, Some(&self.peer));
    }
}

/// `_send`.
async fn send(
    writer: &mut Writer,
    status: u16,
    body: &[u8],
    ctype: &str,
    headers: &[(String, String)],
    cache: &str,
    head_only: bool,
) -> Handled {
    let mut out = head_bytes(status, ctype, body.len(), cache, headers)?;
    if !head_only {
        out.extend_from_slice(body);
    }
    writer.write_all(&out).await?;
    writer.flush().await?;
    Ok(())
}

/// `json.dumps(obj)`.
fn dumps(v: &Value) -> String {
    eve_apps_mcp::py::dumps(v, true)
}

fn set_cookie(name: &str, token: &str) -> Result<String, Failure> {
    eve_web_auth::set_cookie(name, token).map_err(|_| Failure::Error("ValueError"))
}

fn clear_cookie(name: &str) -> Result<String, Failure> {
    eve_web_auth::clear_cookie(name).map_err(|_| Failure::Error("ValueError"))
}

fn sign_in_error_name(e: &SignInError) -> &'static str {
    match e {
        SignInError::Value(_) => "ValueError",
        SignInError::Onboarding(e) if e.is_state() => "StateError",
        SignInError::Onboarding(_) => "OnboardingError",
        SignInError::Runtime(_) => "RuntimeError",
    }
}

fn restart_error_name(e: &anyhow::Error) -> &'static str {
    match e.chain().find_map(|c| c.downcast_ref::<io::Error>()) {
        Some(io) => os_error_name(io),
        None => "RuntimeError",
    }
}

fn libc_eaddrinuse() -> i32 {
    libc_errno::EADDRINUSE
}

fn libc_eaddrnotavail() -> i32 {
    libc_errno::EADDRNOTAVAIL
}

fn libc_eafnosupport() -> i32 {
    libc_errno::EAFNOSUPPORT
}

fn libc_eprotonosupport() -> i32 {
    libc_errno::EPROTONOSUPPORT
}

/// errno values (`errno.EADDRINUSE` and the three a host this Mac cannot have raises).
mod libc_errno {
    #[cfg(target_os = "macos")]
    pub const EADDRINUSE: i32 = 48;
    #[cfg(target_os = "macos")]
    pub const EADDRNOTAVAIL: i32 = 49;
    #[cfg(target_os = "macos")]
    pub const EAFNOSUPPORT: i32 = 47;
    #[cfg(target_os = "macos")]
    pub const EPROTONOSUPPORT: i32 = 43;
    #[cfg(not(target_os = "macos"))]
    pub const EADDRINUSE: i32 = 98;
    #[cfg(not(target_os = "macos"))]
    pub const EADDRNOTAVAIL: i32 = 99;
    #[cfg(not(target_os = "macos"))]
    pub const EAFNOSUPPORT: i32 = 97;
    #[cfg(not(target_os = "macos"))]
    pub const EPROTONOSUPPORT: i32 = 93;
}

// ---- the Mac's own page -----------------------------------------------------------------------------------------

/// `LocalServer`: `http://localhost:<port>`, on loopback only (crate docs). The daemon starts it at boot and stops
/// it last; it brings the LAN door back when web.json says it was on, and owns turning it on and off. Besides the
/// routes, the daemon's IPC uses `port`, `set_lan`, `revoke` and `status`. Clones are one door.
#[derive(Clone)]
pub struct LocalServer(Arc<Door>);

impl LocalServer {
    pub fn new(ctx: Arc<WebContext>) -> Self {
        LocalServer(Door::new(ctx, Where::Local))
    }

    pub fn ctx(&self) -> &Arc<WebContext> {
        &self.0.ctx
    }

    pub fn port(&self) -> u16 {
        self.0.port()
    }

    pub fn url(&self) -> String {
        format!("http://localhost:{}", self.port())
    }

    pub fn running(&self) -> bool {
        self.0.running()
    }

    /// Where this door listens, as (address, port): the sockets themselves, for a status line or a test.
    pub fn bound(&self) -> Vec<(String, u16)> {
        self.0.bound()
    }

    /// Connections past a cap, closed at once.
    pub fn refused(&self) -> u64 {
        self.0.refused.load(Ordering::SeqCst)
    }

    /// Open connections now.
    pub fn open_connections(&self) -> usize {
        self.0.conns.lock().len()
    }

    /// Open connections from one address now.
    pub fn connections_from(&self, address: &str) -> usize {
        self.0.per_peer.lock().get(address).copied().unwrap_or(0)
    }

    /// Whether any address holds a connection.
    pub fn any_peer(&self) -> bool {
        !self.0.per_peer.lock().is_empty()
    }

    pub fn lan(&self) -> Option<LanServer> {
        self.0.shared().lan()
    }

    pub fn lan_on(&self) -> bool {
        self.0.lan_on()
    }

    /// Listen, on the port web.json names when it is free, else the first free of `local_ports`; the LAN door
    /// opens too when it was on (a LAN door that cannot is logged, and left off). The port. PortsTaken when none
    /// is free; Runtime outside eve lite.
    pub async fn start(&self) -> Result<u16, StartError> {
        self.start_with(true).await
    }

    pub async fn start_with(&self, resume_lan: bool) -> Result<u16, StartError> {
        let door = &self.0;
        door.ctx.require_lite()?;
        if door.running() {
            return Ok(door.port());
        }
        let parts = door.ctx.parts();
        let saved = &door.shared().saved;
        // bind sets the Host allowlist (LOOPBACK_NAMES with the port) before it takes a connection
        let port = door.bind(&parts.local_hosts, &ports(&parts.local_ports, saved.local_port()))?;
        if saved.local_port() != Some(port) {
            let ctx = Arc::clone(&door.ctx);
            let _ = tokio::task::spawn_blocking(move || ctx.shared.saved.save(&[Change::LocalPort(port)])).await;
        }
        info!("lite web: the Mac's page is up on port {port}");
        if resume_lan && saved.lan() {
            match door.set_lan(true).await {
                Ok(_) => {}
                Err(e) if e.is_os_or_value() => warn!("lite web: the LAN door did not open again ({})", e.name()),
                Err(e) => return Err(e),
            }
        }
        Ok(port)
    }

    /// LinkChat's open streams end with their [DONE] first (as miniapp.MiniAppServer closes), then the LAN door
    /// closes, then this one. The RtcHub is the daemon's to close.
    pub async fn stop(&self) {
        let door = &self.0;
        if let Some(link) = door.ctx.parts().link {
            link.close();
            link.settle().await;
        }
        if let Some(lan) = door.shared().lan() {
            lan.stop().await;
        }
        let me = tokio::task::try_id();
        let tasks: Vec<(tokio::task::Id, AbortHandle)> =
            door.shared().tasks.lock().iter().map(|(id, h)| (*id, h.clone())).collect();
        for (id, task) in tasks {
            if Some(id) != me {
                // a restart that stops eve through here goes on
                task.abort();
            }
        }
        door.close().await;
        info!("lite web: the Mac's page is down");
    }

    /// Turn the LAN door on (start it if it is not running) or off, and remember it in web.json. Off also closes a
    /// pairing window and the WebRTC peers of the devices that came through it. The LanServer when on. PortsTaken
    /// when no port is free; Tls when the certificate could not be made or loaded.
    pub async fn set_lan(&self, on: bool) -> Result<Option<LanServer>, StartError> {
        self.0.set_lan(on).await
    }

    /// Forget one device by its id, or every device with "all", and close their WebRTC peers: the ids revoked.
    /// An error when the list could not be saved.
    pub async fn revoke(&self, id_or_all: &str) -> io::Result<Vec<String>> {
        self.0.revoke_ids(id_or_all).await
    }

    /// `GET /api/web/status`'s answer.
    pub async fn status(&self) -> Value {
        self.0.status().await
    }
}

// ---- the LAN door ------------------------------------------------------------------------------------------------

/// `LanServer`: `https://<LocalHostName>.local:<port>` on every interface (crate docs). LocalServer turns it on
/// and off; started on its own (a test, the daemon), it still registers as the context's LAN door. Clones are one
/// door.
#[derive(Clone)]
pub struct LanServer(Arc<Door>);

impl LanServer {
    pub fn new(ctx: Arc<WebContext>) -> Self {
        LanServer(Door::new(ctx, Where::Lan))
    }

    pub fn port(&self) -> u16 {
        self.0.port()
    }

    pub fn running(&self) -> bool {
        self.0.running()
    }

    pub fn bound(&self) -> Vec<(String, u16)> {
        self.0.bound()
    }

    /// Connections from outside the home network, dropped unread.
    pub fn dropped(&self) -> u64 {
        self.0.dropped.load(Ordering::SeqCst)
    }

    /// This Mac's LocalHostName, as last read.
    pub fn hostname(&self) -> String {
        self.0.names.lock().0.clone()
    }

    /// This Mac's private addresses, as last read.
    pub fn lan_addresses(&self) -> Vec<String> {
        self.0.names.lock().1.clone()
    }

    /// Whether the context's LAN door is listening (`_Door.lan_on`).
    pub fn lan_on(&self) -> bool {
        self.0.lan_on()
    }

    /// `GET /api/web/status`'s answer (`_Door.status`).
    pub async fn status(&self) -> Value {
        self.0.status().await
    }

    /// Connections past a cap, closed at once.
    pub fn refused(&self) -> u64 {
        self.0.refused.load(Ordering::SeqCst)
    }

    /// Read the Host allowlist again on the next request (`_allowed_at = 0.0`).
    pub fn expire_names(&self) {
        self.0.allowed.lock().1 = 0.0;
    }

    /// Whether two handles are one door.
    pub fn same(&self, other: &LanServer) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Make sure the certificate names this Mac, then listen on the port web.json names when it is free, else the
    /// first free of `lan_ports`. The port. PortsTaken when none is free; Tls when the certificate could not be
    /// made or loaded.
    pub async fn start(&self) -> Result<u16, StartError> {
        let door = &self.0;
        door.ctx.require_lite()?;
        if door.running() {
            return Ok(door.port());
        }
        if let Some(other) = door.shared().lan() {
            if !other.same(self) && other.running() {
                return Err(StartError::Runtime(
                    "this context's LAN door is open already (LocalServer.set_lan opens it)".into(),
                ));
            }
        }
        door.refresh_names().await?;
        // made now, not during the first phone's handshake
        let tls = door.ctx.parts().tls;
        tokio::task::spawn_blocking(move || tls.context())
            .await
            .map_err(|_| StartError::Runtime("the certificate was not loaded".into()))?
            .map_err(StartError::Tls)?;
        let parts = door.ctx.parts();
        let saved = &door.shared().saved;
        let port = door.bind(&parts.lan_hosts, &ports(&parts.lan_ports, saved.lan_port()))?;
        door.allowed.lock().1 = 0.0;
        *door.shared().lan.lock() = Some(self.clone());
        if saved.lan_port() != Some(port) {
            let ctx = Arc::clone(&door.ctx);
            let _ = tokio::task::spawn_blocking(move || ctx.shared.saved.save(&[Change::LanPort(port)])).await;
        }
        info!("lite web: the LAN door is open on port {port}");
        Ok(port)
    }

    pub async fn stop(&self) {
        if !self.running() {
            return;
        }
        self.0.close().await;
        info!("lite web: the LAN door is closed");
    }

    /// This Mac's LocalHostName and private addresses now, and the certificate reissued when it lacks one.
    /// (hostname, addresses).
    pub async fn refresh_names(&self) -> Result<(String, Vec<String>), StartError> {
        self.0.refresh_names().await
    }
}
