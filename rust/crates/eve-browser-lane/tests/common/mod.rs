//! Port of bridge/tests/browser_fakes.py: a fake page that answers the lane's own scripts by identity
//! (`COLLECT_JS`, `RECHECK_JS` …), so no browser runs. Plus the fakes the attach tests built from
//! SimpleNamespace: a browser, a driver, contexts.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eve_browser_lane as bl;
use eve_browser_lane::driver::{BrowserHandle, ContextHandle, Cookie, Driver, PageHandle, SelectBy};
use eve_desktop_lanes::LaneError;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

pub type Row = Map<String, Value>;
pub type Mutate = Box<dyn FnOnce(&mut FakeState) + Send>;

/// One document: `url`, `title`, `body` and `elements` (the collector's raw rows). `routes` maps a URL to
/// (title, body, elements) for goto. `doc` stands for performance.timeOrigin; a goto starts a new document.
/// `mutate` runs after the scroll and before the recheck.
pub struct FakeState {
    pub url: String,
    pub title: String,
    pub body: String,
    pub elements: Vec<Row>,
    pub routes: HashMap<String, (String, String, Vec<Row>)>,
    pub doc: f64,
    pub clicks: Vec<String>,
    pub keys: Vec<String>,
    pub rechecks: usize,
    pub mutate: Option<Mutate>,
    /// the recheck script raises (test_an_unreadable_recheck_refuses)
    pub broken_recheck: bool,
    pub closed: bool,
    /// what close() appends to, when set
    pub close_log: Option<Arc<Mutex<Vec<String>>>>,
}

impl FakeState {
    pub fn element(&mut self, eid: &str) -> Option<&mut Row> {
        self.elements.iter_mut().find(|e| id_of(e) == eid)
    }

    pub fn has(&self, eid: &str) -> bool {
        self.elements.iter().any(|e| id_of(e) == eid)
    }
}

