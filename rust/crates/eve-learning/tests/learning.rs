//! Port of bridge/tests/test_learning.py: lesson workflow, durable work, request boundaries, and live-response
//! contracts. Python's autouse fixture cleared the tutor's environment; here every context starts empty.

mod common;

use std::sync::Arc;

use base64::Engine as _;
use common::*;
use eve_learning::server::{main_with, mark_count, start, validate_work, LearningApp, StartOptions, MAX_BOARD};
use eve_learning::store::Store;
use eve_learning::tutor::{live_settings, validate, DemoTutor, FnTutor, LiveTutor, PostError, Tutor};
use eve_learning::LearningError;
use serde_json::{json, Map, Value};

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn test_learn_hints_steps_complete_recap_and_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    let s = create(&app, "learn", "Algebra");
    assert_eq!(s["problem"], json!("Solve 2x + 3 = 11."));
    let s = dispatch(&app, json!({"action": "hint"})).unwrap();
    let last = |s: &Map<String, Value>| s["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last(&s)["step"], json!(""));
    for (index, expected) in ["2x = 11 - 3", "2x = 8", "x = 4"].iter().enumerate() {
        let s = dispatch(&app, json!({"action": "step"})).unwrap();
        assert_eq!(last(&s)["step"], json!(expected));
        assert_eq!(s["events"].as_array().unwrap().iter().filter(|e| e["action"] == "step").count(), index + 1);
        assert_eq!(s["stage"] == "complete", index == 2);
    }
    assert!(err_text(dispatch(&app, json!({"action": "step"}))).contains("complete"));
    let s = dispatch(&app, json!({"action": "recap"})).unwrap();
    assert!(last(&s)["feedback"].as_str().unwrap().contains("why"));
    let loaded = Store::new(tmp.path()).unwrap().get(s["id"].as_str().unwrap()).unwrap();
    assert_eq!(loaded, s);
    assert_eq!(Store::new(tmp.path()).unwrap().history(s["id"].as_str().unwrap()).unwrap().len() as i64, s["revision"].as_i64().unwrap());
    let dashboard: Value = serde_json::from_str(&std::fs::read_to_string(tmp.path().join("dashboard.json")).unwrap()).unwrap();
    assert_eq!(dashboard["completed"], json!(1));
}

#[test]
fn test_help_requires_confirmation_and_ideas() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    create(&app, "help", "Algebra");
    assert!(err_text(dispatch(&app, json!({"action": "step"}))).contains("Confirm"));
    dispatch(&app, json!({"action": "save", "work": {"problem": "Solve 2x + 3 = 11."}})).unwrap();
    let s = dispatch(&app, json!({"action": "recognize"})).unwrap();
    assert_eq!(s["stage"], json!("confirm"));
    dispatch(&app, json!({"action": "confirm"})).unwrap();
    assert!(err_text(dispatch(&app, json!({"action": "step"}))).contains("ideas"));
    dispatch(&app, json!({"action": "save", "work": {"ideas": "2x = 8"}})).unwrap();
    let s = dispatch(&app, json!({"action": "step"})).unwrap();
    assert_eq!(s["events"].as_array().unwrap().last().unwrap()["step"], json!("x = 4"));
    assert_eq!(s["stage"], json!("complete"));
}

#[test]
fn test_stuck_is_a_valid_start_and_demo_does_not_fake_ocr() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    create(&app, "help", "Algebra");
    dispatch(&app, json!({"action": "save", "work": {"problem": "A different problem"}})).unwrap();
    let s = dispatch(&app, json!({"action": "recognize"})).unwrap();
    assert!(s["events"].as_array().unwrap().last().unwrap()["feedback"].as_str().unwrap().contains("cannot read"));
    dispatch(&app, json!({"action": "confirm"})).unwrap();
    dispatch(&app, json!({"action": "save", "work": {"stuck": true}})).unwrap();
    let s = dispatch(&app, json!({"action": "step"})).unwrap();
    let last = s["events"].as_array().unwrap().last().unwrap();
    assert_eq!(last["step"], json!(""));
    assert_eq!(last["status"], json!("clarify"));
}

#[test]
fn test_work_snapshots_conflicts_end_resume_and_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    let s = create(&app, "learn", "Algebra");
    let strokes = json!([{"tool": "pen", "points": [[0.1, 0.2], [0.3, 0.4]]}]);
    let s = dispatch(&app, json!({"action": "save", "revision": s["revision"], "work": {"ideas": "my first idea", "strokes": strokes}})).unwrap();
    let stale = s["revision"].as_i64().unwrap() - 1;
    assert!(err_text(dispatch(&app, json!({"action": "save", "revision": stale, "work": {"ideas": "overwrite"}}))).contains("another window"));
    dispatch(&app, json!({"action": "save", "work": {"ideas": "a better idea", "strokes": []}})).unwrap();
    assert!(app.store.history(s["id"].as_str().unwrap()).unwrap().iter().any(|r| r["strokes"] == strokes));
    dispatch(&app, json!({"action": "end"})).unwrap();
    assert!(err_text(dispatch(&app, json!({"action": "save", "work": {"ideas": "oops"}}))).contains("Resume"));
    assert_eq!(dispatch(&app, json!({"action": "resume"})).unwrap()["stage"], json!("working"));
    assert_eq!(dispatch(&app, json!({"action": "delete"})).unwrap()["deleted"], json!(true));
    assert!(app.store.list().unwrap().is_empty());
}

#[test]
fn test_failed_tutor_leaves_saved_work_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    let s = create(&app, "learn", "Algebra");
    dispatch(&app, json!({"action": "save", "work": {"ideas": "keep this"}})).unwrap();
    app.set_tutor(Arc::new(FnTutor { demo: true, f: |_: &str, _: &Map<String, Value>| Err(LearningError::value("network failure")) }));
    assert!(err_text(dispatch(&app, json!({"action": "step"}))).contains("network"));
    let id = s["id"].as_str().unwrap();
    assert_eq!(app.store.get(id).unwrap()["ideas"], json!("keep this"));
    assert!(!app.store.get(id).unwrap()["events"].as_array().unwrap().iter().any(|e| e["action"] == "step"));
}

