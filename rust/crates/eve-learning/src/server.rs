//! Loopback-only learning workspace. Run with `eve-learning --demo` (Python: `python -m eve_bridge.learning --demo`).
//!
//! [`LearningApp`] is the lesson state machine (Python's `LearningApp`); [`start`] serves it on 127.0.0.1 with
//! axum (Python's `ThreadingHTTPServer` + `Handler`) on a thread of its own, so it can be started from the
//! daemon or from the standalone launcher [`main`]. Every request runs on a blocking thread, as each Python
//! request has its own thread: a live tutor call takes up to 90 s.

use std::io;
use std::net::TcpListener as StdListener;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::thread::JoinHandle;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use base64::Engine as _;
use eve_web::Ctx;
use parking_lot::{Mutex, MutexGuard, RwLock};
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::py::{self, as_int, as_num, char_len, dumps, get_str, merged, py_eq, py_rstrip, py_str, py_strip, take_chars, truthy};
use crate::store::{self, Store};
use crate::think_aloud::{listen_refusal, LISTEN_ACTIONS, MAX_SPOKEN_IDEA};
use crate::tutor::{live_settings, DemoTutor, LiveTutor, Tutor};
use crate::{LearningError, LessonRequest, ROBOT_APP_NOT_RUNNING};

pub const DEFAULT_PORT: u16 = 48766;
pub const MAX_BODY: usize = 12 * 1024 * 1024;
pub const MAX_BOARD: usize = 4 * 1024 * 1024;
/// Spoken lines kept per lesson so a browser save that missed them can put them back.
pub const SPOKEN_KEEP: usize = 50;
pub const NONCE_SLOT: &[u8] = b"__EVE_NONCE__";
const MAX_TEXT: usize = 20000;
const MAX_IMAGE: usize = 6 * 1024 * 1024;

mod web_assets {
    include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
}

/// One file of `learning/web/` (the page, `fonts/...`, `canvas/...`), embedded at build time.
pub fn asset(name: &str) -> Option<&'static [u8]> {
    web_assets::FILES.binary_search_by(|(n, _)| (*n).cmp(name)).ok().map(|i| web_assets::FILES[i].1)
}

// Fonts are bundled (SIL OFL, see web/fonts/OFL.txt) because the page CSP is same-origin only.
static FONT_FILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9-]+\.woff2$").expect("font regex"));
// The whiteboard is tldraw, built from bridge/web-canvas into web/canvas (see docs/learning.md). Its fonts, icons
// and translations are served from here too, for the same reason as the fonts above.
static CANVAS_FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[A-Za-z0-9_-]+/)*[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)*$").expect("canvas regex")
});

