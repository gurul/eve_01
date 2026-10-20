//! A port of `onboarding.py`, the whole module.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;

// py: onboarding.py:57 `log` (Logger): ported by this file's unit.

/// py: onboarding.py:59 `STATE_TTL_SECS`
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

// py: onboarding.py:76 `CONNECT_TOKEN_FILE` (PosixPath): ported by this file's unit.

/// py: onboarding.py:77 `TOKEN_FILE_VERSION`
pub const TOKEN_FILE_VERSION: i64 = 1;

/// py: onboarding.py:78 `DEFAULT_TOKEN_SECS`
/// neither expires_in nor the token says: refresh early rather than late
pub const DEFAULT_TOKEN_SECS: f64 = 300.0;

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

/// py: onboarding.py:102 `_KEY_SHAPE`
/// one visible-ASCII word: no space, no line break
/// Python flags 32.
pub(crate) static KEY_SHAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[\x21-\x7e]{8,512}"#).expect("KEY_SHAPE"));

/// py: onboarding.py:103 `_TOKEN_SHAPE`
/// an access token may be a long JWT
/// Python flags 32.
pub(crate) static TOKEN_SHAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[\x21-\x7e]{8,16384}"#).expect("TOKEN_SHAPE"));

/// py: onboarding.py:104 `_ID_SHAPE`
/// Python flags 32.
pub(crate) static ID_SHAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[\x21-\x7e]{1,256}"#).expect("ID_SHAPE"));

/// py: onboarding.py:105 `_OAUTH_WORD`
/// an OAuth error code, safe to log
/// Python flags 32.
pub(crate) static OAUTH_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[a-z_]{1,40}"#).expect("OAUTH_WORD"));

/// py: onboarding.py:106 `_MAX_TOKEN_SECS`
/// beyond this an expiry is nonsense, not a lifetime
pub(crate) const MAX_TOKEN_SECS: f64 = 315360000.0;

// py: onboarding.py:107 `_warned` (set): ported by this file's unit.

/// py: onboarding.py:110 `class OnboardingError(Exception):`
///
/// A sign-in that did not finish. ``str(e)`` is one plain line for the setup page; ``status`` is the HTTP
/// status the callback answers with (400: the browser's side, 502: the provider's, 500: this Mac's).
/// (Python bases: Exception.)
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct OnboardingError {
    /// `str(error)`.
    pub message: String,
    pub status: i64,
}

impl OnboardingError {
    /// py: onboarding.py:114 `def __init__(self, line: str, status: int = 502) -> None:`
    pub fn new(line: &str, status: i64) -> Self {
        todo!("onboarding.py:114 OnboardingError.__init__")
    }
}

/// py: onboarding.py:119 `class StateError(OnboardingError):`
///
/// The callback's ``state`` is missing, unknown, used or expired. Raised before any request leaves the Mac.
/// (Python bases: OnboardingError.)
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct StateError {
    /// `str(error)`.
    pub message: String,
}

impl StateError {
    /// py: onboarding.py:122 `def __init__(self, line: str) -> None:`
    pub fn new(line: &str) -> Self {
        todo!("onboarding.py:122 StateError.__init__")
    }
}

/// py: onboarding.py:126 `def mac_label() -> str:`
///
/// "eve lite on <this Mac>", the label OpenRouter gives the key and the name Composio gives the client, so the
/// person can tell them apart in their accounts. Read from the host name: the lite firewall lets almost no
/// program start, so ``scutil --get ComputerName`` is not an option.
pub fn mac_label() -> String {
    todo!("onboarding.py:126 mac_label")
}

/// py: onboarding.py:139 `class _Pending:`
///
/// What one waiting sign-in's callback needs.
#[derive(Debug, Clone)]
pub struct _Pending {
    pub verifier: String,
    pub redirect_uri: String,
    pub client_id: String,
    pub redirect_uris: Vec<String>,
}

/// py: onboarding.py:147 `class _States:`
///
/// The sign-ins waiting for their callback, by ``state``. Each is good once, for ``STATE_TTL_SECS``.
pub struct _States {
    pub(crate) clock: Clock,
    pub(crate) waiting: Mutex<HashMap<String, (f64, _Pending)>>,
}

