//! Eve Link's door: the iPhone → daemon path (docs/action-button.md). Port of `eve_bridge/stick_link.py`.
//!
//! The owner asked on 2026-09-30 to "talk to it from anywhere". The Eve Link app on the iPhone (ios/EveLink)
//! carries each Action Button or Hold to talk turn to the daemon as a call (phone_call.py), through the Mini App's
//! Cloudflare tunnel (miniapp.py). The names here (`/stick`, `stick-link.json`) are from the M5StickS3 button it
//! first carried, removed on 2026-10-07; they are kept so a paired phone keeps working. This crate is what the
//! daemon adds for it:
//!
//! * **The link token.** The Mini App's call proves the owner with Telegram's signed initData, which only
//!   Telegram's in-app browser has. The phone app instead holds a random token, made here on the owner's `/stick`
//!   and kept in `stick-link.json` (0600) next to the Mini App's ledger. `/stick new` replaces it, and the old one
//!   stops working at once: that is how a lost phone is cut off.
//! * **Its own door.** `/api/stick` is the call endpoint for the app, and takes only the token. A browser always
//!   sends an Origin on a WebSocket; the app sends none, so a request that has one is refused there, and the Mini
//!   App keeps `/api/call` exactly as it was.
//! * **Pairing without the token on the wire.** The quick tunnel's address changes every start (the owner chose,
//!   2026-09-30, a tap-to-update link over a fixed address). The pinned "eve: your apps." message gets a second
//!   button, *Update Eve Link*, edited to the new address with the Open button on every start. It opens
//!   `https://<tunnel>/stick#t=<token>`: the token is in the fragment, which the browser never sends, and the
//!   page (static, no secrets) hands the address and the token to the app as `evelink://pair?u=…&t=…`.
//!
//! The network calls ([`post`], [`publish_address`], [`latest_address`]) block, as the Python ones do (the Mini
//! App runs `publish_address` through `asyncio.to_thread`). From async code, run them in
//! `tokio::task::spawn_blocking`.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

pub const PAIR_PATH: &str = "/stick";
pub const CALL_PATH: &str = "/api/stick";
pub const UPDATE_TEXT: &str = "Update Eve Link";
pub const PAIR_LINE: &str = "Tap the button to pair Eve Link with this Mac. After that you can talk to eve \
anywhere your phone has signal. When eve restarts, the pinned message's Update Eve Link button \
has the new address.";
pub const NEW_LINE: &str =
    "A new link token: the old one no longer works. Tap the button on the phone that should keep it.";
pub const OFF_LINE: &str = "Eve Link needs the Mini App's tunnel, and it isn't up right now.";

/// The one token the Eve Link app presents. Absent until the owner pairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkToken {
    pub path: PathBuf,
}

impl LinkToken {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The stored token, or "" when there is none, the file is unreadable or damaged, or the token is shorter than
    /// 32 characters.
    pub fn current(&self) -> String {
        let Ok(bytes) = fs::read(&self.path) else {
            return String::new();
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return String::new();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return String::new();
        };
        // Python: `.get("token", "")` on anything but a dict raises AttributeError, caught as "".
        match value.as_object().and_then(|o| o.get("token")) {
            Some(serde_json::Value::String(token)) if token.chars().count() >= 32 => token.clone(),
            _ => String::new(),
        }
    }

