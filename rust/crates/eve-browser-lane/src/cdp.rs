//! The real browser behind [`crate::driver`]: the Chrome DevTools Protocol spoken directly, in place of
//! Playwright's sync API.
//!
//! One WebSocket per browser, with one flattened session per page. The socket is driven by a small Tokio
//! runtime the connection owns; every call the lane makes is synchronous (it waits on a channel), because the
//! lane runs on its own worker thread exactly as the Python lane runs on its one ThreadPoolExecutor thread.
//!
//! What Playwright adds on top of the protocol, and the lane relies on, is kept:
//!
//! - `browser.contexts`: the default context first, then every context `Target.getBrowserContexts` lists, the
//!   same object for the same context on every call (the lane compares contexts with `is`);
//! - `page.url` is the main frame's last committed URL (Page.frameNavigated / navigatedWithinDocument), read
//!   without a round trip, so the Return-key wait can poll it;
//! - load states are the main frame's lifecycle events per document (`DOMContentLoaded`, `load`,
//!   `networkIdle`), so a wait answers for the committed document and never for one still on its way;
//! - locator actions wait as Playwright's do: the element attached, visible, enabled, still across a frame
//!   and, for a click, not covered at its centre; the click is a real mouse press at that point;
//! - `keyboard.type` presses each character of the US layout (key down with its text, key up) and inserts
//!   any other character as text; `fill` selects the field's contents and inserts the value;
//! - a JavaScript dialog nobody handles is dismissed, as Playwright dismisses it;
//! - a screenshot's mask paints the matched elements in the mask colour before the capture and removes the
//!   paint after it, so the pixels never leave the browser unmasked.
//!
//! `launch_persistent_context` starts Playwright's own Chromium (its headless shell for a headless launch, its
//! Chrome for Testing otherwise) from Playwright's browser cache, else the installed Google Chrome, with
//! Playwright's default switches.

use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::{Duration, Instant};

use base64::Engine;
use eve_desktop_lanes::LaneError;
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use crate::driver::{BrowserHandle, ContextHandle, Cookie, Driver, PageHandle, SelectBy};

/// What Playwright says of a page, context or browser that is gone.
pub const CLOSED: &str = "Target page, context or browser has been closed";
/// A protocol call that never answers gives up after this long (Playwright has no bound; the lane must not
/// hang on a page that stopped answering).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// `locator.evaluate`'s default wait for its element (Playwright's default timeout).
const ATTACH_TIMEOUT_MS: u64 = 30_000;
/// How often a wait looks again.
const POLL: Duration = Duration::from_millis(20);
/// Playwright's waits between retries of an action that is not ready yet, in ms.
const RETRY_MS: [u64; 6] = [0, 20, 50, 100, 100, 500];

fn closed() -> LaneError {
    LaneError::new("TargetClosedError", CLOSED)
}

fn timeout_error(api: &str, ms: u64) -> anyhow::Error {
    LaneError::err("TimeoutError", format!("{api}: Timeout {ms}ms exceeded."))
}

// ---- the connection ----------------------------------------------------------------------------------------

type Reply = std_mpsc::Sender<Result<Value, LaneError>>;

/// One page's main frame, as its events describe it.
#[derive(Default)]
struct Frames {
    main: String,
    url: String,
    /// the committed document's loader
    loader: String,
    /// loader → the lifecycle events it has fired
    lifecycle: HashMap<String, HashSet<String>>,
}

#[derive(Default)]
struct State {
    pending: HashMap<u64, (Option<String>, Reply)>,
    frames: HashMap<String, Frames>,
    gone: HashSet<String>,
    closed: bool,
}

/// One WebSocket to a browser, shared by every page: calls by id, page events folded into [`Frames`].
pub struct Connection {
    out: tokio::sync::mpsc::UnboundedSender<String>,
    state: Mutex<State>,
    next: AtomicU64,
    rt: Mutex<Option<tokio::runtime::Runtime>>,
    /// the subscribers ([`Connection::subscribe`]): every protocol event, as it arrives
    taps: Mutex<Vec<std_mpsc::Sender<Value>>>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.lock().take() {
            rt.shutdown_background();
        }
    }
}

impl Connection {
    /// Open the browser's WebSocket. `timeout` bounds the handshake (Chrome holds it while it asks the owner
    /// "Allow remote debugging?"); a refusal is the HTTP status in the error ("HTTP error: 403 Forbidden").
    pub fn connect(url: &str, timeout: Duration) -> anyhow::Result<Arc<Connection>> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("eve-cdp")
            .enable_all()
            .build()?;
        let (tx, rx) = std_mpsc::channel();
        let target = url.to_string();
        rt.spawn(async move {
            let config = WebSocketConfig::default().max_message_size(Some(512 << 20)).max_frame_size(Some(512 << 20));
            let r = tokio::time::timeout(
                timeout,
                tokio_tungstenite::connect_async_with_config(target, Some(config), false),
            )
            .await;
            let _ = tx.send(r);
        });
        let ws = match rx.recv() {
            Ok(Ok(Ok((ws, _)))) => ws,
            Ok(Ok(Err(e))) => {
                rt.shutdown_background();
                return Err(LaneError::err("Error", format!("BrowserType.connect_over_cdp: {e} ({url})")));
            }
            _ => {
                rt.shutdown_background();
                return Err(timeout_error("BrowserType.connect_over_cdp", timeout.as_millis() as u64));
            }
        };
        let (mut sink, mut stream) = ws.split();
        let (out, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let conn = Arc::new(Connection {
            out,
            state: Mutex::new(State::default()),
            next: AtomicU64::new(1),
            rt: Mutex::new(None),
            taps: Mutex::new(Vec::new()),
        });
        rt.spawn(async move {
            while let Some(text) = out_rx.recv().await {
                if sink.send(Message::text(text)).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });
        let weak = Arc::downgrade(&conn);
        rt.spawn(async move {
            while let Some(msg) = stream.next().await {
                let text = match msg {
                    Ok(Message::Text(t)) => t.as_str().to_string(),
                    Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                };
                let Some(conn) = weak.upgrade() else { return };
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    conn.dispatch(v);
                }
            }
            if let Some(conn) = weak.upgrade() {
                conn.shut();
            }
        });
        *conn.rt.lock() = Some(rt);
        Ok(conn)
    }

