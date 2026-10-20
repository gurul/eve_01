//! The worker seam and the desktop worker client (Python's `WorkerClient`, plus the duck-typed worker the agent
//! drives: the desktop worker for the Mac lane, the browser lane for a web goal, fakes in tests).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::py::{self, truthy, value_repr, value_str};
use crate::{
    runtime, FailSafe, PyError, WorkerDead, DEFAULT_EXEC_TIMEOUT_SECS, LANE_FIRST_TIMEOUT_SECS,
    OUTLINE_TIMEOUT_SECS, RUN_PLAN_TIMEOUT_SECS, WORKER_LINE_LIMIT,
};

fn missing(name: &str) -> anyhow::Error {
    PyError::err("AttributeError", format!("this worker has no attribute {}", py::py_repr(name)))
}

/// What the agent drives: one desktop session (or the browser lane's page). Every method mirrors the Python
/// worker's coroutine of the same name; a method a worker does not have fails as Python's `AttributeError`.
#[async_trait]
pub trait Worker: Send + Sync {
    /// The ready line, `{"ready": true, ...}`.
    async fn start(&self) -> anyhow::Result<Value> {
        Err(missing("start"))
    }

    /// The context line plus a screenshot (the first turn's input).
    async fn observe(&self) -> anyhow::Result<Vec<Value>> {
        Err(missing("observe"))
    }

    /// Run one exec_py step: the model-facing output list.
    async fn execute(&self, _code: &str) -> anyhow::Result<Vec<Value>> {
        Err(missing("execute"))
    }

    /// Kill the session and release every held key and button. Never fails.
    async fn close(&self) {}

    /// The router's attempt at the goal; `None` for a worker with no router (Python's missing attribute).
    async fn lane_first(&self, _goal: &str) -> Option<anyhow::Result<Value>> {
        None
    }

    /// The front window's controls as the planner is shown them: `{"app", "lines", ...}`.
    async fn outline(&self) -> anyhow::Result<Value> {
        Err(missing("outline"))
    }

    /// Walk a plan: PlanResult.to_dict().
    async fn run_plan(
        &self,
        _plan: &Value,
        _request: &str,
        _start: i64,
        _approved: &IndexMap<String, String>,
        _success_rejected: bool,
    ) -> anyhow::Result<Value> {
        Err(missing("run_plan"))
    }

    /// The page's title, URL and visible text (the browser lane).
    async fn page_text(&self) -> anyhow::Result<Value> {
        Err(missing("page_text"))
    }

    /// Open a URL (the browser lane): its sentence.
    async fn open_url(&self, _url: &str) -> anyhow::Result<String> {
        Err(missing("open_url"))
    }

    /// The worker's local ms per sense from the last reply, when it reports them.
    fn last_timing(&self) -> Option<Value> {
        None
    }
}

/// The input release pass, as a seam (`release=` in Python).
pub type ReleaseFn = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// The desktop worker binary: `eve-desktop-worker` beside the running executable, else on PATH.
pub fn default_worker_program() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("eve-desktop-worker");
            if sibling.is_file() {
                return sibling;
            }
            // `cargo test` runs from target/<profile>/deps
            if let Some(up) = dir.parent() {
                let sibling = up.join("eve-desktop-worker");
                if sibling.is_file() {
                    return sibling;
                }
            }
        }
    }
    PathBuf::from("eve-desktop-worker")
}

struct Io {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// bytes of a line not yet complete: kept across a cancelled read so the stream never desyncs
    partial: Vec<u8>,
}

#[derive(Default)]
struct ClientState {
    ready: Value,
    restarts: u32,
    last_timing: Option<Value>,
    grace: f64,
    id: u64,
}

enum ExchangeError {
    Timeout,
    Py(anyhow::Error),
}

/// One desktop worker child. execute(code) -> the model-facing output list.
///
/// A stuck step (past `timeout_secs`) or a dead child restarts the session once, handing the model a fresh
/// observation; the second time fails with [`WorkerDead`]. Every close() ends with a `--release` pass in a
/// fresh process so nothing a killed worker held stays pressed.
pub struct WorkerClient {
    pub timeout_secs: f64,
    program: PathBuf,
    args: Vec<String>,
    release: Option<ReleaseFn>,
    child: Mutex<Option<Child>>,
    io: tokio::sync::Mutex<Option<Io>>,
    state: Mutex<ClientState>,
}

