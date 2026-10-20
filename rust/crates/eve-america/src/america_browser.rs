//! Port of `eve_bridge.america_browser`: a reusable normal-Chrome America.gov chat flow, with Jev control
//! recovery.
//!
//! Only UI fill/click submits a question. The answer is read from that UI request's completed SSE, not guessed
//! from a pause in text growth. Browser connection and semantic locator recipes persist; every question starts
//! a fresh document.
//!
//! The Python flow drives Playwright objects. Here they are the traits below ([`ChatPage`], [`Locator`],
//! [`ChatRequest`], [`Route`]) and the lane is [`AmericaLane`] / [`LaneWorker`]; [`crate::chrome`] fills them
//! with eve-browser-lane's Chrome page and the DevTools protocol's request interception, and the tests fill
//! them with the same fakes the Python tests use.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use eve_browser_lane::BrowserLaneConfig;
use eve_desktop_lanes::ax_candidates::{Candidate, Snapshot};
use eve_desktop_lanes::fast_lane::StepAsk;
use eve_desktop_lanes::{py_collapse, py_round_n};
use eve_typed_ask::{StepAnswer, JEV_STEP_GATES};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::{budget, parse_stream, ProtocolError, Source, AMERICA_URL, API_URL, MAX_STREAM_BYTES};

pub const FORM: &str = r#"form[data-slot="chat-input-shell"][aria-label="Describe what you need"]"#;
pub const FIELD: &str = r#"textarea[name="message"]"#;
pub const POLL_MS: u64 = 100;
pub const CONTROL_MS: u64 = 3000;
/// Read accessible labels without the generic collector's current-value fallback.
pub const NAME_JS: &str = r#"el => {
  const by = el.getAttribute('aria-labelledby');
  if (by) { const name = by.split(/\s+/).map(id => document.getElementById(id)?.textContent || '').join(' ').trim();
    if (name) return name; }
  const aria = el.getAttribute('aria-label'); if (aria) return aria.trim();
  const labels = Array.from(el.labels || []).map(label => label.textContent || '').join(' ').trim();
  if (labels) return labels;
  if (el.tagName === 'TEXTAREA') return (el.title || el.getAttribute('placeholder') || '').trim();
  return (el.textContent || el.title || '').trim();
}"#;
const TAG_JS: &str = "el => el.tagName.toLowerCase()";
const CLOSEST_JS: &str = "(el, selector) => !!el.closest(selector)";
const RECENT_IDS: usize = 128;

/// Static owner-safe failure text; never an upstream body or private question.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BrowserFailure(pub String);

/// The whole question outlived its budget (Python's `TimeoutError` out of `ask`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("TimeoutError")]
pub struct BrowserTimeout;

fn fail(why: &str) -> anyhow::Error {
    anyhow::Error::new(BrowserFailure(why.to_string()))
}

/// One parsed answer: the text and its official sources.
pub type Answer = (String, Vec<Source>);

// ---- the Playwright surface the flow touches --------------------------------------------------------------------

/// A Playwright locator: lazy, re-resolved on every call, strict (an action on more than one element fails).
pub trait Locator {
    /// `locator.locator(css)`: matching descendants.
    fn locator(&self, css: &str) -> Box<dyn Locator + '_>;
    /// `locator.get_by_role(role, name=name, exact=exact)`.
    fn get_by_role(&self, role: &str, name: &str, exact: bool) -> Box<dyn Locator + '_>;
    fn count(&self) -> anyhow::Result<usize>;
    fn is_visible(&self) -> anyhow::Result<bool>;
    fn is_enabled(&self) -> anyhow::Result<bool>;
    /// `locator.wait_for(state="visible", timeout=)`.
    fn wait_for_visible(&self, timeout_ms: u64) -> anyhow::Result<()>;
    /// `locator.evaluate(code, arg)`: `code` takes the element (and the argument).
    fn evaluate(&self, code: &str, arg: Option<&Value>) -> anyhow::Result<Value>;
    fn get_attribute(&self, name: &str) -> anyhow::Result<Option<String>>;
    fn fill(&self, text: &str, timeout_ms: u64) -> anyhow::Result<()>;
    fn input_value(&self, timeout_ms: u64) -> anyhow::Result<String>;
    fn click(&self, timeout_ms: u64) -> anyhow::Result<()>;
}

