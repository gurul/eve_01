//! test_telegram_lite.py: telegram.py in eve lite (EVE_PROFILE=lite), the brain of eve's web app, ported with its
//! names and assertions. Its configuration and 402/401 tests are in telegram_prompts.rs.
//!
//! The `lite` fixture's `monkeypatch.setenv` is the inlet's lent environ (LITE_ENV over conftest's pins); `full` is
//! conftest's alone. `FakeConnect` (composio_connect.ConnectBridge's duck type) is the real [`ComposioBridge`] over a
//! fake session serving Composio Connect's seven meta tools from the Python suite's fixtures. Python's
//! `spy_handlers` swapped TOOL_HANDLERS for spies; Rust dispatches by `match`, so "no handler ran" is read off what
//! each handler would have touched (the agent, the launcher, the screen, the apps, the model, the chat), and "it
//! ran" off the lent fake it reaches.

mod more;
mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_brain::prelude::{BrainMemory, Environ, Obj, Sessions};
use eve_brain::seams::{in_ctx, TurnCtx};
use eve_brain::telegram::{self as telegram, TelegramConfig, LITE_NOT_HERE, LITE_TOOL_OFF};
use eve_composio_tools::{ComposioBridge, ComposioClient, ComposioConfig, ComposioError, ComposioSession};
use more::*;
use support::*;

const PLACEHOLDER: &str = "placeholder-not-a-key";
const COMPOSIO_NAMES: [&str; 7] = [
    "COMPOSIO_SEARCH_TOOLS",
    "COMPOSIO_GET_TOOL_SCHEMAS",
    "COMPOSIO_MULTI_EXECUTE_TOOL",
    "COMPOSIO_MANAGE_CONNECTIONS",
    "COMPOSIO_WAIT_FOR_CONNECTIONS",
    "COMPOSIO_REMOTE_WORKBENCH",
    "COMPOSIO_REMOTE_BASH_TOOL",
];
const REPO: &str = "/Users/g/repo";

/// LITE_ENV over conftest's pins: what the `lite` fixture puts in the environment.
fn lite_env() -> Environ {
    let mut env = conftest_env();
    for (k, v) in [
        ("EVE_PROFILE", "lite"),
        ("OPENROUTER_API_KEY", PLACEHOLDER),
        ("EVE_TELEGRAM", "app"),
        ("EVE_MEMORY", "1"),
        ("EVE_TOOLSET_ROUTER", "off"),
        ("EVE_GRADE_ROUTER", "off"),
        ("EVE_SEARCH_ROUTER", "off"),
        ("EVE_MENU_TOOLS", "off"),
        ("EVE_WATCH_TLS", "0"),
        ("EVE_WATCH_BROWSER", "0"),
        ("EVE_WATCH_HOSTED", "0"),
        ("EVE_COMPOSIO_POLICY", "gmail=auto"),
        ("EVE_APPS", "0"),
        ("EVE_COMPOSIO", "0"),
        ("EVE_CHIEF", "off"),
        ("EVE_ROBOT", "0"),
        ("EVE_COMPUTER_CONTROL", "0"),
    ] {
        env.insert(k.into(), v.into());
    }
    env
}

/// `TelegramConfig(enabled=True, app_only=True, owner_ids=frozenset({OWNER}))`: full eve's app-only door.
fn full_config() -> TelegramConfig {
    TelegramConfig { app_only: true, ..config() }
}

// ---- FakeConnect ---------------------------------------------------------------------------------------------------

/// `connect_tools()`: Composio Connect's meta tools as Responses function tools, from spike S1's tool objects.
fn connect_tools() -> Vec<Value> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bridge/tests/fixtures/composio_connect");
    COMPOSIO_NAMES
        .iter()
        .map(|name| {
            let raw = std::fs::read_to_string(dir.join(format!("{name}.json"))).unwrap();
            let obj: Value = serde_json::from_str(&raw).unwrap();
            let mut schema = obj["inputSchema"].as_object().cloned().unwrap();
            schema.remove("$schema");
            let description: String = obj["description"].as_str().unwrap().trim().chars().take(400).collect();
            json!({"type": "function", "name": name, "description": description, "parameters": schema, "strict": false})
        })
        .collect()
}

