//! claude-mem: eve's memories land in the owner's claude-mem the moment they form, and eve can
//! recall from it.
//!
//! claude-mem (github.com/thedotmack/claude-mem, v13.24.23 on this Mac) is the owner's Claude Code memory
//! plugin. Its worker is a local HTTP service; the port is whatever the plugin chose (37701 today), recorded
//! in `~/.claude-mem/settings.json` (CLAUDE_MEM_WORKER_PORT) and `~/.claude-mem/worker.pid`.
//!
//! Three pieces, all off the daemon's loop and none able to raise into it:
//!
//! * [`ClaudeMemSink`] subscribes to `/eve/memory/*` on the MemoryBus and POSTs each event to
//!   `/api/memory/save` from one worker task through a bounded queue. Project "eve", so a Claude
//!   Code session searching claude-mem sees what eve saw and taught. What was SAID is not on the bus
//!   (owner, 2026-09-23): it stays in eve's own memory folder and never reaches claude-mem.
//! * [`recall`] asks `GET /api/search` and returns a short list, newest first, empty on any error.
//! * [`ClaudeMemMirror`] reads the worker's `GET /stream` (server-sent events) and republishes every
//!   `new_observation` from the owner's other projects on `/claude/observation`, so a rosbridge client
//!   watching eve also sees the owner's Claude Code work land. eve's own project is never mirrored back.
//!
//! Verified routes (source: src/services/worker/http/routes/*.ts at v13.24.23):
//!   POST /api/memory/save   {text, title?, project?, metadata?} -> {"success": true, "id": N, ...}
//!   GET  /api/search?query=&project=&limit=   -> {"content": [{"type": "text", "text": <markdown table>}]}
//!   GET  /api/observation/<id>               -> the observation as JSON (title, narrative, created_at, ...)
//!   GET  /stream                              -> text/event-stream; "new_observation" carries {observation}
//!   GET  /health                              -> {"status": "ok", ...}
//!
//! Privacy: the bus never carries the learner's own words (memory_bus.py), and this module adds nothing.
//! The learner-word field names must not appear in any payload this file builds.
//!
//! The bus itself lives in the memory-bus port; this crate talks to it through the [`Bus`] trait, which has
//! exactly the three calls `claude_mem.py` makes on a `MemoryBus`.

mod py;

use std::collections::VecDeque;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

pub const DEFAULT_PORT: u16 = 37777;
pub const SOURCE: &str = "eve";
pub const MIRROR_TOPIC: &str = "/claude/observation";
pub const SINK_TOPICS: &str = "/eve/memory/*";
/// Lesson actions worth a memory. hint/status/open/listen are chatter; the rest mark progress.
pub const LESSON_ACTIONS_KEPT: [&str; 6] = ["start", "step", "check", "recap", "end", "complete"];
const MAX_STR: usize = 2000;
const MAX_LIST: usize = 20;
const WARN_EVERY_SECS: f64 = 600.0;
static ROW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\|\s*#([0-9]+)\s*\|\s*([^|]*?)\s*\|\s*[^|]*\|\s*([^|]*?)\s*\|").expect("row pattern")
});

// ---- the bus -------------------------------------------------------------------------------

/// A bus subscriber: `(topic, msg)`, called on whatever thread publishes.
pub type Callback = Arc<dyn Fn(&str, &Value) + Send + Sync>;

/// What `subscribe` hands back, and `unsubscribe` takes: the pattern and the subscription's id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub topic: String,
    pub id: u64,
}

/// The three MemoryBus calls this module makes. `subscribe` takes one topic or a prefix ending in `*`.
pub trait Bus: Send + Sync + 'static {
    fn subscribe(&self, topic: &str, callback: Callback) -> Subscription;
    fn unsubscribe(&self, sub: &Subscription);
    fn publish(&self, topic: &str, msg: Value);
}

// ---- where the worker is ----------------------------------------------------------------

/// EVE_CLAUDE_MEM_URL wins; else the plugin's settings.json port; else worker.pid; else 37777.
pub fn worker_url() -> String {
    let env = |name: &str| std::env::var_os(name).map(|v| v.to_string_lossy().into_owned());
    worker_url_from(&env, None)
}

