//! The daemon itself: `Daemon.__init__`, its attributes, `_spawn` and `_work_note`.
//!
//! Python's attributes split three ways here. What `__init__` makes once and never replaces, and what is
//! synchronized on its own (the tracker, the head, the bus), is a field of [`Daemon`]. What Python's methods
//! mutate with plain assignment is one field of [`DaemonState`], behind [`Daemon::st`], a `parking_lot` lock that is
//! never held across an `.await`. The few objects Python mutates in place through their own methods (`State`,
//! `SoundSetting`, `Explorer`, `CaptionPager`, `ThoughtScreen`) have a lock of their own.
//!
//! Python's `getattr(self, "_x", None)` (an attribute some code paths never set) is `None` in the state; Python's
//! `hasattr(self, "_x")` (made once, perhaps as None) is `Option<Option<_>>`.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use serde_json::Value;

use crate::seams::{
    BoardLink, CommandAsker, DaemonHub, Environ, HeadAsker, Inlet, Job, KeyTap, Learning, Mic, Obj, Seams,
    TaskAgent, Thinker, VoiceSessionHandle,
};

/// `Daemon(socket_path=None, device_name_prefix="Claude", device_address=None, matchers=None, serial_port=None,
/// save_frames=None)`: what `eve-bridge daemon` passes.
#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub socket_path: Option<String>,
    pub device_name_prefix: String,
    pub device_address: Option<String>,
    /// None: `matchers.load_config()`.
    pub matchers: Option<eve_matchers::MatcherConfig>,
    /// Some: the USB CDC transport on this port (glob) instead of BLE.
    pub serial_port: Option<String>,
    /// `--save-frames`: where the tracker keeps camera frames (vision.configured_save_dir).
    pub save_frames: Option<String>,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        DaemonOptions {
            socket_path: None,
            device_name_prefix: eve_ble::DEFAULT_NAME_PREFIX.to_string(),
            device_address: None,
            matchers: None,
            serial_port: None,
            save_frames: None,
        }
    }
}

/// What [`Daemon::assemble`] builds the daemon from: everything `__init__` reads from the environment and the
/// disk, made by the caller. [`Daemon::new`] makes the real parts; [`DaemonParts::for_test`] inert ones a test then
/// adjusts (Python's tests built a `SimpleNamespace` with only the attributes a method reads).
pub struct DaemonParts {
    /// `self.ble`: the robot's link (BLE, or USB serial tee'd to the Voice PE), or a test's fake.
    pub link: Arc<dyn BoardLink>,
    /// `self._controller`: the Voice PE, when EVE_CONTROLLER_SERIAL names one (serial transport only).
    pub controller: Option<Arc<eve_controller::Controller>>,
    /// Build `self.ipc` (on `ipc_socket`, None: the default address). Tests leave it off.
    pub with_ipc: bool,
    pub ipc_socket: Option<String>,
    /// Build `self.jsonl`, the transcript tailer. Tests leave it off.
    pub with_jsonl: bool,
    pub matchers: eve_matchers::MatcherConfig,
    pub audit: eve_audit::AuditLog,
    /// `self._sound`, loaded.
    pub sound: eve_sound::SoundSetting,
    /// `self._mic`, loaded.
    pub mic: eve_sound::SoundSetting,
    pub explore_cfg: eve_explore::ExploreConfig,
    pub ears_cfg: eve_ears::EarsConfig,
    pub voice_cfg: eve_brain::voice_agent::VoiceConfig,
    pub recall_cfg: eve_records::RecallConfig,
    pub bus_cfg: eve_memory_bus::BusConfig,
    pub agent_cfg: eve_computer_agent::AgentConfig,
    pub scene_cfg: eve_scene::SceneConfig,
    /// `follow.configured()`.
    pub follow_enabled: bool,
    /// `follow.PresenceMap(follow.presence_path()).load()`.
    pub presence: eve_follow::PresenceMap,
    /// `vision.configured_save_dir(save_frames)`.
    pub save_frames: Option<PathBuf>,
    /// `protocol.CJK_CODECS.get(EVE_CJK_TARGET)`.
    pub cjk_codec: Option<String>,
    /// The environment the daemon's own reads go to (None: the process's).
    pub environ: Option<Environ>,
    pub seams: Seams,
    /// `time.monotonic()` at construction: the explorer's and the idle clock's start.
    pub now: f64,
}