type Executed = Arc<Mutex<Vec<(String, Map<String, Value>)>>>;

struct ConnectSession {
    executed: Executed,
}

#[async_trait]
impl ComposioSession for ConnectSession {
    fn session_id(&self) -> String {
        "connect".into()
    }
    async fn tools(&self) -> Result<Vec<Value>, ComposioError> {
        Ok(connect_tools())
    }
    async fn execute(&self, slug: &str, arguments: &Map<String, Value>) -> Result<Value, ComposioError> {
        self.executed.lock().push((slug.to_string(), arguments.clone()));
        if slug == eve_composio_tools::MANAGE_CONNECTIONS {
            return Ok(json!({"successful": true, "data": {"results": {"gmail": {
                "status": "initiated", "redirect_url": "https://connect.composio.dev/link/ln_test"}}}}));
        }
        Ok(json!({"successful": true, "data": {"results": [{"successful": true}]}}))
    }
}

struct ConnectClient {
    executed: Executed,
}

#[async_trait]
impl ComposioClient for ConnectClient {
    async fn create_session(&self, _user_id: &str, _toolkits: &Value, _multi: Option<&Value>) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        Ok(Arc::new(ConnectSession { executed: self.executed.clone() }))
    }
    async fn use_session(&self, _session_id: &str) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        Ok(Arc::new(ConnectSession { executed: self.executed.clone() }))
    }
    async fn list_connected_accounts(&self, _user_ids: &[String]) -> Result<Vec<Value>, ComposioError> {
        Ok(Vec::new())
    }
    async fn execute_tool(&self, _slug: &str, _arguments: &Map<String, Value>, _user_id: &str, _account: &str) -> Result<Value, ComposioError> {
        Err(ComposioError::new("AssertionError", "not in these tests"))
    }
}

struct FakeConnect {
    bridge: Arc<ComposioBridge>,
    executed: Executed,
    _dir: tempfile::TempDir,
}

impl FakeConnect {
    async fn new() -> Self {
        let dir = tmp();
        let executed: Executed = Arc::new(Mutex::new(Vec::new()));
        let e = executed.clone();
        let mut config = ComposioConfig::new(true, "owner");
        config.state_path = dir.path().join("composio.json");
        config.toolkits = None;
        let bridge = ComposioBridge::new(config, Some(Arc::new(move || Ok(Arc::new(ConnectClient { executed: e.clone() }) as Arc<dyn ComposioClient>))));
        bridge.start().await.unwrap();
        Self { bridge: Arc::new(bridge), executed, _dir: dir }
    }

    fn executed(&self) -> Vec<(String, Map<String, Value>)> {
        self.executed.lock().clone()
    }
}

// ---- the lite rig ------------------------------------------------------------------------------------------------

