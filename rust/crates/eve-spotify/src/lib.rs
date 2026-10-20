//! The owner's Spotify: what spotKnob does on the knob — now playing, play/pause, skip, volume, where it plays —
//! from the chat, the voice and the Mini App. Port of `eve_bridge/spotify.py`.
//!
//! The owner asked on 2026-09-27 to "give eve spotKnob capabilities". spotKnob (gurul/spotify-knob) is a round ESP32
//! dial that drives the Spotify Web API directly: turn for volume, press to play or pause, swipe to skip, hold to pick
//! the Spotify Connect device the music plays on. This module is the same set of powers, on the same Web API, with
//! two more that a voice needs and a knob does not: **play something by name** ("play Daft Punk"), and **wake the
//! Mac's Spotify** when nothing is online to play on.
//!
//! | What | Web API call | Door |
//! |---|---|---|
//! | now playing | `GET  /v1/me/player` | all three |
//! | play / pause | `PUT  /v1/me/player/play\|pause` | all three |
//! | next / previous | `POST /v1/me/player/next\|previous` | all three |
//! | volume | `PUT  /v1/me/player/volume` | all three (relative: read, then set) |
//! | devices | `GET  /v1/me/player/devices` | all three |
//! | move the music | `PUT  /v1/me/player` (`play: true`) | all three (spotKnob's device picker) |
//! | play by name | `GET  /v1/search` then `PUT play` | the chat and the voice |
//!
//! Three doors use one [`Spotify`], the way lights is wired:
//!
//! * **Telegram** (and what rides its brain: calls, the Voice PE). A message that is **only** a music command
//!   ("pause the music", "next song", "music volume 40", "what's playing", "play the music on the kitchen speaker",
//!   "play Daft Punk on Spotify") is done by code, no model turn ([`Spotify::match_text`]). Anything else goes to the
//!   model, which has `spotify_now_playing` and `spotify_control`.
//! * **The robot's live voice** gets the same two tools.
//! * **The Mini App** has a Music card (`/api/spotify`): the track and its art, previous / play-pause / next, a
//!   volume slider and the device list.
//!
//! Setup is one login, on this Mac (docs/spotify.md): `eve-bridge spotify login --client-id ID`. It runs
//! Spotify's authorization-code flow with PKCE against the loopback redirect `http://127.0.0.1:8888/callback` — the
//! same redirect spotKnob's app already registers, so spotKnob's Spotify app can be reused as-is. PKCE needs no client
//! secret. The refresh token is kept in `~/.config/eve-bridge/spotify.json` (mode 600), never in the repo, and
//! is rewritten whenever Spotify rotates it. Playback control needs Spotify Premium, as on the knob.
//!
//! `EVE_SPOTIFY=0` turns it all off. With no login it is off too, and no tool is lent.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// A free-form dict, the shape every door answers with.
pub type Dict = Map<String, Value>;

pub const CONFIG_PATH: &str = "~/.config/eve-bridge/spotify.json";
pub const OFF_REASON: &str = "Spotify is not set up on this computer (eve-bridge spotify login; EVE_SPOTIFY)";
/// Said when a music command arrives with no login: the code path still knows the words, so the model never gets
/// a Spotify request it has no tools for (2026-09-27: "spotify mode" on the Voice PE went to Composio and failed).
pub const NOT_SET_UP_LINE: &str =
    "Spotify isn't set up on this Mac yet. Run: eve-bridge spotify login --client-id ID, then restart eve.";
pub const API: &str = "https://api.spotify.com/v1";
pub const ACCOUNTS: &str = "https://accounts.spotify.com";
/// spotKnob's registered redirect: one Spotify app serves both.
pub const REDIRECT_URI: &str = "http://127.0.0.1:8888/callback";
pub const LOGIN_PORT: u16 = 8888;
/// spotKnob's two, plus the currently-playing read. user-modify-playback-state covers volume, skip and transfer.
pub const SCOPES: &str = "user-read-playback-state user-modify-playback-state user-read-currently-playing";
pub const HTTP_TIMEOUT_SECS: f64 = 10.0;
/// Refresh an access token this long before Spotify says it expires.
pub const TOKEN_MARGIN_SECS: f64 = 60.0;
/// "turn it up": the knob moves 2% a detent; a spoken step is five detents.
pub const VOLUME_STEP: i64 = 10;
/// How long a freshly opened Spotify app gets to show up as a Connect device.
pub const WAKE_SECS: f64 = 12.0;
pub const MAC_APP: &str = "/Applications/Spotify.app";
/// Each Spotify Connect device keeps one colour, on the Voice PE's ring in the picker and on the Mini App's key
/// (owner, 2026-09-27: "can each device be represented with another color"). Green is Spotify mode's own and red
/// is a refusal, so neither is here. A device's colour is kept in COLORS_FILE from the first time it is seen.
pub const DEVICE_COLORS: [(&str, (u8, u8, u8)); 7] = [
    ("blue", (0, 90, 255)),
    ("orange", (255, 110, 0)),
    ("purple", (170, 0, 255)),
    ("cyan", (0, 200, 255)),
    ("yellow", (255, 200, 0)),
    ("pink", (255, 40, 140)),
    ("white", (200, 200, 200)),
];
/// Beside the login, ~/.config/eve-bridge/.
pub const COLORS_FILE: &str = "spotify-colors.json";
/// Album art; anything else is dropped before it reaches a page.
pub const ART_HOST: &str = "https://i.scdn.co/";
/// A device not seen this long gives its colour back (a guest's phone, a hotel TV).
pub const FORGET_DEVICE_DAYS: i64 = 30;

pub const ACTIONS: [&str; 8] = ["play", "pause", "next", "previous", "volume", "transfer", "mode_on", "mode_off"];
pub const KINDS: [&str; 4] = ["track", "artist", "album", "playlist"];
pub const TOOL_NAMES: [&str; 2] = ["spotify_now_playing", "spotify_control"];

pub const INSTRUCTIONS: &str = "You can control the owner's Spotify with spotify_control (play, pause, next, previous, volume,
transfer to another device) and read it with spotify_now_playing. To play something by name, pass it as the query:
an artist's name with kind artist plays that artist; a mood (\"something chill\") is a playlist query. \"Turn it up\"
is volume_change 10. Say what you did in a few words.";

/// The two tools, as the Responses API takes them.
pub static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    vec![
        json!({"type": "function", "name": "spotify_now_playing", "strict": true,
               "description": "What the owner's Spotify is playing (track, artists, album, playing or paused, volume, on which device) and the Spotify Connect devices it could play on.",
               "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}}),
        json!({"type": "function", "name": "spotify_control", "strict": true,
               "description": "Control the owner's Spotify: play (resume, or play something by name), pause, next, previous, volume, or transfer the music to another device. mode_on / mode_off turn Spotify mode on the Voice PE on or off (its ring green, its dial the volume, clicks play/pause, skip and go back). Pass null for what you don't use.",
               "parameters": {"type": "object", "additionalProperties": false,
                              "required": ["action", "query", "kind", "volume", "volume_change", "device"],
                              "properties": {
                                  "action": {"type": "string", "enum": ACTIONS},
                                  "query": {"type": ["string", "null"],
                                            "description": "play only: what to play by name (\"Daft Punk\", \"Blinding Lights\", \"lofi beats\"); null resumes what was playing."},
                                  "kind": {"type": ["string", "null"], "enum": ["track", "artist", "album", "playlist", null],
                                           "description": "play with a query: track (default), artist, album or playlist."},
                                  "volume": {"type": ["integer", "null"], "description": "volume: 0-100 percent; or null."},
                                  "volume_change": {"type": ["integer", "null"],
                                                    "description": "volume: a change such as 10 or -10, when no level was asked; or null."},
                                  "device": {"type": ["string", "null"],
                                             "description": "transfer (required) or play: a device's name from spotify_now_playing; null for the current device."}}}}),
    ]
});

// ---- Python value semantics the module leans on ---------------------------------------------------------------

/// Python truthiness of a JSON value.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str(v)` for a value that reaches a line.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        other => other.to_string(),
    }
}

/// `str(d.get(key) or "")`.
fn or_empty(v: Option<&Value>) -> String {
    if truthy(v) {
        v.map(py_str).unwrap_or_default()
    } else {
        String::new()
    }
}

/// `(v or {})` as a dict: a value that is not a dict reads as empty.
fn as_obj(v: Option<&Value>) -> Option<&Dict> {
    v.and_then(Value::as_object)
}

/// `isinstance(v, int)`: a JSON integer (a bool is an int to Python).
fn py_int(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::Bool(b)) => Some(i64::from(*b)),
        _ => None,
    }
}

/// `int(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else None`.
fn int_or_none(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        _ => None,
    }
}

