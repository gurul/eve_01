//! Host vision: face detection on the Mac for the board's camera frames.
//!
//! The board streams small camera frames over the serial link and the daemon
//! answers each one with where the largest face is, so the robot can turn to
//! look at the person. The Mac does the detection because the board's own
//! detector is slow and coarse; on-board tracking is only the fallback for
//! when no host is attached.
//!
//! Wire contract (fixed; the firmware side is built against it):
//!
//! ```text
//!     host -> board  {"cmd":"cam","on":true,"fps":5,"w":160,"h":120}
//!     host -> board  {"cmd":"cam","on":false}
//!     board -> host  {"frame":{"seq":n,"w":160,"h":120,"fmt":"jpeg"|"gray",
//!                              "b64":"...","yaw":<deg>,"pitch":<deg>}}
//!     host -> board  {"cmd":"face","seq":n,"bx":-100..100,"by":-100..100,
//!                     "size":0..100,"conf":0..100,"yaw":<echo>,"pitch":<echo>,
//!                     "who":"unknown"}
//!     host -> board  {"cmd":"face","seq":n,"conf":0,"who":"unknown"}   (no face)
//! ```
//!
//! `who` is the identity slot: "owner" when identity matched the largest
//! face against the enrolled owner prints, "unknown" otherwise.
//!
//! `fmt:"gray"` is raw 8-bit luma, w*h bytes. `fmt:"jpeg"` is a baseline
//! JPEG. `bx`/`by` is the face centre relative to the frame centre
//! (+bx = right of frame, +by = down), `size` is the face width as a
//! percentage of the frame width.
//!
//! Two halves, kept apart so the geometry and the drop policy are testable
//! without a Mac:
//!
//! * Pure core — [`decode_frame`], [`pick_face`], [`build_face_cmd`],
//!   [`FrameRateGovernor`] and the [`FaceTracker`] driver, which takes any
//!   [`Detector`] callable.
//! * Platform adapter, darwin — [`make_detector`] wraps the macOS Vision
//!   framework (`VNDetectFaceRectanglesRequest`) via objc2. It returns None
//!   when Vision is unavailable and logs one warning; the daemon then never
//!   asks the board to stream.
//!
//! Thread model: the detector runs on a single worker thread so the tokio
//! runtime never blocks on a detect. While a detect is in flight, newer
//! frames replace each other in a one-slot "latest" holder — the governor
//! never queues, so the board always gets an answer for a recent frame
//! rather than a backlog of stale ones.

use std::fmt;
use std::io::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use futures::future::BoxFuture;
use parking_lot::{Mutex, RwLock};
use serde_json::{Map, Value};

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub use mac::cgimage_from_frame;

#[cfg(test)]
mod tests;

/// What we ask the board to stream. 160x120 at 5 fps is ~4 KB/frame as JPEG
/// and ~26 KB/frame as base64 gray — both fine on the USB link, and Vision
/// finds a face at arm's length in 160 px wide frames.
pub const CAM_FPS: i64 = 5;
pub const CAM_W: i64 = 160;
pub const CAM_H: i64 = 120;

/// Bench debugging: write at most one received frame per second to disk.
pub const SAVE_FRAMES_MIN_INTERVAL: f64 = 1.0;
/// Stats line cadence in the daemon.
pub const STATS_INTERVAL_SECS: f64 = 30.0;

pub const FRAME_FORMATS: [&str; 2] = ["jpeg", "gray"];
/// Identity slot on every face cmd. Detection only knows there is a face;
/// identity says "owner".
pub const WHO_UNKNOWN: &str = "unknown";

/// Python's `ValueError` from this module: a malformed frame or luma buffer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ValueError(pub String);

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub seq: i64,
    pub w: i64,
    pub h: i64,
    /// "jpeg" | "gray"
    pub fmt: String,
    /// decoded payload: JPEG file bytes, or w*h luma bytes
    pub data: Vec<u8>,
    pub yaw: Option<f64>,
    pub pitch: Option<f64>,
    /// a one-off full-size photo answering {"cmd":"snap"}, not a stream frame
    pub snap: bool,
}

impl Frame {
    /// `Frame(seq, w, h, fmt, data)` with the defaults for yaw, pitch and snap.
    pub fn new(seq: i64, w: i64, h: i64, fmt: &str, data: Vec<u8>) -> Self {
        Frame { seq, w, h, fmt: fmt.to_string(), data, yaw: None, pitch: None, snap: false }
    }
}

/// A detected face in top-left-origin pixel coordinates of the frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// 0..1
    pub conf: f64,
}