    /// The socket is gone: every waiting call fails, and every later one.
    pub fn shut(&self) {
        let pending: Vec<Reply> = {
            let mut st = self.state.lock();
            st.closed = true;
            st.pending.drain().map(|(_, (_, r))| r).collect()
        };
        for reply in pending {
            let _ = reply.send(Err(closed()));
        }
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    /// Every protocol event from now on (`{"method", "params", "sessionId"}`), in arrival order: what
    /// Playwright's `page.on(...)` and `page.route(...)` are built on. Dropping the receiver ends the tap.
    pub fn subscribe(&self) -> std_mpsc::Receiver<Value> {
        let (tx, rx) = std_mpsc::channel();
        self.taps.lock().push(tx);
        rx
    }

    /// A page's session is gone (closed, crashed, detached): its waiting calls fail, and every later one.
    fn gone(&self, session: &str) {
        let pending: Vec<Reply> = {
            let mut st = self.state.lock();
            st.gone.insert(session.to_string());
            st.frames.remove(session);
            let ids: Vec<u64> =
                st.pending.iter().filter(|(_, (s, _))| s.as_deref() == Some(session)).map(|(id, _)| *id).collect();
            ids.into_iter().filter_map(|id| st.pending.remove(&id)).map(|(_, r)| r).collect()
        };
        for reply in pending {
            let _ = reply.send(Err(closed()));
        }
    }

    fn dispatch(&self, v: Value) {
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            let entry = self.state.lock().pending.remove(&id);
            if let Some((_, reply)) = entry {
                let result = match v.get("error") {
                    Some(e) => Err(LaneError::new(
                        "Error",
                        e.get("message").and_then(Value::as_str).unwrap_or("Protocol error").to_string(),
                    )),
                    None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = reply.send(result);
            }
            return;
        }
        {
            let mut taps = self.taps.lock();
            if !taps.is_empty() {
                taps.retain(|tap| tap.send(v.clone()).is_ok());
            }
        }
        let method = v.get("method").and_then(Value::as_str).unwrap_or("");
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        let session = v.get("sessionId").and_then(Value::as_str);
        let text = |p: &Value, k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match (session, method) {
            (None, "Target.detachedFromTarget") => {
                let sid = text(&params, "sessionId");
                if !sid.is_empty() {
                    self.gone(&sid);
                }
            }
            (Some(sid), "Inspector.detached") | (Some(sid), "Inspector.targetCrashed") => self.gone(sid),
            (Some(sid), "Page.javascriptDialogOpening") => {
                // Playwright dismisses a dialog nobody listens for; an open dialog blocks every script.
                self.send_nowait(Some(sid), "Page.handleJavaScriptDialog", json!({"accept": false}));
            }
            (Some(sid), "Page.frameNavigated") => {
                let frame = params.get("frame").cloned().unwrap_or(Value::Null);
                if frame.get("parentId").is_none() {
                    let mut st = self.state.lock();
                    let f = st.frames.entry(sid.to_string()).or_default();
                    f.main = text(&frame, "id");
                    f.url = format!("{}{}", text(&frame, "url"), text(&frame, "urlFragment"));
                    f.loader = text(&frame, "loaderId");
                }
            }
            (Some(sid), "Page.navigatedWithinDocument") => {
                let mut st = self.state.lock();
                let f = st.frames.entry(sid.to_string()).or_default();
                if f.main.is_empty() || f.main == text(&params, "frameId") {
                    f.url = text(&params, "url");
                }
            }
            (Some(sid), "Page.lifecycleEvent") => {
                let mut st = self.state.lock();
                let f = st.frames.entry(sid.to_string()).or_default();
                if f.main.is_empty() || f.main == text(&params, "frameId") {
                    let loader = text(&params, "loaderId");
                    let name = text(&params, "name");
                    let fired = f.lifecycle.entry(loader).or_default();
                    if name == "init" {
                        fired.clear();
                    } else {
                        fired.insert(name);
                    }
                }
            }
            _ => {}
        }
    }

    /// One protocol call; its result, or the browser's error.
    pub fn call(&self, session: Option<&str>, method: &str, params: Value, timeout: Duration) -> Result<Value, LaneError> {
        let (tx, rx) = std_mpsc::channel();
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        {
            let mut st = self.state.lock();
            if st.closed || session.is_some_and(|s| st.gone.contains(s)) {
                return Err(closed());
            }
            st.pending.insert(id, (session.map(str::to_string), tx));
        }
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(sid) = session {
            msg["sessionId"] = json!(sid);
        }
        if self.out.send(msg.to_string()).is_err() {
            self.state.lock().pending.remove(&id);
            return Err(closed());
        }
        match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                self.state.lock().pending.remove(&id);
                Err(LaneError::new("TimeoutError", format!("{method}: Timeout {}ms exceeded.", timeout.as_millis())))
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => Err(closed()),
        }
    }

    /// A call whose answer nobody waits for.
    pub fn send_nowait(&self, session: Option<&str>, method: &str, params: Value) {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(sid) = session {
            msg["sessionId"] = json!(sid);
        }
        let _ = self.out.send(msg.to_string());
    }

    fn page_url(&self, session: &str) -> String {
        self.state.lock().frames.get(session).map(|f| f.url.clone()).unwrap_or_default()
    }

    /// Whether `loader` (the committed document's when None) has fired lifecycle event `name`.
    fn fired(&self, session: &str, loader: Option<&str>, name: &str) -> bool {
        let st = self.state.lock();
        let Some(f) = st.frames.get(session) else { return false };
        let loader = loader.unwrap_or(&f.loader);
        f.lifecycle.get(loader).is_some_and(|set| set.contains(name))
    }
}

// ---- the driver --------------------------------------------------------------------------------------------

/// `sync_playwright().start()` over CDP.
#[derive(Default)]
pub struct CdpDriver {
    conns: Vec<Arc<Connection>>,
    launched: Vec<Arc<Launched>>,
}

impl CdpDriver {
    pub fn new() -> Self {
        Self::default()
    }

    /// The real driver, as [`crate::driver::StartDriver`] hands it out.
    pub fn start() -> anyhow::Result<Box<dyn Driver>> {
        Ok(Box::new(CdpDriver::new()))
    }
}

impl Driver for CdpDriver {
    fn connect_over_cdp(&mut self, endpoint: &str, timeout_ms: u64) -> anyhow::Result<Arc<dyn BrowserHandle>> {
        let conn = Connection::connect(endpoint, Duration::from_millis(timeout_ms))?;
        self.conns.push(conn.clone());
        Ok(Arc::new(CdpBrowser::new(conn, None, None)))
    }

