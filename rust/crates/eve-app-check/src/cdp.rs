//! Headless Chromium over the Chrome DevTools Protocol: the part of Playwright that app_check.py uses.
//!
//! Python drives Chromium through Playwright. Here the check talks CDP itself, over the browser's WebSocket,
//! with one flattened session per page: a phone-sized context (`Target.createBrowserContext` plus device
//! emulation), init scripts, a binding for the functions the page calls back into the checker, request
//! interception (`Fetch`), and the locator actions the check and jev_verify use (click, fill, select, evaluate
//! on an element) with Playwright's waiting: the element must be there, visible, enabled and, for a click, not
//! covered, and each action is bounded by its timeout. The browser is the one Playwright installs
//! (`chromium_headless_shell` under its cache), launched with Playwright's own default switches.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use eve_jev_verify::PageError;
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

/// What Playwright says of a page, context or browser that is gone.
pub const CLOSED: &str = "Target page, context or browser has been closed";
/// What Playwright says of a page whose renderer died.
pub const CRASHED: &str = "Target crashed";

/// One protocol event of a page's session.
#[derive(Debug, Clone)]
pub struct Event {
    pub method: String,
    pub params: Value,
}

type Reply = oneshot::Sender<Result<Value, PageError>>;

struct Slot {
    tx: Option<mpsc::UnboundedSender<Event>>,
    rx: Option<mpsc::UnboundedReceiver<Event>>,
}

impl Slot {
    fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { tx: Some(tx), rx: Some(rx) }
    }
}

#[derive(Default)]
struct State {
    pending: HashMap<u64, (Option<String>, Reply)>,
    sessions: HashMap<String, Slot>,
    /// session id → why every call on it now fails (crashed, detached)
    gone: HashMap<String, String>,
    /// target id → session id, for Target.targetCrashed
    targets: HashMap<String, String>,
    closed: Option<String>,
}

/// One WebSocket to the browser, shared by every page: calls by id, events routed by session.
pub struct Connection {
    out: mpsc::UnboundedSender<String>,
    state: Mutex<State>,
    next: AtomicU64,
}

/// Removes a call's reply slot when the caller stops waiting (a timeout), so a page that never answers does
/// not keep it forever.
struct PendingGuard<'a> {
    conn: &'a Connection,
    id: u64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.conn.state.lock().pending.remove(&self.id);
    }
}

