//! The IPC handler (`_handle_ipc`): one method per event, by the request's "evt" (`_ipc_<evt>`); unknown events
//! answer {"ok": false, "error": "unknown evt"} as Python's table did. Also `_mcp_late_done` and `_mcp_refusal`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::daemon::Daemon;
use crate::seams::{Inlet, Job, Obj};
use crate::{CELEBRATE_SECS, CHAT_MAX_CHARS, ENTRY_PAYLOAD_MAX_BYTES, MCP_CALL_BOUND_SECS, MCP_CALL_MARGIN_SECS,
    MCP_STILL_RUNNING, TURN_END_DEFER_SECS};

/// The events [`Daemon::handle_ipc`] answers, in Python's table order.
pub const IPC_EVENTS: &[&str] = &[
    "expressions",
    "explore",
    "lesson",
    "notes",
    "trace",
    "pose",
    "follow",
    "axis",
    "move",
    "celebrate",
    "species",
    "diag",
    "identity",
    "session_start",
    "session_end",
    "turn_begin",
    "turn_end",
    "pretooluse",
    "permissionrequest",
    "push_character",
    "unpair",
    "sound",
    "chat",
    "mcp_tools",
    "mcp_call",
    "mic",
    "get_state",
    "posttooluse",
    "notification",
];

// ---- Python's value semantics, for the free-form requests ---------------------------------------------------------

/// A Python exception escaping a handler: the IPC server replies `"<kind>: <message>"`.
pub(crate) fn raised(kind: &str, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(eve_ipc::NamedError::new(kind, message))
}

/// Python's `bool(v)` for a JSON value.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `req.get(key)` when it is truthy (Python's `req.get(key) or ...`).
pub(crate) fn get_truthy<'a>(req: &'a Obj, key: &str) -> Option<&'a Value> {
    req.get(key).filter(|v| truthy(v))
}

/// The class name `type(v).__name__` gives for a JSON value.
pub(crate) fn py_type(v: &Value) -> &'static str {
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

fn py_number(n: &serde_json::Number) -> String {
    match n.as_f64() {
        Some(f) if n.is_f64() => eve_ipc::py_float_repr(f),
        _ => n.to_string(),
    }
}

/// Python's `repr(v)` for a JSON value.
pub(crate) fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => py_number(n),
        Value::String(s) => eve_motion::py_repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", eve_motion::py_repr_str(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Python's `str(v)` for a JSON value.
pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `str(req.get(key) or default)`.
pub(crate) fn str_or(req: &Obj, key: &str, default: &str) -> String {
    get_truthy(req, key).map(py_str).unwrap_or_else(|| default.to_string())
}

/// `str(req.get(key))`: "None" when absent.
fn str_of(req: &Obj, key: &str) -> String {
    req.get(key).map(py_str).unwrap_or_else(|| "None".to_string())
}

/// Python's `float(v)`: a number, a bool, or a string `float()` reads; anything else raises.
pub(crate) fn py_float(v: &Value) -> anyhow::Result<f64> {
    match v {
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => parse_py_float(s)
            .ok_or_else(|| raised("ValueError", format!("could not convert string to float: {}", eve_motion::py_repr_str(s)))),
        other => Err(raised(
            "TypeError",
            format!("float() argument must be a string or a real number, not '{}'", py_type(other)),
        )),
    }
}

fn parse_py_float(s: &str) -> Option<f64> {
    let t = s.trim().replace('_', "");
    if t.is_empty() || s.trim().starts_with('_') || s.trim().ends_with('_') {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let (sign, body) = match lower.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, lower.strip_prefix('+').unwrap_or(&lower)),
    };
    match body {
        "inf" | "infinity" => return Some(sign * f64::INFINITY),
        "nan" => return Some(f64::NAN),
        _ => {}
    }
    if !body.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-')) {
        return None;
    }
    body.parse::<f64>().ok().map(|f| sign * f)
}

/// Python's `round(x, 3)`: the nearest of the decimal's exact value, ties to even.
pub(crate) fn round3(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.3}").parse::<f64>().unwrap_or(x)
}

/// A float as JSON; Python's `json.dumps` writes `-Infinity` where a `Value` can only hold null.
fn float_value(x: f64) -> Value {
    serde_json::Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null)
}

fn chars_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `(req.get("session_id") or "?")[:8]`.
fn sid8(req: &Obj) -> String {
    chars_prefix(&str_or(req, "session_id", "?"), 8)
}

fn obj(v: Value) -> Obj {
    match v {
        Value::Object(m) => m,
        other => {
            let mut m = Map::new();
            m.insert("ok".into(), Value::Bool(true));
            m.insert("value".into(), other);
            m
        }
    }
}

fn ok_obj(pairs: Value) -> Obj {
    obj(pairs)
}

/// `req["session_id"]`: a KeyError when it is absent.
fn session_key(req: &Obj) -> anyhow::Result<String> {
    match req.get("session_id") {
        Some(v) => Ok(py_str(v)),
        None => Err(raised("KeyError", "'session_id'")),
    }
}

// ---- the late MCP calls -------------------------------------------------------------------------------------------

/// How a late call ended: cancelled, failed with this exception class, or done.
fn late_outcome(cancelled: bool, err: Option<&str>) {
    if cancelled {
        tracing::info!("work sessions: a late tool call was cancelled");
        return;
    }
    tracing::info!("work sessions: a late tool call finished ({})", err.unwrap_or("done"));
}

/// `_mcp_late_done(task)`: a Work session's MCP call that outlived its bound finished; log how it ended.
///
/// A [`Job`] carries no exception, so this reports "done" for any call not cancelled; the call itself
/// ([`Daemon::ipc_mcp_call`]) logs the exception class when it knows it.
pub fn mcp_late_done(job: &Job) {
    late_outcome(job.cancelled(), None);
}

