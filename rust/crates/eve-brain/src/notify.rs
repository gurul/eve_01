//! A port of `notify.py`: notifications on the owner's phone through ntfy.sh (docs/notifications.md).
//!
//! The owner asked on 2026-10-08 to "turn on notis for eve". With Telegram app-only (EVE_TELEGRAM=app,
//! telegram.MutedBotApi) nothing reaches Telegram, so nothing buzzed the phone when eve spoke unasked: a watch
//! alert, a chief nudge, a task finishing, a Claude relay reply. Apple's push is out (Eve Link is signed with a
//! free personal team), so eve posts to an ntfy.sh topic and the free ntfy app on the phone shows it.
//!
//! * **What notifies.** Exactly what Telegram would have pushed: MutedBotApi hands every sendMessage, sendPhoto and
//!   sendDocument here (never an edit, "typing…", a draft, a reaction or a tap's answer). A reply to an Eve Link
//!   app turn goes to the app (TelegramInlet._to_app) and never reaches MutedBotApi, so it does not notify. A
//!   progress card (one Stop button) is skipped too: it says work started, and the line that says it finished
//!   notifies.
//! * **The topic.** "eve-" and 32 hex of HMAC-SHA256(link token, "eve notify"): the rendezvous's construction
//!   (stick_link.rendezvous_topic) under its own label, so the two topics differ. Nobody without the link token
//!   can name it; Eve Link computes the same in Settings (ios/EveLink/Shared/Rendezvous.swift, NotifyTopic).
//! * **Bursts.** A message waits COALESCE_SECS for others and one notification goes out with the newest message
//!   and "(+N more)"; while messages keep coming, at most one post per MIN_GAP_SECS, the rest folded into the next.
//! * **Never in the way.** `offer` returns at once; the post runs on a worker thread from a task. A failure is one
//!   warning line with the error's type, never the URL (it holds the topic) and never the message.
//! * **Privacy.** ntfy.sh is a public server: anyone who knows the topic can read what is posted on it. The topic
//!   is unguessable without the link token. EVE_NOTIFY_PREVIEW=0 posts "eve has a new message" instead of the
//!   words, so nothing eve says leaves the Mac. EVE_NOTIFY=0 turns notifications off.

use std::sync::atomic::{AtomicI64, Ordering};

use hmac::{Hmac, Mac};
use sha2::Sha256;

#[allow(unused_imports)]
use crate::prelude::*;

use eve_transcripts::py;

// py: notify.py:38 `log`: tracing, under the target `eve_brain::notify`.

/// py: notify.py:40 `NOTIFY_URL`
pub const NOTIFY_URL: &str = "https://ntfy.sh";

/// py: notify.py:41 `NOTIFY_LABEL`
pub const NOTIFY_LABEL: &[u8] = b"eve notify";

/// py: notify.py:42 `CLICK`
/// a tap on the notification opens Eve Link's chat (LinkRoute.chat)
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
/// The methods whose send Telegram would have pushed to the phone, and the field that holds the words.
pub const NOTIFY_METHODS: &[(&str, &str)] = &[("sendMessage", "text"), ("sendPhoto", "caption"), ("sendDocument", "caption")];

/// py: notify.py:54 `_NO_WORDS`
pub(crate) const NO_WORDS: &[(&str, &str)] = &[("sendPhoto", "eve sent a picture"), ("sendDocument", "eve sent a file")];

/// py: notify.py:57 `def notify_topic(token: str) -> str:`
///
/// The topic the phone computes the same way: "eve-" and 32 hex of HMAC-SHA256(token, "eve notify").
pub fn notify_topic(token: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(NOTIFY_LABEL);
    let digest = hex::encode(mac.finalize().into_bytes());
    format!("eve-{}", &digest[..32])
}

/// py: notify.py:62 `def token_path() -> Path:`
///
/// Where the link token lives: next to the Mini App's ledger (miniapp.MiniApp, stick-link.json).
pub fn token_path() -> PathBuf {
    home().join(".config").join("eve-bridge").join("stick-link.json")
}

/// py: notify.py:67 `def read_token(path: Optional[Path] = None) -> str:`
pub fn read_token(path: Option<&Path>) -> String {
    let path = path.map(Path::to_path_buf).unwrap_or_else(token_path);
    eve_stick_link::LinkToken::new(path).current()
}

/// py: notify.py:74 `class NotifyConfig:`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyConfig {
    pub enabled: bool,
    pub base: String,
    pub preview: bool,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self { enabled: true, base: NOTIFY_URL.to_string(), preview: true }
    }
}