/// What the lite rig lent that a test reads back.
struct Lent {
    memory: Arc<FakeMemory>,
    launched: Arc<Mutex<Vec<String>>>,
    typed: Arc<Mutex<Vec<String>>>,
    shots: Arc<Mutex<i64>>,
    searched: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

fn watcher_at(path: &Path, ledger: &Path) -> Arc<eve_web::watch::watcher::Watcher> {
    let ctx = eve_web::Ctx::global().clone().with_ledger(eve_spend::Ledger::at(ledger));
    let mut deps = eve_web::watch::watcher::WatcherDeps::new(ctx);
    deps.offload = false;
    let config = eve_web::watch::config::WatchConfig { enabled: true, path: path.to_path_buf(), ..Default::default() };
    Arc::new(eve_web::watch::watcher::Watcher::new(config, deps))
}

/// `lite_rig(tmp_path, *responses, config=..., apps=...)`: a TelegramInlet lent what lite.py lends (memory, chat log,
/// watcher, Connect, no agent, no launcher, no screen, no Claude sessions, no Codex folders). `environ` is lite's or
/// full eve's.
fn lite_rig(responses: Vec<Value>, cfg: Option<TelegramConfig>, environ: Environ, apps: Option<&FakeConnect>) -> (Rig, Lent) {
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "Name: Sam. Likes green tea.");
    let (launched, typed, shots, searched) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(0)), Arc::new(Mutex::new(Vec::new())));
    let chat_log = Arc::new(eve_chat_log::ChatLog::new(dir.path().join("chat").join("log.json")));
    let watcher = watcher_at(&dir.path().join("watches.json"), &dir.path().join("ledger"));
    let (m, l, t, s) = (memory.clone(), launched.clone(), typed.clone(), shots.clone());
    let bridge = apps.map(|a| a.bridge.clone());
    let lent = Lent { memory, launched, typed, shots, searched: searched.clone(), _dir: dir };
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(responses), cfg.unwrap_or_else(|| telegram::configured(Some(&lite_env()))), move |o| {
        o.agent_factory = None;
        o.agent_enabled = false;
        o.launcher = Some(Arc::new(move |folder: std::path::PathBuf, _harness: String| {
            l.lock().push(folder.to_string_lossy().into_owned());
            async { Ok("refused".to_string()) }.boxed()
        }));
        o.launch_recent = Some(Arc::new(Vec::new));
        o.claude_sessions = Some(Arc::new(|| Ok(Sessions::Now(vec![REPO.to_string()]))));
        o.codex_folders = Some(Arc::new(Vec::new));
        o.screen = Some(Arc::new(move || {
            *s.lock() += 1;
            None
        }));
        o.thinker = Some(Arc::new(|_q: String, _c: String| async { Ok(obj(json!({"ok": true, "answer": "42"}))) }.boxed()));
        o.terminal = Some(Arc::new(move |_cwd: String, text: String| {
            t.lock().push(text);
            async { Ok(String::new()) }.boxed()
        }));
        o.memory = Some(m as Arc<dyn BrainMemory>);
        o.chat_log = Some(chat_log);
        o.watcher = Some(watcher);
        o.apps = bridge;
        o.environ = Some(environ);
    });
    let found = searched;
    rig.inlet.tt_set_web_search(Some(Arc::new(move |query: &str, _config: &eve_web::websearch::SearchConfig| {
        found.lock().push(query.to_string());
        json!({"ok": true, "answer": "Sunny."})
    })));
    (rig, lent)
}

/// `drain(rig)`: every job the inlet started, and the ones those started, run to their end.
async fn drain(rig: &Rig) {
    for _ in 0..20 {
        settle(50).await;
        rig.inlet.say_relay_probe().jobs().await;
    }
}

type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

/// `rig.inlet.app_out = app_out`: what the app is handed, as (turn id, line, kind).
fn app_seen(rig: &Rig) -> Seen {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    *rig.inlet.app_out.lock() = Some(Arc::new(move |turn: &str, line: &str, kind: &str| {
        s.lock().push((turn.to_string(), line.to_string(), kind.to_string()));
        true
    }));
    seen
}

fn hear_app(rig: &Rig, text: &str, turn: &str) {
    rig.inlet.hear(text, eve_phone_call::Captured::Uncaptured, false, None, Some(turn), "auto", &[]);
}

/// `app_turns(rig, *texts)`: each text as an app turn of its own, one after another.
async fn app_turns(rig: &Rig, texts: &[&str]) -> Vec<(String, String, String)> {
    let seen = app_seen(rig);
    for (i, text) in texts.iter().enumerate() {
        hear_app(rig, text, &format!("t-{}", i + 1));
        drain(rig).await;
    }
    let out = seen.lock().clone();
    out
}

/// `app_turns(rig, text, during=...)`: the turn asks the owner something; once the question reached the app it is
/// answered with `answer`, then the turn is run to its end. → (what the app was handed, the asks it saw).
async fn app_turn_answered(rig: &Rig, text: &str, answer: &str, check: impl FnOnce(&[String])) -> Vec<(String, String, String)> {
    let seen = app_seen(rig);
    hear_app(rig, text, "t-1");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !seen.lock().iter().any(|(_, _, kind)| kind == "ask") && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let asks: Vec<String> = seen.lock().iter().filter(|(_, _, k)| k == "ask").map(|(_, l, _)| l.clone()).collect();
    check(&asks);
    hear_app(rig, answer, "t-answer");
    drain(rig).await;
    let out = seen.lock().clone();
    out
}

fn said(seen: &[(String, String, String)]) -> Vec<String> {
    seen.iter().map(|(_, line, _)| line.clone()).collect()
}

