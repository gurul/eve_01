//! test_lite_brain.py, the turn-driven tests: eve lite's brain as the model reads it in a real turn.
//!
//! A lite turn offers exactly eve lite's tools, whatever else is lent; every instruction it is sent names only tools
//! that turn offers and no app (the owner's rule for prompts); a push-to-talk turn reads the web app's call note; a
//! call turn reads an earlier card in lite words and its taint still carries; full eve's controls are unchanged; and
//! an Office document is read in lite, not handed to a Mac task. No network.
//!
//! The turns run on eve-brain's own test rig (its tests/support and tests/more, the fakes test_telegram.py shares),
//! included here by path so both crates drive the same fakes. `lite_rig` is test_telegram_lite.py's: a TelegramInlet
//! lent what lite.rs lends (memory, chat log, watcher, Connect, no agent, no launcher, no screen, no Claude sessions,
//! no Codex folders). Python's `lite` fixture puts LITE_ENV (less the key) in `os.environ`; here it is the inlet's lent
//! environ. FakeConnect is the real ComposioBridge over a session serving Connect's meta tools, the shape eve lite's
//! daemon lends the brain (`ConnectApps::brain`).
//!
//! Two of Python's reads of private state are made through what they cause instead: `rig.inlet._taint ==
//! tool_gate.CARRIED` is read off the chat log, whose line for the call turn's reply is marked `outside` exactly when
//! the turn carries a taint; and `rig.inlet._maker = SimpleNamespace(tools=...)` is a real ChatMaker whose Mini App is
//! up, so it offers every maker tool rather than one.

#[path = "../../eve-brain/tests/support/mod.rs"]
mod support;
#[path = "../../eve-brain/tests/more/mod.rs"]
mod more;
mod names;

use std::collections::{BTreeSet, HashMap};
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_brain::prelude::{BrainMemory, Sessions};
use eve_brain::telegram::{self as telegram, TelegramConfig};
use eve_composio_tools::{ComposioBridge, ComposioClient, ComposioConfig, ComposioError, ComposioSession};
use more::*;
use names::{problems, COMPOSIO_NAMES};
use support::*;

type Environ = HashMap<String, String>;

const PLACEHOLDER: &str = "placeholder-not-a-key";

/// test_telegram_lite.py's LITE_ENV.
const LITE_ENV: &[(&str, &str)] = &[
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
];

/// `configured(LITE_ENV)`'s mapping: LITE_ENV whole.
fn lite_config_env() -> Environ {
    LITE_ENV.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// The `lite` fixture: conftest's pins with LITE_ENV over them, the key left out (never in os.environ), and `extra`.
fn lite(extra: &[(&str, &str)]) -> Environ {
    let mut env = conftest_env();
    for (k, v) in LITE_ENV {
        if *k != "OPENROUTER_API_KEY" {
            env.insert(k.to_string(), v.to_string());
        }
    }
    for (k, v) in extra {
        env.insert(k.to_string(), v.to_string());
    }
    env
}

/// The `full_profile` fixture: full eve (EVE_PROFILE unset) with lite's routers off, so a control turn is shaped as
/// the lite one is.
fn full_profile() -> Environ {
    let mut env = conftest_env();
    for (k, v) in LITE_ENV {
        if !["OPENROUTER_API_KEY", "EVE_PROFILE"].contains(k) {
            env.insert(k.to_string(), v.to_string());
        }
    }
    env.remove("EVE_PROFILE");
    env
}

/// eve lite's own tools in a chat turn (profile.LITE_TOOL_NAMES less the voice's recent_conversation), with Connect's.
fn lite_chat_tools() -> BTreeSet<String> {
    eve_profile::LITE_TOOL_NAMES
        .iter()
        .filter(|n| **n != "recent_conversation")
        .chain(COMPOSIO_NAMES)
        .map(|n| n.to_string())
        .collect()
}

/// telegram.py's `_TEXT_STYLE_LITE`: INSTRUCTIONS_LITE from "Right now they are writing" to the blank line before
/// "Everything you can do".
fn text_style_lite() -> &'static str {
    let text = telegram::INSTRUCTIONS_LITE;
    let start = text.find("Right now they are writing").expect("the style's first words");
    let end = text.find("\n\nEverything you can do").expect("the style's end");
    &text[start..end]
}

