//! A port of `notify.py`, the whole module.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;

// py: notify.py:38 `log` (Logger): ported by this file's unit.

/// py: notify.py:40 `NOTIFY_URL`
pub const NOTIFY_URL: &str = "https://ntfy.sh";

/// py: notify.py:41 `NOTIFY_LABEL`
pub const NOTIFY_LABEL: &[u8] = b"eve notify";

/// py: notify.py:42 `CLICK`
pub const CLICK: &str = "evelink://chat";

/// py: notify.py:43 `TITLE`
pub const TITLE: &str = "eve";

/// py: notify.py:44 `GENERIC`
pub const GENERIC: &str = "eve has a new message";

/// py: notify.py:45 `TEST_LINE`
pub const TEST_LINE: &str = "Notifications from eve work.";

/// py: notify.py:46 `COALESCE_SECS`
pub const COALESCE_SECS: f64 = 3.0;

/// py: notify.py:47 `MIN_GAP_SECS`
pub const MIN_GAP_SECS: f64 = 20.0;

/// py: notify.py:48 `MAX_CHARS`
pub const MAX_CHARS: usize = 300;

/// py: notify.py:49 `TIMEOUT_SECS`
pub const TIMEOUT_SECS: f64 = 10.0;

/// py: notify.py:50 `APP_STORE`
pub const APP_STORE: &str = "https://apps.apple.com/app/ntfy/id1625396347";

/// py: notify.py:51 `_OFF`
pub(crate) const OFF: &[&str] = &["0", "false", "no", "off"];

/// py: notify.py:53 `NOTIFY_METHODS`
pub const NOTIFY_METHODS: &[(&str, &str)] = &[("sendMessage", "text"), ("sendPhoto", "caption"), ("sendDocument", "caption")];

/// py: notify.py:54 `_NO_WORDS`
pub(crate) const NO_WORDS: &[(&str, &str)] = &[("sendPhoto", "eve sent a picture"), ("sendDocument", "eve sent a file")];

/// py: notify.py:57 `def notify_topic(token: str) -> str:`
///
/// The topic the phone computes the same way: "eve-" and 32 hex of HMAC-SHA256(token, "eve notify").
pub fn notify_topic(token: &str) -> String {
    todo!("notify.py:57 notify_topic")
}

/// py: notify.py:62 `def token_path() -> Path:`
///
/// Where the link token lives: next to the Mini App's ledger (miniapp.MiniApp, stick-link.json).
pub fn token_path() -> PathBuf {
    todo!("notify.py:62 token_path")
}

/// py: notify.py:67 `def read_token(path: Optional[Path] = None) -> str:`
pub fn read_token(path: Option<&Path>) -> String {
    todo!("notify.py:67 read_token")
}

/// py: notify.py:74 `class NotifyConfig:`
#[derive(Debug, Clone)]
pub struct NotifyConfig {
    pub enabled: bool,
    pub base: String,
    pub preview: bool,
}

/// py: notify.py:80 `def configured(environ: Any = None) -> NotifyConfig:`
///
/// EVE_NOTIFY (on unless "0"), EVE_NOTIFY_PREVIEW (on unless "0"), EVE_NOTIFY_URL (ntfy.sh).
/// Only the app-only door asks (telegram.make_inlet): with Telegram on, Telegram itself notifies.
pub fn configured(environ: Option<&Environ>) -> NotifyConfig {
    todo!("notify.py:80 configured")
}

/// py: notify.py:92 `_MD_LINK`
/// Python flags 32.
pub(crate) static MD_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\[([^\]]+)\]\([^)]+\)"#).expect("MD_LINK"));

