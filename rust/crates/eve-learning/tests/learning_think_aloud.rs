//! Port of bridge/tests/test_learning_think_aloud.py: the daemon hook behind the browser toggle, the standalone
//! server's answer, and spoken ideas that append. The shared actions and the listening instructions are unit tests
//! in src/lib.rs and src/think_aloud.rs. No daemon, microphone, browser or network: a fake listener stands in.

mod common;

use std::sync::Arc;

use common::*;
use eve_learning::server::{start, LearningApp, Listener, StartOptions};
use eve_learning::{run_lesson, LearningError, LessonRequest, ROBOT_APP_NOT_RUNNING};
use serde_json::{json, Map, Value};

fn working(app: &LearningApp) -> Map<String, Value> {
    create(app, "learn", "Algebra")
}

fn with_listener(dir: &std::path::Path, listener: Arc<FakeListener>) -> LearningApp {
    LearningApp::new(StartOptions { listener: Some(listener as Arc<dyn Listener>), ..opts(dir, true) }).unwrap()
}

fn voice(app: &LearningApp, action: &str) -> Map<String, Value> {
    app.voice(&LessonRequest { action: action.into(), ..LessonRequest::default() }).unwrap()
}

#[test]
fn test_without_the_daemon_listening_says_the_robot_app_is_not_running() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    working(&app);
    let call = |r: &LessonRequest| -> Result<Map<String, Value>, LearningError> { app.voice(r) };
    for action in ["listen", "stop-listening"] {
        let out = run_lesson(Some(&call), &json!({"action": action})).unwrap();
        assert!(out["ok"] == false && out["reason"] == ROBOT_APP_NOT_RUNNING);
        assert!(out["available"] == false && out["state"] == "off");
    }
}

#[test]
fn test_listening_needs_an_open_lesson_with_a_problem_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = FakeListener::new();
    let app = with_listener(tmp.path(), listener.clone());
    let out = voice(&app, "listen");
    assert!(out["ok"] == false && out["reason"].as_str().unwrap().contains("Open a lesson first"));
    dispatch(&app, json!({"action": "create", "mode": "help", "topic": "Mine", "level": ""})).unwrap();
    let out = voice(&app, "listen");
    assert!(out["ok"] == false && out["reason"].as_str().unwrap().contains("problem ready"));
    assert!(listener.calls().is_empty());

    let s = working(&app);
    let id = s["id"].as_str().unwrap().to_string();
    let out = voice(&app, "listen");
    assert!(out["ok"] == true && out["state"] == "starting" && out["lesson_id"] == json!(id));
    assert_eq!(listener.calls(), vec![("start".to_string(), Some(id.clone()))]);
    assert_eq!(voice(&app, "stop-listening")["state"], json!("off"));

    dispatch(&app, json!({"action": "end"})).unwrap();
    let out = voice(&app, "listen");
    assert!(out["ok"] == false && out["reason"].as_str().unwrap().contains("Resume"));
    assert_eq!(listener.calls(), vec![("start".to_string(), Some(id)), ("stop".to_string(), None)]);
}

#[test]
fn test_a_listener_is_given_the_lesson_the_browser_names() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = FakeListener::new();
    let app = with_listener(tmp.path(), listener.clone());
    let first = working(&app);
    working(&app); // the second lesson is the active one now
    let id = first["id"].as_str().unwrap().to_string();
    assert_eq!(app.listen(Some(&id))["ok"], json!(true));
    assert_eq!(listener.calls(), vec![("start".to_string(), Some(id.clone()))]);
    assert_eq!(app.active(), Some(id));
    assert_eq!(app.listen(Some("no-such-lesson"))["ok"], json!(false));
}

/// A listener whose every call fails: the app answers instead of raising.
struct BrokenListener;

impl Listener for BrokenListener {
    fn start(&self, _: &Map<String, Value>) -> anyhow::Result<Map<String, Value>> {
        anyhow::bail!("mic gone")
    }
    fn stop(&self) -> anyhow::Result<Map<String, Value>> {
        anyhow::bail!("mic gone")
    }
    fn status(&self) -> anyhow::Result<Map<String, Value>> {
        anyhow::bail!("mic gone")
    }
}

#[test]
fn a_failing_listener_is_answered_not_raised() {
    let tmp = tempfile::tempdir().unwrap();
    let app = LearningApp::new(StartOptions { listener: Some(Arc::new(BrokenListener)), ..opts(tmp.path(), true) }).unwrap();
    working(&app);
    let out = app.listen(None);
    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["reason"], json!("eve could not change listening. Try again."));
    assert_eq!(out["available"], json!(false));
    let status = app.listen_status();
    assert_eq!(status["reason"], json!("eve could not say whether it is listening."));
}

