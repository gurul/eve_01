//! Ports of bridge/tests/test_computer_agent.py (the loop against a scripted Responses client and a fake worker),
//! test_worker_client.py (the client against the real fake child), test_agent_lane_first.py,
//! test_agent_plan_once.py, test_agent_browser_lane.py, the agent half of test_task_router.py,
//! test_response_creators.py, test_response_retry_lean.py, test_browser_planner_config.py and the computer-agent
//! rows of test_spend_wiring.py, test_openrouter.py and test_fastlane_docs.py.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::agent::{AgentOptions, AskUser, ClockFn, CreateResponse, OnEvent};
use crate::*;

// ---- fakes -------------------------------------------------------------------------------------------------

fn image() -> Value {
    json!({"type": "input_image", "detail": "original", "image_url": "data:image/png;base64,AAAA"})
}

fn context() -> Value {
    json!({"type": "input_text", "text": "frontmost: Warp — 'zsh'; screen 100x50; 14:02"})
}

fn text(t: &str) -> Value {
    json!({"type": "input_text", "text": t})
}

fn call(name: &str, call_id: &str, args: Value) -> Value {
    json!({"type": "function_call", "name": name, "call_id": call_id, "arguments": serde_json::to_string(&args).unwrap()})
}

fn exec(call_id: &str, code: &str) -> Value {
    call("exec_py", call_id, json!({"code": code}))
}

fn message(t: &str) -> Value {
    json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": t}]})
}

fn response(rid: &str, items: Vec<Value>) -> Value {
    json!({"id": rid, "status": "completed", "output": items})
}

fn response_with(rid: &str, items: Vec<Value>, extra: Value) -> Value {
    let mut r = response(rid, items);
    for (k, v) in extra.as_object().unwrap() {
        r[k] = v.clone();
    }
    r
}

const APPS: [&str; 13] = [
    "Spotify", "TV", "Terminal", "Safari", "Slack", "Google Chrome", "System Settings", "Notes", "Mail", "Music",
    "Reminders", "Preview", "Calendar",
];

/// asyncio.Event: set once, every waiter proceeds.
#[derive(Clone)]
struct Event(watch::Sender<bool>);

impl Event {
    fn new() -> Self {
        Event(watch::channel(false).0)
    }
    fn set(&self) {
        self.0.send_replace(true);
    }
    async fn wait(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|v| *v).await;
    }
}

#[derive(Default)]
struct ClientState {
    responses: VecDeque<Value>,
    raise_first: VecDeque<PyError>,
    requests: Vec<Value>,
    served: Vec<Value>,
    calls: usize,
}

/// Serves scripted responses in order; records every request.
#[derive(Clone, Default)]
struct FakeClient {
    state: Arc<Mutex<ClientState>>,
    gate: Arc<Mutex<Option<Event>>>,
}

impl FakeClient {
    fn new(responses: Vec<Value>) -> Self {
        let c = FakeClient::default();
        c.state.lock().responses = responses.into();
        c
    }

    fn raising(responses: Vec<Value>, raise_first: Vec<PyError>) -> Self {
        let c = FakeClient::new(responses);
        c.state.lock().raise_first = raise_first.into();
        c
    }

    fn create(&self) -> CreateResponse {
        let me = self.clone();
        Arc::new(move |req: Value| {
            let me = me.clone();
            Box::pin(async move {
                let gate = {
                    let mut s = me.state.lock();
                    s.requests.push(req);
                    s.calls += 1;
                    me.gate.lock().clone()
                };
                if let Some(g) = gate {
                    g.wait().await;
                }
                let mut s = me.state.lock();
                if let Some(e) = s.raise_first.pop_front() {
                    return Err(anyhow::Error::new(e));
                }
                let Some(r) = s.responses.pop_front() else {
                    return Err(PyError::err("AssertionError", "client ran out of scripted responses"));
                };
                s.served.push(r.clone());
                Ok(r)
            })
        })
    }

    fn requests(&self) -> Vec<Value> {
        self.state.lock().requests.clone()
    }

    fn calls(&self) -> usize {
        self.state.lock().calls
    }

    fn set_gate(&self, e: Option<Event>) {
        *self.gate.lock() = e;
    }
}

/// What request `i` adds to the loop's history. The chain is stateless (OpenRouter refuses
/// previous_response_id, 2026-10-08): request `i` resends request `prev`'s input and the response it got, then the
/// new items.
fn new_items(client: &FakeClient, i: usize, prev: Option<usize>) -> Vec<Value> {
    let prev = prev.unwrap_or(i - 1);
    let s = client.state.lock();
    let mut before: Vec<Value> = s.requests[prev]["input"].as_array().unwrap().clone();
    if let Some(Value::Array(out)) = s.served[prev].get("output") {
        before.extend(out.iter().cloned());
    }
    let req = &s.requests[i];
    assert!(req.get("previous_response_id").is_none() && req["store"] == json!(false));
    assert_eq!(req["include"], json!(["reasoning.encrypted_content"]));
    let input = req["input"].as_array().unwrap();
    assert_eq!(&input[..before.len()], &before[..]); // the whole history is resent
    input[before.len()..].to_vec()
}

#[derive(Default)]
struct WorkerState {
    executed: Vec<String>,
    started: bool,
    closed: bool,
    observed: usize,
    routed: Vec<String>,
    runs: Vec<Value>,
    results: VecDeque<Value>,
    last_timing: Option<Value>,
}

enum Route {
    None,
    Value(Value),
    Error(PyError),
}

/// test_computer_agent's FakeWorker, with the LaneWorker, PlanWorker, TimedWorker and ReadyWorker extras as
/// options.
struct FakeWorker {
    results: HashMap<String, Vec<Value>>,
    failsafe_on: Option<String>,
    block: Mutex<Option<Event>>,
    ready: Value,
    route: Route,
    outline: Option<Value>,
    plans: bool,
    timed: bool,
    on_start: Option<Box<dyn Fn() + Send + Sync>>,
    s: Mutex<WorkerState>,
}

impl FakeWorker {
    fn new() -> Self {
        FakeWorker {
            results: HashMap::new(),
            failsafe_on: None,
            block: Mutex::new(None),
            ready: json!({"ready": true, "width": 100, "height": 50}),
            route: Route::None,
            outline: None,
            plans: false,
            timed: false,
            on_start: None,
            s: Mutex::new(WorkerState::default()),
        }
    }

    fn with(results: Vec<(&str, Vec<Value>)>) -> Self {
        let mut w = FakeWorker::new();
        w.results = results.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        w
    }

    /// test_agent_lane_first's LaneWorker.
    fn lane(route: Route) -> Self {
        let mut w = FakeWorker::new();
        w.route = route;
        w
    }

    /// test_agent_plan_once's PlanWorker.
    fn plan(results: Vec<Value>) -> Self {
        let mut w = FakeWorker::new();
        w.route = Route::Value(json!({"status": "none", "reason": "no_match", "clicked": []}));
        w.outline = Some(json!({"app": "Finder", "lines": ["Year (radio button)", "Week (radio button)"]}));
        w.plans = true;
        w.s.lock().results = results.into();
        w
    }

    fn executed(&self) -> Vec<String> {
        self.s.lock().executed.clone()
    }
    fn observed(&self) -> usize {
        self.s.lock().observed
    }
    fn runs(&self) -> Vec<Value> {
        self.s.lock().runs.clone()
    }
    fn closed(&self) -> bool {
        self.s.lock().closed
    }
}

#[async_trait]
impl Worker for FakeWorker {
    async fn start(&self) -> anyhow::Result<Value> {
        if let Some(f) = &self.on_start {
            f();
        }
        self.s.lock().started = true;
        Ok(self.ready.clone())
    }

    async fn observe(&self) -> anyhow::Result<Vec<Value>> {
        self.s.lock().observed += 1;
        Ok(vec![image(), context()])
    }

    async fn execute(&self, code: &str) -> anyhow::Result<Vec<Value>> {
        self.s.lock().executed.push(code.to_string());
        let block = self.block.lock().clone();
        if let Some(b) = block {
            b.wait().await;
        }
        if let Some(f) = &self.failsafe_on {
            if code.contains(f.as_str()) {
                return Err(anyhow::Error::new(FailSafe("corner".into())));
            }
        }
        if self.timed {
            self.s.lock().last_timing = Some(json!({"capture": 37.0, "settle": 330.0, "exec": 402.5}));
        }
        Ok(self.results.get(code).cloned().unwrap_or_else(|| vec![text(&format!("ran {code}"))]))
    }

    async fn close(&self) {
        self.s.lock().closed = true;
    }

    async fn lane_first(&self, goal: &str) -> Option<anyhow::Result<Value>> {
        match &self.route {
            Route::None => None,
            Route::Value(v) => {
                self.s.lock().routed.push(goal.to_string());
                Some(Ok(v.clone()))
            }
            Route::Error(e) => {
                self.s.lock().routed.push(goal.to_string());
                Some(Err(anyhow::Error::new(e.clone())))
            }
        }
    }

    async fn outline(&self) -> anyhow::Result<Value> {
        match &self.outline {
            Some(o) => Ok(o.clone()),
            None => Err(PyError::err("AttributeError", "'FakeWorker' object has no attribute 'outline'")),
        }
    }

    async fn run_plan(
        &self,
        plan: &Value,
        _request: &str,
        start: i64,
        approved: &IndexMap<String, String>,
        _success_rejected: bool,
    ) -> anyhow::Result<Value> {
        if !self.plans {
            return Err(PyError::err("AttributeError", "'FakeWorker' object has no attribute 'run_plan'"));
        }
        let mut s = self.s.lock();
        s.runs.push(json!({"plan": plan, "start": start, "approved": approved}));
        Ok(s.results.pop_front().expect("a scripted plan result"))
    }

    fn last_timing(&self) -> Option<Value> {
        if self.timed {
            self.s.lock().last_timing.clone()
        } else {
            None
        }
    }
}

type Events = Arc<Mutex<Vec<AgentEvent>>>;

struct Rig {
    cfg: Option<AgentConfig>,
    clock: Option<ClockFn>,
    ask: Option<AskUser>,
    browser: Option<Arc<dyn Worker>>,
    no_sleep: bool,
    max_turns: i64,
}

impl Default for Rig {
    fn default() -> Self {
        Rig { cfg: None, clock: None, ask: None, browser: None, no_sleep: false, max_turns: 25 }
    }
}

fn base_cfg(tmp: &Path) -> AgentConfig {
    // These tests drive the PLANNER loop. The tiers in front of it (task_router reflexes, the lane router)
    // answer some of these goals before the planner is ever called, and have their own tests below.
    AgentConfig { runs_dir: tmp.join("runs"), reflexes: false, lane_first: false, ..AgentConfig::default() }
}

fn agent_with(client: &FakeClient, worker: Arc<dyn Worker>, tmp: &Path, rig: Rig) -> (Arc<ComputerAgent>, Events) {
    let events: Events = Arc::new(Mutex::new(Vec::new()));
    let cfg = rig.cfg.unwrap_or_else(|| AgentConfig { max_turns: rig.max_turns, ..base_cfg(tmp) });
    let sink = events.clone();
    let on_event: OnEvent = Arc::new(move |e: &AgentEvent| sink.lock().push(e.clone()));
    let w = worker.clone();
    let options = AgentOptions {
        worker_factory: Some(Arc::new(move || w.clone())),
        config: Some(cfg),
        on_event: Some(on_event),
        ask_user: rig.ask,
        clock: Some(rig.clock.unwrap_or_else(|| Arc::new(|| 1.0))),
        sleep: if rig.no_sleep { Some(Arc::new(|_s: f64| Box::pin(async {}))) } else { None },
        browser: rig.browser,
        apps: Some(Arc::new(|| APPS.iter().map(|s| s.to_string()).collect())),
        ledger: Some(eve_spend::Ledger::at(tmp.join("spend"))),
        meter: Some(Arc::new(eve_jev::Meter::new())),
    };
    (Arc::new(ComputerAgent::with_options(client.create(), options)), events)
}

fn agent(client: &FakeClient, worker: Arc<dyn Worker>, tmp: &Path) -> (Arc<ComputerAgent>, Events) {
    agent_with(client, worker, tmp, Rig::default())
}

fn kinds(events: &Events) -> Vec<String> {
    events.lock().iter().map(|e| e.kind().to_string()).collect()
}