/// Python's `repr()` of a str, as `{x!r}` puts it in a line.
pub fn py_repr(s: &str) -> String {
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
            c if !py_printable(c) => {
                let n = c as u32;
                if n <= 0xff {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n <= 0xffff {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable()` for one character, as `repr()` uses it: control characters (Cc), the format characters
/// a name can carry (Cf), private use (Co), the line and paragraph separators and every space but the ASCII one
/// (Zl, Zp, Zs) are escaped.
fn py_printable(c: char) -> bool {
    let n = c as u32;
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    !matches!(n,
        0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x890..=0x891 | 0x8e2 | 0x180e
        | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x2064 | 0x2066..=0x206f | 0xfeff | 0xfff9..=0xfffb
        | 0x110bd | 0x110cd | 0x13430..=0x1343f | 0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a | 0xe0001 | 0xe0020..=0xe007f
        | 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd
        | 0xfdd0..=0xfdef)
        && (n & 0xfffe) != 0xfffe
}

/// `urllib.parse.quote_plus`: letters, digits and `_.-~` stay, a space is `+`, the rest is `%XX`.
pub fn quote_plus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `urllib.parse.urlencode` of ordered pairs.
pub fn urlencode<K: AsRef<str>, V: AsRef<str>>(pairs: &[(K, V)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", quote_plus(k.as_ref()), quote_plus(v.as_ref())))
        .collect::<Vec<_>>()
        .join("&")
}

/// `json.dumps(v)` (`indent=None`) or `json.dumps(v, indent=n)`, byte for byte: Python's separators and
/// `ensure_ascii`.
pub fn py_dumps(v: &Value, indent: Option<usize>) -> String {
    let mut out = String::new();
    dump_into(v, indent, 0, &mut out);
    out
}

fn dump_str(s: &str, out: &mut String) {
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
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn dump_into(v: &Value, indent: Option<usize>, level: usize, out: &mut String) {
    let newline = |out: &mut String, level: usize| {
        if let Some(n) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(n * level));
        }
    };
    let sep = if indent.is_some() { "," } else { ", " };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => dump_str(s, out),
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(sep);
                }
                newline(out, level + 1);
                dump_into(item, indent, level + 1, out);
            }
            newline(out, level);
            out.push(']');
        }
        Value::Object(o) => {
            if o.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, item)) in o.iter().enumerate() {
                if i > 0 {
                    out.push_str(sep);
                }
                newline(out, level + 1);
                dump_str(k, out);
                out.push_str(": ");
                dump_into(item, indent, level + 1, out);
            }
            newline(out, level);
            out.push('}');
        }
    }
}

/// `Path.expanduser()`.
/// `Path.expanduser()`: `$HOME` (else the account's home) with its trailing slashes dropped, `/` when that leaves
/// nothing, as `os.path.expanduser` spells it.
pub fn expanduser(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    let rest = if s == "~" {
        ""
    } else if let Some(rest) = s.strip_prefix("~/") {
        rest
    } else {
        return p.to_path_buf();
    };
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from).or_else(dirs_home) else {
        return p.to_path_buf();
    };
    let home = home.to_string_lossy().trim_end_matches('/').to_string();
    let home = if home.is_empty() { "/".to_string() } else { home };
    if rest.is_empty() {
        PathBuf::from(home)
    } else {
        PathBuf::from(home).join(rest)
    }
}

fn dirs_home() -> Option<PathBuf> {
    #[allow(deprecated)]
    std::env::home_dir()
}

fn config_path(path: Option<&Path>) -> PathBuf {
    expanduser(path.unwrap_or_else(|| Path::new(CONFIG_PATH)))
}

// ---- errors and the transport ----------------------------------------------------------------------------------

/// A Web API refusal, in words the owner can act on.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{line}")]
pub struct SpotifyError {
    pub status: u16,
    pub reason: String,
    pub line: String,
}

impl SpotifyError {
    pub fn new(status: u16, reason: &str, line: impl Into<String>) -> Self {
        Self { status, reason: reason.to_string(), line: line.into() }
    }
}

/// What a Spotify call can fail with: a refusal ([`SpotifyError`]), no answer at all (Python's `OSError`), or
/// anything else (it propagates, as the Python lets it).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Spotify(#[from] SpotifyError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// One HTTP answer: status, headers (lower-cased names), body.
#[derive(Debug, Clone, Default)]
pub struct HttpReply {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpReply {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self { status, headers: HashMap::new(), body: body.into() }
    }
}

/// Python's `Http`: `(method, url, headers, body) -> (status, headers, body)`. Never fails for an HTTP status, only
/// for no answer.
#[async_trait]
pub trait Http: Send + Sync {
    async fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> io::Result<HttpReply>;
}

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs_f64(HTTP_TIMEOUT_SECS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

/// The real transport (Python's `urllib_http`): one HTTPS call to the fixed Spotify hosts.
#[derive(Debug, Clone, Copy, Default)]
pub struct UrllibHttp;

#[async_trait]
impl Http for UrllibHttp {
    async fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> io::Result<HttpReply> {
        let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(io::Error::other)?;
        let mut req = CLIENT.request(m, url);
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(b) = body {
            req = req.body(b);
        }
        let resp = req.send().await.map_err(io::Error::other)?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_lowercase(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let body = resp.bytes().await.map_err(io::Error::other)?.to_vec();
        Ok(HttpReply { status, headers, body })
    }
}

/// The default transport, shared.
pub fn urllib_http() -> Arc<dyn Http> {
    Arc::new(UrllibHttp)
}

// ---- the login and the token ---------------------------------------------------------------------------------

/// The saved login, or `{}` when there is none (no file, not JSON, no client id or no refresh token).
pub fn load(path: Option<&Path>) -> Dict {
    let p = config_path(path);
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Dict::new();
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(d)) if truthy(d.get("client_id")) && truthy(d.get("refresh_token")) => d,
        _ => Dict::new(),
    }
}

/// Written private (600) from the first byte: the refresh token is the owner's Spotify account.
pub fn save(cfg: &Dict, path: Option<&Path>) -> io::Result<PathBuf> {
    let p = config_path(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = p.with_extension("tmp");
    let text = py_dumps(&Value::Object(cfg.clone()), Some(2));
    {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

/// Python's `secrets.token_urlsafe(n)`: n random bytes, URL-safe base64 without padding.
fn token_urlsafe(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::Rng::fill(&mut rand::rng(), bytes.as_mut_slice());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// (verifier, S256 challenge), RFC 7636.
pub fn pkce_pair() -> (String, String) {
    let mut verifier = token_urlsafe(64);
    verifier.truncate(128);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

pub fn authorize_url(client_id: &str, challenge: &str, state: &str) -> String {
    format!(
        "{ACCOUNTS}/authorize?{}",
        urlencode(&[
            ("client_id", client_id),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
            ("code_challenge_method", "S256"),
            ("code_challenge", challenge),
            ("state", state),
        ])
    )
}

async fn token_post(
    form: &[(&str, String)],
    client_id: &str,
    client_secret: &str,
    http: &dyn Http,
) -> Result<Dict, Error> {
    let mut headers = vec![("Content-Type".to_string(), "application/x-www-form-urlencoded".to_string())];
    let mut form: Vec<(&str, String)> = form.to_vec();
    if !client_secret.is_empty() {
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("{client_id}:{client_secret}"));
        headers.push(("Authorization".to_string(), format!("Basic {basic}")));
    } else {
        form.push(("client_id", client_id.to_string())); // PKCE: the client id rides in the body, no secret
    }
    let reply = http
        .request("POST", &format!("{ACCOUNTS}/api/token"), &headers, Some(urlencode(&form).into_bytes()))
        .await?;
    let raw: &[u8] = if reply.body.is_empty() { b"{}" } else { &reply.body };
    let data = match serde_json::from_slice::<Value>(raw) {
        Ok(Value::Object(d)) => d,
        _ => Dict::new(),
    };
    if reply.status != 200 || !truthy(data.get("access_token")) {
        // the body names the error ("invalid_grant") and never echoes the token or the secret
        let why = [data.get("error_description"), data.get("error")]
            .into_iter()
            .find(|v| truthy(*v))
            .flatten()
            .map(py_str)
            .unwrap_or_else(|| format!("HTTP {}", reply.status));
        return Err(SpotifyError::new(
            reply.status,
            "token",
            format!("Spotify refused the login ({why}). Run: eve-bridge spotify login"),
        )
        .into());
    }
    Ok(data)
}

/// A clock in seconds (Python's `time.monotonic` by default).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

static MONOTONIC_START: LazyLock<Instant> = LazyLock::new(Instant::now);

fn monotonic() -> Clock {
    Arc::new(|| MONOTONIC_START.elapsed().as_secs_f64())
}

/// The access token, refreshed before it runs out; a rotated refresh token is saved at once.
pub struct Token {
    cfg: parking_lot::Mutex<Dict>,
    pub http: Arc<dyn Http>,
    pub save_to: Option<PathBuf>,
    clock: Clock,
    // (access token, until): two calls at once refresh once, not twice
    state: tokio::sync::Mutex<(String, f64)>,
}

impl Token {
    pub fn new(cfg: Dict, http: Arc<dyn Http>) -> Self {
        Self::with(cfg, http, None, None)
    }

    pub fn with(cfg: Dict, http: Arc<dyn Http>, save_to: Option<PathBuf>, clock: Option<Clock>) -> Self {
        Self {
            cfg: parking_lot::Mutex::new(cfg),
            http,
            save_to,
            clock: clock.unwrap_or_else(monotonic),
            state: tokio::sync::Mutex::new((String::new(), 0.0)),
        }
    }

    /// The login as it stands (with a rotated refresh token, once there is one).
    pub fn cfg(&self) -> Dict {
        self.cfg.lock().clone()
    }

    pub async fn get(&self, force: bool) -> Result<String, Error> {
        let mut st = self.state.lock().await;
        if !st.0.is_empty() && !force && (self.clock)() < st.1 {
            return Ok(st.0.clone());
        }
        let cfg = self.cfg();
        let refresh = or_empty(cfg.get("refresh_token"));
        let data = token_post(
            &[("grant_type", "refresh_token".to_string()), ("refresh_token", refresh.clone())],
            &or_empty(cfg.get("client_id")),
            &or_empty(cfg.get("client_secret")),
            self.http.as_ref(),
        )
        .await?;
        st.0 = py_str(data.get("access_token").unwrap_or(&Value::Null));
        let expires = match data.get("expires_in") {
            None => 3600.0,
            Some(Value::Number(n)) => n.as_f64().unwrap_or(3600.0),
            Some(Value::String(s)) => s.trim().parse::<f64>().map_err(|e| Error::Other(anyhow::anyhow!(e)))?,
            Some(Value::Bool(b)) => f64::from(u8::from(*b)),
            Some(other) => return Err(Error::Other(anyhow::anyhow!("expires_in is not a number: {other}"))),
        };
        st.1 = (self.clock)() + expires - TOKEN_MARGIN_SECS;
        if truthy(data.get("refresh_token")) {
            let rotated = py_str(&data["refresh_token"]);
            if rotated != refresh {
                // PKCE refresh tokens rotate: the old one may stop working, so the new one is kept before anything else
                let new_cfg = {
                    let mut c = self.cfg.lock();
                    c.insert("refresh_token".to_string(), Value::String(rotated));
                    c.clone()
                };
                if let Some(p) = &self.save_to {
                    save(&new_cfg, Some(p))?;
                }
            }
        }
        tracing::info!("spotify: access token refreshed");
        Ok(st.0.clone())
    }
}

// ---- the Web API ---------------------------------------------------------------------------------------------

fn refusal(status: u16, data: &Dict, headers: &HashMap<String, String>) -> SpotifyError {
    let empty = Dict::new();
    let err = as_obj(data.get("error")).unwrap_or(&empty);
    let reason = or_empty(err.get("reason"));
    let message = or_empty(err.get("message"));
    if status == 404 && (reason == "NO_ACTIVE_DEVICE" || message.to_lowercase().contains("device")) {
        return SpotifyError::new(status, "NO_ACTIVE_DEVICE", "Spotify isn't playing on any device right now.");
    }
    if status == 403 && reason == "PREMIUM_REQUIRED" {
        return SpotifyError::new(status, &reason, "Controlling playback needs Spotify Premium.");
    }
    if status == 403 {
        let why = if reason.is_empty() { "RESTRICTED" } else { &reason };
        let tail = if message.is_empty() { ".".to_string() } else { format!(" ({message}).") };
        return SpotifyError::new(status, why, format!("Spotify won't take that command on this device{tail}"));
    }
    if status == 429 {
        let wait = headers.get("retry-after").cloned().unwrap_or_default();
        let tail = if wait.is_empty() { ".".to_string() } else { format!("; try in {wait} s.") };
        return SpotifyError::new(status, "RATE_LIMITED", format!("Spotify says slow down{tail}"));
    }
    if status == 401 {
        return SpotifyError::new(status, "UNAUTHORIZED", "Spotify turned the login down. Run: eve-bridge spotify login");
    }
    let why = if reason.is_empty() { "HTTP" } else { &reason };
    let tail = if message.is_empty() { ".".to_string() } else { format!(": {message}.") };
    SpotifyError::new(status, why, format!("Spotify answered {status}{tail}"))
}

/// The few Web API calls spotKnob makes, plus search.
pub struct SpotifyApi {
    pub token: Token,
    pub http: Arc<dyn Http>,
}

impl SpotifyApi {
    pub fn new(token: Token, http: Arc<dyn Http>) -> Self {
        Self { token, http }
    }

    /// One call; `None` for an empty success, the JSON otherwise. A query value of `None` is left out.
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        query: Option<&[(&str, Option<String>)]>,
        body: Option<&Value>,
    ) -> Result<Option<Value>, Error> {
        let mut url = format!("{API}{path}");
        if let Some(q) = query.filter(|q| !q.is_empty()) {
            let pairs: Vec<(&str, &str)> =
                q.iter().filter_map(|(k, v)| v.as_deref().map(|v| (*k, v))).collect();
            url.push('?');
            url.push_str(&urlencode(&pairs));
        }
        let payload: Option<Vec<u8>> = match body {
            Some(b) => Some(py_dumps(b, None).into_bytes()),
            None if method == "PUT" || method == "POST" => Some(Vec::new()),
            None => None,
        };
        let mut reply = HttpReply::default();
        for attempt in 0..2 {
            let headers = vec![
                ("Authorization".to_string(), format!("Bearer {}", self.token.get(attempt == 1).await?)),
                ("Content-Type".to_string(), "application/json".to_string()),
            ];
            reply = self.http.request(method, &url, &headers, payload.clone()).await?;
            if reply.status == 401 && attempt == 0 {
                continue; // an expired access token: refreshed and retried once, like the knob
            }
            break;
        }
        let status = reply.status;
        if matches!(status, 200 | 201 | 202 | 204) && reply.body.is_empty() {
            return Ok(None);
        }
        let data = if reply.body.is_empty() {
            Value::Object(Dict::new())
        } else {
            serde_json::from_slice::<Value>(&reply.body).unwrap_or_else(|_| Value::Object(Dict::new()))
        };
        if (200..300).contains(&status) {
            return Ok(Some(data));
        }
        let empty = Dict::new();
        Err(refusal(status, data.as_object().unwrap_or(&empty), &reply.headers).into())
    }

    pub async fn player(&self) -> Result<Option<Value>, Error> {
        self.call("GET", "/me/player", Some(&[("additional_types", Some("track,episode".to_string()))]), None)
            .await
    }

    pub async fn devices(&self) -> Result<Vec<Value>, Error> {
        let data = self.call("GET", "/me/player/devices", None, None).await?;
        Ok(match as_obj(data.as_ref()).and_then(|d| d.get("devices")) {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        })
    }

    pub async fn play(
        &self,
        device_id: Option<&str>,
        uris: Option<&[String]>,
        context_uri: Option<&str>,
    ) -> Result<(), Error> {
        let mut body = Dict::new();
        if let Some(u) = uris.filter(|u| !u.is_empty()) {
            body.insert("uris".to_string(), json!(u));
        }
        if let Some(c) = context_uri.filter(|c| !c.is_empty()) {
            body.insert("context_uri".to_string(), json!(c));
        }
        let q = device_id.filter(|d| !d.is_empty()).map(|d| vec![("device_id", Some(d.to_string()))]);
        let body = (!body.is_empty()).then(|| Value::Object(body));
        self.call("PUT", "/me/player/play", q.as_deref(), body.as_ref()).await?;
        Ok(())
    }

    pub async fn pause(&self) -> Result<(), Error> {
        self.call("PUT", "/me/player/pause", None, None).await.map(|_| ())
    }

    pub async fn next(&self) -> Result<(), Error> {
        self.call("POST", "/me/player/next", None, None).await.map(|_| ())
    }

    pub async fn previous(&self) -> Result<(), Error> {
        self.call("POST", "/me/player/previous", None, None).await.map(|_| ())
    }

    pub async fn volume(&self, percent: i64, device_id: Option<&str>) -> Result<(), Error> {
        let q = [("volume_percent", Some(percent.to_string())), ("device_id", device_id.map(str::to_string))];
        self.call("PUT", "/me/player/volume", Some(&q), None).await.map(|_| ())
    }

    pub async fn transfer(&self, device_id: &str, play: bool) -> Result<(), Error> {
        let body = json!({"device_ids": [device_id], "play": play});
        self.call("PUT", "/me/player", None, Some(&body)).await.map(|_| ())
    }

    pub async fn search(&self, query: &str, kind: &str) -> Result<Option<Value>, Error> {
        let q = [("q", Some(query.to_string())), ("type", Some(kind.to_string())), ("limit", Some("1".to_string()))];
        let data = self.call("GET", "/search", Some(&q), None).await?;
        let items = as_obj(data.as_ref())
            .and_then(|d| as_obj(d.get(&format!("{kind}s"))))
            .and_then(|d| d.get("items"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(items.into_iter().find(|i| i.is_object() && truthy(i.get("uri"))))
    }
}

// ---- each device's colour -------------------------------------------------------------------------------------

/// Days since 1970 (UTC), the default `today`.
pub type Today = Arc<dyn Fn() -> i64 + Send + Sync>;

fn days_since_epoch() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    (secs / 86400.0).floor() as i64
}

/// One device's colour and the last day it was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorEntry {
    pub i: i64,
    pub seen: i64,
}

/// `int(v)` for the colours file: an int, a float (truncated), a bool, or a decimal string.
fn int_of(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::String(s) => s.trim().replace('_', "").parse::<i64>().ok(),
        _ => None,
    }
}

/// Which colour each device has: the first colour no known device holds. A device keeps it while it is seen;
/// one not seen for [`FORGET_DEVICE_DAYS`] is forgotten, so one-off devices do not use up the seven colours
/// (owner, 2026-09-27). Past seven devices colours repeat, the least-used first.
///
/// The file maps a name to `{"i": colour index, "seen": day}`; the day is days since 1970 (UTC). A bare index
/// (the first version's shape) reads as seen today.
pub struct DeviceColors {
    pub path: Option<PathBuf>,
    today: Today,
    pub forget_days: i64,
    pub known: IndexMap<String, ColorEntry>,
}

impl DeviceColors {
    /// Kept in `path`, or only in memory when there is none.
    pub fn new(path: Option<&Path>) -> Self {
        Self::with(path, Arc::new(days_since_epoch), FORGET_DEVICE_DAYS)
    }

    pub fn with(path: Option<&Path>, today: Today, forget_days: i64) -> Self {
        let path = path.map(expanduser);
        let mut known = IndexMap::new();
        if let Some(p) = &path {
            if let Some(k) = Self::read(p, &today) {
                known = k;
            }
        }
        Self { path, today, forget_days, known }
    }

    fn read(p: &Path, today: &Today) -> Option<IndexMap<String, ColorEntry>> {
        let text = std::fs::read_to_string(p).ok()?;
        let raw = serde_json::from_str::<Value>(&text).ok()?;
        let n = DEVICE_COLORS.len() as i64;
        let mut known = IndexMap::new();
        for (k, v) in raw.as_object()? {
            let entry = if let Some(o) = v.as_object() {
                let i = int_of(o.get("i")?)?;
                let seen = match o.get("seen") {
                    Some(s) => int_of(s)?,
                    None => 0,
                };
                ColorEntry { i: i.rem_euclid(n), seen }
            } else {
                ColorEntry { i: int_of(v)?.rem_euclid(n), seen: today() }
            };
            known.insert(k.clone(), entry);
        }
        Some(known)
    }

    fn save(&self) {
        let Some(p) = &self.path else { return };
        let obj: Dict = self
            .known
            .iter()
            .map(|(k, e)| (k.clone(), json!({"i": e.i, "seen": e.seen})))
            .collect();
        let text = py_dumps(&Value::Object(obj), Some(2));
        let r = p
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(p, text));
        if let Err(e) = r {
            tracing::warn!("spotify: could not keep device colours ({})", e);
        }
    }

    pub fn index(&mut self, name: &str) -> i64 {
        let today = (self.today)();
        match self.known.get(name).copied() {
            None => {
                let gone: Vec<String> = self
                    .known
                    .iter()
                    .filter(|(_, e)| today - e.seen > self.forget_days)
                    .map(|(n, _)| n.clone())
                    .collect();
                for n in &gone {
                    self.known.shift_remove(n);
                }
                if !gone.is_empty() {
                    tracing::info!("spotify: forgot {} device(s) not seen in {} days", gone.len(), self.forget_days);
                }
                let used: Vec<i64> = self.known.values().map(|e| e.i).collect();
                let i = (0..DEVICE_COLORS.len() as i64)
                    .min_by_key(|i| (used.iter().filter(|u| *u == i).count(), *i))
                    .unwrap_or(0);
                self.known.insert(name.to_string(), ColorEntry { i, seen: today });
                self.save();
                i
            }
            Some(e) => {
                if e.seen != today {
                    // written at most once a day per device
                    if let Some(entry) = self.known.get_mut(name) {
                        entry.seen = today;
                    }
                    self.save();
                }
                e.i
            }
        }
    }

    pub fn rgb(&mut self, name: &str) -> (u8, u8, u8) {
        let i = self.index(name).rem_euclid(DEVICE_COLORS.len() as i64) as usize;
        DEVICE_COLORS[i].1
    }

    pub fn hex(&mut self, name: &str) -> String {
        let (r, g, b) = self.rgb(name);
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

// ---- what is playing, in plain words -------------------------------------------------------------------------

/// The player state, trimmed to what a person or a model needs. No ids beyond the device's name.
pub fn now_of(player: Option<&Value>) -> Dict {
    let empty = Dict::new();
    let p = as_obj(player).unwrap_or(&empty);
    let mut out = Dict::new();
    if p.is_empty() || !truthy(p.get("item")) {
        out.insert("playing".into(), json!(false));
        out.insert("track".into(), Value::Null);
        if let Some(dev) = as_obj(p.get("device")).filter(|d| !d.is_empty()) {
            out.insert("device".into(), Value::Object(device_public(dev)));
        }
        return out;
    }
    let item = as_obj(p.get("item")).unwrap_or(&empty);
    let artists: Vec<Value> = item
        .get("artists")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("name").filter(|n| truthy(Some(n))).cloned())
                .collect()
        })
        .unwrap_or_default();
    // a podcast episode has a show, not artists
    let show = as_obj(item.get("show")).and_then(|s| s.get("name")).filter(|n| truthy(Some(n))).cloned();
    let album = as_obj(item.get("album")).unwrap_or(&empty);
    let images = [album.get("images"), item.get("images")]
        .into_iter()
        .find(|v| truthy(*v))
        .flatten()
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let art = images
        .iter()
        .filter_map(|i| i.get("url").and_then(Value::as_str))
        .find(|u| u.starts_with(ART_HOST))
        .map(str::to_string);
    let secs = |v: Option<&Value>| -> i64 {
        if truthy(v) {
            v.and_then(Value::as_f64).map(|f| (f / 1000.0).trunc() as i64).unwrap_or(0)
        } else {
            0
        }
    };
    out.insert("playing".into(), json!(truthy(p.get("is_playing"))));
    out.insert(
        "track".into(),
        item.get("name").filter(|n| truthy(Some(n))).cloned().unwrap_or_else(|| json!("")),
    );
    out.insert(
        "artists".into(),
        Value::Array(if !artists.is_empty() {
            artists
        } else {
            show.clone().into_iter().collect()
        }),
    );
    out.insert(
        "album".into(),
        album
            .get("name")
            .filter(|n| truthy(Some(n)))
            .cloned()
            .or_else(|| show.clone())
            .unwrap_or_else(|| json!("")),
    );
    out.insert("progress_s".into(), json!(secs(p.get("progress_ms"))));
    out.insert("duration_s".into(), json!(secs(item.get("duration_ms"))));
    out.insert("shuffle".into(), json!(truthy(p.get("shuffle_state"))));
    out.insert(
        "repeat".into(),
        p.get("repeat_state").filter(|r| truthy(Some(r))).cloned().unwrap_or_else(|| json!("off")),
    );
    if let Some(a) = art {
        out.insert("art".into(), json!(a));
    }
    if let Some(dev) = as_obj(p.get("device")).filter(|d| !d.is_empty()) {
        out.insert("device".into(), Value::Object(device_public(dev)));
    }
    out
}

/// A device as a page may see it: name, kind, active, volume, restricted.
pub fn device_public(d: &Dict) -> Dict {
    let mut out = Dict::new();
    out.insert("name".into(), d.get("name").filter(|n| truthy(Some(n))).cloned().unwrap_or_else(|| json!("")));
    out.insert("type".into(), json!(or_empty(d.get("type")).to_lowercase()));
    out.insert("active".into(), json!(truthy(d.get("is_active"))));
    if let Some(v) = d.get("volume_percent").filter(|v| v.is_i64() || v.is_u64() || v.is_boolean()) {
        out.insert("volume".into(), v.clone());
    }
    if truthy(d.get("is_restricted")) {
        out.insert("restricted".into(), json!(true)); // the Web API refuses to command it: listed, not pickable
    }
    out
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter(|x| truthy(Some(x))).map(py_str).collect())
        .unwrap_or_default()
}

pub fn said_now(now: &Dict) -> String {
    if !truthy(now.get("track")) {
        return "Nothing is playing on Spotify.".to_string();
    }
    let by = join(&str_list(now.get("artists")));
    let where_ = as_obj(now.get("device")).map(|d| or_empty(d.get("name"))).unwrap_or_default();
    let state = if truthy(now.get("playing")) { "Playing" } else { "Paused on" };
    let mut line = format!("{state} {}", py_str(&now["track"]));
    if !by.is_empty() {
        line.push_str(&format!(" by {by}"));
    }
    if !where_.is_empty() {
        line.push_str(&format!(" on {where_}"));
    }
    line.push('.');
    line
}

/// "a", "a and b", "a, b and c".
pub fn join<S: AsRef<str>>(names: &[S]) -> String {
    let names: Vec<&str> = names.iter().map(AsRef::as_ref).filter(|n| !n.is_empty()).collect();
    match names.len() {
        0 => String::new(),
        1 => names[0].to_string(),
        n => format!("{} and {}", names[..n - 1].join(", "), names[n - 1]),
    }
}

/// Python's `\s` (`str.isspace`): Unicode White_Space plus the four ASCII separators \x1c-\x1f.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `\w` as a character class: `str.isalnum()` (a letter, L*, or a character with a numeric value, N*)
/// or `_`. Rust's own `\w` and `char::is_alphanumeric` differ: they take combining marks (a Devanagari vowel sign)
/// and leave out N* digits such as `²`.
const PY_W: &str = r"[\p{L}\p{N}_]";

/// One character of Python's `\w`.
fn py_word(c: char) -> bool {
    static W: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(&format!("^{PY_W}$")).expect("py \\w"));
    c == '_' || (!c.is_ascii() && W.is_match(c.encode_utf8(&mut [0u8; 4]))) || c.is_ascii_alphanumeric()
}

/// Python's `\b` on Python's `\w`, for fancy-regex.
fn py_boundary() -> String {
    format!("(?:(?<={PY_W})(?!{PY_W})|(?<!{PY_W})(?={PY_W}))")
}

/// `str.strip()`: Python's whitespace, which takes in the separators \x1c-\x1f that `str::trim` leaves.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// Lower case, curly apostrophes straight, punctuation out, one space between words. Python's
/// `re.sub(r"\s+", " ", re.sub(r"[^\w%'\s-]", " ", text.lower())).strip()`, on Python's character classes.
pub fn norm(text: &str) -> String {
    let t = text.to_lowercase().replace('\u{2019}', "'");
    let mut out = String::with_capacity(t.len());
    let mut space = false;
    for c in t.chars() {
        let c = if py_word(c) || c == '%' || c == '\'' || c == '-' || py_space(c) { c } else { ' ' };
        if py_space(c) {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

/// `int(s)` for what `\d{1,3}` matched: Python's `\d` and `int()` both take any Unicode decimal digit (Nd), and
/// Unicode keeps each such digit in a contiguous run of ten, 0 to 9.
fn py_int_digits(s: &str) -> Option<i64> {
    static ND: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^\d$").expect("nd"));
    let is_nd = |n: u32| char::from_u32(n).is_some_and(|c| ND.is_match(c.encode_utf8(&mut [0u8; 4])));
    let mut v: i64 = 0;
    for c in s.chars() {
        let d = match c.to_digit(10) {
            Some(d) if c.is_ascii() => i64::from(d),
            _ => {
                let n = c as u32;
                if !is_nd(n) {
                    return None;
                }
                let mut start = n;
                while start > 0 && is_nd(start - 1) {
                    start -= 1;
                }
                i64::from((n - start) % 10)
            }
        };
        v = v.checked_mul(10)?.checked_add(d)?;
    }
    Some(v)
}

// ---- the code path -------------------------------------------------------------------------------------------

/// A music command the code understood.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Command {
    /// now, play, pause, next, previous, volume, transfer, mode
    pub action: String,
    pub query: String,
    pub volume: Option<i64>,
    pub change: Option<i64>,
    pub device: String,
    /// "mode": Spotify mode on the Voice PE (music_mode)
    pub on: Option<bool>,
}

impl Command {
    pub fn new(action: &str) -> Self {
        Self { action: action.to_string(), ..Self::default() }
    }
}

const LEAD: &str = r"(?:(?:hey|ok|okay)\s+)?(?:eve\s+)?(?:(?:please|pls|can you|could you|would you)\s+)?";
const TAIL: &str = r"(?:\s+(?:please|pls|thanks|thank you|eve))?";
const MUSIC: &str = r"(?:the\s+|my\s+)?(?:music|spotify|song|track|playback|tunes)";
const ON_SPOTIFY: &str = r"(?:on|in|from|with)\s+spotify";

type Make = fn(&fancy_regex::Captures<str>) -> Option<Command>;

fn group(m: &fancy_regex::Captures<str>, i: usize) -> Option<String> {
    m.get(i).map(|g| g.as_str().to_string()).filter(|s| !s.is_empty())
}

/// The patterns, in order, each already wrapped in the lead and the tail and anchored at both ends.
static PATTERNS: LazyLock<Vec<(fancy_regex::Regex, Make)>> = LazyLock::new(|| {
    let m = MUSIC;
    let os = ON_SPOTIFY;
    let raw: Vec<(String, Make)> = vec![
        (
            r"(?:(?:turn|switch|put)\s+(on|off)\s+)?(?:the\s+)?spotify\s+mode(?:\s+(on|off))?|(?:start|enter|go\s+into)\s+spotify\s+mode".to_string(),
            |c| {
                let on = group(c, 1).or_else(|| group(c, 2)).unwrap_or_else(|| "on".to_string()) == "on";
                Some(Command { on: Some(on), ..Command::new("mode") })
            },
        ),
        (
            r"(?:exit|leave|stop|end|quit)\s+(?:the\s+)?spotify\s+mode".to_string(),
            |_| Some(Command { on: Some(false), ..Command::new("mode") }),
        ),
        (
            r"(?:what'?s|what is)\s+(?:playing|this\s+song|this\s+track)(?:\s+on\s+spotify)?|(?:what|which)\s+song\s+is\s+(?:this|playing|on)|now playing|what am i listening to".to_string(),
            |_| Some(Command::new("now")),
        ),
        (format!(r"(?:pause|stop)\s+{m}|{m}\s+(?:pause|stop|off)"), |_| Some(Command::new("pause"))),
        (
            format!(r"(?:play|resume|unpause|start)\s+{m}(?:\s+again|\s+{os})?|{m}\s+(?:play|resume|on)"),
            |_| Some(Command::new("play")),
        ),
        (
            format!(r"(?:next|skip)(?:\s+(?:this|the))?(?:\s+(?:song|track))(?:\s+{os})?|skip this"),
            |_| Some(Command::new("next")),
        ),
        (
            r"(?:previous|prev|last|go back a|back a)\s+(?:song|track)|(?:play\s+)?(?:the\s+)?previous song".to_string(),
            |_| Some(Command::new("previous")),
        ),
        (
            format!(
                r"(?:set\s+)?{m}\s+volume\s+(?:to\s+)?(\d{{1,3}})\s*%?|(?:set\s+)?(?:the\s+)?volume\s+of\s+{m}\s+to\s+(\d{{1,3}})\s*%?|{m}\s+(?:to|at)\s+(\d{{1,3}})\s*%"
            ),
            |c| {
                let g = (1..c.len()).find_map(|i| group(c, i))?;
                let v = py_int_digits(&g)?;
                Some(Command { volume: Some(v), ..Command::new("volume") })
            },
        ),
        (
            format!(r"(?:turn\s+)?{m}\s+(?:up|louder)|(?:turn\s+up|louder)\s+{m}"),
            |_| Some(Command { change: Some(VOLUME_STEP), ..Command::new("volume") }),
        ),
        (
            format!(r"(?:turn\s+)?{m}\s+(?:down|quieter|softer)|(?:turn\s+down|quieter)\s+{m}"),
            |_| Some(Command { change: Some(-VOLUME_STEP), ..Command::new("volume") }),
        ),
        (
            // Python's `\w` and `\b` spelled out: Rust's differ on marks and N* digits
            format!(r"(?:play|move|switch|send|put|transfer)\s+{m}\s+(?:on|to|onto)\s+(?:the\s+|my\s+)?(?!spotify(?!{PY_W}))([\p{{L}}\p{{N}}_' -]{{1,40}}?)"),
            |c| Some(Command { device: py_strip(c.get(1)?.as_str()).to_string(), ..Command::new("transfer") }),
        ),
        (
            format!(r"play\s+(?!{m}\s+{os})(.{{1,80}}?)\s+{os}"),
            |c| Some(Command { query: py_strip(c.get(1)?.as_str()).to_string(), ..Command::new("play") }),
        ),
    ];
    raw.into_iter()
        .map(|(p, make)| {
            // the lead and the tail only have non-capturing groups: the pattern's own groups come through as they are
            let full = format!("^{LEAD}(?:{p}){TAIL}$");
            (fancy_regex::Regex::new(&full).expect("spotify pattern"), make)
        })
        .collect()
});

/// A Command when the whole message is a music command, else None (the model answers). Anchored at both ends:
/// "pause the music in ten minutes" leaves words over, so the model gets it. `Spotify.match` without a Spotify:
/// the words alone, for the not-logged-in reply.
pub fn match_text(text: &str) -> Option<Command> {
    let t = norm(text);
    if t.is_empty() || t.chars().count() > 100 {
        return None;
    }
    for (pattern, make) in PATTERNS.iter() {
        if let Ok(Some(m)) = pattern.captures(t.as_str()) {
            let cmd = make(&m)?;
            return if cmd.volume.is_some_and(|v| v > 100) { None } else { Some(cmd) };
        }
    }
    None
}

// ---- the whole thing -----------------------------------------------------------------------------------------

/// Spotify mode on the Voice PE (music_mode's `MusicMode`), lent by the daemon when the board is its controller.
#[async_trait]
pub trait SpotifyMode: Send + Sync {
    /// Turn the mode on or off; the line to tell the owner.
    async fn set(&self, on: bool) -> anyhow::Result<String>;
    /// Whether the mode is on.
    fn on(&self) -> bool;
}

#[async_trait]
impl SpotifyMode for eve_music_mode::MusicMode {
    async fn set(&self, on: bool) -> anyhow::Result<String> {
        eve_music_mode::MusicMode::set(self, on).await
    }

    fn on(&self) -> bool {
        eve_music_mode::MusicMode::on(self)
    }
}

/// The injectable sleep (Python's `sleep=asyncio.sleep`), in seconds.
pub type SleepFn = Arc<dyn Fn(f64) -> BoxFuture<'static, ()> + Send + Sync>;
/// Opens the Mac's Spotify; false when there is none. Blocking: it runs off the event loop.
pub type WakeFn = Arc<dyn Fn() -> bool + Send + Sync>;

/// tokio's timer.
pub fn tokio_sleep() -> SleepFn {
    Arc::new(|secs: f64| -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(Duration::from_secs_f64(secs.max(0.0))))
    })
}

/// The keyword arguments of `Spotify.control`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlArgs {
    pub query: String,
    pub kind: String,
    pub volume: Option<i64>,
    pub volume_change: Option<i64>,
    pub device: String,
}

fn line_out(ok: bool, line: &str, with_reason: bool) -> Dict {
    let mut d = Dict::new();
    d.insert("ok".into(), json!(ok));
    if with_reason {
        d.insert("reason".into(), json!(line));
    }
    d.insert("line".into(), json!(line));
    d
}

/// The owner's Spotify: the tools, the code path, the Mini App panel.
pub struct Spotify {
    pub api: SpotifyApi,
    mode: parking_lot::Mutex<Option<Arc<dyn SpotifyMode>>>,
    pub colors: parking_lot::Mutex<DeviceColors>,
    wake: WakeFn,
    sleep: SleepFn,
    pub wake_secs: f64,
}

impl Spotify {
    /// The defaults: the Mac's Spotify as the wake, tokio's sleep, [`WAKE_SECS`], colours kept only in memory.
    pub fn new(api: SpotifyApi) -> Self {
        Self {
            api,
            mode: parking_lot::Mutex::new(None),
            colors: parking_lot::Mutex::new(DeviceColors::new(None)),
            wake: Arc::new(open_mac_app),
            sleep: tokio_sleep(),
            wake_secs: WAKE_SECS,
        }
    }

    pub fn with_wake(mut self, wake: WakeFn) -> Self {
        self.wake = wake;
        self
    }

    pub fn with_sleep(mut self, sleep: SleepFn) -> Self {
        self.sleep = sleep;
        self
    }

    pub fn with_wake_secs(mut self, secs: f64) -> Self {
        self.wake_secs = secs;
        self
    }

    pub fn with_colors(mut self, colors: DeviceColors) -> Self {
        self.colors = parking_lot::Mutex::new(colors);
        self
    }

    /// Lend (or take back) Spotify mode on the Voice PE.
    pub fn set_mode(&self, mode: Option<Arc<dyn SpotifyMode>>) {
        *self.mode.lock() = mode;
    }

    pub fn mode(&self) -> Option<Arc<dyn SpotifyMode>> {
        self.mode.lock().clone()
    }

    fn mode_value(&self) -> Value {
        self.mode().map_or(Value::Null, |m| json!(m.on()))
    }

    async fn sleep(&self, secs: f64) {
        (self.sleep)(secs).await;
    }

    async fn wake(&self) -> bool {
        let wake = self.wake.clone();
        tokio::task::spawn_blocking(move || wake()).await.unwrap_or(false)
    }

    // -- reading --

    async fn now_inner(&self) -> Result<Dict, Error> {
        let now = now_of(self.api.player().await?.as_ref());
        let raw = self.api.devices().await?;
        let mut devices = Vec::with_capacity(raw.len());
        for d in &raw {
            let empty = Dict::new();
            let dd = d.as_object().unwrap_or(&empty);
            let mut pubd = device_public(dd);
            let color = self.colors.lock().hex(&or_empty(dd.get("name")));
            pubd.insert("color".into(), json!(color));
            devices.push(Value::Object(pubd));
        }
        let mut out = Dict::new();
        out.insert("ok".into(), json!(true));
        out.extend(now);
        out.insert("devices".into(), Value::Array(devices));
        Ok(out)
    }

    pub async fn now_playing(&self) -> anyhow::Result<Dict> {
        match self.now_inner().await {
            Ok(d) => Ok(d),
            Err(Error::Spotify(e)) => {
                let mut d = Dict::new();
                d.insert("ok".into(), json!(false));
                d.insert("reason".into(), json!(e.line));
                Ok(d)
            }
            Err(Error::Io(e)) => {
                // no network, DNS, a timeout
                tracing::warn!("spotify: now playing failed: {}", e);
                let mut d = Dict::new();
                d.insert("ok".into(), json!(false));
                d.insert("reason".into(), json!("Couldn't reach Spotify."));
                Ok(d)
            }
            Err(Error::Other(e)) => Err(e),
        }
    }

    // -- which device --

    /// A device by name: exact, then a word of its name, then its kind ("phone", "speaker", "computer").
    pub fn pick(devices: &[Value], name: &str) -> (Option<Value>, String) {
        static LEAD_THE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^(the|my)\s+").expect("re"));
        static TAIL_KIND: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"\s+(speaker|device)$").expect("re"));
        let n = norm(name);
        let n = LEAD_THE.replace(&n, "").into_owned();
        let n = if n != "speaker" && n != "device" { TAIL_KIND.replace(&n, "").into_owned() } else { n };
        let dname = |d: &Value| norm(&or_empty(d.get("name")));
        let usable: Vec<&Value> = devices.iter().filter(|d| !truthy(d.get("is_restricted"))).collect();
        let kind = match n.as_str() {
            "mac" | "laptop" | "computer" => "computer",
            "phone" | "iphone" => "smartphone",
            "speaker" => "speaker",
            "tv" => "tv",
            "ipad" | "tablet" => "tablet",
            _ => "\0",
        };
        // `\b` and `\w` as Python's re has them
        let b = py_boundary();
        let word = fancy_regex::Regex::new(&format!("{b}{}{b}", fancy_regex::escape(&n))).ok();
        let tests: [&dyn Fn(&Value) -> bool; 3] = [
            &|d| dname(d) == n,
            &|d| word.as_ref().is_some_and(|w| w.is_match(&dname(d)).unwrap_or(false)),
            &|d| or_empty(d.get("type")).to_lowercase() == kind,
        ];
        for test in tests {
            let found: Vec<&Value> = usable.iter().copied().filter(|d| test(d)).collect();
            if found.len() == 1 {
                return (Some(found[0].clone()), String::new());
            }
            if found.len() > 1 {
                let names: Vec<String> = found.iter().map(|d| d.get("name").map(py_str).unwrap_or_default()).collect();
                return (
                    None,
                    format!("More than one Spotify device matches {}: {}.", py_repr(name), join(&names)),
                );
            }
        }
        if devices.iter().any(|d| dname(d) == n) {
            return (None, format!("{name} doesn't take remote control from Spotify."));
        }
        let names = join(&usable.iter().map(|d| or_empty(d.get("name"))).collect::<Vec<_>>());
        let tail = if names.is_empty() {
            "No device is online: open Spotify on a phone, computer or speaker.".to_string()
        } else {
            format!("The devices are {names}.")
        };
        (None, format!("No Spotify device called {}. {tail}", py_repr(name)))
    }

    /// The devices; when there are none, open the Mac's Spotify and wait for it to come online.
    async fn devices_or_wake(&self) -> Result<Vec<Value>, Error> {
        let mut devices = self.api.devices().await?;
        if !devices.is_empty() || !self.wake().await {
            return Ok(devices);
        }
        tracing::info!("spotify: no device online; opened the Mac's Spotify");
        let mut waited = 0.0;
        while waited < self.wake_secs {
            self.sleep(1.0).await;
            waited += 1.0;
            devices = self.api.devices().await?;
            if !devices.is_empty() {
                break;
            }
        }
        Ok(devices)
    }

    /// Play, and when nothing is active, start on a device that is online (the Mac's Spotify, opened if
    /// need be) rather than fail the way the Web API does.
    async fn play_on(&self, device_id: Option<&str>, uris: Option<&[String]>, context: Option<&str>) -> Result<(), Error> {
        match self.api.play(device_id, uris, context).await {
            Ok(()) => return Ok(()),
            Err(Error::Spotify(e)) if e.reason == "NO_ACTIVE_DEVICE" && device_id.is_none_or(str::is_empty) => {}
            Err(e) => return Err(e),
        }
        let devices: Vec<Value> =
            self.devices_or_wake().await?.into_iter().filter(|d| !truthy(d.get("is_restricted"))).collect();
        if devices.is_empty() {
            return Err(SpotifyError::new(
                404,
                "NO_ACTIVE_DEVICE",
                "No Spotify device is online. Open Spotify on a phone, computer or speaker.",
            )
            .into());
        }
        let target = devices
            .iter()
            .find(|d| or_empty(d.get("type")).to_lowercase() == "computer")
            .unwrap_or(&devices[0]);
        let id = or_empty(target.get("id"));
        self.api.play(Some(&id), uris, context).await
    }

    // -- doing --

    /// One command; `{"ok", "line", ...}`. The line is what to tell the owner.
    pub async fn control(&self, action: &str, args: ControlArgs) -> anyhow::Result<Dict> {
        let kind = if args.kind.is_empty() { "track".to_string() } else { args.kind.clone() };
        let r = self
            .control_inner(action, py_strip(&args.query), &kind, args.volume, args.volume_change, py_strip(&args.device))
            .await;
        match r {
            Ok(d) => Ok(d),
            Err(Error::Spotify(e)) => {
                tracing::info!("spotify: {} refused ({} {})", action, e.status, e.reason);
                Ok(line_out(false, &e.line, true))
            }
            Err(Error::Io(e)) => {
                // no network, DNS, a timeout
                tracing::warn!("spotify: {} failed: {}", action, e);
                Ok(line_out(false, "Couldn't reach Spotify.", true))
            }
            Err(Error::Other(e)) => Err(e),
        }
    }

    async fn control_inner(
        &self,
        action: &str,
        query: &str,
        kind: &str,
        volume: Option<i64>,
        volume_change: Option<i64>,
        device: &str,
    ) -> Result<Dict, Error> {
        if action == "mode_on" || action == "mode_off" {
            let Some(mode) = self.mode() else {
                return Ok(line_out(false, "Spotify mode needs the Voice PE connected as eve's controller.", true));
            };
            let line = mode.set(action == "mode_on").await?;
            return Ok(line_out(true, &line, false));
        }
        if action == "now" {
            let mut now = self.now_playing().await?;
            let line = if truthy(now.get("ok")) {
                said_now(&now)
            } else {
                or_empty(now.get("reason"))
            };
            now.insert("line".into(), json!(line));
            return Ok(now);
        }
        let mut device_id: Option<String> = None;
        let mut picked: Option<Value> = None;
        if !device.is_empty() {
            let (found, why) = Self::pick(&self.devices_or_wake().await?, device);
            let Some(found) = found else {
                return Ok(line_out(false, &why, true));
            };
            device_id = Some(or_empty(found.get("id")));
            picked = Some(found);
        }
        let picked_name = picked.as_ref().map(|p| p.get("name").map(py_str).unwrap_or_default());
        let device_id = device_id.as_deref();
        match action {
            "play" => {
                if !query.is_empty() {
                    if !KINDS.contains(&kind) {
                        let mut d = Dict::new();
                        d.insert("ok".into(), json!(false));
                        d.insert("reason".into(), json!(format!("kind must be one of {}", KINDS.join(", "))));
                        d.insert("line".into(), json!("Unknown kind."));
                        return Ok(d);
                    }
                    let Some(item) = self.api.search(query, kind).await? else {
                        return Ok(line_out(false, &format!("Spotify found nothing for {}.", py_repr(query)), true));
                    };
                    let uri = py_str(&item["uri"]);
                    if kind == "track" {
                        self.play_on(device_id, Some(&[uri]), None).await?;
                    } else {
                        self.play_on(device_id, None, Some(&uri)).await?;
                    }
                    let name = item
                        .get("name")
                        .filter(|n| truthy(Some(n)))
                        .map(py_str)
                        .unwrap_or_else(|| query.to_string());
                    let by = if kind == "track" || kind == "album" {
                        let names: Vec<String> = item
                            .get("artists")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().map(|x| or_empty(x.get("name"))).collect())
                            .unwrap_or_default();
                        join(&names)
                    } else {
                        String::new()
                    };
                    let owner = if kind == "playlist" {
                        as_obj(item.get("owner")).map(|o| or_empty(o.get("display_name"))).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let mut line = format!("Playing {name}");
                    if !by.is_empty() {
                        line.push_str(&format!(" by {by}"));
                    }
                    if !owner.is_empty() {
                        line.push_str(&format!(" ({owner}'s playlist)"));
                    }
                    if let Some(p) = &picked_name {
                        line.push_str(&format!(" on {p}"));
                    }
                    line.push('.');
                    let mut d = line_out(true, &line, false);
                    d.insert("playing".into(), json!(name));
                    return Ok(d);
                }
                self.play_on(device_id, None, None).await?;
                let line = match &picked_name {
                    Some(p) => format!("Playing on {p}."),
                    None => "Playing.".to_string(),
                };
                Ok(line_out(true, &line, false))
            }
            "pause" => {
                if let Err(err) = self.api.pause().await {
                    if let Error::Spotify(e) = &err {
                        if e.reason == "NO_ACTIVE_DEVICE" {
                            return Ok(line_out(true, "Spotify is already quiet.", false));
                        }
                        // pausing what is already paused is a 403 "restriction violated", not a failure
                        if e.status == 403 && e.reason != "PREMIUM_REQUIRED" {
                            let player = self.api.player().await?;
                            if !truthy(as_obj(player.as_ref()).and_then(|p| p.get("is_playing"))) {
                                return Ok(line_out(true, "Spotify is already quiet.", false));
                            }
                        }
                    }
                    return Err(err);
                }
                Ok(line_out(true, "Paused.", false))
            }
            "next" | "previous" => {
                if action == "next" {
                    self.api.next().await?;
                } else {
                    self.api.previous().await?;
                }
                self.sleep(0.6).await; // the player takes a moment to change track
                let now = now_of(self.api.player().await?.as_ref());
                let head = if action == "next" { "Skipped." } else { "Back one." };
                let mut line = head.to_string();
                if truthy(now.get("track")) {
                    line.push_str(&format!(" Now {}", py_str(&now["track"])));
                    if truthy(now.get("artists")) {
                        line.push_str(&format!(" by {}", join(&str_list(now.get("artists")))));
                    }
                    line.push('.');
                }
                let mut d = line_out(true, &line, false);
                d.extend(now);
                Ok(d)
            }
            "volume" => {
                if volume.is_none() && volume_change.is_none() {
                    let mut d = Dict::new();
                    d.insert("ok".into(), json!(false));
                    d.insert("reason".into(), json!("give a volume or a volume_change"));
                    d.insert("line".into(), json!("What volume?"));
                    return Ok(d);
                }
                let volume = match volume {
                    Some(v) => v,
                    None => {
                        let player = self.api.player().await?;
                        let current = as_obj(player.as_ref())
                            .and_then(|p| as_obj(p.get("device")))
                            .and_then(|d| py_int(d.get("volume_percent")));
                        let Some(current) = current else {
                            return Err(SpotifyError::new(
                                404,
                                "NO_ACTIVE_DEVICE",
                                "Spotify isn't playing on any device right now.",
                            )
                            .into());
                        };
                        current.saturating_add(volume_change.unwrap_or(0))
                    }
                };
                let level = volume.clamp(0, 100);
                self.api.volume(level, device_id).await?;
                let mut d = line_out(true, &format!("Spotify volume {level}%."), false);
                d.insert("volume".into(), json!(level));
                Ok(d)
            }
            "transfer" => {
                let Some(id) = device_id.filter(|d| !d.is_empty()) else {
                    let mut d = Dict::new();
                    d.insert("ok".into(), json!(false));
                    d.insert("reason".into(), json!("transfer needs a device"));
                    d.insert("line".into(), json!("Which device?"));
                    return Ok(d);
                };
                self.api.transfer(id, true).await?;
                Ok(line_out(true, &format!("Playing on {}.", picked_name.unwrap_or_default()), false))
            }
            _ => {
                let mut d = Dict::new();
                d.insert("ok".into(), json!(false));
                d.insert("reason".into(), json!(format!("unknown action {}", py_repr(action))));
                d.insert("line".into(), json!("I can't do that with Spotify."));
                Ok(d)
            }
        }
    }

    // -- the model's tools --

    pub fn tools(&self) -> Vec<Value> {
        TOOLS.clone()
    }

    pub fn instructions(&self) -> &'static str {
        INSTRUCTIONS
    }

    pub async fn handle(&self, name: &str, args: &Dict) -> anyhow::Result<Dict> {
        if name == "spotify_now_playing" {
            return self.now_playing().await;
        }
        if name == "spotify_control" {
            let a = ControlArgs {
                query: or_empty(args.get("query")),
                kind: or_empty(args.get("kind")),
                volume: int_or_none(args.get("volume")),
                volume_change: int_or_none(args.get("volume_change")),
                device: or_empty(args.get("device")),
            };
            return self.control(&or_empty(args.get("action")), a).await;
        }
        let mut d = Dict::new();
        d.insert("ok".into(), json!(false));
        d.insert("reason".into(), json!(format!("unknown tool {name}")));
        Ok(d)
    }

    // -- a message that is only a music command --

    /// A Command when the whole message is a music command, else None (the model answers). Python's
    /// `Spotify.match`; it reads no state.
    pub fn match_text(&self, text: &str) -> Option<Command> {
        match_text(text)
    }

    pub async fn run(&self, cmd: &Command) -> anyhow::Result<String> {
        if cmd.action == "mode" {
            let out = self
                .control(if cmd.on == Some(true) { "mode_on" } else { "mode_off" }, ControlArgs::default())
                .await?;
            return Ok(out.get("line").map(py_str).unwrap_or_else(|| "None".to_string()));
        }
        let out = self
            .control(
                &cmd.action,
                ControlArgs {
                    query: cmd.query.clone(),
                    kind: String::new(),
                    volume: cmd.volume,
                    volume_change: cmd.change,
                    device: cmd.device.clone(),
                },
            )
            .await?;
        tracing::info!(
            "spotify: by code, {} -> {}",
            cmd.action,
            if truthy(out.get("ok")) { "done" } else { "refused" }
        );
        let line = or_empty(out.get("line"));
        Ok(if line.is_empty() { or_empty(out.get("reason")) } else { line })
    }

    // -- the Mini App's Music card --

    /// `{"action": "status"}` reads the player and the devices; `play`, `pause`, `next`, `previous`,
    /// `volume` (`volume`), `transfer` (`device`) change it and answer with the fresh state.
    pub async fn panel(&self, body: &Dict) -> anyhow::Result<Dict> {
        let action = {
            let a = or_empty(body.get("action"));
            if a.is_empty() { "status".to_string() } else { a }
        };
        if action == "status" {
            let mut now = self.now_playing().await?;
            now.insert("mode".into(), self.mode_value());
            return Ok(now);
        }
        if action == "mode" {
            let on = matches!(body.get("on"), Some(Value::Bool(true)));
            let out = self.control(if on { "mode_on" } else { "mode_off" }, ControlArgs::default()).await?;
            let mut now = self.now_playing().await?;
            now.insert("ok".into(), out.get("ok").cloned().unwrap_or(Value::Null));
            now.insert("line".into(), out.get("line").cloned().unwrap_or(Value::Null));
            now.insert("mode".into(), self.mode_value());
            return Ok(now);
        }
        if !ACTIONS.contains(&action.as_str()) || (action == "play" && truthy(body.get("query"))) {
            let mut d = Dict::new();
            d.insert("ok".into(), json!(false));
            d.insert("line".into(), json!("Not something the Music card does."));
            return Ok(d);
        }
        let out = self
            .control(
                &action,
                ControlArgs {
                    volume: int_or_none(body.get("volume")),
                    device: or_empty(body.get("device")),
                    ..ControlArgs::default()
                },
            )
            .await?;
        if !truthy(out.get("ok")) {
            let line = if truthy(out.get("line")) { out.get("line") } else { out.get("reason") };
            let mut d = Dict::new();
            d.insert("ok".into(), json!(false));
            d.insert("line".into(), line.cloned().unwrap_or(Value::Null));
            return Ok(d);
        }
        if matches!(action.as_str(), "play" | "pause" | "transfer") {
            self.sleep(0.4).await; // the player's state lags a command by a beat
        }
        let mut now = self.now_playing().await?;
        now.insert("ok".into(), json!(true));
        now.insert("line".into(), out.get("line").cloned().unwrap_or_else(|| json!("")));
        now.insert("mode".into(), self.mode_value());
        Ok(now)
    }

    pub async fn close(&self) {}
}

/// What music_mode needs of the hub: `now_playing()` and `control(action, **kw)` with `volume` and `device`.
#[async_trait]
impl eve_music_mode::SpotifyControl for Spotify {
    async fn now_playing(&self) -> anyhow::Result<Value> {
        Spotify::now_playing(self).await.map(Value::Object)
    }

    async fn control(&self, action: &str, kw: Map<String, Value>) -> anyhow::Result<Value> {
        let a = ControlArgs {
            query: kw.get("query").and_then(Value::as_str).unwrap_or_default().to_string(),
            kind: kw.get("kind").and_then(Value::as_str).unwrap_or_default().to_string(),
            volume: int_or_none(kw.get("volume")),
            volume_change: int_or_none(kw.get("volume_change")),
            device: kw.get("device").and_then(Value::as_str).unwrap_or_default().to_string(),
        };
        Spotify::control(self, action, a).await.map(Value::Object)
    }
}

/// Open the Mac's Spotify in the background, so it shows up as a Connect device. False when there is none.
pub fn open_mac_app() -> bool {
    if !cfg!(target_os = "macos") || !Path::new(MAC_APP).exists() || which::which("open").is_err() {
        return false;
    }
    let child = std::process::Command::new("open")
        .args(["-g", "-a", "Spotify"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else { return false };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Err(_) => return false,
        }
    }
}

// ---- construction --------------------------------------------------------------------------------------------

/// `EVE_SPOTIFY` is not one of 0/false/no/off. `environ` stands in for the process environment when given.
pub fn enabled(environ: Option<&HashMap<String, String>>) -> bool {
    let v = match environ {
        Some(env) => env.get("EVE_SPOTIFY").cloned().unwrap_or_default(),
        None => std::env::var("EVE_SPOTIFY").unwrap_or_default(),
    };
    !matches!(py_strip(&v).to_lowercase().as_str(), "0" | "false" | "no" | "off")
}

fn colors_path(p: &Path) -> PathBuf {
    p.with_file_name(COLORS_FILE)
}

/// The owner's Spotify, or None when switched off or never logged in (then no tool is lent).
pub fn make_spotify(
    path: Option<&Path>,
    environ: Option<&HashMap<String, String>>,
    http: Option<Arc<dyn Http>>,
) -> Option<Spotify> {
    if !enabled(environ) {
        tracing::info!("spotify: off (EVE_SPOTIFY)");
        return None;
    }
    let p = config_path(path);
    let cfg = load(Some(&p));
    if cfg.is_empty() {
        tracing::info!("spotify: not logged in (eve-bridge spotify login)");
        return None;
    }
    tracing::info!("spotify: on");
    let http = http.unwrap_or_else(urllib_http);
    let token = Token::with(cfg, http.clone(), Some(p.clone()), None);
    Some(Spotify::new(SpotifyApi::new(token, http)).with_colors(DeviceColors::new(Some(&colors_path(&p)))))
}

// ---- the CLI -------------------------------------------------------------------------------------------------

/// Where a line for the terminal goes (Python's `out=print`).
pub type Out<'a> = &'a mut (dyn FnMut(String) + Send);

/// Python's `webbrowser.open`: the system's browser, best effort.
pub fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[derive(Default)]
struct Got {
    code: String,
    error: String,
}

async fn serve_callback(
    mut stream: tokio::net::TcpStream,
    state: &str,
    got: &parking_lot::Mutex<Got>,
) -> bool {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        let n = match tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk)).await {
            Ok(Ok(n)) => n,
            _ => return false,
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 65536 {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let first = head.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    if method != "GET" {
        let _ = stream.write_all(b"HTTP/1.0 501 Unsupported method\r\nConnection: close\r\n\r\n").await;
        return false;
    }
    // urlparse(self.path): the fragment is not the query, and an absolute-form target still has its path
    let target = target.split_once('#').map_or(target, |(t, _)| t);
    let target = match target.split_once("://") {
        Some((_, after)) => after.find('/').map_or("", |i| &after[i..]),
        None => target,
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/callback" {
        let _ = stream.write_all(b"HTTP/1.0 404 Not Found\r\nConnection: close\r\n\r\n").await;
        return false;
    }
    let first_of = |key: &str| -> String {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(k, v)| k == key && !v.is_empty()) // parse_qs drops blank values
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default()
    };
    let ok = {
        let mut g = got.lock();
        if first_of("state") != state {
            g.error = "state mismatch".to_string();
        } else {
            g.code = first_of("code");
            g.error = first_of("error");
        }
        !g.code.is_empty() && g.error.is_empty()
    };
    let page: &[u8] = if ok {
        b"<h2>eve has Spotify. You can close this tab.</h2>"
    } else {
        b"<h2>Spotify login failed. Check the terminal.</h2>"
    };
    let header = format!(
        "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
        page.len()
    );
    let _ = stream.write_all(header.as_bytes()).await;
    let _ = stream.write_all(page).await;
    let _ = stream.flush().await;
    true
}

/// `str(OSError)`: `[Errno N] message`, as Python prints a socket error.
fn py_oserror(e: &io::Error) -> String {
    let text = e.to_string();
    match e.raw_os_error() {
        Some(n) => {
            let msg = text.strip_suffix(&format!(" (os error {n})")).unwrap_or(&text);
            format!("[Errno {n}] {msg}")
        }
        None => text,
    }
}

/// The one-time authorization-code + PKCE flow on the loopback redirect; saves the refresh token.
pub async fn login(
    client_id: &str,
    client_secret: &str,
    path: Option<&Path>,
    http: Arc<dyn Http>,
    out: Out<'_>,
    browser: Option<&(dyn Fn(&str) + Sync)>,
    timeout: f64,
) -> i32 {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", LOGIN_PORT)).await {
        Ok(l) => l,
        Err(e) => {
            out(format!("Port {LOGIN_PORT} is busy ({}). Close whatever holds it and run this again.", py_oserror(&e)));
            return 1;
        }
    };
    login_on(listener, client_id, client_secret, path, http, out, browser, timeout).await
}

/// [`login`] on a listener already bound (the tests bind any free port).
#[allow(clippy::too_many_arguments)]
pub async fn login_on(
    listener: tokio::net::TcpListener,
    client_id: &str,
    client_secret: &str,
    path: Option<&Path>,
    http: Arc<dyn Http>,
    out: Out<'_>,
    browser: Option<&(dyn Fn(&str) + Sync)>,
    timeout: f64,
) -> i32 {
    let (verifier, challenge) = pkce_pair();
    let state = token_urlsafe(16);
    let got = Arc::new(parking_lot::Mutex::new(Got::default()));
    let done = Arc::new(tokio::sync::Notify::new());
    let server = {
        let (got, done, state) = (got.clone(), done.clone(), state.clone());
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                if serve_callback(stream, &state, &got).await {
                    done.notify_one();
                }
            }
        })
    };
    let url = authorize_url(client_id, &challenge, &state);
    out(format!("Listening on {REDIRECT_URI}. The Spotify app's Redirect URI must be exactly that."));
    out(format!("If no browser opens, paste this:\n\n{url}\n"));
    match browser {
        Some(b) => b(&url),
        None => open_browser(&url),
    }
    let waited = tokio::time::timeout(Duration::from_secs_f64(timeout.max(0.0)), done.notified()).await;
    server.abort();
    if waited.is_err() {
        out("Timed out waiting for Spotify's redirect.".to_string());
        return 1;
    }
    let (code, error) = {
        let g = got.lock();
        (g.code.clone(), g.error.clone())
    };
    if !error.is_empty() || code.is_empty() {
        let why = if error.is_empty() { "no code came back".to_string() } else { error };
        out(format!("Login failed: {why}."));
        return 1;
    }
    let form = [
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("redirect_uri", REDIRECT_URI.to_string()),
        ("code_verifier", verifier),
    ];
    let data = match token_post(&form, client_id, client_secret, http.as_ref()).await {
        Ok(d) => d,
        Err(Error::Spotify(e)) => {
            out(e.line);
            return 1;
        }
        Err(e) => {
            out(e.to_string());
            return 1;
        }
    };
    // data.get("refresh_token", ""): kept as Spotify sent it; a missing, null or empty one is no login
    let refresh = data.get("refresh_token").cloned().unwrap_or_else(|| json!(""));
    let mut cfg = Dict::new();
    cfg.insert("client_id".into(), json!(client_id));
    let has_refresh = truthy(Some(&refresh));
    cfg.insert("refresh_token".into(), refresh);
    cfg.insert("scopes".into(), json!(SCOPES));
    if !client_secret.is_empty() {
        cfg.insert("client_secret".into(), json!(client_secret));
    }
    if !has_refresh {
        out("Spotify sent no refresh token.".to_string());
        return 1;
    }
    match save(&cfg, path) {
        Ok(p) => {
            out(format!(
                "Logged in. Saved to {} (private). Restart the daemon to lend it to the chat and the voice.",
                p.display()
            ));
            0
        }
        Err(e) => {
            out(format!("Could not save the login: {e}"));
            1
        }
    }
}

/// The command line's clap definition, read-only: `eve-bridge` renders argparse's `--help` layout from it (the
/// app's own `cli` still parses). Added for the eve-bridge port.
pub fn clap_command() -> clap::Command {
    cli_parser()
}

fn cli_parser() -> clap::Command {
    use clap::{Arg, ArgAction, Command as C};
    let mut cmd = C::new("eve-bridge spotify")
        .infer_long_args(true)
        .subcommand_required(true)
        .subcommand(
            C::new("login")
                .about("Log eve in to Spotify, once (a browser opens)")
                .arg(
                    Arg::new("client-id")
                        .long("client-id")
                        .help("the Spotify app's client id (or SPOTIFY_CLIENT_ID); spotKnob's app works"),
                )
                .arg(
                    Arg::new("secret-stdin")
                        .long("secret-stdin")
                        .action(ArgAction::SetTrue)
                        .help("read a client secret from stdin (not needed: PKCE logs in with the id alone)"),
                ),
        )
        .subcommand(C::new("logout").about("Forget the Spotify login on this Mac"))
        .subcommand(C::new("status").about("What is playing, and where"))
        .subcommand(C::new("devices").about("The Spotify Connect devices online"))
        .subcommand(
            C::new("play")
                .about("Resume, or play something by name")
                .arg(Arg::new("query").num_args(0..).action(ArgAction::Append))
                .arg(Arg::new("kind").long("kind").value_parser(KINDS).default_value("track"))
                .arg(Arg::new("device").long("device").default_value("")),
        );
    for name in ["pause", "next", "previous"] {
        cmd = cmd.subcommand(C::new(name));
    }
    cmd.subcommand(
        C::new("volume").arg(
            Arg::new("percent")
                .required(true)
                .allow_negative_numbers(true)
                .value_parser(clap::value_parser!(i64)),
        ),
    )
    .subcommand(
        C::new("transfer")
            .about("Move the music to another device")
            .arg(Arg::new("device").required(true).num_args(1..).action(ArgAction::Append)),
    )
}

fn words(m: &clap::ArgMatches, id: &str) -> String {
    m.get_many::<String>(id).map(|v| v.cloned().collect::<Vec<_>>().join(" ")).unwrap_or_default()
}

/// `eve-bridge spotify login [--client-id ID] [--secret-stdin] | status | devices | play [QUERY]
/// [--kind K] [--device D] | pause | next | previous | volume N | transfer DEVICE | logout`.
pub async fn cli(argv: &[String], path: Option<&Path>, out: Out<'_>, http: Option<Arc<dyn Http>>) -> i32 {
    let parsed = cli_parser().try_get_matches_from(std::iter::once("eve-bridge spotify".to_string()).chain(argv.iter().cloned()));
    let m = match parsed {
        Ok(m) => m,
        Err(e) => {
            let code = if e.use_stderr() { 2 } else { 0 };
            let _ = e.print();
            return code;
        }
    };
    let http = http.unwrap_or_else(urllib_http);
    let cfg_path = config_path(path);
    let Some((act, sub)) = m.subcommand() else { return 2 };

    if act == "login" {
        let client_id = sub
            .get_one::<String>("client-id")
            .cloned()
            .unwrap_or_else(|| std::env::var("SPOTIFY_CLIENT_ID").unwrap_or_default());
        if client_id.is_empty() {
            out("Give the Spotify app's client id: --client-id ID (developer.spotify.com/dashboard). See docs/spotify.md"
                .to_string());
            return 2;
        }
        let secret = if sub.get_flag("secret-stdin") {
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            py_strip(&line).to_string()
        } else {
            String::new()
        };
        return login(py_strip(&client_id), &secret, Some(&cfg_path), http, out, None, 300.0).await;
    }
    if act == "logout" {
        match std::fs::remove_file(&cfg_path) {
            Ok(()) => out(format!("Forgot the Spotify login ({}).", cfg_path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => out("Spotify was not logged in.".to_string()),
            Err(e) => {
                out(format!("Could not forget the Spotify login ({}): {e}", cfg_path.display()));
                return 1;
            }
        }
        return 0;
    }
    let cfg = load(Some(&cfg_path));
    if cfg.is_empty() {
        out(format!("Not logged in ({}). Run: eve-bridge spotify login --client-id ID", cfg_path.display()));
        return 1;
    }
    let token = Token::with(cfg, http.clone(), Some(cfg_path.clone()), None);
    let hub = Spotify::new(SpotifyApi::new(token, http))
        .with_colors(DeviceColors::new(Some(&colors_path(&cfg_path))));

    let result: anyhow::Result<Dict> = async {
        match act {
            "status" => {
                let mut now = hub.now_playing().await?;
                let line = if truthy(now.get("ok")) {
                    json!(said_now(&now))
                } else {
                    now.get("reason").cloned().unwrap_or(Value::Null)
                };
                now.insert("line".into(), line);
                Ok(now)
            }
            "devices" => {
                let now = hub.now_playing().await?;
                let mut d = Dict::new();
                d.insert("ok".into(), now.get("ok").cloned().unwrap_or(json!(false)));
                d.insert("devices".into(), now.get("devices").cloned().unwrap_or_else(|| json!([])));
                d.insert("line".into(), now.get("reason").cloned().unwrap_or_else(|| json!("")));
                Ok(d)
            }
            "play" => {
                let a = ControlArgs {
                    query: words(sub, "query"),
                    kind: sub.get_one::<String>("kind").cloned().unwrap_or_default(),
                    device: sub.get_one::<String>("device").cloned().unwrap_or_default(),
                    ..ControlArgs::default()
                };
                hub.control("play", a).await
            }
            "volume" => {
                let v = sub.get_one::<i64>("percent").copied();
                hub.control("volume", ControlArgs { volume: v, ..ControlArgs::default() }).await
            }
            "transfer" => {
                hub.control("transfer", ControlArgs { device: words(sub, "device"), ..ControlArgs::default() }).await
            }
            other => hub.control(other, ControlArgs::default()).await,
        }
    }
    .await;
    let result = match result {
        Ok(r) => r,
        Err(e) => {
            out(e.to_string());
            return 1;
        }
    };
    if truthy(result.get("line")) {
        out(py_str(&result["line"]));
    }
    if act == "status" || act == "devices" {
        for d in result.get("devices").and_then(Value::as_array).cloned().unwrap_or_default() {
            let dot = if truthy(d.get("active")) { '\u{25cf}' } else { '\u{25cb}' };
            let mut line = format!(
                "  {dot} {}  [{}]",
                d.get("name").map(py_str).unwrap_or_default(),
                d.get("type").map(py_str).unwrap_or_default()
            );
            if let Some(v) = d.get("volume") {
                line.push_str(&format!("  {}%", py_str(v)));
            }
            if truthy(d.get("restricted")) {
                line.push_str("  (no remote control)");
            }
            out(line);
        }
    }
    if truthy(result.get("ok")) { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_dumps_matches_python_separators_and_ascii() {
        let v = json!({"a": [1, "é"], "b": {}, "c": []});
        assert_eq!(py_dumps(&v, None), r#"{"a": [1, "\u00e9"], "b": {}, "c": []}"#);
        assert_eq!(py_dumps(&v, Some(2)), "{\n  \"a\": [\n    1,\n    \"\\u00e9\"\n  ],\n  \"b\": {},\n  \"c\": []\n}");
    }

    #[test]
    fn py_repr_picks_quotes_like_python() {
        assert_eq!(py_repr("zzzz"), "'zzzz'");
        assert_eq!(py_repr("don't"), "\"don't\"");
        assert_eq!(py_repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(py_repr("a\u{a0}b\u{200b}c\u{85}"), "'a\\xa0b\\u200bc\\x85'");
    }

    #[test]
    fn quote_plus_matches_python() {
        assert_eq!(quote_plus("a b~*/:"), "a+b~%2A%2F%3A");
        assert_eq!(urlencode(&[("q", "daft punk"), ("type", "artist")]), "q=daft+punk&type=artist");
    }

    #[test]
    fn join_reads_like_a_sentence() {
        assert_eq!(join::<&str>(&[]), "");
        assert_eq!(join(&["a", ""]), "a");
        assert_eq!(join(&["a", "b", "c"]), "a, b and c");
    }

    #[test]
    fn norm_drops_punctuation_and_curly_quotes() {
        assert_eq!(norm("  What\u{2019}s   PLAYING?! "), "what's playing");
        assert_eq!(norm("hey eve, pause spotify"), "hey eve pause spotify");
        assert_eq!(norm("pause\u{1c}the music\u{b2}!"), "pause the music\u{b2}");
    }

    #[test]
    fn the_off_switch_strips_as_python_strips() {
        let env = |v: &str| HashMap::from([("EVE_SPOTIFY".to_string(), v.to_string())]);
        assert!(!enabled(Some(&env(" Off\u{1c}"))));
        assert!(!enabled(Some(&env("0"))));
        assert!(enabled(Some(&env("1"))));
        assert!(enabled(Some(&HashMap::new())));
    }

    #[test]
    fn python_word_class_takes_n_digits_and_leaves_marks() {
        assert!(py_word('\u{b2}') && py_word('a') && py_word('_') && py_word('\u{928}'));
        assert!(!py_word('\u{94d}') && !py_word('\u{301}') && !py_word('-') && !py_word('\u{203f}'));
    }

    #[test]
    fn unicode_digits_read_as_python_int_reads_them() {
        assert_eq!(py_int_digits("40"), Some(40));
        assert_eq!(py_int_digits("\u{664}\u{660}"), Some(40));
        assert_eq!(py_int_digits("\u{1d7ce}\u{1d7d9}"), Some(1)); // bold 0, then double-struck 1
        assert_eq!(py_int_digits("\u{b2}"), None);
    }
}