    fn write(&self, token: &str) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let tmp = with_suffix_tmp(&self.path);
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        // json.dump's default separators: `{"token": "…", "created": 1760000000}`.
        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_else(|e| -(e.duration().as_secs() as i64)); // int() truncates toward zero
        let body = format!(
            "{{\"token\": {}, \"created\": {}}}",
            serde_json::Value::String(token.to_owned()),
            created
        );
        f.write_all(body.as_bytes())?;
        drop(f);
        fs::rename(&tmp, &self.path)
    }

    /// The token, made on first use.
    pub fn ensure(&self) -> io::Result<String> {
        let current = self.current();
        if current.is_empty() {
            self.rotate()
        } else {
            Ok(current)
        }
    }

    /// A new token (`secrets.token_urlsafe(32)`: 43 URL-safe base64 characters); the old one stops working.
    pub fn rotate(&self) -> io::Result<String> {
        let mut raw = [0u8; 32];
        rand::rng().fill_bytes(&mut raw);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        self.write(&token)?;
        tracing::info!("eve link: a new link token (the old one, if any, no longer works)");
        Ok(token)
    }

    /// Whether `presented` is the stored token, compared in constant time. Nothing matches before pairing.
    pub fn check(&self, presented: &str) -> bool {
        let want = self.current();
        !want.is_empty() && !presented.is_empty() && compare_digest(want.as_bytes(), presented.as_bytes())
    }
}

/// `pathlib.Path.with_suffix(".tmp")`: the last suffix replaced (`stick-link.json` → `stick-link.tmp`).
fn with_suffix_tmp(path: &Path) -> PathBuf {
    path.with_extension("tmp")
}

/// `hmac.compare_digest` over bytes: no early exit on the first difference.
fn compare_digest(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---- the rendezvous: the phone finds eve's new address by itself ----------------------------------------
//
// The quick tunnel's address changes every start, and on 2026-10-04 that broke the Action Button: the phone kept
// calling yesterday's address until the owner tapped Update Eve Link ("port isnt being held"). So the daemon also
// posts its address to an ntfy.sh topic (no account) that only the link token can name: the topic is an HMAC of
// a fixed label under the token, so it carries nothing about the token and nobody without the token can find it.
// The message is the tunnel's https address alone, which is no use without the token. The phone reads the topic
// at launch and whenever a call finds the old address gone (ios/EveLink/Shared/Rendezvous.swift).
// ntfy.sh keeps a message 12 hours, so the address is posted again every REPOST_SECS while it stays the same.
// EVE_LINK_RENDEZVOUS=0 turns it off (Update Eve Link still works); EVE_LINK_RENDEZVOUS_URL picks
// another ntfy server.
pub const RENDEZVOUS_URL: &str = "https://ntfy.sh";
pub const RENDEZVOUS_LABEL: &[u8] = b"eve link rendezvous";
pub const REPOST_SECS: f64 = 6.0 * 3600.0;
pub const RENDEZVOUS_TIMEOUT_SECS: f64 = 10.0;

fn rendezvous_timeout() -> Duration {
    Duration::from_secs_f64(RENDEZVOUS_TIMEOUT_SECS)
}

/// The topic the phone computes the same way: "evelink-" and 32 hex of HMAC-SHA256(token, label).
pub fn rendezvous_topic(token: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(RENDEZVOUS_LABEL);
    let hex = hex::encode(mac.finalize().into_bytes());
    format!("evelink-{}", &hex[..32])
}

/// The ntfy server, or "" when the rendezvous is switched off. Reads the process environment.
pub fn rendezvous_base() -> String {
    rendezvous_base_from(|name| std::env::var_os(name).map(|v| v.to_string_lossy().into_owned()))
}

/// [`rendezvous_base`] over another environment (Python's `environ` argument).
pub fn rendezvous_base_from(env: impl Fn(&str) -> Option<String>) -> String {
    let switch = env("EVE_LINK_RENDEZVOUS").unwrap_or_default();
    if matches!(py_strip(&switch).to_lowercase().as_str(), "0" | "false" | "no" | "off") {
        return String::new();
    }
    let url = env("EVE_LINK_RENDEZVOUS_URL")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| RENDEZVOUS_URL.to_owned());
    py_strip(&url).trim_end_matches('/').to_owned()
}

/// A request that failed before an HTTP status came back. `type_name` is the name of the Python exception it
/// stands for (`URLError`, `TimeoutError`, `ValueError`, …): it is all that is logged, because the message can
/// carry the topic, which names the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    pub type_name: String,
}

impl TransportError {
    pub fn new(type_name: impl Into<String>) -> Self {
        Self { type_name: type_name.into() }
    }

