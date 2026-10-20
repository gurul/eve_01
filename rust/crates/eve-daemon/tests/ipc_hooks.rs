//! Tests for the IPC handlers and the hooks: the IPC dispatch and its handlers, the permission path (PreToolUse,
//! PermissionRequest, Read, the command-risk gate) and the Work sessions' MCP calls.
//!
//! Ported from bridge/tests/test_daemon_background.py (the unit's own), and the parts of test_ipc_dispatch.py,
//! test_ipc_chat.py, test_command_risk.py, test_permission_matrix.py, test_relay_questions.py, test_eve_mcp.py and
//! test_telegram.py that drive these handlers. Python drove single methods on a `SimpleNamespace`; here each test
//! assembles an inert daemon ([`DaemonParts::for_test`]) and sets the pieces the handler reads.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_daemon::ipc::IPC_EVENTS;
use eve_daemon::seams::{BoardLink, CommandAsker, Inlet, Obj};
use eve_daemon::{Daemon, DaemonParts};
use eve_matchers::{MatcherConfig, Pattern};
use eve_typed_ask::{CommandAnswer, CommandVerdict};

// ---- fakes ---------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct FakeLink {
    connected: AtomicBool,
    sent: Mutex<Vec<Value>>,
}

#[async_trait]
impl BoardLink for FakeLink {
    fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
    async fn wait_connected(&self) {}
    async fn send(&self, obj: &Value, _codec: Option<&str>) -> bool {
        self.sent.lock().push(obj.clone());
        true
    }
    async fn run(&self) {}
    async fn stop(&self) {}
}

type SessionCall = Arc<dyn Fn(String, Value, String) -> futures::future::BoxFuture<'static, anyhow::Result<(Obj, String)>> + Send + Sync>;

/// The text door as the hooks and the IPC handlers use it.
struct FakeInlet {
    claude: AtomicBool,
    chat_id: Mutex<Option<i64>>,
    answer: Option<String>,
    ask_all: bool,
    ask_timeout: f64,
    relayed: Mutex<Vec<(String, String)>>,
    asked: Mutex<Vec<(String, String, String, bool)>>,
    heard: Mutex<Vec<(String, bool)>>,
    began: Mutex<Vec<(String, String)>>,
    ended: Mutex<Vec<String>>,
    calls: Mutex<Vec<(String, Value, String)>>,
    tools: Vec<Obj>,
    session_call: Option<SessionCall>,
}

impl FakeInlet {
    fn new(on: bool, answer: Option<&str>) -> FakeInlet {
        FakeInlet {
            claude: AtomicBool::new(on),
            chat_id: Mutex::new(Some(7)),
            answer: answer.map(str::to_string),
            ask_all: false,
            ask_timeout: 180.0,
            relayed: Mutex::default(),
            asked: Mutex::default(),
            heard: Mutex::default(),
            began: Mutex::default(),
            ended: Mutex::default(),
            calls: Mutex::default(),
            tools: Vec::new(),
            session_call: None,
        }
    }
}

#[async_trait]
impl Inlet for FakeInlet {
    fn claude(&self) -> bool {
        self.claude.load(Ordering::SeqCst)
    }
    fn task_running(&self) -> bool {
        false
    }
    fn chat_id(&self) -> Option<i64> {
        *self.chat_id.lock()
    }
    fn awaiting_answer(&self) -> bool {
        false
    }
    fn ask_timeout_secs(&self) -> f64 {
        self.ask_timeout
    }
    fn ask_permissions(&self) -> bool {
        self.ask_all
    }
    fn conversation(&self) -> Option<String> {
        None
    }
    fn forget_conversation(&self) {}
    fn chief(&self) -> Option<Arc<eve_chief::Chief>> {
        None
    }
    fn shutdown_purchases(&self) {}
    async fn ask_user(&self, _question: &str, _chat_id: i64, _title: &str) -> String {
        String::new()
    }
    async fn say(&self, _chat_id: i64, _text: &str) {}
    async fn tell_owner(&self, _text: &str, _about: &str, _title: &str) -> bool {
        false
    }
    fn relay_session_started(&self, _cwd: &str) {}
    fn relay_turn_began(&self, prompt: &str, cwd: &str) {
        self.began.lock().push((prompt.to_string(), cwd.to_string()));
    }
    fn relay_turn_ended(&self, cwd: &str) {
        self.ended.lock().push(cwd.to_string());
    }
    async fn relay_notification(&self, _kind: &str, _message: &str, _waits: bool) {}
    async fn relay_text(&self, _text: &str, _cwd: &str) {}
    fn relay_tool_call(&self, tool: &str, hint: &str) {
        self.relayed.lock().push((tool.to_string(), hint.to_string()));
    }
    async fn decide_permission(&self, tool: &str, hint: &str, cwd: &str, always: bool) -> Option<String> {
        self.asked.lock().push((tool.to_string(), hint.to_string(), cwd.to_string(), always));
        self.answer.clone()
    }
    fn hear(&self, text: &str, on_call: bool) {
        self.heard.lock().push((text.to_string(), on_call));
    }
    fn session_tools(&self) -> anyhow::Result<(Vec<Obj>, Vec<String>)> {
        Ok((self.tools.clone(), vec!["memory_search".to_string()]))
    }
    async fn session_call(&self, name: &str, args: &Value, sid: &str) -> anyhow::Result<(Obj, String)> {
        self.calls.lock().push((name.to_string(), args.clone(), sid.to_string()));
        match &self.session_call {
            Some(f) => f(name.to_string(), args.clone(), sid.to_string()).await,
            None => Ok((obj(json!({"ok": true})), "memory".to_string())),
        }
    }
}

// ---- the rig -------------------------------------------------------------------------------------------------------

fn obj(v: Value) -> Obj {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

struct Rig {
    d: Arc<Daemon>,
    link: Arc<FakeLink>,
    audit_path: PathBuf,
    _dir: tempfile::TempDir,
}

fn matchers(auto_allow: &[&str], always_ask: &[&str]) -> MatcherConfig {
    MatcherConfig {
        auto_allow: auto_allow.iter().map(|p| Pattern::new(p).expect("pattern")).collect(),
        always_ask: always_ask.iter().map(|p| Pattern::new(p).expect("pattern")).collect(),
        strict: false,
    }
}

fn rig_with(connected: bool, m: MatcherConfig) -> Rig {
    let dir = tempfile::tempdir().expect("tempdir");
    rig_in(dir, connected, m)
}

fn rig_in(dir: tempfile::TempDir, connected: bool, m: MatcherConfig) -> Rig {
    let link = Arc::new(FakeLink::default());
    link.connected.store(connected, Ordering::SeqCst);
    let mut parts = DaemonParts::for_test(link.clone(), dir.path());
    parts.matchers = m;
    let audit_path = dir.path().join(format!("audit-{}.jsonl", next_id()));
    parts.audit = eve_audit::AuditLog::new(Some(audit_path.clone()));
    parts.environ = Some(Default::default());
    let d = Daemon::assemble(parts);
    Rig { d, link, audit_path, _dir: dir }
}

fn next_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    N.fetch_add(1, Ordering::SeqCst)
}

