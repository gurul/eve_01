//! test_telegram.py's apps by code and by the brain (5634-5930), a typed message on an open call (6009) and a file
//! sent once (6621), ported with their names and assertions.
//!
//! `monkeypatch.setattr(Path, "home", ...)` is the inlet's lent HOME (its environ). `app.mod()` is
//! [`eve_brain::seams::app_module`], the real spotify / firetv / obsbot_panel modules; where a test patched
//! `obsbot_panel.enabled` or set EVE_FIRETV, the module is lent with that answer or that environ.

mod more;
mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_brain::prelude::Obj;
use eve_brain::seams::{app_module, AgentFactory, AppHub, AppHubs, AppModule, AskUser, HubCommand, OnEvent, Reader, TaskAgent};
use eve_brain::telegram::{self as telegram, Inbound};
use more::*;
use support::*;

fn line(text: &str) -> (i64, String) {
    (OWNER, text.to_string())
}

fn last_sent(api: &FakeApi) -> (i64, String) {
    api.log.lock().sent.last().cloned().unwrap()
}

fn tool_names(request: &Value) -> Vec<String> {
    request["tools"].as_array().unwrap().iter().filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string)).collect()
}

fn inbound(text: &str, message_id: i64) -> Inbound {
    Inbound { message_id, ..Inbound::new(OWNER, OWNER, text) }
}

fn hubs(slot: &str, hub: Arc<dyn AppHub>) -> AppHubs {
    AppHubs::new(vec![(slot.to_string(), hub)]).unwrap()
}

/// `first["tools"][:len(own)] == own`.
fn starts_with_own_tools(request: &Value) {
    let own: Vec<Value> = telegram::own_tools_in(Some(&conftest_env())).into_iter().map(Value::Object).collect();
    assert_eq!(request["tools"].as_array().unwrap()[..own.len()].to_vec(), own);
}

/// An app's module whose `enabled()` is patched (`monkeypatch.setattr(mod, "enabled", lambda environ=None: on)`).
struct Patched {
    inner: Arc<dyn AppModule>,
    on: bool,
}

impl AppModule for Patched {
    fn has_match(&self) -> bool {
        self.inner.has_match()
    }
    fn module_match(&self, text: &str) -> Option<HubCommand> {
        self.inner.module_match(text)
    }
    fn not_set_up_line(&self) -> Option<String> {
        self.inner.not_set_up_line()
    }
    fn enabled(&self) -> bool {
        self.on
    }
}

// ---- a file a task made ------------------------------------------------------------------------------------------

/// `class PhotoAgent(FakeAgent)`: the task makes the file, and its result names it (and an old file).
struct PhotoAgent {
    photo: PathBuf,
    goal: Mutex<String>,
    note: Mutex<String>,
}

#[async_trait]
impl TaskAgent for PhotoAgent {
    async fn run(&self, goal: &str, note: &str) -> anyhow::Result<String> {
        *self.goal.lock() = goal.to_string();
        *self.note.lock() = note.to_string();
        std::fs::write(&self.photo, b"jpeg").unwrap(); // the task makes the file
        Ok("Took the photo. Saved it as [Eve Photo 2026-09-25.jpg](/x/Documents/Eve%20Photo%202026-09-25.jpg), also \
            ~/Documents/Eve Photo 2026-09-25.jpg. Your old notes are at ~/Documents/old notes.txt."
            .into())
    }
    fn cancel(&self, _reason: &str) {}
    fn steer(&self, _text: &str) -> bool {
        false
    }
    fn running(&self) -> bool {
        false
    }
}

/// `class Api(FakeApi)` whose `send_document` records (name, caption).
fn recording_documents(api: Arc<FakeApi>) -> (Arc<Hooked>, Arc<Mutex<Vec<(String, String)>>>) {
    let sent: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let s = sent.clone();
    let mut hooked = Hooked::new(api);
    hooked.document = Some(Box::new(move |_chat: i64, path: &Path, caption: &str| {
        s.lock().push((path.file_name().unwrap().to_string_lossy().into_owned(), caption.to_string()));
        Ok(())
    }));
    (Arc::new(hooked), sent)
}

