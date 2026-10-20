//! Scene: what eve can see, as short timestamped sentences for the voice.
//!
//! Port of `eve_bridge/scene.py`.
//!
//! The voice model (gpt-live-1) takes audio and text only — images are an
//! unsupported modality on its model page, and the Live API in the SDK has no
//! image input. So a cheap image model looks for it: while a conversation is
//! open, the newest camera frame goes to `gpt-6-luna` at most once every
//! `interval_secs`, and what it says comes back as one line such as
//!
//! ```text
//! [vision 16:43:05] A person at a desk holding a green mug. Change: they picked up the mug.
//! ```
//!
//! Nothing here is pushed to the voice. Each line goes into the watcher's own
//! journal (`notes`) and the newest view waits in `latest`; the voice reads
//! it only when the owner asks, through the backend's `look` tool. (Pushing
//! every line as silent context made the voice narrate the room unprompted,
//! owner report 2026-09-10.) head.py's `look_around` / `find` use
//! `look(newer_than=...)` and `locate` so a view is always from a frame taken
//! after the head stopped turning.
//!
//! Privacy, by construction:
//!
//! * Nothing runs outside a conversation. `offer` ignores frames until
//!   `start`, and `stop` drops the frame slot and every observation.
//! * One frame at most is held, in memory, and it is released as soon as it is
//!   handed to the describer. Nothing is ever written to disk here.
//! * The API call sets `store=False`, so the response is not kept for the
//!   dashboard or for later retrieval.
//!
//! Honesty about time: every line carries the wall-clock time the frame was
//! *seen*, not the time the answer came back. When no frame arrives for
//! `camera_lost_secs` (board gone, camera off), the journal records once that
//! the camera is lost and `look` answers that nothing can be seen. When the
//! view is unchanged, a "no change since" line — stamped with the newest view
//! that confirmed it — is journaled every `refresh_secs`.
//!
//! Two halves, as in vision.py: a pure core (`SceneWatcher` with an injected
//! client and clock, `parse_scene`, `format_note`) that runs in tests, and
//! `OpenAISceneClient`, the only part that touches the network.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use chrono::NaiveDateTime;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tokio::task::JoinHandle;

use eve_explore::{data_url, frame_image, output_text, Responses, RuntimeError};
use eve_spend::Ledger;
use eve_vision::{decode_frame, opt_float, ValueError};

pub const DEFAULT_MODEL: &str = "gpt-6-luna";
/// minimum gap between two describes
pub const DEFAULT_INTERVAL_SECS: f64 = 3.0;
/// a view older than this is "out of date"
pub const DEFAULT_STALE_SECS: f64 = 15.0;
/// no frame for this long: the camera is gone
pub const DEFAULT_CAMERA_LOST_SECS: f64 = 3.0;
/// one describe; slower answers are dropped
pub const DEFAULT_TIMEOUT_SECS: f64 = 8.0;
/// repeat "no change" this often
pub const DEFAULT_REFRESH_SECS: f64 = 20.0;
/// includes reasoning; the schema/prompt still bounds the visible answer
pub const MAX_OUTPUT_TOKENS: i64 = 2048;
/// session.thinking.append allows 500 tokens; stay far below
pub const NOTE_MAX_CHARS: usize = 300;
/// the look tool reuses a view this young
pub const LOOK_FRESH_SECS: f64 = 4.0;
pub const TICK_SECS: f64 = 0.25;
pub const FRAME_WAIT_POLL_SECS: f64 = 0.05;

pub const PROMPT: &str = "You are the eyes of a small desk robot. This is one frame from its camera. \
In `scene`, describe what is in view in one plain sentence of at most 25 words: \
people (what they are doing, what they hold), notable objects, and the setting. \
Say only what you can see. If the frame is dark, blurry or blocked, say that. \
Never guess names or identities. \
In `change`, say in at most 12 words what differs from the previous description, \
or an empty string if nothing meaningful changed.";

/// The describe answer's JSON schema.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "scene": {"type": "string"},
            "change": {"type": "string"},
        },
        "required": ["scene", "change"],
        "additionalProperties": false,
    })
}

pub const LOCATE_PROMPT: &str = "You are the eyes of a small desk robot looking for something. This is one frame from its camera. \
Is the thing described below visible in this frame? If it is, give the centre of it: \
`x` from -100 (left edge of the image) to 100 (right edge), `y` from -100 (top) to 100 (bottom), \
and in `what` name what you found in a few words. If it is not visible, set `visible` to false, \
`x` and `y` to 0 and `what` to an empty string. Do not guess.\nLooking for: ";

/// The locate answer's JSON schema.
pub fn locate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "visible": {"type": "boolean"},
            "x": {"type": "number"},
            "y": {"type": "number"},
            "what": {"type": "string"},
        },
        "required": ["visible", "x", "y", "what"],
        "additionalProperties": false,
    })
}

pub const NO_CAMERA: &str = "the camera is not connected, so nothing can be seen right now";

// ---- Python string and number semantics -----------------------------------------------