/// The served type of a canvas file by its suffix.
pub fn canvas_mime(suffix: &str) -> Option<&'static str> {
    Some(match suffix {
        ".js" => "text/javascript; charset=utf-8",
        ".css" => "text/css; charset=utf-8",
        ".svg" => "image/svg+xml",
        ".woff2" => "font/woff2",
        ".png" => "image/png",
        // Not the bare "application/json": that one is encoded as a reply, and these are files.
        ".json" => "application/json; charset=utf-8",
        ".md" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

/// `pathlib.Path(name).suffix`.
fn path_suffix(name: &str) -> &str {
    let last = name.split('/').rfind(|c| !c.is_empty() && *c != ".").unwrap_or("");
    match last.rfind('.') {
        Some(i) if i > 0 && i < last.len() - 1 => &last[i..],
        _ => "",
    }
}

/// Same-origin only. Two allowances exist for the tldraw whiteboard, both as narrow as they can be:
///  - img-src blob: tldraw shows and exports pictures through blob: URLs. Only a script already running in this
///    page can mint one, so it opens nothing to another origin.
///  - a per-page-load nonce in style-src: tldraw adds <style> elements at run time (its licence watermark, and
///    the fonts it embeds in a board picture) and stamps them with the nonce it is given. No 'unsafe-inline':
///    this page puts tutor and search text into innerHTML, and the policy is what stands behind that escaping.
pub fn content_policy(nonce: &str) -> String {
    let style = if nonce.is_empty() { "style-src 'self'".to_string() } else { format!("style-src 'self' 'nonce-{nonce}'") };
    format!("default-src 'self'; img-src 'self' data: blob:; {style}; script-src 'self'; font-src 'self'; frame-ancestors 'none'")
}

pub static CSP: LazyLock<String> = LazyLock::new(|| content_policy(""));

/// `{"available": False, "state": "off", "lesson_id": None}`.
pub fn listen_off() -> Map<String, Value> {
    obj(json!({"available": false, "state": "off", "lesson_id": null}))
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn err(msg: &str) -> LearningError {
    LearningError::Value(msg.to_string())
}

/// `base64.b64decode(text, validate=True)`: only the base64 alphabet with at most two trailing `=`, then
/// binascii's lenient decode (padding may stop it early; a missing pad is "Incorrect padding").
fn b64decode_validate(text: &str) -> Result<Vec<u8>, ()> {
    let body = text.trim_end_matches('=');
    if text.len() - body.len() > 2 || !body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/') {
        return Err(());
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut quad, mut pads) = (0u32, 0u8, 0u8);
    for b in text.bytes() {
        if b == b'=' {
            if quad >= 2 {
                pads += 1;
                if quad + pads >= 4 {
                    return Ok(out);
                }
            }
            continue;
        }
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            _ => 63,
        } as u32;
        acc = (acc << 6) | v;
        quad = (quad + 1) % 4;
        match quad {
            2 => out.push((acc >> 4) as u8),
            3 => out.push((acc >> 2) as u8),
            0 => out.push(acc as u8),
            _ => {}
        }
        acc &= 0xff;
    }
    if quad != 0 {
        return Err(());
    }
    Ok(out)
}

const WORK_KEYS: [&str; 7] = ["ideas", "problem", "stuck", "source_image", "board_image", "strokes", "board"];

/// What Python's `validate_work` does with a `work` that is not a dict: `key in data` is a membership test (a
/// substring test on a string), and a hit then fails `data[key]`; no hit is an empty save.
fn not_a_work_dict(data: &Value) -> Result<Map<String, Value>, LearningError> {
    let type_error = |m: &str| Err(LearningError::Type(m.to_string()));
    match data {
        Value::Null => type_error("argument of type 'NoneType' is not iterable"),
        Value::Bool(_) => type_error("argument of type 'bool' is not iterable"),
        Value::Number(n) if n.is_f64() => type_error("argument of type 'float' is not iterable"),
        Value::Number(_) => type_error("argument of type 'int' is not iterable"),
        Value::Array(items) if items.iter().any(|v| v.as_str().is_some_and(|s| WORK_KEYS.contains(&s))) => {
            type_error("list indices must be integers or slices, not str")
        }
        Value::String(text) if WORK_KEYS.iter().any(|k| text.contains(k)) => {
            type_error("string indices must be integers, not 'str'")
        }
        _ => Ok(Map::new()),
    }
}

/// The fields of a save the browser may change, checked. Unknown keys are dropped.
pub fn validate_work(data: &Value) -> Result<Map<String, Value>, LearningError> {
    let Value::Object(data) = data else { return not_a_work_dict(data) };
    let mut result = Map::new();
    for key in ["ideas", "problem"] {
        if let Some(v) = data.get(key) {
            match v {
                Value::String(s) if char_len(s) <= MAX_TEXT => {
                    result.insert(key.into(), v.clone());
                }
                _ => return Err(err("Text is too long.")),
            }
        }
    }
    if let Some(v) = data.get("stuck") {
        result.insert("stuck".into(), Value::Bool(truthy(v)));
    }
    for key in ["source_image", "board_image"] {
        if let Some(value) = data.get(key) {
            let Value::String(text) = value else { return Err(err("Image must be smaller than 4 MB.")) };
            if char_len(text) > MAX_IMAGE {
                return Err(err("Image must be smaller than 4 MB."));
            }
            if !text.is_empty() {
                let (prefix, encoded) = match text.split_once(',') {
                    Some(parts) => parts,
                    None => return Err(err("Use a PNG, JPEG or WebP image.")),
                };
                if !["data:image/png;base64", "data:image/jpeg;base64", "data:image/webp;base64"].contains(&prefix) {
                    return Err(err("Use a PNG, JPEG or WebP image."));
                }
                let raw = b64decode_validate(encoded).map_err(|_| err("Invalid image data."))?;
                if !(raw.starts_with(b"\x89PNG\r\n\x1a\n") || raw.starts_with(b"\xff\xd8\xff") || raw.starts_with(b"RIFF")) {
                    return Err(err("Invalid image file."));
                }
            }
            result.insert(key.into(), value.clone());
        }
    }
    if let Some(strokes) = data.get("strokes") {
        let Value::Array(list) = strokes else { return Err(err("Whiteboard is too large. Start another problem.")) };
        if list.len() > 10000 {
            return Err(err("Whiteboard is too large. Start another problem."));
        }
        for stroke in list {
            let tool = stroke.get("tool").and_then(Value::as_str);
            let Some(stroke) = stroke.as_object().filter(|_| matches!(tool, Some("pen" | "eraser" | "text"))) else {
                return Err(err("Invalid drawing stroke."));
            };
            let points = match stroke.get("points") {
                Some(Value::Array(p)) if p.len() <= 10000 => p,
                _ => return Err(err("Invalid drawing points.")),
            };
            for p in points {
                let ok = match p {
                    Value::Array(xy) if xy.len() == 2 => xy.iter().all(|v| match v {
                        Value::Number(n) => n.as_f64().is_some_and(|f| (0.0..=1.0).contains(&f)),
                        _ => false,
                    }),
                    _ => false,
                };
                if !ok {
                    return Err(err("Invalid drawing coordinates."));
                }
            }
            // A text box is a stroke with one anchor point and its words. Pen and eraser strokes carry no text.
            if tool == Some("text") {
                match stroke.get("text") {
                    Some(Value::String(t)) if !py_strip(t).is_empty() && char_len(t) <= 500 => {}
                    _ => return Err(err("Invalid text box. Keep it under 500 characters.")),
                }
                if points.len() != 1 {
                    return Err(err("Invalid text box position."));
                }
            } else if stroke.contains_key("text") {
                return Err(err("Invalid drawing stroke."));
            }
        }
        result.insert("strokes".into(), strokes.clone());
    }
    if let Some(board) = data.get("board") {
        // The tldraw document. tldraw validates and migrates its own records when it loads one; the server keeps
        // the shape of the envelope and the size honest.
        if !board.is_null() {
            let envelope_ok = match board {
                Value::Object(b) => match (b.get("store"), b.get("schema")) {
                    (Some(Value::Object(store)), Some(Value::Object(_))) => store.values().all(Value::is_object),
                    _ => false,
                },
                _ => false,
            };
            if !envelope_ok {
                return Err(err("Invalid whiteboard."));
            }
            if char_len(&dumps(board, false)) > MAX_BOARD {
                return Err(err("Whiteboard is too large. Remove a pasted image, or start another problem."));
            }
        }
        result.insert("board".into(), board.clone());
    }
    Ok(result)
}

/// How much the learner has put on the board: tldraw shapes, plus pen strokes from a lesson saved before tldraw.
pub fn mark_count(lesson: &Map<String, Value>) -> usize {
    let shapes = match lesson.get("board") {
        Some(Value::Object(board)) => match board.get("store") {
            Some(Value::Object(store)) => {
                store.values().filter(|r| r.get("typeName").and_then(Value::as_str) == Some("shape")).count()
            }
            _ => 0,
        },
        _ => 0,
    };
    let strokes = match lesson.get("strokes") {
        Some(Value::Array(s)) => s.len(),
        _ => 0,
    };
    shapes + strokes
}

/// Put back spoken lines saved after revision `base` that `ideas` does not contain.
///
/// A browser that saves typed ideas can miss a line eve appended a moment earlier. Only lines newer than the text
/// the browser last loaded are restored, so a learner can still delete an older line.
pub fn merge_spoken(ideas: &str, lesson: &Map<String, Value>, base: f64) -> String {
    let spoken = match lesson.get("spoken") {
        Some(Value::Array(s)) => s.as_slice(),
        _ => &[],
    };
    let missing: Vec<&str> = spoken
        .iter()
        .filter(|e| e.get("rev").and_then(as_num).is_some_and(|r| r > base))
        .filter_map(|e| e.get("text").and_then(Value::as_str))
        .filter(|t| !ideas.contains(t))
        .collect();
    if missing.is_empty() {
        return ideas.to_string();
    }
    let mut parts = vec![py_rstrip(ideas)];
    parts.extend(missing);
    parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n")
}

/// A lesson (or a tutor answer): Python's free-form dict.
pub type Lesson = Map<String, Value>;

/// The daemon's microphone, for think out loud. `None` on the app when this server runs without the daemon.
pub trait Listener: Send + Sync {
    fn start(&self, lesson: &Map<String, Value>) -> anyhow::Result<Map<String, Value>>;
    fn stop(&self) -> anyhow::Result<Map<String, Value>>;
    fn status(&self) -> anyhow::Result<Map<String, Value>>;
}

/// The robot notification after a lesson action: (feedback, stage). An error is logged; the lesson is saved.
pub type Notify = Arc<dyn Fn(&str, &str) -> anyhow::Result<()> + Send + Sync>;
/// `webbrowser.open(url)`.
pub type Opener = Arc<dyn Fn(&str) + Send + Sync>;

/// `webbrowser.open`: the system's opener (`open` on macOS, `xdg-open` elsewhere, `start` on Windows).
pub fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Ok(mut child) = cmd.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// How a [`LearningApp`] (and a server) is made: Python's `LearningApp(directory, demo, tutor, notify, listener)`
/// and `start(directory, demo, port, notify, listener)`, plus the seams Python reached through globals.
#[derive(Clone)]
pub struct StartOptions {
    /// None: `default_dir()` (EVE_LEARNING_DIR).
    pub directory: Option<PathBuf>,
    pub demo: bool,
    pub port: u16,
    /// None: DemoTutor when `demo`, else a LiveTutor over `ctx`.
    pub tutor: Option<Arc<dyn Tutor>>,
    pub notify: Option<Notify>,
    pub listener: Option<Arc<dyn Listener>>,
    /// The environment (live settings, the tldraw licence key), TinyFish's HTTP and the spend ledger.
    pub ctx: Ctx,
    pub opener: Opener,
}

impl Default for StartOptions {
    fn default() -> Self {
        StartOptions {
            directory: None,
            demo: false,
            port: DEFAULT_PORT,
            tutor: None,
            notify: None,
            listener: None,
            ctx: Ctx::global().clone(),
            opener: Arc::new(open_in_browser),
        }
    }
}

pub struct LearningApp {
    pub store: Store,
    tutor: RwLock<Arc<dyn Tutor>>,
    pub demo: bool,
    notify: Option<Notify>,
    /// The daemon's microphone, for think out loud: start(lesson), stop() and status(). None when this server
    /// runs without the daemon.
    listener: Option<Arc<dyn Listener>>,
    lock: Mutex<()>,
    active: Mutex<Option<String>>,
    url: RwLock<String>,
    ctx: Ctx,
    opener: Opener,
}

impl LearningApp {
    pub fn new(opts: StartOptions) -> Result<LearningApp, LearningError> {
        let directory = opts.directory.clone().unwrap_or_else(|| {
            store::default_dir_from(opts.ctx.var("EVE_LEARNING_DIR").as_deref())
        });
        let tutor: Arc<dyn Tutor> = match opts.tutor {
            Some(t) => t,
            None if opts.demo => Arc::new(DemoTutor),
            None => Arc::new(LiveTutor { ctx: opts.ctx.clone(), ..LiveTutor::default() }),
        };
        Ok(LearningApp {
            store: Store::new(directory)?,
            demo: tutor.demo(),
            tutor: RwLock::new(tutor),
            notify: opts.notify,
            listener: opts.listener,
            lock: Mutex::new(()),
            active: Mutex::new(None),
            url: RwLock::new(String::new()),
            ctx: opts.ctx,
            opener: opts.opener,
        })
    }

    /// The page's address, `http://127.0.0.1:<port>/` once a server serves this app.
    pub fn url(&self) -> String {
        self.url.read().clone()
    }

    pub fn set_url(&self, url: &str) {
        *self.url.write() = url.to_string();
    }

    /// The lesson the windows and the voice act on when a request names none.
    pub fn active(&self) -> Option<String> {
        self.active.lock().clone()
    }

    fn set_active(&self, id: Option<String>) {
        *self.active.lock() = id;
    }

    /// Replace the tutor (Python's tests assign `app.tutor`). The lesson mode (`demo`) stays.
    pub fn set_tutor(&self, tutor: Arc<dyn Tutor>) {
        *self.tutor.write() = tutor;
    }

    pub fn has_listener(&self) -> bool {
        self.listener.is_some()
    }

    /// Call the tutor with the lesson lock released, then return (result, the lesson as it is now).
    ///
    /// A live tutor call takes up to 90 s, and holding the lock across it stalled every autosave and every spoken
    /// line for that long. The tutor answers from a copy; its answer is applied to the lesson re-read under the
    /// lock, so a save or a spoken append made meanwhile is kept. A lesson that moved to another stage (ended,
    /// completed, deleted) meanwhile is refused instead.
    fn ask_tutor(
        &self,
        guard: &mut MutexGuard<'_, ()>,
        action: &str,
        s: &Map<String, Value>,
    ) -> Result<(Lesson, Lesson), LearningError> {
        let stage = s.get("stage").cloned().unwrap_or(Value::Null);
        let snapshot = s.clone();
        let tutor = self.tutor.read().clone();
        let result = MutexGuard::unlocked(guard, || tutor.call(action, &snapshot))?;
        let fresh = self.store.get(&lesson_id(s)?)?;
        if !py_eq(fresh.get("stage").unwrap_or(&Value::Null), &stage) {
            return Err(err("This lesson changed in another window. Reload it before continuing."));
        }
        Ok((result, fresh))
    }

    fn event(s: &mut Map<String, Value>, action: &str, result: &Map<String, Value>) -> Result<(), LearningError> {
        let mut e = Map::new();
        e.insert("action".into(), json!(action));
        e.insert("time".into(), json!(store::now()));
        for (k, v) in result {
            e.insert(k.clone(), v.clone());
        }
        match s.get_mut("events") {
            Some(Value::Array(events)) => {
                events.push(Value::Object(e));
                Ok(())
            }
            Some(_) => Err(LearningError::Type("lesson events must be a list".into())),
            None => Err(LearningError::NotFound("events".into())),
        }
    }

    /// One lesson action from the browser or the voice: `{"action", "id"?, "revision"?, "work"?, ...}`.
    pub fn dispatch(&self, data: &Map<String, Value>) -> Result<Map<String, Value>, LearningError> {
        let mut guard = self.lock.lock();
        let action = data.get("action").and_then(Value::as_str).map(str::to_string);
        let action = action.as_deref();
        if action == Some("create") {
            let mode = data.get("mode").cloned().unwrap_or(json!("learn"));
            let topic = take_chars(&data.get("topic").map(py_str).unwrap_or_default(), 200);
            let level = take_chars(&data.get("level").map(py_str).unwrap_or_default(), 100);
            if mode.as_str() == Some("learn") && py_strip(&topic).is_empty() {
                return Err(err("What topic would you like to learn?"));
            }
            let s = self.store.create(&mode, &topic, &level, self.demo)?;
            self.set_active(Some(lesson_id(&s)?));
            return Ok(s);
        }
        let key = match data.get("id").filter(|v| truthy(v)) {
            Some(Value::String(id)) => id.clone(),
            // SQLite compares a number with the TEXT id column as its text.
            Some(v @ Value::Number(_)) => py_str(v),
            Some(_) => return Err(LearningError::Other(anyhow::anyhow!("lesson id must be text"))),
            None => match self.active() {
                Some(a) => a,
                None => return Err(err("Open a lesson first.")),
            },
        };
        let mut s = self.store.get(&key)?;
        if action == Some("select") {
            self.set_active(Some(key));
            return Ok(s);
        }
        if action == Some("delete") {
            self.store.delete(&key)?;
            if self.active().as_deref() == Some(key.as_str()) {
                self.set_active(None);
            }
            return Ok(obj(json!({"deleted": true})));
        }
        let current = s.get("revision").cloned().ok_or_else(|| LearningError::NotFound("revision".into()))?;
        let revision = data.get("revision").cloned().unwrap_or_else(|| current.clone());
        if !py_eq(&revision, &current) {
            // The only change since the browser loaded is spoken lines appended while eve listened: merge a save
            // into them instead of refusing it.
            let mark = s.get("last_other_revision").cloned().unwrap_or_else(|| current.clone());
            let merge = action == Some("save")
                && as_int(&revision).is_some()
                && matches!((as_num(&revision), as_num(&mark)), (Some(r), Some(m)) if r >= m);
            if !merge {
                return Err(err("This lesson changed in another window. Reload it before continuing."));
            }
        }
        let demo = s.get("demo").ok_or_else(|| LearningError::NotFound("demo".into()))?;
        if !py_eq(demo, &Value::Bool(self.demo)) {
            return Err(err("Open this lesson using the same demo/live mode it was created in."));
        }
        let stage = |s: &Map<String, Value>| s.get("stage").and_then(Value::as_str).unwrap_or("").to_string();
        if stage(&s) == "ended" && action != Some("resume") {
            return Err(err("Resume this saved lesson before editing it."));
        }
        match action {
            Some("save") => {
                let mut work = validate_work(data.get("work").unwrap_or(&json!({})))?;
                if !matches!(stage(&s).as_str(), "input" | "confirm") {
                    work.shift_remove("problem");
                    work.shift_remove("source_image");
                }
                if work.contains_key("ideas") && s.get("spoken").is_some_and(truthy) {
                    let fallback = as_num(&revision).unwrap_or(0.0);
                    let base = match data.get("ideas_base") {
                        Some(b) => as_int(b).map(|b| b as f64).unwrap_or(fallback),
                        None => fallback,
                    };
                    let ideas = get_str(&work, "ideas").to_string();
                    work.insert("ideas".into(), json!(merge_spoken(&ideas, &s, base)));
                }
                for (k, v) in work {
                    s.insert(k, v);
                }
            }
            Some("generate") => {
                if stage(&s) != "setup" {
                    return Err(err("Start a new lesson to generate another problem."));
                }
                let (result, fresh) = self.ask_tutor(&mut guard, "generate", &s)?;
                s = fresh;
                if py_strip(get_str(&result, "problem")).is_empty() {
                    return Err(err("Eve did not generate a problem. Try again."));
                }
                s.insert("problem".into(), field(&result, "problem")?);
                s.insert("stage".into(), json!("working"));
                Self::event(&mut s, "generate", &result)?;
            }
            Some("recognize") => {
                if !matches!(stage(&s).as_str(), "input" | "confirm") {
                    return Err(err("Start a new lesson to change the problem."));
                }
                let has_problem = !py_strip(get_str(&s, "problem")).is_empty();
                if !(has_problem || s.get("source_image").is_some_and(truthy) || mark_count(&s) > 0) {
                    return Err(err("Write a problem or add a screenshot first."));
                }
                let (result, fresh) = self.ask_tutor(&mut guard, "recognize", &s)?;
                s = fresh;
                s.insert("problem".into(), field(&result, "problem")?);
                s.insert("stage".into(), json!("confirm"));
                Self::event(&mut s, "recognize", &result)?;
            }
            Some("confirm") => {
                if stage(&s) != "confirm" || py_strip(get_str(&s, "problem")).is_empty() {
                    return Err(err("Confirm the problem text first."));
                }
                s.insert("stage".into(), json!("working"));
                s.insert("problem_stroke_count".into(), json!(mark_count(&s)));
                let feedback = obj(json!({"feedback": "Write your ideas or show where you are stuck. If you don't know how to start, tell me."}));
                Self::event(&mut s, "confirm", &feedback)?;
            }
            Some(a @ ("hint" | "step" | "check" | "recap")) => {
                let st = stage(&s);
                if st != "working" && st != "complete" {
                    return Err(err("Confirm your problem before asking for help."));
                }
                if st == "complete" && a != "recap" {
                    return Err(err("This problem is complete. Try another problem or review the recap."));
                }
                if get_str(&s, "mode") == "help" {
                    let before = s.get("problem_stroke_count").and_then(as_num).unwrap_or(0.0);
                    let attempted = !py_strip(get_str(&s, "ideas")).is_empty()
                        || s.get("stuck").is_some_and(truthy)
                        || (mark_count(&s) as f64) > before;
                    if !attempted {
                        return Err(err("Put down your ideas first, or select I don't know how to start."));
                    }
                }
                let (result, fresh) = self.ask_tutor(&mut guard, a, &s)?;
                s = fresh;
                Self::event(&mut s, a, &result)?;
                if get_str(&result, "status") == "complete" {
                    s.insert("stage".into(), json!("complete"));
                }
            }
            Some("end") => {
                let st = s.get("stage").cloned().unwrap_or(Value::Null);
                s.insert("resume_stage".into(), st);
                s.insert("stage".into(), json!("ended"));
                let feedback = obj(json!({"feedback": "Your lesson and all your work are saved. Come back whenever you're ready."}));
                Self::event(&mut s, "end", &feedback)?;
            }
            Some("resume") => {
                if stage(&s) == "ended" {
                    let back = s.shift_remove("resume_stage").unwrap_or(json!("working"));
                    s.insert("stage".into(), back);
                }
            }
            _ => return Err(err("Unknown lesson action.")),
        }
        // Every change except a spoken append moves this mark: a stale save may merge only past appends.
        let rev = s.get("revision").and_then(as_int).ok_or_else(|| LearningError::Type("lesson revision must be a number".into()))?;
        s.insert("last_other_revision".into(), json!(rev + 1));
        let s = self.store.save(&s, Some(rev))?;
        self.set_active(Some(lesson_id(&s)?));
        if let Some(notify) = &self.notify {
            if action != Some("save") {
                if let Some(Value::Object(last)) = s.get("events").and_then(Value::as_array).and_then(|e| e.last()) {
                    let feedback = last.get("feedback").and_then(Value::as_str).unwrap_or("");
                    if let Err(e) = notify(feedback, &stage(&s)) {
                        tracing::error!("learning: robot notification failed; lesson is saved: {e:#}");
                    }
                }
            }
        }
        Ok(s)
    }

    // ---- think out loud ----

    /// Add one thing the learner said while eve listened to the end of their ideas.
    ///
    /// Typed ideas are never replaced. Returns true when the line was saved.
    pub fn append_spoken(&self, lesson_id: &str, text: &str) -> Result<bool, LearningError> {
        let line = take_chars(&py::split_join(text), MAX_SPOKEN_IDEA);
        if line.is_empty() {
            return Ok(false);
        }
        let _guard = self.lock.lock();
        let mut s = self.store.get(lesson_id)?;
        let demo = s.get("demo").cloned().unwrap_or(Value::Null);
        if s.get("stage").and_then(Value::as_str) == Some("ended") || !py_eq(&demo, &Value::Bool(self.demo)) {
            return Ok(false);
        }
        let ideas = [py_rstrip(get_str(&s, "ideas")), line.as_str()]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        s.insert("ideas".into(), json!(py::last_chars(&ideas, MAX_TEXT)));
        s.insert("stuck".into(), json!(false));
        let rev = s.get("revision").and_then(as_int).ok_or_else(|| LearningError::Type("lesson revision must be a number".into()))?;
        let mut spoken = match s.get("spoken") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        spoken.push(json!({"rev": rev + 1, "text": line}));
        let keep = spoken.split_off(spoken.len().saturating_sub(SPOKEN_KEEP));
        s.insert("spoken".into(), Value::Array(keep));
        self.store.save(&s, Some(rev))?;
        Ok(true)
    }

    pub fn listen_status(&self) -> Map<String, Value> {
        let Some(listener) = &self.listener else {
            let mut off = listen_off();
            off.insert("reason".into(), json!(ROBOT_APP_NOT_RUNNING));
            return off;
        };
        match listener.status() {
            Ok(status) => obj(json!({"available": true,
                                     "state": status.get("state").cloned().unwrap_or(json!("off")),
                                     "lesson_id": status.get("lesson_id").cloned().unwrap_or(Value::Null),
                                     "reason": status.get("reason").cloned().unwrap_or(json!(""))})),
            Err(e) => {
                tracing::error!("learning: listen status failed: {e:#}");
                let mut off = listen_off();
                off.insert("reason".into(), json!("eve could not say whether it is listening."));
                off
            }
        }
    }

    /// The listener's answer plus the current state. The status goes first, so its empty "reason" can never
    /// overwrite the sentence that explains a refusal.
    fn listen_result(&self, result: &Map<String, Value>) -> Map<String, Value> {
        merged(&self.listen_status(), result)
    }

    fn call_listener(&self, name: &str, call: impl FnOnce(&dyn Listener) -> anyhow::Result<Map<String, Value>>) -> Map<String, Value> {
        let listener = self.listener.as_ref().expect("checked by the caller");
        match call(listener.as_ref()) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("learning: {name} failed: {e:#}");
                obj(json!({"ok": false, "reason": "eve could not change listening. Try again."}))
            }
        }
    }

    fn not_running() -> Map<String, Value> {
        let mut out = listen_off();
        out.insert("ok".into(), json!(false));
        out.insert("reason".into(), json!(ROBOT_APP_NOT_RUNNING));
        out
    }

    /// Start think out loud for a lesson (the browser's, else the active one). Never fails.
    pub fn listen(&self, lesson_id: Option<&str>) -> Map<String, Value> {
        if self.listener.is_none() {
            return Self::not_running();
        }
        let s = {
            let _guard = self.lock.lock();
            let key = lesson_id.filter(|k| !k.is_empty()).map(str::to_string).or_else(|| self.active());
            let s = key.and_then(|k| match self.store.get(&k) {
                Ok(s) => Some(s),
                Err(LearningError::NotFound(_)) => None,
                Err(e) => {
                    tracing::error!("learning: listen could not read the lesson: {e:#}");
                    None
                }
            });
            if let Some(refusal) = listen_refusal(s.as_ref()) {
                return self.listen_result(&obj(json!({"ok": false, "reason": refusal})));
            }
            let s = s.expect("a lesson that may listen");
            self.set_active(s.get("id").and_then(Value::as_str).map(str::to_string));
            s
        };
        // Outside the lock: the daemon may take a moment, and saving spoken ideas needs the lock.
        let result = self.call_listener("start", |l| l.start(&s));
        self.listen_result(&result)
    }

    pub fn stop_listening(&self) -> Map<String, Value> {
        if self.listener.is_none() {
            return Self::not_running();
        }
        let result = self.call_listener("stop", |l| l.stop());
        self.listen_result(&result)
    }

    /// The voice tool's lesson actions (`run_lesson` calls this with a validated request).
    pub fn voice(&self, req: &LessonRequest) -> Result<Map<String, Value>, LearningError> {
        let mode = req.mode.clone().unwrap_or_default();
        let topic = req.topic.clone().unwrap_or_default();
        let level = req.level.clone().unwrap_or_default();
        let text = req.text.clone().unwrap_or_default();
        match req.action.as_str() {
            "open" => {
                (self.opener)(&self.url());
                Ok(obj(json!({"ok": true, "answer": "Would you like to learn a topic or work on a problem you already have?"})))
            }
            "listen" => Ok(self.listen(None)),
            "stop-listening" => Ok(self.stop_listening()),
            "start" => {
                let mode = if mode.is_empty() { "learn".to_string() } else { mode };
                let mut s = self.dispatch(&obj(json!({"action": "create", "mode": mode, "topic": topic, "level": level})))?;
                if get_str(&s, "mode") == "learn" {
                    s = self.dispatch(&obj(json!({"action": "generate", "id": s["id"]})))?;
                }
                (self.opener)(&format!("{}#lesson/{}", self.url(), get_str(&s, "id")));
                let problem = get_str(&s, "problem");
                let answer = if problem.is_empty() { "Add your problem to the whiteboard, then show me your ideas." } else { problem };
                Ok(obj(json!({"ok": true, "answer": answer})))
            }
            "status" => {
                let s = match self.active() {
                    Some(a) => Some(self.store.get(&a)?),
                    None => None,
                };
                let lesson = match &s {
                    Some(s) => Value::Object(Store::summary(s)?),
                    None => Value::Null,
                };
                let last = s.as_ref().and_then(|s| s.get("events")).and_then(Value::as_array).and_then(|e| e.last());
                let answer = match (&s, last) {
                    (Some(_), Some(e)) => e.get("feedback").cloned().unwrap_or(json!("")),
                    _ => json!("Choose a lesson in the learning window."),
                };
                Ok(obj(json!({"ok": true, "lesson": lesson, "answer": answer})))
            }
            "ideas" => {
                self.dispatch(&obj(json!({"action": "save", "work": {"ideas": text, "stuck": py_strip(&text).is_empty()}})))?;
                Ok(obj(json!({"ok": true, "answer": "I've saved your thinking. Would you like a hint or a check?"})))
            }
            action => {
                let s = self.dispatch(&obj(json!({"action": action})))?;
                let event = s
                    .get("events")
                    .and_then(Value::as_array)
                    .and_then(|e| e.last())
                    .and_then(Value::as_object)
                    .cloned()
                    .ok_or_else(|| LearningError::Other(anyhow::anyhow!("list index out of range")))?;
                let answer = ["step", "feedback"]
                    .iter()
                    .filter_map(|k| event.get(*k).filter(|v| truthy(v)).map(py_str))
                    .collect::<Vec<_>>()
                    .join(" ");
                Ok(obj(json!({"ok": true, "answer": answer})))
            }
        }
    }
}

