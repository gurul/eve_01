//! `tests/test_america_browser.py`: the offline normal-browser America flow — semantic controls, one send,
//! exact completion and Jev recovery — over the same fake page, lane and requests the Python tests use.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use eve_browser_lane::BrowserLaneConfig;
use eve_desktop_lanes::ax_candidates::{Candidate, Snapshot};
use eve_desktop_lanes::fast_lane::StepAsk;
use eve_typed_ask::StepAnswer;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Value};

use super::{args, run, sse, source_struct, ANSWER, HEADERS, JURISDICTION, QUESTION};
use crate::america_browser::*;
use crate::{America, AmericaConfig, ChatResponse, ChatTransport, HttpError, Limits, Source, Transport, AMERICA_URL, API_URL, MAX_STREAM_BYTES};

fn text() -> String {
    format!("Jurisdiction: {JURISDICTION}\nQuestion: {QUESTION}")
}

fn answer() -> (String, Vec<Source>) {
    (ANSWER.to_string(), vec![source_struct()])
}

/// `threading.Event`.
#[derive(Default)]
struct Event {
    set: StdMutex<bool>,
    cv: Condvar,
}

impl Event {
    fn set(&self) {
        *self.set.lock().unwrap() = true;
        self.cv.notify_all();
    }

    fn is_set(&self) -> bool {
        *self.set.lock().unwrap()
    }

    fn wait(&self, secs: f64) -> bool {
        let guard = self.set.lock().unwrap();
        let (guard, _) = self.cv.wait_timeout_while(guard, Duration::from_secs_f64(secs), |s| !*s).unwrap();
        *guard
    }
}

type Body = Arc<dyn Fn() -> Vec<u8> + Send + Sync>;

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Body,
}

/// The fake page's state (`Page` in the Python test).
struct State {
    url: String,
    response: Reply,
    fills: Vec<String>,
    gotos: Vec<String>,
    listeners: Vec<(PageEvent, u64, RequestListener)>,
    next_listener: u64,
    value: String,
    clicks: usize,
    closed: bool,
    waits: usize,
    counts: HashMap<String, usize>,
    visible: HashMap<String, bool>,
    enabled: HashMap<String, bool>,
    field_label: String,
    send_label: String,
    send_type: Option<String>,
    css_field: bool,
    value_correct: bool,
    body: Option<Value>,
    click_error: bool,
    failure: bool,
    delayed: bool,
    duplicate: bool,
    current: Option<Arc<dyn ChatRequest>>,
    release: Option<Arc<Event>>,
    destination: String,
    sent: usize,
    aborted: usize,
    guard: Option<RouteHandler>,
    candidate_label: String,
    page_title: String,
    /// replaces `wait_for_timeout` (the cancel test's controlled worker)
    block: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    snapshots: usize,
    connects: usize,
    lane_page: bool,
}

impl Default for State {
    fn default() -> Self {
        State {
            url: "about:blank".into(),
            response: Reply { status: 200, headers: headers(HEADERS), body: Arc::new(sse) },
            fills: Vec::new(),
            gotos: Vec::new(),
            listeners: Vec::new(),
            next_listener: 0,
            value: "old private question".into(),
            clicks: 0,
            closed: false,
            waits: 0,
            counts: HashMap::new(),
            visible: HashMap::new(),
            enabled: HashMap::new(),
            field_label: "Describe what you need".into(),
            send_label: "Send message".into(),
            send_type: Some("submit".into()),
            css_field: true,
            value_correct: true,
            body: None,
            click_error: false,
            failure: false,
            delayed: false,
            duplicate: false,
            current: None,
            release: None,
            destination: "https://america.gov/".into(),
            sent: 0,
            aborted: 0,
            guard: None,
            candidate_label: "private current-value fallback".into(),
            page_title: "America.gov".into(),
            block: None,
            snapshots: 0,
            connects: 0,
            lane_page: true,
        }
    }
}

fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

type Shared = Arc<Mutex<State>>;
type Change = Box<dyn Fn(&mut State)>;

fn emit(state: &Shared, event: PageEvent, request: &Arc<dyn ChatRequest>) {
    let listeners: Vec<RequestListener> =
        state.lock().listeners.iter().filter(|(e, _, _)| *e == event).map(|(_, _, l)| l.clone()).collect();
    for listener in listeners {
        listener(request);
    }
}

