//! Port of bridge/tests/test_composio_connect.py: ConnectTokenSource and ConnectBridge against a fake Composio
//! Connect.
//!
//! The fake serves spike S1's captures as they are (bridge/tests/fixtures/composio_connect): the seven meta tool
//! objects from tools/list, and the two 401 answers /mcp gives a missing or an unknown bearer. Its token endpoint
//! takes the refresh form S1 verified. Every exchange goes through a Transport, so the real McpClient code runs
//! and nothing leaves the Mac. Tokens are canaries that do not look like tokens (AGENTS rule 2).

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use sha2::{Digest as _, Sha256};

use eve_composio_connect as cc;
use eve_composio_connect::{
    BridgeOptions, ConnectBridge, ConnectTokenSource, ConnectTokens, Obj, Readers, MANAGE_CONNECTIONS, META_TOOLS,
    SEARCH_TOOLS,
};
use eve_google_rest::{HttpRequest, HttpResponse, Transport, TransportError};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/tests/fixtures/composio_connect");
const MCP_URL: &str = "https://connect.composio.dev/mcp";
const TOKEN_URL: &str = "https://login.composio.dev/oauth2/token";
const CANARY: &str = "CANARY-NOT-A-KEY-7f3c";
const LINK: &str = "https://connect.composio.dev/link/ln_0000test";
const SLACK_LINK: &str = "https://connect.composio.dev/link/ln_slack";
const NOTION_LINK: &str = "https://connect.composio.dev/link/ln_notion";

fn a(n: u8) -> String {
    format!("{CANARY}-access-{n}")
}
fn r(n: u8) -> String {
    format!("{CANARY}-refresh-{n}")
}

fn fixture(name: &str) -> Value {
    let text = std::fs::read_to_string(Path::new(FIXTURES).join(name)).expect(name);
    serde_json::from_str(&text).expect(name)
}

static META: LazyLock<IndexMap<String, Value>> =
    LazyLock::new(|| META_TOOLS.iter().map(|n| (n.to_string(), fixture(&format!("{n}.json")))).collect());
static MCP_401: LazyLock<Vec<Value>> =
    LazyLock::new(|| fixture("mcp_401.json")["cases"].as_array().unwrap().clone());
static TOKEN_PROBE: LazyLock<Vec<Value>> =
    LazyLock::new(|| fixture("token_probe.json")["cases"].as_array().unwrap().clone());
static CLIENT_ID: LazyLock<String> =
    LazyLock::new(|| fixture("registration.json")["response"]["client_id"].as_str().unwrap().to_string());

fn meta(name: &str) -> &'static Value {
    &META[name]
}

fn meta_values() -> Vec<Value> {
    META.values().cloned().collect()
}

