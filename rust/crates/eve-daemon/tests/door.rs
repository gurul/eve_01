//! Tests for the door fill unit: the text door's [`Inlet`] mapping onto the real `TelegramInlet`, and the daemon's
//! door methods (the parts of test_chrome_lane.py, test_chrome_consent.py, test_phone_expressions.py,
//! test_telegram.py and test_telegram_chief.py that drive `Daemon._ask_owner_on_phone`, `_start_phone_expressions`,
//! `_make_telegram` and `_make_chief`). A test that builds a daemon needs `DaemonParts::for_test` and
//! `Daemon::assemble`, which another unit fills (and `_make_telegram` the app hubs and the agent factory): those
//! are ignored until the integrator joins them.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_brain::seams::Lent;
use eve_brain::telegram::{InletOptions, MutedBotApi, TelegramConfig, TelegramInlet};
use eve_daemon::seams::{BoardLink, Inlet, Obj};
use eve_daemon::{Daemon, DaemonParts};

const OWNER: i64 = 4242;

/// A door with nothing behind it: the Bot API muted (nothing leaves the Mac), the Responses creator answering
/// nothing, every router and model off, the environment empty.
fn door(owners: Vec<i64>, ask_permissions: bool) -> (Arc<TelegramInlet>, Arc<MutedBotApi>) {
    let cfg = TelegramConfig {
        enabled: true,
        token: "123:abc".into(),
        owner_ids: owners,
        ask_timeout_secs: 7.5,
        ask_permissions,
        ..TelegramConfig::default()
    };
    let api = Arc::new(MutedBotApi::new("123:abc", None));
    let create: eve_brain::seams::Create = Arc::new(|_body: Value| Box::pin(async { Ok(json!({})) }));
    let options = InletOptions {
        brief: Some(Arc::new(String::new)),
        clock: Some(Arc::new(|| 0.0)),
        wall: Some(Arc::new(|| 1_760_000_000.0)),
        screen: Some(Arc::new(|| None)),
        toolset_route: Lent::Given(None),
        grader: Lent::Given(None),
        small_talk_ask: Lent::Given(None),
        environ: Some(Default::default()),
        ..InletOptions::default()
    };
    let inlet = TelegramInlet::new(cfg, api.clone(), create, options);
    (inlet, api)
}

#[tokio::test]
async fn the_inlet_reads_the_doors_own_attributes() {
    let (inlet, _api) = door(vec![OWNER, 9999], true);
    let seen: &dyn Inlet = &*inlet;
    // a private chat's id is the user's: the door starts on the lowest owner id
    assert_eq!(seen.chat_id(), Some(OWNER));
    assert!(!seen.awaiting_answer());
    assert_eq!(seen.ask_timeout_secs(), 7.5);
    assert!(seen.ask_permissions());
    assert!(!seen.claude());
    assert!(!seen.task_running());
    assert!(seen.chief().is_none());
    assert_eq!(seen.conversation(), None);
    seen.forget_conversation();
    assert_eq!(seen.conversation(), None);
    seen.shutdown_purchases(); // no card: nothing to shut down, and no error
    let (inlet, _api) = door(vec![OWNER], false);
    assert!(!Inlet::ask_permissions(&*inlet));
}

#[tokio::test]
async fn a_line_said_through_the_inlet_goes_to_the_bot_api() {
    let (inlet, api) = door(vec![OWNER], false);
    let before = api.dropped();
    Inlet::say(&*inlet, OWNER, "hello from the daemon").await;
    assert!(api.dropped() > before, "the say reached the (muted) Bot API");
}

#[tokio::test]
async fn the_sessions_tools_come_through_the_trait() {
    let (inlet, _api) = door(vec![OWNER], false);
    let (tools, names) = Inlet::session_tools(&*inlet).expect("the chat is up");
    assert_eq!(tools.len(), names.len());
    assert!(tools.iter().all(|t| t.get("name").and_then(Value::as_str).is_some_and(|n| names.iter().any(|m| m == n))));
}

// ---- the daemon's door methods (need DaemonParts::for_test and Daemon::assemble) --------------------------------

/// The robot's link, never connected.
struct NoBoard;

#[async_trait]
impl BoardLink for NoBoard {
    fn connected(&self) -> bool {
        false
    }
    async fn wait_connected(&self) {}
    async fn send(&self, _obj: &Value, _codec: Option<&str>) -> bool {
        false
    }
    async fn run(&self) {}
    async fn stop(&self) {}
}

/// `SimpleNamespace(_chat_id=..., _ask_user=ask_user, _awaiting_answer=...)`: the questions it was asked.
#[derive(Default)]
struct FakeDoor {
    chat: Option<i64>,
    awaiting: bool,
    asked: Mutex<Vec<(String, i64, String)>>,
    said: Mutex<Vec<(i64, String)>>,
}

