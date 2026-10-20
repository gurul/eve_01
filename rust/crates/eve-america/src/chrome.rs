//! The real browser under [`crate::america_browser`]: eve-browser-lane's own page in eve's normal Chrome, plus
//! what the Python flow takes from the raw Playwright page that the Rust lane's page does not carry: locators
//! (CSS and role/name, scoped and strict), `page.route` and the `request` / `requestfinished` /
//! `requestfailed` events.
//!
//! The lane's page is a [`CdpPage`]; on its browser connection this module attaches a second session to the
//! same tab, with the Network domain on (the events) and the Fetch domain on while a route is installed (the
//! routed request pauses until the handler continues or aborts it). Events are delivered on the lane's thread
//! while the flow drives the page, at the latest during `wait_for_timeout`, as Playwright's sync API delivers
//! them during its calls.

use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use eve_browser_lane::cdp::{CdpPage, Connection, CALL_TIMEOUT};
use eve_browser_lane::driver::PageHandle;
use eve_browser_lane::{BrowserLane, BrowserLaneConfig, LaneCore, LaneOptions};
use eve_desktop_lanes::ax_candidates::{Candidate, Snapshot};
use eve_desktop_lanes::LaneError;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::america_browser::{
    AmericaLane, Answer, BrowserFailure, ChatPage, ChatRequest, Job, LaneWorker, Locator, PageEvent, PageResponse,
    RequestListener, Route, RouteHandler,
};

/// Playwright's default action timeout, for the locator calls that take none.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// How often a locator wait looks again.
const LOOK_AGAIN: Duration = Duration::from_millis(100);

// ---- the lane -----------------------------------------------------------------------------------------------

/// `BrowserLane(config)` for America: its one worker thread, its attach rules, its own tab.
pub struct ChromeLane {
    lane: BrowserLane,
    net: Arc<Mutex<Option<NetPage>>>,
}

impl ChromeLane {
    pub fn new(config: BrowserLaneConfig) -> anyhow::Result<ChromeLane> {
        ChromeLane::from_core(LaneCore::new(config, LaneOptions::default())?)
    }

    /// The lane over a prepared core (its seams or launcher set).
    pub fn from_core(core: LaneCore) -> anyhow::Result<ChromeLane> {
        Ok(ChromeLane { lane: BrowserLane::from_core(core)?, net: Arc::new(Mutex::new(None)) })
    }
}

#[async_trait]
impl AmericaLane for ChromeLane {
    fn config(&self) -> BrowserLaneConfig {
        self.lane.config().clone()
    }

    async fn run(&self, job: Job) -> anyhow::Result<Result<Answer, BrowserFailure>> {
        let net = self.net.clone();
        self.lane
            .run(move |core: &mut LaneCore| {
                let mut slot = net.lock();
                let mut worker = ChromeWorker { core, net: &mut slot };
                job(&mut worker)
            })
            .await
    }

    async fn close(&self) -> anyhow::Result<()> {
        let net = self.net.clone();
        let _ = self.lane.run(move |_core: &mut LaneCore| drop(net.lock().take())).await;
        self.lane.close().await
    }
}

struct ChromeWorker<'a> {
    core: &'a mut LaneCore,
    net: &'a mut Option<NetPage>,
}

impl ChromeWorker<'_> {
    fn handle(&self) -> Option<&dyn PageHandle> {
        self.core.page.as_ref().map(|p| &*p.page)
    }
}