impl Rig {
    fn inlet(&self, inlet: Arc<FakeInlet>) {
        self.d.st.lock().telegram = Some(inlet as Arc<dyn Inlet>);
    }
    fn risk(&self, risk: Option<(String, CommandAsker)>) {
        self.d.st.lock().command_risk = Some(risk);
    }
    fn audit_rows(&self) -> Vec<Obj> {
        read_rows(&self.audit_path)
    }
    async fn settle(&self) {
        for job in self.d.background_jobs() {
            job.join().await;
        }
    }
}

fn read_rows(path: &Path) -> Vec<Obj> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| obj(serde_json::from_str(l).expect("an audit line")))
        .collect()
}

fn req(v: Value) -> Obj {
    obj(v)
}

/// A command asker that answers `decision` (risky: destroys 0.9; ms 230), recording each call.
type Calls = Arc<Mutex<Vec<(String, String, String)>>>;

fn asker_saying(decision: &str, why: &str, calls: Option<Calls>) -> CommandAsker {
    let decision = decision.to_string();
    let why = if why.is_empty() && decision == "unknown" { "HTTP 503".to_string() } else { why.to_string() };
    Arc::new(move |tool: &str, command: &str, cwd: &str| {
        if let Some(c) = &calls {
            c.lock().push((tool.to_string(), command.to_string(), cwd.to_string()));
        }
        let mut a = CommandAnswer::new(if decision == "risky" { 0.9 } else { 0.1 }, 0.0, 0.0, 0.0);
        a.ms = 230.0;
        if decision == "unknown" {
            a.error = "HTTP 503".into();
        }
        CommandVerdict::new(&decision, &why, a)
    })
}

// ---- test_daemon_background.py ------------------------------------------------------------------------------------

#[tokio::test]
async fn test_turn_end_side_effects_task_is_held_in_background() {
    let r = rig_with(false, MatcherConfig::default());
    let got = r.d.handle_ipc(req(json!({"evt": "turn_end", "session_id": "s1"}))).await.unwrap();
    assert_eq!(Value::Object(got), json!({"ok": true}));
    let held = r.d.background_jobs();
    assert_eq!(held.len(), 1); // the side-effects task, held until it finishes
    assert_eq!(held[0].name(), "turn-end-side-effects");
    for job in held {
        job.join().await;
    }
    tokio::task::yield_now().await;
    // discarded once done; what is left is the real side effects' own follow-up push at pulse end
    // (Python's test stubbed the side effects, so its set came back empty)
    let left: Vec<String> = r.d.background_jobs().iter().map(|j| j.name().to_string()).collect();
    assert_eq!(left, ["heartbeat-after"]);
    // the deferred running→0 flip is pending for the session, not held in the background
    assert!(r.d.st.lock().pending_turn_ends.contains_key("s1"));
}

#[tokio::test]
async fn test_drain_background_waits_for_held_work() {
    let r = rig_with(false, MatcherConfig::default());
    r.d.handle_ipc(req(json!({"evt": "turn_end", "session_id": "s1"}))).await.unwrap();
    let task = r.d.background_jobs().into_iter().next().expect("held");
    r.d.drain_background().await;
    assert!(task.done() && !task.cancelled());
}

// ---- test_ipc_dispatch.py -----------------------------------------------------------------------------------------

#[test]
fn test_every_entry_is_the_matching_async_method() {
    // Python's IPC_HANDLERS, every key: the Rust table answers exactly these events
    let python: HashSet<&str> = [
        "expressions", "explore", "lesson", "notes", "trace", "pose", "axis", "follow", "move", "celebrate", "species",
        "diag", "identity", "session_start", "session_end", "turn_begin", "turn_end", "pretooluse", "permissionrequest",
        "push_character", "unpair", "sound", "mic", "chat", "mcp_tools", "mcp_call", "get_state", "posttooluse",
        "notification",
    ]
    .into_iter()
    .collect();
    let rust: HashSet<&str> = IPC_EVENTS.iter().copied().collect();
    assert_eq!(rust.len(), IPC_EVENTS.len(), "no event twice");
    assert_eq!(rust, python);
}

#[tokio::test]
async fn test_unknown_and_malformed_events_are_refused_as_before() {
    let r = rig_with(false, MatcherConfig::default());
    let ask = |v: Value| {
        let d = r.d.clone();
        async move { Value::Object(d.handle_ipc(req(v)).await.unwrap()) }
    };
    assert_eq!(ask(json!({"evt": "nope"})).await, json!({"ok": false, "error": "unknown evt: 'nope'"}));
    assert_eq!(ask(json!({})).await, json!({"ok": false, "error": "unknown evt: None"}));
    assert_eq!(
        ask(json!({"evt": ["pretooluse"]})).await,
        json!({"ok": false, "error": "unknown evt: ['pretooluse']"})
    );
    // the server's handler: a request that is not an object raises as `req.get` did
    let h = r.d.ipc_handler();
    let err = h(json!(["evt"])).await.unwrap_err();
    assert_eq!(eve_ipc::error_type_name(&err), "AttributeError");
    assert_eq!(err.to_string(), "'list' object has no attribute 'get'");
    let ok = h(json!({"evt": "nope"})).await.unwrap();
    assert_eq!(ok, json!({"ok": false, "error": "unknown evt: 'nope'"}));
}

// ---- test_ipc_chat.py ---------------------------------------------------------------------------------------------

async fn chat(r: &Rig, v: Value) -> Value {
    Value::Object(r.d.ipc_chat(req(v)).await.unwrap())
}

#[tokio::test]
async fn test_typed_words_reach_the_chat_as_the_owners() {
    let r = rig_with(false, MatcherConfig::default());
    let tg = Arc::new(FakeInlet::new(false, None));
    r.inlet(tg.clone());
    assert_eq!(chat(&r, json!({"evt": "chat", "text": "  what's on my calendar?  "})).await, json!({"ok": true}));
    // typed in the Mac chat window, not said: nothing it starts is read aloud on an open call (CallScope.lean)
    assert_eq!(*tg.heard.lock(), vec![("what's on my calendar?".to_string(), false)]);
}

#[tokio::test]
async fn test_long_words_are_cut_to_a_telegram_message() {
    let r = rig_with(false, MatcherConfig::default());
    let tg = Arc::new(FakeInlet::new(false, None));
    r.inlet(tg.clone());
    chat(&r, json!({"text": "x".repeat(10000)})).await;
    assert_eq!(tg.heard.lock()[0].0.chars().count(), eve_daemon::CHAT_MAX_CHARS);
}