impl Rect {
    /// `Rect(x, y, w, h)` with the default `conf=1.0`.
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Rect { x, y, w, h, conf: 1.0 }
    }

    pub fn with_conf(x: f64, y: f64, w: f64, h: f64, conf: f64) -> Self {
        Rect { x, y, w, h, conf }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceResult {
    /// -100..100, + = right of frame centre
    pub bx: i64,
    /// -100..100, + = below frame centre
    pub by: i64,
    /// 0..100, face width as % of frame width
    pub size: i64,
    /// 0..100
    pub conf: i64,
}

/// A detector takes a frame and returns every face it sees, in pixel coords.
/// It runs on the worker thread, so it may block. An `Err` is Python's
/// exception from the detector: logged, and the frame answers "no face".
pub type Detector = Arc<dyn Fn(&Frame) -> anyhow::Result<Vec<Rect>> + Send + Sync>;
/// The transport's send coroutine.
pub type Sender = Arc<dyn Fn(Value) -> BoxFuture<'static, bool> + Send + Sync>;
/// Every face of every processed frame, with the pose the frame was taken at.
pub type OnFaces = Arc<
    dyn Fn(Vec<FaceResult>, Option<f64>, Option<f64>) -> BoxFuture<'static, anyhow::Result<()>>
        + Send
        + Sync,
>;
/// Seconds on a monotonic clock (Python's `time.monotonic`).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// identity's FaceIdentity: `who` for the largest face. Worker thread.
pub trait Identity: Send + Sync {
    fn process(&self, frame: &Frame, rects: &[Rect]) -> anyhow::Result<String>;
}

// ---- pure core --------------------------------------------------------------

/// `build_cam_cmd(on)` with the default fps/w/h.
pub fn build_cam_cmd(on: bool) -> Value {
    build_cam_cmd_with(on, CAM_FPS, CAM_W, CAM_H)
}

pub fn build_cam_cmd_with(on: bool, fps: i64, w: i64, h: i64) -> Value {
    let mut m = Map::new();
    m.insert("cmd".into(), "cam".into());
    if !on {
        m.insert("on".into(), false.into());
        return Value::Object(m);
    }
    m.insert("on".into(), true.into());
    m.insert("fps".into(), fps.into());
    m.insert("w".into(), w.into());
    m.insert("h".into(), h.into());
    Value::Object(m)
}

/// Python's `type(v).__name__` for a JSON value.
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `float.__repr__` for the values JSON can carry.
fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == f.trunc() && f.abs() < 1e16 {
        return format!("{f:.1}");
    }
    let s = format!("{f}");
    if f.abs() >= 1e16 || (f != 0.0 && f.abs() < 1e-4) {
        // repr switches to exponent form; Rust's LowerExp is the same digits.
        let e = format!("{f:e}");
        if let Some((mant, exp)) = e.split_once('e') {
            let exp: i32 = exp.parse().unwrap_or(0);
            let sign = if exp < 0 { '-' } else { '+' };
            return format!("{mant}e{sign}{:02}", exp.abs());
        }
    }
    s
}

/// Python's `str(v)` for a JSON value, the way `json.loads` would have built it.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match n.as_i64().map(|i| i.to_string()).or_else(|| n.as_u64().map(|u| u.to_string())) {
            Some(s) => s,
            None => py_float_repr(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => s.clone(),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Python's `repr(v)` for a JSON value.
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        other => py_str(other),
    }
}

/// Python's truthiness of a JSON value.
fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `int(s)` for a string: surrounding whitespace, a sign, digits
/// with single underscores between them.
fn py_int_str(s: &str) -> Option<i64> {
    let t = s.trim();
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut acc: i64 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let d = c.to_digit(10)? as i64;
        acc = acc.checked_mul(10)?.checked_add(d)?;
    }
    Some(if neg { -acc } else { acc })
}

/// Python's `int(obj[key])`, with the message `str(e)` of the exception it raises.
fn py_int_field(obj: &Value, key: &str) -> Result<i64, String> {
    let Value::Object(map) = obj else {
        // Indexing a non-dict with a str key: TypeError.
        return Err(match obj {
            Value::Array(_) => "list indices must be integers or slices, not str".into(),
            Value::String(_) => "string indices must be integers, not 'str'".into(),
            other => format!("'{}' object is not subscriptable", py_type_name(other)),
        });
    };
    let Some(v) = map.get(key) else {
        return Err(py_repr_str(key));
    };
    match v {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else if let Some(f) = n.as_f64() {
                if f.is_nan() {
                    Err("cannot convert float NaN to integer".into())
                } else if f.is_infinite() {
                    Err("cannot convert float infinity to integer".into())
                } else if f.trunc().abs() >= i64::MAX as f64 {
                    // Python would make a big int; nothing on the wire is that large.
                    Err(format!("{} does not fit in 64 bits", py_float_repr(f)))
                } else {
                    Ok(f.trunc() as i64)
                }
            } else {
                Err(format!("invalid literal for int() with base 10: {}", py_repr(v)))
            }
        }
        Value::String(s) => {
            py_int_str(s).ok_or_else(|| format!("invalid literal for int() with base 10: {}", py_repr_str(s)))
        }
        other => Err(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            py_type_name(other)
        )),
    }
}