impl LaneWorker for ChromeWorker<'_> {
    fn attach(&self) -> bool {
        self.core.config.attach
    }

    fn ensure(&mut self) -> anyhow::Result<()> {
        let page = self.core.ensure()?;
        let cdp = page
            .page
            .as_any()
            .and_then(|any| any.downcast_ref::<CdpPage>())
            .ok_or_else(|| LaneError::err("Error", "the browser lane's page is not a DevTools page"))?;
        if let Some(net) = self.net.as_ref().filter(|n| n.target == cdp.target_id() && !n.conn.is_closed()) {
            net.pump(Duration::ZERO); // what the page did between questions is nobody's
            return Ok(());
        }
        *self.net = None;
        *self.net = Some(NetPage::attach(cdp.connection().clone(), cdp.target_id())?);
        Ok(())
    }

    fn page(&self) -> Option<Box<dyn ChatPage + '_>> {
        let page = self.handle()?;
        let net = self.net.as_ref()?;
        Some(Box::new(ChromePage { page, net }))
    }

    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        match &self.core.page {
            Some(page) => page.snapshot(),
            None => Err(LaneError::err("Error", "the browser lane has no page")),
        }
    }

    fn candidate_locator(&self, c: &Candidate) -> anyhow::Result<Box<dyn Locator + '_>> {
        let page = self.handle().ok_or_else(|| LaneError::err("Error", "the browser lane has no page"))?;
        let chain = vec![Step::Css(format!("[data-eve-id=\"{}\"]", c.id)), Step::First];
        Ok(Box::new(ChromeLocator { page, chain }))
    }

    fn reset(&mut self) {
        // Closing our own page aborts an abandoned stream and ensures another call cannot read the previous
        // question.
        *self.net = None;
        if let Some(page) = self.core.page.take() {
            let _ = page.page.close(); // cleanup cannot replace the safe failure
        }
    }
}

// ---- the network session ----------------------------------------------------------------------------------

/// A response's status and its headers (names lowercased).
type Received = (u16, Vec<(String, String)>);

/// One page request, as Playwright's Request: the same object in every event about it.
struct ChromeRequest {
    conn: Arc<Connection>,
    session: String,
    id: String,
    method: String,
    url: String,
    has_post_data: bool,
    post_data: Mutex<Option<String>>,
    response: Mutex<Option<Received>>,
}

impl ChromeRequest {
    fn from_cdp(conn: &Arc<Connection>, session: &str, id: &str, request: &Value) -> ChromeRequest {
        let text = |k: &str| request.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        ChromeRequest {
            conn: conn.clone(),
            session: session.to_string(),
            id: id.to_string(),
            method: text("method"),
            url: format!("{}{}", text("url"), text("urlFragment")),
            has_post_data: request.get("hasPostData").and_then(Value::as_bool).unwrap_or(false),
            post_data: Mutex::new(request.get("postData").and_then(Value::as_str).map(str::to_string)),
            response: Mutex::new(None),
        }
    }

    fn set_response(&self, response: &Value) {
        let status = response.get("status").and_then(Value::as_u64).unwrap_or(0) as u16;
        let headers = response
            .get("headers")
            .and_then(Value::as_object)
            .map(|h| {
                h.iter()
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        *self.response.lock() = Some((status, headers));
    }
}

impl ChatRequest for ChromeRequest {
    fn method(&self) -> String {
        self.method.clone()
    }

    fn url(&self) -> String {
        self.url.clone()
    }

    fn post_data_json(&self) -> anyhow::Result<Option<Value>> {
        let mut data = self.post_data.lock().clone();
        if data.is_none() && self.has_post_data {
            let r = self.conn.call(
                Some(&self.session),
                "Network.getRequestPostData",
                json!({"requestId": self.id}),
                CALL_TIMEOUT,
            )?;
            data = r.get("postData").and_then(Value::as_str).map(str::to_string);
            self.post_data.lock().clone_from(&data);
        }
        match data {
            None => Ok(None),
            Some(d) if d.is_empty() => Ok(None),
            Some(d) => serde_json::from_str(&d)
                .map(Some)
                .map_err(|_| LaneError::err("Error", "POST data is not a valid JSON object")),
        }
    }

    fn response(&self) -> anyhow::Result<Option<Box<dyn PageResponse>>> {
        let Some((status, headers)) = self.response.lock().clone() else { return Ok(None) };
        Ok(Some(Box::new(ChromeResponse {
            conn: self.conn.clone(),
            session: self.session.clone(),
            id: self.id.clone(),
            status,
            headers,
        })))
    }
}

struct ChromeResponse {
    conn: Arc<Connection>,
    session: String,
    id: String,
    status: u16,
    headers: Vec<(String, String)>,
}

impl PageResponse for ChromeResponse {
    fn status(&self) -> u16 {
        self.status
    }

    fn header(&self, name: &str) -> String {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone()).unwrap_or_default()
    }

    fn body(&self) -> anyhow::Result<Vec<u8>> {
        let r = self.conn.call(Some(&self.session), "Network.getResponseBody", json!({"requestId": self.id}), CALL_TIMEOUT)?;
        let body = r.get("body").and_then(Value::as_str).unwrap_or("");
        if r.get("base64Encoded").and_then(Value::as_bool).unwrap_or(false) {
            Ok(base64::engine::general_purpose::STANDARD.decode(body)?)
        } else {
            Ok(body.as_bytes().to_vec())
        }
    }
}

struct ChromeRoute {
    conn: Arc<Connection>,
    session: String,
    fetch_id: String,
    request: Arc<dyn ChatRequest>,
}

impl Route for ChromeRoute {
    fn request(&self) -> Arc<dyn ChatRequest> {
        self.request.clone()
    }

