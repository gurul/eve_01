//! test_telegram.py's apps and the record (1567-2220): Composio, the direct apps, an email asked whole, Share,
//! web_search, the second brain, the watcher, receipts and a note read, edited and undone; ported with their names
//! and assertions.
//!
//! Python's `FakeApps` is the real [`ComposioBridge`] over a fake SDK client and session; `fake_apps()` and
//! `scenario_apps()` are the real [`AppsBridge`] over fake servers, as they are in Python.
//! `monkeypatch.setattr(websearch, "search", ...)` is the inlet's `tt_set_web_search`.

mod more;
mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::FutureExt;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_apps_mcp::{AppClient, AppToken, AppsBridge, AppsConfig, CallError, FnToken, Server, OPEN_TOOL};
use eve_brain::prelude::BrainMemory;
use eve_brain::telegram::{self as telegram, Inbound, InletOptions, TelegramApi};
use eve_composio_tools::{ComposioBridge, ComposioClient, ComposioConfig, ComposioError, ComposioSession};
use more::*;
use support::*;

/// `json.loads(next(i for i in rig.create.requests[n]["input"] if i.get("type") == "function_call_output")["output"])`.
fn first_output(rig: &Rig, n: usize) -> Value {
    let requests = rig.create.requests.lock();
    let item = requests[n]["input"].as_array().unwrap().iter().find(|i| i["type"] == "function_call_output").cloned().unwrap();
    serde_json::from_str(item["output"].as_str().unwrap()).unwrap()
}

fn request(rig: &Rig, n: usize) -> Value {
    rig.create.requests.lock()[n].clone()
}

/// `[t.get("name") for t in request["tools"]]`.
fn names(request: &Value) -> Vec<String> {
    request["tools"].as_array().unwrap().iter().map(|t| t.get("name").and_then(Value::as_str).unwrap_or("None").to_string()).collect()
}

fn sent(rig_api: &FakeApi) -> Vec<(i64, String)> {
    rig_api.log.lock().sent.clone()
}

fn line(text: &str) -> (i64, String) {
    (OWNER, text.to_string())
}

// ---- the apps (composio_tools.py) --------------------------------------------------------------------------------

/// FakeApps' one session: its two meta tools, every call recorded, a canned result.
struct FakeSession {
    executed: Arc<Mutex<Vec<(String, Map<String, Value>)>>>,
}

#[async_trait]
impl ComposioSession for FakeSession {
    fn session_id(&self) -> String {
        "s1".into()
    }
    async fn tools(&self) -> Result<Vec<Value>, ComposioError> {
        Ok(vec![
            json!({"type": "function", "name": "COMPOSIO_MULTI_EXECUTE_TOOL", "strict": false,
                   "parameters": {"type": "object", "properties": {"tools": {"type": "array"}}}}),
            json!({"type": "function", "name": "COMPOSIO_SEARCH_TOOLS", "strict": true,
                   "parameters": {"type": "object", "properties": {"queries": {"type": "array"}}}}),
        ])
    }
    async fn execute(&self, slug: &str, arguments: &Map<String, Value>) -> Result<Value, ComposioError> {
        self.executed.lock().push((slug.to_string(), arguments.clone()));
        let results: Vec<Value> = arguments
            .get("tools")
            .and_then(Value::as_array)
            .map(|tools| tools.iter().map(|t| json!({"tool_slug": t["tool_slug"], "successful": true})).collect())
            .unwrap_or_default();
        Ok(json!({"ok": true, "data": {"results": results}, "log_id": "log_1"}))
    }
}

struct FakeClient {
    executed: Arc<Mutex<Vec<(String, Map<String, Value>)>>>,
}

#[async_trait]
impl ComposioClient for FakeClient {
    async fn create_session(&self, _user_id: &str, _toolkits: &Value, _multi: Option<&Value>) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        Ok(Arc::new(FakeSession { executed: self.executed.clone() }))
    }
    async fn use_session(&self, _session_id: &str) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        Ok(Arc::new(FakeSession { executed: self.executed.clone() }))
    }
    async fn list_connected_accounts(&self, _user_ids: &[String]) -> Result<Vec<Value>, ComposioError> {
        Ok(Vec::new())
    }
    async fn execute_tool(&self, _slug: &str, _arguments: &Map<String, Value>, _user_id: &str, _account: &str) -> Result<Value, ComposioError> {
        Err(ComposioError::new("AssertionError", "not in these tests"))
    }
}

/// test_telegram.py's `FakeApps(started=...)`: the bridge, and what it executed.
struct FakeApps {
    bridge: Arc<ComposioBridge>,
    executed: Arc<Mutex<Vec<(String, Map<String, Value>)>>>,
    _dir: tempfile::TempDir,
}

impl FakeApps {
    async fn new(started: bool) -> Self {
        let dir = tmp();
        let executed = Arc::new(Mutex::new(Vec::new()));
        let e = executed.clone();
        let mut config = ComposioConfig::new(true, "owner");
        config.state_path = dir.path().join("composio.json");
        config.toolkits = None;
        let bridge = ComposioBridge::new(config, Some(Arc::new(move || Ok(Arc::new(FakeClient { executed: e.clone() }) as Arc<dyn ComposioClient>))));
        if started {
            bridge.start().await.unwrap();
        }
        Self { bridge: Arc::new(bridge), executed, _dir: dir }
    }

    fn executed_names(&self) -> Vec<String> {
        self.executed.lock().iter().map(|(n, _)| n.clone()).collect()
    }
}

fn private_event() -> Value {
    json!({"summary": "Lunch with Ana", "start_datetime": "2026-10-09T12:00:00"}) // no guests: routine
}

/// `multi(*slugs, arguments=...)`.
fn multi(slugs: &[&str], arguments: Option<Value>) -> Value {
    let arguments = arguments.unwrap_or_else(|| json!({"to": "ana@example.com", "subject": "Lunch"}));
    json!({"tools": slugs.iter().map(|s| json!({"tool_slug": s, "arguments": arguments, "account": "work"})).collect::<Vec<_>>(),
           "thought": "t", "sync_response_to_workbench": false, "current_step": "GO"})
}

fn with_apps(apps: &FakeApps) -> impl FnOnce(&mut InletOptions) {
    let bridge = apps.bridge.clone();
    move |o: &mut InletOptions| {
        o.apps = Some(bridge);
        o.toolset_mode = Some("off".into());
    }
}

