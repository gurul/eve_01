//! Port of bridge/tests/test_claude_mem.py (worker discovery, event -> memory mapping, the non-blocking sink,
//! recall parsing, the SSE mirror, one live probe) and the claude-mem part of test_privacy_guards.py.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use super::*;

// ---- a MemoryBus stand-in: exact topics and "prefix*" patterns, delivered inline --------------------

#[derive(Default)]
struct TestBus {
    subs: Mutex<Vec<(Subscription, Callback)>>,
    next_id: Mutex<u64>,
}

impl Bus for TestBus {
    fn subscribe(&self, topic: &str, callback: Callback) -> Subscription {
        let mut next = self.next_id.lock();
        *next += 1;
        let sub = Subscription { topic: topic.to_string(), id: *next };
        self.subs.lock().push((sub.clone(), callback));
        sub
    }

    fn unsubscribe(&self, sub: &Subscription) {
        self.subs.lock().retain(|(s, _)| s.id != sub.id);
    }

    fn publish(&self, topic: &str, msg: Value) {
        let targets: Vec<Callback> = self
            .subs
            .lock()
            .iter()
            .filter(|(s, _)| {
                s.topic == topic || (s.topic.ends_with('*') && topic.starts_with(&s.topic[..s.topic.len() - 1]))
            })
            .map(|(_, cb)| Arc::clone(cb))
            .collect();
        for cb in targets {
            cb(topic, &msg);
        }
    }
}

fn collector(bus: &TestBus, topic: &str) -> Arc<Mutex<Vec<(String, Value)>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    bus.subscribe(topic, Arc::new(move |t: &str, m: &Value| s.lock().push((t.to_string(), m.clone()))));
    seen
}

// ---- log capture: one global subscriber; each test reads the lines naming its own worker url ----------

static LOGS: LazyLock<Mutex<Vec<u8>>> = LazyLock::new(|| Mutex::new(Vec::new()));

struct LogWriter;

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        LOGS.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_logs() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = tracing_subscriber::fmt()
            .with_writer(|| LogWriter)
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .try_init();
    });
}

fn log_lines(level: &str, needle: &str) -> Vec<String> {
    String::from_utf8_lossy(&LOGS.lock())
        .lines()
        .filter(|l| l.contains(level) && l.contains(needle))
        .map(String::from)
        .collect()
}

// ---- a stand-in for the claude-mem worker: records requests, serves canned answers ------------------

/// A real /api/search body, captured from the worker on 2026-09-15 (v13.24.23).
fn real_search_body() -> Value {
    json!({"content": [{"type": "text", "text": concat!(
        "Found 2 result(s) matching \"eve probe\" (2 obs, 0 sessions, 0 prompts)\n\n### Sep 15, 2026\n\n",
        "**General**\n| ID | Time | T | Title | Read |\n|----|------|---|-------|------|\n",
        "| #7 | 12:40 PM | ○ | eve saw: the chair is pushed in | ~22 |\n",
        "| #2 | 12:06 PM | ○ | eve probe | ~22 |\n")}]})
}

fn real_observation_2() -> Value {
    json!({"id": 2, "project": "eve", "text": null, "type": "discovery", "title": "eve probe",
           "subtitle": "Manual memory", "narrative": "probe: eve realtime memory bus wiring test",
           "created_at": "2026-09-15T19:06:26.217Z", "created_at_epoch": 1789499186217_i64})
}

type Requests = Arc<Mutex<Vec<(String, String, Option<Value>)>>>;

struct FakeWorker {
    url: String,
    requests: Requests,
    closing: watch::Sender<bool>,
    server: JoinHandle<()>,
}

#[derive(Clone)]
struct WorkerState {
    requests: Requests,
    sse_events: Arc<Vec<Value>>,
    sse_hold: Duration,
    closing: watch::Receiver<bool>,
}

fn json_response(code: StatusCode, body: Value) -> Response {
    (code, [("Content-Type", "application/json")], body.to_string()).into_response()
}