impl Connection {
    async fn connect(url: &str) -> anyhow::Result<Arc<Connection>> {
        let config = WebSocketConfig::default().max_message_size(Some(512 << 20)).max_frame_size(Some(512 << 20));
        let (ws, _) = tokio_tungstenite::connect_async_with_config(url, Some(config), true).await?;
        let (mut sink, mut stream) = ws.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
        let conn = Arc::new(Connection { out, state: Mutex::new(State::default()), next: AtomicU64::new(1) });
        tokio::spawn(async move {
            while let Some(text) = out_rx.recv().await {
                if sink.send(Message::text(text)).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });
        let reader = conn.clone();
        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                let text = match msg {
                    Ok(Message::Text(t)) => t.as_str().to_string(),
                    Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                };
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    reader.dispatch(v);
                }
            }
            reader.shut("Browser has been closed");
        });
        Ok(conn)
    }

    fn shut(&self, why: &str) {
        let pending: Vec<Reply> = {
            let mut st = self.state.lock();
            st.closed.get_or_insert_with(|| why.to_string());
            st.sessions.clear();
            st.pending.drain().map(|(_, (_, r))| r).collect()
        };
        for reply in pending {
            let _ = reply.send(Err(PageError::new("TargetClosedError", CLOSED)));
        }
    }

    /// Every call on `session` fails from now on with `why`; the calls waiting on it fail now.
    fn gone(&self, session: &str, why: &str, detach: bool) {
        let pending: Vec<Reply> = {
            let mut st = self.state.lock();
            if !detach && (!st.gone.contains_key(session) || why == CRASHED) {
                st.gone.insert(session.to_string(), why.to_string());
            }
            if detach {
                // the receiver keeps what was already sent; nothing more comes. A session nobody listens to is
                // forgotten, so a long-running daemon does not keep one per page it ever opened.
                let taken = match st.sessions.get_mut(session) {
                    Some(slot) => {
                        slot.tx = None;
                        slot.rx.is_none()
                    }
                    None => false,
                };
                if taken {
                    st.sessions.remove(session);
                }
                st.gone.remove(session);
                st.targets.retain(|_, s| s != session);
            }
            let ids: Vec<u64> =
                st.pending.iter().filter(|(_, (s, _))| s.as_deref() == Some(session)).map(|(id, _)| *id).collect();
            ids.into_iter().filter_map(|id| st.pending.remove(&id)).map(|(_, r)| r).collect()
        };
        let kind = if why == CRASHED { "Error" } else { "TargetClosedError" };
        for reply in pending {
            let _ = reply.send(Err(PageError::new(kind, why)));
        }
    }

    fn dispatch(&self, v: Value) {
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            let entry = self.state.lock().pending.remove(&id);
            if let Some((_, reply)) = entry {
                let result = match v.get("error") {
                    Some(e) => Err(PageError::new(
                        "Error",
                        e.get("message").and_then(Value::as_str).unwrap_or("Protocol error").to_string(),
                    )),
                    None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = reply.send(result);
            }
            return;
        }
        let method = v.get("method").and_then(Value::as_str).unwrap_or("").to_string();
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        let session = v.get("sessionId").and_then(Value::as_str).map(str::to_string);
        match (session.as_deref(), method.as_str()) {
            (_, "Target.attachedToTarget") => {
                if let (Some(sid), Some(tid)) = (
                    params.get("sessionId").and_then(Value::as_str),
                    params.pointer("/targetInfo/targetId").and_then(Value::as_str),
                ) {
                    let mut st = self.state.lock();
                    st.targets.insert(tid.to_string(), sid.to_string());
                    st.sessions.entry(sid.to_string()).or_insert_with(Slot::new);
                }
            }
            (None, "Target.detachedFromTarget") => {
                if let Some(sid) = params.get("sessionId").and_then(Value::as_str) {
                    self.gone(sid, CLOSED, true);
                }
            }
            (None, "Target.targetCrashed") => {
                let sid = params
                    .get("targetId")
                    .and_then(Value::as_str)
                    .and_then(|t| self.state.lock().targets.get(t).cloned());
                if let Some(sid) = sid {
                    self.forward(&sid, Event { method: "Inspector.targetCrashed".into(), params: json!({}) });
                    self.gone(&sid, CRASHED, false);
                }
            }
            (Some(sid), "Inspector.targetCrashed") => {
                self.forward(sid, Event { method: method.clone(), params: params.clone() });
                self.gone(sid, CRASHED, false);
                return;
            }
            _ => {}
        }
        if let Some(sid) = session {
            self.forward(&sid, Event { method, params });
        }
    }

    fn forward(&self, session: &str, event: Event) {
        let mut st = self.state.lock();
        if st.closed.is_some() {
            return;
        }
        let slot = st.sessions.entry(session.to_string()).or_insert_with(Slot::new);
        if let Some(tx) = &slot.tx {
            let _ = tx.send(event);
        }
    }

    /// The events of `session`, once.
    fn take_events(&self, session: &str) -> Option<mpsc::UnboundedReceiver<Event>> {
        self.state.lock().sessions.entry(session.to_string()).or_insert_with(Slot::new).rx.take()
    }

    /// One protocol call; its result, or the browser's error as a PageError.
    pub async fn call(&self, session: Option<&str>, method: &str, params: Value) -> Result<Value, PageError> {
        let (tx, rx) = oneshot::channel();
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        {
            let mut st = self.state.lock();
            if st.closed.is_some() {
                return Err(PageError::new("TargetClosedError", CLOSED));
            }
            if let Some(why) = session.and_then(|s| st.gone.get(s)) {
                let kind = if why == CRASHED { "Error" } else { "TargetClosedError" };
                return Err(PageError::new(kind, why.clone()));
            }
            st.pending.insert(id, (session.map(str::to_string), tx));
        }
        let _guard = PendingGuard { conn: self, id };
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(sid) = session {
            msg["sessionId"] = json!(sid);
        }
        if self.out.send(msg.to_string()).is_err() {
            return Err(PageError::new("TargetClosedError", CLOSED));
        }
        match rx.await {
            Ok(r) => r,
            Err(_) => Err(PageError::new("TargetClosedError", CLOSED)),
        }
    }

    /// A call whose answer nobody waits for (closing a context from a destructor).
    pub fn send_nowait(&self, session: Option<&str>, method: &str, params: Value) {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(sid) = session {
            msg["sessionId"] = json!(sid);
        }
        let _ = self.out.send(msg.to_string());
    }

    fn is_closed(&self) -> bool {
        self.state.lock().closed.is_some()
    }
}

// ---- the browser -------------------------------------------------------------------------------------------

/// The switches Playwright starts Chromium with (chromiumSwitches.ts and its headless additions), plus site
/// isolation off. Request interception covers a dedicated worker's requests only with PlzDedicatedWorker off,
/// and screenshots are in sRGB, as Playwright's are.
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
PlzDedicatedWorker,IsolateOrigins,site-per-process",
    // every frame in the page's own process, so the page's router sees an iframe's requests too (Playwright
    // follows out-of-process frames; one session per page is simpler and fences the same)
    "--disable-site-isolation-trials",
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
    "--headless",
    "--hide-scrollbars",
    "--mute-audio",
    "--blink-settings=primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4",
    "--no-startup-window",
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
    } else if cfg!(target_os = "windows") {
        dirs::data_local_dir().map(|d| d.join("ms-playwright"))
    } else {
        Some(home.join(".cache/ms-playwright"))
    }
}

/// The newest `<prefix><revision>` directory under `root`.
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