/// `result[key]`: Python's KeyError when a tutor answer lacks it.
fn field(result: &Map<String, Value>, key: &str) -> Result<Value, LearningError> {
    result.get(key).cloned().ok_or_else(|| LearningError::NotFound(key.to_string()))
}

fn lesson_id(s: &Map<String, Value>) -> Result<String, LearningError> {
    match s.get("id") {
        Some(Value::String(id)) => Ok(id.clone()),
        _ => Err(LearningError::NotFound("id".into())),
    }
}

// ---- the HTTP handler ----------------------------------------------------------------------------------------

/// One answer: status, body, Content-Type, and the page nonce when it is the page.
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub mime: &'static str,
    pub nonce: String,
}

impl Reply {
    /// A JSON reply (`json.dumps(data)`, ASCII-escaped as Python's default).
    pub fn json(status: u16, data: &Value) -> Reply {
        Reply { status, body: dumps(data, true).into_bytes(), mime: "application/json", nonce: String::new() }
    }

    fn bytes(status: u16, body: Vec<u8>, mime: &'static str) -> Reply {
        Reply { status, body, mime, nonce: String::new() }
    }

    fn error(status: u16, msg: &str) -> Reply {
        Reply::json(status, &json!({"error": msg}))
    }

    fn into_response(self) -> Response {
        let csp = if self.nonce.is_empty() { CSP.clone() } else { content_policy(&self.nonce) };
        let length = self.body.len();
        let mut resp = Response::new(Body::from(self.body));
        *resp.status_mut() = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let h = resp.headers_mut();
        h.insert("Content-Type", HeaderValue::from_static(self.mime));
        h.insert("Content-Length", HeaderValue::from(length));
        h.insert("Cache-Control", HeaderValue::from_static("no-store"));
        h.insert("X-Content-Type-Options", HeaderValue::from_static("nosniff"));
        if let Ok(v) = HeaderValue::from_str(&csp) {
            h.insert("Content-Security-Policy", v);
        }
        resp
    }
}