#[test]
fn test_spoken_ideas_append_and_never_erase_typed_ideas() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    let s = working(&app);
    let id = s["id"].as_str().unwrap().to_string();
    let s = dispatch(&app, json!({"action": "save", "id": id, "revision": s["revision"], "work": {"ideas": "I typed this"}})).unwrap();
    let typed = s["revision"].clone();
    assert!(app.append_spoken(&id, "  I think   I subtract three ").unwrap());
    assert!(app.append_spoken(&id, "then divide by two").unwrap());
    assert!(!app.append_spoken(&id, "   ").unwrap());
    assert_eq!(app.store.get(&id).unwrap()["ideas"], json!("I typed this\nI think I subtract three\nthen divide by two"));

    // The page never saw the spoken lines and saves more typing on its old revision: nothing is lost.
    let out = dispatch(&app, json!({"action": "save", "id": id, "revision": typed, "ideas_base": typed,
                                    "work": {"ideas": "I typed this, and more"}}))
    .unwrap();
    assert_eq!(out["ideas"], json!("I typed this, and more\nI think I subtract three\nthen divide by two"));
    // A page that has loaded them may still delete one.
    let out = dispatch(&app, json!({"action": "save", "id": id, "revision": out["revision"], "ideas_base": out["revision"],
                                    "work": {"ideas": "just my own words"}}))
    .unwrap();
    assert_eq!(out["ideas"], json!("just my own words"));
}

#[test]
fn test_a_stale_save_is_still_refused_after_a_real_change() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    let s = working(&app);
    let id = s["id"].as_str().unwrap().to_string();
    let base = s["revision"].clone();
    app.append_spoken(&id, "I think x is four").unwrap();
    dispatch(&app, json!({"action": "hint"})).unwrap();
    let e = err_text(dispatch(&app, json!({"action": "save", "id": id, "revision": base, "work": {"ideas": "old"}})));
    assert!(e.contains("another window"));
}

#[test]
fn test_spoken_ideas_do_not_reach_an_ended_lesson() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    let s = working(&app);
    let id = s["id"].as_str().unwrap().to_string();
    dispatch(&app, json!({"action": "end"})).unwrap();
    assert!(!app.append_spoken(&id, "hello").unwrap());
    assert_eq!(app.store.get(&id).unwrap()["ideas"], json!(""));
    assert!(matches!(app.append_spoken("gone", "hello"), Err(LearningError::NotFound(_))));
}

#[test]
fn test_the_browser_toggle_reaches_the_daemon_hook() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = FakeListener::new();
    let server = start(StartOptions { listener: Some(listener.clone() as Arc<dyn Listener>), ..opts(tmp.path(), true) }).unwrap();
    let cfg = get_json(&server, "/api/config");
    assert_eq!(cfg["listen_available"], json!(true));
    let token = cfg["token"].as_str().unwrap();
    let lesson = post_ok(&server, token, &json!({"action": "create", "mode": "learn", "topic": "Algebra"}));
    let lesson = post_ok(&server, token, &json!({"action": "generate", "id": lesson["id"]}));
    let id = lesson["id"].as_str().unwrap().to_string();

    // No page token: refused before the hook.
    let r = post(&server, &[], &json!({"action": "listen", "id": id}));
    assert!(r.status == 403 && listener.calls().is_empty());

    let out = post_ok(&server, token, &json!({"action": "listen", "id": id}));
    assert_eq!(out["ok"], json!(true));
    assert_eq!(listener.calls(), vec![("start".to_string(), Some(id.clone()))]);
    assert_eq!(get_json(&server, "/api/listening"), json!({"available": true, "state": "starting", "lesson_id": id, "reason": ""}));
    let out = post_ok(&server, token, &json!({"action": "stop-listening", "id": id}));
    assert!(out["ok"] == true && out["state"] == "off");
    assert_eq!(listener.calls().last().unwrap().0, "stop");
    // Toggling saves nothing.
    assert_eq!(get_json(&server, &format!("/api/lessons/{id}"))["revision"], lesson["revision"]);
    server.shutdown();
}

#[test]
fn test_a_standalone_server_answers_the_toggle_with_robot_app_not_running() {
    let tmp = tempfile::tempdir().unwrap();
    let server = start(opts(tmp.path(), true)).unwrap();
    let cfg = get_json(&server, "/api/config");
    assert_eq!(cfg["listen_available"], json!(false));
    let status = get_json(&server, "/api/listening");
    assert!(status["available"] == false && status["state"] == "off" && status["reason"] == ROBOT_APP_NOT_RUNNING);
    let token = cfg["token"].as_str().unwrap();
    let lesson = post_ok(&server, token, &json!({"action": "create", "mode": "learn", "topic": "Algebra"}));
    for action in ["listen", "stop-listening"] {
        let r = post(&server, &[("X-Eve-Token", token)], &json!({"action": action, "id": lesson["id"]}));
        assert_eq!(r.status, 503);
        assert_eq!(r.json()["error"], json!(ROBOT_APP_NOT_RUNNING));
    }
    server.shutdown();
}

#[test]
fn test_a_refused_listen_from_the_browser_is_a_409_with_the_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let server = start(StartOptions { listener: Some(FakeListener::new() as Arc<dyn Listener>), ..opts(tmp.path(), true) }).unwrap();
    let cfg = get_json(&server, "/api/config");
    let token = cfg["token"].as_str().unwrap();
    let lesson = post_ok(&server, token, &json!({"action": "create", "mode": "help", "topic": "Mine"}));
    let r = post(&server, &[("X-Eve-Token", token)], &json!({"action": "listen", "id": lesson["id"]}));
    assert_eq!(r.status, 409);
    assert!(r.json()["error"].as_str().unwrap().contains("problem ready"));
    server.shutdown();
}