#[tokio::test]
async fn test_nothing_is_sent_when_it_cannot_be() {
    let r = rig_with(false, MatcherConfig::default());
    let tg = Arc::new(FakeInlet::new(false, None));
    r.inlet(tg.clone());
    assert_eq!(chat(&r, json!({"text": "   "})).await["ok"], json!(false));
    assert!(tg.heard.lock().is_empty());
    r.d.st.lock().telegram = None;
    assert_eq!(chat(&r, json!({"text": "hi"})).await["ok"], json!(false));
    let no_owner = Arc::new(FakeInlet::new(false, None));
    *no_owner.chat_id.lock() = None;
    r.inlet(no_owner.clone());
    assert_eq!(chat(&r, json!({"text": "hi"})).await["ok"], json!(false));
    assert!(no_owner.heard.lock().is_empty());
}

// ---- test_command_risk.py: the daemon -----------------------------------------------------------------------------

const REQ: &str = r#"{"tool_use_id": "t1", "session_id": "s1", "tool_name": "Bash", "hint": "git push --force origin main", "cwd": "/r"}"#;

fn base_req() -> Obj {
    obj(serde_json::from_str(REQ).unwrap())
}

fn with(mut r: Obj, extra: Value) -> Obj {
    r.extend(obj(extra));
    r
}

/// `daemon_with(inlet, risk, classify)`: the regex class by a matcher that gives it for any command.
fn risk_rig(inlet: Arc<FakeInlet>, risk: Option<(String, CommandAsker)>, classify: &str) -> Rig {
    let m = match classify {
        "allow" => matchers(&[".*"], &[]),
        "ask" => matchers(&[], &[".*"]),
        _ => MatcherConfig::default(),
    };
    let r = rig_with(false, m);
    r.inlet(inlet);
    r.risk(risk);
    r
}

async fn pre(r: &Rig, q: Obj) -> Value {
    let out = Value::Object(r.d.handle_pretooluse(q).await.unwrap());
    r.settle().await;
    out
}

fn last(r: &Rig) -> Obj {
    r.audit_rows().pop().expect("an audit row")
}