impl DaemonParts {
    /// Inert parts for a test: `link` as the board, nothing on disk outside `dir` (the sound and mic switches,
    /// the audit log, the presence map, the notes and memory folders), every optional piece off, the real seams.
    pub fn for_test(link: Arc<dyn BoardLink>, dir: &std::path::Path) -> DaemonParts {
        // The daemon's own reads go to this map, never to the process's environment: every file the parts
        // would find through it lands in `dir`.
        let mut environ = Environ::new();
        for (key, file) in [
            ("EVE_EXPRESSIONS_FILE", "expressions.json"),
            ("EVE_PRESENCE_FILE", "presence.json"),
            ("EVE_OWNER_PRINTS", "owner_faceprints.json"),
        ] {
            environ.insert(key.to_string(), dir.join(file).to_string_lossy().into_owned());
        }
        let env = Some(&environ);
        let mut sound = eve_sound::SoundSetting::new(Some(dir.join("sound.json")), "sound");
        sound.load();
        let mut mic = eve_sound::SoundSetting::new(Some(dir.join("mic.json")), "mic");
        mic.load();
        let mut explore_cfg = eve_explore::configured(env);
        explore_cfg.notes_dir = dir.join("explore");
        let mut agent_cfg = eve_computer_agent::configured(env);
        agent_cfg.runs_dir = dir.join("runs");
        let presence = eve_follow::PresenceMap::new(Some(dir.join("presence.json"))).load();
        DaemonParts {
            link,
            controller: None,
            with_ipc: false,
            ipc_socket: None,
            with_jsonl: false,
            matchers: eve_matchers::MatcherConfig::default(),
            audit: eve_audit::AuditLog::new(Some(dir.join("audit.jsonl"))),
            sound,
            mic,
            explore_cfg,
            ears_cfg: eve_ears::configured_from(|k| environ.get(k).cloned()),
            voice_cfg: eve_brain::voice_agent::configured(env),
            recall_cfg: eve_records::RecallConfig::new(dir.join("memory"), dir.join("notes")),
            bus_cfg: eve_memory_bus::configured(env),
            agent_cfg,
            scene_cfg: eve_scene::configured(env),
            follow_enabled: eve_follow::configured(env),
            presence,
            save_frames: None,
            cjk_codec: None,
            environ: Some(environ.clone()),
            seams: Seams::default(),
            now: eve_state::monotonic(),
        }
    }
}

/// What Python's methods reassign after `__init__`: one lock, never held across an `.await`.
#[derive(Default)]
pub struct DaemonState {
    // ---- the board's keys and listening pose ----
    /// `_keys`: built on the first swipe.
    pub keys: Option<Box<dyn KeyTap>>,
    /// `_listen_stop`: the listen key's tap, started in run(); None when it could not be created.
    pub listen_stop: Option<eve_listen_key::Stopper>,
    /// `_listen_sent`: the last {"cmd":"listen"} state the board received; None until the first send.
    pub listen_sent: Option<bool>,
    /// `_listen_down`: the physical listen key, independent of what the board received.
    pub listen_down: bool,
    /// `_ptt_held`: the board's hold-to-talk button, by its last ptt edge.
    pub ptt_held: bool,

    // ---- what run() resolves ----
    /// `_identity`: the owner's prints and the Vision describer.
    pub identity: Option<Arc<eve_identity::FaceIdentity>>,
    /// `_intent`: "go away" / "mute" in any words.
    pub intent: Option<eve_intent::Classifier>,
    /// `_thinker`: the slow brain behind the voice (think.py).
    pub thinker: Option<Thinker>,
    /// `_notes`: the diary's note taker (vision notes), None without a diary model.
    pub notes: Option<Arc<eve_diary::DiaryTaker>>,
    /// `_learning_server`: the lesson workspace, None with EVE_LEARNING=0 or when it did not start.
    pub learning_server: Option<Arc<dyn Learning>>,
    /// `_ears`: the wake word's microphone, None with EVE_VOICE=0 or no model.
    pub ears: Option<Arc<dyn Mic>>,
    /// `_memory`: the memory facade, None while memory is off (EVE_MEMORY).
    pub memory: Option<Arc<eve_memory::Memory>>,