/// `secrets.token_urlsafe(n)`.
pub fn token_urlsafe(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::Rng::fill(&mut rand::rng(), bytes.as_mut_slice());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).map(|v| v.to_str().unwrap_or("\u{0}"))
}

/// What the server shares with each request: the app and the page token.
pub struct Shared {
    pub app: Arc<LearningApp>,
    pub token: String,
}

impl Shared {
    /// Only this page on this port: the Host is ours, and an Origin, when sent, is ours.
    fn allowed(&self, headers: &HeaderMap) -> bool {
        let url = self.app.url();
        let base = url.trim_end_matches('/');
        let netloc = base.strip_prefix("http://").unwrap_or(base);
        header(headers, "host") == Some(netloc) && header(headers, "origin").unwrap_or(base) == base
    }

    /// `do_GET`.
    pub fn get(&self, path: &str, headers: &HeaderMap) -> Reply {
        if !self.allowed(headers) {
            return Reply::error(403, "Local access only.");
        }
        match self.get_inner(path) {
            Ok(r) => r,
            Err(LearningError::NotFound(_)) => Reply::error(404, "Lesson not found"),
            Err(e) => {
                tracing::error!("learning request failed: {e:#}");
                Reply::error(500, "Eve could not finish this request. Your previously saved work is safe.")
            }
        }
    }

