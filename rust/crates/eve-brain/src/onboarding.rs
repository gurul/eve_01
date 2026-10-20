//! A port of `onboarding.py`, the whole module.
//!
//! eve lite's two browser sign-ins: OpenRouter mints the model key, Composio Connect grants the apps.
//!
//! Owner, 2026-10-13: "use composio and openrouter as the only setup things". Neither sign-in asks anyone to paste a
//! key. Each is an OAuth authorization-code flow with PKCE (S256) that comes back to eve's own page on this Mac,
//! ``http://localhost:<port>`` (docs/plans/eve-lite.md, M3). The setup page calls ``start``, sends the browser to
//! the URL it returns, and hands the callback's query to ``finish``.
//!
//! * **The callback is checked by ``state`` alone.** The setup page's ``eve_mac`` cookie is ``SameSite=Strict``, and
//!   a browser does not send it on the provider's cross-site redirect back. So each start mints a 128-bit ``state``
//!   that is good for one callback within ``STATE_TTL_SECS``. A missing, unknown, used or expired one is refused
//!   (``StateError``, HTTP 400) before anything leaves the Mac. At most ``MAX_PENDING`` sign-ins wait at once; one
//!   more drops the oldest.
//! * **OpenRouter** (``OpenRouterLogin``). The browser goes to ``https://openrouter.ai/auth`` with ``callback_url``,
//!   the challenge, ``state`` and ``key_label``. The code is exchanged at ``/api/v1/auth/keys`` with the JSON body
//!   ``{code, code_verifier, code_challenge_method}``, and the answer's ``key`` is the model key.
//!   ``save_openrouter_key`` writes it to the env file (0600, atomically) and to this process's environment;
//!   ``credit`` reads what it can spend.
//! * **Composio Connect** (``ComposioConnectLogin``), as spike S1 measured it on 2026-10-13
//!   (docs/plans/eve-lite-s1.md). eve registers itself once as a public client (RFC 7591, no secret) with both
//!   loopback forms of the bound port's callback. The authorize request carries ``resource``, so the token is good
//!   at ``/mcp``. The token endpoint is on ``login.composio.dev``, not the issuer's host, so every endpoint is pinned
//!   here rather than derived from the issuer. The exchange is a form POST with no secret and no Basic header.
//!   ``composio-connect.json`` (0600) is written only once a token arrives, so the file existing means "signed in".
//!   It keeps the client id, which is reused while it lists the callback (Composio cannot delete a client).
//!
//! Failures are ``OnboardingError``. Its text is one plain line for the setup page, never a key, a code or a
//! provider's text. ``status`` is the HTTP status for the callback's answer. A key or token is handed to
//! ``telegram::hide_token`` the moment it arrives, and no log line here carries a key, token, code, state, verifier
//! or client id (AGENTS rule 10).

#[allow(unused_imports)]
use crate::prelude::*;

use std::fmt::Write as _;

use base64::Engine as _;
use indexmap::IndexMap;

// py: onboarding.py:57 `log` (Logger): tracing, target `eve_brain::onboarding`.

/// py: onboarding.py:59 `STATE_TTL_SECS`
/// a sign-in left open longer starts again; OpenRouter's codes live 10 minutes too
pub const STATE_TTL_SECS: f64 = 600.0;

/// py: onboarding.py:60 `MAX_PENDING`
pub const MAX_PENDING: usize = 8;

/// py: onboarding.py:61 `HTTP_TIMEOUT_SECS`
pub const HTTP_TIMEOUT_SECS: f64 = 20.0;

/// py: onboarding.py:62 `CREDIT_TIMEOUT_SECS`
pub const CREDIT_TIMEOUT_SECS: f64 = 15.0;

/// py: onboarding.py:63 `LABEL_MAX`
pub const LABEL_MAX: usize = 60;

/// py: onboarding.py:64 `MAX_CODE_CHARS`
pub const MAX_CODE_CHARS: usize = 4096;

/// py: onboarding.py:66 `OPENROUTER_AUTH_URL`
pub const OPENROUTER_AUTH_URL: &str = "https://openrouter.ai/auth";

/// py: onboarding.py:67 `OPENROUTER_KEYS_URL`
pub const OPENROUTER_KEYS_URL: &str = "https://openrouter.ai/api/v1/auth/keys";

// Composio Connect's authorization-server document, as S1 read it (bridge/tests/fixtures/composio_connect).

/// py: onboarding.py:70 `CONNECT_ISSUER`
pub const CONNECT_ISSUER: &str = "https://connect.composio.dev";

/// py: onboarding.py:71 `CONNECT_AUTHORIZE_URL`
pub const CONNECT_AUTHORIZE_URL: &str = "https://connect.composio.dev/oauth/authorize";

/// py: onboarding.py:72 `CONNECT_TOKEN_URL`
pub const CONNECT_TOKEN_URL: &str = "https://login.composio.dev/oauth2/token";

/// py: onboarding.py:73 `CONNECT_REGISTER_URL`
pub const CONNECT_REGISTER_URL: &str = "https://login.composio.dev/oauth2/register";

/// py: onboarding.py:74 `CONNECT_RESOURCE`
pub const CONNECT_RESOURCE: &str = "https://connect.composio.dev/mcp";

/// py: onboarding.py:75 `CONNECT_SCOPE`
pub const CONNECT_SCOPE: &str = "openid profile email offline_access";

/// py: onboarding.py:76 `CONNECT_TOKEN_FILE`, before `~` is expanded ([`expanduser`] expands it).
pub const CONNECT_TOKEN_FILE: &str = "~/.config/eve-bridge/composio-connect.json";

/// py: onboarding.py:77 `TOKEN_FILE_VERSION`
pub const TOKEN_FILE_VERSION: i64 = 1;

/// py: onboarding.py:78 `DEFAULT_TOKEN_SECS`
/// neither expires_in nor the token says: refresh early rather than late
pub const DEFAULT_TOKEN_SECS: f64 = 300.0;

// What the setup page shows: one plain line each.

/// py: onboarding.py:81 `STATE_MISSING`
pub const STATE_MISSING: &str = "That sign-in was cancelled or did not finish. Start it again from eve's setup page.";

/// py: onboarding.py:82 `STATE_UNKNOWN`
pub const STATE_UNKNOWN: &str = "That sign-in link was already used or is out of date. Start it again from eve's setup page.";

/// py: onboarding.py:83 `STATE_EXPIRED`
pub const STATE_EXPIRED: &str = "That sign-in took longer than 10 minutes. Start it again from eve's setup page.";

/// py: onboarding.py:84 `OPENROUTER_NOT_AUTHORIZED`
pub const OPENROUTER_NOT_AUTHORIZED: &str = "OpenRouter did not authorize eve. Sign in with OpenRouter again and click Authorize.";

/// py: onboarding.py:85 `OPENROUTER_CODE_EXPIRED`
pub const OPENROUTER_CODE_EXPIRED: &str = "OpenRouter's sign-in code expired: it lasts 10 minutes. Sign in with OpenRouter again.";

/// py: onboarding.py:86 `OPENROUTER_REFUSED`
pub const OPENROUTER_REFUSED: &str = "OpenRouter did not accept that sign-in. Sign in with OpenRouter again.";

/// py: onboarding.py:87 `OPENROUTER_FAILED`
pub const OPENROUTER_FAILED: &str = "OpenRouter could not finish the sign-in (HTTP {status}). Try again in a minute.";