    fn continue_(&self) -> anyhow::Result<()> {
        self.conn.call(Some(&self.session), "Fetch.continueRequest", json!({"requestId": self.fetch_id}), CALL_TIMEOUT)?;
        Ok(())
    }

    fn abort(&self) -> anyhow::Result<()> {
        self.conn.call(
            Some(&self.session),
            "Fetch.failRequest",
            json!({"requestId": self.fetch_id, "errorReason": "Failed"}),
            CALL_TIMEOUT,
        )?;
        Ok(())
    }
}

#[derive(Default)]
struct NetState {
    next_id: u64,
    listeners: Vec<(PageEvent, u64, RequestListener)>,
    route: Option<(String, RouteHandler)>,
    /// Network requestId → its Request, while it is in flight
    requests: HashMap<String, Arc<ChromeRequest>>,
}

/// Our session on the lane's tab: its events, its listeners and its route.
pub struct NetPage {
    conn: Arc<Connection>,
    target: String,
    session: String,
    events: std_mpsc::Receiver<Value>,
    state: Mutex<NetState>,
}

/// A Fetch `urlPattern` matching exactly `url` (`*` and `?` are its wildcards, `\` its escape).
fn exact_pattern(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for c in url.chars() {
        if matches!(c, '*' | '?' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl NetPage {
    fn attach(conn: Arc<Connection>, target: &str) -> anyhow::Result<NetPage> {
        let events = conn.subscribe(); // before attaching: no event of the new session is missed
        let attached =
            conn.call(None, "Target.attachToTarget", json!({"targetId": target, "flatten": true}), CALL_TIMEOUT)?;
        let session = attached
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| LaneError::err("Error", "the page could not be attached"))?
            .to_string();
        let net = NetPage { conn, target: target.to_string(), session, events, state: Mutex::new(NetState::default()) };
        net.call("Network.enable", json!({}))?;
        Ok(net)
    }

    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        Ok(self.conn.call(Some(&self.session), method, params, CALL_TIMEOUT)?)
    }

    /// Deliver the events that arrive within `wait` (all already queued when `wait` is zero).
    fn pump(&self, wait: Duration) {
        let deadline = Instant::now() + wait;
        loop {
            let next = match self.events.try_recv() {
                Ok(event) => Some(event),
                Err(std_mpsc::TryRecvError::Empty) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return;
                    }
                    match self.events.recv_timeout(left) {
                        Ok(event) => Some(event),
                        Err(std_mpsc::RecvTimeoutError::Timeout) => return,
                        Err(std_mpsc::RecvTimeoutError::Disconnected) => None,
                    }
                }
                Err(std_mpsc::TryRecvError::Disconnected) => None,
            };
            match next {
                Some(event) => self.dispatch(&event),
                None => {
                    // the connection is gone: the wait still lasts as long as asked
                    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                    return;
                }
            }
        }
    }