fn with_home(home: &Path) -> eve_brain::prelude::Environ {
    let mut env = conftest_env();
    env.insert("HOME".into(), home.to_string_lossy().into_owned());
    env
}

#[tokio::test]
async fn test_a_file_a_task_made_is_sent_with_its_result() {
    // Live, 2026-09-25: a Photo Booth task saved the photo, tried to attach it through a browser, was blocked, and
    // sent only its path. Now the task is told to name the file, and eve sends what it made.
    let _iso = isolated();
    let dir = tmp();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join("Documents")).unwrap();
    let photo = home.join("Documents").join("Eve Photo 2026-09-25.jpg");
    let old = home.join("Documents").join("old notes.txt");
    std::fs::write(&old, "from last year").unwrap();
    std::fs::File::options().write(true).open(&old).unwrap().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)).unwrap();
    let (api, sent) = recording_documents(FakeApi::new(vec![vec![said("take a picture in Photo Booth and send it to me", 1)]]));
    let agents: Arc<Mutex<Vec<Arc<PhotoAgent>>>> = Arc::new(Mutex::new(Vec::new()));
    let (a, p) = (agents.clone(), photo.clone());
    let factory: AgentFactory = Arc::new(move |_on: OnEvent, _ask: AskUser, _floor: Option<String>| {
        let agent = Arc::new(PhotoAgent { photo: p.clone(), goal: Mutex::default(), note: Mutex::default() });
        a.lock().push(agent.clone());
        agent as Arc<dyn TaskAgent>
    });
    let env = with_home(&home);
    let rig = rig_over(
        api,
        FakeCreate::new(vec![call("start_task", json!({"goal": "take a picture in Photo Booth and send it to me"}), "call_1")]),
        move |o| {
            o.agent_factory = Some(factory);
            o.environ = Some(env);
        },
    );
    run_rig(&rig, || async {
        for _ in 0..200 {
            if !sent.lock().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    // once, not the old file
    assert_eq!(*sent.lock(), vec![("Eve Photo 2026-09-25.jpg".to_string(), "From the task: Eve Photo 2026-09-25.jpg".to_string())]);
    assert_eq!(*agents.lock()[0].note.lock(), telegram::TASK_FILE_HINT); // the task was told how files reach the owner
}

#[tokio::test]
async fn test_the_same_file_is_sent_once_even_when_the_task_and_the_turn_both_send_it() {
    // 2026-10-10: a task handed over its photo, then the chat turn that waited on it sent the same photo again.
    let _iso = isolated();
    let dir = tmp();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join("Pictures")).unwrap();
    let photo = home.join("Pictures").join("shot.jpg");
    std::fs::write(&photo, b"\xff\xd8 photo \xff\xd9").unwrap();
    let (api, sent) = recording_documents(FakeApi::new(vec![vec![said("send it", 1)]]));
    let env = with_home(&home);
    let rig = rig_over(api, FakeCreate::new(vec![reply("ok")]), move |o| o.environ = Some(env));
    let a = rig.inlet.tt_send_file(OWNER, "~/Pictures/shot.jpg", "From the task: shot.jpg").await;
    let b = rig.inlet.tt_send_file(OWNER, "~/Pictures/shot.jpg", "").await;
    std::fs::write(&photo, b"\xff\xd8 a new photo \xff\xd9").unwrap(); // changed since: it goes again
    let c = rig.inlet.tt_send_file(OWNER, "~/Pictures/shot.jpg", "again").await;
    let captions: Vec<String> = sent.lock().iter().map(|(_, c)| c.clone()).collect();
    assert_eq!(captions, vec!["From the task: shot.jpg", "again"]);
    assert!(a["ok"] == json!(true) && b["ok"] == json!(true) && b["note"].as_str().unwrap().contains("already sent"));
    assert!(c["ok"] == json!(true) && !c.contains_key("note"));
}

// ---- the lights ---------------------------------------------------------------------------------------------------

type Applied = Arc<Mutex<Vec<(String, eve_lights::Change)>>>;

struct Driver {
    name: String,
    applied: Applied,
}

#[async_trait]
impl eve_lights::Driver for Driver {
    async fn state(&self) -> Result<eve_lights::State, eve_lights::LightError> {
        Ok(eve_lights::State { on: Some(true), brightness: Some(50), rgb: Some([255, 0, 0]), kelvin: None })
    }
    async fn apply(&self, change: &eve_lights::Change) -> Result<(), eve_lights::LightError> {
        self.applied.lock().push((self.name.clone(), change.clone()));
        Ok(())
    }
    async fn close(&self) -> Result<(), eve_lights::LightError> {
        Ok(())
    }
}

/// The daemon's wrap of `lights.Lights` as the chat's hub.
struct LightsHub(eve_lights::Lights);

#[async_trait]
impl AppHub for LightsHub {
    fn tools(&self) -> Vec<Value> {
        self.0.tools()
    }
    fn instructions(&self, _kwargs: &Obj) -> String {
        self.0.instructions()
    }
    async fn handle(&self, name: &str, args: &Value) -> Value {
        self.0.handle(name, args).await
    }
    fn match_command(&self, text: &str) -> Option<HubCommand> {
        self.0.match_command(text).map(|c| HubCommand { action: None, query: None, inner: Arc::new(c) })
    }
    async fn run(&self, cmd: &HubCommand) -> String {
        self.0.run(cmd.inner.downcast_ref::<eve_lights::Command>().unwrap()).await
    }
}

#[tokio::test]
async fn test_lights_by_code_and_by_the_brain() {
    let _iso = isolated();
    let applied: Applied = Arc::new(Mutex::new(Vec::new()));
    let drivers: std::collections::HashMap<String, eve_lights::DriverFactory> = eve_lights::KINDS
        .iter()
        .map(|k| {
            let a = applied.clone();
            let f: eve_lights::DriverFactory = Arc::new(move |lt: &eve_lights::Light| Arc::new(Driver { name: lt.name.clone(), applied: a.clone() }) as Arc<dyn eve_lights::Driver>);
            (k.to_string(), f)
        })
        .collect();
    let hub: Arc<dyn AppHub> = Arc::new(LightsHub(eve_lights::Lights::with(
        vec![eve_lights::Light::new("floor lamp", "govee"), eve_lights::Light::new("strip", "triones")],
        Some(drivers),
        std::time::Duration::from_secs_f64(eve_lights::LIGHT_TIMEOUT_SECS),
    )));
    // "lights blue": every light, by code, no model call
    let api = FakeApi::new(vec![vec![said("lights blue", 1)]]);
    let h = hub.clone();
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), move |o| o.app_hubs = hubs("lights", h));
    run_plain(&rig).await;
    assert_eq!(last_sent(&api), line("Lights blue."));
    let names: Vec<String> = applied.lock().iter().map(|(n, _)| n.clone()).collect();
    assert!(rig.create.requests.lock().is_empty() && names == ["floor lamp", "strip"], "{names:?}");
    // anything more than a command goes to the brain, which has the tools and the list of lights
    applied.lock().clear();
    let h = hub.clone();
    let rig = Rig::new(
        FakeApi::new(vec![]),
        FakeCreate::new(vec![
            call("lights_set", json!({"target": "all", "power": null, "brightness": 30, "color": "warm white"}), "call_1"),
            reply("Cozy."),
        ]),
        config(),
        move |o| o.app_hubs = hubs("lights", h),
    );
    rig.inlet.tt_turn(&inbound("make it cozy in here", 9)).await;
    let first = rig.create.requests.lock()[0].clone();
    let names = tool_names(&first);
    assert!(names.contains(&"lights_set".into()) && names.contains(&"lights_status".into()));
    assert!(first["instructions"].as_str().unwrap().contains("floor lamp"));
    assert_eq!(applied.lock()[0].1, eve_lights::Change { power: None, brightness: Some(30), color: eve_lights::parse_color("warm white") });
    // no lights: no tools, and "lights off" is the brain's like any other text
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![reply("ok")]), config(), |_| {});
    rig.inlet.tt_turn(&inbound("hi", 10)).await;
    let names = tool_names(&rig.create.requests.lock()[0]);
    assert!(!names.iter().any(|n| eve_lights::TOOL_NAMES.contains(&n.as_str())));
}