/// py: onboarding.py:88 `OPENROUTER_UNREACHABLE`
pub const OPENROUTER_UNREACHABLE: &str = "eve could not reach OpenRouter. Check this Mac's internet connection and try again.";

/// py: onboarding.py:89 `OPENROUTER_NO_KEY`
pub const OPENROUTER_NO_KEY: &str = "OpenRouter answered without a key. Sign in with OpenRouter again.";

/// py: onboarding.py:90 `COMPOSIO_CANCELLED`
pub const COMPOSIO_CANCELLED: &str = "Composio sign-in was cancelled. You can sign in later from Settings → Apps.";

/// py: onboarding.py:91 `COMPOSIO_NOT_FINISHED`
pub const COMPOSIO_NOT_FINISHED: &str = "Composio did not finish the sign-in. Sign in with Composio again.";

/// py: onboarding.py:92 `COMPOSIO_GRANT_EXPIRED`
pub const COMPOSIO_GRANT_EXPIRED: &str = "That Composio sign-in expired or was already used. Sign in with Composio again.";

/// py: onboarding.py:93 `COMPOSIO_CLIENT_GONE`
pub const COMPOSIO_CLIENT_GONE: &str = "Composio no longer knows eve on this Mac. Sign in with Composio again.";

/// py: onboarding.py:94 `COMPOSIO_FAILED`
pub const COMPOSIO_FAILED: &str = "Composio could not finish the sign-in (HTTP {status}). Try again in a minute.";

/// py: onboarding.py:95 `COMPOSIO_UNREACHABLE`
pub const COMPOSIO_UNREACHABLE: &str = "eve could not reach Composio. Check this Mac's internet connection and try again.";

/// py: onboarding.py:96 `COMPOSIO_NO_TOKEN`
pub const COMPOSIO_NO_TOKEN: &str = "Composio answered without a sign-in token. Sign in with Composio again.";

/// py: onboarding.py:97 `COMPOSIO_NOT_REGISTERED`
pub const COMPOSIO_NOT_REGISTERED: &str = "Composio did not register eve on this Mac (HTTP {status}). Try again in a minute.";

/// py: onboarding.py:98 `COMPOSIO_NO_CALLBACK`
pub const COMPOSIO_NO_CALLBACK: &str = "Composio did not accept eve's sign-in address on this Mac. Try again in a minute.";

/// py: onboarding.py:99 `NOT_SAVED`
pub const NOT_SAVED: &str = "eve could not save the sign-in on this Mac ({kind}). Check the disk, then sign in again.";

/// py: onboarding.py:101 `_LOOPBACK`
pub(crate) const LOOPBACK: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// py: onboarding.py:102 `_KEY_SHAPE`, anchored (Python calls it with `fullmatch`)
/// one visible-ASCII word: no space, no line break
pub(crate) static KEY_SHAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\A[\x21-\x7e]{8,512}\z"#).expect("KEY_SHAPE"));

/// py: onboarding.py:103 `_TOKEN_SHAPE`, anchored
/// an access token may be a long JWT
pub(crate) static TOKEN_SHAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\A[\x21-\x7e]{8,16384}\z"#).expect("TOKEN_SHAPE"));

/// py: onboarding.py:104 `_ID_SHAPE`, anchored
pub(crate) static ID_SHAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\A[\x21-\x7e]{1,256}\z"#).expect("ID_SHAPE"));

/// py: onboarding.py:105 `_OAUTH_WORD`, anchored
/// an OAuth error code, safe to log
pub(crate) static OAUTH_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\A[a-z_]{1,40}\z"#).expect("OAUTH_WORD"));

/// py: onboarding.py:106 `_MAX_TOKEN_SECS`
/// beyond this an expiry is nonsense, not a lifetime
pub(crate) const MAX_TOKEN_SECS: f64 = 315360000.0;

/// py: onboarding.py:107 `_warned`: the reasons `credit` has logged once already.
static WARNED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// py: onboarding.py:110 `class OnboardingError(Exception):`
///
/// A sign-in that did not finish. ``to_string()`` is one plain line for the setup page; ``status`` is the HTTP
/// status the callback answers with (400: the browser's side, 502: the provider's, 500: this Mac's). `state` is
/// whether it is a [`StateError`] (Python's subclass).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OnboardingError {
    /// `str(error)`.
    pub message: String,
    pub status: i64,
    pub state: bool,
}

impl OnboardingError {
    /// py: onboarding.py:114 `def __init__(self, line: str, status: int = 502) -> None:`
    pub fn new(line: &str, status: i64) -> Self {
        OnboardingError { message: line.to_string(), status, state: false }
    }

    /// `isinstance(error, StateError)`.
    pub fn is_state(&self) -> bool {
        self.state
    }
}

/// py: onboarding.py:119 `class StateError(OnboardingError):`
///
/// The callback's ``state`` is missing, unknown, used or expired. Raised before any request leaves the Mac.
/// (Python bases: OnboardingError; here it converts into one, status 400.)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct StateError {
    /// `str(error)`.
    pub message: String,
}

impl StateError {
    /// py: onboarding.py:122 `def __init__(self, line: str) -> None:`
    pub fn new(line: &str) -> Self {
        StateError { message: line.to_string() }
    }

    /// Always 400: the browser's side.
    pub fn status(&self) -> i64 {
        400
    }
}

impl From<StateError> for OnboardingError {
    fn from(e: StateError) -> Self {
        OnboardingError { message: e.message, status: 400, state: true }
    }
}

/// What a call that is not only a sign-in step can fail with: Python's ValueError (a wiring mistake: a callback
/// off this Mac, a value that is not a key), an [`OnboardingError`], or the RuntimeError pathlib raises when `~`
/// cannot be expanded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignInError {
    #[error("{0}")]
    Value(String),
    #[error(transparent)]
    Onboarding(#[from] OnboardingError),
    #[error("{0}")]
    Runtime(String),
}

/// The line `start` refuses a callback with.
const NOT_LOOPBACK: &str = "onboarding: the callback must be eve's own http://localhost:<port>/... page";

/// `str.isprintable()` for one character: not a control, a format character, a separator other than the ASCII
/// space, a surrogate, a private-use character or a non-character.
pub(crate) fn is_printable(c: char) -> bool {
    let u = c as u32;
    !(u < 0x20
        || (0x7f..=0xa0).contains(&u)
        || u == 0xad
        || (0x600..=0x605).contains(&u)
        || u == 0x61c
        || u == 0x6dd
        || u == 0x70f
        || (0x890..=0x891).contains(&u)
        || u == 0x8e2
        || u == 0x1680
        || u == 0x180e
        || (0x2000..=0x200f).contains(&u)
        || (0x2028..=0x202f).contains(&u)
        || (0x205f..=0x2064).contains(&u)
        || (0x2066..=0x206f).contains(&u)
        || u == 0x3000
        || (0xd800..=0xf8ff).contains(&u)
        || (0xfdd0..=0xfdef).contains(&u)
        || u == 0xfeff
        || (0xfff9..=0xfffb).contains(&u)
        || (u & 0xfffe) == 0xfffe
        || u == 0x110bd
        || u == 0x110cd
        || (0x13430..=0x1343f).contains(&u)
        || (0x1bca0..=0x1bca3).contains(&u)
        || (0x1d173..=0x1d17a).contains(&u)
        || u == 0xe0001
        || (0xe0020..=0xe007f).contains(&u)
        || u >= 0xf0000)
}

