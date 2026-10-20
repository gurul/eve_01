//! The daemon's seams: the duck-typed objects Python's tests stand in for (`self.ble`, `self._telegram`,
//! `self._ears`, `self._learning_server`, `self._voice_session`, an app's hub) as traits, and the module functions
//! the tests patch (`voice_agent.open_session`, `focus_terminal.focus_session_terminal`, `key_tap.KeyTapper`) as
//! [`Seams`]. The real implementations live beside them, or in the file that owns the real object
//! ([`crate::link`] for the board, [`crate::inlet`] for the text door, [`crate::apps`] for the hubs).
//!
//! Architect-owned: no fill unit edits this file.

use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::{Map, Value};

pub use eve_brain::seams::{AgentFactory, AskUser, Job, OnEvent, OnState, TaskAgent, Thinker};

/// A JSON object (Python's `dict[str, Any]`), key order kept.
pub type Obj = Map<String, Value>;

/// The environment a call reads: `None` is the process's (Python's `os.environ`).
pub type Environ = std::collections::HashMap<String, String>;

/// `env.get(key)`: from the lent mapping, else the process environment.
pub fn env_get(environ: Option<&Environ>, key: &str) -> Option<String> {
    match environ {
        Some(map) => map.get(key).cloned(),
        None => std::env::var(key).ok(),
    }
}

// ---- self.ble: the robot's link -------------------------------------------------------------------------------

/// The robot's link as the daemon uses it: `eve_ble::EveBle`, `eve_serial_transport::EveSerial` (with the Voice
/// PE's tee), or a test's `_Ble`. Python duck-typed `self.ble`; `getattr(self.ble, "pulse_reset", None)` and the
/// like are the defaults here.
#[async_trait]
pub trait BoardLink: Send + Sync {
    /// `ble.connected`.
    fn connected(&self) -> bool;
    /// `await ble.wait_connected()`.
    async fn wait_connected(&self);
    /// `await ble.send(obj, codec=codec)`: false when the write failed.
    async fn send(&self, obj: &Value, codec: Option<&str>) -> bool;
    /// `await ble.run()`: the link's own reconnect loop, until stopped.
    async fn run(&self);
    /// `await ble.stop()`.
    async fn stop(&self);
    /// `getattr(ble, "wired", False)`: a USB cable, where the board's BLE "sec" flag means nothing.
    fn wired(&self) -> bool {
        false
    }
    /// `ble.pulse_reset(why)` when the link has one (serial: RTS reboots the board) → true; false: it has none.
    fn pulse_reset(&self, _why: &str) -> bool {
        false
    }
    /// `ble.force_reconnect(why)` when the link has one → true; false: it has none.
    fn force_reconnect(&self, _why: &str) -> bool {
        false
    }
}

/// `self.ble.send` as the head, the tracker and the follower take it (`send(obj) -> bool`).
pub fn link_sender(link: Arc<dyn BoardLink>) -> Arc<dyn Fn(Value) -> BoxFuture<'static, bool> + Send + Sync> {
    Arc::new(move |obj: Value| {
        let link = link.clone();
        Box::pin(async move { link.send(&obj, None).await })
    })
}

// ---- self._ears: the Mac microphone ---------------------------------------------------------------------------

/// The wake word's microphone (`ears.Ears`) as the daemon uses it; the daemon tests lend a bare object.
pub trait Mic: Send + Sync {
    /// `ears.listening` (Python's tests: `getattr(ears, "listening", True)`).
    fn listening(&self) -> bool;
    /// `ears.prepare()`: load the model; false when it cannot.
    fn prepare(&self) -> bool;
    /// `ears.start()`: open the microphone; false when it did not open.
    fn start(&self) -> bool;
    /// `ears.stop()`.
    fn stop(&self);
    /// `ears.subscribe()`: a queue of the microphone's blocks.
    fn subscribe(&self) -> eve_ears::PcmQueue;
    /// `ears.unsubscribe(q)`.
    fn unsubscribe(&self, q: &eve_ears::PcmQueue);
    /// `ears.last_wake_audio`, then `ears.last_wake_audio = b""`: one conversation's enrolment.
    fn take_last_wake_audio(&self) -> Vec<u8>;
    /// The real `Ears`, for what takes it whole (RoomNotes); None for a test's stand-in.
    fn ears(&self) -> Option<Arc<eve_ears::Ears>> {
        None
    }
}