/// `base64.b64decode(s, validate=True)` as CPython 3.13 (the daemon's
/// interpreter) runs it: the str must be ASCII, then
/// `binascii.a2b_base64(s, strict_mode=True)`, character by character, with
/// its messages. Non-zero trailing bits are accepted.
fn b64decode_validate(s: &str) -> Result<Vec<u8>, String> {
    if !s.is_ascii() {
        return Err("string argument should contain only ASCII characters".into());
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 3);
    if bytes.first() == Some(&b'=') {
        return Err("Leading padding not allowed".into());
    }
    let one_more = |decoded: usize| {
        format!(
            "Invalid base64-encoded string: number of data characters ({}) cannot be 1 more than a multiple of 4",
            decoded / 3 * 4 + 1
        )
    };
    let mut quad_pos = 0u8;
    let mut leftchar = 0u8;
    let mut pads = 0u8;
    let mut padding_started = false;
    let mut padding_done = false;
    for &c in bytes {
        if c == b'=' {
            padding_started = true;
            if quad_pos == 0 {
                return Err("Excess padding not allowed".into());
            }
            if quad_pos == 1 {
                return Err(one_more(out.len()));
            }
            pads += 1;
            if quad_pos + pads >= 4 {
                // A complete pad sequence: anything after it is an error.
                quad_pos = 0;
                pads = 0;
                padding_done = true;
            }
            continue;
        }
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err("Only base64 data is allowed".into()),
        };
        if padding_done {
            return Err("Excess data after padding".into());
        }
        if padding_started {
            return Err("Discontinuous padding not allowed".into());
        }
        match quad_pos {
            0 => {
                quad_pos = 1;
                leftchar = v;
            }
            1 => {
                quad_pos = 2;
                out.push((leftchar << 2) | (v >> 4));
                leftchar = v & 0x0f;
            }
            2 => {
                quad_pos = 3;
                out.push((leftchar << 4) | (v >> 2));
                leftchar = v & 0x03;
            }
            _ => {
                quad_pos = 0;
                out.push((leftchar << 6) | v);
                leftchar = 0;
            }
        }
    }
    match quad_pos {
        0 => Ok(out),
        1 => Err(one_more(out.len())),
        _ => Err("Incorrect padding".into()),
    }
}

/// Turn the `frame` object off the wire into a Frame.
///
/// Errors for anything malformed: bad base64, unknown format, a gray payload
/// whose length does not match w*h, or a JPEG payload without the SOI marker.
pub fn decode_frame(obj: &Value) -> Result<Frame, ValueError> {
    let ints = (|| -> Result<(i64, i64, i64), String> {
        Ok((py_int_field(obj, "seq")?, py_int_field(obj, "w")?, py_int_field(obj, "h")?))
    })();
    let (seq, w, h) = ints.map_err(|e| ValueError(format!("frame: bad seq/w/h: {e}")))?;
    if w <= 0 || h <= 0 {
        return Err(ValueError(format!("frame: bad size {w}x{h}")));
    }
    let get = |k: &str| obj.get(k);
    let fmt = match get("fmt") {
        Some(v) if py_truthy(v) => py_str(v),
        _ => String::new(),
    };
    if !FRAME_FORMATS.contains(&fmt.as_str()) {
        return Err(ValueError(format!("frame: unknown fmt {}", py_repr_str(&fmt))));
    }
    let b64 = match get("b64") {
        Some(Value::String(s)) if !s.is_empty() => s.as_str(),
        _ => return Err(ValueError("frame: missing b64".into())),
    };
    let data = b64decode_validate(b64).map_err(|e| ValueError(format!("frame: bad base64: {e}")))?;
    let expected = w.checked_mul(h);
    if fmt == "gray" && expected != Some(data.len() as i64) {
        return Err(ValueError(format!(
            "frame: gray payload is {} bytes, expected {}",
            data.len(),
            w as i128 * h as i128
        )));
    }
    if fmt == "jpeg" && !data.starts_with(b"\xff\xd8") {
        return Err(ValueError("frame: jpeg payload lacks SOI marker".into()));
    }
    Ok(Frame {
        seq,
        w,
        h,
        fmt,
        data,
        yaw: opt_float(get("yaw")),
        pitch: opt_float(get("pitch")),
        snap: matches!(get("snap"), Some(Value::Bool(true))),
    })
}

/// Ask the board for one full-resolution photo (a frame line with `"snap":true`).
pub fn build_snap_cmd() -> Value {
    let mut m = Map::new();
    m.insert("cmd".into(), "snap".into());
    Value::Object(m)
}

