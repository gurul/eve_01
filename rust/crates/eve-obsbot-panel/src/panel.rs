//! `ObsbotPanel`: the Apps tab's panel (`panel(body)`), the models' tools (`handle`, `run`), hold-to-turn with a
//! heartbeat, and the picture (`stream(writer)`; WebRTC in rtc.rs).

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::task::JoinHandle;
use tracing::info;

use eve_obsbot::py::{py_str, truthy};
use eve_obsbot::{Direction, Obsbot, Res, UsbCamera, UsbControl};
use eve_pika::pika::{video_part, write_chunk, write_json, VIDEO_HEAD};

use crate::capture::{ffmpeg_frames, Capture, FrameStream, FramesFn};
use crate::rtc::Peer;
use crate::{
    num, r#match, where_value, Command, PyNum, ENDED_KEEP, HEARTBEAT_SECS, HOLD_SPEED, INSTRUCTIONS, LINGER_SECS,
    TOOLS, TOOL_NAMES, TURN_DEGREES,
};

/// The camera calls the panel makes: `eve_obsbot::Obsbot`'s surface (the tests' fakes implement it). Every call is
/// quick-ish and blocking; the panel runs them on a blocking thread, one at a time.
pub trait Camera: Send {
    /// None when the camera answers over USB; else the owner's line.
    fn ensure(&mut self) -> Option<String>;
    /// Python's `_ready`: the camera answered (the panel clears it after a failed read).
    fn ready(&self) -> bool;
    fn set_ready(&mut self, ready: bool);
    fn status(&mut self) -> Res;
    fn position_reply(&mut self) -> Res;
    fn zoom_reply(&mut self) -> Res;
    /// Python's `move(pan, pitch)`.
    fn move_to(&mut self, pan: Option<f64>, pitch: Option<f64>) -> Res;
    fn reset(&mut self) -> Res;
    fn zoom(&mut self, level: i64) -> Res;
    fn wake(&mut self, awake: bool) -> Res;
    fn track(&mut self, on: bool) -> Res;
    fn mode(&mut self, name: &str) -> Res;
    fn preset(&mut self, n: i64) -> Res;
    fn close(&mut self);
    /// The USB turn and wake for hold-to-turn (Python's `cam.usb`), used outside the read lock so a stop never
    /// waits behind a move's read-back.
    fn turner(&mut self) -> Arc<dyn Turner>;
}

/// The hold-to-turn half of the USB camera: wake, the continuous turn, and why the last wake failed.
pub trait Turner: Send + Sync {
    fn wake(&self) -> bool;
    fn turn(&self, direction: Direction, speed: i64) -> bool;
    fn why(&self) -> String;
}

/// [`Turner`] over the real USB camera: one `UsbCamera` for waking and one for turns, so a stop goes out while a
/// wake (up to ~7 s) is still waiting for the gimbal.
pub struct UsbTurner {
    waker: Mutex<Box<dyn UsbControl>>,
    turns: Mutex<Box<dyn UsbControl>>,
}

impl UsbTurner {
    pub fn new() -> UsbTurner {
        UsbTurner::with(Box::new(UsbCamera::new()), Box::new(UsbCamera::new()))
    }

    pub fn with(waker: Box<dyn UsbControl>, turns: Box<dyn UsbControl>) -> UsbTurner {
        UsbTurner { waker: Mutex::new(waker), turns: Mutex::new(turns) }
    }
}

impl Default for UsbTurner {
    fn default() -> Self {
        UsbTurner::new()
    }
}

impl Turner for UsbTurner {
    fn wake(&self) -> bool {
        self.waker.lock().wake()
    }

    fn turn(&self, direction: Direction, speed: i64) -> bool {
        self.turns.lock().turn(direction, speed)
    }

    fn why(&self) -> String {
        self.waker.lock().why().to_string()
    }
}