fn row(turn: &str, line: &str, kind: &str) -> (String, String, String) {
    (turn.to_string(), line.to_string(), kind.to_string())
}

// ---- configuration --------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_make_inlet_mutes_telegram_and_the_inlet_has_its_one_chat() {
    // Rust: make_inlet's API is make_api's (telegram_wire); the lite env with EVE_NOTIFY=0 lends no notifier, and ""
    // is no token to hide (redact changes nothing).
    let _iso = isolated();
    let mut env = lite_env();
    env.insert("EVE_NOTIFY".into(), "0".into());
    let cfg = telegram::configured(Some(&env));
    let api = telegram::make_api(&cfg, Some(&env));
    assert!(api.muted() && api.notifier().is_none());
    assert_eq!(telegram::redact("POST https://api.telegram.org/bot/sendMessage"), "POST https://api.telegram.org/bot/sendMessage");
    let (rig, _lent) = lite_rig(vec![], None, lite_env(), None);
    assert_eq!(rig.inlet.tt_chat_id(), Some(eve_profile::LITE_CHAT_ID));
}

// ---- code words of features eve lite does not have --------------------------------------------------------------

const BLOCKED_WORDS: [&str; 37] = [
    "claude on", "/claude_on", "claude off", "/claude_off", "claude on eve", "claude: fix the failing tests",
    "> fix the failing tests", "new claude", "/new_claude", "start claude in eve", "codex", "/codex on",
    "codex use eve", "codex: fix the failing tests", "stealth mode", "/stealth", "wake up", "/apps", "my apps",
    "open eve", "/stick", "eve link", "/stick new", "screenshot", "send me a screenshot", "what's on my screen",
    "/meet", "/meet 6pm", "/meet https://meet.google.com/abc-defg-hij", "/america",
    "/america when are taxes due", "/opentable", "/opentable dinner for 2 friday", "/jobs", "/jobs c12", "go c12",
    "done c3",
];

#[tokio::test]
async fn test_a_code_word_of_a_feature_lite_lacks_gets_one_line_and_nothing_else() {
    let _iso = isolated();
    for word in BLOCKED_WORDS {
        let (rig, lent) = lite_rig(vec![], None, lite_env(), None);
        let seen = app_turns(&rig, &[word]).await; // the relay's way into a terminal (the lent terminal): never reached
        assert_eq!(seen, vec![row("t-1", LITE_NOT_HERE, "say")], "{word}");
        assert!(rig.create.requests.lock().is_empty() && rig.api.log.lock().sent.is_empty() && lent.typed.lock().is_empty(), "{word}");
        assert!(!rig.inlet.stealth() && !rig.inlet.claude() && rig.inlet.tt_nothing_started(), "{word}");
        assert!(lent.launched.lock().is_empty() && !rig.inlet.task_running(), "{word}");
        // the exchange is in the chat's record, as a quick answer's is, so the web app shows it after a reload
        let lines: Vec<(String, String)> = lent
            .memory
            .said()
            .iter()
            .filter(|l| l.get("kind").and_then(Value::as_str) == Some("say"))
            .map(|l| (l["who"].as_str().unwrap().to_string(), l["text"].as_str().unwrap().to_string()))
            .collect();
        assert_eq!(lines[lines.len() - 2..].to_vec(), vec![("owner".to_string(), word.to_string()), ("eve".to_string(), LITE_NOT_HERE.to_string())], "{word}");
    }
}

#[tokio::test]
async fn test_the_harness_sees_a_side_effect_when_there_is_one() {
    // Control for the test above: in full eve the same harness sees stealth go on, and the relay's line typed.
    let _iso = isolated();
    let (rig, lent) = lite_rig(vec![], Some(full_config()), conftest_env(), None);
    rig.inlet.tt_relay_to(REPO);
    let seen = app_turns(&rig, &["stealth mode", "claude: fix the failing tests"]).await;
    assert!(rig.inlet.stealth() && said(&seen).contains(&telegram::STEALTH_ON_LINE.to_string()), "{seen:?}");
    assert!(!lent.typed.lock().is_empty(), "{seen:?}");
    assert!(!said(&seen).contains(&LITE_NOT_HERE.to_string()));
}

