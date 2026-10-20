//! The hands: a persistent PyAutoGUI REPL that runs the model's Python on this Mac. Port of `desktop_worker.py`.
//!
//! Runs as a child process of the daemon (the `eve-desktop-worker` binary), one JSON request per stdin line, one
//! JSON reply per stdout line:
//!
//! ```text
//! {"id": 1, "operation": "execute", "code": "display(pyautogui.screenshot())"}
//! {"id": 1, "output": [{"type":"input_text","text":...}, {"type":"input_image",...}]}
//! ```
//!
//! Globals persist between calls, exactly like the `exec_py` tool in OpenAI's computer-use sample app
//! (openai/openai-cua-sample-app, python-app/app/desktop/worker.py, MIT) which this follows: `log(...)` collects
//! text, `display(img)` collects PNGs, exceptions come back as text so the model can correct itself, and the
//! PyAutoGUI fail-safe (mouse into a screen corner) is a terminal error.
//!
//! The model's code is Python, so it runs in the exec host ([`host`], `exec_host.py`): a Python child of this
//! worker that holds the namespace, PyAutoGUI and Pillow. Everything around it is here: the output list and its
//! limits, the helpers (eve_desktop_lanes::desktop_helpers, called back from the model's code), the auto-screenshot
//! rule, the timing, the fast lane's wiring and the daemon's line protocol.
//!
//! Retina: PyAutoGUI screenshots are physical pixels (2x) while its input is logical points.
//! [`normalize_screenshots`] resizes every screenshot to the point grid so the coordinates the model reads are the
//! coordinates it clicks.
//!
//! On top of raw PyAutoGUI the namespace carries the helpers — open_app, open_url, frontmost, screen_text,
//! find_text, click_text, wait_for, wait_settled, type_text, zoom, observe — which sense the screen locally (CG
//! capture, Vision OCR, AX) and log one sentence each. With EVE_FAST_LANE on (default:
//! fast_lane::FAST_LANE_DEFAULT) it also carries `delegate`; in the lane's `model` mode [`start_fast_lane`] loads
//! the hosted decider (eve_jev) in a background thread. The ready line and every reply say where that stands
//! ("fast_lane").
//!
//! Auto-screenshot rule: when a call clicked, typed or pressed keys (any PyAutoGUI input function, type_text,
//! click_text), raised, or produced no output, and did not display() an image itself, [`execute`] appends a
//! screenshot taken after the screen settles (≤ 1.5 s) — then always an `[after]` line with the frontmost app and
//! whether the screen changed since the previous call. Text-only calls (screen_text, find_text) stay cheap: no
//! image.
//!
//! `{"operation": "observe"}` is `execute("observe()")`: the first turn's picture. With helpers, every reply also
//! carries `"timing": {…}` in ms (capture, resize, encode, ocr, ax, settle, act, decide, exec) so the agent's run
//! log shows where local time goes. `eve-desktop-worker --release` posts a key-up for every key code and a mouse-up
//! for every button, in a fresh process, so the daemon can release anything a killed worker left held.

pub mod host;
#[cfg(target_os = "macos")]
mod quartz;

use std::collections::HashMap;
use std::io::BufRead;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use image::RgbImage;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_desktop_lanes::desktop_helpers::{DelegateCall, Helpers, Waited, ACTION_NAMES, FAST_LANE_HELPERS};
use eve_desktop_lanes::fast_lane::{
    LaneDecider, DECIDE_MODES, DEFAULT_DECIDE, DEFAULT_STYLE, FAST_LANE_DEFAULT, LANE_FIRST_DEFAULT,
};
use eve_desktop_lanes::{error_kind, error_message, py_round_n, py_strip, py_take, LaneError};

pub use host::{default_python, HostDied, HostGui, Namespace, PYTHON_ENV};
#[cfg(target_os = "macos")]
pub use quartz::SystemQuartz;

pub const MAX_TEXT_BYTES: usize = 60 * 1024;
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ITEMS: usize = 250;
pub const MAX_CODE_BYTES: usize = 64 * 1024;
pub const MAX_REQUEST_BYTES: usize = 512 * 1024;

/// Python's `sys.platform` for this build.
pub const SYS_PLATFORM: &str = if cfg!(target_os = "macos") {
    "darwin"
} else if cfg!(target_os = "windows") {
    "win32"
} else {
    std::env::consts::OS
};

/// The helpers, shared with the fast lane's loader thread (it sets `decider` and `fast_lane_status` when Jev is
/// warm, as the Python thread set them on the Helpers object).
pub type SharedHelpers = Arc<Mutex<Helpers>>;

/// The environment as Python's `os.environ` mapping (names and values that are valid UTF-8).
pub type Env = HashMap<String, String>;

/// `os.environ`, read now.
pub fn process_env() -> Env {
    std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect()
}

fn env_get<'a>(env: &'a Env, name: &str) -> &'a str {
    env.get(name).map(String::as_str).unwrap_or("")
}

