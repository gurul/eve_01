//! A port of `miniapp.py`, the whole module.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;

// py: miniapp.py:75 `log` (Logger): ported by this file's unit.

/// py: miniapp.py:77 `DEFAULT_MODEL`
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

/// py: miniapp.py:78 `EFFORTS`
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// py: miniapp.py:79 `DEFAULT_MAKE_EFFORT`
/// building an app is real work: think it through
pub const DEFAULT_MAKE_EFFORT: &str = "high";

/// py: miniapp.py:84 `PRICES`
/// The Python value as JSON (tuples are arrays).
pub static PRICES: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(r#"{"claude-opus-5-5": {"in": 4.0, "out": 20.0, "cache_read": 0.2, "cache_write": 5.0, "cache_write_1h": 8.0}}"#).expect("PRICES"));

/// py: miniapp.py:89 `_WORST`
/// The Python value as JSON (tuples are arrays).
pub(crate) static WORST: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(r#"{"in": 10.0, "out": 50.0, "cache_read": 1.0, "cache_write": 12.5, "cache_write_1h": 20.0}"#).expect("WORST"));

/// py: miniapp.py:90 `INIT_DATA_MAX_AGE_SECS`
/// a Mini App left open all day still works; an old leaked string does not
pub const INIT_DATA_MAX_AGE_SECS: f64 = 86400.0;

/// py: miniapp.py:91 `MAX_BODY_BYTES`
pub const MAX_BODY_BYTES: usize = 2000000;

/// py: miniapp.py:92 `APP_ROUTE_RE`
/// Python flags 32.
pub static APP_ROUTE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^/apps/([a-z0-9][a-z0-9-]{0,47})/(index\.html)?(?:\n?\z)"#).expect("APP_ROUTE_RE"));

/// py: miniapp.py:93 `API_APP_RE`
/// Python flags 32.
pub static API_APP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^/api/apps/([a-z0-9][a-z0-9-]{0,47})/(load|save|report|delete|rename|revert|restore|history)(?:\n?\z)"#).expect("API_APP_RE"));

/// py: miniapp.py:95 `APP_TOKEN_SECS`
/// an app left open all day still saves; as long as initData lives
pub const APP_TOKEN_SECS: f64 = 86400.0;

/// py: miniapp.py:96 `CLOSE_WAIT_SECS`
pub const CLOSE_WAIT_SECS: f64 = 2.0;

/// py: miniapp.py:97 `SPEND_ALERTS_USD`
pub const SPEND_ALERTS_USD: &[f64] = &[5.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0];

/// py: miniapp.py:102 `APP_CSP`
pub const APP_CSP: &str = "{sandbox}; default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' https://telegram.org/js/telegram-web-app.js {origin}/eve.js https://cdn.jsdelivr.net/npm/; style-src 'unsafe-inline' https://cdn.jsdelivr.net/npm/; img-src data: blob: https://cdn.jsdelivr.net/npm/; font-src data: https://cdn.jsdelivr.net/npm/; media-src data: blob:; worker-src blob:; connect-src {origin}/api/apps/{slug}/ https://cdn.jsdelivr.net/npm/; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";

/// py: miniapp.py:109 `TUNNEL_URL_RE`
/// Python flags 32.
pub static TUNNEL_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https://[a-z0-9-]+\.trycloudflare\.com"#).expect("TUNNEL_URL_RE"));

/// py: miniapp.py:110 `TUNNEL_START_SECS`
pub const TUNNEL_START_SECS: f64 = 45.0;

/// py: miniapp.py:111 `TUNNEL_RETRY_SECS`
pub const TUNNEL_RETRY_SECS: f64 = 10.0;

// py: miniapp.py:112 `PAGE_PATH` (PosixPath): ported by this file's unit.

// py: miniapp.py:115 `IRO_PATH` (PosixPath): ported by this file's unit.

/// py: miniapp.py:116 `CAP_LINE`
pub const CAP_LINE: &str = "Today's Claude budget is used up (${cap:.2f}). It resets at midnight.";

/// py: miniapp.py:117 `LEDGER_DAYS`
/// how many days of spend the ledger keeps
pub const LEDGER_DAYS: i64 = 400;

/// py: miniapp.py:118 `CORS`
pub const CORS: &[(&str, &str)] = &[("Access-Control-Allow-Origin", "*")];

/// py: miniapp.py:119 `PROGRESS_SECS`
pub const PROGRESS_SECS: f64 = 2.0;

/// py: miniapp.py:125 `class MiniAppConfig:`
#[derive(Debug, Clone)]
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

impl MiniAppConfig {
    // py: miniapp.py:135 `__repr__`: Rust trait impl, ported by this file's unit.
}

/// py: miniapp.py:140 `def configured(environ: Any = None) -> MiniAppConfig:`
pub fn configured(environ: Option<&Environ>) -> MiniAppConfig {
    todo!("miniapp.py:140 configured")
}

/// py: miniapp.py:170 `def phone_report(body: dict[str, Any]) -> str:`
///
/// One log line from an app's own report (eve.js): which Telegram it runs in, whether Telegram's native
/// bridge is there (without it the MainButton and BackButton do nothing), and the page's errors. The only way to
/// see what an app does on the real phone (owner, 2026-09-24: "the expense splitter doesn't work"). Every field
/// is cut short and stripped of line breaks: it is text the page sent.
pub fn phone_report(body: &Obj) -> String {
    todo!("miniapp.py:170 phone_report")
}

/// py: miniapp.py:185 `def check_init_data(init_data: str, token: str, owner_ids: frozenset[int], *, now: Optional[float] = None, max_age: float = INIT_DATA_MAX_AGE_SECS) -> Optional[int]:`
///
/// The owner's user id when ``init_data`` is signed by this bot, fresh, and from an owner; else None.
///
/// Telegram's scheme: secret = HMAC-SHA256(key="WebAppData", msg=bot token); the hash is
/// HMAC-SHA256(key=secret, msg=every other field as key=value, sorted by key, joined by newlines).
pub fn check_init_data(init_data: &str, token: &str, owner_ids: &[i64], now: Option<f64>, max_age: f64) -> Option<i64> {
    todo!("miniapp.py:185 check_init_data")
}

/// py: miniapp.py:216 `def _app_key(token: str) -> bytes:`
pub(crate) fn app_key(token: &str) -> Vec<u8> {
    todo!("miniapp.py:216 _app_key")
}

/// py: miniapp.py:220 `def app_token(slug: str, token: str, *, now: Optional[float] = None) -> str:`
///
/// The key to one app: ``<expiry>.<signature>``, signed with a key derived from the bot token and bound to
/// ``slug``. It opens that app's page and answers its load and save, nothing else, until it expires.
pub fn app_token(slug: &str, token: &str, now: Option<f64>) -> String {
    todo!("miniapp.py:220 app_token")
}

/// py: miniapp.py:227 `def check_app_token(tok: str, slug: str, token: str, *, now: Optional[float] = None) -> bool:`
pub fn check_app_token(tok: &str, slug: &str, token: &str, now: Option<f64>) -> bool {
    todo!("miniapp.py:227 check_app_token")
}

/// py: miniapp.py:236 `def app_csp(origin: str, slug: str) -> str:`
pub fn app_csp(origin: &str, slug: &str) -> String {
    todo!("miniapp.py:236 app_csp")
}

/// py: miniapp.py:244 `def cost_usd(model: str, usage: Any) -> float:`
pub fn cost_usd(model: &str, usage: &Value) -> f64 {
    todo!("miniapp.py:244 cost_usd")
}

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
    pub on_cross: Mutex<Option<Arc<dyn Fn(f64, f64) + Send + Sync>>>,
}

impl SpendLedger {
    /// py: miniapp.py:269 `def __init__(self, path: Path, cap_usd: Optional[float] = None, today: Callable[[], str] = lambda: _dt.date.today().isoformat()):`
    pub fn new(path: &Path, cap_usd: Option<f64>, today: Option<StrFn>) -> Self {
        todo!("miniapp.py:269 SpendLedger.__init__")
    }

    /// py: miniapp.py:276 `def days(self) -> dict[str, float]:`
    ///
    /// Every recorded day's total, by date. A file from the capped version (``{"date", "usd"}``) is read
    /// as its one day.
    pub fn days(&self) -> HashMap<String, f64> {
        todo!("miniapp.py:276 SpendLedger.days")
    }

    /// py: miniapp.py:296 `def spent(self) -> float:`
    pub fn spent(&self) -> f64 {
        todo!("miniapp.py:296 SpendLedger.spent")
    }

    /// py: miniapp.py:299 `def left(self) -> Optional[float]:`
    ///
    /// What the cap leaves today, or None when there is no cap.
    pub fn left(&self) -> Option<f64> {
        todo!("miniapp.py:299 SpendLedger.left")
    }

    /// py: miniapp.py:303 `def over(self) -> bool:`
    pub fn over(&self) -> bool {
        todo!("miniapp.py:303 SpendLedger.over")
    }

    /// py: miniapp.py:306 `def add(self, usd: float) -> float:`
    ///
    /// Record ``usd`` against today; today's new total.
    pub fn add(&self, usd: f64) -> f64 {
        todo!("miniapp.py:306 SpendLedger.add")
    }
}

/// py: miniapp.py:326 `def _error_line(e: Exception) -> str:`
pub(crate) fn error_line(e: &anyhow::Error) -> String {
    todo!("miniapp.py:326 _error_line")
}

/// py: miniapp.py:344 `class MiniAppServer:`
pub struct MiniAppServer {
    pub cfg: MiniAppConfig,
    pub ledger: Arc<SpendLedger>,
    pub(crate) page: Option<Vec<u8>>,
    /// apps_maker.AppStore / AppMaker; None: no apps
    pub store: Option<Arc<crate::apps_maker::AppStore>>,
    /// apps_maker.AppStore / AppMaker; None: no apps
    pub maker: Option<Arc<crate::apps_maker::AppMaker>>,
    /// one build at a time per owner
    pub(crate) building: Mutex<HashSet<i64>>,
    /// the app slugs being changed right now, shared with the chat door
    pub changing: Arc<Mutex<HashSet<String>>>,
    pub builds: Mutex<HashMap<i64, Obj>>,
    pub notify: Option<BuildNotify>,
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

impl MiniAppServer {
    /// py: miniapp.py:345 `def __init__(self, cfg: MiniAppConfig, ledger: SpendLedger, page: Optional[bytes] = None, store: Any = None, maker: Any = None, changing: Optional[set[str]] = None, notify: Optional[Callable[[int, dict[str, Any]], Awaitable[Any]]] = None) -> None:`
    pub fn new(cfg: &MiniAppConfig, ledger: Arc<SpendLedger>, page: Option<&[u8]>, store: Option<Arc<crate::apps_maker::AppStore>>, maker: Option<Arc<crate::apps_maker::AppMaker>>, changing: Option<Arc<Mutex<HashSet<String>>>>, notify: Option<BuildNotify>) -> Arc<Self> {
        todo!("miniapp.py:345 MiniAppServer.__init__")
    }

    /// py: miniapp.py:373 `def page(self) -> bytes:`
    pub fn page(self: &Arc<Self>) -> Vec<u8> {
        todo!("miniapp.py:373 MiniAppServer.page")
    }

    /// py: miniapp.py:378 `def spend(self) -> dict[str, Any]:`
    ///
    /// What the page shows in its header: ALL of eve's spend today (spend.py's meter: chat, voice, search,
    /// builds, everything), the app builder's own share, and its cap (None: none).
    pub fn spend(self: &Arc<Self>) -> Obj {
        todo!("miniapp.py:378 MiniAppServer.spend")
    }

    /// py: miniapp.py:384 `def spending(self) -> dict[str, Any]:`
    ///
    /// The Spending view: eve's own meter (today split by provider, feature and model, yesterday, the last
    /// 30 days, this month) and each provider's own figure where one can be read (spend_sync.py). Aggregates
    /// only: no prompt, no answer, no key ever leaves in it.
    pub fn spending(self: &Arc<Self>) -> Obj {
        todo!("miniapp.py:384 MiniAppServer.spending")
    }

    /// py: miniapp.py:393 `def open_url(self, slug: str) -> str:`
    ///
    /// An app's page with a fresh app token, relative to the home page.
    pub fn open_url(self: &Arc<Self>, slug: &str) -> String {
        todo!("miniapp.py:393 MiniAppServer.open_url")
    }

    /// py: miniapp.py:397 `def app_link(self, slug: str, headers: dict[str, str]) -> str:`
    ///
    /// An app's page with a fresh app token at this server's public origin (the tunnel's): what Eve Link opens
    /// (link_chat.py, ``POST /api/link/apps/<slug>/open``). The token is the one ``check_app_token`` takes.
    pub fn app_link(self: &Arc<Self>, slug: &str, headers: &HashMap<String, String>) -> String {
        todo!("miniapp.py:397 MiniAppServer.app_link")
    }

    /// py: miniapp.py:402 `async def start(self, port: int = 0) -> int:`
    pub async fn start(self: &Arc<Self>, port: i64) -> i64 {
        todo!("miniapp.py:402 MiniAppServer.start")
    }

    /// py: miniapp.py:407 `async def close(self) -> None:`
    ///
    /// Stop serving. Open connections are cut, and the wait for them is bounded: a build's stream stays
    /// open for minutes, and a daemon restart must not wait for it (on Python 3.12+ wait_closed waits for every
    /// handler) until launchd kills it before the pinned button says offline.
    pub async fn close(self: &Arc<Self>) {
        todo!("miniapp.py:407 MiniAppServer.close")
    }

    /// py: miniapp.py:428 `async def _handle(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:`
    pub(crate) async fn handle(self: &Arc<Self>, reader: &mut (dyn tokio::io::AsyncBufRead + Unpin + Send), writer: &mut (dyn tokio::io::AsyncWrite + Unpin + Send)) {
        todo!("miniapp.py:428 MiniAppServer._handle")
    }

    /// py: miniapp.py:442 `def _origin(self, headers: dict[str, str]) -> str:`
    ///
    /// This server's public origin: the tunnel's, else (local use, tests) what the Host header names.
    pub(crate) fn origin(self: &Arc<Self>, headers: &HashMap<String, String>) -> String {
        todo!("miniapp.py:442 MiniAppServer._origin")
    }

    /// py: miniapp.py:449 `async def _serve(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:`
    pub(crate) async fn serve(self: &Arc<Self>, reader: &mut (dyn tokio::io::AsyncBufRead + Unpin + Send), writer: &mut (dyn tokio::io::AsyncWrite + Unpin + Send)) {
        todo!("miniapp.py:449 MiniAppServer._serve")
    }

    /// py: miniapp.py:588 `async def _app_page(self, writer: asyncio.StreamWriter, slug: str, query: str, headers: dict[str, str]) -> None:`
    ///
    /// One app's page, for its app token only, served sandboxed (APP_CSP). Without a valid token (an Open
    /// button from before tokens, a link someone saw) the answer is the same for every slug, so it says
    /// nothing about which apps exist: the home page, which opens the app when Telegram signed the visit.
    pub(crate) async fn app_page(self: &Arc<Self>, writer: &mut (dyn tokio::io::AsyncWrite + Unpin + Send), slug: &str, query: &str, headers: &HashMap<String, String>) {
        todo!("miniapp.py:588 MiniAppServer._app_page")
    }

    /// py: miniapp.py:605 `def _building_now(self, user: int) -> list[dict[str, Any]]:`
    ///
    /// The builds running now: this owner's own from the page, and apps the chat door is changing.
    pub(crate) fn building_now(self: &Arc<Self>, user: i64) -> Vec<Obj> {
        todo!("miniapp.py:605 MiniAppServer._building_now")
    }

    /// py: miniapp.py:617 `async def _apps_api(self, writer: asyncio.StreamWriter, path: str, api_app: Any, body: dict[str, Any], user: int) -> None:`
    pub(crate) async fn apps_api(self: &Arc<Self>, writer: &mut (dyn tokio::io::AsyncWrite + Unpin + Send), path: &str, api_app: &Value, body: &Obj, user: i64) {
        todo!("miniapp.py:617 MiniAppServer._apps_api")
    }

    /// py: miniapp.py:663 `def _history(self, slug: str, user: int) -> tuple[int, dict[str, Any]]:`
    ///
    /// One app's change chat: the app (with a fresh Open address), what was asked of it and how each build
    /// went (AppStore.thread: when, the request, version, the check's line, the journeys, what may still be
    /// wrong, cost), and the change running on it now, if any (``stage`` and ``secs`` only for this owner's
    /// own build from the page; a change from the chat door has neither).
    pub(crate) fn history(self: &Arc<Self>, slug: &str, user: i64) -> (i64, Obj) {
        todo!("miniapp.py:663 MiniAppServer._history")
    }

    /// py: miniapp.py:680 `def _app_io(self, slug: str, action: str, body: dict[str, Any]) -> tuple[int, dict[str, Any]]:`
    pub(crate) fn app_io(self: &Arc<Self>, slug: &str, action: &str, body: &Obj) -> (i64, Obj) {
        todo!("miniapp.py:680 MiniAppServer._app_io")
    }

    /// py: miniapp.py:698 `def _app_action(self, slug: str, action: str, body: dict[str, Any]) -> tuple[int, dict[str, Any]]:`
    ///
    /// One app's load, save, delete, rename, revert or restore. Delete, rename and revert wait while the app
    /// is being changed: the build would otherwise save over a rename or bring a deleted app back.
    pub(crate) fn app_action(self: &Arc<Self>, slug: &str, action: &str, body: &Obj) -> (i64, Obj) {
        todo!("miniapp.py:698 MiniAppServer._app_action")
    }

    /// py: miniapp.py:747 `async def _make(self, writer: asyncio.StreamWriter, request_text: str, slug: str, user: int = 0) -> None:`
    ///
    /// Build or change one app, streaming ``{"s": stage, "p": n}`` (apps_maker's progress: thinking, writing
    /// with the characters so far, testing, fixing with the number of problems, saving) at least every
    /// PROGRESS_SECS and at once on a new stage, then one done or error event. A phone that goes away does not
    /// stop the build: it finishes and saves, and its result goes to the chat instead (``notify``).
    pub(crate) async fn make(self: &Arc<Self>, writer: &mut (dyn tokio::io::AsyncWrite + Unpin + Send), request_text: &str, slug: &str, user: i64) {
        todo!("miniapp.py:747 MiniAppServer._make")
    }

    /// py: miniapp.py:822 `async def _send(self, writer: asyncio.StreamWriter, status: int, body: bytes, ctype: str, extra: Optional[dict[str, str]] = None) -> None:`
    pub(crate) async fn send(self: &Arc<Self>, writer: &mut (dyn tokio::io::AsyncWrite + Unpin + Send), status: i64, body: &[u8], ctype: &str, extra: Option<&HashMap<String, String>>) {
        todo!("miniapp.py:822 MiniAppServer._send")
    }

    /// py: miniapp.py:832 `async def _json(self, writer: asyncio.StreamWriter, status: int, obj: dict[str, Any], extra: Optional[dict[str, str]] = None) -> None:`
    pub(crate) async fn json(self: &Arc<Self>, writer: &mut (dyn tokio::io::AsyncWrite + Unpin + Send), status: i64, obj: &Obj, extra: Option<&HashMap<String, String>>) {
        todo!("miniapp.py:832 MiniAppServer._json")
    }
}

/// py: miniapp.py:839 `def cloudflared_bin() -> Optional[str]:`
pub fn cloudflared_bin() -> Option<String> {
    todo!("miniapp.py:839 cloudflared_bin")
}

/// py: miniapp.py:845 `class QuickTunnel:`
///
/// ``cloudflared tunnel --url http://127.0.0.1:<port>``: prints its trycloudflare.com address on stderr.
pub struct QuickTunnel {
    pub binary: String,
    pub proc: Mutex<Option<Arc<tokio::sync::Mutex<tokio::process::Child>>>>,
    pub url: String,
    pub(crate) drain: Option<tokio::task::JoinHandle<()>>,
}

impl QuickTunnel {
    /// py: miniapp.py:848 `def __init__(self, binary: str) -> None:`
    pub fn new(binary: &str) -> Arc<Self> {
        todo!("miniapp.py:848 QuickTunnel.__init__")
    }

    /// py: miniapp.py:854 `async def start(self, port: int, timeout: float = TUNNEL_START_SECS) -> str:`
    pub async fn start(self: &Arc<Self>, port: i64, timeout: f64) -> String {
        todo!("miniapp.py:854 QuickTunnel.start")
    }

    /// py: miniapp.py:875 `async def _drain_stderr(self) -> None:`
    pub(crate) async fn drain_stderr(self: &Arc<Self>) {
        todo!("miniapp.py:875 QuickTunnel._drain_stderr")
    }

    /// py: miniapp.py:880 `async def wait(self) -> int:`
    pub async fn wait(self: &Arc<Self>) -> i64 {
        todo!("miniapp.py:880 QuickTunnel.wait")
    }

    /// py: miniapp.py:884 `async def stop(self) -> None:`
    pub async fn stop(self: &Arc<Self>) {
        todo!("miniapp.py:884 QuickTunnel.stop")
    }
}

// py: miniapp.py:902 `Bot` (_CallableGenericAlias): ported by this file's unit.

/// py: miniapp.py:903 `PIN_TEXT`
pub const PIN_TEXT: &str = "eve: your apps.";

/// py: miniapp.py:904 `PIN_OFFLINE_TEXT`
pub const PIN_OFFLINE_TEXT: &str = "eve's apps are offline (the Mac is asleep or restarting). This button comes back by itself.";

/// py: miniapp.py:905 `OPEN_TEXT`
pub const OPEN_TEXT: &str = "Open eve";

/// py: miniapp.py:908 `def bot_caller(token: str, environ: Any = None) -> Bot:`
///
/// One Bot API call as ``await bot(method, data)`` -> Telegram's JSON reply (``{"ok": ...}``). With
/// EVE_TELEGRAM=app (telegram.py's app-only mode) nothing reaches Telegram: every call is refused here.
pub fn bot_caller(token: &str, environ: Option<&Environ>) -> Bot {
    todo!("miniapp.py:908 bot_caller")
}

/// py: miniapp.py:930 `def open_button(url: str, stick_url: str = "") -> dict[str, Any]:`
///
/// The pinned message's buttons: Open, and once Eve Link is paired, its tap-to-update link (stick_link.py),
/// a plain URL button because it opens a page that hands off to the app, not a Mini App.
pub fn open_button(url: &str, stick_url: &str) -> Obj {
    todo!("miniapp.py:930 open_button")
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
        todo!("miniapp.py:944 Pins.__init__")
    }

    /// py: miniapp.py:947 `def _load(self) -> dict[str, int]:`
    pub(crate) fn load(&self) -> HashMap<String, i64> {
        todo!("miniapp.py:947 Pins._load")
    }

    /// py: miniapp.py:953 `def _save(self, pins: dict[str, int]) -> None:`
    pub(crate) fn save(&self, pins: &HashMap<String, i64>) {
        todo!("miniapp.py:953 Pins._save")
    }

    /// py: miniapp.py:957 `async def _ok(self, method: str, data: dict) -> Any:`
    pub(crate) async fn ok(&self, method: &str, data: &Obj) -> Value {
        todo!("miniapp.py:957 Pins._ok")
    }

    /// py: miniapp.py:969 `async def show(self, chat_id: int, url: str, stick_url: str = "") -> None:`
    ///
    /// The / menu on the menu button, and the pinned message's button pointing at ``url`` (and Eve Link's
    /// update button at ``stick_url``, when it is paired).
    pub async fn show(&self, chat_id: i64, url: &str, stick_url: &str) {
        todo!("miniapp.py:969 Pins.show")
    }

    /// py: miniapp.py:987 `async def offline(self, chat_id: int) -> None:`
    ///
    /// The daemon is stopping: the pinned button would lead nowhere, so it says so instead.
    pub async fn offline(&self, chat_id: i64) {
        todo!("miniapp.py:987 Pins.offline")
    }
}

// py: miniapp.py:996 `_DEFAULT` (object): ported by this file's unit.

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
    pub store: Arc<crate::apps_maker::AppStore>,
    pub maker: Arc<crate::apps_maker::AppMaker>,
    pub changing: Arc<Mutex<HashSet<String>>>,
    pub server: Arc<MiniAppServer>,
    pub(crate) tunnel_factory: Option<TunnelFactory>,
    pub(crate) bot: Bot,
    pub pins: Pins,
    pub url: Mutex<String>,
    pub(crate) notes: Mutex<Vec<Job>>,
}