/// [`worker_url`] with the environment and the home folder given (`home` None means the real one).
pub fn worker_url_from(env: &dyn Fn(&str) -> Option<String>, home: Option<&Path>) -> String {
    let explicit = env("EVE_CLAUDE_MEM_URL").unwrap_or_default();
    let explicit = py::strip(&explicit);
    if !explicit.is_empty() {
        return explicit.trim_end_matches('/').to_string();
    }
    let home = match home {
        Some(h) => Some(h.to_path_buf()),
        None => dirs::home_dir(),
    };
    if let Some(home) = home {
        let base = home.join(".claude-mem");
        for (path, key) in [(base.join("settings.json"), "CLAUDE_MEM_WORKER_PORT"), (base.join("worker.pid"), "port")] {
            let Some(port) = read_port(&path, key) else { continue };
            if 0 < port && port < 65536 {
                return format!("http://127.0.0.1:{port}");
            }
        }
    }
    format!("http://127.0.0.1:{DEFAULT_PORT}")
}

/// `int(str(json.loads(path.read_text()).get(key, "")).strip())`, None wherever Python raises.
fn read_port(path: &Path, key: &str) -> Option<i128> {
    let text = std::fs::read_to_string(path).ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;
    let obj = data.as_object()?;
    let raw = obj.get(key).map(py::py_str).unwrap_or_default();
    py::parse_int(&raw)
}

// ---- one bus event -> one memory ---------------------------------------------------------

fn clip(value: &str) -> String {
    clip_at(value, MAX_STR)
}

fn clip_at(value: &str, max: usize) -> String {
    if py::char_len(value) <= max {
        value.to_string()
    } else {
        format!("{}…", py::take_chars(value, max - 1))
    }
}

fn title(prefix: &str, text: &str) -> String {
    let squashed = py::squash_ws(text);
    let head = py::take_chars(&squashed, 60);
    if head.is_empty() {
        prefix.trim_end_matches([':', ' ']).to_string()
    } else {
        format!("{prefix}{head}")
    }
}

/// The only msg fields that ride along as metadata. An allowlist, not "every scalar": a producer that
/// one day adds a field with the learner's words in it must not leak it here by accident.
const META_FIELDS: [&str; 14] = [
    "id", "lesson_id", "action", "stage", "mode", "topic", "level", // lessons
    "importance", "novelty", "written", "tags", "changed", // diary
    "state", "source",
];

fn metadata(topic: &str, msg: &Map<String, Value>) -> Map<String, Value> {
    let mut meta = Map::new();
    meta.insert("platformSource".into(), Value::from(SOURCE));
    meta.insert("topic".into(), Value::from(topic));
    meta.insert("time".into(), msg.get("time").cloned().unwrap_or(Value::Null));
    for key in META_FIELDS {
        let Some(value) = msg.get(key) else { continue };
        if key == "topic" {
            continue;
        }
        match value {
            Value::Bool(_) | Value::Null | Value::Number(_) => {
                meta.insert(key.into(), value.clone());
            }
            Value::String(s) => {
                meta.insert(key.into(), Value::from(clip(s)));
            }
            Value::Array(items)
                if items.len() <= MAX_LIST
                    && items.iter().all(|v| matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_))) =>
            {
                let kept = items
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => Value::from(clip(s)),
                        other => other.clone(),
                    })
                    .collect();
                meta.insert(key.into(), Value::Array(kept));
            }
            _ => {}
        }
    }
    if topic == "/eve/memory/lesson" {
        if let Some(subject) = msg.get("topic") {
            meta.insert("subject".into(), Value::from(clip(&py::py_str(subject))));
        }
    }
    meta
}