/// py: notify.py:93 `_MD_MARKS`
/// Python flags 32.
pub(crate) static MD_MARKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(\*\*|__|~~|`+)"#).expect("MD_MARKS"));

/// py: notify.py:94 `_MD_HEADING`
/// Python flags 40.
pub(crate) static MD_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)^\s{0,3}#{1,6}\s+"#).expect("MD_HEADING"));

/// py: notify.py:95 `_BLANKS`
/// Python flags 32.
pub(crate) static BLANKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\n\s*\n+"#).expect("BLANKS"));

/// py: notify.py:96 `_SPACES`
/// Python flags 32.
pub(crate) static SPACES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[ \t]+"#).expect("SPACES"));

/// py: notify.py:99 `def plain_text(text: str) -> str:`
///
/// A message as a notification shows it: OpenUI programs swapped for one line (link_chat.strip_openui),
/// Telegram's HTML and leftover markdown gone, blank runs folded, cut to MAX_CHARS.
pub fn plain_text(text: &str) -> String {
    todo!("notify.py:99 plain_text")
}

/// py: notify.py:115 `def _post(url: str, body: bytes, headers: dict[str, str]) -> int:`
pub(crate) fn post(url: &str, body: &[u8], headers: &HashMap<String, String>) -> i64 {
    todo!("notify.py:115 _post")
}

/// py: notify.py:125 `def post_once(token: str, body: str, base: str = NOTIFY_URL, post: Callable[[str, bytes, dict[str, str]], int] = _post) -> bool:`
///
/// One notification on the token's topic. Blocking; False on any failure (logged by type, never raised).
pub fn post_once(token: &str, body: &str, base: &str, post: &PostFn) -> bool {
    todo!("notify.py:125 post_once")
}

/// py: notify.py:141 `class Notifier:`
///
/// What MutedBotApi hands its sends to. ``offer`` never blocks and never raises.
pub struct Notifier {
    pub(crate) token: StrFn,
    pub base: String,
    pub preview: bool,
    pub(crate) post: PostFn,
    pub(crate) clock: Clock,
    pub coalesce_secs: f64,
    pub gap_secs: f64,
    pub(crate) last: String,
    pub(crate) count: i64,
    pub(crate) posted_at: Option<f64>,
    pub(crate) task: Option<tokio::task::JoinHandle<()>>,
    /// notifications posted, for the log and tests
    pub posts: i64,
}

impl Notifier {
    /// py: notify.py:144 `def __init__(self, token: Callable[[], str] = read_token, *, base: str = NOTIFY_URL, preview: bool = True, post: Callable[[str, bytes, dict[str, str]], int] = _post, clock: Callable[[], float] = time.monotonic, coalesce_secs: float = COALESCE_SECS, gap_secs: float = MIN_GAP_SECS) -> None:`
    pub fn new(token: StrFn, base: &str, preview: bool, post: PostFn, clock: Clock, coalesce_secs: f64, gap_secs: f64) -> Arc<Self> {
        todo!("notify.py:144 Notifier.__init__")
    }

    /// py: notify.py:156 `def offer_send(self, method: str, data: dict[str, Any]) -> None:`
    ///
    /// One Bot API send MutedBotApi swallowed. Only the sends Telegram would have pushed count.
    pub fn offer_send(self: &Arc<Self>, method: &str, data: &Obj) {
        todo!("notify.py:156 Notifier.offer_send")
    }

    /// py: notify.py:170 `def offer(self, text: str) -> None:`
    pub fn offer(self: &Arc<Self>, text: &str) {
        todo!("notify.py:170 Notifier.offer")
    }

    /// py: notify.py:184 `async def _flush_soon(self) -> None:`
    pub(crate) async fn flush_soon(self: &Arc<Self>) {
        todo!("notify.py:184 Notifier._flush_soon")
    }

    /// py: notify.py:209 `async def close(self) -> None:`
    pub async fn close(self: &Arc<Self>) {
        todo!("notify.py:209 Notifier.close")
    }
}

/// py: notify.py:219 `def _is_progress_card(data: dict[str, Any]) -> bool:`
///
/// A progress message (TelegramInlet._open_progress): one inline button, Stop. Work starting is not news.
pub(crate) fn is_progress_card(data: &Obj) -> bool {
    todo!("notify.py:219 _is_progress_card")
}

/// py: notify.py:231 `def make_notifier(environ: Any = None) -> Optional[Notifier]:`
///
/// The app-only door's notifier, or None when EVE_NOTIFY=0. The link token is read at each post, so a
/// phone paired later (or a new token from /stick new) is picked up without a restart.
pub fn make_notifier(environ: Option<&Environ>) -> Option<Notifier> {
    todo!("notify.py:231 make_notifier")
}

/// py: notify.py:243 `def setup_lines(token: str, base: str = NOTIFY_URL) -> list[str]:`
///
/// What ``eve-bridge notify topic`` prints.
pub fn setup_lines(token: &str, base: &str) -> Vec<String> {
    todo!("notify.py:243 setup_lines")
}

/// `post(url, body, headers) -> HTTP status` (`_post`, blocking; a test's fake). 0 or an error status is a failure.
pub type PostFn = Arc<dyn Fn(&str, &[u8], &[(String, String)]) -> anyhow::Result<i64> + Send + Sync>;