/// A Playwright response to one page request.
pub trait PageResponse {
    fn status(&self) -> u16;
    /// `response.headers.get(name, "")` (names are lowercase).
    fn header(&self, name: &str) -> String;
    fn body(&self) -> anyhow::Result<Vec<u8>>;
}

/// A Playwright request the page made.
pub trait ChatRequest: Send + Sync {
    fn method(&self) -> String;
    fn url(&self) -> String;
    /// `request.post_data_json`: None without a body; an error for a body that is not JSON.
    fn post_data_json(&self) -> anyhow::Result<Option<Value>>;
    fn response(&self) -> anyhow::Result<Option<Box<dyn PageResponse>>>;
}

/// A routed (paused) request: continue it unchanged or abort it.
pub trait Route {
    fn request(&self) -> Arc<dyn ChatRequest>;
    fn continue_(&self) -> anyhow::Result<()>;
    fn abort(&self) -> anyhow::Result<()>;
}

/// The page events the flow listens to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageEvent {
    Request,
    RequestFinished,
    RequestFailed,
}

impl PageEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            PageEvent::Request => "request",
            PageEvent::RequestFinished => "requestfinished",
            PageEvent::RequestFailed => "requestfailed",
        }
    }
}

pub type RouteHandler = Arc<dyn Fn(&dyn Route) + Send + Sync>;
pub type RequestListener = Arc<dyn Fn(&Arc<dyn ChatRequest>) + Send + Sync>;

/// A Playwright page, as far as the America flow drives it. Events and routes are delivered while the page is
/// being driven (Playwright's sync dispatch), at the latest during `wait_for_timeout`.
pub trait ChatPage {
    fn url(&self) -> String;
    fn title(&self) -> anyhow::Result<String>;
    fn goto(&self, url: &str, wait_until: &str, timeout_ms: u64) -> anyhow::Result<()>;
    fn locator(&self, css: &str) -> Box<dyn Locator + '_>;
    /// `page.route(url, handler)`: the one handler for that exact URL.
    fn route(&self, url: &str, handler: RouteHandler) -> anyhow::Result<()>;
    /// `page.unroute(url, handler)`.
    fn unroute(&self, url: &str) -> anyhow::Result<()>;
    /// `page.on(event, callback)`: an id for `remove_listener`.
    fn on(&self, event: PageEvent, callback: RequestListener) -> anyhow::Result<u64>;
    fn remove_listener(&self, event: PageEvent, id: u64) -> anyhow::Result<()>;
    fn wait_for_timeout(&self, ms: u64);
}

/// The browser lane as the flow sees it on the lane's own thread (`self.lane` inside `_ask`).
pub trait LaneWorker {
    /// `lane.config.attach`.
    fn attach(&self) -> bool;
    /// `lane._ensure()`: connect and open the lane's page when needed.
    fn ensure(&mut self) -> anyhow::Result<()>;
    /// `wrapper.page` after `ensure`.
    fn page(&self) -> Option<Box<dyn ChatPage + '_>>;
    /// `wrapper.snapshot()`: the lane's look at the page.
    fn snapshot(&self) -> anyhow::Result<Snapshot>;
    /// `wrapper._locator(candidate)`: `[data-eve-id="…"]`, first match.
    fn candidate_locator(&self, c: &Candidate) -> anyhow::Result<Box<dyn Locator + '_>>;
    /// `_reset`: close the lane's page (aborting an abandoned stream) and forget it.
    fn reset(&mut self);
}

/// One job for the lane's thread.
pub type Job = Box<dyn FnOnce(&mut dyn LaneWorker) -> Result<Answer, BrowserFailure> + Send>;

/// The lane: its configuration, its one worker thread (`lane._run`), and `lane.close()`.
#[async_trait]
pub trait AmericaLane: Send + Sync {
    fn config(&self) -> BrowserLaneConfig;
    /// Run `job` on the lane's thread; an `Err` is the lane's own failure (its thread is gone).
    async fn run(&self, job: Job) -> anyhow::Result<Result<Answer, BrowserFailure>>;
    async fn close(&self) -> anyhow::Result<()>;
}

