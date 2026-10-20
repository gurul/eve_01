//! A port of `apps_maker.py`, lines 1127-1956: the model call, the edit/repair prompts, AppMaker, EVE_JS, ChatMaker.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::apps_maker::*;

/// py: apps_maker.py:1130 `class Made:`
pub struct Made {
    pub ok: bool,
    pub app: Option<AppInfo>,
    pub reason: String,
    pub usd: f64,
    pub secs: f64,
    /// the check of the version that was kept
    pub check: Option<eve_app_check::CheckReport>,
    /// repair rounds used
    pub rounds: i64,
    /// what is still wrong with the kept version, if anything
    pub issues: Vec<String>,
    /// the problems each round found, in order
    pub history: Vec<Vec<String>>,
}

/// py: apps_maker.py:1143 `class Generated:`
#[derive(Debug, Clone)]
pub struct Generated {
    pub text: String,
    pub usage: Value,
    pub stop_reason: String,
    pub content: Value,
}

// py: apps_maker.py:1152 `Progress` (_CallableGenericAlias): ported by this file's unit.

// py: apps_maker.py:1153 `Generate` (_CallableGenericAlias): ported by this file's unit.

// py: apps_maker.py:1154 `Check` (_CallableGenericAlias): ported by this file's unit.

/// py: apps_maker.py:1157 `def _no_progress(stage: str, n: int) -> None:`
pub(crate) fn no_progress(stage: &str, n: i64) {
    todo!("apps_maker.py:1157 _no_progress")
}

/// py: apps_maker.py:1161 `def claude_generate(model: str, effort: str = "high", client: Any = None) -> Generate:`
///
/// One maker turn: stream ``messages`` to Claude under MAKER_PROMPT, reporting "thinking", then "writing"
/// with the characters written so far.
///
/// The system prompt is cached for an hour (``SYSTEM_CACHE_TTL``): it is the same for every build, and a first
/// round writes for four minutes or more, so the default five-minute cache had expired by the repair round and
/// every repair paid to write the whole prefix again. The request's top-level breakpoint (five minutes) covers
/// the conversation, which a quick edit-style repair round reads back in time.
pub fn claude_generate(model: &str, effort: &str, client: Option<Http>) -> Generate {
    todo!("apps_maker.py:1161 claude_generate")
}

/// py: apps_maker.py:1201 `def build_prompt(request: str, context: str) -> str:`
pub fn build_prompt(request: &str, context: &str) -> String {
    todo!("apps_maker.py:1201 build_prompt")
}

/// py: apps_maker.py:1205 `def edit_prompt(app: AppInfo, html: str, data: Any, requests: list[dict[str, str]], change: str, context: str, journeys: Optional[list[jev_verify.Journey]] = None) -> str:`
pub fn edit_prompt(app: &AppInfo, html: &str, data: &Value, requests: &[HashMap<String, String>], change: &str, context: &str, journeys: Option<&[eve_jev_verify::Journey]>) -> String {
    todo!("apps_maker.py:1205 edit_prompt")
}

/// py: apps_maker.py:1224 `def repair_prompt(issues: list[str], *, tested: bool = True, editable: bool = True) -> str:`
///
/// The problems, and how to answer. With a file to change (``editable``), edits: a repair of one or two
/// problems then costs a few hundred tokens and seconds, not the whole file again (25k tokens, minutes).
pub fn repair_prompt(issues: &[String], tested: bool, editable: bool) -> String {
    todo!("apps_maker.py:1224 repair_prompt")
}

/// py: apps_maker.py:1245 `JOURNEY_REPAIR`
pub const JOURNEY_REPAIR: &str = "

