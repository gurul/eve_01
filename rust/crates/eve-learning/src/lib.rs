//! Eve's local learning workspace, and the one lesson request shape shared by the voice tool and
//! `eve-bridge lesson`. A module-for-module port of `bridge/src/eve_bridge/learning/`:
//!
//! | Python                    | Rust                                   |
//! |---------------------------|----------------------------------------|
//! | learning/__init__.py      | this file                              |
//! | learning/__main__.py      | src/main.rs (the `eve-learning` bin)   |
//! | learning/search.py        | [`search`]                             |
//! | learning/server.py        | [`server`]                             |
//! | learning/store.py         | [`store`]                              |
//! | learning/think_aloud.py   | [`think_aloud`]                        |
//! | learning/tutor.py         | [`tutor`]                              |
//!
//! Python raises `ValueError` with a learner-safe sentence, `KeyError` for a missing lesson and `TypeError` for
//! a malformed request; [`LearningError`] keeps the three apart because the HTTP handler and [`run_lesson`]
//! answer each differently.

pub mod py;
pub mod search;
pub mod server;
pub mod store;
pub mod think_aloud;
pub mod tutor;

use serde_json::{Map, Value};

pub use server::{start, LearningApp, LearningServer, Listener, StartOptions};
pub use store::Store;
pub use think_aloud::LISTEN_ACTIONS;

/// What a lesson operation can fail with.
#[derive(Debug, thiserror::Error)]
pub enum LearningError {
    /// Python's `ValueError`: a sentence for the learner (HTTP 400).
    #[error("{0}")]
    Value(String),
    /// Python's `KeyError`: the lesson is not there (HTTP 404).
    #[error("{0}")]
    NotFound(String),
    /// Python's `TypeError`: a malformed request (HTTP 400; not caught by `run_lesson`).
    #[error("{0}")]
    Type(String),
    /// Anything else (HTTP 500, logged).
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl LearningError {
    pub fn value(msg: impl Into<String>) -> Self {
        LearningError::Value(msg.into())
    }
}

pub const LESSON_ACTIONS: [&str; 11] =
    ["open", "start", "ideas", "hint", "check", "step", "status", "recap", "end", LISTEN_ACTIONS[0], LISTEN_ACTIONS[1]];
pub const LESSON_MODES: [&str; 2] = ["learn", "help"];
/// Actions that can call the tutor model, so they can take a long time.
pub const TUTOR_ACTIONS: [&str; 5] = ["start", "hint", "check", "step", "recap"];
pub const UNAVAILABLE: &str = "The learning workspace is unavailable. It is off (EVE_LEARNING=0) or its server \
did not start; check the bridge log.";
pub const STALE_LESSON: &str = "That saved lesson is no longer there. Open the learning window and choose a lesson.";
/// The learning server runs without the daemon (`eve-bridge learning`): nothing owns a microphone.
pub const ROBOT_APP_NOT_RUNNING: &str = "Think out loud needs the eve robot app, and it is not running. \
Start it with `eve-bridge daemon` or the launchd service.";
const LIMITS: [(&str, usize); 3] = [("topic", 200), ("level", 100), ("text", 20000)];

/// A validated lesson request: `{"action", "mode"?, "topic"?, "level"?, "text"?}`. The keys left out are the ones
/// the request did not carry, so `LearningApp::voice` uses its defaults for them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LessonRequest {
    pub action: String,
    pub mode: Option<String>,
    pub topic: Option<String>,
    pub level: Option<String>,
    pub text: Option<String>,
}

impl LessonRequest {
    /// The request as Python's dict, keys in its order.
    pub fn to_map(&self) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert("action".into(), Value::String(self.action.clone()));
        for (k, v) in [("mode", &self.mode), ("topic", &self.topic), ("level", &self.level), ("text", &self.text)] {
            if let Some(v) = v {
                out.insert(k.into(), Value::String(v.clone()));
            }
        }
        out
    }
}

/// Validate and normalize a lesson request. `Err` is a learner-safe sentence (Python's ValueError).
///
/// Unknown keys are dropped, never forwarded.
pub fn lesson_request(args: &Value) -> Result<LessonRequest, String> {
    let Value::Object(args) = args else { return Err("A lesson request needs an action.".into()) };
    let action = py::py_strip(&py::str_or_empty(args.get("action"))).to_lowercase();
    if !LESSON_ACTIONS.contains(&action.as_str()) {
        return Err(format!("Unknown lesson action {}. Use one of: {}.", py::repr_str(&action), LESSON_ACTIONS.join(", ")));
    }
    let mut out = LessonRequest { action, ..LessonRequest::default() };
    let mode = py::py_strip(&py::str_or_empty(args.get("mode"))).to_lowercase();
    if !mode.is_empty() && !LESSON_MODES.contains(&mode.as_str()) {
        return Err("Mode must be learn or help.".into());
    }
    if !mode.is_empty() {
        out.mode = Some(mode);
    }
    for (key, limit) in LIMITS {
        if let Some(value) = args.get(key).filter(|v| !v.is_null()) {
            let value = py::take_chars(&py::py_str(value), limit);
            match key {
                "topic" => out.topic = Some(value),
                "level" => out.level = Some(value),
                _ => out.text = Some(value),
            }
        }
    }
    Ok(out)
}