#[tokio::test]
async fn test_off_is_todays_relay() {
    let tg = Arc::new(FakeInlet::new(true, None));
    let r = risk_rig(tg.clone(), None, "default");
    assert_eq!(pre(&r, base_req()).await, json!({"ok": true, "decision": "allow"}));
    assert_eq!(last(&r)["source"], json!("telegram_relay"));
    assert!(tg.asked.lock().is_empty());
    // bypass mode with the gate off: no decision, as before
    let r = risk_rig(Arc::new(FakeInlet::new(true, None)), None, "default");
    assert_eq!(pre(&r, with(base_req(), json!({"permission_mode": "bypassPermissions"}))).await, json!({"ok": true}));
    assert_eq!(last(&r)["source"], json!("ble_disconnected"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_shadow_allows_and_logs_the_verdict() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tg = Arc::new(FakeInlet::new(true, None));
    let r = risk_rig(tg.clone(), Some(("shadow".into(), asker_saying("risky", "destroys data", Some(calls.clone())))), "default");
    assert_eq!(pre(&r, base_req()).await, json!({"ok": true, "decision": "allow"}));
    r.settle().await; // the shadow verdict is written by its own background task
    assert_eq!(
        *calls.lock(),
        vec![("Bash".to_string(), "git push --force origin main".to_string(), "/r".to_string())]
    );
    assert!(tg.asked.lock().is_empty());
    let rows = r.audit_rows();
    let sources: Vec<&str> = rows.iter().map(|r| r["source"].as_str().unwrap()).collect();
    assert_eq!(sources, ["telegram_relay", "jev_shadow"]);
    let shadow = rows.last().unwrap();
    assert_eq!(shadow["decision"], Value::Null);
    assert_eq!(shadow["jev"]["verdict"], json!("risky"));
    assert_eq!(shadow["jev"]["destroys"], json!(0.9));
    assert_eq!(shadow["jev"]["why"], json!("destroys data"));
    assert_eq!(shadow["jev"]["ms"], json!(230));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_ask_routes_a_risky_command_to_the_phone() {
    for (decision, expect) in [("allow", json!({"ok": true, "decision": "allow"})), ("deny", json!({"ok": true, "decision": "deny"}))] {
        let tg = Arc::new(FakeInlet::new(true, Some(decision)));
        let r = risk_rig(tg.clone(), Some(("ask".into(), asker_saying("risky", "destroys data, publishes or sends", None))), "default");
        assert_eq!(pre(&r, with(base_req(), json!({"permission_mode": "bypassPermissions"}))).await, expect); // bypass too
        assert_eq!(
            *tg.asked.lock(),
            vec![(
                "Bash".to_string(),
                "git push --force origin main [Jev: destroys data, publishes or sends]".to_string(),
                "/r".to_string(),
                true
            )]
        );
        let row = last(&r);
        assert_eq!(row["source"], json!("telegram"));
        assert_eq!(row["jev"]["verdict"], json!("risky"));
    }
    // silence defers to Claude Code's own flow, never denies
    let r = risk_rig(Arc::new(FakeInlet::new(true, None)), Some(("ask".into(), asker_saying("risky", "destroys data", None))), "default");
    assert_eq!(pre(&r, base_req()).await, json!({"ok": true}));
    let row = last(&r);
    assert_eq!(row["source"], json!("jev_risky_deferred"));
    assert_eq!(row["decision"], Value::Null);
    // a safe verdict is allowed as before; in bypass mode Claude Code allows on its own (no decision)
    let tg = Arc::new(FakeInlet::new(true, Some("deny")));
    let r = risk_rig(tg.clone(), Some(("ask".into(), asker_saying("safe", "", None))), "default");
    assert_eq!(pre(&r, with(base_req(), json!({"hint": "pytest -q"}))).await, json!({"ok": true, "decision": "allow"}));
    assert!(tg.asked.lock().is_empty());
    assert_eq!(last(&r)["source"], json!("jev_safe"));
    let r = risk_rig(Arc::new(FakeInlet::new(true, Some("deny"))), Some(("ask".into(), asker_saying("safe", "", None))), "default");
    assert_eq!(
        pre(&r, with(base_req(), json!({"hint": "pytest -q", "permission_mode": "bypassPermissions"}))).await,
        json!({"ok": true})
    );
    let row = last(&r);
    assert_eq!(row["source"], json!("jev_safe"));
    assert_eq!(row["decision"], Value::Null);
    // the model failing is allowed and logged, never a denial and never a traceback
    let tg = Arc::new(FakeInlet::new(true, Some("deny")));
    let r = risk_rig(tg.clone(), Some(("ask".into(), asker_saying("unknown", "", None))), "default");
    assert_eq!(pre(&r, base_req()).await, json!({"ok": true, "decision": "allow"}));
    assert!(tg.asked.lock().is_empty());
    assert_eq!(last(&r)["source"], json!("jev_error"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_the_regex_class_asks_before_jev_is_consulted() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tg = Arc::new(FakeInlet::new(true, Some("allow")));
    let r = risk_rig(tg.clone(), Some(("ask".into(), asker_saying("safe", "", Some(calls.clone())))), "ask");
    assert_eq!(pre(&r, with(base_req(), json!({"hint": "sudo rm -rf /"}))).await, json!({"ok": true, "decision": "allow"}));
    assert!(calls.lock().is_empty());
    assert_eq!(*tg.asked.lock(), vec![("Bash".to_string(), "sudo rm -rf /".to_string(), "/r".to_string(), true)]);
    assert_eq!(last(&r)["source"], json!("telegram"));
    // the regex allow class short-circuits before any of this
    let r = risk_rig(Arc::new(FakeInlet::new(true, Some("deny"))), Some(("ask".into(), asker_saying("risky", "", Some(calls.clone())))), "allow");
    assert_eq!(pre(&r, with(base_req(), json!({"hint": "ls"}))).await, json!({"ok": true, "decision": "allow"}));
    assert!(calls.lock().is_empty());
    assert_eq!(last(&r)["source"], json!("auto_allow"));
    // a tool that is not Bash is never judged
    let r = risk_rig(Arc::new(FakeInlet::new(true, None)), Some(("ask".into(), asker_saying("risky", "", Some(calls.clone())))), "default");
    assert_eq!(
        pre(&r, with(base_req(), json!({"tool_name": "Edit", "hint": "src/app.py"}))).await,
        json!({"ok": true, "decision": "allow"})
    );
    assert!(calls.lock().is_empty());
}

#[tokio::test]
async fn test_command_risk_reads_its_mode_once() {
    // EVE_COMMAND_RISK=off: built once as off, and the cache answers after
    let dir = tempfile::tempdir().unwrap();
    let link = Arc::new(FakeLink::default());
    let mut parts = DaemonParts::for_test(link, dir.path());
    parts.environ = Some([("EVE_COMMAND_RISK".to_string(), " OFF ".to_string())].into_iter().collect());
    let d = Daemon::assemble(parts);
    assert!(d.command_risk().is_none());
    assert!(matches!(d.st.lock().command_risk, Some(None)));
    // a mode it does not know is off, with a warning
    let dir = tempfile::tempdir().unwrap();
    let mut parts = DaemonParts::for_test(Arc::new(FakeLink::default()), dir.path());
    parts.environ = Some([("EVE_COMMAND_RISK".to_string(), "loud".to_string())].into_iter().collect());
    let d = Daemon::assemble(parts);
    assert!(d.command_risk().is_none());
    // a preset cache is what it answers
    d.st.lock().command_risk = Some(Some(("ask".into(), asker_saying("safe", "", None))));
    assert_eq!(d.command_risk().map(|(m, _)| m).as_deref(), Some("ask"));
}

// ---- test_telegram.py: the phone's decision -----------------------------------------------------------------------

#[tokio::test]
async fn test_the_daemon_honours_the_phones_decision_and_defers_without_one() {
    let q = obj(json!({"tool_use_id": "t1", "session_id": "s1", "tool_name": "Bash", "hint": "rm -rf build/", "cwd": "/r"}));
    for (decision, expect) in [
        (Some("allow"), json!({"ok": true, "decision": "allow"})),
        (Some("deny"), json!({"ok": true, "decision": "deny"})),
        (None, json!({"ok": true})),
    ] {
        let tg = Arc::new(FakeInlet::new(true, decision));
        let r = risk_rig(tg.clone(), None, "ask"); // the owner's always_ask list: rm, sudo
        assert_eq!(pre(&r, q.clone()).await, expect);
        assert_eq!(*tg.asked.lock(), vec![("Bash".to_string(), "rm -rf build/".to_string(), "/r".to_string(), true)]);
        assert_eq!(last(&r)["source"], json!(if decision.is_some() { "telegram" } else { "ble_disconnected" }));
    }
    // the relay is bypass (owner, 2026-09-21): anything else is allowed without a question
    let quiet = Arc::new(FakeInlet::new(true, None));
    let r = risk_rig(quiet.clone(), None, "default");
    assert_eq!(pre(&r, with(q.clone(), json!({"hint": "pytest -q"}))).await, json!({"ok": true, "decision": "allow"}));
    assert!(quiet.asked.lock().is_empty());
    assert_eq!(last(&r)["source"], json!("telegram_relay"));
    // EVE_TELEGRAM_ASK=1: every call is asked, and silence still defers
    let mut asks = FakeInlet::new(true, None);
    asks.ask_all = true;
    let asks = Arc::new(asks);
    let r = risk_rig(asks.clone(), None, "default");
    assert_eq!(pre(&r, with(q.clone(), json!({"hint": "pytest -q"}))).await, json!({"ok": true}));
    assert_eq!(*asks.asked.lock(), vec![("Bash".to_string(), "pytest -q".to_string(), "/r".to_string(), true)]);
    assert_eq!(last(&r)["source"], json!("ble_disconnected"));
    let off = Arc::new(FakeInlet::new(false, Some("allow")));
    let r = risk_rig(off.clone(), None, "ask");
    assert_eq!(pre(&r, q.clone()).await, json!({"ok": true}));
    assert!(off.asked.lock().is_empty());
    // bypass mode: Claude Code will not prompt, so neither does the phone (live 2026-09-21)
    let bypass = Arc::new(FakeInlet::new(true, Some("allow")));
    let r = risk_rig(bypass.clone(), None, "ask");
    assert_eq!(pre(&r, with(q.clone(), json!({"permission_mode": "bypassPermissions"}))).await, json!({"ok": true}));
    assert!(bypass.asked.lock().is_empty());
    assert_eq!(*bypass.relayed.lock(), vec![("Bash".to_string(), "rm -rf build/".to_string())]); // still shown
    // an out-of-cwd Read is a prompt on the Mac too: with the relay on it is allowed the same way
    let tg = Arc::new(FakeInlet::new(true, None));
    let r = risk_rig(tg.clone(), None, "default");
    let read = obj(json!({"tool_use_id": "t2", "session_id": "s1", "tool_name": "Read", "hint": "/etc/hosts", "cwd": "/r"}));
    assert_eq!(pre(&r, read.clone()).await, json!({"ok": true, "decision": "allow"}));
    assert_eq!(last(&r)["source"], json!("telegram_relay"));
    tg.claude.store(false, Ordering::SeqCst);
    assert_eq!(pre(&r, read).await, json!({"ok": true}));
}

// ---- test_relay_questions.py --------------------------------------------------------------------------------------

#[tokio::test]
async fn test_ask_user_question_is_relayed_and_never_decided() {
    let relay = Arc::new(FakeInlet::new(true, Some("allow")));
    let r = risk_rig(relay.clone(), None, "ask");
    let out = pre(
        &r,
        obj(json!({"tool_use_id": "t1", "tool_name": "AskUserQuestion", "hint": "rm the build? (1. yes / 2. no)"})),
    )
    .await;
    assert_eq!(out, json!({"ok": true})); // no decision, and not the rm matcher's
    assert_eq!(
        *relay.relayed.lock(),
        vec![("AskUserQuestion".to_string(), "rm the build? (1. yes / 2. no)".to_string())]
    );
    assert!(relay.asked.lock().is_empty());
}

async fn perm(r: &Rig, v: Value) -> Value {
    Value::Object(r.d.handle_permission_request(req(v)).await.unwrap())
}

#[tokio::test]
async fn test_permission_request_asks_the_phone_when_the_relay_is_on() {
    let relay = Arc::new(FakeInlet::new(true, Some("deny")));
    let r = risk_rig(relay.clone(), None, "default");
    assert_eq!(perm(&r, json!({"tool_name": "Edit", "hint": "/x/app.py"})).await, json!({"ok": true, "decision": "deny"}));
    let asked: Vec<(String, String)> = relay.asked.lock().iter().map(|a| (a.0.clone(), a.1.clone())).collect();
    assert_eq!(asked, vec![("Edit".to_string(), "/x/app.py".to_string())]);
    let row = last(&r);
    assert_eq!((row["matcher"].clone(), row["session"].clone()), (json!("permission_request"), json!("unknown")));
}

#[tokio::test]
async fn test_permission_request_without_relay_or_answer_leaves_the_dialog() {
    let off = Arc::new(FakeInlet::new(false, Some("allow")));
    let r = risk_rig(off, None, "default");
    assert_eq!(perm(&r, json!({"tool_name": "Edit"})).await, json!({"ok": true}));
    r.inlet(Arc::new(FakeInlet::new(true, None)));
    assert_eq!(perm(&r, json!({"tool_name": "Edit"})).await, json!({"ok": true}));
    let q = Arc::new(FakeInlet::new(true, Some("allow")));
    r.inlet(q.clone());
    assert_eq!(perm(&r, json!({"tool_name": "AskUserQuestion"})).await, json!({"ok": true}));
    assert!(q.asked.lock().is_empty());
    r.d.st.lock().telegram = None;
    assert_eq!(perm(&r, json!({"tool_name": "Edit"})).await, json!({"ok": true}));
}

// ---- test_permission_matrix.py ------------------------------------------------------------------------------------

const TOOLS: [&str; 3] = ["Bash", "Edit", "AskUserQuestion"];
const HINTS: [&str; 4] = ["ls -la", "rm -rf build", "make build", ""];
const MODES: [&str; 2] = ["default", "bypassPermissions"];
const RISK: [&str; 5] = ["none", "shadow", "ask-safe", "ask-risky", "ask-error"];
const ANSWERS: [Option<&str>; 3] = [Some("allow"), Some("deny"), None];
const READ_PATHS: [&str; 3] = ["/repo/src/a.py", "/elsewhere/b.py", ""];

fn py_bool(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

fn py_opt(a: Option<&str>) -> &str {
    a.unwrap_or("None")
}

struct Case {
    key: String,
    call: &'static str,
    req: Obj,
    on: bool,
    answer: Option<&'static str>,
    ask_all: bool,
    link: bool,
    risk: &'static str,
}

fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for tool in TOOLS {
        for hint in HINTS {
            for on in [false, true] {
                for mode in MODES {
                    for link in [true, false] {
                        for risk in RISK {
                            for answer in ANSWERS {
                                for ask_all in [false, true] {
                                    let req = obj(json!({"tool_use_id": "t1", "session_id": "s1", "tool_name": tool,
                                                         "hint": hint, "cwd": "/repo", "permission_mode": mode}));
                                    let key = format!(
                                        "pre|{tool}|{hint}|relay={}|{mode}|link={}|{risk}|ans={}|all={}",
                                        py_bool(on),
                                        py_bool(link),
                                        py_opt(answer),
                                        py_bool(ask_all)
                                    );
                                    out.push(Case { key, call: "pre", req, on, answer, ask_all, link, risk });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for path in READ_PATHS {
        for on in [false, true] {
            for mode in MODES {
                for link in [true, false] {
                    let req = obj(json!({"tool_use_id": "t2", "session_id": "s1", "tool_name": "Read", "hint": path,
                                         "cwd": "/repo", "permission_mode": mode}));
                    let key = format!("read|{path}|relay={}|{mode}|link={}", py_bool(on), py_bool(link));
                    out.push(Case { key, call: "pre", req, on, answer: None, ask_all: false, link, risk: "none" });
                }
            }
        }
    }
    for tool in ["Edit", "Bash", "AskUserQuestion"] {
        for on in [false, true] {
            for answer in ANSWERS {
                let req = obj(json!({"tool_name": tool, "hint": "/repo/x.py", "cwd": "/repo", "session_id": "s1"}));
                let key = format!("perm|{tool}|relay={}|ans={}", py_bool(on), py_opt(answer));
                out.push(Case { key, call: "perm", req, on, answer, ask_all: false, link: true, risk: "none" });
            }
        }
    }
    out.push(Case {
        key: "pre|missing-id".into(),
        call: "pre",
        req: obj(json!({"tool_name": "Bash", "hint": "ls"})),
        on: false,
        answer: None,
        ask_all: false,
        link: true,
        risk: "none",
    });
    out
}

fn verdict_for(kind: &str) -> CommandVerdict {
    let mut a = CommandAnswer::new(0.5, 0.0, 0.0, 0.0);
    a.ms = 12.0;
    CommandVerdict::new(kind, &format!("because {kind}"), a)
}

async fn one(case: &Case, dir: &Path) -> Value {
    let link = Arc::new(FakeLink::default());
    link.connected.store(case.link, Ordering::SeqCst);
    let mut parts = DaemonParts::for_test(link, dir);
    parts.matchers = matchers(&[r"^ls\b"], &[r"^rm\b"]);
    let audit_path = dir.join(format!("audit-{}.jsonl", next_id()));
    parts.audit = eve_audit::AuditLog::new(Some(audit_path.clone()));
    parts.environ = Some(Default::default());
    let d = Daemon::assemble(parts);
    let mut relay = FakeInlet::new(case.on, case.answer);
    relay.ask_all = case.ask_all;
    let relay = Arc::new(relay);
    d.st.lock().telegram = Some(relay.clone() as Arc<dyn Inlet>);
    // the shadow gate's hint goes to the shadow log (Python patched _shadow_command_risk to log the hint)
    let shadow: Arc<Mutex<Vec<String>>> = Arc::default();
    let risk: Option<(String, CommandAsker)> = match case.risk {
        "shadow" => {
            let shadow = shadow.clone();
            Some((
                "shadow".into(),
                Arc::new(move |_t: &str, h: &str, _c: &str| {
                    shadow.lock().push(h.to_string());
                    verdict_for("safe")
                }),
            ))
        }
        r if r.starts_with("ask-") => {
            let kind = r[4..].to_string();
            Some(("ask".into(), Arc::new(move |_t: &str, _h: &str, _c: &str| verdict_for(&kind))))
        }
        _ => None,
    };
    d.st.lock().command_risk = Some(risk);
    let resp = match case.call {
        "pre" => d.handle_pretooluse(case.req.clone()).await.unwrap(),
        _ => d.handle_permission_request(case.req.clone()).await.unwrap(),
    };
    for job in d.background_jobs() {
        job.join().await;
    }
    // Python's patched shadow never audited; the real one's row is left out here
    let audit: Vec<Value> = read_rows(&audit_path)
        .into_iter()
        .filter(|r| r["source"] != json!("jev_shadow"))
        .map(|r| json!([r["decision"], r["source"], r["matcher"], r.contains_key("jev")]))
        .collect();
    let asked: Vec<Value> = relay.asked.lock().iter().map(|a| json!([a.0, a.1, py_bool(a.3)])).collect();
    let relayed: Vec<Value> = relay.relayed.lock().iter().map(|a| json!([a.0, a.1])).collect();
    let shadow = shadow.lock().clone();
    json!({"resp": resp, "audit": audit, "asked": asked, "relayed": relayed, "shadow": shadow})
}

fn golden() -> Map<String, Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bridge/tests/fixtures/permission_matrix.json.gz");
    let out = std::process::Command::new("gzip").arg("-dc").arg(&path).output().expect("gzip");
    assert!(out.status.success(), "gzip -dc {}", path.display());
    obj(serde_json::from_slice(&out.stdout).expect("the golden master"))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_the_permission_path_matches_its_golden_master() {
    let want = golden();
    let dir = tempfile::tempdir().unwrap();
    let mut got = Map::new();
    for case in cases() {
        let v = one(&case, dir.path()).await;
        got.insert(case.key.clone(), v);
    }
    let gk: HashSet<&String> = got.keys().collect();
    let wk: HashSet<&String> = want.keys().collect();
    assert_eq!(gk, wk);
    let diff: Vec<&String> = want.keys().filter(|k| got[*k] != want[*k]).collect();
    assert!(diff.is_empty(), "{} cases changed, e.g. {}: {} != {}", diff.len(), diff[0], got[diff[0]], want[diff[0]]);
}

#[test]
fn test_the_matrix_covers_every_outcome_it_should() {
    // A golden master that never reaches a branch proves nothing about it: every audited outcome of the path,
    // and each response shape, occurs somewhere in the recorded matrix.
    let golden = golden();
    let sources: HashSet<(Value, Value)> = golden
        .values()
        .flat_map(|r| r["audit"].as_array().unwrap().iter().map(|a| (a[0].clone(), a[1].clone())))
        .collect();
    for (d, s) in [
        (json!("allow"), "auto_allow"),
        (json!("allow"), "telegram_relay"),
        (json!("allow"), "telegram"),
        (json!("deny"), "telegram"),
        (Value::Null, "ble_disconnected"),
        (Value::Null, "defer"),
        (json!("allow"), "jev_safe"),
        (Value::Null, "jev_safe"),
        (json!("allow"), "jev_error"),
        (Value::Null, "jev_error"),
        (Value::Null, "jev_risky_deferred"),
    ] {
        assert!(sources.contains(&(d.clone(), json!(s))), "{d} {s}");
    }
    let responses: Vec<&Value> = golden.values().map(|r| &r["resp"]).collect();
    for shape in [
        json!({"ok": true}),
        json!({"ok": true, "decision": "allow"}),
        json!({"ok": true, "decision": "deny"}),
        json!({"ok": false, "error": "missing tool_use_id"}),
    ] {
        assert!(responses.contains(&&shape), "{shape}");
    }
    assert!(golden.values().any(|r| !r["shadow"].as_array().unwrap().is_empty())); // the shadow gate ran
    assert!(golden.values().any(|r| !r["relayed"].as_array().unwrap().is_empty())); // a question reached the phone
    assert_eq!(cases().len(), golden.len());
}

// ---- test_eve_mcp.py: the daemon's side ---------------------------------------------------------------------------

const SID: &str = "11111111-2222-3333-4444-555555555555";

fn tool() -> Obj {
    obj(json!({"name": "memory_search", "description": "d", "input_schema": {"type": "object"}}))
}

fn work_rig(inlet: Arc<FakeInlet>) -> (Rig, String) {
    let r = rig_with(false, MatcherConfig::default());
    let dir = r._dir.path().to_path_buf();
    let work = eve_work_sessions::WorkSessions::new(eve_work_sessions::Options {
        root: Some(dir.join("work")),
        home: Some(dir.join("home")),
        osascript: Arc::new(|_s, _a| Box::pin(async { panic!("nothing may open a terminal here") })),
        ..Default::default()
    });
    let token = "a-session-token".to_string();
    work.records().insert(
        SID.to_string(),
        eve_work_sessions::Record::new(SID, eve_state::wall_time(), "t", eve_work_sessions::hash_token(&token)),
    );
    {
        let mut st = r.d.st.lock();
        st.work = Some(Arc::new(work));
        st.telegram = Some(inlet as Arc<dyn Inlet>);
    }
    (r, token)
}

fn fake_inlet_with_tool() -> FakeInlet {
    let mut i = FakeInlet::new(false, None);
    i.tools = vec![tool()];
    i
}

#[tokio::test]
async fn test_the_daemon_answers_only_a_live_sessions_token() {
    let inlet = Arc::new(fake_inlet_with_tool());
    let (r, token) = work_rig(inlet.clone());
    for (sid, tok) in [
        (json!(SID), json!("wrong")),
        (json!(SID), json!("")),
        (json!(SID), Value::Null),
        (json!("not-a-session"), json!(token)),
        (Value::Null, json!(token)),
    ] {
        let got = r.d.ipc_mcp_tools(req(json!({"evt": "mcp_tools", "sid": sid, "token": tok}))).await.unwrap();
        assert_eq!(got["ok"], json!(false));
        let got = r
            .d
            .ipc_mcp_call(req(json!({"evt": "mcp_call", "sid": sid, "token": tok, "name": "memory_search", "args": {}})))
            .await
            .unwrap();
        assert_eq!(got["ok"], json!(false));
    }
    assert!(inlet.calls.lock().is_empty());
    // positive control
    let got = r.d.ipc_mcp_tools(req(json!({"evt": "mcp_tools", "sid": SID, "token": token}))).await.unwrap();
    assert_eq!(
        Value::Object(got),
        json!({"ok": true, "tools": [tool()], "call_secs": 180.0 + eve_daemon::MCP_CALL_MARGIN_SECS})
    );
    let got = r
        .d
        .ipc_mcp_call(req(json!({"evt": "mcp_call", "sid": SID, "token": token, "name": "memory_search", "args": {"query": "q"}})))
        .await
        .unwrap();
    assert_eq!(Value::Object(got), json!({"ok": true, "result": {"ok": true}, "outside": "memory"}));
    assert_eq!(*inlet.calls.lock(), vec![("memory_search".to_string(), json!({"query": "q"}), SID.to_string())]);
    // a stopped session
    r.d.st.lock().work.clone().unwrap().records().get_mut(SID).unwrap().stopped = true;
    let got = r.d.ipc_mcp_tools(req(json!({"evt": "mcp_tools", "sid": SID, "token": token}))).await.unwrap();
    assert_eq!(got["ok"], json!(false));
    // Work off
    r.d.st.lock().work = None;
    let got = r.d.ipc_mcp_tools(req(json!({"evt": "mcp_tools", "sid": SID, "token": token}))).await.unwrap();
    assert_eq!(Value::Object(got), json!({"ok": false, "error": "not a live Work session eve started"}));
}

#[tokio::test]
async fn test_the_daemon_bounds_a_long_call_and_never_cancels_it() {
    // A call still running past the ask timeout plus MCP_CALL_BOUND_SECS: the daemon answers first (before the MCP
    // side's call_secs), says the outcome is unknown, and the call goes on to finish in _background.
    const _: () = assert!(eve_daemon::MCP_CALL_BOUND_SECS < eve_daemon::MCP_CALL_MARGIN_SECS);
    let release = Arc::new(tokio::sync::Notify::new());
    let finished: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut inlet = fake_inlet_with_tool();
    // Python set the ask timeout to 0 and the bound to 0.05 s; the bound is a constant here, so the ask timeout
    // takes it back to 0.05 s
    inlet.ask_timeout = 0.05 - eve_daemon::MCP_CALL_BOUND_SECS;
    {
        let (release, finished) = (release.clone(), finished.clone());
        inlet.session_call = Some(Arc::new(move |name, _args, _sid| {
            let (release, finished) = (release.clone(), finished.clone());
            Box::pin(async move {
                release.notified().await;
                finished.lock().push(name);
                Ok((obj(json!({"ok": true})), String::new()))
            })
        }));
    }
    let (r, token) = work_rig(Arc::new(inlet));
    let got = r
        .d
        .ipc_mcp_call(req(json!({"sid": SID, "token": token, "name": "remember", "args": {}})))
        .await
        .unwrap();
    assert_eq!(Value::Object(got.clone()), json!({"ok": false, "error": eve_daemon::MCP_STILL_RUNNING}));
    let error = got["error"].as_str().unwrap();
    assert!(error.contains("unknown") && error.contains("not retry"));
    let held = r.d.background_jobs();
    assert_eq!(held.len(), 1);
    assert!(finished.lock().is_empty());
    release.notify_one();
    for job in held {
        job.join().await;
    }
    tokio::task::yield_now().await;
    assert_eq!(*finished.lock(), vec!["remember".to_string()]);
    assert!(r.d.background_jobs().is_empty());
}

#[tokio::test]
async fn test_the_daemon_refuses_while_no_chat_runs() {
    let inlet = Arc::new(fake_inlet_with_tool());
    *inlet.chat_id.lock() = None;
    let (r, token) = work_rig(inlet.clone());
    let got = r
        .d
        .ipc_mcp_call(req(json!({"sid": SID, "token": token, "name": "memory_search", "args": {}})))
        .await
        .unwrap();
    assert_eq!(Value::Object(got), json!({"ok": false, "error": "eve's chat isn't running, or no owner has texted it yet"}));
    assert!(inlet.calls.lock().is_empty());
}

#[test]
fn test_the_hooks_reach_the_work_registry() {
    // Python read daemon.py's source for `_work_note(self, ` in each handler; here the same over ipc.rs
    let src = include_str!("../src/ipc.rs");
    for (handler, evt) in [
        ("ipc_session_start", "session_start"),
        ("ipc_session_end", "session_end"),
        ("ipc_turn_begin", "turn_begin"),
        ("ipc_turn_end", "turn_end"),
        ("ipc_notification", "notification"),
        ("ipc_posttooluse", "posttooluse"),
        ("ipc_pretooluse", "pretooluse"),
    ] {
        let body = src.split(&format!("pub async fn {handler}(")).nth(1).unwrap();
        let body = body.split("\n    pub ").next().unwrap();
        assert!(body.contains(&format!("self.work_note(\"{evt}\", &req)")), "{handler}");
    }
}

// ---- test_telegram.py: the turn's relay ---------------------------------------------------------------------------

#[tokio::test]
async fn test_the_daemon_ends_the_relays_typing_at_the_stop_hook() {
    let r = rig_with(false, MatcherConfig::default());
    let tg = Arc::new(FakeInlet::new(true, None));
    r.inlet(tg.clone());
    r.d.handle_ipc(req(json!({"evt": "session_start", "session_id": "s1", "cwd": "/Users/me/eve"}))).await.unwrap();
    let got = r.d.ipc_turn_end(req(json!({"session_id": "s1"}))).await.unwrap();
    assert_eq!(Value::Object(got), json!({"ok": true}));
    assert_eq!(*tg.ended.lock(), vec!["/Users/me/eve".to_string()]);
    tg.claude.store(false, Ordering::SeqCst);
    r.d.ipc_turn_end(req(json!({"session_id": "s1"}))).await.unwrap(); // relay off: nothing to end
    assert_eq!(tg.ended.lock().len(), 1);
    r.settle().await;
}

#[tokio::test]
async fn test_turn_begin_relays_the_prompt_and_adds_the_entry() {
    let r = rig_with(false, MatcherConfig::default());
    let tg = Arc::new(FakeInlet::new(true, None));
    r.inlet(tg.clone());
    r.d.handle_ipc(req(json!({"evt": "session_start", "session_id": "s1", "cwd": "/r"}))).await.unwrap();
    r.d.handle_ipc(req(json!({"evt": "turn_begin", "session_id": "s1", "prompt": "fix the tests"}))).await.unwrap();
    assert_eq!(*tg.began.lock(), vec![("fix the tests".to_string(), "/r".to_string())]);
    let state = r.d.state.lock();
    assert_eq!(state.entries[0].text, "> fix the tests");
    assert!(state.sessions["s1"].running);
    assert_eq!(state.completed_until, 0.0);
}

// ---- the other handlers -------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_posttooluse_adopts_the_session_and_backfills_its_cwd() {
    let r = rig_with(false, MatcherConfig::default());
    // posttooluse carries no cwd: the session is adopted without one
    r.d.handle_ipc(req(json!({"evt": "posttooluse", "session_id": "s9", "tool_name": "Edit"}))).await.unwrap();
    assert_eq!(r.d.state.lock().sessions["s9"].cwd, None);
    assert_eq!(r.d.state.lock().entries[0].text, "+ Edit");
    // the first hook that carries one names the row
    r.d.ensure_session(&req(json!({"session_id": "s9", "cwd": "/Users/me/eve"})));
    assert_eq!(r.d.state.lock().sessions["s9"].cwd.as_deref(), Some("/Users/me/eve"));
    r.d.ensure_session(&req(json!({"session_id": "s9", "cwd": "/elsewhere"})));
    assert_eq!(r.d.state.lock().sessions["s9"].cwd.as_deref(), Some("/Users/me/eve"));
    r.d.ensure_session(&req(json!({"session_id": ""})));
    assert_eq!(r.d.state.lock().sessions.len(), 1);
}

#[tokio::test]
async fn test_hook_events_without_a_session_id_raise_as_a_key_error() {
    let r = rig_with(false, MatcherConfig::default());
    let err = r.d.handle_ipc(req(json!({"evt": "session_end"}))).await.unwrap_err();
    assert_eq!(eve_ipc::error_type_name(&err), "KeyError");
    assert_eq!(err.to_string(), "'session_id'");
}

#[tokio::test]
async fn test_board_commands_need_the_board() {
    let r = rig_with(false, MatcherConfig::default());
    let ask = |v: Value| {
        let d = r.d.clone();
        async move { Value::Object(d.handle_ipc(req(v)).await.unwrap()) }
    };
    assert_eq!(ask(json!({"evt": "axis"})).await, json!({"ok": false, "error": "the robot is not connected"}));
    assert_eq!(ask(json!({"evt": "unpair"})).await, json!({"ok": false, "error": "ble not connected"}));
    assert_eq!(ask(json!({"evt": "push_character"})).await, json!({"ok": false, "error": "missing 'path'"}));
    assert_eq!(
        ask(json!({"evt": "push_character", "path": "/tmp/x"})).await,
        json!({"ok": false, "error": "ble not connected"})
    );
    assert_eq!(ask(json!({"evt": "move", "kind": "stop"})).await, json!({"ok": false, "error": "the board is not connected"}));
    // a bad gesture is the motion's own ValueError, before the board is asked
    let got = ask(json!({"evt": "move", "kind": "keys", "preset": "nope"})).await;
    assert_eq!(got["ok"], json!(false));
    assert!(got["error"].as_str().unwrap().starts_with("'nope' is not a gesture; try "));
    assert_eq!(ask(json!({"evt": "move", "speed": "fast"})).await,
               json!({"ok": false, "error": "could not convert string to float: 'fast'"}));
    assert_eq!(ask(json!({"evt": "species", "idx": 3})).await, json!({"ok": true, "connected": false}));
    assert!(r.link.sent.lock().is_empty());
    let diag = ask(json!({"evt": "diag"})).await;
    assert_eq!(diag, json!({"ok": true, "connected": false, "diag": null}));
}

#[tokio::test]
async fn test_board_commands_reach_a_connected_board() {
    let r = rig_with(true, MatcherConfig::default());
    let got = Value::Object(r.d.handle_ipc(req(json!({"evt": "species", "idx": "2"}))).await.unwrap());
    assert_eq!(got, json!({"ok": true, "connected": true}));
    let got = Value::Object(r.d.handle_ipc(req(json!({"evt": "move", "kind": "stop"}))).await.unwrap());
    assert_eq!(got["ok"], json!(true));
    assert_eq!(got["sent"], json!({"cmd": "move", "kind": "stop"}));
    let got = Value::Object(r.d.handle_ipc(req(json!({"evt": "unpair"}))).await.unwrap());
    assert_eq!(got, json!({"ok": true}));
    assert_eq!(
        *r.link.sent.lock(),
        vec![json!({"cmd": "species", "idx": 2}), json!({"cmd": "move", "kind": "stop"}), json!({"cmd": "unpair"})]
    );
    // the axis read waits for the board's ack, and times out on an older firmware
    let got = Value::Object(r.d.handle_ipc(req(json!({"evt": "axis", "pitch_rev": 1}))).await.unwrap());
    assert_eq!(got, json!({"ok": false, "error": "the board did not answer (older firmware?)"}));
    assert_eq!(r.link.sent.lock().last().unwrap(), &json!({"cmd": "axis", "pitch_rev": true}));
}

#[tokio::test]
async fn test_trace_and_pose_read_what_the_board_reported() {
    let r = rig_with(false, MatcherConfig::default());
    r.d.st.lock().motion_trace = vec![(1.23456, 10.0, -5.0)];
    let got = Value::Object(r.d.handle_ipc(req(json!({"evt": "trace"}))).await.unwrap());
    assert_eq!(got, json!({"ok": true, "trace": [[1.235, 10.0, -5.0]]}));
    let got = Value::Object(r.d.handle_ipc(req(json!({"evt": "pose"}))).await.unwrap());
    assert_eq!(got["ok"], json!(true));
    assert_eq!(got["at"], Value::Null); // -inf until the board echoes a pose
    assert_eq!(got["connected"], json!(false));
}

#[tokio::test]
async fn test_expressions_refuse_bad_text_and_unknown_actions() {
    let r = rig_with(false, MatcherConfig::default());
    let ask = |v: Value| {
        let d = r.d.clone();
        async move { Value::Object(d.handle_ipc(req(v)).await.unwrap()) }
    };
    assert_eq!(
        ask(json!({"evt": "expressions", "action": "react", "text": "  "})).await,
        json!({"ok": false, "error": "text must contain 1..2000 characters"})
    );
    assert_eq!(
        ask(json!({"evt": "expressions", "action": "react", "text": "x".repeat(2001)})).await,
        json!({"ok": false, "error": "text must contain 1..2000 characters"})
    );
    assert_eq!(
        ask(json!({"evt": "expressions", "action": "react", "text": "hello"})).await,
        json!({"ok": false, "error": "expressions are not ready or board is disconnected"})
    );
    assert_eq!(
        ask(json!({"evt": "expressions", "action": "dance"})).await,
        json!({"ok": false, "error": "unknown expression action"})
    );
    let status = ask(json!({"evt": "expressions"})).await;
    assert_eq!(status["ok"], json!(true));
    assert_eq!(status["phone"], Value::Null);
}

#[tokio::test]
async fn test_get_state_is_the_huds_snapshot() {
    let r = rig_with(false, MatcherConfig::default());
    r.d.state.lock().add_entry("> hello", None);
    let got = Value::Object(r.d.handle_ipc(req(json!({"evt": "get_state"}))).await.unwrap());
    let s = &got["state"];
    assert_eq!(got["ok"], json!(true));
    assert_eq!(s["ble_connected"], json!(false));
    assert_eq!(s["mic_listening"], json!(false));
    assert_eq!(s["sec"], Value::Null);
    assert_eq!(s["last_entry"], json!("> hello"));
    let keys: Vec<&str> = s.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "ble_connected", "mic_listening", "sec", "battery_pct", "total", "running", "waiting", "tokens_cumulative",
            "tokens_today", "cost_cumulative", "cost_today", "update_available", "last_entry"
        ]
    );
}

#[test]
fn test_mcp_late_done_logs_a_cancelled_and_a_finished_call() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let job = eve_daemon::seams::Job::spawn("late", async { tokio::time::sleep(Duration::from_secs(60)).await });
        job.cancel();
        eve_daemon::ipc::mcp_late_done(&job);
        assert!(job.cancelled());
        let done = eve_daemon::seams::Job::spawn("late", async {});
        done.join().await;
        eve_daemon::ipc::mcp_late_done(&done);
        assert!(done.done() && !done.cancelled());
    });
}