impl MiniApp {
    /// py: miniapp.py:1006 `def __init__(self, cfg: MiniAppConfig, *, tunnel_factory: Optional[Callable[[], Any]] = None, bot: Optional[Bot] = None, pins_path: Optional[Path] = None, generate: Any = None, apps_root: Optional[Path] = None, check: Any = _DEFAULT, context: Optional[Callable[[], str]] = None) -> None:`
    pub fn new(cfg: &MiniAppConfig, tunnel_factory: Option<TunnelFactory>, bot: Option<Bot>, pins_path: Option<&Path>, generate: Option<crate::apps_maker::Generate>, apps_root: Option<&Path>, check: Lent<crate::apps_maker::Check>, context: Option<StrFn>) -> Arc<Self> {
        todo!("miniapp.py:1006 MiniApp.__init__")
    }

    /// py: miniapp.py:1035 `def _meter_build(self, usage: Any) -> float:`
    ///
    /// One build round's cost: into the daily spend meter (spend.py, feature "app builder", unpriced for a
    /// model pricing.py does not know), and returned at the cap's never-under-counted price for this ledger.
    pub(crate) fn meter_build(self: &Arc<Self>, usage: &Value) -> f64 {
        todo!("miniapp.py:1035 MiniApp._meter_build")
    }

