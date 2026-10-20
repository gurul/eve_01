//! The whole browser flow through a real Chrome: eve-browser-lane's own page in Playwright's headless Chromium,
//! this crate's DevTools session for routes and request events, and a loopback chat page standing in for
//! America.gov (the flow is pointed at it with `with_site`). Run with `cargo test -p eve-america -- --ignored`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use eve_browser_lane::cdp::CdpDriver;
use eve_browser_lane::driver::Driver;
use eve_browser_lane::{BrowserLaneConfig, LaneCore, LaneOptions};
use eve_desktop_lanes::fast_lane::StepAsk;
use eve_typed_ask::StepAnswer;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::Value;

use super::{run, sse, source_struct, ANSWER, JURISDICTION, QUESTION};
use crate::america_browser::{AmericaBrowser, BrowserFailure};
use crate::chrome::ChromeLane;

/// The chat page: America.gov's form, and a submit that posts one fresh turn (or, `history`, two) and reads
/// the whole streamed reply.
fn page(field: &str, button: &str, history: bool) -> String {
    format!(
        r#"<!doctype html><html><head><title>America.gov</title></head><body>
<form data-slot="chat-input-shell" aria-label="Describe what you need">
  {field}
  {button}
</form>
<script>
document.querySelector('form').addEventListener('submit', async (e) => {{
  e.preventDefault();
  const text = document.querySelector('textarea').value;
  const messages = [{{parts: [{{type: 'text', text}}], id: crypto.randomUUID().slice(0, 16), role: 'user'}}];
  if ({history}) messages.unshift({{parts: [{{type: 'text', text: 'earlier'}}], id: 'earlier', role: 'user'}});
  const r = await fetch('/api/chat', {{method: 'POST', headers: {{'content-type': 'application/json'}},
    body: JSON.stringify({{language: 'en', id: crypto.randomUUID().slice(0, 16), messages, trigger: 'submit-message'}})}});
  const reader = r.body.getReader();
  while (!(await reader.read()).done) {{}}
}});
</script></body></html>"#
    )
}

struct Site {
    base: String,
    posts: Arc<Mutex<Vec<Value>>>,
}

fn answer_one(mut stream: TcpStream, html: &str, sse_type: &str, posts: &Mutex<Vec<Value>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/").to_string());
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
        if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
    let (head, content): (String, Vec<u8>) = match (method.as_str(), path.as_str()) {
        ("GET", "/chat") => ("200 OK\r\ncontent-type: text/html; charset=utf-8".into(), html.as_bytes().to_vec()),
        ("POST", "/api/chat") => {
            posts.lock().push(serde_json::from_slice(&body).unwrap_or(Value::Null));
            (format!("200 OK\r\ncontent-type: {sse_type}\r\nx-vercel-ai-ui-message-stream: v1"), sse())
        }
        _ => ("404 Not Found\r\ncontent-type: text/plain".into(), b"no".to_vec()),
    };
    let _ = stream.write_all(
        format!("HTTP/1.1 {head}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", content.len()).as_bytes(),
    );
    let _ = stream.write_all(&content);
}

/// Serve `html` at /chat and the fixture SSE at /api/chat with content type `sse_type`.
fn serve_as(html: String, sse_type: &'static str) -> Site {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let posts: Arc<Mutex<Vec<Value>>> = Arc::default();
    let seen = posts.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (html, seen) = (html.clone(), seen.clone());
            std::thread::spawn(move || answer_one(stream, &html, sse_type, &seen));
        }
    });
    Site { base, posts }
}

fn serve(html: String) -> Site {
    serve_as(html, "text/event-stream; charset=utf-8")
}

/// The flow over a real lane whose context is a headless Chromium of its own (attach mode's rules, a launched
/// browser).
fn browser(site: &Site, profile: &std::path::Path, asker: Option<Arc<dyn StepAsk + Send + Sync>>) -> AmericaBrowser {
    let config = BrowserLaneConfig { attach: true, headless: true, profile: profile.to_path_buf(), ..BrowserLaneConfig::default() };
    let mut driver = CdpDriver::new();
    let dir = profile.to_path_buf();
    let options = LaneOptions {
        launcher: Some(Box::new(move || driver.launch_persistent_context(&dir, true, (1280, 800), &[]))),
        ..LaneOptions::default()
    };
    let lane = ChromeLane::from_core(LaneCore::new(config, options).unwrap()).unwrap();
    AmericaBrowser::with_options(Arc::new(lane), asker, 30.0, Arc::new(eve_desktop_lanes::perf_counter))
        .with_site(&format!("{}/chat", site.base), &format!("{}/api/chat", site.base))
}