fn log_lines(a: &ComputerAgent) -> Vec<Value> {
    let text = std::fs::read_to_string(a.run_log().expect("a run log")).unwrap();
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn log_text(a: &ComputerAgent) -> String {
    std::fs::read_to_string(a.run_log().expect("a run log")).unwrap()
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn effort_of(req: &Value) -> Value {
    req["reasoning"].clone()
}

fn first_text(req: &Value) -> String {
    req["input"][0]["content"][0]["text"].as_str().unwrap_or("").to_string()
}

async fn wait_until(mut f: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if f() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("condition never held");
}

// ---- config / classification -------------------------------------------------------------------------------

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn test_configured_defaults_and_env() {
    let c = configured(Some(&env(&[])));
    assert!(c.enabled && c.model == "gpt-6-astra" && c.max_turns == 25);
    let c = configured(Some(&env(&[
        ("EVE_COMPUTER_CONTROL", "off"),
        ("EVE_AGENT_MODEL", "gpt-5.6-sol"),
        ("EVE_AGENT_MAX_TURNS", "3"),
    ])));
    assert!(!c.enabled && c.model == "gpt-5.6-sol" && c.max_turns == 3);
    assert_eq!(configured(Some(&env(&[("EVE_AGENT_MAX_TURNS", "lots")]))).max_turns, 25);
    assert_eq!(configured(Some(&env(&[("EVE_AGENT_MAX_TURNS", "0")]))).max_turns, 1);
    assert_eq!(configured(Some(&env(&[("EVE_AGENT_RUNS_DIR", "/tmp/x")]))).runs_dir, PathBuf::from("/tmp/x"));
}

#[test]
fn test_configured_reads_new_knobs_and_wires_exec_timeout() {
    let c = configured(Some(&env(&[])));
    assert_eq!((c.reasoning_effort.as_str(), c.plan_reasoning_effort.as_str()), ("medium", "high"));
    assert!(c.verify && c.verify_reasoning_effort == "low");
    assert!(!configured(Some(&env(&[("EVE_AGENT_VERIFY", "off")]))).verify);
    assert_eq!(configured(Some(&env(&[("EVE_AGENT_PLAN_REASONING", "xhigh")]))).plan_reasoning_effort, "xhigh");
    assert_eq!(configured(Some(&env(&[("EVE_AGENT_PLAN_REASONING", "max")]))).plan_reasoning_effort, "max");
    assert_eq!((c.exec_timeout_secs, c.max_secs, c.api_timeout_secs), (60.0, 180.0, 90.0));
    let c = configured(Some(&env(&[
        ("EVE_AGENT_REASONING", "medium"),
        ("EVE_AGENT_PLAN_REASONING", "high"),
        ("EVE_AGENT_EXEC_TIMEOUT", "30"),
        ("EVE_AGENT_MAX_SECS", "60"),
    ])));
    assert_eq!(
        (c.reasoning_effort.as_str(), c.plan_reasoning_effort.as_str(), c.exec_timeout_secs, c.max_secs),
        ("medium", "high", 30.0, 60.0)
    );
    assert_eq!(configured(Some(&env(&[("EVE_AGENT_REASONING", "turbo")]))).reasoning_effort, "medium");
    assert_eq!(configured(Some(&env(&[("EVE_AGENT_REASONING", " HIGH ")]))).reasoning_effort, "high");
    assert_eq!(configured(Some(&env(&[("EVE_AGENT_EXEC_TIMEOUT", "3")]))).exec_timeout_secs, 10.0);
    assert_eq!(configured(Some(&env(&[("EVE_AGENT_MAX_SECS", "5")]))).max_secs, 30.0);
    assert_eq!(configured(Some(&env(&[("EVE_AGENT_MAX_SECS", "soon")]))).max_secs, 180.0);
    assert_eq!(configured(Some(&env(&[("EVE_PLAN_EXEC_REASONING", "medium")]))).plan_exec_effort, "medium");
    // the default worker hands exec_timeout_secs to the WorkerClient
    let w = WorkerClient::new(AgentConfig { exec_timeout_secs: 30.0, ..AgentConfig::default() }.exec_timeout_secs);
    assert_eq!(w.timeout_secs, 30.0);
}

#[tokio::test]
async fn test_a_worker_that_cannot_start_is_a_generic_failure() {
    // the Spy of the Python test: a worker whose start fails is "failed unexpectedly", and it is still closed
    struct Spy(AtomicBool);
    #[async_trait]
    impl Worker for Spy {
        async fn start(&self) -> anyhow::Result<Value> {
            Err(PyError::err("RuntimeError", "spy: no desktop here"))
        }
        async fn close(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let t = tmp();
    let spy = Arc::new(Spy(AtomicBool::new(false)));
    let (a, events) = agent(&FakeClient::new(vec![]), spy.clone(), t.path());
    assert_eq!(a.run("x").await, "Sorry, that failed unexpectedly.");
    assert!(spy.0.load(Ordering::SeqCst));
    assert_eq!(events.lock().last().unwrap().text(), "RuntimeError: spy: no desktop here");
}

fn exec_tool(tools: &Value) -> Value {
    tools.as_array().unwrap().iter().find(|t| t["name"] == "exec_py").unwrap().clone()
}

#[test]
fn test_tool_contract_is_the_documented_exec_py_shape() {
    let t = exec_tool(&TOOLS);
    assert!(t["type"] == "function" && t["strict"] == json!(true));
    assert_eq!(t["parameters"]["required"], json!(["code"]));
    assert_eq!(t["parameters"]["additionalProperties"], json!(false));
    assert!(INSTRUCTIONS.contains("ask_user") && INSTRUCTIONS.contains("FAILSAFE"));
}

#[test]
fn test_instructions_tool_text_and_helper_names_agree() {
    use eve_desktop_lanes::desktop_helpers::HELPER_NAMES;
    for name in HELPER_NAMES {
        assert!(INSTRUCTIONS.contains(name), "{name}");
    }
    for phrase in ["never Spotlight", "log()", "ask_user", "FAILSAFE"] {
        assert!(INSTRUCTIONS.contains(phrase), "{phrase}");
    }
    assert!(!INSTRUCTIONS.contains("start with `display") && !INSTRUCTIONS.contains("command+space"));
    let code_desc = exec_tool(&TOOLS)["parameters"]["properties"]["code"]["description"].as_str().unwrap().to_string();
    for name in HELPER_NAMES {
        assert!(code_desc.contains(name), "{name}");
    }
}

#[test]
fn test_classify_calls_final_commentary() {
    let c = classify_response(&response("r1", vec![json!({"type": "reasoning"}), exec("c1", "x")])).unwrap();
    assert!(c.kind == "calls" && c.calls[0].args == *json!({"code": "x"}).as_object().unwrap());
    assert_eq!(c.calls[0].to_dict(), json!({"name": "exec_py", "call_id": "c1", "args": {"code": "x"}}));
    assert_eq!(classify_response(&response("r2", vec![message("done")])).unwrap().kind, "final");
    assert_eq!(classify_response(&response("r3", vec![])).unwrap().kind, "commentary");
    let c = classify_response(&response("r4", vec![message("looking"), exec("c2", "y")])).unwrap();
    assert!(c.kind == "calls" && c.text == "looking");
}

#[test]
fn test_classify_rejects_bad_replies() {
    let mut r = response("r", vec![]);
    r["status"] = json!("incomplete");
    let e = classify_response(&r).unwrap_err();
    assert_eq!(e.to_string(), "Responses API did not complete (status 'incomplete')");
    assert_eq!(error_name(&e), "RuntimeError");
    assert!(classify_response(&response("r", vec![call("rm_rf", "c1", json!({}))])).is_err());
    let bad = json!({"type": "function_call", "name": "exec_py", "call_id": "c", "arguments": "{"});
    let e = classify_response(&response("r", vec![bad])).unwrap_err();
    assert_eq!(e.to_string(), "function call arguments are not JSON");
    let list = json!({"type": "function_call", "name": "exec_py", "call_id": "c", "arguments": "[1]"});
    assert_eq!(classify_response(&response("r", vec![list])).unwrap_err().to_string(), "function call arguments must be an object");
    assert_eq!(classify_response(&json!({"status": "completed", "output": []})).unwrap_err().to_string(), "malformed Responses API reply");
    let failed = json!({"id": "r", "status": "failed", "error": {"message": "boom"}, "output": []});
    assert_eq!(classify_response(&failed).unwrap_err().to_string(), "Responses API did not complete (status 'failed') boom");
}

#[test]
fn test_classify_turns_a_refusal_into_the_answer() {
    let refusal = json!({"type": "message", "phase": "commentary", "content": [{"type": "refusal", "refusal": ""}]});
    let c = classify_response(&response("r", vec![refusal])).unwrap();
    assert!(c.kind == "final" && c.text == "I can't do that.");
}

// ---- the loop ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_happy_path_chains_outputs_and_ends_on_final() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "display(pyautogui.screenshot())")]),
        response("r2", vec![message("Looking at it now."), exec("c2", "pyautogui.click(1, 2)")]),
        response("r3", vec![message("Done, your mail is open.")]),
    ]);
    let worker = Arc::new(FakeWorker::with(vec![("display(pyautogui.screenshot())", vec![image()])]));
    let (a, events) = agent(&client, worker.clone(), t.path());
    let fin = a.run("open my mail").await;
    assert_eq!(fin, "Done, your mail is open.");
    assert!(worker.s.lock().started && worker.closed());
    assert_eq!(worker.executed(), vec!["display(pyautogui.screenshot())", "pyautogui.click(1, 2)"]);
    // request shapes: turn 1 carries the goal, the context line and the observe screenshot
    let reqs = client.requests();
    let r1 = &reqs[0];
    assert!(r1["model"] == "gpt-6-astra" && r1.get("previous_response_id").is_none());
    let input = r1["input"].as_array().unwrap();
    assert_eq!(input.len(), 1);
    let first = &input[0];
    assert!(first["type"] == "message" && first["role"] == "user");
    assert_eq!(first["content"][0]["type"], "input_text");
    let t0 = first["content"][0]["text"].as_str().unwrap();
    assert!(t0.contains("Goal: open my mail") && t0.contains("frontmost: Warp"));
    assert_eq!(first["content"][1], image());
    assert!(r1["tools"] == *TOOLS && r1["parallel_tool_calls"] == json!(false));
    assert_eq!(new_items(&client, 1, None), vec![json!({"type": "function_call_output", "call_id": "c1", "output": [image()]})]);
    assert_eq!(new_items(&client, 2, None)[0]["output"], json!([{"type": "input_text", "text": "ran pyautogui.click(1, 2)"}]));
    assert_eq!(kinds(&events), vec!["started", "turn", "exec", "turn", "commentary", "exec", "progress", "turn", "final"]);
    let st = a.status();
    assert!(st["running"] == json!(false) && st["final"] == json!(fin));
}

#[tokio::test]
async fn test_first_turn_carries_context_and_plan_effort() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![exec("c1", "a")]), response("r2", vec![message("ok")])]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::new()), t.path());
    a.run("x").await;
    let reqs = client.requests();
    assert_eq!(effort_of(&reqs[0]), json!({"effort": "high"}));
    assert_eq!(effort_of(&reqs[1]), json!({"effort": "medium"}));
    assert!(reqs.iter().all(|r| r["timeout"] == json!(90.0)));
}

#[tokio::test]
async fn test_step_cap_ends_the_run() {
    let t = tmp();
    let client = FakeClient::new((0..10).map(|i| response(&format!("r{i}"), vec![exec(&format!("c{i}"), &format!("step{i}"))])).collect());
    let (a, events) = agent_with(&client, Arc::new(FakeWorker::new()), t.path(), Rig { max_turns: 3, ..Rig::default() });
    let fin = a.run("loop forever").await;
    assert!(fin.contains("ran out of steps (3)"));
    assert!(client.requests().len() == 3 && events.lock().last().unwrap().kind() == "final");
}

#[tokio::test]
async fn test_wall_clock_budget_ends_the_run() {
    let t = tmp();
    let client = FakeClient::new((0..10).map(|i| response(&format!("r{i}"), vec![exec(&format!("c{i}"), &format!("step{i}"))])).collect());
    let cfg = AgentConfig { runs_dir: t.path().join("runs"), max_secs: 150.0, ..AgentConfig::default() };
    let counter = client.clone();
    let clock: ClockFn = Arc::new(move || 100.0 * counter.calls() as f64);
    let (a, events) = agent_with(&client, Arc::new(FakeWorker::new()), t.path(), Rig { cfg: Some(AgentConfig { reflexes: false, lane_first: false, ..cfg }), clock: Some(clock), ..Rig::default() });
    let fin = a.run("slow").await;
    assert!(fin.starts_with("I ran out of time (150 s)"), "{fin}");
    assert!(client.requests().len() == 2 && events.lock().last().unwrap().kind() == "final");
}

#[tokio::test]
async fn test_commentary_only_turn_continues() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![]), response("r2", vec![message("ok")])]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::new()), t.path());
    assert_eq!(a.run("x").await, "ok");
    assert!(new_items(&client, 1, None).is_empty());
}

#[tokio::test]
async fn test_steer_is_delivered_with_the_next_turn() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "a")]),
        response("r2", vec![exec("c2", "b")]),
        response("r3", vec![message("done")]),
    ]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::new()), t.path());
    assert!(!a.steer("too early")); // not running yet
    let gate = Event::new();
    client.set_gate(Some(gate.clone())); // hold every request until released
    let runner = a.clone();
    let task = tokio::spawn(async move { runner.run("x").await });
    let c = client.clone();
    wait_until(|| c.requests().len() == 1).await; // the run is now waiting on request 1
    assert!(a.steer("actually use safari"));
    gate.set();
    assert_eq!(task.await.unwrap(), "done");
    // the steer rides along with c1's output, tagged, as a user message
    let inp = new_items(&client, 1, None);
    assert_eq!(inp[0]["type"], "function_call_output");
    assert_eq!(inp[1], json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "[steer] actually use safari"}]}));
    let reqs = client.requests();
    assert_eq!(effort_of(&reqs[1]), json!({"effort": "high"})); // a steer is a planning moment
    assert!(new_items(&client, 2, None).iter().all(|i| i["type"] != "message")); // delivered once
    assert_eq!(effort_of(&reqs[2]), json!({"effort": "medium"}));
    assert!(log_lines(&a).iter().any(|l| l.get("steer") == Some(&json!("actually use safari"))));
}

#[tokio::test]
async fn test_cancel_stops_between_turns_and_kills_the_worker() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![exec("c1", "a")]), response("r2", vec![message("never")])]);
    let worker = Arc::new(FakeWorker::new());
    let (a, events) = agent(&client, worker.clone(), t.path());
    let gate = Event::new();
    client.set_gate(Some(gate.clone()));
    let runner = a.clone();
    let task = tokio::spawn(async move { runner.run("x").await });
    let c = client.clone();
    wait_until(|| c.requests().len() == 1).await;
    a.cancel("");
    gate.set();
    assert_eq!(task.await.unwrap(), "Stopped.");
    assert!(worker.closed() && worker.executed().is_empty());
    assert_eq!(events.lock().last().unwrap().kind(), "cancelled");
}

#[tokio::test]
async fn test_cancel_interrupts_an_inflight_request() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![message("never")])]);
    let worker = Arc::new(FakeWorker::new());
    let (a, events) = agent(&client, worker.clone(), t.path());
    client.set_gate(Some(Event::new())); // never set: the request hangs forever
    let runner = a.clone();
    let task = tokio::spawn(async move { runner.run("x").await });
    let c = client.clone();
    wait_until(|| c.requests().len() == 1).await;
    a.cancel("hushed by a touch");
    let said = tokio::time::timeout(std::time::Duration::from_millis(100), task).await.unwrap().unwrap();
    assert_eq!(said, "Stopped: hushed by a touch.");
    assert!(worker.closed() && events.lock().last().unwrap().kind() == "cancelled");
    let lines = log_lines(&a);
    assert_eq!(lines[lines.len() - 3]["cancelled"], "hushed by a touch"); // then final, then the bill
}

#[tokio::test]
async fn test_cancel_interrupts_a_running_exec() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![exec("c1", "wait_for('never')")])]);
    let worker = Arc::new(FakeWorker::new());
    *worker.block.lock() = Some(Event::new()); // the exec never returns on its own
    let (a, _) = agent(&client, worker.clone(), t.path());
    let runner = a.clone();
    let task = tokio::spawn(async move { runner.run("x").await });
    let w = worker.clone();
    wait_until(|| !w.executed().is_empty()).await;
    assert_eq!(worker.executed(), vec!["wait_for('never')"]);
    a.cancel("");
    let said = tokio::time::timeout(std::time::Duration::from_millis(100), task).await.unwrap().unwrap();
    assert_eq!(said, "Stopped.");
    assert!(worker.closed());
}

fn asker(answers: Arc<Mutex<Vec<String>>>, asked: Arc<Mutex<Vec<String>>>) -> AskUser {
    Arc::new(move |q: String| {
        let answers = answers.clone();
        let asked = asked.clone();
        Box::pin(async move {
            asked.lock().push(q);
            let mut a = answers.lock();
            Ok(if a.is_empty() { String::new() } else { a.remove(0) })
        })
    })
}

#[tokio::test]
async fn test_ask_user_routes_to_the_voice_session() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![call("ask_user", "c1", json!({"question": "Send the email?"}))]),
        response("r2", vec![message("sent")]),
    ]);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let ask = asker(Arc::new(Mutex::new(vec!["yes go ahead".to_string()])), asked.clone());
    let (a, events) = agent_with(&client, Arc::new(FakeWorker::new()), t.path(), Rig { ask: Some(ask), ..Rig::default() });
    assert_eq!(a.run("x").await, "sent");
    assert_eq!(*asked.lock(), vec!["Send the email?"]);
    assert_eq!(new_items(&client, 1, None)[0]["output"], json!([{"type": "input_text", "text": "yes go ahead"}]));
    assert!(events.lock().iter().any(|e| e.kind() == "ask" && e.text() == "Send the email?"));
}

#[tokio::test]
async fn test_ask_user_without_a_listener_answers_no() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![call("ask_user", "c1", json!({"question": "Delete it?"}))]),
        response("r2", vec![message("ok")]),
    ]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::new()), t.path());
    a.run("x").await;
    assert!(new_items(&client, 1, None)[0]["output"][0]["text"].as_str().unwrap().starts_with("no ("));
    assert_eq!(effort_of(&client.requests()[1]), json!({"effort": "high"})); // a "no" means re-plan
}

#[tokio::test]
async fn test_ask_user_that_fails_or_says_nothing_is_a_no() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![call("ask_user", "c1", json!({"question": " "}))]),
        response("r2", vec![message("ok")]),
    ]);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let ask = asker(Arc::new(Mutex::new(vec!["  ".to_string()])), asked.clone());
    let (a, _) = agent_with(&client, Arc::new(FakeWorker::new()), t.path(), Rig { ask: Some(ask), ..Rig::default() });
    a.run("x").await;
    assert_eq!(*asked.lock(), vec!["May I continue?"]);
    assert_eq!(new_items(&client, 1, None)[0]["output"][0]["text"], "no (no answer — do not proceed with that action)");
    let client = FakeClient::new(vec![
        response("r1", vec![call("ask_user", "c1", json!({"question": "Pay?"}))]),
        response("r2", vec![message("ok")]),
    ]);
    let broken: AskUser = Arc::new(|_q: String| Box::pin(async { Err(PyError::err("RuntimeError", "voice gone")) }));
    let (a, _) = agent_with(&client, Arc::new(FakeWorker::new()), t.path(), Rig { ask: Some(broken), ..Rig::default() });
    a.run("x").await;
    assert_eq!(new_items(&client, 1, None)[0]["output"][0]["text"], "no (could not reach the human — do not proceed with that action)");
}

#[tokio::test]
async fn test_failsafe_ends_the_run() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![exec("c1", "corner")])]);
    let mut w = FakeWorker::new();
    w.failsafe_on = Some("corner".into());
    let worker = Arc::new(w);
    let (a, events) = agent(&client, worker.clone(), t.path());
    assert!(a.run("x").await.starts_with("Stopped: the mouse"));
    assert!(worker.closed() && events.lock().last().unwrap().kind() == "cancelled");
}

#[tokio::test]
async fn test_client_error_is_reported_not_raised() {
    let t = tmp();
    let mut r = response("r1", vec![]);
    r["status"] = json!("failed");
    let client = FakeClient::new(vec![r]);
    let (a, events) = agent(&client, Arc::new(FakeWorker::new()), t.path());
    assert!(a.run("x").await.starts_with("Sorry, that failed"));
    assert_eq!(events.lock().last().unwrap().kind(), "error");
}

// ---- pacing: effort, repeats, retries, continuation ------------------------------------------------------------

fn no_effect() -> Vec<Value> {
    vec![text("[after your input] frontmost: Spotify — 'Spotify'; screen: unchanged")]
}
fn acted() -> Vec<Value> {
    vec![text("[after your input] frontmost: Spotify — 'Spotify'; screen: changed around (10,10,20,20)")]
}
fn looked() -> Vec<Value> {
    vec![text("[after] frontmost: Spotify — 'Spotify'; screen: unchanged")]
}

fn verdict_response(rid: &str, valid: bool, guidance: &str) -> Value {
    response(rid, vec![message(&serde_json::to_string(&json!({"valid": valid, "guidance": guidance})).unwrap())])
}

#[tokio::test]
async fn test_repeat_exec_gets_a_note_when_the_input_changed_nothing() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "pyautogui.click(1, 2)")]),
        response("r2", vec![exec("c2", "pyautogui.click(1, 2)")]),
        response("r3", vec![message("stuck")]),
        verdict_response("v1", true, ""),
    ]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::with(vec![("pyautogui.click(1, 2)", no_effect())])), t.path());
    a.run("x").await;
    let reqs = client.requests();
    assert_eq!(effort_of(&reqs[1]), json!({"effort": "medium"}));
    let inp = new_items(&client, 2, None);
    let notes: Vec<&Value> = inp.iter().filter(|i| i["type"] == "message").collect();
    assert!(notes.len() == 1 && notes[0]["content"][0]["text"].as_str().unwrap().starts_with("[note] That is the same code"));
    assert_eq!(effort_of(&reqs[2]), json!({"effort": "high"}));
    assert!(log_lines(&a).iter().any(|l| l.get("repeat") == Some(&json!(true))));
}