/// A JSON number as a float; bools, strings and anything else are None.
pub fn opt_float(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

/// `max(lo, min(hi, int(round(v))))` — Python's round is half-to-even.
pub fn clamp(v: f64, lo: i64, hi: i64) -> i64 {
    let r = v.round_ties_even();
    let i = if r.is_nan() { 0 } else { r as i64 };
    lo.max(hi.min(i))
}

/// The face we track and identify: largest by area (the first of equals).
///
/// Panics on an empty slice, as Python's `max` raises on an empty list.
pub fn largest_rect(rects: &[Rect]) -> Rect {
    let mut best = rects[0];
    for r in &rects[1..] {
        if r.w * r.h > best.w * best.h {
            best = *r;
        }
    }
    best
}

/// Largest rect (by area) -> centre offset, width %, confidence %.
pub fn pick_face(rects: &[Rect], w: i64, h: i64) -> Option<FaceResult> {
    if rects.is_empty() || w <= 0 || h <= 0 {
        return None;
    }
    let best = largest_rect(rects);
    let (wf, hf) = (w as f64, h as f64);
    let cx = best.x + best.w / 2.0;
    let cy = best.y + best.h / 2.0;
    Some(FaceResult {
        bx: clamp((cx - wf / 2.0) / (wf / 2.0) * 100.0, -100, 100),
        by: clamp((cy - hf / 2.0) / (hf / 2.0) * 100.0, -100, 100),
        size: clamp(best.w / wf * 100.0, 0, 100),
        conf: clamp(best.conf * 100.0, 0, 100),
    })
}

/// The reply for one processed frame. `conf:0` without bx/by means "seen, no face".
pub fn build_face_cmd(
    seq: i64,
    result: Option<FaceResult>,
    yaw: Option<f64>,
    pitch: Option<f64>,
    who: &str,
) -> Value {
    let mut m = Map::new();
    m.insert("cmd".into(), "face".into());
    m.insert("seq".into(), seq.into());
    let Some(r) = result else {
        m.insert("conf".into(), 0.into());
        m.insert("who".into(), who.into());
        return Value::Object(m);
    };
    m.insert("bx".into(), r.bx.into());
    m.insert("by".into(), r.by.into());
    m.insert("size".into(), r.size.into());
    m.insert("conf".into(), r.conf.into());
    if let Some(y) = yaw {
        m.insert("yaw".into(), y.into());
    }
    if let Some(p) = pitch {
        m.insert("pitch".into(), p.into());
    }
    m.insert("who".into(), who.into());
    Value::Object(m)
}

/// Drop policy: run one detect at a time, keep only the newest waiting frame.
///
/// `offer` returns the frame to run now, or None when a detect is busy — in
/// which case the frame is parked as the single pending one, displacing (and
/// counting as dropped) whatever was parked before. `done` marks the detect
/// finished and hands back the parked frame to run next, or None to go idle.
/// Nothing is ever queued deeper than one.
#[derive(Debug, Default, Clone)]
pub struct FrameRateGovernor {
    pub busy: bool,
    pub pending: Option<Frame>,
    pub received: u64,
    pub processed: u64,
    pub dropped: u64,
}

impl FrameRateGovernor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn offer(&mut self, frame: Frame) -> Option<Frame> {
        self.received += 1;
        if self.busy {
            if self.pending.is_some() {
                self.dropped += 1;
            }
            self.pending = Some(frame);
            return None;
        }
        self.busy = true;
        Some(frame)
    }

    pub fn done(&mut self) -> Option<Frame> {
        self.processed += 1;
        let nxt = self.pending.take();
        if nxt.is_none() {
            self.busy = false;
        }
        nxt
    }
}

/// Counters at the start of the current stats window.
#[derive(Debug, Clone, Copy)]
struct Window {
    at: f64,
    received: u64,
    dropped: u64,
    processed: u64,
    faces: u64,
    detect_secs: f64,
}

impl Window {
    fn at(at: f64) -> Self {
        Window { at, received: 0, dropped: 0, processed: 0, faces: 0, detect_secs: 0.0 }
    }
}

static MONO_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `time.monotonic()`: seconds since an arbitrary fixed point.
pub fn monotonic() -> f64 {
    MONO_START.elapsed().as_secs_f64()
}

type Job = Box<dyn FnOnce() + Send>;
/// What one detect (plus identify) hands back: every face and `who`.
type WorkResult = anyhow::Result<(Vec<Rect>, String)>;

/// The single-worker executor: one named thread running jobs in order.
/// Dropping it (Python's `shutdown(wait=False, cancel_futures=True)`) lets the
/// running job finish and drops any queued ones unrun.
struct Executor {
    tx: std_mpsc::Sender<Job>,
    shut: Arc<std::sync::atomic::AtomicBool>,
}

impl Executor {
    fn spawn() -> std::io::Result<Self> {
        let (tx, rx) = std_mpsc::channel::<Job>();
        let shut = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = shut.clone();
        std::thread::Builder::new().name("vision_0".into()).spawn(move || {
            while let Ok(job) = rx.recv() {
                if flag.load(std::sync::atomic::Ordering::SeqCst) {
                    drop(job);
                    continue;
                }
                job();
            }
        })?;
        Ok(Executor { tx, shut })
    }