    fn from_reqwest(e: &reqwest::Error) -> Self {
        let name = if e.is_builder() {
            "ValueError" // urllib: "unknown url type"
        } else if e.is_timeout() {
            "TimeoutError"
        } else if e.is_decode() || e.is_body() {
            "IncompleteRead"
        } else {
            "URLError"
        };
        Self::new(name)
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.type_name)
    }
}

impl std::error::Error for TransportError {}

/// Python's `_post`: POST `body` as text/plain with `Title: eve`, and return the HTTP status, an error status
/// included (urllib's `HTTPError.code`). Like urllib, a 307/308 answer to a POST is not followed: its status is
/// the answer. Blocking.
pub fn post(url: &str, body: &[u8]) -> Result<u16, TransportError> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if matches!(attempt.status().as_u16(), 307 | 308) || attempt.previous().len() >= 10 {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(rendezvous_timeout())
        .redirect(policy)
        .build()
        .map_err(|e| TransportError::from_reqwest(&e))?;
    let reply = client
        .post(url)
        .header("Content-Type", "text/plain")
        .header("Title", "eve")
        .body(body.to_vec())
        .send()
        .map_err(|e| TransportError::from_reqwest(&e))?;
    Ok(reply.status().as_u16())
}

/// Post the tunnel's address to the token's topic through [`post`]. Blocking; false on any failure (logged,
/// never raised). Python's default `base` is [`RENDEZVOUS_URL`].
pub fn publish_address(public_url: &str, token: &str, base: &str) -> bool {
    publish_address_with(public_url, token, base, post)
}

/// [`publish_address`] with its HTTP seam (Python's `post` argument).
pub fn publish_address_with(
    public_url: &str,
    token: &str,
    base: &str,
    post: impl FnOnce(&str, &[u8]) -> Result<u16, TransportError>,
) -> bool {
    if public_url.is_empty() || token.is_empty() || base.is_empty() {
        return false;
    }
    let url = format!("{base}/{}", rendezvous_topic(token));
    let status = match post(&url, public_url.trim_end_matches('/').as_bytes()) {
        Ok(status) => status,
        Err(e) => {
            // Never raised: a reply that breaks http.client (BadStatusLine) or a base with no scheme (ValueError)
            // killed the announce loop (verification/Eve/Rendezvous.lean, 2026-10-04).
            // The type name only: the message can carry the topic, which names the token.
            tracing::warn!("eve link: could not post the address for Eve Link ({})", e.type_name);
            return false;
        }
    };
    if status != 200 {
        tracing::warn!("eve link: the rendezvous answered {}", status);
        return false;
    }
    tracing::info!("eve link: posted the address for Eve Link"); // the address is in the miniapp's own line
    true
}

/// urllib's `urlopen(url, timeout).read()` for a GET: the body of a 2xx answer (redirects followed, at most 10),
/// any other status an error. Blocking.
pub fn open(url: &str, timeout: Duration) -> Result<Vec<u8>, TransportError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| TransportError::from_reqwest(&e))?;
    let reply = client.get(url).send().map_err(|e| TransportError::from_reqwest(&e))?;
    if !reply.status().is_success() {
        return Err(TransportError::new("HTTPError"));
    }
    reply
        .bytes()
        .map(|b| b.to_vec())
        .map_err(|e| TransportError::from_reqwest(&e))
}