/// `socket.gethostname()`.
fn gethostname() -> std::io::Result<String> {
    let mut buf = vec![0u8; 256];
    // SAFETY: buf is writable for its whole length, and gethostname writes at most that many bytes.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// py: onboarding.py:126 `def mac_label() -> str:`
///
/// "eve lite on <this Mac>", the label OpenRouter gives the key and the name Composio gives the client, so the
/// person can tell them apart in their accounts. Read from the host name: the lite firewall lets almost no
/// program start, so ``scutil --get ComputerName`` is not an option.
pub fn mac_label() -> String {
    mac_label_from(gethostname())
}

/// [`mac_label`] for a host name already read (Python's tests patch ``socket.gethostname``).
pub fn mac_label_from(hostname: std::io::Result<String>) -> String {
    let name = hostname.unwrap_or_default();
    let stripped = eve_envfile::py_strip(&name);
    let stripped = stripped.strip_suffix(".local").unwrap_or(stripped);
    let kept: String = stripped.chars().filter(|c| is_printable(*c)).take(40).collect();
    let name = eve_envfile::py_strip(&kept);
    format!("eve lite on {}", if name.is_empty() { "this Mac" } else { name })
}

/// py: onboarding.py:139 `class _Pending:`
///
/// What one waiting sign-in's callback needs.
#[derive(Clone, PartialEq, Eq)]
pub struct _Pending {
    pub verifier: String,
    pub redirect_uri: String,
    pub client_id: String,
    pub redirect_uris: Vec<String>,
}

impl std::fmt::Debug for _Pending {
    /// The verifier is left out, as Python's `field(repr=False)`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("_Pending")
            .field("redirect_uri", &self.redirect_uri)
            .field("client_id", &self.client_id)
            .field("redirect_uris", &self.redirect_uris)
            .finish()
    }
}

/// `secrets.token_urlsafe(n)`: n random bytes, unpadded base64url.
fn token_urlsafe(n: usize) -> String {
    use rand::RngCore as _;
    let mut bytes = vec![0u8; n];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `hmac.compare_digest(a, b)`: equal bytes, in time that does not depend on where they differ.
fn compare_digest(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (i, x) in a.iter().enumerate() {
        diff |= (*x ^ b.get(i).copied().unwrap_or(!*x)) as usize;
    }
    diff == 0
}

/// py: onboarding.py:147 `class _States:`
///
/// The sign-ins waiting for their callback, by ``state``. Each is good once, for ``STATE_TTL_SECS``.
pub struct _States {
    pub(crate) clock: Clock,
    /// in the order they were issued: the first is the oldest
    pub(crate) waiting: Mutex<IndexMap<String, (f64, _Pending)>>,
}

impl _States {
    /// py: onboarding.py:150 `def __init__(self, clock: Callable[[], float]) -> None:`
    pub fn new(clock: Clock) -> Self {
        _States { clock, waiting: Mutex::new(IndexMap::new()) }
    }

    /// py: onboarding.py:154 `def __len__(self) -> int:`
    pub fn len(&self) -> usize {
        self.waiting.lock().len()
    }

    /// `len(states) == 0`.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// py: onboarding.py:157 `def issue(self, pending: _Pending) -> str:`
    pub fn issue(&self, pending: &_Pending) -> String {
        let now = (self.clock)();
        let mut waiting = self.waiting.lock();
        waiting.retain(|_, w| w.0 > now);
        while waiting.len() >= MAX_PENDING {
            waiting.shift_remove_index(0); // the oldest
        }
        let state = token_urlsafe(16); // 128 bits
        waiting.insert(state.clone(), (now + STATE_TTL_SECS, pending.clone()));
        state
    }

    /// py: onboarding.py:166 `def take(self, state: Any) -> _Pending:`
    ///
    /// The sign-in ``state`` belongs to, which is then used up. StateError when there is none.
    pub fn take(&self, state: &Value) -> Result<_Pending, StateError> {
        let given = match state {
            Value::String(s) if !s.is_empty() => s.as_bytes().to_vec(),
            _ => return Err(StateError::new(STATE_MISSING)),
        };
        let mut waiting = self.waiting.lock();
        let found = waiting.keys().find(|s| compare_digest(s.as_bytes(), &given)).cloned();
        let Some(found) = found else {
            return Err(StateError::new(STATE_UNKNOWN));
        };
        let (expires, pending) = waiting.shift_remove(&found).expect("the state just found");
        if expires <= (self.clock)() {
            return Err(StateError::new(STATE_EXPIRED));
        }
        Ok(pending)
    }
}

// ---- urllib.parse.urlsplit, the parts read here --------------------------------------------------------------

/// `urllib.parse.urlsplit(url)`, the parts a callback is checked by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitUrl {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

const SCHEME_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+-.";

impl SplitUrl {
    /// `urlsplit(url)`: None where Python raises ValueError (unbalanced brackets).
    pub fn parse(url: &str) -> Option<SplitUrl> {
        let trimmed = url.trim_start_matches(|c: char| (c as u32) <= 0x20);
        let url: String = trimmed.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
        let mut scheme = String::new();
        let mut rest: &str = &url;
        if let Some(i) = url.find(':') {
            if i > 0
                && url.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && url[..i].chars().all(|c| SCHEME_CHARS.contains(c))
            {
                scheme = url[..i].to_ascii_lowercase();
                rest = &url[i + 1..];
            }
        }
        let mut netloc = String::new();
        if let Some(after) = rest.strip_prefix("//") {
            let delim = after.find(['/', '?', '#']).unwrap_or(after.len());
            netloc = after[..delim].to_string();
            rest = &after[delim..];
            if netloc.contains('[') != netloc.contains(']') {
                return None;
            }
        }
        let (rest, fragment) = match rest.split_once('#') {
            Some((r, f)) => (r, f.to_string()),
            None => (rest, String::new()),
        };
        let (path, query) = match rest.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (rest.to_string(), String::new()),
        };
        Some(SplitUrl { scheme, netloc, path, query, fragment })
    }

    fn userinfo(&self) -> Option<&str> {
        self.netloc.rsplit_once('@').map(|(u, _)| u)
    }

    fn hostinfo(&self) -> (&str, Option<&str>) {
        let host_port = self.netloc.rsplit_once('@').map_or(self.netloc.as_str(), |(_, h)| h);
        let (hostname, port) = match host_port.split_once('[') {
            Some((_, bracketed)) => {
                let (hostname, rest) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (hostname, rest.split_once(':').map_or("", |(_, p)| p))
            }
            None => host_port.split_once(':').unwrap_or((host_port, "")),
        };
        (hostname, if port.is_empty() { None } else { Some(port) })
    }

    /// `.username`.
    pub fn username(&self) -> Option<&str> {
        self.userinfo().map(|u| u.split_once(':').map_or(u, |(n, _)| n))
    }

    /// `.password`.
    pub fn password(&self) -> Option<&str> {
        self.userinfo().and_then(|u| u.split_once(':').map(|(_, p)| p))
    }

    /// `.hostname`: lower case, brackets off; None when empty.
    pub fn hostname(&self) -> Option<String> {
        let (hostname, _) = self.hostinfo();
        if hostname.is_empty() {
            None
        } else {
            Some(hostname.to_lowercase())
        }
    }

    /// `.port`: None when there is none; `Err` where Python raises ValueError (not digits, out of range).
    pub fn port(&self) -> Result<Option<u32>, ()> {
        match self.hostinfo().1 {
            None => Ok(None),
            Some(p) => {
                if !p.chars().all(|c| c.is_ascii_digit()) {
                    return Err(());
                }
                let trimmed = p.trim_start_matches('0');
                if trimmed.len() > 5 {
                    return Err(());
                }
                let n: u32 = if trimmed.is_empty() { 0 } else { trimmed.parse().map_err(|_| ())? };
                if n > 65535 {
                    return Err(());
                }
                Ok(Some(n))
            }
        }
    }
}

