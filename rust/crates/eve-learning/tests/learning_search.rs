//! Port of bridge/tests/test_learning_search.py (TinyFish practice references: fallbacks, privacy, and persisted
//! references) and the lesson part of test_spend_wiring.py. test_missing_key, test_search_failure and
//! test_search_is_subject_neutral are unit tests in src/search.rs.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::*;
use eve_learning::search::search_problems;
use eve_learning::server::{main_with, LearningApp, StartOptions};
use eve_learning::store::Store;
use eve_learning::tutor::{LiveTutor, Tutor};
use eve_web::{FetchError, HttpOptions};
use serde_json::{json, Value};

fn percent_decode_plus(s: &str) -> String {
    let s = s.replace('+', " ");
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

#[test]
fn test_references_and_privacy() {
    let tmp = tempfile::tempdir().unwrap();
    let r#ref = ("https://openstax.org/books/algebra", "Algebra", "Equation exercises");
    let (http, sent) = tinyfish_api(vec![r#ref, ("javascript:alert(1)", "Algebra", "Equation exercises"), ("http://plain.example/x", "Algebra", "Equation exercises")]);
    let c = ctx(&[("TINYFISH_API_KEY", "tf-secret"), ("OPENROUTER_API_KEY", "tutor-secret")], &tmp.path().join("spend")).with_http(http);
    let reply = json!({"problem": "Solve 3x = 12.", "feedback": "Try this.", "step": "", "status": "continue"});
    let post = FakePost::answering(vec![FakePost::reply(&reply, "stop")]);
    let tutor = LiveTutor::new(c.clone(), post.clone());
    let app = LearningApp::new(StartOptions { tutor: Some(Arc::new(tutor)), ctx: c, ..opts(tmp.path(), false) }).unwrap();
    let s = dispatch(&app, json!({"action": "create", "topic": "Algebra", "level": "Grade 7"})).unwrap();
    dispatch(&app, json!({"action": "save", "work": {"ideas": "private learner work"}})).unwrap();
    dispatch(&app, json!({"action": "generate"})).unwrap();
    let sent = sent.lock();
    let mut seen: Vec<String> = sent.iter().map(|(u, _, _)| percent_decode_plus(u)).collect();
    seen.extend(sent.iter().map(|(_, b, _)| b.clone().unwrap_or(Value::Null).to_string()));
    let seen = seen.join(" ");
    assert!(seen.contains("Grade 7") && !seen.contains("private learner work"));
    assert!(post.calls.lock()[0].2.to_string().contains("Equation exercises"));
    let event = Store::new(tmp.path()).unwrap().get(s["id"].as_str().unwrap()).unwrap()["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(event["sources"], json!([{"title": "Algebra", "url": "https://openstax.org/books/algebra"}])); // https only
    assert_eq!(event["search_note"], json!("Practice references found with TinyFish; Eve creates an adapted problem."));
}

#[test]
fn test_demo_never_searches() {
    let tmp = tempfile::tempdir().unwrap();
    let fail = |_: &str, _: &HttpOptions| -> Result<(u16, String), FetchError> { panic!("the demo never searches") };
    let c = ctx(&[("TINYFISH_API_KEY", "tf-secret")], &tmp.path().join("spend")).with_http(Arc::new(fail));
    let app = LearningApp::new(StartOptions { ctx: c, ..opts(tmp.path(), true) }).unwrap();
    dispatch(&app, json!({"action": "create", "topic": "Counting"})).unwrap();
    let s = dispatch(&app, json!({"action": "generate"})).unwrap();
    assert_eq!(s["problem"], json!("What is 7 + 5?"));
}

#[test]
fn test_env_file_supplies_the_tinyfish_key_to_standalone_launcher() {
    // The standalone launcher loads the env file before the server starts and before any search runs.
    let tmp = tempfile::tempdir().unwrap();
    let env_file = tmp.path().join("env");
    std::fs::write(&env_file, "TINYFISH_API_KEY=tf-from-file\n").unwrap();
    let mut env: HashMap<String, String> = HashMap::new();
    let started = parking_lot::Mutex::new(false);
    let fake_start = |_: StartOptions| -> std::io::Result<eve_learning::LearningServer> {
        *started.lock() = true;
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    };
    let argv: Vec<String> = ["--env-file", env_file.to_str().unwrap(), "--data-dir", tmp.path().to_str().unwrap(), "--no-open"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(main_with(&argv, &mut env, &fake_start, &mut Vec::new()).is_err());
    assert!(*started.lock());
    assert_eq!(env.get("TINYFISH_API_KEY").map(String::as_str), Some("tf-from-file"));
    let (http, sent) = tinyfish_api(vec![]);
    let c = eve_web::Ctx::global().clone().with_env(env).with_http(http).with_ledger(eve_spend::Ledger::at(tmp.path().join("spend")));
    search_problems(&c, "Fractions", "Grade 4");
    let sent = sent.lock();
    assert!(sent[0].2.contains(&("X-API-Key".to_string(), "tf-from-file".to_string())));
}

#[test]
fn test_a_lesson_turn_and_its_reference_search_are_metered() {
    // test_spend_wiring.py: the search line is free and says how many pages; the tutor turn is priced from usage.
    let tmp = tempfile::tempdir().unwrap();
    let ledger = tmp.path().join("spend");
    let search = |_: &str, _: &HttpOptions| -> Result<(u16, String), FetchError> {
        Ok((200, json!({"query": "q", "results": [], "total_results": 0, "page": 0}).to_string()))
    };
    let c = ctx(&[("OPENROUTER_API_KEY", "sk-test"), ("TINYFISH_API_KEY", "tf-test")], &ledger).with_http(Arc::new(search));
    search_problems(&c, "fractions", "grade 5");
    let m = 1_000_000;
    let post = FakePost::answering(vec![Ok(json!({"usage": {"prompt_tokens": m, "completion_tokens": 0},
                                                    "choices": [{"finish_reason": "stop", "message": {"content": "{}"}}]})
        .to_string()
        .into_bytes())]);
    let lesson = obj(json!({"topic": "fractions", "level": "5", "mode": "learn", "problem": "", "ideas": [], "events": [], "stuck": false}));
    // The fake reply is not a lesson; it was metered.
    assert!(LiveTutor::new(c, post).call("next", &lesson).is_err());
    let lines = ledger_lines(&ledger);
    let (search, turn) = (&lines[0], &lines[1]);
    assert_eq!((search["p"].as_str(), search["f"].as_str(), search["usd"].as_f64(), search["note"].as_str()),
               (Some("tinyfish"), Some("lessons"), Some(0.0), Some("0 pages, free")));
    assert_eq!((turn["f"].as_str(), turn["p"].as_str(), turn["m"].as_str()), (Some("lessons"), Some("openrouter"), Some("openai/gpt-6-astra")));
    assert!((turn["usd"].as_f64().unwrap() - 10.0).abs() < 1e-6, "{turn}");
}
