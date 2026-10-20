//! Fixtures shared by the ports of test_apps_mcp.py, test_github_repos.py and test_routine_app_policy.py: the
//! Python's httpx.MockTransport (a scripted transport that records each request), its `Fake` server, the
//! `bridge(...)` helper, and a log capture standing in for pytest's caplog.
#![allow(dead_code)]

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use eve_apps_mcp::{token_fn, AppClient, AppToken, AppsBridge, AppsConfig, CallError, Environ};
use eve_google_rest::{HttpRequest, HttpResponse, Transport, TransportError};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

pub type Reply = Result<(u16, Vec<(String, String)>, Vec<u8>), TransportError>;

/// httpx.MockTransport: every request is recorded and answered by `handler`.
pub struct Mock {
    handler: Box<dyn Fn(&HttpRequest) -> Reply + Send + Sync>,
    pub seen: Mutex<Vec<HttpRequest>>,
}

impl Mock {
    pub fn new(handler: impl Fn(&HttpRequest) -> Reply + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            handler: Box::new(handler),
            seen: Mutex::new(Vec::new()),
        })
    }

    pub fn seen(&self) -> Vec<HttpRequest> {
        self.seen.lock().clone()
    }
}

#[async_trait]
impl Transport for Mock {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.seen.lock().push(request.clone());
        let (status, headers, body) = (self.handler)(&request)?;
        let stream = futures::stream::once(async move { Ok(bytes::Bytes::from(body)) });
        Ok(HttpResponse {
            status,
            headers,
            body: Box::pin(stream),
        })
    }
}

/// httpx.Response(status, json=...).
pub fn json_reply(status: u16, body: Value) -> Reply {
    Ok((
        status,
        vec![("content-type".into(), "application/json".into())],
        serde_json::to_vec(&body).unwrap(),
    ))
}

/// httpx.Response(status) with no body.
pub fn empty_reply(status: u16) -> Reply {
    Ok((status, Vec::new(), Vec::new()))
}

pub fn header<'a>(r: &'a HttpRequest, name: &str) -> Option<&'a str> {
    r.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

pub fn body_json(r: &HttpRequest) -> Value {
    serde_json::from_slice(r.body.as_deref().unwrap_or(b"null")).unwrap()
}

/// The URL's path and its query parameters (the last value of a repeated key).
pub fn url_parts(r: &HttpRequest) -> (String, IndexMap<String, String>) {
    let u = url::Url::parse(&r.url).unwrap();
    (
        u.path().to_string(),
        u.query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect(),
    )
}

/// The Python test's TOOLS for its fake servers.
pub fn tools(app: &str) -> Vec<Value> {
    let t = json!({
        "gmail": [{"name": "search_threads", "annotations": {"readOnlyHint": true}},
                  {"name": "trash_thread", "annotations": {"readOnlyHint": false}}],
        "calendar": [{"name": "list_events", "annotations": {"readOnlyHint": true}},
                     {"name": "create_event", "annotations": {"readOnlyHint": false}}],
        "drive": [{"name": "search_files", "annotations": {"readOnlyHint": true}},
                  {"name": "create_file", "annotations": {"readOnlyHint": false}}],
        "github": [{"name": "list_issues", "annotations": {"readOnlyHint": true}}, {"name": "create_issue"}],
        "maps": [{"name": "compute_routes", "annotations": {"readOnlyHint": true}}],
        "contacts": [{"name": "search_contacts", "annotations": {"readOnlyHint": true}},
                     {"name": "delete_contact", "annotations": {"readOnlyHint": false}}],
    });
    t[app].as_array().cloned().unwrap_or_default()
}

pub type Calls = Arc<Mutex<Vec<(String, String, Map<String, Value>)>>>;

/// What a fake's call_tool answers.
#[derive(Clone)]
pub enum FakeResult {
    Default,
    Value(Value),
    Raise(CallError),
}