// ---- Spotify ------------------------------------------------------------------------------------------------------

/// `class Hub` (and `Music`): Spotify's match is the real one; the doing is faked.
struct SpotifyHub {
    ran: Mutex<Vec<eve_spotify::Command>>,
    handled: Mutex<Vec<(String, Value)>>,
    line: &'static str,
}

impl SpotifyHub {
    fn new(line: &'static str) -> Arc<Self> {
        Arc::new(Self { ran: Mutex::default(), handled: Mutex::default(), line })
    }
}

#[async_trait]
impl AppHub for SpotifyHub {
    fn tools(&self) -> Vec<Value> {
        eve_spotify::TOOLS.clone()
    }
    fn instructions(&self, _kwargs: &Obj) -> String {
        eve_spotify::INSTRUCTIONS.to_string()
    }
    async fn handle(&self, name: &str, args: &Value) -> Value {
        self.handled.lock().push((name.to_string(), args.clone()));
        json!({"ok": true, "line": "Playing Daft Punk."})
    }
    fn match_command(&self, text: &str) -> Option<HubCommand> {
        eve_spotify::match_text(text).map(|c| HubCommand { action: Some(c.action.clone()), query: Some(c.query.clone()), inner: Arc::new(c) })
    }
    async fn run(&self, cmd: &HubCommand) -> String {
        self.ran.lock().push(cmd.inner.downcast_ref::<eve_spotify::Command>().unwrap().clone());
        self.line.to_string()
    }
}