#[test]
fn test_invalid_work_rejected() {
    let nan_point = {
        // Python's json.loads reads NaN; serde_json cannot hold one, so the stroke carries a value outside 0..1 of
        // the same kind (a number the range check refuses) and a non-number, both refused as NaN is.
        json!({"strokes": [{"tool": "pen", "points": [["NaN", 0.5]]}]})
    };
    for work in [
        json!({"board_image": "https://example.org/private"}),
        json!({"source_image": "data:image/svg+xml;base64,abcd"}),
        nan_point,
        json!({"strokes": [{"tool": "pen", "points": [[1.5, 0.5]]}]}),
        json!({"strokes": [{"tool": "pen", "points": [[true, 0.5]]}]}),
        json!({"strokes": [{"tool": "script", "points": []}]}),
        json!({"strokes": [{"tool": "text", "points": [[0.5, 0.5]], "text": "   "}]}),
        json!({"strokes": [{"tool": "text", "points": [[0.5, 0.5]], "text": "x".repeat(501)}]}),
        json!({"strokes": [{"tool": "text", "points": [[0.1, 0.1], [0.2, 0.2]], "text": "two anchors"}]}),
        json!({"strokes": [{"tool": "text", "points": [[0.5, 0.5]], "text": 42}]}),
        json!({"strokes": [{"tool": "text", "points": [[0.5, 0.5]]}]}),
        json!({"strokes": [{"tool": "pen", "points": [[0.5, 0.5]], "text": "pens carry no text"}]}),
        json!({"ideas": "x".repeat(20001)}),
        json!({"source_image": format!("data:image/png;base64,{}", b64(b"GIF89a-not-png"))}),
        json!({"source_image": "data:image/png;base64,iVBORw0KGgo*"}),
        json!({"source_image": "data:image/png;base64,abc"}),
    ] {
        assert!(matches!(validate_work(&work), Err(LearningError::Value(_))), "{work}");
    }
    let png = format!("data:image/png;base64,{}", b64(b"\x89PNG\r\n\x1a\nrest"));
    assert_eq!(validate_work(&json!({"source_image": png, "stuck": 1})).unwrap(), *json!({"stuck": true, "source_image": png}).as_object().unwrap());
}

#[test]
fn test_text_boxes_are_strokes_that_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    let s = create(&app, "learn", "Algebra");
    let strokes = json!([{"tool": "text", "points": [[0.25, 0.1]], "text": "I think 2x = 8\nso x = 4"},
                         {"tool": "pen", "points": [[0.1, 0.2], [0.3, 0.4]]}]);
    assert_eq!(validate_work(&json!({"strokes": strokes})).unwrap()["strokes"], strokes);
    let s = dispatch(&app, json!({"action": "save", "revision": s["revision"], "work": {"strokes": strokes}})).unwrap();
    let id = s["id"].as_str().unwrap();
    assert_eq!(app.store.get(id).unwrap()["strokes"], strokes);
    assert!(app.store.history(id).unwrap().iter().any(|r| r["strokes"] == strokes));
}

/// A tldraw document the way the whiteboard saves it: a store of records and its schema.
fn board(shape_ids: &[&str]) -> Value {
    let mut store = obj(json!({"document:document": {"typeName": "document", "id": "document:document"},
                               "page:page": {"typeName": "page", "id": "page:page"}}));
    for i in shape_ids {
        store.insert(format!("shape:{i}"), json!({"typeName": "shape", "id": format!("shape:{i}"), "type": "draw"}));
    }
    json!({"store": store, "schema": {"schemaVersion": 2, "sequences": {}}})
}

#[test]
fn test_tldraw_board_round_trips_and_keeps_every_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    let s = create(&app, "learn", "Algebra");
    assert_eq!(s["board"], Value::Null);
    let s = dispatch(&app, json!({"action": "save", "revision": s["revision"], "work": {"board": board(&["a", "b"])}})).unwrap();
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(app.store.get(&id).unwrap()["board"], board(&["a", "b"]));
    let s = dispatch(&app, json!({"action": "save", "revision": s["revision"], "work": {"board": board(&["a"])}})).unwrap();
    let boards: Vec<Value> = app.store.history(&id).unwrap().iter().map(|r| r["board"].clone()).collect();
    assert_eq!(boards[boards.len() - 2..], [board(&["a", "b"]), board(&["a"])]);
    // A save that does not mention the board (typed ideas only) leaves it alone; an explicit null clears it.
    let s = dispatch(&app, json!({"action": "save", "revision": s["revision"], "work": {"ideas": "still thinking"}})).unwrap();
    assert_eq!(s["board"], board(&["a"]));
    let s = dispatch(&app, json!({"action": "save", "revision": s["revision"], "work": {"board": null}})).unwrap();
    assert_eq!(s["board"], Value::Null);
}

#[test]
fn test_invalid_board_rejected() {
    for bad in [
        json!([]),
        json!("board"),
        json!(7),
        json!({}),
        json!({"store": {}}),
        json!({"schema": {}}),
        json!({"store": [], "schema": {}}),
        json!({"store": {}, "schema": []}),
        json!({"store": {"shape:a": "not a record"}, "schema": {}}),
        json!({"store": {"shape:a": {"typeName": "shape", "pad": "x".repeat(MAX_BOARD)}}, "schema": {}}),
    ] {
        let e = validate_work(&json!({"board": bad})).unwrap_err().to_string();
        assert!(e.contains("hiteboard"), "{e}");
    }
}