// ---- recipes ------------------------------------------------------------------------------------------------

/// A semantic way to find a control: a CSS selector, or a role and its exact accessible name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recipe {
    pub role: String,
    pub name: String,
    pub css: String,
}

impl Recipe {
    pub fn css(css: &str) -> Recipe {
        Recipe { css: css.to_string(), ..Recipe::default() }
    }

    pub fn role(role: &str, name: &str) -> Recipe {
        Recipe { role: role.to_string(), name: name.to_string(), css: String::new() }
    }

    pub fn locate<'f>(&self, form: &'f dyn Locator) -> Box<dyn Locator + 'f> {
        if self.css.is_empty() {
            form.get_by_role(&self.role, &self.name, true)
        } else {
            form.locator(&self.css)
        }
    }
}

// ---- the flow -----------------------------------------------------------------------------------------------

struct Shared {
    field: Recipe,
    send: Recipe,
    recent_ids: VecDeque<String>,
    last_metrics: Value,
}

/// Where the flow goes: America.gov's chat page and its chat API. Only the tests point them elsewhere (a
/// loopback page), to drive a real Chrome through the whole flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Site {
    pub(crate) chat_url: String,
    pub(crate) api_url: String,
}

impl Default for Site {
    fn default() -> Self {
        Site { chat_url: AMERICA_URL.to_string(), api_url: API_URL.to_string() }
    }
}

pub(crate) struct Inner {
    site: Site,
    shared: Mutex<Shared>,
    asker: Option<Arc<dyn StepAsk + Send + Sync>>,
    timeout_secs: f64,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Field,
    Send,
}

fn unique_visible(locator: &dyn Locator, enabled: bool) -> anyhow::Result<bool> {
    Ok(locator.count()? == 1 && locator.is_visible()? && (!enabled || locator.is_enabled()?))
}

/// Python truthiness of a value a page returned.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn is_text(v: &Value, want: &str) -> bool {
    v.as_str() == Some(want)
}

fn target(request: &dyn ChatRequest, api_url: &str) -> bool {
    request.method() == "POST" && request.url() == api_url
}

/// A string of 1..=256 characters.
fn short_str(v: Option<&Value>) -> bool {
    v.and_then(Value::as_str).is_some_and(|s| (1..=256).contains(&s.chars().count()))
}

/// `_correlate`: the browser's request body is exactly one fresh user turn carrying `text`; its chat id.
fn correlate(request: &dyn ChatRequest, text: &str) -> Result<String, BrowserFailure> {
    let refused = || {
        BrowserFailure(
            "America.gov's browser request did not match this fresh question. It will not be resent.".to_string(),
        )
    };
    let body = request.post_data_json().map_err(|_| refused())?.ok_or_else(refused)?;
    let body = body.as_object().ok_or_else(refused)?;
    let messages = body.get("messages").and_then(Value::as_array).ok_or_else(refused)?;
    let message = messages.first().and_then(Value::as_object).ok_or_else(refused)?;
    let keys = |m: &serde_json::Map<String, Value>, want: &[&str]| m.len() == want.len() && want.iter().all(|k| m.contains_key(*k));
    let parts = json!([{"type": "text", "text": text}]);
    if !keys(body, &["language", "id", "messages", "trigger"])
        || !short_str(body.get("id"))
        || !keys(message, &["id", "role", "parts"])
        || !short_str(message.get("id"))
        || body.get("language").and_then(Value::as_str) != Some("en")
        || body.get("trigger").and_then(Value::as_str) != Some("submit-message")
        || messages.len() != 1
        || message.get("role").and_then(Value::as_str) != Some("user")
        || message.get("parts") != Some(&parts)
    {
        return Err(refused());
    }
    Ok(body["id"].as_str().unwrap_or_default().to_string())
}

impl Inner {
    fn metrics_add_jev_call(&self) {
        let mut s = self.shared.lock();
        let n = s.last_metrics.get("jev_calls").and_then(Value::as_i64).unwrap_or(0);
        s.last_metrics["jev_calls"] = json!(n + 1);
    }