    fn shutdown(self) {
        self.shut.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

struct State {
    governor: FrameRateGovernor,
    /// processed frames that had a face
    faces: u64,
    /// summed wall time inside the executor
    detect_secs: f64,
    bad_frames: u64,
    first_logged: bool,
    last_saved: f64,
    saved: u64,
    window: Window,
    save_dir: Option<PathBuf>,
}

struct Inner {
    detect: RwLock<Option<Detector>>,
    send: Sender,
    on_faces: RwLock<Option<OnFaces>>,
    identity: RwLock<Option<Arc<dyn Identity>>>,
    clock: Clock,
    state: Mutex<State>,
    executor: Mutex<Option<Executor>>,
}

/// Drives frames through the governor, the detector and back to the board.
///
/// `detect` is None when no detector is available (not macOS, Vision
/// missing): `enabled()` is false, the daemon never sends `cam on` and any
/// frame that still arrives is ignored. `send` is the transport's send
/// coroutine. `save_dir` writes every received frame, rate-limited to one per
/// second, for bench debugging. `identity` (identity's FaceIdentity, or
/// anything with `process(frame, rects) -> who`) runs on the same worker
/// thread straight after the detect and fills the face cmd's `who`; None
/// leaves every face "unknown".
///
/// Cheap to clone: clones share one tracker.
#[derive(Clone)]
pub struct FaceTracker {
    inner: Arc<Inner>,
}

impl FaceTracker {
    pub fn new(detect: Option<Detector>, send: Sender) -> Self {
        Self::with_options(detect, send, None, Arc::new(monotonic), None, None)
    }

    /// The full constructor: Python's keyword arguments `save_dir`, `clock`,
    /// `identity` and `on_faces`.
    pub fn with_options(
        detect: Option<Detector>,
        send: Sender,
        save_dir: Option<PathBuf>,
        clock: Clock,
        identity: Option<Arc<dyn Identity>>,
        on_faces: Option<OnFaces>,
    ) -> Self {
        let now = clock();
        FaceTracker {
            inner: Arc::new(Inner {
                detect: RwLock::new(detect),
                send,
                // Every face of every processed frame, with the pose the frame was taken at — for the
                // conversation's follower (follow). The board still gets only the largest, as the wire says.
                on_faces: RwLock::new(on_faces),
                identity: RwLock::new(identity),
                clock,
                state: Mutex::new(State {
                    governor: FrameRateGovernor::new(),
                    faces: 0,
                    detect_secs: 0.0,
                    bad_frames: 0,
                    first_logged: false,
                    last_saved: f64::NEG_INFINITY,
                    saved: 0,
                    window: Window::at(now),
                    save_dir,
                }),
                executor: Mutex::new(None),
            }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.detect.read().is_some()
    }

    /// Python's `tracker.detect = ...`.
    pub fn set_detect(&self, detect: Option<Detector>) {
        *self.inner.detect.write() = detect;
    }

    /// Python's `tracker.identity = ...`.
    pub fn set_identity(&self, identity: Option<Arc<dyn Identity>>) {
        *self.inner.identity.write() = identity;
    }

    /// Python's `tracker.on_faces = ...`.
    pub fn set_on_faces(&self, on_faces: Option<OnFaces>) {
        *self.inner.on_faces.write() = on_faces;
    }

    pub fn save_dir(&self) -> Option<PathBuf> {
        self.inner.state.lock().save_dir.clone()
    }

    pub fn set_save_dir(&self, dir: Option<PathBuf>) {
        self.inner.state.lock().save_dir = dir;
    }

    /// Processed frames that had a face.
    pub fn faces(&self) -> u64 {
        self.inner.state.lock().faces
    }

    /// Summed wall time inside the executor, seconds.
    pub fn detect_secs(&self) -> f64 {
        self.inner.state.lock().detect_secs
    }

    pub fn bad_frames(&self) -> u64 {
        self.inner.state.lock().bad_frames
    }

    /// A copy of the governor's state (busy, pending, counters).
    pub fn governor(&self) -> FrameRateGovernor {
        self.inner.state.lock().governor.clone()
    }

    pub fn stop(&self) {
        if let Some(ex) = self.inner.executor.lock().take() {
            ex.shutdown();
        }
    }

    /// Decode, maybe save, then run or park the frame.
    pub async fn on_frame(&self, obj: &Value) {
        if !self.enabled() {
            return;
        }
        let frame = match decode_frame(obj) {
            Ok(f) => f,
            Err(e) => {
                let mut st = self.inner.state.lock();
                st.bad_frames += 1;
                if st.bad_frames == 1 {
                    tracing::warn!("vision: bad frame from board — {} (further ones counted, not logged)", e);
                }
                return;
            }
        };
        let run_now = {
            let mut st = self.inner.state.lock();
            if !st.first_logged {
                st.first_logged = true;
                tracing::info!(
                    "vision: first frame {}x{} {} {:.1} KB",
                    frame.w,
                    frame.h,
                    frame.fmt,
                    frame.data.len() as f64 / 1024.0
                );
            }
            self.maybe_save(&mut st, &frame);
            st.governor.offer(frame)
        };
        if let Some(f) = run_now {
            let me = self.clone();
            tokio::spawn(async move { me.run(f).await });
        }
    }

    /// Queue one detect on the tracker's worker, starting the worker if there
    /// is none. A worker per tracker, made lazily; after stop() a drain that
    /// is still going gets a fresh one, as Python's `run_in_executor(None)`
    /// falls back to the loop's default executor and keeps detecting. The
    /// check and the send happen under one lock, so a stop() in between
    /// cannot leave the drain without a worker.
    fn submit(&self, frame: Arc<Frame>) -> std::io::Result<Option<tokio::sync::oneshot::Receiver<WorkResult>>> {
        let detect = self.inner.detect.read().clone();
        let identity = self.inner.identity.read().clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let job: Job = Box::new(move || {
            let out = catch_unwind(AssertUnwindSafe(|| work(detect.as_ref(), identity.as_deref(), &frame)))
                .unwrap_or_else(|p| Err(anyhow::anyhow!("detector panicked: {}", panic_message(&p))));
            let _ = tx.send(out);
        });
        let mut ex = self.inner.executor.lock();
        if ex.is_none() {
            *ex = Some(Executor::spawn()?);
        }
        let sent = ex.as_ref().is_some_and(|e| e.tx.send(job).is_ok());
        Ok(sent.then_some(rx))
    }

    /// Detect on the worker, reply, then drain the governor's parked frame.
    async fn run(&self, frame: Frame) {
        let mut nxt = Some(frame);
        while let Some(cur) = nxt {
            let cur = Arc::new(cur);
            let t0 = (self.inner.clock)();
            let rx = match self.submit(cur.clone()) {
                Ok(Some(rx)) => rx,
                // The worker thread ended (it never does while the tracker is alive).
                Ok(None) => return,
                Err(e) => {
                    tracing::error!("vision: could not start the detector thread: {}", e);
                    return;
                }
            };
            // Executor shut down under us (daemon stopping): a queued detect
            // that stop() cancelled ends the drain, as Python's CancelledError does.
            let (rects, who) = match rx.await {
                Err(_) => return,
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    tracing::error!("vision: detector failed on frame {}: {:#}", cur.seq, e);
                    (Vec::new(), WHO_UNKNOWN.to_string())
                }
            };
            let elapsed = (self.inner.clock)() - t0;
            let result = pick_face(&rects, cur.w, cur.h);
            {
                let mut st = self.inner.state.lock();
                st.detect_secs += elapsed;
                if result.is_some() {
                    st.faces += 1;
                }
            }
            (self.inner.send)(build_face_cmd(cur.seq, result, cur.yaw, cur.pitch, &who)).await;
            let on_faces = self.inner.on_faces.read().clone();
            if let Some(cb) = on_faces {
                let each: Vec<FaceResult> =
                    rects.iter().filter_map(|r| pick_face(std::slice::from_ref(r), cur.w, cur.h)).collect();
                // A follower that fails must never stop the tracker.
                let fut = std::panic::AssertUnwindSafe(cb(each, cur.yaw, cur.pitch));
                match futures::FutureExt::catch_unwind(fut).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::error!("vision: on_faces failed on frame {}: {:#}", cur.seq, e),
                    Err(p) => {
                        tracing::error!("vision: on_faces failed on frame {}: {}", cur.seq, panic_message(&p))
                    }
                }
            }
            nxt = self.inner.state.lock().governor.done();
        }
    }

    // ---- stats ----

    /// The once-per-window summary, or None when no frame arrived. Resets the window.
    pub fn stats_line(&self) -> Option<String> {
        let now = (self.inner.clock)();
        let mut st = self.inner.state.lock();
        let w = st.window;
        let g = &st.governor;
        let received = g.received - w.received;
        let new_window = Window {
            at: now,
            received: g.received,
            dropped: g.dropped,
            processed: g.processed,
            faces: st.faces,
            detect_secs: st.detect_secs,
        };
        let dropped = g.dropped - w.dropped;
        let processed = g.processed - w.processed;
        let faces = st.faces - w.faces;
        let detect_secs = st.detect_secs;
        st.window = new_window;
        if received == 0 {
            return None;
        }
        let secs = (now - w.at).max(1e-6);
        let ms = if processed > 0 { (detect_secs - w.detect_secs) / processed as f64 * 1000.0 } else { 0.0 };
        let pct = if processed > 0 { 100.0 * faces as f64 / processed as f64 } else { 0.0 };
        Some(format!(
            "vision: {received} frames, {dropped} dropped, {:.1} fps, {ms:.0} ms/detect, faces {pct:.0}%",
            received as f64 / secs
        ))
    }

    // ---- bench: frame dump ----

    fn maybe_save(&self, st: &mut State, frame: &Frame) {
        let Some(dir) = st.save_dir.clone() else { return };
        let now = (self.inner.clock)();
        if now - st.last_saved < SAVE_FRAMES_MIN_INTERVAL {
            return;
        }
        st.last_saved = now;
        let path = match save_frame(frame, &dir) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "vision: could not save frame to {}: {} — disabling frame dump",
                    dir.display(),
                    e
                );
                st.save_dir = None;
                return;
            }
        };
        st.saved += 1;
        if st.saved == 1 {
            tracing::info!(
                "vision: saving frames to {} (first: {})",
                dir.display(),
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            );
        }
    }
}