impl Default for WorkerClient {
    fn default() -> Self {
        WorkerClient::new(DEFAULT_EXEC_TIMEOUT_SECS)
    }
}

impl WorkerClient {
    /// `WorkerClient(timeout_secs=...)`: the desktop worker binary, no extra arguments, the real release pass.
    pub fn new(timeout_secs: f64) -> Self {
        WorkerClient {
            timeout_secs,
            program: default_worker_program(),
            args: Vec::new(),
            release: None,
            child: Mutex::new(None),
            io: tokio::sync::Mutex::new(None),
            state: Mutex::new(ClientState { ready: json!({}), ..ClientState::default() }),
        }
    }

    /// Another child program and its arguments (`python=` and `args=` in Python; tests run a fake child).
    pub fn with_program(mut self, program: impl Into<PathBuf>, args: Vec<String>) -> Self {
        self.program = program.into();
        self.args = args;
        self
    }

    /// Another release pass (`release=`).
    pub fn with_release(mut self, release: ReleaseFn) -> Self {
        self.release = Some(release);
        self
    }

    pub fn program(&self) -> &PathBuf {
        &self.program
    }

    /// The ready line the child printed.
    pub fn ready(&self) -> Value {
        self.state.lock().ready.clone()
    }

    pub fn restarts(&self) -> u32 {
        self.state.lock().restarts
    }

    /// The child's pid, `None` when no child is running (`proc is None`).
    pub fn pid(&self) -> Option<u32> {
        self.child.lock().as_ref().and_then(Child::id)
    }

    pub fn is_running(&self) -> bool {
        self.child.lock().is_some()
    }