#[test]
fn test_marks_count_tldraw_shapes_and_strokes_from_before_tldraw() {
    assert_eq!(mark_count(&Map::new()), 0);
    assert_eq!(mark_count(&obj(json!({"board": null, "strokes": []}))), 0);
    assert_eq!(mark_count(&obj(json!({"board": board(&[])}))), 0); // the document and page records are not marks
    assert_eq!(mark_count(&obj(json!({"board": board(&["a", "b"])}))), 2);
    let old = json!([{"tool": "pen", "points": [[0.1, 0.2]]}]);
    assert_eq!(mark_count(&obj(json!({"strokes": old}))), 1);
    assert_eq!(mark_count(&obj(json!({"board": board(&["a"]), "strokes": old}))), 2);
}

#[test]
fn test_help_path_counts_a_new_mark_on_the_board_as_an_attempt() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    let s = create(&app, "help", "Algebra");
    assert!(err_text(dispatch(&app, json!({"action": "recognize"}))).contains("Write a problem"));
    // The problem written on the board is enough for eve to try reading it, and is not yet an attempt.
    dispatch(&app, json!({"action": "save", "revision": s["revision"], "work": {"board": board(&["problem"])}})).unwrap();
    assert_eq!(dispatch(&app, json!({"action": "recognize"})).unwrap()["stage"], json!("confirm"));
    // The offline demo cannot read a board, so the learner types what it says before confirming.
    dispatch(&app, json!({"action": "save", "work": {"problem": "Solve 2x + 3 = 11."}})).unwrap();
    let s = dispatch(&app, json!({"action": "confirm"})).unwrap();
    assert_eq!(s["problem_stroke_count"], json!(1));
    assert!(err_text(dispatch(&app, json!({"action": "hint"}))).contains("ideas first"));
    dispatch(&app, json!({"action": "save", "work": {"board": board(&["problem", "attempt"])}})).unwrap();
    let s = dispatch(&app, json!({"action": "hint"})).unwrap();
    assert_eq!(s["events"].as_array().unwrap().last().unwrap()["action"], json!("hint"));
}

fn demo_server(dir: &std::path::Path) -> eve_learning::LearningServer {
    start(opts(dir, true)).unwrap()
}

#[test]
fn test_page_policy_is_same_origin_with_a_fresh_style_nonce_and_never_unsafe_inline() {
    let tmp = tempfile::tempdir().unwrap();
    let server = demo_server(tmp.path());
    let mut seen = std::collections::HashSet::new();
    for _ in 0..2 {
        let r = get(&server, "/");
        let csp = r.header("content-security-policy");
        let html = String::from_utf8(r.body.clone()).unwrap();
        let policy: std::collections::HashMap<&str, &str> =
            csp.split(';').map(|p| p.trim().split_once(' ').unwrap()).collect();
        assert!(policy["default-src"] == "'self'" && policy["script-src"] == "'self'");
        assert_eq!(policy["img-src"], "'self' data: blob:");
        assert!(!csp.contains("unsafe") && !csp.contains("http") && !csp.contains('*'));
        let sources: Vec<&str> = policy["style-src"].split_whitespace().collect();
        assert!(sources[0] == "'self'" && sources.len() == 2 && sources[1].starts_with("'nonce-"));
        let nonce = &sources[1]["'nonce-".len()..sources[1].len() - 1];
        assert!(nonce.len() >= 16 && html.contains(&format!("nonce=\"{nonce}\"")) && !html.contains("__EVE_NONCE__"));
        seen.insert(nonce.to_string());
    }
    assert_eq!(seen.len(), 2, "the nonce must change with every page load");
    assert!(!get(&server, "/app.js").header("content-security-policy").contains("nonce"));
    server.shutdown();
}

#[test]
fn test_whiteboard_bundle_is_served_same_origin_and_traversal_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let server = demo_server(tmp.path());
    let html = String::from_utf8(get(&server, "/").body).unwrap();
    assert!(html.contains("src=\"/canvas/canvas.js\"") && html.contains("href=\"/canvas/canvas.css\""));
    for (path, mime) in [
        ("/canvas/canvas.js", "text/javascript"),
        ("/canvas/canvas.css", "text/css"),
        ("/canvas/assets/translations/fr.json", "application/json"),
        ("/canvas/assets/icons/icon/0_merged.svg", "image/svg+xml"),
        ("/canvas/assets/fonts/IBMPlexSans-Medium.woff2", "font/woff2"),
        ("/canvas/TLDRAW-LICENSE.md", "text/plain"),
    ] {
        let r = get(&server, path);
        assert!(r.status == 200 && r.content_type() == mime, "{path}");
        assert!(r.body.len() > 1000, "{path}");
        if path.ends_with(".json") {
            assert!(r.json().is_object()); // the file itself, not a JSON string wrapped around it
        }
    }
    for bad in [
        "/canvas/../server.py",
        "/canvas/%2e%2e%2fserver.py",
        "/canvas/..%2fserver.py",
        "/canvas/assets/../../app.js",
        "/canvas/.hidden.js",
        "/canvas/nope.js",
        "/canvas/assets",
        "/canvas/canvas.js.map",
        "/canvas/",
    ] {
        assert_eq!(get(&server, bad).status, 404, "{bad}");
    }
    server.shutdown();
}

#[test]
fn test_tldraw_licence_key_reaches_the_page_only_when_set() {
    // Python changes the environment between two requests to one server; here each server reads its own.
    let tmp = tempfile::tempdir().unwrap();
    let server = demo_server(tmp.path());
    assert_eq!(get_json(&server, "/api/config")["tldraw_license_key"], json!(""));
    server.shutdown();
    let tmp = tempfile::tempdir().unwrap();
    let server = start(StartOptions {
        ctx: ctx(&[("EVE_TLDRAW_LICENSE_KEY", "  tldraw-test-key  ")], &tmp.path().join("spend")),
        ..opts(tmp.path(), true)
    })
    .unwrap();
    assert_eq!(get_json(&server, "/api/config")["tldraw_license_key"], json!("tldraw-test-key"));
    server.shutdown();
}

fn live(env: &[(&str, &str)], ledger: &std::path::Path, http: Arc<FakePost>) -> LiveTutor {
    LiveTutor::new(ctx(env, ledger), http)
}