    // ---- lessons and think out loud ----
    /// `_learning_idle_handle`: the pending "back to idle" after a lesson caption.
    pub learning_idle: Option<tokio::task::AbortHandle>,
    /// `_learning_notices`: a count of lesson notices, so _handle_lesson can tell whether dispatch captioned.
    pub learning_notices: u64,
    /// `_lesson_action`: names the action in the lesson event a notice publishes.
    pub lesson_action: String,
    /// `_voice_session`: the open conversation's session, once connected.
    pub voice_session: Option<Arc<dyn VoiceSessionHandle>>,
    /// `_think_aloud_wish`: a lesson waiting for a conversation that is still connecting or closing.
    pub think_aloud_wish: Option<Obj>,
    /// `_think_aloud_state`: "off", "starting" or "on" (Default: "" — [`Daemon::assemble`] sets "off").
    pub think_aloud_state: String,
    pub think_aloud_lesson_id: Option<String>,

    // ---- the memory bus's sinks ----
    pub rosbridge: Option<Arc<eve_rosbridge::RosbridgeServer>>,
    pub claude_mem_sink: Option<eve_claude_mem::ClaudeMemSink<eve_memory_bus::MemoryBus>>,
    pub claude_mem_mirror: Option<eve_claude_mem::ClaudeMemMirror<eve_memory_bus::MemoryBus>>,

    // ---- the conversation and the desktop task ----
    /// `_voice_conv`: the newest voice conversation's transcript id ("" when none).
    pub voice_conv: String,
    /// `_room_notes`: recording the room on request, built lazily.
    pub room_notes: Option<Arc<eve_notes::RoomNotes>>,
    /// `_conversation`: the voice conversation's task.
    pub conversation: Option<Job>,
    /// `_agent_state`: the phase the board mirrors (Default: "" — [`Daemon::assemble`] sets "idle").
    pub agent_state: String,
    /// `_active_agent`: the desktop task's agent, the newest one made.
    pub active_agent: Option<Arc<dyn TaskAgent>>,
    /// `_explore_after_conversation`: "go explore" said by voice, granted once the conversation has closed.
    pub explore_after_conversation: Option<String>,
    /// `_desk_calls`: the board's hold-to-talk calls, made on the first press.
    pub desk_calls: Option<Arc<eve_desk_call::DeskCalls>>,
    /// `_voice_embedder`: the speaker gate's model, loaded once.
    pub voice_embedder: Option<Arc<Mutex<eve_voice_gate::SherpaEmbedder>>>,
    /// `_head_asker`: None until asked; Some(None) is Python's False (off); Some(Some) the Jev asker.
    pub head_asker: Option<Option<HeadAsker>>,

    // ---- the explorer ----
    /// `_last_activity_at`: monotonic time of the last thing a human or a session did.
    pub last_activity_at: f64,
    /// `_explore_raw_frame`: the newest raw {"frame":...}, kept only while exploring.
    pub explore_raw_frame: Option<Value>,
    /// `_snap_waiter`: a pending {"cmd":"snap"}; the diary waits on the receiving end.
    pub snap_waiter: Option<tokio::sync::oneshot::Sender<Result<eve_vision::Frame, eve_vision::ValueError>>>,
    /// `_motion_trace`: the last few seconds of reported head poses (t, yaw, pitch).
    pub motion_trace: Vec<(f64, f64, f64)>,
    /// `_follow_test`: `eve-bridge follow`'s hold.
    pub follow_test: Option<Job>,

    // ---- the board link's health ----
    /// `_last_diag`: the newest {"diag":{...}} the board sent.
    pub last_diag: Option<Obj>,
    pub status_sent_at: Option<f64>,
    pub status_missed: u32,
    /// consecutive missed-ack episodes
    pub ack_escalation: u32,
    /// consecutive answered watchdog polls (resync acks excluded)
    pub clean_polls: u32,
    /// session_id → the task that flips running→0 after a grace window.
    pub pending_turn_ends: HashMap<String, Job>,
    pub last_stick_sec: Option<bool>,
    pub last_stick_battery_pct: Option<i64>,
    pub last_hb_serialized: Option<String>,
    pub last_hb_sent_at: f64,
    /// `_update_available`: the newer release's version, when there is one.
    pub update_available: Option<String>,

