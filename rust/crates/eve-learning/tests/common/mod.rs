//! Shared helpers for the learning tests: an app in a temp folder, a raw HTTP client (urllib does not normalize
//! "..", and neither may the client here), fake TinyFish and OpenRouter transports, and a fake listener.
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use eve_learning::server::{LearningApp, LearningServer, Listener, StartOptions};
use eve_learning::tutor::{PostError, TutorHttp};
use eve_web::{Ctx, FetchError, HttpOptions};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

pub fn obj(v: Value) -> Map<String, Value> {
    v.as_object().expect("an object").clone()
}

/// A context with exactly these environment variables, no network, and a ledger in `ledger`.
pub fn ctx(env: &[(&str, &str)], ledger: &Path) -> Ctx {
    let env: HashMap<String, String> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let refuse = |url: &str, _: &HttpOptions| -> Result<(u16, String), FetchError> {
        panic!("unexpected request to {url}")
    };
    Ctx::global().clone().with_env(env).with_http(Arc::new(refuse)).with_ledger(eve_spend::Ledger::at(ledger))
}

pub fn opts(dir: &Path, demo: bool) -> StartOptions {
    StartOptions {
        directory: Some(dir.to_path_buf()),
        demo,
        port: 0,
        ctx: ctx(&[], &dir.join("spend")),
        opener: Arc::new(|_: &str| {}),
        ..StartOptions::default()
    }
}

pub fn demo_app(dir: &Path) -> LearningApp {
    LearningApp::new(opts(dir, true)).unwrap()
}

pub fn dispatch(app: &LearningApp, v: Value) -> Result<Map<String, Value>, eve_learning::LearningError> {
    app.dispatch(&obj(v))
}

/// `create(app, mode, topic)`: a lesson, generated when it is a learn lesson.
pub fn create(app: &LearningApp, mode: &str, topic: &str) -> Map<String, Value> {
    let s = dispatch(app, json!({"action": "create", "mode": mode, "topic": topic, "level": "Grades 6-8"})).unwrap();
    if mode == "learn" {
        dispatch(app, json!({"id": s["id"], "action": "generate"})).unwrap()
    } else {
        s
    }
}

pub fn err_text(r: Result<Map<String, Value>, eve_learning::LearningError>) -> String {
    match r {
        Ok(v) => panic!("expected an error, got {v:?}"),
        Err(e) => e.to_string(),
    }
}

/// One HTTP exchange: (status, headers lowercased, body).
pub struct HttpReply {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpReply {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }
    pub fn content_type(&self) -> String {
        self.headers.get("content-type").cloned().unwrap_or_default().split(';').next().unwrap_or("").trim().to_string()
    }
    pub fn header(&self, name: &str) -> String {
        self.headers.get(name).cloned().unwrap_or_default()
    }
}