// Pinned to EVE_TOOLSET_ROUTER=off (2026-10-05): the door before the toolset router, where Composio and
// Share ride every turn. With the router on they open by the turn's words; that door is test_telegram_toolsets.py.
#[tokio::test]
async fn test_a_web_lookup_through_composio_is_refused_and_pointed_at_web_search() {
    let _iso = isolated();
    let apps = FakeApps::new(true).await;
    let api = FakeApi::new(vec![vec![said("find the office hours on the course page", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("COMPOSIO_MULTI_EXECUTE_TOOL", multi(&["SERPAPI_GOOGLE_LIGHT_SEARCH"], None), "call_1"), reply("Looking it up with my own search.")]),
        config(),
        with_apps(&apps),
    );
    run_plain(&rig).await;
    assert!(apps.executed.lock().is_empty()); // never reached Composio
    let refused = output_of(&rig, 1);
    assert!(refused["ok"] == json!(false) && refused["reason"].as_str().unwrap().contains("web_search"));
    let first = request(&rig, 0);
    let instructions = first["instructions"].as_str().unwrap();
    assert!(instructions.contains("public web") && instructions.contains("web_search"));
    // control: the owner's calendar through the same door still runs
    let apps = FakeApps::new(true).await;
    let api = FakeApi::new(vec![vec![said("put it in my calendar", 1)]]);
    let rig = Rig::new(
        api,
        FakeCreate::new(vec![call("COMPOSIO_MULTI_EXECUTE_TOOL", multi(&["GOOGLECALENDAR_CREATE_EVENT"], Some(private_event())), "call_1"), reply("Added.")]),
        config(),
        with_apps(&apps),
    );
    run_plain(&rig).await;
    assert_eq!(apps.executed_names(), vec!["COMPOSIO_MULTI_EXECUTE_TOOL"]);
}

#[tokio::test]
async fn test_composio_tools_are_offered_and_executed_through_the_session() {
    let _iso = isolated();
    let apps = FakeApps::new(true).await;
    // 1. a reading call runs at once and its result goes back to the model
    let api = FakeApi::new(vec![vec![said("any mail from Sam?", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("COMPOSIO_MULTI_EXECUTE_TOOL", multi(&["GMAIL_FETCH_EMAILS"], None), "call_1"), reply("Two from Sam today.")]),
        config(),
        with_apps(&apps),
    );
    run_plain(&rig).await;
    let first = request(&rig, 0);
    let composio: Vec<String> = names(&first).into_iter().filter(|n| n.starts_with("COMPOSIO")).collect();
    assert_eq!(composio, vec!["COMPOSIO_MULTI_EXECUTE_TOOL", "COMPOSIO_SEARCH_TOOLS"]);
    let own = telegram::own_tools();
    let tools = first["tools"].as_array().unwrap();
    assert_eq!(tools[..own.len()].to_vec(), own.iter().cloned().map(Value::Object).collect::<Vec<_>>());
    let instructions = first["instructions"].as_str().unwrap();
    assert!(instructions.contains(telegram::APPS_BLOCK));
    assert!(!instructions.contains("not in COMPOSIO")); // no direct app is up, so none is named
    assert_eq!(apps.executed_names(), vec!["COMPOSIO_MULTI_EXECUTE_TOOL"]);
    let output = output_of(&rig, 1);
    assert!(output["ok"] == json!(true) && output["log_id"] == json!("log_1"));
    assert_eq!(sent(&api).last().cloned(), Some(line("Two from Sam today.")));
    // 2. Gmail is read only: a send is refused with no call and no question (owner, 2026-09-21)
    let apps = FakeApps::new(true).await;
    let api = FakeApi::new(vec![vec![said("email Ana about lunch", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("COMPOSIO_MULTI_EXECUTE_TOOL", multi(&["GMAIL_SEND_EMAIL"], None), "call_1"), reply("I can only read mail here, not send it.")]),
        config(),
        with_apps(&apps),
    );
    run_plain(&rig).await;
    assert!(apps.executed.lock().is_empty());
    let refused = output_of(&rig, 1);
    assert!(refused["ok"] == json!(false) && refused["reason"].as_str().unwrap().contains("gmail is read only here"), "{refused}");
    assert_eq!(sent(&api), vec![line("I can only read mail here, not send it.")]); // no yes/no was asked
    // 3. the calendar may write without asking
    let apps = FakeApps::new(true).await;
    let api = FakeApi::new(vec![vec![said("lunch with Ana Friday noon", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("COMPOSIO_MULTI_EXECUTE_TOOL", multi(&["GOOGLECALENDAR_CREATE_EVENT"], Some(private_event())), "call_1"), reply("Booked.")]),
        config(),
        with_apps(&apps),
    );
    run_plain(&rig).await;
    assert!(apps.executed_names() == vec!["COMPOSIO_MULTI_EXECUTE_TOOL"] && sent(&api) == vec![line("Booked.")]);
    // 4. any other app's write is the owner's yes/no first; "no" refuses without a call, "yes" runs it
    for (answer, executed, last) in [("no", 0, "Okay, not sent."), ("yes", 1, "Sent.")] {
        let apps = FakeApps::new(true).await;
        let api = FakeApi::new(vec![vec![said("tell the team lunch is at noon", 1)]]);
        let rig = Rig::new(
            api.clone(),
            FakeCreate::new(vec![call("COMPOSIO_MULTI_EXECUTE_TOOL", multi(&["SLACK_SEND_MESSAGE"], None), "call_1"), reply(last)]),
            config(),
            with_apps(&apps),
        );
        run_rig(&rig, || async {
            let question = sent(&api)[0].1.clone(); // the answer arrives after the question, as on a phone
            assert!(question.starts_with("Run SLACK_SEND_MESSAGE") && question.contains("yes / no?") && question.contains("ana@example.com"), "{question}");
            assert!(apps.executed.lock().is_empty());
            api.feed(vec![said(answer, 2)]);
        })
        .await;
        assert!(apps.executed.lock().len() == executed && sent(&api).last() == Some(&line(last)), "{answer}");
    }
    // 5. a tool nobody offered is still refused, and the apps' tools are not offered before the session is up;
    //    the refusal is the call's result, so the turn goes on (review, 2026-10-05; before, FAILED_LINE)
    let apps = FakeApps::new(false).await;
    let api = FakeApi::new(vec![vec![said("hi", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("COMPOSIO_MULTI_EXECUTE_TOOL", multi(&["GMAIL_FETCH_EMAILS"], None), "call_1"), reply("x")]),
        config(),
        with_apps(&apps),
    );
    run_plain(&rig).await;
    assert!(!names(&request(&rig, 0)).iter().any(|n| n.starts_with("COMPOSIO")));
    assert!(apps.executed.lock().is_empty());
    assert!(output_of(&rig, 1)["reason"].as_str().unwrap().contains("not a tool here"));
    assert_eq!(sent(&api), vec![line("x")]);
}

// ---- the direct apps (apps_mcp.py) -------------------------------------------------------------------------------

/// `APP_TOOLS`.
fn app_tools(app: &str, with_send: bool) -> Vec<Value> {
    match app {
        "gmail" => {
            let mut tools = vec![
                json!({"name": "search_threads", "annotations": {"readOnlyHint": true},
                       "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}}}),
                json!({"name": "trash_thread", "annotations": {"readOnlyHint": false},
                       "inputSchema": {"type": "object", "properties": {"threadId": {"type": "string"}}}}),
            ];
            if with_send {
                tools.push(json!({"name": "send_message", "annotations": {"readOnlyHint": false},
                                  "inputSchema": {"type": "object", "properties": {"to": {"type": "array"}}}}));
            }
            tools
        }
        "calendar" => vec![
            json!({"name": "list_events", "annotations": {"readOnlyHint": true},
                   "inputSchema": {"type": "object", "properties": {"startTime": {"type": "string"}}}}),
            json!({"name": "list_calendars", "annotations": {"readOnlyHint": true}, "inputSchema": {"type": "object"}}),
            json!({"name": "create_event", "annotations": {"readOnlyHint": false},
                   "inputSchema": {"type": "object", "properties": {"summary": {"type": "string"}}}}),
        ],
        "github" => vec![
            json!({"name": "create_issue", "inputSchema": {"type": "object", "properties": {
                "repo": {"type": "string"}, "title": {"type": "string"}}}}),
            json!({"name": "list_issues", "inputSchema": {"type": "object", "properties": {}}}),
        ],
        "contacts" => vec![json!({"name": "search_contacts", "annotations": {"readOnlyHint": true},
                                  "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}}})],
        _ => Vec::new(),
    }
}

const MAPS_URL: &str = "https://www.google.com/maps/search/?api=1&query=Room+101";

type Calls = Arc<Mutex<Vec<(String, Map<String, Value>)>>>;

/// `FakeMcp` (and `ScenarioMcp` with `scenario`): one app's server, its tools listed, every call recorded.
struct FakeMcp {
    app: String,
    calls: Calls,
    scenario: bool,
    with_send: bool,
}

#[async_trait]
impl AppClient for FakeMcp {
    async fn list_tools(&self) -> Result<Vec<Value>, CallError> {
        if self.scenario && self.app == "maps" {
            return Ok(vec![json!({"name": "compute_routes", "annotations": {"readOnlyHint": true},
                                  "inputSchema": {"type": "object", "properties": {"destination": {"type": "string"}}}})]);
        }
        Ok(app_tools(&self.app, self.with_send))
    }
    async fn call_tool(&self, name: &str, args: &Map<String, Value>) -> Result<Value, CallError> {
        self.calls.lock().push((format!("{}__{}", self.app, name), args.clone()));
        if self.scenario {
            if self.app == "calendar" && name == "list_events" {
                return Ok(json!({"structuredContent": {"events": [{"summary": "CS 101 lecture", "start": "2026-10-05T10:00:00",
                                                                   "location": "Room 101", "mapsUrl": MAPS_URL}]}}));
            }
            return Ok(json!({"structuredContent": {"ok": true}}));
        }
        Ok(json!({"structuredContent": {"threads": [{"from": "Sam", "subject": "Lunch"}]}}))
    }
}

struct DirectApps {
    bridge: Arc<AppsBridge>,
    executed: Calls,
}

impl DirectApps {
    fn executed_names(&self) -> Vec<String> {
        self.executed.lock().iter().map(|(n, _)| n.clone()).collect()
    }
}

fn token(value: &'static str) -> Arc<dyn AppToken> {
    Arc::new(FnToken(move || Some(value.to_string())))
}

async fn build_apps(started: bool, policy: &[(&str, &str)], scenario: bool, with_send: bool) -> DirectApps {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let mut env = eve_apps_mcp::Environ::new();
    if !policy.is_empty() {
        env.insert("EVE_APPS_POLICY".into(), policy.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(","));
    }
    let config = AppsConfig { enabled: true, policy: eve_apps_mcp::policy(Some(&env)), environ: env, ..AppsConfig::new(true) };
    let (c1, c2) = (calls.clone(), calls.clone());
    let mut tokens: IndexMap<String, Arc<dyn AppToken>> = IndexMap::from([("google".to_string(), token("t")), ("github".to_string(), token("t"))]);
    if scenario {
        tokens.insert("maps".to_string(), token("k"));
    }
    let mut bridge = AppsBridge::new(config)
        .with_client_factory(Arc::new(move |s: &Server, _tok| {
            Ok(Arc::new(FakeMcp { app: s.app.to_string(), calls: c1.clone(), scenario, with_send }) as Arc<dyn AppClient>)
        }))
        .with_rest_factory(Arc::new(move |app: &str, _tok| {
            Ok(Arc::new(FakeMcp { app: app.to_string(), calls: c2.clone(), scenario, with_send }) as Arc<dyn AppClient>)
        }))
        .with_tokens(tokens);
    if started {
        bridge.start().await;
    }
    DirectApps { bridge: Arc::new(bridge), executed: calls }
}

/// `fake_apps(started, **policy)`: the real AppsBridge over fake servers: Gmail, the calendar, GitHub, contacts.
async fn fake_apps(started: bool, policy: &[(&str, &str)]) -> DirectApps {
    build_apps(started, policy, false, false).await
}

/// `scenario_apps()`: the same with Maps up too and a calendar that answers with a class.
async fn scenario_apps() -> DirectApps {
    build_apps(true, &[], true, false).await
}

fn with_direct(apps: &DirectApps) -> impl FnOnce(&mut InletOptions) {
    let bridge = apps.bridge.clone();
    move |o: &mut InletOptions| o.direct = Some(bridge)
}

#[tokio::test]
async fn test_direct_app_tools_are_opened_by_the_message_and_executed_through_the_bridge() {
    let _iso = isolated();
    let apps = fake_apps(true, &[("gmail", "read")]).await;
    // 1. "mail" opens Gmail: its read tool is offered (its trash is not: Gmail is read only here), the call runs
    let api = FakeApi::new(vec![vec![said("any mail from Sam?", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("gmail__search_threads", json!({"query": "from:sam"}), "call_1"), reply("Two from Sam today.")]),
        config(),
        with_direct(&apps),
    );
    run_plain(&rig).await;
    let first = request(&rig, 0);
    let n = names(&first);
    assert!(n.contains(&"gmail__search_threads".into()) && !n.contains(&"gmail__trash_thread".into()));
    assert!(!n.contains(&"calendar__list_events".into()) && n.contains(&OPEN_TOOL.into())); // the rest wait for open_app
    let own = telegram::own_tools();
    assert_eq!(first["tools"].as_array().unwrap()[..own.len()].to_vec(), own.iter().cloned().map(Value::Object).collect::<Vec<_>>());
    assert!(first["instructions"].as_str().unwrap().contains("Gmail (gmail__…): read only"));
    assert_eq!(*apps.executed.lock(), vec![("gmail__search_threads".to_string(), obj(json!({"query": "from:sam"})))]);
    let output = output_of(&rig, 1);
    assert!(output["ok"] == json!(true) && output["result"]["threads"][0]["from"] == json!("Sam"));
    assert_eq!(sent(&api).last().cloned(), Some(line("Two from Sam today.")));
    // 2. Gmail read only: its changing tools are never offered, so a call to one runs nothing and asks nothing;
    //    the refusal (naming Gmail's real tools) is the call's result, and the model's next reply goes out
    let apps = fake_apps(true, &[("gmail", "read")]).await;
    let api = FakeApi::new(vec![vec![said("bin that mail from Sam", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("gmail__trash_thread", json!({"threadId": "t1"}), "call_1"), reply("I can only read mail here.")]),
        config(),
        with_direct(&apps),
    );
    run_plain(&rig).await;
    assert!(apps.executed.lock().is_empty());
    let refused = output_of(&rig, 1);
    assert!(refused["ok"] == json!(false) && refused["reason"].as_str().unwrap().contains("gmail__search_threads"));
    assert_eq!(sent(&api), vec![line("I can only read mail here.")]);
    assert_eq!(apps.bridge.decide("gmail__trash_thread", None).action.as_str(), "refuse");
    // 3. open_app loads the calendar for the next round, and the calendar may write without asking
    let apps = fake_apps(true, &[]).await;
    let api = FakeApi::new(vec![vec![said("lunch with Ana Friday noon", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![
            call(OPEN_TOOL, json!({"app": "calendar"}), "call_1"),
            call("calendar__create_event", json!({"summary": "Lunch with Ana"}), "call_1"),
            reply("Booked."),
        ]),
        config(),
        with_direct(&apps),
    );
    run_plain(&rig).await;
    assert!(!names(&request(&rig, 0)).contains(&"calendar__create_event".into()));
    assert!(names(&request(&rig, 1)).contains(&"calendar__create_event".into()));
    assert_eq!(*apps.executed.lock(), vec![("calendar__create_event".to_string(), obj(json!({"summary": "Lunch with Ana"})))]);
    assert_eq!(sent(&api), vec![line("Booked.")]);
    // 4. GitHub's write is the owner's yes/no first; "no" refuses without a call, "yes" runs it
    for (answer, executed, last) in [("no", 0, "Okay, not filed."), ("yes", 1, "Filed.")] {
        let apps = fake_apps(true, &[]).await;
        let api = FakeApi::new(vec![vec![said("file an issue on the eve repo: login is slow", 1)]]);
        let rig = Rig::new(
            api.clone(),
            FakeCreate::new(vec![call("github__create_issue", json!({"repo": "eve", "title": "Login is slow"}), "call_1"), reply(last)]),
            config(),
            with_direct(&apps),
        );
        run_rig(&rig, || async {
            let question = sent(&api)[0].1.clone(); // the answer arrives after the question, as on a phone
            assert!(question.starts_with("GitHub: create issue with") && question.contains("yes / no?"), "{question}");
            assert!(question.contains("Login is slow") && apps.executed.lock().is_empty());
            api.feed(vec![said(answer, 2)]);
        })
        .await;
        assert!(apps.executed.lock().len() == executed && sent(&api).last() == Some(&line(last)), "{answer}");
    }
    // 5. before the apps are up nothing of theirs is offered, and a call to them is refused as its result
    let apps = fake_apps(false, &[]).await;
    let api = FakeApi::new(vec![vec![said("any mail?", 1)]]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![call("gmail__search_threads", json!({}), "call_1"), reply("x")]), config(), with_direct(&apps));
    run_plain(&rig).await;
    let first = request(&rig, 0);
    assert!(!first["tools"].as_array().unwrap().iter().any(|t| {
        let name = t.get("name").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_default();
        name.contains("__") || name == OPEN_TOOL
    }));
    assert!(apps.executed.lock().is_empty());
    assert!(output_of(&rig, 1)["reason"].as_str().unwrap().contains("not a tool here"));
    assert_eq!(sent(&api), vec![line("x")]);
}

#[tokio::test]
async fn test_an_email_is_asked_whole_and_sent_only_on_yes() {
    let _iso = isolated();
    let body = format!("Hi Ana,\n\nRunning about 15 minutes late to lunch. {}\n\nSee you soon", "Save me a seat by the window please. ".repeat(4));
    let args = json!({"to": ["ana@example.com"], "subject": "Running late", "body": body});
    for (answer, executed, last) in [("no", 0, "Okay, not sent."), ("yes", 1, "Sent.")] {
        let api = FakeApi::new(vec![vec![said("email ana that im running late", 1)]]);
        // `APP_TOOLS["gmail"].append(send_message)` for this round
        let apps = build_apps(true, &[], false, true).await;
        let rig = Rig::new(api.clone(), FakeCreate::new(vec![call("gmail__send_message", args.clone(), "call_1"), reply(last)]), config(), with_direct(&apps));
        run_rig(&rig, || async {
            let question = sent(&api)[0].1.clone();
            assert!(question.starts_with("Send this email?") && question.contains("To: ana@example.com"), "{question}");
            assert!(question.contains("Subject: Running late") && question.contains(&body)); // every word, line breaks kept
            assert!(!question.contains('…') && apps.executed.lock().is_empty());
            api.feed(vec![said(answer, 2)]);
        })
        .await;
        assert!(apps.executed.lock().len() == executed && sent(&api).last() == Some(&line(last)), "{answer}");
    }
}

#[tokio::test]
async fn test_my_next_class_and_a_maps_link_is_answered_in_two_model_calls() {
    let _iso = isolated();
    let apps = scenario_apps().await;
    let api = FakeApi::new(vec![vec![said("look for my next class on my calendar and give me a maps link", 1)]]);
    let answer = format!("CS 101 at 10, Room 101: {MAPS_URL}");
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("calendar__list_events", json!({"startTime": "now"}), "call_1"), reply(&answer)]),
        config(),
        with_direct(&apps),
    );
    run_plain(&rig).await;
    let first = names(&request(&rig, 0));
    assert!(first.contains(&"calendar__list_events".into()) && first.contains(&"maps__compute_routes".into())); // both open from round one
    assert_eq!(*apps.executed.lock(), vec![("calendar__list_events".to_string(), obj(json!({"startTime": "now"})))]);
    let second = request(&rig, 1);
    let items = second["input"].as_array().unwrap();
    assert!(items.last().unwrap()["output"].as_str().unwrap().contains(MAPS_URL)); // the event carries its link
    assert_eq!(rig.create.requests.lock().len(), 2);
    assert_eq!(sent(&api), vec![line(&answer)]);
}

#[tokio::test]
async fn test_a_direct_tool_called_without_its_app_open_runs_and_opens_the_app() {
    let _iso = isolated();
    let apps = scenario_apps().await;
    let text = "anything coming up for me?";
    assert!(apps.bridge.guess(text).is_empty()); // no app word: nothing opens
    let api = FakeApi::new(vec![vec![said(text, 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("calendar__list_events", json!({}), "call_1"), call("calendar__list_calendars", json!({}), "call_1"), reply("CS 101 at 10.")]),
        config(),
        with_direct(&apps),
    );
    run_plain(&rig).await;
    assert!(!names(&request(&rig, 0)).contains(&"calendar__list_events".into()));
    assert!(names(&request(&rig, 1)).contains(&"calendar__list_events".into())); // offered from the next round
    assert_eq!(apps.executed_names(), vec!["calendar__list_events", "calendar__list_calendars"]);
    assert!(!sent(&api).iter().any(|(_, t)| t == telegram::FAILED_LINE));
    assert_eq!(sent(&api), vec![line("CS 101 at 10.")]);
}

#[tokio::test]
async fn test_an_email_never_goes_to_a_mac_task_while_gmail_can_send_it() {
    // 2026-10-09: "Email me a test saying hi" went to Codex, which drafted it in the browser and asked twice
    let _iso = isolated();
    let apps = scenario_apps().await;
    let api = FakeApi::new(vec![vec![said("Email me a test saying hi", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("start_task", json!({"goal": "Email me a test saying hi"}), "call_1"), reply("Sending it with Gmail.")]),
        config(),
        with_direct(&apps),
    );
    run_plain(&rig).await;
    assert_eq!(first_output(&rig, 1), json!({"ok": false, "reason": telegram::MAIL_SEND_REASON}));
    assert_eq!(sent(&api), vec![line("Sending it with Gmail.")]); // no task, no question asked
}

#[tokio::test]
async fn test_a_made_up_direct_tool_gets_the_apps_real_tools_back_and_the_turn_goes_on() {
    let _iso = isolated();
    let apps = scenario_apps().await;
    let api = FakeApi::new(vec![vec![said("email Sam the notes", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("gmail__send_email", json!({"to": "sam"}), "call_1"), reply("I can read your mail here but not send it.")]),
        config(),
        with_direct(&apps),
    );
    run_plain(&rig).await;
    assert!(apps.executed.lock().is_empty());
    let result = output_of(&rig, 1);
    assert!(result["ok"] == json!(false) && result["reason"].as_str().unwrap().contains("Its tools: gmail__search_threads"), "{result}");
    assert_eq!(sent(&api), vec![line("I can read your mail here but not send it.")]);
}

#[tokio::test]
async fn test_open_app_with_two_apps_opens_both_in_one_round() {
    let _iso = isolated();
    let apps = scenario_apps().await;
    let api = FakeApi::new(vec![vec![said("hi there", 1)]]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![call(OPEN_TOOL, json!({"apps": ["calendar", "maps"]}), "call_1"), reply("Ready.")]), config(), with_direct(&apps));
    run_plain(&rig).await;
    let second = names(&request(&rig, 1));
    assert!(second.contains(&"calendar__list_events".into()) && second.contains(&"maps__compute_routes".into()));
    let opened = output_of(&rig, 1);
    assert!(opened["ok"] == json!(true) && opened["opened"] == json!(["calendar", "maps"]));
    assert_eq!(sent(&api), vec![line("Ready.")]);
    let calls = vec![obj(json!({"name": OPEN_TOOL, "args": {"apps": ["calendar", "maps"]}})), obj(json!({"name": OPEN_TOOL, "args": {"app": "gmail"}}))];
    assert_eq!(telegram::call_names(&calls), "open_app[calendar,maps], open_app[gmail]");
}

// ---- Share ---------------------------------------------------------------------------------------------------

/// `ShareRunner`: osascript, faked: every call recorded; a send says Messages took it.
fn share_runner() -> (eve_brain::share::Runner, Arc<Mutex<Vec<Vec<String>>>>) {
    let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let c = calls.clone();
    let runner: eve_brain::share::Runner = Arc::new(move |argv: Vec<String>, _stdin: Vec<u8>, _timeout: f64| {
        c.lock().push(argv);
        async { (0, String::new(), String::new()) }.boxed()
    });
    (runner, calls)
}

#[tokio::test]
async fn test_share_file_asks_allow_or_deny_and_deny_sends_nothing() {
    for (tap, sent_it) in [("Deny", false), ("Allow", true)] {
        let _iso = isolated();
        let dir = tmp();
        let tmp_path = dir.path().to_path_buf();
        std::fs::create_dir(tmp_path.join("Documents")).unwrap();
        std::fs::write(tmp_path.join("Documents").join("report.pdf"), b"%PDF-1.4 test").unwrap();
        let (runner, calls) = share_runner();
        let hub = eve_brain::share::Share::new(eve_brain::share::ShareConfig { enabled: true, imessage: true })
            .with_runner(runner)
            .with_home(&tmp_path)
            .with_outbox(tmp_path.join("Pictures").join("eve-outbox"));
        let hub = Arc::new(hub);
        let args = json!({"path": tmp_path.join("Documents").join("report.pdf").to_string_lossy(), "to": "+1 555 010 0199",
                          "via": "imessage", "message": null});
        let api = FakeApi::new(vec![vec![said("send the report to +1 555 010 0199", 1)]]);
        let h = hub.clone();
        let rig = Rig::new(api.clone(), FakeCreate::new(vec![call("share_file", args, "call_1"), reply("Done.")]), config(), move |o| o.share = Some(h));
        run_rig(&rig, || async {
            {
                let log = api.log.lock();
                assert_eq!(log.titled.last().unwrap().0.as_deref(), Some("Share a file"));
                let inline: Vec<String> = log.keyboards.last().unwrap().1.iter().flatten().map(|(t, _, _)| t.clone()).collect();
                assert_eq!(inline, vec!["Allow", "Deny"]);
                assert!(log.sent.last().unwrap().1.contains("report.pdf") && calls.lock().is_empty()); // asked first, nothing sent yet
            }
            let (message_id, key) = api.key(tap, -1);
            api.feed(vec![tap_update(2, message_id, &key, OWNER, "q1")]);
        })
        .await;
        assert_eq!(!calls.lock().is_empty(), sent_it, "{tap}");
        let result = output_of(&rig, 1);
        assert!(result["ok"] == json!(sent_it) && result.get("token").is_none(), "{tap}: {result}"); // the token never reaches the model
        assert!(request(&rig, 0)["instructions"].as_str().unwrap().contains(eve_brain::share::INSTRUCTIONS));
    }
}

/// `telegram.request(CFG, [], **parts)`.
fn request_kw(parts: &Obj) -> Obj {
    let s = |k: &str| parts.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let objs = |k: &str| -> Vec<Obj> { parts.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.as_object().cloned()).collect()).unwrap_or_default() };
    let strs = |k: &str| -> Vec<String> { parts.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect()).unwrap_or_default() };
    let environ = conftest_env();
    telegram::request_in(
        Some(&environ),
        &config(),
        &[],
        "",
        &s("profile"),
        &objs("app_tools"),
        parts.get("vault").and_then(Value::as_bool).unwrap_or(false),
        &s("today"),
        &objs("memory_tools"),
        None,
        &objs("watch_tools"),
        &s("watch_block"),
        &strs("direct_apps"),
        &s("lent_block"),
        "",
        &s("recall"),
        &objs("chat_tools"),
    )
}

#[tokio::test]
async fn test_with_sharing_on_the_brain_is_told_someone_else_is_share_file_and_me_is_send_file() {
    let _iso = isolated();
    let dir = tmp();
    let (runner, _) = share_runner();
    let hub = Arc::new(eve_brain::share::Share::new(eve_brain::share::ShareConfig { enabled: true, imessage: true }).with_runner(runner).with_home(dir.path()));
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), move |o| o.share = Some(hub));
    let (parts, allowed, _) = rig.inlet.turn_parts(0, false).await;
    let block = parts["watch_block"].as_str().unwrap();
    assert!(block.contains(r#""Send the report to Ana" is share_file; "send me the report" is send_file"#), "{block}");
    assert!(allowed.iter().any(|a| a == "share_file") && telegram::TOOL_NAMES.contains(&"send_file"));
    // off, send_file's own description is what it was, and no prompt names share_file
    let off = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
    let body = request_kw(&off.inlet.turn_parts(0, false).await.0);
    assert!(!serde_json::to_string(&body).unwrap().contains("share_file"));
}

// ---- web_search (websearch.py) -------------------------------------------------------------------------------

#[tokio::test]
async fn test_web_search_is_a_function_tool_answered_by_exa_when_explicitly_selected() {
    let _iso = isolated();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let environ: eve_brain::prelude::Environ = [
        ("EVE_TELEGRAM", "1"),
        ("EVE_TELEGRAM_TOKEN", "t"),
        ("EVE_TELEGRAM_OWNER", "42"),
        ("OPENROUTER_API_KEY", "r"),
        ("EVE_WEB_SEARCH", "openrouter-exa"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let cfg = telegram::configured(Some(&environ));
    assert_eq!(cfg.search.engine, "openrouter-exa");
    let api = FakeApi::new(vec![vec![said("who won the lakers game", 1)]]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![call("web_search", json!({"query": "lakers score"}), "call_1"), reply("Lakers lost, 104 to 112.")]), cfg, |_| {});
    rig.inlet.tt_set_web_search(Some(Arc::new(move |query: &str, _cfg: &eve_web::websearch::SearchConfig| {
        s.lock().push(query.to_string());
        json!({"ok": true, "answer": "112 to 104", "sources": [{"url": "https://espn.com"}]})
    })));
    run_plain(&rig).await;
    let first = request(&rig, 0);
    let tools = first["tools"].as_array().unwrap();
    assert!(tools.contains(&eve_web::websearch::WEB_SEARCH_TOOL) && !tools.contains(&json!({"type": "web_search"})));
    assert_eq!(*seen.lock(), vec!["lakers score"]);
    assert_eq!(output_of(&rig, 1)["sources"], json!([{"url": "https://espn.com"}]));
    assert_eq!(sent(&api).last().cloned(), Some(line("Lakers lost, 104 to 112.")));
}

// ---- the second brain (second_brain.py) ----------------------------------------------------------------------

fn vault_at(root: PathBuf) -> Arc<eve_second_brain::VaultConfig> {
    Arc::new(eve_second_brain::VaultConfig { enabled: true, root })
}

fn inbound(text: &str, message_id: i64) -> Inbound {
    Inbound { message_id, ..Inbound::new(OWNER, OWNER, text) }
}

fn glob_inbox(root: &Path, needle: &str) -> Vec<PathBuf> {
    std::fs::read_dir(root.join("01-inbox"))
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.file_name().unwrap().to_string_lossy().contains(needle)).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn test_the_second_brain_is_offered_and_captures_from_the_chat() {
    let _iso = isolated();
    let dir = tmp();
    let vault = vault_at(dir.path().join("vault"));
    let api = FakeApi::new(vec![vec![said("note: the pasta place on 5th is great", 1)]]);
    let v = vault.clone();
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("capture_note", json!({"text": "the pasta place on 5th is great", "kind": "note"}), "call_1")]),
        config(),
        move |o| o.vault = Some(v),
    );
    // Wait for the actual filesystem-backed turn, not a fixed number of scheduler yields.
    rig.inlet.tt_turn(&inbound("note: the pasta place on 5th is great", 7)).await;
    let first = request(&rig, 0);
    let offered: Vec<String> = names(&first).into_iter().filter(|n| eve_second_brain::SECOND_BRAIN_TOOL_NAMES.contains(&n.as_str())).collect();
    assert_eq!(offered, eve_second_brain::SECOND_BRAIN_TOOL_NAMES.to_vec());
    assert!(first["instructions"].as_str().unwrap().contains(eve_second_brain::INSTRUCTIONS_BLOCK));
    assert_eq!(rig.create.requests.lock().len(), 1); // no second model call to say "Saved."
    let notes = glob_inbox(&dir.path().join("vault"), "pasta");
    assert!(notes.len() == 1 && std::fs::read_to_string(&notes[0]).unwrap().trim_end().ends_with("the pasta place on 5th is great"));
    let log = api.log.lock();
    assert!(log.reactions == vec![(OWNER, 7, telegram::VAULT_REACTION.to_string())] && log.sent.is_empty()); // ✍ is the receipt
    let last = rig.inlet.turns().last().cloned().unwrap();
    assert!(last.0 == "eve" && last.1.contains("01-inbox/"), "{last:?}"); // history says where
    // off: the tools are not offered and a stray call is told why
    let api = FakeApi::new(vec![vec![said("note: x", 1)]]);
    let rig = Rig::new(api, FakeCreate::new(vec![reply("ok")]), config(), |_| {});
    run_plain(&rig).await;
    let first = request(&rig, 0);
    assert!(!names(&first).iter().any(|n| eve_second_brain::SECOND_BRAIN_TOOL_NAMES.contains(&n.as_str())));
    assert!(!first["instructions"].as_str().unwrap().contains(eve_second_brain::INSTRUCTIONS_BLOCK));
    let result = rig.inlet.tt_tool("capture_note", &obj(json!({"text": "x", "kind": "note"})), OWNER).await;
    assert_eq!(Value::Object(result), json!({"ok": false, "reason": "the second brain is off on this computer (EVE_SECOND_BRAIN)"}));
}

// ---- the watcher (watch.py) ----------------------------------------------------------------------------------

/// `watch.Watcher(watch.WatchConfig(enabled=True, path=path), fetch=fetch, offload=False)`, its ledger in `ledger`.
fn watcher_at(path: &Path, ledger: &Path, fetch: Option<Arc<dyn eve_web::Http>>) -> Arc<eve_web::watch::watcher::Watcher> {
    let mut ctx = eve_web::Ctx::global().clone().with_ledger(eve_spend::Ledger::at(ledger));
    if let Some(f) = &fetch {
        ctx = ctx.with_http(f.clone());
    }
    let mut deps = eve_web::watch::watcher::WatcherDeps::new(ctx);
    if let Some(f) = fetch {
        deps.fetch = f;
    }
    deps.offload = false;
    let config = eve_web::watch::config::WatchConfig { enabled: true, path: path.to_path_buf(), ..Default::default() };
    Arc::new(eve_web::watch::watcher::Watcher::new(config, deps))
}

#[tokio::test]
async fn test_the_watcher_is_offered_and_adds_from_the_chat() {
    let _iso = isolated();
    let dir = tmp();
    let path = dir.path().join("watches.json");
    let quote = json!({"chart": {"result": [{"meta": {"symbol": "AAPL", "currency": "USD", "regularMarketPrice": 341.07}}]}}).to_string();
    let fetched: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let f = fetched.clone();
    let fetch: Arc<dyn eve_web::Http> = Arc::new(move |url: &str, _opts: &eve_web::HttpOptions| -> Result<(u16, String), eve_web::FetchError> {
        f.lock().push(url.to_string());
        Ok((200, quote.clone()))
    });
    let watcher = watcher_at(&path, dir.path(), Some(fetch));
    let add = json!({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": "below", "value": 300, "text": null,
                     "city": null, "every_minutes": null});
    let api = FakeApi::new(vec![]);
    let w = watcher.clone();
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("watch_add", add, "call_1"), reply("Apple is at $341 now. I'll text you under $300.")]),
        config(),
        move |o| o.watcher = Some(w),
    );
    rig.inlet.tt_turn(&inbound("tell me when apple drops under 300", 7)).await;
    let first = request(&rig, 0);
    let offered: Vec<String> = names(&first).into_iter().filter(|n| eve_web::watch::tools::TOOL_NAMES.contains(&n.as_str())).collect();
    assert_eq!(offered, eve_web::watch::tools::TOOL_NAMES.to_vec());
    assert!(first["instructions"].as_str().unwrap().contains(&watcher.instructions()));
    // the first check ran through the watcher and its reading went back to the model
    assert!(fetched.lock().len() == 1 && fetched.lock()[0].contains("AAPL"));
    let output = first_output(&rig, 1);
    assert!(output["ok"] == json!(true) && output["id"] == json!("w1") && output["now"] == json!("$341.07"), "{output}");
    assert_eq!(sent(&api).last().cloned(), Some(line("Apple is at $341 now. I'll text you under $300.")));
    let reread = watcher_at(&path, dir.path(), None);
    assert_eq!(reread.watches().iter().map(|w| w.lock().target.clone()).collect::<Vec<_>>(), vec!["AAPL"]);
    // an alert reaches the owner's chat, titled, and joins the history so a reply can point at it
    rig.inlet.tell_owner("Apple dropped to $299, past your $300 mark.", "", telegram::WATCH_TITLE, &[]).await;
    assert_eq!(
        api.log.lock().titled.last().cloned().unwrap(),
        (Some(telegram::WATCH_TITLE.to_string()), None, "Apple dropped to $299, past your $300 mark.".to_string())
    );
    assert_eq!(rig.inlet.turns().last().cloned().unwrap(), ("eve".to_string(), "Apple dropped to $299, past your $300 mark.".to_string()));
    // /watches is answered by code, no model call
    let before = rig.create.requests.lock().len();
    let api = FakeApi::new(vec![vec![said("/watch", 3)]]);
    let w = watcher.clone();
    run_plain(&Rig::new(api.clone(), FakeCreate::new(vec![]), config(), move |o| o.watcher = Some(w))).await;
    let last = sent(&api).last().cloned().unwrap().1;
    assert!(last.starts_with("w1 · Apple: at or below $300 · now $341.07"), "{last}");
    assert_eq!(rig.create.requests.lock().len(), before);
    // "/watch <words>" is a watch request for the brain, with the watch tools, worded as the owner would say it
    let api = FakeApi::new(vec![vec![said("/watch BTC-USD above 100000", 4)]]);
    let w = watcher.clone();
    let rig = Rig::new(api, FakeCreate::new(vec![reply("On it.")]), config(), move |o| o.watcher = Some(w));
    run_plain(&rig).await;
    let first = request(&rig, 0);
    let said_texts: Vec<String> = first["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i.get("role") == Some(&json!("user")))
        .flat_map(|i| i["content"].as_array().cloned().unwrap_or_default())
        .filter(|c| c.get("type") == Some(&json!("input_text")))
        .map(|c| c["text"].as_str().unwrap().to_string())
        .collect();
    assert!(said_texts.last().unwrap() == "Watch BTC-USD above 100000" && names(&first).contains(&"watch_add".into()));
    // off: no watch tools, no block, and a stray call is told why
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![reply("ok")]), config(), |_| {});
    rig.inlet.tt_turn(&inbound("hi", 8)).await;
    let first = request(&rig, 0);
    assert!(!names(&first).iter().any(|n| eve_web::watch::tools::TOOL_NAMES.contains(&n.as_str())));
    assert!(!first["instructions"].as_str().unwrap().contains("watch_add"));
    assert_eq!(Value::Object(rig.inlet.tt_tool("watch_list", &Map::new(), OWNER).await), json!({"ok": false, "reason": eve_web::watch::OFF_REASON}));
}

// ---- receipts --------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_refused_vault_receipt_says_where_it_went() {
    let _iso = isolated();
    let dir = tmp();
    let vault = vault_at(dir.path().join("vault"));
    let inner = FakeApi::new(vec![]);
    let mut api = Hooked::new(inner.clone());
    api.react = Some(Box::new(|_, _, _| Err(api_error(400, "Bad Request: REACTION_INVALID"))));
    let rig = rig_over(Arc::new(api), FakeCreate::new(vec![call("capture_note", json!({"text": "buy milk", "kind": "todo"}), "call_1")]), move |o| o.vault = Some(vault));
    rig.inlet.tt_turn(&inbound("todo: buy milk", 7)).await;
    let sent = sent(&inner);
    assert!(sent.len() == 1 && sent[0].1.starts_with("Added to the todo list ("), "{sent:?}");
    assert_eq!(rig.create.requests.lock().len(), 1);
}

#[tokio::test]
async fn test_a_star_is_a_trophy_and_a_refused_one_is_a_line() {
    let _iso = isolated();
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let api = FakeApi::new(vec![]);
    let m = memory.clone();
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![call("remember", json!({"claim": "I like ramen"}), "call_1")]), config(), move |o| {
        o.memory = Some(m as Arc<dyn BrainMemory>)
    });
    rig.inlet.tt_turn(&inbound("remember that I like ramen", 4)).await;
    let log = api.log.lock();
    assert!(*memory.stars.lock() == vec!["I like ramen"] && log.reactions == vec![(OWNER, 4, telegram::STAR_REACTION.to_string())] && log.sent.is_empty());
    // the star's result is in the transcript as a tool line; the history's "(Starred for good.)" is not
    let said_lines = memory.said();
    let tools: Vec<&Map<String, Value>> = said_lines.iter().filter(|l| l.get("kind") == Some(&json!("tool"))).collect();
    assert_eq!(tools.iter().map(|l| l["tool"].as_str().unwrap()).collect::<Vec<_>>(), vec!["remember"]);
    assert!(tools[0]["text"].as_str().unwrap().contains("I like ramen"));
    assert_eq!(rig.inlet.turns().last().cloned().unwrap(), ("eve".to_string(), format!("({})", telegram::STARRED_LINE)));
    assert!(!said_lines.iter().any(|l| l.get("text").and_then(Value::as_str).unwrap_or("").contains("Starred for good")));
    // no message id to put it on: the line it stands for
    let api = FakeApi::new(vec![]);
    let m = FakeMemory::new(&dir.path().join("b"), "");
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![call("remember", json!({"claim": "I like ramen"}), "call_1")]), config(), move |o| {
        o.memory = Some(m as Arc<dyn BrainMemory>)
    });
    rig.inlet.tt_turn(&Inbound::new(OWNER, OWNER, "remember that I like ramen")).await;
    assert_eq!(sent(&api), vec![line(telegram::STARRED_LINE)]);
    // no memory lent: nothing is starred, and the tool says why (the model answers with it)
    let api = FakeApi::new(vec![]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("remember", json!({"claim": "I like ramen"}), "call_1"), reply("I can't keep that for good here.")]),
        config(),
        |_| {},
    );
    rig.inlet.tt_turn(&inbound("remember that I like ramen", 5)).await;
    let out = output_of(&rig, 1);
    assert!(out == json!({"ok": false, "reason": "permanent memory is not set up on this computer"}) && api.log.lock().reactions.is_empty());
}

