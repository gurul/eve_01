//! Head: eve turns its head when the owner asks — to any described pose, not a fixed menu.
//!
//! Port of `eve_bridge/head.py`.
//!
//! The voice's backend model reads "look a bit up and to your left", "turn all the
//! way round", "look at the door" and picks the numbers; this module owns the
//! geometry and the wire. One absolute frame, robot-centred, the same one the
//! firmware uses:
//!
//! ```text
//!     yaw    0 = straight ahead (toward the owner at the desk)
//!            + = the robot's own RIGHT, - = the robot's own LEFT   (body.cpp, bench-verified 2026-09-05)
//!            ±120 = as far as the neck turns: "behind you" is the nearest of the two
//!     pitch  45 = level, 5 = all the way down at the desk, 85 = up at the ceiling
//! ```
//!
//! The owner faces the robot, so "my left" is the robot's right. The backend
//! prompt says so; this module only ever speaks the robot's frame.
//!
//! Poses go out as `{"cmd":"look","yaw","pitch","hold"}`. The board echoes its
//! real pose on every camera frame (`"yaw"`/`"pitch"`), which [`Head::observe`]
//! records, so a relative move ("a bit more left") starts from where the head
//! actually is, not from where it was last told to go.
//!
//! [`look_around`] and [`find`] combine the head with the scene: turn, let the
//! image settle, then describe (or search) a frame taken after the turn — a view
//! from before the move is never reported as the view at the new pose.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

pub const YAW_MAX: i64 = 120; // body.cpp YAW_MAX
pub const PITCH_MIN: i64 = 5; // body.cpp PITCH_MIN
pub const PITCH_MAX: i64 = 85; // body.cpp PITCH_MAX
pub const PITCH_LEVEL: i64 = 45; // body.cpp PITCH_LEVEL
pub const CAMERA_HFOV_DEG: f64 = 66.0; // gaze.cpp kCameraHfovDeg (GC0308 lens assumption)
pub const CAMERA_VFOV_DEG: f64 = CAMERA_HFOV_DEG * 3.0 / 4.0;
pub const DEFAULT_HOLD_SECS: f64 = 15.0;
pub const MAX_HOLD_SECS: f64 = 60.0; // the wire's hold is a uint16 in ms
pub const SETTLE_SECS: f64 = 1.0; // head tween + a fresh frame after the move
pub const SWEEP: &[(i64, i64)] = &[(-100, 50), (-50, 50), (0, 50), (50, 50), (100, 50)];

/// Sends one command dict to the board; true when it went out.
pub type Sender = Arc<dyn Fn(Value) -> BoxFuture<'static, bool> + Send + Sync>;
/// Whether the board is connected.
pub type Connected = Arc<dyn Fn() -> bool + Send + Sync>;
/// A monotonic clock in seconds.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
/// An async sleep in seconds.
pub type Sleep = Arc<dyn Fn(f64) -> BoxFuture<'static, ()> + Send + Sync>;
/// Told of every pose this class commands, with its hold.
pub type OnMove = Arc<dyn Fn(f64) + Send + Sync>;

/// A named pose's move, in the robot's frame. `None` keeps that axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PoseMove {
    pub yaw: Option<i64>,
    pub pitch: Option<i64>,
    pub relative: bool,
}

const fn pm(yaw: Option<i64>, pitch: Option<i64>, relative: bool) -> PoseMove {
    PoseMove { yaw, pitch, relative }
}

/// A named pose → the move it means, in the robot's frame (negative yaw is its own left). The numbers are
/// the ones the backend's prompt already uses ("a bit" ≈ 15°, "left" ≈ 70°, "all the way" 120), so a pose
/// chosen by the fast path (typed_ask.py) lands where the backend would have put it.
pub const POSE_MOVES: &[(&str, PoseMove)] = &[
    ("left", pm(Some(-70), None, false)),
    ("right", pm(Some(70), None, false)),
    ("up", pm(None, Some(70), false)),
    ("down", pm(None, Some(25), false)),
    ("center", pm(Some(0), Some(PITCH_LEVEL), false)),
    ("far_left", pm(Some(-YAW_MAX), None, false)),
    ("far_right", pm(Some(YAW_MAX), None, false)),
    ("bit_left", pm(Some(-15), None, true)),
    ("bit_right", pm(Some(15), None, true)),
    ("bit_up", pm(None, Some(12), true)),
    ("bit_down", pm(None, Some(-12), true)),
    ("desk", pm(None, Some(10), false)),
    ("ceiling", pm(None, Some(PITCH_MAX), false)),
];