    fn launch_persistent_context(
        &mut self,
        profile: &Path,
        headless: bool,
        viewport: (u32, u32),
        args: &[&str],
    ) -> anyhow::Result<Arc<dyn ContextHandle>> {
        let exe = find_chromium(headless).ok_or_else(|| {
            LaneError::err(
                "Error",
                "BrowserType.launch_persistent_context: Executable doesn't exist (install it with: playwright install chromium)",
            )
        })?;
        let launched = Arc::new(Launched::start(&exe, profile, headless, args)?);
        self.conns.push(launched.conn.clone());
        self.launched.push(launched.clone());
        let browser = CdpBrowser::new(launched.conn.clone(), Some(viewport), Some(launched));
        let default = browser.context_for(None);
        Ok(default)
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        for launched in self.launched.drain(..) {
            launched.end();
        }
        for conn in self.conns.drain(..) {
            conn.shut();
        }
        Ok(())
    }
}

/// A Chromium this driver started, with its connection.
pub struct Launched {
    child: Mutex<Option<Child>>,
    conn: Arc<Connection>,
}

impl Launched {
    fn start(exe: &Path, profile: &Path, headless: bool, args: &[&str]) -> anyhow::Result<Launched> {
        let mut cmd = Command::new(exe);
        cmd.args(PLAYWRIGHT_SWITCHES);
        if headless {
            cmd.args(HEADLESS_SWITCHES);
        }
        cmd.arg(format!("--user-data-dir={}", profile.display()))
            .arg("--remote-debugging-port=0")
            .args(args)
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow::anyhow!("no stderr from Chromium"))?;
        let (tx, rx) = std_mpsc::channel();
        std::thread::Builder::new().name("eve-chromium-stderr".into()).spawn(move || {
            let mut sent = false;
            for line in std::io::BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if !sent && line.contains("DevTools listening on") {
                    if let Some(at) = line.find("ws://") {
                        let _ = tx.send(line[at..].trim().to_string());
                        sent = true;
                    }
                }
                // keep reading, so a full pipe never blocks Chromium
            }
        })?;
        let Ok(url) = rx.recv_timeout(Duration::from_secs(30)) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LaneError::err(
                "Error",
                "BrowserType.launch_persistent_context: Chromium did not start (no DevTools endpoint)",
            ));
        };
        let conn = match Connection::connect(&url, Duration::from_secs(30)) {
            Ok(c) => c,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        Ok(Launched { child: Mutex::new(Some(child)), conn })
    }

    /// Ask Chromium to quit, and end the process if it does not.
    fn end(&self) {
        if !self.conn.is_closed() {
            let _ = self.conn.call(None, "Browser.close", json!({}), Duration::from_secs(3));
        }
        self.conn.shut();
        if let Some(mut child) = self.child.lock().take() {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
    }
}

impl Drop for Launched {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The switches Playwright starts Chromium with (chromiumSwitches.ts).
pub const PLAYWRIGHT_SWITCHES: &[&str] = &[
    "--disable-field-trial-config",
    "--disable-background-networking",
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-back-forward-cache",
    "--disable-breakpad",
    "--disable-client-side-phishing-detection",
    "--disable-component-extensions-with-background-pages",
    "--disable-component-update",
    "--no-default-browser-check",
    "--disable-default-apps",
    "--disable-dev-shm-usage",
    "--disable-extensions",
    "--disable-features=ImprovedCookieControls,LazyFrameLoading,GlobalMediaControls,DestroyProfileOnBrowserClose,\
MediaRouter,DialMediaRouteProvider,AcceptCHFrame,AutoExpandDetailsElement,CertificateTransparencyComponentUpdater,\
AvoidUnnecessaryBeforeUnloadCheckSync,Translate,HttpsUpgrades,PaintHolding,ThirdPartyStoragePartitioning,LensOverlay,\
PlzDedicatedWorker",
    "--allow-pre-commit-input",
    "--disable-hang-monitor",
    "--disable-ipc-flooding-protection",
    "--disable-popup-blocking",
    "--disable-prompt-on-repost",
    "--disable-renderer-backgrounding",
    "--force-color-profile=srgb",
    "--metrics-recording-only",
    "--no-first-run",
    "--enable-automation",
    "--password-store=basic",
    "--use-mock-keychain",
    "--no-service-autorun",
    "--export-tagged-pdf",
    "--disable-search-engine-choice-screen",
    "--unsafely-disable-devtools-self-xss-warnings",
];

/// Playwright's additions for a headless launch.
pub const HEADLESS_SWITCHES: &[&str] = &[
    "--headless",
    "--hide-scrollbars",
    "--mute-audio",
    "--blink-settings=primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4",
];

/// Where Playwright keeps its browsers: PLAYWRIGHT_BROWSERS_PATH, else its per-user cache.
fn playwright_cache() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PLAYWRIGHT_BROWSERS_PATH") {
        if !p.is_empty() && p != "0" {
            return Some(PathBuf::from(p));
        }
    }
    let home = dirs::home_dir()?;
    if cfg!(target_os = "macos") {
        Some(home.join("Library/Caches/ms-playwright"))
    } else {
        Some(home.join(".cache/ms-playwright"))
    }
}

/// The `<prefix><revision>` directories under `root`, newest first.
fn newest(root: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut found: Vec<(u64, PathBuf)> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let rev = name.strip_prefix(prefix)?.parse::<u64>().ok()?;
            Some((rev, e.path()))
        })
        .collect();
    found.sort_by_key(|(rev, _)| std::cmp::Reverse(*rev));
    found.into_iter().map(|(_, p)| p).collect()
}

/// Every executable an app bundle or a platform directory under `dir` holds (`chrome-mac*/X.app/Contents/MacOS/*`,
/// `chrome-linux*/chrome`).
fn executables_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for sub in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let sub = sub.path();
        for app in std::fs::read_dir(&sub).into_iter().flatten().flatten() {
            let app = app.path();
            if app.extension().is_some_and(|e| e == "app") {
                let macos = app.join("Contents/MacOS");
                for exe in std::fs::read_dir(&macos).into_iter().flatten().flatten() {
                    out.push(exe.path());
                }
            }
        }
        for name in ["chrome", "chrome-headless-shell", "headless_shell"] {
            out.push(sub.join(name));
        }
    }
    out
}