fn obj(v: Value) -> Obj {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn now_wall() -> f64 {
    cc::wall_clock()
}

/// composio-connect.json as onboarding's ComposioConnectLogin writes it: its contract keys, 0600.
fn sign_in_with(path: &Path, access: &str, refresh: &str, expires_in: f64, now: Option<f64>, extra: &[(&str, Value)]) -> PathBuf {
    let mut data = obj(json!({
        "version": 1, "issuer": "https://connect.composio.dev", "client_id": *CLIENT_ID,
        "redirect_uris": ["http://localhost:48780/onboard/composio/callback",
                          "http://127.0.0.1:48780/onboard/composio/callback"],
        "access_token": access, "refresh_token": refresh,
        "expires_at": now.unwrap_or_else(now_wall) + expires_in,
        "scope": "openid profile email offline_access"}));
    for (k, v) in extra {
        data.insert(k.to_string(), v.clone());
    }
    eve_apps_mcp::write_private_json(path, &data).unwrap();
    path.to_path_buf()
}

fn sign_in(path: &Path) -> PathBuf {
    sign_in_with(path, &a(1), &r(1), 3600.0, None, &[])
}

// ---- replies --------------------------------------------------------------------------------------------------

#[derive(Clone)]
enum Reply {
    Http { status: u16, headers: Vec<(String, String)>, body: Vec<u8> },
    Fail(&'static str, &'static str),
}

impl Reply {
    fn json(status: u16, value: Value) -> Self {
        Reply::Http {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: serde_json::to_vec(&value).unwrap(),
        }
    }

    fn text(status: u16, text: &str, ctype: Option<&str>) -> Self {
        let headers = ctype.map(|c| vec![("content-type".to_string(), c.to_string())]).unwrap_or_default();
        Reply::Http { status, headers, body: text.as_bytes().to_vec() }
    }

    fn into_response(self) -> Result<HttpResponse, TransportError> {
        match self {
            Reply::Http { status, headers, body } => {
                let stream = futures::stream::iter(vec![Ok::<Bytes, TransportError>(Bytes::from(body))]);
                Ok(HttpResponse { status, headers, body: Box::pin(stream) })
            }
            Reply::Fail(kind, message) => Err(TransportError::new(kind, message)),
        }
    }
}

fn rpc(msg: &Value, result: Value, headers: &[(&str, &str)]) -> Reply {
    let Reply::Http { status, headers: mut hs, body } =
        Reply::json(200, json!({"jsonrpc": "2.0", "id": msg.get("id").cloned().unwrap_or(Value::Null), "result": result}))
    else {
        unreachable!()
    };
    hs.extend(headers.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    Reply::Http { status, headers: hs, body }
}

/// An MCP tools/call result whose one text part is `obj` as JSON. S1 ran no tools/call, so this shape (Composio's
/// own object as the text) is an assumption; the bridge reads structuredContent as well, and plain text as text.
fn text_result(obj: Value) -> Value {
    json!({"content": [{"type": "text", "text": eve_apps_mcp::py::dumps(&obj, true)}]})
}

/// /mcp's 401 as S1 captured it: 0 for no bearer, 1 for a bearer that is not a token Composio knows.
fn unauthorized(case: usize) -> Reply {
    let c = &MCP_401[case];
    let Reply::Http { status: _, mut headers, body } = Reply::json(401, c["body"].clone()) else { unreachable!() };
    headers.push(("WWW-Authenticate".into(), c["www_authenticate"].as_str().unwrap().into()));
    Reply::Http { status: c["status"].as_u64().unwrap() as u16, headers, body }
}

/// An event that async code can wait on.
#[derive(Clone)]
struct Event(Arc<tokio::sync::watch::Sender<bool>>);

impl Event {
    fn new() -> Self {
        Event(Arc::new(tokio::sync::watch::channel(false).0))
    }
    fn set(&self) {
        self.0.send_replace(true);
    }
    async fn wait(&self, secs: f64) -> bool {
        let mut rx = self.0.subscribe();
        let waited = tokio::time::timeout(Duration::from_secs_f64(secs), rx.wait_for(|v| *v)).await;
        waited.is_ok()
    }
}

/// What a tools/call answer is: a result (wrapped in the JSON-RPC reply), a reply as it is, or a result after a
/// wait (a slow call).
enum Answer {
    Result(Value),
    Reply(Reply),
    Slow { inside: Event, release: Event, result: Value },
}

type AnswerFn = Arc<dyn Fn(&Value) -> Answer + Send + Sync>;
type HookFn = Arc<dyn Fn(&Value) -> Option<Reply> + Send + Sync>;

/// A fake Composio Connect: /mcp and the token endpoint, every request kept.
struct Composio {
    tools: Mutex<Vec<Value>>,
    /// the access tokens /mcp takes
    live: Mutex<HashSet<String>>,
    /// refresh -> (access, refresh), each used once
    grants: Mutex<HashMap<String, (String, String)>>,
    /// a refreshed access token is taken at /mcp
    take_new: Mutex<bool>,
    /// tool name -> args -> result
    answers: Mutex<HashMap<String, AnswerFn>>,
    hook: Mutex<Option<HookFn>>,
    /// initialize waits for this before its hook
    init_gate: Mutex<Option<Event>>,
    token_reply: Mutex<Option<Reply>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl Composio {
    fn new() -> Arc<Self> {
        Self::with_tools(meta_values())
    }

    fn with_tools(tools: Vec<Value>) -> Arc<Self> {
        Arc::new(Self {
            tools: Mutex::new(tools),
            live: Mutex::new(HashSet::from([a(1)])),
            grants: Mutex::new(HashMap::from([(r(1), (a(2), r(2)))])),
            take_new: Mutex::new(true),
            answers: Mutex::new(HashMap::new()),
            hook: Mutex::new(None),
            init_gate: Mutex::new(None),
            token_reply: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn transport(self: &Arc<Self>) -> Arc<dyn Transport> {
        self.clone()
    }

    fn answer(&self, name: &str, f: impl Fn(&Value) -> Answer + Send + Sync + 'static) {
        self.answers.lock().insert(name.to_string(), Arc::new(f));
    }

    fn set_hook(&self, f: impl Fn(&Value) -> Option<Reply> + Send + Sync + 'static) {
        *self.hook.lock() = Some(Arc::new(f));
    }

    fn grant(&self, refresh: String, access: String, rotated: String) {
        self.grants.lock().insert(refresh, (access, rotated));
    }

    async fn handle(&self, request: HttpRequest) -> Reply {
        self.requests.lock().push(request.clone());
        if request.url == TOKEN_URL {
            return self.token(&request);
        }
        assert_eq!(request.url, MCP_URL);
        let msg: Value = serde_json::from_slice(request.body.as_deref().unwrap_or_default()).unwrap();
        let auth = header(&request, "authorization").unwrap_or_default();
        let Some(bearer) = auth.strip_prefix("Bearer ") else {
            return unauthorized(0);
        };
        if !self.live.lock().contains(bearer) {
            return unauthorized(1);
        }
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
        if method == "initialize" {
            let gate = self.init_gate.lock().clone();
            if let Some(gate) = gate {
                gate.wait(5.0).await;
            }
        }
        let hook = self.hook.lock().clone();
        if let Some(hook) = hook {
            if let Some(hooked) = hook(&msg) {
                return hooked;
            }
        }
        if method == "initialize" {
            return rpc(
                &msg,
                json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {"listChanged": true}},
                       "serverInfo": {"name": "mcp-typescript server on vercel", "version": "0.1.0"}}),
                &[("Mcp-Session-Id", "session-1")],
            );
        }
        if msg.get("id").is_none() {
            return Reply::text(202, "", None);
        }
        if method == "tools/list" {
            let tools = self.tools.lock().clone();
            return rpc(&msg, json!({"tools": tools}), &[]);
        }
        if method == "tools/call" {
            let params = &msg["params"];
            let name = params["name"].as_str().unwrap_or_default().to_string();
            let answer = self.answers.lock().get(&name).cloned();
            let answered = match answer {
                Some(f) => f(&params["arguments"]),
                None => Answer::Result(text_result(
                    json!({"data": {"echo": params["arguments"]}, "error": null, "successful": true}),
                )),
            };
            return match answered {
                Answer::Result(result) => rpc(&msg, result, &[]),
                Answer::Reply(reply) => reply,
                Answer::Slow { inside, release, result } => {
                    inside.set();
                    release.wait(5.0).await;
                    rpc(&msg, result, &[])
                }
            };
        }
        Reply::json(200, json!({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": "no such method"}}))
    }

    fn token(&self, request: &HttpRequest) -> Reply {
        if let Some(reply) = self.token_reply.lock().clone() {
            return reply;
        }
        let form = parse_form(request.body.as_deref().unwrap_or_default());
        let grant = self.grants.lock().remove(&form["refresh_token"][0]);
        let Some((access, refresh)) = grant else {
            return Reply::json(400, TOKEN_PROBE[1]["answer"].clone()); // invalid_grant, as S1 saw it
        };
        if *self.take_new.lock() {
            self.live.lock().insert(access.clone());
        }
        Reply::json(200, json!({"access_token": access, "refresh_token": refresh, "token_type": "Bearer", "expires_in": 3600}))
    }

    // what reached the fake
    fn mcp_bodies(&self, method: &str) -> Vec<(HttpRequest, Value)> {
        self.requests
            .lock()
            .iter()
            .filter(|r| r.url == MCP_URL)
            .map(|r| (r.clone(), serde_json::from_slice::<Value>(r.body.as_deref().unwrap_or_default()).unwrap()))
            .filter(|(_, m)| m.get("method") == Some(&json!(method)))
            .collect()
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        self.mcp_bodies(method).into_iter().map(|(_, m)| m).collect()
    }

    fn bearers(&self, method: &str) -> Vec<String> {
        self.mcp_bodies(method)
            .into_iter()
            .map(|(r, _)| header(&r, "authorization").unwrap_or_default().trim_start_matches("Bearer ").to_string())
            .collect()
    }

    fn token_forms(&self) -> Vec<IndexMap<String, Vec<String>>> {
        self.requests
            .lock()
            .iter()
            .filter(|r| r.url == TOKEN_URL)
            .map(|r| parse_form(r.body.as_deref().unwrap_or_default()))
            .collect()
    }

    fn sent(&self) -> usize {
        self.requests.lock().len()
    }
}

#[async_trait]
impl Transport for Composio {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.handle(request).await.into_response()
    }
}

/// A transport that answers every request with `f`.
struct FnTransport<F>(F);

#[async_trait]
impl<F> Transport for FnTransport<F>
where
    F: Fn(&HttpRequest) -> Reply + Send + Sync,
{
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        (self.0)(&request).into_response()
    }
}

fn header(request: &HttpRequest, name: &str) -> Option<String> {
    request.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
}

/// `parse_qs(body, keep_blank_values=True)`.
fn parse_form(body: &[u8]) -> IndexMap<String, Vec<String>> {
    let mut out: IndexMap<String, Vec<String>> = IndexMap::new();
    for (k, v) in url::form_urlencoded::parse(body) {
        out.entry(k.into_owned()).or_default().push(v.into_owned());
    }
    out
}

type Now = Arc<Mutex<f64>>;

fn clock_of(now: &Now) -> cc::Clock {
    let now = now.clone();
    Arc::new(move || *now.lock())
}

fn source(path: &Path, fake: &Arc<Composio>, now: Option<&Now>) -> ConnectTokenSource {
    let src = ConnectTokenSource::new(Some(path)).with_transport(fake.transport());
    match now {
        Some(now) => src.with_clock(clock_of(now)),
        None => src,
    }
}

struct Made {
    bridge: Arc<ConnectBridge>,
    tokens: Arc<ConnectTokenSource>,
    path: PathBuf,
}

/// A bridge over the fake, signed in unless told otherwise. `now` is the token source's wall clock (and the
/// sign-in's), for tests that move time; `options` changes the bridge's other arguments.
async fn connect_with(
    dir: &Path,
    fake: &Arc<Composio>,
    signed_in: bool,
    start: bool,
    now: Option<&Now>,
    options: impl FnOnce(&mut BridgeOptions),
) -> Made {
    let path = dir.join("composio-connect.json");
    if signed_in && !path.exists() {
        sign_in_with(&path, &a(1), &r(1), 3600.0, Some(now.map(|n| *n.lock()).unwrap_or_else(now_wall)), &[]);
    }
    let tokens = Arc::new(source(&path, fake, now));
    let mut opts = BridgeOptions { transport: Some(fake.transport()), ..BridgeOptions::default() };
    options(&mut opts);
    let bridge = Arc::new(ConnectBridge::with_options(tokens.clone() as Arc<dyn ConnectTokens>, opts));
    if start {
        assert!(bridge.try_start().await);
    }
    Made { bridge, tokens, path }
}

async fn connect(dir: &Path, fake: &Arc<Composio>) -> Arc<ConnectBridge> {
    connect_with(dir, fake, true, true, None, |_| {}).await.bridge
}

async fn wait_until(check: impl Fn() -> bool, secs: f64) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    check()
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn read_file(path: &Path) -> Obj {
    obj(serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap())
}

fn failed(reason: &str) -> Obj {
    obj(json!({"ok": false, "reason": reason}))
}

fn apps_all(value: bool) -> IndexMap<String, bool> {
    cc::app_slugs().into_iter().map(|s| (s.to_string(), value)).collect()
}

fn bools(pairs: &[(&str, bool)]) -> IndexMap<String, bool> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

/// A log capture for this thread (pytest's caplog): every tracing line, DEBUG and up.
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Logs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }
}

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_logs() -> (tracing::subscriber::DefaultGuard, Logs) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || LogWriter(writer.clone()))
        .finish();
    (tracing::subscriber::set_default(subscriber), Logs(buf))
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ---- the fixtures and the tools as the brain gets them ---------------------------------------------------------

#[test]
fn test_the_fixtures_are_the_ones_spike_s1_captured() {
    let files = fixture("provenance.json")["files"].as_object().unwrap().clone();
    let mut listed: Vec<String> = files.keys().cloned().collect();
    listed.sort();
    let mut expected: Vec<String> = META_TOOLS.iter().map(|n| format!("{n}.json")).collect();
    expected.sort();
    assert_eq!(listed, expected);
    for (name, want) in &files {
        let bytes = std::fs::read(Path::new(FIXTURES).join(name)).unwrap();
        let got: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(&json!(got), want, "{name}");
    }
    let names: Vec<&str> = META_TOOLS.iter().map(|n| meta(n)["name"].as_str().unwrap()).collect();
    assert_eq!(names, META_TOOLS.to_vec());
    assert_eq!(MCP_401.iter().map(|c| c["status"].as_u64().unwrap()).collect::<Vec<_>>(), vec![401, 401]);
}

#[test]
fn test_the_seven_meta_tools_are_offered_under_composios_own_names() {
    let tools = cc::offered(&meta_values());
    assert_eq!(tools.iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>(), META_TOOLS.to_vec());
    for tool in &tools {
        let raw = meta(tool["name"].as_str().unwrap());
        assert!(tool["type"] == json!("function") && tool["strict"] == json!(false));
        assert_eq!(tool["parameters"], eve_apps_mcp::slim_schema(&raw["inputSchema"]));
        assert!(tool["parameters"].get("$schema").is_none());
        assert_eq!(tool["parameters"]["required"], raw["inputSchema"]["required"]);
        assert!(eve_profile::lite_tool_allowed(tool["name"].as_str().unwrap())); // eve lite's filter keeps every one
    }
    // the property descriptions are clipped as the direct apps' are; the arguments themselves are all there
    let multi = tools.iter().find(|t| t["name"] == json!("COMPOSIO_MULTI_EXECUTE_TOOL")).unwrap()["parameters"].clone();
    let props: HashSet<&String> = multi["properties"].as_object().unwrap().keys().collect();
    let raw_props: HashSet<&String> =
        meta("COMPOSIO_MULTI_EXECUTE_TOOL")["inputSchema"]["properties"].as_object().unwrap().keys().collect();
    assert_eq!(props, raw_props);
    assert!(multi["properties"].as_object().unwrap().values().all(|p| p
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .count()
        <= eve_apps_mcp::PROP_CLIP));
}

#[test]
fn test_the_descriptions_keep_composios_protocol_and_leave_out_the_catalog_line() {
    let tools: HashMap<String, Value> =
        cc::offered(&meta_values()).into_iter().map(|t| (t["name"].as_str().unwrap().to_string(), t)).collect();
    let raw = meta(SEARCH_TOOLS)["description"].as_str().unwrap();
    // control: the captured description names the web tools eve never runs, in its catalog line
    assert!(raw.contains("Tool Server Info:") && raw.contains("Browser tool") && raw.contains("Web Search"));
    for tool in tools.values() {
        let text = tool["description"].as_str().unwrap();
        assert!(!text.contains("Tool Server Info") && !text.contains("Browser tool") && !text.contains("Web Search"));
        assert!(text == text.trim() && !text.contains("\n\n\n") && text.chars().count() <= cc::DESC_MAX);
    }
    // what the model needs from them is all there, not clipped at apps_mcp.DESC_CLIP
    let search = tools[SEARCH_TOOLS]["description"].as_str().unwrap();
    assert!(search.contains("SESSION: ALWAYS set this parameter") && search.contains("COMPOSIO_GET_TOOL_SCHEMAS first"));
    assert!(search.chars().count() > 3 * eve_apps_mcp::DESC_CLIP);
    assert!(tools[MANAGE_CONNECTIONS]["description"].as_str().unwrap().contains("redirect_url as a FORMATTED MARKDOWN LINK"));
    assert!(tools["COMPOSIO_MULTI_EXECUTE_TOOL"]["description"].as_str().unwrap().contains("sync_response_to_workbench"));
    // every other line of Composio's text is there as it was
    let kept: Vec<&str> =
        raw.lines().map(str::trim).filter(|l| !l.is_empty() && !l.contains("Tool Server Info:")).collect();
    let got: Vec<&str> = search.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    assert_eq!(got, kept);
}

#[test]
fn test_a_description_is_clipped_past_its_limit_and_falls_back_to_the_title() {
    let long = cc::describe(&obj(json!({"name": "COMPOSIO_X", "description": "word ".repeat(5000)})));
    assert!(long.chars().count() == cc::DESC_MAX && long.ends_with('…'));
    assert_eq!(cc::describe(&obj(json!({"name": "COMPOSIO_X", "title": " Search ", "description": "\n  "}))), "Search");
    assert_eq!(cc::describe(&obj(json!({"name": "COMPOSIO_X", "description": 7}))), "COMPOSIO_X");
}

#[test]
fn test_tools_beyond_the_seven_are_left_out_and_the_first_of_a_name_counts() {
    let mut second = meta(MANAGE_CONNECTIONS).clone();
    second["description"] = json!("a second copy");
    let listed = vec![
        json!({"name": "COMPOSIO_SEARCH_WEB", "description": "a web search", "inputSchema": {"type": "object"}}),
        json!({"name": "GMAIL_SEND_EMAIL", "description": "sends", "inputSchema": {"type": "object"}}),
        json!("not a tool"),
        json!({"name": ["not", "a", "name"]}),
        json!({"description": "no name"}),
        meta(MANAGE_CONNECTIONS).clone(),
        second,
        json!({"name": SEARCH_TOOLS, "inputSchema": "not a schema"}),
        json!({"name": "COMPOSIO_REMOTE_BASH_TOOL", "inputSchema": {"type": "object", "properties": ["x"]}}),
    ];
    let tools = cc::offered(&listed);
    assert_eq!(
        tools.iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>(),
        vec![SEARCH_TOOLS, MANAGE_CONNECTIONS, "COMPOSIO_REMOTE_BASH_TOOL"]
    );
    assert_eq!(tools[1]["description"], json!(cc::describe(meta(MANAGE_CONNECTIONS).as_object().unwrap())));
    // a schema the model's API would refuse is replaced by an empty one, never sent as it is
    let empty = json!({"type": "object", "properties": {}});
    assert!(tools[0]["parameters"] == empty && tools[2]["parameters"] == empty);
}

#[test]
fn test_a_connect_link_on_composio_dev_is_accepted() {
    for url in [
        LINK,
        "https://composio.dev/link/x",
        "https://app.composio.dev/a?b=c#d",
        "https://connect.composio.dev:443/link/x",
        "HTTPS://Connect.Composio.Dev/link/x",
    ] {
        assert!(cc::link_ok_str(url), "{url}");
        assert!(cc::link_ok(&json!(url)), "{url}");
    }
}

#[test]
fn test_a_connect_link_anywhere_else_is_refused() {
    let long = format!("https://connect.composio.dev/{}", "x".repeat(5000));
    let urls: Vec<Value> = vec![
        json!("http://connect.composio.dev/link/x"),          // not https
        json!("https://composio.dev.example.com/link/x"),     // another site's subdomain
        json!("https://evilcomposio.dev/link/x"),             // a name that ends like it
        json!("https://connect.composio.dev@example.com/x"),  // a user part: the host is example.com
        json!("https://user:pw@connect.composio.dev/x"),      // a user part on the right host
        json!("https://example.com\\@connect.composio.dev/x"), // a browser reads example.com
        json!("https://example.com#@connect.composio.dev"),
        json!("https://connect.composio.dev:8443/link/x"), // another port
        json!("https://connect.composio.dev./link/x"),     // a trailing dot
        json!("https://connect.c\u{043e}mposio.dev/link/x"), // a Cyrillic o
        json!("https://connect.composio.dev%2eexample.com/x"),
        json!("https://example.com%2f.composio.dev/x"),
        json!("https://a..composio.dev/x"),
        json!("https://x_y.composio.dev/x"), // the host
        json!("https://connect.composio.dev/link/\u{0430}"),
        json!("https://connect.composio.dev/\u{202e}link"), // what the page shows
        json!("https://[::1]/x"),
        json!("https://connect.composio.dev/li nk"),
        json!("https://connect.composio.dev/\nx"),
        json!("//connect.composio.dev/x"),
        json!("javascript:alert(1)//connect.composio.dev"),
        json!("data:text/html,composio.dev"),
        json!(""),
        json!("https://"),
        Value::Null,
        json!(7),
        json!(["https://connect.composio.dev/x"]),
        json!(long),
        // what Python cannot split: refused, never raised (urlsplit raises ValueError for each)
        json!("https://[x/y"),
        json!("https://connect.composio.dev]/x"),
        json!("https://[connect.composio.dev]/x"),
        json!("https://[1.2.3.4]/x"),
        json!("https://[vz]/x"),
        json!("https://a]b[c.composio.dev/x"),
    ];
    for url in &urls {
        assert!(!cc::link_ok(url), "{url}");
    }
}

/// Every short URL over the characters that trip urlsplit gets a plain answer: link_ok is public, and its callers
/// hand it whatever Composio sent.
#[test]
fn test_link_ok_answers_every_url_python_cannot_split() {
    let alphabet: Vec<char> = "[]:@/.v1a".chars().collect();
    let mut urls = Vec::new();
    for base in ["https://", "https://connect.composio.dev", ""] {
        for size in 0..5u32 {
            let total = alphabet.len().pow(size);
            for mut n in 0..total {
                let mut tail = Vec::new();
                for _ in 0..size {
                    tail.push(alphabet[n % alphabet.len()]);
                    n /= alphabet.len();
                }
                tail.reverse();
                urls.push(format!("{base}{}", tail.into_iter().collect::<String>()));
            }
        }
    }
    let mut accepted = 0;
    for url in &urls {
        // an answer for each, whatever the brackets say; only the composio.dev host itself (and a tail that keeps
        // it) can pass
        if cc::link_ok_str(url) {
            accepted += 1;
            assert!(url.starts_with("https://connect.composio.dev"), "{url}");
        }
    }
    assert!(urls.len() > 1000 && accepted > 0);
    assert!(cc::link_ok_str("https://connect.composio.dev/link/ln_1?a=[1]")); // a bracket past the host
}

#[test]
fn test_a_toolkit_slug_is_lowercase_letters_digits_and_underscores() {
    let cases: Vec<(Value, Option<&str>)> = vec![
        (json!("gmail"), Some("gmail")),
        (json!(" GoogleCalendar "), Some("googlecalendar")),
        (json!("google_maps"), Some("google_maps")),
        (json!("../gmail"), None),
        (json!("gmail;rm"), None),
        (json!(""), None),
        (json!("_x"), None),
        (json!("x".repeat(65)), None),
        (Value::Null, None),
        (json!(5), None),
    ];
    for (toolkit, slug) in cases {
        assert_eq!(cc::toolkit_slug(&toolkit).as_deref(), slug, "{toolkit}");
    }
}

// ---- the sign-in ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_live_token_is_handed_out_with_no_request() {
    let dir = tmp();
    let fake = Composio::new();
    let src = source(&sign_in(&dir.path().join("c.json")), &fake, None);
    assert_eq!(src.get().await, Some(a(1)));
    assert_eq!(src.get().await, Some(a(1)));
    assert_eq!(fake.sent(), 0);
    let shown = format!("{src:?}");
    assert!(!src.needs_sign_in() && !shown.contains(&a(1)) && !shown.contains(&r(1)));
}

