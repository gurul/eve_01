//! Tests for the agent factory and the apps: the agent factory's pieces (`_warn_removed_computer`, `_bodies`' router, the
//! Chrome lane's rule, the reflex's forwarding to the body that ran) and eve's apps' modules and switches. The
//! Python tests these port: test_telegram_chief.py (the Holo warning, the chief's Codex floor), test_web_reader.py
//! (the daemon's wiring), test_chrome_lane.py (the daemon's wiring), test_codex_computer.py (the daemon factory),
//! test_desk_lane.py (one lane, the switch), test_app_registry.py (an app switched off is gone).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_app_reflex::{Agent as ReflexAgent, MakeAgent, ReflexFirstAgent, ReflexOptions, RouteBody};
use eve_brain::seams::{AppModule, TaskAgent};
use eve_daemon::agents::{
    chrome_route, route_bodies, warn_removed_computer, Bodies, FloorAgent, MakeInner, ReaderRoute, ReflexTask,
    TaskBody,
};
use eve_daemon::apps::{make_hub, AppModuleOf};
use eve_daemon::seams::{AskUser, BoardLink, OnEvent};

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

// ---- a stand-in for Codex -------------------------------------------------------------------------------------------

#[derive(Default)]
struct Codex {
    goals: Mutex<Vec<String>>,
    discarded: Mutex<bool>,
    used: bool,
}

#[async_trait]
impl TaskAgent for Codex {
    async fn run(&self, goal: &str, note: &str) -> anyhow::Result<String> {
        self.goals.lock().push(format!("{goal}{note}"));
        Ok("done".into())
    }
    fn cancel(&self, _reason: &str) {}
    fn steer(&self, _text: &str) -> bool {
        true
    }
    fn status(&self) -> Value {
        json!({"running": false, "provider": "codex"})
    }
    fn provider(&self) -> String {
        "codex".into()
    }
    fn browser_used(&self) -> bool {
        self.used
    }
    fn browser_screenshot(&self) -> Option<(Vec<u8>, String)> {
        self.used.then(|| (vec![1, 2, 3], ".png".to_string()))
    }
    fn browser_tab_id(&self) -> String {
        "7".into()
    }
}

#[async_trait]
impl FloorAgent for Codex {
    async fn discard(&self) {
        *self.discarded.lock() = true;
    }
}

fn no_events() -> OnEvent {
    Arc::new(|_e| {})
}

fn no_ask() -> AskUser {
    Arc::new(|_q| Box::pin(async { Ok(String::new()) }))
}

// ---- _warn_removed_computer ------------------------------------------------------------------------------------------

#[test]
fn a_leftover_holo_setting_warns_and_anything_else_does_not() {
    assert!(warn_removed_computer(Some(&env(&[("EVE_COMPUTER", "holo")]))));
    assert!(warn_removed_computer(Some(&env(&[("EVE_COMPUTER", "  HOLO ")]))));
    assert!(!warn_removed_computer(Some(&env(&[("EVE_COMPUTER", "codex")])))); // control
    assert!(!warn_removed_computer(Some(&env(&[]))));
}

// ---- _bodies: Jev first, then the Chrome lane's rule --------------------------------------------------------------

fn web_body() -> MakeAgent {
    let codex: Arc<dyn FloorAgent> = Arc::new(Codex::default());
    Arc::new(move || Arc::new(TaskBody(codex.clone())) as Arc<dyn ReflexAgent>)
}

#[tokio::test]
async fn the_web_reader_takes_what_jev_says_is_the_webs_and_the_chrome_rule_the_rest() {
    let jev: ReaderRoute = Arc::new(|goal: &str| match goal {
        "weather in denver" => Ok("web".to_string()),
        "check my gmail" => Ok("codex".to_string()),
        _ => Err("RuntimeError".to_string()),
    });
    let chrome_route: RouteBody = Arc::new(|_goal: String| Box::pin(async { Ok("chrome".to_string()) }));
    let chrome = Bodies { make_auto: Some(web_body()), route_body: Some(chrome_route), bodies: HashMap::new() };
    let wiring = route_bodies(chrome, Some(jev), web_body());
    let route = wiring.route_body.clone().expect("a router");
    assert_eq!(route("weather in denver".into()).await.unwrap(), "web");
    // Jev said owner: the Chrome rule decides
    assert_eq!(route("check my gmail".into()).await.unwrap(), "chrome");
    // Jev failed: as if it had said owner
    assert_eq!(route("boom".into()).await.unwrap(), "chrome");
    assert!(wiring.bodies.contains_key("web"));
    assert!(wiring.make_auto.is_some());
}