/// telegram.py's `_RECALL_NOTES_LITE`.
const RECALL_NOTES_LITE: &str = "A note in brackets in the history, like (2 hours later), marks a quiet spell.";

// ---- FakeConnect ---------------------------------------------------------------------------------------------------

/// `connect_tools(names)`: Composio Connect's meta tools as Responses function tools, from spike S1's tool objects.
fn connect_tools(names: &[&str]) -> Vec<Value> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bridge/tests/fixtures/composio_connect");
    names
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

struct ConnectSession {
    names: Vec<&'static str>,
}

#[async_trait]
impl ComposioSession for ConnectSession {
    fn session_id(&self) -> String {
        "connect".into()
    }
    async fn tools(&self) -> Result<Vec<Value>, ComposioError> {
        Ok(connect_tools(&self.names))
    }
    async fn execute(&self, slug: &str, _arguments: &Map<String, Value>) -> Result<Value, ComposioError> {
        if slug == eve_composio_tools::MANAGE_CONNECTIONS {
            return Ok(json!({"successful": true, "data": {"results": {"gmail": {
                "status": "initiated", "redirect_url": "https://connect.composio.dev/link/ln_test"}}}}));
        }
        Ok(json!({"successful": true, "data": {"results": [{"successful": true}]}}))
    }
}

struct ConnectClient {
    names: Vec<&'static str>,
}

#[async_trait]
impl ComposioClient for ConnectClient {
    async fn create_session(&self, _user: &str, _toolkits: &Value, _multi: Option<&Value>) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        Ok(Arc::new(ConnectSession { names: self.names.clone() }))
    }
    async fn use_session(&self, _session_id: &str) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        Ok(Arc::new(ConnectSession { names: self.names.clone() }))
    }
    async fn list_connected_accounts(&self, _user_ids: &[String]) -> Result<Vec<Value>, ComposioError> {
        Ok(Vec::new())
    }
    async fn execute_tool(&self, slug: &str, _arguments: &Map<String, Value>, _user: &str, _account: &str) -> Result<Value, ComposioError> {
        Err(ComposioError::new("RuntimeError", format!("{slug} runs through Composio Connect's own tools")))
    }
}

/// `FakeConnect(names, started)`: the bridge the brain reads, serving `names`; not started when `started` is false.
struct FakeConnect {
    bridge: Arc<ComposioBridge>,
    _dir: tempfile::TempDir,
}

impl FakeConnect {
    async fn new(names: &[&'static str], started: bool) -> Self {
        let dir = tmp();
        let mut config = ComposioConfig::new(true, "eve-lite");
        config.state_path = dir.path().join("composio.json");
        config.toolkits = None;
        let names = names.to_vec();
        let bridge = ComposioBridge::new(
            config,
            Some(Arc::new(move || Ok(Arc::new(ConnectClient { names: names.clone() }) as Arc<dyn ComposioClient>))),
        );
        if started {
            bridge.start().await.unwrap();
        }
        Self { bridge: Arc::new(bridge), _dir: dir }
    }

    async fn all() -> Self {
        Self::new(COMPOSIO_NAMES, true).await
    }
}

// ---- the lite rig ------------------------------------------------------------------------------------------------

/// What lite_rig lent that a test reads back.
struct Lent {
    chat_log: Option<Arc<eve_chat_log::ChatLog>>,
    dir: tempfile::TempDir,
}

/// What `lite_rig` lends, as its keywords (`memory=`, `chat=`, `watcher=`, `apps=`, `vault=`, `config=`).
struct RigOptions<'a> {
    config: Option<TelegramConfig>,
    memory: bool,
    chat: bool,
    watcher: bool,
    apps: Option<&'a FakeConnect>,
    vault: bool,
    /// what web_search answers (TOOL_HANDLERS["web_search"] as a Spy)
    search: Value,
}