    async fn spawn_and_ready(&self) -> anyhow::Result<Value> {
        // One reply line carries a base64 PNG of the whole screen (a few MB): the reader takes lines up to
        // WORKER_LINE_LIMIT. stderr is inherited so the child's tracebacks land in the daemon log.
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| runtime("desktop worker has no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| runtime("desktop worker has no stdout"))?;
        *self.child.lock() = Some(child);
        let mut io = Io { stdin, stdout: BufReader::new(stdout), partial: Vec::new() };
        let first = tokio::time::timeout(Duration::from_secs(30), read_line(&mut io)).await;
        let line = match first {
            Err(_) => {
                self.close().await;
                return Err(runtime("desktop worker failed to start: no ready line within 30 s"));
            }
            Ok(Err(e)) => {
                self.close().await;
                return Err(e);
            }
            Ok(Ok(line)) => line,
        };
        let text = match &line {
            Some(l) if !l.is_empty() => String::from_utf8_lossy(l).into_owned(),
            _ => "{}".to_string(),
        };
        let msg: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                self.close().await;
                return Err(e.into());
            }
        };
        if !msg.get("ready").is_some_and(truthy) {
            let err = msg
                .get("error")
                .and_then(|e| e.get("message"))
                .filter(|m| truthy(m))
                .map(value_str)
                .unwrap_or_else(|| "worker did not report ready".to_string());
            self.close().await;
            return Err(runtime(format!("desktop worker failed to start: {err}")));
        }
        *self.io.lock().await = Some(io);
        self.state.lock().ready = msg.clone();
        Ok(msg)
    }

    /// One request, the reply with the matching id (stale replies from a timed-out request are skipped).
    /// `Ok(None)` when the child closed the pipe; `Timeout` past `timeout` — the io lock is held throughout so
    /// requests never interleave.
    async fn exchange(&self, body: &Map<String, Value>, timeout: f64) -> Result<Option<Value>, ExchangeError> {
        let mut guard = self.io.lock().await;
        let Some(io) = guard.as_mut() else {
            return Err(ExchangeError::Py(runtime("desktop worker is not running")));
        };
        let (rid, grace) = {
            let mut s = self.state.lock();
            s.id += 1;
            let g = s.grace;
            s.grace = 0.0;
            (s.id, g)
        };
        let mut req = Map::new();
        req.insert("id".into(), json!(rid));
        for (k, v) in body {
            req.insert(k.clone(), v.clone());
        }
        let line = py::dumps(&Value::Object(req)) + "\n";
        if let Err(e) = io.stdin.write_all(line.as_bytes()).await {
            return Err(ExchangeError::Py(e.into()));
        }
        if let Err(e) = io.stdin.flush().await {
            return Err(ExchangeError::Py(e.into()));
        }
        let deadline = Instant::now() + Duration::from_secs_f64((timeout + grace).max(0.0));
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(ExchangeError::Timeout);
            }
            let got = match tokio::time::timeout(deadline - now, read_line(io)).await {
                Err(_) => return Err(ExchangeError::Timeout),
                Ok(r) => r.map_err(ExchangeError::Py)?,
            };
            let Some(raw) = got else {
                return Ok(None);
            };
            let msg: Value = serde_json::from_slice(&raw).map_err(|e| ExchangeError::Py(e.into()))?;
            let Value::Object(m) = &msg else {
                return Err(ExchangeError::Py(PyError::err(
                    "AttributeError",
                    format!("{} object has no attribute 'get'", py::py_repr(json_type(&msg))),
                )));
            };
            let id = m.get("id");
            // the worker answers id-less only for a line it could not parse at all
            let mine = id == Some(&json!(rid)) || id.and_then(Value::as_f64) == Some(rid as f64);
            let unparsed = matches!(id, None | Some(Value::Null)) && m.contains_key("error");
            if mine || unparsed {
                if let Some(t @ Value::Object(_)) = m.get("timing") {
                    self.state.lock().last_timing = Some(t.clone());
                }
                return Ok(Some(msg));
            }
            tracing::debug!(
                "agent: skipping a stale worker reply (id {}, waiting for {})",
                id.map(value_str).unwrap_or_else(|| "None".into()),
                rid
            );
        }
    }

    async fn request(&self, body: Map<String, Value>, timeout: f64) -> anyhow::Result<Vec<Value>> {
        if !self.is_running() {
            return Err(runtime("desktop worker is not running"));
        }
        let msg = match self.exchange(&body, timeout).await {
            Err(ExchangeError::Timeout) => {
                return self.restart(&format!("exec_py exceeded its {timeout:.0} s deadline")).await;
            }
            Err(ExchangeError::Py(e)) => return Err(e),
            Ok(None) => return self.restart("the desktop helper stopped (see the daemon log)").await,
            Ok(Some(m)) => m,
        };
        if let Some(err) = msg.get("error").filter(|e| truthy(e)) {
            if err.get("code").and_then(Value::as_str) == Some("failsafe") {
                self.close().await;
                let message = err.get("message").map(value_str).unwrap_or_else(|| "fail-safe".into());
                return Err(anyhow::Error::new(FailSafe(message)));
            }
            let message = match err {
                Value::Object(m) => m.get("message").map(value_str).unwrap_or_else(|| value_repr(err)),
                other => value_str(other),
            };
            return Ok(vec![json!({"type": "input_text", "text": format!("exec_py error: {message}")})]);
        }
        match msg.get("output") {
            Some(Value::Array(items)) if !items.is_empty() => Ok(items.clone()),
            _ => Ok(vec![json!({"type": "input_text", "text": "exec_py completed with no output."})]),
        }
    }

    async fn restart(&self, reason: &str) -> anyhow::Result<Vec<Value>> {
        self.close().await;
        let n = {
            let mut s = self.state.lock();
            s.restarts += 1;
            s.restarts
        };
        if n > 1 {
            return Err(anyhow::Error::new(WorkerDead(format!("{reason}, twice"))));
        }
        tracing::warn!("agent: {}; restarting the desktop worker", reason);
        self.spawn_and_ready().await?;
        let items = Box::pin(self.request(op("observe"), 15.0)).await?;
        let mut out = vec![json!({"type": "input_text", "text": format!(
            "{reason}; the desktop session was restarted: your variables are gone. Fresh screenshot:")})];
        out.extend(items);
        Ok(out)
    }

    async fn soft(&self, body: Map<String, Value>, key: &str, timeout: f64, fallback: Value) -> Value {
        let with_reason = |reason: String| {
            let mut f = fallback.as_object().cloned().unwrap_or_default();
            f.insert("reason".into(), Value::String(reason));
            Value::Object(f)
        };
        if !self.is_running() {
            return with_reason("desktop worker is not running".into());
        }
        match self.exchange(&body, timeout).await {
            Err(ExchangeError::Timeout) => {
                self.state.lock().grace = timeout;
                with_reason("timeout".into())
            }
            Err(ExchangeError::Py(e)) => with_reason(py::take(&crate::name_and_text(&e), 200)),
            Ok(msg) => match msg.as_ref().and_then(|m| m.get(key)) {
                Some(v @ Value::Object(_)) => v.clone(),
                _ => with_reason("no reply".into()),
            },
        }
    }

    /// The router's attempt at the goal (desktop_worker.lane_first): RouteResult.to_dict(), or
    /// `{"status": "unavailable", "reason": …}`. It never restarts the worker: a slow or failed route means the
    /// planner does the task, and a stale reply is skipped by id.
    pub async fn lane_first_with(&self, goal: &str, timeout: f64) -> Value {
        if !self.is_running() {
            return json!({"status": "unavailable", "reason": "desktop worker is not running"});
        }
        let mut body = op("lane_first");
        body.insert("goal".into(), json!(goal));
        match self.exchange(&body, timeout).await {
            Err(ExchangeError::Timeout) => {
                self.state.lock().grace = LANE_FIRST_TIMEOUT_SECS;
                json!({"status": "unavailable", "reason": "timeout"})
            }
            Err(ExchangeError::Py(e)) => {
                json!({"status": "unavailable", "reason": py::take(&crate::name_and_text(&e), 200)})
            }
            Ok(msg) => match msg.as_ref().and_then(|m| m.get("lane_first")) {
                Some(v @ Value::Object(_)) => v.clone(),
                _ => json!({"status": "unavailable", "reason": "no route in the worker reply"}),
            },
        }
    }

    /// The front window's controls: `{"app", "lines"}`. Never restarts the worker: no outline means the planner
    /// plans from the request alone.
    pub async fn outline_with(&self, timeout: f64) -> Value {
        self.soft(op("outline"), "outline", timeout, json!({"app": "", "lines": []})).await
    }

    /// Walk a plan in the worker (desktop_worker.run_plan): PlanResult.to_dict(). A timeout or a dead pipe reads
    /// as `partial` with an unknown ledger — the turn-by-turn loop then looks for itself.
    pub async fn run_plan_with(
        &self,
        plan: &Value,
        request: &str,
        start: i64,
        approved: &IndexMap<String, String>,
        timeout: f64,
        success_rejected: bool,
    ) -> Value {
        let mut body = op("run_plan");
        body.insert("plan".into(), plan.clone());
        body.insert("request".into(), json!(request));
        body.insert("start".into(), json!(start));
        body.insert("approved".into(), json!(approved));
        body.insert("success_rejected".into(), json!(success_rejected));
        let fallback = json!({"status": "partial", "next_index": start, "ledger": [], "reason": "worker"});
        self.soft(body, "run_plan", timeout, fallback).await
    }

    /// Key-ups and mouse-ups for everything, in a fresh process (works when the worker is dead): the worker
    /// program and its arguments plus `--release` (Python: `python -m eve_bridge.desktop_worker --release`).
    async fn release_inputs(&self) -> anyhow::Result<()> {
        let mut p = Command::new(&self.program)
            .args(&self.args)
            .arg("--release")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        match tokio::time::timeout(Duration::from_secs(5), p.wait()).await {
            Err(_) => {
                let _ = p.start_kill();
                tracing::warn!("agent: input release timed out");
                Ok(())
            }
            Ok(Err(e)) => Err(e.into()),
            Ok(Ok(status)) => {
                if !status.success() {
                    tracing::warn!(
                        "agent: input release exited {} — a key or button may still be held",
                        returncode(&status)
                    );
                }
                Ok(())
            }
        }
    }
}