/// `print(json.dumps(payload))` to the daemon on the real stdout, flushed.
pub fn emit(payload: &Value) -> anyhow::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    out.write_all(eve_jev::py_dumps(payload).as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

// ---- the output of one call ---------------------------------------------------------------------------

/// The model-facing output list of one exec call with its size accounting (Python's `output`, `text_bytes`,
/// `image_bytes`, `encode_secs`).
#[derive(Debug, Default)]
pub struct Output {
    pub items: Vec<Value>,
    pub text_bytes: usize,
    pub image_bytes: usize,
    pub encode_secs: f64,
    /// A limit a helper's own log or display hit: Python raised it inside the helper; here the helper's call answers
    /// it when the helper returns.
    pending: Option<LaneError>,
}

impl Output {
    /// Python `log(*values)` after the join: one text item, ValueError past the limits.
    pub fn log(&mut self, text: &str) -> Result<(), LaneError> {
        self.text_bytes += text.len();
        if self.text_bytes > MAX_TEXT_BYTES || self.items.len() >= MAX_ITEMS {
            return Err(LaneError::new("ValueError", "text output exceeds its size limit"));
        }
        self.items.push(json!({"type": "input_text", "text": text}));
        Ok(())
    }

    /// Python `display(value)` once the value is bytes (None: it was neither a Pillow image nor bytes).
    pub fn display_png(&mut self, png: Option<&[u8]>) -> Result<(), LaneError> {
        let Some(png) = png.filter(|p| p.starts_with(b"\x89PNG\r\n\x1a\n")) else {
            return Err(LaneError::new("TypeError", "display() expects a Pillow image or PNG bytes"));
        };
        self.image_bytes += png.len();
        if self.image_bytes > MAX_IMAGE_BYTES || self.items.len() >= MAX_ITEMS {
            return Err(LaneError::new("ValueError", "image output exceeds its size limit"));
        }
        let url = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png));
        self.items.push(json!({"type": "input_image", "detail": "original", "image_url": url}));
        Ok(())
    }

    /// Python `display(image)` for an image: PNG-encode it (timed under "encode"), then [`Output::display_png`].
    pub fn display_image(&mut self, img: &RgbImage) -> Result<(), LaneError> {
        let t0 = Instant::now();
        let png = encode_png(img);
        self.encode_secs += t0.elapsed().as_secs_f64();
        let png = png.map_err(|e| LaneError::new("OSError", e.to_string()))?;
        self.display_png(Some(&png))
    }

    /// One host message, `{"t": "log", "text"}` or `{"t": "display", "png", "encode"}`, answered.
    pub(crate) fn handle(&mut self, msg: &Value) -> Value {
        let result = match msg.get("t").and_then(Value::as_str) {
            Some("log") => self.log(msg.get("text").and_then(Value::as_str).unwrap_or("")),
            _ => {
                self.encode_secs += msg.get("encode").and_then(Value::as_f64).unwrap_or(0.0);
                let png = msg
                    .get("png")
                    .and_then(Value::as_str)
                    .map(|b| base64::engine::general_purpose::STANDARD.decode(b).unwrap_or_default());
                self.display_png(png.as_deref())
            }
        };
        match result {
            Ok(()) => json!({"ok": null}),
            Err(e) => json!({"err": [e.kind, e.message]}),
        }
    }
}

fn encode_png(img: &RgbImage) -> anyhow::Result<Vec<u8>> {
    use image::ImageEncoder;
    let mut buf = Vec::new();
    image::codecs::png::PngEncoder::new(&mut buf).write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(buf)
}

// ---- startup checks --------------------------------------------------------------------------------------

/// Fail loudly at startup when the daemon cannot see or touch the screen. The interpreter that posts the input
/// (the exec host) and this process (capture, AX) both need Accessibility and Screen Recording.
pub fn check_desktop(ns: &Namespace) -> anyhow::Result<(i64, i64)> {
    let size = ns.gui_call("size", json!([]), json!({}))?;
    let (width, height) = match size.as_array().map(Vec::as_slice) {
        Some([w, h]) => (w.as_i64().unwrap_or(0), h.as_i64().unwrap_or(0)),
        _ => (0, 0),
    };
    if width <= 0 || height <= 0 {
        return Err(LaneError::err(
            "RuntimeError",
            "no desktop: the daemon must run in the user's GUI session (LaunchAgent)",
        ));
    }
    if SYS_PLATFORM == "darwin" {
        ns.request(json!({"op": "check_desktop"}))?;
        check_this_process()?;
    }
    Ok((width, height))
}

