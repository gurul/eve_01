//! Ports of bridge/tests/test_codex_chat.py (new-chat lifecycle over JSON lines, folder selection), and of the
//! chat cases of test_codex_api_metering.py and test_codex_permissions.py.
//!
//! The fake is Python's `ChatProcess` (and `MeterProcess`): it answers each line the chat writes synchronously,
//! in order, and feeds its replies to the chat's stdout.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use eve_codex_computer::{Child, Spawned, Spawner, APP_ACCESS_TOOLS, OWNER_ACTION_INSTRUCTIONS};
use eve_spend::Ledger;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, watch, Notify};

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
    mode: Mutex<&'static str>,
    finished: watch::Sender<bool>,
    returncode: Mutex<Option<i32>>,
    meter: Option<MeterCfg>,
    turn: Mutex<usize>,
    started: watch::Sender<bool>,
}

impl Fake {
    fn build(mode: &'static str, meter: Option<MeterCfg>) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            tx,
            rx: Mutex::new(Some(rx)),
            eof: Mutex::new(false),
            requests: Mutex::new(vec![]),
            mode: Mutex::new(mode),
            finished: watch::Sender::new(false),
            returncode: Mutex::new(None),
            meter,
            turn: Mutex::new(0),
            started: watch::Sender::new(false),
        })
    }

    /// `ChatProcess(mode=...)`.
    fn chat(mode: &'static str) -> Arc<Self> {
        Self::build(mode, None)
    }

    /// `MeterProcess(...)`.
    fn metered(cfg: MeterCfg) -> Arc<Self> {
        Self::build("complete", Some(cfg))
    }

    fn set_mode(&self, mode: &'static str) {
        *self.mode.lock() = mode;
    }

    fn mode(&self) -> &'static str {
        *self.mode.lock()
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

    fn count(&self, method: &str) -> usize {
        self.requests().iter().filter(|m| m.get("method").and_then(Value::as_str) == Some(method)).count()
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

    /// `ChatProcess.write`, then `Process.write`: one line from the chat.
    fn write(&self, data: &[u8]) {
        let m: Value = serde_json::from_slice(data).unwrap();
        if self.meter_write(&m) {
            return;
        }
        let method = m.get("method").and_then(Value::as_str);
        if method == Some("thread/start") {
            self.requests.lock().push(m.clone());
            self.out(json!({"id": m["id"], "result": {"thread": {"id": "thread", "cwd": m["params"]["cwd"]}}}));
            return;
        }
        self.requests.lock().push(m.clone());
        let mode = self.mode();
        let mut result = json!({});
        match method {
            Some("initialize") => result = json!({"userAgent": "test"}),
            Some("mcpServerStatus/list") => {
                result = json!({"data": [{"name": "cua_repl", "tools": {"js": {}}}], "nextCursor": null});
            }
            Some("turn/start") => {
                result = json!({"turn": {"id": "turn"}});
                self.started.send_replace(true);
                self.event("item/completed", json!({"item": {"type": "agentMessage", "phase": "commentary",
                    "text": "Opening Calculator"}}));
                match mode {
                    "approval" => self.out(json!({"id": 100, "method": "mcpServer/elicitation/request", "params": {
                        "threadId": "thread", "serverName": "cua_repl", "mode": "form",
                        "message": "Allow Computer Use to use \"Calculator\"?",
                        "requestedSchema": {"type": "object", "properties": {}}}})),
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
        if method.is_some() && m.get("id").is_some() && mode != "disconnect" {
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

/// The chat's stdout: what the fake feeds, then end of stream.
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

/// The chat's stdin: each complete line goes to `Fake::write` at once, as Python's fake `write()` handles it.
struct FakeStdin {
    fake: Arc<Fake>,
    buf: Vec<u8>,
}

impl AsyncWrite for FakeStdin {
    fn poll_write(mut self: Pin<&mut Self>, _: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        if self.fake.returncode().is_some() {
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

type Launches = Arc<Mutex<Vec<(String, Vec<String>, IndexMap<String, String>)>>>;

/// Hands out the fake in `slot`, or a new `ChatProcess` per launch when `fresh` (Python's `processes` list).
struct Rig {
    slot: Arc<Mutex<Arc<Fake>>>,
    fresh: bool,
    processes: Arc<Mutex<Vec<Arc<Fake>>>>,
    launches: Launches,
    launch_env: Arc<Mutex<(IndexMap<String, String>, AuthMode)>>,
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
            fresh: false,
            processes: Arc::new(Mutex::new(vec![])),
            launches: Arc::new(Mutex::new(vec![])),
            launch_env: Arc::new(Mutex::new((env, mode))),
            tmp,
        }
    }

    /// A rig whose every launch gets a new `ChatProcess()`.
    fn fresh() -> Self {
        Self { fresh: true, ..Self::new(Fake::chat("complete"), AuthMode::Plan) }
    }

    fn fake(&self) -> Arc<Fake> {
        self.slot.lock().clone()
    }

    fn folder(&self) -> PathBuf {
        let f = self.tmp.path().join("work");
        std::fs::create_dir_all(&f).unwrap();
        f
    }

    fn ledger_dir(&self) -> PathBuf {
        self.tmp.path().join("spend")
    }

    fn spawner(&self) -> Spawner {
        let (slot, fresh, processes, launches) =
            (self.slot.clone(), self.fresh, self.processes.clone(), self.launches.clone());
        Arc::new(move |program, args, env| {
            let fake = if fresh { Fake::chat("complete") } else { slot.lock().clone() };
            processes.lock().push(fake.clone());
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

    fn options(&self) -> Options {
        let env = self.launch_env.clone();
        Options {
            binary: Some("codex".into()),
            spawner: Some(self.spawner()),
            launch_environment: Some(Arc::new(move || env.lock().clone())),
            ledger: Some(Ledger::at(self.ledger_dir())),
            vault: Some(Arc::new(eve_vault::Vault::with_store(
                self.tmp.path().join("vault.json"),
                Box::new(eve_vault::MemKeychain::new()),
            ))),
            ..Options::default()
        }
    }

    fn chat(&self, ask: Ask) -> CodexChat {
        CodexChat::new(ask, self.options())
    }

    fn rows(&self) -> Vec<Map<String, Value>> {
        eve_spend::day_rows(&chrono::Local::now().format("%Y-%m-%d").to_string(), Some(&self.ledger_dir()))
    }
}

fn usage(inp: i64, cached: i64, out: i64) -> Value {
    json!({"inputTokens": inp, "cachedInputTokens": cached, "outputTokens": out,
           "totalTokens": inp + out, "reasoningOutputTokens": out, "cacheWriteInputTokens": 0})
}

fn answering(text: &'static str) -> Ask {
    eve_codex_computer::ask_fn(move |_q| async move { text.to_string() })
}

fn never_asked() -> Ask {
    Arc::new(|q| Box::pin(async move { panic!("unexpected question: {q}") }))
}

fn collect() -> (Emit, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(vec![]));
    let s = seen.clone();
    (emit_fn(move |t| {
        s.lock().push(t);
        async {}
    }), seen)
}

fn ignore() -> Emit {
    emit_fn(|_| async {})
}

/// Lets the reader and the chat's background tasks catch up (Python's `await asyncio.sleep(0)`).
async fn settle() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

fn browser_result() -> Value {
    use base64::Engine as _;
    let mut png = vec![];
    image::RgbImage::from_pixel(16, 12, image::Rgb([0, 0, 255]))
        .write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    json!({"content": [
        {"type": "text", "text": "EVE_BROWSER_CAPTURE {\"tabId\":\"7\"}"},
        {"type": "text", "text": "Browser tab: 7, Title: \"Example\", URL: \"https://example.com/\".\n0 AXWebArea Example"},
        {"type": "image", "mimeType": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(&png)},
    ]})
}

// ---- test_codex_chat.py ----------------------------------------------------------------------------------------

#[test]
fn test_accessible_folders_come_from_saved_projects_not_tasks() {
    let tmp = tempfile::tempdir().unwrap();
    let base = eve_codex_auth::resolve(tmp.path());
    let (first, other) = (base.join("eve"), base.join("My Notes"));
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&other).unwrap();
    let state = json!({"local-projects": {"a": {"rootPaths": [first.to_str().unwrap(), other.to_str().unwrap()]},
                                          "b": {"rootPaths": [first.to_str().unwrap(), "/no/such/folder", "relative"]}},
                       "thread-project-assignments": {"guardian": "/private/internal"}});
    let catalog = base.join(".codex-global-state.json");
    std::fs::write(&catalog, serde_json::to_string(&state).unwrap()).unwrap();
    let before = std::fs::read(&catalog).unwrap();
    assert_eq!(accessible_folders(Some(&base)).unwrap(), vec![first.clone(), other.clone()]);
    assert_eq!(folder_menu(&accessible_folders(Some(&base)).unwrap()), "eve\nMy Notes");
    assert_eq!(std::fs::read(&catalog).unwrap(), before);
    let f = first.to_str().unwrap();
    for value in ["eve".to_string(), "EVE".to_string(), format!("{f}/"), format!("\"{f}\"")] {
        assert_eq!(select_folder(&[first.clone(), other.clone()], &value).unwrap(), first, "{value}");
    }
    let duplicate = base.join("elsewhere").join("eve");
    let err = select_folder(&[first.clone(), duplicate.clone()], "eve").unwrap_err();
    assert!(err.to_string().contains("full path"), "{err}");
    assert_eq!(folder_menu(&[first.clone(), duplicate.clone()]), format!("{f}\n{}", duplicate.to_str().unwrap()));
    let err = select_folder(std::slice::from_ref(&first), "1").unwrap_err();
    assert!(err.to_string().contains("not available"), "{err}");
}

#[tokio::test]
async fn test_new_thread_reused_for_followups_and_recreated_on_selection() {
    let rig = Rig::fresh();
    let folder = rig.folder();
    let (emit, replies) = collect();
    let chat = rig.chat(never_asked());
    chat.start(&folder, emit.clone(), None, None).await.unwrap();
    assert!(chat.connected() && !chat.running());
    chat.send("first prompt").await.unwrap();
    chat.wait_turn().await.unwrap();
    chat.send("follow up").await.unwrap();
    chat.wait_turn().await.unwrap();
    for (_, _, env) in rig.launches.lock().iter() {
        assert!(!env.contains_key("CODEX_APP_TOOLS_PIPE_PATH"));
    }
    let first = rig.processes.lock()[0].clone();
    let requests = first.requests();
    let start = requests.iter().find(|m| m["method"] == "thread/start").unwrap()["params"].clone();
    assert!(start["ephemeral"] == json!(false) && start["cwd"] == json!(folder.to_str().unwrap()));
    assert!(start["sandbox"] == "workspace-write" && start["approvalPolicy"] == "on-request");
    assert!(start.get("model").is_none());
    assert_eq!(start["developerInstructions"], json!(CHAT_INSTRUCTIONS.as_str()));
    let turns: Vec<&Value> = requests.iter().filter(|m| m["method"] == "turn/start").map(|m| &m["params"]).collect();
    assert!(turns.len() == 2 && turns.iter().all(|t| t["threadId"] == "thread"));
    let got = replies.lock().clone();
    assert_eq!(got.iter().filter(|t| *t == "Calculator displays 4.").count(), 2);
    assert!(!got.iter().any(|t| t.contains("UI-state"))); // coding/chat need no UI proof
    chat.start(&folder, emit, None, None).await.unwrap();
    assert!(rig.processes.lock().len() == 2 && first.returncode() == Some(0));
    assert!(!rig.processes.lock().iter().any(|p| p.count("thread/resume") > 0));
    chat.close().await;
    assert!(!chat.connected() && rig.processes.lock().last().unwrap().returncode() == Some(0));
}

#[tokio::test]
async fn test_active_followup_steers_and_stop_keeps_chat_usable() {
    let rig = Rig::new(Fake::chat("hold"), AuthMode::Plan);
    let p = rig.fake();
    let (emit, replies) = collect();
    let chat = rig.chat(answering("no"));
    chat.start(&rig.folder(), emit, None, None).await.unwrap();
    chat.send("work").await.unwrap();
    chat.send("correction").await.unwrap();
    assert!(p.count("turn/steer") > 0);
    chat.interrupt().await.unwrap();
    chat.wait_turn().await.unwrap();
    assert!(chat.connected() && !chat.running() && replies.lock().iter().any(|t| t == "Codex stopped."));
    chat.send("next").await.unwrap();
    chat.close().await;
    assert!(p.count("turn/interrupt") >= 2);
    assert!(!chat.running());
}

#[tokio::test]
async fn test_missing_folder_and_disconnection_never_retry() {
    let rig = Rig::new(Fake::chat("disconnect"), AuthMode::Plan);
    let p = rig.fake();
    let chat = CodexChat::new(answering("no"), Options { rpc_timeout: 0.1, ..rig.options() });
    let err = chat.start(&rig.tmp.path().join("missing"), ignore(), None, None).await.unwrap_err();
    assert!(err.to_string().contains("accessible"), "{err}");
    // Allow bootstrap responses, then drop at turn/start.
    p.set_mode("hold");
    chat.start(&rig.folder(), ignore(), None, None).await.unwrap();
    p.set_mode("disconnect");
    let err = chat.send("work").await.unwrap_err();
    assert!(err.to_string().contains("not retried"), "{err}");
    assert!(!chat.connected());
    assert_eq!(p.count("turn/start"), 1);
    chat.close().await;
}

#[tokio::test]
async fn test_browser_capture_callback_uses_current_chat_result() {
    let rig = Rig::new(Fake::chat("hold"), AuthMode::Plan);
    let p = rig.fake();
    let chat = rig.chat(answering("no"));
    let pictures: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(vec![]));
    let (shot, seen) = (chat.clone(), pictures.clone());
    let picture = picture_fn(move || {
        seen.lock().push(shot.browser_screenshot().expect("a verified picture").data.clone());
        async {}
    });
    chat.start(&rig.folder(), ignore(), Some(picture), None).await.unwrap();
    chat.send("browse").await.unwrap();
    p.event("item/completed", json!({"item": {"id": "capture", "type": "mcpToolCall", "server": "cua_repl",
        "tool": "js", "status": "completed", "result": browser_result()}}));
    p.event("turn/completed", json!({"turn": {"id": "turn", "status": "completed"}}));
    chat.wait_turn().await.unwrap();
    {
        let pictures = pictures.lock();
        assert!(pictures.len() == 1 && pictures[0].starts_with(b"\x89PNG"));
    }
    chat.send("ordinary question").await.unwrap();
    assert!(chat.browser_screenshot().is_none() && !chat.browser_used());
    chat.close().await;
}

/// A started chat on a held fake, for calling `answer_request` directly (Python sets `thread_id` and patches
/// `_send`; here the fake records what is sent).
async fn started(ask: Ask) -> (Rig, CodexChat, Arc<Fake>) {
    let rig = Rig::new(Fake::chat("hold"), AuthMode::Plan);
    let p = rig.fake();
    let chat = rig.chat(ask);
    chat.start(&rig.folder(), ignore(), None, None).await.unwrap();
    (rig, chat, p)
}

fn sent_with_id(p: &Fake, id: i64) -> Vec<Value> {
    p.requests().into_iter().filter(|m| m.get("id") == Some(&json!(id)) && m.get("method").is_none()).collect()
}

#[tokio::test]
async fn test_command_permission_has_exact_scope() {
    for (answer, decision) in [("yes", "accept"), ("yes but not yet", "decline"), ("no", "decline")] {
        let questions = Arc::new(Mutex::new(Vec::<String>::new()));
        let q = questions.clone();
        let ask = eve_codex_computer::ask_fn(move |question| {
            q.lock().push(question);
            async move { answer.to_string() }
        });
        let (_rig, chat, p) = started(ask).await;
        chat.answer_request(json!({"id": 123, "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "thread", "command": "git status", "cwd": "/project"}}))
            .await;
        let questions = questions.lock().clone();
        assert!(!questions.is_empty() && questions[0].contains("git status") && questions[0].contains("/project"));
        assert_eq!(sent_with_id(&p, 123), vec![json!({"id": 123, "result": {"decision": decision}})], "{answer}");
        chat.close().await;
    }
}

/// Only OSError and TimeoutError were caught: any other failure to ask left Codex waiting on the approval until
/// max_secs, with no decision sent.
#[tokio::test]
async fn test_a_broken_question_still_answers_the_approval_with_decline() {
    let ask: Ask = Arc::new(|_q| Box::pin(async { Err(anyhow::anyhow!("telegram said 400")) }));
    let (_rig, chat, p) = started(ask).await;
    chat.answer_request(json!({"id": 7, "method": "item/commandExecution/requestApproval",
        "params": {"threadId": "thread", "command": "rm -rf build", "cwd": "/project"}}))
        .await;
    assert_eq!(sent_with_id(&p, 7), vec![json!({"id": 7, "result": {"decision": "decline"}})]);
    chat.close().await;
}

#[tokio::test]
async fn test_stop_clears_a_pending_permission_question() {
    struct SetOnDrop(Arc<Notify>, Arc<Mutex<bool>>);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            *self.1.lock() = true;
            self.0.notify_one();
        }
    }
    let rig = Rig::new(Fake::chat("approval"), AuthMode::Plan);
    let p = rig.fake();
    let asking = Arc::new(Notify::new());
    let cancelled = Arc::new(Mutex::new(false));
    let (a, c) = (asking.clone(), cancelled.clone());
    let ask: Ask = Arc::new(move |_q| {
        let (a, c) = (a.clone(), c.clone());
        Box::pin(async move {
            a.notify_one();
            let _finally = SetOnDrop(Arc::new(Notify::new()), c);
            std::future::pending::<()>().await;
            Ok(String::new())
        })
    });
    let chat = rig.chat(ask);
    chat.start(&rig.folder(), ignore(), None, None).await.unwrap();
    chat.send("work").await.unwrap();
    asking.notified().await;
    chat.interrupt().await.unwrap();
    chat.wait_turn().await.unwrap();
    assert!(*cancelled.lock() && chat.connected() && !chat.running());
    assert!(!p.requests().iter().any(|m| m.get("id") == Some(&json!(100))));
    chat.close().await;
}

#[tokio::test]
async fn test_the_turn_result_goes_to_done_when_the_caller_tells_it_from_progress() {
    let rig = Rig::fresh();
    let (emit, steps) = collect();
    let (done, results) = collect();
    let chat = rig.chat(never_asked());
    chat.start(&rig.folder(), emit, None, Some(done)).await.unwrap();
    chat.send("first prompt").await.unwrap();
    chat.wait_turn().await.unwrap();
    assert_eq!(*results.lock(), vec!["Calculator displays 4.".to_string()]);
    assert!(!steps.lock().iter().any(|t| t == "Calculator displays 4.")); // the result is not also a step
    chat.close().await;
}

/// Review finding: a step's delivery was a background job that _finish_turn cancelled (the step was lost) or left
/// running (it arrived after the result).
#[tokio::test]
async fn test_a_step_emitted_just_before_the_turn_ends_arrives_before_the_result() {
    let order: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::new(Mutex::new(vec![]));
    let o = order.clone();
    let emit = emit_fn(move |text| {
        let o = o.clone();
        async move {
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            o.lock().push(("step", text));
        }
    });
    let o = order.clone();
    let done = emit_fn(move |text| {
        o.lock().push(("result", text));
        async {}
    });
    let chat = CodexChat::new(never_asked(), Options { binary: Some("codex".into()), ..Options::default() });
    {
        let mut st = chat.inner.st.lock();
        st.output = Some(emit);
        st.result = Some(done);
    }
    chat.set_final_text("All done.");
    let rx = chat.new_done();
    chat.event(&AgentEvent::with_text("progress", "Running the tests"));
    chat.complete_turn(json!({"status": "completed"}));
    assert!(chat.inner.clone().finish_turn(rx).await.is_none());
    assert_eq!(
        *order.lock(),
        vec![("step", "Running the tests".to_string()), ("result", "All done.".to_string())]
    );
}

// ---- test_codex_api_metering.py (chat cases) -------------------------------------------------------------------

#[tokio::test]
async fn test_chat_prices_deltas_and_ignores_old_or_unrelated_events() {
    let p = Fake::metered(MeterCfg {
        totals: vec![vec![usage(1000, 200, 100)], vec![usage(4000, 500, 300)]],
        hold: true,
        ..MeterCfg::default()
    });
    let rig = Rig::new(p.clone(), AuthMode::Api);
    let chat = rig.chat(answering("no"));
    chat.start(&rig.folder(), ignore(), None, None).await.unwrap();
    chat.send("first").await.unwrap();
    assert!(rig.rows().is_empty());
    p.finish("completed");
    chat.wait_turn().await.unwrap();
    // The auth mode is the launch's: a later environment change does not reach this process.
    rig.launch_env.lock().1 = AuthMode::Plan;
    chat.send("second").await.unwrap();
    chat.send("correction").await.unwrap();
    p.token_event(usage(99000, 0, 0), Some("turn-1"), "thread");
    p.token_event(usage(99000, 0, 0), None, "another-thread");
    p.event("turn/completed", json!({"turn": {"id": "turn-1", "status": "completed"}}));
    settle().await;
    assert!(chat.running() && !chat.done_finished());
    p.token_event(usage(4000, 500, 300), None, "thread");
    p.finish("completed");
    chat.wait_turn().await.unwrap();
    chat.close().await;
    let rows = rig.rows();
    assert_eq!(rows.len(), 2);
    let (first, second) = (&rows[0], &rows[1]);
    assert!((first["usd"].as_f64().unwrap() - 0.00264).abs() < 1e-9, "{first:?}");
    assert_eq!(second["tok"], json!({"in": 3000, "cached": 300, "out": 200}));
    assert!((second["usd"].as_f64().unwrap() - 0.00746).abs() < 1e-9, "{second:?}");
    assert_eq!(second["note"], "API key, billed per token (chat turn)");
    assert_eq!(p.count("turn/start"), 2);
}

#[tokio::test]
async fn test_cancellation_collects_interrupt_usage_and_records_once() {
    let p = Fake::metered(MeterCfg { hold: true, interrupt_total: Some(usage(1000, 100, 20)), ..MeterCfg::default() });
    let rig = Rig::new(p.clone(), AuthMode::Api);
    let chat = rig.chat(answering("no"));
    chat.start(&rig.folder(), ignore(), None, None).await.unwrap();
    chat.send("work").await.unwrap();
    chat.close().await;
    chat.close().await;
    let rows = rig.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!((rows[0]["usd"].as_f64().unwrap() - 0.00202).abs() < 1e-9);
    assert_eq!(rows[0]["tok"], json!({"in": 1000, "cached": 100, "out": 20}));
}

#[tokio::test]
async fn test_interrupt_ack_without_completion_never_prices_partial_usage() {
    let p = Fake::metered(MeterCfg {
        totals: vec![vec![usage(100, 0, 0)]],
        hold: true,
        interrupt_completed: false,
        ..MeterCfg::default()
    });
    let rig = Rig::new(p.clone(), AuthMode::Api);
    let chat = rig.chat(answering("no"));
    chat.start(&rig.folder(), ignore(), None, None).await.unwrap();
    chat.send("work").await.unwrap();
    chat.close().await;
    let rows = rig.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0]["usd"].is_null() && eve_vault::pystr::truthy(&rows[0]["unpriced"]));
    assert_eq!(rows[0]["tok"], json!({"in": 100}));
    assert!(rows[0]["note"].as_str().unwrap().contains("incomplete usage"));
}

#[tokio::test]
async fn test_chat_missing_or_reset_counter_does_not_charge_previous_turns() {
    type Case = (Vec<Vec<Value>>, Vec<Option<f64>>);
    let cases: [Case; 2] = [
        (vec![vec![], vec![usage(200, 0, 0)], vec![usage(300, 0, 0)]], vec![None, None, Some(0.0002)]),
        (vec![vec![usage(200, 0, 0)], vec![usage(100, 0, 0)], vec![usage(300, 0, 0)]], vec![Some(0.0004), None, Some(0.0004)]),
    ];
    for (totals, expected) in cases {
        let turns = totals.len();
        let p = Fake::metered(MeterCfg { totals, ..MeterCfg::default() });
        let rig = Rig::new(p, AuthMode::Api);
        let chat = rig.chat(answering("no"));
        chat.start(&rig.folder(), ignore(), None, None).await.unwrap();
        for _ in 0..turns {
            chat.send("work").await.unwrap();
            chat.wait_turn().await.unwrap();
        }
        chat.close().await;
        let got: Vec<Option<f64>> = rig.rows().iter().map(|r| r["usd"].as_f64()).collect();
        assert_eq!(got.len(), expected.len());
        for (g, e) in got.iter().zip(&expected) {
            match (g, e) {
                (Some(g), Some(e)) => assert!((g - e).abs() < 1e-12, "{got:?} vs {expected:?}"),
                (None, None) => {}
                _ => panic!("{got:?} vs {expected:?}"),
            }
        }
    }
}

#[tokio::test]
async fn test_rerouted_models_are_priced_only_when_usage_is_unambiguous() {
    for (prior_usage, expected) in [(false, Some(0.01)), (true, None)] {
        let totals = if prior_usage { vec![vec![usage(100, 0, 0)]] } else { vec![] };
        let p = Fake::metered(MeterCfg { totals, hold: true, ..MeterCfg::default() });
        let rig = Rig::new(p.clone(), AuthMode::Api);
        let chat = rig.chat(answering("no"));
        chat.start(&rig.folder(), ignore(), None, None).await.unwrap();
        chat.send("work").await.unwrap();
        p.event("model/rerouted", json!({"turnId": p.turn_id(), "fromModel": "gpt-6-sol",
                                         "toModel": "gpt-6-astra", "reason": "test"}));
        p.token_event(usage(1000, 0, 0), None, "thread");
        p.finish("completed");
        chat.wait_turn().await.unwrap();
        chat.close().await;
        let rows = rig.rows();
        assert_eq!(rows.len(), 1);
        match expected {
            Some(e) => assert!((rows[0]["usd"].as_f64().unwrap() - e).abs() < 1e-12, "{rows:?}"),
            None => assert!(rows[0]["usd"].is_null(), "{rows:?}"),
        }
    }
}

#[tokio::test]
async fn plan_turns_are_counted_unpriced_before_the_turn_runs() {
    let rig = Rig::new(Fake::chat("hold"), AuthMode::Plan);
    let chat = rig.chat(answering("no"));
    chat.start(&rig.folder(), ignore(), None, None).await.unwrap();
    chat.send("work").await.unwrap();
    let rows = rig.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!((&rows[0]["p"], &rows[0]["usd"], &rows[0]["note"]),
               (&json!("chatgpt"), &Value::Null, &json!("ChatGPT plan, not per-call (chat turn)")));
    chat.close().await;
    assert_eq!(rig.rows().len(), 1);
}

// ---- test_codex_permissions.py (chat cases) --------------------------------------------------------------------

fn request() -> Value {
    json!({"id": 10, "method": "mcpServer/elicitation/request", "params": {
        "threadId": "thread", "serverName": "cua_repl", "mode": "form",
        "message": "Allow Computer Use to use \"Calculator\"?",
        "requestedSchema": {"type": "object", "properties": {}},
        "_meta": {"codex_approval_kind": "mcp_tool_call", "connector_id": "computer-use",
                  "persist": ["session", "always"], "tool_name": "get_app_state",
                  "tool_params": {"app": "com.apple.calculator"}, "riskLevel": "low"}}})
}

/// `answer(text, msg, routine_permissions=..., chat=True)`: the reply sent, and the questions asked.
async fn answer(text: &'static str, msg: Value, routine_permissions: &str) -> (Value, Vec<String>) {
    let rig = Rig::new(Fake::chat("hold"), AuthMode::Plan);
    let p = rig.fake();
    let questions = Arc::new(Mutex::new(Vec::<String>::new()));
    let q = questions.clone();
    let ask = eve_codex_computer::ask_fn(move |question| {
        q.lock().push(question);
        async move { text.to_string() }
    });
    let chat = CodexChat::new(ask, Options {
        site_access: Some("ask".into()),
        routine_permissions: Some(routine_permissions.into()),
        ..rig.options()
    });
    chat.start(&rig.folder(), ignore(), None, None).await.unwrap();
    let id = msg["id"].as_i64().unwrap();
    chat.answer_request(msg).await;
    let reply = sent_with_id(&p, id).into_iter().next().expect("a reply");
    chat.close().await;
    let questions = questions.lock().clone();
    (reply, questions)
}

#[tokio::test]
async fn test_routine_app_access_does_not_interrupt_chat_or_call_tasks() {
    for tool in APP_ACCESS_TOOLS {
        let mut msg = request();
        let meta = msg["params"]["_meta"].as_object_mut().unwrap();
        meta.insert("tool_name".into(), json!(tool));
        meta.insert("riskLevel".into(), json!("high"));
        let (reply, questions) = answer("no", msg.clone(), "allow").await;
        assert_eq!(reply["result"], json!({"action": "accept", "content": {}}), "{tool}");
        assert!(questions.is_empty());
        // The same request with the preference disabled must actually ask.
        let (reply, questions) = answer("no", msg, "ask").await;
        assert!(!questions.is_empty() && reply["result"]["action"] == "decline");
    }
}

#[test]
fn test_both_agents_receive_owner_action_confirmation_policy() {
    assert!(CHAT_INSTRUCTIONS.contains(OWNER_ACTION_INSTRUCTIONS));
    assert!(CHAT_INSTRUCTIONS.contains(BROWSER_INSTRUCTIONS));
    assert!(CHAT_INSTRUCTIONS.starts_with("The owner is chatting with you through Eve on Telegram.\n"));
    assert!(CHAT_INSTRUCTIONS.contains("ChatGPT's native UI is blocked by Computer Use; do not bypass that restriction.\n"));
}

// ---- parity additions ------------------------------------------------------------------------------------------

#[tokio::test]
async fn file_changes_and_bare_commands_are_declined_with_a_note() {
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let (rig, chat, p) = started(never_asked()).await;
    let ev = events.clone();
    chat.inner.st.lock().output = Some(emit_fn(move |t| {
        ev.lock().push(t);
        async {}
    }));
    chat.answer_request(json!({"id": 21, "method": "item/fileChange/requestApproval",
        "params": {"threadId": "thread", "itemId": "x"}})).await;
    chat.answer_request(json!({"id": 22, "method": "item/commandExecution/requestApproval",
        "params": {"threadId": "thread", "command": ""}})).await;
    // Another thread, or a stopped chat, is never asked either.
    chat.answer_request(json!({"id": 23, "method": "item/commandExecution/requestApproval",
        "params": {"threadId": "other", "command": "ls"}})).await;
    // A reason that is not text cannot be put in the question: declined without asking.
    chat.answer_request(json!({"id": 24, "method": "item/commandExecution/requestApproval",
        "params": {"threadId": "thread", "command": "ls", "reason": 5}})).await;
    // A params that is not an object raises before answering: nothing is sent.
    chat.answer_request(json!({"id": 25, "method": "item/commandExecution/requestApproval", "params": []})).await;
    // `msg['method']` and `msg['id']` raise on a message without them: nothing is sent.
    chat.answer_request(json!({"id": 26, "params": {"threadId": "thread", "command": "ls"}})).await;
    chat.answer_request(json!({"method": "item/commandExecution/requestApproval",
        "params": {"threadId": "thread", "command": "ls", "reason": 5}})).await;
    chat.answer_request(json!(["method", "id"])).await;
    for id in [21, 22, 23, 24] {
        assert_eq!(sent_with_id(&p, id), vec![json!({"id": id, "result": {"decision": "decline"}})], "{id}");
    }
    assert!(sent_with_id(&p, 25).is_empty());
    assert!(sent_with_id(&p, 26).is_empty());
    assert!(!p.requests().iter().any(|m| m.get("method").is_none() && m.get("id").is_none_or(Value::is_null)));
    settle().await;
    let note = "Codex requested permission without enough detail to review here. \
                It was declined; no permission was changed.";
    assert_eq!(events.lock().iter().filter(|t| *t == note).count(), 2);
    chat.close().await;
    drop(rig);
}

#[tokio::test]
async fn the_question_names_the_chat_folder_and_the_reason() {
    let questions = Arc::new(Mutex::new(Vec::<String>::new()));
    let q = questions.clone();
    let ask = eve_codex_computer::ask_fn(move |question| {
        q.lock().push(question);
        async { "Approve!".to_string() }
    });
    let (rig, chat, p) = started(ask).await;
    chat.answer_request(json!({"id": 31, "method": "item/commandExecution/requestApproval",
        "params": {"threadId": "thread", "command": "make", "reason": "build it"}})).await;
    assert_eq!(
        questions.lock()[0],
        format!("Codex asks to run this command in {}:\nmake\nbuild it\nReply yes or no.", rig.folder().to_str().unwrap())
    );
    assert_eq!(sent_with_id(&p, 31), vec![json!({"id": 31, "result": {"decision": "accept"}})]);
    chat.close().await;
}

#[tokio::test]
async fn messages_need_a_connected_chat_and_interrupt_needs_a_turn() {
    let rig = Rig::new(Fake::chat("hold"), AuthMode::Plan);
    let chat = rig.chat(answering("no"));
    assert_eq!(chat.send("hi").await.unwrap_err().to_string(), "Codex disconnected. Send codex <folder> to start a new chat.");
    chat.start(&rig.folder(), ignore(), None, None).await.unwrap();
    assert_eq!(chat.interrupt().await.unwrap_err().to_string(), "Nothing is running in this Codex chat.");
    chat.close().await;
    assert!(chat.thread_id().is_null() && !chat.connected());
    assert_eq!(chat.send("hi").await.unwrap_err().to_string(), "Codex disconnected. Send codex <folder> to start a new chat.");
}

#[tokio::test]
async fn a_thread_in_another_folder_is_closed() {
    // The same folder reached through a symlink resolves to the thread's folder.
    let rig = Rig::new(Fake::chat("hold"), AuthMode::Plan);
    let link = rig.tmp.path().join("link");
    std::os::unix::fs::symlink(rig.folder(), &link).unwrap();
    let chat = rig.chat(answering("no"));
    chat.start(&link, ignore(), None, None).await.unwrap();
    assert!(chat.connected());
    chat.close().await;
    // A server that opens a different folder.
    let rig = Rig::new(Fake::chat("hold"), AuthMode::Plan);
    let p = rig.fake();
    let wrong = rig.tmp.path().join("wrong");
    std::fs::create_dir(&wrong).unwrap();
    let (spawner, wrong) = (rig.spawner(), wrong.to_str().unwrap().to_string());
    let redirect: Spawner = Arc::new(move |prog, args, env| {
        let (fut, wrong) = (spawner(prog, args, env), wrong.clone());
        Box::pin(async move {
            let mut s = fut.await?;
            s.stdin = Box::new(Rewrite(s.stdin, wrong));
            Ok(s)
        })
    });
    let chat = CodexChat::new(answering("no"), Options { spawner: Some(redirect), ..rig.options() });
    let err = chat.start(&rig.folder(), ignore(), None, None).await.unwrap_err();
    assert_eq!(err.to_string(), "Could not start a fresh Codex chat. Eve is still available.");
    assert!(!chat.connected() && p.returncode() == Some(0));
}

/// Rewrites the `cwd` of `thread/start` on its way to the fake (a server that opens another folder).
struct Rewrite(Box<dyn AsyncWrite + Send + Unpin>, String);

impl AsyncWrite for Rewrite {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        let mut msg: Value = match serde_json::from_slice(data.strip_suffix(b"\n").unwrap_or(data)) {
            Ok(m) => m,
            Err(_) => return Pin::new(&mut self.0).poll_write(cx, data),
        };
        if msg["method"] == "thread/start" {
            msg["params"]["cwd"] = json!(self.1.clone());
        }
        let line = format!("{}\n", serde_json::to_string(&msg).unwrap());
        match Pin::new(&mut self.0).poll_write(cx, line.as_bytes()) {
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(data.len())),
            other => other,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

#[test]
fn folder_menu_drops_debrief_folders_and_paths_print_as_pathlib_does() {
    let folders = vec![PathBuf::from("/a/eve"), PathBuf::from("/b/.Debrief/x"), PathBuf::from("/c/debrief"),
                       PathBuf::from("/d/Eve")];
    assert_eq!(folder_menu(&folders), "/a/eve\n/d/Eve");
    assert_eq!(path_str("//net//x/./y/"), "//net/x/y");
    assert_eq!(path_str("///x"), "/x");
    assert_eq!(path_str(""), ".");
    assert_eq!(casefold("Straße"), "strasse");
}

#[test]
fn select_folder_expands_the_home_and_reads_the_root() {
    let folders = vec![PathBuf::from("/Users/me/eve"), PathBuf::from("/")];
    assert_eq!(select_folder_in(&folders, "  ~/eve/ ", Some("/Users/me/")).unwrap(), folders[0]);
    assert_eq!(select_folder_in(&folders, "'/'", Some("/Users/me")).unwrap(), folders[1]);
    assert_eq!(select_folder_in(&folders, "\"\"", Some("/Users/me")).unwrap(), folders[1]);
    let err = select_folder_in(&folders, "~no-such-user-for-eve-tests/eve", Some("/Users/me")).unwrap_err();
    assert_eq!(err, CodexError::Runtime("Could not determine home directory.".into()));
    assert_eq!(expanduser_str("~", Some("")).unwrap(), "/");
}

#[test]
fn an_unreadable_or_malformed_catalog_is_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let unavailable = CodexError::Unavailable("Could not read saved Codex folders.".into());
    assert_eq!(accessible_folders(Some(tmp.path())).unwrap_err(), unavailable);
    let catalog = tmp.path().join(".codex-global-state.json");
    for (text, expected) in [("{", Some(unavailable.clone())), ("{\"local-projects\": []}", Some(unavailable.clone())),
                             ("{\"local-projects\": null}", Some(unavailable.clone())), ("{}", None),
                             ("{\"local-projects\": {\"a\": 1, \"b\": {\"rootPaths\": \"/x\"}}}", None)] {
        std::fs::write(&catalog, text).unwrap();
        match expected {
            Some(e) => assert_eq!(accessible_folders(Some(tmp.path())).unwrap_err(), e, "{text}"),
            None => assert!(accessible_folders(Some(tmp.path())).unwrap().is_empty(), "{text}"),
        }
    }
    std::fs::write(&catalog, "[]").unwrap();
    assert_eq!(accessible_folders(Some(tmp.path())).unwrap_err().class_name(), "AttributeError");
}
