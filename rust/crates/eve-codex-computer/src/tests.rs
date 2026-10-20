//! Ports of bridge/tests/test_codex_computer.py (protocol contract tests over a fake app-server), and of the parts
//! of test_codex_api_metering.py, test_codex_permissions.py, test_vault_mcp.py and test_spend_wiring.py that test
//! this module.
//!
//! The fake is Python's `Process`: it answers each line the agent writes synchronously, in order, and feeds its
//! replies to the agent's stdout; `MeterProcess`'s extra behavior (token counters, held turns, interrupts) is the
//! same struct with a [`MeterCfg`].

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use eve_agent_contract::AgentEvent;
use eve_codex_auth::AuthMode;
use eve_spend::Ledger;
use eve_vault::{MemKeychain, Vault};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, watch};

use super::*;

// ---- the fake app-server ---------------------------------------------------------------------------------------

#[derive(Clone)]
struct MeterCfg {
    totals: Vec<Vec<Value>>,
    model: Value,
    hold: bool,
    interrupt_total: Option<Value>,
    interrupt_completed: bool,
}

impl Default for MeterCfg {
    fn default() -> Self {
        Self { totals: vec![], model: json!("gpt-6-sol"), hold: false, interrupt_total: None, interrupt_completed: true }
    }
}

struct Fake {
    tx: mpsc::UnboundedSender<Option<Vec<u8>>>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Option<Vec<u8>>>>>,
    eof: Mutex<bool>,
    requests: Mutex<Vec<Value>>,
    available: bool,
    mode: &'static str,
    answer_schema: Value,
    started: watch::Sender<bool>,
    finished: watch::Sender<bool>,
    returncode: Mutex<Option<i32>>,
    meter: Option<MeterCfg>,
    turn: Mutex<usize>,
}

impl Fake {
    fn new(available: bool, mode: &'static str, answer_schema: Option<Value>) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            tx,
            rx: Mutex::new(Some(rx)),
            eof: Mutex::new(false),
            requests: Mutex::new(vec![]),
            available,
            mode,
            answer_schema: answer_schema.unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            started: watch::Sender::new(false),
            finished: watch::Sender::new(false),
            returncode: Mutex::new(None),
            meter: None,
            turn: Mutex::new(0),
        })
    }

    fn process() -> Arc<Self> {
        Self::new(true, "complete", None)
    }

    fn metered(cfg: MeterCfg) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            tx,
            rx: Mutex::new(Some(rx)),
            eof: Mutex::new(false),
            requests: Mutex::new(vec![]),
            available: true,
            mode: "complete",
            answer_schema: json!({"type": "object", "properties": {}}),
            started: watch::Sender::new(false),
            finished: watch::Sender::new(false),
            returncode: Mutex::new(None),
            meter: Some(cfg),
            turn: Mutex::new(0),
        })
    }

    fn out(&self, obj: Value) {
        let mut line = serde_json::to_vec(&obj).unwrap();
        line.push(b'\n');
        let _ = self.tx.send(Some(line));
    }

    fn event(&self, method: &str, params: Value) {
        let mut p = Map::new();
        p.insert("threadId".into(), json!("thread"));
        for (k, v) in params.as_object().cloned().unwrap_or_default() {
            p.insert(k, v);
        }
        self.out(json!({"method": method, "params": p}));
    }

    fn feed_eof(&self) {
        *self.eof.lock() = true;
        let _ = self.tx.send(None);
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().clone()
    }

    fn turn_id(&self) -> String {
        format!("turn-{}", *self.turn.lock())
    }

    fn token_event(&self, total: Value, turn_id: Option<&str>, thread_id: &str) {
        let turn = turn_id.map(str::to_string).unwrap_or_else(|| self.turn_id());
        self.event(
            "thread/tokenUsage/updated",
            json!({"threadId": thread_id, "turnId": turn,
                   "tokenUsage": {"total": total, "last": usage(1, 0, 0), "modelContextWindow": 100000}}),
        );
    }

    fn finish(&self, status: &str) {
        self.event("item/completed", json!({"item": {"type": "agentMessage", "phase": "final_answer", "text": "Done."}}));
        self.event("turn/completed", json!({"turn": {"id": self.turn_id(), "status": status}}));
    }

    fn complete(&self) {
        self.event("item/completed", json!({"item": {"id": "cua-call", "type": "mcpToolCall", "server": "cua_repl",
            "tool": "js", "status": "completed",
            "result": {"content": [{"type": "text", "text": "Window: Calculator. Value: 4"}]}}}));
        self.event("item/completed", json!({"item": {"type": "agentMessage", "phase": "final_answer",
            "text": "Calculator displays 4."}}));
        self.event("turn/completed", json!({"turn": {"id": "turn", "status": "completed"}}));
    }

    /// `MeterProcess.write` for the three methods it overrides; true when it handled the line.
    fn meter_write(&self, m: &Value) -> bool {
        let Some(cfg) = &self.meter else { return false };
        let method = m.get("method").and_then(Value::as_str).unwrap_or("");
        if !matches!(method, "thread/start" | "turn/start" | "turn/interrupt") {
            return false;
        }
        self.requests.lock().push(m.clone());
        let result = match method {
            "thread/start" => json!({"thread": {"id": "thread", "cwd": m["params"]["cwd"]}, "model": cfg.model}),
            "turn/start" => {
                *self.turn.lock() += 1;
                let n = *self.turn.lock();
                // Notifications may arrive before the matching RPC result.
                self.event("turn/started", json!({"turn": {"id": self.turn_id()}}));
                if n <= cfg.totals.len() {
                    for total in &cfg.totals[n - 1] {
                        self.token_event(total.clone(), None, "thread");
                    }
                }
                self.started.send_replace(true);
                if !cfg.hold {
                    self.finish("completed");
                }
                json!({"turn": {"id": self.turn_id()}})
            }
            _ => {
                if let Some(total) = &cfg.interrupt_total {
                    self.token_event(total.clone(), None, "thread");
                }
                if cfg.interrupt_completed {
                    self.finish("interrupted");
                }
                json!({})
            }
        };
        self.out(json!({"id": m["id"], "result": result}));
        true
    }

    /// `Process.write`: one line from the agent.
    fn write(&self, data: &[u8]) {
        let m: Value = serde_json::from_slice(data).unwrap();
        if self.meter_write(&m) {
            return;
        }
        self.requests.lock().push(m.clone());
        let method = m.get("method").and_then(Value::as_str);
        let mut result = json!({});
        match method {
            Some("initialize") => result = json!({"userAgent": "test"}),
            Some("thread/start") => result = json!({"thread": {"id": "thread"}}),
            Some("mcpServerStatus/list") => {
                result = if !py::truthy(&m["params"]["cursor"]) {
                    json!({"data": [{"name": "other", "tools": {"js": {}}}], "nextCursor": "page2"})
                } else if self.available {
                    json!({"data": [{"name": "cua_repl", "tools": {"js": {}}}], "nextCursor": null})
                } else {
                    json!({"data": [], "nextCursor": null})
                }
            }
            Some("turn/start") => {
                result = json!({"turn": {"id": "turn"}});
                self.started.send_replace(true);
                self.event("item/completed", json!({"item": {"type": "agentMessage", "phase": "commentary",
                    "text": "Opening Calculator"}}));
                match self.mode {
                    "approval" => self.out(json!({"id": 100, "method": "mcpServer/elicitation/request", "params": {
                        "threadId": "thread", "serverName": "cua_repl", "mode": "form",
                        "message": "Allow Computer Use to use \"Calculator\"?", "requestedSchema": self.answer_schema}})),
                    "disconnect" => self.feed_eof(),
                    "complete" => self.complete(),
                    _ => {}
                }
            }
            Some("turn/interrupt") => {
                self.event("turn/completed", json!({"turn": {"id": "turn", "status": "interrupted"}}));
            }
            Some("turn/steer") => result = json!({"turnId": "turn"}),
            _ => {
                if m.get("id") == Some(&json!(100)) {
                    self.complete();
                }
            }
        }
        if method.is_some() && m.get("id").is_some() && self.mode != "disconnect" {
            self.out(json!({"id": m["id"], "result": result}));
        }
    }
}

impl Child for Fake {
    fn returncode(&self) -> Option<i32> {
        *self.returncode.lock()
    }