impl Default for RigOptions<'_> {
    fn default() -> Self {
        RigOptions {
            config: None,
            memory: true,
            chat: true,
            watcher: true,
            apps: None,
            vault: false,
            search: json!({"ok": true, "answer": "Sunny."}),
        }
    }
}

fn watcher_at(path: &Path, ledger: &Path) -> Arc<eve_web::watch::Watcher> {
    let ctx = eve_web::Ctx::global().clone().with_ledger(eve_spend::Ledger::at(ledger));
    let mut deps = eve_web::watch::WatcherDeps::new(ctx);
    deps.offload = false;
    let config = eve_web::watch::WatchConfig { enabled: true, path: path.to_path_buf(), ..Default::default() };
    Arc::new(eve_web::watch::Watcher::new(config, deps))
}

/// `lite_rig(tmp_path, *responses, **kw)` with `environ` as the process's environment.
fn lite_rig(responses: Vec<Value>, environ: Environ, o: RigOptions<'_>) -> (Rig, Lent) {
    let dir = tmp();
    let memory = o.memory.then(|| FakeMemory::new(dir.path(), "Name: Sam. Likes green tea."));
    // The log reads the rig's wall clock: support's NOW is a past date (Python's is 2027), and a log on the real clock
    // would prune every line the rig dates as more than MAX_DAYS old.
    let chat_log = o.chat.then(|| {
        let options = eve_chat_log::Options { wall: Some(Arc::new(|| NOW)), ..Default::default() };
        Arc::new(eve_chat_log::ChatLog::with_options(dir.path().join("chat").join("log.json"), options))
    });
    let watcher = o.watcher.then(|| watcher_at(&dir.path().join("watches.json"), &dir.path().join("ledger")));
    let vault = o.vault.then(|| Arc::new(eve_second_brain::VaultConfig { enabled: true, root: dir.path().join("vault") }));
    let bridge = o.apps.map(|a| a.bridge.clone());
    let cfg = o.config.unwrap_or_else(|| telegram::configured(Some(&lite_config_env())));
    let log = chat_log.clone();
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(responses), cfg, move |opts| {
        opts.agent_factory = None;
        opts.agent_enabled = false;
        opts.launcher = Some(Arc::new(|_folder: std::path::PathBuf, _harness: String| async { Ok("refused".to_string()) }.boxed()));
        opts.launch_recent = Some(Arc::new(Vec::new));
        opts.claude_sessions = Some(Arc::new(|| Ok(Sessions::Now(Vec::new()))));
        opts.codex_folders = Some(Arc::new(Vec::new));
        opts.screen = Some(Arc::new(|| None));
        opts.thinker = Some(Arc::new(|_q: String, _c: String| async { Ok(obj(json!({"ok": true, "answer": "42"}))) }.boxed()));
        opts.memory = memory.map(|m| m as Arc<dyn BrainMemory>);
        opts.chat_log = log;
        opts.watcher = watcher;
        opts.apps = bridge;
        opts.vault = vault;
        opts.environ = Some(environ);
    });
    let answer = o.search;
    rig.inlet.tt_set_web_search(Some(Arc::new(move |_query: &str, _config: &eve_web::websearch::SearchConfig| answer.clone())));
    (rig, Lent { chat_log, dir })
}

/// `drain(rig)`: every job the inlet started, and the ones those started, run to their end.
async fn drain(rig: &Rig) {
    for _ in 0..20 {
        settle(50).await;
        rig.inlet.say_relay_probe().jobs().await;
    }
}