#[test]
fn test_live_requires_key_and_strict_reply() {
    let tmp = tempfile::tempdir().unwrap();
    let tutor = live(&[("OPENAI_API_KEY", "not-a-fallback")], tmp.path(), FakePost::answering(vec![]));
    let e = tutor.call("generate", &Map::new()).unwrap_err().to_string();
    assert!(e.contains("OPENROUTER_API_KEY"));
    assert!(validate(json!({"step": "x=2"}), "step").is_err());
}

#[test]
fn test_live_request_uses_current_images_and_a_strict_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    let mut s = create(&app, "learn", "Algebra");
    s.insert("source_image".into(), json!(format!("data:image/png;base64,{}", b64(b"fake-image-for-mock"))));
    let reply = json!({"problem": "", "feedback": "Subtract three from both sides.", "step": "2x = 11 - 3", "status": "continue"});
    let http = FakePost::answering(vec![FakePost::reply(&reply, "stop")]);
    let result = live(&[("OPENROUTER_API_KEY", "test-only")], tmp.path(), http.clone()).call("step", &s).unwrap();
    let calls = http.calls.lock();
    let (url, _, body) = &calls[0];
    assert_eq!(url, "https://openrouter.ai/api/v1/chat/completions");
    assert_eq!(body["response_format"]["json_schema"]["strict"], json!(true));
    assert!(body["messages"][1]["content"].as_array().unwrap().iter().any(|c| c["type"] == "image_url"));
    assert_eq!(result["step"], json!("2x = 11 - 3"));
}

#[test]
fn test_http_origin_token_and_persistence() {
    let tmp = tempfile::tempdir().unwrap();
    let server = demo_server(tmp.path());
    let cfg = get_json(&server, "/api/config");
    let token = cfg["token"].as_str().unwrap();
    let data = json!({"action": "create", "mode": "learn", "topic": "Addition"});
    for headers in [vec![], vec![("X-Eve-Token", token), ("Origin", "https://evil.example")]] {
        assert_eq!(post(&server, &headers, &data).status, 403);
    }
    // A wrong Host (a DNS-rebinding page) is refused too, for reads as well.
    assert_eq!(request(&server, "GET", "/api/lessons", &[("Host", "evil.example")], None).status, 403);
    let lesson = post_ok(&server, token, &data);
    assert_eq!(get_json(&server, "/api/lessons")[0]["id"], lesson["id"]);
    assert_eq!(get_json(&server, &format!("/api/lessons/{}", lesson["id"].as_str().unwrap()))["topic"], json!("Addition"));
    let history = get_json(&server, &format!("/api/lessons/{}/history", lesson["id"].as_str().unwrap()));
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(get(&server, "/api/lessons/nope").json(), json!({"error": "Lesson not found"}));
    let origin = format!("http://127.0.0.1:{}", server.port);
    let r = post(&server, &[("X-Eve-Token", token), ("Origin", &origin)], &json!({"action": "select", "id": "nope"}));
    assert_eq!((r.status, r.json()), (404, json!({"error": "Lesson not found"})));
    let r = post(&server, &[("X-Eve-Token", token)], &json!({"action": "create", "mode": "learn", "topic": " "}));
    assert_eq!((r.status, r.json()), (400, json!({"error": "What topic would you like to learn?"})));
    let r = post(&server, &[("X-Eve-Token", token)], &json!([1]));
    assert_eq!((r.status, r.json()), (400, json!({"error": "Invalid request."})));
    let r = request(&server, "POST", "/api/action", &[("X-Eve-Token", token)], None);
    assert_eq!((r.status, r.json()), (413, json!({"error": "Request is too large."})));
    let r = request(&server, "POST", "/api/other", &[("X-Eve-Token", token)], Some(b"{}"));
    assert_eq!(r.status, 404);
    assert_eq!(get(&server, "/nothing").json(), json!({"error": "Not found"}));
    let r = get(&server, "/api/lessons");
    assert_eq!((r.header("cache-control"), r.header("x-content-type-options")), ("no-store".into(), "nosniff".into()));
    server.shutdown();
}

#[test]
fn test_bundled_fonts_are_served_same_origin_and_traversal_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let server = demo_server(tmp.path());
    assert!(get(&server, "/").header("content-security-policy").contains("font-src 'self'"));
    let css = String::from_utf8(get(&server, "/style.css").body).unwrap();
    let mut fonts: Vec<String> = css
        .split("url(")
        .skip(1)
        .filter(|p| p.contains(".woff2"))
        .map(|p| p.split(')').next().unwrap().trim_matches(|c| c == '\'' || c == '"').to_string())
        .collect();
    fonts.sort();
    fonts.dedup();
    assert!(!fonts.is_empty(), "style.css references no bundled woff2 fonts");
    assert!(!fonts.iter().any(|f| f.contains("googleapis") || f.contains("gstatic")));
    for f in &fonts {
        // urljoin(url + "style.css", ref): a root-relative or relative reference from /.
        let path = if f.starts_with('/') { f.clone() } else { format!("/{f}") };
        let r = get(&server, &path);
        assert_eq!(r.status, 200, "{path}");
        assert_eq!(r.content_type(), "font/woff2");
        assert!(r.body.len() > 1000);
    }
    assert_eq!(get(&server, "/fonts/OFL.txt").content_type(), "text/plain");
    for bad in ["/fonts/../server.py", "/fonts/%2e%2e%2fserver.py", "/fonts/..%2fserver.py", "/fonts/nope.woff2"] {
        assert_eq!(get(&server, bad).status, 404, "{bad}");
    }
    server.shutdown();
}