const FIELD: &str = r#"<textarea name="message" rows="3"></textarea>"#;
const SEND: &str = r#"<button type="submit" aria-label="Send message">&#x27a4;</button>"#;

fn text_of(post: &Value) -> Value {
    post["messages"][0]["parts"][0]["text"].clone()
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_live_flow_sends_once_through_the_route_and_reads_the_finished_stream() {
    let site = serve(page(FIELD, SEND, false));
    let profile = tempfile::tempdir().unwrap();
    let service = browser(&site, profile.path(), None);
    run(async {
        assert_eq!(service.ask(QUESTION, JURISDICTION).await.unwrap(), (ANSWER.to_string(), vec![source_struct()]));
        assert_eq!(service.ask("Second question?", "California").await.unwrap().0, ANSWER);
        service.close().await.unwrap();
    });
    let posts = site.posts.lock();
    assert_eq!(posts.len(), 2);
    assert_eq!(text_of(&posts[0]), Value::from(format!("Jurisdiction: {JURISDICTION}\nQuestion: {QUESTION}")));
    assert_eq!(text_of(&posts[1]), Value::from("Jurisdiction: California\nQuestion: Second question?"));
    assert_eq!(service.last_metrics()["jev_calls"], Value::from(0));
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_live_restored_history_is_aborted_before_it_leaves_chrome() {
    let site = serve(page(FIELD, SEND, true));
    let profile = tempfile::tempdir().unwrap();
    let service = browser(&site, profile.path(), None);
    let err = run(async {
        let err = service.ask(QUESTION, JURISDICTION).await.unwrap_err();
        service.close().await.unwrap();
        err
    });
    let failure = err.downcast_ref::<BrowserFailure>().unwrap_or_else(|| panic!("{err:#}"));
    assert!(failure.0.contains("fresh question"), "{}", failure.0);
    assert!(site.posts.lock().is_empty(), "the aborted request reached the server");
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_live_jev_recovers_renamed_controls_by_role_and_name() {
    let field = r#"<label for="q">Your government question</label><textarea id="q" rows="3"></textarea>"#;
    let send = r#"<button type="submit"><span>Ask America</span></button>"#;
    let site = serve(page(field, send, false));
    let profile = tempfile::tempdir().unwrap();
    let calls: Arc<Mutex<Vec<IndexMap<String, String>>>> = Arc::default();
    let seen = calls.clone();
    let asker = move |_step: &str, _app: &str, _context: &str, options: &IndexMap<String, String>, _recent: &[String]| {
        seen.lock().push(options.clone());
        StepAnswer::new("0", 0.98, 0.8, 0.99, 0.0, 0.0)
    };
    let service = browser(&site, profile.path(), Some(Arc::new(asker)));
    run(async {
        assert_eq!(service.ask(QUESTION, JURISDICTION).await.unwrap().0, ANSWER);
        service.close().await.unwrap();
    });
    let calls = calls.lock();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].get("0").map(String::as_str), Some("text field: Your government question"), "{calls:?}");
    assert_eq!(calls[1].get("0").map(String::as_str), Some("button: Ask America"), "{calls:?}");
    assert_eq!(service.field_recipe().name, "Your government question");
    assert_eq!(service.send_recipe().name, "Ask America");
    assert_eq!(site.posts.lock().len(), 1);
}

/// The Vercel AI SDK names no charset on its stream. Chrome then decodes the body as windows-1252 and
/// `Network.getResponseBody` hands back that text, which Playwright's `response.body()` (what the Python flow
/// reads) re-encodes as UTF-8; this flow reads the same bytes, so a non-ASCII answer comes back exactly as
/// Python's does.
#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_live_a_stream_without_charset_reads_as_playwright_reads_it() {
    let site = serve_as(page(FIELD, SEND, false), "text/event-stream");
    let profile = tempfile::tempdir().unwrap();
    let service = browser(&site, profile.path(), None);
    let answer = run(async {
        let answer = service.ask(QUESTION, JURISDICTION).await.unwrap().0;
        service.close().await.unwrap();
        answer
    });
    assert_eq!(answer, ANSWER.replace('\u{2014}', "\u{e2}\u{20ac}\u{201d}"));
}