    // ---- expressions ----
    pub expression_audition: Option<Job>,
    /// `_phone_expressions`: the phone's picker, made with the Mini App.
    pub phone_expressions: Option<Arc<eve_live_expressions::LiveExpressions>>,

    // ---- the text door and what it is lent ----
    /// `_telegram`: the text door, None unless EVE_TELEGRAM is on with a token and an owner id.
    pub telegram: Option<Arc<dyn Inlet>>,
    /// The same door as its concrete type, for what takes it whole (the Mini App's calls, Eve Link, the desk
    /// calls, the app maker). None when `telegram` is a test's fake.
    pub telegram_inlet: Option<Arc<eve_brain::telegram::TelegramInlet>>,
    pub watcher: Option<Arc<eve_web::watch::Watcher>>,
    pub meeter: Option<Arc<eve_meet::Meeter>>,
    pub agentcard: Option<Arc<eve_agentcard::AgentCard>>,
    /// `_canvas` (hasattr): Some(None) once tried and off.
    pub canvas: Option<Option<Arc<eve_canvas::Canvas>>>,
    /// `_share` (hasattr).
    pub share: Option<Option<Arc<eve_brain::share::Share>>>,
    /// `_america` (hasattr).
    pub america: Option<Option<Arc<eve_america::America>>>,
    /// `_miniapp`: the "Ask Claude" Mini App, EVE_MINIAPP=1.
    pub miniapp: Option<Arc<eve_brain::miniapp::MiniApp>>,
    /// `_work`: the Work tab's Claude Code sessions.
    pub work: Option<Arc<eve_work_sessions::WorkSessions>>,

    // ---- eve's apps and the agents' bodies ----
    /// `_<slot>` for each app made (hasattr: a slot present with None is an app that is off this run).
    pub hubs: HashMap<String, Option<Arc<dyn DaemonHub>>>,
    /// `_music_mode`: Spotify mode on the Voice PE.
    pub music_mode: Option<Arc<eve_music_mode::MusicMode>>,
    /// `_codex_warm`: one Codex agent started ahead of time.
    pub codex_warm: Option<Arc<eve_codex_warm::WarmCodex<eve_codex_computer::CodexComputerAgent>>>,
    /// `_chrome_lane`: the browser lane for the daemon's life, EVE_BROWSER_ATTACH=1.
    pub chrome_lane: Option<Arc<eve_browser_lane::BrowserLane>>,
    /// `_desk_lane`: made once, EVE_DESK_LANE on.
    pub desk_lane: Option<Arc<eve_desk_lane::DeskLane>>,
    /// `_reader_router` (hasattr): browser_router's Jev router.
    pub reader_router: Option<Option<Arc<eve_browser_router::Router>>>,
    /// `_planner_create`: the Chrome lane planner's Responses creator.
    pub planner_create: Option<eve_computer_agent::CreateResponse>,
    /// `_command_risk_cache`: None until built; Some(None) is Python's False (off).
    pub command_risk: Option<Option<(String, CommandAsker)>>,
}