/// py: notify.py:80 `def configured(environ: Any = None) -> NotifyConfig:`
///
/// EVE_NOTIFY (on unless "0"), EVE_NOTIFY_PREVIEW (on unless "0"), EVE_NOTIFY_URL (ntfy.sh).
/// Only the app-only door asks (telegram.make_inlet): with Telegram on, Telegram itself notifies.
pub fn configured(environ: Option<&Environ>) -> NotifyConfig {
    let switch = |key: &str| {
        let raw = env_get(environ, key).unwrap_or_default();
        !OFF.contains(&py::strip(&raw).to_lowercase().as_str())
    };
    let url = env_get(environ, "EVE_NOTIFY_URL").filter(|u| !u.is_empty()).unwrap_or_else(|| NOTIFY_URL.to_string());
    let base = py::strip(&url).trim_end_matches('/').to_string();
    NotifyConfig { enabled: switch("EVE_NOTIFY"), base, preview: switch("EVE_NOTIFY_PREVIEW") }
}

// ---- the words: plain text, short ---------------------------------------------------------------------------

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
    let out = eve_telegram_format::visible(&eve_link::link_chat::strip_openui(text));
    let out = MD_LINK.replace_all(&out, "$1");
    let out = MD_HEADING.replace_all(&out, "");
    let out = MD_MARKS.replace_all(&out, "");
    let out = BLANKS.replace_all(&out, "\n");
    let out = SPACES.replace_all(&out, " ");
    let out = py::strip(&out).to_string();
    if py::len(&out) > MAX_CHARS {
        return format!("{}…", py::rstrip(py::head(&out, MAX_CHARS - 1)));
    }
    out
}

/// Why a post raised: the Python exception's class name (what the warning names) and its words (never logged:
/// they can carry the URL, which holds the topic).
#[derive(Debug, Clone, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct PostError {
    pub kind: String,
    pub message: String,
}

impl PostError {
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
        Self { kind: kind.to_string(), message: message.into() }
    }
}

/// `type(e).__name__` of a post's failure.
fn error_type(e: &anyhow::Error) -> String {
    if let Some(p) = e.downcast_ref::<PostError>() {
        return p.kind.clone();
    }
    if let Some(io) = e.downcast_ref::<std::io::Error>() {
        return py::os_error_name(io).to_string();
    }
    if let Some(r) = e.downcast_ref::<reqwest::Error>() {
        return reqwest_kind(r).to_string();
    }
    "Exception".to_string()
}

/// What urllib raises for what reqwest saw.
fn reqwest_kind(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "TimeoutError"
    } else if e.is_builder() {
        "ValueError"
    } else {
        "URLError"
    }
}

/// py: notify.py:115 `def _post(url: str, body: bytes, headers: dict[str, str]) -> int:`
///
/// One blocking POST (Content-Type text/plain, TIMEOUT_SECS): the HTTP status, an error status included.
pub(crate) fn post(url: &str, body: &[u8], headers: &[(String, String)]) -> anyhow::Result<i64> {
    let (url, body, headers) = (url.to_string(), body.to_vec(), headers.to_vec());
    // its own thread and runtime: a blocking call, whoever makes it (post_once runs on a worker thread)
    let worker = std::thread::spawn(move || -> anyhow::Result<i64> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        runtime.block_on(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs_f64(TIMEOUT_SECS))
                .build()
                .map_err(|e| PostError::new(reqwest_kind(&e), e.to_string()))?;
            let mut request = client.post(&url).header("Content-Type", "text/plain; charset=utf-8");
            for (k, v) in &headers {
                request = request.header(k, v);
            }
            let response =
                request.body(body).send().await.map_err(|e| PostError::new(reqwest_kind(&e), e.to_string()))?;
            Ok(response.status().as_u16() as i64)
        })
    });
    worker.join().map_err(|_| anyhow::Error::new(PostError::new("RuntimeError", "the post's thread stopped")))?
}

/// The real `_post`, as a [`PostFn`].
pub fn default_post() -> PostFn {
    Arc::new(|url: &str, body: &[u8], headers: &[(String, String)]| post(url, body, headers))
}

/// py: notify.py:125 `def post_once(token: str, body: str, base: str = NOTIFY_URL, post: Callable[[str, bytes, dict[str, str]], int] = _post) -> bool:`
///
/// One notification on the token's topic. Blocking; False on any failure (logged by type, never raised).
pub fn post_once(token: &str, body: &str, base: &str, post: &PostFn) -> bool {
    if token.is_empty() || base.is_empty() {
        return false;
    }
    let headers = vec![("Title".to_string(), TITLE.to_string()), ("Click".to_string(), CLICK.to_string())];
    let status = match post(&format!("{base}/{}", notify_topic(token)), body.as_bytes(), &headers) {
        Ok(status) => status,
        Err(e) => {
            // the type only: the message can carry the URL, which holds the topic
            tracing::warn!("notify: could not post a notification ({})", error_type(&e));
            return false;
        }
    };
    if status != 200 {
        tracing::warn!("notify: ntfy answered {status}");
        return false;
    }
    true
}