#[tokio::test]
async fn test_spotify_by_code_and_by_the_brain() {
    let _iso = isolated();
    let hub = SpotifyHub::new("Paused.");
    // "pause the music": by code, no model call
    let api = FakeApi::new(vec![vec![said("pause the music", 1)]]);
    let h = hub.clone();
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), move |o| o.app_hubs = hubs("spotify", h));
    run_plain(&rig).await;
    assert_eq!(last_sent(&api), line("Paused."));
    assert!(rig.create.requests.lock().is_empty() && *hub.ran.lock() == vec![eve_spotify::Command::new("pause")]);
    // anything more goes to the brain, which has the tools
    let args = json!({"action": "play", "query": "daft punk", "kind": "artist", "volume": null, "volume_change": null, "device": null});
    let h = hub.clone();
    let rig = Rig::new(
        FakeApi::new(vec![]),
        FakeCreate::new(vec![call("spotify_control", args.clone(), "call_1"), reply("On it.")]),
        config(),
        move |o| o.app_hubs = hubs("spotify", h),
    );
    rig.inlet.tt_turn(&inbound("put on some daft punk", 9)).await;
    let first = rig.create.requests.lock()[0].clone();
    let names = tool_names(&first);
    assert!(names.contains(&"spotify_now_playing".into()) && names.contains(&"spotify_control".into()));
    assert!(first["instructions"].as_str().unwrap().contains("spotify_control"));
    assert_eq!(*hub.handled.lock(), vec![("spotify_control".to_string(), args)]);
    // no login: no tools, and "pause the music" is the brain's like any other text
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![reply("ok")]), config(), |_| {});
    rig.inlet.tt_turn(&inbound("hi", 10)).await;
    let names = tool_names(&rig.create.requests.lock()[0]);
    assert!(!names.iter().any(|n| eve_spotify::TOOL_NAMES.contains(&n.as_str())));
}

#[tokio::test]
async fn test_a_music_command_without_a_spotify_login_says_how_to_set_it_up() {
    // "spotify mode" said on the Voice PE with no login went to the model, which had no Spotify tools (2026-09-27)
    let _iso = isolated();
    let api = FakeApi::new(vec![vec![said("spotify mode", 1)]]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), |_| {});
    run_plain(&rig).await;
    assert!(last_sent(&api) == line(eve_spotify::NOT_SET_UP_LINE) && rig.create.requests.lock().is_empty());
    // anything that is not a music command still goes to the brain
    let rig = Rig::new(FakeApi::new(vec![vec![said("what's the plan today", 1)]]), FakeCreate::new(vec![reply("ok")]), config(), |_| {});
    run_plain(&rig).await;
    assert!(!rig.create.requests.lock().is_empty());
}