When a journey problem came from the journey itself (the app does the right thing and the journey names a control or a result in other words than the app shows), put the whole corrected ```journeys block after your fixes. Never change a journey to get around a problem in the app.";

/// py: apps_maker.py:1250 `EDIT_START`
pub const EDIT_START: &str = "<<<<<<< FIND";

/// py: apps_maker.py:1250 `EDIT_MID`
pub const EDIT_MID: &str = "=======";

/// py: apps_maker.py:1250 `EDIT_END`
pub const EDIT_END: &str = ">>>>>>> REPLACE";

/// py: apps_maker.py:1251 `_EDIT`
/// Python flags 56.
pub(crate) static EDIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?ms)^<{7} ?FIND[^\n]*\n(.*?)\n={7}[ \t]*\n(.*?)\n?>{7} ?REPLACE[^\n]*$"#).expect("EDIT"));

/// py: apps_maker.py:1254 `def apply_edits(html: str, answer: str) -> tuple[Optional[str], str]:`
///
/// ``html`` with the FIND/REPLACE blocks of ``answer`` applied in order: (the new file, "") or (None, what to
/// tell Claude). (None, "") when the answer holds no edits at all. A FIND must occur exactly once; when it
/// differs from the file only in the spaces around its lines, the one place it fits is used.
pub fn apply_edits(html: &str, answer: &str) -> (Option<String>, String) {
    todo!("apps_maker.py:1254 apply_edits")
}

/// py: apps_maker.py:1285 `_DATE_LIKE`
/// Python flags 32.
pub(crate) static DATE_LIKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\d{4}-\d{2}-\d{2}"#).expect("DATE_LIKE"));

/// py: apps_maker.py:1288 `def owner_values(data: Any, html: str) -> set[str]:`
///
/// The words in the owner's saved data that are theirs: every string and key with a letter, 3 characters or
/// more, that the app's own page does not hold (a default category the app ships is not the owner's). Dates are
/// left out: a journey that types today's date is not the owner's record.
pub fn owner_values(data: &Value, html: &str) -> Vec<String> {
    todo!("apps_maker.py:1288 owner_values")
}

/// py: apps_maker.py:1314 `def without_owner_data(journeys: list[jev_verify.Journey], data: Any, html: str, ) -> tuple[list[jev_verify.Journey], list[str]]:`
///
/// The journeys that carry none of the owner's saved data, and a problem for each that does. Jev runs off
/// the Mac (jev_verify: the step's words, the values typed and the expected text go to it), and an edit's
/// prompt shows Claude a sample of the saved data, which a journey can copy ("type Priya into Name").
pub fn without_owner_data(journeys: &[eve_jev_verify::Journey], data: &Value, html: &str) -> (Vec<eve_jev_verify::Journey>, Vec<String>) {
    todo!("apps_maker.py:1314 without_owner_data")
}

/// py: apps_maker.py:1333 `def stopped_reason(e: BaseException) -> str:`
///
/// Why a build stopped on an exception, as the change chat keeps it: the Claude API's own reason where
/// there is one (a refused key, a rate limit), else that it stopped.
pub fn stopped_reason(e: &anyhow::Error) -> String {
    todo!("apps_maker.py:1333 stopped_reason")
}

/// py: apps_maker.py:1343 `class AppMaker:`
///
/// Claude builds or changes one app: generate, check (in code, then in a headless phone), repair, keep the
/// best version. Every dollar goes to ``spend``; there is no cap (owner: "no claude limit, just track spend").
pub struct AppMaker {
    pub store: Arc<AppStore>,
    pub(crate) generate: Generate,
    pub(crate) cost: CostFn,
    pub(crate) spend: SpendFn,
    pub(crate) context: StrFn,
    pub(crate) check: Option<Check>,
    pub max_repairs: i64,
}

impl AppMaker {
    /// py: apps_maker.py:1347 `def __init__(self, store: AppStore, generate: Generate, *, cost: Callable[[Any], float], spend: Callable[[float], Any], context: Optional[Callable[[], str]] = None, check: Optional[Check] = app_check.check_app, max_repairs: int = 2) -> None:`
    pub fn new(store: Arc<AppStore>, generate: Generate, cost: CostFn, spend: SpendFn, context: Option<StrFn>, check: Option<Check>, max_repairs: i64) -> Arc<Self> {
        todo!("apps_maker.py:1347 AppMaker.__init__")
    }

    /// py: apps_maker.py:1355 `async def make(self, request: str, progress: Progress = _no_progress) -> Made:`
    pub async fn make(self: &Arc<Self>, request: &str, progress: Option<MakerProgress>) -> Made {
        todo!("apps_maker.py:1355 AppMaker.make")
    }

    /// py: apps_maker.py:1358 `async def edit(self, name: str, change: str, progress: Progress = _no_progress) -> Made:`
    pub async fn edit(self: &Arc<Self>, name: &str, change: &str, progress: Option<MakerProgress>) -> Made {
        todo!("apps_maker.py:1358 AppMaker.edit")
    }

    /// py: apps_maker.py:1385 `def _record_failed(self, slug: str, change: str, made: Made) -> None:`
    pub(crate) fn record_failed(self: &Arc<Self>, slug: &str, change: &str, made: &Made) {
        todo!("apps_maker.py:1385 AppMaker._record_failed")
    }

    /// py: apps_maker.py:1393 `async def _test(self, html: str, seed: Any, journeys: Optional[list[jev_verify.Journey]] = None, ) -> tuple[list[str], Optional[app_check.CheckReport]]:`
    pub(crate) async fn test(self: &Arc<Self>, html: &str, seed: &Value, journeys: Option<&[eve_jev_verify::Journey]>) -> (Vec<String>, Option<eve_app_check::CheckReport>) {
        todo!("apps_maker.py:1393 AppMaker._test")
    }

    /// py: apps_maker.py:1407 `async def _run(self, prompt: str, request: str, slug: str, seed: Any, progress: Progress, journeys: Optional[list[jev_verify.Journey]], tally: Optional[dict[str, Any]] = None) -> Made:`
    ///
    /// The build loop. ``journeys`` are the ones in hand (an edit's current ones, else None): an answer with a
    /// usable ```journeys block replaces them, one without keeps them, and the version kept is saved with the
    /// journeys it was checked with. A block that is empty or does not parse keeps them too, and stays a
    /// problem until a usable one comes: otherwise it would switch the verifier off for the app. A repair
    /// round's block never weakens the set: a journey it leaves out is still walked. On an edit (``seed``, the
    /// saved data), a journey that carries the owner's data is never walked (without_owner_data).
    /// ``tally`` gets the running cost and rounds, for a caller whose build raises part way.
    pub(crate) async fn run(self: &Arc<Self>, prompt: &str, request: &str, slug: &str, seed: &Value, progress: &MakerProgress, journeys: Option<&[eve_jev_verify::Journey]>, tally: Option<&Obj>) -> Made {
        todo!("apps_maker.py:1407 AppMaker._run")
    }
}