    fn recover<'f>(
        &self,
        worker: &dyn LaneWorker,
        form: &'f dyn Locator,
        kind: Kind,
    ) -> anyhow::Result<(Box<dyn Locator + 'f>, Recipe)> {
        let Some(asker) = &self.asker else {
            return Err(fail("America.gov's chat controls changed and Jev is unavailable. Open its browser chat."));
        };
        let field = kind == Kind::Field;
        let snap = worker.snapshot()?;
        let mut candidates: Vec<(Candidate, String)> = Vec::new();
        for candidate in &snap.elements {
            let wanted = if field { candidate.editable } else { candidate.role == "button" };
            if !wanted {
                continue;
            }
            let loc = worker.candidate_locator(candidate)?;
            if loc.count()? == 1 && truthy(&loc.evaluate(CLOSEST_JS, Some(&json!(FORM)))?) {
                let tag = loc.evaluate(TAG_JS, None)?;
                if !is_text(&tag, if field { "textarea" } else { "button" }) {
                    continue;
                }
                if !field && loc.get_attribute("type")?.as_deref() != Some("submit") {
                    continue;
                }
                if let Value::String(name) = loc.evaluate(NAME_JS, None)? {
                    let name = py_collapse(&name);
                    if (1..=120).contains(&name.chars().count()) {
                        candidates.push((candidate.clone(), name));
                    }
                }
            }
        }
        // The static objective, chat URL and labels are enough. Never send field values, page prose, legal
        // question or answer to the grounding provider.
        let options: IndexMap<String, String> = candidates
            .iter()
            .take(12)
            .enumerate()
            .map(|(i, (c, name))| (i.to_string(), format!("{}: {name}", c.role)))
            .collect();
        let step = if field { "Find the chat question textbox" } else { "Find the Send button for the chat question" };
        let answer = asker.ask(step, "America.gov", AMERICA_URL, &options, &[]);
        self.metrics_add_jev_call();
        // The owner already authorized sending this America question. Only this verified chat-submit operation
        // can use that authorization; confidence, presence, margin and already-done gates still apply unchanged.
        let answer = if field { answer } else { StepAnswer { risky: 0.0, ..answer } };
        let selected = JEV_STEP_GATES.decide(&answer);
        if !options.contains_key(&selected) {
            return Err(fail("Jev could not safely identify America.gov's chat control. Open its browser chat."));
        }
        let index: usize = selected.parse()?;
        let name = candidates[index].1.clone();
        // Cache a semantic recipe, never a per-snapshot data-eve-id or a screen point.
        let recipe = Recipe::role(if field { "textbox" } else { "button" }, &name);
        let locator = recipe.locate(form);
        if !unique_visible(&*locator, field)? {
            return Err(fail("America.gov's selected chat control changed before use. Open its browser chat."));
        }
        if field && !is_text(&locator.evaluate(TAG_JS, None)?, "textarea") {
            return Err(fail("America.gov did not expose a supported chat textarea. Open its browser chat."));
        }
        if !field
            && (!is_text(&locator.evaluate(TAG_JS, None)?, "button")
                || locator.get_attribute("type")?.as_deref() != Some("submit"))
        {
            return Err(fail("America.gov did not expose a supported chat submit button. Open its browser chat."));
        }
        Ok((locator, recipe))
    }

    /// `_controls`: the question field and the Send button inside the chat form.
    #[allow(clippy::type_complexity)]
    fn controls<'f>(
        &self,
        worker: &dyn LaneWorker,
        form: &'f dyn Locator,
    ) -> anyhow::Result<(Box<dyn Locator + 'f>, Box<dyn Locator + 'f>)> {
        if !unique_visible(form, false)? {
            return Err(fail("America.gov's chat form is unavailable. Open its browser chat."));
        }
        let (field_recipe, send_recipe) = {
            let s = self.shared.lock();
            (s.field.clone(), s.send.clone())
        };
        let mut field = field_recipe.locate(form);
        let mut send = send_recipe.locate(form);
        if !unique_visible(&*field, true)? {
            let (located, recipe) = self.recover(worker, form, Kind::Field)?;
            field = located;
            self.shared.lock().field = recipe;
        }
        if !unique_visible(&*send, false)? {
            // Older observed accessible name was Send; still an exact semantic match.
            let alternative = Recipe::role("button", "Send");
            let located = alternative.locate(form);
            if unique_visible(&*located, false)? {
                send = located;
                self.shared.lock().send = alternative;
            } else {
                let (located, recipe) = self.recover(worker, form, Kind::Send)?;
                send = located;
                self.shared.lock().send = recipe;
            }
        }
        if !is_text(&field.evaluate(TAG_JS, None)?, "textarea") || send.get_attribute("type")?.as_deref() != Some("submit") {
            return Err(fail("America.gov's chat controls are incompatible. Open its browser chat."));
        }
        Ok((field, send))
    }
}