#[tokio::test]
async fn test_stop_spend_watch_and_quick_answers_keep_working_in_lite() {
    // Rust: `_spend_text` is not patched; /spend answers with the spend brief of the test's own (empty) ledger.
    let _iso = isolated();
    for word in ["stop", "/spend", "/watches", "what time is it"] {
        let (rig, _lent) = lite_rig(vec![], None, lite_env(), None);
        let seen = app_turns(&rig, &[word]).await;
        assert!(!seen.is_empty() && !said(&seen).contains(&LITE_NOT_HERE.to_string()) && rig.create.requests.lock().is_empty(), "{word}: {seen:?}");
        match word {
            "stop" => assert_eq!(said(&seen), vec![telegram::NOTHING_TO_STOP_LINE]),
            "/spend" => assert_eq!(said(&seen).len(), 1),
            "/watches" => assert!(said(&seen)[0].contains("Watching"), "{seen:?}"),
            _ => {}
        }
    }
}

// ---- tools eve lite does not have: refused before any handler ---------------------------------------------------

const NOT_LITE_TOOLS: [&str; 21] = [
    "screenshot", "start_task", "send_file", "list_files", "read_file", "take_photo", "start_coding_session",
    "claude_relay", "codex_relay", "steer_task", "stop_task", "look", "move_head", "take_notes", "set_stealth",
    "pair_eve_link", "open_app", "capture_note", "opentable_search", "lights_control", "",
];

#[tokio::test]
async fn test_a_tool_lite_lacks_is_refused_and_no_handler_runs() {
    let _iso = isolated();
    for name in NOT_LITE_TOOLS {
        let apps = FakeConnect::new().await;
        let (rig, lent) = lite_rig(vec![], None, lite_env(), Some(&apps));
        let result = rig.inlet.tt_tool(name, &obj(json!({"goal": "open Safari", "path": "~/Desktop/a.txt"})), 1).await;
        assert_eq!(Value::Object(result), json!({"ok": false, "reason": LITE_TOOL_OFF}), "{name}");
        settle(50).await;
        assert!(apps.executed().is_empty() && rig.create.requests.lock().is_empty(), "{name}");
        assert!(rig.api.log.lock().sent.is_empty() && rig.api.log.lock().photos.is_empty() && rig.api.log.lock().documents.is_empty(), "{name}");
        assert!(lent.launched.lock().is_empty() && *lent.shots.lock() == 0 && lent.typed.lock().is_empty(), "{name}");
        assert!(rig.inlet.tt_nothing_started() && !rig.inlet.stealth() && lent.memory.stars.lock().is_empty(), "{name}");
    }
}

#[tokio::test]
async fn test_the_handlers_do_run_in_full_eve_and_for_lites_own_tools() {
    // Controls: lite's own fixed tools reach their handlers, and in full eve a Mac tool reaches its handler.
    let _iso = isolated();
    let (rig, lent) = lite_rig(vec![], None, lite_env(), None);
    rig.inlet.tt_tool("remember", &obj(json!({"claim": "Sam likes green tea"})), 1).await;
    rig.inlet.tt_tool("web_search", &obj(json!({"query": "weather in Lisbon"})), 1).await;
    assert_eq!(*lent.memory.stars.lock(), vec!["Sam likes green tea"]);
    assert_eq!(*lent.searched.lock(), vec!["weather in Lisbon"]);
    let (full, lent) = lite_rig(vec![], Some(full_config()), conftest_env(), None);
    full.inlet.tt_tool("screenshot", &obj(json!({"caption": ""})), 1).await;
    assert_eq!(*lent.shots.lock(), 1);
}