    fn emit(&self, event: PageEvent, request: &Arc<ChromeRequest>) {
        let listeners: Vec<RequestListener> =
            self.state.lock().listeners.iter().filter(|(e, _, _)| *e == event).map(|(_, _, l)| l.clone()).collect();
        let request: Arc<dyn ChatRequest> = request.clone();
        for listener in listeners {
            listener(&request);
        }
    }

    fn dispatch(&self, event: &Value) {
        if event.get("sessionId").and_then(Value::as_str) != Some(self.session.as_str()) {
            return;
        }
        let method = event.get("method").and_then(Value::as_str).unwrap_or("");
        let params = event.get("params").cloned().unwrap_or(Value::Null);
        let id = params.get("requestId").and_then(Value::as_str).unwrap_or("").to_string();
        match method {
            "Network.requestWillBeSent" => {
                if let Some(redirect) = params.get("redirectResponse") {
                    // a redirect ends this Request; the next hop is a new one
                    let old = self.state.lock().requests.remove(&id);
                    if let Some(old) = old {
                        old.set_response(redirect);
                        self.emit(PageEvent::RequestFinished, &old);
                    }
                }
                let request = params.get("request").cloned().unwrap_or(Value::Null);
                let known = self.state.lock().requests.get(&id).cloned();
                if let Some(known) = known {
                    // the route saw it first
                    let mut data = known.post_data.lock();
                    if data.is_none() {
                        *data = request.get("postData").and_then(Value::as_str).map(str::to_string);
                    }
                    return;
                }
                let made = Arc::new(ChromeRequest::from_cdp(&self.conn, &self.session, &id, &request));
                self.state.lock().requests.insert(id, made.clone());
                self.emit(PageEvent::Request, &made);
            }
            "Fetch.requestPaused" => {
                let fetch_id = id;
                let network_id = params.get("networkId").and_then(Value::as_str).unwrap_or("").to_string();
                let request = params.get("request").cloned().unwrap_or(Value::Null);
                let known = if network_id.is_empty() { None } else { self.state.lock().requests.get(&network_id).cloned() };
                let made = match known {
                    Some(r) => r,
                    None => {
                        let key = if network_id.is_empty() { fetch_id.clone() } else { network_id.clone() };
                        let made = Arc::new(ChromeRequest::from_cdp(&self.conn, &self.session, &key, &request));
                        if !network_id.is_empty() {
                            self.state.lock().requests.insert(network_id, made.clone());
                        }
                        self.emit(PageEvent::Request, &made);
                        made
                    }
                };
                let handler = self.state.lock().route.clone();
                let route = ChromeRoute {
                    conn: self.conn.clone(),
                    session: self.session.clone(),
                    fetch_id,
                    request: made.clone(),
                };
                match handler {
                    Some((url, handler)) if url == made.url => handler(&route),
                    // not ours (or the route was removed while it paused): unchanged
                    _ => {
                        let _ = route.continue_();
                    }
                }
            }
            "Network.responseReceived" => {
                let known = self.state.lock().requests.get(&id).cloned();
                if let (Some(request), Some(response)) = (known, params.get("response")) {
                    request.set_response(response);
                }
            }
            "Network.loadingFinished" => {
                let known = self.state.lock().requests.remove(&id);
                if let Some(request) = known {
                    self.emit(PageEvent::RequestFinished, &request);
                }
            }
            "Network.loadingFailed" => {
                let known = self.state.lock().requests.remove(&id);
                if let Some(request) = known {
                    self.emit(PageEvent::RequestFailed, &request);
                }
            }
            _ => {}
        }
    }
}

impl Drop for NetPage {
    fn drop(&mut self) {
        if self.state.get_mut().route.take().is_some() {
            self.conn.send_nowait(Some(&self.session), "Fetch.disable", json!({}));
        }
        self.conn.send_nowait(None, "Target.detachFromTarget", json!({"sessionId": self.session}));
    }
}