#[tokio::test]
async fn test_only_a_clean_receipt_round_skips_the_answer() {
    // A capture that failed, or one next to another tool, is answered by the model as before; words the model wrote
    // beside a capture still go, with the ✍.
    let _iso = isolated();
    let dir = tmp();
    let vault = vault_at(dir.path().join("vault"));
    // failed: the model explains
    let api = FakeApi::new(vec![]);
    let v = vault.clone();
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("capture_note", json!({"text": "", "kind": "note"}), "call_1"), reply("There was nothing to save.")]),
        config(),
        move |o| o.vault = Some(v),
    );
    rig.inlet.tt_turn(&inbound("note:", 2)).await;
    assert!(api.log.lock().reactions.is_empty() && sent(&api) == vec![line("There was nothing to save.")]);
    // beside another tool: the model answers
    let both = json!({"id": "resp", "output": [
        {"type": "function_call", "name": "capture_note", "call_id": "a",
         "arguments": json!({"text": "call the bank", "kind": "todo"}).to_string()},
        {"type": "function_call", "name": "list_todos", "call_id": "b", "arguments": "{}"}]});
    let api = FakeApi::new(vec![]);
    let v = vault.clone();
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![both, reply("Added. You have one todo.")]), config(), move |o| o.vault = Some(v));
    rig.inlet.tt_turn(&inbound("todo: call the bank, then list them", 3)).await;
    assert!(api.log.lock().reactions.is_empty() && sent(&api) == vec![line("Added. You have one todo.")]);
    // with words of its own: they go, and the ✍ too
    let worded = json!({"id": "resp", "output": [
        {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Good idea!"}]},
        {"type": "function_call", "name": "capture_note", "call_id": "a",
         "arguments": json!({"text": "a bike rack", "kind": "idea"}).to_string()}]});
    let api = FakeApi::new(vec![]);
    let v = vault.clone();
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![worded]), config(), move |o| o.vault = Some(v));
    rig.inlet.tt_turn(&inbound("idea: a bike rack", 5)).await;
    assert!(api.log.lock().reactions == vec![(OWNER, 5, telegram::VAULT_REACTION.to_string())] && sent(&api) == vec![line("Good idea!")]);
}