#[tokio::test]
async fn test_repeat_exec_that_changed_the_screen_gets_no_repeat_note() {
    // Scrolling twice is a repeat that works; the note used to accuse it of changing nothing.
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "pyautogui.scroll(-10)")]),
        response("r2", vec![exec("c2", "pyautogui.scroll(-10)")]),
        response("r3", vec![message("scrolled")]),
        verdict_response("v1", true, ""),
    ]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::with(vec![("pyautogui.scroll(-10)", acted())])), t.path());
    a.run("x").await;
    assert!(!new_items(&client, 2, None).iter().any(|i| i["type"] == "message"));
    assert_eq!(effort_of(&client.requests()[2]), json!({"effort": "medium"}));
    assert!(!log_lines(&a).iter().any(|l| l.get("repeat").is_some()));
}

#[tokio::test]
async fn test_error_result_escalates_effort_once() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "bad")]),
        response("r2", vec![exec("c2", "good")]),
        response("r3", vec![exec("c3", "better")]),
        response("r4", vec![message("ok")]),
    ]);
    let worker = FakeWorker::with(vec![("bad", vec![text("Traceback (most recent call last):\n  boom")])]);
    let (a, _) = agent(&client, Arc::new(worker), t.path());
    a.run("x").await;
    let efforts: Vec<Value> = client.requests().iter().map(|r| r["reasoning"]["effort"].clone()).collect();
    assert_eq!(efforts, vec![json!("high"), json!("high"), json!("medium"), json!("medium")]);
}

#[tokio::test]
async fn test_transient_api_error_retries_once() {
    let t = tmp();
    let client = FakeClient::raising(vec![response("r1", vec![message("ok")])], vec![PyError::new("APIConnectionError", "down")]);
    let (a, _) = agent_with(&client, Arc::new(FakeWorker::new()), t.path(), Rig { no_sleep: true, ..Rig::default() });
    assert_eq!(a.run("x").await, "ok");
    let reqs = client.requests();
    assert!(reqs.len() == 2 && reqs[0] == reqs[1]);
    assert!(log_lines(&a).iter().any(|l| l.get("retry") == Some(&json!("APIConnectionError"))));
    let client = FakeClient::raising(
        vec![response("r1", vec![message("ok")])],
        vec![PyError::new("APIConnectionError", "down"), PyError::new("APIConnectionError", "still down")],
    );
    let (a, events) = agent_with(&client, Arc::new(FakeWorker::new()), t.path(), Rig { no_sleep: true, ..Rig::default() });
    assert_eq!(a.run("x").await, "Sorry, I lost my connection to the model service.");
    let last = events.lock().last().unwrap().clone();
    assert!(last.kind() == "error" && last.text().starts_with("APIConnectionError"));
}

#[tokio::test]
async fn test_a_non_transient_api_error_is_not_retried() {
    let t = tmp();
    let client = FakeClient::raising(vec![response("r1", vec![message("ok")])], vec![PyError::new("BadRequestError", "no")]);
    let (a, _) = agent_with(&client, Arc::new(FakeWorker::new()), t.path(), Rig { no_sleep: true, ..Rig::default() });
    assert_eq!(a.run("x").await, "Sorry, that failed unexpectedly.");
    assert_eq!(client.calls(), 1);
}

#[tokio::test]
async fn test_incomplete_response_is_continued_once() {
    let t = tmp();
    let cut = json!({"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}});
    let client = FakeClient::new(vec![response_with("r1", vec![], cut.clone()), response("r2", vec![message("finished")])]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::new()), t.path());
    assert_eq!(a.run("x").await, "finished");
    assert_eq!(
        new_items(&client, 1, None),
        vec![json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "[note] Your reply was cut off; continue."}]})]
    );
    let client = FakeClient::new(vec![response_with("r1", vec![], cut.clone()), response_with("r2", vec![], cut)]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::new()), t.path());
    assert_eq!(a.run("x").await, "Sorry, that failed unexpectedly.");
}

#[test]
fn test_failure_text_is_human() {
    assert!(describe_failure(&anyhow::Error::new(WorkerDead("deadline, twice".into()))).contains("crashed twice"));
    assert!(describe_failure(&PyError::err("APITimeoutError", "slow")).contains("lost my connection"));
    assert_eq!(describe_failure(&PyError::err("ValueError", "x")), "Sorry, that failed unexpectedly.");
    assert!(describe_failure(&PyError::err("RuntimeError", "Screen Recording is not granted")).contains("grant Screen Recording"));
    assert!(describe_failure(&PyError::err("RateLimitError", "busy")).contains("busy; try again"));
}

#[tokio::test]
async fn test_progress_events_from_short_text_lines() {
    let t = tmp();
    let result: Vec<Value> = [
        "opened Spotify",
        "{\"app\": \"Spotify\", \"title\": \"Spotify\"}",
        "[after] frontmost: Spotify — 'Spotify'; screen: changed",
        "Traceback (most recent call last):\n  x",
    ]
    .iter()
    .map(|s| text(s))
    .collect();
    let client = FakeClient::new(vec![response("r1", vec![exec("c1", "open_app('Spotify')")]), response("r2", vec![message("ok")])]);
    let (a, events) = agent(&client, Arc::new(FakeWorker::with(vec![("open_app('Spotify')", result)])), t.path());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (sink, seen2, me) = (events.clone(), seen.clone(), Arc::downgrade(&a));
    a.set_on_event(Some(Arc::new(move |ev: &AgentEvent| {
        sink.lock().push(ev.clone());
        if ev.kind() == "progress" {
            if let Some(a) = me.upgrade() {
                seen2.lock().push(a.status()["last"].as_str().unwrap().to_string());
            }
        }
    })));
    a.run("x").await;
    let progress: Vec<AgentEvent> = events.lock().iter().filter(|e| e.kind() == "progress").cloned().collect();
    assert_eq!(progress, vec![AgentEvent::with_turn("progress", "opened Spotify", 1)]);
    assert_eq!(*seen.lock(), vec!["opened Spotify"]); // status()["last"] while the task runs
    let lines = progress_lines(&[
        text("hi"),
        json!({"type": "input_image"}),
        text("typed 22 characters"),
        text("typed 22 characters"),
    ]);
    assert_eq!(lines, vec!["typed 22 characters"]);
}

#[tokio::test]
async fn test_action_log_records_goal_exec_results_and_final() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "display(pyautogui.screenshot())")]),
        response("r2", vec![message("all done")]),
    ]);
    let worker = FakeWorker::with(vec![(
        "display(pyautogui.screenshot())",
        vec![text("hi"), json!({"type": "input_image", "detail": "original", "image_url": "data:x"})],
    )]);
    let (a, _) = agent(&client, Arc::new(worker), t.path());
    a.run("look").await;
    let log = a.run_log().unwrap();
    assert_eq!(log.parent().unwrap(), t.path().join("runs"));
    let lines = log_lines(&a);
    assert!(lines[0]["goal"] == "look" && lines[0]["model"] == "gpt-6-astra");
    assert_eq!(lines[1], json!({"t": 1.0, "worker": {"width": 100, "height": 50}})); // the ready line, minus "ready"
    assert_eq!(lines[2], json!({"t": 1.0, "turn": 0, "context": "frontmost: Warp — 'zsh'; screen 100x50; 14:02"}));
    assert_eq!(lines[3], json!({"t": 1.0, "turn": 1, "effort": "high", "api_secs": 0.0}));
    assert_eq!(lines[4]["exec"], "display(pyautogui.screenshot())");
    assert_eq!(lines[5], json!({"t": 1.0, "turn": 1, "result": ["hi"], "images": 1})); // no image bytes, no timing
    assert!(lines[lines.len() - 2]["final"] == "all done" && lines[lines.len() - 1].get("bill").is_some());
    let raw = log_text(&a);
    assert!(!raw.contains("data:x") && !raw.contains("AAAA"));
    // the log is Python's json.dumps(..., ensure_ascii=False): ", " and ": " separators, non-ASCII as itself
    let third = raw.lines().nth(2).unwrap();
    assert_eq!(third, r#"{"t": 1.0, "turn": 0, "context": "frontmost: Warp — 'zsh'; screen 100x50; 14:02"}"#);
}

#[tokio::test]
async fn test_an_unwritable_runs_dir_still_runs() {
    let t = tmp();
    let file = t.path().join("not-a-dir");
    std::fs::write(&file, "x").unwrap();
    let client = FakeClient::new(vec![response("r1", vec![message("ok")])]);
    let cfg = AgentConfig { runs_dir: file.join("runs"), ..base_cfg(t.path()) };
    let (a, _) = agent_with(&client, Arc::new(FakeWorker::new()), t.path(), Rig { cfg: Some(cfg), ..Rig::default() });
    assert_eq!(a.run("x").await, "ok");
    assert!(a.run_log().is_none());
}

// ---- phase, usage, and the final-answer check -------------------------------------------------------------------

fn phased(t: &str, phase: &str) -> Value {
    let mut m = message(t);
    m["phase"] = json!(phase);
    m
}

#[test]
fn test_classify_reads_phase() {
    let c = classify_response(&response("r1", vec![phased("Spotify is playing, let me check", "commentary")])).unwrap();
    assert!(c.kind == "commentary" && c.text == "Spotify is playing, let me check" && c.phases == vec!["commentary"]);
    let c = classify_response(&response("r2", vec![phased("looking", "commentary"), phased("It is playing.", "final_answer")])).unwrap();
    assert!(c.kind == "final" && c.text == "It is playing.");
    assert_eq!(classify_response(&response("r3", vec![message("no phase is an answer")])).unwrap().kind, "final");
}

#[tokio::test]
async fn test_phase_commentary_does_not_end_the_run() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![phased("Spotify is playing, let me check", "commentary")]),
        response("r2", vec![phased("Spotify is playing.", "final_answer")]),
    ]);
    let (a, events) = agent(&client, Arc::new(FakeWorker::new()), t.path());
    assert_eq!(a.run("play").await, "Spotify is playing.");
    assert!(new_items(&client, 1, None).is_empty());
    assert_eq!(kinds(&events).iter().filter(|k| *k == "commentary").count(), 1);
    let phases: Vec<Value> = log_lines(&a).iter().filter_map(|l| l.get("phase").cloned()).collect();
    assert_eq!(phases, vec![json!(["commentary"]), json!(["final_answer"])]);
}

#[tokio::test]
async fn test_usage_tokens_are_logged_per_turn() {
    let t = tmp();
    let usage = json!({"input_tokens": 1200, "output_tokens": 90, "output_tokens_details": {"reasoning_tokens": 64}});
    let client = FakeClient::new(vec![response_with("r1", vec![message("ok")], json!({"usage": usage}))]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::new()), t.path());
    a.run("x").await;
    let lines: Vec<Value> = log_lines(&a).into_iter().filter(|l| l.get("api_secs").is_some()).collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["tokens"], json!({"in": 1200, "out": 90, "reasoning": 64}));
}

#[tokio::test]
async fn test_verifier_passes_a_confirmed_claim() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "pyautogui.press('space')")]),
        response("r2", vec![message("Spotify is playing.")]),
        verdict_response("v1", true, ""),
    ]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::with(vec![("pyautogui.press('space')", acted())])), t.path());
    assert_eq!(a.run("play spotify").await, "Spotify is playing.");
    assert_eq!(client.requests().len(), 3);
    let valid: Vec<Value> = log_lines(&a).iter().filter_map(|l| l.get("verify").map(|v| v["valid"].clone())).collect();
    assert_eq!(valid, vec![json!(true)]);
}

#[tokio::test]
async fn test_verifier_sends_back_an_unconfirmed_claim_once() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "pyautogui.press('space')")]),
        response("r2", vec![message("Spotify is playing.")]),
        verdict_response("v1", false, "Finder is in front and Spotify shows a play button."),
        response("r3", vec![exec("c3", "open_app('Spotify')")]),
        response("r4", vec![message("Now Spotify is playing.")]),
        verdict_response("v2", true, ""),
    ]);
    let worker = FakeWorker::with(vec![("pyautogui.press('space')", acted()), ("open_app('Spotify')", acted())]);
    let (a, _) = agent(&client, Arc::new(worker), t.path());
    assert_eq!(a.run("play spotify").await, "Now Spotify is playing.");
    let retry = client.requests()[3].clone();
    assert_eq!(effort_of(&retry), json!({"effort": "high"}));
    let items = new_items(&client, 3, Some(1)); // the loop's request before it is r2's (index 1)
    assert_eq!(items.len(), 1);
    let note = items[0]["content"][0]["text"].as_str().unwrap();
    assert!(note.starts_with("[note] A fresh screenshot does not confirm your answer:"));
    assert!(note.contains("Finder is in front"));
}

#[tokio::test]
async fn test_verifier_second_failure_says_it_could_not_confirm() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "pyautogui.press('space')")]),
        response("r2", vec![message("Spotify is playing.")]),
        verdict_response("v1", false, "A play button is showing."),
        response("r3", vec![message("It is playing now.")]),
        verdict_response("v2", false, "A play button is still showing."),
    ]);
    let (a, events) = agent(&client, Arc::new(FakeWorker::with(vec![("pyautogui.press('space')", acted())])), t.path());
    let fin = a.run("play spotify").await;
    assert_eq!(fin, "I couldn't confirm that on screen. It is playing now.");
    assert_eq!(*events.lock().last().unwrap(), AgentEvent::with_turn("final", fin, 3)); // turn 3: the retried answer
}

#[tokio::test]
async fn test_verifier_skipped_for_read_only_runs() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "log(screen_text())")]),
        response("r2", vec![message("Google shows 60°F.")]),
    ]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::with(vec![("log(screen_text())", looked())])), t.path());
    assert_eq!(a.run("weather").await, "Google shows 60°F.");
    assert_eq!(client.requests().len(), 2); // no check: a run that only looked cannot have misfired
}

#[tokio::test]
async fn test_verifier_failure_lets_the_answer_through() {
    let t = tmp();
    let mut failed = response("v1", vec![]);
    failed["status"] = json!("failed");
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "pyautogui.press('space')")]),
        response("r2", vec![message("Spotify is playing.")]),
        failed,
    ]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::with(vec![("pyautogui.press('space')", acted())])), t.path());
    assert_eq!(a.run("play spotify").await, "Spotify is playing.");
    let entries: Vec<Value> = log_lines(&a).iter().filter_map(|l| l.get("verify").cloned()).collect();
    assert_eq!(entries, vec![json!({"error": "RuntimeError"})]);
}

#[tokio::test]
async fn test_verifier_request_shape() {
    let t = tmp();
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "pyautogui.press('space')")]),
        response("r2", vec![message("Spotify is playing.")]),
        verdict_response("v1", true, ""),
    ]);
    let (a, _) = agent(&client, Arc::new(FakeWorker::with(vec![("pyautogui.press('space')", acted())])), t.path());
    a.run("play spotify").await;
    let req = client.requests()[2].clone();
    assert!(req.get("previous_response_id").is_none() && req.get("tools").is_none()); // judges the screen, not the story
    assert!(req["instructions"] == VERIFY_INSTRUCTIONS && req["store"] == json!(false));
    assert_eq!(req["text"]["format"], json!({"type": "json_schema", "name": "verdict", "schema": *VERIFY_SCHEMA, "strict": true}));
    assert_eq!(effort_of(&req), json!({"effort": "low"}));
    let input = req["input"].as_array().unwrap();
    assert_eq!(input.len(), 1);
    let content = input[0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    let t0 = content[0]["text"].as_str().unwrap();
    assert!(t0.contains("Goal: play spotify") && t0.contains("Spotify is playing."));
    assert!(t0.contains("[after your input] frontmost: Spotify"));
    assert_eq!(content[1], image()); // a FRESH observe screenshot
}

#[test]
fn test_verdict_reads_only_a_real_verdict() {
    assert_eq!(verdict(&verdict_response("v", false, " Finder. ")).unwrap(), (false, "Finder.".to_string()));
    assert_eq!(verdict(&response("v", vec![message("{\"valid\": 1}")])).unwrap_err().to_string(), "verifier reply is not a verdict");
    assert_eq!(verdict(&response("v", vec![])).unwrap_err().to_string(), "verifier reply has no text");
    assert!(verdict(&response("v", vec![message("not json")])).is_err());
}

// ---- the fast lane's log fields: worker timing -------------------------------------------------------------------

#[tokio::test]
async fn test_loop_logs_worker_timing() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![exec("c1", "log(screen_text())")]), response("r2", vec![message("done")])]);
    let mut w = FakeWorker::with(vec![("log(screen_text())", looked())]);
    w.timed = true;
    let (a, _) = agent(&client, Arc::new(w), t.path());
    a.run("look").await;
    let entries: Vec<Value> = log_lines(&a).into_iter().filter(|l| l.get("result").is_some()).collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["timing"], json!({"capture": 37.0, "settle": 330.0, "exec": 402.5}));
}

#[tokio::test]
async fn test_worker_ready_line_is_logged() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![message("done")])]);
    let mut w = FakeWorker::new();
    w.ready = json!({"ready": true, "width": 100, "height": 50, "fast_lane": "loading"});
    let (a, _) = agent(&client, Arc::new(w), t.path());
    a.run("x").await;
    let logged = log_lines(&a);
    assert_eq!(logged[1], json!({"t": 1.0, "worker": {"width": 100, "height": 50, "fast_lane": "loading"}}));
}

// ---- the fast lane in the prompt and the tool text ----------------------------------------------------------------