#[tokio::test]
async fn without_a_jev_route_the_chrome_wiring_is_the_whole_answer() {
    let off = route_bodies(Bodies::default(), None, web_body());
    assert!(off.is_empty() && !off.bodies.contains_key("web"));
    // no Chrome lane, Jev on: Jev's "codex" stays Codex
    let jev: ReaderRoute = Arc::new(|_goal: &str| Ok("codex".to_string()));
    let wiring = route_bodies(Bodies::default(), Some(jev), web_body());
    assert_eq!(wiring.route_body.unwrap()("check my gmail".into()).await.unwrap(), "codex");
}

#[tokio::test]
async fn with_the_lane_on_web_goals_take_the_chrome_body_and_the_rest_stay_codex() {
    let route = chrome_route();
    assert_eq!(route("how many unread emails are in my gmail inbox".into()).await.unwrap(), "chrome");
    assert_eq!(route("search amazon for AA batteries".into()).await.unwrap(), "chrome");
    assert_eq!(route("open spotify and play jazz".into()).await.unwrap(), "codex");
    assert_eq!(route("put the calendar on year view".into()).await.unwrap(), "codex");
}

// ---- the reflex as the doors' task -----------------------------------------------------------------------------------

fn floor_reflex(codex: Arc<Codex>) -> ReflexTask {
    let inner: MakeInner = Arc::new(move || codex.clone() as Arc<dyn FloorAgent>);
    let make: MakeAgent = Arc::new(move || Arc::new(TaskBody(inner())) as Arc<dyn ReflexAgent>);
    let options = ReflexOptions { enabled: false, ..ReflexOptions::default() };
    ReflexTask(Arc::new(ReflexFirstAgent::new(make, Arc::new(|_e| {}), options)))
}

#[tokio::test]
async fn the_codex_floor_gets_the_goal_and_the_note_whole() {
    let codex = Arc::new(Codex { used: true, ..Codex::default() });
    let agent = floor_reflex(codex.clone());
    assert_eq!(agent.provider(), "codex");
    // before the handoff there is no body to ask
    assert!(!agent.browser_used() && agent.browser_screenshot().is_none() && agent.browser_tab_id().is_empty());
    assert_eq!(agent.run("open the report", " (the desk lane did step one)").await.unwrap(), "done");
    assert_eq!(codex.goals.lock().clone(), vec!["open the report (the desk lane did step one)".to_string()]);
    // what Python's __getattr__ reached on Codex, once it took the task
    assert!(agent.browser_used());
    assert_eq!(agent.browser_screenshot(), Some((vec![1, 2, 3], ".png".to_string())));
    assert_eq!(agent.browser_tab_id(), "7");
    assert!(agent.browser_shot().await.is_none()); // Codex has no lane picture of its own
    assert!(agent.steer("use 3+3 instead"));
}

#[tokio::test]
async fn a_task_body_closes_its_floor_when_discarded() {
    let codex = Arc::new(Codex::default());
    let body = TaskBody(codex.clone());
    body.discard().await.unwrap();
    assert!(*codex.discarded.lock());
    assert_eq!(ReflexAgent::status(&body).get("provider"), Some(&json!("codex")));
    assert_eq!(ReflexAgent::provider(&body), "codex");
}

// ---- eve's apps: their modules and their switches ----------------------------------------------------------------------

#[test]
fn an_app_switched_off_is_gone_everywhere() {
    for app in eve_app_registry::APPS.iter() {
        let off = env(&[(app.switch.as_str(), "0")]);
        // the daemon makes no hub, so no site gets one
        assert!(make_hub(&app.slot, Some(&off)).is_none(), "{}", app.slot);
        let module = AppModuleOf { slot: app.slot.clone(), environ: Some(off) };
        assert!(!module.enabled(), "{}", app.slot);
    }
}