/// What a notifier holds between sends (Python's `_last`, `_count`, `_posted_at`, `_task`).
#[derive(Default)]
pub(crate) struct NotifierState {
    pub(crate) last: String,
    pub(crate) count: i64,
    pub(crate) posted_at: Option<f64>,
    pub(crate) task: Option<tokio::task::JoinHandle<()>>,
    /// a flush task is running (Python's `not self._task.done()`), set and cleared under this lock
    pub(crate) running: bool,
    /// which flush task `running` belongs to
    pub(crate) generation: u64,
}

/// py: notify.py:141 `class Notifier:`
///
/// What MutedBotApi hands its sends to. ``offer`` never blocks and never raises.
///
/// Rust: shared as `Arc<Notifier>`; what Python keeps in attributes between sends is behind one lock.
pub struct Notifier {
    pub(crate) token: StrFn,
    pub base: String,
    pub preview: bool,
    pub(crate) post: PostFn,
    pub(crate) clock: Clock,
    pub coalesce_secs: f64,
    pub gap_secs: f64,
    pub(crate) state: Mutex<NotifierState>,
    /// notifications posted, for the log and tests
    pub(crate) posts: AtomicI64,
}

/// Clears `running` when its flush task ends (returns, fails or is cancelled), unless a newer task took over.
struct Running {
    notifier: Arc<Notifier>,
    generation: u64,
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut state = self.notifier.state.lock();
        if state.generation == self.generation {
            state.running = false;
        }
    }
}

impl Notifier {
    /// py: notify.py:144 `def __init__(self, token: Callable[[], str] = read_token, *, base: str = NOTIFY_URL, preview: bool = True, post: Callable[[str, bytes, dict[str, str]], int] = _post, clock: Callable[[], float] = time.monotonic, coalesce_secs: float = COALESCE_SECS, gap_secs: float = MIN_GAP_SECS) -> None:`
    pub fn new(token: StrFn, base: &str, preview: bool, post: PostFn, clock: Clock, coalesce_secs: f64, gap_secs: f64) -> Arc<Self> {
        Arc::new(Self {
            token,
            base: base.to_string(),
            preview,
            post,
            clock,
            coalesce_secs,
            gap_secs,
            state: Mutex::new(NotifierState::default()),
            posts: AtomicI64::new(0),
        })
    }

    /// `Notifier(token, post=post, ...)` with the other defaults (ntfy.sh, previews, `time.monotonic`, 3 s, 20 s).
    pub fn with_defaults(token: StrFn, post: PostFn) -> Arc<Self> {
        Self::new(token, NOTIFY_URL, true, post, Arc::new(monotonic), COALESCE_SECS, MIN_GAP_SECS)
    }

    /// `notifier.posts`: notifications posted.
    pub fn posts(&self) -> i64 {
        self.posts.load(Ordering::SeqCst)
    }

    /// py: notify.py:156 `def offer_send(self, method: str, data: dict[str, Any]) -> None:`
    ///
    /// One Bot API send MutedBotApi swallowed. Only the sends Telegram would have pushed count.
    pub fn offer_send(self: &Arc<Self>, method: &str, data: &Obj) {
        let Some((_, field)) = NOTIFY_METHODS.iter().find(|(m, _)| *m == method) else {
            return;
        };
        if is_progress_card(data) {
            return;
        }
        let words = match data.get(*field) {
            Some(Value::String(words)) if !py::strip(words).is_empty() => words.clone(),
            _ => NO_WORDS.iter().find(|(m, _)| *m == method).map(|(_, w)| *w).unwrap_or(GENERIC).to_string(),
        };
        self.offer(&words);
    }