// ---- one note read, edited and undone --------------------------------------------------------------------------

/// The last request's last tool output.
fn last_output(create: &FakeCreate) -> Value {
    let requests = create.requests.lock();
    let items = requests.last().unwrap()["input"].as_array().unwrap();
    serde_json::from_str(items.last().unwrap()["output"].as_str().unwrap()).unwrap()
}

fn later(make: impl FnOnce() -> Value + Send + 'static) -> Scripted {
    Scripted::Later(Box::new(move || -> BoxFuture<'static, anyhow::Result<Value>> { async move { Ok(make()) }.boxed() }))
}

#[tokio::test]
async fn test_telegram_reads_edits_and_undoes_the_same_note() {
    let _iso = isolated();
    let dir = tmp();
    let vault = vault_at(dir.path().join("vault"));
    let root = vault.root.clone();
    let path = eve_second_brain::capture(&root, "# Shopping\n- [ ] bathroom mat", "note", "telegram", None).unwrap().path;
    let original = std::fs::read(root.join(&path)).unwrap();
    let api = FakeApi::new(vec![]);
    let create = FakeCreate::new(vec![]);
    let (c, p) = (create.clone(), path.clone());
    let edit_from_read = later(move || {
        let note = last_output(&c);
        assert!(note["ok"] == json!(true) && note["path"] == json!(p));
        call("edit_note", json!({"path": note["path"], "revision": note["revision"], "old_text": "", "new_text": "- [ ] alcohol wipes"}), "call_1")
    });
    let (c, r, p) = (create.clone(), root.clone(), path.clone());
    let confirm_edit = later(move || {
        let changed = last_output(&c);
        assert!(changed["ok"] == json!(true) && changed["undo_id"].as_str().is_some_and(|u| !u.is_empty()));
        assert!(std::fs::read_to_string(r.join(&p)).unwrap().ends_with("- [ ] alcohol wipes\n"));
        assert_eq!(eve_second_brain::inbox(&r).unwrap().len(), 1);
        reply("Added alcohol wipes to Shopping.")
    });
    let c = create.clone();
    let undo_from_read = later(move || {
        let note = last_output(&c);
        assert!(note["ok"] == json!(true) && note["undo_id"].as_str().is_some_and(|u| !u.is_empty()));
        call("undo_note", json!({"path": note["path"], "revision": note["revision"], "undo_id": note["undo_id"]}), "call_1")
    });
    let (c, r, p, o) = (create.clone(), root.clone(), path.clone(), original.clone());
    let confirm_undo = later(move || {
        let undone = last_output(&c);
        assert!(undone["ok"] == json!(true) && std::fs::read(r.join(&p)).unwrap() == o);
        reply("Undid that edit to Shopping.")
    });
    create.push(Scripted::Body(call("read_note", json!({"path": path}), "call_1")));
    create.push(edit_from_read);
    create.push(confirm_edit);
    create.push(Scripted::Body(call("read_note", json!({"path": path}), "call_1")));
    create.push(undo_from_read);
    create.push(confirm_undo);
    let v = vault.clone();
    let rig = Rig::new(api.clone(), create.clone(), config(), move |o| o.vault = Some(v));
    // A vault write after reading the note is routine (owner, 2026-10-09): it runs with no question.
    for text in ["add alcohol wipes to my shopping list", "undo that edit"] {
        let asked = api.log.lock().keyboards.len();
        rig.inlet.tt_turn(&Inbound::new(OWNER, OWNER, text)).await;
        let log = api.log.lock();
        assert!(log.keyboards.len() == asked && !log.titled.iter().any(|t| t.0.as_deref() == Some(telegram::GATE_TITLE)), "{text}");
    }
    let stale = rig.inlet.tt_tool("edit_note", &obj(json!({"path": path, "revision": "stale", "old_text": "", "new_text": "oops"})), OWNER).await;
    assert_eq!(stale["ok"], json!(false));
    assert_eq!(sent(&api), vec![line("Added alcohol wipes to Shopping."), line("Undid that edit to Shopping.")]);
    let first_tools = names(&create.requests.lock()[0]);
    assert!(first_tools.contains(&"edit_note".into()) && first_tools.contains(&"undo_note".into()));
    assert_eq!(std::fs::read(root.join(&path)).unwrap(), original);
    assert_eq!(eve_second_brain::inbox(&root).unwrap().len(), 1);
    let off = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
    for name in ["edit_note", "undo_note"] {
        assert_eq!(off.inlet.tt_tool(name, &Map::new(), OWNER).await["ok"], json!(false));
    }
}

// ---- review findings, 2026-10-05: names the prompt offers, and whose question a typed yes answers -----------

// Pinned to EVE_TOOLSET_ROUTER=off (2026-10-05): the door before the toolset router, where Composio and
// Share ride every turn. With the router on they open by the turn's words; that door is test_telegram_toolsets.py.
#[tokio::test]
async fn test_a_wrong_tool_name_is_answered_with_the_offered_tools_and_the_turn_goes_on() {
    // Review, 2026-10-05: a call to a name the turn never offered raised in parse_response and the owner got
    // FAILED_LINE, throwing away every read already done. It is now the call's result, naming the real tools.
    let _iso = isolated();
    let apps = FakeApps::new(true).await;
    let api = FakeApi::new(vec![vec![said("any unread mail?", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("gmail__search_threads", json!({"query": "is:unread"}), "call_1"), reply("Mail isn't connected.")]),
        config(),
        with_apps(&apps),
    );
    run_plain(&rig).await;
    assert!(apps.executed.lock().is_empty());
    let refused = last_output(&rig.create);
    assert!(refused["ok"] == json!(false) && refused["reason"].as_str().unwrap().contains("not a tool here"), "{refused}");
    assert!(refused["reason"].as_str().unwrap().contains("COMPOSIO_SEARCH_TOOLS")); // the tools it does have
    assert_eq!(sent(&api), vec![line("Mail isn't connected.")]);
    // the strict reader is unchanged
    assert!(telegram::parse_response(&call("gmail__search_threads", json!({}), "call_1"), &[], false).is_err());
}

/// `_rundown_skill(request)`: the message part that carries the daily rundown skill.
fn rundown_skill(request: &Value) -> String {
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i.get("type") == Some(&json!("message")))
        .flat_map(|i| i["content"].as_array().cloned().unwrap_or_default())
        .filter_map(|p| p.get("text").and_then(Value::as_str).map(str::to_string))
        .find(|t| t.contains("daily rundown"))
        .unwrap()
}

#[tokio::test]
async fn test_the_rundown_names_composio_only_when_it_is_up() {
    // Review, 2026-10-05: the rundown skill named COMPOSIO_SEARCH_TOOLS with Composio off; one such call threw
    // away the calendar read already done. The skill now names only the sources that are present, and a wrong
    // name is a refused call, not a failed rundown.
    let _iso = isolated();
    let apps = scenario_apps().await;
    let api = FakeApi::new(vec![vec![said("rundown", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![
            call("calendar__list_events", json!({}), "call_1"),
            call("COMPOSIO_SEARCH_TOOLS", json!({}), "call_2"),
            reply("Your day: CS 101 at 10."),
        ]),
        config(),
        with_direct(&apps),
    );
    run_plain(&rig).await;
    let skill = rundown_skill(&request(&rig, 0));
    assert!(!skill.contains("COMPOSIO") && skill.contains("gmail__") && skill.contains("calendar__"), "{skill}");
    assert!(skill.contains("Slack is not connected"));
    assert_eq!(apps.executed_names(), vec!["calendar__list_events"]);
    let refused = last_output(&rig.create);
    assert_eq!(refused["ok"], json!(false));
    assert_eq!(sent(&api), vec![line("Your day: CS 101 at 10.")]);
    // the reverse: Composio alone, no direct apps; the skill names no direct tool
    let composio = FakeApps::new(true).await;
    let api = FakeApi::new(vec![vec![said("rundown", 1)]]);
    let bridge = composio.bridge.clone();
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![reply("Your day.")]), config(), move |o| o.apps = Some(bridge));
    run_plain(&rig).await;
    let skill = rundown_skill(&request(&rig, 0));
    assert!(!skill.contains("gmail__") && !skill.contains("calendar__") && skill.contains("COMPOSIO_SEARCH_TOOLS"), "{skill}");
}

#[tokio::test]
async fn test_a_typed_yes_for_an_older_question_never_shares_a_file() {
    // Review, 2026-10-05: with a task's question open, an Allow question became the newest and the owner's "yes"
    // for the task approved it. An outward send is asked only while no other question waits (as chief_ask).
    let _iso = isolated();
    let dir = tmp();
    let tmp_path = dir.path().to_path_buf();
    std::fs::create_dir(tmp_path.join("Documents")).unwrap();
    std::fs::write(tmp_path.join("Documents").join("report.pdf"), b"%PDF-1.4 test").unwrap();
    let (runner, calls) = share_runner();
    let hub = Arc::new(
        eve_brain::share::Share::new(eve_brain::share::ShareConfig { enabled: true, imessage: true })
            .with_runner(runner)
            .with_home(&tmp_path)
            .with_outbox(tmp_path.join("Pictures").join("eve-outbox")),
    );
    let args = json!({"path": tmp_path.join("Documents").join("report.pdf").to_string_lossy(), "to": "+1 555 010 0199",
                      "via": "imessage", "message": null});
    let api = FakeApi::new(vec![vec![said("send the report to +1 555 010 0199", 1)]]);
    let create = FakeCreate::new(vec![]);
    let h = hub.clone();
    let rig = Rig::new(api.clone(), create.clone(), config(), move |o| o.share = Some(h));
    // a first model round that, while it runs, has a task open its own yes/no question; then it calls share_file
    let task: Arc<Mutex<Option<tokio::task::JoinHandle<String>>>> = Arc::new(Mutex::new(None));
    let (inlet, t) = (rig.inlet.clone(), task.clone());
    create.push(Scripted::Later(Box::new(move || {
        async move {
            let asker = inlet.clone();
            *t.lock() = Some(tokio::spawn(async move { asker.tt_ask_user("Install the package? Reply yes or no.", OWNER).await }));
            settle(20).await;
            Ok(call("share_file", args, "call_1"))
        }
        .boxed()
    })));
    create.push(Scripted::Body(reply("Answer it first.")));
    run_rig(&rig, || async {
        api.feed(vec![said("yes", 2)]);
        settle(200).await;
    })
    .await;
    assert!(calls.lock().is_empty() && hub.pending_count() == 0);
    let handle = task.lock().take().unwrap();
    assert!(handle.is_finished());
    assert_eq!(handle.await.unwrap(), "yes");
    let result = output_of(&rig, 1);
    assert!(result["ok"] == json!(false) && result["reason"].as_str().unwrap().contains("another question"), "{result}");
}

#[tokio::test]
async fn test_a_sentence_that_starts_with_ok_never_approves_a_direct_app_write() {
    // Review, 2026-10-05: the direct app's ask was strict but not firm, so with no relay on "ok, and what's the
    // weather tomorrow?" was its answer and consent.approves read it as a yes: the GitHub issue was filed.
    let _iso = isolated();
    let apps = fake_apps(true, &[]).await;
    let api = FakeApi::new(vec![vec![said("file an issue on the eve repo: login is slow", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![
            call("github__create_issue", json!({"repo": "eve", "title": "Login is slow"}), "call_1"),
            reply("Okay, not filed."),
            reply("Sunny tomorrow."),
        ]),
        config(),
        with_direct(&apps),
    );
    run_rig(&rig, || async {
        api.feed(vec![said("ok, and what's the weather tomorrow?", 2)]);
        settle(200).await;
        assert!(apps.executed.lock().is_empty()); // the sentence approved nothing
        let (message_id, key) = api.key("Deny", -1);
        api.feed(vec![tap_update(3, message_id, &key, OWNER, "q1")]);
        settle(200).await;
    })
    .await;
    assert!(apps.executed.lock().is_empty());
    assert!(sent(&api).contains(&line("Okay, not filed.")));
}

#[tokio::test]
async fn test_a_sentence_that_starts_with_ok_never_approves_a_composio_write() {
    let _iso = isolated();
    let apps = FakeApps::new(true).await;
    let api = FakeApi::new(vec![vec![said("tell the team lunch is at noon", 1)]]);
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![
            call("COMPOSIO_MULTI_EXECUTE_TOOL", multi(&["SLACK_SEND_MESSAGE"], None), "call_1"),
            reply("Okay, not sent."),
            reply("Sunny tomorrow."),
        ]),
        config(),
        with_apps(&apps),
    );
    run_rig(&rig, || async {
        api.feed(vec![said("ok, and what's the weather tomorrow?", 2)]);
        settle(200).await;
        assert!(apps.executed.lock().is_empty());
        let (message_id, key) = api.key("Deny", -1);
        api.feed(vec![tap_update(3, message_id, &key, OWNER, "q1")]);
        settle(200).await;
    })
    .await;
    assert!(apps.executed.lock().is_empty());
}

// ---- test_telegram_toolsets.py: the toolset router wired into the turn ------------------------------------------

const HAND_OFF: &str = "get my resume over to ana before her interview"; // share, with no share word

/// `FakeRouter(*answers, wait=...)`: Jev's stand-in; answers each turn with the toolsets it was given, and records
/// what it was asked.
struct FakeRouter {
    answers: Mutex<Vec<Vec<String>>>,
    asked: Mutex<Vec<(String, String)>>,
    wait: f64,
}

impl FakeRouter {
    fn new(answers: &[&[&str]], wait: f64) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.iter().map(|a| a.iter().map(|s| s.to_string()).collect()).collect()),
            asked: Mutex::default(),
            wait,
        })
    }

    fn router(self: &Arc<Self>) -> eve_toolset_router::Router {
        let me = self.clone();
        Arc::new(move |text: &str, previous: &str| {
            me.asked.lock().push((text.to_string(), previous.to_string()));
            if me.wait > 0.0 {
                std::thread::sleep(std::time::Duration::from_secs_f64(me.wait));
            }
            let mut answers = me.answers.lock();
            let found = if answers.is_empty() { Vec::new() } else { answers.remove(0) };
            Ok(eve_toolset_router::Route::new(found, 180.0, ""))
        })
    }

    fn asked(&self) -> Vec<(String, String)> {
        self.asked.lock().clone()
    }
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