#[tokio::test]
async fn test_a_token_near_expiry_is_refreshed_with_the_form_s1_verified_and_saved_0600() {
    let dir = tmp();
    let fake = Composio::new();
    let path = sign_in_with(&dir.path().join("c.json"), &a(1), &r(1), 30.0, None, &[]);
    let before = read_file(&path);
    let src = source(&path, &fake, None);
    assert_eq!(src.get().await, Some(a(2)));
    let requests = fake.requests.lock().clone();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.url == TOKEN_URL && request.method == "POST");
    assert_eq!(header(request, "content-type").as_deref(), Some("application/x-www-form-urlencoded"));
    assert!(header(request, "authorization").is_none()); // a public client: no secret, no Basic
    let form = parse_form(request.body.as_deref().unwrap());
    let mut keys: Vec<&String> = form.keys().collect();
    keys.sort();
    let mut want: Vec<String> =
        TOKEN_PROBE[1]["request_keys"].as_array().unwrap().iter().map(|k| k.as_str().unwrap().to_string()).collect();
    want.sort();
    assert_eq!(keys, want.iter().collect::<Vec<_>>());
    let expected: IndexMap<String, Vec<String>> = [
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", r(1)),
        ("client_id", CLIENT_ID.clone()),
        ("resource", MCP_URL.to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), vec![v]))
    .collect();
    assert_eq!(form, expected);
    let saved = read_file(&path);
    let mut want_saved = before.clone();
    want_saved.insert("access_token".into(), json!(a(2)));
    want_saved.insert("refresh_token".into(), json!(r(2)));
    let at = saved["expires_at"].as_f64().unwrap();
    assert!((at - (now_wall() + 3600.0)).abs() <= 30.0);
    want_saved.insert("expires_at".into(), saved["expires_at"].clone());
    assert_eq!(saved, want_saved);
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600);
    assert_eq!(src.get().await, Some(a(2))); // the new token holds for an hour
    assert_eq!(fake.sent(), 1);
}

#[tokio::test]
async fn test_a_refresh_that_brings_no_new_refresh_token_keeps_the_old_one() {
    let dir = tmp();
    let fake = Composio::new();
    let path = sign_in_with(&dir.path().join("c.json"), &a(1), &r(1), -5.0, None, &[]);
    *fake.token_reply.lock() = Some(Reply::json(200, json!({"access_token": a(2), "expires_in": 120, "scope": "openid email"})));
    assert_eq!(source(&path, &fake, None).get().await, Some(a(2)));
    let saved = read_file(&path);
    assert!(saved["refresh_token"] == json!(r(1)) && saved["access_token"] == json!(a(2)) && saved["scope"] == json!("openid email"));
    assert!((saved["expires_at"].as_f64().unwrap() - (now_wall() + 120.0)).abs() <= 30.0);
}

#[tokio::test]
async fn test_a_refresh_composio_refuses_is_never_sent_again() {
    let cases: Vec<(u16, Value)> = vec![
        (400, TOKEN_PROBE[1]["answer"].clone()), // invalid_grant: the refresh token is gone
        (401, TOKEN_PROBE[3]["answer"].clone()), // invalid_client: the registration is gone
        (401, json!({})),                        // a 401 says the client is refused, error code or not
    ];
    for (status, body) in cases {
        let dir = tmp();
        let fake = Composio::new();
        let path = sign_in_with(&dir.path().join("c.json"), &a(1), &r(1), -5.0, None, &[]);
        *fake.token_reply.lock() = Some(Reply::json(status, body.clone()));
        let src = source(&path, &fake, None);
        let (guard, logs) = capture_logs();
        assert_eq!(src.get().await, None);
        assert_eq!(src.get().await, None);
        assert_eq!(src.refresh(Some(&a(1))).await, None);
        drop(guard);
        assert!(fake.token_forms().len() == 1 && src.needs_sign_in());
        let word = body.get("error").and_then(Value::as_str).unwrap_or("no error code");
        let text = logs.text();
        assert!(text.contains(&format!("HTTP {status}, {word}")), "{text}");
        assert!(!text.contains(CANARY));
        sign_in_with(&path, &a(3), &r(3), -5.0, None, &[]); // signed in again: a new refresh token
        *fake.token_reply.lock() = None;
        fake.grant(r(3), a(2), r(2));
        assert_eq!(src.get().await, Some(a(2)));
        assert!(!src.needs_sign_in());
    }
}

#[tokio::test]
async fn test_a_refresh_that_fails_for_a_reason_that_may_pass_keeps_a_live_token_and_waits() {
    let dir = tmp();
    let fake = Composio::new();
    let now: Now = Arc::new(Mutex::new(1_000_000.0));
    let path = sign_in_with(&dir.path().join("c.json"), &a(1), &r(1), 30.0, Some(*now.lock()), &[]);
    *fake.token_reply.lock() = Some(Reply::text(503, "busy", None));
    let src = source(&path, &fake, Some(&now));
    assert_eq!(src.get().await, Some(a(1))); // 30 s left: the token still goes
    assert_eq!(src.get().await, Some(a(1)));
    assert_eq!(fake.token_forms().len(), 1); // no second try this soon
    *now.lock() += cc::REFRESH_RETRY_SECS + 1.0;
    *fake.token_reply.lock() = None;
    assert_eq!(src.get().await, Some(a(2)));
    assert_eq!(fake.token_forms().len(), 2);
    *now.lock() += 7200.0; // A2 lapsed; the token endpoint cannot be reached
    let offline = ConnectTokenSource::new(Some(&path))
        .with_transport(Arc::new(FnTransport(|_: &HttpRequest| Reply::Fail("ConnectError", "no route"))))
        .with_clock(clock_of(&now));
    assert_eq!(offline.get().await, None);
    assert!(!offline.needs_sign_in()); // lapsed, but a refresh may still work later
}

#[tokio::test]
async fn test_a_token_answer_nested_past_pythons_limit_is_a_failed_refresh() {
    let dir = tmp();
    let fake = Composio::new();
    let path = sign_in_with(&dir.path().join("c.json"), &a(1), &r(1), 30.0, None, &[]);
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    *fake.token_reply.lock() = Some(Reply::text(200, &deep, Some("application/json")));
    let src = source(&path, &fake, None);
    assert_eq!(src.get().await, Some(a(1))); // the live token still goes, and the refresh cools off
    assert_eq!(src.get().await, Some(a(1)));
    assert_eq!(fake.token_forms().len(), 1);
}

#[tokio::test]
async fn test_no_sign_in_eve_can_use_is_no_token_and_no_request() {
    let dir = tmp();
    let fake = Composio::new();
    let missing = source(&dir.path().join("none.json"), &fake, None);
    assert_eq!(missing.get().await, None);
    assert_eq!(missing.refresh(Some(&a(1))).await, None);
    assert!(missing.needs_sign_in());
    for extra in [("version", json!(2)), ("issuer", json!("https://login.composio.dev"))] {
        let path = sign_in_with(&dir.path().join("c.json"), &a(1), &r(1), 3600.0, None, &[extra]);
        let src = source(&path, &fake, None);
        assert_eq!(src.get().await, None);
        assert!(src.needs_sign_in());
    }
    std::fs::write(dir.path().join("bad.json"), "{not json").unwrap();
    assert_eq!(source(&dir.path().join("bad.json"), &fake, None).get().await, None);
    let path = sign_in_with(&dir.path().join("nore.json"), &a(1), "", -5.0, None, &[]);
    assert_eq!(source(&path, &fake, None).get().await, None);
    assert_eq!(fake.sent(), 0);
}

#[tokio::test]
async fn test_after_a_401_a_token_another_call_already_refreshed_is_used_with_no_request() {
    let dir = tmp();
    let fake = Composio::new();
    let path = sign_in_with(&dir.path().join("c.json"), &a(2), &r(2), 3600.0, None, &[]);
    let src = source(&path, &fake, None);
    assert_eq!(src.refresh(Some(&a(1))).await, Some(a(2)));
    assert_eq!(fake.sent(), 0);
    assert_eq!(src.refresh(Some(&a(2))).await, None);
    assert_eq!(fake.token_forms().len(), 1); // R2 has no grant in the fake: refused
    assert_eq!(src.get().await, None); // A2 was refused: it is never sent again
}