    /// py: miniapp.py:1041 `def app_url(self, slug: str) -> str:`
    ///
    /// An Open button's address for one app right now ("" while the tunnel is down). It goes through the
    /// home page, which gets Telegram's signed initData and opens the app with an app token: the app never
    /// sees the initData, which answers for the whole API.
    pub fn app_url(self: &Arc<Self>, slug: &str) -> String {
        todo!("miniapp.py:1041 MiniApp.app_url")
    }

    /// py: miniapp.py:1047 `def chat_url(self, slug: str) -> str:`
    ///
    /// A Change button's address: the app's change chat on the home page ("" while the tunnel is down).
    pub fn chat_url(self: &Arc<Self>, slug: &str) -> String {
        todo!("miniapp.py:1047 MiniApp.chat_url")
    }

    /// py: miniapp.py:1051 `async def _say(self, text: str, buttons: Optional[list[tuple[str, str]]] = None) -> None:`
    pub(crate) async fn say(self: &Arc<Self>, text: &str, buttons: Option<&[(String, String)]>) {
        todo!("miniapp.py:1051 MiniApp._say")
    }

    /// py: miniapp.py:1059 `async def _build_result(self, user: int, out: dict[str, Any]) -> None:`
    ///
    /// A build from the page finished after the page went away (a locked phone): its result comes to the
    /// chat, as a build from the chat does, so it is never lost.
    pub(crate) async fn build_result(self: &Arc<Self>, user: i64, out: &Obj) {
        todo!("miniapp.py:1059 MiniApp._build_result")
    }