/// `POSE_MOVES.get(name)`.
pub fn pose_move(name: &str) -> Option<PoseMove> {
    POSE_MOVES.iter().find(|(n, _)| *n == name).map(|(_, m)| *m)
}

/// Python's `int(round(x))`: round half to even, then to an integer.
fn py_round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

pub fn clamp_pose(yaw: f64, pitch: f64) -> (i64, i64, bool) {
    let ry = py_round(yaw);
    let rp = py_round(pitch);
    let y = ry.clamp(-YAW_MAX, YAW_MAX);
    let p = rp.clamp(PITCH_MIN, PITCH_MAX);
    (y, p, y != ry || p != rp)
}

pub fn build_look_cmd(yaw: i64, pitch: i64, hold_ms: i64) -> Value {
    json!({"cmd": "look", "yaw": yaw, "pitch": pitch, "hold": hold_ms.clamp(0, 65535)})
}

/// A spoken name for a yaw, in the robot's frame.
pub fn direction_word(yaw: f64) -> String {
    let a = yaw.abs();
    let side = if yaw > 0.0 { "right" } else { "left" };
    if a < 15.0 {
        return "straight ahead".to_string();
    }
    if a < 40.0 {
        return format!("a little to my {side}");
    }
    if a < 80.0 {
        return format!("to my {side}");
    }
    if a < 110.0 {
        return format!("far to my {side}");
    }
    format!("behind me on my {side}")
}

/// Absolute pose that centres a point seen at (x, y) in a frame taken at the given pose.
/// x: -100 left edge .. 100 right edge; y: -100 top .. 100 bottom. Same mapping as
/// gaze.cpp's host-face path: +x = +yaw, +y (down) = lower pitch.
pub fn target_pose(pose_yaw: f64, pose_pitch: f64, x: f64, y: f64) -> (i64, i64, bool) {
    let yaw = pose_yaw + (x / 100.0) * (CAMERA_HFOV_DEG / 2.0);
    let pitch = pose_pitch - (y / 100.0) * (CAMERA_VFOV_DEG / 2.0);
    clamp_pose(yaw, pitch)
}

struct State {
    yaw: f64,
    pitch: f64,
    pose_at: f64,
}

/// The pose eve is holding, and the one command that changes it.
pub struct Head {
    pub send: Sender,
    pub connected: Connected,
    pub clock: Clock,
    pub sleep: Sleep,
    state: Mutex<State>,
    // Told of every pose this class commands, with its hold: an asked-for pose (move_head, look_around,
    // find) owns the head, and the conversation's follower (follow.py) stands down for that long.
    on_move: Mutex<Option<OnMove>>,
}

static MONO_START: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

/// `time.monotonic` stand-in: seconds since first use in this process.
pub fn monotonic() -> f64 {
    MONO_START.elapsed().as_secs_f64()
}

/// `asyncio.sleep` stand-in.
pub fn tokio_sleep() -> Sleep {
    Arc::new(|secs: f64| {
        Box::pin(async move {
            if secs > 0.0 && secs.is_finite() {
                tokio::time::sleep(Duration::from_secs_f64(secs)).await;
            } else {
                tokio::task::yield_now().await;
            }
        })
    })
}

impl Head {
    /// `Head(send)`: always connected, monotonic clock, tokio sleep.
    pub fn new(send: Sender) -> Self {
        Self::with(send, Arc::new(|| true), Arc::new(monotonic), tokio_sleep())
    }