/// A checked loopback callback: the split URL and its port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callback {
    pub parts: SplitUrl,
    pub hostname: String,
    pub port: u32,
}

/// py: onboarding.py:180 `def _loopback(url: str) -> urllib.parse.SplitResult:`
///
/// ``url`` checked as eve's own callback on this Mac: plain http on a loopback host and port, no credentials,
/// query or fragment (both providers append their own query). ValueError otherwise: the onboarding routes live
/// only on the local listener, so anything else is a wiring mistake.
pub(crate) fn loopback(url: &str) -> Result<Callback, SignInError> {
    let refuse = || SignInError::Value(NOT_LOOPBACK.to_string());
    let parts = SplitUrl::parse(url).ok_or_else(refuse)?;
    let port = parts.port().unwrap_or(None);
    let hostname = parts.hostname();
    let ok = parts.scheme == "http"
        && hostname.as_deref().is_some_and(|h| LOOPBACK.contains(&h))
        && port.is_some_and(|p| p != 0)
        && parts.username().is_none()
        && parts.password().is_none()
        && parts.query.is_empty()
        && parts.fragment.is_empty()
        && parts.path.starts_with('/');
    if !ok {
        return Err(refuse());
    }
    Ok(Callback { hostname: hostname.unwrap_or_default(), port: port.unwrap_or_default(), parts })
}

/// py: onboarding.py:195 `def _redirect_forms(parts: urllib.parse.SplitResult) -> list[str]:`
///
/// The two loopback forms of the callback that S1 registered and Composio accepted: localhost, then 127.0.0.1.
pub(crate) fn redirect_forms(parts: &Callback) -> Vec<String> {
    ["localhost", "127.0.0.1"].iter().map(|host| format!("http://{host}:{}{}", parts.port, parts.parts.path)).collect()
}

/// py: onboarding.py:200 `def _redirect_uri(parts: urllib.parse.SplitResult) -> str:`
///
/// The callback Composio is told: the page's own form, except that an IPv6 literal comes back on localhost,
/// a form S1 tested. (OpenRouter is always told the localhost form: localhost is what it documents.)
pub(crate) fn redirect_uri(parts: &Callback) -> String {
    let mut forms = redirect_forms(parts);
    if parts.hostname == "127.0.0.1" {
        forms.swap_remove(1)
    } else {
        forms.swap_remove(0)
    }
}

/// py: onboarding.py:207 `def _clean_label(label: str) -> str:`
pub(crate) fn clean_label(label: &str) -> String {
    let text = crate::think::squash(label);
    let kept: String = text.chars().filter(|c| is_printable(*c)).take(LABEL_MAX).collect();
    let kept = eve_envfile::py_strip(&kept);
    if kept.is_empty() {
        mac_label()
    } else {
        kept.to_string()
    }
}

/// `urllib.parse.quote(s, safe="")`: letters, digits and `_.-~` kept, every other UTF-8 byte as %XX.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// `urlencode(pairs, quote_via=quote)`.
fn urlencode_quote(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", quote(k), quote(v))).collect::<Vec<_>>().join("&")
}

/// py: onboarding.py:213 `async def _client(http: Optional[httpx.AsyncClient], timeout: float = HTTP_TIMEOUT_SECS ) -> AsyncIterator[httpx.AsyncClient]:`
///
/// The client to call with: the one handed in (tests pass one on a mock transport), else a fresh one with
/// `timeout`.
pub(crate) fn client(http: Option<&Http>, timeout: f64) -> Client {
    match http {
        Some(h) => Client { http: h.clone(), timeout: None },
        None => Client { http: real_http(), timeout: Some(timeout) },
    }
}

/// An httpx client: a transport, and the timeout of a client made here (None: the lent client's own).
#[derive(Clone)]
pub struct Client {
    pub http: Http,
    pub timeout: Option<f64>,
}

impl Client {
    async fn send(&self, method: &str, url: &str, headers: Vec<(String, String)>, body: Vec<u8>) -> Result<HttpResponse, HttpError> {
        self.http
            .send(HttpRequest { method: method.into(), url: url.into(), headers, body, timeout: self.timeout })
            .await
    }

    /// `client.post(url, json=body, headers=...)`.
    async fn post_json(&self, url: &str, body: &Value, extra: &[(&str, &str)]) -> Result<HttpResponse, HttpError> {
        let mut headers: Vec<(String, String)> = extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        headers.push(("Content-Type".into(), "application/json".into()));
        self.send("POST", url, headers, serde_json::to_vec(body).unwrap_or_default()).await
    }

    /// `client.post(url, data=form, headers=...)`: the form url-encoded as httpx writes it.
    async fn post_form(&self, url: &str, form: &[(&str, String)], extra: &[(&str, &str)]) -> Result<HttpResponse, HttpError> {
        let mut headers: Vec<(String, String)> = extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        headers.push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
        self.send("POST", url, headers, eve_apps_mcp::py::urlencode(form).into_bytes()).await
    }

    /// `client.get(url, headers=...)`.
    async fn get(&self, url: &str, headers: &[(String, String)]) -> Result<HttpResponse, HttpError> {
        self.send("GET", url, headers.to_vec(), Vec::new()).await
    }
}

/// `json.loads` of a provider's reply, as Python reads it: `NaN`, `Infinity` and a number too big for a float are
/// accepted. A `serde_json::Value` cannot hold those, so each becomes null, which every reader here takes as
/// "not a usable number", as Python's readers take them. None where Python raises ValueError.
pub(crate) fn loads(text: &str) -> Option<Value> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            let ch = text[i..].chars().next()?;
            out.push(ch);
            if b == b'\\' {
                if let Some(next) = text[i + 1..].chars().next() {
                    out.push(next);
                    i += 1 + next.len_utf8();
                    continue;
                }
            } else if b == b'"' {
                in_string = false;
            }
            i += ch.len_utf8();
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        let rest = &text[i..];
        if let Some(word) = ["NaN", "Infinity", "-Infinity"].iter().find(|w| rest.starts_with(**w)) {
            out.push_str("null");
            i += word.len();
            continue;
        }
        if b == b'-' || b.is_ascii_digit() {
            let end = rest.find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))).unwrap_or(rest.len());
            let token = &rest[..end];
            match token.parse::<f64>() {
                Ok(f) if !f.is_finite() => out.push_str("null"),
                _ => out.push_str(token),
            }
            i += end;
            continue;
        }
        let ch = rest.chars().next()?;
        out.push(ch);
        i += ch.len_utf8();
    }
    serde_json::from_str(&out).ok()
}