// ---- the Fire TV --------------------------------------------------------------------------------------------------

/// `_FakeTV`: a paired Fire TV; firetv.match is the real one; nothing is sent to a TV.
struct FakeTv {
    ran: Mutex<Vec<eve_firetv::Command>>,
    handled: Mutex<Vec<(String, Value)>>,
}

impl FakeTv {
    fn new() -> Arc<Self> {
        Arc::new(Self { ran: Mutex::default(), handled: Mutex::default() })
    }
}

#[async_trait]
impl AppHub for FakeTv {
    fn tools(&self) -> Vec<Value> {
        eve_firetv::TOOLS.clone()
    }
    fn instructions(&self, _kwargs: &Obj) -> String {
        eve_firetv::INSTRUCTIONS.clone()
    }
    async fn handle(&self, name: &str, args: &Value) -> Value {
        self.handled.lock().push((name.to_string(), args.clone()));
        json!({"ok": true, "line": "Opened Netflix."})
    }
    fn match_command(&self, text: &str) -> Option<HubCommand> {
        eve_firetv::match_command(text).map(|c| HubCommand { action: Some(c.action.clone()), query: Some(c.query.clone()), inner: Arc::new(c) })
    }
    async fn run(&self, cmd: &HubCommand) -> String {
        self.ran.lock().push(cmd.inner.downcast_ref::<eve_firetv::Command>().unwrap().clone());
        "TV on — YouTube.".into()
    }
    async fn screenshot_for_model(&self) -> Option<Value> {
        Some(json!({"ok": false, "reason": "The TV didn't send a picture."}))
    }
}

/// conftest's environ with `monkeypatch.delenv("EVE_FIRETV")` (or `setenv("EVE_FIRETV", value)`).
fn firetv_env(value: Option<&str>) -> eve_brain::prelude::Environ {
    let mut env = conftest_env();
    env.remove("EVE_FIRETV");
    if let Some(v) = value {
        env.insert("EVE_FIRETV".into(), v.into());
    }
    env
}

#[tokio::test]
async fn test_firetv_by_code_and_by_the_brain() {
    let _iso = isolated();
    let tv = FakeTv::new();
    // "turn on the tv": by code, no model call
    let api = FakeApi::new(vec![vec![said("turn on the tv", 1)]]);
    let t = tv.clone();
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), move |o| {
        o.app_hubs = hubs("firetv", t);
        o.environ = Some(firetv_env(None));
    });
    run_plain(&rig).await;
    assert_eq!(last_sent(&api), line("TV on — YouTube."));
    assert!(rig.create.requests.lock().is_empty() && *tv.ran.lock() == vec![eve_firetv::Command::new("power_on")]);
    // anything more goes to the brain, which has the TV's tools and instructions
    let args = json!({"action": "open_app", "app": "Netflix", "query": null, "text": null, "volume": null, "volume_change": null, "repeat": null});
    let t = tv.clone();
    let rig = Rig::new(
        FakeApi::new(vec![]),
        FakeCreate::new(vec![call("firetv_control", args.clone(), "call_1"), reply("On it.")]),
        config(),
        move |o| {
            o.app_hubs = hubs("firetv", t);
            o.environ = Some(firetv_env(None));
        },
    );
    rig.inlet.tt_turn(&inbound("find me something to watch", 9)).await;
    let first = rig.create.requests.lock()[0].clone();
    let names = tool_names(&first);
    assert!(eve_firetv::TOOL_NAMES.iter().all(|n| names.contains(&n.to_string())));
    starts_with_own_tools(&first); // the TV rides the optional slot
    assert!(first["instructions"].as_str().unwrap().contains("firetv_control"));
    assert_eq!(*tv.handled.lock(), vec![("firetv_control".to_string(), args)]);
    // no paired TV: no TV tools, and a message that is not a TV command is the brain's like any other text
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![reply("ok")]), config(), |_| {});
    rig.inlet.tt_turn(&inbound("hi", 10)).await;
    let request = rig.create.requests.lock()[0].clone();
    assert!(!tool_names(&request).iter().any(|n| eve_firetv::TOOL_NAMES.contains(&n.as_str())));
    assert!(!request["instructions"].as_str().unwrap().contains("firetv_control"));
}