impl _States {
    /// py: onboarding.py:150 `def __init__(self, clock: Callable[[], float]) -> None:`
    pub fn new(clock: Clock) -> Self {
        todo!("onboarding.py:150 _States.__init__")
    }

    /// py: onboarding.py:154 `def __len__(self) -> int:`
    pub fn len(&self) -> i64 {
        todo!("onboarding.py:154 _States.__len__")
    }

    /// py: onboarding.py:157 `def issue(self, pending: _Pending) -> str:`
    pub fn issue(&self, pending: &_Pending) -> String {
        todo!("onboarding.py:157 _States.issue")
    }

    /// py: onboarding.py:166 `def take(self, state: Any) -> _Pending:`
    ///
    /// The sign-in ``state`` belongs to, which is then used up. StateError when there is none.
    pub fn take(&self, state: &Value) -> _Pending {
        todo!("onboarding.py:166 _States.take")
    }
}

/// py: onboarding.py:180 `def _loopback(url: str) -> urllib.parse.SplitResult:`
///
/// ``url`` checked as eve's own callback on this Mac: plain http on a loopback host and port, no credentials,
/// query or fragment (both providers append their own query). ValueError otherwise: the onboarding routes live
/// only on the local listener, so anything else is a wiring mistake.
pub(crate) fn loopback(url: &str) -> Option<url::Url> {
    todo!("onboarding.py:180 _loopback")
}

/// py: onboarding.py:195 `def _redirect_forms(parts: urllib.parse.SplitResult) -> list[str]:`
///
/// The two loopback forms of the callback that S1 registered and Composio accepted: localhost, then 127.0.0.1.
pub(crate) fn redirect_forms(parts: &url::Url) -> Vec<String> {
    todo!("onboarding.py:195 _redirect_forms")
}

/// py: onboarding.py:200 `def _redirect_uri(parts: urllib.parse.SplitResult) -> str:`
///
/// The callback Composio is told: the page's own form, except that an IPv6 literal comes back on localhost,
/// a form S1 tested. (OpenRouter is always told the localhost form: localhost is what it documents.)
pub(crate) fn redirect_uri(parts: &url::Url) -> String {
    todo!("onboarding.py:200 _redirect_uri")
}

/// py: onboarding.py:207 `def _clean_label(label: str) -> str:`
pub(crate) fn clean_label(label: &str) -> String {
    todo!("onboarding.py:207 _clean_label")
}

/// py: onboarding.py:213 `async def _client(http: Optional[httpx.AsyncClient], timeout: float = HTTP_TIMEOUT_SECS ) -> AsyncIterator[httpx.AsyncClient]:`
///
/// The client to call with: the one handed in (tests pass one on a mock transport), else a fresh one.
pub(crate) async fn client(http: Option<&Http>, timeout: f64) -> Http {
    todo!("onboarding.py:213 _client")
}

/// py: onboarding.py:223 `def _answer(response: httpx.Response) -> dict[str, Any]:`
pub(crate) fn answer(response: &HttpResponse) -> Obj {
    todo!("onboarding.py:223 _answer")
}

/// py: onboarding.py:231 `def _hide(*values: str) -> None:`
///
/// Keep these out of every log record in the process from now on, whoever writes it. ``hide_token`` wraps the
/// record factory; it is idempotent and ignores "".
/// (Python also takes *values.)
pub(crate) fn hide() {
    todo!("onboarding.py:231 _hide")
}

/// py: onboarding.py:241 `def _code_shaped(code: Any) -> bool:`
///
/// An authorization code worth sending on: a non-empty string of sane length. Codes are opaque and short; a
/// huge or empty one is refused here rather than posted to the provider.
pub(crate) fn code_shaped(code: &Value) -> bool {
    todo!("onboarding.py:241 _code_shaped")
}

/// py: onboarding.py:247 `def _oauth_word(answer: Mapping[str, Any]) -> str:`
///
/// The answer's OAuth ``error`` code when it looks like one (safe to log and to branch on), else "".
pub(crate) fn oauth_word(answer: &Obj) -> String {
    todo!("onboarding.py:247 _oauth_word")
}

/// py: onboarding.py:255 `class OpenRouterLogin:`
///
/// OpenRouter's PKCE sign-in, which mints eve's one model key: ``start``, the browser, then ``finish``.
pub struct OpenRouterLogin {
    pub(crate) http: Option<Http>,
    pub(crate) states: _States,
}