/// py: onboarding.py:223 `def _answer(response: httpx.Response) -> dict[str, Any]:`
pub(crate) fn answer(response: &HttpResponse) -> Obj {
    let Ok(text) = std::str::from_utf8(&response.body) else {
        return Obj::new();
    };
    match loads(text.strip_prefix('\u{feff}').unwrap_or(text)) {
        Some(Value::Object(m)) => m,
        _ => Obj::new(),
    }
}

/// `_hide(*values)`: what keeps a key or token out of every log line from now on.
pub type Hide = Arc<dyn Fn(&[&str]) + Send + Sync>;

/// The real `_hide`: ``telegram::hide_token`` for each value that is not "".
pub fn default_hide() -> Hide {
    Arc::new(|values: &[&str]| hide(values))
}

/// py: onboarding.py:231 `def _hide(*values: str) -> None:`
///
/// Keep these out of every log record in the process from now on, whoever writes it. ``hide_token`` is
/// idempotent; "" is passed over.
pub(crate) fn hide(values: &[&str]) {
    for value in values {
        if !value.is_empty() {
            crate::telegram::hide_token(value);
        }
    }
}

/// py: onboarding.py:241 `def _code_shaped(code: Any) -> bool:`
///
/// An authorization code worth sending on: a non-empty string of sane length. Codes are opaque and short; a
/// huge or empty one is refused here rather than posted to the provider.
pub(crate) fn code_shaped(code: &Value) -> bool {
    code.as_str().is_some_and(|c| {
        let n = c.chars().count();
        0 < n && n <= MAX_CODE_CHARS
    })
}

/// py: onboarding.py:247 `def _oauth_word(answer: Mapping[str, Any]) -> str:`
///
/// The answer's OAuth ``error`` code when it looks like one (safe to log and to branch on), else "".
pub(crate) fn oauth_word(answer: &Obj) -> String {
    match answer.get("error").and_then(Value::as_str) {
        Some(w) if OAUTH_WORD.is_match(w) => w.to_string(),
        _ => String::new(),
    }
}

/// Python's `type(e).__name__` for an OSError.
fn os_error_name(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind::*;
    match e.kind() {
        PermissionDenied => "PermissionError",
        NotFound => "FileNotFoundError",
        AlreadyExists => "FileExistsError",
        IsADirectory => "IsADirectoryError",
        NotADirectory => "NotADirectoryError",
        Interrupted => "InterruptedError",
        TimedOut => "TimeoutError",
        _ => "OSError",
    }
}

// ---- OpenRouter -------------------------------------------------------------------------------------------

/// py: onboarding.py:255 `class OpenRouterLogin:`
///
/// OpenRouter's PKCE sign-in, which mints eve's one model key: ``start``, the browser, then ``finish``.
pub struct OpenRouterLogin {
    pub(crate) http: Option<Http>,
    pub(crate) states: _States,
    pub(crate) hide: Hide,
}

impl OpenRouterLogin {
    /// py: onboarding.py:258 `def __init__(self, *, http: Optional[httpx.AsyncClient] = None, clock: Callable[[], float] = time.time) -> None:`
    ///
    /// `clock` None is `time.time`.
    pub fn new(http: Option<Http>, clock: Option<Clock>) -> Self {
        OpenRouterLogin {
            http,
            states: _States::new(clock.unwrap_or_else(|| Arc::new(wall))),
            hide: default_hide(),
        }
    }

    /// `_hide` lent (Python's tests patch the module function).
    pub fn with_hide(mut self, hide: Hide) -> Self {
        self.hide = hide;
        self
    }

    /// py: onboarding.py:263 `def start(self, callback_url: str, label: str = "") -> str:`
    ///
    /// The URL to send the browser to. ``callback_url`` is eve's own loopback callback, which OpenRouter is
    /// given in its ``localhost`` form, the one it documents for any port; ``label`` names the key in the person's
    /// OpenRouter account (default: ``mac_label()``).
    pub fn start(&self, callback_url: &str, label: &str) -> Result<String, SignInError> {
        let redirect = redirect_forms(&loopback(callback_url)?).swap_remove(0);
        let (verifier, challenge) = eve_apps_mcp::pkce();
        let state = self.states.issue(&_Pending {
            verifier,
            redirect_uri: redirect.clone(),
            client_id: String::new(),
            redirect_uris: Vec::new(),
        });
        let label = clean_label(label);
        Ok(format!(
            "{OPENROUTER_AUTH_URL}?{}",
            urlencode_quote(&[
                ("callback_url", &redirect),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("state", &state),
                ("key_label", &label),
            ])
        ))
    }

    /// py: onboarding.py:274 `async def finish(self, code: str, state: str, *, error: str = "") -> str:`
    ///
    /// The key, for the callback's ``code`` and ``state`` (strings, or anything a query gives: a missing one is
    /// null). ``error`` is the callback's own, when it has one. StateError (400) with no request made for a bad
    /// ``state``; OnboardingError for anything else.
    pub async fn finish(&self, code: impl Into<Value>, state: impl Into<Value>, error: &str) -> Result<String, OnboardingError> {
        let (code, state) = (code.into(), state.into());
        let pending = self.states.take(&state)?;
        if !error.is_empty() || !code_shaped(&code) {
            return Err(OnboardingError::new(OPENROUTER_NOT_AUTHORIZED, 400));
        }
        let body = json!({"code": code, "code_verifier": pending.verifier, "code_challenge_method": "S256"});
        let response = match client(self.http.as_ref(), HTTP_TIMEOUT_SECS).post_json(OPENROUTER_KEYS_URL, &body, &[]).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("onboarding: the OpenRouter key exchange failed ({})", e.kind);
                return Err(OnboardingError::new(OPENROUTER_UNREACHABLE, 502));
            }
        };
        let answer = answer(&response);
        if response.status != 200 {
            tracing::warn!("onboarding: OpenRouter refused the key exchange (HTTP {})", response.status);
            if response.status == 403 && openrouter_reason(&answer).to_lowercase().contains("expire") {
                return Err(OnboardingError::new(OPENROUTER_CODE_EXPIRED, 502));
            }
            if matches!(response.status, 400 | 403) {
                return Err(OnboardingError::new(OPENROUTER_REFUSED, 502));
            }
            return Err(OnboardingError::new(&OPENROUTER_FAILED.replace("{status}", &response.status.to_string()), 502));
        }
        let key = answer.get("key").and_then(Value::as_str).map(eve_envfile::py_strip).unwrap_or("").to_string();
        if !KEY_SHAPE.is_match(&key) {
            tracing::warn!("onboarding: OpenRouter's answer held no usable key");
            return Err(OnboardingError::new(OPENROUTER_NO_KEY, 502));
        }
        (self.hide)(&[&key]);
        tracing::info!("onboarding: OpenRouter minted a key");
        Ok(key)
    }
}

/// py: onboarding.py:305 `def _openrouter_reason(answer: Mapping[str, Any]) -> str:`
///
/// OpenRouter's error message ("Authorization code expired"), to tell its two 403s apart. Never shown or
/// logged.
pub(crate) fn openrouter_reason(answer: &Obj) -> String {
    let error = answer.get("error");
    let message = match error {
        Some(Value::Object(e)) => e.get("message"),
        other => other,
    };
    if let Some(Value::String(m)) = message {
        return m.clone();
    }
    match answer.get("message") {
        Some(m) if crate::think::truthy(m) => crate::think::py_str(m),
        _ => String::new(),
    }
}