    fn get_inner(&self, path: &str) -> Result<Reply, LearningError> {
        let app = &self.app;
        if path == "/api/config" {
            let settings = if app.demo {
                obj(json!({"provider": "demo", "model": "offline examples", "ready": true, "key_name": ""}))
            } else {
                live_settings(&app.ctx)?.to_map()
            };
            let mut out = settings;
            out.insert("token".into(), json!(self.token));
            out.insert("demo".into(), json!(app.demo));
            out.insert("active".into(), json!(app.active()));
            out.insert("listen_available".into(), json!(app.has_listener()));
            // A tldraw licence key is shipped to the browser by design; it is not a secret.
            out.insert("tldraw_license_key".into(), json!(py_strip(&app.ctx.get("EVE_TLDRAW_LICENSE_KEY"))));
            return Ok(Reply::json(200, &Value::Object(out)));
        }
        if path == "/api/listening" {
            return Ok(Reply::json(200, &Value::Object(app.listen_status())));
        }
        if path == "/api/lessons" {
            let rows: Vec<Value> = app.store.list()?.into_iter().map(Value::Object).collect();
            return Ok(Reply::json(200, &Value::Array(rows)));
        }
        if path.starts_with("/api/lessons/") {
            let parts: Vec<&str> = path.split('/').collect();
            let key = parts[3];
            let body = if parts.len() > 4 && parts[4] == "history" {
                Value::Array(app.store.history(key)?)
            } else {
                Value::Object(app.store.get(key)?)
            };
            return Ok(Reply::json(200, &body));
        }
        if let Some(name) = path.strip_prefix("/fonts/") {
            // The path is not percent-decoded, so "..", "%2f" and friends fail the name checks.
            let mime = if name == "OFL.txt" {
                "text/plain; charset=utf-8"
            } else if FONT_FILE.is_match(name) {
                "font/woff2"
            } else {
                return Ok(Reply::error(404, "Not found"));
            };
            return Ok(match asset(&format!("fonts/{name}")) {
                Some(bytes) => Reply::bytes(200, bytes.to_vec(), mime),
                None => Reply::error(404, "Not found"),
            });
        }
        if let Some(name) = path.strip_prefix("/canvas/") {
            let Some(mime) = canvas_mime(path_suffix(name)).filter(|_| CANVAS_FILE.is_match(name)) else {
                return Ok(Reply::error(404, "Not found"));
            };
            return Ok(match asset(&format!("canvas/{name}")) {
                Some(bytes) => Reply::bytes(200, bytes.to_vec(), mime),
                None => Reply::error(404, "Not found"),
            });
        }
        let file = match path {
            "/" => Some(("index.html", "text/html; charset=utf-8")),
            "/app.js" => Some(("app.js", "text/javascript; charset=utf-8")),
            "/style.css" => Some(("style.css", "text/css; charset=utf-8")),
            _ => None,
        };
        if let Some((name, mime)) = file {
            let body = asset(name).ok_or_else(|| LearningError::Other(anyhow::anyhow!("{name} is not bundled")))?;
            if path == "/" {
                let nonce = token_urlsafe(16);
                let page = replace_bytes(body, NONCE_SLOT, nonce.as_bytes());
                return Ok(Reply { status: 200, body: page, mime, nonce });
            }
            return Ok(Reply::bytes(200, body.to_vec(), mime));
        }
        Ok(Reply::error(404, "Not found"))
    }