#[async_trait]
impl Inlet for FakeDoor {
    fn claude(&self) -> bool {
        false
    }
    fn task_running(&self) -> bool {
        false
    }
    fn chat_id(&self) -> Option<i64> {
        self.chat
    }
    fn awaiting_answer(&self) -> bool {
        self.awaiting
    }
    fn ask_timeout_secs(&self) -> f64 {
        60.0
    }
    fn ask_permissions(&self) -> bool {
        false
    }
    fn conversation(&self) -> Option<String> {
        None
    }
    fn forget_conversation(&self) {}
    fn chief(&self) -> Option<Arc<eve_chief::Chief>> {
        None
    }
    fn shutdown_purchases(&self) {}
    async fn ask_user(&self, question: &str, chat_id: i64, title: &str) -> String {
        self.asked.lock().push((question.to_string(), chat_id, title.to_string()));
        "yes".to_string()
    }
    async fn say(&self, chat_id: i64, text: &str) {
        self.said.lock().push((chat_id, text.to_string()));
    }
    async fn tell_owner(&self, _text: &str, _about: &str, _title: &str) -> bool {
        true
    }
    fn relay_session_started(&self, _cwd: &str) {}
    fn relay_turn_began(&self, _prompt: &str, _cwd: &str) {}
    fn relay_turn_ended(&self, _cwd: &str) {}
    async fn relay_notification(&self, _kind: &str, _message: &str, _waits: bool) {}
    async fn relay_text(&self, _text: &str, _cwd: &str) {}
    fn relay_tool_call(&self, _tool: &str, _hint: &str) {}
    async fn decide_permission(&self, _tool: &str, _hint: &str, _cwd: &str, _always: bool) -> Option<String> {
        None
    }
    fn hear(&self, _text: &str, _on_call: bool) {}
    fn session_tools(&self) -> anyhow::Result<(Vec<Obj>, Vec<String>)> {
        Ok((Vec::new(), Vec::new()))
    }
    async fn session_call(&self, _name: &str, _args: &Value, _sid: &str) -> anyhow::Result<(Obj, String)> {
        Ok((Obj::new(), String::new()))
    }
}

/// A daemon with inert parts and the environment `env` (nothing read from the process's).
fn daemon(dir: &std::path::Path, env: &[(&str, &str)]) -> Arc<Daemon> {
    let mut parts = DaemonParts::for_test(Arc::new(NoBoard), dir);
    parts.environ = Some(env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect());
    Daemon::assemble(parts)
}

#[tokio::test]
#[ignore = "needs unit daemon (DaemonParts::for_test, Daemon::assemble)"]
async fn test_chrome_approval_is_asked_in_the_owners_telegram_chat() {
    let dir = tempfile::tempdir().unwrap();
    let d = daemon(dir.path(), &[]);
    let fake = Arc::new(FakeDoor { chat: Some(4242), ..Default::default() });
    d.st.lock().telegram = Some(fake.clone() as Arc<dyn Inlet>);
    assert_eq!(d.ask_owner_checked("Allow?").await.unwrap(), "yes");
    assert_eq!(*fake.asked.lock(), vec![("Allow?".to_string(), 4242, "Chrome access".to_string())]);
    assert_eq!(d.ask_owner_on_phone("Allow?").await, "yes");
    d.st.lock().telegram = None;
    let err = d.ask_owner_checked("Allow?").await.unwrap_err();
    assert_eq!(err.to_string(), "no Telegram chat to ask the owner in");
    assert_eq!(d.ask_owner_on_phone("Allow?").await, "");
    // a door with no chat yet is no way to ask either
    d.st.lock().telegram = Some(Arc::new(FakeDoor::default()) as Arc<dyn Inlet>);
    assert!(d.ask_owner_checked("Allow?").await.is_err());
}

#[tokio::test]
#[ignore = "needs unit daemon (DaemonParts::for_test, Daemon::assemble)"]
async fn test_a_waiting_question_is_never_overwritten_by_the_chrome_question() {
    let dir = tempfile::tempdir().unwrap();
    let d = daemon(dir.path(), &[]);
    let busy = Arc::new(FakeDoor { chat: Some(1), awaiting: true, ..Default::default() });
    d.st.lock().telegram = Some(busy.clone() as Arc<dyn Inlet>);
    let err = d.ask_owner_checked("Allow?").await.unwrap_err();
    assert!(err.to_string().contains("already waiting"));
    assert!(busy.asked.lock().is_empty());
}