/// Python's `Popen.returncode`: the exit code, or minus the signal that ended it.
fn returncode(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return -sig;
        }
    }
    -1
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn op(name: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("operation".into(), json!(name));
    m
}

/// One line from the child (newline included in the limit, not in the result); `None` at EOF. Cancel-safe:
/// the bytes of an unfinished line stay in `io.partial`. A line past WORKER_LINE_LIMIT is a ValueError, as
/// asyncio's StreamReader raises it.
async fn read_line(io: &mut Io) -> anyhow::Result<Option<Vec<u8>>> {
    loop {
        if let Some(pos) = io.partial.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = io.partial.drain(..=pos).collect();
            line.pop();
            return Ok(Some(line));
        }
        if io.partial.len() > WORKER_LINE_LIMIT {
            io.partial.clear();
            return Err(PyError::err("ValueError", "Separator is not found, and chunk exceed the limit"));
        }
        let buf = io.stdout.fill_buf().await?;
        if buf.is_empty() {
            if io.partial.is_empty() {
                return Ok(None);
            }
            // asyncio's readline returns the partial last line at EOF
            return Ok(Some(std::mem::take(&mut io.partial)));
        }
        let n = buf.len();
        io.partial.extend_from_slice(buf);
        io.stdout.consume(n);
    }
}