    /// py: miniapp.py:1080 `def _spend_note(self, mark: float, total: float) -> None:`
    ///
    /// Today's Claude spend passed ``mark``: a line in the chat. Tracking, not a limit (owner: "no claude
    /// limit, just track spend"), so a runaway (a leaked key, a loop) is seen the same day.
    pub(crate) fn spend_note(self: &Arc<Self>, mark: f64, total: f64) {
        todo!("miniapp.py:1080 MiniApp._spend_note")
    }

    /// py: miniapp.py:1092 `def stick_url(self) -> str:`
    ///
    /// Eve Link's tap-to-update link at the tunnel's address now; "" before pairing or with no tunnel.
    pub fn stick_url(self: &Arc<Self>) -> String {
        todo!("miniapp.py:1092 MiniApp.stick_url")
    }

    /// py: miniapp.py:1099 `async def stick_link(self, rotate: bool = False) -> str:`
    ///
    /// The owner's /stick: the link token (a new one with ``rotate``), and the pinned message's Update
    /// button pointing at it. Returns the pairing link, or "" while the tunnel is down.
    pub async fn stick_link(self: &Arc<Self>, rotate: bool) -> String {
        todo!("miniapp.py:1099 MiniApp.stick_link")
    }

    /// py: miniapp.py:1111 `async def _point_pins(self, url: str) -> None:`
    pub(crate) async fn point_pins(self: &Arc<Self>, url: &str) {
        todo!("miniapp.py:1111 MiniApp._point_pins")
    }