/// `app_turns(rig, *texts)`: each text as an app turn of its own, one after another; what the app was handed.
async fn app_turns(rig: &Rig, texts: &[&str]) -> Vec<(String, String, String)> {
    let seen: Arc<Mutex<Vec<(String, String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    *rig.inlet.app_out.lock() = Some(Arc::new(move |turn: &str, line: &str, kind: &str| {
        s.lock().push((turn.to_string(), line.to_string(), kind.to_string()));
        true
    }));
    for (i, text) in texts.iter().enumerate() {
        rig.inlet.hear(text, eve_phone_call::Captured::Uncaptured, false, None, Some(&format!("t-{}", i + 1)), "auto", &[]);
        drain(rig).await;
    }
    let out = seen.lock().clone();
    out
}

async fn first_request(rig: &Rig) -> Value {
    app_turns(rig, &["hello there, what can you do"]).await;
    let requests = rig.create.requests.lock();
    requests.first().cloned().expect("the turn asked the model")
}

fn offered(request: &Value) -> Vec<String> {
    let tools: Vec<Obj> = request["tools"].as_array().unwrap().iter().filter_map(|t| t.as_object().cloned()).collect();
    telegram::offered_names(&tools)
}

/// The request's items of one role, each as its whole text (Python's `str(item)`).
fn items_of(request: &Value, role: &str) -> Vec<String> {
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i.get("role").and_then(Value::as_str) == Some(role))
        .map(|i| serde_json::to_string(i).unwrap())
        .collect()
}

fn instructions(request: &Value) -> String {
    request["instructions"].as_str().unwrap().to_string()
}

/// A call's reader that keeps what it reads out.
fn reader() -> (eve_phone_call::Reader, Arc<Mutex<Vec<String>>>) {
    let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let h = heard.clone();
    (eve_phone_call::Reader::new(move |text: &str| h.lock().push(text.to_string())), heard)
}

/// `rig.inlet.hear(text, call=heard.append)`: the owner's words from a push-to-talk call.
async fn hear_on_call(rig: &Rig, text: &str, call: &eve_phone_call::Reader) {
    rig.inlet.hear(text, eve_phone_call::Captured::Uncaptured, true, Some(call), None, "auto", &[]);
    drain(rig).await;
}

// ---- the offered set ------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_lite_turn_offers_exactly_eve_lites_tools() {
    let _iso = isolated();
    let apps = FakeConnect::all().await;
    let (rig, _lent) = lite_rig(vec![reply("Hi!")], lite(&[("EVE_SCIENCE_SEARCH", "1")]), RigOptions { apps: Some(&apps), ..Default::default() });
    let names = offered(&first_request(&rig).await);
    assert_eq!(names.len(), names.iter().collect::<BTreeSet<_>>().len(), "{names:?}");
    assert_eq!(names.iter().cloned().collect::<BTreeSet<_>>(), lite_chat_tools());
    // in the order full eve lends them: its own, memory, the chat log, the web, the watcher's slot, the apps
    assert_eq!(names[..2], ["remember", "think_hard"]);
    assert_eq!(names[names.len() - COMPOSIO_NAMES.len()..], *COMPOSIO_NAMES);
}

/// A Mini App that is up: what a ChatMaker needs to offer its tools.
struct UpMiniApp {
    maker: Arc<eve_brain::apps_maker::AppMaker>,
    store: Arc<eve_brain::apps_maker::AppStore>,
}

#[async_trait]
impl eve_brain::seams::MakerHost for UpMiniApp {
    fn url(&self) -> String {
        "https://quiet-owl.trycloudflare.com".into()
    }
    fn maker(&self) -> Arc<eve_brain::apps_maker::AppMaker> {
        self.maker.clone()
    }
    fn store(&self) -> Arc<eve_brain::apps_maker::AppStore> {
        self.store.clone()
    }
    fn app_url(&self, slug: &str) -> String {
        format!("{}/apps/{slug}/", self.url())
    }
    fn chat_url(&self, slug: &str) -> String {
        format!("{}/?chat={slug}", self.url())
    }
    async fn stick_link(&self, _rotate: bool) -> String {
        String::new()
    }
}