/// Where Python's `for o in msg.get("observations") or []` raises: a truthy number or `True` is not
/// iterable. The bus logs the error and drops the event.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("TypeError: '{0}' object is not iterable")]
pub struct NotIterable(pub &'static str);

/// `str(o)` for each item Python's `for o in value` yields: a list's items, a string's characters, a dict's
/// keys.
fn iter_strs(value: Option<&Value>) -> Result<Vec<String>, NotIterable> {
    if !py::truthy(value) {
        return Ok(Vec::new());
    }
    match value {
        Some(Value::Array(items)) => Ok(items.iter().map(py::py_str).collect()),
        Some(Value::String(s)) => Ok(s.chars().map(String::from).collect()),
        Some(Value::Object(m)) => Ok(m.keys().cloned().collect()),
        Some(Value::Bool(_)) => Err(NotIterable("bool")),
        Some(Value::Number(n)) => Err(NotIterable(if n.is_f64() { "float" } else { "int" })),
        _ => Ok(Vec::new()),
    }
}

/// The /api/memory/save body for one bus event, or None when the event is not worth keeping.
pub fn to_memory(topic: &str, msg: &Value, project: &str) -> Result<Option<Value>, NotIterable> {
    let Some(m) = msg.as_object() else { return Ok(None) };
    let (text, ttl) = match topic {
        "/eve/memory/observation" => {
            let thought = py::str_or_empty(py::or(m.get("thought"), m.get("text")));
            let thought = py::strip(&thought).to_string();
            let seen: Vec<String> = iter_strs(m.get("observations"))?
                .into_iter()
                .filter(|s| !py::strip(s).is_empty())
                .take(MAX_LIST)
                .collect();
            if thought.is_empty() && seen.is_empty() {
                return Ok(None);
            }
            let mut text = clip(&thought);
            if !seen.is_empty() {
                text.push_str("\n\nSeen: ");
                text.push_str(&seen.iter().map(|s| clip(s)).collect::<Vec<_>>().join("; "));
            }
            let head = if thought.is_empty() { &seen[0] } else { &thought };
            (text, title("eve saw: ", head))
        }
        "/eve/memory/lesson" => {
            let action = py::str_or_empty(m.get("action"));
            let action = py::strip(&action).to_lowercase();
            if !LESSON_ACTIONS_KEPT.contains(&action.as_str()) {
                return Ok(None);
            }
            let subject = py::str_or(m.get("topic"), "a lesson");
            let subject = py::strip(&subject);
            let level = py::str_or_empty(m.get("level"));
            let level = py::strip(&level);
            let feedback = py::str_or_empty(m.get("feedback"));
            let feedback = py::strip(&feedback);
            let mut text = format!("{action} on {subject}");
            if !level.is_empty() {
                text.push_str(&format!(" ({level})"));
            }
            if !feedback.is_empty() {
                text.push_str(&format!(": {feedback}"));
            }
            (text, title("lesson: ", subject))
        }
        "/eve/memory/remember" => {
            let text = py::str_or_empty(m.get("text"));
            let text = py::strip(&text).to_string();
            if text.is_empty() {
                return Ok(None);
            }
            let given = py::str_or_empty(m.get("title"));
            let given = py::strip(&given);
            let ttl = if given.is_empty() { title("", &text) } else { given.to_string() };
            (text, ttl)
        }
        _ => return Ok(None),
    };
    let mut body = Map::new();
    body.insert("text".into(), Value::from(clip_at(&text, MAX_STR * 2)));
    body.insert("title".into(), Value::from(clip(&ttl)));
    body.insert("project".into(), Value::from(project));
    body.insert("metadata".into(), Value::Object(metadata(topic, m)));
    Ok(Some(Value::Object(body)))
}

// ---- HTTP, kept small ---------------------------------------------------------------------

/// A failed request, named the way Python's urllib names it (URLError, HTTPError, TimeoutError,
/// JSONDecodeError, ...), so the owner reads the same log line from either daemon.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct HttpError {
    pub kind: &'static str,
    pub message: String,
}

impl HttpError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        HttpError { kind, message: message.into() }
    }

    fn timed_out() -> Self {
        HttpError::new("TimeoutError", "timed out")
    }

    fn from_reqwest(e: reqwest::Error) -> Self {
        let mut message = e.to_string();
        let mut source = std::error::Error::source(&e);
        while let Some(s) = source {
            message.push_str(": ");
            message.push_str(&s.to_string());
            source = s.source();
        }
        let kind = if e.is_timeout() {
            "TimeoutError"
        } else if e.is_builder() {
            "ValueError"
        } else {
            "URLError"
        };
        HttpError::new(kind, message)
    }
}

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

/// Sends the request; `timeout` bounds the connect and the response head, as urllib's socket timeout does.
/// A status outside 2xx is an HTTPError, as urlopen raises.
async fn send(req: reqwest::RequestBuilder, timeout: Duration) -> Result<reqwest::Response, HttpError> {
    let resp = tokio::time::timeout(timeout, req.send())
        .await
        .map_err(|_| HttpError::timed_out())?
        .map_err(HttpError::from_reqwest)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(HttpError::new(
            "HTTPError",
            format!("HTTP Error {}: {}", status.as_u16(), status.canonical_reason().unwrap_or("")),
        ));
    }
    Ok(resp)
}