#[test]
fn test_instructions_variants_track_fast_lane_default() {
    use eve_desktop_lanes::fast_lane::FAST_LANE_DEFAULT;
    let off = instructions(false, "keyword");
    let on = instructions(true, "model");
    assert!(!off.contains("delegate") && !serde_json::to_string(&tools(false)).unwrap().contains("delegate"));
    assert!(on.contains("11. delegate(objective") && on.contains("approve=") && on.contains("confirm") && on.contains("escalate"));
    let keyword = instructions(true, "keyword"); // the default decide mode: exact labels, in order
    assert!(keyword.contains("11. delegate(steps=[") && keyword.contains("approve=") && keyword.contains("script: complete"));
    assert!(!keyword.contains("done_when") && !keyword.contains("{fast_lane"));
    assert!(tools(true)[0]["parameters"]["properties"]["code"]["description"].as_str().unwrap().contains(", delegate"));
    assert!(!on.contains("{fast_lane") && !off.contains("{fast_lane"));
    assert_eq!(AgentConfig::default().fast_lane, FAST_LANE_DEFAULT);
    assert_eq!(configured(Some(&env(&[]))).fast_lane, FAST_LANE_DEFAULT);
    assert!(configured(Some(&env(&[("EVE_FAST_LANE", "1")]))).fast_lane);
    assert!(!configured(Some(&env(&[("EVE_FAST_LANE", "off")]))).fast_lane);
    assert!(*INSTRUCTIONS == off && *TOOLS == tools(false));
    // test_fastlane_docs: the prompt teaches the script form
    assert!(keyword.contains("delegate(steps=[") && keyword.contains("script: complete") && keyword.contains("approve="));
}

#[test]
fn test_instructions_tool_text_and_helper_names_agree_in_both_variants() {
    use eve_desktop_lanes::desktop_helpers::{FAST_LANE_HELPERS, HELPER_NAMES};
    for fast_lane in [false, true] {
        let text = instructions(fast_lane, "keyword");
        let code_desc = tools(fast_lane)[0]["parameters"]["properties"]["code"]["description"].as_str().unwrap().to_string();
        for name in HELPER_NAMES {
            assert!(text.contains(name) && code_desc.contains(name), "{name}");
        }
        for name in FAST_LANE_HELPERS {
            assert_eq!(text.contains(name), fast_lane, "{name}");
            assert_eq!(code_desc.contains(name), fast_lane, "{name}");
        }
    }
}

#[tokio::test]
async fn test_loop_sends_the_variant_the_config_selects_and_escalates_on_delegate_lines() {
    let t = tmp();
    let line = vec![text("delegate confirm: \"Send\" needs the human's yes; app=\"Mail\"; action=click; steps=0; last=none; text=none; key=none")];
    let client = FakeClient::new(vec![
        response("r1", vec![exec("c1", "delegate('send it')")]),
        response("r2", vec![exec("c2", "log(1)")]),
        response("r3", vec![message("done")]),
    ]);
    let cfg = AgentConfig { fast_lane: true, ..base_cfg(t.path()) };
    let cfg = AgentConfig { reflexes: true, lane_first: true, ..cfg };
    let (a, _) = agent_with(&client, Arc::new(FakeWorker::with(vec![("delegate('send it')", line)])), t.path(), Rig { cfg: Some(cfg), ..Rig::default() });
    a.run("send the mail").await;
    let reqs = client.requests();
    assert!(reqs[0]["instructions"] == json!(instructions(true, "keyword")) && reqs[0]["tools"] == tools(true));
    assert_eq!(effort_of(&reqs[1]), json!({"effort": "high"})); // the confirm is a recovery turn
    assert_eq!(effort_of(&reqs[2]), json!({"effort": "medium"}));
}

// ---- the bill per task -------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_the_run_log_ends_with_the_bill() {
    let t = tmp();
    let usage = json!({"input_tokens": 1_000_000, "input_tokens_details": {"cached_tokens": 200_000}, "output_tokens": 100_000});
    let client = FakeClient::new(vec![response_with("r1", vec![message("ok")], json!({"usage": usage}))]);
    let ticks = Arc::new(AtomicUsize::new(0));
    let clock: ClockFn = Arc::new(move || if ticks.fetch_add(1, Ordering::SeqCst) < 3 { 1.0 } else { 7.5 });
    let meter = Arc::new(eve_jev::Meter::new());
    meter.add(&json!({"usage": {"input_tokens": 500}}), 230.0, false); // a Jev call made before the run: not billed
    let options = AgentOptions {
        worker_factory: Some(Arc::new(|| Arc::new(FakeWorker::new()) as Arc<dyn Worker>)),
        config: Some(base_cfg(t.path())),
        clock: Some(clock),
        apps: Some(Arc::new(Vec::new)),
        ledger: Some(eve_spend::Ledger::at(t.path().join("spend"))),
        meter: Some(meter),
        ..AgentOptions::default()
    };
    let a = ComputerAgent::with_options(client.create(), options);
    a.run("x").await;
    let lines = log_lines(&a);
    assert!(lines[lines.len() - 1].get("bill").is_some() && lines[lines.len() - 2].get("final").is_some());
    let bill = &lines[lines.len() - 1]["bill"];
    assert!(bill["model"] == "gpt-6-astra" && bill["calls"] == 1);
    assert_eq!(bill["tokens"], json!({"in": 1_000_000, "cached": 200_000, "out": 100_000}));
    assert!((bill["usd"].as_f64().unwrap() - 13.2).abs() < 1e-6);
    assert_eq!(bill["jev"], json!({"calls": 0, "input_tokens": 0, "ms": 0, "errors": 0, "usd": 0.0}));
    assert!((bill["total_usd"].as_f64().unwrap() - 13.2).abs() < 1e-6 && bill["secs"] == json!(6.5));
    // a model the table does not know: tokens yes, dollars null, never a guess
    let cfg = AgentConfig { runs_dir: t.path().join("runs2"), model: "gpt-9-mystery".into(), reflexes: false, lane_first: false, ..AgentConfig::default() };
    let client = FakeClient::new(vec![response_with("r1", vec![message("ok")], json!({"usage": usage}))]);
    let (b, _) = agent_with(&client, Arc::new(FakeWorker::new()), t.path(), Rig { cfg: Some(cfg), ..Rig::default() });
    b.run("y").await;
    let lines = log_lines(&b);
    let bill = &lines[lines.len() - 1]["bill"];
    assert!(bill["usd"].is_null() && bill["total_usd"].is_null() && bill["tokens"]["in"] == 1_000_000);
}

#[tokio::test]
async fn test_jev_calls_during_a_run_are_billed() {
    let t = tmp();
    let client = FakeClient::new(vec![response_with("r1", vec![message("ok")], json!({"usage": {"input_tokens": 10, "output_tokens": 1}}))]);
    let meter = Arc::new(eve_jev::Meter::new());
    let m = meter.clone();
    let mut w = FakeWorker::new();
    w.on_start = Some(Box::new(move || {
        m.add(&json!({"usage": {"input_tokens": 3000}}), 240.0, false); // a step asked mid-run
        m.add(&json!({}), 3000.0, true); // and one that failed
    }));
    let worker: Arc<dyn Worker> = Arc::new(w);
    let options = AgentOptions {
        worker_factory: Some(Arc::new(move || worker.clone())),
        config: Some(base_cfg(t.path())),
        clock: Some(Arc::new(|| 1.0)),
        apps: Some(Arc::new(Vec::new)),
        ledger: Some(eve_spend::Ledger::at(t.path().join("spend"))),
        meter: Some(meter),
        ..AgentOptions::default()
    };
    let a = ComputerAgent::with_options(client.create(), options);
    a.run("x").await;
    let lines = log_lines(&a);
    let bill = &lines[lines.len() - 1]["bill"];
    assert!(bill["jev"]["calls"] == 2 && bill["jev"]["input_tokens"] == 3000 && bill["jev"]["errors"] == 1);
    assert!((bill["jev"]["usd"].as_f64().unwrap() - 3000.0 * 0.042 / 1_000_000.0).abs() < 1e-12 && bill["jev"]["ms"] == 3240);
}

// test_spend_wiring.py: every model call of a run is metered under "browser & Mac tasks"
#[tokio::test]
async fn test_every_computer_agent_model_call_is_metered() {
    let t = tmp();
    let ledger = eve_spend::Ledger::at(t.path().join("spend"));
    let create: CreateResponse = Arc::new(|_req: Value| {
        Box::pin(async { Ok(json!({"id": "r", "usage": {"input_tokens": 1_000_000, "output_tokens": 0}})) })
    });
    let options = AgentOptions {
        config: Some(AgentConfig { model: "gpt-6-astra".into(), runs_dir: t.path().join("runs"), ..AgentConfig::default() }),
        ledger: Some(ledger.clone()),
        ..AgentOptions::default()
    };
    let a = ComputerAgent::with_options(create, options);
    a.create(&json!({"model": "gpt-6-astra"}), 1).await.unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let rows = ledger.day_rows(&today);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert!(row["f"] == "browser & Mac tasks" && row["p"] == "openrouter");
    assert!((row["usd"].as_f64().unwrap() - 10.0).abs() < 1e-9);
}

// ---- the worker client against the real fake child (test_worker_client.py) -------------------------------------------

fn fake_child() -> String {
    format!("{}/../../../bridge/tests/fake_worker_child.py", env!("CARGO_MANIFEST_DIR"))
}

struct Bench {
    released: Arc<AtomicUsize>,
    client: WorkerClient,
}

fn bench() -> Bench {
    let released = Arc::new(AtomicUsize::new(0));
    let r = released.clone();
    let client = WorkerClient::new(0.5)
        .with_program("python3", vec![fake_child()])
        .with_release(Arc::new(move || {
            let r = r.clone();
            Box::pin(async move {
                r.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }));
    Bench { released, client }
}

#[tokio::test]
async fn test_start_reads_ready_and_observe_returns_items() {
    let b = bench();
    let ready = b.client.start().await.unwrap();
    assert!(ready["width"] == 100 && b.client.ready() == ready);
    let items = b.client.observe().await.unwrap();
    let types: Vec<&str> = items.iter().map(|i| i["type"].as_str().unwrap()).collect();
    assert_eq!(types, vec!["input_image", "input_text"]);
    assert!(items[1]["text"].as_str().unwrap().starts_with("frontmost: Warp"));
    assert_eq!(b.client.execute("log(1)").await.unwrap(), vec![text("ran log(1)")]);
    b.client.close().await;
    assert!(b.client.pid().is_none() && b.released.load(Ordering::SeqCst) == 1);
}

#[tokio::test]
async fn test_timeout_restarts_once_then_dies() {
    let b = bench();
    b.client.start().await.unwrap();
    let pid = b.client.pid().unwrap();
    let out = b.client.execute("hang").await.unwrap();
    let first = out[0]["text"].as_str().unwrap();
    assert!(out[0]["type"] == "input_text" && first.contains("was restarted"));
    assert!(first.contains("0 s deadline"), "{first}");
    assert!(out.iter().any(|o| o["type"] == "input_image"));
    assert!(b.client.restarts() == 1 && b.client.pid().is_some() && b.client.pid() != Some(pid));
    assert_eq!(b.client.execute("still here").await.unwrap(), vec![text("ran still here")]);
    let e = b.client.execute("hang").await.unwrap_err();
    assert!(is_a::<WorkerDead>(&e) && e.to_string().contains("twice"));
    assert!(b.client.pid().is_none() && b.released.load(Ordering::SeqCst) == 2);
}

#[tokio::test]
async fn test_exit_is_a_restart_and_failsafe_closes() {
    let b = bench();
    b.client.start().await.unwrap();
    let out = b.client.execute("exit").await.unwrap();
    let first = out[0]["text"].as_str().unwrap();
    assert!(first.contains("desktop helper stopped") && first.contains("was restarted"));
    assert!(b.client.restarts() == 1 && b.released.load(Ordering::SeqCst) == 1);
    let e = b.client.execute("corner").await.unwrap_err();
    assert!(is_a::<FailSafe>(&e));
    assert!(b.client.pid().is_none() && b.released.load(Ordering::SeqCst) == 2);
}

#[tokio::test]
async fn test_large_reply_line_is_accepted() {
    let b = bench();
    b.client.start().await.unwrap();
    let out = b.client.execute("big").await.unwrap();
    assert_eq!(out[0]["text"].as_str().unwrap().len(), 100 * 1024);
    b.client.close().await;
}

#[tokio::test]
async fn test_lane_first_timeout_does_not_restart() {
    let b = bench();
    b.client.start().await.unwrap();
    let pid = b.client.pid();
    assert_eq!(b.client.execute("log(1)").await.unwrap(), vec![text("ran log(1)")]);
    assert_eq!(b.client.last_timing(), Some(json!({"exec": 1.0}))); // every reply's timing is kept
    assert_eq!(b.client.lane_first_with("open notes", LANE_FIRST_TIMEOUT_SECS).await, json!({"status": "none", "reason": "no_match"}));
    assert_eq!(b.client.lane_first_with("hang", 0.3).await, json!({"status": "unavailable", "reason": "timeout"}));
    assert!(b.client.restarts() == 0 && b.client.pid() == pid); // no restart, same child
    assert_eq!(b.client.execute("after").await.unwrap(), vec![text("ran after")]);
    let items = b.client.observe().await.unwrap();
    assert!(b.client.last_timing() == Some(json!({"capture": 1.0, "exec": 2.0})) && items[0]["type"] == "input_image");
    b.client.close().await;
    assert_eq!(b.released.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_slow_lane_first_reply_is_skipped_and_the_next_exec_gets_grace() {
    let b = bench(); // execute deadline 0.5 s; the slow route answers at 0.6 s
    b.client.start().await.unwrap();
    let pid = b.client.pid();
    assert_eq!(b.client.lane_first_with("slow", 0.2).await, json!({"status": "unavailable", "reason": "timeout"}));
    // would time out at 0.5 s behind the stale reply without grace
    assert_eq!(b.client.execute("after").await.unwrap(), vec![text("ran after")]);
    assert!(b.client.restarts() == 0 && b.client.pid() == pid);
    b.client.close().await;
}

#[tokio::test]
async fn test_a_client_with_no_child_answers_softly() {
    // test_agent_lane_first: the client never restarts the worker over a route, and says so when there is none
    let w = WorkerClient::default();
    assert_eq!(w.lane_first_with("x", 1.0).await, json!({"status": "unavailable", "reason": "desktop worker is not running"}));
    assert_eq!(w.outline_with(1.0).await, json!({"app": "", "lines": [], "reason": "desktop worker is not running"}));
    assert_eq!(
        w.run_plan_with(&json!({}), "x", 2, &IndexMap::new(), 1.0, false).await,
        json!({"status": "partial", "next_index": 2, "ledger": [], "reason": "desktop worker is not running"})
    );
    assert_eq!(w.execute("x").await.unwrap_err().to_string(), "desktop worker is not running");
    const { assert!(LANE_FIRST_TIMEOUT_SECS >= 5.0) };
}

#[tokio::test]
async fn test_a_child_that_never_says_ready_fails_to_start() {
    let t = tmp();
    let script = t.path().join("not_ready.py");
    std::fs::write(&script, "import json,sys\nsys.stdout.write(json.dumps({'error': {'message': 'no Accessibility'}}) + '\\n')\nsys.stdout.flush()\n").unwrap();
    let released = Arc::new(AtomicUsize::new(0));
    let r = released.clone();
    let c = WorkerClient::new(1.0)
        .with_program("python3", vec![script.to_string_lossy().into_owned()])
        .with_release(Arc::new(move || {
            let r = r.clone();
            Box::pin(async move {
                r.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }));
    let e = c.start().await.unwrap_err();
    assert_eq!(e.to_string(), "desktop worker failed to start: no Accessibility");
    assert!(c.pid().is_none() && released.load(Ordering::SeqCst) == 1);
}

// ---- lane first (test_agent_lane_first.py) ----------------------------------------------------------------------

fn lane_cfg(tmp: &Path) -> AgentConfig {
    AgentConfig { runs_dir: tmp.join("runs"), lane_first: true, ..AgentConfig::default() }
}

fn complete_route() -> Value {
    json!({"status": "complete", "reason": "", "clicked": ["Week"], "already": [], "sentence": "Done. I clicked Week.",
           "line": "script: complete; applied=[Week (radio button)]; step 1/1 stopped: reason=one_step",
           "log": ["delegate step 1: Week (radio button) (keyword) act", "delegate script: complete"], "ms": 412.0})
}

#[tokio::test]
async fn test_a_lane_decided_task_makes_zero_planner_calls() {
    let t = tmp();
    let client = FakeClient::new(vec![]); // any request would fail "ran out of responses"
    let worker = Arc::new(FakeWorker::lane(Route::Value(complete_route())));
    let (a, events) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(lane_cfg(t.path())), ..Rig::default() });
    let said = a.run("switch to week view").await;
    assert_eq!(said, "Done. I clicked Week.");
    assert!(client.calls() == 0 && client.requests().is_empty()); // no planner turn, and no final-answer check
    assert_eq!(worker.s.lock().routed, vec!["switch to week view"]);
    assert!(worker.observed() == 0 && worker.executed().is_empty());
    assert!(worker.closed() && a.final_answer() == Some(said.clone()));
    let k = kinds(&events);
    assert!(k[0] == "started" && k[k.len() - 1] == "final" && k.contains(&"progress".to_string()));
    let entry = log_lines(&a).into_iter().find(|e| e.get("lane_first").is_some()).unwrap();
    assert!(entry["lane_first"]["status"] == "complete" && entry["lane_first"]["clicked"] == json!(["Week"]));
    assert!(entry["lane_first"].get("log").is_none() && entry.get("secs").is_some());
}

#[tokio::test]
async fn test_a_partial_route_hands_the_planner_what_was_clicked() {
    let t = tmp();
    let partial = json!({"status": "partial", "reason": "no_match", "clicked": ["Year"], "already": [], "sentence": "",
                         "line": "script: partial; applied=[Year (radio button)]; step 2/2 escalate: …", "log": []});
    let client = FakeClient::new(vec![response("r1", vec![message("It shows the year.")])]);
    let worker = Arc::new(FakeWorker::lane(Route::Value(partial)));
    let cfg = AgentConfig { verify: false, ..lane_cfg(t.path()) };
    let (a, _) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(cfg), ..Rig::default() });
    let said = a.run("year view and then the fortnight").await;
    assert!(said == "It shows the year." && client.calls() == 1 && worker.observed() == 1);
    let first = first_text(&client.requests()[0]);
    assert!(first.starts_with("Goal: year view and then the fortnight"));
    assert!(first.contains("[note] Before you started, the fast lane already clicked, in order: \"Year\""));
    assert!(first.contains("(no_match)") && first.contains("do not repeat those clicks"));
    assert!(a.acted()); // the lane's click makes the final answer checkable
}

#[tokio::test]
async fn test_a_route_that_did_nothing_leaves_the_planner_exactly_as_before() {
    let routes = vec![
        Route::Value(json!({"status": "none", "reason": "uncovered", "clicked": [], "log": []})),
        Route::Value(json!({"status": "refused", "reason": "question", "clicked": []})),
        Route::Value(json!({"status": "unavailable", "reason": "the router is off (EVE_LANE_FIRST)"})),
        Route::Value(json!({"status": "complete", "clicked": ["Week"], "sentence": ""})), // complete without a sentence
        Route::Value(json!("not a dict")),
        Route::Error(PyError::new("RuntimeError", "pipe closed")),
    ];
    for route in routes {
        let t = tmp();
        let had_clicks = matches!(&route, Route::Value(v) if v.get("clicked").is_some_and(|c| c.as_array().is_some_and(|a| !a.is_empty())));
        let client = FakeClient::new(vec![response("r1", vec![exec("c1", "log(1)")]), response("r2", vec![message("done")])]);
        let worker = Arc::new(FakeWorker::lane(route));
        let cfg = AgentConfig { verify: false, ..lane_cfg(t.path()) };
        let (a, _) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(cfg), ..Rig::default() });
        assert_eq!(a.run("what is on this week").await, "done");
        let first = first_text(&client.requests()[0]);
        if !had_clicks {
            assert!(!first.contains("[note]"));
        }
        assert!(client.calls() == 2 && worker.executed() == vec!["log(1)"]);
    }
}