struct FakeResponse(Reply);

impl PageResponse for FakeResponse {
    fn status(&self) -> u16 {
        self.0.status
    }

    fn header(&self, name: &str) -> String {
        self.0.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default()
    }

    fn body(&self) -> anyhow::Result<Vec<u8>> {
        Ok((self.0.body)())
    }
}

struct FakeRequest {
    state: Shared,
    body: Option<Value>,
}

impl ChatRequest for FakeRequest {
    fn method(&self) -> String {
        "POST".into()
    }

    fn url(&self) -> String {
        API_URL.into()
    }

    fn post_data_json(&self) -> anyhow::Result<Option<Value>> {
        Ok(self.body.clone())
    }

    fn response(&self) -> anyhow::Result<Option<Box<dyn PageResponse>>> {
        Ok(Some(Box::new(FakeResponse(self.state.lock().response.clone()))))
    }
}

struct FakeRoute {
    state: Shared,
    request: Arc<dyn ChatRequest>,
}

impl Route for FakeRoute {
    fn request(&self) -> Arc<dyn ChatRequest> {
        self.request.clone()
    }

    fn continue_(&self) -> anyhow::Result<()> {
        self.state.lock().sent += 1;
        Ok(())
    }

    fn abort(&self) -> anyhow::Result<()> {
        self.state.lock().aborted += 1;
        Ok(())
    }
}

struct FakeLocator {
    state: Shared,
    kind: String,
}

impl FakeLocator {
    fn of(state: &Shared, kind: &str) -> Box<dyn Locator + 'static> {
        Box::new(FakeLocator { state: state.clone(), kind: kind.to_string() })
    }
}

fn page_locator(state: &Shared, css: &str) -> Box<dyn Locator + 'static> {
    if css == FORM {
        return FakeLocator::of(state, "form");
    }
    if css == FIELD {
        let field = state.lock().css_field;
        return FakeLocator::of(state, if field { "field" } else { "missing" });
    }
    FakeLocator::of(state, "missing")
}

impl Locator for FakeLocator {
    fn locator(&self, css: &str) -> Box<dyn Locator + '_> {
        page_locator(&self.state, css)
    }

    fn get_by_role(&self, role: &str, name: &str, _exact: bool) -> Box<dyn Locator + '_> {
        let (field, send) = {
            let s = self.state.lock();
            (s.field_label.clone(), s.send_label.clone())
        };
        if role == "textbox" && name == field {
            return FakeLocator::of(&self.state, "field");
        }
        if role == "button" && name == send {
            return FakeLocator::of(&self.state, "send");
        }
        FakeLocator::of(&self.state, "missing")
    }

    fn count(&self) -> anyhow::Result<usize> {
        Ok(if self.kind == "missing" { 0 } else { *self.state.lock().counts.get(&self.kind).unwrap_or(&1) })
    }

    fn is_visible(&self) -> anyhow::Result<bool> {
        Ok(*self.state.lock().visible.get(&self.kind).unwrap_or(&true))
    }

    fn is_enabled(&self) -> anyhow::Result<bool> {
        Ok(*self.state.lock().enabled.get(&self.kind).unwrap_or(&true))
    }

    fn wait_for_visible(&self, _timeout_ms: u64) -> anyhow::Result<()> {
        if !self.is_visible()? {
            return Err(anyhow::anyhow!("private page body"));
        }
        Ok(())
    }

    fn evaluate(&self, code: &str, _arg: Option<&Value>) -> anyhow::Result<Value> {
        let s = self.state.lock();
        if code == NAME_JS {
            return Ok(json!(if self.kind == "field" { &s.field_label } else { &s.send_label }));
        }
        if code.contains("closest") {
            return Ok(json!(self.kind == "field" || self.kind == "send"));
        }
        Ok(json!(if self.kind == "field" { "textarea" } else { "button" }))
    }

    fn get_attribute(&self, name: &str) -> anyhow::Result<Option<String>> {
        Ok(if name == "type" { self.state.lock().send_type.clone() } else { None })
    }

    fn fill(&self, text: &str, _timeout_ms: u64) -> anyhow::Result<()> {
        assert_eq!(self.kind, "field");
        let mut s = self.state.lock();
        s.value = text.to_string();
        s.fills.push(text.to_string());
        Ok(())
    }

    fn input_value(&self, _timeout_ms: u64) -> anyhow::Result<String> {
        let s = self.state.lock();
        Ok(if s.value_correct { s.value.clone() } else { "private mismatch".into() })
    }

    fn click(&self, _timeout_ms: u64) -> anyhow::Result<()> {
        assert_eq!(self.kind, "send");
        let (body, guard) = {
            let mut s = self.state.lock();
            s.clicks += 1;
            let body = s.body.clone().unwrap_or_else(|| {
                json!({"language": "en", "id": format!("freshchatid{}", s.gotos.len()), "trigger": "submit-message",
                       "messages": [{"role": "user", "parts": [{"type": "text", "text": s.value}], "id": "message"}]})
            });
            (body, s.guard.clone())
        };
        let request: Arc<dyn ChatRequest> = Arc::new(FakeRequest { state: self.state.clone(), body: Some(body) });
        self.state.lock().current = Some(request.clone());
        emit(&self.state, PageEvent::Request, &request);
        let route = FakeRoute { state: self.state.clone(), request: request.clone() };
        let guard = guard.expect("the flow routes the chat API before it clicks");
        guard(&route);
        let (aborted, duplicate, click_error, failure, delayed) = {
            let s = self.state.lock();
            (s.aborted > 0, s.duplicate, s.click_error, s.failure, s.delayed)
        };
        if aborted {
            emit(&self.state, PageEvent::RequestFailed, &request);
            return Ok(());
        }
        if duplicate {
            emit(&self.state, PageEvent::Request, &request);
            guard(&route);
        }
        if click_error {
            return Err(anyhow::anyhow!("private browser error after submission"));
        }
        if failure {
            emit(&self.state, PageEvent::RequestFailed, &request);
        } else if !delayed {
            emit(&self.state, PageEvent::RequestFinished, &request);
        }
        Ok(())
    }
}