/// The whole body; `timeout` bounds each read, not the total.
async fn read_body(mut resp: reqwest::Response, timeout: Duration) -> Result<Vec<u8>, HttpError> {
    let mut buf = Vec::new();
    loop {
        match tokio::time::timeout(timeout, resp.chunk()).await {
            Err(_) => return Err(HttpError::timed_out()),
            Ok(Err(e)) => return Err(HttpError::from_reqwest(e)),
            Ok(Ok(None)) => return Ok(buf),
            Ok(Ok(Some(chunk))) => buf.extend_from_slice(&chunk),
        }
    }
}

/// `json.loads(body.decode("utf-8") or empty)`.
fn decode_json(bytes: Vec<u8>, empty: &str) -> Result<Value, HttpError> {
    let text = String::from_utf8(bytes).map_err(|e| HttpError::new("UnicodeDecodeError", e.to_string()))?;
    let text = if text.is_empty() { empty } else { text.as_str() };
    serde_json::from_str(text).map_err(|e| HttpError::new("JSONDecodeError", e.to_string()))
}

/// POST `body` as JSON (the bytes Python's `json.dumps` writes) and parse the answer.
pub async fn post(url: &str, body: &Value, timeout: Duration) -> Result<Value, HttpError> {
    let req = CLIENT
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(py::dumps(body).into_bytes());
    let resp = send(req, timeout).await?;
    decode_json(read_body(resp, timeout).await?, "{}")
}

/// GET `url` and parse the JSON answer (`null` for an empty body).
pub async fn get(url: &str, timeout: Duration) -> Result<Value, HttpError> {
    let resp = send(CLIENT.get(url), timeout).await?;
    decode_json(read_body(resp, timeout).await?, "null")
}

/// The future a [`Poster`] returns.
pub type PostFuture = Pin<Box<dyn Future<Output = Result<Value, HttpError>> + Send>>;
/// How the sink sends one memory: `(url, body, timeout)`. [`http_poster`] is the real one; the seam lets a
/// test see every body without a worker.
pub type Poster = Arc<dyn Fn(String, Value, Duration) -> PostFuture + Send + Sync>;

/// The real [`Poster`]: [`post`].
pub fn http_poster() -> Poster {
    Arc::new(|url: String, body: Value, timeout: Duration| -> PostFuture {
        Box::pin(async move { post(&url, &body, timeout).await })
    })
}

// ---- the sink -------------------------------------------------------------------------------

/// The bounded queue between the bus thread and the sink's task. `None` is the stop sentinel; a maxsize of
/// 0 is unbounded, as in `queue.Queue`.
struct BodyQueue {
    items: Mutex<VecDeque<Option<Value>>>,
    maxsize: usize,
    ready: Notify,
}

impl BodyQueue {
    fn new(maxsize: usize) -> Self {
        BodyQueue { items: Mutex::new(VecDeque::new()), maxsize, ready: Notify::new() }
    }

    /// False when full.
    fn put_nowait(&self, item: Option<Value>) -> bool {
        {
            let mut items = self.items.lock();
            if self.maxsize > 0 && items.len() >= self.maxsize {
                return false;
            }
            items.push_back(item);
        }
        self.ready.notify_one();
        true
    }

    async fn get(&self) -> Option<Value> {
        loop {
            let notified = self.ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(item) = self.items.lock().pop_front() {
                return item;
            }
            notified.await;
        }
    }

    #[cfg(test)]
    fn qsize(&self) -> usize {
        self.items.lock().len()
    }
}

#[derive(Default)]
struct SinkCounters {
    saved: AtomicU64,
    dropped: AtomicU64,
    failed: AtomicU64,
}

/// The sink's `stats`: `{"saved", "dropped", "failed"}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SinkStats {
    pub saved: u64,
    pub dropped: u64,
    pub failed: u64,
}