/// The call's task side: its drop with `late` set and no result is a late call cancelled (at shutdown's drain).
struct LateGuard {
    late: Arc<AtomicBool>,
    finished: bool,
}

impl Drop for LateGuard {
    fn drop(&mut self) {
        if !self.finished && self.late.load(Ordering::SeqCst) {
            late_outcome(true, None);
        }
    }
}

/// The handler's side: the handler itself cancelled (`except asyncio.CancelledError: task.cancel()`) cancels the
/// call, unless it was handed to the background first.
struct CancelOnDrop(Option<Job>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            if !job.done() {
                job.cancel();
            }
        }
    }
}

type CallResult = anyhow::Result<(Obj, String)>;

fn call_error_kind(r: &CallResult) -> Option<String> {
    r.as_ref().err().map(eve_ipc::error_type_name)
}

// ---- the audition's and the follow test's ends --------------------------------------------------------------------

/// `restore_phase()`'s finally: the phase the audition borrowed goes back, unless something else moved it.
struct RestorePhase {
    d: Arc<Daemon>,
    phase: String,
    previous: String,
}

impl Drop for RestorePhase {
    fn drop(&mut self) {
        let now = self.d.st.lock().agent_state.clone();
        if now == self.phase && !self.d.conversation_open() {
            self.d.on_agent_state(&self.previous);
        }
    }
}

/// `hold()`'s finally, which awaits: run on the loop when the hold is cancelled under it.
struct FollowOver {
    d: Option<Arc<Daemon>>,
}

async fn follow_over(d: Arc<Daemon>) {
    if d.follower.phase() == "listening" {
        d.follower.on_phase("idle").await;
        if d.ble.connected() {
            d.ble.send(&json!({"cmd": "agent", "state": "idle"}), None).await;
        }
        tracing::info!("follow test: over");
    }
}

impl Drop for FollowOver {
    fn drop(&mut self) {
        if let Some(d) = self.d.take() {
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(follow_over(d));
            }
        }
    }
}

impl Daemon {
    /// `_handle_ipc(req)`: dispatch by "evt"; the Work registry is noted first.
    pub async fn handle_ipc(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        let evt = req.get("evt").cloned().unwrap_or(Value::Null);
        let name = evt.as_str();
        // Drop pretooluse from the trace: it has its own dedicated INFO log,
        // and the volume would drown out everything else. get_state is the
        // hud polling — also too chatty to be useful here.
        if !matches!(name, Some("pretooluse" | "get_state" | "expressions")) {
            tracing::info!("ipc evt={} session={}", py_repr(&evt), sid8(&req));
        }
        // Every hook event is activity for the idle explorer, except the
        // polls that fire on their own (statusline, diag watch).
        if !matches!(name, Some("get_state" | "diag" | "explore" | "expressions")) {
            self.note_activity();
        }
        let Some(name) = name else {
            return Ok(unknown_evt(&evt));
        };
        match name {
            "expressions" => self.ipc_expressions(req).await,
            "explore" => self.ipc_explore(req).await,
            "lesson" => self.ipc_lesson(req).await,
            "notes" => self.ipc_notes(req).await,
            "trace" => self.ipc_trace(req).await,
            "pose" => self.ipc_pose(req).await,
            "axis" => self.ipc_axis(req).await,
            "follow" => self.ipc_follow(req).await,
            "move" => self.ipc_move(req).await,
            "celebrate" => self.ipc_celebrate(req).await,
            "species" => self.ipc_species(req).await,
            "diag" => self.ipc_diag(req).await,
            "identity" => self.ipc_identity(req).await,
            "session_start" => self.ipc_session_start(req).await,
            "session_end" => self.ipc_session_end(req).await,
            "turn_begin" => self.ipc_turn_begin(req).await,
            "turn_end" => self.ipc_turn_end(req).await,
            "pretooluse" => self.ipc_pretooluse(req).await,
            "permissionrequest" => self.ipc_permissionrequest(req).await,
            "push_character" => self.ipc_push_character(req).await,
            "unpair" => self.ipc_unpair(req).await,
            "sound" => self.ipc_sound(req).await,
            "mic" => self.ipc_mic(req).await,
            "chat" => self.ipc_chat(req).await,
            "mcp_tools" => self.ipc_mcp_tools(req).await,
            "mcp_call" => self.ipc_mcp_call(req).await,
            "get_state" => self.ipc_get_state(req).await,
            "posttooluse" => self.ipc_posttooluse(req).await,
            "notification" => self.ipc_notification(req).await,
            _ => Ok(unknown_evt(&evt)),
        }
    }

    /// The daemon's IPC handler as the server takes it.
    pub fn ipc_handler(self: &Arc<Self>) -> eve_ipc::Handler {
        // Weak: the daemon holds the server, which holds this handler.
        let me = Arc::downgrade(self);
        eve_ipc::handler(move |req: Value| {
            let me = me.clone();
            async move {
                let d = me.upgrade().ok_or_else(|| raised("RuntimeError", "the daemon is gone"))?;
                match req {
                    Value::Object(m) => Ok(Value::Object(d.handle_ipc(m).await?)),
                    // `req.get("evt")` on something that is not a dict
                    other => Err(raised("AttributeError", format!("'{}' object has no attribute 'get'", py_type(&other)))),
                }
            }
        })
    }