/// What `_ask` registered on the page, removed again whatever happens.
#[derive(Default)]
struct Registered {
    route: bool,
    listeners: Vec<(PageEvent, u64)>,
}

impl Registered {
    fn undo(&mut self, page: &dyn ChatPage, api_url: &str) {
        if std::mem::take(&mut self.route) {
            let _ = page.unroute(api_url); // the page may already be closed
        }
        for (event, id) in self.listeners.drain(..) {
            let _ = page.remove_listener(event, id);
        }
    }
}

#[derive(Default)]
struct Preflight {
    refused: bool,
    accepted: bool,
}

type Bucket = Arc<Mutex<Vec<Arc<dyn ChatRequest>>>>;

fn same_request(a: &Arc<dyn ChatRequest>, b: &Arc<dyn ChatRequest>) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
}

/// `_ask`: runs only on the lane's thread.
fn ask_sync(
    inner: &Arc<Inner>,
    worker: &mut dyn LaneWorker,
    question: &str,
    jurisdiction: &str,
    cancelled: &AtomicBool,
) -> Result<Answer, BrowserFailure> {
    let started = (inner.clock)();
    inner.shared.lock().last_metrics = json!({"jev_calls": 0, "submitted": false});
    let text = format!("Jurisdiction: {jurisdiction}\nQuestion: {question}");
    let mut registered = Registered::default();
    let outcome = flow(inner, worker, &text, started, cancelled, &mut registered);
    match outcome {
        Ok(answer) => Ok(answer),
        Err(e) => {
            worker.reset();
            if let Some(f) = e.downcast_ref::<BrowserFailure>() {
                return Err(f.clone());
            }
            // No raw question, body, browser path or Jev error in the output.
            let submitted = inner.shared.lock().last_metrics.get("submitted") == Some(&Value::Bool(true));
            let suffix = if submitted { " The question may have been sent; it will not be resent." } else { "" };
            Err(BrowserFailure(format!("America.gov's browser flow could not complete.{suffix} Open its browser chat.")))
        }
    }
}