/// Python's `str.isspace` for one character.
fn py_is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `" ".join(text.split())`.
fn collapse_ws(text: &str) -> String {
    text.split(py_is_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Python's `float(str)`: decimal literals with single underscores between
/// digits, `inf` / `infinity` / `nan` in any case, an optional sign.
fn py_float(raw: &str) -> Option<f64> {
    let s = raw.trim_matches(py_is_space);
    let (sign, body) = match s.as_bytes().first() {
        Some(b'+') => (1.0, &s[1..]),
        Some(b'-') => (-1.0, &s[1..]),
        _ => (1.0, s),
    };
    let lower = body.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" {
        return Some(sign * f64::INFINITY);
    }
    if lower == "nan" {
        return Some(f64::NAN);
    }
    let bytes = body.as_bytes();
    let mut cleaned = String::with_capacity(body.len());
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-' => cleaned.push(b as char),
            b'_' => {
                let before = i > 0 && bytes[i - 1].is_ascii_digit();
                let after = bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
                if !(before && after) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    if cleaned.is_empty() || cleaned.starts_with(['+', '-']) || !cleaned.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    cleaned.parse::<f64>().ok().map(|v| sign * v)
}

/// Python's `min(a, b)` on floats: `b` only when `b < a` (so a NaN `b` loses).
fn py_min(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}

/// Python's `max(a, b)` on floats: `b` only when `b > a`.
fn py_max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

/// Python's `repr(float)`.
fn py_float_str(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    eve_spend::py_float_repr(x)
}

/// Python's `repr(str)`.
fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr()` of what `json.loads` returns.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => py_num_str(n),
        Value::String(s) => py_repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter().map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn py_num_str(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    py_float_str(n.as_f64().unwrap_or(0.0))
}

/// Python truthiness of a JSON value.
fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// `str(obj.get(key) or "")`.
fn py_str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if py_truthy(v) => match v {
            Value::String(s) => s.clone(),
            other => py_repr(other),
        },
        _ => String::new(),
    }
}

/// Python's `float(v)` on a JSON value; None where it raises TypeError or ValueError.
fn py_float_of(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => py_float(s),
        _ => None,
    }
}

/// Python's `type(obj).__name__` for what `json.loads` returns.
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

// ---- errors -----------------------------------------------------------------------------

/// `asyncio.wait_for` ran out of time (Python's `TimeoutError`, whose `str` is empty).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("")]
pub struct TimeoutError;

/// Python's `AttributeError`: a locate answer that is JSON but not an object.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AttributeError(pub String);

/// Python's `AssertionError`: no client or no executor when a describe runs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("")]
pub struct AssertionError;

/// The describer panicked (Python has no counterpart; it counts as a failed call).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Panicked(pub String);

/// The executor was shut down before the call ran (Python's `CancelledError` out of a cancelled future).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("")]
pub struct Cancelled;

/// `type(e).__name__` as the Python log prints it.
pub fn error_name(e: &anyhow::Error) -> &'static str {
    if e.downcast_ref::<TimeoutError>().is_some() {
        return "TimeoutError";
    }
    if e.downcast_ref::<AttributeError>().is_some() {
        return "AttributeError";
    }
    if e.downcast_ref::<AssertionError>().is_some() {
        return "AssertionError";
    }
    if e.downcast_ref::<Cancelled>().is_some() {
        return "CancelledError";
    }
    if e.downcast_ref::<Panicked>().is_some() {
        return "Exception";
    }
    eve_explore::error_name(e)
}

/// `str(e)` as the Python log prints it.
pub fn error_text(e: &anyhow::Error) -> String {
    if e.downcast_ref::<TimeoutError>().is_some()
        || e.downcast_ref::<AttributeError>().is_some()
        || e.downcast_ref::<AssertionError>().is_some()
        || e.downcast_ref::<Cancelled>().is_some()
        || e.downcast_ref::<Panicked>().is_some()
    {
        return e.to_string();
    }
    eve_explore::error_text(e)
}

// ---- config -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct SceneConfig {
    pub enabled: bool,
    pub model: String,
    pub interval_secs: f64,
    pub stale_secs: f64,
    pub camera_lost_secs: f64,
    pub timeout_secs: f64,
    pub refresh_secs: f64,
}

impl Default for SceneConfig {
    fn default() -> Self {
        SceneConfig {
            enabled: true,
            model: DEFAULT_MODEL.to_string(),
            interval_secs: DEFAULT_INTERVAL_SECS,
            stale_secs: DEFAULT_STALE_SECS,
            camera_lost_secs: DEFAULT_CAMERA_LOST_SECS,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            refresh_secs: DEFAULT_REFRESH_SECS,
        }
    }
}

fn env_get(env: Option<&HashMap<String, String>>, name: &str) -> String {
    match env {
        Some(m) => m.get(name).cloned().unwrap_or_default(),
        None => std::env::var(name).unwrap_or_default(),
    }
}

fn float_env(env: Option<&HashMap<String, String>>, name: &str, default: f64, lo: f64, hi: f64) -> f64 {
    let value = env_get(env, name);
    let raw = value.trim_matches(py_is_space);
    if raw.is_empty() {
        return default;
    }
    match py_float(raw) {
        Some(v) => py_min(hi, py_max(lo, v)),
        None => {
            tracing::warn!("scene: {}={} is not a number; using {}", name, py_repr_str(raw), py_float_str(default));
            default
        }
    }
}

/// `EVE_SCENE=0` turns it off; the rest tune it. `None` reads the process environment.
pub fn configured(environ: Option<&HashMap<String, String>>) -> SceneConfig {
    let on = env_get(environ, "EVE_SCENE");
    let on = if on.is_empty() { "1".to_string() } else { on };
    let enabled = !["0", "false", "no", "off"].contains(&on.trim_matches(py_is_space).to_lowercase().as_str());
    let model = env_get(environ, "EVE_SCENE_MODEL");
    let model = model.trim_matches(py_is_space);
    let model = if model.is_empty() { DEFAULT_MODEL.to_string() } else { model.to_string() };
    SceneConfig {
        enabled,
        model,
        interval_secs: float_env(environ, "EVE_SCENE_INTERVAL_SECS", DEFAULT_INTERVAL_SECS, 1.0, 60.0),
        stale_secs: float_env(environ, "EVE_SCENE_STALE_SECS", DEFAULT_STALE_SECS, 3.0, 300.0),
        ..SceneConfig::default()
    }
}

// ---- observations and notes -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// monotonic time the frame arrived
    pub seen_at: f64,
    /// the same instant on the wall clock
    pub seen_wall: NaiveDateTime,
    pub text: String,
    /// "" when nothing meaningful changed
    pub change: String,
    /// how long the describe took
    pub latency_secs: f64,
    /// head pose the board echoed on that frame
    pub yaw: Option<f64>,
    pub pitch: Option<f64>,
}

impl Observation {
    /// `Observation(seen_at, seen_wall, text, change, latency_secs)` with no head pose.
    pub fn new(seen_at: f64, seen_wall: NaiveDateTime, text: &str, change: &str, latency_secs: f64) -> Self {
        Observation {
            seen_at,
            seen_wall,
            text: text.to_string(),
            change: change.to_string(),
            latency_secs,
            yaw: None,
            pitch: None,
        }
    }
}

/// What the describer and locator do. Both block; both are run on the watcher's one worker thread.
pub trait SceneClient: Send + Sync {
    /// (scene sentence, change phrase or ""). Errs on any failure.
    fn describe(&self, image: &[u8], mime: &str, previous: Option<&str>) -> anyhow::Result<(String, String)>;
    /// {"visible", "x", "y", "what"}. Errs on any failure.
    fn locate(&self, image: &[u8], mime: &str, target: &str) -> anyhow::Result<Map<String, Value>>;
}

/// The model's JSON answer → (scene, change). A plain-text answer is the scene.
pub fn parse_scene(raw: &str) -> Result<(String, String), ValueError> {
    let raw = raw.trim_matches(py_is_space);
    if raw.is_empty() {
        return Err(ValueError("empty answer".into()));
    }
    let obj = match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(m)) => m,
        _ => return Ok((collapse_ws(raw), String::new())),
    };
    let scene = collapse_ws(&py_str_or_empty(obj.get("scene")));
    let mut change = collapse_ws(&py_str_or_empty(obj.get("change")));
    if scene.is_empty() {
        return Err(ValueError("answer has no scene".into()));
    }
    let lowered = change.to_lowercase();
    if ["none", "no change", "nothing", "n/a", "nothing changed"].contains(&lowered.trim_end_matches('.')) {
        change = String::new();
    }
    Ok((scene, change))
}

/// The model's locate answer → {"visible", "x", "y", "what"}, coordinates clamped to ±100.
/// Errs (`serde_json::Error`, Python's JSONDecodeError) on anything but JSON, and with
/// `AttributeError` on JSON that is not an object.
pub fn parse_locate(raw: &str) -> anyhow::Result<Map<String, Value>> {
    let obj: Value = serde_json::from_str(raw)?;
    let Value::Object(obj) = obj else {
        return Err(AttributeError(format!("'{}' object has no attribute 'get'", py_type_name(&obj))).into());
    };
    let visible = matches!(obj.get("visible"), Some(Value::Bool(true)));
    let coord = |v: Option<&Value>| match py_float_of(v) {
        Some(f) => py_max(-100.0, py_min(100.0, f)),
        None => 0.0,
    };
    let mut out = Map::new();
    out.insert("visible".into(), Value::Bool(visible));
    out.insert("x".into(), json!(if visible { coord(obj.get("x")) } else { 0.0 }));
    out.insert("y".into(), json!(if visible { coord(obj.get("y")) } else { 0.0 }));
    out.insert(
        "what".into(),
        Value::String(if visible { collapse_ws(&py_str_or_empty(obj.get("what"))) } else { String::new() }),
    );
    Ok(out)
}

fn hms(t: &NaiveDateTime) -> String {
    t.format("%H:%M:%S").to_string()
}

fn clip(text: &str, n: usize) -> String {
    if text.chars().count() <= n {
        return text.to_string();
    }
    let head: String = text.chars().take(n.saturating_sub(1)).collect();
    format!("{}…", head.trim_end_matches(py_is_space))
}

/// One observation as the line the voice reads.
pub fn format_note(obs: &Observation) -> String {
    let mut line = format!("[vision {}] {}", hms(&obs.seen_wall), obs.text);
    if !obs.change.is_empty() {
        line.push_str(&format!(" Change: {}", obs.change));
    }
    clip(&line, NOTE_MAX_CHARS)
}

/// Stamped with the newest view that confirmed it — never with 'now', which
/// would claim a look that did not happen.
pub fn unchanged_note(latest: &Observation, since: &Observation) -> String {
    clip(
        &format!("[vision {}] No change since {}: {}", hms(&latest.seen_wall), hms(&since.seen_wall), latest.text),
        NOTE_MAX_CHARS,
    )
}

pub fn lost_note(now_wall: &NaiveDateTime, last: Option<&Observation>) -> String {
    let mut base = format!("[vision {}] Camera lost: you cannot see anything right now.", hms(now_wall));
    if let Some(last) = last {
        base.push_str(&format!(" Your last view, from {}, may be out of date.", hms(&last.seen_wall)));
    }
    clip(&base, NOTE_MAX_CHARS)
}

pub fn blind_note(now_wall: &NaiveDateTime, last: Option<&Observation>) -> String {
    let mut base = format!("[vision {}] You cannot make out the view right now.", hms(now_wall));
    if let Some(last) = last {
        base.push_str(&format!(" Your last clear view is from {} and may be out of date.", hms(&last.seen_wall)));
    }
    clip(&base, NOTE_MAX_CHARS)
}

// ---- the one-worker executor --------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send>;

/// `ThreadPoolExecutor(max_workers=1, thread_name_prefix="scene")`: one thread running jobs in
/// order. A job whose caller gave up (timed out) before it started is skipped, as cancelling a
/// pending `concurrent.futures.Future` skips it; a running one finishes and its answer is dropped.
struct Worker {
    tx: Mutex<Option<std_mpsc::Sender<Job>>>,
    closed: Arc<AtomicBool>,
}

impl Worker {
    fn spawn() -> Arc<Worker> {
        let (tx, rx) = std_mpsc::channel::<Job>();
        let closed = Arc::new(AtomicBool::new(false));
        let spawned = std::thread::Builder::new().name("scene_0".into()).spawn(move || {
            while let Ok(job) = rx.recv() {
                job();
            }
        });
        if let Err(e) = spawned {
            tracing::warn!("scene: worker thread could not start: {}", e);
        }
        Arc::new(Worker { tx: Mutex::new(Some(tx)), closed })
    }

    /// `shutdown(wait=False, cancel_futures=True)`.
    fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.tx.lock().take();
    }

    /// Run `f` on the worker, giving up after `timeout_secs` (`asyncio.wait_for`).
    async fn run<T, F>(&self, timeout_secs: f64, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<std::thread::Result<anyhow::Result<T>>>();
        let cancel = Arc::new(AtomicBool::new(false));
        let job_cancel = cancel.clone();
        let closed = self.closed.clone();
        let job: Job = Box::new(move || {
            if job_cancel.load(Ordering::SeqCst) || closed.load(Ordering::SeqCst) {
                return;
            }
            let _ = done_tx.send(std::panic::catch_unwind(AssertUnwindSafe(f)));
        });
        let sent = match self.tx.lock().as_ref() {
            Some(tx) => tx.send(job).is_ok(),
            None => false,
        };
        if !sent {
            // `run_in_executor` on a shut-down executor.
            return Err(RuntimeError("cannot schedule new futures after shutdown".into()).into());
        }
        let limit = Duration::try_from_secs_f64(timeout_secs.max(0.0)).unwrap_or(Duration::MAX);
        match tokio::time::timeout(limit, done_rx).await {
            Err(_) => {
                cancel.store(true, Ordering::SeqCst);
                Err(TimeoutError.into())
            }
            Ok(Err(_)) => Err(Cancelled.into()),
            Ok(Ok(Err(panic))) => {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                Err(Panicked(msg).into())
            }
            Ok(Ok(Ok(result))) => result,
        }
    }
}

// ---- the watcher --------------------------------------------------------------------------

pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
pub type Wall = Arc<dyn Fn() -> NaiveDateTime + Send + Sync>;
pub type CameraOk = Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CamState {
    Unknown,
    Ok,
    Lost,
}

struct State {
    active: bool,
    latest: Option<Observation>,
    /// this conversation's notes, for tests and the log; cleared on stop
    notes: Vec<String>,
    describes: u64,
    failures: u64,
    frame: Option<Value>,
    frame_at: f64,
    last_describe_at: f64,
    last_note_at: f64,
    camera_state: CamState,
    blind_said: bool,
    force: bool,
    /// the next describe must use a frame newer than this
    min_frame_at: f64,
    busy: bool,
    started_at: f64,
    /// when the current view last changed
    view_since: Option<Observation>,
    /// seen_at of the last view a note reported
    confirmed_at: f64,
    task: Option<JoinHandle<()>>,
    executor: Option<Arc<Worker>>,
}

impl State {
    fn new() -> Self {
        State {
            active: false,
            latest: None,
            notes: Vec::new(),
            describes: 0,
            failures: 0,
            frame: None,
            frame_at: f64::NEG_INFINITY,
            last_describe_at: f64::NEG_INFINITY,
            last_note_at: f64::NEG_INFINITY,
            camera_state: CamState::Unknown,
            blind_said: false,
            force: false,
            min_frame_at: f64::NEG_INFINITY,
            busy: false,
            started_at: f64::NEG_INFINITY,
            view_since: None,
            confirmed_at: f64::NEG_INFINITY,
            task: None,
            executor: None,
        }
    }

    fn reset(&mut self) {
        self.frame = None;
        self.frame_at = f64::NEG_INFINITY;
        self.latest = None;
        self.notes = Vec::new();
        self.describes = 0;
        self.failures = 0;
        self.last_describe_at = f64::NEG_INFINITY;
        self.last_note_at = f64::NEG_INFINITY;
        self.camera_state = CamState::Unknown;
        self.blind_said = false;
        self.force = false;
        self.min_frame_at = f64::NEG_INFINITY;
        self.view_since = None;
        self.confirmed_at = f64::NEG_INFINITY;
    }

    fn usable_frame(&self) -> bool {
        self.frame.is_some() && self.frame_at > self.min_frame_at
    }
}

/// What a describe takes out of the state before it leaves the lock.
struct DescribeJob {
    raw: Value,
    seen_at: f64,
    prev: Option<String>,
    pose_yaw: Option<f64>,
    pose_pitch: Option<f64>,
}

/// Frames in, timestamped notes out. The describe runs on a one-worker thread
/// so the event loop never blocks on the network.
///
/// `client` may be set after construction (the daemon resolves it in
/// run()). `camera_ok` says whether a camera could be streaming at all —
/// the board is connected and the host asked it to stream. The watcher
/// never calls out: `look` and `locate` are the only way a view leaves it.
pub struct SceneWatcher {
    client: Mutex<Option<Arc<dyn SceneClient>>>,
    config: SceneConfig,
    clock: Clock,
    wall: Wall,
    camera_ok: CameraOk,
    tick_secs: f64,
    state: Mutex<State>,
}

impl SceneWatcher {
    /// `SceneWatcher(client, config)` with the real clocks, a camera that is always ok and `TICK_SECS`.
    pub fn new(client: Option<Arc<dyn SceneClient>>, config: Option<SceneConfig>) -> Self {
        SceneWatcher {
            client: Mutex::new(client),
            config: config.unwrap_or_default(),
            clock: Arc::new(eve_vision::monotonic),
            wall: Arc::new(|| chrono::Local::now().naive_local()),
            camera_ok: Arc::new(|| true),
            tick_secs: TICK_SECS,
            state: Mutex::new(State::new()),
        }
    }

    /// The monotonic clock (`clock=`).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The wall clock (`wall=`).
    pub fn with_wall(mut self, wall: Wall) -> Self {
        self.wall = wall;
        self
    }

    /// Whether a camera could be streaming at all (`camera_ok=`).
    pub fn with_camera_ok(mut self, camera_ok: CameraOk) -> Self {
        self.camera_ok = camera_ok;
        self
    }

    /// The loop's step (`tick_secs=`).
    pub fn with_tick_secs(mut self, tick_secs: f64) -> Self {
        self.tick_secs = tick_secs;
        self
    }

    pub fn config(&self) -> &SceneConfig {
        &self.config
    }

    /// The injected monotonic clock's now.
    pub fn clock(&self) -> f64 {
        (self.clock)()
    }

    pub fn client(&self) -> Option<Arc<dyn SceneClient>> {
        self.client.lock().clone()
    }

    /// Set (or clear) the describer after construction.
    pub fn set_client(&self, client: Option<Arc<dyn SceneClient>>) {
        *self.client.lock() = client;
    }

    pub fn enabled(&self) -> bool {
        self.client.lock().is_some() && self.config.enabled
    }

    pub fn active(&self) -> bool {
        self.state.lock().active
    }

    pub fn latest(&self) -> Option<Observation> {
        self.state.lock().latest.clone()
    }

    /// This conversation's notes; cleared on stop.
    pub fn notes(&self) -> Vec<String> {
        self.state.lock().notes.clone()
    }

    pub fn describes(&self) -> u64 {
        self.state.lock().describes
    }

    pub fn failures(&self) -> u64 {
        self.state.lock().failures
    }

    /// The frame waiting to be described (Python's `_frame`), whatever its age.
    pub fn held_frame(&self) -> Option<Value> {
        self.state.lock().frame.clone()
    }

    /// Mark a describe as just done (tests set `_last_describe_at` to hold one off).
    pub fn set_last_describe_at(&self, t: f64) {
        self.state.lock().last_describe_at = t;
    }

    // -- lifecycle --------------------------------------------------------------

    /// A conversation opened: begin watching. No-op when disabled.
    /// `run_loop=false` leaves the stepping to the caller (tests call tick()).
    /// With `run_loop` it must be called inside a tokio runtime.
    pub fn start(self: &Arc<Self>, run_loop: bool) {
        if !self.enabled() {
            return;
        }
        let now = self.clock();
        let mut st = self.state.lock();
        if st.active {
            return;
        }
        st.active = true;
        st.reset();
        st.started_at = now;
        st.executor = Some(Worker::spawn());
        if run_loop {
            let me = self.clone();
            st.task = Some(tokio::spawn(async move { me.run_loop().await }));
        }
    }

    /// The conversation closed: stop, and forget every frame and observation.
    pub async fn stop(&self) {
        let (task, executor) = {
            let mut st = self.state.lock();
            st.active = false;
            (st.task.take(), st.executor.take())
        };
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        if let Some(executor) = executor {
            executor.shutdown();
        }
        let mut st = self.state.lock();
        if st.describes > 0 {
            tracing::info!("scene: {} view(s) described, {} failed", st.describes, st.failures);
        }
        st.reset();
    }

    /// A camera frame off the wire. Kept only while active, and only the newest.
    pub fn offer(&self, raw: Value) {
        let now = self.clock();
        let mut st = self.state.lock();
        if !st.active {
            return;
        }
        st.frame = Some(raw);
        st.frame_at = now;
    }

    /// The newest raw frame, for a photo when the board cannot snap one. None
    /// when there is none, or it is older than the camera-lost window.
    pub fn newest_frame(&self) -> Option<Value> {
        let now = self.clock();
        let st = self.state.lock();
        if st.frame.is_none() || now - st.frame_at > self.config.camera_lost_secs {
            return None;
        }
        st.frame.clone()
    }

    // -- state ------------------------------------------------------------------

    fn lost_with(&self, st: &State, now: f64, camera_ok: bool) -> bool {
        !camera_ok || now - st.frame_at > self.config.camera_lost_secs
    }

    /// "ok" or "lost".
    pub fn camera_state(&self, now: f64) -> &'static str {
        let ok = (self.camera_ok)();
        let st = self.state.lock();
        if self.lost_with(&st, now, ok) {
            "lost"
        } else {
            "ok"
        }
    }

    fn wall_at(&self, t: f64) -> NaiveDateTime {
        let ago = py_max(0.0, self.clock() - t);
        let micros = (ago * 1e6).round_ties_even();
        let delta = if micros.is_finite() && micros.abs() < i64::MAX as f64 {
            chrono::Duration::microseconds(micros as i64)
        } else {
            chrono::Duration::zero()
        };
        (self.wall)() - delta
    }

    /// Journal one line. It goes nowhere else: the voice pulls views with `look`.
    fn emit(&self, st: &mut State, note: String) {
        st.last_note_at = self.clock();
        // The log gets the tag and the kind of line, never what the camera saw.
        let kind = if note.contains("Camera lost") {
            "camera lost"
        } else if note.contains("cannot make out") {
            "cannot see"
        } else if note.contains("No change since") {
            "no change"
        } else {
            "view"
        };
        let tag = note.split_once(']').map_or(note.as_str(), |(head, _)| head);
        tracing::info!("scene: {}] {}", tag, kind);
        st.notes.push(note);
    }

    // -- the loop -----------------------------------------------------------------

    async fn run_loop(self: Arc<Self>) {
        while self.active() {
            self.tick().await;
            tokio::time::sleep(Duration::try_from_secs_f64(self.tick_secs.max(0.0)).unwrap_or(Duration::ZERO)).await;
        }
    }

    /// One step: notice camera loss, maybe describe, maybe repeat 'no change'.
    pub async fn tick(&self) {
        let camera_ok = (self.camera_ok)();
        let now = self.clock();
        let job = {
            let mut st = self.state.lock();
            let lost = self.lost_with(&st, now, camera_ok);
            // The camera gets camera_lost_secs from start() to deliver a first frame
            // before "camera lost" is said: the board may be starting its stream.
            let grace = st.camera_state == CamState::Unknown && now - st.started_at < self.config.camera_lost_secs;
            if lost {
                if st.camera_state != CamState::Lost && !grace {
                    st.camera_state = CamState::Lost;
                    let note = lost_note(&(self.wall)(), st.latest.as_ref());
                    self.emit(&mut st, note);
                }
                return;
            }
            if st.camera_state == CamState::Lost {
                tracing::info!("scene: camera back");
            }
            st.camera_state = CamState::Ok;
            let due = st.force || now - st.last_describe_at >= self.config.interval_secs;
            if due && !st.busy && st.usable_frame() {
                Some(self.begin_describe(&mut st))
            } else {
                let refresh = match (&st.latest, &st.view_since) {
                    (Some(latest), Some(since))
                        if !st.busy
                            && latest.seen_at > st.confirmed_at
                            && now - st.last_note_at >= self.config.refresh_secs =>
                    {
                        Some((latest.seen_at, unchanged_note(latest, since)))
                    }
                    _ => None,
                };
                if let Some((seen_at, note)) = refresh {
                    st.confirmed_at = seen_at;
                    self.emit(&mut st, note);
                }
                None
            }
        };
        if let Some(job) = job {
            self.describe(job).await;
        }
    }

    fn begin_describe(&self, st: &mut State) -> DescribeJob {
        // released: the describer holds the only copy now
        let raw = st.frame.take().unwrap_or(Value::Null);
        let seen_at = st.frame_at;
        st.force = false;
        st.min_frame_at = f64::NEG_INFINITY;
        st.busy = true;
        st.last_describe_at = self.clock();
        let prev = st.latest.as_ref().map(|o| o.text.clone());
        let pose_yaw = opt_float(raw.get("yaw"));
        let pose_pitch = opt_float(raw.get("pitch"));
        DescribeJob { raw, seen_at, prev, pose_yaw, pose_pitch }
    }

    async fn describe(&self, job: DescribeJob) {
        let DescribeJob { raw, seen_at, prev, pose_yaw, pose_pitch } = job;
        let result = self.call_describe(raw, prev.clone()).await;
        let mut st = self.state.lock();
        let (text, change, latency) = match result {
            Ok(v) => v,
            // stop() dropped the queued call: Python's CancelledError is re-raised out of
            // `_describe`, leaving every field (the busy flag included) as it is.
            Err(e) if e.downcast_ref::<Cancelled>().is_some() => return,
            Err(e) => {
                st.failures += 1;
                if st.failures == 1 || st.failures.is_multiple_of(10) {
                    tracing::warn!(
                        "scene: describe failed ({} so far): {}: {}",
                        st.failures,
                        error_name(&e),
                        error_text(&e)
                    );
                }
                st.busy = false;
                self.maybe_blind(&mut st);
                return;
            }
        };
        st.busy = false;
        st.describes += 1;
        st.blind_said = false;
        let obs = Observation {
            seen_at,
            seen_wall: self.wall_at(seen_at),
            text,
            change: if prev.is_some() { change } else { String::new() },
            latency_secs: latency,
            yaw: pose_yaw,
            pitch: pose_pitch,
        };
        let first = st.latest.is_none();
        st.latest = Some(obs.clone());
        if first || !obs.change.is_empty() {
            // the view as it has been since this moment
            st.view_since = Some(obs.clone());
            st.confirmed_at = obs.seen_at;
            self.emit(&mut st, format_note(&obs));
        }
        if latency > self.config.interval_secs {
            tracing::info!("scene: describe took {:.1} s (interval {:.1} s)", latency, self.config.interval_secs);
        }
    }

    /// decode → image → the client on the worker, bounded by the timeout. (text, change, latency).
    async fn call_describe(&self, raw: Value, prev: Option<String>) -> anyhow::Result<(String, String, f64)> {
        let frame = decode_frame(&raw)?;
        drop(raw);
        let (image, mime) = frame_image(&frame)?;
        drop(frame);
        let client = self.client();
        let executor = self.state.lock().executor.clone();
        let (Some(client), Some(executor)) = (client, executor) else {
            return Err(AssertionError.into());
        };
        let t0 = self.clock();
        let (text, change) = executor
            .run(self.config.timeout_secs, move || client.describe(&image, mime, prev.as_deref()))
            .await?;
        Ok((text, change, self.clock() - t0))
    }

    /// Describes keep failing and the last view has aged out: say so, once.
    fn maybe_blind(&self, st: &mut State) {
        let now = self.clock();
        if st.blind_said {
            return;
        }
        let stale = match &st.latest {
            None => true,
            Some(last) => now - last.seen_at > self.config.stale_secs,
        };
        if stale {
            st.blind_said = true;
            let note = blind_note(&(self.wall)(), st.latest.as_ref());
            self.emit(st, note);
        }
    }

    // -- on demand ------------------------------------------------------------------

    fn unavailable(&self) -> Option<Map<String, Value>> {
        let reason = if !self.enabled() {
            "vision is off on this computer (no API key or EVE_SCENE=0)"
        } else if !self.active() {
            "vision only runs during a conversation"
        } else {
            return None;
        };
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(false));
        out.insert("reason".into(), Value::String(reason.into()));
        Some(out)
    }

    /// The current view, refreshed if it is older than `fresh_secs` (None: `LOOK_FRESH_SECS`) — or,
    /// with `newer_than`, from a frame that arrived after that moment (the head has just
    /// turned). Never waits longer than one describe timeout.
    pub async fn look(&self, fresh_secs: Option<f64>, newer_than: Option<f64>) -> Map<String, Value> {
        if let Some(off) = self.unavailable() {
            return off;
        }
        let fresh_secs = fresh_secs.unwrap_or(LOOK_FRESH_SECS);
        if self.camera_state(self.clock()) == "lost" {
            return self.answer(false, Some(NO_CAMERA));
        }
        let floor = newer_than.unwrap_or(f64::NEG_INFINITY);
        let good = |me: &Self| -> bool {
            let now = me.clock();
            let st = me.state.lock();
            match &st.latest {
                Some(last) if last.seen_at > floor => newer_than.is_some() || now - last.seen_at <= fresh_secs,
                _ => false,
            }
        };
        if good(self) {
            return self.answer(true, None);
        }
        // Ask the loop for a describe now (of a frame newer than `floor`) and wait
        // until that attempt has finished, one way or the other. The wait is bounded
        // on the runtime's own clock, never the injectable one.
        let attempts = {
            let mut st = self.state.lock();
            st.force = true;
            st.min_frame_at = floor;
            st.describes + st.failures
        };
        let wait = Duration::try_from_secs_f64((self.config.timeout_secs + self.config.camera_lost_secs).max(0.0))
            .unwrap_or(Duration::MAX);
        let deadline = Instant::now().checked_add(wait);
        while deadline.is_none_or(|d| Instant::now() < d) && !good(self) {
            {
                let st = self.state.lock();
                if st.describes + st.failures > attempts && !st.force && !st.busy {
                    break; // the forced attempt is done; it failed or was too old
                }
                if st.camera_state == CamState::Lost {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_secs_f64(FRAME_WAIT_POLL_SECS)).await;
        }
        if self.camera_state(self.clock()) == "lost" {
            return self.answer(false, Some(NO_CAMERA));
        }
        if good(self) {
            return self.answer(true, None);
        }
        let done = {
            let st = self.state.lock();
            st.describes + st.failures > attempts
        };
        if done {
            return self.answer(false, Some("could not make out the view"));
        }
        self.answer(false, Some("no new view came in time"))
    }

    /// Is `target` in view? Uses the newest frame (one taken after `newer_than`
    /// when given). {"ok", "visible", "x", "y", "what", "yaw", "pitch"}.
    pub async fn locate(&self, target: &str, newer_than: Option<f64>) -> Map<String, Value> {
        if let Some(off) = self.unavailable() {
            return off;
        }
        let refusal = |reason: &str| {
            let mut out = Map::new();
            out.insert("ok".into(), Value::Bool(false));
            out.insert("reason".into(), Value::String(reason.into()));
            out
        };
        let floor = newer_than.unwrap_or(f64::NEG_INFINITY);
        let deadline = Instant::now().checked_add(
            Duration::try_from_secs_f64(self.config.camera_lost_secs.max(0.0)).unwrap_or(Duration::MAX),
        );
        let raw = loop {
            {
                let st = self.state.lock();
                if let Some(frame) = &st.frame {
                    if st.frame_at > floor {
                        break frame.clone();
                    }
                }
            }
            if !(self.camera_ok)() {
                return refusal(NO_CAMERA);
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                if self.camera_state(self.clock()) == "lost" {
                    return refusal(NO_CAMERA);
                }
                return refusal("no new frame came in time");
            }
            tokio::time::sleep(Duration::from_secs_f64(FRAME_WAIT_POLL_SECS)).await;
        };
        let pose_yaw = opt_float(raw.get("yaw"));
        let pose_pitch = opt_float(raw.get("pitch"));
        let found = match self.call_locate(raw, target).await {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(
                    "scene: locate {} failed: {}: {}",
                    py_repr_str(target),
                    error_name(&e),
                    error_text(&e)
                );
                return refusal("could not check the view");
            }
        };
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(true));
        let visible = found.get("visible").is_some_and(py_truthy);
        let coord = |k: &str| found.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let said = if visible {
            format!("visible at x={:.0} y={:.0}", coord("x"), coord("y"))
        } else {
            "not visible".to_string()
        };
        for (k, v) in found {
            out.insert(k, v);
        }
        out.insert("yaw".into(), pose_yaw.map_or(Value::Null, |v| json!(v)));
        out.insert("pitch".into(), pose_pitch.map_or(Value::Null, |v| json!(v)));
        tracing::info!("scene: locate {} → {}", py_repr_str(target), said);
        out
    }

    async fn call_locate(&self, raw: Value, target: &str) -> anyhow::Result<Map<String, Value>> {
        let frame = decode_frame(&raw)?;
        drop(raw);
        let (image, mime) = frame_image(&frame)?;
        drop(frame);
        let client = self.client();
        let executor = self.state.lock().executor.clone();
        let (Some(client), Some(executor)) = (client, executor) else {
            return Err(AssertionError.into());
        };
        let target = target.to_string();
        executor.run(self.config.timeout_secs, move || client.locate(&image, mime, &target)).await
    }

    fn answer(&self, ok: bool, reason: Option<&str>) -> Map<String, Value> {
        let now = self.clock();
        let st = self.state.lock();
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(ok));
        if let Some(last) = &st.latest {
            let age = now - last.seen_at;
            out.insert("view".into(), Value::String(last.text.clone()));
            out.insert("seen_at".into(), Value::String(hms(&last.seen_wall)));
            out.insert("age_secs".into(), json!(round1(age)));
            out.insert("stale".into(), Value::Bool(age > self.config.stale_secs));
        }
        if let Some(reason) = reason {
            out.insert("reason".into(), Value::String(reason.into()));
        } else if st.latest.is_none() {
            out.insert("reason".into(), Value::String("no clear view yet".into()));
        }
        out
    }
}

/// Python's `round(x, 1)`: correctly rounded, ties to even on the exact binary value.
fn round1(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.1}").parse().unwrap_or(x)
}

// ---- the network --------------------------------------------------------------------------

/// The OpenRouter blocking client, built on first use. reqwest's blocking client
/// panics when it is built on an async runtime thread, and the daemon makes the
/// scene client from async code; the first call runs on the watcher's worker
/// thread, where building it is allowed. The key is checked up front.
struct LazyOpenRouter {
    api_key: String,
    opts: eve_openrouter::ClientOptions,
    client: OnceLock<eve_openrouter::BlockingClient>,
}

impl Responses for LazyOpenRouter {
    fn create(&self, body: Value) -> anyhow::Result<Value> {
        let client = match self.client.get() {
            Some(c) => c,
            None => {
                let built = eve_openrouter::client(Some(&self.api_key), self.opts.clone())?;
                self.client.get_or_init(|| built)
            }
        };
        Ok(client.responses_create(body)?)
    }
}

/// How the SDK's typed reply prints `status` and `incomplete_details` in an f-string.
fn py_status(resp: &Value) -> String {
    match resp.get("status") {
        Some(Value::String(s)) => s.clone(),
        None | Some(Value::Null) => "None".into(),
        Some(other) => other.to_string(),
    }
}

fn py_incomplete(resp: &Value) -> String {
    match resp.get("incomplete_details") {
        None | Some(Value::Null) => "None".into(),
        Some(Value::Object(m)) => {
            let reason = match m.get("reason") {
                Some(Value::String(s)) => py_repr_str(s),
                _ => "None".into(),
            };
            format!("IncompleteDetails(reason={reason})")
        }
        Some(other) => other.to_string(),
    }
}

/// Responses API: one low-detail `input_image`, a strict JSON answer, and
/// `store=False` so nothing is retained on OpenAI's side for later retrieval.
/// Retries off: a slow or failed view is skipped, the next frame comes soon.
pub struct OpenAISceneClient {
    pub model: String,
    client: Box<dyn Responses>,
    ledger: Ledger,
}

impl OpenAISceneClient {
    /// The OpenRouter client with `timeout` seconds and no retries. Errs with `NoKey` when no key is found.
    pub fn new(model: &str, api_key: Option<&str>, timeout: f64) -> Result<Self, eve_openrouter::Error> {
        let opts = eve_openrouter::ClientOptions::default()
            .timeout(Duration::try_from_secs_f64(timeout.max(0.0)).unwrap_or(Duration::MAX))
            .max_retries(0);
        Self::with_options(model, api_key, opts)
    }

    /// As `new`, with the client options given in full (tests point `base_url` at a local server).
    pub fn with_options(
        model: &str,
        api_key: Option<&str>,
        opts: eve_openrouter::ClientOptions,
    ) -> Result<Self, eve_openrouter::Error> {
        let api_key = eve_openrouter::key_or_raise(api_key, opts.env.as_ref())?;
        let lazy = LazyOpenRouter { api_key, opts, client: OnceLock::new() };
        Ok(Self::with_responses(model, Box::new(lazy)))
    }

    /// Any `responses.create` (the Python tests swap `_client` for a fake SDK).
    pub fn with_responses(model: &str, client: Box<dyn Responses>) -> Self {
        OpenAISceneClient { model: model.to_string(), client, ledger: Ledger::default() }
    }

    /// Meter into this ledger instead of the configured spend folder.
    pub fn with_ledger(mut self, ledger: Ledger) -> Self {
        self.ledger = ledger;
        self
    }

    fn body(&self, prompt: &str, image: &[u8], mime: &str, name: &str, schema: Value) -> Value {
        json!({
            "model": self.model,
            "input": [{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": prompt},
                    {"type": "input_image", "image_url": data_url(image, mime), "detail": "low"},
                ],
            }],
            "text": {"format": {"type": "json_schema", "name": name, "strict": true, "schema": schema}},
            "max_output_tokens": MAX_OUTPUT_TOKENS,
            "reasoning": {"effort": "low"},
            "store": false,
        })
    }

    /// The exact describe request body (tests check store=False and the image part).
    pub fn request(&self, image: &[u8], mime: &str, previous: Option<&str>) -> Value {
        let prompt = match previous {
            Some(p) if !p.is_empty() => format!("{PROMPT}\nPrevious description: {p}"),
            _ => format!("{PROMPT}\nThere is no previous description; set change to an empty string."),
        };
        self.body(&prompt, image, mime, "scene", schema())
    }

    pub fn locate_request(&self, image: &[u8], mime: &str, target: &str) -> Value {
        let target: String = collapse_ws(target).chars().take(80).collect();
        self.body(&format!("{LOCATE_PROMPT}{target}"), image, mime, "locate", locate_schema())
    }

    /// One call, metered as camera spend; the answer's text, or RuntimeError when it is blank.
    fn answer_text(&self, body: Value) -> anyhow::Result<String> {
        let resp = self.client.create(body)?;
        self.ledger.record_response(eve_spend::CAMERA, &resp, &self.model, eve_openrouter::PROVIDER, None);
        let text = output_text(&resp);
        if text.trim_matches(py_is_space).is_empty() {
            return Err(RuntimeError(format!(
                "empty answer (status={}, incomplete={})",
                py_status(&resp),
                py_incomplete(&resp)
            ))
            .into());
        }
        Ok(text)
    }
}

impl SceneClient for OpenAISceneClient {
    fn describe(&self, image: &[u8], mime: &str, previous: Option<&str>) -> anyhow::Result<(String, String)> {
        let text = self.answer_text(self.request(image, mime, previous))?;
        Ok(parse_scene(&text)?)
    }

    fn locate(&self, image: &[u8], mime: &str, target: &str) -> anyhow::Result<Map<String, Value>> {
        let text = self.answer_text(self.locate_request(image, mime, target))?;
        parse_locate(&text)
    }
}

/// The real client, or None (with one log line) when vision cannot run. `None` env reads the process environment.
pub fn make_scene_client(
    config: &SceneConfig,
    environ: Option<&HashMap<String, String>>,
) -> Option<Arc<dyn SceneClient>> {
    if !config.enabled {
        tracing::info!("scene: off (EVE_SCENE=0) — eve cannot describe what it sees");
        return None;
    }
    let key = eve_openrouter::key(environ);
    if key.is_empty() {
        tracing::warn!("scene: OPENROUTER_API_KEY not set — eve cannot describe what it sees");
        return None;
    }
    let client = match OpenAISceneClient::new(&config.model, Some(&key), config.timeout_secs) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("scene: client could not be built ({}) — no scene descriptions", e);
            return None;
        }
    };
    tracing::info!(
        "scene: describing camera views with {} during conversations (every {:.0} s at most)",
        config.model,
        config.interval_secs
    );
    Some(Arc::new(client))
}

// ---- bench CLI ----------------------------------------------------------------------------

/// The image type by its magic bytes: JPEG, PNG, or None.
pub fn mime_for(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\xff\xd8") {
        return Some("image/jpeg");
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    None
}

/// `str(OSError)` as Python prints it for a failed `read_bytes`.
fn py_os_error(e: &std::io::Error, path: &str) -> String {
    match e.raw_os_error() {
        Some(code) => {
            let text = e.to_string();
            let suffix = format!(" (os error {code})");
            let msg = text.strip_suffix(&suffix).unwrap_or(&text);
            format!("[Errno {code}] {msg}: {}", py_repr_str(path))
        }
        None => e.to_string(),
    }
}

/// `eve-bridge scene-test <image> [--find thing]`: one real describe (and
/// locate), timed. Prints `scene-test ok (<model>, <ms> ms)` on success.
pub fn run_scene_test(path: &str, find: Option<&str>) -> i32 {
    let cfg = configured(None);
    let client = make_scene_client(
        &SceneConfig { model: cfg.model.clone(), timeout_secs: cfg.timeout_secs, ..SceneConfig::default() },
        None,
    );
    let Some(client) = client else {
        eprintln!("scene-test: no client (OPENROUTER_API_KEY not set?)");
        return 2;
    };
    let data = match std::fs::read(Path::new(path)) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("scene-test: cannot read {path}: {}", py_os_error(&e, path));
            return 2;
        }
    };
    let Some(mime) = mime_for(&data) else {
        eprintln!("scene-test: {path} is not a JPEG or PNG");
        return 2;
    };
    // The blocking HTTP client must not be built or used on an async runtime thread.
    std::thread::scope(|s| {
        s.spawn(|| bench(client.as_ref(), &cfg, &data, mime, find)).join().unwrap_or(1)
    })
}

fn bench(client: &dyn SceneClient, cfg: &SceneConfig, data: &[u8], mime: &str, find: Option<&str>) -> i32 {
    let t0 = Instant::now();
    let (scene, change) = match client.describe(data, mime, None) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("scene-test: describe failed: {}: {}", error_name(&e), error_text(&e));
            return 1;
        }
    };
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    if ms > cfg.timeout_secs * 1000.0 {
        eprintln!("scene-test: answer took {:.0} ms, over the {:.0} s timeout", ms, cfg.timeout_secs);
        return 1;
    }
    println!("scene: {scene}");
    println!("change: {}", if change.is_empty() { "(none)" } else { change.as_str() });
    if let Some(find) = find.filter(|f| !f.is_empty()) {
        let t1 = Instant::now();
        let loc = match client.locate(data, mime, find) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("scene-test: locate failed: {}: {}", error_name(&e), error_text(&e));
                return 1;
            }
        };
        println!(
            "locate {}: {} ({:.0} ms)",
            py_repr_str(find),
            py_repr(&Value::Object(loc)),
            t1.elapsed().as_secs_f64() * 1000.0
        );
    }
    println!("scene-test ok ({}, {:.0} ms)", cfg.model, ms);
    0
}

#[cfg(test)]
mod tests;