/// The Chromium to run: Playwright's headless shell (what `chromium.launch(headless=True)` runs), else its
/// full Chromium, else an installed Chrome or Chromium.
pub fn find_chromium() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(cache) = playwright_cache() {
        for dir in newest(&cache, "chromium_headless_shell-") {
            for sub in ["chrome-headless-shell-mac-arm64", "chrome-headless-shell-mac-x64", "chrome-headless-shell-linux64",
                        "chrome-linux"] {
                candidates.push(dir.join(sub).join("chrome-headless-shell"));
                candidates.push(dir.join(sub).join("headless_shell"));
            }
            candidates.push(dir.join("chrome-headless-shell-win64").join("chrome-headless-shell.exe"));
        }
        for dir in newest(&cache, "chromium-") {
            for sub in ["chrome-mac-arm64", "chrome-mac", "chrome-mac-x64"] {
                candidates.push(dir.join(sub).join("Chromium.app/Contents/MacOS/Chromium"));
            }
            candidates.push(dir.join("chrome-linux").join("chrome"));
            candidates.push(dir.join("chrome-linux64").join("chrome"));
            candidates.push(dir.join("chrome-win").join("chrome.exe"));
        }
    }
    candidates.push(PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
    candidates.push(PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"));
    for name in ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"] {
        if let Ok(p) = which::which(name) {
            candidates.push(p);
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// A running headless Chromium.
pub struct Browser {
    conn: Arc<Connection>,
    child: Mutex<Option<tokio::process::Child>>,
    _profile: Option<tempfile::TempDir>,
    user_agent: String,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser").field("user_agent", &self.user_agent).finish_non_exhaustive()
    }
}

/// No Chromium on this Mac: the check cannot run (Python: Playwright is not installed).
#[derive(Debug, Clone, thiserror::Error)]
#[error("no Chromium found (install it with: playwright install chromium)")]
pub struct NoChromium;

impl Browser {
    /// `p.chromium.launch(headless=True, args=args)`.
    pub async fn launch(args: &[&str]) -> anyhow::Result<Browser> {
        let exe = find_chromium().ok_or(NoChromium)?;
        Self::launch_at(&exe, args).await
    }

    /// Launch the Chromium at `exe`.
    pub async fn launch_at(exe: &Path, args: &[&str]) -> anyhow::Result<Browser> {
        let profile = tempfile::Builder::new().prefix("eve-app-check-").tempdir()?;
        let mut cmd = tokio::process::Command::new(exe);
        cmd.args(PLAYWRIGHT_SWITCHES)
            .args(args)
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .arg("--remote-debugging-port=0")
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow::anyhow!("no stderr from Chromium"))?;
        let mut lines = BufReader::new(stderr).lines();
        let url = tokio::time::timeout(Duration::from_secs(30), async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(at) = line.find("ws://") {
                    if line.contains("DevTools listening on") {
                        return Some(line[at..].trim().to_string());
                    }
                }
            }
            None
        })
        .await
        .ok()
        .flatten();
        let Some(url) = url else {
            let _ = child.kill().await;
            anyhow::bail!("Chromium did not start (no DevTools endpoint)");
        };
        // keep reading what Chromium writes, so a full pipe never blocks it
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        let conn = Connection::connect(&url).await?;
        let version = conn.call(None, "Browser.getVersion", json!({})).await?;
        let user_agent = version
            .get("userAgent")
            .and_then(Value::as_str)
            .unwrap_or("")
            .replace("HeadlessChrome/", "Chrome/");
        Ok(Browser { conn, child: Mutex::new(Some(child)), _profile: Some(profile), user_agent })
    }

    /// `browser.new_context(...)`.
    pub async fn new_context(&self, opts: ContextOptions) -> anyhow::Result<Context> {
        let made = self.conn.call(None, "Target.createBrowserContext", json!({"disposeOnDetach": true})).await?;
        let id = made
            .get("browserContextId")
            .and_then(Value::as_str)
            .ok_or_else(|| PageError::new("Error", "no browser context"))?
            .to_string();
        Ok(Context {
            conn: self.conn.clone(),
            id,
            opts,
            user_agent: self.user_agent.clone(),
            closed: AtomicBool::new(false),
        })
    }

    pub fn is_closed(&self) -> bool {
        self.conn.is_closed()
    }

    /// `browser.close()`: asks Chromium to quit, and ends the process if it does not.
    pub async fn close(&self) {
        if !self.conn.is_closed() {
            let _ = tokio::time::timeout(Duration::from_secs(3), self.conn.call(None, "Browser.close", json!({}))).await;
        }
        self.conn.shut("Browser has been closed");
        let child = self.child.lock().take();
        if let Some(mut child) = child {
            if tokio::time::timeout(Duration::from_secs(3), child.wait()).await.is_err() {
                let _ = child.kill().await;
            }
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.start_kill();
        }
    }
}

/// `browser.new_context(viewport=, device_scale_factor=, is_mobile=, has_touch=, locale=, color_scheme=)`.
#[derive(Debug, Clone)]
pub struct ContextOptions {
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: f64,
    pub is_mobile: bool,
    pub has_touch: bool,
    pub locale: String,
    pub color_scheme: String,
}

/// An isolated browser context. Closed by `close()`, or when dropped (a check stopped by its time limit).
pub struct Context {
    conn: Arc<Connection>,
    id: String,
    opts: ContextOptions,
    user_agent: String,
    closed: AtomicBool,
}