fn flow(
    inner: &Arc<Inner>,
    worker: &mut dyn LaneWorker,
    text: &str,
    started: f64,
    cancelled: &AtomicBool,
    registered: &mut Registered,
) -> anyhow::Result<Answer> {
    let site = &inner.site;
    let remaining = || -> Result<u64, BrowserFailure> {
        let left = inner.timeout_secs - ((inner.clock)() - started);
        if cancelled.load(Ordering::SeqCst) || left <= 0.0 || left.is_nan() {
            return Err(BrowserFailure(
                "America.gov's browser question was stopped or timed out. It will not be resent.".to_string(),
            ));
        }
        Ok(((left * 1000.0) as u64).max(1))
    };
    if !worker.attach() {
        return Err(fail("America.gov needs Eve's normal Chrome profile. Configure EVE_BROWSER_OWN=1."));
    }
    worker.ensure()?;
    let worker: &dyn LaneWorker = worker;
    let page = worker.page().ok_or_else(|| anyhow::anyhow!("the browser lane opened no page"))?;
    let page: &dyn ChatPage = &*page;
    let result = (|| -> anyhow::Result<Answer> {
        remaining()?;
        // Unmount the prior app before loading /chat: its history is browser memory. Do not reuse an owner's
        // tab or replay previous messages.
        page.goto("about:blank", "domcontentloaded", remaining()?.min(3000))?;
        page.goto(&site.chat_url, "domcontentloaded", remaining()?.min(15000))?;
        let parsed = eve_web::watch::net::urlsplit(&page.url()).map_err(|_| anyhow::anyhow!("ValueError"))?;
        // https://america.gov, at / or /chat
        let chat = eve_web::watch::net::urlsplit(&site.chat_url).map_err(|_| anyhow::anyhow!("ValueError"))?;
        if parsed.scheme != chat.scheme || parsed.netloc != chat.netloc || !(parsed.path == "/" || parsed.path == chat.path) {
            return Err(fail("America.gov did not open its ordinary chat page. Open it in your browser."));
        }
        if page.title()? == "Just a moment..." {
            return Err(fail("America.gov is showing a browser security check. Open its chat in your browser."));
        }
        let form = page.locator(FORM);
        form.wait_for_visible(remaining()?.min(10000))?;
        let (field, send) = inner.controls(worker, &*form)?;
        remaining()?;
        field.fill(text, remaining()?.min(CONTROL_MS))?;
        if field.input_value(remaining()?.min(CONTROL_MS))? != text {
            return Err(fail("America.gov's question field did not contain the complete question."));
        }
        if !unique_visible(&*send, true)? {
            return Err(fail("America.gov's Send button is unavailable. Open its browser chat."));
        }

        let preflight = Arc::new(Mutex::new(Preflight::default()));
        let guard: RouteHandler = {
            let (inner, preflight, text) = (inner.clone(), preflight.clone(), text.to_string());
            Arc::new(move |route: &dyn Route| {
                let request = route.request();
                let fresh = correlate(&*request, &text).ok().filter(|chat_id| {
                    let mut shared = inner.shared.lock();
                    let mut state = preflight.lock();
                    if state.accepted || shared.recent_ids.contains(chat_id) {
                        return false; // a repeated chat request
                    }
                    state.accepted = true;
                    if shared.recent_ids.len() == RECENT_IDS {
                        shared.recent_ids.pop_front();
                    }
                    shared.recent_ids.push_back(chat_id.clone());
                    true
                });
                if fresh.is_some() {
                    let _ = route.continue_(); // pass the browser-generated body through unchanged
                } else {
                    preflight.lock().refused = true;
                    let _ = route.abort();
                }
            })
        };
        // Reject restored history/metadata before any question leaves Chrome. This does not build or replay an
        // HTTP request.
        page.route(&site.api_url, guard)?;
        registered.route = true;

        let (requests, finished, failed): (Bucket, Bucket, Bucket) = Default::default();
        // Listen before the one submit attempt; even a click exception can follow a successful send. Never infer
        // that such a send is safe to repeat.
        for (event, bucket) in [
            (PageEvent::Request, &requests),
            (PageEvent::RequestFinished, &finished),
            (PageEvent::RequestFailed, &failed),
        ] {
            let bucket = bucket.clone();
            let api_url = site.api_url.clone();
            let callback: RequestListener = Arc::new(move |request: &Arc<dyn ChatRequest>| {
                if target(&**request, &api_url) {
                    bucket.lock().push(request.clone());
                }
            });
            let id = page.on(event, callback)?;
            registered.listeners.push((event, id));
        }
        inner.shared.lock().last_metrics["submitted"] = json!(true);
        send.click(remaining()?.min(CONTROL_MS))?;
        while finished.lock().is_empty() {
            remaining()?;
            if preflight.lock().refused {
                return Err(fail(
                    "America.gov's browser request did not match this fresh question. It will not be resent.",
                ));
            }
            if !failed.lock().is_empty() || requests.lock().len() > 1 {
                return Err(fail("America.gov's chat request failed or repeated. It will not be resent."));
            }
            page.wait_for_timeout(remaining()?.min(POLL_MS));
        }
        let (request, done) = {
            let (state, requests, finished, failed) = (preflight.lock(), requests.lock(), finished.lock(), failed.lock());
            if state.refused
                || !state.accepted
                || requests.len() != 1
                || finished.len() != 1
                || !same_request(&finished[0], &requests[0])
                || !failed.is_empty()
            {
                return Err(fail(
                    "America.gov's answer could not be tied to this one question. It will not be resent.",
                ));
            }
            (requests[0].clone(), finished[0].clone())
        };
        correlate(&*request, text)?;
        let response = match done.response()? {
            Some(r) if r.status() == 200 => r,
            _ => return Err(fail("America.gov did not answer this browser question. It will not be resent.")),
        };
        let content_type = response.header("content-type");
        let media = eve_desktop_lanes::py_strip(content_type.split(';').next().unwrap_or("")).to_lowercase();
        if media != "text/event-stream" || response.header("x-vercel-ai-ui-message-stream") != "v1" {
            return Err(fail("America.gov changed its browser answer protocol. It will not be resent."));
        }
        let raw = response.body()?;
        remaining()?;
        if raw.len() > MAX_STREAM_BYTES {
            return Err(anyhow::Error::new(ProtocolError("Stream limit exceeded.".to_string())));
        }
        let (answer, sources) = parse_stream(&raw)?;
        inner.shared.lock().last_metrics["seconds"] = json!(py_round_n((inner.clock)() - started, 3));
        Ok((answer, sources))
    })();
    registered.undo(page, &site.api_url);
    result
}

