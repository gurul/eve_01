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