async fn handle(
    axum::extract::State(st): axum::extract::State<WorkerState>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let path = uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
    if method == Method::POST {
        let parsed: Value = serde_json::from_slice(if body.is_empty() { b"{}" } else { &body }).unwrap_or(Value::Null);
        let title = parsed.get("title").cloned().unwrap_or(Value::Null);
        let n = {
            let mut reqs = st.requests.lock();
            reqs.push(("POST".into(), path, Some(parsed)));
            reqs.len()
        };
        return json_response(StatusCode::OK, json!({"success": true, "id": n, "title": title}));
    }
    st.requests.lock().push(("GET".into(), path.clone(), None));
    if path.starts_with("/api/search?") {
        json_response(StatusCode::OK, real_search_body())
    } else if let Some(ident) = path.strip_prefix("/api/observation/") {
        let ident: i64 = ident.parse().unwrap_or(0);
        let mut obs = real_observation_2();
        obs["id"] = json!(ident);
        obs["title"] = json!(format!("title {ident}"));
        obs["narrative"] = json!(format!("body {ident}"));
        json_response(StatusCode::OK, obs)
    } else if path == "/stream" {
        let chunks: Vec<Result<Bytes, Infallible>> =
            st.sse_events.iter().map(|e| Ok(Bytes::from(format!("data: {}\n\n", py::dumps(e))))).collect();
        let hold = st.sse_hold;
        let mut closing = st.closing.clone();
        // hold the stream open, but never past exit
        let tail = futures::stream::once(async move {
            tokio::select! {
                _ = closing.wait_for(|v| *v) => {}
                _ = tokio::time::sleep(hold) => {}
            }
        })
        .filter_map(|_| async { None::<Result<Bytes, Infallible>> });
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/event-stream")
            .body(Body::from_stream(futures::stream::iter(chunks).chain(tail)))
            .unwrap()
    } else {
        json_response(StatusCode::NOT_FOUND, json!({"error": "no"}))
    }
}

impl FakeWorker {
    async fn start(sse_events: Vec<Value>, sse_hold: f64) -> FakeWorker {
        let requests: Requests = Arc::default();
        let (closing, closing_rx) = watch::channel(false);
        let state = WorkerState {
            requests: Arc::clone(&requests),
            sse_events: Arc::new(sse_events),
            sse_hold: Duration::from_secs_f64(sse_hold),
            closing: closing_rx,
        };
        let app = axum::Router::new().fallback(handle).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeWorker { url, requests, closing, server }
    }

    fn posts(&self) -> Vec<Value> {
        self.requests
            .lock()
            .iter()
            .filter(|(m, p, _)| m == "POST" && p == "/api/memory/save")
            .filter_map(|(_, _, b)| b.clone())
            .collect()
    }

    fn paths(&self, prefix: &str) -> Vec<String> {
        self.requests.lock().iter().filter(|(_, p, _)| p.starts_with(prefix)).map(|(_, p, _)| p.clone()).collect()
    }
}

impl Drop for FakeWorker {
    fn drop(&mut self) {
        let _ = self.closing.send(true);
        self.server.abort();
    }
}

fn closed_port_url() -> String {
    let s = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = s.local_addr().unwrap().port();
    drop(s);
    format!("http://127.0.0.1:{port}")
}

async fn wait_until(mut predicate: impl FnMut() -> bool, timeout: f64) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    predicate()
}

fn sink_opts(url: &str) -> SinkOptions {
    SinkOptions { url: Some(url.to_string()), ..SinkOptions::default() }
}

fn mirror_opts(url: &str, reconnect_secs: f64) -> MirrorOptions {
    MirrorOptions {
        url: Some(url.to_string()),
        reconnect_secs: Duration::from_secs_f64(reconnect_secs),
        ..MirrorOptions::default()
    }
}

fn mem(topic: &str, msg: Value) -> Option<Value> {
    to_memory(topic, &msg, "eve").expect("no TypeError")
}

// ---- worker_url ------------------------------------------------------------------------------

#[test]
fn test_worker_url_precedence_env_settings_pid_default() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let none = |_: &str| None;
    assert_eq!(worker_url_from(&none, Some(home)), "http://127.0.0.1:37777");
    std::fs::create_dir(home.join(".claude-mem")).unwrap();
    std::fs::write(home.join(".claude-mem/worker.pid"), py::dumps(&json!({"pid": 1, "port": 37701}))).unwrap();
    assert_eq!(worker_url_from(&none, Some(home)), "http://127.0.0.1:37701");
    std::fs::write(home.join(".claude-mem/settings.json"), py::dumps(&json!({"CLAUDE_MEM_WORKER_PORT": "37702"})))
        .unwrap();
    assert_eq!(worker_url_from(&none, Some(home)), "http://127.0.0.1:37702");
    let env: HashMap<&str, &str> = HashMap::from([("EVE_CLAUDE_MEM_URL", "http://127.0.0.1:4000/")]);
    let lookup = |k: &str| env.get(k).map(|v| v.to_string());
    assert_eq!(worker_url_from(&lookup, Some(home)), "http://127.0.0.1:4000");
    std::fs::write(home.join(".claude-mem/settings.json"), "not json").unwrap();
    assert_eq!(worker_url_from(&none, Some(home)), "http://127.0.0.1:37701"); // falls through to the pid file
}