impl fmt::Debug for FaceTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let st = self.inner.state.lock();
        f.debug_struct("FaceTracker")
            .field("enabled", &self.enabled())
            .field("governor", &st.governor)
            .field("faces", &st.faces)
            .field("bad_frames", &st.bad_frames)
            .field("save_dir", &st.save_dir)
            .finish()
    }
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".into()
    }
}

/// Worker thread: detect, then identify the largest face on the same thread
/// so two Vision requests never run at once.
fn work(detect: Option<&Detector>, identity: Option<&dyn Identity>, frame: &Frame) -> WorkResult {
    let detect = detect.ok_or_else(|| anyhow::anyhow!("AssertionError: no detector"))?;
    let rects = detect(frame)?;
    let mut who = WHO_UNKNOWN.to_string();
    if !rects.is_empty() {
        if let Some(id) = identity {
            who = id.process(frame, &rects)?;
        }
    }
    Ok((rects, who))
}

/// Write the frame as it arrived: JPEG bytes verbatim, gray as an 8-bit PNG.
pub fn save_frame(frame: &Frame, directory: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(directory)?;
    let path;
    if frame.fmt == "jpeg" {
        path = directory.join(format!("frame_{:06}.jpg", frame.seq));
        std::fs::write(&path, &frame.data)?;
    } else {
        path = directory.join(format!("frame_{:06}.png", frame.seq));
        let png = encode_gray_png(frame.w, frame.h, &frame.data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, png)?;
    }
    Ok(path)
}