struct FakePage {
    state: Shared,
}

impl ChatPage for FakePage {
    fn url(&self) -> String {
        self.state.lock().url.clone()
    }

    fn title(&self) -> anyhow::Result<String> {
        Ok(self.state.lock().page_title.clone())
    }

    fn goto(&self, url: &str, _wait_until: &str, _timeout_ms: u64) -> anyhow::Result<()> {
        let mut s = self.state.lock();
        s.gotos.push(url.to_string());
        s.url = if url == "about:blank" { url.to_string() } else { s.destination.clone() };
        s.value.clear();
        Ok(())
    }

    fn locator(&self, css: &str) -> Box<dyn Locator + '_> {
        page_locator(&self.state, css)
    }

    fn route(&self, url: &str, handler: RouteHandler) -> anyhow::Result<()> {
        assert_eq!(url, API_URL);
        self.state.lock().guard = Some(handler);
        Ok(())
    }

    fn unroute(&self, url: &str) -> anyhow::Result<()> {
        let mut s = self.state.lock();
        assert!(url == API_URL && s.guard.is_some());
        s.guard = None;
        Ok(())
    }

    fn on(&self, event: PageEvent, callback: RequestListener) -> anyhow::Result<u64> {
        let mut s = self.state.lock();
        s.next_listener += 1;
        let id = s.next_listener;
        s.listeners.push((event, id, callback));
        Ok(id)
    }

    fn remove_listener(&self, event: PageEvent, id: u64) -> anyhow::Result<()> {
        let mut s = self.state.lock();
        let before = s.listeners.len();
        s.listeners.retain(|(e, i, _)| !(*e == event && *i == id));
        assert_eq!(s.listeners.len() + 1, before, "removed a listener that was added");
        Ok(())
    }

    fn wait_for_timeout(&self, ms: u64) {
        let block = self.state.lock().block.clone();
        if let Some(block) = block {
            block(ms);
            return;
        }
        let release = {
            let mut s = self.state.lock();
            s.waits += 1;
            s.release.clone()
        };
        if let Some(release) = &release {
            release.wait(0.01);
        }
        let (delayed, current) = {
            let s = self.state.lock();
            (s.delayed, s.current.clone())
        };
        if delayed && release.as_ref().is_none_or(|r| r.is_set()) {
            if let Some(current) = current {
                emit(&self.state, PageEvent::RequestFinished, &current);
            }
        }
    }
}

struct FakeWorker {
    state: Shared,
}