/// The Chromium Playwright would run: its headless shell for a headless launch, its full Chromium (Chrome for
/// Testing) otherwise, else an installed Google Chrome or Chromium.
pub fn find_chromium(headless: bool) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(cache) = playwright_cache() {
        if headless {
            for dir in newest(&cache, "chromium_headless_shell-") {
                candidates.extend(executables_in(&dir));
            }
        }
        for dir in newest(&cache, "chromium-") {
            candidates.extend(executables_in(&dir));
        }
    }
    candidates.push(PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
    candidates.push(PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"));
    candidates.into_iter().find(|p| p.is_file())
}

// ---- the browser and its contexts --------------------------------------------------------------------------

/// A browser on one connection.
pub struct CdpBrowser {
    conn: Arc<Connection>,
    viewport: Option<(u32, u32)>,
    launched: Option<Arc<Launched>>,
    contexts: Mutex<Vec<Arc<CdpContext>>>,
}

impl CdpBrowser {
    fn new(conn: Arc<Connection>, viewport: Option<(u32, u32)>, launched: Option<Arc<Launched>>) -> Self {
        CdpBrowser { conn, viewport, launched, contexts: Mutex::new(Vec::new()) }
    }

    /// The context object for `id` (None: the default context), the same one every time.
    fn context_for(&self, id: Option<&str>) -> Arc<CdpContext> {
        let mut known = self.contexts.lock();
        if let Some(c) = known.iter().find(|c| c.id.as_deref() == id) {
            return c.clone();
        }
        let made = Arc::new(CdpContext {
            conn: self.conn.clone(),
            id: id.map(str::to_string),
            viewport: self.viewport,
            launched: self.launched.clone(),
        });
        known.push(made.clone());
        made
    }

    /// `Browser.close` on this connection (eve's own Chrome quits through its own port).
    pub fn close_browser(&self) -> anyhow::Result<()> {
        self.conn.call(None, "Browser.close", json!({}), Duration::from_secs(10))?;
        Ok(())
    }
}

/// The browser contexts the browser lists besides its default one.
fn explicit_contexts(conn: &Connection) -> Vec<String> {
    conn.call(None, "Target.getBrowserContexts", json!({}), CALL_TIMEOUT)
        .ok()
        .and_then(|r| r.get("browserContextIds").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

impl BrowserHandle for CdpBrowser {
    fn contexts(&self) -> anyhow::Result<Vec<Arc<dyn ContextHandle>>> {
        if self.conn.is_closed() {
            return Err(closed().into());
        }
        let mut out: Vec<Arc<dyn ContextHandle>> = vec![self.context_for(None)];
        for id in explicit_contexts(&self.conn) {
            out.push(self.context_for(Some(&id)));
        }
        Ok(out)
    }

    fn is_connected(&self) -> bool {
        !self.conn.is_closed()
    }
}

/// One browser context (a Chrome profile, or the default context).
pub struct CdpContext {
    conn: Arc<Connection>,
    id: Option<String>,
    viewport: Option<(u32, u32)>,
    launched: Option<Arc<Launched>>,
}

impl ContextHandle for CdpContext {
    fn pages(&self) -> anyhow::Result<Vec<Box<dyn PageHandle>>> {
        let targets = self.conn.call(None, "Target.getTargets", json!({}), CALL_TIMEOUT)?;
        let explicit: HashSet<String> =
            if self.id.is_none() { explicit_contexts(&self.conn).into_iter().collect() } else { HashSet::new() };
        let mut out: Vec<Box<dyn PageHandle>> = Vec::new();
        for t in targets.get("targetInfos").and_then(Value::as_array).cloned().unwrap_or_default() {
            if t.get("type").and_then(Value::as_str) != Some("page") {
                continue;
            }
            let ctx = t.get("browserContextId").and_then(Value::as_str).unwrap_or("");
            let mine = match &self.id {
                Some(id) => ctx == id,
                None => !explicit.contains(ctx),
            };
            if mine {
                let target = t.get("targetId").and_then(Value::as_str).unwrap_or("");
                out.push(Box::new(CdpPage::attach(self.conn.clone(), target, self.viewport)?));
            }
        }
        Ok(out)
    }

    fn new_page(&self) -> anyhow::Result<Box<dyn PageHandle>> {
        let mut params = json!({"url": "about:blank"});
        if let Some(id) = &self.id {
            params["browserContextId"] = json!(id);
        }
        let made = self.conn.call(None, "Target.createTarget", params, CALL_TIMEOUT)?;
        let target = made.get("targetId").and_then(Value::as_str).unwrap_or("").to_string();
        Ok(Box::new(CdpPage::attach(self.conn.clone(), &target, self.viewport)?))
    }

    fn cookies(&self, urls: &[&str]) -> anyhow::Result<Vec<Cookie>> {
        let mut params = json!({});
        if let Some(id) = &self.id {
            params["browserContextId"] = json!(id);
        }
        let r = self.conn.call(None, "Storage.getCookies", params, CALL_TIMEOUT)?;
        let all = r.get("cookies").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(filter_cookies(&all, urls))
    }

    fn close(&self) -> anyhow::Result<()> {
        match &self.launched {
            // a persistent context is its browser: closing it ends the browser
            Some(launched) => launched.end(),
            None => {
                if let Some(id) = &self.id {
                    self.conn.call(None, "Target.disposeBrowserContext", json!({"browserContextId": id}), CALL_TIMEOUT)?;
                }
            }
        }
        Ok(())
    }
}

/// Playwright's `filterCookies`: the cookies a request to any of `urls` would carry (domain, path, and a
/// secure cookie only over https or to a local host).
pub fn filter_cookies(cookies: &[Value], urls: &[&str]) -> Vec<Cookie> {
    let parsed: Vec<url::Url> = urls.iter().filter_map(|u| url::Url::parse(u).ok()).collect();
    let local = |host: &str| host == "localhost" || host.ends_with(".localhost") || host == "127.0.0.1" || host == "[::1]";
    cookies
        .iter()
        .filter(|c| {
            if parsed.is_empty() {
                return true;
            }
            let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let mut domain = s("domain");
            if !domain.starts_with('.') {
                domain = format!(".{domain}");
            }
            let secure = c.get("secure").and_then(Value::as_bool).unwrap_or(false);
            parsed.iter().any(|u| {
                let host = u.host_str().unwrap_or("");
                format!(".{host}").ends_with(&domain)
                    && u.path().starts_with(&s("path"))
                    && !(u.scheme() != "https" && !local(host) && secure)
            })
        })
        .map(|c| Cookie {
            name: c.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            value: c.get("value").and_then(Value::as_str).unwrap_or("").to_string(),
        })
        .collect()
}

// ---- the page ----------------------------------------------------------------------------------------------

/// One tab, on its own flattened session.
pub struct CdpPage {
    conn: Arc<Connection>,
    session: String,
    target: String,
}

/// `(js)(arg)` when `js` is a function, else the value of `js`: what `page.evaluate(js, arg)` runs.
fn call_expr(js: &str, arg: Option<&Value>) -> String {
    let arg = arg.map(Value::to_string).unwrap_or_else(|| "undefined".into());
    format!("(() => {{ const __f = ({js}\n); return typeof __f === 'function' ? __f({arg}) : __f; }})()")
}

/// Playwright's message for a thrown value: its description, else its value, else the exception text.
fn exception_message(details: &Value) -> String {
    if let Some(exc) = details.get("exception") {
        if let Some(d) = exc.get("description").and_then(Value::as_str) {
            return d.to_string();
        }
        return match exc.get("value") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => {
                exc.get("unserializableValue").and_then(Value::as_str).unwrap_or("undefined").to_string()
            }
            Some(v) => v.to_string(),
        };
    }
    details.get("text").and_then(Value::as_str).unwrap_or("").to_string()
}

/// A read that failed because the document was being replaced (a navigation), which the next read survives.
fn navigation_race(e: &anyhow::Error) -> bool {
    let text = eve_desktop_lanes::error_message(e).to_lowercase();
    text.contains("execution context was destroyed") || text.contains("cannot find context with specified id")
}

/// The checks Playwright makes before an action, on the first element `a.sel` matches.
const ACT_JS: &str = r#"(a) => {
  const el = document.querySelector(a.sel);
  if (!el || !el.isConnected) return { s: "missing" };
  if (a.op === "attached") return { s: "ok" };
  const box = el.getBoundingClientRect(), st = getComputedStyle(el);
  const visible = box.width > 0 && box.height > 0 && st.visibility !== "hidden";
  const aria = el.closest("[aria-disabled]");
  const disabled = el.matches(":disabled") || (!!aria && aria.getAttribute("aria-disabled") === "true");
  const scroll = () => { const r = el.getBoundingClientRect();
    const inside = r.top >= 0 && r.left >= 0 && r.bottom <= innerHeight && r.right <= innerWidth;
    if (inside) return;
    if (el.scrollIntoViewIfNeeded) el.scrollIntoViewIfNeeded(true);
    else el.scrollIntoView({ block: "center", inline: "center" }); };
  const fire = (...names) => { for (const n of names) el.dispatchEvent(new Event(n, { bubbles: true, composed: n === "input" })); };
  if (!visible) return { s: "not visible" };
  if (a.op === "scroll") { scroll(); return { s: "ok" }; }
  if (disabled) return { s: "not enabled" };
  if (a.op === "point") {
    scroll();
    const r = el.getBoundingClientRect();
    const x0 = Math.max(r.left, 0), x1 = Math.min(r.right, innerWidth), y0 = Math.max(r.top, 0), y1 = Math.min(r.bottom, innerHeight);
    if (x1 <= x0 || y1 <= y0) return { s: "outside of the viewport" };
    return { s: "ok", x: (x0 + x1) / 2, y: (y0 + y1) / 2, rect: [r.left, r.top, r.width, r.height].join(",") };
  }
  if (a.op === "hit") {
    const hit = document.elementFromPoint(a.x, a.y);
    if (!hit || hit === el || el.contains(hit)) return { s: "ok" };
    const lbl = hit.closest("label");
    if (lbl && (lbl.contains(el) || (el.id && lbl.htmlFor === el.id))) return { s: "ok" };
    return { s: "intercepted" };
  }
  if (a.op === "fill") {
    const tag = el.tagName;
    const editable = tag === "INPUT" || tag === "TEXTAREA" || el.isContentEditable;
    if (!editable) return { error: "Element is not an <input>, <textarea> or [contenteditable] element" };
    if (el.readOnly) return { s: "not editable" };
    if (tag === "INPUT") {
      const type = (el.getAttribute("type") || "text").toLowerCase();
      if (["checkbox", "radio", "file", "button", "submit", "reset", "image", "hidden"].includes(type))
        return { error: `Input of type "${type}" cannot be filled` };
      if (["color", "date", "time", "datetime-local", "month", "range", "week"].includes(type)) {
        el.focus(); el.value = a.value;
        if (el.value !== a.value) return { error: "Malformed value" };
        fire("input", "change"); return { s: "done" };
      }
      if (type === "number" && a.value.trim() !== "" && isNaN(Number(a.value)))
        return { error: "Cannot type text into input[type=number]" };
      el.focus(); el.select(); return { s: "type" };
    }
    if (tag === "TEXTAREA") { el.focus(); el.select(); return { s: "type" }; }
    el.focus();
    const range = document.createRange(); range.selectNodeContents(el);
    const sel = getSelection(); sel.removeAllRanges(); sel.addRange(range); return { s: "type" };
  }
  if (a.op === "select") {
    if (el.tagName !== "SELECT") return { error: "Element is not a <select> element" };
    const options = Array.from(el.options);
    const chosen = a.value !== null && a.value !== undefined ? options.find((o) => o.value === a.value)
                                                            : options.find((o) => o.label === a.label);
    if (!chosen) return { s: "no such option" };
    for (const o of options) o.selected = o === chosen;
    fire("input", "change");
    return { s: "done" };
  }
  return { error: "unknown action " + a.op };
}"#;

/// Paint every element `a.sel` matches in `a.color`, over everything, until [`UNMASK_JS`].
const MASK_JS: &str = r#"(a) => {
  const host = document.createElement("div");
  host.setAttribute("data-eve-mask", "");
  host.style.cssText = "position:fixed;left:0;top:0;width:0;height:0;z-index:2147483647;pointer-events:none;";
  for (const e of document.querySelectorAll(a.sel)) {
    const r = e.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue;
    const d = document.createElement("div");
    d.style.cssText = `position:fixed;left:${r.left}px;top:${r.top}px;width:${r.width}px;height:${r.height}px;` +
                      `background:${a.color};pointer-events:none;`;
    host.appendChild(d);
  }
  (document.body || document.documentElement).appendChild(host);
  return true;
}"#;
const UNMASK_JS: &str = r#"() => { for (const e of document.querySelectorAll("[data-eve-mask]")) e.remove(); return true; }"#;