#[test]
fn test_voice_uses_same_lesson_state() {
    let tmp = tempfile::tempdir().unwrap();
    let opened = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let log = opened.clone();
    let app = LearningApp::new(StartOptions { opener: Arc::new(move |u: &str| log.lock().push(u.to_string())), ..opts(tmp.path(), true) }).unwrap();
    let voice = |v: Value| app.voice(&eve_learning::lesson_request(&v).unwrap()).unwrap();
    assert_eq!(voice(json!({"action": "open"}))["ok"], json!(true));
    voice(json!({"action": "start", "mode": "learn", "topic": "Calculus", "level": "College year 1"}));
    assert!(voice(json!({"action": "step"}))["answer"].as_str().unwrap().contains("3x^(3 - 1)"));
    assert!(voice(json!({"action": "step"}))["answer"].as_str().unwrap().contains("3x^2"));
    assert_eq!(app.store.get(&app.active().unwrap()).unwrap()["stage"], json!("complete"));
    let opened = opened.lock();
    assert_eq!(opened.len(), 2);
    assert!(opened[1].ends_with(&format!("#lesson/{}", app.active().unwrap())));
    let status = voice(json!({"action": "status"}));
    assert_eq!(status["lesson"]["stage"], json!("complete"));
    let ideas = voice(json!({"action": "ideas", "text": ""}));
    assert_eq!(ideas["answer"], json!("I've saved your thinking. Would you like a hint or a check?"));
}

#[test]
fn test_provider_selection_and_astra_default() {
    let cases: [(&[(&str, &str)], bool); 6] = [
        (&[], false),
        (&[("OPENROUTER_API_KEY", "router")], true),
        (&[("OPENAI_API_KEY", "direct")], false), // never a fallback
        (&[("EVE_LEARNING_PROVIDER", "openrouter"), ("OPENROUTER_API_KEY", "router")], true),
        (&[("EVE_LEARNING_MODEL", "gpt-6-astra"), ("OPENROUTER_API_KEY", "router")], true),
        // a leftover provider=openai (the old default) means OpenRouter too since 2026-10-08
        (&[("EVE_LEARNING_PROVIDER", "openai"), ("OPENAI_API_KEY", "direct"), ("OPENROUTER_API_KEY", "router")], true),
    ];
    let tmp = tempfile::tempdir().unwrap();
    for (env, ready) in cases {
        let cfg = live_settings(&ctx(env, tmp.path())).unwrap();
        assert_eq!((cfg.provider.as_str(), cfg.model.as_str(), cfg.ready), ("openrouter", "openai/gpt-6-astra", ready));
        assert!(!cfg.to_map().values().any(|v| v == "router" || v == "direct"));
    }
}

#[test]
fn test_the_tutor_goes_to_openrouter_even_with_an_openai_key() {
    let tmp = tempfile::tempdir().unwrap();
    let s = create(&demo_app(tmp.path()), "learn", "Algebra");
    let reply = json!({"problem": "", "feedback": "Look at the ones place.", "step": "", "status": "continue"});
    let http = FakePost::answering(vec![FakePost::reply(&reply, "stop")]);
    live(&[("OPENAI_API_KEY", "direct-test"), ("OPENROUTER_API_KEY", "router-test")], tmp.path(), http.clone()).call("hint", &s).unwrap();
    assert_eq!(http.calls.lock()[0].0, "https://openrouter.ai/api/v1/chat/completions");
    assert_eq!(http.header(0, "Authorization").as_deref(), Some("Bearer router-test"));
}

#[test]
fn test_an_openai_key_alone_asks_for_the_openrouter_key() {
    let tmp = tempfile::tempdir().unwrap();
    let http = FakePost::answering(vec![]);
    let e = live(&[("OPENAI_API_KEY", "direct-test")], tmp.path(), http.clone()).call("generate", &Map::new()).unwrap_err();
    assert!(e.to_string().contains("OPENROUTER_API_KEY"));
    assert!(http.calls.lock().is_empty());
}

#[test]
fn test_invalid_provider_fails_without_guessing() {
    let tmp = tempfile::tempdir().unwrap();
    let e = live_settings(&ctx(&[("EVE_LEARNING_PROVIDER", "typo")], tmp.path())).unwrap_err();
    assert!(e.to_string().contains("must be"));
}

#[test]
fn test_openrouter_request_and_parser() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = create(&demo_app(tmp.path()), "learn", "Algebra");
    s.insert("source_image".into(), json!("data:image/png;base64,original"));
    s.insert("board_image".into(), json!("data:image/png;base64,current"));
    let reply = json!({"problem": "", "feedback": "Subtract three from both sides.", "step": "2x = 11 - 3", "status": "continue"});
    let http = FakePost::answering(vec![FakePost::reply(&reply, "stop")]);
    let env = [("EVE_LEARNING_PROVIDER", "openrouter"), ("OPENROUTER_API_KEY", "router-test-secret"), ("OPENAI_API_KEY", "do-not-send-this")];
    assert_eq!(Value::Object(live(&env, tmp.path(), http.clone()).call("step", &s).unwrap()), reply);
    assert_eq!(http.header(0, "Authorization").as_deref(), Some("Bearer router-test-secret"));
    let calls = http.calls.lock();
    let (url, _, body) = &calls[0];
    assert_eq!(url, "https://openrouter.ai/api/v1/chat/completions");
    assert_eq!(body["model"], json!("openai/gpt-6-astra"));
    assert!(body.get("models").is_none()); // no alternative model routing
    assert_eq!(body["response_format"]["json_schema"]["strict"], json!(true));
    assert_eq!(body["provider"]["require_parameters"], json!(true));
    let images: Vec<&Value> =
        body["messages"][1]["content"].as_array().unwrap().iter().filter(|c| c["type"] == "image_url").map(|c| &c["image_url"]["url"]).collect();
    assert_eq!(images, vec![&s["source_image"], &s["board_image"]]);
    assert!(!body.to_string().contains("do-not-send-this"));
    // The text context carries no images and no whiteboard snapshots.
    let text: Value = serde_json::from_str(body["messages"][1]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text["action"], json!("step"));
    assert_eq!(
        text["lesson"].as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["topic", "level", "mode", "problem", "ideas", "events", "stuck"]
    );
    assert_eq!(body["messages"][0]["content"], json!(eve_learning::tutor::PROMPT));
    assert_eq!(body["max_tokens"], json!(2400));
}