#[async_trait]
impl Worker for WorkerClient {
    async fn start(&self) -> anyhow::Result<Value> {
        self.spawn_and_ready().await
    }

    async fn observe(&self) -> anyhow::Result<Vec<Value>> {
        self.request(op("observe"), 15.0).await
    }

    async fn execute(&self, code: &str) -> anyhow::Result<Vec<Value>> {
        let mut body = op("execute");
        body.insert("code".into(), json!(code));
        self.request(body, self.timeout_secs).await
    }

    async fn close(&self) {
        let taken = self.child.lock().take();
        if let Some(mut p) = taken {
            let _ = p.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), p.wait()).await;
        }
        if let Ok(mut io) = self.io.try_lock() {
            *io = None;
        }
        let released = match &self.release {
            Some(f) => f().await,
            None => self.release_inputs().await,
        };
        if let Err(e) = released {
            // releasing is best effort; never mask the real error
            tracing::warn!("agent: releasing keys and mouse buttons failed: {}", e);
        }
    }

    async fn lane_first(&self, goal: &str) -> Option<anyhow::Result<Value>> {
        Some(Ok(self.lane_first_with(goal, LANE_FIRST_TIMEOUT_SECS).await))
    }

    async fn outline(&self) -> anyhow::Result<Value> {
        Ok(self.outline_with(OUTLINE_TIMEOUT_SECS).await)
    }

    async fn run_plan(
        &self,
        plan: &Value,
        request: &str,
        start: i64,
        approved: &IndexMap<String, String>,
        success_rejected: bool,
    ) -> anyhow::Result<Value> {
        Ok(self.run_plan_with(plan, request, start, approved, RUN_PLAN_TIMEOUT_SECS, success_rejected).await)
    }

    fn last_timing(&self) -> Option<Value> {
        self.state.lock().last_timing.clone()
    }
}

/// The browser lane (eve's own Chromium, eve-browser-lane) as the agent's worker for a web goal: the outline,
/// the plan executor, the page text and a navigation. It has no desktop session: no observe, no exec.
pub struct BrowserLaneWorker(pub Arc<eve_browser_lane::BrowserLane>);

#[async_trait]
impl Worker for BrowserLaneWorker {
    async fn outline(&self) -> anyhow::Result<Value> {
        Ok(serde_json::to_value(self.0.outline().await?)?)
    }

    async fn run_plan(
        &self,
        plan: &Value,
        request: &str,
        start: i64,
        approved: &IndexMap<String, String>,
        success_rejected: bool,
    ) -> anyhow::Result<Value> {
        self.0.run_plan(plan.clone(), request, start.max(0) as usize, approved.clone(), success_rejected).await
    }

    async fn page_text(&self) -> anyhow::Result<Value> {
        Ok(serde_json::to_value(self.0.page_text(eve_browser_lane::PAGE_TEXT_LIMIT).await?)?)
    }

    async fn open_url(&self, url: &str) -> anyhow::Result<String> {
        self.0.open_url(url).await
    }
}