/// [`ClaudeMemSink`]'s keyword arguments, with Python's defaults.
#[derive(Debug, Clone)]
pub struct SinkOptions {
    pub project: String,
    /// None (or empty) asks [`worker_url`].
    pub url: Option<String>,
    /// 0 is unbounded.
    pub queue_size: usize,
    pub timeout: Duration,
}

impl Default for SinkOptions {
    fn default() -> Self {
        SinkOptions { project: "eve".into(), url: None, queue_size: 256, timeout: Duration::from_secs_f64(5.0) }
    }
}

/// The half the bus callback holds.
struct SinkIntake {
    project: String,
    queue: BodyQueue,
    counters: SinkCounters,
}

impl SinkIntake {
    // bus thread: enqueue only
    fn on_event(&self, topic: &str, msg: &Value) {
        let body = match to_memory(topic, msg, &self.project) {
            Ok(Some(body)) => body,
            Ok(None) => return,
            Err(e) => {
                tracing::error!("memory bus: subscriber failed on {topic}: {e}");
                return;
            }
        };
        if !self.queue.put_nowait(Some(body)) {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn resolve_url(url: Option<&str>) -> String {
    let url = match url {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => worker_url(),
    };
    url.trim_end_matches('/').to_string()
}

fn warn_due(last: &Mutex<Option<Instant>>) -> bool {
    let mut last = last.lock();
    let now = Instant::now();
    let due = last.is_none_or(|t| now.duration_since(t).as_secs_f64() >= WARN_EVERY_SECS);
    if due {
        *last = Some(now);
    }
    due
}

/// Bus events -> POST /api/memory/save, from one task, never blocking the publisher.
///
/// `start` spawns on the current tokio runtime.
pub struct ClaudeMemSink<B: Bus + ?Sized> {
    bus: Arc<B>,
    pub project: String,
    pub url: String,
    pub timeout: Duration,
    intake: Arc<SinkIntake>,
    poster: Poster,
    task: Option<JoinHandle<()>>,
    sub: Option<Subscription>,
    last_warned: Arc<Mutex<Option<Instant>>>,
}

impl<B: Bus + ?Sized> ClaudeMemSink<B> {
    pub fn new(bus: Arc<B>, options: SinkOptions) -> Self {
        let url = resolve_url(options.url.as_deref());
        let intake = Arc::new(SinkIntake {
            project: options.project.clone(),
            queue: BodyQueue::new(options.queue_size),
            counters: SinkCounters::default(),
        });
        ClaudeMemSink {
            bus,
            project: options.project,
            url,
            timeout: options.timeout,
            intake,
            poster: http_poster(),
            task: None,
            sub: None,
            last_warned: Arc::new(Mutex::new(None)),
        }
    }

    /// Replace how each memory is sent (the HTTP POST by default).
    pub fn with_poster(mut self, poster: Poster) -> Self {
        self.poster = poster;
        self
    }

    pub fn stats(&self) -> SinkStats {
        let c = &self.intake.counters;
        SinkStats {
            saved: c.saved.load(Ordering::Relaxed),
            dropped: c.dropped.load(Ordering::Relaxed),
            failed: c.failed.load(Ordering::Relaxed),
        }
    }

    pub fn start(&mut self) {
        if self.task.is_some() {
            return;
        }
        let intake = Arc::clone(&self.intake);
        let callback: Callback = Arc::new(move |topic: &str, msg: &Value| intake.on_event(topic, msg));
        self.sub = Some(self.bus.subscribe(SINK_TOPICS, callback));
        let run = SinkRun {
            url: self.url.clone(),
            timeout: self.timeout,
            intake: Arc::clone(&self.intake),
            poster: Arc::clone(&self.poster),
            last_warned: Arc::clone(&self.last_warned),
        };
        self.task = Some(tokio::spawn(run.run()));
    }

    pub async fn stop(&mut self, wait: Duration) {
        if let Some(sub) = self.sub.take() {
            self.bus.unsubscribe(&sub);
        }
        let Some(task) = self.task.take() else { return };
        // A full queue keeps its items and the task drains them; the wait still bounds this call.
        self.intake.queue.put_nowait(None);
        let _ = tokio::time::timeout(wait, task).await;
    }
}

struct SinkRun {
    url: String,
    timeout: Duration,
    intake: Arc<SinkIntake>,
    poster: Poster,
    last_warned: Arc<Mutex<Option<Instant>>>,
}

impl SinkRun {
    // worker task
    async fn run(self) {
        loop {
            let Some(body) = self.intake.queue.get().await else { return };
            match (self.poster)(format!("{}/api/memory/save", self.url), body, self.timeout).await {
                Ok(_) => {
                    self.intake.counters.saved.fetch_add(1, Ordering::Relaxed);
                }
                // a dead worker must never take the daemon down
                Err(e) => {
                    self.intake.counters.failed.fetch_add(1, Ordering::Relaxed);
                    if warn_due(&self.last_warned) {
                        tracing::warn!(
                            "claude-mem: could not save a memory at {} ({}: {}); further failures are counted, not logged, for {} minutes",
                            self.url,
                            e.kind,
                            e.message,
                            (WARN_EVERY_SECS / 60.0) as i64
                        );
                    }
                }
            }
        }
    }
}

// ---- recall -----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    id: i64,
    time: String,
    title: String,
}

fn parse_rows(markdown: &str) -> Vec<Row> {
    py::splitlines(markdown)
        .into_iter()
        .filter_map(|line| {
            let m = ROW.captures(py::strip(line))?;
            Some(Row {
                id: m[1].parse().ok()?,
                time: py::strip(&m[2]).to_string(),
                title: py::strip(&m[3]).to_string(),
            })
        })
        .collect()
}

/// One recall result: `{"id", "title", "time", "text"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecallHit {
    pub id: i64,
    pub title: String,
    pub time: String,
    pub text: String,
}

fn py_type_name(v: &Value) -> &'static str {
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

/// The search answer's text parts joined, or the Python exception the same walk would raise.
fn search_text(data: &Value) -> Result<String, HttpError> {
    // (data or {}).get("content", [])
    let content = if !py::truthy(Some(data)) {
        None
    } else {
        match data {
            Value::Object(m) => m.get("content"),
            other => {
                return Err(HttpError::new(
                    "AttributeError",
                    format!("'{}' object has no attribute 'get'", py_type_name(other)),
                ))
            }
        }
    };
    let parts: Vec<String> = match content {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            items.iter().filter_map(|p| p.as_object()).map(|p| py::str_or_empty(p.get("text"))).collect()
        }
        // a string's characters and a dict's keys are never dicts
        Some(Value::String(_)) | Some(Value::Object(_)) => Vec::new(),
        Some(other) => {
            return Err(HttpError::new("TypeError", format!("'{}' object is not iterable", py_type_name(other))))
        }
    };
    Ok(parts.join("\n"))
}