/// The early refresh is for an ordinary call. After a 401, a token another call put in the file goes at once
/// while it has not lapsed, with no second token pair minted beside the one that call just got.
#[tokio::test]
async fn test_after_a_401_the_files_other_token_goes_as_it_is_until_it_lapses() {
    let dir = tmp();
    let fake = Composio::new();
    let now: Now = Arc::new(Mutex::new(1_000_000.0));
    let path = sign_in_with(&dir.path().join("c.json"), &a(2), &r(2), 30.0, Some(*now.lock()), &[]);
    fake.grant(r(2), a(3), r(3));
    let src = source(&path, &fake, Some(&now));
    assert_eq!(src.refresh(Some(&a(1))).await, Some(a(2))); // 30 s left: it goes as it is
    assert_eq!(fake.sent(), 0);
    assert_eq!(src.get().await, Some(a(3))); // control: an ordinary call refreshes it
    assert_eq!(fake.token_forms().len(), 1);
}

#[tokio::test]
async fn test_a_hold_sends_nothing_for_a_while_then_tries_the_same_token() {
    let dir = tmp();
    let fake = Composio::new();
    let now: Now = Arc::new(Mutex::new(1_000_000.0));
    // no access token yet: the first call refreshes
    let path = sign_in_with(&dir.path().join("c.json"), "", &r(1), 3600.0, Some(*now.lock()), &[]);
    let src = source(&path, &fake, Some(&now));
    src.hold(None).await;
    src.hold(Some("")).await; // no token named: nothing to hold
    assert_eq!(src.get().await, Some(a(2)));
    assert_eq!(fake.token_forms().len(), 1);
    src.hold(Some(&a(1))).await; // not the token the file holds: nothing changes
    assert_eq!(src.get().await, Some(a(2)));
    src.hold(Some(&a(2))).await;
    fake.grant(r(2), a(3), r(3));
    assert_eq!(src.get().await, None); // held: no token goes out, and no pair is minted
    assert_eq!(src.refresh(Some(&a(2))).await, None);
    assert!(!src.needs_sign_in() && fake.token_forms().len() == 1); // a hold never asks for a new sign-in
    *now.lock() += cc::REFUSED_RETRY_SECS - 1.0;
    assert_eq!(src.get().await, None);
    assert_eq!(src.refresh(Some(&a(2))).await, None);
    *now.lock() += 1.0;
    assert_eq!(src.get().await, Some(a(2))); // over: the same token, with no request
    assert_eq!(fake.token_forms().len(), 1);
    *now.lock() -= 100.0;
    assert_eq!(src.get().await, Some(a(2))); // and over for good: a clock set back does not revive it
    assert_eq!(src.refresh(Some(&a(2))).await, Some(a(3))); // after a 401 to it, one refresh
    assert_eq!(fake.token_forms().len(), 2);
    src.hold(Some(&a(3))).await;
    sign_in_with(&path, &a(4), &r(4), 3600.0, Some(*now.lock()), &[]); // signed in again: that sign-in is not held
    assert_eq!(src.get().await, Some(a(4)));
    assert!(!src.needs_sign_in());
}

#[tokio::test]
async fn test_a_clock_set_back_does_not_stretch_a_cool_off() {
    let dir = tmp();
    let fake = Composio::new();
    let now: Now = Arc::new(Mutex::new(1_000_000.0));
    let path = sign_in_with(&dir.path().join("c.json"), &a(1), &r(1), 30.0, Some(*now.lock()), &[]);
    *fake.token_reply.lock() = Some(Reply::text(503, "busy", None));
    let src = source(&path, &fake, Some(&now));
    assert_eq!(src.get().await, Some(a(1)));
    assert_eq!(src.refresh(Some(&a(1))).await, None); // a refresh cooling off
    assert_eq!(fake.token_forms().len(), 1);
    *now.lock() -= 3600.0; // the wall clock set back an hour
    *fake.token_reply.lock() = None;
    assert_eq!(src.refresh(Some(&a(1))).await, Some(a(2))); // over, not an hour longer
    assert_eq!(fake.token_forms().len(), 2);
    src.hold(Some(&a(2))).await;
    assert_eq!(src.get().await, None);
    *now.lock() -= 3600.0;
    assert_eq!(src.get().await, Some(a(2))); // a hold is not stretched either
}

#[tokio::test]
async fn test_a_new_sign_in_written_during_a_refresh_is_kept() {
    let dir = tmp();
    let path = sign_in_with(&dir.path().join("c.json"), &a(1), &r(1), -5.0, None, &[]);
    let written = path.clone();
    let meanwhile = move |_: &HttpRequest| {
        sign_in_with(&written, &a(3), &r(3), 3600.0, None, &[]); // the setup page signs in again
        Reply::json(200, json!({"access_token": a(2), "refresh_token": r(2), "expires_in": 3600}))
    };
    let src = ConnectTokenSource::new(Some(&path)).with_transport(Arc::new(FnTransport(meanwhile)));
    assert_eq!(src.get().await, Some(a(2)));
    let saved = read_file(&path);
    assert!(saved["access_token"] == json!(a(3)) && saved["refresh_token"] == json!(r(3)));
    assert_eq!(src.get().await, Some(a(3)));
}

#[tokio::test]
async fn test_a_refresh_that_cannot_be_saved_is_held_until_a_restart() {
    let dir = tmp();
    let fake = Composio::new();
    let path = sign_in_with(&dir.path().join("c.json"), &a(1), &r(1), -5.0, None, &[]);
    let disk_full: cc::Writer = Arc::new(|_: &Path, _: &Obj| Err(std::io::Error::from_raw_os_error(28)));
    let src = source(&path, &fake, None).with_writer(disk_full);
    assert_eq!(src.get().await, Some(a(2)));
    assert_eq!(src.get().await, Some(a(2)));
    assert_eq!(fake.token_forms().len(), 1);
    assert_eq!(read_file(&path)["refresh_token"], json!(r(1))); // not saved, and not lost: held in memory
    sign_in_with(&path, &a(3), &r(3), 3600.0, None, &[]); // a new sign-in replaces what was held
    assert_eq!(src.get().await, Some(a(3)));
}

// ---- the bridge over MCP -------------------------------------------------------------------------------------

#[tokio::test]
async fn test_the_bridge_lists_the_seven_meta_tools_over_mcp() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect_with(dir.path(), &fake, true, false, None, |_| {}).await.bridge;
    assert!(!bridge.started() && bridge.tools().is_empty() && bridge.names().is_empty());
    assert!(bridge.try_start().await && bridge.started() && bridge.try_start().await);
    let requests = fake.requests.lock().clone();
    let methods: Vec<String> = requests
        .iter()
        .map(|r| serde_json::from_slice::<Value>(r.body.as_deref().unwrap()).unwrap()["method"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(methods, vec!["initialize", "notifications/initialized", "tools/list"]);
    let auths: HashSet<String> = requests.iter().map(|r| header(r, "authorization").unwrap()).collect();
    assert_eq!(auths, HashSet::from([format!("Bearer {}", a(1))]));
    assert_eq!(header(&requests[2], "mcp-session-id").as_deref(), Some("session-1"));
    assert_eq!(header(&requests[0], "mcp-protocol-version").as_deref(), Some(eve_apps_mcp::PROTOCOL_VERSION));
    assert_eq!(bridge.names(), META_TOOLS.iter().map(|s| s.to_string()).collect());
    let tools: Vec<Value> = bridge.tools().into_iter().map(Value::Object).collect();
    assert_eq!(tools, cc::offered(&meta_values()));
    assert_eq!(bridge.accounts_note(), "");
    let mut copy = bridge.tools();
    copy[0].insert("name".into(), json!("changed"));
    assert_eq!(bridge.tools()[0]["name"], json!(SEARCH_TOOLS)); // each caller gets its own list
}

#[tokio::test]
async fn test_a_server_without_the_meta_tools_does_not_start() {
    let dir = tmp();
    let fake = Composio::with_tools(vec![json!({"name": "GMAIL_SEND_EMAIL", "inputSchema": {"type": "object"}})]);
    let bridge = connect_with(dir.path(), &fake, true, false, None, |_| {}).await.bridge;
    assert!(!bridge.try_start().await && !bridge.started() && bridge.tools().is_empty());
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, failed(cc::NOT_UP));
}

#[tokio::test]
async fn test_signed_out_the_bridge_makes_no_request() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect_with(dir.path(), &fake, false, false, None, |_| {}).await.bridge;
    assert!(!bridge.try_start().await && bridge.needs_sign_in());
    assert_eq!(bridge.connect_link("gmail").await, None);
    assert_eq!(bridge.connections(&cc::app_slugs(), cc::CONNECTIONS_TTL_SECS).await, apps_all(false));
    assert_eq!(fake.sent(), 0);
}

#[tokio::test]
async fn test_execute_runs_a_meta_tool_and_answers_with_composios_own_object() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    let answer = json!({"data": {"results": [{"response": {"data": {"messages": [{"subject": "Lunch"}]}}}]}, "error": null,
                        "successful": true, "log_id": "log_1"});
    let given = answer.clone();
    fake.answer("COMPOSIO_MULTI_EXECUTE_TOOL", move |_| Answer::Result(text_result(given.clone())));
    let args = obj(json!({"tools": [{"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {"query": "from:sam"}}],
                          "sync_response_to_workbench": false, "session_id": "s-1"}));
    let out = bridge.execute("COMPOSIO_MULTI_EXECUTE_TOOL", &args).await;
    let mut want = obj(json!({"ok": true}));
    want.extend(obj(answer));
    assert_eq!(out, want);
    let calls = fake.calls("tools/call");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["params"], json!({"name": "COMPOSIO_MULTI_EXECUTE_TOOL", "arguments": args}));
}

#[tokio::test]
async fn test_execute_reads_structured_content_from_an_event_stream() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    fake.set_hook(|msg| {
        if msg.get("method") != Some(&json!("tools/call")) {
            return None;
        }
        let body = json!({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "structuredContent": {"data": {"tool_slugs": ["X"]}, "successful": true},
            "content": [{"type": "text", "text": "a summary"}]}});
        Some(Reply::text(200, &format!(": ping\n\nevent: message\ndata: {body}\n\n"), Some("text/event-stream")))
    });
    let out = bridge.execute(cc::META_TOOLS[1], &obj(json!({"tool_slugs": ["X"]}))).await;
    assert_eq!(out, obj(json!({"ok": true, "data": {"tool_slugs": ["X"]}, "successful": true})));
}

#[tokio::test]
async fn test_a_call_composio_did_not_finish_is_not_ok_with_its_reason() {
    let cases: Vec<(Value, &str)> = vec![
        (text_result(json!({"data": {}, "error": "1 out of 1 searches failed", "successful": false})), "1 out of 1 searches failed"),
        (text_result(json!({"data": {}, "error": {"code": 7}, "successful": true})), "{\"code\": 7}"),
        (text_result(json!({"data": {}, "error": null, "successful": false})), "Composio did not finish the call"),
        (json!({"isError": true, "content": [{"type": "text", "text": "{\"data\": {}}"}]}), "Composio did not finish the call"),
        (json!({"isError": true, "content": [{"type": "text", "text": "Tool not found"}]}), "Tool not found"),
    ];
    for (result, reason) in cases {
        let dir = tmp();
        let fake = Composio::new();
        let bridge = connect(dir.path(), &fake).await;
        fake.answer(SEARCH_TOOLS, move |_| Answer::Result(result.clone()));
        let out = bridge.execute(SEARCH_TOOLS, &obj(json!({"queries": [{"use_case": "x"}]}))).await;
        assert!(out["ok"] == json!(false) && out["reason"] == json!(reason), "{out:?}");
    }
}