fn a_live_maker(root: &Path) -> Arc<eve_brain::apps_maker::ChatMaker> {
    let store = Arc::new(eve_brain::apps_maker::AppStore::new(&root.join("apps")));
    let maker = eve_brain::apps_maker::AppMaker::new(
        store.clone(),
        Arc::new(|_messages: Vec<Value>, _progress| async { Err(anyhow::anyhow!("no app is made in these tests")) }.boxed()),
        Arc::new(|_usage: &Value| 0.0),
        Arc::new(|usd: f64| usd),
        Some(Arc::new(String::new)),
        None,
        0,
    );
    let spawn: eve_brain::seams::SpawnFn = Arc::new(|job, _name: &str| {
        tokio::spawn(job);
    });
    eve_brain::apps_maker::ChatMaker::new(Arc::new(UpMiniApp { maker, store }), spawn)
}

#[tokio::test]
async fn whatever_else_is_lent_a_lite_turn_offers_only_eve_lites_tools() {
    // The robot's switch on, the menu's tools on, a vault lent, the science switch on, an app maker's tools: the
    // filter (profile.filter_tools) leaves eve lite's, and the instructions still name none of the others.
    let _iso = isolated();
    let apps = FakeConnect::all().await;
    let env = lite(&[("EVE_ROBOT", "1"), ("EVE_SCIENCE_SEARCH", "1")]);
    let (rig, lent) = lite_rig(vec![reply("Hi!")], env.clone(), RigOptions { apps: Some(&apps), vault: true, ..Default::default() });
    let maker = a_live_maker(lent.dir.path());
    assert!(!maker.tools().is_empty()); // the maker would offer its tools
    *rig.inlet.maker.lock() = Some(maker);
    let request = first_request(&rig).await;
    let names = offered(&request);
    assert_eq!(names.iter().cloned().collect::<BTreeSet<_>>(), lite_chat_tools());
    assert_eq!(problems(&instructions(&request), &names), Vec::<String>::new());
    // control: full eve lends them all
    let mut full = env;
    full.remove("EVE_PROFILE");
    let all = telegram::offered_names(&telegram::own_tools_in(Some(&full)));
    for name in ["start_task", "screenshot", "take_photo", "look", "move_head"] {
        assert!(all.iter().any(|n| n == name), "{name}");
    }
}

// ---- every instruction names only offered tools and no app ---------------------------------------------------

async fn every_lite_app_turns_instructions_name_only_what_it_offers(shape: &str) {
    let _iso = isolated();
    let apps = match shape {
        "no_apps" => None,
        "apps_not_started" => Some(FakeConnect::new(COMPOSIO_NAMES, false).await),
        "search_and_execute_only" => Some(FakeConnect::new(&["COMPOSIO_SEARCH_TOOLS", "COMPOSIO_MULTI_EXECUTE_TOOL"], true).await),
        _ => Some(FakeConnect::all().await),
    };
    let options = RigOptions {
        apps: apps.as_ref(),
        memory: shape != "no_memory_no_chat_log",
        chat: shape != "no_memory_no_chat_log",
        watcher: shape != "no_watcher",
        ..Default::default()
    };
    let (rig, _lent) = lite_rig(vec![reply("Hi!")], lite(&[("EVE_SCIENCE_SEARCH", "1")]), options);
    let request = first_request(&rig).await;
    let names = offered(&request);
    let text = instructions(&request);
    assert_eq!(problems(&text, &names), Vec::<String>::new(), "{shape}");
    assert!(text.contains("eve's web app") && text.contains(telegram::APP_STYLE_LITE), "{shape}");
    // no task, no agent, in lite
    assert!(!text.contains("computer agent") && !text.contains("the Mac's screen"), "{shape}");
    if shape == "everything" {
        assert!(text.contains(RECALL_NOTES_LITE) && text.contains("recent_messages"));
    }
    if shape == "search_and_execute_only" {
        assert!(!text.contains("COMPOSIO_MANAGE_CONNECTIONS") && text.contains("COMPOSIO_SEARCH_TOOLS"));
    }
    if shape == "no_apps" || shape == "apps_not_started" {
        assert!(!text.replace("through the COMPOSIO tools.", "").contains("COMPOSIO"), "{shape}");
    }
    // the turn's own note (the clock, the brief) names nothing either
    for note in items_of(&request, "developer") {
        assert_eq!(problems(&note, &names), Vec::<String>::new(), "{shape}: {note}");
    }
}