#[cfg(target_os = "macos")]
fn check_this_process() -> anyhow::Result<()> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "eve-desktop-worker".to_string());
    if !quartz::ax_is_process_trusted() {
        return Err(LaneError::err(
            "RuntimeError",
            format!("Accessibility is not granted to {exe} — System Settings > Privacy & Security > Accessibility"),
        ));
    }
    if !quartz::screen_capture_access() {
        return Err(LaneError::err(
            "RuntimeError",
            format!(
                "Screen Recording is not granted to {exe} — System Settings > Privacy & Security > Screen & System \
                 Audio Recording"
            ),
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn check_this_process() -> anyhow::Result<()> {
    Ok(())
}

/// Make `pyautogui.screenshot()` return images in point coordinates (Retina 2x → 1x, Lanczos), honouring
/// `region=` and `imageFilename=` as PyAutoGUI does. Runs in the exec host: the images are Pillow images there.
pub fn normalize_screenshots(ns: &Namespace) -> anyhow::Result<()> {
    ns.request(json!({"op": "normalize_screenshots"})).map(|_| ())
}

/// `Helpers.install(namespace)`: bind the helpers' names into the namespace (the lane's `delegate` only while the
/// fast lane is on, removed otherwise) and wrap every PyAutoGUI input call so it marks `acted`.
pub fn install(helpers: &mut Helpers, ns: &Namespace) -> anyhow::Result<()> {
    let names = helpers.install();
    ns.request(json!({"op": "install", "names": names, "optional": FAST_LANE_HELPERS, "actions": ACTION_NAMES}))
        .map(|_| ())
}

// ---- execute ---------------------------------------------------------------------------------------------

/// Run one exec_py call. Returns `{"output": [...]}` or `{"error": {...}}` (fail-safe). Err only when the exec
/// host is gone ([`HostDied`]).
///
/// With `helpers` the auto-screenshot rule from the crate docs applies and an `[after]` line closes every reply.
pub fn execute(code: &str, ns: &Namespace, mut helpers: Option<&mut Helpers>) -> anyhow::Result<Value> {
    let out = Arc::new(Mutex::new(Output::default()));
    let t_start = Instant::now();
    if let Some(h) = helpers.as_deref_mut() {
        let log_out = out.clone();
        let display_out = out.clone();
        h.bind(
            Box::new(move |line: &str| {
                let mut o = log_out.lock();
                if let Err(e) = o.log(line) {
                    o.pending.get_or_insert(e);
                }
            }),
            Box::new(move |img: RgbImage| {
                let mut o = display_out.lock();
                if let Err(e) = o.display_image(&img) {
                    o.pending.get_or_insert(e);
                }
            }),
        );
        h.begin();
    }
    ns.set_sink(Some(out.clone()));
    let done = run_in_host(code, ns, &out, helpers.as_deref_mut());
    ns.set_sink(None);
    let done = done?;
    if done.get("failsafe").and_then(Value::as_bool) == Some(true) {
        return Ok(json!({"error": {"code": "failsafe", "message": "desktop fail-safe: the mouse hit a screen corner"}}));
    }
    let errored = done.get("errored").and_then(Value::as_bool) == Some(true);
    if errored {
        let tb = done.get("traceback").and_then(Value::as_str).unwrap_or("");
        out.lock().items.push(json!({"type": "input_text", "text": tb}));
    }
    if let Some(h) = helpers.as_deref_mut() {
        // An action needs its settled result in the same reply (a blind repeat or a screenshot-only turn costs 3-5 s
        // of model time; 1.5 s here caps the worker at ~2 s). Text-only calls stay cheap; failed or empty calls still
        // end with a picture so the next turn can act.
        let (has_image, empty) = {
            let o = out.lock();
            (o.items.iter().any(|i| i["type"] == "input_image"), o.items.is_empty())
        };
        if !has_image && (h.acted || errored || empty) {
            if let Ok(img) = h.settled_screenshot(if h.acted { 1.5 } else { 0.0 }) {
                let _ = out.lock().display_image(&img);
            }
        }
        if let Ok(line) = h.after_line() {
            let _ = out.lock().log(&line);
        }
    }
    let mut o = out.lock();
    let items = if o.items.is_empty() {
        vec![json!({"type": "input_text", "text": "exec_py completed with no output."})]
    } else {
        std::mem::take(&mut o.items)
    };
    let mut result = Map::new();
    result.insert("output".into(), Value::Array(items));
    if let Some(h) = helpers {
        let mut timing: IndexMap<String, f64> = h.timing_ms();
        timing.insert("encode".into(), py_round_n(o.encode_secs * 1000.0, 2));
        timing.insert("exec".into(), py_round_n(t_start.elapsed().as_secs_f64() * 1000.0, 2));
        result.insert("timing".into(), json!(timing));
        if h.fast_lane {
            result.insert("fast_lane".into(), Value::String(h.fast_lane_status.clone()));
        }
    }
    Ok(Value::Object(result))
}

/// Send the code to the host and serve its messages until the call is done: its log / display, its helper calls,
/// and its input calls' acting wrap. Returns the `done` message.
fn run_in_host(
    code: &str,
    ns: &Namespace,
    out: &Arc<Mutex<Output>>,
    mut helpers: Option<&mut Helpers>,
) -> anyhow::Result<Value> {
    ns.send(&json!({"op": "exec", "code": code}))?;
    // the open input calls (nested when PyAutoGUI calls its own wrapped functions): when each began, if timed
    let mut acting: Vec<Option<Instant>> = Vec::new();
    loop {
        let msg = ns.recv()?;
        let reply = match msg.get("t").and_then(Value::as_str) {
            Some("done") => return Ok(msg),
            Some("log") | Some("display") => out.lock().handle(&msg),
            Some("call") => match helpers.as_deref_mut() {
                Some(h) => {
                    let name = msg.get("name").and_then(Value::as_str).unwrap_or("");
                    let empty = Map::new();
                    let kwargs = msg.get("kwargs").and_then(Value::as_object).unwrap_or(&empty);
                    let result = call_helper(h, name, kwargs);
                    let pending = out.lock().pending.take();
                    match (result, pending) {
                        (Ok(v), None) => json!({"ok": v}),
                        (_, Some(e)) | (Err(e), None) => json!({"err": [e.kind, e.message]}),
                    }
                }
                None => json!({"err": ["RuntimeError", "no helpers in this worker"]}),
            },
            Some("act") => {
                if let Some(h) = helpers.as_deref_mut() {
                    let name = msg.get("name").and_then(Value::as_str).unwrap_or("action").to_string();
                    let point = msg.get("point").and_then(Value::as_array).and_then(|p| match p.as_slice() {
                        [x, y] => Some((x.as_i64()?, y.as_i64()?)),
                        _ => None,
                    });
                    let _ = h.gui_action(&name, point, |_gui| Ok(()));
                }
                let timed = msg.get("timed").and_then(Value::as_bool) == Some(true);
                if timed {
                    acting.push(Some(Instant::now()));
                }
                json!({"ok": null})
            }
            Some("act_end") => {
                if let (Some(Some(t0)), Some(h)) = (acting.pop(), helpers.as_deref_mut()) {
                    *h.timing.entry("act".to_string()).or_insert(0.0) += t0.elapsed().as_secs_f64() * 1000.0;
                }
                json!({"ok": null})
            }
            _ => json!({"err": ["RuntimeError", format!("unknown worker message {msg}")]}),
        };
        ns.send(&reply)?;
    }
}

// ---- the helpers' calls from the model's code ------------------------------------------------------------

fn type_error(name: &str, want: &str, got: &Value) -> LaneError {
    LaneError::new("TypeError", format!("{name} must be {want}, not {}", py_type_name(got)))
}

/// `type(v).__name__` of a decoded JSON value.
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

fn kw<'a>(kwargs: &'a Map<String, Value>, name: &str) -> &'a Value {
    kwargs.get(name).unwrap_or(&Value::Null)
}

fn arg_str(kwargs: &Map<String, Value>, name: &str) -> Result<String, LaneError> {
    match kw(kwargs, name) {
        Value::String(s) => Ok(s.clone()),
        other => Err(type_error(name, "str", other)),
    }
}

fn arg_opt_str(kwargs: &Map<String, Value>, name: &str) -> Result<Option<String>, LaneError> {
    match kw(kwargs, name) {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        other => Err(type_error(name, "str or None", other)),
    }
}

/// A number the helper reads as an int: ints as they are, floats truncated (what PyAutoGUI's `int()` does to a
/// coordinate), bools as 0 / 1.
fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        _ => None,
    }
}