// ---- the page -----------------------------------------------------------------------------------------------

struct ChromePage<'a> {
    page: &'a dyn PageHandle,
    net: &'a NetPage,
}

impl ChatPage for ChromePage<'_> {
    fn url(&self) -> String {
        self.page.url()
    }

    fn title(&self) -> anyhow::Result<String> {
        self.page.title()
    }

    fn goto(&self, url: &str, wait_until: &str, timeout_ms: u64) -> anyhow::Result<()> {
        self.page.goto(url, wait_until, timeout_ms)
    }

    fn locator(&self, css: &str) -> Box<dyn Locator + '_> {
        Box::new(ChromeLocator { page: self.page, chain: vec![Step::Css(css.to_string())] })
    }

    fn route(&self, url: &str, handler: RouteHandler) -> anyhow::Result<()> {
        self.net.pump(Duration::ZERO); // what happened before the route is not the route's
        self.net.call(
            "Fetch.enable",
            json!({"patterns": [{"urlPattern": exact_pattern(url), "requestStage": "Request"}]}),
        )?;
        self.net.state.lock().route = Some((url.to_string(), handler));
        Ok(())
    }

    fn unroute(&self, url: &str) -> anyhow::Result<()> {
        let removed = {
            let mut st = self.net.state.lock();
            match &st.route {
                Some((u, _)) if u == url => st.route.take(),
                _ => None,
            }
        };
        if removed.is_some() {
            self.net.call("Fetch.disable", json!({}))?;
        }
        Ok(())
    }

    fn on(&self, event: PageEvent, callback: RequestListener) -> anyhow::Result<u64> {
        self.net.pump(Duration::ZERO); // a listener hears only what happens after it is added
        let mut st = self.net.state.lock();
        st.next_id += 1;
        let id = st.next_id;
        st.listeners.push((event, id, callback));
        Ok(id)
    }

    fn remove_listener(&self, event: PageEvent, id: u64) -> anyhow::Result<()> {
        self.net.state.lock().listeners.retain(|(e, i, _)| !(*e == event && *i == id));
        Ok(())
    }

    fn wait_for_timeout(&self, ms: u64) {
        self.net.pump(Duration::from_millis(ms));
    }
}

// ---- locators -----------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Css(String),
    Role { role: String, name: String, exact: bool },
    First,
}

fn chain_json(chain: &[Step]) -> Value {
    Value::Array(
        chain
            .iter()
            .map(|s| match s {
                Step::Css(css) => json!({"css": css}),
                Step::Role { role, name, exact } => json!({"role": role, "name": name, "exact": exact}),
                Step::First => json!({"first": true}),
            })
            .collect(),
    )
}