#[tokio::test]
async fn every_lite_app_turns_instructions_name_only_what_it_offers_everything() {
    every_lite_app_turns_instructions_name_only_what_it_offers("everything").await;
}

#[tokio::test]
async fn every_lite_app_turns_instructions_name_only_what_it_offers_no_apps() {
    every_lite_app_turns_instructions_name_only_what_it_offers("no_apps").await;
}

#[tokio::test]
async fn every_lite_app_turns_instructions_name_only_what_it_offers_apps_not_started() {
    every_lite_app_turns_instructions_name_only_what_it_offers("apps_not_started").await;
}

#[tokio::test]
async fn every_lite_app_turns_instructions_name_only_what_it_offers_search_and_execute_only() {
    every_lite_app_turns_instructions_name_only_what_it_offers("search_and_execute_only").await;
}

#[tokio::test]
async fn every_lite_app_turns_instructions_name_only_what_it_offers_no_memory_no_chat_log() {
    every_lite_app_turns_instructions_name_only_what_it_offers("no_memory_no_chat_log").await;
}

#[tokio::test]
async fn every_lite_app_turns_instructions_name_only_what_it_offers_no_watcher() {
    every_lite_app_turns_instructions_name_only_what_it_offers("no_watcher").await;
}

#[tokio::test]
async fn a_push_to_talk_turn_reads_the_web_apps_call_note() {
    let _iso = isolated();
    let apps = FakeConnect::all().await;
    let (rig, lent) = lite_rig(vec![reply("It's sunny.")], lite(&[]), RigOptions { apps: Some(&apps), ..Default::default() });
    let (call, heard) = reader();
    hear_on_call(&rig, "what's the weather like outside", &call).await;
    let request = rig.create.requests.lock()[0].clone();
    let note = items_of(&request, "developer").join(" ");
    assert!(note.contains(&serde_json::to_string(telegram::CALL_NOTE_LITE).unwrap().trim_matches('"').to_string()), "{note}");
    assert!(!note.contains(&serde_json::to_string(telegram::CALL_NOTE).unwrap().trim_matches('"').to_string()));
    // a call turn keeps the texting style
    assert!(instructions(&request).contains(text_style_lite()));
    assert_eq!(problems(&instructions(&request), &offered(&request)), Vec::<String>::new());
    assert_eq!(problems(telegram::CALL_NOTE_LITE, &[] as &[&str]), Vec::<String>::new());
    assert!(telegram::CALL_NOTE_LITE.contains("shown in the chat"));
    assert_eq!(*heard.lock(), vec!["It's sunny."]);
    // the taint reading's negative control (the card tests read it): a reply after no outside words is not marked
    assert!(!written_from_outside_words(&lent, rig.inlet.tt_chat_id().expect("lite's one chat"), "It's sunny."));
    // the call's warm-up reads it too
    rig.create.push(Scripted::Body(reply("ok")));
    rig.inlet.warm().await;
    let last = rig.create.requests.lock().last().cloned().unwrap();
    let warm = items_of(&last, "developer").join(" ");
    assert!(warm.contains(&serde_json::to_string(telegram::CALL_NOTE_LITE).unwrap().trim_matches('"').to_string()), "{warm}");
    assert!(!warm.contains(&serde_json::to_string(telegram::CALL_NOTE).unwrap().trim_matches('"').to_string()));
}

const PROGRAM: &str = "Here's how they compare.\n```openui-lang\nroot = Card([table])\n\
                       table = Table([Col(\"Drink\", [\"Tea\", \"Coffee\"]), Col(\"Caffeine (mg)\", [47, 95], \"number\")])\n```";
const CALL_ANSWER: &str = "Tea, it has half the caffeine.";

const CHILD: &str = "EVE_LITE_TURN_CHILD";