/// One key of the US layout: what `Input.dispatchKeyEvent` is told.
struct Key {
    key: String,
    code: &'static str,
    key_code: i64,
    text: Option<String>,
}

/// The key a character is typed with on Playwright's US layout, None for a character it inserts as text.
fn key_for_char(c: char) -> Option<Key> {
    let shifted_digits = [')', '!', '@', '#', '$', '%', '^', '&', '*', '('];
    let digit_codes = ["Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8", "Digit9"];
    let letter_codes = [
        "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK", "KeyL", "KeyM", "KeyN",
        "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV", "KeyW", "KeyX", "KeyY", "KeyZ",
    ];
    let k = |code: &'static str, key_code: i64| Some(Key { key: c.to_string(), code, key_code, text: Some(c.to_string()) });
    match c {
        'a'..='z' => {
            let i = (c as u8 - b'a') as usize;
            k(letter_codes[i], 65 + i as i64)
        }
        'A'..='Z' => {
            let i = (c as u8 - b'A') as usize;
            k(letter_codes[i], 65 + i as i64)
        }
        '0'..='9' => {
            let i = (c as u8 - b'0') as usize;
            k(digit_codes[i], 48 + i as i64)
        }
        c if shifted_digits.contains(&c) => {
            let i = shifted_digits.iter().position(|&d| d == c).unwrap_or(0);
            k(digit_codes[i], 48 + i as i64)
        }
        ' ' => k("Space", 32),
        '-' | '_' => k("Minus", 189),
        '=' | '+' => k("Equal", 187),
        '[' | '{' => k("BracketLeft", 219),
        ']' | '}' => k("BracketRight", 221),
        '\\' | '|' => k("Backslash", 220),
        ';' | ':' => k("Semicolon", 186),
        '\'' | '"' => k("Quote", 222),
        ',' | '<' => k("Comma", 188),
        '.' | '>' => k("Period", 190),
        '/' | '?' => k("Slash", 191),
        '`' | '~' => k("Backquote", 192),
        '\n' | '\r' => Some(Key { key: "Enter".into(), code: "Enter", key_code: 13, text: Some("\r".into()) }),
        '\t' => Some(Key { key: "Tab".into(), code: "Tab", key_code: 9, text: None }),
        _ => None,
    }
}