/// A raw request to the server: `target` is sent as it is. `Host` is the server's unless `headers` sets it.
pub fn request(server: &LearningServer, method: &str, target: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> HttpReply {
    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut head = format!("{method} {target} HTTP/1.1\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        head.push_str(&format!("Host: 127.0.0.1:{}\r\n", server.port));
    }
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        head.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    if let Some(b) = body {
        stream.write_all(b).unwrap();
    }
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("a header block");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect::<HashMap<_, _>>();
    HttpReply { status, headers, body: raw[split + 4..].to_vec() }
}

pub fn get(server: &LearningServer, path: &str) -> HttpReply {
    request(server, "GET", path, &[], None)
}

pub fn get_json(server: &LearningServer, path: &str) -> Value {
    let r = get(server, path);
    assert_eq!(r.status, 200, "{path}: {}", String::from_utf8_lossy(&r.body));
    r.json()
}

pub fn post(server: &LearningServer, headers: &[(&str, &str)], body: &Value) -> HttpReply {
    request(server, "POST", "/api/action", headers, Some(body.to_string().as_bytes()))
}

pub fn post_ok(server: &LearningServer, token: &str, body: &Value) -> Value {
    let r = post(server, &[("X-Eve-Token", token)], body);
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    r.json()
}

/// One recorded POST: url, headers, JSON body.
pub type Call = (String, Vec<(String, String)>, Value);

/// A fake OpenRouter POST: answers from `replies` in order, records (url, headers, body).
#[derive(Default)]
pub struct FakePost {
    pub replies: Mutex<VecDeque<Result<Vec<u8>, PostError>>>,
    pub calls: Mutex<Vec<Call>>,
}

impl FakePost {
    pub fn answering(replies: Vec<Result<Vec<u8>, PostError>>) -> Arc<FakePost> {
        Arc::new(FakePost { replies: Mutex::new(replies.into()), calls: Mutex::new(Vec::new()) })
    }
    pub fn reply(content: &Value, finish: &str) -> Result<Vec<u8>, PostError> {
        Ok(json!({"choices": [{"finish_reason": finish, "message": {"content": content.to_string()}}]}).to_string().into_bytes())
    }
    pub fn header(&self, i: usize, name: &str) -> Option<String> {
        self.calls.lock()[i].1.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }
}

impl TutorHttp for FakePost {
    fn post(&self, url: &str, headers: &[(String, String)], body: Vec<u8>, _timeout: Duration) -> Result<Vec<u8>, PostError> {
        self.calls.lock().push((url.to_string(), headers.to_vec(), serde_json::from_slice(&body).unwrap()));
        self.replies.lock().pop_front().expect("no reply left")
    }
}

/// What was sent to the fake TinyFish: (url, JSON body, headers).
pub type Sent = Arc<Mutex<Vec<(String, Option<Value>, Vec<(String, String)>)>>>;

/// A fake `watch.http_request` answering TinyFish's search (GET) and fetch (POST) with `pages` (url, title,
/// markdown); records what was sent.
pub fn tinyfish_api(pages: Vec<(&str, &str, &str)>) -> (Arc<dyn eve_web::Http>, Sent) {
    let pages: Vec<(String, String, String)> =
        pages.into_iter().map(|(u, t, m)| (u.to_string(), t.to_string(), m.to_string())).collect();
    let sent: Sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    let fake = move |url: &str, opts: &HttpOptions| -> Result<(u16, String), FetchError> {
        let body = opts.data.as_ref().map(|d| serde_json::from_slice::<Value>(d).unwrap());
        log.lock().push((url.to_string(), body.clone(), opts.headers.clone()));
        if body.is_none() {
            let results: Vec<Value> = pages.iter().map(|(u, t, _)| json!({"url": u, "title": t, "snippet": ""})).collect();
            return Ok((200, json!({"query": "q", "results": results}).to_string()));
        }
        let results: Vec<Value> = pages.iter().map(|(u, t, m)| json!({"url": u, "title": t, "text": m})).collect();
        Ok((200, json!({"results": results, "errors": []}).to_string()))
    };
    (Arc::new(fake), sent)
}

/// The daemon's side of `LearningApp.listener`.
#[derive(Default)]
pub struct FakeListener {
    pub calls: Mutex<Vec<(String, Option<String>)>>,
    pub state: Mutex<(String, Option<String>)>,
}

impl FakeListener {
    pub fn new() -> Arc<FakeListener> {
        Arc::new(FakeListener { calls: Mutex::new(Vec::new()), state: Mutex::new(("off".into(), None)) })
    }
    pub fn calls(&self) -> Vec<(String, Option<String>)> {
        self.calls.lock().clone()
    }
}

impl Listener for FakeListener {
    fn start(&self, lesson: &Map<String, Value>) -> anyhow::Result<Map<String, Value>> {
        let id = lesson["id"].as_str().unwrap().to_string();
        self.calls.lock().push(("start".into(), Some(id.clone())));
        *self.state.lock() = ("starting".into(), Some(id));
        Ok(obj(json!({"ok": true, "answer": "I'm listening. Think out loud whenever you're ready."})))
    }
    fn stop(&self) -> anyhow::Result<Map<String, Value>> {
        self.calls.lock().push(("stop".into(), None));
        *self.state.lock() = ("off".into(), None);
        Ok(obj(json!({"ok": true, "answer": "Okay, I stopped listening."})))
    }
    fn status(&self) -> anyhow::Result<Map<String, Value>> {
        let (state, id) = self.state.lock().clone();
        Ok(obj(json!({"state": state, "lesson_id": id})))
    }
}

/// The spend lines written into `dir`, oldest first.
pub fn ledger_lines(dir: &Path) -> Vec<Value> {
    let mut files: Vec<_> = std::fs::read_dir(dir).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    files.sort();
    files
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .flat_map(|p| std::fs::read_to_string(p).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect::<Vec<Value>>())
        .collect()
}