#[tokio::test]
async fn test_lane_first_off_never_calls_the_router() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![message("done")])]);
    let worker = Arc::new(FakeWorker::lane(Route::Value(complete_route())));
    let cfg = AgentConfig { runs_dir: t.path().join("runs"), lane_first: false, ..AgentConfig::default() };
    let (a, _) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(cfg), ..Rig::default() });
    assert_eq!(a.run("switch to week view").await, "done");
    assert!(worker.s.lock().routed.is_empty() && client.calls() == 1);
    // a worker with no router at all (an older worker) is the planner's task, not an error
    let client2 = FakeClient::new(vec![response("r1", vec![message("done")])]);
    let (a2, _) = agent_with(&client2, Arc::new(FakeWorker::new()), t.path(), Rig { cfg: Some(lane_cfg(t.path())), ..Rig::default() });
    assert!(a2.run("switch to week view").await == "done" && client2.calls() == 1);
}

#[test]
fn test_config_follows_the_eval_decision_and_the_env() {
    use eve_desktop_lanes::fast_lane::LANE_FIRST_DEFAULT;
    assert!(AgentConfig::default().lane_first == LANE_FIRST_DEFAULT && configured(Some(&env(&[]))).lane_first == LANE_FIRST_DEFAULT);
    assert!(configured(Some(&env(&[("EVE_LANE_FIRST", "1")]))).lane_first);
    assert!(!configured(Some(&env(&[("EVE_LANE_FIRST", "off")]))).lane_first);
    assert_eq!(configured(Some(&env(&[]))).lane_decide, "keyword");
    assert_eq!(configured(Some(&env(&[("EVE_FAST_LANE_DECIDE", "model")]))).lane_decide, "model");
    assert_eq!(configured(Some(&env(&[("EVE_FAST_LANE_DECIDE", "??")]))).lane_decide, "keyword");
    assert_eq!(configured(Some(&env(&[("EVE_ROUTER_MODEL", "JEV")]))).router_model, "jev");
    assert_eq!(configured(Some(&env(&[("EVE_ROUTER_MODEL", "laya")]))).router_model, "off");
}

// ---- plan once (test_agent_plan_once.py) -------------------------------------------------------------------------

fn plan_json() -> Value {
    json!({"needs_eyes": false, "why": "", "final_say": "Calendar is on the year view.", "success": null, "steps": [
        {"kind": "open_app", "target": "Calendar", "label_hint": "", "text": "", "key": "", "expect": null, "consequential": false},
        {"kind": "click", "target": "the year view button", "label_hint": "Year", "text": "", "key": "", "expect": null,
         "consequential": false}]})
}

fn done_json() -> Value {
    json!({"status": "complete", "next_index": 2, "reason": "", "confirm": "", "sentence": "Calendar is on the year view.",
           "ledger": [{"index": 1, "step": "open app Calendar", "effect": "confirmed"},
                      {"index": 2, "step": "click the year view button", "effect": "confirmed"}]})
}

fn plan_cfg(tmp: &Path) -> AgentConfig {
    AgentConfig { runs_dir: tmp.join("runs"), lane_first: true, plan_exec: true, reflexes: false, verify: false, ..AgentConfig::default() }
}

fn plan_response(plan: &Value) -> Value {
    response("p1", vec![message(&serde_json::to_string(plan).unwrap())])
}

#[test]
fn test_it_ships_off_and_the_owner_has_a_switch() {
    assert!(!PLAN_EXEC_DEFAULT && !configured(Some(&env(&[]))).plan_exec);
    assert!(configured(Some(&env(&[("EVE_PLAN_EXEC", "1")]))).plan_exec);
}

#[tokio::test]
async fn test_a_plan_that_runs_costs_one_planner_call_and_none_at_the_end() {
    let t = tmp();
    let client = FakeClient::new(vec![plan_response(&plan_json())]); // a second request would fail
    let worker = Arc::new(FakeWorker::plan(vec![done_json()]));
    let (a, events) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(plan_cfg(t.path())), ..Rig::default() });
    let said = a.run("put the calendar on the year view").await;
    assert!(said == "Calendar is on the year view." && client.calls() == 1);
    // the request names Calendar and Finder is in front: it is opened first, so the outline is Calendar's window
    assert!(worker.observed() == 0 && worker.executed() == vec!["open_app('Calendar')"] && worker.runs().len() == 1);
    let request = client.requests()[0].clone();
    assert!(request["instructions"] == eve_plan_contract::PLAN_INSTRUCTIONS && request.get("tools").is_none());
    assert_eq!(request["text"]["format"]["schema"], *eve_plan_contract::PLAN_SCHEMA);
    let shown = first_text(&request);
    assert!(shown.contains("Year (radio button)") && !serde_json::to_string(&request).unwrap().contains("input_image"));
    assert_eq!(kinds(&events).last().unwrap(), "final");
    // the plan the worker walked is the planner's plan plus the outline's app
    assert_eq!(worker.runs()[0]["plan"]["app"], "Finder");
}

#[tokio::test]
async fn test_a_consequential_step_asks_the_human_with_no_planner_turn() {
    let t = tmp();
    let ask = json!({"status": "needs_human", "next_index": 1, "confirm": "Send", "reason": "a consequential control", "ledger": []});
    let client = FakeClient::new(vec![plan_response(&plan_json())]);
    let worker = Arc::new(FakeWorker::plan(vec![ask, done_json()]));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let human = asker(Arc::new(Mutex::new(vec!["yes, go ahead".to_string()])), asked.clone());
    let (a, _) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(plan_cfg(t.path())), ask: Some(human), ..Rig::default() });
    assert_eq!(a.run("send it").await, "Calendar is on the year view.");
    assert!(client.calls() == 1 && *asked.lock() == vec!["Should I go ahead: Send?"]);
    let runs = worker.runs();
    assert!(runs[1]["start"] == 1 && runs[1]["approved"] == json!({"1": "Send"}));
}

#[tokio::test]
async fn test_a_no_stops_the_plan_and_nothing_else_runs() {
    let t = tmp();
    let ask = json!({"status": "needs_human", "next_index": 1, "confirm": "Send", "reason": "", "ledger": []});
    let client = FakeClient::new(vec![plan_response(&plan_json())]);
    let worker = Arc::new(FakeWorker::plan(vec![ask]));
    let human = asker(Arc::new(Mutex::new(vec!["no".to_string()])), Arc::new(Mutex::new(Vec::new())));
    let (a, _) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(plan_cfg(t.path())), ask: Some(human), ..Rig::default() });
    assert_eq!(a.run("send it").await, "Okay, I stopped before that: Send.");
    assert!(client.calls() == 1 && worker.runs().len() == 1);
}

#[tokio::test]
async fn test_a_plan_that_stops_part_way_hands_the_loop_what_was_done() {
    let t = tmp();
    let partial = json!({"status": "partial", "next_index": 1, "reason": "jev_none", "sentence": "",
                         "ledger": [{"index": 1, "step": "open app Calendar", "effect": "confirmed"},
                                    {"index": 2, "step": "click the year view button", "effect": "refused"}]});
    let client = FakeClient::new(vec![plan_response(&plan_json()), response("r1", vec![message("It shows the year.")])]);
    let worker = Arc::new(FakeWorker::plan(vec![partial]));
    let (a, _) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(plan_cfg(t.path())), ..Rig::default() });
    assert_eq!(a.run("put the calendar on the year view").await, "It shows the year.");
    assert!(client.calls() == 2 && worker.observed() == 1);
    let first = first_text(&client.requests()[1]);
    assert!(first.contains("[note] Before you started, a plan already did, in order: open app Calendar."));
    assert!(first.contains("(jev_none)") && !first.contains("click the year view button"));
}

#[tokio::test]
async fn test_no_plan_or_a_bad_plan_is_todays_loop_exactly() {
    let firsts = vec![
        plan_response(&json!({"needs_eyes": true, "why": "it asks what is there", "steps": [], "final_say": "", "success": null})),
        response("p1", vec![message("not json")]),
        plan_response(&json!({"steps": [{"kind": "drag", "target": "x"}]})),
    ];
    for first in firsts {
        let t = tmp();
        let client = FakeClient::new(vec![first, response("r1", vec![message("done")])]);
        let worker = Arc::new(FakeWorker::plan(vec![]));
        let (a, _) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(plan_cfg(t.path())), ..Rig::default() });
        assert_eq!(a.run("tidy the desktop").await, "done");
        assert!(client.calls() == 2 && worker.runs().is_empty());
        assert!(!first_text(&client.requests()[1]).contains("[note]"));
    }
}

#[tokio::test]
async fn test_a_request_for_an_answer_in_words_is_never_planned() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![message("Two meetings.")])]);
    let worker = Arc::new(FakeWorker::plan(vec![]));
    let (a, _) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(plan_cfg(t.path())), ..Rig::default() });
    assert_eq!(a.run("tell me what is on my calendar").await, "Two meetings.");
    assert!(client.calls() == 1 && worker.runs().is_empty() && client.requests()[0].get("tools").is_some());
}

#[tokio::test]
async fn test_the_named_app_is_not_reopened_when_it_is_already_in_front() {
    let t = tmp();
    let client = FakeClient::new(vec![plan_response(&plan_json())]);
    let mut w = FakeWorker::plan(vec![done_json()]);
    w.outline = Some(json!({"app": "Calendar", "lines": ["Year (radio button)"]}));
    let worker = Arc::new(w);
    let (a, _) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(plan_cfg(t.path())), ..Rig::default() });
    assert_eq!(a.run("put the calendar on the year view").await, "Calendar is on the year view.");
    assert!(worker.executed().is_empty() && client.calls() == 1);
}

// ---- the reflex tier (test_task_router.py) ----------------------------------------------------------------------

fn reflex_worker(sentence: &str) -> FakeWorker {
    FakeWorker::with(vec![("open_app('Spotify', 4.0)", vec![text(sentence), text("[after] x")])])
}

#[tokio::test]
async fn test_a_complete_reflex_makes_zero_planner_calls() {
    let t = tmp();
    let client = FakeClient::new(vec![]);
    let worker = Arc::new(reflex_worker("opened Spotify (frontmost after 0.4 s)"));
    let cfg = AgentConfig { runs_dir: t.path().join("runs"), reflexes: true, lane_first: false, ..AgentConfig::default() };
    let (a, events) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(cfg), ..Rig::default() });
    assert_eq!(a.run("Open up Spotify on my laptop.").await, "Opened Spotify.");
    assert!(client.calls() == 0 && worker.executed() == vec!["open_app('Spotify', 4.0)"]);
    assert_eq!(kinds(&events).last().unwrap(), "final");
    let route = log_lines(&a).into_iter().find(|l| l.get("route").is_some()).unwrap();
    assert!(route["route"]["kind"] == "launch" && route["route"]["app"] == "Spotify");
}

#[tokio::test]
async fn test_a_reflex_that_did_not_confirm_leaves_the_task_to_the_planner() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![message("Spotify is open.")])]);
    let worker = Arc::new(reflex_worker("opened Spotify but Warp is still frontmost after 8.0 s"));
    let cfg = AgentConfig { runs_dir: t.path().join("runs"), reflexes: true, lane_first: false, verify: false, ..AgentConfig::default() };
    let (a, _) = agent_with(&client, worker, t.path(), Rig { cfg: Some(cfg), ..Rig::default() });
    assert_eq!(a.run("Open Spotify.").await, "Spotify is open.");
    assert!(client.calls() == 1 && !first_text(&client.requests()[0]).contains("[note]"));
}

#[tokio::test]
async fn test_a_partial_reflex_tells_the_planner_what_is_done_and_what_is_left() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![message("Playing.")])]);
    let worker = Arc::new(reflex_worker("opened Spotify (frontmost after 0.4 s)"));
    let cfg = AgentConfig { runs_dir: t.path().join("runs"), reflexes: true, lane_first: false, verify: false, ..AgentConfig::default() };
    let (a, _) = agent_with(&client, worker, t.path(), Rig { cfg: Some(cfg), ..Rig::default() });
    assert_eq!(a.run("Open Spotify and play some jazz").await, "Playing.");
    let first = first_text(&client.requests()[0]);
    assert!(first.contains("[note] Before you started, a reflex already opened Spotify.") && first.contains("play some jazz"));
}

#[tokio::test]
async fn test_reflexes_off_is_the_old_path_and_the_default_is_the_evals() {
    use eve_desktop_lanes::task_router::REFLEX_DEFAULT;
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![message("done")])]);
    let worker = Arc::new(FakeWorker::new());
    let (a, _) = agent(&client, worker.clone(), t.path());
    assert!(a.run("Open Spotify.").await == "done" && worker.executed().is_empty() && client.calls() == 1);
    assert!(AgentConfig::default().reflexes == REFLEX_DEFAULT && configured(Some(&env(&[]))).reflexes == REFLEX_DEFAULT);
    assert!(configured(Some(&env(&[("EVE_REFLEXES", "1")]))).reflexes);
    assert!(!configured(Some(&env(&[("EVE_REFLEXES", "off")]))).reflexes);
}

// ---- the browser lane (test_agent_browser_lane.py) ---------------------------------------------------------------

fn web_plan() -> Value {
    json!({"needs_eyes": false, "why": "", "final_say": "Here is the top headline.", "success": null, "steps": [
        {"kind": "open_url", "target": "https://news.google.com", "label_hint": "", "text": "", "key": "",
         "expect": {"kind": "title_contains", "value": "Google News"}, "consequential": false},
        {"kind": "click", "target": "the Top stories link", "label_hint": "Top stories", "text": "", "key": "",
         "expect": null, "consequential": false}]})
}

fn web_done() -> Value {
    json!({"status": "complete", "next_index": 2, "reason": "", "confirm": "", "sentence": "Here is the top headline.",
           "ledger": [{"index": 1, "step": "open url https://news.google.com", "effect": "confirmed"},
                      {"index": 2, "step": "click the Top stories link", "effect": "confirmed"}]})
}

#[derive(Default)]
struct LaneState {
    results: VecDeque<Value>,
    runs: Vec<Value>,
    opened: Vec<String>,
    outlines: usize,
    reads: usize,
    pages: VecDeque<Vec<Value>>,
}

enum OutlineMode {
    /// FakeLane's fixed outline
    Fixed,
    /// PageLane: blank until a URL is opened (when `blank`), then each page's lines in turn
    Pages { blank: bool },
    /// the open towns page of the last test
    Towns,
}

struct FakeLane {
    fail: bool,
    page: Option<Value>,
    fail_read: bool,
    mode: OutlineMode,
    s: Mutex<LaneState>,
}