/// `run_until(rig, requests, sent)`: run_rig, waiting in real time (the router answers on a thread) until the model
/// calls and replies are in.
async fn run_until(rig: &Rig, api: &FakeApi, requests: usize, sent_n: usize) {
    run_rig(rig, || async {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (rig.create.requests.lock().len() < requests || api.log.lock().sent.len() < sent_n) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
}

/// `open_enum(body)`: the toolsets open_app can open.
fn open_enum(body: &Value) -> Vec<String> {
    let tool = body["tools"].as_array().unwrap().iter().find(|t| t.get("name") == Some(&json!(OPEN_TOOL))).cloned().unwrap();
    tool["parameters"]["properties"]["apps"]["items"]["enum"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

fn has_open(body: &Value) -> bool {
    names(body).iter().any(|n| n == OPEN_TOOL)
}

/// test_share.py's `FakeRunner([ANA])`: the contacts lookup finds Ana; a send says Messages took it.
fn contacts_runner() -> (eve_brain::share::Runner, Arc<Mutex<Vec<Vec<String>>>>) {
    let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let c = calls.clone();
    let runner: eve_brain::share::Runner = Arc::new(move |argv: Vec<String>, _stdin: Vec<u8>, _timeout: f64| {
        let javascript = argv.iter().any(|a| a == "JavaScript");
        c.lock().push(argv);
        async move {
            if javascript {
                let ana = json!({"name": "Ana Lee", "phones": [{"label": "_$!<Mobile>!$_", "value": "+1 (555) 010-0199"}],
                                 "emails": [{"label": "_$!<Home>!$_", "value": "ana@example.com"}]});
                (0, format!("{}\n", json!({"count": 1, "people": [ana]})), String::new())
            } else {
                (0, String::new(), String::new())
            }
        }
        .boxed()
    });
    (runner, calls)
}

/// `a_share(tmp_path, runner)`: Share over a home with a résumé in Documents.
fn a_share(dir: &Path, runner: Option<eve_brain::share::Runner>) -> Arc<eve_brain::share::Share> {
    let home = dir.join("home");
    std::fs::create_dir_all(home.join("Documents")).unwrap();
    let mut pdf = b"%PDF-1.4 ".to_vec();
    pdf.extend(std::iter::repeat_n(b'x', 2000));
    std::fs::write(home.join("Documents").join("resume.pdf"), pdf).unwrap();
    let runner = runner.unwrap_or_else(|| contacts_runner().0);
    Arc::new(
        eve_brain::share::Share::new(eve_brain::share::ShareConfig { enabled: true, imessage: true })
            .with_runner(runner)
            .with_home(&home)
            .with_outbox(home.join("Pictures").join("eve-outbox")),
    )
}

/// `routed(api, create, router, **kw)`: toolset_mode "auto" with this router (None: keywords only).
fn routed(router: Option<eve_toolset_router::Router>, tweak: impl FnOnce(&mut InletOptions)) -> impl FnOnce(&mut InletOptions) {
    move |o: &mut InletOptions| {
        o.toolset_mode = Some("auto".into());
        o.toolset_route = eve_brain::prelude::Lent::Given(router);
        tweak(o);
    }
}

fn share_names() -> Vec<String> {
    eve_brain::share::TOOL_NAMES.iter().map(|s| s.to_string()).collect()
}

fn offers_all(have: &[String], want: &[String]) -> bool {
    want.iter().all(|w| have.contains(w))
}

fn offers_none(have: &[String], want: &[String]) -> bool {
    !want.iter().any(|w| have.contains(w))
}

#[tokio::test]
async fn test_a_hand_off_with_no_keyword_offers_share_from_round_one_when_jev_says_so() {
    let _iso = isolated();
    assert!(eve_toolset_router::keyword_toolsets(HAND_OFF, "").is_empty()); // the words alone open nothing
    let dir = tmp();
    let router = FakeRouter::new(&[&[eve_toolset_router::SHARE]], 0.0);
    let api = FakeApi::new(vec![vec![said(HAND_OFF, 1)]]);
    let sh = a_share(&dir.path().join("on"), None);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![reply("By text or email?")]), config(), routed(Some(router.router()), move |o| o.share = Some(sh)));
    run_until(&rig, &api, 1, 1).await;
    let first = request(&rig, 0);
    assert!(offers_all(&names(&first), &share_names()));
    assert!(first["instructions"].as_str().unwrap().contains(eve_brain::share::INSTRUCTIONS));
    assert_eq!(router.asked(), vec![pair(HAND_OFF, "")]);
    assert_eq!(sent(&api), vec![line("By text or email?")]);
    // control: Jev says nothing, so Share stays closed, its instructions out, and open_app can open it
    let router = FakeRouter::new(&[&[]], 0.0);
    let api = FakeApi::new(vec![vec![said(HAND_OFF, 1)]]);
    let sh = a_share(&dir.path().join("off"), None);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![reply("By text or email?")]), config(), routed(Some(router.router()), move |o| o.share = Some(sh)));
    run_until(&rig, &api, 1, 1).await;
    let first = request(&rig, 0);
    assert!(offers_none(&names(&first), &share_names()) && !first["instructions"].as_str().unwrap().contains(eve_brain::share::INSTRUCTIONS));
    assert_eq!(open_enum(&first), vec![eve_toolset_router::SHARE]);
}