fn arg_int(kwargs: &Map<String, Value>, name: &str) -> Result<i64, LaneError> {
    let v = kw(kwargs, name);
    as_int(v).ok_or_else(|| type_error(name, "int", v))
}

fn arg_clicks(kwargs: &Map<String, Value>, name: &str) -> Result<u32, LaneError> {
    Ok(arg_int(kwargs, name)?.clamp(0, i64::from(u32::MAX)) as u32)
}

fn arg_float(kwargs: &Map<String, Value>, name: &str) -> Result<f64, LaneError> {
    match kw(kwargs, name) {
        Value::Bool(b) => Ok(f64::from(u8::from(*b))),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        other => Err(type_error(name, "a number", other)),
    }
}

/// `bool(v)` of a decoded JSON value.
pub fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `x, y, w, h = region`.
fn arg_region(kwargs: &Map<String, Value>) -> Result<Option<(i64, i64, i64, i64)>, LaneError> {
    match kw(kwargs, "region") {
        Value::Null => Ok(None),
        Value::Array(parts) => {
            if parts.len() > 4 {
                return Err(LaneError::new("ValueError", "too many values to unpack (expected 4)"));
            }
            if parts.len() < 4 {
                return Err(LaneError::new(
                    "ValueError",
                    format!("not enough values to unpack (expected 4, got {})", parts.len()),
                ));
            }
            let n: Vec<i64> = parts.iter().map(|p| as_int(p).ok_or_else(|| type_error("region", "numbers", p))).collect::<Result<_, _>>()?;
            Ok(Some((n[0], n[1], n[2], n[3])))
        }
        other => Err(LaneError::new("TypeError", format!("cannot unpack non-iterable {} object", py_type_name(other)))),
    }
}

fn lane_error(e: anyhow::Error) -> LaneError {
    LaneError::new(error_kind(&e), error_message(&e))
}

fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// One helper call from the model's code: the keyword arguments the host's stub bound with the Python signature,
/// the return value as JSON (dicts for frontmost / screen_text / find_text / wait_for's hit).
pub fn call_helper(h: &mut Helpers, name: &str, kwargs: &Map<String, Value>) -> Result<Value, LaneError> {
    let k = kwargs;
    let s = |r: anyhow::Result<String>| r.map(Value::String).map_err(lane_error);
    match name {
        "click_element" => {
            s(h.click_element(arg_int(k, "x")?, arg_int(k, "y")?, &arg_str(k, "expect")?, arg_clicks(k, "clicks")?))
        }
        "open_app" => s(h.open_app(&arg_str(k, "name")?, arg_float(k, "wait")?)),
        "open_url" => s(h.open_url(&arg_str(k, "url")?, arg_opt_str(k, "app")?.as_deref(), arg_float(k, "wait")?)),
        "frontmost" => Ok(to_json(&h.frontmost())),
        "screen_text" => {
            h.screen_text(arg_region(k)?, &arg_str(k, "level")?).map(|v| to_json(&v)).map_err(lane_error)
        }
        "find_text" => h
            .find_text(&arg_str(k, "text")?, arg_region(k)?, &arg_str(k, "level")?)
            .map(|v| to_json(&v))
            .map_err(lane_error),
        "click_text" => s(h.click_text(
            &arg_str(k, "text")?,
            arg_region(k)?,
            arg_float(k, "timeout")?,
            arg_clicks(k, "clicks")?,
        )),
        "wait_for" => {
            let waited = h
                .wait_for(
                    &arg_str(k, "text")?,
                    py_truthy(kw(k, "gone")),
                    arg_float(k, "timeout")?,
                    arg_float(k, "interval")?,
                    arg_region(k)?,
                )
                .map_err(lane_error)?;
            Ok(match waited {
                Waited::Gone => Value::Bool(true),
                Waited::StillThere => Value::Bool(false),
                Waited::Found(item) => to_json(&item),
                Waited::NotSeen => Value::Null,
            })
        }
        "wait_settled" => h
            .wait_settled(arg_float(k, "timeout")?, arg_float(k, "interval")?)
            .map(Value::Bool)
            .map_err(lane_error),
        "type_text" => s(h.type_text(&arg_str(k, "text")?, py_truthy(kw(k, "submit")))),
        "zoom" => s(h.zoom(arg_int(k, "x")?, arg_int(k, "y")?, arg_int(k, "w")?, arg_int(k, "h")?)),
        "observe" => s(h.observe(&arg_str(k, "note")?)),
        "delegate" => {
            let steps = match kw(k, "steps") {
                Value::Null => None,
                Value::Array(items) => Some(
                    items
                        .iter()
                        .map(|i| i.as_str().map(str::to_string).ok_or_else(|| type_error("steps", "strings", i)))
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                other => return Err(type_error("steps", "a list", other)),
            };
            let call = DelegateCall {
                objective: if steps.is_some() { String::new() } else { arg_str(k, "objective")? },
                text: arg_opt_str(k, "text")?,
                key: arg_opt_str(k, "key")?,
                done_when: arg_opt_str(k, "done_when")?,
                approve: arg_opt_str(k, "approve")?,
                max_steps: if steps.is_some() { 4 } else { arg_int(k, "max_steps")? },
                dry_run: py_truthy(kw(k, "dry_run")),
                steps,
            };
            Ok(Value::String(h.delegate(call)))
        }
        other => Err(LaneError::new("NameError", format!("name '{other}' is not defined"))),
    }
}

// ---- the fast lane ----------------------------------------------------------------------------------------

fn switch(raw: &str, default: bool) -> bool {
    let raw = py_strip(raw).to_lowercase();
    if raw.is_empty() {
        default
    } else {
        !["0", "false", "no", "off"].contains(&raw.as_str())
    }
}

/// (enabled, style) from EVE_FAST_LANE / EVE_FAST_LANE_STYLE. The default for the switch is
/// fast_lane::FAST_LANE_DEFAULT, which the holdout eval sets.
pub fn fast_lane_config(env: &Env) -> (bool, String) {
    let enabled = switch(env_get(env, "EVE_FAST_LANE"), FAST_LANE_DEFAULT);
    let mut style = py_strip(env_get(env, "EVE_FAST_LANE_STYLE")).to_lowercase();
    if !eve_decider::STYLES.contains(&style.as_str()) {
        style = DEFAULT_STYLE.to_string(); // "jev": the style Jev scored best with on select (eve_jev)
    }
    (enabled, style)
}

/// (who decides a step, whether the router may run) from EVE_FAST_LANE_DECIDE and EVE_LANE_FIRST. The defaults are
/// fast_lane::DEFAULT_DECIDE and fast_lane::LANE_FIRST_DEFAULT.
pub fn lane_modes(env: &Env) -> (String, bool) {
    let mut decide = py_strip(env_get(env, "EVE_FAST_LANE_DECIDE")).to_lowercase();
    if !DECIDE_MODES.contains(&decide.as_str()) {
        decide = DEFAULT_DECIDE.to_string();
    }
    let first = switch(env_get(env, "EVE_LANE_FIRST"), LANE_FIRST_DEFAULT);
    (decide, first)
}

pub const DECIDER_BACKENDS: [&str; 1] = ["jev"];

/// Who answers the lane's one `choice` question in `model` mode: EVE_DECIDER, unset by default.
///
/// "jev" is TypeSafe's hosted System One model over eve_decider::Decider's predict seam (eve_jev): a network call,
/// and the state it sends carries the focused window's title and visible text, so it runs only when the owner
/// names it. Unset or any other value is "" — no decider, and `model` mode stays off rather than sending anything
/// anywhere.
pub fn decider_backend(env: &Env) -> String {
    let name = py_strip(env_get(env, "EVE_DECIDER")).to_lowercase();
    if DECIDER_BACKENDS.contains(&name.as_str()) {
        name
    } else {
        String::new()
    }
}

/// What a decider loader hands back: the decider and its `load_ms` / `warm_ms` for the status line.
pub struct Loaded {
    pub decider: Box<dyn LaneDecider + Send>,
    pub load_ms: f64,
    pub warm_ms: f64,
}

/// `loader(style)`: load the lane's decider (Python's `loader` argument; `jev.load` by default). The error is the
/// exception it raised (kind and message).
pub type Loader = Box<dyn FnOnce(&str) -> Result<Loaded, LaneError> + Send>;

/// The default loader: `jev.load(env=env, style=style)`. `opener` is eve_jev's socket seam (None: real HTTPS).
pub fn jev_loader(env: Env, opener: Option<Arc<dyn eve_jev::Opener>>) -> Loader {
    Box::new(move |style: &str| {
        let decider = eve_jev::load(&env, eve_jev::LoadOptions { style: style.to_string(), opener, ..Default::default() })
            .map_err(|e| LaneError::new(e.kind, e.message))?;
        let (load_ms, warm_ms) = (decider.load_ms, decider.warm_ms);
        Ok(Loaded { decider: Box::new(decider), load_ms, warm_ms })
    })
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_string())
}

/// Turn the lane on for `helpers`, and in `model` mode load the decider off the critical path.
///
/// Returns the status the ready line carries: "ready (keyword gate)" (no model is needed), "loading" (a background
/// thread is running `loader(style)`, [`jev_loader`] by default), or "off (<reason>)". Later replies carry
/// `helpers.fast_lane_status`: "ready (load … warm …)" or "failed: <one line>" — the delegate helper answers
/// `unavailable` until ready.
pub fn start_fast_lane(helpers: &SharedHelpers, env: &Env, loader: Option<Loader>, thread: bool) -> String {
    start_fast_lane_on(SYS_PLATFORM, helpers, env, loader, thread)
}

/// [`start_fast_lane`] for a given `sys.platform`.
pub fn start_fast_lane_on(
    platform: &str,
    helpers: &SharedHelpers,
    env: &Env,
    loader: Option<Loader>,
    thread: bool,
) -> String {
    let (enabled, style) = fast_lane_config(env);
    let (decide, first) = lane_modes(env);
    let backend = decider_backend(env);
    let router = {
        let mut h = helpers.lock();
        h.lane_decide = decide.clone();
        h.lane_first_on = first && platform == "darwin";
        let router = if h.lane_first_on { "; router on" } else { "" };
        if !enabled {
            h.fast_lane_status = format!("off (EVE_FAST_LANE){router}");
            return h.fast_lane_status.clone();
        }
        if platform != "darwin" {
            h.fast_lane_status = format!("off (not macOS: {platform})");
            return h.fast_lane_status.clone();
        }
        if decide == "keyword" || (decide == "jev" && backend.is_empty()) {
            // The keyword gate is code, and in "jev" mode the step asker (start_jev_asker) decides: neither needs a
            // decider, so the lane is ready now and nothing is loaded.
            h.fast_lane = true;
            let what = if decide == "keyword" { "ready (keyword gate)" } else { "ready (jev step asker; no decider)" };
            h.fast_lane_status = format!("{what}{router}");
            return h.fast_lane_status.clone();
        }
        if backend.is_empty() {
            h.fast_lane_status = format!("off (model mode needs EVE_DECIDER=jev){router}");
            return h.fast_lane_status.clone();
        }
        h.fast_lane = true;
        h.fast_lane_status = "loading".to_string();
        router
    };
    let loader = loader.unwrap_or_else(|| jev_loader(env.clone(), None));
    let target = helpers.clone();
    let load = move || {
        let outcome = catch_unwind(AssertUnwindSafe(|| loader(&style)));
        let mut h = target.lock();
        match outcome {
            Ok(Ok(loaded)) => {
                let line = format!("load {:.0} ms, warm {:.0} ms, style {style}", loaded.load_ms, loaded.warm_ms);
                h.decider = Some(loaded.decider);
                h.fast_lane_status = format!("ready ({line}){router}");
            }
            // the lane stays unavailable; the model keeps its ordinary helpers
            Ok(Err(e)) => h.fast_lane_status = py_take(&format!("failed: {}: {}", e.kind, e.message), 200),
            Err(p) => h.fast_lane_status = py_take(&format!("failed: panic: {}", panic_text(p.as_ref())), 200),
        }
    };
    if thread {
        if let Err(e) = std::thread::Builder::new().name("fast-lane-loader".into()).spawn(load) {
            helpers.lock().fast_lane_status = py_take(&format!("failed: RuntimeError: {e}"), 200);
        }
    } else {
        load();
    }
    let status = helpers.lock().fast_lane_status.clone();
    status
}