/// Search claude-mem. `[{"id", "title", "time", "text"}]` newest first; `[]` on any error.
pub async fn recall(query: &str, limit: i64, url: Option<&str>, timeout: Duration, project: &str) -> Vec<RecallHit> {
    let base = resolve_url(url);
    let limit = if limit == 0 { 5 } else { limit }.clamp(1, 20);
    match recall_at(&base, query, limit, timeout, project).await {
        Ok(results) => results,
        Err(e) => {
            tracing::debug!("claude-mem: recall failed at {}: {}: {}", base, e.kind, e.message);
            Vec::new()
        }
    }
}

async fn recall_at(
    base: &str,
    query: &str,
    limit: i64,
    timeout: Duration,
    project: &str,
) -> Result<Vec<RecallHit>, HttpError> {
    let params = py::urlencode(&[("query", query.to_string()), ("project", project.to_string()), ("limit", limit.to_string())]);
    let data = get(&format!("{base}/api/search?{params}"), timeout).await?;
    let text = search_text(&data)?;
    let mut rows = parse_rows(&text);
    rows.truncate(limit as usize);
    let mut results = Vec::with_capacity(rows.len());
    for mut row in rows {
        let mut body = String::new();
        // the row still counts without its body
        match get(&format!("{base}/api/observation/{}", row.id), timeout).await {
            Ok(Value::Object(obs)) => {
                body = py::str_or_empty(py::or(obs.get("narrative"), obs.get("text")));
                row.time = py::str_or(obs.get("created_at"), &row.time);
                row.title = py::str_or(obs.get("title"), &row.title);
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("claude-mem: observation {} body unavailable: {}", row.id, e.message),
        }
        results.push(RecallHit { id: row.id, title: row.title, time: row.time, text: clip(&body) });
    }
    Ok(results)
}

// ---- the mirror ---------------------------------------------------------------------------

/// How long the stream may stay quiet before the mirror reconnects. In Python a timed-out read could never
/// be retried: the socket was marked "timed out" and every later read failed at once ("cannot read from
/// timed out object"). The loop used to treat that as another quiet second and retry, so after the first
/// quiet second it spun about two million times a second holding the GIL, starving the daemon's event loop
/// (Telegram replies lagged, 2026-09-24) and never reading another event. A quiet stream now means
/// reconnect.
pub const IDLE_SECS: f64 = 30.0;

#[derive(Default)]
struct MirrorCounters {
    mirrored: AtomicU64,
    reconnects: AtomicU64,
}

/// The mirror's `stats`: `{"mirrored", "reconnects"}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MirrorStats {
    pub mirrored: u64,
    pub reconnects: u64,
}