/// The real microphone.
pub struct EarsMic(pub Arc<eve_ears::Ears>);

impl Mic for EarsMic {
    fn listening(&self) -> bool {
        self.0.listening()
    }
    fn prepare(&self) -> bool {
        self.0.prepare()
    }
    fn start(&self) -> bool {
        self.0.start()
    }
    fn stop(&self) {
        self.0.stop()
    }
    fn subscribe(&self) -> eve_ears::PcmQueue {
        self.0.subscribe()
    }
    fn unsubscribe(&self, q: &eve_ears::PcmQueue) {
        self.0.unsubscribe(q)
    }
    fn take_last_wake_audio(&self) -> Vec<u8> {
        self.0.take_last_wake_audio()
    }
    fn ears(&self) -> Option<Arc<eve_ears::Ears>> {
        Some(self.0.clone())
    }
}

// ---- self._telegram: the text door ----------------------------------------------------------------------------

/// The text door (`telegram.TelegramInlet`) as the daemon uses it: the relay of Claude Code's hooks, the owner's
/// questions, the chat window's typed words and the Work sessions' tools. The real one is in [`crate::inlet`]; the
/// IPC tests lend fakes.
#[async_trait]
pub trait Inlet: Send + Sync {
    /// `inlet.claude`: the owner said "claude on" (the relay).
    fn claude(&self) -> bool;
    /// `inlet.task_running`.
    fn task_running(&self) -> bool;
    /// `inlet._chat_id`: the owner's chat, None until the owner has texted.
    fn chat_id(&self) -> Option<i64>;
    /// `inlet._awaiting_answer`.
    fn awaiting_answer(&self) -> bool;
    /// `inlet.config.ask_timeout_secs`.
    fn ask_timeout_secs(&self) -> f64;
    /// `inlet.config.ask_permissions` (EVE_TELEGRAM_ASK=1).
    fn ask_permissions(&self) -> bool;
    /// `inlet._conv`: the open text conversation's transcript id.
    fn conversation(&self) -> Option<String>;
    /// `inlet._conv = None`: closed by the daemon at shutdown, so a late close does not close it twice.
    fn forget_conversation(&self);
    /// `inlet.chief`.
    fn chief(&self) -> Option<Arc<eve_chief::Chief>>;
    /// `inlet._purchases.shutdown()` when it has purchases.
    fn shutdown_purchases(&self);
    /// `await inlet._ask_user(question, chat_id, title=title)`.
    async fn ask_user(&self, question: &str, chat_id: i64, title: &str) -> String;
    /// `await inlet._say(chat_id, text)`.
    async fn say(&self, chat_id: i64, text: &str);
    /// `await inlet.tell_owner(text, about=about, title=title)`.
    async fn tell_owner(&self, text: &str, about: &str, title: &str) -> bool;
    /// `inlet.relay_session_started(cwd)`.
    fn relay_session_started(&self, cwd: &str);
    /// `inlet.relay_turn_began(prompt, cwd)`.
    fn relay_turn_began(&self, prompt: &str, cwd: &str);
    /// `inlet.relay_turn_ended(cwd)`.
    fn relay_turn_ended(&self, cwd: &str);
    /// `await inlet.relay_notification(kind, message, waits)`.
    async fn relay_notification(&self, kind: &str, message: &str, waits: bool);
    /// `await inlet.relay_text(text, cwd)`.
    async fn relay_text(&self, text: &str, cwd: &str);
    /// `inlet.relay_tool_call(tool, hint)`.
    fn relay_tool_call(&self, tool: &str, hint: &str);
    /// `await inlet.decide_permission(tool, hint, cwd, always=always)`: "allow", "deny", or None (no answer).
    async fn decide_permission(&self, tool: &str, hint: &str, cwd: &str, always: bool) -> Option<String>;
    /// `inlet.hear(text, on_call=on_call)`: typed words handled as a text from the owner.
    fn hear(&self, text: &str, on_call: bool);
    /// `inlet.session_tools()`: the chat brain's tools for a Work session's MCP server, and their names.
    fn session_tools(&self) -> anyhow::Result<(Vec<Obj>, Vec<String>)>;
    /// `await inlet.session_call(name, args, sid=sid)`: (result, outside).
    async fn session_call(&self, name: &str, args: &Value, sid: &str) -> anyhow::Result<(Obj, String)>;
}