    /// `_mcp_refusal(daemon, req)`: why a Work session's MCP request is refused (no door, a bad token, a session
    /// that is not eve's), None when it may go on.
    pub fn mcp_refusal(&self, req: &Obj) -> Option<Obj> {
        let (work, inlet) = {
            let st = self.st.lock();
            (st.work.clone(), st.telegram.clone())
        };
        let sid = req.get("sid").and_then(Value::as_str);
        let token = req.get("token").and_then(Value::as_str);
        if work.is_none_or(|w| !w.check_token(sid, token)) {
            tracing::info!("work sessions: refused an eve MCP request (no such session, or a wrong token)");
            return Some(ok_obj(json!({"ok": false, "error": "not a live Work session eve started"})));
        }
        if inlet.is_none_or(|i| i.chat_id().is_none()) {
            return Some(ok_obj(json!({"ok": false, "error": "eve's chat isn't running, or no owner has texted it yet"})));
        }
        None
    }

    /// `_ipc_expressions(req)`.
    pub async fn ipc_expressions(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        let mut service = self.expressions.clone();
        let action = match req.get("action") {
            None => Value::String("status".into()),
            Some(v) => v.clone(),
        };
        let action_s = action.as_str().unwrap_or("");
        let phone = self.st.lock().phone_expressions.clone();
        if action.is_string() && matches!(action_s, "on" | "off") {
            service.set_enabled(action_s == "on").await.map_err(|e| raised(&e.kind, e.message))?;
        } else if action.is_string() && matches!(action_s, "react" | "audition") {
            let text = req.get("text").and_then(Value::as_str);
            let Some(text) = text.filter(|t| !t.trim().is_empty() && t.chars().count() <= 2000) else {
                return Ok(ok_obj(json!({"ok": false, "error": "text must contain 1..2000 characters"})));
            };
            let connected = self.ble.connected();
            match &phone {
                Some(p) if action_s == "react" && !connected && p.enabled() && p.connected() => {
                    service = p.clone(); // no board: a react shows on the live phone call's face
                }
                _ if !service.enabled() || !service.ready() || !connected => {
                    return Ok(ok_obj(
                        json!({"ok": false, "error": "expressions are not ready or board is disconnected"}),
                    ));
                }
                _ => {}
            }
            if action_s == "audition" {
                let phase = match req.get("phase") {
                    None => "speaking".to_string(),
                    Some(Value::String(s)) if matches!(s.as_str(), "speaking" | "listening" | "idle") => s.clone(),
                    Some(_) => {
                        return Ok(ok_obj(
                            json!({"ok": false, "error": "audition phase must be speaking, listening, or idle"}),
                        ));
                    }
                };
                let auditioning = self.st.lock().expression_audition.as_ref().is_some_and(|a| !a.done());
                if self.conversation_open() || auditioning {
                    return Ok(ok_obj(json!({"ok": false, "error": "wait until the conversation or audition ends"})));
                }
                let previous = self.st.lock().agent_state.clone();
                self.on_agent_state(&phase);
                let restore = RestorePhase { d: self.clone(), phase, previous };
                let job = Job::spawn("expression-audition", async move {
                    let _restore = restore; // its drop is the finally
                    tokio::time::sleep(Duration::from_secs(6)).await;
                });
                self.st.lock().expression_audition = Some(job);
            }
            let event = service.offer("demo", text);
            return Ok(ok_obj(json!({"ok": event.is_some(), "id": event, "expressions": service.status()})));
        } else if !(action.is_string() && action_s == "status") {
            return Ok(ok_obj(json!({"ok": false, "error": "unknown expression action"})));
        }
        let phone_v = match &phone {
            None => Value::Null,
            Some(p) => {
                let mut m = Map::new();
                m.insert("connected".into(), Value::Bool(p.connected()));
                if let Value::Object(s) = p.status() {
                    m.extend(s);
                }
                Value::Object(m)
            }
        };
        Ok(ok_obj(json!({"ok": true, "connected": self.ble.connected(), "expressions": service.status(),
                         "phone": phone_v})))
    }

    /// `_ipc_explore(req)`.
    pub async fn ipc_explore(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // `eve-bridge explore [start|stop|status]`: the owner sends
        // the robot off to look around (or calls it back) by hand.
        let action = str_or(&req, "action", "start");
        if action == "start" {
            if let Err(e) = self.request_explore("requested by cli").await {
                let status = self.explorer.lock().status();
                return Ok(ok_obj(
                    json!({"ok": false, "error": e.to_string(), "connected": self.ble.connected(), "explore": status}),
                ));
            }
        } else if action == "stop" {
            self.note_activity(); // the human is here
            self.dismiss_explore("requested by cli").await;
        } else if action != "status" {
            return Ok(ok_obj(
                json!({"ok": false, "error": format!("unknown explore action: {}", eve_motion::py_repr_str(&action))}),
            ));
        }
        let status = self.explorer.lock().status();
        Ok(ok_obj(json!({"ok": true, "connected": self.ble.connected(), "explore": status})))
    }

    /// `_ipc_lesson(req)`.
    pub async fn ipc_lesson(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // `eve-bridge lesson <action>`: the lesson voice tool's path, from a terminal.
        Ok(self.handle_lesson(req).await)
    }