#[tokio::test]
async fn test_a_router_that_misses_its_budget_leaves_the_keywords_and_the_turn_still_answers() {
    // Rust: TIMEOUT_S is not patched (Python set it to 0.05 s for speed); the router sleeps past the real budget.
    let _iso = isolated();
    let dir = tmp();
    let apps = FakeApps::new(true).await;
    let slow = FakeRouter::new(&[&[eve_toolset_router::SHARE]], eve_toolset_router::TIMEOUT_S + 1.2);
    let api = FakeApi::new(vec![vec![said("any slack dms", 1)]]);
    let (bridge, sh) = (apps.bridge.clone(), a_share(dir.path(), None));
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![reply("Two from Priya.")]),
        config(),
        routed(Some(slow.router()), move |o| {
            o.apps = Some(bridge);
            o.share = Some(sh);
        }),
    );
    run_until(&rig, &api, 1, 1).await;
    let first = request(&rig, 0);
    assert!(names(&first).contains(&"COMPOSIO_SEARCH_TOOLS".to_string())); // the keywords' workspace
    assert!(offers_none(&names(&first), &share_names())); // the late answer added nothing
    assert_eq!(sent(&api), vec![line("Two from Priya.")]);
}

#[tokio::test]
async fn test_the_off_switch_reads_from_the_environment() {
    let _iso = isolated();
    for (mode, want) in [("off", "off"), ("auto", "auto")] {
        // "auto": no Jev key here (conftest): keywords
        let mut env = conftest_env();
        env.insert("EVE_TOOLSET_ROUTER".into(), mode.into());
        let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), move |o| {
            o.toolset_route = eve_brain::prelude::Lent::FromEnv;
            o.environ = Some(env);
        });
        assert_eq!(rig.inlet.tt_toolset(), (want.to_string(), false));
    }
}