/// The Python test's `Fake` server.
pub struct Fake {
    pub app: String,
    pub calls: Calls,
    pub fail: bool,
    pub result: FakeResult,
    pub tools: Vec<Value>,
}

#[async_trait]
impl AppClient for Fake {
    async fn list_tools(&self) -> Result<Vec<Value>, CallError> {
        if self.fail {
            return Err(CallError::new(
                "McpError",
                "not signed in or not allowed (HTTP 403)",
            ));
        }
        Ok(self.tools.clone())
    }

    async fn call_tool(&self, name: &str, args: &Map<String, Value>) -> Result<Value, CallError> {
        self.calls
            .lock()
            .push((self.app.clone(), name.to_string(), args.clone()));
        match &self.result {
            FakeResult::Raise(e) => Err(e.clone()),
            FakeResult::Value(v) => Ok(v.clone()),
            FakeResult::Default => Ok(json!({"structuredContent": {"ok": name}})),
        }
    }
}

/// `cfg(tmp_path, environ, **policy)`.
pub fn cfg(dir: &Path, environ: &[(&str, &str)], policy: &[(&str, &str)]) -> AppsConfig {
    let mut env: Environ = environ
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if !policy.is_empty() {
        env.insert(
            "EVE_APPS_POLICY".into(),
            policy
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    AppsConfig {
        enabled: true,
        config_dir: dir.to_path_buf(),
        policy: eve_apps_mcp::policy(Some(&env)),
        environ: env,
        ..AppsConfig::new(true)
    }
}

pub fn fake_tokens() -> IndexMap<String, Arc<dyn AppToken>> {
    IndexMap::from([
        ("google".to_string(), token_fn(|| Some("g".into()))),
        ("github".to_string(), token_fn(|| Some("h".into()))),
        ("maps".to_string(), token_fn(|| Some("m".into()))),
    ])
}

/// `bridge(tmp_path, fail=..., result=..., **policy)`, started.
pub async fn bridge(
    dir: &Path,
    fail: &[&str],
    result: FakeResult,
    policy: &[(&str, &str)],
) -> (AppsBridge, Calls) {
    bridge_with(dir, fail, result, policy, tools).await
}

/// The same over other fake tool lists.
pub async fn bridge_with(
    dir: &Path,
    fail: &[&str],
    result: FakeResult,
    policy: &[(&str, &str)],
    tool_list: fn(&str) -> Vec<Value>,
) -> (AppsBridge, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let fail: Vec<String> = fail.iter().map(|s| s.to_string()).collect();
    let (c1, f1, r1) = (calls.clone(), fail.clone(), result.clone());
    let (c2, f2, r2) = (calls.clone(), fail, result);
    let mut b = AppsBridge::new(cfg(dir, &[], policy))
        .with_client_factory(Arc::new(move |s, _tok| {
            Ok(Arc::new(Fake {
                app: s.app.to_string(),
                calls: c1.clone(),
                fail: f1.iter().any(|a| a == s.app),
                result: r1.clone(),
                tools: tool_list(s.app),
            }) as Arc<dyn AppClient>)
        }))
        .with_rest_factory(Arc::new(move |app, _tok| {
            Ok(Arc::new(Fake {
                app: app.to_string(),
                calls: c2.clone(),
                fail: f2.iter().any(|a| a == app),
                result: r2.clone(),
                tools: tool_list(app),
            }) as Arc<dyn AppClient>)
        }))
        .with_tokens(fake_tokens());
    b.start().await;
    (b, calls)
}

pub fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

/// pytest's caplog: every log line written while the guard lives, as text.
#[derive(Clone, Default)]
pub struct Lines(pub Arc<Mutex<Vec<u8>>>);

impl Write for Lines {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Lines {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }
}

pub fn capture() -> (tracing::subscriber::DefaultGuard, Lines) {
    let lines = Lines::default();
    let sink = lines.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || sink.clone())
        .finish();
    (tracing::subscriber::set_default(subscriber), lines)
}