/// Sets the stop flag when the awaiting `ask` is dropped (Python's CancelledError) unless it finished.
struct StopOnDrop(Option<Arc<AtomicBool>>);

impl StopOnDrop {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        if let Some(flag) = self.0.take() {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

/// Dedicated lane/tab; repeated calls reuse Chrome without sharing another task's page.
pub struct AmericaBrowser {
    lane: Arc<dyn AmericaLane>,
    pub(crate) inner: Arc<Inner>,
    gate: tokio::sync::Mutex<()>,
    inflight: Mutex<Option<tokio::task::JoinHandle<()>>>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    closed: AtomicBool,
}

impl AmericaBrowser {
    /// `AmericaBrowser(lane)`: no Jev, 60 s, the monotonic clock.
    pub fn new(lane: Arc<dyn AmericaLane>) -> Self {
        AmericaBrowser::with_options(lane, None, 60.0, Arc::new(eve_desktop_lanes::perf_counter))
    }

    /// `AmericaBrowser(lane, step_asker=, timeout_secs=, clock=)`.
    pub fn with_options(
        lane: Arc<dyn AmericaLane>,
        step_asker: Option<Arc<dyn StepAsk + Send + Sync>>,
        timeout_secs: f64,
        clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    ) -> Self {
        let shared = Shared {
            field: Recipe::css(FIELD),
            send: Recipe::role("button", "Send message"),
            recent_ids: VecDeque::with_capacity(RECENT_IDS),
            last_metrics: json!({}),
        };
        AmericaBrowser {
            lane,
            inner: Arc::new(Inner {
                site: Site::default(),
                shared: Mutex::new(shared),
                asker: step_asker,
                timeout_secs,
                clock,
            }),
            gate: tokio::sync::Mutex::new(()),
            inflight: Mutex::new(None),
            cancel: Mutex::new(None),
            closed: AtomicBool::new(false),
        }
    }

    /// The flow pointed at another chat page and API (a test's loopback page); call before the first `ask`.
    #[cfg(test)]
    pub(crate) fn with_site(mut self, chat_url: &str, api_url: &str) -> Self {
        let inner = Arc::get_mut(&mut self.inner).expect("with_site before any ask");
        inner.site = Site { chat_url: chat_url.to_string(), api_url: api_url.to_string() };
        self
    }

    /// `self.lane.config`.
    pub fn lane_config(&self) -> BrowserLaneConfig {
        self.lane.config()
    }

    pub fn timeout_secs(&self) -> f64 {
        self.inner.timeout_secs
    }

    /// `last_metrics`: `jev_calls`, `submitted` and, after an answer, `seconds`.
    pub fn last_metrics(&self) -> Value {
        self.inner.shared.lock().last_metrics.clone()
    }

    /// `_field`: the cached recipe for the question field.
    pub fn field_recipe(&self) -> Recipe {
        self.inner.shared.lock().field.clone()
    }

    /// `_send`: the cached recipe for the Send button.
    pub fn send_recipe(&self) -> Recipe {
        self.inner.shared.lock().send.clone()
    }

    /// `_recent_ids.append(chat_id)`: a chat id the preflight treats as already sent.
    pub fn remember_chat_id(&self, chat_id: &str) {
        let mut s = self.inner.shared.lock();
        if s.recent_ids.len() == RECENT_IDS {
            s.recent_ids.pop_front();
        }
        s.recent_ids.push_back(chat_id.to_string());
    }