#[tokio::test]
async fn test_a_401_refreshes_the_sign_in_once_and_tries_once_more() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    fake.live.lock().remove(&a(1)); // Composio no longer takes A1 (revoked, or lapsed early)
    let out = bridge.execute(SEARCH_TOOLS, &obj(json!({"queries": [{"use_case": "read mail"}]}))).await;
    assert!(out["ok"] == json!(true) && out["data"] == json!({"echo": {"queries": [{"use_case": "read mail"}]}}));
    assert_eq!(fake.bearers("tools/call"), vec![a(1), a(2)]);
    assert_eq!(fake.token_forms().len(), 1);
    let saved = read_file(&dir.path().join("composio-connect.json"));
    assert_eq!((saved["access_token"].clone(), saved["refresh_token"].clone()), (json!(a(2)), json!(r(2))));
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await["ok"], json!(true)); // A2 from now on, with no refresh
    assert_eq!(fake.bearers("tools/call").last().cloned(), Some(a(2)));
    assert_eq!(fake.token_forms().len(), 1);
}

#[tokio::test]
async fn test_a_401_while_starting_refreshes_too() {
    let dir = tmp();
    let fake = Composio::new();
    sign_in_with(&dir.path().join("composio-connect.json"), &a(3), &r(1), 3600.0, None, &[]); // a token Composio does not take
    let bridge = connect_with(dir.path(), &fake, true, false, None, |_| {}).await.bridge;
    assert!(bridge.try_start().await);
    assert_eq!(fake.bearers("initialize"), vec![a(3), a(2)]);
    assert_eq!(fake.token_forms().len(), 1);
}

/// Composio refuses every token for a while (its 401 also covers not finding the account). That may pass: the
/// model is told to try again later, Settings does not ask for a new sign-in, and once the hold is over the same
/// sign-in works again.
#[tokio::test]
async fn test_a_second_401_holds_the_sign_in_then_tries_it_again() {
    let dir = tmp();
    let fake = Composio::new();
    let now: Now = Arc::new(Mutex::new(1_000_000.0));
    let bridge = connect_with(dir.path(), &fake, true, true, Some(&now), |_| {}).await.bridge;
    fake.live.lock().remove(&a(1));
    *fake.take_new.lock() = false; // Composio refuses even a fresh token
    fake.grant(r(2), a(3), r(3)); // a further refresh would mint a pair: not during the hold
    let held = failed(cc::NOT_RENEWED);
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, held);
    assert_eq!(fake.bearers("tools/call"), vec![a(1), a(2)]);
    assert_eq!(fake.token_forms().len(), 1);
    assert!(!bridge.needs_sign_in()); // nothing says the sign-in itself is gone
    let sent = fake.sent();
    *now.lock() += cc::REFUSED_RETRY_SECS - 1.0;
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, held);
    assert_eq!(bridge.connect_link("gmail").await, None);
    assert!(!bridge.connections(&cc::app_slugs(), 0.0).await["gmail"]);
    assert!(fake.sent() == sent && !bridge.needs_sign_in()); // held: nothing sent, no pair minted
    *now.lock() += 1.0;
    fake.live.lock().insert(a(2)); // Composio takes its tokens again
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await["ok"], json!(true));
    assert_eq!(fake.bearers("tools/call").last().cloned(), Some(a(2))); // the held token, no new pair
    assert_eq!(fake.token_forms().len(), 1);
}

#[tokio::test]
async fn test_a_refusal_that_lasts_costs_one_refresh_per_hold_until_the_token_endpoint_says_no() {
    let dir = tmp();
    let fake = Composio::new();
    let now: Now = Arc::new(Mutex::new(1_000_000.0));
    let bridge = connect_with(dir.path(), &fake, true, true, Some(&now), |_| {}).await.bridge;
    fake.live.lock().clear();
    *fake.take_new.lock() = false;
    fake.grant(r(2), a(3), r(3));
    let held = failed(cc::NOT_RENEWED);
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, held);
    *now.lock() += cc::REFUSED_RETRY_SECS;
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, held);
    assert!(!bridge.needs_sign_in());
    assert_eq!(fake.bearers("tools/call"), vec![a(1), a(2), a(2), a(3)]); // one refresh a round
    assert_eq!(fake.token_forms().len(), 2);
    *now.lock() += cc::REFUSED_RETRY_SECS;
    let signed_out = failed(cc::SIGNED_OUT);
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, signed_out); // R3 has no grant: invalid_grant
    assert!(bridge.needs_sign_in() && fake.token_forms().len() == 3);
    let sent = fake.sent();
    *now.lock() += 86_400.0;
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, signed_out); // for good
    assert_eq!(fake.sent(), sent);
    let at = *now.lock();
    sign_in_with(&dir.path().join("composio-connect.json"), &a(4), &r(4), 3600.0, Some(at), &[]); // the owner signs in again
    fake.live.lock().insert(a(4));
    assert!(!bridge.needs_sign_in());
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await["ok"], json!(true));
    assert_eq!(fake.bearers("tools/call").last().cloned(), Some(a(4)));
}

#[tokio::test]
async fn test_a_start_attempt_sends_nothing_while_the_sign_in_is_held() {
    let dir = tmp();
    let fake = Composio::new();
    let now: Now = Arc::new(Mutex::new(1_000_000.0));
    fake.live.lock().clear();
    *fake.take_new.lock() = false;
    fake.grant(r(2), a(3), r(3));
    let bridge = connect_with(dir.path(), &fake, true, false, Some(&now), |_| {}).await.bridge;
    assert!(!bridge.try_start().await);
    assert_eq!(fake.bearers("initialize"), vec![a(1), a(2)]);
    assert!(!bridge.needs_sign_in());
    let sent = fake.sent();
    *now.lock() += cc::REFUSED_RETRY_SECS - 1.0;
    assert!(!bridge.try_start().await && fake.sent() == sent);
    *now.lock() += 1.0;
    fake.live.lock().insert(a(2));
    assert!(bridge.try_start().await);
    assert_eq!(fake.bearers("initialize").last().cloned(), Some(a(2)));
    assert_eq!(fake.token_forms().len(), 1);
}

/// Composio gave no refresh token (onboarding saves such a sign-in, which lapses with its access token). Once that
/// token is refused there is nothing to renew: a new sign-in is the one way back.
#[tokio::test]
async fn test_a_refused_token_with_no_refresh_token_behind_it_needs_a_new_sign_in() {
    let dir = tmp();
    let fake = Composio::new();
    sign_in_with(&dir.path().join("composio-connect.json"), &a(1), "", 3600.0, None, &[]);
    let bridge = connect(dir.path(), &fake).await;
    fake.live.lock().remove(&a(1));
    let signed_out = failed(cc::SIGNED_OUT);
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, signed_out);
    assert!(bridge.needs_sign_in() && fake.token_forms().is_empty());
    let sent = fake.sent();
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, signed_out);
    assert_eq!(fake.sent(), sent); // it is not sent again
}

/// The fake, with the setup page signing in again just as a request with A2 arrives.
struct Meanwhile {
    fake: Arc<Composio>,
    path: PathBuf,
}

#[async_trait]
impl Transport for Meanwhile {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if header(&request, "authorization") == Some(format!("Bearer {}", a(2))) {
            sign_in_with(&self.path, &a(3), &r(3), 3600.0, None, &[]); // the setup page signs in again, just then
            self.fake.live.lock().insert(a(3));
        }
        self.fake.handle(request).await.into_response()
    }
}

#[tokio::test]
async fn test_a_new_sign_in_written_while_the_old_one_is_refused_gets_its_own_try() {
    let dir = tmp();
    let fake = Composio::new();
    let path = sign_in(&dir.path().join("composio-connect.json"));
    let tokens = Arc::new(source(&path, &fake, None));
    let opts = BridgeOptions {
        transport: Some(Arc::new(Meanwhile { fake: fake.clone(), path: path.clone() })),
        ..BridgeOptions::default()
    };
    let bridge = ConnectBridge::with_options(tokens, opts);
    assert!(bridge.try_start().await);
    fake.live.lock().remove(&a(1));
    *fake.take_new.lock() = false;
    // A2, fresh from the refresh, is refused too, but the file holds another sign-in by then: that one is not set
    // aside
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, failed(cc::NOT_RENEWED));
    assert!(!bridge.needs_sign_in());
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await["ok"], json!(true));
    assert_eq!(fake.bearers("tools/call"), vec![a(1), a(2), a(3)]);
    assert_eq!(fake.token_forms().len(), 1);
}

#[tokio::test]
async fn test_a_refused_refresh_asks_for_a_new_sign_in_and_stops_calling() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    fake.live.lock().remove(&a(1));
    fake.grants.lock().clear(); // the refresh token is gone: invalid_grant
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, failed(cc::SIGNED_OUT));
    assert!(bridge.needs_sign_in());
    let sent = fake.sent();
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, failed(cc::SIGNED_OUT));
    assert_eq!(fake.sent(), sent); // neither the refused token nor the dead refresh is sent
}

#[tokio::test]
async fn test_a_refresh_that_may_work_later_says_try_again() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    fake.live.lock().remove(&a(1));
    *fake.token_reply.lock() = Some(Reply::text(502, "bad gateway", None));
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, failed(cc::NOT_RENEWED));
    assert!(!bridge.needs_sign_in());
}

#[tokio::test]
async fn test_a_token_source_with_no_refresh_gets_one_try() {
    let fake = Composio::new();
    let plain: Arc<dyn ConnectTokens> = Arc::new(|| Some(a(1))); // a plain callable: no refresh to call
    let bridge =
        ConnectBridge::with_options(plain, BridgeOptions { transport: Some(fake.transport()), ..BridgeOptions::default() });
    assert!(bridge.try_start().await && !bridge.needs_sign_in());
    fake.live.lock().clear();
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, failed(cc::NOT_RENEWED));
    assert!(fake.calls("tools/call").len() == 1 && fake.token_forms().is_empty());
}

#[tokio::test]
async fn test_a_403_is_not_a_reason_to_refresh() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    fake.set_hook(|msg| (msg.get("method") == Some(&json!("tools/call"))).then(|| Reply::json(403, json!({"error": "forbidden"}))));
    let out = bridge.execute(SEARCH_TOOLS, &Obj::new()).await;
    assert!(out["ok"] == json!(false) && out["reason"].as_str().unwrap().contains("HTTP 403"));
    assert!(fake.token_forms().is_empty());
    assert_eq!(fake.calls("tools/call").len(), 1);
}

#[tokio::test]
async fn test_a_call_that_timed_out_is_not_sent_again_and_may_have_run() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    fake.set_hook(|msg| (msg.get("method") == Some(&json!("tools/call"))).then_some(Reply::Fail("ReadTimeout", "timed out")));
    let args = obj(json!({"tools": [{"tool_slug": "GMAIL_SEND_EMAIL", "arguments": {"to": "sam@example.com"}}],
                          "sync_response_to_workbench": false}));
    let out = bridge.execute("COMPOSIO_MULTI_EXECUTE_TOOL", &args).await;
    let reason = out["reason"].as_str().unwrap();
    assert!(out["ok"] == json!(false) && reason.starts_with("ReadTimeout"));
    assert!(reason.ends_with(cc::MAY_HAVE_RUN));
    assert_eq!(fake.calls("tools/call").len(), 1); // a send that may have gone is never sent twice by code
}