/// Give `helpers` a Jev step asker when the owner's switches call for one: the lane's "jev" decide mode
/// (EVE_FAST_LANE_DECIDE=jev) or the plan executor (EVE_PLAN_EXEC=1). Returns a status for the ready line. No
/// request is sent here — the first step pays its own 240 ms — and a route with no key leaves the asker unset, so
/// the keyword gate decides alone rather than the lane going dark.
pub fn start_jev_asker(helpers: &mut Helpers, env: &Env) -> String {
    let (decide, _first) = lane_modes(env);
    let plan_exec = ["1", "true", "yes", "on"].contains(&py_strip(env_get(env, "EVE_PLAN_EXEC")).to_lowercase().as_str());
    if decide != "jev" && !plan_exec {
        return "off".to_string();
    }
    let (url, key, model) = match eve_jev::route_config(env, "") {
        Ok(route) => route,
        Err(e) => return py_take(&format!("off ({})", e.0), 160),
    };
    let seconds = eve_jev::timeout_from_env(env);
    let predict = eve_jev::make_predict(&url, &key, &model, eve_jev::PredictOptions { timeout_s: seconds, ..Default::default() });
    let asker = eve_typed_ask::make_step_asker(predict.anyhow_fn(), eve_typed_ask::perf_counter, Some(env));
    helpers.step_asker = Some(Box::new(asker));
    format!("ready ({model})")
}

// ---- the planner's operations ------------------------------------------------------------------------------

fn lines_collector(helpers: &mut Helpers) -> Arc<Mutex<Vec<String>>> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    helpers.bind(Box::new(move |line: &str| sink.lock().push(line.to_string())), Box::new(|_image| {}));
    helpers.begin();
    lines
}

fn timing_json(helpers: &Helpers) -> Value {
    json!(helpers.timing_ms())
}

/// The `outline` operation: the front window's controls, as the planner is shown them.
pub fn outline(_req: &Value, helpers: Option<&mut Helpers>) -> Value {
    let Some(h) = helpers else {
        return json!({"outline": {"app": "", "lines": [], "error": "no helpers in this worker"}});
    };
    // no outline means the planner plans blind, never a protocol error
    match catch_unwind(AssertUnwindSafe(|| h.outline(80))) {
        Ok(Ok(shown)) => json!({"outline": shown}),
        Ok(Err(e)) => json!({"outline": {"app": "", "lines": [],
                                          "error": py_take(&format!("{}: {}", error_kind(&e), error_message(&e)), 200)}}),
        Err(p) => json!({"outline": {"app": "", "lines": [],
                                     "error": py_take(&format!("panic: {}", panic_text(p.as_ref())), 200)}}),
    }
}

/// `int(v or 0)` of a decoded JSON value, with Python's errors.
pub fn py_int_or_zero(v: Option<&Value>) -> Result<i64, LaneError> {
    let v = match v {
        Some(v) if py_truthy(v) => v,
        _ => return Ok(0),
    };
    match v {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Number(n) => Ok(n.as_i64().unwrap_or_else(|| n.as_f64().map(|f| f.trunc() as i64).unwrap_or(0))),
        Value::String(s) => {
            let t = py_strip(s);
            let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
            let valid = !digits.is_empty()
                && !digits.starts_with('_')
                && !digits.ends_with('_')
                && !digits.contains("__")
                && digits.chars().all(|c| c.is_ascii_digit() || c == '_');
            let parsed = if valid { t.replace('_', "").parse::<i64>().ok() } else { None };
            parsed.ok_or_else(|| {
                LaneError::new("ValueError", format!("invalid literal for int() with base 10: {}", eve_decider::py_str_repr(s)))
            })
        }
        other => Err(LaneError::new(
            "TypeError",
            format!("int() argument must be a string, a bytes-like object or a real number, not '{}'", py_type_name(other)),
        )),
    }
}

/// `str(v)` of a decoded JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => eve_decider::py_repr(other),
    }
}

/// What the `run_plan` operation hands the helpers, read from the request as Python reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct RunPlanArgs {
    pub request: String,
    pub start: i64,
    pub approved: Option<Map<String, Value>>,
    pub dry_run: bool,
    pub success_rejected: bool,
}

/// `str(req.get("request") or "")`, `int(req.get("start") or 0)`, `approved` when it is a dict, and the two
/// `bool(...)` switches. The `int()` is the only part that can raise.
pub fn run_plan_args(req: &Value) -> Result<RunPlanArgs, LaneError> {
    let request = match req.get("request") {
        Some(v) if py_truthy(v) => py_str(v),
        _ => String::new(),
    };
    Ok(RunPlanArgs {
        request,
        start: py_int_or_zero(req.get("start"))?,
        approved: req.get("approved").and_then(Value::as_object).cloned(),
        dry_run: req.get("dry_run").is_some_and(py_truthy),
        success_rejected: req.get("success_rejected").is_some_and(py_truthy),
    })
}