#[test]
fn worker_url_skips_ports_python_would_not_read() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let none = |_: &str| None;
    std::fs::create_dir(home.join(".claude-mem")).unwrap();
    std::fs::write(home.join(".claude-mem/worker.pid"), r#"{"port": " 37703 "}"#).unwrap();
    for settings in [
        r#"{"CLAUDE_MEM_WORKER_PORT": 37701.0}"#, // str(37701.0) is not an int
        r#"{"CLAUDE_MEM_WORKER_PORT": 70000}"#,   // out of range
        r#"{"CLAUDE_MEM_WORKER_PORT": true}"#,
        r#"["CLAUDE_MEM_WORKER_PORT"]"#, // not a dict
        r#"{}"#,
    ] {
        std::fs::write(home.join(".claude-mem/settings.json"), settings).unwrap();
        assert_eq!(worker_url_from(&none, Some(home)), "http://127.0.0.1:37703", "{settings}");
    }
    std::fs::write(home.join(".claude-mem/settings.json"), r#"{"CLAUDE_MEM_WORKER_PORT": 37704}"#).unwrap();
    assert_eq!(worker_url_from(&none, Some(home)), "http://127.0.0.1:37704");
    let blank = |_: &str| Some("   ".to_string());
    assert_eq!(worker_url_from(&blank, Some(home)), "http://127.0.0.1:37704"); // a blank override is no override
}

// ---- to_memory -----------------------------------------------------------------------------

#[test]
fn test_to_memory_shapes_each_topic() {
    let obs = mem(
        "/eve/memory/observation",
        json!({"thought": "The chair is pushed in again.", "observations": ["chair in", "lamp on"],
               "importance": 7, "time": 1.5}),
    )
    .unwrap();
    assert_eq!(obs["title"], "eve saw: The chair is pushed in again.");
    assert_eq!(obs["text"], "The chair is pushed in again.\n\nSeen: chair in; lamp on");
    assert_eq!(obs["project"], "eve");
    assert!(obs["metadata"]["platformSource"] == "eve" && obs["metadata"]["topic"] == "/eve/memory/observation");
    assert!(obs["metadata"]["importance"] == 7 && obs["metadata"]["time"] == 1.5);
    assert!(obs["metadata"].get("thought").is_none() && obs["metadata"].get("observations").is_none());

    // What was said is not on the bus any more (owner, 2026-09-23): a conversation topic is dropped.
    assert_eq!(
        mem("/eve/memory/conversation", json!({"title": "Coffee plans", "note": "The owner wants coffee at four."})),
        None
    );

    let lesson = mem(
        "/eve/memory/lesson",
        json!({"action": "step", "stage": "working", "topic": "Fractions", "level": "Grade 4",
               "feedback": "Add the tops, keep the bottom."}),
    )
    .unwrap();
    assert_eq!(lesson["title"], "lesson: Fractions");
    assert_eq!(lesson["text"], "step on Fractions (Grade 4): Add the tops, keep the bottom.");
    assert!(lesson["metadata"]["stage"] == "working" && lesson["metadata"]["subject"] == "Fractions");
    assert_eq!(lesson["metadata"]["topic"], "/eve/memory/lesson"); // the bus topic, never overwritten

    let keep = mem("/eve/memory/remember", json!({"text": "The blue mug is Sam's.", "title": ""})).unwrap();
    assert!(keep["title"] == "The blue mug is Sam's." && keep["text"] == "The blue mug is Sam's.");
    assert_eq!(mem("/eve/memory/remember", json!({"text": "x".repeat(100)})).unwrap()["title"], "x".repeat(60));
}

#[test]
fn test_to_memory_returns_none_for_noise() {
    let cases = [
        ("/eve/state", json!({"state": "wake"})),
        ("/claude/observation", json!({"title": "t", "text": "x"})),
        ("/eve/memory/lesson", json!({"action": "hint", "topic": "Fractions", "feedback": "Try the tops."})),
        ("/eve/memory/lesson", json!({"action": "open"})),
        ("/eve/memory/lesson", json!({"action": "listen"})),
        ("/eve/memory/lesson", json!({"action": "status"})),
        ("/eve/memory/observation", json!({"thought": "", "observations": []})),
        ("/eve/memory/conversation", json!({"note": "   "})),
        ("/eve/memory/remember", json!({"text": ""})),
        ("/eve/memory/remember", json!("not a dict")),
    ];
    for (topic, msg) in cases {
        assert_eq!(mem(topic, msg.clone()), None, "{topic} {msg}");
    }
}

#[test]
fn test_to_memory_never_carries_learner_words_even_when_offered() {
    let leaked = json!({"action": "check", "topic": "Fractions", "level": "Grade 4", "feedback": "Look at the 3.",
                        "ideas": "I think I add the tops", "spoken": ["um, four?"]});
    let body = mem("/eve/memory/lesson", leaked).unwrap();
    let flat = py::dumps(&body);
    assert!(body["metadata"].get("ideas").is_none() && body["metadata"].get("spoken").is_none());
    assert!(!flat.contains("add the tops") && !flat.contains("um, four"));
    assert_eq!(body["text"], "check on Fractions (Grade 4): Look at the 3.");
}

#[test]
fn test_to_memory_clips_long_strings_and_lists() {
    let observations: Vec<String> = (0..50).map(|i| i.to_string()).collect();
    let body = mem(
        "/eve/memory/observation",
        json!({"thought": "t".repeat(5000), "observations": observations, "tags": vec!["a"; 30],
               "note": "n".repeat(3000)}),
    )
    .unwrap();
    let text = body["text"].as_str().unwrap();
    assert!(text.chars().count() <= 4000 && text.matches(';').count() == 19); // thought clipped, 20 seen kept
    assert!(body["metadata"].get("tags").is_none()); // list too long: dropped
    assert!(body["metadata"].get("note").is_none()); // not on the allowlist
    assert!(body["metadata"].get("thought").is_none() && body["metadata"].get("observations").is_none());
}

#[test]
fn to_memory_follows_python_on_odd_values() {
    // a clipped thought ends in the ellipsis at 2000 characters
    let body = mem("/eve/memory/observation", json!({"thought": "é".repeat(2500)})).unwrap();
    let text = body["text"].as_str().unwrap();
    assert_eq!(text.chars().count(), 2000);
    assert!(text.ends_with("é…"));
    // a string of observations is iterated by character, as Python does; the title falls back to the first
    let body = mem("/eve/memory/observation", json!({"observations": "ab"})).unwrap();
    assert_eq!(body["text"], "\n\nSeen: a; b");
    assert_eq!(body["title"], "eve saw: a");
    // a dict yields its keys; a number raises TypeError
    assert_eq!(mem("/eve/memory/observation", json!({"observations": {"k": 1}})).unwrap()["text"], "\n\nSeen: k");
    assert_eq!(to_memory("/eve/memory/observation", &json!({"observations": 3}), "eve"), Err(NotIterable("int")));
    // the text falls back to "text", and non-strings are str()'d the Python way
    let body = mem("/eve/memory/observation", json!({"thought": "", "text": 42, "observations": [1.0, null]})).unwrap();
    assert_eq!(body["text"], "42\n\nSeen: 1.0; None");
    // a lesson action is case-folded; a whitespace title collapses to the bare prefix
    let body = mem("/eve/memory/lesson", json!({"action": " END ", "topic": "   "})).unwrap();
    assert_eq!(body["text"], "end on ");
    assert_eq!(body["title"], "lesson");
    let body = mem("/eve/memory/lesson", json!({"action": "recap"})).unwrap();
    assert_eq!(body["text"], "recap on a lesson");
    assert!(body["metadata"].get("subject").is_none());
    // metadata: bools count as ints in a list, None in a list drops it, dicts never ride along, time is null
    let body = mem(
        "/eve/memory/remember",
        json!({"text": "k", "tags": ["a", 1, true], "changed": ["a", null], "state": {"x": 1}, "written": false}),
    )
    .unwrap();
    let meta = &body["metadata"];
    assert_eq!(meta["tags"], json!(["a", 1, true]));
    assert!(meta.get("changed").is_none() && meta.get("state").is_none());
    assert_eq!(meta["written"], false);
    assert_eq!(meta["time"], Value::Null);
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["text", "title", "project", "metadata"]);
    // the whole text is clipped at 4000
    let body = mem("/eve/memory/remember", json!({"text": "z".repeat(4500)})).unwrap();
    assert_eq!(body["text"].as_str().unwrap().chars().count(), 4000);
}