/// `^https://[a-z0-9-]+\.trycloudflare\.com$`: a bare quick-tunnel origin.
fn is_quick_tunnel_origin(s: &str) -> bool {
    s.strip_prefix("https://")
        .and_then(|rest| rest.strip_suffix(".trycloudflare.com"))
        .is_some_and(|name| {
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// The newest address the daemon posted to the rendezvous (what the phone reads), or "" when none is there or
/// the read fails. Only a bare quick-tunnel origin is taken, as the phone does (Rendezvous.swift). Blocking.
pub fn latest_address(token: &str, base: &str) -> String {
    latest_address_with(token, base, open)
}

/// [`latest_address`] with its HTTP seam (Python's `opener` argument): it gets the URL and the timeout and
/// returns the body.
pub fn latest_address_with(
    token: &str,
    base: &str,
    opener: impl FnOnce(&str, Duration) -> Result<Vec<u8>, TransportError>,
) -> String {
    if token.is_empty() || base.is_empty() {
        return String::new();
    }
    let url = format!("{}/{}/json?poll=1&since=all", base.trim_end_matches('/'), rendezvous_topic(token));
    let Ok(body) = opener(&url, rendezvous_timeout()) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&body);
    let mut newest = String::new();
    for line in py_splitlines(&text) {
        let Ok(serde_json::Value::Object(event)) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if event.get("event").and_then(|v| v.as_str()) != Some("message") {
            continue;
        }
        // `str(event.get("message") or "")`: only a string can look like an address.
        let Some(message) = event.get("message").and_then(|v| v.as_str()) else {
            continue;
        };
        let message = py_strip(message).trim_end_matches('/');
        if is_quick_tunnel_origin(message) {
            newest = message.to_owned();
        }
    }
    newest
}

/// The tap-to-update link. The token rides in the fragment, which never leaves the phone.
pub fn pair_url(public_url: &str, token: &str) -> String {
    format!("{}{}#t={}", public_url.trim_end_matches('/'), PAIR_PATH, quote_all(token))
}

/// `urllib.parse.quote(s, safe='')`: every byte but `A-Za-z0-9_.-~` as `%XX`.
fn quote_all(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Python's `str.isspace()` for one character.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// Python's `str.splitlines()`: breaks on \n, \r, \r\n, \v, \f, \x1c, \x1d, \x1e, \x85,   and  .
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let is_break = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        lines.push(&s[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = chars.peek() {
                chars.next();
                end = j + 1;
            }
        }
        start = end;
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

// The pairing page: static, holds nothing secret, reads the fragment and offers the app's link. A button rather
// than an automatic jump: Telegram's in-app browser only opens another app from a tap.
pub const PAIR_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
/// Python's `PAIR_HEADERS` dict, in its order. no-store comes with every reply.
pub const PAIR_HEADERS: [(&str, &str); 3] = [
    ("Content-Security-Policy", PAIR_CSP),
    ("Referrer-Policy", "no-referrer"),
    ("X-Content-Type-Options", "nosniff"),
];
pub const PAIR_PAGE: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Eve Link</title>
<style>
  :root { color-scheme: light dark; --bg: #faf7f2; --fg: #1d1b18; --muted: #6b655c; --accent: #d9774b; }
  @media (prefers-color-scheme: dark) { :root { --bg: #161412; --fg: #f1ece4; --muted: #a39b8f; } }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: var(--bg); color: var(--fg);
         font: 17px/1.45 -apple-system, system-ui, sans-serif; }
  main { max-width: 22rem; padding: 24px 16px; text-align: center; }
  h1 { font-size: 1.5rem; margin: 0 0 .5rem; }
  p { color: var(--muted); margin: 0 0 1.5rem; }
  a.btn { display: inline-block; padding: 14px 26px; border-radius: 14px; background: var(--accent); color: #fff;
          font-weight: 600; text-decoration: none; }
  a.btn:focus-visible { outline: 3px solid var(--fg); outline-offset: 3px; }
</style></head>
<body><main>
  <h1>Eve Link</h1>
  <p id="say">Opening Eve Link.</p>
  <a class="btn" id="go" href="#">Open in Eve Link</a>
</main>
<script>
  var t = new URLSearchParams(location.hash.slice(1)).get("t") || "";
  history.replaceState(null, "", location.pathname);
  var go = document.getElementById("go");
  if (!t) {
    document.getElementById("say").textContent = "This link is missing its token. On the Mac, run eve-bridge link pair again.";
    go.remove();
  } else {
    go.href = "evelink://pair?u=" + encodeURIComponent(location.origin) + "&t=" + encodeURIComponent(t);
  }
</script>
</body></html>
"##;

#[cfg(test)]
mod tests;