/// py: apps_maker.py:1537 `EVE_JS`
pub const EVE_JS: &str = "(() => {
  const slug = location.pathname.split(\"/\").filter(Boolean)[1] || \"\";
  const token = new URLSearchParams(location.search).get(\"t\") || \"\";
  const body = (rest) => '{\"t\":' + JSON.stringify(token) + rest + \"}\";
  async function post(path, raw) {
    const r = await fetch(path, { method: \"POST\", headers: { \"Content-Type\": \"text/plain\" }, body: raw,
      keepalive: raw.length < 60000 });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(j.error || (\"HTTP \" + r.status));
    return j;
  }
  let busy = false, next = null;       // next: { data, waiting: [[resolve, reject], ...] }, the newest value
  function pump() {
    if (busy || !next) return;
    const job = next; next = null; busy = true;
    post(\"/api/apps/\" + slug + \"/save\", body(',\"data\":' + job.data))
      .then(() => job.waiting.forEach((w) => w[0](true)), (e) => job.waiting.forEach((w) => w[1](e)))
      .finally(() => { busy = false; pump(); });
  }
  window.eve = {
    slug,
    load: async () => (await post(\"/api/apps/\" + slug + \"/load\", body(\"\"))).data ?? null,
    save: (value) => new Promise((resolve, reject) => {
      const data = JSON.stringify(value);  // a snapshot now: later changes to the object are the next save's
      if (data === undefined) { reject(new Error(\"eve.save needs a JSON value\")); return; }
      next = next || { waiting: [] };
      next.data = data; next.waiting.push([resolve, reject]);
      pump();
    }),
  };
  // What this page does on the real phone, sent once when it has settled and again after a new error (at most 3
  // reports): the Telegram it runs in, whether Telegram's native bridge is here (without it the MainButton and
  // BackButton do nothing), the MainButton's state, and the page's errors. The server logs it (phone_report).
  const errors = [];
  let reports = 0, timer = null;
  function report() {
    timer = null;
    if (reports >= 3) return;
    reports++;
    const w = window.Telegram && window.Telegram.WebApp, mb = w && w.MainButton;
    const bridge = window.TelegramWebviewProxy ? \"proxy\" : (window.external && window.external.notify) ? \"external\"
      : (window.parent !== window) ? \"iframe\" : \"none\";
    const info = { platform: w ? w.platform : \"\", version: w ? w.version : \"\", bridge,
      main_button: mb ? (mb.isVisible ? \"shown \" + JSON.stringify(mb.text || \"\") : \"hidden\") : \"\", errors: errors.slice(-5) };
    post(\"/api/apps/\" + slug + \"/report\", body(\",\" + JSON.stringify(info).slice(1, -1))).catch(() => {});
  }
  const later = (ms) => { if (!timer) timer = setTimeout(report, ms); };
  window.addEventListener(\"error\", (e) => { errors.push(String(e.message || e.error || \"error\") + (e.lineno ? \" @\" + e.lineno : \"\")); later(1000); });
  window.addEventListener(\"unhandledrejection\", (e) => { errors.push(\"promise: \" + String((e.reason && e.reason.message) || e.reason)); later(1000); });
  window.addEventListener(\"load\", () => later(3000));
  const tg = window.Telegram && window.Telegram.WebApp, bb = tg && tg.BackButton;
  if (bb && tg.isVersionAtLeast && tg.isVersionAtLeast(\"6.1\")) {
    const show = bb.show, on = bb.onClick, own = [];
    let appShows = false;              // the app wants Back (a view below its first): its handlers run
    on.call(bb, () => { if (appShows) own.slice().forEach((f) => f()); else location.href = \"/\"; });
    show.call(bb);
    bb.show = () => { appShows = true; return bb; };
    bb.hide = () => { appShows = false; return bb; };
    bb.onClick = (f) => { if (typeof f === \"function\" && !own.includes(f)) own.push(f); return bb; };
    bb.offClick = (f) => { const i = own.indexOf(f); if (i >= 0) own.splice(i, 1); return bb; };
  }
  // The way to change this app: its chat on eve's home page (/?chat=<slug>), where the owner's signed
  // initData lives. The app only navigates there; it is never given the power to change apps or spend.
  // Telegram's Settings item in the Mini App's \"...\" menu (7.0+) goes there, and so does a small pencil in the
  // page's top right corner, beside the app's title, that scrolls away with the page (the maker is told to
  // keep that corner free, and app_check reports a control under it). The pencil lives in a
  // closed shadow root on <html>, with its host's position set inline and !important, so the app's own CSS
  // (a \"button { ... }\" rule, a body that is re-rendered) cannot restyle or remove it by accident.
  const toChat = () => { location.href = \"/?chat=\" + encodeURIComponent(slug); };
  const sb = tg && tg.SettingsButton;
  if (sb && tg.isVersionAtLeast && tg.isVersionAtLeast(\"7.0\")) {
    try { sb.onClick(toChat); sb.show(); } catch (e) {}
  }
  function pencil() {
    if (!slug || document.querySelector(\"eve-change\")) return;
    const host = document.createElement(\"eve-change\");
    host.setAttribute(\"style\", \"all: initial !important; position: absolute !important; z-index: 2147483647 !important; \"
      + \"display: block !important; width: 44px !important; height: 44px !important; \"
      + \"top: calc(6px + var(--tg-safe-area-inset-top, 0px) + var(--tg-content-safe-area-inset-top, 0px)) !important; \"
      + \"right: calc(6px + var(--tg-safe-area-inset-right, 0px) + var(--tg-content-safe-area-inset-right, 0px)) !important;\");
    const root = host.attachShadow({ mode: \"closed\" });
    root.innerHTML = '<style>button { all: initial; box-sizing: border-box; width: 44px; height: 44px; display: grid; '
      + 'place-items: center; cursor: pointer; border-radius: 22px; -webkit-tap-highlight-color: transparent; } '
      + 'span { width: 32px; height: 32px; border-radius: 16px; display: grid; place-items: center; '
      + 'font: 17px/1 -apple-system, system-ui, sans-serif; opacity: .8; '
      + 'background: var(--tg-theme-secondary-bg-color, rgba(127, 127, 127, .16)); '
      + 'color: var(--tg-theme-link-color, #2481cc); } '
      + 'button:focus-visible span { outline: 2px solid var(--tg-theme-link-color, #2481cc); outline-offset: 2px; } '
      + '</style><button type=\"button\" aria-label=\"Change this app\" title=\"Change this app\">'
      + '<span aria-hidden=\"true\">✎</span></button>';
    const btn = root.querySelector(\"button\");
    btn.addEventListener(\"click\", toChat);
    document.documentElement.appendChild(host);
    // The corner belongs to the app first: while one of its own controls is under the pencil (a header's
    // Settings button, a dialog's close), the pencil is hidden, so a tap there reaches the app. Checked again
    // after every change to the page, a scroll or a resize, at most once a frame.
    const TAP = \"button,a[href],input,select,textarea,label,summary,[role=button],[role=tab],[role=link],\"
      + \"[role=checkbox],[role=switch],[role=radio],[role=menuitem],[role=option],[onclick]\";
    let queued = false, hidden = false;
    const under = () => {
      const r = host.getBoundingClientRect();
      if (r.bottom <= 0 || r.top >= innerHeight) return hidden;      // scrolled away: nothing to decide
      const pts = [[r.left + r.width / 2, r.top + r.height / 2], [r.left + 8, r.top + 8], [r.right - 8, r.top + 8],
        [r.left + 8, r.bottom - 8], [r.right - 8, r.bottom - 8]];
      return pts.some(([x, y]) => {
        const el = document.elementsFromPoint(x, y).find((e) => e !== host);
        return !!(el && el !== document.documentElement && el !== document.body && el.closest(TAP));
      });
    };
    const settle = () => {
      queued = false;
      const now = under();
      if (now === hidden) return;
      hidden = now;                    // the host and its button both: the button's \"all: initial\" is visible
      [host, btn].forEach((e) => e.style.setProperty(\"visibility\", now ? \"hidden\" : \"visible\", \"important\"));
    };
    const later = () => { if (!queued) { queued = true; requestAnimationFrame(settle); } };
    new MutationObserver(later).observe(document.documentElement, { subtree: true, childList: true, attributes: true,
      characterData: true });
    addEventListener(\"scroll\", later, { passive: true, capture: true });
    addEventListener(\"resize\", later);
    settle();
  }
  if (document.readyState === \"loading\") document.addEventListener(\"DOMContentLoaded\", pencil); else pencil();
})();
";

/// py: apps_maker.py:1665 `def serve_app_html(html: str) -> str:`
///
/// The app as served: Telegram's script and window.eve put in the head, ahead of the app's own code.
pub fn serve_app_html(html: &str) -> String {
    todo!("apps_maker.py:1665 serve_app_html")
}

/// py: apps_maker.py:1677 `def _app_arg(description: str = "Which app, by its name.") -> dict[str, Any]:`
pub(crate) fn app_arg(description: &str) -> Obj {
    todo!("apps_maker.py:1677 _app_arg")
}

/// py: apps_maker.py:1681 `MAKER_TOOLS`
/// The Python value as JSON (tuples are arrays).
pub static MAKER_TOOLS: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(r#"[{"type": "function", "name": "make_app", "strict": true, "description": "Build a NEW small app (a tracker, a log, a list, a calculator, a planner…) that opens inside Telegram and saves its data. Use it whenever the owner asks for an app, tracker or tool to be made. Takes a few minutes; the app's picture and an Open button are sent to the chat when it is ready.", "parameters": {"type": "object", "additionalProperties": false, "required": ["request"], "properties": {"request": {"type": "string", "description": "What the app should do, in the owner's words plus any detail they gave."}}}}, {"type": "function", "name": "change_app", "strict": true, "description": "Change one of the owner's EXISTING apps: add a feature, fix something, restyle it. Its saved data is kept. The new version's picture and Open button are sent when it is ready.", "parameters": {"type": "object", "additionalProperties": false, "required": ["app", "change"], "properties": {"app": {"type": "string", "description": "Which app, by its name."}, "change": {"type": "string", "description": "What to change."}}}}, {"type": "function", "name": "undo_app", "strict": true, "description": "Put one of the owner's apps back to how it was before its last change (undo, revert, go back, \"the last change broke it\"). Its saved data is untouched. When the name fits no app or more than one, nothing is undone and the candidates come back: ask which.", "parameters": {"type": "object", "additionalProperties": false, "required": ["app"], "properties": {"app": {"type": "string", "description": "Which app, by its name."}}}}, {"type": "function", "name": "rename_app", "strict": true, "description": "Give one of the owner's apps a new name. Only the name changes.", "parameters": {"type": "object", "additionalProperties": false, "required": ["app", "title"], "properties": {"app": {"type": "string", "description": "Which app, by its current name."}, "title": {"type": "string", "description": "The new name."}}}}, {"type": "function", "name": "delete_app", "strict": true, "description": "Delete one of the owner's apps when they ask to delete or remove it. Code asks for approval before deletion. It moves to eve's trash and restore_app brings it back. When the name fits no app or more than one, nothing is deleted and the candidates come back: ask which.", "parameters": {"type": "object", "additionalProperties": false, "required": ["app"], "properties": {"app": {"type": "string", "description": "Which app, by its name."}}}}, {"type": "function", "name": "restore_app", "strict": true, "description": "Bring back an app the owner deleted (\"undelete\", \"I deleted it by mistake\"), with its saved data.", "parameters": {"type": "object", "additionalProperties": false, "required": ["app"], "properties": {"app": {"type": "string", "description": "Which deleted app, by its name."}}}}, {"type": "function", "name": "list_apps", "strict": true, "description": "The apps eve has made for the owner, and a button to open each.", "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}}]"#).expect("MAKER_TOOLS"));

/// py: apps_maker.py:1736 `MAKER_TOOL_NAMES`
/// A Python set: sorted here; membership is what matters.
pub const MAKER_TOOL_NAMES: &[&str] = &["change_app", "delete_app", "list_apps", "make_app", "rename_app", "restore_app", "undo_app"];

// py: apps_maker.py:1738 `SendButtons` (_CallableGenericAlias): ported by this file's unit.

// py: apps_maker.py:1739 `SendPhoto` (_CallableGenericAlias): ported by this file's unit.

/// py: apps_maker.py:1742 `class ChatMaker:`
///
/// The Telegram brain's app tools, over a live Mini App (miniapp.MiniApp).
///
/// A build runs in the background like a Mac task: the tool returns at once, and when it is done the app's
/// screenshot (from the check) goes to the chat with an Open button under it (an inline ``web_app`` button,
/// so it opens as a Mini App with the owner's signed initData); a change also gets a Change button to the
/// app's change chat on the home page, to keep refining it. Undo, rename, delete and restore are instant;
/// the first three act only on an app the owner named for sure (AppStore.resolve), never on a guess.
pub struct ChatMaker {
    pub(crate) app: Arc<dyn MakerHost>,
    pub(crate) spawn: SpawnFn,
    pub building: Mutex<HashSet<String>>,
}

impl ChatMaker {
    /// py: apps_maker.py:1751 `def __init__(self, app: Any, spawn: Callable[[Awaitable[Any], str], Any]) -> None:`
    pub fn new(app: Arc<dyn MakerHost>, spawn: SpawnFn) -> Arc<Self> {
        todo!("apps_maker.py:1751 ChatMaker.__init__")
    }

    /// py: apps_maker.py:1757 `def live(self) -> bool:`
    pub fn live(self: &Arc<Self>) -> bool {
        todo!("apps_maker.py:1757 ChatMaker.live")
    }

    /// py: apps_maker.py:1760 `def tools(self) -> list[dict[str, Any]]:`
    pub fn tools(self: &Arc<Self>) -> Vec<Obj> {
        todo!("apps_maker.py:1760 ChatMaker.tools")
    }

    /// py: apps_maker.py:1763 `def home_url(self) -> str:`
    ///
    /// The Mini App's home screen right now ("" while the tunnel is down).
    pub fn home_url(self: &Arc<Self>) -> String {
        todo!("apps_maker.py:1763 ChatMaker.home_url")
    }

    /// py: apps_maker.py:1767 `async def stick_link(self, rotate: bool = False) -> str:`
    ///
    /// The owner's /stick (stick_link.py): Eve Link's pairing link, "" while the tunnel is down.
    pub async fn stick_link(self: &Arc<Self>, rotate: bool) -> String {
        todo!("apps_maker.py:1767 ChatMaker.stick_link")
    }

    /// py: apps_maker.py:1771 `def _missing(self, target: str, candidates: Optional[list[AppInfo]] = None) -> dict[str, Any]:`
    ///
    /// Nothing was done: the name fits no app, or more than one. The brain asks the owner which.
    pub(crate) fn missing(self: &Arc<Self>, target: &str, candidates: Option<&[AppInfo]>) -> Obj {
        todo!("apps_maker.py:1771 ChatMaker._missing")
    }

    /// py: apps_maker.py:1782 `async def handle(self, name: str, args: dict[str, Any], chat_id: int, send: SendButtons, say: Callable[[int, str], Awaitable[Any]], send_photo: Optional[SendPhoto] = None ) -> dict[str, Any]:`
    pub async fn handle(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64, send: SendButtons, say: SayFn, send_photo: Option<SendPhoto>) -> Obj {
        todo!("apps_maker.py:1782 ChatMaker.handle")
    }

    /// py: apps_maker.py:1852 `async def _restore(self, target: str, chat_id: int, send: SendButtons) -> dict[str, Any]:`
    pub(crate) async fn restore(self: &Arc<Self>, target: &str, chat_id: i64, send: &SendButtons) -> Obj {
        todo!("apps_maker.py:1852 ChatMaker._restore")
    }

    /// py: apps_maker.py:1874 `def _progress(self, chat_id: int, say: Callable[[int, str], Awaitable[Any]]) -> Progress:`
    ///
    /// A short line when a chat build reaches the phone test and each repair round, never more often than
    /// CHAT_PROGRESS_SECS: a build runs for minutes, and silence reads as a build that died.
    pub(crate) fn progress(self: &Arc<Self>, chat_id: i64, say: SayFn) -> MakerProgress {
        todo!("apps_maker.py:1874 ChatMaker._progress")
    }

    /// py: apps_maker.py:1905 `async def _build(self, chat_id: int, request: str, target: str, key: str, send: SendButtons, say: Callable[[int, str], Awaitable[Any]], send_photo: Optional[SendPhoto]) -> None:`
    pub(crate) async fn build(self: &Arc<Self>, chat_id: i64, request: &str, target: &str, key: &str, send: SendButtons, say: SayFn, send_photo: Option<SendPhoto>) {
        todo!("apps_maker.py:1905 ChatMaker._build")
    }
}

/// py: apps_maker.py:1152 `Progress = Callable[[str, int], None]`: (phase, characters written so far).
pub type MakerProgress = Arc<dyn Fn(&str, i64) + Send + Sync>;
/// py: apps_maker.py:1153 `Generate = Callable[[list[dict[str, Any]], Progress], Awaitable[Generated]]`; an error is the
/// call raising (stopped_reason reads it).
pub type Generate =
    Arc<dyn Fn(Vec<Value>, MakerProgress) -> futures::future::BoxFuture<'static, anyhow::Result<Generated>> + Send + Sync>;
/// py: apps_maker.py:1154 `Check = Callable[..., Awaitable[app_check.CheckReport]]`: (html, options) as
/// app_check.check_app takes them.
pub type Check = Arc<
    dyn Fn(String, eve_app_check::CheckOptions) -> futures::future::BoxFuture<'static, eve_app_check::CheckReport>
        + Send
        + Sync,
>;
/// `cost(usage) -> dollars`.
pub type CostFn = Arc<dyn Fn(&Value) -> f64 + Send + Sync>;
/// `spend(dollars)` (SpendLedger.add).
pub type SpendFn = Arc<dyn Fn(f64) -> f64 + Send + Sync>;