impl LaneWorker for FakeWorker {
    fn attach(&self) -> bool {
        true
    }

    fn ensure(&mut self) -> anyhow::Result<()> {
        let mut s = self.state.lock();
        s.connects += 1;
        assert!(s.lane_page, "the lane's page was reset");
        Ok(())
    }

    fn page(&self) -> Option<Box<dyn ChatPage + '_>> {
        Some(Box::new(FakePage { state: self.state.clone() }))
    }

    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        let mut s = self.state.lock();
        s.snapshots += 1;
        let c = |role: &str, label: &str, value: &str, editable: bool, id: &str| Candidate {
            role: role.into(),
            label: label.into(),
            value: value.into(),
            editable,
            id: id.into(),
            ..Candidate::default()
        };
        Ok(Snapshot {
            elements: vec![
                c("text field", &s.candidate_label, "private value", true, "field"),
                c("button", &s.send_label, "", false, "send"),
                c("button", "private page unrelated button", "", false, "outside"),
            ],
            ..Snapshot::default()
        })
    }

    fn candidate_locator(&self, c: &Candidate) -> anyhow::Result<Box<dyn Locator + '_>> {
        Ok(FakeLocator::of(&self.state, &c.id))
    }

    fn reset(&mut self) {
        let mut s = self.state.lock();
        s.closed = true;
        s.lane_page = false;
    }
}

/// The fake lane: jobs run inline (Python's `async def _run(fn): return fn()`), or on a thread.
struct FakeLane {
    state: Shared,
    closed: AtomicBool,
    threaded: bool,
}

#[async_trait]
impl AmericaLane for FakeLane {
    fn config(&self) -> BrowserLaneConfig {
        BrowserLaneConfig { attach: true, ..BrowserLaneConfig::default() }
    }

    async fn run(&self, job: Job) -> anyhow::Result<Result<Answer, BrowserFailure>> {
        let mut worker = FakeWorker { state: self.state.clone() };
        if self.threaded {
            return Ok(tokio::task::spawn_blocking(move || job(&mut worker)).await?);
        }
        Ok(job(&mut worker))
    }

    async fn close(&self) -> anyhow::Result<()> {
        self.closed.store(true, Ordering::SeqCst);
        self.state.lock().closed = true;
        Ok(())
    }
}

type Asker = Arc<dyn StepAsk + Send + Sync>;

fn asker(f: impl Fn(&str, &str, &str, &IndexMap<String, String>, &[String]) -> StepAnswer + Send + Sync + 'static) -> Option<Asker> {
    Some(Arc::new(f))
}

fn never_asked() -> Option<Asker> {
    asker(|_, _, _, _, _| panic!("Jev must not run on known controls"))
}

struct Flow {
    service: AmericaBrowser,
    state: Shared,
    lane: Arc<FakeLane>,
}

fn flow_with(state: State, asker: Option<Asker>, timeout_secs: f64, clock: Option<Arc<dyn Fn() -> f64 + Send + Sync>>, threaded: bool) -> Flow {
    let state: Shared = Arc::new(Mutex::new(state));
    let lane = Arc::new(FakeLane { state: state.clone(), closed: AtomicBool::new(false), threaded });
    let clock = clock.unwrap_or_else(|| Arc::new(eve_desktop_lanes::perf_counter));
    let service = AmericaBrowser::with_options(lane.clone(), asker, timeout_secs, clock);
    Flow { service, state, lane }
}

fn flow(state: State, asker: Option<Asker>) -> Flow {
    flow_with(state, asker, 60.0, None, false)
}

fn ask(service: &AmericaBrowser) -> anyhow::Result<Answer> {
    run(service.ask(QUESTION, JURISDICTION))
}

fn failure_of(result: anyhow::Result<Answer>) -> String {
    let err = result.expect_err("the flow must fail");
    err.downcast_ref::<BrowserFailure>().unwrap_or_else(|| panic!("not a BrowserFailure: {err:#}")).0.clone()
}