pub fn id_of(e: &Row) -> String {
    match e.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn doc_str(doc: f64) -> String {
    eve_vault::pystr::float_repr(doc)
}

#[derive(Clone)]
pub struct FakePage(pub Arc<Mutex<FakeState>>);

impl FakePage {
    pub fn new(url: &str, title: &str, body: &str, elements: Vec<Row>) -> FakePage {
        FakePage(Arc::new(Mutex::new(FakeState {
            url: url.into(),
            title: title.into(),
            body: body.into(),
            elements,
            routes: HashMap::new(),
            doc: 1000.5,
            clicks: vec![],
            keys: vec![],
            rechecks: 0,
            mutate: None,
            broken_recheck: false,
            closed: false,
            close_log: None,
        })))
    }

    pub fn with_route(self, url: &str, title: &str, body: &str, elements: Vec<Row>) -> FakePage {
        self.0.lock().routes.insert(url.into(), (title.into(), body.into(), elements));
        self
    }

    pub fn state(&self) -> parking_lot::MutexGuard<'_, FakeState> {
        self.0.lock()
    }

    pub fn clicks(&self) -> Vec<String> {
        self.0.lock().clicks.clone()
    }

    pub fn keys(&self) -> Vec<String> {
        self.0.lock().keys.clone()
    }

    pub fn set_mutate(&self, f: impl FnOnce(&mut FakeState) + Send + 'static) {
        self.0.lock().mutate = Some(Box::new(f));
    }

    pub fn boxed(&self) -> Box<dyn PageHandle> {
        Box::new(self.clone())
    }

    fn eid(selector: &str) -> String {
        selector.split('"').nth(1).unwrap_or(selector).to_string()
    }

    fn before_click(&self) {
        let f = self.0.lock().mutate.take();
        if let Some(f) = f {
            f(&mut self.0.lock());
        }
    }
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

impl PageHandle for FakePage {
    fn evaluate(&self, script: &str, arg: Option<&Value>) -> anyhow::Result<Value> {
        let mut st = self.0.lock();
        if script == bl::COLLECT_JS {
            return Ok(json!({"elements": st.elements.clone(), "title": st.title, "url": st.url, "dialog": "",
                             "focused_id": null, "document_id": doc_str(st.doc), "head": head(&st.body, 600),
                             "text_hash": format!("{}:{}", st.body.chars().count(), head(&st.body, 4000))}));
        }
        if script == bl::RECHECK_JS {
            // The script's read, answered from Rust; recheck.rs runs the real one in node.
            if st.broken_recheck {
                return Err(LaneError::err(
                    "Error",
                    "Execution context was destroyed, most likely because of a navigation",
                ));
            }
            st.rechecks += 1;
            let id = arg.and_then(|a| a.get("id")).map(|v| v.as_str().map_or(v.to_string(), str::to_string));
            let id = id.unwrap_or_default();
            let (url, doc) = (st.url.clone(), doc_str(st.doc));
            let mut now = json!({"url": url, "doc": doc, "found": false});
            if let Some(e) = st.element(&id) {
                let g = |k: &str| e.get(k).cloned().unwrap_or(json!(""));
                now = json!({"url": url, "doc": doc, "found": true, "role": g("role"), "name": g("name"),
                             "href": g("href"), "dom_id": g("dom_id")});
            }
            return Ok(now);
        }
        if script == bl::CONSENT_JS {
            return Ok(Value::Null);
        }
        if script == bl::WEBMCP_JS {
            return Ok(json!({"api": false}));
        }
        Ok(Value::String(format!("{} ", st.body))) // text_visible's read
    }

    fn title(&self) -> anyhow::Result<String> {
        let st = self.0.lock();
        if st.closed {
            return Err(LaneError::err("TargetClosedError", "Target page, context or browser has been closed"));
        }
        Ok(st.title.clone())
    }

    fn url(&self) -> String {
        self.0.lock().url.clone()
    }

    fn inner_text(&self, _selector: &str, _timeout_ms: u64) -> anyhow::Result<String> {
        Ok(self.0.lock().body.clone())
    }

    fn count(&self, selector: &str) -> anyhow::Result<usize> {
        Ok(usize::from(self.0.lock().has(&Self::eid(selector))))
    }

    fn scroll_into_view_if_needed(&self, _selector: &str, _timeout_ms: u64) -> anyhow::Result<()> {
        self.before_click();
        Ok(())
    }

    fn click(&self, selector: &str, _timeout_ms: u64) -> anyhow::Result<()> {
        self.0.lock().clicks.push(Self::eid(selector));
        Ok(())
    }

    fn fill(&self, selector: &str, _value: &str, _timeout_ms: u64) -> anyhow::Result<()> {
        self.0.lock().clicks.push(Self::eid(selector));
        Ok(())
    }

    fn select_option(&self, _selector: &str, _choice: SelectBy<'_>, _timeout_ms: u64) -> anyhow::Result<()> {
        Err(LaneError::err("AttributeError", "'FakeLocator' object has no attribute 'select_option'"))
    }

    fn evaluate_on(&self, _selector: &str, _script: &str) -> anyhow::Result<Value> {
        Err(LaneError::err("AttributeError", "'FakeLocator' object has no attribute 'evaluate'"))
    }

    fn keyboard_type(&self, text: &str, _delay_ms: u64) -> anyhow::Result<()> {
        self.0.lock().keys.push(text.into());
        Ok(())
    }

    fn keyboard_press(&self, key: &str) -> anyhow::Result<()> {
        self.0.lock().keys.push(key.into());
        Ok(())
    }

    fn wait_for_timeout(&self, _ms: u64) {}

    fn wait_for_load_state(&self, _state: &str, _timeout_ms: u64) -> anyhow::Result<()> {
        Ok(())
    }

    fn goto(&self, url: &str, _wait_until: &str, _timeout_ms: u64) -> anyhow::Result<()> {
        let mut st = self.0.lock();
        st.url = url.into();
        let (title, body, elements) = st.routes.get(url).cloned().unwrap_or_default();
        st.title = title;
        st.body = body;
        st.elements = elements;
        st.doc += 1.0;
        Ok(())
    }

    fn bring_to_front(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn close(&self) -> anyhow::Result<()> {
        let log = self.0.lock().close_log.clone();
        if let Some(log) = log {
            log.lock().push("eve tab.close".into());
        }
        Ok(())
    }

    fn screenshot_jpeg(&self, _quality: u8, _mask: Option<(&str, &str)>) -> anyhow::Result<Vec<u8>> {
        Ok(vec![])
    }
}

/// A context holding pages; `new_page` hands out `fresh` (or the first page) and logs it.
pub struct FakeContext {
    pub pages: Vec<FakePage>,
    pub fresh: Option<FakePage>,
    pub opened: Arc<Mutex<Vec<String>>>,
    pub log: Option<Arc<Mutex<Vec<String>>>>,
    pub sid: String,
}

impl FakeContext {
    pub fn new(page: FakePage) -> FakeContext {
        FakeContext { pages: vec![page], fresh: None, opened: Arc::default(), log: None, sid: String::new() }
    }
}

impl ContextHandle for FakeContext {
    fn pages(&self) -> anyhow::Result<Vec<Box<dyn PageHandle>>> {
        Ok(self.pages.iter().map(|p| p.boxed()).collect())
    }

    fn new_page(&self) -> anyhow::Result<Box<dyn PageHandle>> {
        self.opened.lock().push("new".into());
        match self.fresh.as_ref().or(self.pages.first()) {
            Some(p) => Ok(p.boxed()),
            None => Err(LaneError::err("Error", "no page")),
        }
    }

    fn cookies(&self, _urls: &[&str]) -> anyhow::Result<Vec<Cookie>> {
        if self.sid.is_empty() {
            return Ok(vec![]);
        }
        Ok(vec![Cookie { name: "SID".into(), value: self.sid.clone() }])
    }

    fn close(&self) -> anyhow::Result<()> {
        if let Some(log) = &self.log {
            log.lock().push("context.close".into());
        }
        Ok(())
    }
}

pub struct FakeBrowser {
    pub contexts: Vec<Arc<dyn ContextHandle>>,
    pub connected: bool,
}

impl BrowserHandle for FakeBrowser {
    fn contexts(&self) -> anyhow::Result<Vec<Arc<dyn ContextHandle>>> {
        Ok(self.contexts.clone())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}

/// A driver: `connect_over_cdp` hands back `browser` and records the endpoint; `stop` sets `stopped`.
pub struct FakeDriver {
    pub stopped: Arc<AtomicBool>,
    pub browser: Option<Arc<dyn BrowserHandle>>,
    pub endpoints: Arc<Mutex<Vec<String>>>,
    pub log: Option<Arc<Mutex<Vec<String>>>>,
}

impl FakeDriver {
    pub fn logging(log: Arc<Mutex<Vec<String>>>) -> FakeDriver {
        FakeDriver { stopped: Arc::default(), browser: None, endpoints: Arc::default(), log: Some(log) }
    }
}

impl Driver for FakeDriver {
    fn connect_over_cdp(&mut self, endpoint: &str, _timeout_ms: u64) -> anyhow::Result<Arc<dyn BrowserHandle>> {
        self.endpoints.lock().push(endpoint.into());
        self.browser.clone().ok_or_else(|| LaneError::err("Error", "no browser"))
    }

    fn launch_persistent_context(
        &mut self,
        _profile: &Path,
        _headless: bool,
        _viewport: (u32, u32),
        _args: &[&str],
    ) -> anyhow::Result<Arc<dyn ContextHandle>> {
        Err(LaneError::err("Error", "no launch in the fakes"))
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(log) = &self.log {
            log.lock().push("playwright.stop".into());
        }
        Ok(())
    }
}

/// `NoVault`: the planner's vault lines are empty; a fill finds nothing.
pub struct NoVault;

impl bl::LaneVault for NoVault {
    fn lines(&self) -> Vec<String> {
        vec![]
    }

    fn fill_values(
        &self,
        handle: &str,
        _page_url: &str,
    ) -> Result<(eve_vault::Item, eve_vault::FillValues), eve_vault::VaultError> {
        Err(eve_vault::VaultError::Error(format!("no vault item {handle}")))
    }
}

pub fn no_vault() -> Option<Arc<dyn bl::LaneVault>> {
    Some(Arc::new(NoVault))
}

pub fn options() -> bl::LaneOptions {
    bl::LaneOptions { vault: no_vault(), ..bl::LaneOptions::default() }
}

/// A core whose launcher hands it `page`: no driver, no browser.
pub fn core_on(page: &FakePage, tmp: &Path) -> bl::LaneCore {
    let page = page.clone();
    let cfg = bl::BrowserLaneConfig { profile: tmp.join("profile"), headless: true, ..Default::default() };
    bl::LaneCore::new(
        cfg,
        bl::LaneOptions {
            launcher: Some(Box::new(move || Ok(Arc::new(FakeContext::new(page.clone())) as Arc<dyn ContextHandle>))),
            vault: no_vault(),
            ..Default::default()
        },
    )
    .expect("a lane")
}

/// `lane_on(page, tmp_path)`.
pub fn lane_on(page: &FakePage, tmp: &Path) -> bl::BrowserLane {
    bl::BrowserLane::from_core(core_on(page, tmp)).expect("a lane")
}

/// A core with no browser and the given config (vault: none).
pub fn core_with(cfg: bl::BrowserLaneConfig) -> bl::LaneCore {
    bl::LaneCore::new(cfg, options()).expect("a lane")
}

pub fn row(v: Value) -> Row {
    v.as_object().expect("an object").clone()
}

pub fn link(eid: &str, name: &str, href: &str) -> Row {
    let n: i64 = eid.parse().unwrap();
    row(json!({"id": eid, "role": "link", "name": name, "value": "", "x": 10, "y": 10 + 30 * n, "w": 120,
               "h": 20, "enabled": true, "href": href}))
}

pub fn button(eid: &str, name: &str) -> Row {
    let n: i64 = eid.parse().unwrap();
    row(json!({"id": eid, "role": "button", "name": name, "value": "", "x": 10, "y": 10 + 30 * n, "w": 120,
               "h": 20, "enabled": true, "href": ""}))
}

/// Run an async block to completion on a fresh runtime (`asyncio.run`).
pub fn run<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

/// `run(lane, coro)`: the coroutine, then `lane.close()` whatever happened.
pub async fn closing<T>(lane: &bl::BrowserLane, f: impl std::future::Future<Output = T>) -> T {
    let out = f.await;
    let _ = lane.close().await;
    out
}

/// A plan as the lane takes it, from JSON.
pub fn plan(v: Value) -> Value {
    v
}

pub fn no_approvals() -> indexmap::IndexMap<String, String> {
    indexmap::IndexMap::new()
}

/// Capture what the lane logs (tracing) while `f` runs on this thread: the formatted messages, one per line.
pub fn capture_logs<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .without_time()
        .with_level(false)
        .with_target(false)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let text = String::from_utf8_lossy(&buf.0.lock()).into_owned();
    (out, text.lines().map(|l| l.trim().to_string()).collect())
}