impl Camera for Obsbot {
    fn ensure(&mut self) -> Option<String> {
        Obsbot::ensure(self)
    }
    fn ready(&self) -> bool {
        self.ready
    }
    fn set_ready(&mut self, ready: bool) {
        self.ready = ready;
    }
    fn status(&mut self) -> Res {
        Obsbot::status(self)
    }
    fn position_reply(&mut self) -> Res {
        Obsbot::position_reply(self)
    }
    fn zoom_reply(&mut self) -> Res {
        Obsbot::zoom_reply(self)
    }
    fn move_to(&mut self, pan: Option<f64>, pitch: Option<f64>) -> Res {
        Obsbot::move_to(self, pan, pitch)
    }
    fn reset(&mut self) -> Res {
        Obsbot::reset(self)
    }
    fn zoom(&mut self, level: i64) -> Res {
        Obsbot::zoom(self, level)
    }
    fn wake(&mut self, awake: bool) -> Res {
        Obsbot::wake(self, awake)
    }
    fn track(&mut self, on: bool) -> Res {
        Obsbot::track(self, on)
    }
    fn mode(&mut self, name: &str) -> Res {
        Obsbot::mode(self, name)
    }
    fn preset(&mut self, n: i64) -> Res {
        Obsbot::preset(self, n)
    }
    fn close(&mut self) {
        Obsbot::close(self)
    }
    fn turner(&mut self) -> Arc<dyn Turner> {
        Arc::new(UsbTurner::new())
    }
}

/// Makes the camera (Python's `make=obsbot.Obsbot`).
pub type MakeCamera = Arc<dyn Fn() -> io::Result<Box<dyn Camera>> + Send + Sync>;

/// `ObsbotPanel(make=, ffmpeg=, environ=, clock=)`, plus the seams the tests patch in Python (`LINGER_SECS`) and
/// where the WebRTC peers bind.
pub struct PanelOptions {
    pub make: MakeCamera,
    pub ffmpeg: Option<String>,
    /// None reads the process environment.
    pub environ: Option<HashMap<String, String>>,
    pub clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub linger_secs: f64,
    pub stun: String,
    pub udp_addrs: Vec<String>,
}

impl Default for PanelOptions {
    fn default() -> Self {
        PanelOptions {
            make: Arc::new(|| Obsbot::new().map(|o| Box::new(o) as Box<dyn Camera>)),
            ffmpeg: None,
            environ: None,
            clock: Arc::new(eve_pika::pika::monotonic),
            linger_secs: LINGER_SECS,
            stun: eve_pika::pika::DEFAULT_STUN.to_string(),
            udp_addrs: eve_pika::pika_rtc::DEFAULT_UDP_ADDRS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Default)]
struct Hold {
    /// (press id, direction) while the gimbal turns
    hold: Option<(String, String)>,
    beat: f64,
    ended: VecDeque<String>,
    watch: Option<JoinHandle<()>>,
}

impl Hold {
    fn end(&mut self, hold: &str) {
        if !hold.is_empty() && !self.ended.iter().any(|h| h == hold) {
            self.ended.push_back(hold.to_string());
            while self.ended.len() > ENDED_KEEP {
                self.ended.pop_front();
            }
        }
    }
}

type CamSlot = Arc<tokio::sync::Mutex<Option<Box<dyn Camera>>>>;

pub(crate) struct Inner {
    make: MakeCamera,
    environ: Option<HashMap<String, String>>,
    ffmpeg: Option<String>,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    /// reads, one at a time (its socket's replies are shared)
    cam: CamSlot,
    ready: AtomicBool,
    turner: Mutex<Option<Arc<dyn Turner>>>,
    hold: Mutex<Hold>,
    pub(crate) capture: Capture,
    /// WebRTC peers -> their frame senders, oldest first
    pub(crate) peers: Mutex<IndexMap<u64, Peer>>,
    pub(crate) next_peer: AtomicU64,
    pub(crate) stun: String,
    pub(crate) udp_addrs: Vec<String>,
}

/// `panel(body)` for the Apps tab, and `stream(writer)` for the picture. Clones share one panel.
#[derive(Clone)]
pub struct ObsbotPanel {
    pub(crate) inner: Arc<Inner>,
}

fn obj(v: Value) -> Res {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// `str(x or "")`.
fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(Some(v)) => py_str(v),
        _ => String::new(),
    }
}