/// The voice entry point `run_lesson` calls: `LearningApp::voice`, or a test's fake.
pub type VoiceFn<'a> = &'a (dyn Fn(&LessonRequest) -> Result<Map<String, Value>, LearningError> + Send + Sync);

fn refusal(reason: &str) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(false));
    out.insert("reason".into(), Value::String(reason.to_string()));
    out
}

/// Run one lesson action through `LearningApp::voice`. Blocking: call it from `spawn_blocking`.
///
/// User-level problems come back as `{"ok": false, "reason": ...}`. Unexpected errors (Python's TypeError and
/// anything else) propagate.
pub fn run_lesson(voice: Option<VoiceFn<'_>>, args: &Value) -> Result<Map<String, Value>, LearningError> {
    let Some(voice) = voice else { return Ok(refusal(UNAVAILABLE)) };
    let request = match lesson_request(args) {
        Ok(r) => r,
        Err(reason) => return Ok(refusal(&reason)),
    };
    match voice(&request) {
        Ok(out) => Ok(out),
        Err(LearningError::NotFound(_)) => Ok(refusal(STALE_LESSON)),
        Err(LearningError::Value(reason)) => Ok(refusal(&reason)),
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_listen_and_stop_listening_are_shared_lesson_actions() {
        assert!(LESSON_ACTIONS.contains(&"listen") && LESSON_ACTIONS.contains(&"stop-listening"));
        assert_eq!(lesson_request(&json!({"action": " Stop-Listening "})).unwrap().to_map(), *json!({"action": "stop-listening"}).as_object().unwrap());
        assert_eq!(lesson_request(&json!({"action": "listen", "bogus": 1})).unwrap().to_map(), *json!({"action": "listen"}).as_object().unwrap());
    }

    #[test]
    fn test_lesson_drops_unknown_arguments() {
        let called = parking_lot::Mutex::new(Vec::new());
        let voice = |r: &LessonRequest| {
            called.lock().push(r.to_map());
            Ok(json!({"ok": true, "answer": "Try the ones place."}).as_object().unwrap().clone())
        };
        let out = run_lesson(Some(&voice), &json!({"action": "step", "bogus": 1})).unwrap();
        assert_eq!(*called.lock(), vec![json!({"action": "step"}).as_object().unwrap().clone()]);
        assert_eq!(out["ok"], json!(true));
    }

    #[test]
    fn test_lesson_with_unreadable_arguments_does_not_open_the_whiteboard() {
        // The voice agent hands an argument string that is not a JSON object over as {} (no action).
        let called = parking_lot::Mutex::new(0);
        let voice = |_: &LessonRequest| {
            *called.lock() += 1;
            Ok(Map::new())
        };
        for arguments in [json!({}), json!({"action": null})] {
            let out = run_lesson(Some(&voice), &arguments).unwrap();
            assert_eq!(out["ok"], json!(false));
            assert!(out["reason"].as_str().unwrap().contains("Unknown lesson action"));
        }
        assert_eq!(*called.lock(), 0);
        let out = run_lesson(Some(&voice), &json!([1, 2])).unwrap();
        assert_eq!(out["reason"], json!("A lesson request needs an action."));
    }

    #[test]
    fn test_lesson_stale_lesson_is_actionable() {
        let voice = |_: &LessonRequest| Err(LearningError::NotFound("gone".into()));
        let out = run_lesson(Some(&voice), &json!({"action": "hint"})).unwrap();
        assert_eq!(out["ok"], json!(false));
        let reason = out["reason"].as_str().unwrap();
        assert!(reason.contains("no longer there") && !reason.contains("lesson failed"));
    }

    #[test]
    fn requests_are_normalized_and_bounded() {
        assert_eq!(run_lesson(None, &json!({"action": "hint"})).unwrap()["reason"], json!(UNAVAILABLE));
        let err = lesson_request(&json!({"action": "Dance"})).unwrap_err();
        assert_eq!(
            err,
            "Unknown lesson action 'dance'. Use one of: open, start, ideas, hint, check, step, status, recap, end, listen, stop-listening."
        );
        assert_eq!(lesson_request(&json!({"action": "start", "mode": "teach"})).unwrap_err(), "Mode must be learn or help.");
        let r = lesson_request(&json!({"action": "START", "mode": " Help ", "topic": "t".repeat(300), "level": 7, "text": null}))
            .unwrap();
        assert_eq!(r.action, "start");
        assert_eq!(r.mode.as_deref(), Some("help"));
        assert_eq!(r.topic.unwrap().len(), 200);
        assert_eq!(r.level.as_deref(), Some("7"));
        assert_eq!(r.text, None);
        let voice = |_: &LessonRequest| Err(LearningError::Value("Open a lesson first.".into()));
        assert_eq!(run_lesson(Some(&voice), &json!({"action": "hint"})).unwrap()["reason"], json!("Open a lesson first."));
        let voice = |_: &LessonRequest| Err(LearningError::Type("boom".into()));
        assert!(run_lesson(Some(&voice), &json!({"action": "hint"})).is_err());
    }
}