#[test]
fn test_openrouter_incomplete_response_is_not_accepted() {
    for finish in ["length", "error", "content_filter"] {
        let tmp = tempfile::tempdir().unwrap();
        let s = create(&demo_app(tmp.path()), "learn", "Algebra");
        let http = FakePost::answering(vec![FakePost::reply(&json!({}), finish)]);
        let e = live(&[("EVE_LEARNING_PROVIDER", "openrouter"), ("OPENROUTER_API_KEY", "router-test")], tmp.path(), http)
            .call("check", &s)
            .unwrap_err();
        assert!(e.to_string().contains("did not finish"), "{finish}");
    }
}

#[test]
fn test_openrouter_http_error_never_exposes_credentials() {
    let tmp = tempfile::tempdir().unwrap();
    let s = create(&demo_app(tmp.path()), "learn", "Algebra");
    let http = FakePost::answering(vec![Err(PostError::Status { code: 402, body: b"secret-provider-body".to_vec() })]);
    let e = live(&[("OPENROUTER_API_KEY", "private-test-key")], tmp.path(), http).call("check", &s).unwrap_err().to_string();
    assert!(e.contains("credits"));
    assert!(!e.contains("private-test-key") && !e.contains("secret-provider-body"));
    assert_eq!(e, "openrouter tutor request failed (HTTP 402). Check your provider credits. Your work is saved.");
    let http = FakePost::answering(vec![Err(PostError::Io("timed out".into()))]);
    let e = live(&[("OPENROUTER_API_KEY", "k")], tmp.path(), http).call("check", &s).unwrap_err().to_string();
    assert_eq!(e, "Eve could not reach the tutor. Your work is saved; try again.");
}

#[test]
fn test_config_reports_openrouter_readiness_without_key() {
    let tmp = tempfile::tempdir().unwrap();
    let server = start(StartOptions {
        ctx: ctx(&[("EVE_LEARNING_PROVIDER", "openrouter"), ("OPENROUTER_API_KEY", "private-test-key")], &tmp.path().join("spend")),
        ..opts(tmp.path(), false)
    })
    .unwrap();
    let r = get(&server, "/api/config");
    let cfg = r.json();
    assert!(cfg["provider"] == "openrouter" && cfg["ready"] == true);
    assert_eq!(cfg["model"], json!("openai/gpt-6-astra"));
    assert!(!String::from_utf8_lossy(&r.body).contains("private-test-key"));
    assert_eq!(
        cfg.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["provider", "model", "key_name", "ready", "token", "demo", "active", "listen_available", "tldraw_license_key"]
    );
    server.shutdown();
}

#[test]
fn test_openrouter_terms_restriction_is_actionable_without_leaking_body() {
    let tmp = tempfile::tempdir().unwrap();
    let s = create(&demo_app(tmp.path()), "learn", "Algebra");
    let payload = json!({"error": {"message": "The request is prohibited due to a violation of provider Terms Of Service."},
                         "user_id": "private-user-id"});
    let http = FakePost::answering(vec![Err(PostError::Status { code: 403, body: payload.to_string().into_bytes() })]);
    let e = live(&[("EVE_LEARNING_PROVIDER", "openrouter"), ("OPENROUTER_API_KEY", "private-test-key")], tmp.path(), http)
        .call("generate", &s)
        .unwrap_err()
        .to_string();
    assert!(e.contains("Terms of Service restriction"));
    assert!(!e.contains("private-test-key") && !e.contains("private-user-id"));
    assert!(e.contains("support"));
}

#[test]
fn test_launcher_explains_busy_port_without_traceback() {
    let tmp = tempfile::tempdir().unwrap();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let mut env = std::collections::HashMap::<String, String>::new();
    env.insert("EVE_ENV_FILE".into(), tmp.path().join("no-env").display().to_string());
    let mut stderr = Vec::new();
    let argv: Vec<String> = ["--data-dir", tmp.path().to_str().unwrap(), "--no-open", "--port", &port.to_string()]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let started = |o: StartOptions| start(StartOptions { ctx: ctx(&[], &tmp.path().join("spend")), ..o });
    assert_eq!(main_with(&argv, &mut env, &started, &mut stderr).unwrap(), 2);
    let error = String::from_utf8(stderr).unwrap();
    assert!(error.contains(&format!("http://127.0.0.1:{port}/")));
    assert!(error.contains("stop the existing Eve process"));
    assert!(!error.contains("Traceback"));
}

#[test]
fn test_launcher_does_not_hide_other_startup_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let mut env = std::collections::HashMap::<String, String>::new();
    env.insert("EVE_ENV_FILE".into(), tmp.path().join("no-env").display().to_string());
    let refuse = |_: StartOptions| -> std::io::Result<eve_learning::LearningServer> {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    };
    let argv: Vec<String> = vec!["--data-dir".into(), tmp.path().display().to_string(), "--no-open".into()];
    assert!(main_with(&argv, &mut env, &refuse, &mut Vec::new()).is_err());
}

fn png(size: usize) -> String {
    let mut raw = b"\x89PNG\r\n\x1a\n".to_vec();
    raw.extend(std::iter::repeat_n(b'x', size));
    format!("data:image/png;base64,{}", b64(&raw))
}