#[tokio::test]
async fn test_a_turn_whose_model_calls_a_tool_lite_lacks_is_told_which_it_has_and_goes_on() {
    // parse_response takes any of eve's own names; in lite the turn refuses one it does not have as it refuses any
    // name not offered, and the result lists the tools it has, so the next round repairs it. No handler runs and the
    // owner is asked nothing.
    let _iso = isolated();
    for (name, args) in [("screenshot", json!({"caption": "here"})), ("take_notes", json!({"action": "start"})), ("start_task", json!({"goal": "open Safari"}))] {
        let (rig, lent) = lite_rig(vec![call(name, args, "call_1"), reply("eve lite can't do that, but I can look it up for you.")], None, lite_env(), None);
        let seen = app_turns(&rig, &["do that thing on my mac now"]).await;
        assert_eq!(said(&seen), vec!["eve lite can't do that, but I can look it up for you."], "{name}");
        assert!(!seen.iter().any(|(_, _, kind)| kind == "ask") && *lent.shots.lock() == 0 && rig.inlet.tt_nothing_started(), "{name}");
        let requests = rig.create.requests.lock().clone();
        let output = requests[1]["input"].as_array().unwrap().last().unwrap()["output"].as_str().unwrap().to_string();
        let refused: Value = serde_json::from_str(&output).unwrap();
        let tools: Vec<Obj> = requests[0]["tools"].as_array().unwrap().iter().map(|t| t.as_object().cloned().unwrap()).collect();
        let offered = telegram::offered_names(&tools);
        assert_eq!(refused, json!({"ok": false, "reason": format!("that name is not a tool here; the tools this turn has: {}", offered.join(", "))}));
        assert!(!offered.contains(&name.to_string()) && !refused["reason"].as_str().unwrap().contains(name), "{name}");
    }
}

#[tokio::test]
async fn test_a_tainted_lite_turn_never_asks_about_a_tool_lite_lacks() {
    // Recording the room after outside words is a question first (tool_gate.needs_confirmation); in lite the call is
    // refused before that question exists.
    let _iso = isolated();
    assert!(eve_tool_gate::needs_confirmation("take_notes", &obj(json!({"action": "start"})), "a web search")); // the gate would ask
    let (rig, _lent) = lite_rig(vec![call("take_notes", json!({"action": "start"}), "call_1"), reply("Not in eve lite.")], None, lite_env(), None);
    let (reply_text, rounds) = rig.inlet.tt_think(&[telegram::message_item("user", "take notes")], 1, "a web search").await.unwrap();
    assert!(reply_text == "Not in eve lite." && rounds == 2);
    assert!(rig.inlet.tt_live_question().is_none() && rig.api.log.lock().sent.is_empty());
}

// ---- OpenRouter's 402 and 401 -----------------------------------------------------------------------------------

/// `openai.APIStatusError(f"HTTP {code}")` as eve-openrouter raises it, as the model call's failure.
fn status_error(code: u16) -> Scripted {
    Scripted::Later(Box::new(move || {
        async move { Err(anyhow::Error::new(eve_openrouter::Error::Status { status: code, body: format!("HTTP {code}"), headers: Box::default() })) }.boxed()
    }))
}

fn failing_rig(code: u16, cfg: Option<TelegramConfig>, environ: Environ) -> (Rig, Lent) {
    let made = lite_rig(vec![], cfg, environ, None);
    made.0.create.push(status_error(code));
    made
}

#[tokio::test]
async fn test_a_turn_that_gets_402_says_to_add_credits_in_lite_and_full_eve_says_what_it_said() {
    let _iso = isolated();
    let (rig, _lent) = failing_rig(402, None, lite_env());
    assert_eq!(said(&app_turns(&rig, &["hello there, how are you doing"]).await), vec![telegram::LITE_NO_CREDIT_LINE]);
    assert_eq!(telegram::LITE_NO_CREDIT_LINE, "OpenRouter has no credit on this key. Add credits at openrouter.ai, then send that again.");
    // control: full eve's turn failure line is unchanged
    let (rig, _lent) = failing_rig(402, Some(full_config()), conftest_env());
    assert_eq!(said(&app_turns(&rig, &["hello there, how are you doing"]).await), vec![telegram::FAILED_LINE]);
}