    /// `_ipc_notes(req)`.
    pub async fn ipc_notes(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // `eve-bridge notes start|stop|status`, and the voice path.
        let action = str_or(&req, "action", "status");
        let reason = str_or(&req, "reason", "asked");
        // stop and status reach the phone's notes while they are on; start is always the room's
        let miniapp = self.st.lock().miniapp.clone();
        let calls = miniapp.and_then(|m| m.server.calls.lock().clone());
        if action != "start" {
            if let Some(calls) = calls.filter(|c| c.notes().is_some()) {
                if let Some(phone) = calls.notes_action(&action, None, &reason, None).await {
                    return Ok(obj(phone));
                }
            }
        }
        let taker = self.room_notes_taker();
        if action == "start" {
            return Ok(obj(taker.start()));
        }
        if action == "stop" {
            return Ok(obj(taker.stop(&reason).await));
        }
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(true));
        if let Value::Object(s) = taker.status() {
            out.extend(s);
        }
        Ok(out)
    }

    /// `_ipc_trace(req)`.
    pub async fn ipc_trace(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // The pose series the board reported during the last motion.
        let trace: Vec<Value> = self
            .st
            .lock()
            .motion_trace
            .iter()
            .map(|&(t, y, p)| Value::Array(vec![float_value(round3(t)), float_value(y), float_value(p)]))
            .collect();
        Ok(ok_obj(json!({"ok": true, "trace": trace})))
    }

    /// `_ipc_pose(req)`.
    pub async fn ipc_pose(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // Where the head actually is, as the BOARD reports it on every camera
        // frame (~4.3/s, head.observe). Sampling this during a motion is how a
        // delivered swing is measured without a camera pointed at the room —
        // the plan's own check, and the only one that reads the servos rather
        // than the request.
        let head = &self.head;
        Ok(ok_obj(json!({"ok": true, "yaw": float_value(head.yaw()), "pitch": float_value(head.pitch()),
                         "at": float_value(head.pose_at()), "connected": self.ble.connected()})))
    }

    /// `_ipc_follow(req)`.
    pub async fn ipc_follow(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // A follow-only test: the follower gets a conversation phase ("listening") for `secs` seconds, with no
        // voice, ring or expressions, so eve just keeps its eyes on whoever is in front of it. "stop" ends it.
        let follower = self.follower.clone();
        let task = self.st.lock().follow_test.clone();
        if let Some(task) = task.filter(|t| !t.done()) {
            task.cancel();
        }
        if str_or(&req, "action", "start") == "stop" {
            follower.on_phase("idle").await;
            if self.ble.connected() {
                self.ble.send(&json!({"cmd": "agent", "state": "idle"}), None).await;
            }
            return Ok(ok_obj(json!({"ok": true, "phase": follower.phase()})));
        }
        let secs = match get_truthy(&req, "secs") {
            Some(v) => py_float(v)?,
            None => 60.0,
        };
        let secs = 1.0f64.max(600.0f64.min(secs));

        // The board accepts a host look in any state only during a conversation (hostlook.h), so the test
        // shows it one, as a desk call would; otherwise an attention state silently keeps the head.
        if self.ble.connected() {
            self.ble.send(&json!({"cmd": "agent", "state": "listening"}), None).await;
        }
        follower.on_phase("listening").await;
        tracing::info!("follow test: following for {:.0} s", secs);
        let me = self.clone();
        let job = Job::spawn("follow-test", async move {
            let mut over = FollowOver { d: Some(me) };
            tokio::time::sleep(Duration::try_from_secs_f64(secs).unwrap_or(Duration::from_secs(600))).await;
            if let Some(d) = over.d.take() {
                follow_over(d).await;
            }
        });
        self.st.lock().follow_test = Some(job);
        Ok(ok_obj(json!({"ok": true, "phase": follower.phase(), "secs": secs, "connected": self.ble.connected()})))
    }

    /// `_ipc_axis(req)`.
    pub async fn ipc_axis(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // The head's axis directions (body.cpp, saved on the board). With "pitch_rev"/"yaw_rev" it sets them;
        // without, it reads them. The head calibration page (head_cal.py) drives this.
        if !self.ble.connected() {
            return Ok(ok_obj(json!({"ok": false, "error": "the robot is not connected"})));
        }
        let mut cmd = Map::new();
        cmd.insert("cmd".into(), json!("axis"));
        for k in ["pitch_rev", "yaw_rev"] {
            if let Some(v) = req.get(k).filter(|v| !v.is_null()) {
                cmd.insert(k.into(), Value::Bool(truthy(v)));
            }
        }
        let waiter = self.expect_ack("axis", None);
        self.ble.send(&Value::Object(cmd), None).await;
        let ack = match self.wait_for_ack("axis", Duration::from_secs(3), Some(waiter)).await {
            Ok(ack) => ack,
            Err(eve_folder_push::AckWaitError::Timeout) => {
                return Ok(ok_obj(json!({"ok": false, "error": "the board did not answer (older firmware?)"})));
            }
            Err(eve_folder_push::AckWaitError::Cancelled) => return Err(raised("CancelledError", "")),
        };
        let n = match ack.get("n").filter(|v| truthy(v)) {
            Some(v) => eve_motion::py_int(v).map_err(|e| raised(e.python_type(), e.to_string()))?,
            None => 0,
        };
        tracing::info!("axis: pitch_rev={} yaw_rev={}", py_bool(n & 2 != 0), py_bool(n & 1 != 0));
        Ok(ok_obj(json!({"ok": true, "pitch_rev": n & 2 != 0, "yaw_rev": n & 1 != 0})))
    }

    /// `_ipc_move(req)`.
    pub async fn ipc_move(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // `eve-bridge move …`: a named motion the BOARD runs. The host
        // only names it; motion.h admits every number before a servo sees it,
        // so this path cannot ask for something unsafe however it is called.
        let kind = str_or(&req, "kind", "osc");
        let preset = req.get("preset").and_then(Value::as_str);
        let built: anyhow::Result<eve_motion::Command> = (|| {
            let motion = |e: eve_motion::MotionError| raised(e.python_type(), e.to_string());
            if kind == "stop" {
                Ok(eve_motion::stop_command())
            } else if kind == "keys" {
                match req.get("keys") {
                    None | Some(Value::Null) => eve_motion::keys_command(preset, None).map_err(motion),
                    Some(Value::Array(rows)) => eve_motion::keys_command(preset, Some(rows)).map_err(motion),
                    // a string's keys are its characters, each shorter than a key
                    Some(Value::String(s)) if s.is_empty() => Err(raised("ValueError", "no keys")),
                    Some(Value::String(_)) => {
                        Err(raised("ValueError", "a key is [at_ms, yaw, pitch] with an optional speed"))
                    }
                    Some(other) => Err(raised("TypeError", format!("'{}' object is not subscriptable", py_type(other)))),
                }
            } else {
                let speed = match get_truthy(&req, "speed") {
                    Some(v) => py_float(v)?,
                    None => 1.0,
                };
                let params = eve_motion::OscParams::from_request(&req).map_err(motion)?;
                eve_motion::osc_command(preset, speed, &params).map_err(motion)
            }
        })();
        let cmd = match built {
            Ok(cmd) => cmd,
            Err(e) if eve_ipc::error_type_name(&e) == "ValueError" => {
                return Ok(ok_obj(json!({"ok": false, "error": e.to_string()})));
            }
            Err(e) => return Err(e),
        };
        if !self.ble.connected() {
            return Ok(ok_obj(json!({"ok": false, "error": "the board is not connected"})));
        }
        let ok = self.ble.send(&Value::Object(cmd.clone()), None).await;
        let motion = |e: eve_motion::MotionError| raised(e.python_type(), e.to_string());
        if kind != "stop" {
            let shown = get_truthy(&req, "preset").map(py_str).unwrap_or_else(|| kind.clone());
            tracing::info!("move: {} — {}", shown, eve_motion::describe(&cmd).map_err(motion)?);
        } else {
            tracing::info!("move: stop");
        }
        let asked = eve_motion::predict(&cmd).map_err(motion)?;
        Ok(ok_obj(json!({"ok": ok, "sent": cmd, "asked": asked, "connected": self.ble.connected()})))
    }

    /// `_ipc_celebrate(req)`.
    pub async fn ipc_celebrate(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // Host-triggered celebration. Reuses the same `completed: true`
        // heartbeat flag the pet already celebrates on, so no firmware
        // support is needed — the board cannot tell this from a finished
        // Claude turn, which is exactly the point.
        let secs = match get_truthy(&req, "secs") {
            Some(v) => py_float(v)?,
            None => 5.0,
        };
        self.state.lock().pulse_completed(secs);
        self.push_heartbeat(true).await;
        tracing::info!("celebrate: pulsed for {:.0}s", secs);
        Ok(ok_obj(json!({"ok": true, "connected": self.ble.connected()})))
    }

    /// `_ipc_species(req)`.
    pub async fn ipc_species(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // Replaces the retired on-device menu. Firmware persists the index
        // in NVS, so this survives reboots.
        let idx = match get_truthy(&req, "idx") {
            Some(v) => eve_motion::py_int(v).map_err(|e| raised(e.python_type(), e.to_string()))?,
            None => 0,
        };
        if self.ble.connected() {
            self.ble.send(&json!({"cmd": "species", "idx": idx}), None).await;
            tracing::info!("species: set to index {}", idx);
        }
        Ok(ok_obj(json!({"ok": true, "connected": self.ble.connected()})))
    }

    /// `_ipc_diag(req)`.
    pub async fn ipc_diag(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // `eve-bridge diag`: ask the board for a fresh report, then
        // return the last one we hold. The board's reply arrives
        // asynchronously over serial, so a request now shows up in the
        // NEXT call — hence returning both.
        if self.ble.connected() {
            self.ble.send(&json!({"cmd": "diag"}), None).await;
        }
        let diag = self.st.lock().last_diag.clone().map(Value::Object).unwrap_or(Value::Null);
        Ok(ok_obj(json!({"ok": true, "connected": self.ble.connected(), "diag": diag})))
    }

    /// `_ipc_identity(req)`.
    pub async fn ipc_identity(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // `eve-bridge identity status|reset`. The daemon owns the
        // in-memory prints, so a reset must go through it while it runs.
        Ok(self.handle_identity(&str_or(&req, "action", "status")))
    }

    /// `_ipc_session_start(req)`.
    pub async fn ipc_session_start(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        let session_id = session_key(&req)?;
        self.state.lock().session_start(
            &session_id,
            req.get("transcript_path").and_then(Value::as_str),
            req.get("cwd").and_then(Value::as_str),
        );
        self.work_note("session_start", &req);
        if let Some(inlet) = self.relay() {
            // the hook's reply must never wait on the phone
            inlet.relay_session_started(&str_or(&req, "cwd", ""));
        }
        self.push_heartbeat(false).await;
        Ok(ok_obj(json!({"ok": true})))
    }

    /// `_ipc_session_end(req)`.
    pub async fn ipc_session_end(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        let session_id = session_key(&req)?;
        self.state.lock().session_end(&session_id);
        self.work_note("session_end", &req);
        self.push_heartbeat(false).await;
        Ok(ok_obj(json!({"ok": true})))
    }

    /// `_ipc_turn_begin(req)`.
    pub async fn ipc_turn_begin(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        let session_id = session_key(&req)?;
        // A new user prompt cancels any pending deferred turn_end.
        let pending = self.st.lock().pending_turn_ends.remove(&session_id);
        if let Some(p) = pending.filter(|p| !p.done()) {
            p.cancel();
        }
        let prompt = req.get("prompt").and_then(Value::as_str).unwrap_or("");
        {
            let mut state = self.state.lock();
            // Also kill any active celebrate pulse — user moved on.
            state.completed_until = 0.0;
            state.session_start(&session_id, None, None); // idempotent
            state.turn_begin(&session_id);
        }
        self.work_note("turn_begin", &req);
        if !prompt.is_empty() {
            let entry = format!("> {}", eve_protocol::truncate_utf8_bytes(prompt, ENTRY_PAYLOAD_MAX_BYTES));
            self.state.lock().add_entry(&entry, None);
        }
        // The relay's turn boundary: the lines that follow answer this prompt, read on the call it was said on only
        // if eve typed it from a call (telegram.relay_turn_began; verification/Eve/CallScope.lean).
        if let Some(inlet) = self.relay() {
            let cwd = self.session_cwd(&session_id);
            inlet.relay_turn_began(prompt, &cwd);
        }
        self.push_heartbeat(false).await;
        Ok(ok_obj(json!({"ok": true})))
    }

    /// `_ipc_turn_end(req)`.
    pub async fn ipc_turn_end(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // Don't flip running→0 immediately; the firmware enters clock mode
        // as soon as running+waiting both hit zero, which blanks the
        // transcript HUD before the user has a chance to read the entry
        // we just added. Schedule the flip 15s out — long enough to read,
        // short enough that idle really does clock. A new turn_begin
        // cancels the scheduled task.
        let session_id = session_key(&req)?;
        self.work_note("turn_end", &req); // exact, not the 15 s deferral below
        let previous = self.st.lock().pending_turn_ends.get(&session_id).cloned();
        if let Some(p) = previous.filter(|p| !p.done()) {
            p.cancel();
        }
        let me = self.clone();
        let sid = session_id.clone();
        let job = Job::spawn("deferred-turn-end", async move { me.deferred_turn_end(sid, TURN_END_DEFER_SECS).await });
        self.st.lock().pending_turn_ends.insert(session_id.clone(), job);
        // Trigger the firmware's celebrate animation for a few seconds.
        // Set the pulse state synchronously so the heartbeat snapshot is
        // correct before anything is pushed.
        self.state.lock().pulse_completed(CELEBRATE_SECS);
        // The phone's "typing…" for a relayed line ends with the joined session's turn.
        if let Some(inlet) = self.relay() {
            let cwd = self.session_cwd(&session_id);
            inlet.relay_turn_ended(&cwd);
        }
        // Kick off the BLE push in the background so this coroutine can
        // return {"ok": True} immediately — the Stop hook caller must not
        // block on _push_heartbeat(force=True) or it surfaces as ETIMEDOUT
        // in the plugin's spawnSync call.
        let me = self.clone();
        self.spawn("turn-end-side-effects", async move { me.turn_end_side_effects(CELEBRATE_SECS).await });
        Ok(ok_obj(json!({"ok": true})))
    }

    /// `_ipc_pretooluse(req)`.
    pub async fn ipc_pretooluse(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        self.work_note("pretooluse", &req); // an AskUserQuestion: the Work session waits on the owner
        self.handle_pretooluse(req).await
    }

    /// `_ipc_permissionrequest(req)`.
    pub async fn ipc_permissionrequest(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        self.handle_permission_request(req).await
    }

    /// `_ipc_push_character(req)`.
    pub async fn ipc_push_character(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        let Some(path) = req.get("path").and_then(Value::as_str).filter(|p| !p.is_empty()) else {
            return Ok(ok_obj(json!({"ok": false, "error": "missing 'path'"})));
        };
        if !self.ble.connected() {
            return Ok(ok_obj(json!({"ok": false, "error": "ble not connected"})));
        }
        match eve_folder_push::push_character(&**self, path).await {
            Ok(summary) => {
                let mut out = Map::new();
                out.insert("ok".into(), Value::Bool(true));
                if let Value::Object(s) = summary.to_json() {
                    out.extend(s);
                }
                Ok(out)
            }
            // CancelledError is not an Exception: it escapes the handler
            Err(eve_folder_push::PushError::Cancelled) => Err(raised("CancelledError", "")),
            Err(e) => {
                tracing::error!("push_character failed: {}: {}", e.type_name(), e);
                Ok(ok_obj(json!({"ok": false, "error": format!("{}: {}", e.type_name(), e)})))
            }
        }
    }

    /// `_ipc_unpair(req)`.
    pub async fn ipc_unpair(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // Tell the stick to erase its stored bond so the next pairing
        // shows a fresh passkey (REFERENCE.md §Security and pairing).
        // Macos side still needs a manual 'Forget' from System Settings.
        if !self.ble.connected() {
            return Ok(ok_obj(json!({"ok": false, "error": "ble not connected"})));
        }
        let ok = self.ble.send(&json!({"cmd": "unpair"}), None).await;
        tracing::info!("unpair: sent cmd:unpair to stick (ble write {})", if ok { "ok" } else { "fail" });
        Ok(ok_obj(json!({"ok": ok})))
    }

    /// `_ipc_sound(req)`.
    pub async fn ipc_sound(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // `eve-bridge sound [on|off|status]`
        if let Some(action @ ("on" | "off")) = req.get("action").and_then(Value::as_str) {
            self.set_sound(action == "on");
        }
        let on = self.sound.lock().on;
        Ok(ok_obj(json!({"ok": true, "sound": if on { "on" } else { "off" }, "connected": self.ble.connected()})))
    }

    /// `_ipc_chat(req)`.
    ///
    /// The chat window's reply box (widget/StackChanNotes/ChatView.swift): typed words, handled as a
    /// Telegram text from the owner, so they reach the same brain with every tool. The transcript records
    /// them and the replies, which is where the window reads the conversation from. The socket is 0600:
    /// only the owner's own programs can speak as the owner here.
    pub async fn ipc_chat(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        let raw = str_or(&req, "text", "");
        let text = chars_prefix(raw.trim(), CHAT_MAX_CHARS);
        let tg = self.st.lock().telegram.clone();
        let Some(tg) = tg.filter(|t| t.chat_id().is_some()) else {
            return Ok(ok_obj(
                json!({"ok": false, "error": "eve's Telegram chat isn't running, or no owner has texted it yet"}),
            ));
        };
        if text.is_empty() {
            return Ok(ok_obj(json!({"ok": false, "error": "nothing to send"})));
        }
        tg.hear(&text, false); // typed, not said: never read on a call
        Ok(ok_obj(json!({"ok": true})))
    }

    /// `_ipc_mcp_tools(req)`.
    ///
    /// A Work session's eve MCP server (eve_mcp.py) asks for the tools: the chat brain's own, now
    /// (TelegramInlet.session_tools). Only with the token of a Work session eve started that has not ended.
    pub async fn ipc_mcp_tools(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        if let Some(refused) = self.mcp_refusal(&req) {
            return Ok(refused);
        }
        let Some(inlet) = self.st.lock().telegram.clone() else {
            return Err(raised("AttributeError", "'NoneType' object has no attribute 'session_tools'"));
        };
        let tools = match inlet.session_tools() {
            Ok((tools, _)) => tools,
            Err(e) => {
                // the type only: the request holds a token
                tracing::warn!("work sessions: the tool list failed ({})", eve_ipc::error_type_name(&e));
                return Ok(ok_obj(json!({"ok": false, "error": "eve couldn't list its tools"})));
            }
        };
        tracing::info!("work sessions: {} listed {} tools", chars_prefix(&str_of(&req, "sid"), 8), tools.len());
        let secs = inlet.ask_timeout_secs() + MCP_CALL_MARGIN_SECS;
        Ok(ok_obj(json!({"ok": true, "tools": tools, "call_secs": secs})))
    }

    /// `_ipc_mcp_call(req)`.
    ///
    /// One tool call from a Work session's eve MCP server, run through the chat's own gate and tool path
    /// (TelegramInlet.session_call): an acting tool asks the owner on the phone, and this waits for the answer.
    pub async fn ipc_mcp_call(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        if let Some(refused) = self.mcp_refusal(&req) {
            return Ok(refused);
        }
        let Some(inlet) = self.st.lock().telegram.clone() else {
            return Err(raised("AttributeError", "'NoneType' object has no attribute 'session_call'"));
        };
        let name = req.get("name").map(py_str).unwrap_or_default();
        let args = match req.get("args") {
            Some(Value::Object(m)) => Value::Object(m.clone()),
            _ => Value::Object(Map::new()),
        };
        let sid = str_of(&req, "sid");
        // The daemon bounds the call, never the MCP side: it answers within MCP_CALL_BOUND_SECS of the ask timeout,
        // before eve_mcp.py gives up waiting. A call still running then is not cancelled (an owner-approved
        // purchase or send may be half done); it goes on in _background and the session is told the outcome is
        // unknown and not to retry it.
        let ask = inlet.ask_timeout_secs();
        let bound = ask + MCP_CALL_BOUND_SECS;
        let late = Arc::new(AtomicBool::new(false));
        let (tx, mut rx) = tokio::sync::oneshot::channel::<CallResult>();
        let job = {
            let late = late.clone();
            let inlet: Arc<dyn Inlet> = inlet.clone();
            Job::spawn("mcp-call", async move {
                let mut guard = LateGuard { late, finished: false };
                let r = inlet.session_call(&name, &args, &sid).await;
                guard.finished = true;
                if let Err(r) = tx.send(r) {
                    // the handler answered without it: the call is late
                    late_outcome(false, call_error_kind(&r).as_deref());
                }
            })
        };
        let mut cancel = CancelOnDrop(Some(job.clone()));
        let waited =
            tokio::time::timeout(Duration::try_from_secs_f64(bound.max(0.0)).unwrap_or(Duration::MAX), &mut rx).await;
        let result = match waited {
            Err(_elapsed) => {
                late.store(true, Ordering::SeqCst);
                cancel.0 = None;
                if let Ok(r) = rx.try_recv() {
                    // it finished at the bound itself
                    late_outcome(false, call_error_kind(&r).as_deref());
                }
                drop(rx);
                self.background.lock().push(job);
                tracing::warn!(
                    "work sessions: {}'s tool call is still running after {:.0} s; told the session the outcome is unknown",
                    chars_prefix(&str_of(&req, "sid"), 8),
                    bound
                );
                return Ok(ok_obj(json!({"ok": false, "error": MCP_STILL_RUNNING})));
            }
            Ok(Ok(r)) => r,
            Ok(Err(_gone)) => Err(raised("CancelledError", "")),
        };
        cancel.0 = None;
        match result {
            Ok((result, outside)) => Ok(ok_obj(json!({"ok": true, "result": result, "outside": outside}))),
            Err(e) => {
                // the type only: the request holds a token and the arguments
                tracing::warn!("work sessions: a tool call failed ({})", eve_ipc::error_type_name(&e));
                Ok(ok_obj(json!({"ok": false, "error": "that tool call failed in eve"})))
            }
        }
    }

    /// `_ipc_mic(req)`.
    pub async fn ipc_mic(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // `eve-bridge mic [on|off|status]`, and the menu-bar app's switch.
        if let Some(action @ ("on" | "off")) = req.get("action").and_then(Value::as_str) {
            if self.mic.lock().set(action == "on") {
                tracing::info!("mic: {} (owner's choice)", action);
            }
            self.apply_mic("your choice");
        }
        Ok(self.mic_status())
    }

    /// `_ipc_get_state(req)`.
    pub async fn ipc_get_state(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // Queried by the `eve-bridge hud` subcommand (or anyone else
        // who wants a one-shot snapshot). Kept small on purpose.
        let (ears, sec, battery, update) = {
            let st = self.st.lock();
            (st.ears.clone(), st.last_stick_sec, st.last_stick_battery_pct, st.update_available.clone())
        };
        let mic_listening = ears.is_some_and(|e| e.listening());
        let state = self.state.lock();
        Ok(ok_obj(json!({
            "ok": true,
            "state": {
                "ble_connected": self.ble.connected(),
                "mic_listening": mic_listening,
                "sec": sec,
                "battery_pct": battery,
                "total": state.total(),
                "running": state.running_count(),
                "waiting": state.waiting_count(),
                "tokens_cumulative": state.tokens_cumulative,
                "tokens_today": state.tokens_today,
                "cost_cumulative": float_value(state.cost_cumulative),
                "cost_today": float_value(state.cost_today),
                "update_available": update,
                "last_entry": state.entries.first().map(|e| e.text.clone()).unwrap_or_default(),
            },
        })))
    }

    /// `_ipc_posttooluse(req)`.
    pub async fn ipc_posttooluse(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // A tool ran → any terminal-side permission prompt was answered.
        let sid = req.get("session_id").map(py_str).unwrap_or_default();
        self.state.lock().input_received(&sid);
        self.work_note("posttooluse", &req);
        if let Some(tool_name) = req.get("tool_name").and_then(Value::as_str) {
            self.state.lock().add_entry(&format!("+ {tool_name}"), None);
            self.ensure_session(&req);
            self.state.lock().note_tool(&sid, tool_name);
        }
        self.push_heartbeat(false).await;
        Ok(ok_obj(json!({"ok": true})))
    }

    /// `_ipc_notification(req)`.
    pub async fn ipc_notification(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        // Only a session blocked on the user (a permission prompt, a question
        // dialog) marks the session, so heartbeats carry waiting>0 — the
        // firmware's attention animation + LED pulse. An idle reminder is
        // news, not a request: it must not take over a live conversation.
        let kind = req.get("notification_type").and_then(Value::as_str);
        let waits = eve_state::notification_waits(kind);
        if waits {
            let sid = req.get("session_id").map(py_str).unwrap_or_default();
            self.state.lock().needs_input(&sid);
        }
        self.work_note("notification", &req);
        let msg = req.get("message").and_then(Value::as_str);
        if let Some(m) = msg.map(str::trim).filter(|m| !m.is_empty()) {
            self.state.lock().add_entry(&format!("! {m}"), None);
        }
        tracing::info!(
            "notification: session={} type={} → {}",
            sid8(&req),
            kind.filter(|k| !k.is_empty()).unwrap_or("?"),
            if waits { "attention" } else { "not waiting" }
        );
        self.push_heartbeat(false).await;
        if let Some(inlet) = self.relay() {
            let kind = kind.unwrap_or("").to_string();
            let msg = msg.unwrap_or("").to_string();
            self.spawn("relay-notification", async move { inlet.relay_notification(&kind, &msg, waits).await });
        }
        Ok(ok_obj(json!({"ok": true})))
    }

    /// `self._telegram` when the owner said "claude on" (`inlet is not None and inlet.claude`).
    pub(crate) fn relay(&self) -> Option<Arc<dyn Inlet>> {
        self.st.lock().telegram.clone().filter(|i| i.claude())
    }

    /// `getattr(self.state.sessions.get(session_id), "cwd", None) or ""`.
    fn session_cwd(&self, session_id: &str) -> String {
        self.state.lock().sessions.get(session_id).and_then(|s| s.cwd.clone()).unwrap_or_default()
    }
}