fn malformed() -> Vec<(&'static str, HookFn)> {
    fn hook(f: impl Fn(&Value) -> Reply + Send + Sync + 'static) -> HookFn {
        Arc::new(move |m: &Value| Some(f(m)))
    }
    let deep_text = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
    let deep_body = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    vec![
        ("a body that is not json", hook(|_| Reply::text(200, "<html>", Some("application/json")))),
        ("a json array", hook(|_| Reply::json(200, json!([1, 2, 3])))),
        ("no reply with this id", hook(|_| rpc(&json!({"id": 999}), json!({"content": []}), &[]))),
        ("an empty body", hook(|_| Reply::text(200, "", Some("application/json")))),
        ("an event stream of junk", hook(|_| Reply::text(200, "data: {not json\n\n", Some("text/event-stream")))),
        ("a json-rpc error", hook(|m| Reply::json(200, json!({"jsonrpc": "2.0", "id": m["id"], "error": {"code": -32000, "message": "boom"}})))),
        ("an error that is a word", hook(|m| Reply::json(200, json!({"jsonrpc": "2.0", "id": m["id"], "error": "boom"})))),
        ("a result that is a word", hook(|m| rpc(m, json!("nope"), &[]))),
        ("content that is not a list", hook(|m| rpc(m, json!({"content": 7}), &[]))),
        ("content parts that are not objects", hook(|m| rpc(m, json!({"content": [1, "x", null, {"type": "text"}]}), &[]))),
        ("text that is not json", hook(|m| rpc(m, json!({"content": [{"type": "text", "text": "plain words"}]}), &[]))),
        ("text that is a json list", hook(|m| rpc(m, json!({"content": [{"type": "text", "text": "[1, 2]"}]}), &[]))),
        ("text nested past python's limit", hook(move |m| rpc(m, json!({"content": [{"type": "text", "text": deep_text}]}), &[]))),
        ("two text parts", hook(|m| rpc(m, json!({"content": [{"type": "text", "text": "{}"}, {"type": "text", "text": "{}"}]}), &[]))),
        ("structured content that is a list", hook(|m| rpc(m, json!({"structuredContent": [1, 2]}), &[]))),
        ("an image", hook(|m| rpc(m, json!({"content": [{"type": "image", "data": "AAAA", "mimeType": "image/png"}]}), &[]))),
        ("a body nested past python's limit", hook(move |_| Reply::text(200, &deep_body, Some("application/json")))),
        ("a server error", hook(|_| Reply::text(500, "Internal Server Error", None))),
        ("a dropped connection", hook(|_| Reply::Fail("RemoteProtocolError", "peer closed"))),
    ]
}

#[tokio::test]
async fn test_a_malformed_answer_never_raises() {
    for (answer, reply) in malformed() {
        let dir = tmp();
        let fake = Composio::new();
        let bridge = connect(dir.path(), &fake).await;
        fake.set_hook(move |msg| if msg.get("method") == Some(&json!("tools/call")) { reply(msg) } else { None });
        for name in [SEARCH_TOOLS, MANAGE_CONNECTIONS, "COMPOSIO_REMOTE_WORKBENCH"] {
            let out = bridge.execute(name, &obj(json!({"toolkits": ["gmail"]}))).await;
            let ok = out["ok"].as_bool().unwrap_or_else(|| panic!("{answer}: {out:?}"));
            assert!(ok || out["reason"].as_str().is_some_and(|r| !r.is_empty()), "{answer}: {out:?}");
            serde_json::to_string(&out).unwrap(); // what the brain hands back to the model
        }
        assert_eq!(bridge.connect_link("gmail").await, None, "{answer}");
        assert_eq!(bridge.connections(&cc::app_slugs(), 0.0).await, apps_all(false), "{answer}");
        assert!(fake.token_forms().is_empty(), "{answer}");
    }
}

/// The model reads every part: of two, neither is dropped. Settings reads the one part that is Composio's object,
/// so a note after it (as Composio sends since 2026-10-14) does not make a connected app read as not connected.
#[tokio::test]
async fn test_a_result_with_two_text_parts_is_read_as_its_text() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    let first = eve_apps_mcp::py::dumps(
        &json!({"data": {"toolkit_connection_statuses": [{"toolkit": "gmail", "has_active_connection": true}]}, "successful": true}),
        true,
    );
    let given = first.clone();
    fake.answer(SEARCH_TOOLS, move |_| {
        Answer::Result(json!({"content": [{"type": "text", "text": given}, {"type": "text", "text": "and a note"}]}))
    });
    assert_eq!(
        bridge.execute(SEARCH_TOOLS, &Obj::new()).await,
        obj(json!({"ok": true, "result": format!("{first}\nand a note")}))
    );
    assert_eq!(bridge.connections(&["gmail"], cc::CONNECTIONS_TTL_SECS).await, bools(&[("gmail", true)])); // seen in the brain's search
    assert_eq!(fake.calls("tools/call").len(), 1);
    assert_eq!(bridge.connections(&["gmail"], 0.0).await, bools(&[("gmail", true)])); // Settings' own read, the same answer
    assert_eq!(fake.calls("tools/call").len(), 2);
}

#[tokio::test]
async fn test_two_json_objects_in_one_result_are_read_as_neither() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    let one = eve_apps_mcp::py::dumps(
        &json!({"data": {"toolkit_connection_statuses": [{"toolkit": "gmail", "has_active_connection": true}]}}),
        true,
    );
    fake.answer(SEARCH_TOOLS, move |_| {
        Answer::Result(json!({"content": [{"type": "text", "text": one}, {"type": "text", "text": one}]}))
    });
    assert_eq!(bridge.connections(&["gmail"], 0.0).await, bools(&[("gmail", false)]));
}

fn broken_statuses(_: &Value) -> IndexMap<String, bool> {
    panic!("a reader bug")
}

fn broken_result(_: &Value, _: Option<&Obj>) -> Obj {
    panic!("a reader bug")
}

#[tokio::test]
async fn test_an_answer_that_cannot_be_read_after_the_call_ran_says_it_may_have_run() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    fake.answer(SEARCH_TOOLS, |_| Answer::Result(search_answer(&[("gmail", true)])));
    // Settings' bookkeeping failing costs the model nothing: the answer is the answer
    bridge.set_readers(Readers { search_statuses: broken_statuses, ..Readers::default() });
    let out = bridge.execute(SEARCH_TOOLS, &Obj::new()).await;
    assert!(out["ok"] == json!(true) && out["data"]["toolkit_connection_statuses"][0]["toolkit"] == json!("gmail"));
    // the answer itself not read: never a raise, never a plain failure the model would run again
    bridge.set_readers(Readers { search_statuses: broken_statuses, result_for_model: broken_result });
    assert_eq!(bridge.execute(SEARCH_TOOLS, &Obj::new()).await, failed(cc::UNREADABLE));
    assert_eq!(fake.calls("tools/call").len(), 2); // each call made once
}

#[tokio::test]
async fn test_a_call_eve_cannot_make_is_a_plain_answer_with_no_request() {
    let cases: Vec<(&str, Value)> = vec![
        ("COMPOSIO_SEARCH_WEB", json!({})),
        ("GMAIL_SEND_EMAIL", json!({})),
        ("", json!({})),
        (SEARCH_TOOLS, json!(["not", "an", "object"])),
        (SEARCH_TOOLS, json!("text")),
    ];
    for (name, args) in cases {
        let dir = tmp();
        let fake = Composio::new();
        let bridge = connect(dir.path(), &fake).await;
        let sent = fake.sent();
        let out = bridge.execute_value(name, &args).await;
        assert!(out["ok"] == json!(false) && out["reason"].as_str().is_some_and(|r| !r.is_empty()));
        assert_eq!(fake.sent(), sent);
    }
}

#[tokio::test]
async fn test_no_arguments_are_an_empty_object() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    assert_eq!(bridge.execute_value(SEARCH_TOOLS, &Value::Null).await["ok"], json!(true));
    assert_eq!(fake.calls("tools/call").last().unwrap()["params"]["arguments"], json!({}));
}

#[tokio::test]
async fn test_a_huge_answer_is_clipped_as_a_direct_apps_is() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    fake.answer(SEARCH_TOOLS, |_| Answer::Result(text_result(json!({"data": {"blob": "x".repeat(200_000)}, "successful": true}))));
    let out = bridge.execute(SEARCH_TOOLS, &Obj::new()).await;
    assert!(out["ok"] == json!(true) && out["clipped"] == json!(true));
    let result = out["result"].as_str().unwrap();
    assert!(result.chars().count() == eve_apps_mcp::RESULT_CLIP + 1 && result.ends_with('…'));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_start_returns_at_once_and_keeps_trying_until_composio_answers() {
    let dir = tmp();
    let fake = Composio::new();
    let failures = Arc::new(AtomicUsize::new(2));
    let gate = Event::new();
    *fake.init_gate.lock() = Some(gate.clone());
    let left = failures.clone();
    fake.set_hook(move |msg| {
        if msg.get("method") == Some(&json!("initialize")) && left.load(Ordering::SeqCst) > 0 {
            left.fetch_sub(1, Ordering::SeqCst);
            return Some(Reply::text(503, "starting", None));
        }
        None
    });
    let bridge = connect_with(dir.path(), &fake, true, false, None, |o| o.backoff = vec![0.01]).await.bridge;
    bridge.start();
    assert_eq!(bridge.start_runs(), 1);
    bridge.start(); // while it runs: nothing new
    assert!(bridge.start_runs() == 1 && !bridge.started());
    gate.set();
    assert!(wait_until(|| bridge.started(), 5.0).await);
    assert_eq!(fake.calls("initialize").len(), 3);
    assert_eq!(bridge.names(), META_TOOLS.iter().map(|s| s.to_string()).collect());
    assert!(wait_until(|| !bridge.starting(), 5.0).await);
    bridge.start(); // once up: nothing new
    assert_eq!(bridge.start_runs(), 1);
    bridge.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_close_stops_the_tries() {
    let dir = tmp();
    let fake = Composio::new();
    fake.set_hook(|msg| (msg.get("method") == Some(&json!("initialize"))).then(|| Reply::text(503, "", None)));
    let bridge = connect_with(dir.path(), &fake, true, false, None, |o| o.backoff = vec![0.01]).await.bridge;
    bridge.start();
    assert!(wait_until(|| fake.calls("initialize").len() >= 2, 5.0).await);
    bridge.close().await;
    assert!(wait_until(|| !bridge.starting(), 5.0).await);
    let tried = fake.calls("initialize").len();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(fake.calls("initialize").len() == tried && !bridge.started());
    bridge.start();
    assert!(bridge.start_runs() == 1 && !bridge.starting()); // closed: never starts again
}

/// Python's close() closes the three httpx clients: a call after it sends nothing and is httpx's RuntimeError.
#[tokio::test]
async fn test_a_call_after_close_sends_nothing_and_says_the_client_is_closed() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    bridge.close().await;
    let sent = fake.sent();
    let out = bridge.execute(SEARCH_TOOLS, &Obj::new()).await;
    assert_eq!(out, failed("RuntimeError: Cannot send a request, as the client has been closed."));
    assert_eq!(bridge.connect_link("gmail").await, None);
    assert_eq!(bridge.connections(&cc::app_slugs(), 0.0).await, apps_all(false));
    assert_eq!(fake.sent(), sent);
}

/// start() called outside any tokio runtime runs its attempts on a thread of its own, as Python's daemon thread.
#[test]
fn test_start_outside_a_runtime_keeps_trying_on_its_own_thread() {
    let dir = tmp();
    let fake = Composio::new();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let bridge = runtime.block_on(connect_with(dir.path(), &fake, true, false, None, |o| o.backoff = vec![0.01])).bridge;
    drop(runtime);
    bridge.start();
    assert_eq!(bridge.start_runs(), 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !bridge.started() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(bridge.started());
    assert_eq!(bridge.names(), META_TOOLS.iter().map(|s| s.to_string()).collect());
}

// ---- Settings → Apps: Connect Links and the connected apps -----------------------------------------------------

/// COMPOSIO_MANAGE_CONNECTIONS's documented envelope (docs.composio.dev meta-tools page, 2026-10-13). Composio
/// documents no shape for a toolkit's own part, so the parts here are assumptions the bridge reads loosely.
fn manage_answer(results: Value, summary: &[(&str, i64)]) -> Value {
    let mut data = obj(json!({"message": "done", "results": results}));
    if !summary.is_empty() {
        data.insert("summary".into(), Value::Object(summary.iter().map(|(k, v)| (k.to_string(), json!(v))).collect()));
    }
    text_result(json!({"data": data, "error": null, "successful": true}))
}

#[tokio::test]
async fn test_connect_link_asks_for_one_toolkit_in_the_shape_that_needs_no_yes() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    fake.answer(MANAGE_CONNECTIONS, |_| {
        Answer::Result(manage_answer(
            json!({"gmail": {"toolkit": "gmail", "status": "initiated", "redirect_url": LINK}}),
            &[("total_toolkits", 1), ("active_connections", 0), ("initiated_connections", 1), ("failed_connections", 0)],
        ))
    });
    assert_eq!(bridge.connect_link(" Gmail ").await.as_deref(), Some(LINK));
    let calls = fake.calls("tools/call");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["params"], json!({"name": MANAGE_CONNECTIONS, "arguments": {"toolkits": ["gmail"]}}));
    assert!(!eve_composio_tools::manage_needs_yes(&calls[0]["params"]["arguments"])); // control: it only checks or links
    assert!(eve_composio_tools::manage_needs_yes(&json!({"toolkits": ["gmail"], "reinitiate_all": true})));
}