    /// `do_POST`'s checks before the body is read: Ok(length) to read, or the reply.
    pub fn post_head(&self, target: &str, headers: &HeaderMap) -> Result<usize, Reply> {
        if !self.allowed(headers) || header(headers, "x-eve-token") != Some(self.token.as_str()) {
            return Err(Reply::error(403, "Reload this local page before saving."));
        }
        if target != "/api/action" {
            return Err(Reply::error(404, "Not found"));
        }
        let length = match py::parse_int(header(headers, "content-length").unwrap_or("0")) {
            Ok(n) => n,
            Err(msg) => return Err(Reply::error(400, &msg)),
        };
        if !(length > 0 && length as usize <= MAX_BODY) {
            return Err(Reply::error(413, "Request is too large."));
        }
        Ok(length as usize)
    }

    /// `do_POST` once the body is read.
    pub fn post_body(&self, body: &[u8]) -> Reply {
        let body = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
        let data: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => return Reply::error(400, &e.to_string()),
        };
        let Value::Object(data) = data else { return Reply::error(400, "Invalid request.") };
        let app = &self.app;
        if let Some(action) = data.get("action").and_then(Value::as_str).filter(|a| LISTEN_ACTIONS.contains(a)) {
            // The "Think out loud" toggle. The daemon owns the microphone; standalone, nobody does.
            let result = if action == "listen" {
                app.listen(data.get("id").and_then(Value::as_str))
            } else {
                app.stop_listening()
            };
            let ok = match result.get("ok") {
                Some(Value::Bool(true)) => "True",
                Some(Value::Bool(false)) => "False",
                None => "None",
                Some(_) => "?",
            };
            tracing::info!("learning: {action} from the browser -> ok={ok}");
            if !result.get("ok").is_some_and(truthy) {
                let status = if !result.get("available").is_some_and(truthy) { 503 } else { 409 };
                let reason = result.get("reason").filter(|r| truthy(r)).map(py_str);
                let mut out = result.clone();
                out.insert("error".into(), json!(reason.unwrap_or_else(|| "eve cannot listen now.".into())));
                return Reply::json(status, &Value::Object(out));
            }
            return Reply::json(200, &Value::Object(result));
        }
        match app.dispatch(&data) {
            Ok(s) => Reply::json(200, &Value::Object(s)),
            Err(LearningError::NotFound(_)) => Reply::error(404, "Lesson not found"),
            Err(LearningError::Value(m) | LearningError::Type(m)) => Reply::error(400, &m),
            Err(LearningError::Other(e)) => {
                tracing::error!("learning request failed: {e:#}");
                Reply::error(500, "Eve could not finish this request. Your previously saved work is safe.")
            }
        }
    }
}