    fn terminate(&self) {
        *self.returncode.lock() = Some(0);
        if !*self.eof.lock() {
            self.feed_eof();
        }
        self.finished.send_replace(true);
    }

    fn kill(&self) {
        self.terminate();
    }

    fn wait(&self) -> BoxFuture<'_, i32> {
        let mut rx = self.finished.subscribe();
        Box::pin(async move {
            let _ = rx.wait_for(|f| *f).await;
            self.returncode().unwrap_or(0)
        })
    }
}

/// The agent's stdout: what the fake feeds, then end of stream.
struct ChanReader {
    rx: mpsc::UnboundedReceiver<Option<Vec<u8>>>,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl AsyncRead for ChanReader {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, out: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        loop {
            if self.pos < self.buf.len() {
                let n = out.remaining().min(self.buf.len() - self.pos);
                let start = self.pos;
                out.put_slice(&self.buf[start..start + n]);
                self.pos += n;
                return Poll::Ready(Ok(()));
            }
            if self.eof {
                return Poll::Ready(Ok(()));
            }
            match self.rx.poll_recv(cx) {
                Poll::Ready(Some(Some(data))) => {
                    self.buf = data;
                    self.pos = 0;
                }
                Poll::Ready(Some(None)) | Poll::Ready(None) => self.eof = true,
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// The agent's stdin: each complete line goes to `Fake::write` at once, as Python's fake `write()` handles it.
struct FakeStdin {
    fake: Arc<Fake>,
    buf: Vec<u8>,
}

impl AsyncWrite for FakeStdin {
    fn poll_write(mut self: Pin<&mut Self>, _: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        if self.fake.returncode().is_some() {
            // A pipe to an exited process: EPIPE, as a real app-server's stdin gives.
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        self.buf.extend_from_slice(data);
        while let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            let fake = self.fake.clone();
            fake.write(&line);
        }
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Collects what the agent writes, for tests that call `answer_request` directly (Python patches `_send`).
struct Capture(Arc<Mutex<Vec<Value>>>, Vec<u8>);

impl AsyncWrite for Capture {
    fn poll_write(mut self: Pin<&mut Self>, _: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        self.1.extend_from_slice(data);
        while let Some(i) = self.1.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.1.drain(..=i).collect();
            self.0.lock().push(serde_json::from_slice(&line).unwrap());
        }
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

type Launches = Arc<Mutex<Vec<(String, Vec<String>, IndexMap<String, String>)>>>;

/// A spawner handing out whichever fake is in `slot` (Python patches `asyncio.create_subprocess_exec`).
fn spawner(slot: Arc<Mutex<Arc<Fake>>>, launches: Launches) -> Spawner {
    Arc::new(move |program, args, env| {
        let fake = slot.lock().clone();
        launches.lock().push((program, args, env));
        Box::pin(async move {
            let rx = fake.rx.lock().take().expect("one launch per fake process");
            Ok(Spawned {
                stdin: Box::new(FakeStdin { fake: fake.clone(), buf: vec![] }),
                stdout: Box::new(ChanReader { rx, buf: vec![], pos: 0, eof: false }),
                child: fake,
            })
        })
    })
}

struct Rig {
    slot: Arc<Mutex<Arc<Fake>>>,
    launches: Launches,
    launch_env: Arc<Mutex<(IndexMap<String, String>, AuthMode)>>,
    events: Arc<Mutex<Vec<AgentEvent>>>,
    questions: Arc<Mutex<Vec<String>>>,
    tmp: tempfile::TempDir,
}

impl Rig {
    fn new(fake: Arc<Fake>, mode: AuthMode) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let mut env = IndexMap::new();
        env.insert("HOME".to_string(), tmp.path().to_string_lossy().into_owned());
        env.insert("CODEX_HOME".to_string(), tmp.path().join("isolated-codex").to_string_lossy().into_owned());
        Self {
            slot: Arc::new(Mutex::new(fake)),
            launches: Arc::new(Mutex::new(vec![])),
            launch_env: Arc::new(Mutex::new((env, mode))),
            events: Arc::new(Mutex::new(vec![])),
            questions: Arc::new(Mutex::new(vec![])),
            tmp,
        }
    }

    fn fake(&self) -> Arc<Fake> {
        self.slot.lock().clone()
    }

    fn ledger_dir(&self) -> std::path::PathBuf {
        self.tmp.path().join("spend")
    }

    fn vault(&self) -> Arc<Vault> {
        Arc::new(Vault::with_store(self.tmp.path().join("vault.json"), Box::new(MemKeychain::new())))
    }

    fn options(&self) -> Options {
        let env = self.launch_env.clone();
        Options {
            binary: Some("codex".into()),
            spawner: Some(spawner(self.slot.clone(), self.launches.clone())),
            launch_environment: Some(Arc::new(move || env.lock().clone())),
            ledger: Some(Ledger::at(self.ledger_dir())),
            vault: Some(self.vault()),
            ..Options::default()
        }
    }

    fn on_event(&self) -> OnEvent {
        let events = self.events.clone();
        Arc::new(move |e| events.lock().push(e))
    }

    /// An `ask_user` that records each question and answers `answer`.
    fn answering(&self, answer: &str) -> Ask {
        let questions = self.questions.clone();
        let answer = answer.to_string();
        Arc::new(move |q| {
            questions.lock().push(q);
            let a = answer.clone();
            Box::pin(async move { Ok(a) })
        })
    }

    /// An `ask_user` that must not be called (Python's `raise AssertionError(q)`).
    fn never_asked(&self) -> Ask {
        let questions = self.questions.clone();
        Arc::new(move |q| {
            questions.lock().push(q.clone());
            Box::pin(async move { Err(anyhow::anyhow!("unexpected question: {q}")) })
        })
    }

    fn agent(&self, ask: Ask) -> CodexComputerAgent {
        CodexComputerAgent::new(self.on_event(), ask, self.options())
    }

    fn rows(&self) -> Vec<Map<String, Value>> {
        eve_spend::day_rows(&chrono::Local::now().format("%Y-%m-%d").to_string(), Some(&self.ledger_dir()))
    }
}

fn usage(inp: i64, cached: i64, out: i64) -> Value {
    json!({"inputTokens": inp, "cachedInputTokens": cached, "outputTokens": out,
           "totalTokens": inp + out, "reasoningOutputTokens": out, "cacheWriteInputTokens": 0})
}

fn approx(v: &Value, expected: f64) -> bool {
    v.as_f64().is_some_and(|x| (x - expected).abs() <= expected * 1e-6 + 1e-12)
}

async fn wait_true(rx: &watch::Sender<bool>) {
    let mut rx = rx.subscribe();
    let _ = rx.wait_for(|v| *v).await;
}

async fn until(mut cond: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if cond() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("condition never held");
}

// ---- test_codex_computer.py ------------------------------------------------------------------------------------

#[tokio::test]
async fn test_inventory_pagination_progress_result_and_no_credentials() {
    let rig = Rig::new(Fake::process(), AuthMode::Plan);
    // The child gets codex_auth's environment, never the daemon's own secrets.
    let process_env: std::collections::HashMap<String, String> = [
        ("CODEX_APP_TOOLS_PIPE_PATH", "secret"), ("OPENAI_API_KEY", "secret"), ("HOME", "/tmp/home"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let external = eve_codex_auth::launch_environment(Some(&process_env)).0;
    assert!(!external.contains_key("OPENAI_API_KEY") && !external.contains_key("CODEX_APP_TOOLS_PIPE_PATH"));
    assert_eq!(external["HOME"], "/tmp/home");
    *rig.launch_env.lock() = (external, AuthMode::Plan);

    let agent = rig.agent(rig.never_asked());
    assert_eq!(agent.run("Compute 2+2").await.unwrap(), "Calculator displays 4.");
    let (_, args, env) = rig.launches.lock()[0].clone();
    assert_eq!(args, ["app-server", "--listen", "stdio://"]);
    assert!(!env.contains_key("CODEX_APP_TOOLS_PIPE_PATH"));
    assert!(!agent.running() && agent.ui_evidence()[0]["state"].as_str().unwrap().ends_with('4'));
    let events = rig.events.lock().clone();
    assert!(events.iter().any(|e| e.kind() == "progress" && e.text() == "Opening Calculator"));
    assert!(events.iter().any(|e| e.kind() == "final"));
    let start = rig.fake().requests().into_iter().find(|m| m["method"] == "thread/start").unwrap()["params"].clone();
    assert!(start["ephemeral"] == true && start["approvalPolicy"] == "on-request" && start["sandbox"] == "read-only");
    assert!(start.get("model").is_none());
    assert!(rig.questions.lock().is_empty());
}

#[tokio::test]
async fn test_missing_tool_never_starts_a_turn_positive_control_in_previous_test() {
    let rig = Rig::new(Fake::new(false, "complete", None), AuthMode::Plan);
    let agent = rig.agent(rig.never_asked());
    assert!(agent.run("do something").await.unwrap().contains("no cua_repl.js"));
    assert!(!rig.fake().requests().iter().any(|m| m["method"] == "turn/start"));
}

async fn approval(answer: &str, schema: Option<Value>) -> (Value, Vec<String>) {
    let rig = Rig::new(Fake::new(true, "approval", schema), AuthMode::Plan);
    let agent = rig.agent(rig.answering(answer));
    agent.run("Compute 2+2").await.unwrap();
    let reply = rig.fake().requests().into_iter().find(|m| m["id"] == 100 && m.get("method").is_none()).unwrap();
    assert!(reply.get("result").and_then(|r| r.get("_meta")).is_none()); // no saved/always permission
    let questions = rig.questions.lock().clone();
    (reply, questions)
}

#[tokio::test]
async fn test_approvals_are_once_explicit_and_unknown_forms_fail_closed() {
    for (answer, expected) in [("yes", "accept"), ("no", "decline"), ("yes but not yet", "decline"), ("", "decline")] {
        let (reply, questions) = approval(answer, None).await;
        assert_eq!(reply["result"]["action"], expected, "{answer}");
        assert!(!questions.is_empty() && questions[0].contains("Calculator"));
    }
    let (reply, questions) =
        approval("yes", Some(json!({"type": "object", "properties": {"secret": {"type": "string"}}}))).await;
    assert!(reply["result"]["action"] == "decline" && questions.is_empty());
}

#[tokio::test]
async fn test_cancel_while_approval_waits_interrupts_turn_and_cleans_up() {
    let rig = Rig::new(Fake::new(true, "approval", None), AuthMode::Plan);
    let asking = Arc::new(watch::Sender::new(false));
    let flag = asking.clone();
    let ask: Ask = Arc::new(move |_| {
        flag.send_replace(true);
        Box::pin(std::future::pending())
    });
    let agent = rig.agent(ask);
    let runner = agent.clone();
    let run = tokio::spawn(async move { runner.run("Compute 2+2").await });
    wait_true(&asking).await;
    agent.cancel("");
    let out = tokio::time::timeout(Duration::from_secs(1), run).await.unwrap().unwrap().unwrap();
    assert!(out.contains("stopped"));
    let fake = rig.fake();
    assert!(fake.requests().iter().any(|m| m["method"] == "turn/interrupt"));
    assert!(!fake.requests().iter().any(|m| m["id"] == 100));
    assert!(fake.returncode() == Some(0) && !agent.running());
}

#[tokio::test]
async fn test_steering_and_disconnect() {
    let rig = Rig::new(Fake::new(true, "hold", None), AuthMode::Plan);
    let agent = CodexComputerAgent::new(rig.on_event(), rig.answering("no"), Options { rpc_timeout: 0.2, ..rig.options() });
    let runner = agent.clone();
    let run = tokio::spawn(async move { runner.run("test").await });
    let fake = rig.fake();
    wait_true(&fake.started).await;
    until(|| py::truthy(&agent.turn_id())).await;
    assert!(agent.steer("Use 3+3 instead"));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(fake.requests().iter().any(|m| m["method"] == "turn/steer"));
    fake.feed_eof();
    let out = tokio::time::timeout(Duration::from_secs(1), run).await.unwrap().unwrap().unwrap();
    assert!(out.contains("disconnected"));
}

fn png_16x12_blue() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(16, 12, image::Rgb([0, 0, 255]));
    let mut out = io::Cursor::new(vec![]);
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn browser_result(tab_id: &str, state_id: &str, data: Option<&str>) -> Value {
    let encoded = data
        .map(str::to_string)
        .unwrap_or_else(|| base64::engine::general_purpose::STANDARD.encode(png_16x12_blue()));
    json!({"content": [
        {"type": "text", "text": format!("EVE_BROWSER_CAPTURE {{\"tabId\":\"{tab_id}\"}}")},
        {"type": "text", "text": format!("Browser tab: {state_id}, Title: \"Example\", URL: \"https://example.com/\".\n0 AXWebArea Example")},
        {"type": "image", "mimeType": "image/png", "data": encoded},
    ]})
}

fn good() -> Value {
    browser_result("7", "7", None)
}

fn notify_browser(agent: &CodexComputerAgent, result: Value, thread: &str) {
    let msg = json!({"method": "item/completed", "params": {"threadId": thread, "item": {
        "id": "capture", "type": "mcpToolCall", "server": "cua_repl", "tool": "js",
        "status": "completed", "result": result}}});
    agent.shared.st.lock().notification(&msg).unwrap();
}

fn bare_agent() -> (Rig, CodexComputerAgent) {
    let rig = Rig::new(Fake::process(), AuthMode::Plan);
    let agent = rig.agent(rig.answering(""));
    agent.shared.st.lock().thread_id = json!("thread");
    (rig, agent)
}

#[tokio::test]
async fn test_browser_image_is_bound_to_the_observed_task_tab_and_verified_bytes() {
    let (_rig, agent) = bare_agent();
    notify_browser(&agent, good(), "other");
    assert!(!agent.browser_used() && agent.browser_screenshot().is_none());
    notify_browser(&agent, good(), "thread");
    assert!(agent.browser_used() && agent.browser_tab_id().as_deref() == Some("7"));
    let shot = agent.browser_screenshot().unwrap();
    assert_eq!(shot.mime, "image/png");
    assert!(agent.ui_evidence().last().unwrap()["state"].as_str().unwrap().contains("Browser tab: 7,"));
    let im = image::load_from_memory(&shot.data).unwrap().to_rgb8();
    assert_eq!(im.dimensions(), (16, 12));
    assert_eq!(im.get_pixel(0, 0).0, [0, 0, 255]);
    let content = good()["content"].as_array().unwrap().clone();
    let mut errored = good();
    errored["isError"] = json!(true);
    let bads = [
        browser_result("7", "8", None),
        browser_result("7", "7", Some("invalid!")),
        browser_result("7", "7", Some(&base64::engine::general_purpose::STANDARD.encode(b"not an image"))),
        errored,
        json!({"content": content[1..].to_vec()}),
        json!({"content": [content[0].clone(), content[2].clone()]}),
    ];
    for bad in bads {
        notify_browser(&agent, good(), "thread"); // a known good image must be invalidated
        assert!(agent.browser_screenshot().is_some());
        notify_browser(&agent, bad.clone(), "thread");
        assert!(agent.browser_screenshot().is_none(), "{bad}");
    }
}

#[tokio::test]
async fn test_new_cua_action_invalidates_previous_browser_picture() {
    let (_rig, agent) = bare_agent();
    notify_browser(&agent, good(), "thread");
    let msg = json!({"method": "item/started", "params": {"threadId": "thread",
        "item": {"type": "mcpToolCall", "server": "cua_repl", "tool": "js"}}});
    agent.shared.st.lock().notification(&msg).unwrap();
    assert!(agent.browser_screenshot().is_none() && agent.browser_used());
}

#[test]
fn base64_validate_is_pythons() {
    // Measured with CPython: base64.b64decode(s, validate=True).
    for (s, want) in [
        ("QQ==", Some(&b"A"[..])), ("QQ", None), ("QQ=", None), ("QUFB", Some(b"AAA")), ("QUFB=", Some(b"AAA")),
        ("QUFB==", Some(b"AAA")), ("QUFBQQ===", None), ("QR==", Some(b"A")), ("Q===", None), ("", Some(b"")),
        ("AB=C", None), ("====", None), ("A", None), ("invalid!", None),
    ] {
        assert_eq!(b64decode_validate(s).as_deref(), want, "{s:?}");
    }
}

// ---- test_codex_permissions.py ---------------------------------------------------------------------------------

fn request() -> Value {
    json!({"id": 10, "method": "mcpServer/elicitation/request", "params": {
        "threadId": "thread", "serverName": "cua_repl", "mode": "form",
        "message": "Allow Computer Use to use \"Calculator\"?",
        "requestedSchema": {"type": "object", "properties": {}},
        "_meta": {"codex_approval_kind": "mcp_tool_call", "connector_id": "computer-use",
                  "persist": ["session", "always"], "tool_name": "get_app_state",
                  "tool_params": {"app": "com.apple.calculator"}, "riskLevel": "low"}}})
}

fn meta_update(msg: &mut Value, change: Value) {
    for (k, v) in change.as_object().unwrap() {
        msg["params"]["_meta"][k] = v.clone();
    }
}

/// Python's `answer()`: the reply, the questions asked, the events emitted.
async fn answer(text: &str, msg: Value, site_access: &str, routine_permissions: &str) -> (Value, Vec<String>, Vec<AgentEvent>) {
    let rig = Rig::new(Fake::process(), AuthMode::Plan);
    let agent = CodexComputerAgent::new(
        rig.on_event(),
        rig.answering(text),
        Options {
            site_access: Some(site_access.into()),
            routine_permissions: Some(routine_permissions.into()),
            ..rig.options()
        },
    );
    agent.shared.st.lock().thread_id = json!("thread");
    let sent = Arc::new(Mutex::new(vec![]));
    *agent.shared.stdin.lock().await = Some(Box::new(Capture(sent.clone(), vec![])));
    agent.shared.clone().answer_request(msg).await;
    let reply = sent.lock()[0].clone();
    let questions = rig.questions.lock().clone();
    let events = rig.events.lock().clone();
    (reply, questions, events)
}

#[tokio::test]
async fn test_persistent_choices_go_to_codex() {
    for (text, expected) in
        [("always allow", "always"), ("allow always", "always"), ("allow for task", "session"), ("allow for this task", "session")]
    {
        let (reply, questions, _) = answer(text, request(), "ask", "ask").await;
        assert_eq!(reply["result"], json!({"action": "accept", "content": {}, "_meta": {"persist": expected}}));
        assert!(questions[0].contains("Calculator") && questions[0].contains("always allow") && questions[0].contains("allow for task"));
    }
}

#[tokio::test]
async fn test_once_and_denial_never_persist() {
    for (text, decision) in [("yes", "accept"), ("allow once", "accept"), ("no", "decline"),
                             ("always allow everything", "decline"), ("yes but later", "decline"), ("", "decline")] {
        let (reply, _, _) = answer(text, request(), "ask", "ask").await;
        assert!(reply["result"]["action"] == decision && reply["result"].get("_meta").is_none(), "{text}");
    }
}

#[tokio::test]
async fn test_managed_restriction_removes_always_without_downgrading_choice() {
    let mut msg = request();
    meta_update(&mut msg, json!({"persist": ["session"]}));
    let (reply, questions, events) = answer("always allow", msg.clone(), "ask", "ask").await;
    assert!(!questions[0].contains("always allow"));
    assert!(reply["result"]["action"] == "decline" && reply["result"].get("_meta").is_none());
    assert!(events.iter().any(|e| e.text().contains("no permission was saved")));
    assert_eq!(answer("allow for task", msg, "ask", "ask").await.0["result"]["_meta"]["persist"], "session");
}

#[tokio::test]
async fn test_unknown_audio_action_scopes_cannot_be_remembered() {
    for change in [
        json!({"connector_id": "browser"}), json!({"codex_request_type": "approval_request"}),
        json!({"tool_name": "start_audio_recording"}), json!({"tool_params": {}}),
        json!({"tool_params": {"app": "com.apple.calculator", "action": "delete"}}),
        json!({"tool_params": {"app": ""}}), json!({"persist": "always"}), json!({"persist": null}),
    ] {
        let mut msg = request();
        meta_update(&mut msg, change.clone());
        assert!(app_persistence(&msg["params"]).unwrap().is_empty(), "{change}");
        let (reply, questions, _) = answer("always allow", msg, "ask", "ask").await;
        assert!(reply["result"]["action"] == "decline" && !questions[0].contains("always allow"), "{change}");
    }
}

#[tokio::test]
async fn test_risk_warning_is_preserved_and_forms_still_fail_closed() {
    let mut msg = request();
    meta_update(&mut msg, json!({"subtitle": "This app can change account settings."}));
    let (_, questions, _) = answer("always allow", msg.clone(), "ask", "ask").await;
    assert!(questions[0].contains("This app can change account settings."));
    let mut bad = msg.clone();
    bad["params"]["requestedSchema"]["properties"]["password"] = json!({"type": "string"});
    let (reply, questions, _) = answer("always allow", bad, "ask", "ask").await;
    assert!(reply["result"]["action"] == "decline" && questions.is_empty());
    let mut bad = msg;
    bad["params"]["threadId"] = json!("other");
    let (reply, questions, _) = answer("always allow", bad, "ask", "ask").await;
    assert!(reply.get("error").is_some() && questions.is_empty());
}

fn site_request() -> Value {
    let mut msg = request();
    msg["params"]["message"] = json!("Allow Browser Use to access https://example.com?");
    msg["params"]["_meta"] = json!({
        "codex_approval_kind": "mcp_tool_call", "codex_sensitive_action": true,
        "codex_request_type": "approval_request", "connector_id": "browser-use",
        "persist": "always", "tool_name": "access_browser_origin",
        "tool_params": {"origin": "https://example.com"}, "origin": "https://example.com"});
    msg
}

#[tokio::test]
async fn test_owner_site_preference_avoids_prompt_without_saving_or_forging_review() {
    let msg = site_request();
    assert_eq!(browser_origin(&msg["params"]).unwrap().as_deref(), Some("https://example.com"));
    let (reply, questions, events) = answer("no", msg.clone(), "allow", "ask").await;
    assert_eq!(reply["result"], json!({"action": "accept", "content": {}}));
    assert!(questions.is_empty() && events.is_empty());
    let (reply, questions, _) = answer("no", msg, "ask", "ask").await;
    assert!(!questions.is_empty() && reply["result"]["action"] == "decline");
    let (reply, questions, _) = answer("no", request(), "allow", "ask").await;
    assert!(!questions.is_empty() && reply["result"]["action"] == "decline");
}

#[tokio::test]
async fn test_site_preference_does_not_approve_other_permissions() {
    for change in [
        json!({"tool_name": "access_browser_origin_with_raw_cdp"}),
        json!({"tool_name": "upload_browser_files"}), json!({"tool_name": "webmcp.tool"}),
        json!({"codex_strict_auto_review": true}), json!({"codex_requires_user_input": true}),
        json!({"full_cdp_access": true}), json!({"file_transfer": "upload"}),
        json!({"sensitive_data": "browsing_history"}), json!({"riskLevel": "high"}),
        json!({"connector_id": "computer-use"}), json!({"codex_approval_kind": "browser_auth"}),
        json!({"tool_params": {"origin": "https://example.com", "action": "send"}}),
        json!({"origin": "https://different.example"}), json!({"codex_request_type": "unknown"}),
    ] {
        let mut msg = site_request();
        meta_update(&mut msg, change.clone());
        assert_eq!(browser_origin(&msg["params"]).unwrap(), None, "{change}");
        let (reply, _, _) = answer("no", msg, "allow", "ask").await;
        assert_eq!(reply["result"]["action"], "decline", "{change}");
    }
}

#[tokio::test]
async fn test_only_bare_http_origins_receive_site_preference() {
    for origin in ["file:///tmp/a", "https://example.com/path", "https://user:secret@example.com",
                   "https://example.com?token=secret", "https://example.com#action", "https://example.com:bad", "",
                   "https://example.com\n"] {
        let mut msg = site_request();
        meta_update(&mut msg, json!({"origin": origin, "tool_params": {"origin": origin}}));
        assert_eq!(browser_origin(&msg["params"]).unwrap(), None, "{origin:?}");
        assert_eq!(answer("no", msg, "allow", "ask").await.0["result"]["action"], "decline", "{origin:?}");
    }
}

#[tokio::test]
async fn test_site_preference_does_not_approve_stale_threads_or_forms() {
    let mut msg = site_request();
    msg["params"]["threadId"] = json!("other");
    assert!(answer("yes", msg, "allow", "ask").await.0.get("error").is_some());
    let mut msg = site_request();
    msg["params"]["requestedSchema"]["properties"]["password"] = json!({"type": "string"});
    let (reply, questions, _) = answer("yes", msg, "allow", "ask").await;
    assert!(reply["result"]["action"] == "decline" && questions.is_empty());
}

#[tokio::test]
async fn test_routine_app_access_does_not_interrupt_chat_or_call_tasks() {
    for tool in APP_ACCESS_TOOLS {
        let mut msg = request();
        meta_update(&mut msg, json!({"tool_name": tool, "riskLevel": "high"}));
        let (reply, questions, _) = answer("no", msg.clone(), "ask", "allow").await;
        assert_eq!(reply["result"], json!({"action": "accept", "content": {}}), "{tool}");
        assert!(questions.is_empty());
        // The same request with the preference disabled must actually ask.
        let (reply, questions, _) = answer("no", msg, "ask", "ask").await;
        assert!(!questions.is_empty() && reply["result"]["action"] == "decline");
    }
}

#[tokio::test]
async fn test_file_transfer_preference() {
    for (transfer, origin, allowed) in [
        ("download", "https://example.com", true),
        ("upload", "https://web.telegram.org", true),
        ("upload", "https://example.com", false),
        ("upload", "https://web.telegram.org.evil.example", false),
        ("upload", "http://web.telegram.org", false),
        ("upload", "https://web.telegram.org:8443", false),
        ("download", "file:///private/data", false),
    ] {
        let mut msg = site_request();
        msg["params"]["_meta"].as_object_mut().unwrap().shift_remove("codex_request_type");
        meta_update(&mut msg, json!({"tool_name": format!("{transfer}_browser_files"), "file_transfer": transfer,
                                     "origin": origin, "tool_params": {"origin": origin}}));
        let (reply, questions, _) = answer("no", msg, "ask", "allow").await;
        assert_eq!(reply["result"]["action"], if allowed { "accept" } else { "decline" }, "{transfer} {origin}");
        assert_eq!(!questions.is_empty(), !allowed);
    }
}

#[tokio::test]
async fn test_routine_preference_never_inherits_action_or_strict_permissions() {
    for change in [
        json!({"codex_request_type": "approval_request"}), json!({"codex_strict_auto_review": true}),
        json!({"codex_requires_user_input": true}), json!({"full_cdp_access": true}),
        json!({"sensitive_data": "password"}), json!({"file_transfer": "upload"}),
        json!({"tool_name": "purchase"}), json!({"tool_name": "delete"}), json!({"tool_name": "send"}),
        json!({"tool_name": "start_audio_recording"}), json!({"connector_id": "unknown"}),
        json!({"tool_params": {"app": "com.apple.calculator", "action": "delete"}}),
        json!({"persist": null}),
    ] {
        let mut msg = request();
        meta_update(&mut msg, change.clone());
        let (reply, questions, _) = answer("no", msg, "ask", "allow").await;
        assert!(!questions.is_empty() && reply["result"]["action"] == "decline", "{change}");
    }
}

#[tokio::test]
async fn test_routine_preference_rejects_stale_and_nonempty_forms() {
    let mut msg = request();
    msg["params"]["threadId"] = json!("other");
    let (reply, questions, _) = answer("yes", msg, "ask", "allow").await;
    assert!(reply.get("error").is_some() && questions.is_empty());
    let mut msg = request();
    msg["params"]["requestedSchema"]["properties"]["consent"] = json!({"type": "boolean"});
    let (reply, questions, _) = answer("yes", msg, "ask", "allow").await;
    assert!(reply["result"]["action"] == "decline" && questions.is_empty());
}

#[test]
fn test_owner_defaults_and_opt_out() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    };
    assert_eq!(access_settings(None, None, &env(&[])), ("allow".into(), "allow".into()));
    assert_eq!(access_settings(None, None, &env(&[("EVE_CODEX_ROUTINE_ACCESS", "ask")])), ("ask".into(), "ask".into()));
    assert_eq!(
        access_settings(None, None, &env(&[("EVE_CODEX_ROUTINE_ACCESS", "ask"), ("EVE_CODEX_SITE_ACCESS", "allow")])),
        ("ask".into(), "allow".into())
    );
    // Explicit arguments win over the environment.
    assert_eq!(
        access_settings(Some("ask".into()), Some("ask".into()), &env(&[("EVE_CODEX_SITE_ACCESS", "allow")])),
        ("ask".into(), "ask".into())
    );
}

#[test]
fn test_both_agents_receive_owner_action_confirmation_policy() {
    // The chat half (CHAT_INSTRUCTIONS) is eve-codex-chat's.
    assert!(INSTRUCTIONS.contains(OWNER_ACTION_INSTRUCTIONS));
    assert!(OWNER_ACTION_INSTRUCTIONS.contains("Ask before committing a purchase, deleting anything"));
    assert!(OWNER_ACTION_INSTRUCTIONS.contains("outside Telegram"));
    assert!(INSTRUCTIONS.ends_with(BROWSER_INSTRUCTIONS));
    assert_eq!(INSTRUCTIONS_WITH_VAULT, format!("{INSTRUCTIONS}{VAULT_INSTRUCTIONS}"));
}

#[tokio::test]
async fn test_routine_preference_keeps_explicit_user_questions() {
    let msg = json!({"id": 10, "method": "item/tool/requestUserInput", "params": {
        "threadId": "thread", "questions": [
            {"id": "purchase", "question": "Buy this item for $10?", "options": []}]}});
    let (reply, questions, _) = answer("no", msg, "ask", "allow").await;
    assert_eq!(questions, ["Codex: Buy this item for $10?"]);
    assert_eq!(reply["result"], json!({"answers": {"purchase": {"answers": ["no"]}}}));
}

#[tokio::test]
async fn questions_list_their_options_and_unknown_methods_are_refused() {
    let msg = json!({"id": 11, "method": "tool/requestUserInput", "params": {
        "threadId": "thread", "questions": [
            {"id": 3, "question": "Which size?", "options": [{"label": "S"}, {"label": "M"}]}]}});
    let (reply, questions, events) = answer("M", msg, "ask", "ask").await;
    assert_eq!(questions, ["Codex: Which size?\nOptions: S, M"]);
    assert!(events.iter().any(|e| e.kind() == "ask" && e.text() == "Which size?\nOptions: S, M"));
    assert_eq!(reply, json!({"id": 11, "result": {"answers": {"3": {"answers": ["M"]}}}}));
    let msg = json!({"id": 12, "method": "item/commandExecution/requestApproval", "params": {"threadId": "thread"}});
    let (reply, questions, events) = answer("yes", msg, "allow", "allow").await;
    assert!(questions.is_empty());
    assert_eq!(reply["error"]["code"], -32601);
    assert!(events[0].text().contains("outside the Computer Use adapter"));
    // A form Eve cannot show is declined with a word to the owner; a malformed one is "User input unavailable".
    let mut msg = request();
    msg["params"]["serverName"] = json!("other_server");
    let (reply, _, events) = answer("yes", msg, "ask", "ask").await;
    assert_eq!(reply["result"], json!({"action": "decline"}));
    assert!(events[0].text().contains("cannot display safely"));
    let mut msg = request();
    msg["params"]["requestedSchema"] = Value::Null;
    msg["params"]["serverName"] = json!("cua_repl");
    let (reply, _, _) = answer("yes", msg, "ask", "ask").await;
    assert_eq!(reply["error"], json!({"code": -32603, "message": "User input unavailable"}));
}

#[tokio::test]
async fn a_broken_question_transport_never_becomes_approval() {
    let rig = Rig::new(Fake::process(), AuthMode::Plan);
    let agent = CodexComputerAgent::new(
        rig.on_event(),
        rig.never_asked(),
        Options { routine_permissions: Some("ask".into()), site_access: Some("ask".into()), ..rig.options() },
    );
    agent.shared.st.lock().thread_id = json!("thread");
    let sent = Arc::new(Mutex::new(vec![]));
    *agent.shared.stdin.lock().await = Some(Box::new(Capture(sent.clone(), vec![])));
    agent.shared.clone().answer_request(request()).await;
    assert_eq!(sent.lock()[0], json!({"id": 10, "error": {"code": -32603, "message": "User input unavailable"}}));
}

// ---- test_vault_mcp.py (the Codex wiring) ----------------------------------------------------------------------

const CANARY_NUMBER: &str = "4000056655665556";

fn card() -> eve_vault::Values {
    let mut v = Map::new();
    for (k, x) in [("number", CANARY_NUMBER), ("exp_month", "7"), ("exp_year", "2031"), ("cvc", "713")] {
        v.insert(k.into(), json!(x));
    }
    v
}

#[tokio::test]
async fn test_codex_threads_get_the_vault_server_only_when_the_vault_has_items() {
    let (rig, agent) = bare_agent();
    let vault = rig.vault();
    agent.set_vault(Some(vault.clone()));
    let empty = agent.shared.thread_params();
    assert!(empty.get("config").is_none() && empty["developerInstructions"] == INSTRUCTIONS);
    vault.add("card", "Visa", &card(), "").unwrap();
    let full = agent.shared.thread_params();
    let server = &full["config"]["mcp_servers"]["eve_vault"];
    // The Rust daemon's threads start the Rust server (eve-vault-mcp's binary) with no arguments.
    assert_eq!(server["args"], json!([]));
    assert!(server["command"].as_str().is_some_and(|c| !c.is_empty()));
    assert!(full["developerInstructions"].as_str().unwrap().ends_with(VAULT_INSTRUCTIONS));
    assert!(full["sandbox"] == "read-only" && full["approvalPolicy"] == "on-request");
    assert!(!full.to_string().contains(CANARY_NUMBER));
}

#[tokio::test]
async fn test_codex_a_warm_thread_is_stale_once_the_vault_gains_or_loses_its_items() {
    let (rig, agent) = bare_agent();
    let vault = rig.vault();
    agent.set_vault(Some(vault.clone()));
    agent.shared.thread_params(); // warmed with an empty vault
    assert!(!agent.shared.vault_changed());
    let item = vault.add("card", "Visa", &card(), "").unwrap();
    assert!(agent.shared.vault_changed()); // run() opens a thread with the vault
    agent.shared.thread_params();
    assert!(!agent.shared.vault_changed());
    vault.remove(&item.handle).unwrap();
    assert!(agent.shared.vault_changed());
}

#[test]
fn test_codex_accepts_only_the_vault_servers_own_tool_approvals() {
    let form = |server: &str, tool: &str, meta: Value| {
        let mut m = json!({"codex_approval_kind": "mcp_tool_call", "tool_name": tool});
        for (k, v) in meta.as_object().unwrap() {
            m[k] = v.clone();
        }
        json!({"serverName": server, "mode": "form", "requestedSchema": {"type": "object", "properties": {}}, "_meta": m})
    };
    assert!(vault_tool_call(&form("eve_vault", "vault_fill", json!({}))).unwrap());
    assert!(vault_tool_call(&form("eve_vault", "vault_list", json!({}))).unwrap());
    assert!(!vault_tool_call(&form("cua_repl", "vault_fill", json!({}))).unwrap());
    assert!(!vault_tool_call(&form("eve_vault", "something_else", json!({}))).unwrap());
    assert!(!vault_tool_call(&form("eve_vault", "vault_fill", json!({"codex_approval_kind": "other"}))).unwrap());
    let live = json!({"serverName": "eve_vault", "mode": "form", "requestedSchema": {"type": "object", "properties": {}},
        "message": "Allow the eve_vault MCP server to run tool \"vault_fill\"?",
        "_meta": {"codex_approval_kind": "mcp_tool_call", "persist": ["session", "always"], "tool_params": {}}});
    assert!(vault_tool_call(&live).unwrap()); // Codex 0.159's real form (2026-10-12)
    let mut other = live.clone();
    other["message"] = json!("Allow the eve_vault MCP server to run tool \"other\"?");
    assert!(!vault_tool_call(&other).unwrap());
    let mut cua = live.clone();
    cua["serverName"] = json!("cua_repl");
    assert!(!vault_tool_call(&cua).unwrap());
    let mut asks_for_input = form("eve_vault", "vault_fill", json!({}));
    asks_for_input["requestedSchema"] = json!({"type": "object", "properties": {"x": {"type": "string"}}});
    assert!(!vault_tool_call(&asks_for_input).unwrap());
    // re.match's `$` also matches before a final newline.
    let mut newline = live;
    newline["message"] = json!("Allow the eve_vault MCP server to run tool \"vault_list\"?\n");
    assert!(vault_tool_call(&newline).unwrap());
}

#[tokio::test]
async fn vault_tool_approvals_are_accepted_without_asking() {
    let msg = json!({"id": 5, "method": "mcpServer/elicitation/request", "params": {"threadId": "thread",
        "serverName": "eve_vault", "mode": "form", "requestedSchema": {"type": "object", "properties": {}},
        "message": "Allow the eve_vault MCP server to run tool \"vault_fill\"?",
        "_meta": {"codex_approval_kind": "mcp_tool_call"}}});
    let (reply, questions, _) = answer("no", msg, "ask", "ask").await;
    assert_eq!(reply, json!({"id": 5, "result": {"action": "accept", "content": {}}}));
    assert!(questions.is_empty());
}

// ---- test_codex_api_metering.py (the computer agent) -----------------------------------------------------------

async fn metered_run(cfg: MeterCfg) -> Rig {
    let rig = Rig::new(Fake::metered(cfg), AuthMode::Api);
    rig.agent(rig.answering("no")).run("inspect").await.unwrap();
    rig
}

#[tokio::test]
async fn test_api_computer_uses_process_home_and_prices_full_turn_once() {
    let rig = metered_run(MeterCfg {
        totals: vec![vec![usage(1000, 400, 100), usage(2000, 800, 300), usage(2000, 800, 300)]],
        ..MeterCfg::default()
    })
    .await;
    let launches = rig.launches.lock().clone();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].2, rig.launch_env.lock().0);
    assert!(rig.fake().requests().iter().all(|m| m.get("params").and_then(|p| p.get("model")).is_none()));
    let got = rig.rows();
    assert_eq!(got.len(), 1);
    let mut row = got[0].clone();
    assert!(approx(&row["usd"], 0.00556));
    row.insert("t".into(), json!(0));
    row.insert("usd".into(), json!(0));
    assert_eq!(Value::Object(row), json!({
        "t": 0, "p": "openai", "m": "codex", "f": "codex", "usd": 0,
        "src": "priced", "tok": {"in": 2000, "cached": 800, "out": 300},
        "note": "API key, billed per token (computer task)"}));
}

#[tokio::test]
async fn test_plan_metering_stays_unpriced_and_is_recorded_before_turn() {
    let rig = Rig::new(Fake::metered(MeterCfg { totals: vec![vec![usage(1000, 0, 0)]], hold: true, ..MeterCfg::default() }), AuthMode::Plan);
    let agent = rig.agent(rig.answering("no"));
    let runner = agent.clone();
    let task = tokio::spawn(async move { runner.run("inspect").await });
    let fake = rig.fake();
    wait_true(&fake.started).await;
    let rows = rig.rows();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!((&row["p"], &row["m"], &row["usd"], &row["note"]),
               (&json!("chatgpt"), &json!("codex"), &Value::Null, &json!("ChatGPT plan, not per-call (computer task)")));
    assert!(row.get("tok").is_none());
    fake.finish("completed");
    task.await.unwrap().unwrap();
    assert_eq!(rig.rows().len(), 1);
}

#[tokio::test]
async fn test_warm_process_keeps_its_launch_auth_identity() {
    let rig = Arc::new(Rig::new(Fake::metered(MeterCfg { totals: vec![vec![usage(1000, 0, 0)]], ..MeterCfg::default() }), AuthMode::Api));
    let env = rig.launch_env.lock().0.clone();
    let factory_rig = rig.clone();
    let pool = eve_codex_warm::WarmCodex::new(
        Arc::new(move |on_event, ask_user| {
            CodexComputerAgent::new(on_event, ask_from_warm(ask_user), factory_rig.options())
        }),
        true,
        900.0,
    );
    pool.kick();
    pool.join_jobs().await;
    assert!(rig.rows().is_empty());
    let home = rig.tmp.path().to_string_lossy().into_owned();
    *rig.launch_env.lock() = ([("HOME".to_string(), home)].into_iter().collect(), AuthMode::Plan);
    let agent = pool.take(Arc::new(|_| {}), eve_codex_warm::nobody_ask());
    agent.run("inspect").await.unwrap();
    pool.close().await;
    let launches = rig.launches.lock().clone();
    assert!(launches.len() == 1 && launches[0].2 == env);
    let rows = rig.rows();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["p"] == "openai" && approx(&rows[0]["usd"], 0.002));
}

#[tokio::test]
async fn test_unknown_model_or_absent_usage_is_unpriced() {
    for (model, totals) in [
        (json!("unknown-model"), vec![vec![usage(1000, 0, 0)]]),
        (Value::Null, vec![vec![usage(1000, 0, 0)]]),
        (json!("gpt-6-sol"), vec![]),
    ] {
        let rig = metered_run(MeterCfg { model: model.clone(), totals, ..MeterCfg::default() }).await;
        let rows = rig.rows();
        assert_eq!(rows.len(), 1);
        assert!(rows[0]["p"] == "openai" && rows[0]["usd"].is_null() && rows[0]["unpriced"] == true, "{model}");
    }
}

#[tokio::test]
async fn test_malformed_usage_never_becomes_free_or_guessed_spend() {
    for bad in [
        json!({}), Value::Null, json!({"inputTokens": true, "cachedInputTokens": 0, "outputTokens": 0}),
        usage(-1, 0, 0), usage(5, 6, 0), json!({"inputTokens": "5"}),
    ] {
        let rig = metered_run(MeterCfg { totals: vec![vec![bad.clone()]], ..MeterCfg::default() }).await;
        let rows = rig.rows();
        assert_eq!(rows.len(), 1);
        assert!(rows[0]["usd"].is_null() && rows[0]["unpriced"] == true && rows[0].get("tok").is_none(), "{bad}");
    }
}

async fn cancel_held_turn(cfg: MeterCfg) -> Rig {
    let rig = Rig::new(Fake::metered(MeterCfg { hold: true, ..cfg }), AuthMode::Api);
    let agent = rig.agent(rig.answering("no"));
    let runner = agent.clone();
    let task = tokio::spawn(async move { runner.run("work").await });
    wait_true(&rig.fake().started).await;
    until(|| !agent.turn_id().is_null()).await;
    agent.cancel("");
    task.await.unwrap().unwrap();
    rig
}

#[tokio::test]
async fn test_cancellation_collects_interrupt_usage_and_records_once() {
    let rig = cancel_held_turn(MeterCfg { interrupt_total: Some(usage(1000, 100, 20)), ..MeterCfg::default() }).await;
    let rows = rig.rows();
    assert_eq!(rows.len(), 1);
    assert!(approx(&rows[0]["usd"], 0.00202));
    assert_eq!(rows[0]["tok"], json!({"in": 1000, "cached": 100, "out": 20}));
}

#[tokio::test]
async fn test_interrupt_ack_without_completion_never_prices_partial_usage() {
    let rig = cancel_held_turn(MeterCfg {
        totals: vec![vec![usage(100, 0, 0)]],
        interrupt_completed: false,
        ..MeterCfg::default()
    })
    .await;
    let rows = rig.rows();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["usd"].is_null() && rows[0]["unpriced"] == true);
    assert_eq!(rows[0]["tok"], json!({"in": 100}));
    assert!(rows[0]["note"].as_str().unwrap().contains("incomplete usage"));
}

#[tokio::test]
async fn test_failed_api_launch_counts_one_unpriced_task() {
    let rig = Rig::new(Fake::process(), AuthMode::Api);
    let agent = CodexComputerAgent::new(
        rig.on_event(),
        rig.answering("no"),
        Options { binary: Some("/nonexistent/codex".into()), spawner: Some(os_spawner()), ..rig.options() },
    );
    let out = agent.run("work").await.unwrap();
    assert_eq!(out, NOT_STARTED);
    let rows = rig.rows();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["p"] == "openai" && rows[0]["usd"].is_null());
}

#[tokio::test]
async fn test_reusing_computer_agent_starts_fresh_usage_baseline() {
    let rig = Rig::new(Fake::metered(MeterCfg { totals: vec![vec![usage(1000, 0, 0)]], ..MeterCfg::default() }), AuthMode::Api);
    let agent = rig.agent(rig.answering("no"));
    assert!(agent.run("inspect").await.unwrap().contains("Done."));
    *rig.slot.lock() = Fake::metered(MeterCfg { totals: vec![vec![usage(200, 0, 0)]], ..MeterCfg::default() });
    assert!(agent.run("inspect").await.unwrap().contains("Done."));
    let usd: Vec<f64> = rig.rows().iter().map(|r| r["usd"].as_f64().unwrap()).collect();
    assert_eq!(usd, [0.002, 0.0004]);
}

#[tokio::test]
async fn test_missing_api_home_falls_back_in_both_environment_and_spend() {
    let rig = Rig::new(Fake::metered(MeterCfg { totals: vec![vec![usage(1000, 0, 0)]], ..MeterCfg::default() }), AuthMode::Api);
    let personal = rig.tmp.path().join("personal").to_string_lossy().into_owned();
    let process_env: std::collections::HashMap<String, String> =
        [("EVE_CODEX_AUTH".to_string(), "api".to_string()), ("CODEX_HOME".to_string(), personal.clone())].into_iter().collect();
    let missing = rig.tmp.path().join("missing");
    let warned: eve_codex_auth::Warned = Default::default();
    *rig.launch_env.lock() = eve_codex_auth::launch_environment_with(&process_env, &missing, &warned);
    rig.agent(rig.answering("no")).run("inspect").await.unwrap();
    assert_eq!(rig.launches.lock()[0].2["CODEX_HOME"], personal);
    let rows = rig.rows();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["p"] == "chatgpt" && rows[0]["usd"].is_null());
    assert_eq!(rows[0]["note"], "ChatGPT plan, not per-call (computer task)");
}