// ---- the sink -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_sink_posts_each_memory_with_project_and_source() {
    let worker = FakeWorker::start(vec![], 0.0).await;
    let bus = Arc::new(TestBus::default());
    let mut sink = ClaudeMemSink::new(Arc::clone(&bus), sink_opts(&worker.url));
    sink.start();
    bus.publish("/eve/memory/observation", json!({"thought": "Lamp on."}));
    bus.publish("/eve/memory/lesson", json!({"action": "start", "topic": "Fractions", "level": "Grade 4"}));
    bus.publish("/eve/memory/remember", json!({"text": "Keep this."}));
    bus.publish("/eve/state", json!({"state": "wake"})); // not a memory
    bus.publish("/eve/memory/lesson", json!({"action": "hint", "feedback": "nudge"})); // not a memory
    assert!(wait_until(|| sink.stats().saved == 3, 3.0).await);
    sink.stop(Duration::from_secs(2)).await;
    let posts = worker.posts();
    assert_eq!(posts.len(), 3);
    assert!(posts.iter().all(|p| p["project"] == "eve" && p["metadata"]["platformSource"] == "eve"));
    let titles: Vec<&str> = posts.iter().map(|p| p["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["eve saw: Lamp on.", "lesson: Fractions", "Keep this."]);
    assert_eq!(sink.stats(), SinkStats { saved: 3, dropped: 0, failed: 0 });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_sink_with_the_worker_down_never_blocks_and_warns_once() {
    capture_logs();
    let url = closed_port_url();
    let bus = Arc::new(TestBus::default());
    let mut sink = ClaudeMemSink::new(
        Arc::clone(&bus),
        SinkOptions { timeout: Duration::from_secs_f64(0.5), ..sink_opts(&url) },
    );
    sink.start();
    for i in 0..5 {
        let started = Instant::now();
        bus.publish("/eve/memory/remember", json!({"text": format!("line {i}")}));
        assert!(started.elapsed() < Duration::from_millis(100));
    }
    assert!(wait_until(|| sink.stats().failed == 5, 8.0).await);
    sink.stop(Duration::from_secs(2)).await;
    let warnings: Vec<String> =
        log_lines("WARN", &url).into_iter().filter(|l| l.contains("claude-mem")).collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("could not save"));
    assert!(warnings[0].contains("further failures are counted, not logged, for 10 minutes"));
    assert_eq!(sink.stats().saved, 0);
}

#[tokio::test]
async fn test_sink_queue_overflow_drops_and_counts() {
    let bus = Arc::new(TestBus::default());
    let sink = ClaudeMemSink::new(Arc::clone(&bus), SinkOptions { queue_size: 2, ..sink_opts(&closed_port_url()) });
    let intake = Arc::clone(&sink.intake);
    // subscribe without the drain task
    let sub = bus.subscribe("/eve/memory/*", Arc::new(move |t: &str, m: &Value| intake.on_event(t, m)));
    for i in 0..5 {
        bus.publish("/eve/memory/remember", json!({"text": format!("line {i}")}));
    }
    assert!(sink.stats().dropped == 3 && sink.intake.queue.qsize() == 2);
    bus.unsubscribe(&sub);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_sink_start_and_stop_are_idempotent() {
    let worker = FakeWorker::start(vec![], 0.0).await;
    let bus = Arc::new(TestBus::default());
    let mut sink = ClaudeMemSink::new(Arc::clone(&bus), sink_opts(&worker.url));
    sink.start();
    sink.start();
    assert_eq!(bus.subs.lock().len(), 1);
    sink.stop(Duration::from_secs(2)).await;
    sink.stop(Duration::from_secs(2)).await;
    assert_eq!(bus.subs.lock().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sink_posts_the_bytes_python_json_dumps_writes() {
    let worker = FakeWorker::start(vec![], 0.0).await;
    let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let s = Arc::clone(&seen);
    let poster: Poster = Arc::new(move |url: String, body: Value, timeout: Duration| -> PostFuture {
        s.lock().push((url.clone(), body.clone()));
        Box::pin(async move { post(&url, &body, timeout).await })
    });
    let bus = Arc::new(TestBus::default());
    let mut sink = ClaudeMemSink::new(Arc::clone(&bus), sink_opts(&format!("{}/", worker.url))).with_poster(poster);
    assert_eq!(sink.url, worker.url); // the trailing slash is dropped
    sink.start();
    bus.publish("/eve/memory/remember", json!({"text": "café", "time": 2.0}));
    assert!(wait_until(|| sink.stats().saved == 1, 3.0).await);
    sink.stop(Duration::from_secs(2)).await;
    let (url, body) = seen.lock()[0].clone();
    assert_eq!(url, format!("{}/api/memory/save", worker.url));
    assert_eq!(
        py::dumps(&body),
        r#"{"text": "caf\u00e9", "title": "caf\u00e9", "project": "eve", "metadata": {"platformSource": "eve", "topic": "/eve/memory/remember", "time": 2.0}}"#
    );
    assert_eq!(worker.posts()[0], body);
}

// ---- recall -------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_recall_parses_the_search_table_and_fetches_bodies() {
    let worker = FakeWorker::start(vec![], 0.0).await;
    let results = recall("eve probe", 5, Some(&worker.url), Duration::from_secs(5), "eve").await;
    let ids: Vec<i64> = results.iter().map(|r| r.id).collect();
    assert_eq!(ids, [7, 2]); // newest first, as the table lists them
    assert!(results[0].title == "title 7" && results[0].text == "body 7");
    assert_eq!(results[1].time, "2026-09-15T19:06:26.217Z");
    let search = worker.paths("/api/search?")[0].clone();
    assert!(search.contains("query=eve+probe") && search.contains("project=eve") && search.contains("limit=5"));
    assert_eq!(
        serde_json::to_string(&results[1]).unwrap(),
        r#"{"id":2,"title":"title 2","time":"2026-09-15T19:06:26.217Z","text":"body 2"}"#
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_recall_returns_an_empty_list_when_the_worker_is_down() {
    assert_eq!(recall("anything", 5, Some(&closed_port_url()), Duration::from_secs_f64(0.5), "eve").await, vec![]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_recall_clamps_limit() {
    let worker = FakeWorker::start(vec![], 0.0).await;
    assert_eq!(recall("eve probe", 1, Some(&worker.url), Duration::from_secs(5), "eve").await.len(), 1);
    assert!(worker.paths("/api/search?").last().unwrap().contains("limit=1"));
    recall("eve probe", 99, Some(&worker.url), Duration::from_secs(5), "eve").await;
    assert!(worker.paths("/api/search?").last().unwrap().contains("limit=20"));
    recall("eve probe", 0, Some(&worker.url), Duration::from_secs(5), "eve").await; // `limit or 5`
    assert!(worker.paths("/api/search?").last().unwrap().contains("limit=5"));
    recall("eve probe", -3, Some(&worker.url), Duration::from_secs(5), "eve").await;
    assert!(worker.paths("/api/search?").last().unwrap().ends_with("limit=1"));
}

#[test]
fn recall_walks_the_answer_as_python_does() {
    let rows = parse_rows(real_search_body()["content"][0]["text"].as_str().unwrap());
    assert_eq!(
        rows,
        vec![
            Row { id: 7, time: "12:40 PM".into(), title: "eve saw: the chair is pushed in".into() },
            Row { id: 2, time: "12:06 PM".into(), title: "eve probe".into() },
        ]
    );
    assert!(parse_rows("| ID | Time | T | Title | Read |\n|----|\n  | #3|t|x|y|  \r\n").len() == 1);
    assert_eq!(search_text(&json!(null)).unwrap(), "");
    assert_eq!(search_text(&json!({})).unwrap(), "");
    assert_eq!(search_text(&json!({"content": "abc"})).unwrap(), "");
    assert_eq!(search_text(&json!({"content": [{"text": "a"}, "x", {"text": null}, {"text": 5}]})).unwrap(), "a\n\n5");
    assert_eq!(search_text(&json!([1])).unwrap_err().kind, "AttributeError");
    assert_eq!(search_text(&json!({"content": null})).unwrap_err().kind, "TypeError");
}

// ---- the mirror ---------------------------------------------------------------------------

fn obs(ident: i64, project: &str, title: &str) -> Value {
    json!({"type": "new_observation", "observation": {
        "id": ident, "memory_session_id": null, "session_id": "s", "platform_source": "claude",
        "type": "decision", "title": title, "subtitle": null, "text": null, "narrative": format!("narrative {ident}"),
        "facts": "[]", "concepts": "[]", "files_read": "[]", "files_modified": "[]", "project": project,
        "prompt_number": 1, "created_at_epoch": 1789499186217_i64}})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_mirror_republishes_other_projects_and_skips_eve() {
    let events = vec![
        json!({"type": "connected", "timestamp": 1}),
        json!({"type": "processing_status", "isProcessing": true}),
        obs(11, "debrief", "Fixed the tailer"),
        obs(12, "eve", "eve saw: lamp"),
        json!({"type": "new_summary", "summary": {"id": 3, "project": "debrief"}}),
    ];
    let worker = FakeWorker::start(events, 30.0).await;
    let bus = Arc::new(TestBus::default());
    let seen = collector(&bus, "/claude/observation");
    let mut mirror = ClaudeMemMirror::new(Arc::clone(&bus), mirror_opts(&worker.url, 0.2));
    mirror.start();
    assert!(wait_until(|| seen.lock().len() == 1, 3.0).await);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    mirror.stop(Duration::from_secs(3)).await;
    assert!(started.elapsed() < Duration::from_secs(3));
    drop(worker);
    assert_eq!(
        *seen.lock(),
        vec![(
            "/claude/observation".to_string(),
            json!({"id": 11, "title": "Fixed the tailer", "project": "debrief", "type": "decision",
                   "time": 1789499186217_i64, "text": "narrative 11"})
        )]
    );
    assert_eq!(mirror.stats().mirrored, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_mirror_reconnects_after_the_stream_ends_and_stops_cleanly() {
    let worker = FakeWorker::start(vec![obs(1, "debrief", "one")], 0.0).await;
    let bus = Arc::new(TestBus::default());
    let seen = collector(&bus, "/claude/observation");
    let mut mirror = ClaudeMemMirror::new(Arc::clone(&bus), mirror_opts(&worker.url, 0.05));
    mirror.start();
    assert!(wait_until(|| mirror.stats().reconnects >= 2, 3.0).await);
    mirror.stop(Duration::from_secs(3)).await;
    assert!(seen.lock().len() >= 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_quiet_stream_reconnects_and_keeps_mirroring_without_spinning() {
    // The worker sends one observation and then says nothing. In Python a timed-out read could never be
    // retried, and the old loop spun on it at full speed forever and never read again (2026-09-24).
    let worker = FakeWorker::start(vec![obs(1, "debrief", "one")], 30.0).await;
    let bus = Arc::new(TestBus::default());
    let seen = collector(&bus, "/claude/observation");
    let mut mirror = ClaudeMemMirror::new(
        Arc::clone(&bus),
        MirrorOptions { read_timeout: Duration::from_secs_f64(0.3), ..mirror_opts(&worker.url, 5.0) },
    );
    mirror.start();
    assert!(wait_until(|| !seen.lock().is_empty(), 3.0).await);
    let streams_before = worker.paths("/stream").len();
    tokio::time::sleep(Duration::from_secs(1)).await; // quiet: at least two reconnects, each one mirrored
    let streams_during = worker.paths("/stream").len() - streams_before;
    let started = Instant::now();
    mirror.stop(Duration::from_secs(3)).await;
    let stopped_in = started.elapsed();
    assert!(seen.lock().len() >= 3); // it kept reading after the quiet spells
    assert_eq!(mirror.stats().reconnects, 0); // a quiet stream is not a lost one
    assert!(streams_during <= 5, "{streams_during} connections in one second"); // one per quiet spell, no spin
    assert!(stopped_in < Duration::from_secs(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stop_ends_a_blocked_read_at_once() {
    let worker = FakeWorker::start(vec![obs(1, "debrief", "one")], 30.0).await;
    let bus = Arc::new(TestBus::default());
    let seen = collector(&bus, "/claude/observation");
    // the real 30 s quiet window
    let mut mirror = ClaudeMemMirror::new(
        Arc::clone(&bus),
        MirrorOptions { url: Some(worker.url.clone()), ..MirrorOptions::default() },
    );
    assert_eq!(mirror.read_timeout, Duration::from_secs(30));
    mirror.start();
    assert!(wait_until(|| seen.lock().len() == 1, 3.0).await);
    let started = Instant::now();
    mirror.stop(Duration::from_secs(3)).await;
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_mirror_survives_a_dead_worker() {
    capture_logs();
    let url = closed_port_url();
    let mut mirror = ClaudeMemMirror::new(Arc::new(TestBus::default()), mirror_opts(&url, 0.05));
    mirror.start();
    assert!(wait_until(|| mirror.stats().reconnects >= 3, 3.0).await);
    mirror.stop(Duration::from_secs(3)).await;
    let lost = log_lines("WARN", &url).into_iter().filter(|l| l.contains("mirror lost")).collect::<Vec<_>>();
    assert_eq!(lost.len(), 1, "{lost:?}");
    assert!(lost[0].contains(&format!("mirror lost {url}/stream (")) && lost[0].contains("retrying every 0 s"));
}

#[test]
fn mirror_lines_follow_python() {
    let bus = TestBus::default();
    let seen = collector(&bus, "/claude/observation");
    let counters = MirrorCounters::default();
    let line = |l: &str| on_line(&bus, &counters, "eve", l);
    line("event: new_observation");
    line("data: not json");
    line("data:");
    line("data: [1]");
    line(&format!("data: {}", json!({"type": "new_observation", "observation": "x"})));
    let mut own = obs(5, "other", "from eve");
    own["observation"]["platform_source"] = json!("eve");
    line(&format!("data: {}", own));
    assert!(seen.lock().is_empty());
    line(&format!("data:{}", json!({"type": "new_observation", "observation": {"text": "t", "project": 3}})));
    assert_eq!(
        seen.lock()[0].1,
        json!({"id": null, "title": "", "project": "3", "type": "", "time": null, "text": "t"})
    );
    assert_eq!(counters.mirrored.load(Ordering::Relaxed), 1);
}

// ---- privacy (test_privacy_guards.py: the sink check sees a leak) ---------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn privacy_guard_the_sink_check_sees_a_leak() {
    let word = format!("quokka{:012x}", std::process::id() as u64 * 7919);
    let sent: Arc<Mutex<Vec<Value>>> = Arc::default();
    let s = Arc::clone(&sent);
    let poster: Poster = Arc::new(move |_url: String, body: Value, _timeout: Duration| -> PostFuture {
        s.lock().push(body);
        Box::pin(async { Ok(json!({})) })
    });
    let bus = Arc::new(TestBus::default());
    let mut sink = ClaudeMemSink::new(Arc::clone(&bus), sink_opts("http://127.0.0.1:9")).with_poster(poster);
    sink.start();
    bus.publish("/eve/memory/remember", json!({"text": format!("a line with {word}"), "time": 1.0}));
    assert!(wait_until(|| sink.stats().saved == 1, 3.0).await);
    sink.stop(Duration::from_secs(5)).await;
    assert!(sent.lock().iter().any(|b| py::dumps(b).contains(&word)));
}

#[test]
fn test_module_never_names_the_learner_word_fields_in_payload_code() {
    // Privacy line: the learner-word field names stay in the lesson store, never in a payload builder.
    let source = include_str!("lib.rs");
    let code_only: Vec<&str> = source.lines().filter(|l| !l.trim_start().starts_with("//")).collect();
    let code_only = code_only.join("\n");
    let body = code_only.split_once("fn clip").expect("clip is defined").1; // everything after the docs
    let (ideas, spoken) = (["id", "eas"].concat(), ["spo", "ken"].concat());
    assert!(!body.contains(&format!("\"{ideas}\"")) && !body.contains(&format!("'{ideas}'")));
    assert!(!body.contains(&spoken));
}

// ---- live ---------------------------------------------------------------------------------

/// True when the worker answers /health with status ok.
async fn worker_up(url: &str) -> bool {
    matches!(get(&format!("{}/health", url.trim_end_matches('/')), Duration::from_secs(2)).await,
             Ok(v) if v.get("status") == Some(&json!("ok")))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: needs the owner's claude-mem worker running"]
async fn test_live_save_then_recall_by_title() {
    let live_url = worker_url(); // resolved only when the live test is asked for
    if !worker_up(&live_url).await {
        eprintln!("claude-mem worker is not running");
        return;
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let title = format!("eve live probe {stamp}");
    let bus = Arc::new(TestBus::default());
    let mut sink = ClaudeMemSink::new(Arc::clone(&bus), sink_opts(&live_url));
    sink.start();
    bus.publish(
        "/eve/memory/remember",
        json!({"text": format!("live probe from test_claude_mem at {stamp}; safe to delete"), "title": title}),
    );
    assert!(wait_until(|| sink.stats().saved == 1, 5.0).await, "{:?}", sink.stats());
    sink.stop(Duration::from_secs(2)).await;
    let mut hits = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        hits = recall(&title, 5, Some(&live_url), Duration::from_secs(5), "eve")
            .await
            .into_iter()
            .filter(|r| r.title == title)
            .collect();
        if !hits.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    println!("\nLIVE {live_url}: saved {:?}, recall found {}: {:?}", sink.stats(), hits.len(), hits.first());
    let ok = !hits.is_empty() && hits[0].text.starts_with("live probe from test_claude_mem");
    // Leave nothing behind in the owner's memory: every run would otherwise add one probe.
    for hit in &hits {
        let _ = CLIENT.delete(format!("{live_url}/api/observation/{}", hit.id)).send().await;
    }
    assert!(ok);
}