fn replace_bytes(haystack: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i..].starts_with(needle) {
            out.extend_from_slice(with);
            i += needle.len();
        } else {
            out.push(haystack[i]);
            i += 1;
        }
    }
    out
}

async fn handle(State(shared): State<Arc<Shared>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let reply = match parts.method {
        Method::GET => {
            let path = parts.uri.path().to_string();
            let headers = parts.headers.clone();
            let sh = shared.clone();
            tokio::task::spawn_blocking(move || sh.get(&path, &headers)).await
        }
        Method::POST => {
            let target = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
            let length = match shared.post_head(&target, &parts.headers) {
                Ok(n) => n,
                Err(reply) => return reply.into_response(),
            };
            let bytes = match axum::body::to_bytes(body, length.max(1)).await {
                Ok(b) => b,
                Err(_) => return Reply::error(400, "Invalid request.").into_response(),
            };
            let sh = shared.clone();
            tokio::task::spawn_blocking(move || sh.post_body(&bytes)).await
        }
        other => {
            let mut r = Response::new(Body::from(format!("Unsupported method ({:?})", other.as_str())));
            *r.status_mut() = StatusCode::NOT_IMPLEMENTED;
            return r;
        }
    };
    match reply {
        Ok(r) => r.into_response(),
        Err(e) => {
            tracing::error!("learning request failed: {e}");
            Reply::error(500, "Eve could not finish this request. Your previously saved work is safe.").into_response()
        }
    }
}