// ---- self._learning_server: the lesson workspace ----------------------------------------------------------------

/// The learning server (`learning.server.start(...)`) as the daemon uses it: `server.app.voice`,
/// `server.app.append_spoken`, `server.app.url`, the active lesson's summary, and shutdown. The daemon's tests lend
/// `SimpleNamespace(app=SimpleNamespace(voice=fake))` or a real demo `LearningApp`.
pub trait Learning: Send + Sync {
    /// `server.app.voice(**request)`.
    fn voice(&self, req: &eve_learning::LessonRequest) -> Result<Obj, eve_learning::LearningError>;
    /// `server.app.append_spoken(lesson_id, text)`.
    fn append_spoken(&self, lesson_id: &str, text: &str) -> Result<bool, eve_learning::LearningError>;
    /// `server.app.url`.
    fn url(&self) -> String;
    /// `store.summary(store.get(app.active))` for the active lesson; None when there is none (or it is gone).
    fn active_summary(&self) -> Option<Obj>;
    /// `server.shutdown(); server.server_close()`.
    fn shutdown(&self);
}

impl Learning for eve_learning::LearningServer {
    fn voice(&self, req: &eve_learning::LessonRequest) -> Result<Obj, eve_learning::LearningError> {
        self.app.voice(req)
    }
    fn append_spoken(&self, lesson_id: &str, text: &str) -> Result<bool, eve_learning::LearningError> {
        self.app.append_spoken(lesson_id, text)
    }
    fn url(&self) -> String {
        self.app.url()
    }
    fn active_summary(&self) -> Option<Obj> {
        let active = self.app.active()?;
        let lesson = self.app.store.get(&active).ok()?;
        eve_learning::Store::summary(&lesson).ok()
    }
    fn shutdown(&self) {
        eve_learning::LearningServer::shutdown(self)
    }
}

// ---- self._voice_session: the open conversation -----------------------------------------------------------------

/// The open voice conversation (`voice_agent.VoiceSession`) as think out loud drives it.
#[async_trait]
pub trait VoiceSessionHandle: Send + Sync {
    /// `session.ended`.
    fn ended(&self) -> bool;
    /// `session.think_aloud_lesson_id`.
    fn think_aloud_lesson_id(&self) -> Option<String>;
    /// `await session.enter_think_aloud(lesson)`.
    async fn enter_think_aloud(&self, lesson: &Obj) -> Obj;
    /// `session.prepare_think_aloud(lesson)`.
    fn prepare_think_aloud(&self, lesson: &Obj);
    /// `session.stop_think_aloud(reason)`.
    fn stop_think_aloud(&self, reason: &str);
}

#[async_trait]
impl VoiceSessionHandle for Arc<eve_brain::voice_agent::VoiceSession> {
    fn ended(&self) -> bool {
        eve_brain::voice_agent::VoiceSession::ended(self)
    }
    fn think_aloud_lesson_id(&self) -> Option<String> {
        eve_brain::voice_agent::VoiceSession::think_aloud_lesson_id(self)
    }
    async fn enter_think_aloud(&self, lesson: &Obj) -> Obj {
        eve_brain::voice_agent::VoiceSession::enter_think_aloud(self, lesson).await
    }
    fn prepare_think_aloud(&self, lesson: &Obj) {
        eve_brain::voice_agent::VoiceSession::prepare_think_aloud(self, lesson)
    }
    fn stop_think_aloud(&self, reason: &str) {
        eve_brain::voice_agent::VoiceSession::stop_think_aloud(self, reason)
    }
}

// ---- self._keys: swipe-down → Enter ---------------------------------------------------------------------------