impl Context {
    /// `context.new_page()`: a page with the context's device, locale and color scheme.
    pub async fn new_page(&self) -> anyhow::Result<Page> {
        let made = self
            .conn
            .call(None, "Target.createTarget", json!({"url": "about:blank", "browserContextId": self.id}))
            .await?;
        let target = made.get("targetId").and_then(Value::as_str).unwrap_or("").to_string();
        let attached =
            self.conn.call(None, "Target.attachToTarget", json!({"targetId": target, "flatten": true})).await?;
        let session = attached
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| PageError::new("Error", "the page could not be attached"))?
            .to_string();
        self.conn.state.lock().targets.insert(target.clone(), session.clone());
        let raw = self.conn.take_events(&session).ok_or_else(|| PageError::new("Error", "the page has no events"))?;
        let (user_tx, user_rx) = mpsc::unbounded_channel();
        let loads = Arc::new(watch::channel(0u64).0);
        let crashed = Arc::new(AtomicBool::new(false));
        let page = Page {
            inner: Arc::new(PageInner {
                conn: self.conn.clone(),
                session: session.clone(),
                target,
                opts: self.opts.clone(),
                loads: loads.clone(),
                crashed: crashed.clone(),
                events: Mutex::new(Some(user_rx)),
            }),
        };
        tokio::spawn(page_events(raw, user_tx, loads, crashed, self.conn.clone(), session));
        let o = &self.opts;
        for (method, params) in [
            ("Page.enable", json!({})),
            ("Runtime.enable", json!({})),
            ("Network.enable", json!({})),
            ("Log.enable", json!({})),
            ("Inspector.enable", json!({})),
            ("Emulation.setFocusEmulationEnabled", json!({"enabled": true})),
            ("Emulation.setTouchEmulationEnabled", json!({"enabled": o.has_touch})),
            (
                "Emulation.setEmulatedMedia",
                json!({"media": "", "features": [{"name": "prefers-color-scheme", "value": o.color_scheme}]}),
            ),
            ("Emulation.setLocaleOverride", json!({"locale": o.locale})),
            ("Network.setUserAgentOverride", json!({"userAgent": self.user_agent, "acceptLanguage": o.locale})),
        ] {
            page.call(method, params).await?;
        }
        page.set_viewport_size(o.width, o.height).await?;
        Ok(page)
    }

    /// `context.close()`.
    pub async fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                self.conn.call(None, "Target.disposeBrowserContext", json!({"browserContextId": self.id})),
            )
            .await;
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.conn.send_nowait(None, "Target.disposeBrowserContext", json!({"browserContextId": self.id}));
        }
    }
}

/// A page's own bookkeeping (loads, a crash, dialogs) before its events reach the caller.
async fn page_events(
    mut raw: mpsc::UnboundedReceiver<Event>,
    user: mpsc::UnboundedSender<Event>,
    loads: Arc<watch::Sender<u64>>,
    crashed: Arc<AtomicBool>,
    conn: Arc<Connection>,
    session: String,
) {
    while let Some(ev) = raw.recv().await {
        match ev.method.as_str() {
            "Page.loadEventFired" => {
                loads.send_modify(|n| *n += 1);
            }
            "Inspector.targetCrashed" => crashed.store(true, Ordering::SeqCst),
            // Playwright dismisses a dialog nobody listens for; the stand-ins answer alert/confirm/prompt
            // before one could open, so this is only a beforeunload or the like
            "Page.javascriptDialogOpening" => {
                conn.send_nowait(Some(&session), "Page.handleJavaScriptDialog", json!({"accept": false}));
            }
            _ => {}
        }
        let _ = user.send(ev);
    }
}

// ---- the page ----------------------------------------------------------------------------------------------

struct PageInner {
    conn: Arc<Connection>,
    session: String,
    target: String,
    opts: ContextOptions,
    loads: Arc<watch::Sender<u64>>,
    crashed: Arc<AtomicBool>,
    events: Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
}

/// One page (a tab) of a context. Cheap to clone.
#[derive(Clone)]
pub struct Page {
    inner: Arc<PageInner>,
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page").field("target", &self.inner.target).finish_non_exhaustive()
    }
}

/// Playwright's message for an exception: the description of the thrown value, with its stack, or the text
/// and the call frames when there is no value (getExceptionMessage).
pub fn exception_message(details: &Value) -> String {
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
    let mut message = details.get("text").and_then(Value::as_str).unwrap_or("").to_string();
    if let Some(frames) = details.pointer("/stackTrace/callFrames").and_then(Value::as_array) {
        for f in frames {
            let url = f.get("url").and_then(Value::as_str).unwrap_or("");
            let line = f.get("lineNumber").and_then(Value::as_i64).unwrap_or(0);
            let col = f.get("columnNumber").and_then(Value::as_i64).unwrap_or(0);
            let name = f.get("functionName").and_then(Value::as_str).unwrap_or("");
            let location = format!("{url}:{line}:{col}");
            if name.is_empty() {
                message.push_str(&format!("\n    at {location}"));
            } else {
                message.push_str(&format!("\n    at {name} ({location})"));
            }
        }
    }
    message
}

/// A page error as Playwright hands it to `page.on("pageerror")`: its message (the text after the error's
/// name) and its stack (empty when there is no `    at` line).
pub fn page_error(details: &Value) -> (String, String) {
    let with_stack = exception_message(details);
    let lines: Vec<&str> = with_stack.split('\n').collect();
    let (with_name, stack) = match lines.iter().position(|l| l.starts_with("    at")) {
        None => (with_stack.clone(), String::new()),
        Some(i) => (lines[..i].join("\n"), with_stack.clone()),
    };
    let message = match with_name.find(':') {
        Some(i) if i + 2 <= with_name.len() => with_name[i + 2..].to_string(),
        _ => with_name,
    };
    (message, stack)
}

/// `(js)(arg)` when `js` is a function, else the value of `js`: what `page.evaluate(js, arg)` runs.
fn call_expr(js: &str, arg: Option<&Value>) -> String {
    let arg = arg.map(Value::to_string).unwrap_or_else(|| "undefined".into());
    format!("(() => {{ const __f = ({js}\n); return typeof __f === 'function' ? __f({arg}) : __f; }})()")
}

fn timeout_error(api: &str, ms: u64) -> anyhow::Error {
    PageError::new("TimeoutError", format!("{api}: Timeout {ms}ms exceeded.")).into()
}

/// Playwright's waits between retries of an action that is not ready yet, in ms.
const RETRY_MS: [u64; 6] = [0, 20, 50, 100, 100, 500];