/// Resolves a locator chain in the page, and the element checks Playwright makes (visible, enabled, its role
/// and accessible name), then runs one operation on what it found. Returns `{n: matches, v: value}`.
const LOCATE_JS: &str = r#"(args) => {
  const norm = s => String(s || '').replace(/\s+/g, ' ').trim();
  const styleOf = el => (el.ownerDocument.defaultView || window).getComputedStyle(el);
  const visible = el => {
    const s = styleOf(el);
    if (!s) return true;
    if (s.display === 'contents') {
      for (const child of el.children) if (visible(child)) return true;
      return false;
    }
    if (typeof el.checkVisibility === 'function' && !el.checkVisibility()) return false;
    if (s.visibility !== 'visible') return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  };
  const hiddenForAria = el => {
    for (let e = el; e && e.nodeType === 1; e = e.parentElement) {
      if (e.getAttribute('aria-hidden') === 'true') return true;
      if (styleOf(e).display === 'none') return true;
    }
    const v = styleOf(el).visibility;
    return v === 'hidden' || v === 'collapse';
  };
  const roleOf = el => {
    const explicit = (el.getAttribute('role') || '').trim().split(/\s+/)[0];
    if (explicit) return explicit.toLowerCase();
    const tag = el.tagName.toUpperCase();
    if (tag === 'BUTTON') return 'button';
    if (tag === 'TEXTAREA') return 'textbox';
    if (tag === 'INPUT') {
      const type = (el.getAttribute('type') || 'text').toLowerCase();
      if (['button', 'submit', 'reset', 'image'].includes(type)) return 'button';
      if (['text', 'email', 'tel', 'url', ''].includes(type)) return el.hasAttribute('list') ? 'combobox' : 'textbox';
      if (type === 'search') return el.hasAttribute('list') ? 'combobox' : 'searchbox';
    }
    return '';
  };
  const textOf = node => {
    let out = '';
    for (const child of node.childNodes) {
      if (child.nodeType === 3) { out += child.textContent; continue; }
      if (child.nodeType !== 1) continue;
      if (child.getAttribute('aria-hidden') === 'true') continue;
      const s = styleOf(child);
      if (s.display === 'none' || s.visibility === 'hidden') continue;
      const label = (child.getAttribute('aria-label') || '').trim();
      const inline = s.display === 'inline';
      const pad = inline ? '' : ' ';
      if (label) { out += ' ' + label + ' '; continue; }
      if (child.tagName.toUpperCase() === 'IMG') { out += ' ' + (child.getAttribute('alt') || '') + ' '; continue; }
      out += pad + textOf(child) + pad;
    }
    return out;
  };
  const nameOf = el => {
    const by = el.getAttribute('aria-labelledby');
    if (by) {
      const n = norm(by.split(/\s+/).map(id => { const t = document.getElementById(id); return t ? textOf(t) : ''; }).join(' '));
      if (n) return n;
    }
    const aria = norm(el.getAttribute('aria-label'));
    if (aria) return aria;
    const tag = el.tagName.toUpperCase();
    if (tag === 'INPUT') {
      const type = (el.getAttribute('type') || 'text').toLowerCase();
      if (['button', 'submit', 'reset'].includes(type)) {
        const v = norm(el.value);
        if (v) return v;
        return type === 'submit' ? 'Submit' : type === 'reset' ? 'Reset' : norm(el.title);
      }
    }
    if (el.labels && el.labels.length) {
      const l = norm(Array.from(el.labels).map(textOf).join(' '));
      if (l) return l;
    }
    if (roleOf(el) === 'button' || ['A', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'OPTION', 'SUMMARY'].includes(tag)) {
      const t = norm(textOf(el));
      if (t) return t;
    }
    const title = norm(el.title);
    if (title) return title;
    if (tag === 'TEXTAREA' || tag === 'INPUT') return norm(el.getAttribute('placeholder'));
    return '';
  };
  const resolve = chain => {
    let scope = [document];
    for (const step of chain) {
      let next = [];
      if (step.first) next = scope.length ? [scope[0]] : [];
      else for (const root of scope) {
        if (step.css !== undefined) next.push(...root.querySelectorAll(step.css));
        else next.push(...Array.from(root.querySelectorAll('*')).filter(el => {
          if (roleOf(el) !== step.role || hiddenForAria(el)) return false;
          const name = nameOf(el), want = norm(step.name);
          return step.exact ? name === want : name.toLowerCase().includes(want.toLowerCase());
        }));
      }
      scope = Array.from(new Set(next));
    }
    return scope.filter(e => e !== document);
  };
  const disabled = el => {
    const tag = el.tagName.toUpperCase();
    if (['BUTTON', 'INPUT', 'SELECT', 'TEXTAREA', 'OPTION', 'OPTGROUP'].includes(tag)) {
      if (el.hasAttribute('disabled')) return true;
      const fieldset = el.closest('fieldset[disabled]');
      if (fieldset) {
        const legend = fieldset.querySelector(':scope > legend');
        if (!legend || !legend.contains(el)) return true;
      }
    }
    for (let e = el; e && e.nodeType === 1; e = e.parentElement) if (e.getAttribute('aria-disabled') === 'true') return true;
    return false;
  };
  const els = resolve(args.chain);
  if (args.op === 'elements') return els;
  const n = els.length;
  if (n !== 1) return { n, v: args.op === 'visible' && n > 0 ? visible(els[0]) : null };
  const el = els[0];
  switch (args.op) {
    case 'count': return { n };
    case 'visible': return { n, v: visible(el) };
    case 'enabled': return { n, v: !disabled(el) };
    case 'attr': return { n, v: el.getAttribute(args.name) };
    case 'value': {
      const tag = el.tagName.toUpperCase();
      if (!['INPUT', 'TEXTAREA', 'SELECT'].includes(tag)) return { n, error: 'Node is not an <input>, <textarea> or <select> element' };
      return { n, v: el.value };
    }
    case 'mark': el.setAttribute('data-eve-america', args.token); return { n };
    default: return { n, error: 'unknown locator operation' };
  }
}"#;