#[tokio::test]
async fn test_a_share_tool_called_while_closed_opens_share_and_runs() {
    let _iso = isolated();
    let dir = tmp();
    let (runner, calls) = contacts_runner();
    let api = FakeApi::new(vec![vec![said("hi there", 1)]]);
    let sh = a_share(dir.path(), Some(runner));
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("find_contact", json!({"name": "Ana"}), "call_1"), reply("Ana Lee, +1 (555) 010-0199.")]),
        config(),
        routed(None, move |o| o.share = Some(sh)),
    );
    run_until(&rig, &api, 2, 1).await;
    let (first, second) = (request(&rig, 0), request(&rig, 1));
    assert!(!names(&first).contains(&"find_contact".to_string()));
    assert!(!calls.lock().is_empty()); // it ran
    assert_ne!(output_of(&rig, 1).get("ok"), Some(&json!(false)));
    assert!(offers_all(&names(&second), &share_names()) && second["instructions"].as_str().unwrap().contains(eve_brain::share::INSTRUCTIONS));
    if has_open(&second) {
        assert!(!open_enum(&second).contains(&eve_toolset_router::SHARE.to_string()));
    }
    assert!(!sent(&api).iter().any(|(_, t)| t == telegram::FAILED_LINE));
    assert_eq!(sent(&api), vec![line("Ana Lee, +1 (555) 010-0199.")]);
}