impl OpenRouterLogin {
    /// py: onboarding.py:258 `def __init__(self, *, http: Optional[httpx.AsyncClient] = None, clock: Callable[[], float] = time.time) -> None:`
    pub fn new(http: Option<Http>, clock: Clock) -> Self {
        todo!("onboarding.py:258 OpenRouterLogin.__init__")
    }

    /// py: onboarding.py:263 `def start(self, callback_url: str, label: str = "") -> str:`
    ///
    /// The URL to send the browser to. ``callback_url`` is eve's own loopback callback, which OpenRouter is
    /// given in its ``localhost`` form, the one it documents for any port; ``label`` names the key in the person's
    /// OpenRouter account (default: ``mac_label()``).
    pub fn start(&self, callback_url: &str, label: &str) -> String {
        todo!("onboarding.py:263 OpenRouterLogin.start")
    }

    /// py: onboarding.py:274 `async def finish(self, code: str, state: str, *, error: str = "") -> str:`
    ///
    /// The key, for the callback's ``code`` and ``state``. ``error`` is the callback's own, when it has one.
    /// StateError (400) with no request made for a bad ``state``; OnboardingError for anything else.
    pub async fn finish(&self, code: &str, state: &str, error: &str) -> String {
        todo!("onboarding.py:274 OpenRouterLogin.finish")
    }
}

/// py: onboarding.py:305 `def _openrouter_reason(answer: Mapping[str, Any]) -> str:`
///
/// OpenRouter's error message ("Authorization code expired"), to tell its two 403s apart. Never shown or
/// logged.
pub(crate) fn openrouter_reason(answer: &Obj) -> String {
    todo!("onboarding.py:305 _openrouter_reason")
}

/// py: onboarding.py:313 `def save_openrouter_key(key: str, *, path: Optional[Path] = None, environ: MutableMapping[str, str] = os.environ) -> None:`
///
/// Keep the key OpenRouter minted. It is hidden from every log line, written to the env file the daemon reads
/// (``envfile.set_key``: 0600, atomic, the file's other lines kept) and set in ``environ``. The caller restarts the
/// daemon. ValueError for a value that is not a key; OnboardingError (500) when the file cannot be written.
pub fn save_openrouter_key(key: &str, path: Option<&Path>, environ: &Environ) {
    todo!("onboarding.py:313 save_openrouter_key")
}

/// py: onboarding.py:331 `REJECTED_WHY`
/// /api/v1/key answering 401: the key was deleted or revoked, not a passing error
pub const REJECTED_WHY: &str = "HTTP 401";

/// py: onboarding.py:334 `async def credit(key: str, *, http: Optional[httpx.AsyncClient] = None) -> dict[str, Any]:`
///
/// What the key can spend, for the setup page: ``{"ok", "usd", "funded"}``, plus ``"error"`` (a short reason:
/// "HTTP 401", "timed out") when the key itself could not be read, and ``"rejected": True`` when that reason is a
/// 401 (the key no longer works and a new sign-in is the only fix, not a passing network error). Never raises, and
/// never holds the key.
///
/// ``usd`` is the smaller of the account's balance (``/api/v1/credits``: bought less used) and the key's own
/// remaining limit (``limit_remaining``, null for a key with no limit, which is how OAuth mints them). It is None
/// when neither can be read: OpenRouter documents ``/credits`` as needing a management key, which an OAuth key is
/// not. ``funded`` is whether a paid call can go through: usd above zero; without a figure, False only when the
/// account has never bought credits (``is_free_tier``), else None (not known).
pub async fn credit(key: &str, http: Option<Http>) -> Obj {
    todo!("onboarding.py:334 credit")
}

/// py: onboarding.py:377 `async def _read(client: httpx.AsyncClient, url: str, headers: Mapping[str, str] ) -> tuple[Optional[dict[str, Any]], str]:`
///
/// ``data`` of a GET, or None and a short reason. Never raises.
pub(crate) async fn read(client: &Http, url: &str, headers: &HashMap<String, String>) -> (Option<Obj>, String) {
    todo!("onboarding.py:377 _read")
}