/// [`ClaudeMemMirror`]'s keyword arguments, with Python's defaults.
#[derive(Debug, Clone)]
pub struct MirrorOptions {
    /// None (or empty) asks [`worker_url`].
    pub url: Option<String>,
    pub reconnect_secs: Duration,
    pub skip_project: String,
    pub read_timeout: Duration,
}

impl Default for MirrorOptions {
    fn default() -> Self {
        MirrorOptions {
            url: None,
            reconnect_secs: Duration::from_secs_f64(5.0),
            skip_project: "eve".into(),
            read_timeout: Duration::from_secs_f64(IDLE_SECS),
        }
    }
}

/// GET /stream -> /claude/observation for every new observation from the owner's other projects.
///
/// `start` spawns on the current tokio runtime. `stop` ends a blocked read at once.
pub struct ClaudeMemMirror<B: Bus + ?Sized> {
    shared: Arc<MirrorShared<B>>,
    pub url: String,
    pub reconnect_secs: Duration,
    pub skip_project: String,
    pub read_timeout: Duration,
    stop_tx: Option<watch::Sender<bool>>,
    task: Option<JoinHandle<()>>,
}

struct MirrorShared<B: Bus + ?Sized> {
    bus: Arc<B>,
    counters: MirrorCounters,
    last_warned: Mutex<Option<Instant>>,
}

struct MirrorRun<B: Bus + ?Sized> {
    shared: Arc<MirrorShared<B>>,
    url: String,
    reconnect_secs: Duration,
    skip_project: String,
    read_timeout: Duration,
}

impl<B: Bus + ?Sized> ClaudeMemMirror<B> {
    pub fn new(bus: Arc<B>, options: MirrorOptions) -> Self {
        ClaudeMemMirror {
            shared: Arc::new(MirrorShared {
                bus,
                counters: MirrorCounters::default(),
                last_warned: Mutex::new(None),
            }),
            url: resolve_url(options.url.as_deref()),
            reconnect_secs: options.reconnect_secs,
            skip_project: options.skip_project,
            read_timeout: options.read_timeout,
            stop_tx: None,
            task: None,
        }
    }

    pub fn stats(&self) -> MirrorStats {
        let c = &self.shared.counters;
        MirrorStats { mirrored: c.mirrored.load(Ordering::Relaxed), reconnects: c.reconnects.load(Ordering::Relaxed) }
    }

    pub fn start(&mut self) {
        if self.task.is_some() {
            return;
        }
        let (tx, rx) = watch::channel(false);
        self.stop_tx = Some(tx);
        let run = MirrorRun {
            shared: Arc::clone(&self.shared),
            url: self.url.clone(),
            reconnect_secs: self.reconnect_secs,
            skip_project: self.skip_project.clone(),
            read_timeout: self.read_timeout,
        };
        self.task = Some(tokio::spawn(run.run(rx)));
    }

    pub async fn stop(&mut self, wait: Duration) {
        // The stop signal also ends a read that is waiting on the stream.
        if let Some(tx) = &self.stop_tx {
            let _ = tx.send(true);
        }
        let Some(task) = self.task.take() else { return };
        let _ = tokio::time::timeout(wait, task).await;
    }
}

/// True once stop() was called, or the mirror was dropped.
fn stopped(stop: &watch::Receiver<bool>) -> bool {
    *stop.borrow() || stop.has_changed().is_err()
}

/// Resolves when stop() is called (or the mirror is dropped).
async fn stop_signal(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|v| *v).await;
}