/// A named key as `keyboard.press` takes it ("Enter", "ArrowUp", " ", a single character).
fn named_key(name: &str) -> Option<Key> {
    let k = |key: &str, code: &'static str, key_code: i64, text: Option<&str>| {
        Some(Key { key: key.into(), code, key_code, text: text.map(str::to_string) })
    };
    match name {
        "Enter" => k("Enter", "Enter", 13, Some("\r")),
        "Escape" => k("Escape", "Escape", 27, None),
        "Tab" => k("Tab", "Tab", 9, None),
        "Backspace" => k("Backspace", "Backspace", 8, None),
        "Delete" => k("Delete", "Delete", 46, None),
        "ArrowUp" => k("ArrowUp", "ArrowUp", 38, None),
        "ArrowDown" => k("ArrowDown", "ArrowDown", 40, None),
        "ArrowLeft" => k("ArrowLeft", "ArrowLeft", 37, None),
        "ArrowRight" => k("ArrowRight", "ArrowRight", 39, None),
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => key_for_char(c),
                _ => None,
            }
        }
    }
}

/// The lifecycle event a Playwright load state waits for.
fn lifecycle_name(state: &str) -> &'static str {
    match state {
        "domcontentloaded" => "DOMContentLoaded",
        "networkidle" => "networkIdle",
        "commit" => "commit",
        _ => "load",
    }
}

impl CdpPage {
    /// Attach to `target` on its own session and learn its main frame; `viewport` is the persistent context's.
    pub fn attach(conn: Arc<Connection>, target: &str, viewport: Option<(u32, u32)>) -> anyhow::Result<CdpPage> {
        let attached =
            conn.call(None, "Target.attachToTarget", json!({"targetId": target, "flatten": true}), CALL_TIMEOUT)?;
        let session = attached
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| LaneError::err("Error", "the page could not be attached"))?
            .to_string();
        conn.state.lock().frames.entry(session.clone()).or_default();
        let page = CdpPage { conn, session, target: target.to_string() };
        page.call("Page.enable", json!({}))?;
        page.call("Page.setLifecycleEventsEnabled", json!({"enabled": true}))?;
        let tree = page.call("Page.getFrameTree", json!({}))?;
        let frame = tree.pointer("/frameTree/frame").cloned().unwrap_or(Value::Null);
        let text = |k: &str| frame.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let loader = {
            let mut st = page.conn.state.lock();
            let f = st.frames.entry(page.session.clone()).or_default();
            if f.main.is_empty() {
                f.main = text("id");
                f.url = format!("{}{}", text("url"), text("urlFragment"));
                f.loader = text("loaderId");
            }
            f.loader.clone()
        };
        if let Some((w, h)) = viewport {
            page.call(
                "Emulation.setDeviceMetricsOverride",
                json!({"width": w, "height": h, "deviceScaleFactor": 1, "mobile": false,
                       "screenWidth": w, "screenHeight": h}),
            )?;
        }
        // A document that had loaded before this session attached fired its events before anyone listened.
        let ready = page.evaluate("() => document.readyState", None).unwrap_or(Value::Null);
        let reached: &[&str] = match ready.as_str() {
            Some("interactive") => &["commit", "DOMContentLoaded"],
            Some("complete") => &["commit", "DOMContentLoaded", "load", "networkAlmostIdle", "networkIdle"],
            _ => &[],
        };
        if !reached.is_empty() {
            let mut st = page.conn.state.lock();
            let f = st.frames.entry(page.session.clone()).or_default();
            let fired = f.lifecycle.entry(loader).or_default();
            for name in reached {
                fired.insert((*name).to_string());
            }
        }
        Ok(page)
    }

    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        Ok(self.conn.call(Some(&self.session), method, params, CALL_TIMEOUT)?)
    }

    /// The tab's target id (what `Target.attachToTarget` takes).
    pub fn target_id(&self) -> &str {
        &self.target
    }

    /// The browser connection this tab is on.
    pub fn connection(&self) -> &Arc<Connection> {
        &self.conn
    }

    fn evaluate_expression(&self, api: &str, expression: &str) -> anyhow::Result<Value> {
        let r = self.call(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true, "awaitPromise": true, "userGesture": true}),
        )?;
        if let Some(details) = r.get("exceptionDetails") {
            return Err(LaneError::err("Error", format!("{api}: {}", exception_message(details))));
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    fn act(&self, args: Value) -> anyhow::Result<Value> {
        self.evaluate(ACT_JS, Some(&args))
    }

    fn act_error(api: &str, r: &Value) -> Option<anyhow::Error> {
        r.get("error").and_then(Value::as_str).map(|e| LaneError::err("Error", format!("{api}: Error: {e}")))
    }

    /// Retry `step` (Playwright's waits; a navigation race is one more try) until it says done or `ms` pass.
    fn retry(&self, api: &str, ms: u64, mut step: impl FnMut() -> anyhow::Result<bool>) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_millis(ms);
        let mut n = 0usize;
        loop {
            let wait = RETRY_MS[n.min(RETRY_MS.len() - 1)];
            n += 1;
            if wait > 0 {
                let left = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(Duration::from_millis(wait).min(left));
            }
            match step() {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(e) if navigation_race(&e) => {}
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                return Err(timeout_error(api, ms));
            }
        }
    }

    fn dispatch_key(&self, key: &Key, delay_ms: u64) -> anyhow::Result<()> {
        let mut down = json!({"type": if key.text.is_some() { "keyDown" } else { "rawKeyDown" }, "modifiers": 0,
                              "windowsVirtualKeyCode": key.key_code, "code": key.code, "key": key.key,
                              "autoRepeat": false, "location": 0, "isKeypad": false});
        if let Some(text) = &key.text {
            down["text"] = json!(text);
            down["unmodifiedText"] = json!(text);
        }
        self.call("Input.dispatchKeyEvent", down)?;
        if delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
        self.call(
            "Input.dispatchKeyEvent",
            json!({"type": "keyUp", "modifiers": 0, "windowsVirtualKeyCode": key.key_code, "code": key.code,
                   "key": key.key, "location": 0}),
        )?;
        Ok(())
    }

    fn wait_fired(&self, api: &str, loader: Option<&str>, name: &str, timeout_ms: u64) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if self.conn.fired(&self.session, loader, name) {
                return Ok(());
            }
            if self.conn.is_closed() || self.conn.state.lock().gone.contains(&self.session) {
                return Err(closed().into());
            }
            if Instant::now() >= deadline {
                return Err(timeout_error(api, timeout_ms));
            }
            std::thread::sleep(POLL);
        }
    }
}