#[test]
fn test_an_unchanged_image_is_stored_once_across_revisions() {
    // Review D4: every autosave copied the screenshot (up to 6 MB) and the board into a new revision.
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    let s = create(&app, "help", "Algebra");
    let image = png(300_000);
    let mut s = dispatch(&app, json!({"action": "save", "revision": s["revision"], "work": {"source_image": image}})).unwrap();
    for i in 0..10 {
        s = dispatch(&app, json!({"action": "save", "revision": s["revision"], "work": {"ideas": format!("idea {i}")}})).unwrap();
    }
    let db = rusqlite::Connection::open(tmp.path().join("lessons.sqlite3")).unwrap();
    let stored: i64 = db.query_row("SELECT SUM(LENGTH(data)) FROM revisions", [], |r| r.get(0)).unwrap();
    let blobs: i64 = db.query_row("SELECT SUM(LENGTH(data)) FROM blobs", [], |r| r.get(0)).unwrap();
    drop(db);
    assert!(((stored + blobs) as usize) < 2 * image.len(), "the image was copied into every revision");
    let id = s["id"].as_str().unwrap().to_string();
    let history = app.store.history(&id).unwrap();
    assert_eq!(history.len() as i64, s["revision"].as_i64().unwrap());
    let images: Vec<&Value> = history.iter().map(|r| &r["source_image"]).collect();
    assert!(images[images.len() - 11..].iter().all(|i| **i == json!(image))); // history still returns it whole
    assert_eq!(*history.last().unwrap(), Value::Object(app.store.get(&id).unwrap()));
    dispatch(&app, json!({"action": "delete", "id": id})).unwrap();
    let db = rusqlite::Connection::open(tmp.path().join("lessons.sqlite3")).unwrap();
    let count: i64 = db.query_row("SELECT COUNT(*) FROM blobs", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_the_widget_and_list_read_summaries_without_parsing_lessons() {
    let tmp = tempfile::tempdir().unwrap();
    let app = demo_app(tmp.path());
    let s = create(&app, "learn", "Algebra");
    let rows = app.store.list().unwrap();
    app.store.write_widget().unwrap();
    assert_eq!(rows, vec![Store::summary(&app.store.get(s["id"].as_str().unwrap()).unwrap()).unwrap()]);
    assert_eq!(rows[0]["demo"], json!(true));
    let dashboard: Value = serde_json::from_str(&std::fs::read_to_string(tmp.path().join("dashboard.json")).unwrap()).unwrap();
    assert_eq!(dashboard["lessons"], Value::Array(rows.into_iter().map(Value::Object).collect()));
}

/// DemoTutor that waits on `release` before answering a hint, like a live model call.
struct SlowTutor {
    entered: std::sync::mpsc::SyncSender<()>,
    release: parking_lot::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl Tutor for SlowTutor {
    fn demo(&self) -> bool {
        true
    }
    fn call(&self, action: &str, lesson: &Map<String, Value>) -> Result<Map<String, Value>, LearningError> {
        if action == "hint" {
            self.entered.send(()).unwrap();
            self.release.lock().recv_timeout(std::time::Duration::from_secs(5)).expect("released");
        }
        DemoTutor.call(action, lesson)
    }
}

fn slow_app(dir: &std::path::Path) -> (Arc<LearningApp>, std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let tutor = SlowTutor { entered: entered_tx, release: parking_lot::Mutex::new(release_rx) };
    let app = LearningApp::new(StartOptions { tutor: Some(Arc::new(tutor)), ..opts(dir, false) }).unwrap();
    (Arc::new(app), entered_rx, release_tx)
}

#[test]
fn test_the_tutor_call_does_not_hold_the_lesson_lock() {
    // Review D3: a slow tutor (up to 90 s live) must not block autosave or a spoken append.
    let tmp = tempfile::tempdir().unwrap();
    let (app, entered, release) = slow_app(tmp.path());
    let s = create(&app, "learn", "Algebra");
    let id = s["id"].as_str().unwrap().to_string();
    let (a, i) = (app.clone(), id.clone());
    let asker = std::thread::spawn(move || a.dispatch(&obj(json!({"id": i, "action": "hint"}))));
    entered.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    let (a, i) = (app.clone(), id.clone());
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let spoken = std::thread::spawn(move || {
        let r = a.append_spoken(&i, "maybe subtract three");
        done_tx.send(()).unwrap();
        r
    });
    let saved = done_rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok();
    release.send(()).unwrap();
    let out = asker.join().unwrap().unwrap();
    let spoken = spoken.join().unwrap().unwrap();
    assert!(saved, "append_spoken waited for the tutor");
    assert!(spoken);
    assert_eq!(out["events"].as_array().unwrap().last().unwrap()["action"], json!("hint"));
    assert!(out["ideas"].as_str().unwrap().contains("maybe subtract three")); // the answer lands on top of the spoken line
}

#[test]
fn test_a_lesson_moved_on_during_the_tutor_call_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, entered, release) = slow_app(tmp.path());
    let s = create(&app, "learn", "Algebra");
    let id = s["id"].as_str().unwrap().to_string();
    let (a, i) = (app.clone(), id.clone());
    let asker = std::thread::spawn(move || a.dispatch(&obj(json!({"id": i, "action": "hint"}))));
    entered.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    dispatch(&app, json!({"id": id, "action": "end"})).unwrap();
    release.send(()).unwrap();
    let e = asker.join().unwrap().unwrap_err().to_string();
    assert!(e.contains("changed in another window"));
    assert_eq!(app.store.get(&id).unwrap()["stage"], json!("ended"));
}

#[test]
fn notify_gets_each_action_but_not_saves_and_a_failure_keeps_the_lesson() {
    let tmp = tempfile::tempdir().unwrap();
    let seen = Arc::new(parking_lot::Mutex::new(Vec::<(String, String)>::new()));
    let log = seen.clone();
    let app = LearningApp::new(StartOptions {
        notify: Some(Arc::new(move |text: &str, stage: &str| {
            log.lock().push((text.to_string(), stage.to_string()));
            anyhow::bail!("the robot is unplugged")
        })),
        ..opts(tmp.path(), true)
    })
    .unwrap();
    let s = create(&app, "learn", "Algebra");
    dispatch(&app, json!({"action": "save", "work": {"ideas": "x"}})).unwrap();
    assert_eq!(*seen.lock(), vec![("Try this example. Write your thinking, or ask for a hint.".to_string(), "working".to_string())]);
    assert_eq!(app.store.get(s["id"].as_str().unwrap()).unwrap()["ideas"], json!("x"));
    assert!(err_text(dispatch(&app, json!({"action": "dance"}))).contains("Unknown lesson action."));
}

#[test]
fn lessons_keep_their_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let demo = demo_app(tmp.path());
    let s = create(&demo, "learn", "Algebra");
    let live_app = LearningApp::new(opts(tmp.path(), false)).unwrap();
    let e = err_text(live_app.dispatch(&obj(json!({"id": s["id"], "action": "hint"}))));
    assert_eq!(e, "Open this lesson using the same demo/live mode it was created in.");
    assert!(err_text(dispatch(&demo, json!({"action": "create", "mode": "teach", "topic": "x"}))).contains("Choose learn or help."));
    let fresh = demo_app(&tmp.path().join("other"));
    assert_eq!(err_text(dispatch(&fresh, json!({"action": "hint"}))), "Open a lesson first.");
}