// ---- test_spend_wiring.py --------------------------------------------------------------------------------------

#[tokio::test]
async fn test_codex_is_counted_without_a_price() {
    let rig = Rig::new(Fake::process(), AuthMode::Plan);
    let agent = CodexComputerAgent::new(
        rig.on_event(),
        rig.answering("no"),
        Options { binary: Some("/nonexistent/codex".into()), spawner: Some(os_spawner()), ..rig.options() },
    );
    agent.run("open notes").await.unwrap(); // cannot start: still one task counted
    let rows = rig.rows();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!((&row["p"], &row["m"], &row["f"], &row["usd"]), (&json!("chatgpt"), &json!("codex"), &json!("codex"), &Value::Null));
    assert!(row["note"].as_str().unwrap().starts_with("ChatGPT plan, not per-call"));
}

// ---- parity additions ------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_second_run_while_running_is_a_runtime_error_and_status_reports_the_task() {
    let rig = Rig::new(Fake::new(true, "hold", None), AuthMode::Plan);
    let agent = rig.agent(rig.answering("no"));
    let runner = agent.clone();
    let task = tokio::spawn(async move { runner.run("hold on").await });
    wait_true(&rig.fake().started).await;
    assert_eq!(agent.run("again").await.unwrap_err(), CodexError::Runtime("A Codex task is already running.".into()));
    until(|| agent.status()["last"] == "Opening Calculator").await;
    let status = agent.status();
    assert_eq!(status, json!({"running": true, "provider": "codex", "goal": "hold on",
                              "last": "Opening Calculator", "final": "", "thread_id": "thread"}));
    assert!(!agent.steer("   "));
    agent.cancel("owner said stop");
    assert_eq!(task.await.unwrap().unwrap(), STOPPED);
    assert!(!agent.steer("too late"));
    // The agent stays cancelled, as Python's `_cancelled` is never reset.
    assert_eq!(agent.run("next").await.unwrap(), STOPPED);
}