/// A running learning server: Python's `ThreadingHTTPServer` with `.app` and `.token`.
pub struct LearningServer {
    pub app: Arc<LearningApp>,
    pub token: String,
    pub port: u16,
    stop: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl LearningServer {
    /// Stop serving and wait for the server thread.
    pub fn shutdown(&self) {
        if let Some(tx) = self.stop.lock().take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.lock().take() {
            let _ = t.join();
        }
    }

    /// Close the listening socket (done by [`shutdown`](Self::shutdown); kept for Python's call order).
    pub fn server_close(&self) {
        self.shutdown();
    }
}

impl Drop for LearningServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Serve a new [`LearningApp`] on 127.0.0.1:`opts.port` (0 picks a free port) on a thread of its own.
/// An `io::Error` of kind `AddrInUse` when the port is taken.
pub fn start(opts: StartOptions) -> io::Result<LearningServer> {
    let port = opts.port;
    let app = Arc::new(LearningApp::new(opts).map_err(|e| io::Error::other(e.to_string()))?);
    let listener = StdListener::bind(("127.0.0.1", port))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let token = token_urlsafe(32);
    app.set_url(&format!("http://127.0.0.1:{port}/"));
    let shared = Arc::new(Shared { app: app.clone(), token: token.clone() });
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("eve-learning")
        .enable_all()
        .build()?;
    let thread = std::thread::Builder::new().name("eve-learning".into()).spawn(move || {
        runtime.block_on(async move {
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("learning: could not serve: {e}");
                    return;
                }
            };
            let router = axum::Router::new().fallback(handle).with_state(shared);
            let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
                let _ = rx.await;
            });
            if let Err(e) = serve.await {
                tracing::error!("learning: server stopped: {e}");
            }
        });
        // A tutor call still running finishes on its own thread; the server does not wait for it.
        runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    })?;
    Ok(LearningServer { app, token, port, stop: Mutex::new(Some(tx)), thread: Mutex::new(Some(thread)) })
}

// ---- the launcher ----------------------------------------------------------------------------------------------

#[derive(clap::Parser, Debug)]
#[command(about = "Eve's saved lessons and whiteboard")]
struct Args {
    /// Offline examples; no API calls or handwriting recognition
    #[arg(long)]
    demo: bool,
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    #[arg(long = "data-dir")]
    data_dir: Option<PathBuf>,
    /// Explicit environment file (use a /mnt/c/... path from WSL)
    #[arg(long = "env-file")]
    env_file: Option<PathBuf>,
    #[arg(long = "no-open")]
    no_open: bool,
}

/// What the launcher starts the server with: [`start`], or a test's stand-in.
pub type StartFn<'a> = &'a dyn Fn(StartOptions) -> io::Result<LearningServer>;

/// `server.main(argv)` with its seams: the environment the env file loads into, and the start function. Blocks
/// until Ctrl-C once the server is up; returns the exit code (2: the port is taken, or bad arguments).
pub fn main_with(
    argv: &[String],
    environ: &mut dyn eve_envfile::Environ,
    start_fn: StartFn<'_>,
    stderr: &mut dyn io::Write,
) -> anyhow::Result<i32> {
    use clap::Parser as _;
    let mut full = vec!["eve-learning".to_string()];
    full.extend(argv.iter().cloned());
    let args = match Args::try_parse_from(full) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return Ok(e.exit_code());
        }
    };
    eve_envfile::load_env_file(args.env_file.as_deref(), environ)?;
    let directory = match args.data_dir.clone() {
        Some(d) => d,
        None => {
            let base = store::default_dir_from(environ.var("EVE_LEARNING_DIR").map(|v| v.to_string_lossy().into_owned()).as_deref());
            if args.demo { base.join("demo") } else { base }
        }
    };
    let opts = StartOptions { directory: Some(directory), demo: args.demo, port: args.port, ..StartOptions::default() };
    let server = match start_fn(opts) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            let _ = writeln!(
                stderr,
                "Port {port} is already in use. If Eve is already running, open http://127.0.0.1:{port}/ instead of starting another instance.\n\
To load changed settings, stop the existing Eve process first, then restart it. Or choose a different port with --port.",
                port = args.port
            );
            return Ok(2);
        }
        Err(e) => return Err(e.into()),
    };
    println!("Eve learning: {} ({})", server.app.url(), if args.demo { "offline demo" } else { "live tutor" });
    if !args.no_open {
        open_in_browser(&server.app.url());
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    server.shutdown();
    server.server_close();
    Ok(0)
}

/// `python -m eve_bridge.learning`: the process's arguments and environment, the real [`start`].
pub fn main() -> anyhow::Result<i32> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    main_with(&argv, &mut eve_envfile::ProcessEnv, &start, &mut io::stderr())
}