#[test]
fn a_python_written_lesson_reads_back() {
    // The JSON text Python's json.dumps(ensure_ascii=False) wrote, read by the Rust store and saved again.
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path()).unwrap();
    let db = rusqlite::Connection::open(&store.path).unwrap();
    let data = r#"{"id": "abc", "mode": "help", "topic": "Résumé", "level": "", "demo": true, "problem": "", "ideas": "é", "stuck": false, "strokes": [], "board": null, "source_image": "", "board_image": "", "events": [], "stage": "input", "revision": 1, "updated": 1790000000.5}"#;
    db.execute("INSERT INTO lessons VALUES (?,?,?)", rusqlite::params!["abc", 1790000000.5, data]).unwrap();
    let s = store.get("abc").unwrap();
    assert_eq!(s["topic"], json!("Résumé"));
    let saved = store.save(&s, Some(1)).unwrap();
    assert_eq!(saved["revision"], json!(2));
    let text: String = db.query_row("SELECT data FROM lessons WHERE id='abc'", [], |r| r.get(0)).unwrap();
    assert!(text.starts_with(r#"{"id": "abc", "mode": "help", "topic": "Résumé", "#), "{text}");
    assert!(store.save(&s, Some(1)).unwrap_err().to_string().contains("another window"));
}

#[test]
fn a_stored_lesson_without_a_revision_is_a_conflict_for_a_checked_save() {
    // Python compares the stored revision (None) with `expected` before adding one to it.
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path()).unwrap();
    let db = rusqlite::Connection::open(&store.path).unwrap();
    db.execute("INSERT INTO lessons VALUES ('r', 1.0, '{\"id\": \"r\"}')", []).unwrap();
    let s = store.get("r").unwrap();
    assert!(matches!(store.save(&s, Some(0)), Err(LearningError::Value(m)) if m.contains("another window")));
    assert!(matches!(store.save(&s, None), Err(LearningError::Type(_))));
}

/// One raw HTTP answer on a local port, for the real tutor POST.
fn answer_once(status_line: &'static str, body: &'static str) -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 65536];
        let mut got = Vec::new();
        // Read the head and the body the client announced.
        loop {
            let n = conn.read(&mut buf).unwrap();
            got.extend_from_slice(&buf[..n]);
            if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&got[..end]).to_ascii_lowercase();
                let len = head.lines().find_map(|l| l.strip_prefix("content-length:")).map(|v| v.trim().parse::<usize>().unwrap()).unwrap_or(0);
                if got.len() >= end + 4 + len {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let reply = format!("{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        conn.write_all(reply.as_bytes()).unwrap();
    });
    port
}

#[test]
fn the_real_post_answers_from_a_worker_thread_of_a_runtime() {
    // The server runs each tutor call on a blocking thread of its tokio runtime; the blocking client must work
    // there, and report an HTTP error status with its body, a refused connection as a network failure.
    use eve_learning::tutor::{ReqwestPost, TutorHttp};
    use std::time::Duration;
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let ok_port = answer_once("HTTP/1.1 200 OK", "{\"choices\": []}");
    let err_port = answer_once("HTTP/1.1 402 Payment Required", "{\"error\": {\"message\": \"credits\"}}");
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let results = rt.block_on(async move {
        tokio::task::spawn_blocking(move || {
            let post = |port: u16| {
                ReqwestPost.post(
                    &format!("http://127.0.0.1:{port}/api/v1/chat/completions"),
                    &[("Content-Type".to_string(), "application/json".to_string())],
                    b"{}".to_vec(),
                    Duration::from_secs(10),
                )
            };
            (post(ok_port), post(err_port), post(closed))
        })
        .await
        .unwrap()
    });
    assert_eq!(results.0, Ok(b"{\"choices\": []}".to_vec()));
    assert_eq!(results.1, Err(PostError::Status { code: 402, body: b"{\"error\": {\"message\": \"credits\"}}".to_vec() }));
    assert!(matches!(results.2, Err(PostError::Io(_))));
}

#[test]
fn shutdown_does_not_wait_for_a_tutor_call_in_flight() {
    // Python's shutdown() returns while a request thread is still asking the tutor; so does this one.
    let tmp = tempfile::tempdir().unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let tutor = SlowTutor { entered: entered_tx, release: parking_lot::Mutex::new(release_rx) };
    let server = start(StartOptions { tutor: Some(Arc::new(tutor)), ..opts(tmp.path(), false) }).unwrap();
    let token = get_json(&server, "/api/config")["token"].as_str().unwrap().to_string();
    let lesson = post_ok(&server, &token, &json!({"action": "create", "mode": "learn", "topic": "Algebra"}));
    post_ok(&server, &token, &json!({"action": "generate", "id": lesson["id"]}));
    let port = server.port;
    let asker = std::thread::spawn(move || {
        let body = json!({"action": "hint", "id": lesson["id"]}).to_string();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let head = format!(
            "POST /api/action HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Eve-Token: {token}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        std::io::Write::write_all(&mut stream, head.as_bytes()).unwrap();
        let mut sink = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stream, &mut sink);
    });
    entered_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    let began = std::time::Instant::now();
    server.shutdown();
    assert!(began.elapsed() < std::time::Duration::from_secs(4), "shutdown waited {:?}", began.elapsed());
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err(), "the port is closed");
    release_tx.send(()).unwrap();
    asker.join().unwrap();
}