fn get_or_null(m: &Res, key: &str) -> Value {
    m.get(key).cloned().unwrap_or(Value::Null)
}

/// `int(n)` of a float that Python can convert; None for inf and nan (Python raises).
fn int_of(f: f64) -> Option<i64> {
    f.is_finite().then_some(f as i64)
}

/// Python's `min(hi, v)` then `max(lo, ...)`, which keeps NaN's side the way Python's comparisons do.
fn py_clamp(lo: f64, hi: f64, v: f64) -> f64 {
    let v = if v < hi { v } else { hi };
    if v > lo {
        v
    } else {
        lo
    }
}

impl ObsbotPanel {
    pub fn new(o: PanelOptions) -> ObsbotPanel {
        let ffmpeg = o.ffmpeg.clone();
        let frames: FramesFn = Arc::new(move || ffmpeg_frames(ffmpeg.clone()));
        ObsbotPanel {
            inner: Arc::new(Inner {
                make: o.make,
                environ: o.environ,
                ffmpeg: o.ffmpeg,
                clock: o.clock,
                cam: Arc::new(tokio::sync::Mutex::new(None)),
                ready: AtomicBool::new(false),
                turner: Mutex::new(None),
                hold: Mutex::new(Hold::default()),
                capture: Capture::with_linger(frames, o.linger_secs),
                peers: Mutex::new(IndexMap::new()),
                next_peer: AtomicU64::new(0),
                stun: o.stun,
                udp_addrs: o.udp_addrs,
            }),
        }
    }

    /// The panel with a camera maker and Python's other defaults.
    pub fn with_make(make: MakeCamera) -> ObsbotPanel {
        ObsbotPanel::new(PanelOptions { make, ..Default::default() })
    }

    pub fn available(&self) -> bool {
        crate::enabled(self.inner.environ.as_ref())
    }

    /// The shared capture (Python's `_capture`).
    pub fn capture(&self) -> &Capture {
        &self.inner.capture
    }

    /// The press being held, if any (Python's `_hold`).
    pub fn holding(&self) -> Option<(String, String)> {
        self.inner.hold.lock().hold.clone()
    }

    // -- the camera --

    /// None when the camera answers over USB; else the owner's line (no camera, no clang). Python's `_ready`.
    pub async fn ready(&self) -> Option<String> {
        if self.inner.ready.load(Ordering::SeqCst) {
            return None; // no lock: a beat must not wait behind a move's read-back
        }
        let mut slot = self.inner.cam.clone().lock_owned().await;
        if slot.is_none() {
            match (self.inner.make)() {
                Ok(mut cam) => {
                    *self.inner.turner.lock() = Some(cam.turner());
                    *slot = Some(cam);
                }
                Err(e) => return Some(e.to_string()),
            }
        }
        let (why, ready) = blocking(move || {
            let cam = slot.as_mut().expect("made above");
            let why = cam.ensure();
            (why, cam.ready())
        })
        .await;
        self.inner.ready.store(ready, Ordering::SeqCst);
        why
    }

    /// One camera call under the lock, on a blocking thread. A failed one makes the next call ask the camera again
    /// (it may have been unplugged).
    async fn read(&self, f: impl FnOnce(&mut dyn Camera) -> Res + Send + 'static) -> Res {
        let mut slot = self.inner.cam.clone().lock_owned().await;
        let res = blocking(move || {
            let Some(cam) = slot.as_mut() else {
                // only reached through `control` before the camera was made, which asks for it first
                return obj(json!({"ok": false, "line": crate::NOT_SET_UP_LINE}));
            };
            let res = f(cam.as_mut());
            if !truthy(res.get("ok")) {
                cam.set_ready(false);
            }
            res
        })
        .await;
        if !truthy(res.get("ok")) {
            self.inner.ready.store(false, Ordering::SeqCst);
        }
        res
    }

    fn made(&self) -> bool {
        self.inner.turner.lock().is_some()
    }