#[tokio::test]
async fn test_a_turn_that_gets_401_says_to_sign_in_again_in_lite_and_full_eve_does_not() {
    // OpenRouter turns the OAuth key down (a 401): eve lite's turn says to sign in again on the Mac, where the only
    // fix is; full eve, which has no such sign-in, keeps its generic failure line.
    let _iso = isolated();
    let (rig, _lent) = failing_rig(401, None, lite_env());
    assert_eq!(said(&app_turns(&rig, &["what's the weather like"]).await), vec![telegram::LITE_KEY_REJECTED_LINE]);
    assert!(telegram::LITE_KEY_REJECTED_LINE.contains("sign in with OpenRouter again") && telegram::LITE_KEY_REJECTED_LINE.contains("the Mac"));
    assert!(!telegram::LITE_KEY_REJECTED_LINE.contains("credit"));
    // control: full eve's 401 is its generic line, not the lite re-sign-in line
    let (rig, _lent) = failing_rig(401, Some(full_config()), conftest_env());
    assert_eq!(said(&app_turns(&rig, &["what's the weather like"]).await), vec![telegram::FAILED_LINE]);
}

// ---- a rundown's words are an ordinary turn in lite -------------------------------------------------------------

fn has_source_context(request: &Value) -> bool {
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i.get("role") == Some(&json!("developer")) && serde_json::to_string(i).unwrap().contains("Current source context"))
}

#[tokio::test]
async fn test_the_calendar_today_is_an_ordinary_turn_that_can_send_a_sign_in_link() {
    // User flow A.6: "what's on my calendar today?" with Gmail and the calendar not connected yet reaches the
    // connection manager, whose link goes back in the reply. Full eve takes the same words as its rundown.
    let _iso = isolated();
    let apps = FakeConnect::new().await;
    let (rig, _lent) = lite_rig(
        vec![
            call("COMPOSIO_MANAGE_CONNECTIONS", json!({"toolkits": ["googlecalendar"]}), "call_1"),
            reply("Connect your calendar here: https://connect.composio.dev/link/ln_test"),
        ],
        None,
        lite_env(),
        Some(&apps),
    );
    let seen = app_turns(&rig, &["what's on my calendar today"]).await;
    let first = rig.create.requests.lock()[0].clone();
    let tools: Vec<Obj> = first["tools"].as_array().unwrap().iter().map(|t| t.as_object().cloned().unwrap()).collect();
    assert!(telegram::offered_names(&tools).contains(&"COMPOSIO_MANAGE_CONNECTIONS".to_string()));
    assert!(!has_source_context(&first));
    assert_eq!(apps.executed(), vec![("COMPOSIO_MANAGE_CONNECTIONS".to_string(), obj(json!({"toolkits": ["googlecalendar"]})))]); // no question
    assert_eq!(said(&seen), vec!["Connect your calendar here: https://connect.composio.dev/link/ln_test"]);
}

#[tokio::test]
async fn test_full_eve_still_takes_those_words_as_its_rundown() {
    let _iso = isolated();
    let (rig, _lent) = lite_rig(vec![reply("Your day.")], Some(full_config()), conftest_env(), None);
    app_turns(&rig, &["what's on my calendar today"]).await;
    assert!(has_source_context(&rig.create.requests.lock()[0]));
}

// ---- Composio's gate in a lite turn -----------------------------------------------------------------------------