/// py: onboarding.py:313 `def save_openrouter_key(key: str, *, path: Optional[Path] = None, environ: MutableMapping[str, str] = os.environ) -> None:`
///
/// Keep the key OpenRouter minted. It is hidden from every log line, written to the env file the daemon reads
/// (``envfile::set_key``: 0600, atomic, the file's other lines kept) and set in ``environ`` (the process's
/// environment is `eve_envfile::ProcessEnv`). The caller restarts the daemon. ValueError for a value that is not a
/// key; OnboardingError (500) when the file cannot be written.
pub fn save_openrouter_key(key: &str, path: Option<&Path>, environ: &mut dyn eve_envfile::Environ) -> Result<(), SignInError> {
    save_openrouter_key_with(key, path, environ, &default_hide())
}

/// [`save_openrouter_key`] with `_hide` lent.
pub fn save_openrouter_key_with(
    key: &str,
    path: Option<&Path>,
    environ: &mut dyn eve_envfile::Environ,
    hide: &Hide,
) -> Result<(), SignInError> {
    if !KEY_SHAPE.is_match(key) {
        return Err(SignInError::Value("onboarding: that is not an OpenRouter key".to_string()));
    }
    hide(&[key]);
    let target = match path {
        Some(p) => p.to_path_buf(),
        None => eve_envfile::env_path(&*environ).map_err(|e| SignInError::Runtime(e.to_string()))?,
    };
    if let Err(e) = eve_envfile::set_key(&target, eve_openrouter::KEY_ENV, key) {
        let kind = match &e {
            eve_envfile::EnvFileError::Io(io) => os_error_name(io),
            eve_envfile::EnvFileError::HomeDir => return Err(SignInError::Runtime(e.to_string())),
            other if other.is_value_error() => "ValueError",
            _ => "OSError",
        };
        tracing::warn!("onboarding: the OpenRouter key was not saved ({})", kind);
        return Err(OnboardingError::new(&NOT_SAVED.replace("{kind}", kind), 500).into());
    }
    environ
        .set_var(eve_openrouter::KEY_ENV, key)
        .map_err(|_| SignInError::Value("embedded null byte".to_string()))?;
    tracing::info!("onboarding: saved {}", eve_openrouter::KEY_ENV);
    Ok(())
}

/// py: onboarding.py:331 `REJECTED_WHY`
/// /api/v1/key answering 401: the key was deleted or revoked, not a passing error
pub const REJECTED_WHY: &str = "HTTP 401";

/// py: onboarding.py:334 `async def credit(key: str, *, http: Optional[httpx.AsyncClient] = None) -> dict[str, Any]:`
///
/// What the key can spend, for the setup page: ``{"ok", "usd", "funded"}``, plus ``"error"`` (a short reason:
/// "HTTP 401", "timed out") when the key itself could not be read, and ``"rejected": True`` when that reason is a
/// 401 (the key no longer works and a new sign-in is the only fix, not a passing network error). Never fails, and
/// never holds the key.
///
/// ``usd`` is the smaller of the account's balance (``/api/v1/credits``: bought less used) and the key's own
/// remaining limit (``limit_remaining``, null for a key with no limit, which is how OAuth mints them). It is None
/// when neither can be read: OpenRouter documents ``/credits`` as needing a management key, which an OAuth key is
/// not. ``funded`` is whether a paid call can go through: usd above zero; without a figure, False only when the
/// account has never bought credits (``is_free_tier``), else None (not known).
pub async fn credit(key: &str, http: Option<Http>) -> Obj {
    credit_with(key, http, &default_hide()).await
}

/// [`credit`] with `_hide` lent.
pub async fn credit_with(key: &str, http: Option<Http>, hide: &Hide) -> Obj {
    let key = eve_envfile::py_strip(key);
    if key.is_empty() {
        return obj(json!({"ok": false, "usd": null, "funded": null, "error": "no key"}));
    }
    hide(&[key]); // after a restart the key comes from the env file, not from finish
    let auth = vec![("Authorization".to_string(), format!("Bearer {key}")), ("Accept".to_string(), "application/json".to_string())];
    let c = client(http.as_ref(), CREDIT_TIMEOUT_SECS);
    let ((info, why), (account, _)) = futures::join!(
        read(&c, eve_spend_sync::OPENROUTER_KEY_URL, &auth),
        read(&c, eve_spend_sync::OPENROUTER_CREDITS_URL, &auth)
    );
    let Some(info) = info else {
        if WARNED.lock().insert(why.clone()) {
            tracing::warn!("onboarding: the OpenRouter key could not be read ({})", why);
        }
        let mut answer = obj(json!({"ok": false, "usd": null, "funded": null, "error": why}));
        if why == REJECTED_WHY {
            answer.insert("rejected".into(), json!(true)); // the key endpoint turned the key down: sign in again
        }
        return answer;
    };
    let mut left: Vec<f64> = Vec::new();
    if let Some(account) = account {
        let bought = account.get("total_credits").and_then(num);
        let used = account.get("total_usage").and_then(num);
        if let (Some(bought), Some(used)) = (bought, used) {
            left.push(bought - used);
        }
    }
    if let Some(limit_left) = info.get("limit_remaining").and_then(num) {
        left.push(limit_left);
    }
    if !left.is_empty() {
        let low = left.iter().copied().fold(left[0], |a, b| if b < a { b } else { a });
        let rounded = round2(low);
        let usd = if rounded > 0.0 { rounded } else { 0.0 };
        return obj(json!({"ok": true, "usd": usd, "funded": usd > 0.0}));
    }
    let funded = if info.get("is_free_tier") == Some(&Value::Bool(true)) { json!(false) } else { Value::Null };
    obj(json!({"ok": true, "usd": null, "funded": funded}))
}

/// `round(x, 2)`: to the nearest hundredth of the exact binary value, ties to even, as Python rounds.
fn round2(x: f64) -> f64 {
    format!("{x:.2}").parse().unwrap_or(x)
}

fn obj(v: Value) -> Obj {
    v.as_object().cloned().unwrap_or_default()
}

/// py: onboarding.py:377 `async def _read(client: httpx.AsyncClient, url: str, headers: Mapping[str, str] ) -> tuple[Optional[dict[str, Any]], str]:`
///
/// ``data`` of a GET, or None and a short reason. Never fails.
pub(crate) async fn read(client: &Client, url: &str, headers: &[(String, String)]) -> (Option<Obj>, String) {
    let response = match client.get(url, headers).await {
        Ok(r) => r,
        Err(e) if e.kind.contains("Timeout") => return (None, "timed out".to_string()),
        // any transport failure reads the same on the page
        Err(_) => return (None, "unreachable".to_string()),
    };
    if response.status != 200 {
        return (None, format!("HTTP {}", response.status));
    }
    match answer(&response).get("data") {
        Some(Value::Object(data)) => (Some(data.clone()), String::new()),
        _ => (None, "unreadable reply".to_string()),
    }
}

/// py: onboarding.py:392 `def _num(value: Any) -> Optional[float]:`
///
/// A finite number, else None. A provider's figure is a plain JSON number; anything else (a string, a bool, an
/// integer too big for a float, Infinity or NaN, which [`loads`] reads as null) reads as unknown, so it never
/// reaches the page's JSON.
pub(crate) fn num(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        _ => None,
    }
}

// ---- Composio Connect -------------------------------------------------------------------------------------

/// py: onboarding.py:407 `class _Client:`
///
/// eve's registration at Composio: a public client, so the id is all there is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct _Client {
    pub client_id: String,
    pub redirect_uris: Vec<String>,
}