#[test]
fn test_stable_flow_fills_in_one_operation_sends_once_and_consumes_exact_sse() {
    let f = flow(State::default(), never_asked());
    assert_eq!(ask(&f.service).unwrap(), answer());
    let s = f.state.lock();
    assert_eq!(s.gotos, vec!["about:blank".to_string(), AMERICA_URL.to_string()]);
    assert_eq!(s.fills, vec![text()]);
    assert_eq!(s.clicks, 1);
    assert_eq!(s.sent, 1);
    assert_eq!(s.aborted, 0);
    assert!(s.guard.is_none());
    assert_eq!(s.snapshots, 0);
    assert!(!s.closed && s.listeners.is_empty());
    let metrics = f.service.last_metrics();
    assert_eq!(metrics["jev_calls"], json!(0));
    assert_eq!(metrics["submitted"], json!(true));
    assert!(metrics["seconds"].as_f64().is_some());
}

#[test]
fn test_repeated_questions_reuse_lane_but_unmount_prior_history_every_time() {
    let f = flow(State::default(), None);
    run(async {
        assert_eq!(f.service.ask(QUESTION, JURISDICTION).await.unwrap(), answer());
        f.state.lock().value = "private previous question".into();
        assert_eq!(f.service.ask("Second question?", "California").await.unwrap(), answer());
        f.service.close().await.unwrap();
        f.service.close().await.unwrap();
    });
    let s = f.state.lock();
    let one = vec!["about:blank".to_string(), AMERICA_URL.to_string()];
    assert_eq!(s.gotos, [one.clone(), one].concat());
    assert_eq!(s.fills, vec![text(), "Jurisdiction: California\nQuestion: Second question?".to_string()]);
    assert_eq!(s.clicks, 2);
    assert!(f.lane.closed.load(Ordering::SeqCst));
}

#[test]
fn test_jev_recovers_changed_names_then_reuses_semantic_recipe_without_private_values() {
    let state = State {
        css_field: false,
        field_label: "Your government question".into(),
        send_label: "Ask America".into(),
        ..State::default()
    };
    let calls: Arc<Mutex<Vec<Value>>> = Arc::default();
    let seen = calls.clone();
    let choose = asker(move |step, app, context, options, _recent| {
        seen.lock().push(json!([step, {"app": app, "context": context, "options": options}]));
        StepAnswer::new("0", 0.98, 0.8, 0.99, 0.0, 0.0)
    });
    let f = flow(state, choose);
    assert_eq!(ask(&f.service).unwrap(), answer());
    assert_eq!(calls.lock().len(), 2);
    assert_eq!(f.state.lock().snapshots, 2);
    let field = f.service.field_recipe();
    assert_eq!((field.role.as_str(), field.name.as_str()), ("textbox", "Your government question"));
    assert_eq!(f.service.send_recipe().name, "Ask America");
    let wire = serde_json::to_string(&*calls.lock()).unwrap();
    assert!(!wire.contains("private") && !wire.contains(QUESTION) && wire.contains("text field"), "{wire}");
    assert_eq!(ask(&f.service).unwrap(), answer());
    assert_eq!(calls.lock().len(), 2);
    assert_eq!(f.service.last_metrics()["jev_calls"], json!(0));
}

#[test]
fn test_authorized_submit_recovery_keeps_confidence_gates_even_when_send_is_risky() {
    let state = State { send_label: "Ask America".into(), ..State::default() };
    let f = flow(state, asker(|_, _, _, _, _| StepAnswer::new("0", 0.98, 0.8, 0.99, 0.0, 1.0)));
    assert_eq!(ask(&f.service).unwrap(), answer());
    assert_eq!(f.state.lock().sent, 1);
    let weak_state = State { send_label: "Ask America".into(), ..State::default() };
    let weak = flow(weak_state, asker(|_, _, _, _, _| StepAnswer::new("0", 0.4, 0.1, 0.4, 0.0, 1.0)));
    failure_of(ask(&weak.service));
    let s = weak.state.lock();
    assert_eq!(s.clicks, 0);
    assert_eq!(s.sent, 0);
}

#[test]
fn test_unavailable_or_uncertain_jev_cannot_send() {
    let answers = [
        StepAnswer::new("none", 1.0, 1.0, 0.0, 0.0, 0.0),
        StepAnswer::new("0", 0.4, 0.1, 0.4, 0.0, 0.0),
        StepAnswer::new("0", 0.99, 0.8, 0.99, 0.0, 1.0),
        StepAnswer::new("99", 1.0, 1.0, 1.0, 0.0, 0.0),
    ];
    for a in answers {
        let state = State { css_field: false, ..State::default() };
        let f = flow(state, asker(move |_, _, _, _, _| a.clone()));
        failure_of(ask(&f.service));
        let s = f.state.lock();
        assert_eq!(s.clicks, 0);
        assert!(s.closed);
    }
}