    pub async fn panel(&self, body: &Value) -> Value {
        let action = match body.get("action") {
            Some(v) if truthy(Some(v)) => py_str(v),
            _ => "status".to_string(),
        };
        if action == "close" {
            // the picture ends with each viewer's own connection
            self.stop_turning("").await;
            return json!({"ok": true, "line": ""});
        }
        if action == "rtc" {
            return self.rtc(body.get("sdp")).await;
        }
        if action == "stop" {
            self.stop_turning(&str_or_empty(body.get("hold"))).await;
            return json!({"ok": true, "line": "", "stopped": true});
        }
        if let Some(why) = self.ready().await {
            return json!({"ok": false, "line": why, "stopped": action == "hold"});
        }
        let res = match action.as_str() {
            "hold" => return self.turn(&str_or_empty(body.get("hold")), &str_or_empty(body.get("dir"))).await,
            "status" => {
                let mut res = self.read(|c| c.status()).await;
                let modes: Vec<Value> =
                    eve_obsbot::keys(&eve_obsbot::AI_MODES).into_iter().map(Value::from).collect();
                res.insert("modes".into(), Value::Array(modes));
                res
            }
            "move" => {
                let (pan, pitch) = (num(body.get("pan")), num(body.get("pitch")));
                self.read(move |c| c.move_to(pan, pitch)).await
            }
            "center" => self.read(|c| c.reset()).await,
            "zoom" => match num(body.get("level")).and_then(int_of) {
                None => return json!({"ok": false, "line": "Zoom is 0 to 100."}),
                Some(level) => self.read(move |c| c.zoom(level)).await,
            },
            "wake" | "sleep" => {
                let awake = action == "wake";
                self.read(move |c| c.wake(awake)).await
            }
            "track" => {
                let on = truthy(body.get("on"));
                self.read(move |c| c.track(on)).await
            }
            "mode" => {
                let name = str_or_empty(body.get("name"));
                self.read(move |c| c.mode(&name)).await
            }
            "preset" => {
                let n = num(body.get("n")).map_or(Some(0), int_of).unwrap_or(0);
                self.read(move |c| c.preset(n)).await
            }
            _ => return json!({"ok": false, "line": "That isn't something the camera does."}),
        };
        Value::Object(res)
    }

    // -- the models' tools (chat, app chat, voice) and the code path --

    pub fn tools(&self) -> Vec<Value> {
        TOOLS.clone()
    }

    pub fn instructions(&self) -> String {
        INSTRUCTIONS.clone()
    }