/// The `run_plan` operation: walk a plan with no planner turn between its steps (plan_executor).
///
/// Err is the one exception Python let escape: a `start` that `int()` refuses fails inside the `try` and again in
/// its `except`, which ends the worker.
pub fn run_plan(req: &Value, helpers: Option<&mut Helpers>) -> Result<Value, LaneError> {
    let plan = req.get("plan").filter(|p| p.is_object());
    let (Some(plan), Some(h)) = (plan, helpers) else {
        return Ok(json!({"run_plan": {"status": "unavailable", "reason": "run_plan needs a plan object"}}));
    };
    let lines = lines_collector(h);
    let mut result = match run_plan_args(req) {
        Ok(args) => {
            let walked = catch_unwind(AssertUnwindSafe(|| {
                h.run_plan(plan, &args.request, args.start, args.approved.as_ref(), args.dry_run, args.success_rejected)
            }));
            match walked {
                Ok(Value::Object(m)) => m,
                Ok(other) => {
                    let mut m = Map::new();
                    m.insert("result".into(), other);
                    m
                }
                Err(p) => json!({"status": "partial", "next_index": args.start, "ledger": [],
                                 "reason": py_take(&format!("error:panic: {}", panic_text(p.as_ref())), 200)})
                .as_object()
                .cloned()
                .unwrap_or_default(),
            }
        }
        Err(e) => return Err(e),
    };
    if !result.contains_key("log") {
        result.insert("log".into(), json!(*lines.lock()));
    }
    Ok(json!({"run_plan": Value::Object(result), "timing": timing_json(h)}))
}

/// The `lane_first` operation: the router's attempt at the goal before any planner call
/// (lane_router::RouteResult::to_dict(), plus the worker's timing). Never a terminal error: a worker without the
/// router answers {"status": "unavailable"} and the planner runs as before.
pub fn lane_first(req: &Value, helpers: Option<&mut Helpers>) -> Value {
    let goal = match req.get("goal") {
        Some(Value::String(g)) if !py_strip(g).is_empty() => g.clone(),
        _ => return json!({"lane_first": {"status": "unavailable", "reason": "lane_first needs a string goal"}}),
    };
    let h = match helpers {
        Some(h) if h.lane_first_on => h,
        _ => return json!({"lane_first": {"status": "unavailable", "reason": "the router is off (EVE_LANE_FIRST)"}}),
    };
    let lines = lines_collector(h);
    let dry_run = req.get("dry_run").is_some_and(py_truthy);
    // the router failing is "the planner does it"
    let mut result = match catch_unwind(AssertUnwindSafe(|| h.lane_first(&goal, dry_run))) {
        Ok(Value::Object(m)) => m,
        Ok(_) | Err(_) => json!({"status": "none", "reason": "error:panic"}).as_object().cloned().unwrap_or_default(),
    };
    if !result.contains_key("log") {
        result.insert("log".into(), json!(*lines.lock()));
    }
    json!({"lane_first": Value::Object(result), "timing": timing_json(h)})
}

// ---- the line protocol --------------------------------------------------------------------------------------

fn with_id(rid: &Value, result: Value) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), rid.clone());
    if let Value::Object(rest) = result {
        for (k, v) in rest {
            m.insert(k, v);
        }
    }
    Value::Object(m)
}

/// Read requests from `stream` until it ends, answering each through `emit`. Err when the exec host is gone
/// ([`HostDied`]) or `run_plan` raised the error Python let through.
pub fn serve(
    ns: &Namespace,
    stream: impl BufRead,
    helpers: Option<&SharedHelpers>,
    emit: &mut dyn FnMut(&Value),
) -> anyhow::Result<()> {
    let mut terminal: Option<Value> = None;
    let mut stream = stream;
    let mut raw = Vec::new();
    loop {
        raw.clear();
        if stream.read_until(b'\n', &mut raw)? == 0 {
            return Ok(());
        }
        if raw.len() > MAX_REQUEST_BYTES {
            emit(&json!({"error": {"code": "request_too_large", "message": "request exceeds 512 KiB"}}));
            continue;
        }
        let line = String::from_utf8_lossy(&raw);
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                emit(&json!({"error": {"code": "bad_json", "message": "request is not JSON"}}));
                continue;
            }
        };
        let rid = req.get("id").cloned().unwrap_or(Value::Null);
        if let Some(err) = &terminal {
            emit(&json!({"id": rid, "error": err}));
            continue;
        }
        let operation = req.get("operation").and_then(Value::as_str);
        let mut guard = helpers.map(|h| h.lock());
        match operation {
            Some("observe") => {
                let result = execute("observe()", ns, guard.as_deref_mut())?;
                terminal = result.get("error").cloned();
                emit(&with_id(&rid, result));
            }
            Some("lane_first") => emit(&with_id(&rid, lane_first(&req, guard.as_deref_mut()))),
            Some("outline") => emit(&with_id(&rid, outline(&req, guard.as_deref_mut()))),
            Some("run_plan") => {
                let result = run_plan(&req, guard.as_deref_mut()).map_err(anyhow::Error::new)?;
                emit(&with_id(&rid, result));
            }
            Some("execute") => {
                let code = req.get("code").and_then(Value::as_str).filter(|c| !py_strip(c).is_empty() && c.len() <= MAX_CODE_BYTES);
                let Some(code) = code else {
                    emit(&json!({"id": rid, "error": {"code": "bad_code",
                                                      "message": "code must be a non-empty string under 64 KiB"}}));
                    continue;
                };
                let result = execute(code, ns, guard.as_deref_mut())?;
                terminal = result.get("error").cloned();
                emit(&with_id(&rid, result));
            }
            _ => emit(&json!({"id": rid, "error": {"code": "unsupported",
                                                     "message": "only execute, observe, lane_first, outline and run_plan are supported"}})),
        }
    }
}