#[test]
fn test_invalid_or_ambiguous_controls_or_navigation_never_submit() {
    let changes: Vec<Change> = vec![
        Box::new(|s| {
            s.counts.insert("form".into(), 2);
        }),
        Box::new(|s| {
            s.visible.insert("form".into(), false);
        }),
        Box::new(|s| {
            s.enabled.insert("field".into(), false);
        }),
        Box::new(|s| s.send_type = Some("button".into())),
        Box::new(|s| s.value_correct = false),
        Box::new(|s| {
            s.enabled.insert("send".into(), false);
        }),
        Box::new(|s| s.destination = "https://attacker.test/chat".into()),
    ];
    for change in changes {
        let mut state = State::default();
        change(&mut state);
        let f = flow(state, None);
        failure_of(ask(&f.service));
        let s = f.state.lock();
        assert_eq!(s.clicks, 0);
        assert!(s.closed);
    }
}

#[test]
fn test_post_click_error_does_not_retry_and_removes_listeners() {
    let f = flow(State { click_error: true, ..State::default() }, None);
    let why = failure_of(ask(&f.service));
    assert!(why.contains("may have been sent"), "{why}");
    assert!(!why.contains("private"));
    let s = f.state.lock();
    assert_eq!(s.clicks, 1);
    assert!(s.closed);
    assert!(s.listeners.is_empty());
}

#[test]
fn test_observed_browser_security_page_has_no_question_or_jev_call() {
    let state = State { page_title: "Just a moment...".into(), ..State::default() };
    let f = flow(state, asker(|_, _, _, _, _| panic!("do not ground a challenge")));
    assert!(failure_of(ask(&f.service)).contains("security check"));
    let s = f.state.lock();
    assert!(s.fills.is_empty());
    assert_eq!(s.clicks, 0);
    assert_eq!(s.sent, 0);
    assert_eq!(s.snapshots, 0);
}

#[test]
fn test_failed_unsourced_or_uncorrelated_request_has_no_answer_and_no_second_click() {
    let changes: Vec<Change> = vec![
        Box::new(|s| s.failure = true),
        Box::new(|s| s.duplicate = true),
        Box::new(|s| s.response.status = 418),
        Box::new(|s| s.response.headers = headers(&[("content-type", "text/html")])),
        Box::new(|s| {
            s.response.body = Arc::new(|| {
                let full = sse();
                full[..full.len() - 16].to_vec()
            })
        }),
        Box::new(|s| s.response.body = Arc::new(|| vec![b'x'; MAX_STREAM_BYTES + 1])),
        Box::new(|s| {
            s.body = Some(json!({"messages": [{"role": "user", "parts": [{"type": "text", "text": "old private question"}]}]}))
        }),
    ];
    for change in changes {
        let mut state = State::default();
        change(&mut state);
        let f = flow(state, None);
        let why = failure_of(ask(&f.service));
        assert!(!why.contains("private"), "{why}");
        let s = f.state.lock();
        assert_eq!(s.clicks, 1);
        assert!(s.closed);
    }
}

#[test]
fn test_delayed_completion_waits_for_finished_response() {
    let f = flow(State { delayed: true, ..State::default() }, None);
    assert_eq!(ask(&f.service).unwrap(), answer());
    assert_eq!(f.state.lock().waits, 1);
}

#[test]
fn test_prior_history_or_signed_metadata_is_rejected_before_network_egress() {
    for extra in ["history", "metadata", "top_metadata", "reused_id"] {
        let mut body = json!({"language": "en", "id": "seeded", "trigger": "submit-message", "messages": [
            {"id": "freshmessage", "role": "user", "parts": [{"type": "text", "text": text()}]}]});
        match extra {
            "history" => body["messages"].as_array_mut().unwrap().push(json!({"role": "assistant", "parts": []})),
            "metadata" => body["messages"][0]["metadata"] = json!({"signed": "private earlier turn"}),
            "top_metadata" => body["metadata"] = json!({"signed": "private earlier turn"}),
            _ => {}
        }
        let f = flow(State { body: Some(body), ..State::default() }, None);
        if extra == "reused_id" {
            f.service.remember_chat_id("seeded");
        }
        assert!(failure_of(ask(&f.service)).contains("fresh question"), "{extra}");
        let s = f.state.lock();
        assert_eq!(s.clicks, 1);
        assert_eq!(s.aborted, 1);
        assert_eq!(s.sent, 0);
    }
}