/// Whether this is the child that runs `test`'s body in a process whose environment is the `lite` fixture's. In the
/// parent it runs the child (this test binary, `--exact test`) and asserts it ran the body and passed.
fn in_a_lite_process(test: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", test, "--nocapture", "--test-threads=1"]).env(CHILD, "1");
    for (k, v) in lite(&[]) {
        child.env(k, v);
    }
    child.env_remove("OPENROUTER_API_KEY");
    let out = child.output().expect("the test binary runs");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{test} failed in eve lite's process:\n{text}");
    assert!(text.contains("1 passed"), "{test} did not run in eve lite's process:\n{text}");
    false
}

/// An app turn whose reply is a card written after a web search (so it is kept as outside words), then a call turn:
/// what the call heard, and the call turn's request.
async fn card_then_call(rig: &Rig) -> (Vec<String>, Value) {
    app_turns(rig, &["tea or coffee, which has more caffeine"]).await;
    let (call, heard) = reader();
    hear_on_call(rig, "so which one should I drink tonight", &call).await;
    let request = rig.create.requests.lock().last().cloned().unwrap();
    let heard = heard.lock().clone();
    (heard, request)
}

fn card_rig(environ: Environ, config: Option<TelegramConfig>) -> (Rig, Lent) {
    let responses = vec![call("web_search", json!({"query": "caffeine tea vs coffee"}), "call_1"), reply(PROGRAM), reply(CALL_ANSWER)];
    // no transcript: the marks alone carry
    let options = RigOptions { config, memory: false, search: json!({"ok": true, "answer": "Coffee: 95 mg; tea: 47."}), ..Default::default() };
    lite_rig(responses, environ, options)
}

/// `rig.inlet._taint == tool_gate.CARRIED`, read off what it causes: the call turn's reply goes into the chat log
/// marked as written from words eve did not write. The card itself (after the web search) is the positive control.
fn written_from_outside_words(lent: &Lent, chat: i64, text: &str) -> bool {
    let log = lent.chat_log.as_ref().expect("a chat log");
    let lines = log.lines(chat);
    let line = lines.iter().rev().find(|l| l.who == "eve" && l.text == text).unwrap_or_else(|| panic!("{text:?} is not in the chat log"));
    line.outside
}

#[tokio::test]
async fn a_call_turn_reads_an_earlier_card_in_lite_words_and_its_taint_still_carries() {
    // A call cannot draw a card, so its history reads the card as a placeholder (link_chat.history_text). eve lite's
    // placeholder points at its chat, not the phone app; and the card, written after a web search, still taints the
    // call turn that shows it (the taint marks are made from the same words).
    // The history's placeholder is read off the process's profile (telegram's `_lite()`, os.environ), as Python's
    // `lite` fixture sets it: the body runs in a child process of this test binary whose environment is eve lite's.
    if !in_a_lite_process("a_call_turn_reads_an_earlier_card_in_lite_words_and_its_taint_still_carries") {
        return;
    }
    let _iso = isolated();
    let (rig, lent) = card_rig(lite(&[]), None);
    let (heard, request) = card_then_call(&rig).await;
    let history = items_of(&request, "assistant").join(" ");
    assert!(history.contains(telegram::OPENUI_PLACEHOLDER_LITE), "{history}");
    assert!(!history.contains("Eve Link") && !history.contains("openui-lang"), "{history}");
    // the card's outside words carried over
    let chat = rig.inlet.tt_chat_id().expect("lite's one chat");
    assert!(written_from_outside_words(&lent, chat, PROGRAM));
    assert!(written_from_outside_words(&lent, chat, CALL_ANSWER));
    assert_eq!(heard, vec![CALL_ANSWER]);
}

/// `TelegramConfig(enabled=True, app_only=True, owner_ids=frozenset({4242}))`.
fn full_app_config() -> TelegramConfig {
    TelegramConfig { enabled: true, app_only: true, owner_ids: vec![4242], ..TelegramConfig::default() }
}