/// The checks Playwright makes before an action, on the first element `a.sel` matches.
const ACT_JS: &str = r#"(a) => {
  const el = document.querySelector(a.sel);
  if (!el || !el.isConnected) return { s: "missing" };
  const box = el.getBoundingClientRect(), st = getComputedStyle(el);
  const visible = box.width > 0 && box.height > 0 && st.visibility !== "hidden";
  const aria = el.closest("[aria-disabled]");
  const disabled = el.matches(":disabled") || (!!aria && aria.getAttribute("aria-disabled") === "true");
  const scroll = () => { if (el.scrollIntoViewIfNeeded) el.scrollIntoViewIfNeeded(true);
    else el.scrollIntoView({ block: "center", inline: "center" }); };
  const fire = (...names) => { for (const n of names) el.dispatchEvent(new Event(n, { bubbles: true, composed: n === "input" })); };
  if (a.op === "attached") return { s: "ok" };
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
    const cls = hit.getAttribute("class");
    return { s: `<${hit.tagName.toLowerCase()}${cls ? ` class="${cls}"` : ""}> intercepts pointer events` };
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
      el.select(); el.focus(); return { s: "type" };
    }
    if (tag === "TEXTAREA") { el.select(); el.focus(); return { s: "type" }; }
    const range = document.createRange(); range.selectNodeContents(el);
    const sel = getSelection(); sel.removeAllRanges(); sel.addRange(range); el.focus(); return { s: "type" };
  }
  if (a.op === "select") {
    if (el.tagName !== "SELECT") return { error: "Element is not a <select> element" };
    const options = Array.from(el.options);
    let chosen = null;
    if (a.index !== null && a.index !== undefined) { chosen = options[a.index]; if (!chosen) return { s: "no such option" }; }
    else if (a.label !== null && a.label !== undefined) { chosen = options.find((o) => o.label === a.label); if (!chosen) return { s: "no such option" }; }
    for (const o of options) o.selected = o === chosen;
    fire("input", "change");
    return { s: "done" };
  }
  return { error: "unknown action " + a.op };
}"#;