/// py: onboarding.py:413 `class ComposioConnectLogin:`
///
/// Composio Connect's OAuth sign-in for eve lite's apps: ``start``, the browser, then ``finish``, which writes
/// ``token_path`` (``composio-connect.json``) for the ConnectBridge.
pub struct ComposioConnectLogin {
    pub token_path: PathBuf,
    pub(crate) client_name: String,
    pub(crate) http: Option<Http>,
    pub(crate) clock: Clock,
    pub(crate) states: _States,
    pub(crate) registered: Mutex<Option<_Client>>,
    /// client ids the token endpoint no longer knows
    pub(crate) gone: Mutex<HashSet<String>>,
    /// one registration at a time (None only in the tests' control, Python's `NoLock`)
    pub(crate) lock: Option<tokio::sync::Mutex<()>>,
    pub(crate) hide: Hide,
}

impl ComposioConnectLogin {
    /// py: onboarding.py:417 `def __init__(self, token_path: Path = CONNECT_TOKEN_FILE, *, client_name: str = "", http: Optional[httpx.AsyncClient] = None, clock: Callable[[], float] = time.time) -> None:`
    ///
    /// `token_path` None is [`CONNECT_TOKEN_FILE`]; `clock` None is `time.time`.
    pub fn new(token_path: Option<&Path>, client_name: &str, http: Option<Http>, clock: Option<Clock>) -> Self {
        let token_path = match token_path {
            Some(p) => expanduser(&p.to_string_lossy()),
            None => expanduser(CONNECT_TOKEN_FILE),
        };
        let clock = clock.unwrap_or_else(|| Arc::new(wall));
        ComposioConnectLogin {
            token_path,
            client_name: clean_label(client_name),
            http,
            states: _States::new(clock.clone()),
            clock,
            registered: Mutex::new(None),
            gone: Mutex::new(HashSet::new()),
            lock: Some(tokio::sync::Mutex::new(())),
            hide: default_hide(),
        }
    }

    /// `_hide` lent (Python's tests patch the module function).
    pub fn with_hide(mut self, hide: Hide) -> Self {
        self.hide = hide;
        self
    }

    /// The registration lock taken away: the tests' control, where two quick clicks both register.
    pub fn without_lock(mut self) -> Self {
        self.lock = None;
        self
    }

    /// The name eve registers under.
    pub fn client_name(&self) -> &str {
        &self.client_name
    }

    /// How many sign-ins wait for their callback (`len(login._states)`).
    pub fn waiting(&self) -> usize {
        self.states.len()
    }

    /// py: onboarding.py:428 `async def start(self, callback_url: str) -> str:`
    ///
    /// The URL to send the browser to. Registers eve first when no known registration lists this callback.
    /// OnboardingError when Composio cannot be reached or refuses the registration.
    pub async fn start(&self, callback_url: &str) -> Result<String, SignInError> {
        let parts = loopback(callback_url)?;
        let redirect = redirect_uri(&parts);
        let client = self.client_for(&redirect_forms(&parts), &redirect).await?;
        let (verifier, challenge) = eve_apps_mcp::pkce();
        let state = self.states.issue(&_Pending {
            verifier,
            redirect_uri: redirect.clone(),
            client_id: client.client_id.clone(),
            redirect_uris: client.redirect_uris.clone(),
        });
        Ok(format!(
            "{CONNECT_AUTHORIZE_URL}?{}",
            urlencode_quote(&[
                ("response_type", "code"),
                ("client_id", &client.client_id),
                ("redirect_uri", &redirect),
                ("scope", CONNECT_SCOPE),
                ("state", &state),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("resource", CONNECT_RESOURCE),
            ])
        ))
    }