#[tokio::test]
async fn requests_go_out_as_python_json_dumps_writes_them() {
    let rig = Rig::new(Fake::process(), AuthMode::Plan);
    let agent = rig.agent(rig.answering("no"));
    let sent = Arc::new(Mutex::new(vec![]));
    struct Raw(Arc<Mutex<Vec<u8>>>);
    impl AsyncWrite for Raw {
        fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, d: &[u8]) -> Poll<io::Result<usize>> {
            self.0.lock().extend_from_slice(d);
            Poll::Ready(Ok(d.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    *agent.shared.stdin.lock().await = Some(Box::new(Raw(sent.clone())));
    agent.shared.send(&json!({"method": "initialized", "params": {}})).await.unwrap();
    agent.shared.send(&json!({"id": 1, "result": {"text": "café"}})).await.unwrap();
    assert_eq!(
        String::from_utf8(sent.lock().clone()).unwrap(),
        "{\"method\": \"initialized\", \"params\": {}}\n{\"id\": 1, \"result\": {\"text\": \"caf\\u00e9\"}}\n"
    );
}

#[tokio::test]
async fn a_malformed_event_stream_fails_the_task_without_a_fallback() {
    for line in ["not json\n", "{\"method\": \"item/completed\", \"params\": null}\n", "5\n"] {
        let rig = Rig::new(Fake::new(true, "hold", None), AuthMode::Plan);
        let agent = rig.agent(rig.answering("no"));
        let runner = agent.clone();
        let task = tokio::spawn(async move { runner.run("x").await });
        let fake = rig.fake();
        wait_true(&fake.started).await;
        until(|| py::truthy(&agent.turn_id())).await;
        let _ = fake.tx.send(Some(line.as_bytes().to_vec()));
        assert_eq!(task.await.unwrap().unwrap(), INVALID_STREAM, "{line:?}");
        assert!(rig.events.lock().iter().any(|e| e.kind() == "error" && e.text() == INVALID_STREAM));
    }
}

#[tokio::test]
async fn a_failed_or_interrupted_turn_is_never_reported_as_done() {
    for (status, want, kind) in [
        ("failed", "Codex could not complete the task. The requested result is not verified.", "error"),
        ("interrupted", STOPPED, "cancelled"),
    ] {
        let rig = Rig::new(Fake::metered(MeterCfg { hold: true, ..MeterCfg::default() }), AuthMode::Plan);
        let agent = rig.agent(rig.answering("no"));
        let runner = agent.clone();
        let task = tokio::spawn(async move { runner.run("x").await });
        let fake = rig.fake();
        wait_true(&fake.started).await;
        fake.finish(status);
        assert_eq!(task.await.unwrap().unwrap(), want);
        assert!(rig.events.lock().iter().any(|e| e.kind() == kind && e.text() == want));
    }
}

#[tokio::test]
async fn completion_without_ui_evidence_says_so() {
    let rig = metered_run(MeterCfg::default()).await;
    let _ = rig;
    let rig = Rig::new(Fake::metered(MeterCfg::default()), AuthMode::Plan);
    let out = rig.agent(rig.answering("no")).run("x").await.unwrap();
    assert_eq!(out, "Done.\nEve received no UI-state evidence; do not treat this as verified completion.");
}

#[tokio::test]
async fn a_task_past_max_secs_times_out_and_is_closed() {
    let rig = Rig::new(Fake::new(true, "hold", None), AuthMode::Plan);
    let agent = CodexComputerAgent::new(rig.on_event(), rig.answering("no"), Options { max_secs: 0.2, ..rig.options() });
    assert_eq!(agent.run("x").await.unwrap(), NOT_STARTED);
    let fake = rig.fake();
    assert!(fake.requests().iter().any(|m| m["method"] == "turn/interrupt"));
    assert_eq!(fake.returncode(), Some(0));
}

#[tokio::test]
async fn prewarm_then_run_only_starts_the_turn_and_warm_tracks_the_process() {
    let rig = Rig::new(Fake::process(), AuthMode::Plan);
    let agent = rig.agent(rig.answering("no"));
    assert!(!agent.warm(900.0));
    agent.prewarm().await.unwrap();
    assert!(agent.warm(900.0) && !agent.warm(0.0));
    let before = rig.fake().requests().len();
    assert_eq!(agent.run("Compute 2+2").await.unwrap(), "Calculator displays 4.");
    let after: Vec<String> = rig.fake().requests()[before..]
        .iter()
        .filter_map(|m| m["method"].as_str().map(str::to_string))
        .collect();
    assert_eq!(after, ["turn/start"]);
    assert!(!agent.warm(900.0));
    let rig = Rig::new(Fake::process(), AuthMode::Plan);
    let agent = rig.agent(rig.answering("no"));
    agent.prewarm().await.unwrap();
    agent.discard().await;
    assert!(!agent.warm(900.0) && rig.fake().returncode() == Some(0));
}

#[tokio::test]
async fn a_failed_prewarm_closes_what_it_started() {
    let rig = Rig::new(Fake::new(false, "complete", None), AuthMode::Plan);
    let agent = rig.agent(rig.answering("no"));
    let err = agent.prewarm().await.unwrap_err();
    assert!(matches!(&err, CodexError::Unavailable(m) if m.contains("no cua_repl.js")));
    assert_eq!(<CodexComputerAgent as eve_codex_warm::WarmAgent>::failure_name(&anyhow::Error::from(err)), "CodexUnavailable");
    assert_eq!(rig.fake().returncode(), Some(0));
    assert!(!agent.warm(900.0));
}

#[tokio::test]
async fn dropping_a_run_stops_it_in_the_background() {
    let rig = Rig::new(Fake::new(true, "hold", None), AuthMode::Plan);
    let agent = rig.agent(rig.answering("no"));
    let runner = agent.clone();
    let task = tokio::spawn(async move { runner.run("x").await });
    let fake = rig.fake();
    wait_true(&fake.started).await;
    until(|| py::truthy(&agent.turn_id())).await;
    task.abort();
    let _ = task.await;
    let mut finished = fake.finished.subscribe();
    tokio::time::timeout(Duration::from_secs(2), finished.wait_for(|f| *f)).await.unwrap().unwrap();
    until(|| !agent.running()).await;
    assert_eq!(agent.final_text(), STOPPED);
    assert!(rig.events.lock().iter().any(|e| e.kind() == "cancelled"));
}

#[test]
fn executable_prefers_the_setting_then_chatgpt_then_path() {
    let tmp = tempfile::tempdir().unwrap();
    let bundled = tmp.path().join("codex");
    let none = || None;
    assert_eq!(executable_from(Some("/opt/codex"), &bundled, &none), "/opt/codex");
    assert_eq!(executable_from(Some(""), &bundled, &|| Some("/usr/local/bin/codex".into())), "/usr/local/bin/codex");
    assert_eq!(executable_from(None, &bundled, &none), "codex");
    std::fs::write(&bundled, "").unwrap();
    assert_eq!(executable_from(None, &bundled, &none), bundled.to_string_lossy());
    assert_eq!(which_codex(Some("")), None);
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let exe = bin.join("codex");
    std::fs::write(&exe, "#!/bin/sh\n").unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(which_codex(Some(&format!("/nonexistent:{}", bin.display()))), Some(exe.to_string_lossy().into_owned()));
}

#[tokio::test]
async fn read_line_keeps_asyncios_limit_and_last_partial_line() {
    let data: &[u8] = b"ab\ncd";
    let mut r = BufReader::new(data);
    assert!(matches!(read_line(&mut r, 10).await.unwrap(), ReadLine::Line(l) if l == b"ab\n"));
    assert!(matches!(read_line(&mut r, 10).await.unwrap(), ReadLine::Line(l) if l == b"cd"));
    assert!(matches!(read_line(&mut r, 10).await.unwrap(), ReadLine::Eof));
    let mut r = BufReader::new(&b"abcdef\n"[..]);
    assert!(matches!(read_line(&mut r, 5).await.unwrap(), ReadLine::TooLong));
    let mut r = BufReader::new(&b"abcde\n"[..]);
    assert!(matches!(read_line(&mut r, 5).await.unwrap(), ReadLine::Line(_)));
}

#[test]
fn malformed_forms_raise_where_python_raises() {
    // An unhashable codex_request_type is a TypeError in Python's set test; the approval then fails closed.
    let mut msg = site_request();
    meta_update(&mut msg, json!({"codex_request_type": ["x"]}));
    assert_eq!(browser_origin(&msg["params"]).unwrap_err().class, "TypeError");
    assert_eq!(browser_origin(&json!(null)).unwrap_err().class, "AttributeError");
    let mut msg = request();
    meta_update(&mut msg, json!({"tool_name": {"x": 1}}));
    assert_eq!(app_persistence(&msg["params"]).unwrap_err().class, "TypeError");
    assert_eq!(pending_key(&json!(1.0)).unwrap(), Some(1));
    assert_eq!(pending_key(&json!(true)).unwrap(), Some(1));
    assert_eq!(pending_key(&json!("1")).unwrap(), None);
    assert!(pending_key(&json!([1])).is_err());
}
