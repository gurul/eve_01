//! `tests/test_america.py`: offline adapter checks; SSE/error cases are synthetic, based on an observed first
//! turn.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use super::{args, events, run, source, sse, sse_of, ANSWER, HEADERS};
use crate::*;

#[derive(Clone, Debug)]
struct Sent {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

type Respond = Box<dyn Fn(&Sent) -> Result<ChatResponse, HttpError> + Send + Sync>;

/// `httpx.MockTransport`: records every request and answers it with `respond`.
struct Mock {
    respond: Respond,
    sent: Mutex<Vec<Sent>>,
}

impl Mock {
    fn new(respond: impl Fn(&Sent) -> Result<ChatResponse, HttpError> + Send + Sync + 'static) -> Arc<Mock> {
        Arc::new(Mock { respond: Box::new(respond), sent: Mutex::new(Vec::new()) })
    }

    /// `Mock(side_effect=AssertionError("network attempted"))`: any request is recorded and refused.
    fn forbidden() -> Arc<Mock> {
        Mock::new(|_| Err(HttpError("network attempted".into())))
    }

    fn calls(&self) -> usize {
        self.sent.lock().len()
    }
}

#[async_trait]
impl ChatTransport for Mock {
    async fn post(&self, url: &str, headers: &[(&str, &str)], body: Vec<u8>, _limits: Limits) -> Result<ChatResponse, HttpError> {
        let sent = Sent {
            url: url.to_string(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body,
        };
        self.sent.lock().push(sent.clone());
        (self.respond)(&sent)
    }
}

fn headers_of(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// `httpx.Response(status, headers=, content=)`.
fn response(status: u16, headers: &[(&str, &str)], content: Vec<u8>) -> ChatResponse {
    ChatResponse {
        status,
        headers: headers_of(headers),
        body: Box::pin(futures::stream::iter(vec![Ok::<Vec<u8>, HttpError>(content)])),
    }
}

/// `ByteStream`: 7-byte chunks (splitting multibyte Unicode and SSE frames), an optional delay before each and
/// an optional error after the last; `closed` turns true when the response is dropped.
fn byte_stream(data: Vec<u8>, delay: f64, error: Option<&str>) -> (BodyStream, Arc<AtomicBool>) {
    struct Closed(Arc<AtomicBool>);
    impl Drop for Closed {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let closed = Arc::new(AtomicBool::new(false));
    let guard = Closed(closed.clone());
    let mut items: Vec<Result<Vec<u8>, HttpError>> = data.chunks(7).map(|c| Ok(c.to_vec())).collect();
    if let Some(e) = error {
        items.push(Err(HttpError(e.to_string())));
    }
    let stream = futures::stream::iter(items).then(move |item| {
        let _held = &guard;
        async move {
            if delay > 0.0 {
                tokio::time::sleep(Duration::from_secs_f64(delay)).await;
            }
            item
        }
    });
    (Box::pin(stream), closed)
}

fn handle_full(args: &Value, config: Option<AmericaConfig>, name: &str, transport: Arc<Mock>) -> Value {
    let mut selected = config.unwrap_or(AmericaConfig::on(true));
    selected.transport = Transport::Api;
    run(America::new(selected).with_transport(transport).handle(name, args)).expect("handle answers")
}

fn handle_args(args: &Value, transport: Arc<Mock>) -> Value {
    handle_full(args, None, "america_ask", transport)
}

/// `handle(response=...)`: the default ARGS answered by `make`.
fn handle_response(make: impl Fn() -> ChatResponse + Send + Sync + 'static) -> Value {
    handle_args(&args(), Mock::new(move |_| Ok(make())))
}

fn handle_sse(content: Vec<u8>) -> Value {
    handle_response(move || response(200, HEADERS, content.clone()))
}

fn handle_default() -> Value {
    handle_sse(sse())
}

fn assert_failure(result: &Value, status: &str) {
    assert_eq!(result["ok"], json!(false), "{result}");
    assert_eq!(result["status"], json!(status), "{result}");
    let keys: Vec<&str> = result.as_object().unwrap().keys().map(String::as_str).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["america_url", "ok", "reason", "status"]);
    assert_eq!(result["america_url"], json!("https://america.gov/chat"));
    assert!(!result.to_string().contains("private"), "{result}");
}

fn upstream(result: &Value) {
    assert_failure(result, "upstream_unavailable");
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn test_feature_is_explicitly_opt_in_and_has_no_startup_request() {
    assert!(make_america(Some(&env(&[]))).unwrap().is_none());
    assert!(make_america(Some(&env(&[("EVE_AMERICA", "0")]))).unwrap().is_none());
    for flag in ["1", "true", " yes ", "ON"] {
        assert!(make_america(Some(&env(&[("EVE_AMERICA", flag)]))).unwrap().unwrap().config.enabled);
    }
    let client = Mock::forbidden();
    let service = America::new(AmericaConfig::default()).with_transport(client.clone());
    assert!(service.tools().is_empty() && service.instructions().is_empty());
    assert_failure(&run(service.handle("america_ask", &args())).unwrap(), "unavailable");
    assert_eq!(client.calls(), 0);
}

#[test]
fn test_tool_returns_independent_strict_schema_copies() {
    let service = America::new(AmericaConfig::on(true));
    let mut tool = service.tools().remove(0);
    assert_eq!(tool["strict"], json!(true));
    assert_eq!(tool["parameters"]["additionalProperties"], json!(false));
    let mut required: Vec<String> =
        tool["parameters"]["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    let mut properties: Vec<String> = tool["parameters"]["properties"].as_object().unwrap().keys().cloned().collect();
    required.sort();
    properties.sort();
    assert_eq!(required, properties);
    assert_eq!(required, vec!["jurisdiction", "question"]);
    tool["parameters"]["properties"]["question"]["maxLength"] = json!(99);
    assert_eq!(service.tools()[0]["parameters"]["properties"]["question"]["maxLength"], json!(MAX_QUESTION_CHARS));
}

#[test]
fn test_invalid_requests_do_not_send_or_answer() {
    let a = args();
    let with = |key: &str, value: Value| {
        let mut v = a.clone();
        v[key] = value;
        v
    };
    let cases = vec![
        Value::Null,
        json!([]),
        json!({}),
        json!({"question": "q"}),
        with("history", json!(["private memory"])),
        with("attachments", json!(["private.pdf"])),
        with("question", Value::Null),
        with("question", json!(123)),
        with("question", json!("")),
        with("question", json!("   ")),
        with("question", json!("q".repeat(MAX_QUESTION_CHARS + 1))),
        with("jurisdiction", json!(123)),
        with("jurisdiction", json!([])),
        with("jurisdiction", json!("j".repeat(MAX_JURISDICTION_CHARS + 1))),
        with("jurisdiction", json!("")),
    ];
    for case in cases {
        let request = Mock::forbidden();
        assert_failure(&handle_args(&case, request.clone()), "invalid_request");
        assert_eq!(request.calls(), 0, "{case}");
    }
}

#[test]
fn test_unknown_tool_is_rejected_without_http() {
    let request = Mock::forbidden();
    assert_failure(&handle_full(&args(), None, "america_submit", request.clone()), "invalid_request");
    assert_eq!(request.calls(), 0);
}

#[test]
fn test_unknown_jurisdiction_requests_clarification_without_http() {
    for jurisdiction in [Value::Null, json!("   ")] {
        let request = Mock::forbidden();
        let mut a = args();
        a["jurisdiction"] = jurisdiction;
        let result = handle_args(&a, request.clone());
        assert_failure(&result, "needs_jurisdiction");
        let reason = result["reason"].as_str().unwrap();
        assert!(reason.contains("state or city") && reason.contains("federal"));
        assert_eq!(request.calls(), 0);
    }
}

#[test]
fn test_real_request_shape_and_unicode_answer_with_sources() {
    let transport = Mock::new(|_| Ok(response(200, HEADERS, sse())));
    let result = handle_args(&json!({"question": " Where are\n the rules? ", "jurisdiction": " U.S.  federal "}), transport.clone());
    assert_eq!(result["ok"], json!(true));
    assert_eq!(result["status"], json!("answered"));
    assert_eq!(result["provider"], json!("america_gov"));
    assert_eq!(result["answer"], json!(ANSWER));
    assert_eq!(result["sources"], json!([source()]));
    assert_eq!(result["question"], json!("Where are\n the rules?"));
    assert_eq!(result["jurisdiction"], json!("U.S. federal"));
    assert_eq!(result["legal_advice"], json!(false));
    let at = chrono::DateTime::parse_from_rfc3339(result["retrieved_at"].as_str().unwrap()).unwrap();
    assert_eq!(at.offset().local_minus_utc(), 0);
    assert!(result["retrieved_at"].as_str().unwrap().ends_with("+00:00"));
    assert!(!result.to_string().contains("private-opaque"));
    let sent = transport.sent.lock().clone();
    assert_eq!(sent.len(), 1);
    let request = &sent[0];
    assert_eq!(request.url, API_URL);
    let header = |name: &str| {
        request.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
    };
    assert_eq!(header("accept").as_deref(), Some("text/event-stream"));
    assert!(header("user-agent").unwrap().starts_with("Eve/"));
    assert!(header("cookie").is_none() && header("authorization").is_none());
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    let keys: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["language", "id", "messages", "trigger"]);
    assert_eq!(body["language"], json!("en"));
    assert_eq!(body["trigger"], json!("submit-message"));
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    let message = &body["messages"][0];
    let mut keys: Vec<&str> = message.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, vec!["id", "parts", "role"]);
    assert_eq!(message["role"], json!("user"));
    assert_eq!(message["parts"], json!([{"type": "text", "text": "Jurisdiction: U.S. federal\nQuestion: Where are\n the rules?"}]));
    assert_eq!(body["id"].as_str().unwrap().len(), 16);
    assert_eq!(message["id"].as_str().unwrap().len(), 16);
    assert!(body["id"].as_str().unwrap().chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(body["id"], message["id"]);
    assert_ne!(chat_request("q", "California")["id"], body["id"]);
    // httpx 0.28's json= body: compact separators, non-ASCII as it is
    assert_eq!(request.body, serde_json::to_vec(&body).unwrap());
}

#[test]
fn test_maximum_inputs_are_sent_in_full_without_query_truncation() {
    let transport = Mock::new(|_| Ok(response(200, HEADERS, sse())));
    let (question, jurisdiction) = ("q".repeat(MAX_QUESTION_CHARS), "j".repeat(MAX_JURISDICTION_CHARS));
    assert_eq!(handle_args(&json!({"question": question, "jurisdiction": jurisdiction}), transport.clone())["ok"], json!(true));
    let body: Value = serde_json::from_slice(&transport.sent.lock()[0].body).unwrap();
    assert_eq!(body["messages"][0]["parts"][0]["text"], json!(format!("Jurisdiction: {jurisdiction}\nQuestion: {question}")));
}

#[test]
fn test_chunked_utf8_crlf_stream_is_consumed_through_done_and_closed() {
    let crlf: Vec<u8> = String::from_utf8(sse()).unwrap().replace('\n', "\r\n").into_bytes();
    let closed = Arc::new(Mutex::new(None::<Arc<AtomicBool>>));
    let seen = closed.clone();
    let result = handle_response(move || {
        let (body, flag) = byte_stream(crlf.clone(), 0.0, None);
        *seen.lock() = Some(flag);
        ChatResponse { status: 200, headers: headers_of(HEADERS), body }
    });
    assert_eq!(result["answer"], json!(ANSWER));
    assert_eq!(result["sources"], json!([source()]));
    assert!(closed.lock().as_ref().unwrap().load(Ordering::SeqCst));
}

#[test]
fn test_http_blockage_and_redirects_are_not_retried_or_followed() {
    for status in [301u16, 302, 307, 403, 418, 429, 500, 503] {
        let transport = Mock::new(move |_| {
            Ok(response(status, &[("location", "https://other.gov/private")], b"private body and cookie".to_vec()))
        });
        let result = handle_args(&args(), transport.clone());
        upstream(&result);
        assert!(result["reason"].as_str().unwrap().contains(&status.to_string()));
        assert_eq!(transport.calls(), 1);
    }
}

#[test]
fn test_unsupported_transport_is_a_handoff() {
    let cases: Vec<Vec<(&str, &str)>> = vec![
        vec![("content-type", "text/html")],
        vec![("content-type", "application/json")],
        vec![("content-type", "text/event-stream")],
        vec![("content-type", "text/event-stream"), ("x-vercel-ai-ui-message-stream", "v2")],
    ];
    for headers in cases {
        let headers: Vec<(String, String)> = headers_of(&headers);
        upstream(&handle_response(move || ChatResponse {
            status: 200,
            headers: headers.clone(),
            body: Box::pin(futures::stream::iter(vec![Ok(sse())])),
        }));
    }
}

#[test]
fn test_transport_errors_do_not_leak_provider_or_user_data() {
    for error in ["private credentials", "private body"] {
        upstream(&handle_args(&args(), Mock::new(move |_| Err(HttpError(error.to_string())))));
    }
}

#[test]
fn test_truncated_network_response_discards_partial_text_and_closes() {
    let closed = Arc::new(Mutex::new(None::<Arc<AtomicBool>>));
    let seen = closed.clone();
    upstream(&handle_response(move || {
        let (body, flag) = byte_stream(sse(), 0.0, Some("private partial data"));
        *seen.lock() = Some(flag);
        ChatResponse { status: 200, headers: headers_of(HEADERS), body }
    }));
    assert!(closed.lock().as_ref().unwrap().load(Ordering::SeqCst));
}

#[test]
fn test_total_deadline_cancels_dribbling_stream_and_closes_client() {
    let closed = Arc::new(Mutex::new(None::<Arc<AtomicBool>>));
    let seen = closed.clone();
    let transport = Mock::new(move |_| {
        let (body, flag) = byte_stream(sse(), 0.02, None);
        *seen.lock() = Some(flag);
        Ok(ChatResponse { status: 200, headers: headers_of(HEADERS), body })
    });
    let config = AmericaConfig { timeout_secs: 0.01, ..AmericaConfig::on(true) };
    upstream(&handle_full(&args(), Some(config), "america_ask", transport));
    assert!(closed.lock().as_ref().unwrap().load(Ordering::SeqCst));
}

#[test]
fn test_incomplete_or_error_streams_cannot_become_legal_answers() {
    let all = events();
    let without_last = all[..all.len() - 1].to_vec();
    let mut no_finish: Vec<Value> = all.iter().filter(|e| !e.is_object() || e["type"] != "finish").cloned().collect();
    let mut with_error = without_last.clone();
    with_error.extend([json!({"type": "error", "errorText": "private error"}), json!("[DONE]")]);
    let mut with_abort = without_last.clone();
    with_abort.extend([json!({"type": "abort"}), json!("[DONE]")]);
    let mut after_done = all.clone();
    after_done.push(json!({"type": "text-delta", "id": "text", "delta": "extra"}));
    no_finish.shrink_to_fit();
    let cases: Vec<Vec<u8>> = vec![
        sse_of(&without_last),
        sse_of(&no_finish),
        sse_of(&with_error),
        sse_of(&with_abort),
        sse_of(&after_done),
        b"data: {invalid}\n\n".to_vec(),
        b"data: []\n\n".to_vec(),
        b"data: {\"type\": \"text-delta\", \"delta\": \"\xff\"}\n\n".to_vec(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    for bad in cases {
        upstream(&handle_sse(bad));
    }
}

#[test]
fn test_malformed_or_safety_events_do_not_return_an_answer() {
    let cases: Vec<(&str, Value)> = vec![
        ("finish", json!({"messageMetadata": {"answerComplete": false}})),
        ("finish", json!({"messageMetadata": {"answerComplete": 1}})),
        ("finish", json!({"messageMetadata": {}})),
        ("text-start", json!({"id": null})),
        ("text-delta", json!({"id": "unknown"})),
        ("text-delta", json!({"delta": null})),
        ("text-end", json!({"id": "unknown"})),
        ("data-search", json!({"data": {"status": "done", "groundings": null}})),
        ("data-search", json!({"data": null})),
        ("message-metadata", json!({"messageMetadata": {"safetyCard": {"private": "unsupported"}}})),
    ];
    for (kind, updates) in cases {
        let mut items = events();
        for event in items.iter_mut() {
            if event.is_object() && event["type"] == kind {
                for (k, v) in updates.as_object().unwrap() {
                    event[k] = v.clone();
                }
            }
        }
        upstream(&handle_sse(sse_of(&items)));
    }
}

#[test]
fn test_missing_answer_or_groundings_is_not_success() {
    for kind in ["text-end", "text-delta", "data-search"] {
        let items: Vec<Value> = events().into_iter().filter(|e| !e.is_object() || e["type"] != kind).collect();
        upstream(&handle_sse(sse_of(&items)));
    }
}

#[test]
fn test_limits_refuse_oversized_response_and_answer_without_truncating() {
    upstream(&handle_sse(vec![b'x'; MAX_STREAM_BYTES + 1]));
    let mut items = events();
    items[8]["delta"] = json!("x".repeat(MAX_ANSWER_CHARS + 1));
    upstream(&handle_sse(sse_of(&items)));
}

#[test]
fn test_unsafe_sources_are_filtered_with_a_known_good_positive_control() {
    let urls = [
        "http://agency.gov/info",
        "https://agency.gov.attacker.test/",
        "https://agency.gov@attacker.test/",
        "https://attacker@agency.gov/",
        "https://agency.gov:8443/",
        "https://127.0.0.1/",
        "javascript:alert(1)",
        "https://agency.gov/has space",
        "https://agency.gov:invalid/",
    ];
    for url in urls {
        let mut bad = source();
        bad["url"] = json!(url);
        let mut items = events();
        for event in items.iter_mut() {
            if event.is_object() && event["type"] == "data-search" {
                event["data"]["groundings"].as_array_mut().unwrap().push(bad.clone());
            }
        }
        let result = handle_sse(sse_of(&items));
        assert_eq!(result["sources"], json!([source()]), "{url}");
        for event in items.iter_mut() {
            if event.is_object() && event["type"] == "data-search" {
                event["data"]["groundings"] = json!([bad.clone()]);
            }
        }
        upstream(&handle_sse(sse_of(&items)));
    }
}

#[test]
fn test_multiple_text_blocks_preserve_order_and_markdown_links() {
    let mut items = events();
    items.splice(
        11..11,
        [
            json!({"type": "text-start", "id": "second"}),
            json!({"type": "text-delta", "id": "second", "delta": "More at [GovInfo](https://www.govinfo.gov/)."}),
            json!({"type": "text-end", "id": "second"}),
        ],
    );
    assert_eq!(
        handle_sse(sse_of(&items))["answer"],
        json!(format!("{ANSWER}\nMore at [GovInfo](https://www.govinfo.gov/)."))
    );
}

#[test]
fn test_overlapping_blocks_are_rejected_instead_of_reordering_text() {
    let mut items = events();
    items.splice(
        9..9,
        [
            json!({"type": "text-start", "id": "overlap"}),
            json!({"type": "text-delta", "id": "overlap", "delta": "interleaved"}),
            json!({"type": "text-end", "id": "overlap"}),
        ],
    );
    upstream(&handle_sse(sse_of(&items)));
}

#[test]
fn test_answer_limit_includes_separators_between_sequential_blocks() {
    let mut items = events();
    items[8]["delta"] = json!("x".repeat(MAX_ANSWER_CHARS / 2));
    items[9]["delta"] = json!("");
    items.splice(
        11..11,
        [
            json!({"type": "text-start", "id": "second"}),
            json!({"type": "text-delta", "id": "second", "delta": "y".repeat(MAX_ANSWER_CHARS / 2)}),
            json!({"type": "text-end", "id": "second"}),
        ],
    );
    upstream(&handle_sse(sse_of(&items)));
    let shorter: String = "y".repeat(MAX_ANSWER_CHARS / 2 - 1);
    items[12]["delta"] = json!(shorter);
    let result = handle_sse(sse_of(&items));
    assert_eq!(result["ok"], json!(true));
    assert_eq!(result["answer"].as_str().unwrap().chars().count(), MAX_ANSWER_CHARS);
}

#[test]
fn test_saved_openapi_matches_tool_and_observed_body_and_result_outcomes() {
    let spec: Value =
        serde_json::from_str(&std::fs::read_to_string(super::repo_root().join("docs/contracts/america-gov.openapi.json")).unwrap())
            .unwrap();
    assert_eq!(spec["openapi"], json!("3.1.0"));
    assert_eq!(spec["servers"][0]["url"], json!("https://america.gov"));
    let operation = &spec["paths"]["/api/chat"]["post"];
    assert!(operation["requestBody"]["content"]["application/json"]["schema"]["$ref"].as_str().unwrap().ends_with("/ChatRequest"));
    assert!(operation["responses"]["200"]["content"]["text/event-stream"].is_object());
    assert_eq!(spec["x-america-gov"]["api_status"], json!("observed_unpublished_protocol"));
    assert_eq!(spec["x-eve-interface"]["transport"], json!("in_process_function"));
    assert_eq!(spec["x-eve-interface"]["tool"], *TOOL);
    let schemas = &spec["components"]["schemas"];
    assert_eq!(schemas["AskRequest"], TOOL["parameters"]);
    let body = chat_request("question", "California");
    let mut required: Vec<&str> = schemas["ChatRequest"]["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let mut keys: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
    required.sort();
    keys.sort();
    assert_eq!(required, keys);
    assert_eq!(body["language"], schemas["ChatRequest"]["properties"]["language"]["const"]);
    assert_eq!(body["trigger"], schemas["ChatRequest"]["properties"]["trigger"]["const"]);
    let mut no_jurisdiction = args();
    no_jurisdiction["jurisdiction"] = Value::Null;
    let outcomes = vec![
        handle_default(),
        handle_args(&no_jurisdiction, Mock::forbidden()),
        handle_args(&json!({}), Mock::forbidden()),
        handle_full(&args(), Some(AmericaConfig::on(false)), "america_ask", Mock::forbidden()),
        handle_response(|| response(418, &[], Vec::new())),
    ];
    for result in outcomes {
        let name = spec["x-eve-interface"]["result_schemas"][result["status"].as_str().unwrap()].as_str().unwrap().to_string();
        let schema = &schemas[&name];
        assert_eq!(schema["additionalProperties"], json!(false));
        let props = schema["properties"].as_object().unwrap();
        for required in schema["required"].as_array().unwrap() {
            assert!(result.get(required.as_str().unwrap()).is_some(), "{name} needs {required}");
        }
        for (key, value) in result.as_object().unwrap() {
            let prop = props.get(key).unwrap_or_else(|| panic!("{name} has no {key}"));
            if let Some(c) = prop.get("const") {
                assert_eq!(value, c);
            }
            if let Some(e) = prop.get("enum") {
                assert!(e.as_array().unwrap().contains(value));
            }
        }
    }
}

#[test]
fn test_transport_configuration_is_explicit_and_invalid_modes_rejected() {
    assert_eq!(configured(Some(&env(&[]))).unwrap().transport, Transport::Alexandria);
    assert_eq!(configured(Some(&env(&[("EVE_AMERICA_TRANSPORT", "api")]))).unwrap().transport, Transport::Api);
    assert_eq!(configured(Some(&env(&[("EVE_AMERICA_TRANSPORT", " Browser ")]))).unwrap().transport, Transport::Browser);
    let err = configured(Some(&env(&[("EVE_AMERICA_TRANSPORT", "stealth")]))).unwrap_err();
    assert!(err.to_string().contains("alexandria, browser or api"));
    // set but empty is not the default
    assert!(configured(Some(&env(&[("EVE_AMERICA_TRANSPORT", "")]))).is_err());
    assert!(make_america(Some(&env(&[("EVE_AMERICA", "1"), ("EVE_AMERICA_TRANSPORT", "stealth")]))).is_err());
}

// ---- parity additions ----------------------------------------------------------------------------------------

/// One loopback HTTP exchange: what the server read, and the reply it sent.
fn serve_once(reply: &'static [u8]) -> (String, std::thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/api/chat", listener.local_addr().unwrap().port());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&got).to_string();
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                    .unwrap_or(0);
                if got.len() >= end + 4 + length {
                    break;
                }
            }
        }
        stream.write_all(reply).unwrap();
        stream.flush().unwrap();
        got
    });
    (url, server)
}

#[test]
fn test_http_transport_posts_the_headers_and_streams_the_reply() {
    let mut reply = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream; charset=utf-8\r\nx-vercel-ai-ui-message-stream: v1\r\nconnection: close\r\n\r\n".to_vec();
    reply.extend(sse());
    let reply: &'static [u8] = Box::leak(reply.into_boxed_slice());
    let (url, server) = serve_once(reply);
    let limits = Limits { total_secs: 10.0, connect_secs: 10.0, read_secs: 15.0 };
    let headers = [("Accept", "text/event-stream"), ("User-Agent", "Eve/0.1 America.gov integration"), ("Content-Type", "application/json")];
    let (status, content_type, body) = run(async {
        let mut r = HttpTransport.post(&url, &headers, b"{\"a\":1}".to_vec(), limits).await.unwrap();
        let mut body = Vec::new();
        while let Some(chunk) = r.body.next().await {
            body.extend(chunk.unwrap());
        }
        (r.status, r.header("Content-Type"), body)
    });
    let request = String::from_utf8(server.join().unwrap()).unwrap();
    assert_eq!(status, 200);
    assert_eq!(content_type, "text/event-stream; charset=utf-8");
    assert_eq!(parse_stream(&body).unwrap(), (ANSWER.to_string(), vec![super::source_struct()]));
    let lower = request.to_ascii_lowercase();
    assert!(request.starts_with("POST /api/chat HTTP/1.1\r\n"));
    assert!(lower.contains("\r\naccept: text/event-stream\r\n"));
    assert!(lower.contains("\r\nuser-agent: eve/0.1 america.gov integration\r\n"));
    assert!(lower.contains("\r\ncontent-type: application/json\r\n"));
    assert!(!lower.contains("\r\ncookie:"));
    assert!(request.ends_with("{\"a\":1}"));
}

#[test]
fn test_http_transport_does_not_follow_a_redirect() {
    let (url, server) = serve_once(b"HTTP/1.1 307 Temporary Redirect\r\nlocation: http://127.0.0.1:9/private\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    let limits = Limits { total_secs: 10.0, connect_secs: 10.0, read_secs: 15.0 };
    let status = run(async { HttpTransport.post(&url, &[], Vec::new(), limits).await.unwrap().status });
    server.join().unwrap();
    assert_eq!(status, 307);
}

#[test]
fn test_parse_stream_reports_why_without_the_body() {
    assert_eq!(parse_stream(b"data: [DONE]\n\ndata: [DONE]\n\n").unwrap_err(), ProtocolError("Data after DONE.".into()));
    assert_eq!(parse_stream(b"\xff").unwrap_err(), ProtocolError("Invalid UTF-8.".into()));
    assert_eq!(parse_stream(b"data: {\"type\": 1}\n\n").unwrap_err(), ProtocolError("Invalid event.".into()));
    // a data line without the space, a comment line, and an empty data frame are all SSE
    let mut stream = b": comment\n\ndata:\n\n".to_vec();
    stream.extend(String::from_utf8(sse()).unwrap().replace("data: ", "data:").into_bytes());
    assert_eq!(parse_stream(&stream).unwrap().0, ANSWER);
}