impl<B: Bus + ?Sized> MirrorRun<B> {
    async fn run(self, mut stop: watch::Receiver<bool>) {
        let (mut first, mut quiet) = (true, false);
        while !stopped(&stop) {
            if !first && !quiet {
                // a quiet stream reconnects at once, uncounted
                self.shared.counters.reconnects.fetch_add(1, Ordering::Relaxed);
                tokio::select! {
                    biased;
                    _ = stop_signal(&mut stop) => return,
                    _ = tokio::time::sleep(self.reconnect_secs) => {}
                }
                if stopped(&stop) {
                    return;
                }
            }
            first = false;
            quiet = false;
            // reconnect, never raise
            match self.read_stream(&mut stop).await {
                Ok(q) => quiet = q,
                Err(e) => {
                    if warn_due(&self.shared.last_warned) {
                        tracing::warn!(
                            "claude-mem: mirror lost {}/stream ({}: {}); retrying every {:.0} s",
                            self.url,
                            e.kind,
                            e.message,
                            self.reconnect_secs.as_secs_f64()
                        );
                    }
                }
            }
        }
    }

    /// Read the stream until it ends (false) or stays quiet for read_timeout (true: reconnect at once).
    async fn read_stream(&self, stop: &mut watch::Receiver<bool>) -> Result<bool, HttpError> {
        let req = CLIENT.get(format!("{}/stream", self.url)).header(reqwest::header::ACCEPT, "text/event-stream");
        let mut resp = tokio::select! {
            biased;
            _ = stop_signal(stop) => return Ok(false),
            r = send(req, self.read_timeout) => r?,
        };
        let mut buf: Vec<u8> = Vec::new();
        loop {
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                if stopped(stop) {
                    return Ok(false);
                }
                let line: Vec<u8> = buf.drain(..=pos).collect();
                self.on_line(String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']));
            }
            if stopped(stop) {
                return Ok(false);
            }
            tokio::select! {
                biased;
                // stop() ends the read now, as shutting the socket did
                _ = stop_signal(stop) => return Ok(false),
                r = tokio::time::timeout(self.read_timeout, resp.chunk()) => match r {
                    // quiet: reconnect at once (IDLE_SECS)
                    Err(_) => return Ok(true),
                    Ok(Err(e)) => return Err(HttpError::from_reqwest(e)),
                    // the worker closed the stream
                    Ok(Ok(None)) => {
                        if !buf.is_empty() {
                            let line = std::mem::take(&mut buf);
                            self.on_line(String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']));
                        }
                        return Ok(false);
                    }
                    Ok(Ok(Some(chunk))) => buf.extend_from_slice(&chunk),
                },
            }
        }
    }

    fn on_line(&self, line: &str) {
        on_line(&*self.shared.bus, &self.shared.counters, &self.skip_project, line);
    }
}

fn on_line<B: Bus + ?Sized>(bus: &B, counters: &MirrorCounters, skip_project: &str, line: &str) {
    let Some(rest) = line.strip_prefix("data:") else { return };
    let rest = py::strip(rest);
    let Ok(event) = serde_json::from_str::<Value>(if rest.is_empty() { "{}" } else { rest }) else { return };
    let Some(event) = event.as_object() else { return };
    if event.get("type") != Some(&Value::from("new_observation")) {
        return;
    }
    let Some(obs) = event.get("observation").and_then(Value::as_object) else { return };
    let project = py::str_or_empty(obs.get("project"));
    if project == skip_project || py::str_or_empty(obs.get("platform_source")) == SOURCE {
        return;
    }
    let mut msg = Map::new();
    msg.insert("id".into(), obs.get("id").cloned().unwrap_or(Value::Null));
    msg.insert("title".into(), Value::from(py::str_or_empty(obs.get("title"))));
    msg.insert("project".into(), Value::from(project));
    msg.insert("type".into(), Value::from(py::str_or_empty(obs.get("type"))));
    msg.insert("time".into(), obs.get("created_at_epoch").cloned().unwrap_or(Value::Null));
    msg.insert("text".into(), Value::from(clip(&py::str_or_empty(py::or(obs.get("narrative"), obs.get("text"))))));
    bus.publish(MIRROR_TOPIC, Value::Object(msg));
    counters.mirrored.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests;