#[test]
fn test_cancelled_worker_is_quarantined_and_new_question_is_not_queued() {
    let (release, blocked) = (Arc::new(Event::default()), Arc::new(Event::default()));
    let (r, b) = (release.clone(), blocked.clone());
    let state = State {
        delayed: true,
        block: Some(Arc::new(move |_ms| {
            b.set();
            assert!(r.wait(3.0), "test did not release controlled browser worker");
        })),
        ..State::default()
    };
    let f = Arc::new(flow_with(state, None, 1.0, None, true));
    let g = f.clone();
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap().block_on(async move {
        let service = g.clone();
        let pending = tokio::spawn(async move { service.service.ask(QUESTION, JURISDICTION).await });
        while !blocked.is_set() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        pending.abort();
        assert!(pending.await.unwrap_err().is_cancelled());
        assert!(g.service.inflight_running());
        let why = failure_of(g.service.ask("Do not send me", "California").await);
        assert!(why.contains("previous"), "{why}");
        release.set();
        g.service.close().await.unwrap();
    });
    let s = f.state.lock();
    assert_eq!(s.clicks, 1);
    assert!(s.closed);
    assert!(f.lane.closed.load(Ordering::SeqCst));
}

#[test]
fn test_total_worker_budget_expires_and_closes_the_owned_page() {
    let ticks = Arc::new(AtomicUsize::new(0));
    let t = ticks.clone();
    // nine readings of 0, then 2 for ever (Python's iter([0] * 9 + [2]) with next(ticks, 2))
    let clock: Arc<dyn Fn() -> f64 + Send + Sync> = Arc::new(move || if t.fetch_add(1, Ordering::SeqCst) < 9 { 0.0 } else { 2.0 });
    let state = State { delayed: true, release: Some(Arc::new(Event::default())), ..State::default() };
    let f = flow_with(state, None, 1.0, Some(clock), false);
    assert!(failure_of(ask(&f.service)).contains("timed out"));
    let s = f.state.lock();
    assert!(s.closed);
    assert!(s.clicks <= 1);
}

#[test]
fn test_factory_requires_normal_chrome_and_respects_america_jev_opt_out() {
    let env = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    let calls: Arc<Mutex<Vec<HashMap<String, String>>>> = Arc::default();
    let seen = calls.clone();
    let make = move |e: &HashMap<String, String>| -> Option<Box<dyn StepAsk + Send + Sync>> {
        seen.lock().push(e.clone());
        None
    };
    assert!(make_browser_with(Some(&env(&[])), &make).unwrap().is_none());
    assert!(calls.lock().is_empty());
    let service = make_browser_with(Some(&env(&[("EVE_BROWSER_OWN", "1"), ("EVE_AMERICA_JEV", "0")])), &make).unwrap().unwrap();
    let config = service.lane_config();
    assert!(config.attach && config.own);
    assert_eq!(calls.lock()[0]["EVE_JEV_STEP"], "0");
    let default_on = make_browser_with(Some(&env(&[("EVE_BROWSER_OWN", "1")])), &make).unwrap().unwrap();
    assert_eq!(calls.lock()[1]["EVE_JEV_STEP"], "1");
    run(async {
        service.close().await.unwrap();
        default_on.close().await.unwrap();
    });
}

#[test]
fn test_tool_uses_browser_by_default_and_keeps_normalized_answer_contract() {
    let f = flow(State::default(), None);
    let service = Arc::new(f.service);
    let provider = America::new(AmericaConfig { transport: Transport::Browser, ..AmericaConfig::on(true) }).with_browser(service);
    let result = run(provider.handle("america_ask", &args())).unwrap();
    assert_eq!(result["ok"], json!(true));
    assert_eq!(result["provider"], json!("america_gov"));
    assert_eq!(result["answer"], json!(ANSWER));
    assert_eq!(result["sources"], json!([super::source()]));
    assert_eq!(f.state.lock().clicks, 1);
    run(provider.close()).unwrap();
    assert!(f.lane.closed.load(Ordering::SeqCst));
}