#[tokio::test]
#[ignore = "needs unit daemon (DaemonParts::for_test, Daemon::assemble)"]
async fn a_line_for_the_owner_goes_to_the_doors_chat_only_when_there_is_one() {
    let dir = tempfile::tempdir().unwrap();
    let d = daemon(dir.path(), &[]);
    d.tell_owner_on_phone("Chrome asked again").await; // no door: nothing, and no error
    let fake = Arc::new(FakeDoor { chat: Some(7), ..Default::default() });
    d.st.lock().telegram = Some(fake.clone() as Arc<dyn Inlet>);
    d.tell_owner_on_phone("Chrome asked again").await;
    assert_eq!(*fake.said.lock(), vec![(7, "Chrome asked again".to_string())]);
    let quiet = Arc::new(FakeDoor::default());
    d.st.lock().telegram = Some(quiet.clone() as Arc<dyn Inlet>);
    d.tell_owner_on_phone("nobody to tell").await;
    assert!(quiet.said.lock().is_empty());
}

#[tokio::test]
#[ignore = "needs units daemon and agents (Daemon::assemble, app_hubs, agent_factory)"]
async fn test_the_daemon_builds_no_inlet_when_off() {
    let dir = tempfile::tempdir().unwrap();
    let d = daemon(dir.path(), &[]);
    assert!(d.make_telegram().is_none());
    let st = d.st.lock();
    assert!(st.telegram.is_none() && st.telegram_inlet.is_none() && st.watcher.is_none() && st.agentcard.is_none());
}

#[tokio::test]
#[ignore = "needs unit daemon (DaemonParts::for_test, Daemon::assemble)"]
async fn test_the_default_switch_builds_no_chief_and_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (inlet, _api) = door(vec![OWNER], false);
    let off = daemon(dir.path(), &[("EVE_CHIEF", "off")]);
    assert!(off.make_chief(&inlet, None).is_none());
    let unset = daemon(dir.path(), &[("EVE_CHIEF", "0")]);
    assert!(unset.make_chief(&inlet, None).is_none());
    // (Python's positive control builds a chief on a temporary ledger; the port's ledger folder is the process's
    // EVE_CHIEF_DIR, so building one here would write the owner's real ledger: eve-chief's own tests cover it.)
}

#[tokio::test]
#[ignore = "needs unit daemon (DaemonParts::for_test, Daemon::assemble)"]
async fn the_apps_and_the_direct_apps_are_off_unless_switched_on() {
    let dir = tempfile::tempdir().unwrap();
    let d = daemon(dir.path(), &[]);
    assert!(d.make_apps(&[OWNER].into_iter().collect()).is_none());
    assert!(d.make_direct().is_none());
}

#[tokio::test]
#[ignore = "needs unit daemon (DaemonParts::for_test, Daemon::assemble)"]
async fn canvas_sharing_and_america_are_made_once() {
    let dir = tempfile::tempdir().unwrap();
    let d = daemon(dir.path(), &[]);
    assert!(d.make_canvas().is_none());
    assert!(matches!(d.st.lock().canvas, Some(None)), "tried once, and off");
    assert!(d.make_canvas().is_none());
    assert!(d.make_share().is_none());
    assert!(matches!(d.st.lock().share, Some(None)));
    assert!(d.make_america().is_none());
    assert!(matches!(d.st.lock().america, Some(None)));
    assert!(d.make_america().is_none());
}

#[tokio::test]
#[ignore = "needs unit daemon (DaemonParts::for_test, Daemon::assemble)"]
async fn the_mini_app_stays_off_without_eve_miniapp() {
    let dir = tempfile::tempdir().unwrap();
    let d = daemon(dir.path(), &[]);
    let mut tasks = Vec::new();
    d.start_miniapp(&mut tasks);
    assert!(tasks.is_empty());
    assert!(d.st.lock().miniapp.is_none());
}

#[tokio::test]
#[ignore = "needs unit daemon (DaemonParts::for_test, Daemon::assemble)"]
async fn test_the_daemon_wires_the_phone_picker_on_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let d = daemon(dir.path(), &[]);
    let calls = Arc::new(eve_phone_call::PhoneCalls::new(Arc::new(|_: &str| None), None, None));
    let mut tasks = Vec::new();
    d.start_phone_expressions(&calls, &mut tasks);
    let service = d.st.lock().phone_expressions.clone().expect("the phone's picker");
    assert!(service.enabled());
    assert!(!service.connected(), "no call yet");
    assert!(calls.on_text().is_some());
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].name(), "phone-expressions");
    for t in &tasks {
        t.cancel();
    }
}

#[tokio::test]
#[ignore = "needs unit daemon (DaemonParts::for_test, Daemon::assemble)"]
async fn test_the_daemon_leaves_the_phone_picker_off_when_switched_off() {
    let dir = tempfile::tempdir().unwrap();
    let d = daemon(dir.path(), &[("EVE_PHONE_EXPRESSIONS", "0")]);
    let calls = Arc::new(eve_phone_call::PhoneCalls::new(Arc::new(|_: &str| None), None, None));
    let mut tasks = Vec::new();
    d.start_phone_expressions(&calls, &mut tasks);
    assert!(d.st.lock().phone_expressions.is_none());
    assert!(calls.on_text().is_none());
    assert!(tasks.is_empty());
}