impl Page {
    /// One protocol call on this page's session.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, PageError> {
        if self.inner.crashed.load(Ordering::SeqCst) {
            return Err(PageError::new("Error", CRASHED));
        }
        self.inner.conn.call(Some(&self.inner.session), method, params).await
    }

    /// The page's protocol events, once (the check wires them to its session).
    pub fn take_events(&self) -> Option<mpsc::UnboundedReceiver<Event>> {
        self.inner.events.lock().take()
    }

    /// Whether the page's renderer died.
    pub fn crashed(&self) -> bool {
        self.inner.crashed.load(Ordering::SeqCst)
    }

    /// How many times the page fired "load".
    pub fn loads(&self) -> u64 {
        *self.inner.loads.borrow()
    }

    /// `page.add_init_script(source)`.
    pub async fn add_init_script(&self, source: &str) -> anyhow::Result<()> {
        self.call("Page.addScriptToEvaluateOnNewDocument", json!({"source": source})).await?;
        Ok(())
    }

    /// A function `window[name](payload: string)` that reaches the checker as Runtime.bindingCalled.
    pub async fn add_binding(&self, name: &str) -> anyhow::Result<()> {
        self.call("Runtime.addBinding", json!({"name": name})).await?;
        Ok(())
    }

    /// Every request of the page pauses for the checker (Fetch.requestPaused) until it is fulfilled,
    /// continued or failed.
    pub async fn route_all(&self) -> anyhow::Result<()> {
        self.call("Fetch.enable", json!({"patterns": [{"urlPattern": "*", "requestStage": "Request"}]})).await?;
        Ok(())
    }

    /// `route.fulfill(status=, content_type=, body=, headers=)`.
    pub async fn fulfill(&self, request_id: &str, status: u16, headers: &[(&str, &str)], body: &str) -> anyhow::Result<()> {
        let mut list: Vec<Value> = headers.iter().map(|(k, v)| json!({"name": k, "value": v})).collect();
        list.push(json!({"name": "content-length", "value": body.len().to_string()}));
        self.call(
            "Fetch.fulfillRequest",
            json!({"requestId": request_id, "responseCode": status, "responseHeaders": list,
                   "body": base64::engine::general_purpose::STANDARD.encode(body.as_bytes())}),
        )
        .await?;
        Ok(())
    }

    /// `route.continue_()`.
    pub async fn continue_request(&self, request_id: &str) -> anyhow::Result<()> {
        self.call("Fetch.continueRequest", json!({"requestId": request_id})).await?;
        Ok(())
    }

    /// `route.abort()`: net::ERR_FAILED.
    pub async fn abort(&self, request_id: &str) -> anyhow::Result<()> {
        self.call("Fetch.failRequest", json!({"requestId": request_id, "errorReason": "Failed"})).await?;
        Ok(())
    }

    /// `page.goto(url, wait_until="load", timeout=)`.
    pub async fn goto(&self, url: &str, timeout_ms: u64) -> anyhow::Result<()> {
        let mut loads = self.inner.loads.subscribe();
        let before = *loads.borrow_and_update();
        let navigate = async {
            let r = self.call("Page.navigate", json!({"url": url})).await?;
            if let Some(err) = r.get("errorText").and_then(Value::as_str).filter(|e| !e.is_empty()) {
                return Err(PageError::new("Error", format!("Page.goto: {err} at {url}")).into());
            }
            while *loads.borrow_and_update() <= before {
                if loads.changed().await.is_err() {
                    return Err(PageError::new("TargetClosedError", CLOSED).into());
                }
            }
            Ok::<(), anyhow::Error>(())
        };
        match tokio::time::timeout(Duration::from_millis(timeout_ms), navigate).await {
            Ok(r) => r,
            Err(_) => Err(timeout_error("Page.goto", timeout_ms)),
        }
    }

    /// `page.evaluate(js, arg)`: the result as JSON (None for undefined). A script that throws is an Error
    /// whose message is Playwright's ("Page.evaluate: TypeError: ...").
    pub async fn evaluate(&self, js: &str, arg: Option<Value>) -> anyhow::Result<Value> {
        self.evaluate_as("Page.evaluate", &call_expr(js, arg.as_ref())).await
    }

    async fn evaluate_as(&self, api: &str, expression: &str) -> anyhow::Result<Value> {
        let r = self
            .call(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true, "awaitPromise": true, "userGesture": true}),
            )
            .await?;
        if let Some(details) = r.get("exceptionDetails") {
            return Err(PageError::new("Error", format!("{api}: {}", exception_message(details))).into());
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// Run `expression` in one execution context (a binding's answer goes back to the frame that asked).
    pub async fn evaluate_in(&self, context_id: i64, expression: &str) -> anyhow::Result<()> {
        self.call("Runtime.evaluate", json!({"expression": expression, "contextId": context_id, "returnByValue": true}))
            .await?;
        Ok(())
    }

    /// `page.wait_for_timeout(ms)`.
    pub async fn wait_for_timeout(&self, ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    /// `page.set_viewport_size({"width":, "height":})`, keeping the context's phone.
    pub async fn set_viewport_size(&self, width: u32, height: u32) -> anyhow::Result<()> {
        let o = &self.inner.opts;
        let mut params = json!({"width": width, "height": height, "deviceScaleFactor": o.device_scale_factor,
                                "mobile": o.is_mobile, "screenWidth": width, "screenHeight": height});
        if o.is_mobile {
            params["screenOrientation"] = json!({"angle": 0, "type": "portraitPrimary"});
        }
        self.call("Emulation.setDeviceMetricsOverride", params).await?;
        Ok(())
    }

    /// `page.screenshot(type="jpeg", quality=)` / `page.screenshot(type="png")`: what is on screen.
    pub async fn screenshot(&self, jpeg_quality: Option<u8>) -> anyhow::Result<Vec<u8>> {
        let params = match jpeg_quality {
            Some(q) => json!({"format": "jpeg", "quality": q, "captureBeyondViewport": false}),
            None => json!({"format": "png", "captureBeyondViewport": false}),
        };
        let r = self.call("Page.captureScreenshot", params).await?;
        let data = r.get("data").and_then(Value::as_str).unwrap_or("");
        Ok(base64::engine::general_purpose::STANDARD.decode(data)?)
    }

    /// `page.set_content(html)`: the document replaced, then its load (images included) waited for.
    pub async fn set_content(&self, html: &str) -> anyhow::Result<()> {
        let write = format!("(() => {{ document.open(); document.write({}); document.close(); }})()", json!(html));
        self.evaluate_as("Page.set_content", &write).await?;
        let done = async {
            loop {
                let state = self.evaluate_as("Page.set_content", "document.readyState").await?;
                if state == "complete" {
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        match tokio::time::timeout(Duration::from_secs(30), done).await {
            Ok(r) => r,
            Err(_) => Err(timeout_error("Page.set_content", 30_000)),
        }
    }

    /// Playwright's `page.clock.install()`: the page's timers and Date run on a clock that keeps real time
    /// until `clock_fast_forward` jumps it.
    pub async fn install_clock(&self) -> anyhow::Result<()> {
        self.add_init_script(CLOCK_JS).await
    }

    /// `page.clock.fast_forward(ms)`: every timer due by then fires once; the first error a timer throws is
    /// this call's ("Clock.fast_forward: ReferenceError: ...").
    pub async fn clock_fast_forward(&self, ms: u64) -> anyhow::Result<()> {
        self.evaluate_as(
            "Clock.fast_forward",
            &format!("(() => {{ const c = window.__checkClock; if (!c) throw new Error('Clock was not installed'); c.fastForward({ms}); }})()"),
        )
        .await?;
        Ok(())
    }

    /// `page.close()`.
    pub async fn close(&self) {
        let _ = self.inner.conn.call(None, "Target.closeTarget", json!({"targetId": self.inner.target})).await;
    }

    // ---- locators: the first element a CSS selector matches -------------------------------------------

    async fn act(&self, args: Value) -> anyhow::Result<Value> {
        self.evaluate(ACT_JS, Some(args)).await
    }

    /// Retry `step` (Playwright's waits) until it says done or `ms` pass; the whole action is bounded too.
    async fn retry<F, Fut>(&self, api: &str, ms: u64, mut step: F) -> anyhow::Result<()>
    where
        F: FnMut() -> Fut + Send,
        Fut: std::future::Future<Output = anyhow::Result<bool>> + Send,
    {
        let deadline = Instant::now() + Duration::from_millis(ms);
        let attempts = async {
            let mut n = 0usize;
            loop {
                let wait = RETRY_MS[n.min(RETRY_MS.len() - 1)];
                n += 1;
                if wait > 0 {
                    tokio::time::sleep(Duration::from_millis(wait)).await;
                }
                if step().await? {
                    return Ok::<(), anyhow::Error>(());
                }
                if Instant::now() >= deadline {
                    return Err(timeout_error(api, ms));
                }
            }
        };
        match tokio::time::timeout_at(deadline.into(), attempts).await {
            Ok(r) => r,
            Err(_) => Err(timeout_error(api, ms)),
        }
    }

    fn act_error(api: &str, r: &Value) -> Option<anyhow::Error> {
        r.get("error").and_then(Value::as_str).map(|e| PageError::new("Error", format!("{api}: Error: {e}")).into())
    }

    /// `page.locator(selector).count()`.
    pub async fn count(&self, selector: &str) -> anyhow::Result<usize> {
        let n = self.evaluate("(s) => document.querySelectorAll(s).length", Some(json!(selector))).await?;
        Ok(n.as_u64().unwrap_or(0) as usize)
    }

    /// `locator.click(timeout=)`: waits for the element to be visible, enabled, still and not covered, then
    /// clicks the middle of its visible part with the mouse.
    pub async fn click(&self, selector: &str, timeout_ms: u64) -> anyhow::Result<()> {
        let api = "Locator.click";
        self.retry(api, timeout_ms, || async move {
            let first = self.act(json!({"sel": selector, "op": "point"})).await?;
            if let Some(e) = Self::act_error(api, &first) {
                return Err(e);
            }
            if first["s"] != "ok" {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(16)).await;
            let again = self.act(json!({"sel": selector, "op": "point"})).await?;
            if again["s"] != "ok" || again["rect"] != first["rect"] {
                return Ok(false); // still moving
            }
            let (x, y) = (again["x"].as_f64().unwrap_or(0.0), again["y"].as_f64().unwrap_or(0.0));
            let hit = self.act(json!({"sel": selector, "op": "hit", "x": x, "y": y})).await?;
            if hit["s"] != "ok" {
                return Ok(false);
            }
            for (kind, buttons) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 0)] {
                let mut p = json!({"type": kind, "x": x, "y": y, "button": "left", "buttons": buttons,
                                   "pointerType": "mouse"});
                if kind != "mouseMoved" {
                    p["clickCount"] = json!(1);
                } else {
                    p["button"] = json!("none");
                }
                self.call("Input.dispatchMouseEvent", p).await?;
            }
            Ok(true)
        })
        .await
    }

    /// `locator.fill(value, timeout=)`: a date-like field gets its value set; any other field is selected
    /// and typed over (an empty value deletes what is there).
    pub async fn fill(&self, selector: &str, value: &str, timeout_ms: u64) -> anyhow::Result<()> {
        let api = "Locator.fill";
        self.retry(api, timeout_ms, || async move {
            let r = self.act(json!({"sel": selector, "op": "fill", "value": value})).await?;
            if let Some(e) = Self::act_error(api, &r) {
                return Err(e);
            }
            match r["s"].as_str() {
                Some("done") => Ok(true),
                Some("type") => {
                    if value.is_empty() {
                        for kind in ["keyDown", "keyUp"] {
                            self.call(
                                "Input.dispatchKeyEvent",
                                json!({"type": kind, "key": "Delete", "code": "Delete", "windowsVirtualKeyCode": 46}),
                            )
                            .await?;
                        }
                    } else {
                        self.call("Input.insertText", json!({"text": value})).await?;
                    }
                    Ok(true)
                }
                _ => Ok(false),
            }
        })
        .await
    }

    async fn select(&self, selector: &str, index: Option<i64>, label: Option<&str>, timeout_ms: u64) -> anyhow::Result<()> {
        let api = "Locator.select_option";
        self.retry(api, timeout_ms, || async move {
            let r = self.act(json!({"sel": selector, "op": "select", "index": index, "label": label})).await?;
            if let Some(e) = Self::act_error(api, &r) {
                return Err(e);
            }
            Ok(r["s"] == "done")
        })
        .await
    }

    /// `locator.select_option(index=, timeout=)`.
    pub async fn select_index(&self, selector: &str, index: i64, timeout_ms: u64) -> anyhow::Result<()> {
        self.select(selector, Some(index), None, timeout_ms).await
    }

    /// `locator.select_option(label=, timeout=)`.
    pub async fn select_label(&self, selector: &str, label: Option<&str>, timeout_ms: u64) -> anyhow::Result<()> {
        self.select(selector, None, label, timeout_ms).await
    }

    /// `locator.evaluate(js, timeout=)`: `js` takes the element, once it is in the page.
    pub async fn evaluate_on(&self, selector: &str, js: &str, timeout_ms: u64) -> anyhow::Result<Value> {
        let api = "Locator.evaluate";
        self.retry(api, timeout_ms, || async move {
            let r = self.act(json!({"sel": selector, "op": "attached"})).await?;
            Ok(r["s"] == "ok")
        })
        .await?;
        let expr = format!(
            "(() => {{ const el = document.querySelector({}); if (!el) throw new Error('the element is gone'); return ({js}\n)(el); }})()",
            json!(selector)
        );
        self.evaluate_as(api, &expr).await
    }

    /// `locator.scroll_into_view_if_needed(timeout=)`.
    pub async fn scroll_into_view_if_needed(&self, selector: &str, timeout_ms: u64) -> anyhow::Result<()> {
        let api = "Locator.scroll_into_view_if_needed";
        self.retry(api, timeout_ms, || async move {
            let r = self.act(json!({"sel": selector, "op": "scroll"})).await?;
            Ok(r["s"] == "ok")
        })
        .await
    }
}

#[async_trait]
impl eve_jev_verify::Page for Page {
    async fn evaluate(&self, js: &str, arg: Option<Value>) -> anyhow::Result<Value> {
        Page::evaluate(self, js, arg).await
    }

    async fn count(&self, selector: &str) -> anyhow::Result<usize> {
        Page::count(self, selector).await
    }

    async fn fill(&self, selector: &str, value: &str, timeout_ms: u64) -> anyhow::Result<()> {
        Page::fill(self, selector, value, timeout_ms).await
    }

    async fn evaluate_on(&self, selector: &str, js: &str, timeout_ms: u64) -> anyhow::Result<Value> {
        Page::evaluate_on(self, selector, js, timeout_ms).await
    }

    async fn select_option(&self, selector: &str, label: Option<&str>, timeout_ms: u64) -> anyhow::Result<()> {
        Page::select_label(self, selector, label, timeout_ms).await
    }

    async fn scroll_into_view_if_needed(&self, selector: &str, timeout_ms: u64) -> anyhow::Result<()> {
        Page::scroll_into_view_if_needed(self, selector, timeout_ms).await
    }

    async fn click(&self, selector: &str, timeout_ms: u64) -> anyhow::Result<()> {
        Page::click(self, selector, timeout_ms).await
    }

    async fn wait_for_timeout(&self, ms: u64) {
        Page::wait_for_timeout(self, ms).await
    }
}

/// Playwright's clock, as the check uses it: installed before the page's own code, it runs in real time (each
/// timer is also a native one) until `fastForward(ms)` moves Date and performance.now by `ms` and fires every
/// timer due by then once (an interval is set again from the new time). A timer that throws during the jump
/// throws out of `fastForward`, after the rest have run.
pub const CLOCK_JS: &str = r#"(() => {
  if (window.__checkClock) return;
  const native = { setTimeout: window.setTimeout.bind(window), clearTimeout: window.clearTimeout.bind(window),
    Date: window.Date, perfNow: performance.now.bind(performance) };
  let skew = 0, nextId = 1;
  const now = () => native.Date.now() + skew;
  const timers = new Map();
  const arm = (t) => { native.clearTimeout(t.native);
    t.native = native.setTimeout(() => fire(t), Math.min(Math.max(0, t.at - now()), 2147483647)); };
  const run = (t) => {
    if (t.interval) { t.at = now() + t.delay; arm(t); } else timers.delete(t.id);
    const f = typeof t.fn === "function" ? t.fn : () => (0, eval)(String(t.fn));
    f.apply(window, t.args);
  };
  const fire = (t) => { if (!timers.has(t.id)) return; if (t.at > now()) { arm(t); return; } run(t); };
  const add = (fn, delay, args, interval) => {
    delay = Math.max(0, Number(delay) || 0);
    const t = { id: nextId++, fn, args, delay, interval, at: now() + delay, native: 0 };
    timers.set(t.id, t); arm(t); return t.id;
  };
  const clear = (id) => { const t = timers.get(Number(id)); if (t) { native.clearTimeout(t.native); timers.delete(t.id); } };
  window.setTimeout = function setTimeout(fn, delay, ...args) { return add(fn, delay, args, false); };
  window.setInterval = function setInterval(fn, delay, ...args) { return add(fn, delay, args, true); };
  window.clearTimeout = function clearTimeout(id) { clear(id); };
  window.clearInterval = function clearInterval(id) { clear(id); };
  function Date(...a) {
    if (!new.target) return new native.Date(now()).toString();
    return Reflect.construct(native.Date, a.length ? a : [now()], new.target);
  }
  Date.prototype = native.Date.prototype;
  Date.now = () => now(); Date.parse = native.Date.parse; Date.UTC = native.Date.UTC;
  window.Date = Date;
  performance.now = () => native.perfNow() + skew;
  Object.defineProperty(window, "__checkClock", { value: { fastForward(ms) {
    skew += ms;
    const due = Array.from(timers.values()).filter((t) => t.at <= now()).sort((a, b) => a.at - b.at || a.id - b.id);
    const errors = [];
    for (const t of due) {
      if (!timers.has(t.id)) continue;
      native.clearTimeout(t.native);
      try { run(t); } catch (e) { errors.push(e); }
    }
    for (const t of timers.values()) arm(t);
    if (errors.length) throw errors[0];
  } } });
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exception_reads_as_playwrights_page_error() {
        let details = json!({"exception": {"className": "ReferenceError",
            "description": "ReferenceError: undefinedHelper is not defined\n    at HTMLButtonElement.onclick (https://app.eve.test/apps/check/:31:5)"}});
        let (message, stack) = page_error(&details);
        assert_eq!(message, "undefinedHelper is not defined");
        assert!(stack.contains("app.eve.test/apps/check/:31:5"));
        let thrown = json!({"exception": {"type": "string", "value": "boom"}, "text": "Uncaught"});
        assert_eq!(page_error(&thrown), ("boom".to_string(), String::new()));
        let text_only = json!({"text": "Uncaught", "stackTrace": {"callFrames": [
            {"functionName": "", "url": "https://x/", "lineNumber": 3, "columnNumber": 1}]}});
        assert_eq!(exception_message(&text_only), "Uncaught\n    at https://x/:3:1");
    }

    #[test]
    fn evaluate_calls_a_function_and_reads_an_expression() {
        let e = call_expr("(n) => n + 1", Some(&json!(2)));
        assert!(e.contains("__f(2)"));
        assert!(call_expr("1", None).contains("__f(undefined)"));
    }
}