/// `key_tap.KeyTapper` as the board's swipe uses it.
pub trait KeyTap: Send {
    /// `keys.tap(name)`.
    fn tap(&mut self, name: &str) -> bool;
}

impl KeyTap for eve_key_tap::KeyTapper {
    fn tap(&mut self, name: &str) -> bool {
        eve_key_tap::KeyTapper::tap(self, name)
    }
}

// ---- eve's apps' hubs (app_registry.APPS) ---------------------------------------------------------------------

/// One of eve's apps, made once (`Daemon._app_hub`) and shared by the chat, the voice, the Mini App and Eve Link:
/// each face the app shows, and its close. [`crate::apps`] makes the real ones.
#[async_trait]
pub trait DaemonHub: Send + Sync {
    /// `app.slot`: "lights", "spotify", "firetv", "obsbot", "pika".
    fn slot(&self) -> String;
    /// The hub as the chat and the voice use it (their `**app_hubs`); None for an app that gives no tools.
    fn app_hub(&self) -> Option<Arc<dyn eve_brain::seams::AppHub>>;
    /// The hub as the Mini App's card takes it (`setattr(server, slot, hub)`): lights, Spotify, the TV.
    fn link_hub(&self) -> Option<Arc<dyn eve_link::seams::Hub>> {
        None
    }
    /// The hub as the Mini App's panel takes it: the car and the camera.
    fn panel(&self) -> Option<Arc<dyn eve_brain::seams::PanelCard>> {
        None
    }
    /// `await hub.close()`: a held Bluetooth light goes back to the phone app, the car stops, the picture ends.
    async fn close(&self);
}

// ---- the command-risk gate (typed_ask.py) ---------------------------------------------------------------------

/// `typed_ask.make_jev_command_asker(...)`: (tool_name, hint, cwd) → the verdict. Blocking: run off the loop.
pub type CommandAsker = Arc<dyn Fn(&str, &str, &str) -> eve_typed_ask::CommandVerdict + Send + Sync>;

/// `typed_ask.make_jev_head_asker(...)`, made async: the voice's `head_pose`.
pub type HeadAsker = eve_brain::seams::HeadPoseFn;

// ---- the module functions the tests patch ----------------------------------------------------------------------

/// `voice_agent.open_session(mic, on_state, agent_factory, on_open=..., **options)`: one conversation, to its end.
/// An error is the conversation failing (Python's exception); a hush is the future being dropped.
pub type OpenSession = Arc<
    dyn Fn(
            eve_ears::PcmQueue,
            OnState,
            AgentFactory,
            Option<eve_brain::voice_agent::OnOpenSession>,
            eve_brain::voice_agent::VoiceSessionOptions,
        ) -> BoxFuture<'static, anyhow::Result<()>>
        + Send
        + Sync,
>;

/// `focus_terminal.focus_session_terminal(cwd)`.
pub type FocusTerminal = Arc<dyn Fn(String) -> BoxFuture<'static, ()> + Send + Sync>;

/// `key_tap.KeyTapper()`.
pub type KeyTapperFactory = Arc<dyn Fn() -> Box<dyn KeyTap> + Send + Sync>;

/// What Python's tests monkeypatch at module level, as values the daemon carries. [`Seams::default`] is the real
/// module functions.
#[derive(Clone)]
pub struct Seams {
    pub open_session: OpenSession,
    pub focus_terminal: FocusTerminal,
    pub key_tapper: KeyTapperFactory,
}

impl Default for Seams {
    fn default() -> Self {
        Seams {
            open_session: Arc::new(|mic, on_state, agent_factory, on_open, options| {
                Box::pin(async move {
                    eve_brain::voice_agent::open_session(mic, on_state, agent_factory, None, None, on_open, options)
                        .await;
                    Ok(())
                })
            }),
            focus_terminal: Arc::new(|cwd: String| {
                Box::pin(async move { eve_focus_terminal::focus_session_terminal(&cwd).await })
            }),
            key_tapper: Arc::new(|| {
                Box::new(eve_key_tap::KeyTapper::new(eve_key_tap::quartz_poster(), None)) as Box<dyn KeyTap>
            }),
        }
    }
}

impl std::fmt::Debug for Seams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Seams")
    }
}