#[tokio::test]
async fn test_a_tv_command_without_a_paired_tv_says_how_to_set_it_up() {
    let _iso = isolated();
    let api = FakeApi::new(vec![vec![said("tv volume 30", 1)]]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), |o| o.environ = Some(firetv_env(None)));
    run_plain(&rig).await;
    assert!(last_sent(&api) == line(eve_firetv::NOT_SET_UP_LINE) && rig.create.requests.lock().is_empty());
    // switched off on purpose: no set-up nag, the brain answers
    let api = FakeApi::new(vec![vec![said("tv volume 30", 1)]]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![reply("ok")]), config(), |o| o.environ = Some(firetv_env(Some("0"))));
    run_plain(&rig).await;
    assert!(!rig.create.requests.lock().is_empty() && last_sent(&api) == line("ok"));
}

#[tokio::test]
async fn test_music_on_the_tv_is_still_spotify_and_a_bare_pause_is_not_the_tv() {
    let _iso = isolated();
    let (tv, music) = (FakeTv::new(), SpotifyHub::new("Moved the music to the TV."));
    let api = FakeApi::new(vec![vec![said("play music on the tv", 1)]]);
    let (t, m) = (tv.clone(), music.clone());
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), move |o| {
        o.app_hubs = AppHubs::new(vec![("firetv".to_string(), t as Arc<dyn AppHub>), ("spotify".to_string(), m as Arc<dyn AppHub>)]).unwrap();
        o.environ = Some(firetv_env(None));
    });
    run_plain(&rig).await;
    assert_eq!(last_sent(&api), line("Moved the music to the TV."));
    let transfer = eve_spotify::Command { device: "tv".into(), ..eve_spotify::Command::new("transfer") };
    assert!(tv.ran.lock().is_empty() && *music.ran.lock() == vec![transfer] && rig.create.requests.lock().is_empty());
    // "pause" names no TV: the brain's, never the TV's
    let t = tv.clone();
    let rig = Rig::new(FakeApi::new(vec![vec![said("pause", 1)]]), FakeCreate::new(vec![reply("ok")]), config(), move |o| {
        o.app_hubs = hubs("firetv", t);
        o.environ = Some(firetv_env(None));
    });
    run_plain(&rig).await;
    assert!(tv.ran.lock().is_empty() && !rig.create.requests.lock().is_empty());
}

// ---- the camera ---------------------------------------------------------------------------------------------------