fn unknown_evt(evt: &Value) -> Obj {
    ok_obj(json!({"ok": false, "error": format!("unknown evt: {}", py_repr(evt))}))
}

fn py_bool(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_is_pythons() {
        assert_eq!(py_repr(&json!("nope")), "'nope'");
        assert_eq!(py_repr(&Value::Null), "None");
        assert_eq!(py_repr(&json!(["pretooluse"])), "['pretooluse']");
        assert_eq!(py_repr(&json!({"a": [1, 2.5, true]})), "{'a': [1, 2.5, True]}");
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
        assert_eq!(py_str(&json!(3.0)), "3.0");
        assert_eq!(py_str(&json!("x")), "x");
    }

    #[test]
    fn float_is_pythons() {
        assert_eq!(py_float(&json!("1.5")).unwrap(), 1.5);
        assert_eq!(py_float(&json!(" 2 ")).unwrap(), 2.0);
        assert_eq!(py_float(&json!(true)).unwrap(), 1.0);
        assert_eq!(py_float(&json!("-inf")).unwrap(), f64::NEG_INFINITY);
        let e = py_float(&json!("fast")).unwrap_err();
        assert_eq!(eve_ipc::error_type_name(&e), "ValueError");
        assert_eq!(e.to_string(), "could not convert string to float: 'fast'");
        assert_eq!(eve_ipc::error_type_name(&py_float(&json!([1])).unwrap_err()), "TypeError");
    }

    #[test]
    fn truthiness_and_rounding() {
        for v in [json!(null), json!(0), json!(0.0), json!(""), json!([]), json!({}), json!(false)] {
            assert!(!truthy(&v), "{v}");
        }
        assert!(truthy(&json!("0")) && truthy(&json!(1)) && truthy(&json!([0])));
        assert_eq!(round3(1.23456), 1.235);
        assert_eq!(round3(0.0005), 0.001); // 0.0005 is just above the tie in binary
        assert_eq!(round3(2.675), 2.675);
        let mut m = Map::new();
        m.insert("session_id".into(), json!("0123456789"));
        assert_eq!(sid8(&m), "01234567");
        assert_eq!(sid8(&Map::new()), "?");
    }
}