    /// `_inflight is not None and not _inflight.done()`.
    pub fn inflight_running(&self) -> bool {
        self.inflight.lock().as_ref().is_some_and(|job| !job.is_finished())
    }

    pub async fn ask(&self, question: &str, jurisdiction: &str) -> anyhow::Result<Answer> {
        let _held = self.gate.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(fail("America.gov's browser is closed."));
        }
        if self.inflight_running() {
            return Err(fail("The previous America.gov browser question is still stopping. It will not be resent."));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel.lock() = Some(cancel.clone());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (lane, inner, stop) = (self.lane.clone(), self.inner.clone(), cancel.clone());
        let (question, jurisdiction) = (question.to_string(), jurisdiction.to_string());
        let job: Job = Box::new(move |worker: &mut dyn LaneWorker| ask_sync(&inner, worker, &question, &jurisdiction, &stop));
        *self.inflight.lock() = Some(tokio::spawn(async move {
            let _ = tx.send(lane.run(job).await);
        }));
        // A sync worker cannot be killed by dropping its future. Keep it tracked and quarantine the page until
        // its finite waits end.
        let mut armed = StopOnDrop(Some(cancel.clone()));
        let outcome = match budget(self.inner.timeout_secs) {
            Some(limit) => tokio::time::timeout(limit, rx).await.ok(),
            None => Some(rx.await),
        };
        let Some(outcome) = outcome else {
            return Err(anyhow::Error::new(BrowserTimeout)); // `armed` sets the stop flag
        };
        armed.disarm();
        match outcome {
            Ok(Ok(Ok(answer))) => Ok(answer),
            Ok(Ok(Err(failure))) => Err(anyhow::Error::new(failure)),
            Ok(Err(lane)) => Err(lane),
            Err(_) => Err(anyhow::anyhow!("the America.gov browser job ended without an answer")),
        }
    }

    pub async fn close(&self) -> anyhow::Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(cancel) = self.cancel.lock().as_ref() {
            cancel.store(true, Ordering::SeqCst);
        }
        let job = self.inflight.lock().take();
        if let Some(job) = job {
            let _ = job.await;
        }
        self.lane.close().await
    }
}

#[async_trait]
impl crate::AskBrowser for AmericaBrowser {
    async fn ask(&self, question: &str, jurisdiction: &str) -> anyhow::Result<Answer> {
        AmericaBrowser::ask(self, question, jurisdiction).await
    }

    async fn close(&self) -> anyhow::Result<()> {
        AmericaBrowser::close(self).await
    }
}

/// What `make_browser` builds the Jev step asker with.
pub type MakeAsker<'a> = &'a dyn Fn(&HashMap<String, String>) -> Option<Box<dyn StepAsk + Send + Sync>>;

/// `make_browser`: None unless the browser lane attaches (eve's own Chrome, the owner's, or Steel).
pub fn make_browser(environ: Option<&HashMap<String, String>>) -> anyhow::Result<Option<AmericaBrowser>> {
    make_browser_with(environ, &|env| eve_browser_lane::make_step_asker(Some(env)))
}

/// `make_browser` with the step-asker factory as a parameter (the seam the Python test monkeypatched).
pub fn make_browser_with(environ: Option<&HashMap<String, String>>, make_asker: MakeAsker<'_>) -> anyhow::Result<Option<AmericaBrowser>> {
    let config = eve_browser_lane::configured(environ);
    if !config.attach {
        return Ok(None);
    }
    // America has its own opt-out; enabling this fixed flow does not enable Jev on unrelated desktop tasks.
    // Existing route/key/timeout/spend tracking apply.
    let mut env = environ.cloned().unwrap_or_else(|| std::env::vars().collect());
    let jev = env.get("EVE_AMERICA_JEV").cloned().unwrap_or_else(|| "1".to_string());
    env.insert("EVE_JEV_STEP".to_string(), jev);
    let asker = make_asker(&env).map(Arc::<dyn StepAsk + Send + Sync>::from);
    let lane = crate::chrome::ChromeLane::new(config)?;
    Ok(Some(AmericaBrowser::with_options(Arc::new(lane), asker, 60.0, Arc::new(eve_desktop_lanes::perf_counter))))
}