    /// `Head(send, connected, clock, sleep)`.
    pub fn with(send: Sender, connected: Connected, clock: Clock, sleep: Sleep) -> Self {
        Head {
            send,
            connected,
            clock,
            sleep,
            state: Mutex::new(State { yaw: 0.0, pitch: PITCH_LEVEL as f64, pose_at: f64::NEG_INFINITY }),
            on_move: Mutex::new(None),
        }
    }

    pub fn yaw(&self) -> f64 {
        self.state.lock().yaw
    }

    pub fn pitch(&self) -> f64 {
        self.state.lock().pitch
    }

    /// When the board last echoed its pose (`-inf` until it has).
    pub fn pose_at(&self) -> f64 {
        self.state.lock().pose_at
    }

    pub fn set_on_move(&self, f: Option<OnMove>) {
        *self.on_move.lock() = f;
    }

    pub fn now(&self) -> f64 {
        (self.clock)()
    }

    pub async fn pause(&self, secs: f64) {
        (self.sleep)(secs).await
    }

    /// The pose echoed on a camera frame. Ignores anything that is not a number.
    pub fn observe(&self, yaw: &Value, pitch: &Value) {
        if let Some(y) = yaw.as_f64() {
            let now = self.now();
            let mut s = self.state.lock();
            s.yaw = y;
            s.pose_at = now;
        }
        if let Some(p) = pitch.as_f64() {
            self.state.lock().pitch = p;
        }
    }

    /// Turn to a pose (absolute) or by an offset (relative). A missing axis keeps
    /// its current value. Hold 0 hands the head back to the robot's own behaviour.
    /// (`Head.move` in Python; `move` is a Rust keyword.)
    pub async fn move_to(&self, yaw: Option<f64>, pitch: Option<f64>, relative: bool, hold_secs: f64) -> Value {
        if !(self.connected)() {
            return json!({"ok": false, "reason": "the robot is not connected, so it cannot move"});
        }
        let (base_y, base_p) = {
            let s = self.state.lock();
            (s.yaw, s.pitch)
        };
        let want_y = match yaw {
            None => base_y,
            Some(v) if relative => base_y + v,
            Some(v) => v,
        };
        let want_p = match pitch {
            None => base_p,
            Some(v) if relative => base_p + v,
            Some(v) => v,
        };
        let (y, p, clamped) = clamp_pose(want_y, want_p);
        // Not f64::clamp: a NaN hold must become MAX_HOLD_SECS, as Python's max(0, min(60, nan)) does.
        #[allow(clippy::manual_clamp)]
        let hold = hold_secs.min(MAX_HOLD_SECS).max(0.0);
        let cmd = build_look_cmd(y, p, (hold * 1000.0) as i64);
        let ok = (self.send)(cmd).await;
        if !ok {
            return json!({"ok": false, "reason": "the move did not reach the robot"});
        }
        {
            let mut s = self.state.lock();
            s.yaw = y as f64;
            s.pitch = p as f64;
        }
        let cb = self.on_move.lock().clone();
        if let Some(cb) = cb {
            cb(hold);
        }
        let mut out = json!({"ok": true, "yaw": y, "pitch": p, "facing": direction_word(y as f64), "hold_secs": hold});
        if clamped {
            out["note"] = Value::String(format!(
                "that is as far as the head goes (yaw ±{YAW_MAX}, pitch {PITCH_MIN}..{PITCH_MAX}); it stopped at the limit"
            ));
        }
        out
    }

    pub async fn face_owner(&self) -> Value {
        self.move_to(Some(0.0), Some(PITCH_LEVEL as f64), false, 3.0).await
    }
}

/// What `look_around` and `find` need from scene.py's watcher.
#[async_trait::async_trait]
pub trait Scene: Send + Sync {
    /// `scene.look(fresh_secs=..., newer_than=...)`.
    async fn look(&self, fresh_secs: f64, newer_than: Option<f64>) -> Value;
    /// `scene.locate(target, newer_than=...)`.
    async fn locate(&self, target: &str, newer_than: Option<f64>) -> Value;
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python `str(v)` for the reason checks.
fn py_str(v: Option<&Value>) -> String {
    match v {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) => "None".to_string(),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.to_string(),
        Some(other) => other.to_string(),
    }
}