    /// py: notify.py:170 `def offer(self, text: str) -> None:`
    pub fn offer(self: &Arc<Self>, text: &str) {
        let line = if self.preview { plain_text(text) } else { GENERIC.to_string() };
        if line.is_empty() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // no running loop: nothing to post from (never the case in the inlet)
            tracing::debug!("notify: no event loop; a notification was skipped");
            return;
        };
        let generation = {
            let mut state = self.state.lock();
            state.last = line;
            state.count += 1;
            if state.running {
                return;
            }
            state.running = true;
            state.generation += 1;
            state.generation
        };
        // spawned with the lock released: a task dropped at once (a runtime shutting down) takes it in its guard
        let guard = Running { notifier: self.clone(), generation };
        let me = self.clone();
        let task = runtime.spawn(async move {
            let _guard = guard;
            me.flush_soon().await;
        });
        let mut state = self.state.lock();
        if state.generation == generation {
            state.task = Some(task);
        }
    }

    /// py: notify.py:184 `async def _flush_soon(self) -> None:`
    pub(crate) async fn flush_soon(self: &Arc<Self>) {
        loop {
            let wait = {
                let mut state = self.state.lock();
                if state.count == 0 {
                    // done, decided under the lock an offer takes, so no message is left behind
                    state.running = false;
                    return;
                }
                let mut wait = self.coalesce_secs;
                if let Some(at) = state.posted_at {
                    wait = wait.max(at + self.gap_secs - (self.clock)());
                }
                wait
            };
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait.max(0.0))).await;
            let (last, count) = {
                let mut state = self.state.lock();
                (std::mem::take(&mut state.last), std::mem::replace(&mut state.count, 0))
            };
            let body = if !self.preview {
                if count == 1 {
                    GENERIC.to_string()
                } else {
                    format!("eve has {count} new messages")
                }
            } else if count > 1 {
                format!("{last} (+{} more)", count - 1)
            } else {
                last
            };
            let token = (self.token)();
            self.state.lock().posted_at = Some((self.clock)());
            if token.is_empty() {
                tracing::debug!("notify: no link token yet (Eve Link not paired); not posted");
                continue;
            }
            let (base, post) = (self.base.clone(), self.post.clone());
            match tokio::task::spawn_blocking(move || post_once(&token, &body, &base, &post)).await {
                Ok(true) => {
                    self.posts.fetch_add(1, Ordering::SeqCst);
                }
                Ok(false) => {}
                Err(e) if e.is_cancelled() => return,
                Err(_) => {
                    tracing::warn!("notify: the notifier stopped a round (RuntimeError)");
                    self.state.lock().running = false;
                    return;
                }
            }
        }
    }

    /// py: notify.py:209 `async def close(self) -> None:`
    pub async fn close(self: &Arc<Self>) {
        let task = self.state.lock().task.take();
        if let Some(task) = task {
            if !task.is_finished() {
                task.abort();
                let _ = task.await;
            }
        }
    }
}

/// py: notify.py:219 `def _is_progress_card(data: dict[str, Any]) -> bool:`
///
/// A progress message (TelegramInlet._open_progress): one inline button, Stop. Work starting is not news.
pub(crate) fn is_progress_card(data: &Obj) -> bool {
    let rows = match data.get("reply_markup") {
        Some(Value::Object(markup)) => markup.get("inline_keyboard"),
        _ => None,
    };
    let Some(Value::Array(rows)) = rows else {
        return false;
    };
    let [Value::Array(row)] = rows.as_slice() else {
        return false;
    };
    let [button] = row.as_slice() else {
        return false;
    };
    match button {
        Value::Object(b) => {
            b.get("text").and_then(Value::as_str) == Some(crate::telegram::STOP_BUTTON) && b.contains_key("callback_data")
        }
        _ => false,
    }
}

/// py: notify.py:231 `def make_notifier(environ: Any = None) -> Optional[Notifier]:`
///
/// The app-only door's notifier, or None when EVE_NOTIFY=0. The link token is read at each post, so a
/// phone paired later (or a new token from /stick new) is picked up without a restart.
pub fn make_notifier(environ: Option<&Environ>) -> Option<Arc<Notifier>> {
    let cfg = configured(environ);
    if !cfg.enabled || cfg.base.is_empty() {
        tracing::info!("notify: off (EVE_NOTIFY=0)");
        return None;
    }
    tracing::info!(
        "notify: on through ntfy ({}); the topic is Eve Link's (eve-bridge notify topic)",
        if cfg.preview { "previews" } else { "no previews" }
    );
    Some(Notifier::new(
        Arc::new(|| read_token(None)),
        &cfg.base,
        cfg.preview,
        default_post(),
        Arc::new(monotonic),
        COALESCE_SECS,
        MIN_GAP_SECS,
    ))
}

/// py: notify.py:243 `def setup_lines(token: str, base: str = NOTIFY_URL) -> list[str]:`
///
/// What ``eve-bridge notify topic`` prints.
pub fn setup_lines(token: &str, base: &str) -> Vec<String> {
    let topic = notify_topic(token);
    vec![
        topic.clone(),
        format!("{base}/{topic}"),
        "On the iPhone: install ntfy from the App Store, tap +, paste the topic, keep the default server, Subscribe. (Or Eve Link > Settings > Notifications > Copy.)".to_string(),
    ]
}

/// `post(url, body, headers) -> HTTP status` (`_post`, blocking; a test's fake). Any status but 200 is a failure;
/// an error is logged by its type ([`PostError`]'s `kind`), never its words.
pub type PostFn = Arc<dyn Fn(&str, &[u8], &[(String, String)]) -> anyhow::Result<i64> + Send + Sync>;