/// eve's daemon. Always held as an `Arc` (see [`Daemon::me`]): its loops and callbacks hold it.
pub struct Daemon {
    /// The daemon's own `Arc`, for callbacks made in `&self` methods.
    pub me: Weak<Daemon>,
    /// `self.state`: the sessions, entries and tokens the heartbeat carries.
    pub state: Mutex<eve_state::State>,
    /// `self.ipc`: None in tests.
    pub ipc: Option<eve_ipc::IpcServer>,
    /// `self.ble`.
    pub ble: Arc<dyn BoardLink>,
    /// `self._controller`.
    pub controller: Option<Arc<eve_controller::Controller>>,
    /// `self.jsonl`: None in tests.
    pub jsonl: Option<Arc<eve_jsonl_tailer::JsonlTailer>>,
    pub matchers: eve_matchers::MatcherConfig,
    /// Per-decision append-only log; see audit.py
    pub audit: eve_audit::AuditLog,
    /// `self._vision`: the board streams camera frames, the host answers with the face position.
    pub vision: Arc<eve_vision::FaceTracker>,
    /// `self._scene`: eyes for the voice.
    pub scene: Arc<eve_scene::SceneWatcher>,
    /// `self._head`: head control for the voice.
    pub head: Arc<eve_head::Head>,
    /// `self._follower`: eyes on whoever is talking.
    pub follower: Arc<eve_follow::SpeakerFollower>,
    /// `self._sound`: the owner's mute choice.
    pub sound: Mutex<eve_sound::SoundSetting>,
    /// `self._expressions`: the board's live expressions.
    pub expressions: Arc<eve_live_expressions::LiveExpressions>,
    /// `self._mic`: the owner's microphone switch.
    pub mic: Mutex<eve_sound::SoundSetting>,
    pub explore_cfg: eve_explore::ExploreConfig,
    /// `self._explorer`: the idle explorer, ticked from the explore loop.
    pub explorer: Mutex<eve_explore::Explorer>,
    pub ears_cfg: eve_ears::EarsConfig,
    pub voice_cfg: eve_brain::voice_agent::VoiceConfig,
    pub recall_cfg: eve_records::RecallConfig,
    /// `self.bus`: eve's memory as it forms.
    pub bus: eve_memory_bus::MemoryBus,
    pub bus_cfg: eve_memory_bus::BusConfig,
    pub agent_cfg: eve_computer_agent::AgentConfig,
    /// `self._thought_pager`: thoughts on the robot's own screen while it explores.
    pub thought_pager: Mutex<eve_caption_pager::CaptionPager>,
    /// `self._screen`: what is worth putting there.
    pub screen: Mutex<eve_thought_screen::ThoughtScreen>,
    /// `self._ack_waiters`, `expect_ack`, `wait_for_ack` and the ack-routing branch of `_handle_ble`.
    pub acks: eve_folder_push::AckRouter,
    /// `self._cjk_codec`: the wire codec for heartbeat string fields (CJK firmware), None for stock firmware.
    pub cjk_codec: Option<String>,
    /// `self._shutdown`: set once by [`Daemon::shutdown`]; every loop watches it.
    pub shutdown: tokio::sync::watch::Sender<bool>,
    /// `self._background`: fire-and-forget work, held so shutdown drains it rather than cancels it.
    pub background: Mutex<Vec<Job>>,
    /// The environment the daemon's own reads go to (None: the process's).
    pub environ: Option<Environ>,
    pub seams: Seams,
    /// Everything Python reassigns.
    pub st: Mutex<DaemonState>,
}

impl Daemon {
    /// `Daemon(...)`: the real parts from the environment and the disk, assembled. Fails only when the IPC address
    /// is invalid.
    pub fn new(opts: DaemonOptions) -> anyhow::Result<Arc<Daemon>> {
        // The IPC address first, as `IPCServer(...)` is the first thing `__init__` makes after the state.
        let probe: eve_ipc::Handler = Arc::new(|_req| Box::pin(async { Ok(Value::Null) }));
        eve_ipc::IpcServer::new(probe, opts.socket_path.as_deref())?;
        let inbox: crate::link::Inbox = Arc::new(std::sync::OnceLock::new());
        let (link, controller) = crate::link::make_link(&opts, None, inbox.clone());
        // JSONLTailer() resolves its roots now; a home that cannot be found fails here, not later.
        eve_claude_home::transcript_roots(None)?;
        let matchers = match opts.matchers.clone() {
            Some(m) => m,
            None => eve_matchers::load_config(None)?,
        };
        let audit = eve_audit::AuditLog::new(None);
        let mut sound = eve_sound::SoundSetting::new(None, "sound");
        sound.load();
        let mut mic = eve_sound::SoundSetting::new(None, "mic");
        mic.load();
        // Wire codec for heartbeat string fields. Set when the user has
        // flashed the fork-only CJK firmware variant (`m5stickc-plus-cjk-*`)
        // and tells the bridge which one via env var. None = stock firmware,
        // ASCII-only content. See protocol.CJK_CODECS for the mapping.
        let target = std::env::var("EVE_CJK_TARGET").unwrap_or_default().trim().to_string();
        let cjk_codec =
            if target.is_empty() { None } else { eve_protocol::cjk_codec(&target).map(str::to_string) };
        if let Some(codec) = &cjk_codec {
            tracing::info!("cjk firmware target={}, wire codec={}", target, codec);
        }
        let parts = DaemonParts {
            link,
            controller,
            with_ipc: true,
            ipc_socket: opts.socket_path.clone(),
            with_jsonl: true,
            matchers,
            audit,
            sound,
            mic,
            explore_cfg: eve_explore::configured(None),
            ears_cfg: eve_ears::configured(),
            voice_cfg: eve_brain::voice_agent::configured(None),
            recall_cfg: eve_records::configured(None),
            bus_cfg: eve_memory_bus::configured(None),
            agent_cfg: eve_computer_agent::configured(None),
            scene_cfg: eve_scene::configured(None),
            follow_enabled: eve_follow::configured(None),
            presence: eve_follow::PresenceMap::new(Some(eve_follow::presence_path(None))).load(),
            save_frames: eve_vision::configured_save_dir(opts.save_frames.as_deref()),
            cjk_codec,
            environ: None,
            seams: Seams::default(),
            now: eve_state::monotonic(),
        };
        let daemon = Daemon::assemble(parts);
        let _ = inbox.set(Arc::downgrade(&daemon));
        Ok(daemon)
    }