const UNMARK_JS: &str =
    "(token) => { for (const el of document.querySelectorAll(`[data-eve-america=\"${token}\"]`)) el.removeAttribute('data-eve-america'); }";

/// A Playwright locator over the lane's page: re-resolved in the page on every call.
struct ChromeLocator<'a> {
    page: &'a dyn PageHandle,
    chain: Vec<Step>,
}

impl ChromeLocator<'_> {
    fn child(&self, step: Step) -> Box<dyn Locator + '_> {
        let mut chain = self.chain.clone();
        chain.push(step);
        Box::new(ChromeLocator { page: self.page, chain })
    }

    fn op(&self, op: &str, extra: Value) -> anyhow::Result<Value> {
        let mut args = json!({"chain": chain_json(&self.chain), "op": op});
        if let (Value::Object(a), Value::Object(e)) = (&mut args, extra) {
            a.extend(e);
        }
        self.page.evaluate(LOCATE_JS, Some(&args))
    }

    fn matches(r: &Value) -> usize {
        r.get("n").and_then(Value::as_u64).unwrap_or(0) as usize
    }

    /// Wait (up to `timeout_ms`) for exactly one element, then `op` on it: Playwright's strict locator.
    fn one(&self, api: &str, timeout_ms: u64, mut op: impl FnMut() -> anyhow::Result<Value>) -> anyhow::Result<Value> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let r = op()?;
            match Self::matches(&r) {
                1 => {
                    if let Some(e) = r.get("error").and_then(Value::as_str) {
                        return Err(LaneError::err("Error", format!("{api}: Error: {e}")));
                    }
                    return Ok(r.get("v").cloned().unwrap_or(Value::Null));
                }
                0 => {}
                n => {
                    return Err(LaneError::err(
                        "Error",
                        format!("{api}: Error: strict mode violation: locator resolved to {n} elements"),
                    ))
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(LaneError::err("TimeoutError", format!("{api}: Timeout {timeout_ms}ms exceeded.")));
            }
            self.page.wait_for_timeout(left.min(LOOK_AGAIN).as_millis() as u64);
        }
    }

    /// Mark the one element, act on it through the lane's page (Playwright's actionability waits and real
    /// input), and take the mark off again.
    fn act(&self, api: &str, timeout_ms: u64, act: impl FnOnce(&str, u64) -> anyhow::Result<()>) -> anyhow::Result<()> {
        let started = Instant::now();
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.one(api, timeout_ms, || self.op("mark", json!({"token": token})))?;
        let left = (timeout_ms as u128).saturating_sub(started.elapsed().as_millis()).max(1) as u64;
        let result = act(&format!("[data-eve-america=\"{token}\"]"), left);
        let _ = self.page.evaluate(UNMARK_JS, Some(&json!(token)));
        result
    }
}