impl PageHandle for CdpPage {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn evaluate(&self, script: &str, arg: Option<&Value>) -> anyhow::Result<Value> {
        self.evaluate_expression("Page.evaluate", &call_expr(script, arg))
    }

    fn title(&self) -> anyhow::Result<String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.evaluate_expression("Page.title", "document.title") {
                Ok(v) => return Ok(v.as_str().unwrap_or("").to_string()),
                Err(e) if navigation_race(&e) && Instant::now() < deadline => std::thread::sleep(POLL),
                Err(e) => return Err(e),
            }
        }
    }

    fn url(&self) -> String {
        self.conn.page_url(&self.session)
    }

    fn inner_text(&self, selector: &str, timeout_ms: u64) -> anyhow::Result<String> {
        let mut text = String::new();
        self.retry("Page.inner_text", timeout_ms, || {
            let r = self.evaluate(
                "(s) => { const e = document.querySelector(s); return e ? { t: e.innerText } : null; }",
                Some(&json!(selector)),
            )?;
            match r.get("t") {
                Some(t) => {
                    text = t.as_str().unwrap_or("").to_string();
                    Ok(true)
                }
                None => Ok(false),
            }
        })?;
        Ok(text)
    }

    fn count(&self, selector: &str) -> anyhow::Result<usize> {
        let n = self.evaluate("(s) => document.querySelectorAll(s).length", Some(&json!(selector)))?;
        Ok(n.as_u64().unwrap_or(0) as usize)
    }

    fn scroll_into_view_if_needed(&self, selector: &str, timeout_ms: u64) -> anyhow::Result<()> {
        self.retry("Locator.scroll_into_view_if_needed", timeout_ms, || {
            Ok(self.act(json!({"sel": selector, "op": "scroll"}))?["s"] == "ok")
        })
    }

    fn click(&self, selector: &str, timeout_ms: u64) -> anyhow::Result<()> {
        let api = "Locator.click";
        self.retry(api, timeout_ms, || {
            let first = self.act(json!({"sel": selector, "op": "point"}))?;
            if let Some(e) = Self::act_error(api, &first) {
                return Err(e);
            }
            if first["s"] != "ok" {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(16));
            let again = self.act(json!({"sel": selector, "op": "point"}))?;
            if again["s"] != "ok" || again["rect"] != first["rect"] {
                return Ok(false); // still moving
            }
            let (x, y) = (again["x"].as_f64().unwrap_or(0.0), again["y"].as_f64().unwrap_or(0.0));
            let hit = self.act(json!({"sel": selector, "op": "hit", "x": x, "y": y}))?;
            if hit["s"] != "ok" {
                return Ok(false);
            }
            for (kind, buttons) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 0)] {
                let mut p = json!({"type": kind, "x": x, "y": y, "button": "left", "buttons": buttons,
                                   "pointerType": "mouse", "modifiers": 0});
                if kind == "mouseMoved" {
                    p["button"] = json!("none");
                } else {
                    p["clickCount"] = json!(1);
                }
                self.call("Input.dispatchMouseEvent", p)?;
            }
            Ok(true)
        })
    }

    fn fill(&self, selector: &str, value: &str, timeout_ms: u64) -> anyhow::Result<()> {
        let api = "Locator.fill";
        self.retry(api, timeout_ms, || {
            let r = self.act(json!({"sel": selector, "op": "fill", "value": value}))?;
            if let Some(e) = Self::act_error(api, &r) {
                return Err(e);
            }
            match r["s"].as_str() {
                Some("done") => Ok(true),
                Some("type") => {
                    if value.is_empty() {
                        if let Some(key) = named_key("Delete") {
                            self.dispatch_key(&key, 0)?;
                        }
                    } else {
                        self.call("Input.insertText", json!({"text": value}))?;
                    }
                    Ok(true)
                }
                _ => Ok(false),
            }
        })
    }

    fn select_option(&self, selector: &str, choice: SelectBy<'_>, timeout_ms: u64) -> anyhow::Result<()> {
        let api = "Locator.select_option";
        let (value, label) = match choice {
            SelectBy::Value(v) => (json!(v), Value::Null),
            SelectBy::Label(l) => (Value::Null, json!(l)),
        };
        self.retry(api, timeout_ms, || {
            let r = self.act(json!({"sel": selector, "op": "select", "value": value, "label": label}))?;
            if let Some(e) = Self::act_error(api, &r) {
                return Err(e);
            }
            Ok(r["s"] == "done")
        })
    }

    fn evaluate_on(&self, selector: &str, script: &str) -> anyhow::Result<Value> {
        let api = "Locator.evaluate";
        self.retry(api, ATTACH_TIMEOUT_MS, || Ok(self.act(json!({"sel": selector, "op": "attached"}))?["s"] == "ok"))?;
        let expr = format!(
            "(() => {{ const el = document.querySelector({}); if (!el) throw new Error('the element is gone'); return ({script}\n)(el); }})()",
            json!(selector)
        );
        self.evaluate_expression(api, &expr)
    }

    fn keyboard_type(&self, text: &str, delay_ms: u64) -> anyhow::Result<()> {
        for c in text.chars() {
            match key_for_char(c) {
                Some(key) => self.dispatch_key(&key, delay_ms)?,
                None => {
                    if delay_ms > 0 {
                        std::thread::sleep(Duration::from_millis(delay_ms));
                    }
                    self.call("Input.insertText", json!({"text": c.to_string()}))?;
                }
            }
        }
        Ok(())
    }

    fn keyboard_press(&self, key: &str) -> anyhow::Result<()> {
        let k = named_key(key).ok_or_else(|| LaneError::err("Error", format!("Keyboard.press: Unknown key: \"{key}\"")))?;
        self.dispatch_key(&k, 0)
    }

    fn wait_for_timeout(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn wait_for_load_state(&self, state: &str, timeout_ms: u64) -> anyhow::Result<()> {
        self.wait_fired("Page.wait_for_load_state", None, lifecycle_name(state), timeout_ms)
    }

    fn goto(&self, url: &str, wait_until: &str, timeout_ms: u64) -> anyhow::Result<()> {
        let api = "Page.goto";
        let started = Instant::now();
        let r = self
            .conn
            .call(Some(&self.session), "Page.navigate", json!({"url": url}), Duration::from_millis(timeout_ms.max(1)))
            .map_err(|e| {
                if e.kind == "TimeoutError" {
                    timeout_error(api, timeout_ms)
                } else {
                    anyhow::Error::new(e)
                }
            })?;
        if let Some(err) = r.get("errorText").and_then(Value::as_str).filter(|e| !e.is_empty()) {
            return Err(LaneError::err("Error", format!("{api}: {err} at {url}")));
        }
        let Some(loader) = r.get("loaderId").and_then(Value::as_str).filter(|l| !l.is_empty()) else {
            return Ok(()); // a same-document navigation: nothing to load
        };
        let left = timeout_ms.saturating_sub(started.elapsed().as_millis() as u64);
        self.wait_fired(api, Some(loader), lifecycle_name(wait_until), left).map_err(|e| {
            if eve_desktop_lanes::error_kind(&e) == "TimeoutError" {
                timeout_error(api, timeout_ms)
            } else {
                e
            }
        })
    }

    fn bring_to_front(&self) -> anyhow::Result<()> {
        self.call("Page.bringToFront", json!({}))?;
        Ok(())
    }

    fn close(&self) -> anyhow::Result<()> {
        let r = self.conn.call(None, "Target.closeTarget", json!({"targetId": self.target}), CALL_TIMEOUT);
        self.conn.gone(&self.session);
        r?;
        Ok(())
    }

    fn screenshot_jpeg(&self, quality: u8, mask: Option<(&str, &str)>) -> anyhow::Result<Vec<u8>> {
        if let Some((selector, color)) = mask {
            self.evaluate(MASK_JS, Some(&json!({"sel": selector, "color": color})))?;
        }
        let shot = self.call(
            "Page.captureScreenshot",
            json!({"format": "jpeg", "quality": quality, "captureBeyondViewport": false}),
        );
        if mask.is_some() {
            let _ = self.evaluate(UNMASK_JS, None);
        }
        let data = shot?.get("data").and_then(Value::as_str).unwrap_or("").to_string();
        Ok(base64::engine::general_purpose::STANDARD.decode(data)?)
    }
}