/// py: onboarding.py:392 `def _num(value: Any) -> Optional[float]:`
///
/// A finite number, else None. A provider's figure is a plain JSON number; anything else (a string, a bool, an
/// integer too big for a float, Infinity or NaN) reads as unknown, so it never reaches the page's JSON.
pub(crate) fn num(value: &Value) -> Option<f64> {
    todo!("onboarding.py:392 _num")
}

/// py: onboarding.py:407 `class _Client:`
///
/// eve's registration at Composio: a public client, so the id is all there is.
#[derive(Debug, Clone)]
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
    pub(crate) gone: Mutex<Vec<String>>,
    pub(crate) lock: tokio::sync::Mutex<()>,
}

impl ComposioConnectLogin {
    /// py: onboarding.py:417 `def __init__(self, token_path: Path = CONNECT_TOKEN_FILE, *, client_name: str = "", http: Optional[httpx.AsyncClient] = None, clock: Callable[[], float] = time.time) -> None:`
    pub fn new(token_path: &Path, client_name: &str, http: Option<Http>, clock: Clock) -> Self {
        todo!("onboarding.py:417 ComposioConnectLogin.__init__")
    }

    /// py: onboarding.py:428 `async def start(self, callback_url: str) -> str:`
    ///
    /// The URL to send the browser to. Registers eve first when no known registration lists this callback.
    /// OnboardingError when Composio cannot be reached or refuses the registration.
    pub async fn start(&self, callback_url: &str) -> String {
        todo!("onboarding.py:428 ComposioConnectLogin.start")
    }

    /// py: onboarding.py:441 `async def finish(self, code: str, state: str, *, error: str = "") -> None:`
    ///
    /// Exchange the callback's ``code`` and write the token file (0600). ``error`` is the callback's own, when
    /// it has one: Composio sends ``error`` and ``state``, and no ``iss``, when it refuses. StateError (400) with no
    /// request made for a bad ``state``; OnboardingError for anything else, and then no file is written.
    pub async fn finish(&self, code: &str, state: &str, error: &str) {
        todo!("onboarding.py:441 ComposioConnectLogin.finish")
    }

    /// py: onboarding.py:492 `async def _client_for(self, forms: list[str], redirect: str) -> _Client:`
    ///
    /// A registration that lists ``redirect``: the one made in this process, else the token file's, else a
    /// new one. One at a time, so two quick clicks register once.
    pub(crate) async fn client_for(&self, forms: &[String], redirect: &str) -> _Client {
        todo!("onboarding.py:492 ComposioConnectLogin._client_for")
    }

    /// py: onboarding.py:503 `def _stored(self) -> Optional[_Client]:`
    pub(crate) fn stored(&self) -> Option<_Client> {
        todo!("onboarding.py:503 ComposioConnectLogin._stored")
    }

    /// py: onboarding.py:511 `async def _register(self, forms: list[str], redirect: str) -> _Client:`
    ///
    /// Dynamic client registration with the body S1 verified: a public client with both loopback callbacks.
    pub(crate) async fn register(&self, forms: &[String], redirect: &str) -> _Client {
        todo!("onboarding.py:511 ComposioConnectLogin._register")
    }

    /// py: onboarding.py:538 `def _forget(self, client_id: str) -> None:`
    pub(crate) fn forget(&self, client_id: &str) {
        todo!("onboarding.py:538 ComposioConnectLogin._forget")
    }
}

/// py: onboarding.py:544 `def _expires_at(answer: Mapping[str, Any], access: str, now: float) -> float:`
///
/// When the access token lapses, in epoch seconds: ``expires_in`` when the server says, else the token's own
/// ``exp`` claim (read, not verified: it only times the refresh), else ``DEFAULT_TOKEN_SECS`` from now. A value
/// that is not a sane lifetime (none, zero, past, absurdly far, not finite) is passed over, so the file always
/// holds a finite number.
pub(crate) fn expires_at(answer: &Obj, access: &str, now: f64) -> f64 {
    todo!("onboarding.py:544 _expires_at")
}

/// py: onboarding.py:563 `def _jwt_exp(token: str) -> Optional[float]:`
pub(crate) fn jwt_exp(token: &str) -> Option<f64> {
    todo!("onboarding.py:563 _jwt_exp")
}