    /// The daemon from its parts: the tracker, the scene watcher, the head, the follower and the expressions wired
    /// to the link and to each other as `__init__` does, the IPC server and the tailer bound to the daemon's
    /// handlers when asked for.
    pub fn assemble(parts: DaemonParts) -> Arc<Daemon> {
        Arc::new_cyclic(|weak: &Weak<Daemon>| {
            let link = parts.link.clone();
            let send = crate::seams::link_sender(link.clone());
            let ipc = if parts.with_ipc {
                let w = weak.clone();
                let handler: eve_ipc::Handler = Arc::new(move |req: Value| {
                    let w = w.clone();
                    Box::pin(async move {
                        let daemon = w.upgrade().ok_or_else(|| anyhow::anyhow!("the daemon has shut down"))?;
                        match req {
                            Value::Object(req) => daemon.handle_ipc(req).await.map(Value::Object),
                            other => Err(eve_ipc::NamedError::new(
                                "AttributeError",
                                format!("'{}' object has no attribute 'get'", py_type_name(&other)),
                            )
                            .into()),
                        }
                    })
                });
                match eve_ipc::IpcServer::new(handler, parts.ipc_socket.as_deref()) {
                    Ok(server) => Some(server),
                    Err(e) => {
                        tracing::error!("ipc: bad address {:?}: {}", parts.ipc_socket, e);
                        None
                    }
                }
            } else {
                None
            };
            let jsonl = if parts.with_jsonl {
                let (wt, wa) = (weak.clone(), weak.clone());
                let on_tokens: eve_jsonl_tailer::TokensCallback =
                    Arc::new(move |cumulative, today, cost_cumulative, cost_today, entries| {
                        let w = wt.clone();
                        Box::pin(async move {
                            match w.upgrade() {
                                Some(d) => d.on_tokens(cumulative, today, cost_cumulative, cost_today, entries).await,
                                None => Ok(()),
                            }
                        })
                    });
                let on_text: eve_jsonl_tailer::AssistantTextCallback = Arc::new(move |path, text, uuid| {
                    let w = wa.clone();
                    Box::pin(async move {
                        match w.upgrade() {
                            Some(d) => d.on_assistant_text(&path, &text, &uuid).await,
                            None => Ok(()),
                        }
                    })
                });
                match eve_jsonl_tailer::JsonlTailer::new(on_tokens, None, Some(on_text)) {
                    Ok(t) => Some(Arc::new(t)),
                    Err(e) => {
                        tracing::error!("tailer: cannot find the transcript roots: {e}");
                        None
                    }
                }
            } else {
                None
            };
            // Host vision: the board streams camera frames, we answer with the
            // face position. The detector (macOS Vision) is resolved in run() so
            // constructing a Daemon never loads Vision; until then, and on
            // hosts without Vision, the tracker is disabled and we never ask the
            // board to stream — it keeps its on-board tracking.
            let vision = Arc::new(eve_vision::FaceTracker::with_options(
                None,
                send.clone(),
                parts.save_frames.clone(),
                Arc::new(eve_vision::monotonic),
                None,
                None,
            ));
            // Eyes for the voice (scene.py): camera frames become timestamped
            // descriptions, only while a conversation is open. The client (an
            // image model) is resolved in run(), like the detector.
            let scene = {
                let (link, vision) = (link.clone(), vision.clone());
                Arc::new(
                    eve_scene::SceneWatcher::new(None, Some(parts.scene_cfg.clone()))
                        .with_camera_ok(Arc::new(move || link.connected() && vision.enabled())),
                )
            };
            // Head control for the voice (head.py). The board echoes its real
            // pose on every camera frame; relative moves start from that.
            let head = {
                let link = link.clone();
                Arc::new(eve_head::Head::with(
                    send.clone(),
                    Arc::new(move || link.connected()),
                    Arc::new(eve_head::monotonic),
                    eve_head::tokio_sleep(),
                ))
            };
            // In a conversation eve keeps its eyes on whoever is talking (follow.py): every face of every
            // frame goes to the follower, which stands down for an asked-for pose, an explore, the dictation
            // key and the lesson's listening pose.
            let follower = {
                let (wb, wp) = (weak.clone(), weak.clone());
                let blocked: eve_follow::Blocked = Arc::new(move || {
                    let Some(d) = wb.upgrade() else { return String::new() };
                    if d.explorer.lock().active() {
                        return "explore".into();
                    }
                    let st = d.st.lock();
                    if st.listen_down {
                        "dictation".into()
                    } else if st.think_aloud_state == "on" {
                        "lesson listening pose".into()
                    } else {
                        String::new()
                    }
                });
                // /eve/presence: seen, lost, found and how
                let publish: eve_follow::Publisher = Arc::new(move |topic: &str, msg| {
                    if let Some(d) = wp.upgrade() {
                        d.bus.publish(topic, msg);
                    }
                    Ok(())
                });
                Arc::new(eve_follow::SpeakerFollower::with(
                    send.clone(),
                    eve_follow::FollowerOptions {
                        enabled: parts.follow_enabled,
                        blocked,
                        presence: Some(parts.presence),
                        publish: Some(publish),
                        ..Default::default()
                    },
                ))
            };
            {
                let f = follower.clone();
                head.set_on_move(Some(Arc::new(move |hold_secs| f.owner_moved(hold_secs))));
                let f = follower.clone();
                let on_faces: eve_vision::OnFaces = Arc::new(move |faces, yaw, pitch| {
                    let f = f.clone();
                    Box::pin(async move {
                        let faces: Vec<eve_follow::Face> =
                            faces.iter().map(|r| eve_follow::Face::new(r.bx, r.by, r.size, r.conf)).collect();
                        f.on_faces(&faces, yaw, pitch).await;
                        Ok(())
                    })
                });
                vision.set_on_faces(Some(on_faces));
            }
            // The board's live expressions, muted with the owner's sound and told the conversation's phase.
            let expressions = {
                let send_link = link.clone();
                let send_fn: eve_live_expressions::SendFn = Arc::new(move |obj: Value| {
                    let link = send_link.clone();
                    Box::pin(async move { Ok(link.send(&obj, None).await) })
                });
                let (lc, wm, wph) = (link.clone(), weak.clone(), weak.clone());
                let env: Option<Arc<dyn eve_jev::Env + Send + Sync>> =
                    parts.environ.clone().map(|e| Arc::new(e) as Arc<dyn eve_jev::Env + Send + Sync>);
                Arc::new(eve_live_expressions::LiveExpressions::new(
                    send_fn,
                    eve_live_expressions::Options {
                        connected: Arc::new(move || lc.connected()),
                        muted: Arc::new(move || wm.upgrade().is_some_and(|d| d.sound.lock().muted())),
                        phase: Arc::new(move || {
                            let state = wph.upgrade().map(|d| d.st.lock().agent_state.clone()).unwrap_or_default();
                            if state.is_empty() { "idle".to_string() } else { state }
                        }),
                        env,
                        ..Default::default()
                    },
                ))
            };
            let thought_pager = eve_caption_pager::CaptionPager::new(eve_caption_pager::PagerConfig {
                read_cps: parts.voice_cfg.caption_cps,
                ..Default::default()
            });
            let st = DaemonState {
                think_aloud_state: "off".into(),
                agent_state: "idle".into(),
                last_activity_at: parts.now,
                ..Default::default()
            };
            Daemon {
                me: weak.clone(),
                state: Mutex::new(eve_state::State::new()),
                ipc,
                ble: link,
                controller: parts.controller,
                jsonl,
                matchers: parts.matchers,
                audit: parts.audit,
                vision,
                scene,
                head,
                follower,
                sound: Mutex::new(parts.sound),
                expressions,
                mic: Mutex::new(parts.mic),
                explorer: Mutex::new(eve_explore::Explorer::new(parts.explore_cfg.clone(), parts.now)),
                explore_cfg: parts.explore_cfg,
                ears_cfg: parts.ears_cfg,
                voice_cfg: parts.voice_cfg,
                recall_cfg: parts.recall_cfg,
                bus: eve_memory_bus::MemoryBus::new(),
                bus_cfg: parts.bus_cfg,
                agent_cfg: parts.agent_cfg,
                thought_pager: Mutex::new(thought_pager),
                screen: Mutex::new(eve_thought_screen::ThoughtScreen::new()),
                acks: eve_folder_push::AckRouter::new(),
                cjk_codec: parts.cjk_codec,
                shutdown: tokio::sync::watch::channel(false).0,
                background: Mutex::new(Vec::new()),
                environ: parts.environ,
                seams: parts.seams,
                st: Mutex::new(st),
            }
        })
    }