/// Connect to `endpoint` and send `Browser.close` (quit_own_chrome); the connection drops as Chrome quits.
pub fn close_browser_at(endpoint: &str, timeout: Duration) -> anyhow::Result<()> {
    let conn = Connection::connect(endpoint, timeout)?;
    let browser = CdpBrowser::new(conn, None, None);
    let _ = browser.close_browser();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_calls_a_function_with_its_argument_or_none() {
        assert!(call_expr("(n) => n + 1", Some(&json!(2))).contains("__f(2)"));
        assert!(call_expr("() => 1", None).contains("__f(undefined)"));
    }

    #[test]
    fn the_us_layout_types_ascii_and_inserts_the_rest() {
        let a = key_for_char('a').expect("a");
        assert_eq!((a.code, a.key_code, a.text.as_deref()), ("KeyA", 65, Some("a")));
        let bang = key_for_char('!').expect("!");
        assert_eq!((bang.code, bang.key_code), ("Digit1", 49));
        assert!(key_for_char('é').is_none());
        let enter = named_key("Enter").expect("Enter");
        assert_eq!((enter.key_code, enter.text.as_deref()), (13, Some("\r")));
        assert!(named_key(" ").is_some() && named_key("F13").is_none());
    }

    #[test]
    fn cookies_are_filtered_as_playwright_filters_them() {
        let cookies = vec![
            json!({"name": "SID", "value": "a", "domain": ".google.com", "path": "/", "secure": true}),
            json!({"name": "HOST", "value": "b", "domain": "accounts.google.com", "path": "/", "secure": false}),
            json!({"name": "OTHER", "value": "c", "domain": "example.com", "path": "/", "secure": false}),
            json!({"name": "PATH", "value": "d", "domain": ".google.com", "path": "/mail", "secure": false}),
        ];
        let got = filter_cookies(&cookies, &["https://accounts.google.com/ListAccounts?gpsia=1"]);
        let names: Vec<&str> = got.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["SID", "HOST"]);
        let plain = filter_cookies(&cookies, &["http://accounts.google.com/"]);
        assert_eq!(plain.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["HOST"]);
    }
}