/// A transport that records any attempt (the "direct HTTP attempted" sentinel).
#[derive(Default)]
struct Blocked {
    posts: AtomicUsize,
}

#[async_trait]
impl ChatTransport for Blocked {
    async fn post(&self, _u: &str, _h: &[(&str, &str)], _b: Vec<u8>, _l: Limits) -> Result<ChatResponse, HttpError> {
        self.posts.fetch_add(1, Ordering::SeqCst);
        Err(HttpError("direct HTTP attempted".into()))
    }
}

#[test]
fn test_tool_browser_failure_does_not_fall_back_to_a_direct_post() {
    let blocked = Arc::new(Blocked::default());
    // positive control: the sentinel sees a direct call
    let api = America::new(AmericaConfig { transport: Transport::Api, ..AmericaConfig::on(true) }).with_transport(blocked.clone());
    assert_eq!(run(api.handle("america_ask", &args())).unwrap()["status"], json!("upstream_unavailable"));
    assert_eq!(blocked.posts.swap(0, Ordering::SeqCst), 1);
    let mut state = State::default();
    state.counts.insert("form".into(), 0);
    let f = flow(state, None);
    let provider = America::new(AmericaConfig { transport: Transport::Browser, ..AmericaConfig::on(true) })
        .with_browser(Arc::new(f.service))
        .with_transport(blocked.clone());
    let result = run(provider.handle("america_ask", &args())).unwrap();
    assert_eq!(result["ok"], json!(false));
    assert_eq!(result["status"], json!("upstream_unavailable"));
    assert!(result.get("answer").is_none());
    assert_eq!(f.state.lock().clicks, 0);
    assert_eq!(blocked.posts.load(Ordering::SeqCst), 0);
}

#[test]
fn test_transport_configuration_is_explicit_and_invalid_modes_rejected() {
    let env = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    assert_eq!(crate::configured(Some(&env(&[]))).unwrap().transport, Transport::Alexandria);
    assert_eq!(crate::configured(Some(&env(&[("EVE_AMERICA_TRANSPORT", "api")]))).unwrap().transport, Transport::Api);
    let err = crate::configured(Some(&env(&[("EVE_AMERICA_TRANSPORT", "stealth")]))).unwrap_err();
    assert!(err.to_string().contains("alexandria, browser or api"));
}

// ---- parity additions ----------------------------------------------------------------------------------------

#[test]
fn test_closed_browser_refuses_new_questions() {
    let f = flow(State::default(), None);
    run(f.service.close()).unwrap();
    assert!(failure_of(ask(&f.service)).contains("closed"));
    assert_eq!(f.state.lock().clicks, 0);
}

#[test]
fn test_a_whole_question_past_its_budget_is_a_timeout_and_stops_the_worker() {
    let (release, blocked) = (Arc::new(Event::default()), Arc::new(Event::default()));
    let (r, b) = (release.clone(), blocked.clone());
    let state = State {
        delayed: true,
        block: Some(Arc::new(move |_ms| {
            b.set();
            r.wait(3.0);
        })),
        ..State::default()
    };
    let f = flow_with(state, None, 0.2, None, true);
    let err = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap().block_on(async {
        let err = f.service.ask(QUESTION, JURISDICTION).await.unwrap_err();
        release.set();
        f.service.close().await.unwrap();
        err
    });
    assert!(err.downcast_ref::<BrowserTimeout>().is_some(), "{err:#}");
    assert!(blocked.is_set());
    // the same timeout, through the tool
    let release = Arc::new(Event::default());
    let r = release.clone();
    let state = State {
        delayed: true,
        block: Some(Arc::new(move |_ms| {
            r.wait(3.0);
        })),
        ..State::default()
    };
    let f = flow_with(state, None, 0.2, None, true);
    let provider = America::new(AmericaConfig { transport: Transport::Browser, ..AmericaConfig::on(true) })
        .with_browser(Arc::new(f.service));
    let result = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap().block_on(async {
        let result = provider.handle("america_ask", &args()).await.unwrap();
        release.set();
        provider.close().await.unwrap();
        result
    });
    assert_eq!(
        result["reason"],
        json!("America.gov's browser timed out. The question may have been sent; it will not be resent.")
    );
    assert_eq!(result["status"], json!("upstream_unavailable"));
}