/// Python `float(v)` for a locate coordinate; anything that is not a number reads as 0.
fn py_float(v: Option<&Value>, default: f64) -> f64 {
    match v {
        None => default,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0),
        Some(_) => 0.0,
    }
}

fn with_key(r: &Value, key: &str, v: Value) -> Value {
    let mut m: Map<String, Value> = r.as_object().cloned().unwrap_or_default();
    m.insert(key.to_string(), v);
    Value::Object(m)
}

/// Pan left to right, describe each stop from a frame taken after the turn, then face the owner.
pub async fn look_around(head: &Head, scene: &dyn Scene, stops: &[(i64, i64)], settle: f64) -> Value {
    let mut views: Vec<Value> = Vec::new();
    for &(yaw, pitch) in stops {
        let r = head.move_to(Some(yaw as f64), Some(pitch as f64), false, settle + 8.0).await;
        if !truthy(r.get("ok")) {
            return with_key(&r, "views", Value::Array(views));
        }
        let moved_at = head.now();
        head.pause(settle).await;
        let v = scene.look(0.0, Some(moved_at)).await;
        let ry = r["yaw"].as_f64().unwrap_or(0.0);
        let mut entry = json!({"direction": direction_word(ry), "yaw": r["yaw"].clone()});
        if truthy(v.get("ok")) && truthy(v.get("view")) {
            entry["view"] = v["view"].clone();
        } else {
            entry["view"] = Value::Null;
            entry["reason"] = v.get("reason").cloned().unwrap_or_else(|| Value::String("no clear view".into()));
            if py_str(v.get("reason")).contains("not connected") {
                head.face_owner().await;
                return json!({"ok": false, "reason": v["reason"].clone(), "views": views});
            }
        }
        views.push(entry);
    }
    head.face_owner().await;
    json!({"ok": true, "views": views})
}

/// Look for `target`: first where the head already points, then along the sweep.
/// Centre on the first sighting and hold there.
pub async fn find(
    head: &Head,
    scene: &dyn Scene,
    target: &str,
    stops: &[(i64, i64)],
    settle: f64,
    hold_secs: f64,
) -> Value {
    let joined = target.split_whitespace().collect::<Vec<_>>().join(" ");
    let target: String = joined.chars().take(80).collect();
    if target.is_empty() {
        return json!({"ok": false, "reason": "no target given"});
    }
    let mut searched = 0;
    let plan = std::iter::once(None).chain(stops.iter().copied().map(Some));
    for stop in plan {
        if let Some((sy, sp)) = stop {
            let r = head.move_to(Some(sy as f64), Some(sp as f64), false, settle + 8.0).await;
            if !truthy(r.get("ok")) {
                return with_key(&r, "found", Value::Bool(false));
            }
        }
        let moved_at = head.now();
        if stop.is_some() {
            head.pause(settle).await;
        }
        let loc = scene.locate(&target, if stop.is_some() { Some(moved_at) } else { None }).await;
        searched += 1;
        if !truthy(loc.get("ok")) {
            if py_str(loc.get("reason")).contains("not connected") {
                return json!({"ok": false, "found": false, "reason": loc["reason"].clone()});
            }
            continue;
        }
        if truthy(loc.get("visible")) {
            let py = loc.get("yaw").and_then(Value::as_f64).unwrap_or_else(|| head.yaw());
            let pp = loc.get("pitch").and_then(Value::as_f64).unwrap_or_else(|| head.pitch());
            let (ty, tp, _) = target_pose(py, pp, py_float(loc.get("x"), 0.0), py_float(loc.get("y"), 0.0));
            let r = head.move_to(Some(ty as f64), Some(tp as f64), false, hold_secs).await;
            let what = if truthy(loc.get("what")) { loc["what"].clone() } else { Value::String(target.clone()) };
            return json!({"ok": r["ok"].clone(), "found": true, "what": what,
                          "facing": direction_word(ty as f64), "yaw": ty, "pitch": tp});
        }
    }
    head.face_owner().await;
    json!({"ok": true, "found": false, "searched_stops": searched,
           "reason": format!("could not see {target} anywhere the head can turn")})
}

#[cfg(test)]
mod tests;