#[tokio::test]
async fn test_the_owners_own_gmail_is_read_without_a_question() {
    let _iso = isolated();
    let apps = FakeConnect::new().await;
    let read = json!({"tools": [{"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {"query": "from:sam"}}], "sync_response_to_workbench": false});
    let (rig, _lent) = lite_rig(vec![call("COMPOSIO_MULTI_EXECUTE_TOOL", read.clone(), "call_1"), reply("Two from Sam today.")], None, lite_env(), Some(&apps));
    let seen = app_turns(&rig, &["any mail from sam today"]).await;
    assert_eq!(apps.executed(), vec![("COMPOSIO_MULTI_EXECUTE_TOOL".to_string(), obj(read))]);
    assert_eq!(said(&seen), vec!["Two from Sam today."]);
}

#[tokio::test]
async fn test_a_send_and_a_reconnect_ask_first_and_run_only_on_yes() {
    let _iso = isolated();
    let send = json!({"tools": [{"tool_slug": "GMAIL_SEND_EMAIL", "arguments": {
        "recipient_email": "reader@example.org", "subject": "Lunch", "body": "Noon works."}}], "sync_response_to_workbench": false});
    let reconnect = json!({"toolkits": ["gmail"], "reinitiate_all": true});
    for (answer, runs) in [("no", false), ("yes", true)] {
        for (name, args, word) in [("COMPOSIO_MULTI_EXECUTE_TOOL", send.clone(), "GMAIL_SEND_EMAIL"), ("COMPOSIO_MANAGE_CONNECTIONS", reconnect.clone(), "reinitiate_all")] {
            let apps = FakeConnect::new().await;
            let (rig, _lent) = lite_rig(vec![call(name, args.clone(), "call_1"), reply(if runs { "Done." } else { "Okay, not done." })], None, lite_env(), Some(&apps));
            let text = if word == "GMAIL_SEND_EMAIL" { "send it" } else { "reconnect my gmail" };
            let executed = apps.executed.clone();
            app_turn_answered(&rig, text, answer, |asks| {
                assert!(asks.len() == 1 && asks[0].contains(word) && asks[0].contains("yes / no?"), "{asks:?}");
                if word == "GMAIL_SEND_EMAIL" {
                    assert!(asks[0].contains("reader@example.org") && asks[0].contains("Noon works.")); // the whole email
                }
                assert!(executed.lock().is_empty());
            })
            .await;
            let want = if runs { vec![(name.to_string(), obj(args))] } else { vec![] };
            assert_eq!(apps.executed(), want, "{answer} {word}");
        }
    }
}

#[tokio::test]
async fn test_a_long_email_is_asked_whole_with_gmails_room_and_a_long_post_elsewhere_is_not() {
    // eve lite's mail goes only through Composio: a send is shown whole with the room full eve gives the owner's own
    // Gmail (apps_mcp.WHOLE_ASK_CHARS), where the shorter limit would refuse it unasked. Any other app keeps that
    // limit (control), and full eve's question for the same call is what it was (control).
    // Rust: `_ask_user` is not patched; the question reaches the app (this is an app turn) and the owner says no.
    let _iso = isolated();
    let body = "A paragraph of the letter, as the owner dictated it. ".repeat(40); // about 2,100 characters
    assert!(eve_tool_gate::SHOW_CHARS < body.chars().count() && body.chars().count() < eve_apps_mcp::WHOLE_ASK_CHARS - 200);
    let mail = obj(json!({"tools": [{"tool_slug": "GMAIL_SEND_EMAIL", "arguments": {
        "recipient_email": "reader@example.org", "subject": "The letter", "body": body}}], "sync_response_to_workbench": false}));
    let post = obj(json!({"tools": [{"tool_slug": "SLACK_SEND_MESSAGE", "arguments": {"channel": "design", "text": body}}],
                          "sync_response_to_workbench": false}));
    let apps = FakeConnect::new().await;
    let (rig, _lent) = lite_rig(vec![], None, lite_env(), Some(&apps));
    let seen = app_seen(&rig);
    let in_app = || TurnCtx { app: Some("t-1".into()), ..TurnCtx::default() };
    let inlet = rig.inlet.clone();
    let m = mail.clone();
    let asking = tokio::spawn(in_ctx(in_app(), async move { inlet.tt_app_tool("COMPOSIO_MULTI_EXECUTE_TOOL", &m, 1).await }));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !seen.lock().iter().any(|(_, _, kind)| kind == "ask") && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let asked: Vec<String> = seen.lock().iter().filter(|(_, _, k)| k == "ask").map(|(_, l, _)| l.clone()).collect();
    assert!(asked.len() == 1 && asked[0].contains(body.trim()) && asked[0].contains("reader@example.org"), "{asked:?}");
    hear_app(&rig, "no", "t-answer");
    let refused = tokio::time::timeout(Duration::from_secs(5), asking).await.unwrap().unwrap();
    assert!(refused["ok"] == json!(false) && refused["reason"].as_str().unwrap().contains("said no") && apps.executed().is_empty(), "{refused:?}");
    let too_long = in_ctx(in_app(), rig.inlet.tt_app_tool("COMPOSIO_MULTI_EXECUTE_TOOL", &post, 1)).await;
    assert_eq!(Value::Object(too_long), eve_tool_gate::TOO_LONG.clone());
    assert_eq!(seen.lock().iter().filter(|(_, _, k)| k == "ask").count(), 1);
    assert!(rig.inlet.tt_composio_write_question("COMPOSIO_MULTI_EXECUTE_TOOL", &mail).is_none());
}