#[tokio::test]
async fn test_connect_link_reads_the_link_from_other_answer_shapes() {
    let answers = vec![
        manage_answer(json!([{"toolkit": "GMAIL", "redirectUrl": LINK}]), &[]), // a list of parts
        manage_answer(json!({"gmail_v2": {"redirect_url": LINK, "status": "INITIATED"}}), &[]), // one part, under another key
        text_result(json!({"data": {"connection": {"auth": {"redirect_url": LINK}}}, "successful": true})), // nested
        json!({"structuredContent": {"data": {"results": {"gmail": {"connect_link": LINK}}}}}),
        manage_answer(json!({"gmail": {"status": "initiated", "connection": {"redirect_url": LINK}}}), &[]), // nested in its part
        manage_answer(json!({"slack": {"redirect_url": SLACK_LINK}, "gmail": {"redirect_url": LINK},
                             "notion": {"redirect_url": NOTION_LINK}}), &[]), // its part beside others'
        manage_answer(json!([{"toolkit": "slack", "redirect_url": SLACK_LINK}, {"toolkit": "gmail", "redirect_url": LINK}]), &[]),
        text_result(json!({"data": {"redirect_url": LINK, "auth": {"redirect_url": LINK}}, "successful": true})), // said twice
    ];
    for answer in answers {
        let dir = tmp();
        let fake = Composio::new();
        let bridge = connect(dir.path(), &fake).await;
        let given = answer.clone();
        fake.answer(MANAGE_CONNECTIONS, move |_| Answer::Result(given.clone()));
        assert_eq!(bridge.connect_link("gmail").await.as_deref(), Some(LINK), "{answer}");
    }
}

#[tokio::test]
async fn test_connect_link_refuses_a_link_that_is_not_composios() {
    for link in [
        "https://connect.composio.dev.example.com/link/ln_1",
        "http://connect.composio.dev/link/ln_1",
        "https://example.com\\@connect.composio.dev/link",
        "javascript:alert(1)",
        "https://[evil/x",
        "https://connect.composio.dev]/x", // Python cannot split these: None, not a raise
    ] {
        let dir = tmp();
        let fake = Composio::new();
        let bridge = connect(dir.path(), &fake).await;
        fake.answer(MANAGE_CONNECTIONS, move |_| Answer::Result(manage_answer(json!({"gmail": {"redirect_url": link}}), &[])));
        assert_eq!(bridge.connect_link("gmail").await, None, "{link}");
        assert_eq!(fake.calls("tools/call").len(), 1); // it was asked: the link it sent is what is refused
    }
}

#[tokio::test]
async fn test_connect_link_for_an_app_already_connected_is_none_and_settings_shows_it() {
    let answers = vec![
        manage_answer(json!({"gmail": {"toolkit": "gmail", "status": "ACTIVE", "connected_account_id": "ca_1"}}), &[]),
        manage_answer(json!({"gmail": {"has_active_connection": true}}), &[]),
        manage_answer(json!({"gmail_v2": {"status": "ACTIVE"}}), &[]), // one toolkit was asked about: its one part
        manage_answer(json!({"gmail": {"connected_account_id": "ca_1"}}),
                      &[("total_toolkits", 1), ("active_connections", 1), ("initiated_connections", 0)]), // the summary says it
    ];
    for answer in answers {
        let dir = tmp();
        let fake = Composio::new();
        let bridge = connect(dir.path(), &fake).await;
        let given = answer.clone();
        fake.answer(MANAGE_CONNECTIONS, move |_| Answer::Result(given.clone()));
        assert_eq!(bridge.connect_link("gmail").await, None, "{answer}");
        assert_eq!(bridge.connections(&["gmail"], cc::CONNECTIONS_TTL_SECS).await, bools(&[("gmail", true)]));
        assert_eq!(fake.calls("tools/call").len(), 1); // known from the check: no search
    }
}

#[tokio::test]
async fn test_connect_link_reads_nothing_into_an_answer_about_other_apps() {
    let answers = vec![
        manage_answer(json!({"slack": {"status": "ACTIVE"}, "notion": {"status": "ACTIVE"}}), &[]),
        manage_answer(json!({"slack": {"status": "initiated", "redirect_url": SLACK_LINK},
                             "notion": {"status": "initiated", "redirect_url": NOTION_LINK}}), &[]),
        manage_answer(json!([{"toolkit": "slack", "redirect_url": SLACK_LINK},
                             {"toolkit": "notion", "redirect_url": NOTION_LINK}]), &[]),
        text_result(json!({"data": {"results": {"slack": {"status": "ACTIVE"}, "notion": {"status": "ACTIVE"}},
                                    "redirect_url": SLACK_LINK}, "successful": true})),
        manage_answer(json!({"slack": {"connected_account_id": "ca_1"}, "notion": {}}),
                      &[("total_toolkits", 1), ("active_connections", 1)]),
        text_result(json!({"data": {"results": [{"redirect_url": SLACK_LINK}, {"auth": {"redirect_url": NOTION_LINK}}]},
                           "successful": true})),
    ];
    for answer in answers {
        let dir = tmp();
        let fake = Composio::new();
        let bridge = connect(dir.path(), &fake).await;
        let given = answer.clone();
        fake.answer(MANAGE_CONNECTIONS, move |_| Answer::Result(given.clone()));
        fake.answer(SEARCH_TOOLS, |_| Answer::Result(search_answer(&[("gmail", false)])));
        assert_eq!(bridge.connect_link("gmail").await, None, "{answer}");
        assert_eq!(bridge.connections(&["gmail"], cc::CONNECTIONS_TTL_SECS).await, bools(&[("gmail", false)]));
        assert_eq!(fake.calls("tools/call").len(), 2); // nothing learned of gmail from that check: the search reads it
    }
}

#[tokio::test]
async fn test_after_a_connect_link_settings_reads_that_app_again() {
    let dir = tmp();
    let fake = Composio::new();
    let mono: Now = Arc::new(Mutex::new(100.0));
    let clock = clock_of(&mono);
    let bridge = connect_with(dir.path(), &fake, true, true, None, |o| o.clock = clock).await.bridge;
    fake.answer(SEARCH_TOOLS, |_| Answer::Result(search_answer(&[("gmail", false)])));
    assert_eq!(bridge.connections(&["gmail"], cc::CONNECTIONS_TTL_SECS).await, bools(&[("gmail", false)]));
    fake.answer(MANAGE_CONNECTIONS, |_| {
        Answer::Result(manage_answer(json!({"gmail": {"status": "initiated", "redirect_url": LINK}}), &[]))
    });
    assert_eq!(bridge.connect_link("gmail").await.as_deref(), Some(LINK));
    fake.answer(SEARCH_TOOLS, |_| Answer::Result(search_answer(&[("gmail", true)]))); // the owner follows the link
    assert_eq!(bridge.connections(&["gmail"], cc::CONNECTIONS_TTL_SECS).await, bools(&[("gmail", true)])); // read again now
    let names: Vec<Value> = fake.calls("tools/call").iter().map(|c| c["params"]["name"].clone()).collect();
    assert_eq!(names, vec![json!(SEARCH_TOOLS), json!(MANAGE_CONNECTIONS), json!(SEARCH_TOOLS)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_connect_link_never_waits_behind_a_slow_search_or_a_slow_brain_call() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    let release = Event::new();
    let inside: HashMap<&str, Event> =
        HashMap::from([(SEARCH_TOOLS, Event::new()), ("COMPOSIO_REMOTE_WORKBENCH", Event::new())]);
    for name in [SEARCH_TOOLS, "COMPOSIO_REMOTE_WORKBENCH"] {
        let (inside, release) = (inside[name].clone(), release.clone());
        fake.answer(name, move |_| Answer::Slow {
            inside: inside.clone(),
            release: release.clone(),
            result: search_answer(&[("gmail", true)]),
        });
    }
    fake.answer(MANAGE_CONNECTIONS, |_| Answer::Result(manage_answer(json!({"gmail": {"redirect_url": LINK}}), &[])));
    let looking = {
        let bridge = bridge.clone();
        tokio::spawn(async move { bridge.connections(&cc::app_slugs(), cc::CONNECTIONS_TTL_SECS).await })
    };
    let working = {
        let bridge = bridge.clone();
        tokio::spawn(async move {
            bridge.execute("COMPOSIO_REMOTE_WORKBENCH", &obj(json!({"code_to_execute": "1"}))).await
        })
    };
    let mut all_inside = true;
    for event in inside.values() {
        all_inside &= event.wait(5.0).await; // Settings' read and the brain's cell both running
    }
    let began = Instant::now();
    let link = bridge.connect_link("gmail").await;
    let took = began.elapsed();
    release.set();
    let _ = tokio::time::timeout(Duration::from_secs(5), looking).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), working).await;
    assert!(all_inside);
    assert_eq!(link.as_deref(), Some(LINK));
    assert!(took < Duration::from_secs(2));
}

#[tokio::test]
async fn test_connect_link_makes_no_request_for_a_bad_slug_or_a_web_toolkit() {
    for toolkit in ["../gmail", "gmail;rm", "", "exa", "composio_search", "browser_tool"] {
        let dir = tmp();
        let fake = Composio::new();
        let bridge = connect(dir.path(), &fake).await;
        let sent = fake.sent();
        assert_eq!(bridge.connect_link(toolkit).await, None);
        assert_eq!(fake.sent(), sent, "{toolkit}");
    }
}

/// COMPOSIO_SEARCH_TOOLS's documented answer (docs.composio.dev meta-tools page, 2026-10-13), trimmed.
fn search_answer(connected: &[(&str, bool)]) -> Value {
    let statuses: Vec<Value> = connected
        .iter()
        .map(|(kit, on)| json!({"toolkit": kit, "description": "", "has_active_connection": on, "connection_details": {},
                                 "status_message": "ok"}))
        .collect();
    text_result(json!({"data": {
        "results": [], "tool_schemas": {}, "time_info": {}, "session": {"id": "s", "generate_id": true},
        "next_steps_guidance": [], "toolkit_connection_statuses": statuses},
        "error": null, "successful": true}))
}

#[tokio::test]
async fn test_connections_reads_the_search_and_trusts_it_for_a_minute() {
    let dir = tmp();
    let fake = Composio::new();
    let mono: Now = Arc::new(Mutex::new(100.0));
    let clock = clock_of(&mono);
    let bridge = connect_with(dir.path(), &fake, true, true, None, |o| o.clock = clock).await.bridge;
    fake.answer(SEARCH_TOOLS, |_| Answer::Result(search_answer(&[("gmail", true), ("slack", false), ("outlook", true)])));
    let mut want = apps_all(false);
    want.insert("gmail".into(), true);
    assert_eq!(bridge.connections(&cc::app_slugs(), cc::CONNECTIONS_TTL_SECS).await, want);
    let calls = fake.calls("tools/call");
    assert_eq!(calls.len(), 1);
    let args = &calls[0]["params"]["arguments"];
    assert!(calls[0]["params"]["name"] == json!(SEARCH_TOOLS) && args["session"] == json!({"generate_id": true}));
    let queries = args["queries"].as_array().unwrap();
    assert_eq!(queries.len(), cc::APPS.len());
    for (query, (_, label)) in queries.iter().zip(cc::APPS.iter()) {
        assert!(query["use_case"].as_str().unwrap().contains(label)); // each app named, so the answer reports it
    }
    let order: Vec<String> = bridge.connections(&cc::app_slugs(), cc::CONNECTIONS_TTL_SECS).await.into_keys().collect();
    assert_eq!(order, cc::app_slugs());
    assert_eq!(fake.calls("tools/call").len(), 1); // trusted for CONNECTIONS_TTL_SECS
    *mono.lock() += cc::CONNECTIONS_TTL_SECS;
    fake.answer(SEARCH_TOOLS, |_| Answer::Result(search_answer(&[("gmail", false), ("slack", true)])));
    let got = bridge.connections(&["slack", "gmail", "slack"], cc::CONNECTIONS_TTL_SECS).await;
    assert_eq!(got, bools(&[("slack", true), ("gmail", false)]));
    assert_eq!(got.keys().collect::<Vec<_>>(), vec!["slack", "gmail"]);
    assert_eq!(bridge.connections(&["notion"], 0.0).await, bools(&[("notion", false)]));
    assert_eq!(fake.calls("tools/call").len(), 3);
    assert_eq!(
        fake.calls("tools/call").last().unwrap()["params"]["arguments"]["queries"],
        json!([{"use_case": "search my pages in Notion"}])
    );
}