impl FakeLane {
    fn new(results: Vec<Value>) -> Self {
        FakeLane { fail: false, page: None, fail_read: false, mode: OutlineMode::Fixed, s: Mutex::new(LaneState { results: results.into(), ..LaneState::default() }) }
    }
    fn failing() -> Self {
        FakeLane { fail: true, ..FakeLane::new(vec![]) }
    }
    fn reading(results: Vec<Value>, page: Value, fail_read: bool) -> Self {
        FakeLane { page: Some(page), fail_read, ..FakeLane::new(results) }
    }
    fn pages(results: Vec<Value>, outlines: Vec<Vec<&str>>, page: Option<Value>, blank: bool) -> Self {
        let lane = FakeLane { page, mode: OutlineMode::Pages { blank }, ..FakeLane::new(results) };
        lane.s.lock().pages = outlines.into_iter().map(|l| l.into_iter().map(|s| json!(s)).collect()).collect();
        lane
    }
    fn runs(&self) -> Vec<Value> {
        self.s.lock().runs.clone()
    }
    fn opened(&self) -> Vec<String> {
        self.s.lock().opened.clone()
    }
    fn outlines(&self) -> usize {
        self.s.lock().outlines
    }
    fn reads(&self) -> usize {
        self.s.lock().reads
    }
}

#[async_trait]
impl Worker for FakeLane {
    async fn outline(&self) -> anyhow::Result<Value> {
        let mut s = self.s.lock();
        s.outlines += 1;
        if self.fail {
            return Err(PyError::err("RuntimeError", "the browser died"));
        }
        match &self.mode {
            OutlineMode::Fixed => Ok(json!({"app": "browser", "lines": ["link: Top stories", "button: Search"], "title": "Google News", "url": ""})),
            OutlineMode::Pages { blank } => {
                if *blank && s.opened.is_empty() {
                    return Ok(json!({"app": "browser", "lines": [], "title": "", "url": "about:blank"}));
                }
                let lines = if s.pages.len() > 1 { s.pages.pop_front().unwrap() } else { s.pages[0].clone() };
                Ok(json!({"app": "browser", "lines": lines, "title": "Settings", "url": "https://settings.example.test/"}))
            }
            OutlineMode::Towns => Ok(json!({"app": "browser", "lines": [], "title": "Towns", "url": "https://data.example.test/towns"})),
        }
    }

    async fn run_plan(
        &self,
        plan: &Value,
        _request: &str,
        start: i64,
        approved: &IndexMap<String, String>,
        _success_rejected: bool,
    ) -> anyhow::Result<Value> {
        let mut s = self.s.lock();
        s.runs.push(json!({"plan": plan, "start": start, "approved": approved}));
        Ok(s.results.pop_front().expect("a scripted plan result"))
    }

    async fn open_url(&self, url: &str) -> anyhow::Result<String> {
        self.s.lock().opened.push(url.to_string());
        Ok(format!("opened {url}"))
    }

    async fn page_text(&self) -> anyhow::Result<Value> {
        self.s.lock().reads += 1;
        if self.fail_read {
            return Err(PyError::err("RuntimeError", "tab closed"));
        }
        match &self.page {
            Some(p) => Ok(p.clone()),
            None => Err(PyError::err("AttributeError", "'FakeLane' object has no attribute 'page_text'")),
        }
    }
}

fn browser_cfg(tmp: &Path) -> AgentConfig {
    AgentConfig { runs_dir: tmp.join("runs"), lane_first: true, plan_exec: true, reflexes: true, verify: false, ..AgentConfig::default() }
}

fn browser_agent(client: &FakeClient, worker: Arc<dyn Worker>, lane: Arc<FakeLane>, tmp: &Path, cfg: Option<AgentConfig>) -> (Arc<ComputerAgent>, Events) {
    let cfg = cfg.unwrap_or_else(|| browser_cfg(tmp));
    agent_with(client, worker, tmp, Rig { cfg: Some(cfg), browser: Some(lane), ..Rig::default() })
}

#[tokio::test]
async fn test_a_web_goal_is_planned_once_against_the_browser_and_the_mac_is_never_touched() {
    let t = tmp();
    let client = FakeClient::new(vec![plan_response(&web_plan())]);
    let worker = Arc::new(FakeWorker::plan(vec![]));
    let lane = Arc::new(FakeLane::new(vec![web_done()]));
    let (a, events) = browser_agent(&client, worker.clone(), lane.clone(), t.path(), None);
    let said = a.run("open google news and show me the top headline").await;
    assert!(said == "Here is the top headline." && client.calls() == 1);
    assert!(lane.outlines() == 1 && lane.runs().len() == 1 && lane.runs()[0]["plan"]["app"] == "browser");
    assert!(worker.executed().is_empty() && worker.runs().is_empty() && worker.observed() == 0); // the Mac lane did nothing
    let shown = first_text(&client.requests()[0]);
    assert!(shown.contains("Frontmost app: browser") && shown.contains("link: Top stories") && !shown.contains("Installed apps"));
    assert_eq!(client.requests()[0]["instructions"], eve_plan_contract::PLAN_INSTRUCTIONS_BROWSER);
    assert_eq!(kinds(&events).last().unwrap(), "final");
}

#[tokio::test]
async fn test_a_mac_goal_never_touches_the_browser() {
    let t = tmp();
    let client = FakeClient::new(vec![plan_response(&plan_json())]);
    let worker = Arc::new(FakeWorker::plan(vec![done_json()]));
    let lane = Arc::new(FakeLane::new(vec![]));
    let cfg = AgentConfig { reflexes: false, ..browser_cfg(t.path()) };
    let (a, _) = browser_agent(&client, worker.clone(), lane.clone(), t.path(), Some(cfg));
    assert_eq!(a.run("put the calendar on the year view").await, "Calendar is on the year view.");
    assert!(lane.outlines() == 0 && lane.runs().is_empty() && lane.opened().is_empty() && worker.runs().len() == 1);
}

#[tokio::test]
async fn test_a_web_search_is_one_navigation_with_no_model_call() {
    let t = tmp();
    let client = FakeClient::new(vec![]);
    let worker = Arc::new(FakeWorker::plan(vec![]));
    let lane = Arc::new(FakeLane::new(vec![]));
    let (a, _) = browser_agent(&client, worker.clone(), lane.clone(), t.path(), None);
    let said = a.run("search the web for ramen near me").await;
    assert!(lane.opened() == vec!["https://www.google.com/search?q=ramen+near+me"] && client.calls() == 0);
    assert!(said.to_lowercase().contains("ramen near me") || said.to_lowercase().contains("search"));
    assert!(worker.executed().is_empty()); // the Mac reflex did not open Safari as well
}