    pub fn r#match(&self, text: &str) -> Option<Command> {
        r#match(text)
    }

    /// A model's call. Every answer has `ok` and a plain `line`; a move adds where the camera ended up.
    pub async fn handle(&self, name: &str, args: &Value) -> Value {
        if !TOOL_NAMES.contains(&name) {
            return json!({"ok": false, "reason": format!("unknown tool {name}")});
        }
        if let Some(why) = self.ready().await {
            return json!({"ok": false, "reason": why});
        }
        if name == "camera_status" {
            let st = self.read(|c| c.status()).await;
            if !truthy(st.get("ok")) {
                return json!({"ok": false, "reason": st.get("line").cloned().unwrap_or_else(|| json!(""))});
            }
            let (pan, pitch) = (get_or_null(&st, "pan"), get_or_null(&st, "pitch"));
            let saved: Vec<Value> = match st.get("presets") {
                Some(Value::Array(presets)) => presets
                    .iter()
                    .filter(|p| truthy(p.get("set")))
                    .map(|p| match p.get("name") {
                        Some(n) if truthy(Some(n)) => n.clone(),
                        _ => Value::String(format!("preset {}", p.get("n").map(py_str).unwrap_or_default())),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let tilt = PyNum::of(&pitch).map_or(Value::Null, |p| p.neg().to_value());
            let camera = st.get("camera").map_or_else(|| "The camera".to_string(), py_str);
            let asleep = st.get("awake") == Some(&Value::Bool(false));
            let zoom = st.get("zoom").map_or_else(|| "0".to_string(), py_str);
            let line = format!(
                "{camera} is {}, pointing {}, zoom {zoom}%.",
                if asleep { "asleep" } else { "awake" },
                where_value(&pan, &pitch)
            );
            return json!({"ok": true, "awake": get_or_null(&st, "awake"), "pan": pan, "tilt": tilt,
                          "zoom": get_or_null(&st, "zoom"), "presets": saved, "line": line});
        }
        let mut call = Map::new();
        for k in ["action", "direction", "degrees", "pan", "tilt", "zoom", "zoom_change", "mode", "preset"] {
            call.insert(k.to_string(), args.get(k).cloned().unwrap_or(Value::Null));
        }
        self.control(&call).await
    }

    /// `control(action, direction=, degrees=, pan=, tilt=, zoom=, zoom_change=, mode=, preset=)`: the arguments
    /// by name, a missing one is None.
    pub async fn control(&self, args: &Map<String, Value>) -> Value {
        if !self.made() {
            // Python asserts the camera was made (handle and run ask first); here it is asked for
            if let Some(why) = self.ready().await {
                return json!({"ok": false, "reason": why});
            }
        }
        let arg = |k: &str| args.get(k).filter(|v| !v.is_null());
        let action = args.get("action").cloned().unwrap_or(Value::Null);
        match action.as_str().unwrap_or("") {
            "turn" => {
                let direction = match arg("direction").and_then(Value::as_str) {
                    Some(d) if eve_obsbot::DIRECTIONS.iter().any(|(k, _)| *k == d) => d.to_string(),
                    _ => return json!({"ok": false, "reason": "turn needs a direction: left, right, up or down"}),
                };
                let deg = match num(arg("degrees")) {
                    None => TURN_DEGREES as f64,
                    Some(d) => py_clamp(1.0, 120.0, d.abs()),
                };
                let now = self.read(|c| c.position_reply()).await;
                if !truthy(now.get("ok")) {
                    return json!({"ok": false, "reason": get_or_null(&now, "line")});
                }
                let p = now.get("pan").and_then(PyNum::of).map_or(f64::NAN, PyNum::f);
                let q = now.get("pitch").and_then(PyNum::of).map_or(f64::NAN, PyNum::f);
                let (p, q) = match direction.as_str() {
                    "left" => (p + deg, q),
                    "right" => (p - deg, q),
                    "up" => (p, q - deg),
                    _ => (p, q + deg),
                };
                let res = self.read(move |c| c.move_to(Some(p), Some(q))).await;
                moved(&res, &format!("turn {direction}"))
            }
            "point" => {
                let (pt, tl) = (num(arg("pan")), num(arg("tilt")));
                if pt.is_none() && tl.is_none() {
                    return json!({"ok": false, "reason": "point needs pan, tilt or both"});
                }
                let res = self.read(move |c| c.move_to(pt, tl.map(|t| -t))).await;
                moved(&res, "point")
            }
            "center" => moved(&self.read(|c| c.reset()).await, "center"),
            "zoom" => {
                let (z, dz) = (num(arg("zoom")), num(arg("zoom_change")));
                let z = match (z, dz) {
                    (None, None) => return json!({"ok": false, "reason": "zoom needs zoom or zoom_change"}),
                    (Some(z), _) => z,
                    (None, Some(dz)) => {
                        let now = self.read(|c| c.zoom_reply()).await;
                        if !truthy(now.get("ok")) {
                            return json!({"ok": false, "reason": get_or_null(&now, "line")});
                        }
                        now.get("zoom").and_then(PyNum::of).map_or(f64::NAN, PyNum::f) + dz
                    }
                };
                let level = py_clamp(0.0, 100.0, z) as i64;
                let res = self.read(move |c| c.zoom(level)).await;
                json!({"ok": truthy(res.get("ok")), "zoom": get_or_null(&res, "zoom"),
                       "line": res.get("line").cloned().unwrap_or_else(|| json!(""))})
            }
            a @ ("wake" | "sleep") => {
                let awake = a == "wake";
                let res = self.read(move |c| c.wake(awake)).await;
                json!({"ok": truthy(res.get("ok")), "line": res.get("line").cloned().unwrap_or_else(|| json!(""))})
            }
            a @ ("track_on" | "track_off") => {
                let on = a == "track_on";
                let mut res = self.read(move |c| c.track(on)).await;
                let followed = on && truthy(res.get("ok"));
                let line = res.get("line").map(py_str).unwrap_or_default();
                let tail = if followed { " It follows a person now, not where it was pointed." } else { "" };
                res.insert("line".into(), Value::String(format!("{line}{tail}")));
                Value::Object(res)
            }
            "mode" => {
                let name = str_or_empty(arg("mode"));
                Value::Object(self.read(move |c| c.mode(&name)).await)
            }
            "preset" => {
                let n = num(arg("preset")).map_or(Some(0), int_of).unwrap_or(0);
                Value::Object(self.read(move |c| c.preset(n)).await)
            }
            _ => json!({"ok": false, "reason": format!("unknown action {}", py_str(&action))}),
        }
    }

    /// A code-path command (`match`): the line for the owner.
    pub async fn run(&self, cmd: &Command) -> String {
        if let Some(why) = self.ready().await {
            return why;
        }
        let out = if cmd.action == "status" {
            self.handle("camera_status", &json!({})).await
        } else {
            let opt = |v: Option<i64>| v.map_or(Value::Null, Value::from);
            let direction = if cmd.direction.is_empty() { Value::Null } else { json!(cmd.direction) };
            let args = obj(json!({"action": cmd.action, "direction": direction, "degrees": opt(cmd.degrees),
                                  "zoom": opt(cmd.zoom), "zoom_change": opt(cmd.zoom_change),
                                  "preset": opt(cmd.preset)}));
            self.control(&args).await
        };
        info!(
            "obsbot: by code, {} -> {}",
            cmd.action,
            if truthy(out.get("ok")) { "done" } else { "refused" }
        );
        for key in ["line", "reason"] {
            if let Some(v) = out.get(key).filter(|v| truthy(Some(v))) {
                return py_str(v);
            }
        }
        String::new()
    }

    // -- hold to turn --

    async fn turn(&self, hold: &str, direction: &str) -> Value {
        let dir = match direction.parse::<Direction>() {
            Ok(d) if !hold.is_empty() && eve_obsbot::DIRECTIONS.iter().any(|(k, _)| *k == direction) => d,
            _ => return json!({"ok": false, "line": "Hold an arrow.", "stopped": true}),
        };
        let Some(turner) = self.inner.turner.lock().clone() else {
            return json!({"ok": false, "line": crate::NOT_SET_UP_LINE, "stopped": true});
        };
        let me = (hold.to_string(), direction.to_string());
        let first = {
            let mut h = self.inner.hold.lock();
            if h.ended.iter().any(|e| e == hold) {
                return json!({"ok": false, "line": "", "stopped": true}); // a late beat of a press that ended
            }
            h.beat = (self.inner.clock)();
            if h.hold.as_ref() == Some(&me) {
                None
            } else {
                if let Some((old, _)) = h.hold.clone() {
                    h.end(&old);
                }
                let first = h.hold.is_none();
                h.hold = Some(me.clone());
                Some(first)
            }
        };
        if let Some(first) = first {
            if first {
                let t = turner.clone();
                if !blocking(move || t.wake()).await {
                    // a sleeping camera ignores the turn
                    let mut h = self.inner.hold.lock();
                    h.hold = None;
                    h.end(hold);
                    let why = turner.why();
                    let line = if why.is_empty() { "The camera is asleep and didn't wake.".to_string() } else { why };
                    return json!({"ok": false, "line": line, "stopped": true});
                }
            }
            if self.inner.hold.lock().hold.as_ref() != Some(&me) {
                // The wake takes up to about 7 s and the press ended (or another replaced it) meanwhile: the
                // stop already went out, and a turn now would run the gimbal to the end of its range.
                return json!({"ok": false, "line": "", "stopped": true});
            }
            // a new direction replaces the old turn in one request; later beats only renew it
            let t = turner.clone();
            blocking(move || t.turn(dir, HOLD_SPEED)).await;
            self.inner.hold.lock().beat = (self.inner.clock)();
        }
        let mut h = self.inner.hold.lock();
        if h.watch.as_ref().is_none_or(|w| w.is_finished()) {
            h.watch = Some(tokio::spawn(watchdog(self.clone())));
        }
        json!({"ok": true, "line": ""})
    }

    /// Stop every direction now (a stop is never queued behind a read), and end the press.
    async fn stop_turning(&self, hold: &str) {
        {
            let mut h = self.inner.hold.lock();
            if let Some((old, _)) = h.hold.clone() {
                h.end(&old);
            }
            h.end(hold);
            h.hold = None;
        }
        let turner = self.inner.turner.lock().clone();
        if let Some(t) = turner {
            blocking(move || t.turn(Direction::Stop, eve_obsbot::TURN_SPEED)).await;
        }
    }

    // -- the picture --

    /// The webcam as whole JPEGs, the newest only; an `Err` with the owner's line when it can't start.
    pub fn frames(&self) -> FrameStream {
        ffmpeg_frames(self.inner.ffmpeg.clone())
    }

    /// The picture to the app as multipart JPEGs (pika's format) through the tunnel: WebRTC's fallback. Any number
    /// of viewers share one capture. Returns when the picture ends or the app hangs up.
    pub async fn stream<W: AsyncWrite + Unpin + Send>(&self, writer: &mut W) {
        let capture = &self.inner.capture;
        let mut watch = capture.watch();
        // an I/O error is the app hanging up
        let _ = async {
            let first = match watch.next().await {
                Some(Ok(jpeg)) => jpeg,
                Some(Err(e)) => {
                    write_json(writer, 503, &json!({"ok": false, "line": e.0})).await?;
                    return Ok(());
                }
                None => return Ok(()),
            };
            writer.write_all(VIDEO_HEAD).await?;
            writer.flush().await?;
            info!("obsbot: picture on (tunnel; {} watching)", capture.viewers());
            write_chunk(writer, &video_part(&first, None)).await?;
            while let Some(item) = watch.next().await {
                match item {
                    Ok(jpeg) => write_chunk(writer, &video_part(&jpeg, None)).await?,
                    Err(_) => {
                        let _ = write_chunk(writer, b"").await;
                        break;
                    }
                }
            }
            Ok::<(), io::Error>(())
        }
        .await;
        watch.close();
        info!("obsbot: picture off (tunnel; {} watching)", capture.viewers());
    }

    pub async fn close(&self) {
        self.stop_turning("").await;
        self.close_peers().await;
        self.inner.capture.stop().await;
        let mut slot = self.inner.cam.clone().lock_owned().await;
        if slot.is_some() {
            blocking(move || {
                if let Some(cam) = slot.as_mut() {
                    cam.close();
                }
            })
            .await;
        }
    }
}

/// A move's answer for the model and the owner: where the camera ended up, tilt + up. Python's `_moved`.
pub(crate) fn moved(res: &Res, what: &str) -> Value {
    let pan = res.get("pan").filter(|v| !v.is_null());
    let pitch = res.get("pitch").filter(|v| !v.is_null());
    let (Some(pan), Some(pitch)) = (pan, pitch) else {
        let reason = res.get("line").cloned().unwrap_or_else(|| json!(format!("the camera didn't confirm the {what}")));
        return json!({"ok": false, "reason": reason});
    };
    let ok = truthy(res.get("ok"));
    let at = where_value(pan, pitch);
    let line = if ok {
        format!("Camera pointing {at}.")
    } else {
        format!("The camera stopped at {at} (the end of its range, or it is asleep).")
    };
    let tilt = PyNum::of(pitch).map_or(Value::Null, |p| p.neg().to_value());
    json!({"ok": ok, "pan": pan, "tilt": tilt, "line": line})
}

async fn watchdog(panel: ObsbotPanel) {
    loop {
        if panel.inner.hold.lock().hold.is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let late = {
            let h = panel.inner.hold.lock();
            h.hold.is_some() && (panel.inner.clock)() - h.beat > HEARTBEAT_SECS
        };
        if late {
            info!("obsbot: no beat for {HEARTBEAT_SECS:.2} s, stopping the gimbal");
            panel.stop_turning("").await;
        }
    }
}

/// `asyncio.to_thread`: a panic on the thread is raised here, as the exception was in Python.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => v,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}