/// `_FakeCamera`: obsbot_panel.match is the real one; nothing reaches a camera.
struct FakeCamera {
    ran: Mutex<Vec<eve_obsbot_panel::Command>>,
    handled: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl AppHub for FakeCamera {
    fn tools(&self) -> Vec<Value> {
        eve_obsbot_panel::TOOLS.clone()
    }
    fn instructions(&self, _kwargs: &Obj) -> String {
        eve_obsbot_panel::INSTRUCTIONS.clone()
    }
    async fn handle(&self, name: &str, args: &Value) -> Value {
        self.handled.lock().push((name.to_string(), args.clone()));
        json!({"ok": true, "pan": 30, "tilt": 0, "line": "Camera pointing 30° left."})
    }
    async fn run(&self, cmd: &HubCommand) -> String {
        self.ran.lock().push(cmd.inner.downcast_ref::<eve_obsbot_panel::Command>().unwrap().clone());
        "Camera pointing 20° left.".into()
    }
}

/// `obsbot_panel` with `enabled` patched to `on`.
fn obsbot_module(on: bool) -> Vec<(String, Arc<dyn AppModule>)> {
    vec![("obsbot".to_string(), Arc::new(Patched { inner: app_module("obsbot", None).unwrap(), on }) as Arc<dyn AppModule>)]
}

#[tokio::test]
async fn test_camera_by_code_and_by_the_brain() {
    let _iso = isolated();
    let cam = Arc::new(FakeCamera { ran: Mutex::default(), handled: Mutex::default() });
    // "camera left": by code, no model call
    let api = FakeApi::new(vec![vec![said("camera left", 1)]]);
    let c = cam.clone();
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), move |o| o.app_hubs = hubs("obsbot", c));
    run_plain(&rig).await;
    assert_eq!(last_sent(&api), line("Camera pointing 20° left."));
    assert!(rig.create.requests.lock().is_empty() && *cam.ran.lock() == vec![eve_obsbot_panel::Command::turn("left", 20)]);
    // anything more goes to the brain, which has the camera's tools and instructions
    let args = json!({"action": "point", "direction": null, "degrees": null, "pan": 30, "tilt": null, "zoom": null,
                      "zoom_change": null, "mode": null, "preset": null});
    let c = cam.clone();
    let rig = Rig::new(
        FakeApi::new(vec![]),
        FakeCreate::new(vec![call("camera_control", args.clone(), "call_1"), reply("Done.")]),
        config(),
        move |o| o.app_hubs = hubs("obsbot", c),
    );
    rig.inlet.tt_turn(&inbound("aim the webcam over toward the window", 9)).await;
    let first = rig.create.requests.lock()[0].clone();
    let names = tool_names(&first);
    assert!(eve_obsbot_panel::TOOL_NAMES.iter().all(|n| names.contains(&n.to_string())));
    starts_with_own_tools(&first); // the camera rides the optional slot
    assert!(first["instructions"].as_str().unwrap().contains("camera_control"));
    assert_eq!(*cam.handled.lock(), vec![("camera_control".to_string(), args)]);
    // no camera: no camera tools
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![reply("ok")]), config(), |_| {});
    rig.inlet.tt_turn(&inbound("hi", 10)).await;
    let names = tool_names(&rig.create.requests.lock()[0]);
    assert!(!names.iter().any(|n| eve_obsbot_panel::TOOL_NAMES.contains(&n.as_str())));
}

#[tokio::test]
async fn test_a_camera_command_without_a_camera() {
    // a camera on the USB bus but no hub (it failed to load): say how to set it up, no model turn
    let _iso = isolated();
    let api = FakeApi::new(vec![vec![said("camera left", 1)]]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), |o| o.app_hubs = AppHubs::default().with_modules(obsbot_module(true)));
    run_plain(&rig).await;
    assert!(last_sent(&api) == line(eve_obsbot_panel::NOT_SET_UP_LINE) && rig.create.requests.lock().is_empty());
    // no camera at all (or EVE_OBSBOT=0): the brain answers
    let api = FakeApi::new(vec![vec![said("camera left", 1)]]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![reply("ok")]), config(), |o| o.app_hubs = AppHubs::default().with_modules(obsbot_module(false)));
    run_plain(&rig).await;
    assert!(!rig.create.requests.lock().is_empty() && last_sent(&api) == line("ok"));
}

// ---- an open call -------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_typed_message_is_not_read_aloud_on_an_open_call() {
    // owner, 2026-10-04: "dont read aloud things from telegram unless its something explicit from like the action
    // call". A call is open (it took the listener), the owner types: the answer is texted, never spoken.
    let _iso = isolated();
    let api = FakeApi::new(vec![]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![reply("ok"), reply("Typed answer."), reply("Spoken answer.")]), config(), |_| {}); // "ok": the warm-up
    rig.inlet.tt_set_chat_id(Some(OWNER));
    let read: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let r = read.clone();
    rig.inlet.listen(Some(Reader::new(move |text: &str| r.lock().push(text.to_string()))), None);
    settle(200).await;
    rig.inlet.tt_handle(&Inbound::new(OWNER, OWNER, "what should I cook tonight")); // typed
    settle(200).await;
    assert!(read.lock().is_empty());
    assert!(api.texts().iter().any(|t| t.contains("Typed answer.")));
    rig.inlet.hear("and tomorrow", eve_phone_call::Captured::Uncaptured, true, None, None, "auto", &[]); // said on the call
    settle(200).await;
    assert_eq!(*read.lock(), vec!["Spoken answer."]);
}