/// Minimal PNG writer for 8-bit grayscale.
pub fn encode_gray_png(w: i64, h: i64, luma: &[u8]) -> Result<Vec<u8>, ValueError> {
    if w as i128 * h as i128 != luma.len() as i128 {
        return Err(ValueError(format!("luma is {} bytes, expected {}", luma.len(), w as i128 * h as i128)));
    }
    // Python's struct.error from packing IHDR (only reachable with negative sizes).
    if !(0..=u32::MAX as i64).contains(&w) || !(0..=u32::MAX as i64).contains(&h) {
        return Err(ValueError("'I' format requires 0 <= number <= 4294967295".into()));
    }

    fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(body);
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(tag);
        hasher.update(body);
        out.extend_from_slice(&hasher.finalize().to_be_bytes());
    }

    let (wu, hu) = (w as usize, h as usize);
    let mut raw = Vec::with_capacity((wu + 1) * hu);
    for y in 0..hu {
        raw.push(0u8); // filter 0 per row
        raw.extend_from_slice(&luma[y * wu..(y + 1) * wu]);
    }
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    // Writing into a Vec cannot fail.
    let _ = enc.write_all(&raw);
    let idat = enc.finish().unwrap_or_default();

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", b"");
    Ok(out)
}

/// `--save-frames DIR` wins, else `EVE_SAVE_FRAMES`; None when unset.
pub fn configured_save_dir(cli_value: Option<&str>) -> Option<PathBuf> {
    let env = std::env::var("EVE_SAVE_FRAMES").ok();
    configured_save_dir_from(cli_value, env.as_deref())
}

/// [`configured_save_dir`] with the environment value passed in.
pub fn configured_save_dir_from(cli_value: Option<&str>, env_value: Option<&str>) -> Option<PathBuf> {
    let raw = [cli_value, env_value].into_iter().flatten().find(|s| !s.is_empty()).unwrap_or("");
    let raw = raw.trim();
    if raw.is_empty() {
        None
    } else {
        Some(expanduser(raw))
    }
}

/// `PurePosixPath(p)` split the way pathlib parses it: the root ("/", or "//"
/// for exactly two leading slashes, as POSIX allows) and the parts, with
/// empty and "." parts dropped.
fn pathlib_parse(p: &str) -> (&'static str, Vec<&str>) {
    let root = if p.starts_with("//") && !p.starts_with("///") {
        "//"
    } else if p.starts_with('/') {
        "/"
    } else {
        ""
    };
    (root, p.split('/').filter(|s| !s.is_empty() && *s != ".").collect())
}

/// The string `str(PurePosixPath(...))` gives for a root and parts.
fn pathlib_join(root: &str, parts: &[&str]) -> PathBuf {
    let s = format!("{root}{}", parts.join("/"));
    PathBuf::from(if s.is_empty() { ".".to_string() } else { s })
}