    /// py: miniapp.py:1116 `async def _announce_loop(self, every: float = 30.0, clock: Callable[[], float] = time.monotonic) -> None:`
    ///
    /// Eve Link's rendezvous (stick_link.publish_address): the address goes out when it changes, when the token
    /// does, and again every REPOST_SECS, so a phone that missed the restart finds it.
    pub(crate) async fn announce_loop(self: &Arc<Self>, every: f64, clock: Clock) {
        todo!("miniapp.py:1116 MiniApp._announce_loop")
    }

    /// py: miniapp.py:1141 `async def run(self) -> None:`
    pub async fn run(self: &Arc<Self>) {
        todo!("miniapp.py:1141 MiniApp.run")
    }
}

/// py: miniapp.py:902 `Bot = Callable[[str, dict], Awaitable[Any]]`: a Bot API call (method, data) → its result; an
/// error is the call failing.
pub type Bot = Arc<dyn Fn(String, Obj) -> futures::future::BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;
/// `notify(user, build)`: a build finished (MiniApp._build_result).
pub type BuildNotify = Arc<dyn Fn(i64, Obj) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;
/// `tunnel_factory()`: a tunnel to start (QuickTunnel(binary), or a test's fake).
pub type TunnelFactory = Arc<dyn Fn() -> Arc<dyn Tunnel> + Send + Sync>;