    /// py: onboarding.py:441 `async def finish(self, code: str, state: str, *, error: str = "") -> None:`
    ///
    /// Exchange the callback's ``code`` and write the token file (0600). ``error`` is the callback's own, when
    /// it has one: Composio sends ``error`` and ``state``, and no ``iss``, when it refuses. StateError (400) with no
    /// request made for a bad ``state``; OnboardingError for anything else, and then no file is written.
    pub async fn finish(&self, code: impl Into<Value>, state: impl Into<Value>, error: &str) -> Result<(), OnboardingError> {
        let (code, state) = (code.into(), state.into());
        let pending = self.states.take(&state)?;
        if !error.is_empty() {
            tracing::info!(
                "onboarding: Composio sent the sign-in back with an error ({})",
                if OAUTH_WORD.is_match(error) { error } else { "?" }
            );
            let line = if error == "access_denied" { COMPOSIO_CANCELLED } else { COMPOSIO_NOT_FINISHED };
            return Err(OnboardingError::new(line, 400));
        }
        let Some(code) = code.as_str().filter(|_| code_shaped(&code)) else {
            return Err(OnboardingError::new(COMPOSIO_NOT_FINISHED, 400));
        };
        let form: [(&str, String); 6] = [
            ("grant_type", "authorization_code".into()),
            ("code", code.to_string()),
            ("redirect_uri", pending.redirect_uri.clone()),
            ("client_id", pending.client_id.clone()),
            ("code_verifier", pending.verifier.clone()),
            ("resource", CONNECT_RESOURCE.into()),
        ];
        let sent = client(self.http.as_ref(), HTTP_TIMEOUT_SECS)
            .post_form(CONNECT_TOKEN_URL, &form, &[("Accept", "application/json")])
            .await;
        let response = match sent {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("onboarding: the Composio token exchange failed ({})", e.kind);
                return Err(OnboardingError::new(COMPOSIO_UNREACHABLE, 502));
            }
        };
        let answer = answer(&response);
        if response.status != 200 {
            let word = oauth_word(&answer);
            tracing::warn!(
                "onboarding: Composio refused the token exchange (HTTP {}, {})",
                response.status,
                if word.is_empty() { "no error code" } else { &word }
            );
            if word == "invalid_client" || response.status == 401 {
                self.forget(&pending.client_id); // S1: the client is gone, so register again
                return Err(OnboardingError::new(COMPOSIO_CLIENT_GONE, 502));
            }
            if word == "invalid_grant" {
                return Err(OnboardingError::new(COMPOSIO_GRANT_EXPIRED, 502));
            }
            return Err(OnboardingError::new(&COMPOSIO_FAILED.replace("{status}", &response.status.to_string()), 502));
        }
        let access = match answer.get("access_token").and_then(Value::as_str) {
            Some(a) if TOKEN_SHAPE.is_match(a) => a.to_string(),
            _ => {
                tracing::warn!("onboarding: Composio's answer held no usable access token");
                return Err(OnboardingError::new(COMPOSIO_NO_TOKEN, 502));
            }
        };
        let refresh = match answer.get("refresh_token").and_then(Value::as_str) {
            Some(r) if TOKEN_SHAPE.is_match(r) => r.to_string(),
            _ => String::new(),
        };
        (self.hide)(&[&access, &refresh]);
        let scope = match answer.get("scope").and_then(Value::as_str) {
            Some(s) if !eve_envfile::py_strip(s).is_empty() => s.to_string(),
            _ => CONNECT_SCOPE.to_string(),
        };
        let mut data = Obj::new();
        data.insert("version".into(), json!(TOKEN_FILE_VERSION));
        data.insert("issuer".into(), json!(CONNECT_ISSUER));
        data.insert("client_id".into(), json!(pending.client_id));
        data.insert("redirect_uris".into(), json!(pending.redirect_uris));
        data.insert("access_token".into(), json!(access));
        data.insert("refresh_token".into(), json!(refresh));
        data.insert("expires_at".into(), json!(expires_at(&answer, &access, (self.clock)())));
        data.insert("scope".into(), json!(scope));
        if let Err(e) = eve_apps_mcp::write_private_json(&self.token_path, &data) {
            let kind = os_error_name(&e);
            tracing::warn!("onboarding: the Composio sign-in was not saved ({})", kind);
            return Err(OnboardingError::new(&NOT_SAVED.replace("{kind}", kind), 500));
        }
        *self.registered.lock() =
            Some(_Client { client_id: pending.client_id.clone(), redirect_uris: pending.redirect_uris.clone() });
        if refresh.is_empty() {
            tracing::warn!("onboarding: Composio gave no refresh token; the sign-in lapses with its access token");
        }
        tracing::info!("onboarding: signed in to Composio Connect");
        Ok(())
    }

    /// py: onboarding.py:492 `async def _client_for(self, forms: list[str], redirect: str) -> _Client:`
    ///
    /// A registration that lists ``redirect``: the one made in this process, else the token file's, else a
    /// new one. One at a time, so two quick clicks register once.
    pub(crate) async fn client_for(&self, forms: &[String], redirect: &str) -> Result<_Client, OnboardingError> {
        let _guard = match &self.lock {
            Some(lock) => Some(lock.lock().await),
            None => None,
        };
        let known = [self.registered.lock().clone(), self.stored()];
        for known in known.into_iter().flatten() {
            if !self.gone.lock().contains(&known.client_id) && known.redirect_uris.iter().any(|u| u == redirect) {
                *self.registered.lock() = Some(known.clone());
                return Ok(known);
            }
        }
        let made = self.register(forms, redirect).await?;
        *self.registered.lock() = Some(made.clone());
        Ok(made)
    }

    /// py: onboarding.py:503 `def _stored(self) -> Optional[_Client]:`
    pub(crate) fn stored(&self) -> Option<_Client> {
        let data = eve_apps_mcp::read_json(&self.token_path);
        // Python's `==`: 1, 1.0 and True are all the version
        let version_ok = match data.get("version") {
            Some(Value::Number(n)) => n.as_f64() == Some(TOKEN_FILE_VERSION as f64),
            Some(Value::Bool(b)) => *b && TOKEN_FILE_VERSION == 1,
            _ => false,
        };
        let client_id = data.get("client_id").and_then(Value::as_str).filter(|c| ID_SHAPE.is_match(c))?;
        let uris = data.get("redirect_uris").and_then(Value::as_array)?;
        if !version_ok {
            return None;
        }
        Some(_Client {
            client_id: client_id.to_string(),
            redirect_uris: uris.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        })
    }

    /// py: onboarding.py:511 `async def _register(self, forms: list[str], redirect: str) -> _Client:`
    ///
    /// Dynamic client registration with the body S1 verified: a public client with both loopback callbacks.
    pub(crate) async fn register(&self, forms: &[String], redirect: &str) -> Result<_Client, OnboardingError> {
        let body = json!({"client_name": self.client_name, "redirect_uris": forms, "token_endpoint_auth_method": "none",
                          "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"],
                          "scope": CONNECT_SCOPE});
        let sent = client(self.http.as_ref(), HTTP_TIMEOUT_SECS)
            .post_json(CONNECT_REGISTER_URL, &body, &[("Accept", "application/json")])
            .await;
        let response = match sent {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("onboarding: the Composio registration failed ({})", e.kind);
                return Err(OnboardingError::new(COMPOSIO_UNREACHABLE, 502));
            }
        };
        let answer = answer(&response);
        let client_id = answer.get("client_id").and_then(Value::as_str).filter(|c| ID_SHAPE.is_match(c));
        let Some(client_id) = client_id.filter(|_| matches!(response.status, 200 | 201)) else {
            let word = oauth_word(&answer);
            tracing::warn!(
                "onboarding: Composio refused the registration (HTTP {}, {})",
                response.status,
                if word.is_empty() { "no error code" } else { &word }
            );
            return Err(OnboardingError::new(
                &COMPOSIO_NOT_REGISTERED.replace("{status}", &response.status.to_string()),
                502,
            ));
        };
        let registered: Vec<String> = match answer.get("redirect_uris") {
            Some(Value::Array(uris)) => uris.iter().filter_map(Value::as_str).map(str::to_string).collect(),
            _ => forms.to_vec(),
        };
        if !registered.iter().any(|u| u == redirect) {
            tracing::warn!("onboarding: Composio registered eve without its callback");
            return Err(OnboardingError::new(COMPOSIO_NO_CALLBACK, 502));
        }
        tracing::info!("onboarding: registered eve with Composio ({} callbacks)", registered.len());
        Ok(_Client { client_id: client_id.to_string(), redirect_uris: registered })
    }

    /// py: onboarding.py:538 `def _forget(self, client_id: str) -> None:`
    pub(crate) fn forget(&self, client_id: &str) {
        self.gone.lock().insert(client_id.to_string());
        let mut registered = self.registered.lock();
        if registered.as_ref().is_some_and(|r| r.client_id == client_id) {
            *registered = None;
        }
    }
}

/// py: onboarding.py:544 `def _expires_at(answer: Mapping[str, Any], access: str, now: float) -> float:`
///
/// When the access token lapses, in epoch seconds: ``expires_in`` when the server says, else the token's own
/// ``exp`` claim (read, not verified: it only times the refresh), else ``DEFAULT_TOKEN_SECS`` from now. A value
/// that is not a sane lifetime (none, zero, past, absurdly far, not finite) is passed over, so the file always
/// holds a finite number.
pub(crate) fn expires_at(answer: &Obj, access: &str, now: f64) -> f64 {
    let secs = match answer.get("expires_in") {
        Some(Value::String(s)) => eve_apps_mcp::py::parse_float(eve_envfile::py_strip(s)),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None, // a bool is not a number here; null is none
    };
    if let Some(secs) = secs.filter(|s| 0.0 < *s && *s <= MAX_TOKEN_SECS) {
        return now + secs;
    }
    if let Some(exp) = jwt_exp(access).filter(|e| now < *e && *e <= now + MAX_TOKEN_SECS) {
        return exp;
    }
    now + DEFAULT_TOKEN_SECS
}

/// `base64.urlsafe_b64decode(text + padding)`: `-` and `_` read as `+` and `/`, characters outside the alphabet
/// dropped, as Python's non-validating decoder does. None where it raises.
fn urlsafe_b64decode(text: &str) -> Option<Vec<u8>> {
    if !text.is_ascii() {
        return None;
    }
    let kept: String = text
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            other => other,
        })
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/'))
        .collect();
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    engine.decode(kept).ok()
}

/// py: onboarding.py:563 `def _jwt_exp(token: str) -> Optional[float]:`
pub(crate) fn jwt_exp(token: &str) -> Option<f64> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let raw = urlsafe_b64decode(parts[1])?;
    let text = std::str::from_utf8(&raw).ok()?;
    match loads(text.strip_prefix('\u{feff}').unwrap_or(text))? {
        Value::Object(claims) => claims.get("exp").and_then(num),
        _ => None,
    }
}