#[tokio::test]
async fn test_a_composio_tool_called_while_the_workspace_is_closed_runs_and_brings_its_block() {
    let _iso = isolated();
    let apps = FakeApps::new(true).await;
    let api = FakeApi::new(vec![vec![said("hi there", 1)]]);
    let bridge = apps.bridge.clone();
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("COMPOSIO_MULTI_EXECUTE_TOOL", multi(&["SLACK_FETCH_CONVERSATION_HISTORY"], None), "call_1"), reply("Nothing new.")]),
        config(),
        routed(None, move |o| o.apps = Some(bridge)),
    );
    run_until(&rig, &api, 2, 1).await;
    let (first, second) = (request(&rig, 0), request(&rig, 1));
    assert!(!names(&first).iter().any(|n| n.starts_with("COMPOSIO")));
    assert!(!first["instructions"].as_str().unwrap().contains(telegram::APPS_BLOCK)); // its block rides only with its tools
    assert_eq!(open_enum(&first), vec![eve_toolset_router::WORKSPACE]);
    assert_eq!(apps.executed_names(), vec!["COMPOSIO_MULTI_EXECUTE_TOOL"]);
    assert!(names(&second).contains(&"COMPOSIO_SEARCH_TOOLS".to_string()) && second["instructions"].as_str().unwrap().contains(telegram::APPS_BLOCK));
    assert_eq!(sent(&api), vec![line("Nothing new.")]);
}

#[tokio::test]
async fn test_open_app_share_offers_the_share_tools_on_the_next_round() {
    let _iso = isolated();
    let dir = tmp();
    let apps = scenario_apps().await;
    let api = FakeApi::new(vec![vec![said("hi there", 1)]]);
    let (sh, bridge) = (a_share(dir.path(), None), apps.bridge.clone());
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call(OPEN_TOOL, json!({"apps": ["share"]}), "call_1"), reply("Ready.")]),
        config(),
        routed(None, move |o| {
            o.share = Some(sh);
            o.direct = Some(bridge);
        }),
    );
    run_until(&rig, &api, 2, 1).await;
    let (first, second) = (request(&rig, 0), request(&rig, 1));
    let opens = open_enum(&first);
    assert!(opens.contains(&eve_toolset_router::SHARE.to_string()) && opens.contains(&"calendar".to_string()));
    assert!(offers_none(&names(&first), &share_names()));
    let opened = output_of(&rig, 1);
    let mut tools: Vec<String> = opened["tools"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
    tools.sort();
    let mut want = share_names();
    want.sort();
    assert!(opened["ok"] == json!(true) && opened["opened"] == json!(["share"]) && tools == want, "{opened}");
    assert!(offers_all(&names(&second), &share_names()) && !open_enum(&second).contains(&eve_toolset_router::SHARE.to_string()));
    assert_eq!(sent(&api), vec![line("Ready.")]);
}

#[tokio::test]
async fn test_the_routed_toolsets_sit_last_and_open_app_is_the_very_last() {
    let _iso = isolated();
    let dir = tmp();
    let router = FakeRouter::new(&[&[eve_toolset_router::SHARE, "calendar"]], 0.0);
    let apps = scenario_apps().await;
    let api = FakeApi::new(vec![vec![said(HAND_OFF, 1)]]);
    let (sh, bridge) = (a_share(dir.path(), None), apps.bridge.clone());
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![reply("ok")]),
        config(),
        routed(Some(router.router()), move |o| {
            o.share = Some(sh);
            o.direct = Some(bridge);
        }),
    );
    run_until(&rig, &api, 1, 1).await;
    let have = names(&request(&rig, 0));
    assert_eq!(have.last().unwrap(), OPEN_TOOL);
    let at = |n: &str| have.iter().position(|h| h == n).unwrap();
    let first_lent = share_names().iter().map(|n| at(n)).min().unwrap();
    let stable = telegram::offered_names(&telegram::own_tools_in(Some(&conftest_env())));
    assert!(stable.iter().all(|n| at(n) < first_lent));
    assert!(have[..first_lent].iter().all(|n| !share_names().contains(n)));
    assert!(at("calendar__list_events") < first_lent); // the direct apps, then the lent ones
}

#[tokio::test]
async fn test_my_next_class_and_a_maps_link_is_still_two_model_calls_with_the_router_on() {
    let _iso = isolated();
    let apps = scenario_apps().await;
    let text = "look for my next class on my calendar and give me a maps link";
    let router = FakeRouter::new(&[&["calendar", "maps"]], 0.0);
    let api = FakeApi::new(vec![vec![said(text, 1)]]);
    let bridge = apps.bridge.clone();
    let answer = format!("CS 101, Room 101: {MAPS_URL}");
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![call("calendar__list_events", json!({"startTime": "now"}), "call_1"), reply(&answer)]),
        config(),
        routed(Some(router.router()), move |o| o.direct = Some(bridge)),
    );
    run_until(&rig, &api, 2, 1).await;
    let first = names(&request(&rig, 0));
    assert!(first.contains(&"calendar__list_events".to_string()) && first.contains(&"maps__compute_routes".to_string()));
    assert!(rig.create.requests.lock().len() == 2 && router.asked() == vec![pair(text, "")]);
    assert_eq!(sent(&api), vec![line(&answer)]);
}

#[tokio::test]
async fn test_and_send_it_to_ana_after_find_my_resume_in_drive_opens_drive_and_share() {
    let _iso = isolated();
    let dir = tmp();
    let apps = scenario_apps().await;
    let router = FakeRouter::new(&[&["drive"], &[]], 0.0); // Jev adds nothing on the follow-up
    let api = FakeApi::new(vec![vec![said("find my resume in drive", 1)], vec![said("and send it to ana", 2)]]);
    let (sh, bridge) = (a_share(dir.path(), None), apps.bridge.clone());
    let rig = Rig::new(
        api.clone(),
        FakeCreate::new(vec![reply("Found Resume.pdf."), reply("Sending it to Ana?")]),
        config(),
        routed(Some(router.router()), move |o| {
            o.direct = Some(bridge);
            o.share = Some(sh);
        }),
    );
    run_until(&rig, &api, 2, 2).await;
    assert_eq!(router.asked(), vec![pair("find my resume in drive", ""), pair("and send it to ana", "find my resume in drive")]);
    let second = request(&rig, 1);
    assert!(offers_all(&names(&second), &share_names()));
    let opens = open_enum(&second);
    assert!(!opens.contains(&"drive".to_string()) && !opens.contains(&eve_toolset_router::SHARE.to_string()));
    assert_eq!(rig.inlet.tt_open_toolsets(), (vec!["drive".to_string()], vec![eve_toolset_router::SHARE.to_string()]));
}

#[tokio::test]
async fn test_a_rundown_carries_no_share_block_since_it_offers_no_share_tool() {
    let _iso = isolated();
    for mode in ["off", "auto"] {
        let dir = tmp();
        let api = FakeApi::new(vec![vec![said("/rundown", 1)]]);
        let sh = a_share(dir.path(), None);
        let rig = Rig::new(api.clone(), FakeCreate::new(vec![reply("Quiet day.")]), config(), move |o| {
            o.toolset_mode = Some(mode.into());
            o.share = Some(sh);
        });
        run_plain(&rig).await;
        let body = request(&rig, 0);
        assert!(offers_none(&names(&body), &share_names()), "{mode}");
        assert!(!body["instructions"].as_str().unwrap().contains("share_file"), "{mode}");
        // control: an ordinary turn in off mode still lends it, block and tools
        let api = FakeApi::new(vec![vec![said("hi", 1)]]);
        let sh = a_share(&dir.path().join("again"), None);
        let rig = Rig::new(api.clone(), FakeCreate::new(vec![reply("Hi.")]), config(), move |o| {
            o.toolset_mode = Some("off".into());
            o.share = Some(sh);
        });
        run_plain(&rig).await;
        let body = request(&rig, 0);
        assert!(body["instructions"].as_str().unwrap().contains("share_file") && offers_all(&names(&body), &share_names()), "{mode}");
    }
}
