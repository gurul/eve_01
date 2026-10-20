//! openrouter.py: one key for every model call (owner, 2026-10-08), and a guard that keeps it that way.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::http::{HeaderMap, Uri};
use axum::Router;
use eve_openrouter as openrouter;
use eve_openrouter::{ClientOptions, Part};
use serde_json::{json, Value};

// ---- model ids ---------------------------------------------------------------------------------------------

#[test]
fn test_model_ids_get_their_openrouter_prefix() {
    let cases = [
        ("gpt-6-luna", "openai/gpt-6-luna"),
        ("gpt-6.1-sol", "openai/gpt-6.1-sol"),
        ("gpt-5.4-nano", "openai/gpt-5.4-nano"),
        ("o4-mini", "openai/o4-mini"),
        ("text-embedding-3-small", "openai/text-embedding-3-small"),
        ("gpt-4o-transcribe", "openai/gpt-4o-transcribe"),
        ("gpt-4o-mini-transcribe", "openai/gpt-4o-mini-transcribe"),
        ("claude-opus-5-5", "anthropic/claude-opus-5.5"), // the dot, not the dash
        ("openai/gpt-6-astra", "openai/gpt-6-astra"),     // already prefixed: unchanged
        ("anthropic/claude-opus-5.5", "anthropic/claude-opus-5.5"),
        ("deepgram/aura-2", "deepgram/aura-2"),
        ("google/gemini-3.1-flash-lite", "google/gemini-3.1-flash-lite"),
        ("", ""),
    ];
    for (name, wire) in cases {
        assert_eq!(openrouter::model(name), wire, "{name}");
        assert_eq!(openrouter::model(wire), wire, "{wire}"); // idempotent: a retry re-maps the same body
    }
}

#[test]
fn test_model_ids_edge_cases_match_python() {
    // case-insensitive families, whitespace stripped, a Claude name without a two-part version, unknown names kept
    assert_eq!(openrouter::model("  GPT-6-luna "), "openai/GPT-6-luna");
    assert_eq!(openrouter::model("whisper-1"), "openai/whisper-1");
    assert_eq!(openrouter::model("tts-1"), "openai/tts-1");
    assert_eq!(openrouter::model("chatgpt-4o-latest"), "openai/chatgpt-4o-latest");
    assert_eq!(openrouter::model("computer-use-preview"), "openai/computer-use-preview");
    assert_eq!(openrouter::model("o3"), "openai/o3");
    assert_eq!(openrouter::model("Claude-Sonnet-4-6"), "anthropic/Claude-Sonnet-4.6");
    assert_eq!(openrouter::model("claude-haiku-4-5-20251001"), "anthropic/claude-haiku-4-5-20251001");
    assert_eq!(openrouter::model("claude-3-5-sonnet"), "anthropic/claude-3-5-sonnet");
    assert_eq!(openrouter::model("mercury-3"), "mercury-3");
    assert_eq!(openrouter::model("omni"), "omni");
}