#[tokio::test]
async fn test_connections_reads_many_apps_a_few_at_a_time() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    let kits: Vec<String> = (0..cc::LOOK_MAX * 2 + 3).map(|n| format!("kit{n}")).collect();
    let refs: Vec<&str> = kits.iter().map(String::as_str).collect();
    fake.answer(SEARCH_TOOLS, |_| Answer::Result(search_answer(&[("kit0", true), ("kit22", true)])));
    let want: IndexMap<String, bool> = kits.iter().map(|k| (k.clone(), k == "kit0" || k == "kit22")).collect();
    assert_eq!(bridge.connections(&refs, cc::CONNECTIONS_TTL_SECS).await, want);
    let sizes: Vec<usize> =
        fake.calls("tools/call").iter().map(|c| c["params"]["arguments"]["queries"].as_array().unwrap().len()).collect();
    assert_eq!(sizes, vec![cc::LOOK_MAX, cc::LOOK_MAX, 3]);
    assert_eq!(
        fake.calls("tools/call")[0]["params"]["arguments"]["queries"][0],
        json!({"use_case": "list my recent items in kit0"})
    );
}

#[tokio::test]
async fn test_connections_keeps_what_was_known_while_composio_fails() {
    let dir = tmp();
    let fake = Composio::new();
    let mono: Now = Arc::new(Mutex::new(100.0));
    let clock = clock_of(&mono);
    let bridge = connect_with(dir.path(), &fake, true, true, None, |o| o.clock = clock).await.bridge;
    fake.answer(SEARCH_TOOLS, |_| Answer::Result(search_answer(&[("gmail", true)])));
    assert_eq!(bridge.connections(&["gmail"], cc::CONNECTIONS_TTL_SECS).await, bools(&[("gmail", true)]));
    *mono.lock() += cc::CONNECTIONS_TTL_SECS;
    fake.answer(SEARCH_TOOLS, |_| Answer::Reply(Reply::text(500, "down", None)));
    let known = bools(&[("gmail", true), ("slack", false)]);
    assert_eq!(bridge.connections(&["gmail", "slack"], cc::CONNECTIONS_TTL_SECS).await, known);
    assert_eq!(bridge.connections(&["gmail", "slack"], cc::CONNECTIONS_TTL_SECS).await, known);
    assert_eq!(fake.calls("tools/call").len(), 2); // no new read for LOOK_RETRY_SECS after a failed one
    assert!(bridge.connections_value(&Value::Null, cc::CONNECTIONS_TTL_SECS).await.is_empty());
    assert!(bridge.connections_value(&json!(7), cc::CONNECTIONS_TTL_SECS).await.is_empty());
}

#[tokio::test]
async fn test_what_the_brain_runs_updates_what_settings_shows() {
    let dir = tmp();
    let fake = Composio::new();
    let mono: Now = Arc::new(Mutex::new(100.0));
    let clock = clock_of(&mono);
    let bridge = connect_with(dir.path(), &fake, true, true, None, |o| o.clock = clock).await.bridge;
    fake.answer(SEARCH_TOOLS, |_| Answer::Result(search_answer(&[("gmail", true), ("notion", false)])));
    let out = bridge.execute(SEARCH_TOOLS, &obj(json!({"queries": [{"use_case": "read my mail"}]}))).await;
    assert_eq!(out["ok"], json!(true));
    fake.answer(MANAGE_CONNECTIONS, |_| {
        Answer::Result(manage_answer(
            json!({"slack": {"status": "initiated", "redirect_url": LINK}, "linear": {"status": "ACTIVE"}}),
            &[],
        ))
    });
    assert_eq!(bridge.execute(MANAGE_CONNECTIONS, &obj(json!({"toolkits": ["slack", "linear"]}))).await["ok"], json!(true));
    let calls = fake.calls("tools/call").len();
    assert_eq!(
        bridge.connections(&["gmail", "notion", "linear"], cc::CONNECTIONS_TTL_SECS).await,
        bools(&[("gmail", true), ("notion", false), ("linear", true)])
    );
    assert_eq!(fake.calls("tools/call").len(), calls); // all three known from the brain's own calls
    fake.answer(SEARCH_TOOLS, |_| Answer::Result(search_answer(&[("slack", true)])));
    assert_eq!(bridge.connections(&["slack"], cc::CONNECTIONS_TTL_SECS).await, bools(&[("slack", true)])); // read again
    assert_eq!(fake.calls("tools/call").len(), calls + 1);
}

// ---- secrets and the brain's duck type -------------------------------------------------------------------------

#[tokio::test]
async fn test_no_token_reaches_a_log_a_result_or_a_repr() {
    let (guard, logs) = capture_logs();
    let dir = tmp();
    let fake = Composio::new();
    let made = connect_with(dir.path(), &fake, true, false, None, |_| {}).await;
    let bridge = made.bridge.clone();
    let mut seen: Vec<String> = vec![format!("{:?}", bridge.try_start().await)];
    fake.live.lock().remove(&a(1)); // a 401, a refresh, a retry
    seen.push(format!("{:?}", bridge.execute(SEARCH_TOOLS, &obj(json!({"queries": [{"use_case": "read mail"}]}))).await));
    fake.answer(MANAGE_CONNECTIONS, |_| Answer::Result(manage_answer(json!({"gmail": {"redirect_url": LINK}}), &[])));
    seen.push(format!("{:?}", bridge.connect_link("gmail").await));
    fake.live.lock().clear(); // then every token refused, and no refresh left
    seen.push(format!("{:?}", bridge.execute(SEARCH_TOOLS, &Obj::new()).await));
    seen.push(format!("{:?}", bridge.connect_link("slack").await));
    seen.push(format!("{:?}", bridge.connections(&cc::app_slugs(), 0.0).await));
    sign_in_with(&made.path, &a(3), &r(3), 3600.0, None, &[]); // a new sign-in, whose fresh token is
    fake.grant(r(3), a(4), r(4)); // refused too: it is held (hold)
    *fake.take_new.lock() = false;
    let held = bridge.execute(SEARCH_TOOLS, &Obj::new()).await;
    let needs = bridge.needs_sign_in();
    seen.push(format!("{held:?}"));
    seen.push(format!("{needs:?}"));
    seen.push(format!("{:?}", bridge.connect_link("slack").await));
    assert!(held["reason"] == json!(cc::NOT_RENEWED) && !needs);
    seen.push(format!("{bridge:?}"));
    seen.push(format!("{:?}", made.tokens));
    seen.push(format!("{:?}", bridge.tools()));
    let all = seen.join("\n");
    assert!(!all.contains(CANARY));
    assert!(!logs.text().contains(CANARY));
    // controls: the tokens were really there to leak, and this log capture sees this crate's lines
    assert!(fake.requests.lock().iter().any(|r| header(r, "authorization").unwrap_or_default().contains(CANARY)));
    assert!(std::fs::read_to_string(&made.path).unwrap().contains(CANARY));
    assert!(logs.text().contains("composio connect:"));
    tracing::debug!("control {}", a(2));
    assert!(logs.text().contains(CANARY));
    drop(guard);
}

#[tokio::test]
async fn test_the_bridge_has_the_duck_type_the_brain_reads() {
    let dir = tmp();
    let fake = Composio::new();
    let bridge = connect(dir.path(), &fake).await;
    // what the brain's inlet reads of its apps (eve_composio_tools::ComposioBridge's shape)
    let _: bool = bridge.started();
    let names = bridge.names();
    let tool_names: std::collections::BTreeSet<String> =
        bridge.tools().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(tool_names, names);
    assert!(bridge.tools().iter().all(|t| t["type"] == json!("function") && t["parameters"].is_object()));
    let tools: Vec<Value> = bridge.tools().into_iter().map(Value::Object).collect();
    let kept: Vec<Value> = eve_profile::filter_tools(&tools).into_iter().cloned().collect();
    assert_eq!(kept, tools); // eve lite offers all of them
    let _: Map<String, Value> = bridge.execute(SEARCH_TOOLS, &Obj::new()).await;
    assert_eq!(bridge.accounts_note(), "");
}

// ---- the pure readers, case by case (beyond the Python file) ----------------------------------------------------

#[test]
fn test_search_statuses_reads_a_keyed_form_and_skips_what_does_not_read() {
    let keyed = json!({"data": {"toolkit_connection_statuses": {
        "gmail": {"has_active_connection": true}, "slack": {"toolkit": "notion", "has_active_connection": false},
        "x": "not an object", "linear": {"has_active_connection": "yes"}}}});
    assert_eq!(cc::search_statuses(&keyed), bools(&[("gmail", true), ("notion", false)]));
    let flat = json!({"toolkit_connection_statuses": [{"toolkit": " GMAIL ", "has_active_connection": true}, 7,
                                                        {"toolkit": "../x", "has_active_connection": true}]});
    assert_eq!(cc::search_statuses(&flat), bools(&[("gmail", true)]));
    assert!(cc::search_statuses(&json!([1])).is_empty());
}

#[test]
fn test_an_items_status_says_connected_only_in_its_own_words() {
    let cases: Vec<(Value, Option<bool>)> = vec![
        (json!({"is_active": false, "status": "ACTIVE"}), Some(false)),
        (json!({"connected": true}), Some(true)),
        (json!({"status": " Connected "}), Some(true)),
        (json!({"status": "", "connection_status": "active"}), Some(true)),
        (json!({"status": "  "}), None),
        (json!({"status": "  ", "link": LINK}), Some(false)),
        (json!({"connectLink": "  "}), None),
        (json!({}), None),
    ];
    for (item, want) in cases {
        assert_eq!(cc::item_active(item.as_object().unwrap()), want, "{item}");
    }
}

#[test]
fn test_a_summary_equal_to_one_reads_as_pythons_equality() {
    let answer = obj(json!({"data": {"results": {}, "summary": {"total_toolkits": 1.0, "active_connections": true}}}));
    assert_eq!(cc::connect_answer(Some(&answer), "gmail"), (None, Some(true)));
    let answer = obj(json!({"data": {"summary": {"total_toolkits": 2, "active_connections": 1}}}));
    assert_eq!(cc::connect_answer(Some(&answer), "gmail"), (None, None));
    assert_eq!(cc::connect_answer(None, "gmail"), (None, None));
}

#[test]
fn test_a_result_that_is_not_an_object_is_no_result() {
    assert_eq!(cc::result_for_model(&json!("x"), None), failed("Composio sent no result"));
    assert_eq!(cc::payload(&json!([1])), None);
    assert_eq!(cc::answer_object(&json!(7)), None);
}

// keep the write helper honest: a file written here reads back as the token source reads it
#[test]
fn test_a_sign_in_file_reads_back_through_read_json() {
    let dir = tmp();
    let path = sign_in(&dir.path().join("c.json"));
    let data = eve_apps_mcp::read_json(&path);
    assert_eq!(data["version"], json!(1));
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(b"junk").unwrap();
    assert!(eve_apps_mcp::read_json(&path).is_empty());
}