/// QuickTunnel as MiniApp drives it: `start(port) -> url`, `wait() -> exit code`, `stop()`.
#[async_trait::async_trait]
pub trait Tunnel: Send + Sync {
    async fn start(&self, port: i64, timeout: f64) -> anyhow::Result<String>;
    async fn wait(&self) -> i64;
    async fn stop(&self);
}

/// The Mini App server as Eve Link's door reads it (`getattr(server, name)`, `server.store`, `server.app_link`).
impl eve_link::seams::MiniApp for MiniAppServer {
    fn hub(&self, name: &str) -> Option<Arc<dyn eve_link::seams::Hub>> {
        todo!("miniapp.py getattr(server, name, None)")
    }
    fn app_store(&self) -> Option<Arc<dyn eve_link::seams::AppStoreView>> {
        todo!("miniapp.py server.store")
    }
    fn app_link(&self, slug: &str, headers: &eve_link::seams::Headers) -> String {
        todo!("miniapp.py MiniAppServer.app_link")
    }
}

/// The live Mini App as the chat's app tools use it (apps_maker.ChatMaker's `app`).
#[async_trait::async_trait]
impl MakerHost for MiniApp {
    fn url(&self) -> String {
        self.url.lock().clone()
    }
    fn maker(&self) -> Arc<crate::apps_maker::AppMaker> {
        self.maker.clone()
    }
    fn store(&self) -> Arc<crate::apps_maker::AppStore> {
        self.store.clone()
    }
    fn app_url(&self, slug: &str) -> String {
        todo!("miniapp.py:1041 MiniApp.app_url (through the Arc)")
    }
    fn chat_url(&self, slug: &str) -> String {
        todo!("miniapp.py:1047 MiniApp.chat_url (through the Arc)")
    }
    async fn stick_link(&self, rotate: bool) -> String {
        todo!("miniapp.py:1099 MiniApp.stick_link (through the Arc)")
    }
}