impl Locator for ChromeLocator<'_> {
    fn locator(&self, css: &str) -> Box<dyn Locator + '_> {
        self.child(Step::Css(css.to_string()))
    }

    fn get_by_role(&self, role: &str, name: &str, exact: bool) -> Box<dyn Locator + '_> {
        self.child(Step::Role { role: role.to_string(), name: name.to_string(), exact })
    }

    fn count(&self) -> anyhow::Result<usize> {
        Ok(Self::matches(&self.op("count", json!({}))?))
    }

    fn is_visible(&self) -> anyhow::Result<bool> {
        let r = self.op("visible", json!({}))?;
        let n = Self::matches(&r);
        if n > 1 {
            return Err(LaneError::err(
                "Error",
                format!("Locator.is_visible: Error: strict mode violation: locator resolved to {n} elements"),
            ));
        }
        Ok(r.get("v").and_then(Value::as_bool).unwrap_or(false))
    }

    fn is_enabled(&self) -> anyhow::Result<bool> {
        let v = self.one("Locator.is_enabled", DEFAULT_TIMEOUT_MS, || self.op("enabled", json!({})))?;
        Ok(v.as_bool().unwrap_or(false))
    }

    fn wait_for_visible(&self, timeout_ms: u64) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let r = self.op("visible", json!({}))?;
            let n = Self::matches(&r);
            if n > 1 {
                return Err(LaneError::err(
                    "Error",
                    format!("Locator.wait_for: Error: strict mode violation: locator resolved to {n} elements"),
                ));
            }
            if n == 1 && r.get("v").and_then(Value::as_bool) == Some(true) {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(LaneError::err("TimeoutError", format!("Locator.wait_for: Timeout {timeout_ms}ms exceeded.")));
            }
            self.page.wait_for_timeout(left.min(LOOK_AGAIN).as_millis() as u64);
        }
    }

    fn evaluate(&self, code: &str, arg: Option<&Value>) -> anyhow::Result<Value> {
        let script = format!(
            "(args) => {{ const els = ({LOCATE_JS})({{chain: args.chain, op: 'elements'}}); \
             if (els.length !== 1) return {{n: els.length}}; \
             return {{n: 1, v: ({code}\n)(els[0], args.arg)}}; }}"
        );
        let args = json!({"chain": chain_json(&self.chain), "arg": arg.cloned().unwrap_or(Value::Null)});
        self.one("Locator.evaluate", DEFAULT_TIMEOUT_MS, || self.page.evaluate(&script, Some(&args)))
    }

    fn get_attribute(&self, name: &str) -> anyhow::Result<Option<String>> {
        let v = self.one("Locator.get_attribute", DEFAULT_TIMEOUT_MS, || self.op("attr", json!({"name": name})))?;
        Ok(v.as_str().map(str::to_string))
    }

    fn fill(&self, text: &str, timeout_ms: u64) -> anyhow::Result<()> {
        self.act("Locator.fill", timeout_ms, |selector, left| self.page.fill(selector, text, left))
    }

    fn input_value(&self, timeout_ms: u64) -> anyhow::Result<String> {
        let v = self.one("Locator.input_value", timeout_ms, || self.op("value", json!({})))?;
        Ok(v.as_str().unwrap_or("").to_string())
    }

    fn click(&self, timeout_ms: u64) -> anyhow::Result<()> {
        self.act("Locator.click", timeout_ms, |selector, left| self.page.click(selector, left))
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn exact_pattern_escapes_the_fetch_wildcards() {
        assert_eq!(exact_pattern(crate::API_URL), "https://america.gov/api/chat");
        assert_eq!(exact_pattern("https://a.gov/x?y=*\\"), "https://a.gov/x\\?y=\\*\\\\");
    }

    #[test]
    fn chain_json_names_each_step() {
        let chain = vec![
            Step::Css("form".into()),
            Step::Role { role: "button".into(), name: "Send".into(), exact: true },
            Step::First,
        ];
        assert_eq!(
            chain_json(&chain),
            json!([{"css": "form"}, {"role": "button", "name": "Send", "exact": true}, {"first": true}])
        );
    }
}