    /// The daemon's own `Arc`.
    pub fn me(&self) -> Arc<Daemon> {
        self.me.upgrade().expect("the daemon is held in an Arc while it runs")
    }

    /// The environment the daemon's own reads go to.
    pub fn environ(&self) -> Option<&Environ> {
        self.environ.as_ref()
    }

    /// `env.get(key)` from the daemon's environment.
    pub fn env(&self, key: &str) -> Option<String> {
        crate::seams::env_get(self.environ(), key)
    }

    /// `self._conversation is not None and not self._conversation.done()`.
    pub fn conversation_open(&self) -> bool {
        self.st.lock().conversation.as_ref().is_some_and(|c| !c.done())
    }

    /// `self._shutdown.is_set()`.
    pub fn shutting_down(&self) -> bool {
        *self.shutdown.borrow()
    }

    /// A receiver of the shutdown flag, for the loops that watch it (`await self._shutdown.wait()`).
    pub fn shutdown_rx(&self) -> tokio::sync::watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    /// `_spawn(daemon, coro, name)`: fire-and-forget work, held in `background` until it finishes. The event loop
    /// keeps only a weak reference to a task, so one nobody holds can be collected mid-run; held here it also gets
    /// drained at shutdown (_drain_background) rather than cancelled.
    pub fn spawn<F>(&self, name: &str, fut: F) -> Job
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let job = Job::spawn(name, fut);
        let mut background = self.background.lock();
        background.retain(|j| !j.done());
        background.push(job.clone());
        job
    }

    /// `self._background` as a list: the jobs still running (finished ones are dropped from the set first, as
    /// their done callbacks did).
    pub fn background_jobs(&self) -> Vec<Job> {
        let mut background = self.background.lock();
        background.retain(|j| !j.done());
        background.clone()
    }

    /// `_work_note(daemon, evt, req)`: a hook event for the Work sessions' registry, when it exists. A failure
    /// costs the Work tab's state, never the hook's reply.
    pub fn work_note(&self, evt: &str, req: &Obj) {
        let work = self.st.lock().work.clone();
        let Some(work) = work else { return };
        let req = Value::Object(req.clone());
        let noted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work.note(evt, &req)));
        if noted.is_err() {
            tracing::warn!("work sessions: note {} failed ({})", evt, "panic");
        }
    }
}

/// Python's `type(x).__name__` for a JSON value.
fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}
