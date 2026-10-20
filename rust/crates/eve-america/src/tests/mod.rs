//! The Python tests, ported: `test_america.py` ([`america`]), `test_america_alexandria.py` ([`alexandria`]),
//! `test_america_browser.py` ([`browser`]) and `test_america_source_guard.py` ([`source_guard`]), and the browser
//! flow driven through a real headless Chromium ([`live`]). This module
//! holds the shared fixtures `test_america.py` defines (`ARGS`, `SOURCE`, `ANSWER`, `HEADERS`, `events`,
//! `sse`).

mod alexandria;
mod america;
mod browser;
mod live;
mod source_guard;

use std::future::Future;
use std::path::PathBuf;

use serde_json::{json, Value};

pub(crate) const QUESTION: &str = "Where can I read the official United States Code?";
pub(crate) const JURISDICTION: &str = "U.S. federal";
pub(crate) const ANSWER: &str = "Read the Code — including current supplements — at [OLRC](https://uscode.house.gov/).";
pub(crate) const HEADERS: &[(&str, &str)] =
    &[("content-type", "text/event-stream"), ("x-vercel-ai-ui-message-stream", "v1")];

pub(crate) fn args() -> Value {
    json!({"question": QUESTION, "jurisdiction": JURISDICTION})
}

pub(crate) fn source() -> Value {
    json!({"title": "United States Code | GovInfo", "url": "https://www.govinfo.gov/help/uscode", "currency": "unverified"})
}

pub(crate) fn source_struct() -> crate::Source {
    crate::Source {
        title: "United States Code | GovInfo".into(),
        url: "https://www.govinfo.gov/help/uscode".into(),
        currency: "unverified".into(),
    }
}

/// The first 16 characters of ANSWER (Python's `ANSWER[:16]`) and the rest.
fn answer_split() -> (String, String) {
    let head: String = ANSWER.chars().take(16).collect();
    let tail: String = ANSWER.chars().skip(16).collect();
    (head, tail)
}

pub(crate) fn events() -> Vec<Value> {
    let (head, tail) = answer_split();
    vec![
        json!({"type": "start", "messageId": "answer", "messageMetadata": {"interruptedSignature": "opaque"}}),
        json!({"type": "message-metadata", "messageMetadata": {"generation": {"america": {"encrypted": "private-opaque"}}}}),
        json!({"type": "reasoning-start", "id": "reasoning"}),
        json!({"type": "reasoning-end", "id": "reasoning"}),
        json!({"type": "tool-input-available", "toolCallId": "ground", "input": {"america": {"encrypted": "private-opaque"}}}),
        json!({"type": "data-search", "data": {"status": "done", "groundings": [source()],
                                               "sealed": {"america": {"encrypted": "private-opaque"}}}}),
        json!({"type": "data-search", "data": {"status": "done", "groundings": [source()]}}),
        json!({"type": "text-start", "id": "text"}),
        json!({"type": "text-delta", "id": "text", "delta": head}),
        json!({"type": "text-delta", "id": "text", "delta": tail}),
        json!({"type": "text-end", "id": "text"}),
        json!({"type": "finish", "messageMetadata": {"answerComplete": true}}),
        json!({"type": "message-metadata", "messageMetadata": {"conversationEnded": false, "safetyCard": null,
                                                                "followUpSuggestions": ["Learn about supplements"]}}),
        json!({"type": "message-metadata", "messageMetadata": {"diagnostics": {"outcome": "complete"}}}),
        json!({"type": "message-metadata", "messageMetadata": {"historySignature": "private-opaque"}}),
        json!("[DONE]"),
    ]
}

/// `sse(items)`: one `data:` frame per item; a string item is sent as it is.
pub(crate) fn sse_of(items: &[Value]) -> Vec<u8> {
    let mut out = String::new();
    for item in items {
        match item {
            Value::String(s) => out.push_str(&format!("data: {s}\n\n")),
            other => out.push_str(&format!("data: {other}\n\n")),
        }
    }
    out.into_bytes()
}

pub(crate) fn sse() -> Vec<u8> {
    sse_of(&events())
}

/// `asyncio.run`.
pub(crate) fn run<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a test runtime").block_on(fut)
}

/// The repository root (docs/contracts lives there).
pub(crate) fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