#[tokio::test]
async fn full_eves_call_turn_reads_the_card_as_it_always_did() {
    // Control: with EVE_PROFILE unset the placeholder is link_chat's own, and the taint carries as before.
    let _iso = isolated();
    let (rig, lent) = card_rig(full_profile(), Some(full_app_config()));
    let (_heard, request) = card_then_call(&rig).await;
    let history = items_of(&request, "assistant").join(" ");
    assert!(history.contains(eve_link::link_chat::OPENUI_PLACEHOLDER), "{history}");
    assert!(!history.contains(telegram::OPENUI_PLACEHOLDER_LITE));
    assert!(written_from_outside_words(&lent, 4242, CALL_ANSWER));
}

#[test]
fn full_eves_call_note_and_style_are_its_own() {
    // Control: with EVE_PROFILE unset a call turn reads CALL_NOTE and an app turn APP_STYLE (the golden holds the
    // bytes).
    let mut env = conftest_env();
    env.remove("EVE_PROFILE");
    let items = [telegram::message_item("user", "hi")];
    let block = eve_link::link_chat::app_block(None);
    let body = telegram::request_in(Some(&env), &full_app_config(), &items, "", "", &[], false, "", &[], None, &[], "", &[], "", &block, "", &[]);
    let text = body["instructions"].as_str().unwrap();
    assert!(text.contains(telegram::APP_STYLE) && !text.contains(telegram::INSTRUCTIONS_LITE));
    assert!(text.contains("Eve Link app"));
}

// ---- an Office document is read in lite, not handed to a Mac task --------------------------------------------

/// A smallest real .docx (a zip with word/document.xml): what the app uploads for a Word file.
fn docx(paragraphs: &[&str]) -> Vec<u8> {
    let body: String =
        paragraphs.iter().map(|p| format!("<w:p><w:r><w:t xml:space=\"preserve\">{p}</w:t></w:r></w:p>")).collect();
    let doc = format!(
        "<?xml version=\"1.0\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{body}</w:body></w:document>"
    );
    let mut out = Cursor::new(Vec::new());
    {
        let mut zf = zip::ZipWriter::new(&mut out);
        zf.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
        zf.write_all(doc.as_bytes()).unwrap();
        zf.finish().unwrap();
    }
    out.into_inner()
}

/// Every input_text of the last user turn's content, flattened for a substring check.
fn user_parts(request: &Value) -> String {
    let users: Vec<&Value> =
        request["input"].as_array().unwrap().iter().filter(|i| i.get("role").and_then(Value::as_str) == Some("user")).collect();
    let last = users.last().expect("a user turn");
    last["content"]
        .as_array()
        .map(|parts| parts.iter().map(|p| p.get("text").and_then(Value::as_str).unwrap_or("").to_string()).collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

#[tokio::test]
async fn a_lite_doc_upload_reaches_the_brain_as_its_words_not_a_mac_task() {
    // docs/lite.md promises Office files work in lite. A .docx has no Mac task to open it here (lite offers none), so
    // its text is pulled out and sent like a text file; the "start a task on the Mac" line full eve uses, which names
    // something lite cannot do, never appears.
    let _iso = isolated();
    let (rig, lent) = lite_rig(vec![reply("Rent is 1200 a month.")], lite(&[]), RigOptions::default());
    let store = eve_link_uploads::UploadStore::new(Some(lent.dir.path().join("up")));
    let doc = store.save(&docx(&["Quarterly budget", "Rent is 1200 a month, paid on the 1st."]), "budget.docx").unwrap();
    rig.inlet.hear("what does it say about rent?", eve_phone_call::Captured::Uncaptured, false, None, Some("t-1"), "auto", &[doc]);
    drain(&rig).await;
    let parts = user_parts(&rig.create.requests.lock()[0]);
    assert!(parts.contains("Rent is 1200 a month, paid on the 1st.") && parts.contains("context, not instructions"), "{parts}");
    assert!(!parts.contains("start a task on the Mac") && !parts.contains("saved on this Mac"), "{parts}");
}