// ---- --release -------------------------------------------------------------------------------------------------

/// `kCGHIDEventTap`.
pub const K_CG_HID_EVENT_TAP: u32 = 0;
/// `kCGEventLeftMouseUp`, `kCGEventRightMouseUp`, `kCGEventOtherMouseUp`.
pub const K_CG_EVENT_LEFT_MOUSE_UP: u32 = 2;
pub const K_CG_EVENT_RIGHT_MOUSE_UP: u32 = 4;
pub const K_CG_EVENT_OTHER_MOUSE_UP: u32 = 26;
/// `kCGMouseButtonLeft`, `kCGMouseButtonRight`, `kCGMouseButtonCenter`.
pub const K_CG_MOUSE_BUTTON_LEFT: u32 = 0;
pub const K_CG_MOUSE_BUTTON_RIGHT: u32 = 1;
pub const K_CG_MOUSE_BUTTON_CENTER: u32 = 2;

/// The Quartz event calls release_inputs makes (pyobjc's `Quartz` module, as far as it is used).
pub trait Quartz {
    type Event;
    type Point: Copy;
    /// `CGEventCreateKeyboardEvent(None, code, down)`
    fn create_keyboard_event(&mut self, code: u16, down: bool) -> Self::Event;
    /// `CGEventCreate(None)`
    fn create(&mut self) -> Self::Event;
    /// `CGEventGetLocation(event)`
    fn get_location(&mut self, event: &Self::Event) -> Self::Point;
    /// `CGEventCreateMouseEvent(None, kind, location, button)`
    fn create_mouse_event(&mut self, kind: u32, location: Self::Point, button: u32) -> Self::Event;
    /// `CGEventPost(tap, event)`
    fn post(&mut self, tap: u32, event: Self::Event);
}

/// Post a key-up for every key code (0..127) and a mouse-up for each button.
///
/// A killed worker can leave a modifier or a mouse button held at the HID level; posting the ups clears that. Port
/// of openai/openai-cua-sample-app python-app/app/desktop/release_inputs.py (MIT). Returns the events posted.
pub fn release_inputs<Q: Quartz>(quartz: &mut Q) -> usize {
    let mut count = 0;
    for code in 0u16..128 {
        let ev = quartz.create_keyboard_event(code, false);
        quartz.post(K_CG_HID_EVENT_TAP, ev);
        count += 1;
    }
    let probe = quartz.create();
    let location = quartz.get_location(&probe);
    let buttons = [
        (K_CG_EVENT_LEFT_MOUSE_UP, K_CG_MOUSE_BUTTON_LEFT),
        (K_CG_EVENT_RIGHT_MOUSE_UP, K_CG_MOUSE_BUTTON_RIGHT),
        (K_CG_EVENT_OTHER_MOUSE_UP, K_CG_MOUSE_BUTTON_CENTER),
    ];
    for (kind, button) in buttons {
        let ev = quartz.create_mouse_event(kind, location, button);
        quartz.post(K_CG_HID_EVENT_TAP, ev);
        count += 1;
    }
    count
}

// ---- main -------------------------------------------------------------------------------------------------------

/// The worker process: check the desktop, build the namespace (`{"__builtins__", "pyautogui", "time"}` plus the
/// helpers), start the lane, emit the ready line, then serve stdin until it closes. `check` is `--check`: stop
/// after the ready line.
pub fn main_with(check: bool) -> anyhow::Result<()> {
    let ns = Namespace::spawn(&default_python())?;
    ns.init(&["pyautogui", "time"])?;
    let (width, height) = check_desktop(&ns)?;
    normalize_screenshots(&ns)?;
    ns.setup("pyautogui.FAILSAFE = True\npyautogui.PAUSE = 0.05")?;
    // __builtins__ stays open on purpose: exec_py is unrestricted Python as the daemon user (docs/stackchan/voice.md,
    // Safety); the helpers exist so the model has no reason to reach past them.
    let helpers: SharedHelpers = Arc::new(Mutex::new(Helpers::new(
        Box::new(HostGui::new(&ns)),
        eve_desktop_lanes::desktop_helpers::Backends::default(),
    )));
    let env = process_env();
    let fast_lane = start_fast_lane(&helpers, &env, None, true);
    let jev_asker = start_jev_asker(&mut helpers.lock(), &env);
    install(&mut helpers.lock(), &ns)?;
    emit(&json!({"ready": true, "platform": ns.platform(), "width": width, "height": height,
                 "fast_lane": fast_lane, "jev_asker": jev_asker}))?;
    READY.store(true, std::sync::atomic::Ordering::SeqCst);
    if check {
        return Ok(());
    }
    watch_parent();
    let stdin = std::io::stdin();
    // A reply that cannot be written means the daemon is gone; Python's emit raised and the worker ended.
    let mut out = |payload: &Value| {
        if let Err(e) = emit(payload) {
            eprintln!("desktop worker: writing a reply failed: {e}");
            std::process::exit(1);
        }
    };
    serve(&ns, stdin.lock(), Some(&helpers), &mut out)
}

static READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether [`main_with`] has emitted its ready line.
pub fn ready_sent() -> bool {
    READY.load(std::sync::atomic::Ordering::SeqCst)
}

/// Kill this process when the daemon that started it goes away (the exec host watches this process the same way).
fn watch_parent() {
    // SAFETY: getppid / getpid / kill take no pointers.
    let parent = unsafe { libc::getppid() };
    let _ = std::thread::Builder::new().name("watch-parent".into()).spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        // SAFETY: as above.
        unsafe {
            if libc::getppid() != parent {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
        }
    });
}

#[cfg(test)]
mod tests;