#[test]
fn an_apps_module_matches_without_a_hub_and_says_it_is_not_set_up() {
    let on = |slot: &str| AppModuleOf { slot: slot.to_string(), environ: Some(env(&[])) };
    let spotify = on("spotify");
    assert!(spotify.has_match());
    let cmd = spotify.module_match("pause the music").expect("a music command");
    assert_eq!(cmd.action.as_deref(), Some("pause"));
    assert_eq!(spotify.not_set_up_line().as_deref(), Some(eve_spotify::NOT_SET_UP_LINE));
    let tv = on("firetv");
    assert!(tv.has_match() && tv.module_match("turn on the tv").is_some());
    assert_eq!(tv.not_set_up_line().as_deref(), Some(eve_firetv::NOT_SET_UP_LINE));
    let camera = on("obsbot");
    assert_eq!(camera.not_set_up_line().as_deref(), Some(eve_obsbot_panel::NOT_SET_UP_LINE));
    // the lights match their names on the hub; the car has no code path
    for slot in ["lights", "pika"] {
        let m = on(slot);
        assert!(!m.has_match() && m.module_match("turn on the lamp").is_none() && m.not_set_up_line().is_none());
    }
    // the car is off unless switched on (default_on=False)
    assert!(!on("pika").enabled() && on("lights").enabled());
}

// ---- the daemon's own wiring (needs the daemon's construction, another unit) -------------------------------------------

struct Ble;

#[async_trait]
impl BoardLink for Ble {
    fn connected(&self) -> bool {
        false
    }
    async fn wait_connected(&self) {}
    async fn send(&self, _obj: &Value, _codec: Option<&str>) -> bool {
        true
    }
    async fn run(&self) {}
    async fn stop(&self) {}
}

fn daemon(dir: &std::path::Path, environ: HashMap<String, String>) -> Arc<eve_daemon::Daemon> {
    let mut parts = eve_daemon::DaemonParts::for_test(Arc::new(Ble), dir);
    parts.environ = Some(environ);
    eve_daemon::Daemon::assemble(parts)
}

#[tokio::test]
async fn the_daemon_factory_routes_the_shared_task_contract_to_codex() {
    let dir = tempfile::tempdir().unwrap();
    let d = daemon(dir.path(), env(&[("EVE_DESK_LANE", "0")]));
    let agent = d.make_agent(no_events(), no_ask(), None);
    // Codex behind the launch reflex (app_reflex.py): built on handoff, with Codex's contract
    assert_eq!(agent.provider(), "codex");
    assert!(!agent.running());
    let active = d.st.lock().active_agent.clone().expect("remembered");
    assert!(Arc::ptr_eq(&active, &agent));
    let floor = d.make_agent(no_events(), no_ask(), Some("codex".into()));
    assert_eq!(floor.provider(), "codex");
}

#[tokio::test]
async fn the_daemon_makes_one_lane_and_the_switch_turns_it_off() {
    let dir = tempfile::tempdir().unwrap();
    assert!(daemon(dir.path(), env(&[("EVE_DESK_LANE", "0")])).desk().is_none());
    let d = daemon(dir.path(), env(&[("EVE_DESK_LANE", "1")]));
    let (first, second) = (d.desk().unwrap(), d.desk().unwrap());
    assert!(Arc::ptr_eq(&first, &second));
}

#[tokio::test]
async fn with_the_lane_off_nothing_changes() {
    let dir = tempfile::tempdir().unwrap();
    let d = daemon(dir.path(), env(&[]));
    let codex: MakeInner = Arc::new(|| Arc::new(Codex::default()) as Arc<dyn FloorAgent>);
    assert!(d.chrome_body(codex.clone(), no_events(), no_ask()).is_empty());
    // no lane and no Jev route: every task stays Codex's
    assert!(d.bodies(codex, no_events(), no_ask()).is_empty());
}

#[tokio::test]
async fn an_app_switched_off_makes_no_hub_once() {
    let dir = tempfile::tempdir().unwrap();
    for app in eve_app_registry::APPS.iter() {
        let d = daemon(dir.path(), env(&[(app.switch.as_str(), "0")]));
        assert!(d.app_hub(app).is_none());
        // made once: the slot is kept, with no hub
        assert!(matches!(d.st.lock().hubs.get(&app.slot), Some(None)));
        assert!(d.app_hubs(false).get(&app.slot).is_none());
    }
}