/// Python's `Path(p).expanduser()`: a leading `~` from HOME (else the
/// password database), `~user` from the password database; the result is
/// spelled as pathlib spells it (`//` runs collapsed, `.` parts and a
/// trailing `/` dropped).
pub fn expanduser(p: &str) -> PathBuf {
    let (root, parts) = pathlib_parse(p);
    let first = parts.first().copied().unwrap_or("");
    if !root.is_empty() || !first.starts_with('~') {
        return pathlib_join(root, &parts);
    }
    let user = &first[1..];
    let home = if user.is_empty() {
        std::env::var_os("HOME")
            .map(|h| h.to_string_lossy().into_owned())
            .or_else(|| passwd_home(None).map(|h| h.to_string_lossy().into_owned()))
    } else {
        passwd_home(Some(user)).map(|h| h.to_string_lossy().into_owned())
    };
    let Some(home) = home else {
        // Python's Path.expanduser raises RuntimeError here; keep the path as given.
        return pathlib_join(root, &parts);
    };
    // posixpath.expanduser: `userhome.rstrip("/")`, and an empty result is "/".
    let home = home.trim_end_matches('/');
    let home = if home.is_empty() { "/" } else { home };
    let (hroot, mut hparts) = pathlib_parse(home);
    hparts.extend_from_slice(&parts[1..]);
    pathlib_join(hroot, &hparts)
}

fn passwd_home(user: Option<&str>) -> Option<PathBuf> {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: getpwnam/getpwuid return a pointer into static storage or NULL;
    // pw_dir is copied out before any other passwd call can overwrite it.
    unsafe {
        let pw = match user {
            Some(u) => {
                let c = CString::new(u).ok()?;
                libc::getpwnam(c.as_ptr())
            }
            None => libc::getpwuid(libc::getuid()),
        };
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let dir = CStr::from_ptr((*pw).pw_dir);
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
    }
}

// ---- platform adapter: macOS Vision ------------------------------------------

/// The Vision-framework detector, or None (with one warning) when unavailable.
pub fn make_detector() -> Option<Detector> {
    #[cfg(target_os = "macos")]
    {
        mac::make_detector()
    }
    #[cfg(not(target_os = "macos"))]
    {
        tracing::warn!("vision: host face tracking needs macOS Vision — board keeps on-board tracking");
        None
    }
}

/// VNDetectFaceRectanglesRequest over a CGImage of the frame (worker thread).
///
/// Vision reports each face as a normalized boundingBox with its origin at
/// the BOTTOM-LEFT of the image (Core Graphics convention). The board and
/// pick_face use top-left pixel coordinates, so y is flipped here:
///     px = bb.x * W,  py = (1 - bb.y - bb.h) * H,  pw = bb.w * W,  ph = bb.h * H
/// W/H are taken from the decoded CGImage, not the wire header, so a JPEG
/// whose real size disagrees with w/h still maps correctly.
pub fn vision_detect(frame: &Frame) -> anyhow::Result<Vec<Rect>> {
    #[cfg(target_os = "macos")]
    {
        mac::vision_detect(frame)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = frame;
        anyhow::bail!("No module named 'Quartz'")
    }
}

/// (w, h) of an image file's first frame via ImageIO, or None if unreadable.
pub fn probe_image_size(data: &[u8]) -> Option<(i64, i64)> {
    #[cfg(target_os = "macos")]
    {
        mac::probe_image_size(data)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = data;
        None
    }
}

// ---- bench CLI --------------------------------------------------------------

/// `eve-bridge vision-test <image>`: detect on a file, print what we'd send.
pub fn run_vision_test(path: &str) -> i32 {
    let Some(detect) = make_detector() else {
        eprintln!("vision-test: no detector available on this host");
        return 2;
    };
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("vision-test: cannot read {path}: {}", py_os_error(&e, path));
            return 2;
        }
    };
    let Some((w, h)) = probe_image_size(&data) else {
        eprintln!("vision-test: {path} is not an image ImageIO can decode");
        return 2;
    };
    // Any ImageIO-decodable file (JPEG, PNG, HEIC...) goes down the "jpeg"
    // branch: that branch is "decode a file with CGImageSource".
    let frame = Frame::new(0, w, h, "jpeg", data);
    // Warm up: the first Vision call pays model load. Python lets an
    // exception here escape as a traceback; report it and fail instead.
    if let Err(e) = detect(&frame) {
        eprintln!("vision-test: {e:#}");
        return 1;
    }
    let t0 = Instant::now();
    let rects = match detect(&frame) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("vision-test: {e:#}");
            return 1;
        }
    };
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!("{path}: {w}x{h}, {} face(s), detect {ms:.1} ms", rects.len());
    for r in &rects {
        println!(
            "  rect x={:.0} y={:.0} w={:.0} h={:.0} conf={:.2}  (top-left px)",
            r.x, r.y, r.w, r.h, r.conf
        );
    }
    let cmd = build_face_cmd(0, pick_face(&rects, w, h), None, None, WHO_UNKNOWN);
    println!("  send: {}", serde_json::to_string(&cmd).unwrap_or_default());
    0
}

/// `str(OSError)` as Python prints it: `[Errno 2] No such file or directory: 'path'`.
fn py_os_error(e: &std::io::Error, path: &str) -> String {
    match e.raw_os_error() {
        Some(code) => {
            // SAFETY: strerror returns a valid NUL-terminated string.
            let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }.to_string_lossy().into_owned();
            format!("[Errno {code}] {msg}: {}", py_repr_str(path))
        }
        None => e.to_string(),
    }
}
