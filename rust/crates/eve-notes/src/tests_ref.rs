//! The parts of other Python test files that exercise notes.py: `test_model_memory_roles.py` (the request bodies the
//! summary and the reduce send) and `test_spend_wiring.py` (the room notes' calls are metered).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::tests::{response, tmp, FakeSdk};
use crate::*;

/// A local stand-in for OpenRouter: answers every POST with a Responses reply whose text is `{}`, and keeps each
/// request's path and JSON body (the Python test's mock transport).
fn fake_openrouter() -> (String, Arc<Mutex<Vec<(String, Value)>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let seen = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            if reader.read_line(&mut first).is_err() {
                continue;
            }
            let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            let model = serde_json::from_slice::<Value>(&body).ok().and_then(|b| b.get("model").cloned());
            seen.lock().unwrap().push((path, serde_json::from_slice(&body).unwrap_or(Value::Null)));
            let reply = json!({"id": "test", "object": "response", "created_at": 0, "status": "completed",
                               "model": model, "output": [{"id": "message", "type": "message", "role": "assistant",
                               "status": "completed", "content": [{"type": "output_text", "text": "{}",
                                                                   "annotations": []}]}]});
            let payload = serde_json::to_vec(&reply).unwrap();
            let mut stream = stream;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            let _ = stream.write_all(&payload);
        }
    });
    (base, requests)
}

#[test]
fn test_room_notes_summary_and_reduce_use_summary_model() {
    // tests/test_model_memory_roles.py, both parametrizations
    for (summary_model, model) in [(DEFAULT_SUMMARY_MODEL, "openai/gpt-6-luna"), ("gpt-6.1-sol", "openai/gpt-6.1-sol")] {
        let (base, sent) = fake_openrouter();
        let d = tmp();
        let opts = eve_openrouter::ClientOptions::default().base_url(base);
        let sdk = Arc::new(OpenRouterSdk::new("test-provider-key".into(), opts));
        let mut client = OpenAINotesClient::with_sdk(DEFAULT_MODEL, summary_model, sdk);
        client.ledger = eve_spend::Ledger::at(d.path());
        assert_eq!(client.summarize("transcript").unwrap(), "{}");
        assert_eq!(client.combine("partial summaries").unwrap(), "{}");
        let sent = sent.lock().unwrap().clone();
        assert_eq!(sent.iter().map(|(_, r)| r["model"].clone()).collect::<Vec<_>>(), [json!(model), json!(model)]);
        assert_eq!(
            sent.iter().map(|(_, r)| r["instructions"].clone()).collect::<Vec<_>>(),
            [json!(SUMMARY_PROMPT), json!(COMBINE_PROMPT)]
        );
        for (path, request) in &sent {
            assert!(path.ends_with("/responses"), "{path}");
            assert_eq!(request["reasoning"], json!({"effort": "low"}));
            assert_eq!(request["text"]["format"]["schema"], *SUMMARY_SCHEMA);
            assert_eq!(request["max_output_tokens"], json!(SUMMARY_MAX_OUTPUT_TOKENS));
        }
    }
}

const M: u64 = 1_000_000;

#[test]
fn test_room_notes_transcription_by_tokens_or_by_the_clip_and_the_summary() {
    // tests/test_spend_wiring.py
    let d = tmp();
    let ledger = eve_spend::Ledger::at(d.path());
    let wav = |seconds: f64| to_wav(&vec![0u8; (16000.0 * seconds) as usize * 2], 16000);
    let client_with = |reply: Value| -> (OpenAINotesClient, Arc<FakeSdk>) {
        let sdk = FakeSdk::new(reply);
        let mut c = OpenAINotesClient::with_sdk(DEFAULT_MODEL, DEFAULT_SUMMARY_MODEL, sdk.clone());
        c.ledger = ledger.clone();
        (c, sdk)
    };
    let (c, sdk) = client_with(json!({"text": "hello", "usage": {"type": "tokens", "input_tokens": M, "output_tokens": 0}}));
    assert_eq!(c.transcribe(&wav(1.0), "").unwrap(), "hello");
    assert_eq!(sdk.fields.lock()[0], *json!({"model": "gpt-4o-mini-transcribe"}).as_object().unwrap(), "no prompt field when empty");
    let (c, sdk) = client_with(json!({"text": " hello ", "usage": {}}));
    assert_eq!(c.transcribe(&wav(60.0), "the tail").unwrap(), "hello"); // no token usage: the clip's length at $0.003/min
    assert_eq!(sdk.fields.lock()[0]["prompt"], json!("the tail"));
    let mut reply = response(r#"{"summary": ""}"#);
    reply["usage"] = json!({"input_tokens": M, "output_tokens": 0});
    let (c, _) = client_with(reply);
    c.summarize("we talked").unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let rows = ledger.day_rows(&today);
    assert_eq!(rows.len(), 3);
    let (a, b, s) = (&rows[0], &rows[1], &rows[2]);
    assert_eq!((a["f"].clone(), a["m"].clone()), (json!("transcription"), json!("gpt-4o-mini-transcribe")));
    assert!((a["usd"].as_f64().unwrap() - 1.25).abs() < 1e-9);
    assert!((b["usd"].as_f64().unwrap() - 0.003).abs() < 1e-9);
    assert_eq!((s["f"].clone(), s["m"].clone()), (json!("room notes"), json!("gpt-6-luna")));
}

#[test]
fn an_empty_summary_is_an_error() {
    let d = tmp();
    let mut c = OpenAINotesClient::with_sdk(DEFAULT_MODEL, DEFAULT_SUMMARY_MODEL, FakeSdk::new(response("  ")));
    c.ledger = eve_spend::Ledger::at(d.path());
    let err = c.summarize("x").unwrap_err();
    assert_eq!(error_name(&err), "RuntimeError");
    assert_eq!(err.to_string(), "empty summary");
}

#[test]
fn make_notes_client_reads_the_key_and_the_model() {
    let env: std::collections::HashMap<String, String> =
        [("OPENROUTER_API_KEY", " k "), ("EVE_NOTES_MODEL_STT", "  ")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    assert!(make_notes_client(Some(&env)).is_some());
    let no_key: std::collections::HashMap<String, String> = [("OPENROUTER_API_KEY".to_string(), "  ".to_string())].into();
    assert!(make_notes_client(Some(&no_key)).is_none());
}