#[test]
fn test_the_rewrite_touches_only_a_string_model() {
    let mut body = json!({"model": "gpt-6-luna", "input": "hi"});
    assert!(openrouter::rewrite(&mut body));
    assert_eq!(body, json!({"model": "openai/gpt-6-luna", "input": "hi"}));
    assert!(!openrouter::rewrite(&mut body)); // idempotent
    let mut no_model = json!({"input": "hi"});
    assert!(!openrouter::rewrite(&mut no_model));
    let mut not_str = json!({"model": 5});
    assert!(!openrouter::rewrite(&mut not_str));
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn test_the_key_gate_reads_only_the_openrouter_key() {
    assert!(openrouter::available(Some(&env(&[("OPENROUTER_API_KEY", "or-test")]))));
    assert!(!openrouter::available(Some(&env(&[("OPENROUTER_API_KEY", "  ")]))));
    assert!(!openrouter::available(Some(&env(&[
        ("OPENAI_API_KEY", "openai-test"),
        ("ANTHROPIC_API_KEY", "anthropic-test")
    ]))));
    assert_eq!(openrouter::key(Some(&env(&[("OPENROUTER_API_KEY", " or-test ")]))), "or-test");
}

#[test]
fn test_no_client_is_built_without_the_key() {
    // an OpenAI key is not a fallback
    let e = env(&[("OPENAI_API_KEY", "sk-test")]);
    let err = openrouter::client(None, ClientOptions::default().env(e.clone())).unwrap_err();
    assert!(matches!(err, openrouter::Error::NoKey(_)));
    assert_eq!(err.to_string(), "OPENROUTER_API_KEY is not set");
    let err = openrouter::async_client(None, ClientOptions::default().env(e.clone())).unwrap_err();
    assert!(matches!(err, openrouter::Error::NoKey(_)));
    let err = openrouter::anthropic_client(Some("   "), ClientOptions::default().env(e)).unwrap_err();
    assert!(matches!(err, openrouter::Error::NoKey(_)));
    // a blank argument falls back to the environment's key
    let c = openrouter::async_client(Some(" "), ClientOptions::default().env(env(&[("OPENROUTER_API_KEY", "or-env")])))
        .unwrap();
    assert_eq!(c.api_key(), "or-env");
}

// ---- the clients ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

/// A local server standing in for OpenRouter (the Python tests' MockTransport): records each request and answers
/// every one with `reply`.
async fn capture(reply: Value) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let sent: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let s = sent.clone();
    let app = Router::new().fallback(move |uri: Uri, headers: HeaderMap, body: Bytes| {
        let s = s.clone();
        let reply = reply.clone();
        async move {
            s.lock().unwrap().push(Seen { path: uri.path().to_string(), headers, body: body.to_vec() });
            axum::Json(reply)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), sent)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn test_every_request_goes_to_openrouter_with_the_prefixed_model() {
    let (server, sent) = capture(json!({"object": "list", "model": "x", "usage": {"prompt_tokens": 1, "total_tokens": 1},
                                        "data": [{"object": "embedding", "index": 0, "embedding": [0.5]}]}))
    .await;
    let c = openrouter::async_client(Some("or-test"), ClientOptions::default().base_url(format!("{server}/api/v1")))
        .unwrap();
    let out = c
        .embeddings_create(json!({"model": "text-embedding-3-small", "input": ["hi"], "dimensions": 1536,
                                  "encoding_format": "float"}))
        .await
        .unwrap();
    assert_eq!(out["data"][0]["embedding"][0], json!(0.5));
    let sent = sent.lock().unwrap().clone();
    let [req] = sent.as_slice() else { panic!("one request, got {}", sent.len()) };
    assert_eq!(req.path, "/api/v1/embeddings");
    assert_eq!(req.headers["authorization"], "Bearer or-test");
    assert_eq!(req.headers["x-title"], "eve");
    assert_eq!(req.headers["http-referer"], "https://github.com/gurul/eve_01");
    assert!(contains(&req.body, br#""model":"openai/text-embedding-3-small""#));
    // and with no override, the client is on OpenRouter, never api.openai.com
    let real = openrouter::async_client(Some("or-test"), ClientOptions::default()).unwrap();
    assert_eq!(real.base_url(), "https://openrouter.ai/api/v1");
    assert!(!real.base_url().contains("api.openai.com"));
}

#[test]
fn test_the_blocking_client_sends_the_prefixed_model() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (server, sent) = rt.block_on(capture(json!({"id": "r1", "output": []})));
    let c = openrouter::client(Some("or-test"), ClientOptions::default().base_url(format!("{server}/api/v1"))).unwrap();
    let out = std::thread::spawn(move || c.responses_create(json!({"model": "gpt-6-luna", "input": "hi"})))
        .join()
        .unwrap()
        .unwrap();
    assert_eq!(out["id"], "r1");
    let sent = sent.lock().unwrap().clone();
    let [req] = sent.as_slice() else { panic!("one request") };
    assert_eq!(req.path, "/api/v1/responses");
    assert!(contains(&req.body, br#""model":"openai/gpt-6-luna""#));
    assert_eq!(openrouter::client(Some("k"), ClientOptions::default()).unwrap().base_url(), openrouter::BASE_URL);
}

#[tokio::test]
async fn test_a_multipart_transcription_gets_the_prefixed_model_too() {
    let (server, sent) = capture(json!({"text": "hi"})).await;
    let c = openrouter::async_client(Some("or-test"), ClientOptions::default().base_url(format!("{server}/api/v1")))
        .unwrap();
    let fields = json!({"model": "gpt-4o-transcribe", "language": "en"});
    let out = c
        .audio_transcriptions_create(fields.as_object().unwrap().clone(), Part::file("file", "a.wav", b"RIFF".to_vec(), "audio/wav"))
        .await
        .unwrap();
    assert_eq!(out["text"], "hi");
    let sent = sent.lock().unwrap().clone();
    let [req] = sent.as_slice() else { panic!("one request") };
    assert_eq!(req.path, "/api/v1/audio/transcriptions");
    assert!(contains(&req.body, b"openai/gpt-4o-transcribe"));
    assert!(!contains(&req.body, b"\r\n\r\ngpt-4o-transcribe\r\n"));
}

#[tokio::test]
async fn test_the_async_client_points_at_openrouter() {
    let c = openrouter::async_client(Some("or-test"), ClientOptions::default()).unwrap();
    assert!(c.base_url().starts_with("https://openrouter.ai/api/v1"));
    assert_eq!(c.timeout(), openrouter::DEFAULT_TIMEOUT);
    assert_eq!(c.max_retries(), 2);
    let tuned = openrouter::async_client(
        Some("or-test"),
        ClientOptions::default().timeout(std::time::Duration::from_secs(30)).max_retries(0).header("X-Title", "eve-test"),
    )
    .unwrap();
    assert_eq!(tuned.timeout(), std::time::Duration::from_secs(30));
    assert_eq!(tuned.max_retries(), 0);
    assert_eq!(tuned.default_headers()["x-title"], "eve-test"); // the caller's headers win
    assert_eq!(tuned.default_headers()["http-referer"], "https://github.com/gurul/eve_01");
}

#[tokio::test]
async fn test_the_anthropic_client_is_on_openrouter_with_the_dotted_opus_id() {
    let (server, sent) = capture(json!({
        "id": "m1", "type": "message", "role": "assistant", "model": "anthropic/claude-opus-5.5",
        "content": [{"type": "text", "text": "hi"}], "stop_reason": "end_turn", "stop_sequence": null,
        "usage": {"input_tokens": 3, "output_tokens": 1}}))
    .await;
    let real = openrouter::anthropic_client(Some("or-test"), ClientOptions::default()).unwrap();
    assert!(real.base_url().starts_with("https://openrouter.ai/api"));
    assert!(real.is_anthropic());
    let c = openrouter::anthropic_client(Some("or-test"), ClientOptions::default().base_url(format!("{server}/api")))
        .unwrap();
    let out = c
        .messages_create(json!({"model": "claude-opus-5-5", "max_tokens": 5,
                                "messages": [{"role": "user", "content": "x"}]}))
        .await
        .unwrap();
    assert_eq!(out["content"][0]["text"], "hi");
    let sent = sent.lock().unwrap().clone();
    let [req] = sent.as_slice() else { panic!("one request") };
    assert_eq!(req.path, "/api/v1/messages");
    assert_eq!(req.headers["x-api-key"], "or-test");
    assert_eq!(req.headers["anthropic-version"], openrouter::ANTHROPIC_VERSION);
    assert!(contains(&req.body, br#""model":"anthropic/claude-opus-5.5""#));
}

#[tokio::test]
async fn test_a_non_2xx_reply_is_a_status_error_after_the_retries() {
    let hits = Arc::new(Mutex::new(0u32));
    let h = hits.clone();
    let app = Router::new().fallback(move || {
        let h = h.clone();
        async move {
            *h.lock().unwrap() += 1;
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                [("retry-after-ms", "1")],
                axum::Json(json!({"error": {"message": "down"}})),
            )
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let c = openrouter::async_client(Some("k"), ClientOptions::default().base_url(format!("http://{addr}"))).unwrap();
    let err = c.chat_completions_create(json!({"model": "gpt-6-luna", "messages": []})).await.unwrap_err();
    assert_eq!(err.status(), Some(503));
    assert_eq!(err.body_json().unwrap()["error"]["message"], "down");
    assert_eq!(*hits.lock().unwrap(), 3); // the first try and two retries
}

#[tokio::test]
async fn test_a_stream_yields_each_event_until_done() {
    let app = Router::new().fallback(|| async {
        (
            [("content-type", "text/event-stream")],
            "event: response.output_text.delta\ndata: {\"delta\":\"he\"}\n\n: ping\n\ndata: {\"delta\":\"llo\"}\n\ndata: [DONE]\n\ndata: {\"after\":1}\n\n",
        )
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let c = openrouter::async_client(Some("k"), ClientOptions::default().base_url(format!("http://{addr}"))).unwrap();
    use futures::StreamExt;
    let events: Vec<_> = c
        .responses_stream(json!({"model": "gpt-6-luna", "input": "hi"}))
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|e| e.unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event.as_deref(), Some("response.output_text.delta"));
    assert_eq!(events[0].data, json!({"delta": "he"}));
    assert_eq!(events[1].data, json!({"delta": "llo"}));
}

// ---- the guard: no production module reads the old keys or calls OpenAI/Anthropic directly ---------------------

const KEYS: [&str; 2] = ["OPENAI_API_KEY", "ANTHROPIC_API_KEY"];
const DIRECT_HOSTS: [&str; 2] = ["https://api.openai.com", "https://api.anthropic.com"];
// The only modules allowed to: each one is an explicit, documented exception (docs/openrouter.md).
const ALLOWED: [&str; 5] = [
    "codex_auth",  // Codex CLI's optional `api` login mode (the default is the ChatGPT plan login)
    "voice_agent", // the desk voice: OpenAI's realtime Live API, until its rebuild (wave 2)
    "phone_call",  // the call's live ears: OpenAI realtime transcription, opt-in (EVE_CALL_LIVE_STT=1)
    "spend_sync",  // the providers' own cost figures, on optional admin keys (never a model call)
    "decisions",   // OpenAI's Decisions beta: dev eval tools only; the daemon never builds a client
];

/// The string literals in Rust source, with their line numbers. Comments (doc comments included) are prose and are
/// skipped, as the Python guard skips docstrings.
fn string_literals(src: &str) -> Vec<(usize, String)> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let n = c.len();
    while i < n {
        let ch = c[i];
        if ch == '\n' {
            line += 1;
            i += 1;
        } else if ch == '/' && i + 1 < n && c[i + 1] == '/' {
            while i < n && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && i + 1 < n && c[i + 1] == '*' {
            let mut depth = 1;
            i += 2;
            while i < n && depth > 0 {
                if c[i] == '\n' {
                    line += 1;
                }
                if c[i] == '/' && i + 1 < n && c[i + 1] == '*' {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && i + 1 < n && c[i + 1] == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if (ch == 'r' || (ch == 'b' && i + 1 < n && c[i + 1] == 'r'))
            && (i == 0 || !(c[i - 1].is_alphanumeric() || c[i - 1] == '_'))
            && {
                let mut j = i + if ch == 'b' { 2 } else { 1 };
                while j < n && c[j] == '#' {
                    j += 1;
                }
                j < n && c[j] == '"'
            }
        {
            let mut j = i + if ch == 'b' { 2 } else { 1 };
            let mut hashes = 0;
            while c[j] == '#' {
                hashes += 1;
                j += 1;
            }
            j += 1;
            let start_line = line;
            let start = j;
            loop {
                if j >= n {
                    break;
                }
                if c[j] == '"' && (0..hashes).all(|h| j + 1 + h < n && c[j + 1 + h] == '#') {
                    break;
                }
                if c[j] == '\n' {
                    line += 1;
                }
                j += 1;
            }
            out.push((start_line, c[start..j.min(n)].iter().collect()));
            i = j + 1 + hashes;
        } else if ch == '"' {
            let start_line = line;
            let mut s = String::new();
            i += 1;
            while i < n && c[i] != '"' {
                if c[i] == '\\' && i + 1 < n {
                    if c[i + 1] == '\n' {
                        line += 1;
                    }
                    s.push(c[i]);
                    s.push(c[i + 1]);
                    i += 2;
                    continue;
                }
                if c[i] == '\n' {
                    line += 1;
                }
                s.push(c[i]);
                i += 1;
            }
            out.push((start_line, s));
            i += 1;
        } else if ch == '\'' {
            // a char literal ('"', '\'', 'x') or a lifetime ('a)
            if i + 1 < n && c[i + 1] == '\\' {
                i += 2;
                while i < n && c[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if i + 2 < n && c[i + 2] == '\'' {
                i += 3;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// What in this module's CODE reads an OpenAI/Anthropic key or talks to them directly: a key name or a direct host
/// as a string literal. Prose is ignored. (The Python guard also catches an SDK client constructed by name; Rust has
/// no such SDK, so a direct call shows up as its host.)
fn offences(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (line, lit) in string_literals(source) {
        if KEYS.contains(&lit.as_str()) {
            found.push(format!("line {line}: key {lit}"));
        } else if DIRECT_HOSTS.iter().any(|h| lit.starts_with(h)) {
            found.push(format!("line {line}: host {lit}"));
        }
    }
    found
}

#[test]
fn test_the_guard_catches_a_known_positive_control() {
    // What every module looked like before 2026-10-08: each pattern must be caught, or the guard proves nothing.
    let bad = r####"
use std::env;
let key = env::var("OPENAI_API_KEY");
let other = std::env::var(r#"ANTHROPIC_API_KEY"#);
const URL: &str = "https://api.openai.com/v1/responses";
let c = '"'; let s = "https://api.anthropic.com/v1/messages";
"####;
    let found = offences(bad);
    assert!(found.iter().any(|f| f.contains("OPENAI_API_KEY")), "{found:?}");
    assert!(found.iter().any(|f| f.contains("ANTHROPIC_API_KEY")), "{found:?}");
    assert!(found.iter().any(|f| f.contains("api.openai.com")), "{found:?}");
    assert!(found.iter().any(|f| f.contains("line 6: host https://api.anthropic.com")), "{found:?}");
    // prose is not code: a doc comment or a comment that names the old key is fine
    assert!(offences("//! Set OPENAI_API_KEY for the desk voice.\n// OPENAI_API_KEY\n/* ANTHROPIC_API_KEY */\nfn x<'a>() {}\n")
        .is_empty());
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Whether a file is one of the allowed modules: `<module>.rs` anywhere, a file inside a `<module>/` directory (a
/// module split into submodules, like eve-brain's `voice_agent/live.rs`), or the root of the crate named after it
/// (a single-module crate keeps its code in src/lib.rs).
fn allowed(path: &Path, crates: &Path) -> Option<&'static str> {
    let stem = path.file_stem()?.to_str()?;
    let rel = path.strip_prefix(crates).ok()?;
    let krate = rel.components().next()?.as_os_str().to_str()?.to_string();
    let dirs: Vec<&str> = rel.parent()?.components().filter_map(|c| c.as_os_str().to_str()).collect();
    ALLOWED.iter().copied().find(|m| {
        stem == *m
            || dirs.contains(m)
            || ((stem == "lib" || stem == "main") && krate == format!("eve-{}", m.replace('_', "-")))
    })
}

/// Whether a file under `src/` is a `#[cfg(test)]` module (`tests.rs`, `tests_*.rs`, `test_support.rs`, or anything in
/// a `tests/` directory). Python's guard scans the package and never `bridge/tests`; these are Rust's equivalent.
fn is_test_module(path: &Path, crates: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { return false };
    let in_tests_dir = path
        .strip_prefix(crates)
        .ok()
        .and_then(|rel| rel.parent())
        .is_some_and(|dir| dir.components().skip(2).any(|c| c.as_os_str() == "tests"));
    stem == "tests" || stem.starts_with("tests_") || stem == "test_support" || in_tests_dir
}

#[test]
fn test_the_scope_is_production_modules_and_split_allowed_modules() {
    let crates = Path::new("/w/crates");
    // positive controls: test modules are out of scope, production modules are in
    for t in ["eve-x/src/tests.rs", "eve-x/src/tests_call.rs", "eve-x/src/test_support.rs", "eve-x/src/tests/live.rs"] {
        assert!(is_test_module(&crates.join(t), crates), "{t}");
    }
    for p in ["eve-x/src/lib.rs", "eve-x/src/telegram/inlet/call.rs", "eve-x/src/contests.rs", "eve-tests/src/lib.rs"] {
        assert!(!is_test_module(&crates.join(p), crates), "{p}");
    }
    assert_eq!(allowed(&crates.join("eve-brain/src/voice_agent/live.rs"), crates), Some("voice_agent"));
    assert_eq!(allowed(&crates.join("eve-phone-call/src/lib.rs"), crates), Some("phone_call"));
    assert_eq!(allowed(&crates.join("eve-brain/src/telegram/inlet/call.rs"), crates), None);
}

#[test]
fn test_no_production_module_reads_the_openai_or_anthropic_key_except_the_allowed() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut files = Vec::new();
    for e in std::fs::read_dir(&crates).unwrap().flatten() {
        rust_sources(&e.path().join("src"), &mut files);
    }
    files.retain(|p| !is_test_module(p, &crates));
    files.sort();
    assert!(files.len() > 100); // the workspace, not an empty folder
    let mut bad: Vec<(String, Vec<String>)> = Vec::new();
    let mut seen_allowed: HashMap<&str, bool> = HashMap::new();
    for p in &files {
        let Ok(text) = std::fs::read_to_string(p) else { continue };
        let found = offences(&text);
        match allowed(p, &crates) {
            Some(m) => {
                *seen_allowed.entry(m).or_default() |= !found.is_empty();
            }
            None if !found.is_empty() => bad.push((p.strip_prefix(&crates).unwrap().display().to_string(), found)),
            None => {}
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
    // each allowed module still needs its exception; one that no longer does should leave the list. Only modules
    // already ported are checked: the rest of the workspace is ported crate by crate.
    for (m, needs) in seen_allowed {
        if !needs {
            // a crate root of an allowed module may be split across files; check the whole crate before failing
            let krate = crates.join(format!("eve-{}", m.replace('_', "-")));
            let mut all = Vec::new();
            rust_sources(&krate.join("src"), &mut all);
            all.retain(|p| !is_test_module(p, &crates));
            let any = all.iter().any(|p| std::fs::read_to_string(p).map(|t| !offences(&t).is_empty()).unwrap_or(false))
                || files.iter().any(|p| {
                    p.file_stem().and_then(|s| s.to_str()) == Some(m)
                        && std::fs::read_to_string(p).map(|t| !offences(&t).is_empty()).unwrap_or(false)
                });
            assert!(any, "{m}");
        }
    }
}

#[test]
fn test_google_models_prefer_ai_studio_and_keep_fallbacks() {
    // 2026-10-08: Vertex "global" was 0% up for 5 minutes and every web search waited on it.
    assert_eq!(
        openrouter::provider_prefs("google/gemini-3.1-flash-lite"),
        Some(json!({"order": ["google-ai-studio"], "allow_fallbacks": true}))
    );
    assert_eq!(openrouter::provider_prefs("openai/gpt-6-luna"), None);
}

#[test]
fn test_the_spend_provider_and_constants_are_python_s() {
    assert_eq!(openrouter::PROVIDER, "openrouter");
    assert_eq!(openrouter::KEY_ENV, "OPENROUTER_API_KEY");
    assert_eq!(openrouter::ANTHROPIC_BASE_URL, "https://openrouter.ai/api");
    assert_eq!(
        openrouter::merge_headers(&[]),
        vec![("HTTP-Referer".to_string(), "https://github.com/gurul/eve_01".to_string()), ("X-Title".to_string(), "eve".to_string())]
    );
}