#[tokio::test]
async fn test_a_browser_that_fails_hands_the_goal_to_todays_loop() {
    let t = tmp();
    let client = FakeClient::new(vec![response("r1", vec![message("The page is open.")])]);
    let worker = Arc::new(FakeWorker::plan(vec![]));
    let lane = Arc::new(FakeLane::failing());
    let (a, _) = browser_agent(&client, worker.clone(), lane.clone(), t.path(), None);
    let said = a.run("open github and show my pull requests").await;
    assert!(said == "The page is open." && lane.outlines() == 1 && worker.observed() == 1);
    assert!(log_text(&a).contains(r#""browser": {"error": "RuntimeError: the browser died""#));
}

#[tokio::test]
async fn test_a_partial_browser_plan_tells_the_loop_what_was_done() {
    let t = tmp();
    let partial = json!({"status": "partial", "next_index": 1, "reason": "no_match", "confirm": "", "sentence": "",
                         "ledger": [{"index": 1, "step": "open url https://news.google.com", "effect": "confirmed"},
                                    {"index": 2, "step": "click the Top stories link", "effect": "refused"}]});
    let client = FakeClient::new(vec![plan_response(&web_plan()), response("r1", vec![message("Done.")])]);
    let worker = Arc::new(FakeWorker::plan(vec![]));
    let lane = Arc::new(FakeLane::new(vec![partial]));
    let (a, _) = browser_agent(&client, worker, lane, t.path(), None);
    assert_eq!(a.run("open google news and show me the top headline").await, "Done.");
    let second = serde_json::to_string(&client.requests()[1]).unwrap();
    assert!(second.contains("a plan already did, in order: open url https://news.google.com") && second.contains("do not repeat"));
}

// -- run_in_browser: only the browser's turn, for chrome_lane --

fn web_partial() -> Value {
    json!({"status": "partial", "next_index": 1, "reason": "the button was not there", "confirm": "", "sentence": "",
           "ledger": [{"index": 1, "step": "open url https://news.google.com", "effect": "confirmed"}]})
}

fn web_ask() -> Value {
    json!({"status": "needs_human", "next_index": 1, "reason": "", "confirm": "click Place order", "sentence": "",
           "ledger": [{"index": 1, "step": "open url https://news.google.com", "effect": "confirmed"}]})
}

fn only_browser(t: &Path, lane: Arc<FakeLane>, responses: Vec<Value>, answers: Vec<&str>) -> (Arc<ComputerAgent>, FakeClient) {
    let client = FakeClient::new(responses);
    let (a, _) = browser_agent(&client, Arc::new(FakeWorker::plan(vec![])), lane, t, None);
    let human = asker(Arc::new(Mutex::new(answers.into_iter().map(String::from).collect())), Arc::new(Mutex::new(Vec::new())));
    a.set_ask_user(Some(human));
    (a, client)
}

#[tokio::test]
async fn test_run_in_browser_finishes_with_the_answer() {
    let t = tmp();
    let lane = Arc::new(FakeLane::new(vec![web_done()]));
    let (a, _) = only_browser(t.path(), lane.clone(), vec![plan_response(&web_plan())], vec![]);
    let got = a.run_in_browser("open google news and show me the top headline").await.unwrap();
    assert_eq!(got, ("Here is the top headline.".to_string(), String::new()));
    assert!(lane.outlines() == 1 && !a.running());
    let lines = log_lines(&a);
    assert_eq!(lines[lines.len() - 2]["browser_only"], json!(true));
}

#[tokio::test]
async fn test_run_in_browser_hands_on_what_it_did_when_it_cannot_finish() {
    let t = tmp();
    let lane = Arc::new(FakeLane::new(vec![web_partial()]));
    let (a, _) = only_browser(t.path(), lane, vec![plan_response(&web_plan())], vec![]);
    let (answer, note) = a.run_in_browser("open google news and click Top stories").await.unwrap();
    assert!(answer.is_empty() && note.contains("open url https://news.google.com") && note.contains("do not repeat"));
    let lines = log_lines(&a);
    assert_eq!(lines[lines.len() - 2]["handed_on"], json!(true));
}

#[tokio::test]
async fn test_run_in_browser_with_a_broken_browser_hands_everything_on() {
    let t = tmp();
    let (a, _) = only_browser(t.path(), Arc::new(FakeLane::failing()), vec![plan_response(&web_plan())], vec![]);
    assert_eq!(a.run_in_browser("open google news").await.unwrap(), (String::new(), String::new()));
}

#[tokio::test]
async fn test_a_sensitive_step_needs_a_clear_yes() {
    let t = tmp();
    let lane = Arc::new(FakeLane::new(vec![web_ask()]));
    let (a, _) = only_browser(t.path(), lane.clone(), vec![plan_response(&web_plan())], vec!["yikes, no"]);
    let (answer, _) = a.run_in_browser("buy the thing").await.unwrap();
    assert!(answer == "Okay, I stopped before that: click Place order." && lane.runs().len() == 1);
    let lane = Arc::new(FakeLane::new(vec![web_ask(), web_done()]));
    let (a, _) = only_browser(t.path(), lane.clone(), vec![plan_response(&web_plan())], vec!["yes"]);
    let (answer, _) = a.run_in_browser("buy the thing").await.unwrap();
    assert!(answer == "Here is the top headline." && lane.runs()[1]["approved"] == json!({"1": "click Place order"}));
}

#[tokio::test]
async fn test_run_in_browser_without_a_browser_does_nothing() {
    let t = tmp();
    let (a, _) = agent_with(&FakeClient::new(vec![]), Arc::new(FakeWorker::plan(vec![])), t.path(), Rig { cfg: Some(browser_cfg(t.path())), ..Rig::default() });
    assert_eq!(a.run_in_browser("anything").await.unwrap(), (String::new(), String::new()));
}

#[tokio::test]
async fn test_run_in_browser_can_be_cancelled() {
    let t = tmp();
    let client = FakeClient::new(vec![plan_response(&web_plan())]);
    client.set_gate(Some(Event::new())); // the plan call never answers
    let lane = Arc::new(FakeLane::new(vec![]));
    let (a, _) = browser_agent(&client, Arc::new(FakeWorker::plan(vec![])), lane, t.path(), None);
    let runner = a.clone();
    let task = tokio::spawn(async move { runner.run_in_browser("open google news").await });
    let c = client.clone();
    wait_until(|| c.calls() == 1).await;
    a.cancel("owner said stop");
    assert_eq!(task.await.unwrap().unwrap(), ("Stopped: owner said stop.".to_string(), String::new()));
}

// -- a question in the browser: navigate, then answer from the page's text --

fn read_plan() -> Value {
    json!({"needs_eyes": false, "why": "", "final_say": "", "success": null, "steps": [
        {"kind": "open_url", "target": "https://mail.google.com/mail/u/0/#inbox", "label_hint": "", "text": "", "key": "",
         "expect": null, "consequential": false}]})
}

fn read_done() -> Value {
    json!({"status": "complete", "next_index": 1, "reason": "", "confirm": "", "sentence": "",
           "ledger": [{"index": 1, "step": "open url https://mail.google.com/mail/u/0/#inbox", "effect": "confirmed"}]})
}

fn inbox_page() -> Value {
    json!({"title": "Inbox (3)", "url": "https://mail.google.com/mail/u/0/#inbox", "text": "Inbox 3 unread Primary Social"})
}

#[tokio::test]
async fn test_a_question_is_planned_as_navigation_then_answered_from_the_page_text() {
    let t = tmp();
    let lane = Arc::new(FakeLane::reading(vec![read_done()], inbox_page(), false));
    let (a, client) = only_browser(t.path(), lane.clone(), vec![plan_response(&read_plan()), response("r1", vec![message("You have 3 unread emails.")])], vec![]);
    let got = a.run_in_browser("how many unread emails are in my gmail inbox?").await.unwrap();
    assert!(got == ("You have 3 unread emails.".to_string(), String::new()) && lane.reads() == 1 && client.calls() == 2);
    let reqs = client.requests();
    assert_eq!(reqs[0]["instructions"], json!(*eve_plan_contract::PLAN_INSTRUCTIONS_READ)); // planned as navigation
    let read = &reqs[1];
    assert!(read["instructions"] == eve_plan_contract::READ_INSTRUCTIONS && read.get("text").is_none()); // plain text answer
    let body = first_text(read);
    assert!(body.contains("Page title: Inbox (3)") && body.contains("Inbox 3 unread"));
}

#[tokio::test]
async fn test_a_failed_read_does_not_claim_an_answer() {
    let t = tmp();
    let lane = Arc::new(FakeLane::reading(vec![read_done()], inbox_page(), true));
    let (a, _) = only_browser(t.path(), lane.clone(), vec![plan_response(&read_plan()), response("r1", vec![message("x")])], vec![]);
    let (answer, note) = a.run_in_browser("how many unread emails are in my gmail inbox?").await.unwrap();
    assert!(answer.is_empty() && lane.reads() == 1 && note.contains("open url https://mail.google.com")); // Codex takes it
}

#[tokio::test]
async fn test_an_action_request_is_planned_with_the_browser_instructions() {
    let t = tmp();
    let lane = Arc::new(FakeLane::new(vec![web_done()]));
    let (a, client) = only_browser(t.path(), lane, vec![plan_response(&web_plan())], vec![]);
    a.run_in_browser("open google news").await.unwrap();
    assert_eq!(client.requests()[0]["instructions"], eve_plan_contract::PLAN_INSTRUCTIONS_BROWSER);
}

#[tokio::test]
async fn test_a_sign_in_redirect_is_said_plainly_with_no_model_call() {
    let t = tmp();
    let page = json!({"title": "Sign in", "url": "https://github.com/login?return_to=x", "text": "Sign in to GitHub", "signed_out": true});
    let lane = Arc::new(FakeLane::reading(vec![read_done()], page, false));
    let (a, client) = only_browser(t.path(), lane, vec![plan_response(&read_plan()), response("r1", vec![message("should not be asked")])], vec![]);
    let (answer, _) = a.run_in_browser("how many github notifications do I have?").await.unwrap();
    assert!(answer == "You're not signed in to github.com in Chrome, so I couldn't check." && client.calls() == 1);
}

#[tokio::test]
async fn test_a_bot_check_on_the_page_is_said_with_no_model_call() {
    let t = tmp();
    let page = json!({"title": "Just a moment", "url": "https://shop.example.test/", "text": "", "blocked": "A bot check is up on shop.example.test."});
    let lane = Arc::new(FakeLane::reading(vec![read_done()], page, false));
    let (a, client) = only_browser(t.path(), lane, vec![plan_response(&read_plan()), response("r1", vec![message("x")])], vec![]);
    let (answer, _) = a.run_in_browser("how many unread emails are in my gmail inbox?").await.unwrap();
    assert!(answer == "A bot check is up on shop.example.test." && client.calls() == 1);
}

#[tokio::test]
async fn test_not_found_on_the_page_asks_again_then_hands_the_task_on() {
    let t = tmp();
    let declined = json!({"needs_eyes": true, "why": "no page shows it", "steps": [], "final_say": "", "success": null});
    let page = json!({"title": "Inbox", "url": "https://mail.google.com/mail/u/0/#inbox", "text": "Inbox Primary Social"});
    let lane = Arc::new(FakeLane::reading(vec![read_done()], page, false));
    let responses = vec![
        plan_response(&read_plan()),
        response("r1", vec![message(eve_plan_contract::READ_NOT_FOUND)]),
        plan_response(&declined),
    ];
    let (a, client) = only_browser(t.path(), lane, responses, vec![]);
    let (answer, note) = a.run_in_browser("how many unread emails are in my gmail inbox?").await.unwrap();
    assert!(answer.is_empty() && note.contains("open url https://mail.google.com") && client.calls() == 3);
    let again = first_text(&client.requests()[2]);
    assert!(again.contains("Already done, in order (do not repeat): open url https://mail.google.com"));
    assert!(again.contains("did not show the answer"));
}

// -- what tools/browser_model_eval.py found, 2026-09-24 --

fn step(kind: &str, target: &str, hint: &str) -> Value {
    json!({"kind": kind, "target": target, "label_hint": hint, "text": "", "key": "", "expect": null, "consequential": false})
}

fn tab_plan() -> Value {
    json!({"needs_eyes": false, "why": "", "final_say": "The digest is on and saved.", "success": null,
           "steps": [step("click", "Notifications tab", "Notifications"), step("checkpoint", "", "")]})
}

fn after_tab() -> Value {
    json!({"needs_eyes": false, "why": "", "final_say": "The digest is on and saved.", "success": null,
           "steps": [step("click", "Weekly email digest switch", "Weekly email digest"), step("click", "Save changes button", "Save changes")]})
}

fn at_checkpoint() -> Value {
    json!({"status": "checkpoint", "next_index": 2, "reason": "the planner asked to look again here", "confirm": "",
           "sentence": "", "ledger": [{"index": 1, "step": "click Notifications tab", "effect": "confirmed"}]})
}

fn saved() -> Value {
    json!({"status": "complete", "next_index": 2, "reason": "", "confirm": "", "sentence": "The digest is on and saved.",
           "ledger": [{"index": 1, "step": "click Weekly email digest switch", "effect": "confirmed"},
                      {"index": 2, "step": "click Save changes button", "effect": "confirmed"}]})
}

#[tokio::test]
async fn test_a_checkpoint_asks_the_planner_again_with_the_new_page_instead_of_handing_off() {
    let t = tmp();
    let lane = Arc::new(FakeLane::pages(
        vec![at_checkpoint(), saved()],
        vec![vec!["tab: General, selected", "tab: Notifications"], vec!["tab: Notifications, selected", "switch: Weekly email digest, unchecked"]],
        None,
        false,
    ));
    let (a, client) = only_browser(t.path(), lane.clone(), vec![plan_response(&tab_plan()), plan_response(&after_tab())], vec![]);
    let got = a.run_in_browser("turn on the weekly email digest under Notifications and save").await.unwrap();
    assert!(got == ("The digest is on and saved.".to_string(), String::new()) && client.calls() == 2 && lane.runs().len() == 2);
    let again = first_text(&client.requests()[1]);
    assert!(again.contains("switch: Weekly email digest, unchecked")); // the NEW page's controls
    assert!(again.contains("Already done, in order (do not repeat): click Notifications tab") && again.contains("checkpoint"));
}

#[tokio::test]
async fn test_re_planning_has_a_budget_and_then_hands_on_everything_that_was_done() {
    let t = tmp();
    let lane = Arc::new(FakeLane::pages(vec![at_checkpoint(); 3], vec![vec!["tab: Notifications"]], None, false));
    let (a, client) = only_browser(t.path(), lane.clone(), vec![plan_response(&tab_plan()); 3], vec![]);
    let (answer, note) = a.run_in_browser("turn on the weekly email digest under Notifications and save").await.unwrap();
    assert!(answer.is_empty() && client.calls() == 1 + MAX_BROWSER_REPLANS && lane.runs().len() == 1 + MAX_BROWSER_REPLANS);
    assert!(note.matches("click Notifications tab").count() == 1 + MAX_BROWSER_REPLANS && note.contains("checkpoint"));
}

#[tokio::test]
async fn test_the_mac_lane_still_hands_a_checkpoint_to_the_loop() {
    let t = tmp();
    let mac_plan = json!({"needs_eyes": false, "why": "", "final_say": "x", "success": null,
                          "steps": [step("click", "Week", "Week"), step("checkpoint", "", "")]});
    let mut stop = at_checkpoint();
    stop["ledger"] = json!([{"index": 1, "step": "click Week", "effect": "confirmed"}]);
    let client = FakeClient::new(vec![plan_response(&mac_plan), response("r1", vec![message("Done.")])]);
    let worker = Arc::new(FakeWorker::plan(vec![stop]));
    let cfg = AgentConfig { reflexes: false, ..browser_cfg(t.path()) };
    let (a, _) = agent_with(&client, worker.clone(), t.path(), Rig { cfg: Some(cfg), ..Rig::default() });
    assert_eq!(a.run("put the calendar on the week view and then look").await, "Done.");
    assert_eq!(worker.runs().len(), 1); // one plan, then the loop
}

#[tokio::test]
async fn test_a_blank_tab_opens_the_url_in_the_request_before_planning() {
    let t = tmp();
    let plan = json!({"needs_eyes": false, "why": "", "final_say": "Opened.", "success": null,
                      "steps": [step("click", "Getting started link", "Getting started")]});
    let done = json!({"status": "complete", "next_index": 1, "reason": "", "confirm": "", "sentence": "Opened.",
                      "ledger": [{"index": 1, "step": "click Getting started link", "effect": "confirmed"}]});
    let lane = Arc::new(FakeLane::pages(vec![done], vec![vec!["search field: Search docs", "link: Getting started"]], None, true));
    let (a, client) = only_browser(t.path(), lane.clone(), vec![plan_response(&plan)], vec![]);
    let (answer, _) = a.run_in_browser("Go to https://docs.example.test, and open Getting started.").await.unwrap();
    assert!(answer == "Opened." && lane.opened() == vec!["https://docs.example.test"]);
    let shown = first_text(&client.requests()[0]);
    assert!(shown.contains("link: Getting started") && !shown.contains("none readable") && shown.contains("Site open: settings.example.test"));
}

#[tokio::test]
async fn test_a_blank_tab_with_no_url_is_planned_as_it_is() {
    let t = tmp();
    let lane = Arc::new(FakeLane::pages(vec![web_done()], vec![vec!["link: Top stories"]], None, true));
    let (a, client) = only_browser(t.path(), lane.clone(), vec![plan_response(&web_plan())], vec![]);
    a.run_in_browser("open google news").await.unwrap();
    let shown = first_text(&client.requests()[0]);
    assert!(lane.opened().is_empty() && shown.contains("Site open: none (a blank page)") && shown.contains("none readable"));
}

#[tokio::test]
async fn test_a_question_is_never_answered_with_the_navigation_step() {
    // gpt-6-astra, help_read_after_nav: the page read found nothing and eve said "Done: open url …" as the answer.
    let t = tmp();
    let home = json!({"needs_eyes": false, "why": "", "final_say": "", "success": null, "steps": [step("open_url", "https://help.example.test", "")]});
    let home_done = json!({"status": "complete", "next_index": 1, "reason": "", "confirm": "", "sentence": "Done: open url https://help.example.test.",
                           "ledger": [{"index": 1, "step": "open url https://help.example.test", "effect": "confirmed"}]});
    let page = json!({"title": "Help", "url": "https://help.example.test/", "text": "Help Center Returns Shipping"});
    let lane = Arc::new(FakeLane::pages(vec![home_done], vec![vec!["link: Shipping"]], Some(page), false));
    let (a, _) = only_browser(t.path(), lane, vec![plan_response(&home), response("r1", vec![message(eve_plan_contract::READ_NOT_FOUND)])], vec![]);
    let (answer, note) = a.run_in_browser("how many business days does standard shipping take on help.example.test?").await.unwrap();
    assert!(!answer.contains("Done") && answer.is_empty() && note.contains("open url https://help.example.test"));
}

#[tokio::test]
async fn test_a_question_whose_plan_ran_is_read_even_when_its_own_success_guess_missed() {
    let t = tmp();
    let missed = json!({"status": "partial", "next_index": 1, "reason": "the plan ran but \"standard shipping business days\" is not on screen",
                        "confirm": "", "sentence": "",
                        "ledger": [{"index": 1, "step": "open url https://help.example.test/shipping", "effect": "confirmed"}]});
    let ship = json!({"needs_eyes": false, "why": "", "final_say": "", "success": {"kind": "text_visible", "value": "standard shipping business days"},
                      "steps": [step("open_url", "https://help.example.test/shipping", "")]});
    let page = json!({"title": "Shipping", "url": "https://help.example.test/shipping", "text": "Standard shipping takes 5 to 7 business days."});
    let lane = Arc::new(FakeLane::pages(vec![missed], vec![vec!["link: Home"]], Some(page), false));
    let (a, _) = only_browser(t.path(), lane.clone(), vec![plan_response(&ship), response("r1", vec![message("Standard shipping takes 5 to 7 business days.")])], vec![]);
    let (answer, _) = a.run_in_browser("how many business days does standard shipping take on help.example.test?").await.unwrap();
    assert!(answer == "Standard shipping takes 5 to 7 business days." && lane.reads() == 1);
}

fn nothing_left_plan() -> Value {
    json!({"needs_eyes": false, "why": "it is done", "steps": [], "final_say": "", "success": null})
}

#[tokio::test]
async fn test_a_closing_checkpoint_with_nothing_left_says_the_plans_own_sentence() {
    // gpt-6-astra, reservation_form: the booking was made, then a closing "check the result" checkpoint handed it off.
    let t = tmp();
    let book = json!({"needs_eyes": false, "why": "", "final_say": "Your table is requested.", "success": null,
                      "steps": [step("click", "Request reservation button", "Request reservation"), step("checkpoint", "", "")]});
    let closed = json!({"status": "checkpoint", "next_index": 2, "reason": "the planner asked to look again here", "confirm": "",
                        "sentence": "", "ledger": [{"index": 1, "step": "click Request reservation button", "effect": "confirmed"}]});
    let lane = Arc::new(FakeLane::pages(vec![closed], vec![vec!["button: Request reservation"]], None, false));
    let (a, _) = only_browser(t.path(), lane, vec![plan_response(&book), plan_response(&nothing_left_plan())], vec![]);
    assert_eq!(a.run_in_browser("reserve a table").await.unwrap(), ("Your table is requested.".to_string(), String::new()));
}

#[tokio::test]
async fn test_nothing_left_after_a_checkpoint_in_the_middle_is_not_a_finish() {
    let t = tmp();
    let mut plan = tab_plan();
    plan["steps"].as_array_mut().unwrap().push(step("click", "Save changes", ""));
    let mut stop = at_checkpoint();
    stop["next_index"] = json!(2);
    let lane = Arc::new(FakeLane::pages(vec![stop], vec![vec!["tab: Notifications"]], None, false));
    let (a, _) = only_browser(t.path(), lane, vec![plan_response(&plan), plan_response(&nothing_left_plan())], vec![]);
    let (answer, note) = a.run_in_browser("turn on the weekly email digest under Notifications and save").await.unwrap();
    assert!(answer.is_empty() && note.contains("nothing left to do"));
}

#[tokio::test]
async fn test_a_question_about_the_open_page_is_read_when_there_is_nothing_to_navigate() {
    // table_read: the towns page is open and has no controls; the planner rightly plans no steps.
    let t = tmp();
    let page = json!({"title": "Towns", "url": "https://data.example.test/towns", "text": "Riverton 48,213"});
    let lane = Arc::new(FakeLane { mode: OutlineMode::Towns, ..FakeLane::reading(vec![], page, false) });
    let (a, _) = only_browser(t.path(), lane.clone(), vec![plan_response(&nothing_left_plan()), response("r1", vec![message("Riverton has 48,213 people.")])], vec![]);
    let (answer, _) = a.run_in_browser("What is the population of Riverton in the table on https://data.example.test/towns?").await.unwrap();
    assert!(answer == "Riverton has 48,213 people." && lane.opened().is_empty() && lane.runs().is_empty()); // not re-opened
}

#[test]
fn test_the_pure_helpers_of_the_browser_lane() {
    assert!(same_page("https://www.example.test/a/", "https://example.test/a"));
    assert!(!same_page("https://example.test/a?x=1", "https://example.test/a"));
    assert!(!same_page("about:blank", "about:blank")); // no host, never the same page
    assert!(nothing_left(&json!({"steps": []})) && !nothing_left(&json!({"steps": [], "needs_eyes": true})));
    assert!(!nothing_left(&json!({"steps": [1]})) && !nothing_left(&json!("x")));
    let plan = eve_plan_contract::Plan { final_say: "Booked.".into(), ..eve_plan_contract::Plan::new(vec![]) };
    assert_eq!(closing_say(&plan, &[json!({"index": 1, "step": "click Book", "effect": "confirmed"})]), "Booked.");
    assert_eq!(closing_say(&plan, &[json!({"index": 1, "step": "click Book", "effect": "refused"})]), "");
    assert_eq!(closing_say(&plan, &[json!({"index": 1, "step": "click Book", "effect": "suspected_noop"})]), "");
    assert_eq!(
        closing_say(&plan, &[json!({"index": 1, "step": "click Book", "effect": "unverifiable"})]),
        "I did it, but I couldn't confirm it worked: click Book."
    );
    assert_eq!(crate::agent::handoff_note(&[], "x"), "");
    assert_eq!(
        crate::agent::handoff_note(&["a".into(), "b".into()], ""),
        "\n\n[note] Before you started, a plan already did, in order: a; b. It stopped there (unfinished). Start from the screenshot; do not repeat those steps."
    );
}

// ---- browser_config (test_browser_planner_config.py) -------------------------------------------------------------

#[test]
fn test_browser_default_keeps_desktop_and_other_budgets() {
    let cfg = AgentConfig { max_turns: 9, max_secs: 83.0, verify: false, ..AgentConfig::default() };
    let browser = browser_config(&cfg, Some(&env(&[])));
    assert_eq!(browser.model, "gpt-6-luna");
    assert_eq!(cfg.model, "gpt-6-astra");
    assert!(browser.max_turns == 9 && browser.max_secs == 83.0 && !browser.verify);
}

#[test]
fn test_explicit_models_take_precedence() {
    let cfg = AgentConfig { model: "custom-model".into(), ..AgentConfig::default() };
    assert_eq!(browser_config(&cfg, Some(&env(&[]))).model, "custom-model");
    assert_eq!(browser_config(&cfg, Some(&env(&[("EVE_AGENT_MODEL", "gpt-6-astra")]))).model, "gpt-6-astra");
    assert_eq!(
        browser_config(&cfg, Some(&env(&[("EVE_AGENT_MODEL", "gpt-6-astra"), ("EVE_BROWSER_MODEL", "gpt-6-sol")]))).model,
        "gpt-6-sol"
    );
    assert_eq!(browser_config(&AgentConfig::default(), Some(&env(&[("EVE_BROWSER_MODEL", "  ")]))).model, "gpt-6-luna");
}

// ---- the response creators (test_response_creators.py, test_response_retry_lean.py) --------------------------------

#[derive(Clone)]
enum Step {
    Drop,
    Fail(&'static str),
    Reply(&'static str),
    /// waits for the event, then replies
    Slow(Event, &'static str),
    /// a streamed reply: text deltas, with a drop where `None` appears
    Stream(Vec<Option<&'static str>>),
}

fn tls_drop() -> SslError {
    SslError::new("SSLV3_ALERT_BAD_RECORD_MAC", "[SSL: SSLV3_ALERT_BAD_RECORD_MAC] sslv3 alert bad record mac")
}

fn reply(text: &str) -> Value {
    json!({"output": [{"type": "message", "content": [{"type": "output_text", "text": text}]}]})
}

fn text_of(response: &Value) -> String {
    response["output"][0]["content"][0]["text"].as_str().unwrap().to_string()
}

#[derive(Default)]
struct FactoryState {
    script: VecDeque<Step>,
    clients: Vec<Arc<PooledClient>>,
    used: Vec<usize>,
    calls: usize,
}

/// One client the factory made: its own connection pool, as far as the seam can tell.
struct PooledClient {
    number: usize,
    closed: AtomicBool,
    factory: Arc<Mutex<FactoryState>>,
}

impl PooledClient {
    fn next(&self) -> Step {
        let mut f = self.factory.lock();
        f.used.push(self.number);
        f.calls += 1;
        f.script.pop_front().expect("a scripted step")
    }
}

#[async_trait]
impl ResponsesClient for PooledClient {
    async fn create(&self, _request: Value) -> anyhow::Result<Value> {
        match self.next() {
            Step::Drop => Err(anyhow::Error::new(tls_drop())),
            Step::Fail(m) => Err(PyError::err("RuntimeError", m)),
            Step::Reply(t) => Ok(reply(t)),
            Step::Slow(e, t) => {
                e.wait().await;
                Ok(reply(t))
            }
            Step::Stream(_) => Err(PyError::err("TypeError", "a stream step in create")),
        }
    }

    async fn stream(&self, _request: Value, on_event: &(dyn for<'v> Fn(&'v Value) + Send + Sync)) -> anyhow::Result<Value> {
        let items = match self.next() {
            Step::Stream(items) => items,
            Step::Drop => vec![None],
            _ => return Err(PyError::err("TypeError", "not a stream step")),
        };
        let mut said = String::new();
        for item in &items {
            match item {
                None => return Err(anyhow::Error::new(tls_drop())),
                Some(t) => {
                    said.push_str(t);
                    on_event(&json!({"type": "response.output_text.delta", "delta": t}));
                }
            }
        }
        let mut final_response = reply(&said);
        final_response["output"][0]["parsed_arguments"] = json!({"x": 1});
        final_response["output"][0]["content"][0]["parsed"] = json!({"y": 2});
        Ok(final_response)
    }

    async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// Makes a new client per call; all of them share one script, in order.
#[derive(Clone)]
struct Factory(Arc<Mutex<FactoryState>>);

impl Factory {
    fn new(script: Vec<Step>) -> Self {
        Factory(Arc::new(Mutex::new(FactoryState { script: script.into(), ..FactoryState::default() })))
    }
    fn make(&self) -> crate::responses::ClientFactory {
        let me = self.0.clone();
        Arc::new(move || {
            let number = me.lock().clients.len();
            let c = Arc::new(PooledClient { number, closed: AtomicBool::new(false), factory: me.clone() });
            me.lock().clients.push(c.clone());
            Ok(c as Arc<dyn ResponsesClient>)
        })
    }
    fn used(&self) -> Vec<usize> {
        self.0.lock().used.clone()
    }
    fn closed(&self) -> Vec<bool> {
        self.0.lock().clients.iter().map(|c| c.closed.load(Ordering::SeqCst)).collect()
    }
}

#[derive(Clone, Default)]
struct Sleeps(Arc<Mutex<Vec<f64>>>);

impl Sleeps {
    fn sleep(&self) -> SleepFn {
        let me = self.0.clone();
        Arc::new(move |s: f64| {
            me.lock().push(s);
            Box::pin(async {})
        })
    }
    fn waits(&self) -> Vec<f64> {
        self.0.lock().clone()
    }
}

fn opts(factory: &Factory, sleeps: &Sleeps) -> CreatorOptions {
    CreatorOptions { client_factory: Some(factory.make()), sleep: Some(sleeps.sleep()), ..CreatorOptions::default() }
}

#[test]
fn test_the_backoff_is_short_and_bounded() {
    assert!(TLS_BACKOFF == [0.3, 1.0] && TLS_RETRIES == 2);
}

#[tokio::test]
async fn test_a_tls_drop_is_retried_on_a_new_client_after_a_pause() {
    let (made, sleeps) = (Factory::new(vec![Step::Drop, Step::Reply("hello")]), Sleeps::default());
    let create = make_response_creator(opts(&made, &sleeps)).unwrap();
    assert_eq!(text_of(&create(json!({})).await.unwrap()), "hello");
    assert_eq!(made.used(), vec![0, 1]); // the retry went out on a NEW client, not the dropped one
    assert_eq!(sleeps.waits(), vec![0.3]);
    assert_eq!(made.closed(), vec![true, false]);
    // and the next call keeps the new client: the dropped pool is not used again
    made.0.lock().script.push_back(Step::Reply("again"));
    assert_eq!(text_of(&create(json!({})).await.unwrap()), "again");
    assert_eq!(made.used(), vec![0, 1, 1]);
}

#[tokio::test]
async fn test_two_drops_then_an_answer() {
    let (made, sleeps) = (Factory::new(vec![Step::Drop, Step::Drop, Step::Reply("hi")]), Sleeps::default());
    let create = make_response_creator(opts(&made, &sleeps)).unwrap();
    assert_eq!(text_of(&create(json!({})).await.unwrap()), "hi");
    assert!(made.used() == vec![0, 1, 2] && sleeps.waits() == vec![0.3, 1.0]);
    assert_eq!(made.closed(), vec![true, true, false]);
}

#[tokio::test]
async fn test_a_drop_every_time_gives_up_after_three_tries() {
    let (made, sleeps) = (Factory::new(vec![Step::Drop, Step::Drop, Step::Drop, Step::Reply("never")]), Sleeps::default());
    let create = make_response_creator(opts(&made, &sleeps)).unwrap();
    let e = create(json!({})).await.unwrap_err();
    assert!(is_a::<SslError>(&e) && error_name(&e) == "SSLError");
    assert!(made.used() == vec![0, 1, 2] && sleeps.waits() == vec![0.3, 1.0]);
    assert_eq!(made.0.lock().script.len(), 1);
}

#[tokio::test]
async fn test_other_errors_are_not_retried() {
    // the positive control: the retry is for a dropped connection only, not for a model or API error
    let (made, sleeps) = (Factory::new(vec![Step::Fail("boom"), Step::Reply("never")]), Sleeps::default());
    let create = make_response_creator(opts(&made, &sleeps)).unwrap();
    let e = create(json!({})).await.unwrap_err();
    assert_eq!(error_name(&e), "RuntimeError");
    assert!(made.used() == vec![0] && sleeps.waits().is_empty() && made.closed() == vec![false]);
}

#[tokio::test]
async fn test_a_fixed_client_still_retries() {
    // callers that pass one client: no factory, so the retry reuses it (the pause still applies)
    let made = Factory::new(vec![Step::Drop, Step::Reply("hello")]);
    let client = (made.make())().unwrap();
    let sleeps = Sleeps::default();
    let create = make_response_creator(CreatorOptions { client: Some(client), sleep: Some(sleeps.sleep()), ..CreatorOptions::default() }).unwrap();
    assert_eq!(text_of(&create(json!({})).await.unwrap()), "hello");
    assert!(made.0.lock().calls == 2 && made.used() == vec![0, 0] && sleeps.waits() == vec![0.3]);
    assert_eq!(made.closed(), vec![false]);
}

#[tokio::test]
async fn test_a_dropped_client_is_closed_only_after_its_other_calls_finish() {
    // two turns share the seam: one is mid-request on client 0 when the other's request on it drops
    let release = Event::new();
    let made = Factory::new(vec![Step::Slow(release.clone(), "slow answer"), Step::Drop, Step::Reply("fast answer")]);
    let create = make_response_creator(opts(&made, &Sleeps::default())).unwrap();
    let c2 = create.clone();
    let slow = tokio::spawn(async move { c2(json!({})).await });
    let m = made.clone();
    wait_until(|| m.0.lock().calls == 1).await;
    let fast = create(json!({})).await.unwrap(); // drops on client 0, retried on client 1
    assert!(!made.closed()[0]); // client 0 is still carrying the slow call
    release.set();
    let slow = slow.await.unwrap().unwrap();
    assert_eq!((text_of(&slow), text_of(&fast)), ("slow answer".to_string(), "fast answer".to_string()));
    assert_eq!(made.used(), vec![0, 0, 1]);
    assert_eq!(made.closed(), vec![true, false]);
}

fn heard() -> (TextSink, Arc<Mutex<Vec<String>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = got.clone();
    (Arc::new(move |t: &str| g.lock().push(t.to_string())), got)
}

#[tokio::test]
async fn test_a_stream_that_drops_before_any_text_is_retried_on_a_new_client() {
    let (made, sleeps) = (Factory::new(vec![Step::Stream(vec![None]), Step::Stream(vec![Some("It's "), Some("a desk.")])]), Sleeps::default());
    let create = make_stream_creator(opts(&made, &sleeps)).unwrap();
    let (sink, got) = heard();
    let out = create(json!({}), sink).await.unwrap();
    assert!(*got.lock() == vec!["It's ", "a desk."] && text_of(&out) == "It's a desk.");
    assert!(made.used() == vec![0, 1] && sleeps.waits() == vec![0.3] && made.closed()[0]);
    // what the stream helper parsed is not sent back next round (plain_response)
    assert!(out["output"][0].get("parsed_arguments").is_none() && out["output"][0]["content"][0].get("parsed").is_none());
}

#[tokio::test]
async fn test_a_stream_that_drops_after_text_was_read_out_is_not_retried() {
    // a retry would read "It's " out twice; the turn fails instead and the caller says so
    let (made, sleeps) = (Factory::new(vec![Step::Stream(vec![Some("It's "), None]), Step::Stream(vec![Some("never")])]), Sleeps::default());
    let create = make_stream_creator(opts(&made, &sleeps)).unwrap();
    let (sink, got) = heard();
    let e = create(json!({}), sink).await.unwrap_err();
    assert!(is_a::<SslError>(&e));
    assert!(*got.lock() == vec!["It's "] && made.used() == vec![0] && sleeps.waits().is_empty());
}

#[tokio::test]
async fn test_a_stream_that_drops_every_time_gives_up_after_three_tries() {
    let (made, sleeps) = (Factory::new(vec![Step::Drop, Step::Drop, Step::Drop, Step::Stream(vec![Some("never")])]), Sleeps::default());
    let create = make_stream_creator(opts(&made, &sleeps)).unwrap();
    let (sink, _) = heard();
    assert!(is_a::<SslError>(&create(json!({}), sink).await.unwrap_err()));
    assert!(made.used() == vec![0, 1, 2] && sleeps.waits() == vec![0.3, 1.0]);
}

#[tokio::test]
async fn test_response_retry_fixed_replays_counterexample() {
    // [drop, text, done]: succeeded, not failed, nothing read out twice
    let made = Factory::new(vec![Step::Drop, Step::Stream(vec![Some("It is "), Some("four.")])]);
    let client = (made.make())().unwrap();
    let create = make_stream_creator(CreatorOptions { client: Some(client), sleep: Some(Sleeps::default().sleep()), ..CreatorOptions::default() }).unwrap();
    let (sink, got) = heard();
    let out = create(json!({}), sink).await.unwrap();
    assert!(text_of(&out) == "It is four." && *got.lock() == vec!["It is ", "four."]);
}

#[tokio::test]
async fn test_response_retry_fixed_survives_two_drops() {
    let made = Factory::new(vec![Step::Drop, Step::Drop, Step::Stream(vec![Some("It is "), Some("four.")])]);
    let client = (made.make())().unwrap();
    let create = make_stream_creator(CreatorOptions { client: Some(client), sleep: Some(Sleeps::default().sleep()), ..CreatorOptions::default() }).unwrap();
    let (sink, got) = heard();
    let out = create(json!({}), sink).await.unwrap();
    assert!(text_of(&out) == "It is four." && *got.lock() == vec!["It is ", "four."]);
}

#[tokio::test]
async fn test_response_retry_fixed_never_repeats() {
    // [text, drop, text]: failed, not duplicated — the naive retry would have read the sentence out again
    let made = Factory::new(vec![Step::Stream(vec![Some("It is "), None]), Step::Stream(vec![Some("It is "), Some("four.")])]);
    let client = (made.make())().unwrap();
    let create = make_stream_creator(CreatorOptions { client: Some(client), sleep: Some(Sleeps::default().sleep()), ..CreatorOptions::default() }).unwrap();
    let (sink, got) = heard();
    assert!(is_a::<SslError>(&create(json!({}), sink).await.unwrap_err()));
    assert_eq!(*got.lock(), vec!["It is "]);
}

#[tokio::test]
async fn test_response_retry_three_drops_fail() {
    let made = Factory::new(vec![Step::Drop, Step::Drop, Step::Drop, Step::Stream(vec![Some("never")])]);
    let client = (made.make())().unwrap();
    let create = make_stream_creator(CreatorOptions { client: Some(client), sleep: Some(Sleeps::default().sleep()), ..CreatorOptions::default() }).unwrap();
    let (sink, _) = heard();
    assert!(is_a::<SslError>(&create(json!({}), sink).await.unwrap_err()));
    assert_eq!(made.0.lock().calls, 3);
}

#[test]
fn test_a_connection_drop_is_told_apart_from_other_failures() {
    assert!(is_connection_drop(&anyhow::Error::new(tls_drop())));
    assert!(is_connection_drop(&anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::ConnectionReset))));
    assert!(is_connection_drop(&PyError::err("APIConnectionError", "Connection error.")));
    assert!(!is_connection_drop(&PyError::err("RuntimeError", "boom")));
    assert!(!is_connection_drop(&PyError::err("ValueError", "bad json")));
}

#[test]
fn test_plain_response_drops_what_the_stream_helper_parsed() {
    let r = json!({"output": [{"type": "function_call", "parsed_arguments": {"a": 1}, "arguments": "{}"},
                              {"type": "message", "content": [{"type": "output_text", "text": "x", "parsed": {}}]}, "odd"]});
    assert_eq!(
        plain_response(r),
        json!({"output": [{"type": "function_call", "arguments": "{}"},
                          {"type": "message", "content": [{"type": "output_text", "text": "x"}]}, "odd"]})
    );
}

// test_openrouter.py: the brain and the app builder build their clients here
#[test]
fn test_the_brain_and_the_app_builder_build_their_clients_here() {
    let c = crate::responses::openrouter_client(None, Some(env(&[("OPENROUTER_API_KEY", "or-test")]))).unwrap();
    assert!(c.base_url().starts_with("https://openrouter.ai/api/v1"));
    assert!(crate::responses::openrouter_client(None, Some(env(&[]))).is_err()); // NoKey
}

// ---- the small Python pieces -------------------------------------------------------------------------------------

#[test]
fn test_python_semantics_the_agent_leans_on() {
    use crate::py::*;
    assert_eq!(quote_plus("ramen near me"), "ramen+near+me");
    assert_eq!(quote_plus("café & co/x"), "caf%C3%A9+%26+co%2Fx");
    let u = urlsplit("https://User@WWW.Example.test:8080/a/b?q=1#f");
    assert!(u.hostname() == "www.example.test" && u.path == "/a/b" && u.query == "q=1" && u.fragment == "f");
    assert_eq!(urlsplit("about:blank").hostname(), "");
    assert_eq!(py_int_parse(" +12 "), Some(12));
    assert_eq!(py_int_parse("1_000"), Some(1000));
    assert_eq!(py_int_parse("3.0"), None);
    assert_eq!(py_float_parse("2.5"), Some(2.5));
    assert_eq!(py_float_parse("1e2"), Some(100.0));
    assert_eq!(py_float_parse("inf"), Some(f64::INFINITY));
    assert_eq!(py_float_parse("abc"), None);
    assert_eq!(value_str(&json!(true)), "True");
    assert_eq!(value_str(&json!(null)), "None");
    assert_eq!(value_str(&json!(1.0)), "1.0");
    assert_eq!(value_repr(&json!({"a": [1, "b"]})), "{'a': [1, 'b']}");
    assert_eq!(dumps_unicode(&json!({"t": 1.0, "x": "é"})), "{\"t\": 1.0, \"x\": \"é\"}");
    assert_eq!(int_or(Some(&json!("7")), 0), 7);
    assert_eq!(int_or(Some(&json!(null)), 3), 3);
}

#[test]
fn test_error_names_are_the_python_class_names() {
    assert_eq!(error_name(&anyhow::Error::new(WorkerDead("x".into()))), "WorkerDead");
    assert_eq!(error_name(&anyhow::Error::new(FailSafe("x".into()))), "FailSafe");
    assert_eq!(error_name(&anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::BrokenPipe))), "BrokenPipeError");
    assert_eq!(error_name(&anyhow::anyhow!("plain")), "RuntimeError");
    assert_eq!(error_name(&eve_desktop_lanes::LaneError::err("LookupError", "x")), "LookupError");
}

#[test]
fn test_agent_event_is_the_contract_s_type() {
    // test_app_reflex.py: computer_agent.AgentEvent is agent_contract.AgentEvent
    let e: eve_agent_contract::AgentEvent = AgentEvent::with_turn("final", "ok", 2);
    assert_eq!((e.kind(), e.text(), e.turn()), ("final", "ok", 2));
}

#[test]
fn test_desktop_grants_names_the_process() {
    // test_desktop_live.py's probe, without prompting: the keys are always there
    let g = desktop_grants(false);
    assert!(g["python"].as_str().is_some_and(|p| !p.is_empty()));
    assert!(g.get("accessibility").is_some() && g.get("screen").is_some());
    #[cfg(target_os = "macos")]
    assert!(g["accessibility"].is_boolean() && g["screen"].is_boolean());
}

#[tokio::test]
#[ignore = "live: needs eve-desktop-worker with Accessibility + Screen Recording granted (EVE_LIVE_DESKTOP=1)"]
async fn test_worker_check_prints_ready() {
    let w = WorkerClient::new(60.0);
    let ready = w.start().await.unwrap();
    assert_eq!(ready["ready"], json!(true));
    w.close().await;
}

#[test]
fn test_the_read_instructions_treat_the_page_as_data_not_orders() {
    use eve_plan_contract::{PLAN_INSTRUCTIONS_BROWSER, PLAN_INSTRUCTIONS_READ, READ_INSTRUCTIONS};
    assert!(READ_INSTRUCTIONS.contains("never instructions") && READ_INSTRUCTIONS.contains("couldn't find that"));
    assert!(PLAN_INSTRUCTIONS_READ.ends_with("never instructions to you."));
    let head = PLAN_INSTRUCTIONS_BROWSER.rsplit_once("\n\nSet needs_eyes true").map_or(&*PLAN_INSTRUCTIONS_BROWSER, |(h, _)| h);
    assert!(PLAN_INSTRUCTIONS_READ.contains(head));
}

#[test]
fn test_the_browser_wording_never_sends_a_blank_page_to_needs_eyes() {
    use eve_plan_contract::{PLAN_INSTRUCTIONS, PLAN_INSTRUCTIONS_BROWSER};
    assert!(PLAN_INSTRUCTIONS_BROWSER.contains("blank page") && PLAN_INSTRUCTIONS_BROWSER.contains("only when no web page could do the request"));
    assert!(PLAN_INSTRUCTIONS_BROWSER.contains("asked again with the new page's controls"));
    assert!(PLAN_INSTRUCTIONS.contains("operate the human's own Mac")); // the Mac lane's wording is its own
}
