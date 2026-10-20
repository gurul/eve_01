//! A port of `voice_agent.py`, lines 744-1287: VoiceSession: its state, run, billing, memory capture, think-aloud.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::voice_agent::*;

/// py: voice_agent.py:744 `class VoiceSession:`
///
/// One conversation. See the module docstring.
///
/// Rust: built by [`VoiceSession::new`] as `Arc<Self>`; what Python reassigns is in `st`, a `parking_lot::Mutex`
/// never held across an await. The tests drive it with a fake [`LiveConnection`], speaker and clock.
pub struct VoiceSession {
    pub meeter: Option<Arc<eve_meet::Meeter>>,
    pub on_expression: Option<ExpressionFn>,
    pub mac_busy: BoolFn,
    pub head_pose: Option<HeadPoseFn>,
    pub gate: Option<Arc<dyn SpeakerGate>>,
    pub learning: Option<LearningFn>,
    pub(crate) lesson_wake: bool,
    pub on_spoken_idea: Option<SpokenIdeaFn>,
    pub on_think_aloud: Option<ThinkAloudFn>,
    /// session.started arrived: context sent now is not discarded
    pub(crate) started: Flag,
    pub conn: Arc<dyn LiveConnection>,
    pub scene: Option<Arc<dyn SceneWatch>>,
    pub head: Option<Arc<eve_head::Head>>,
    pub intent: Option<IntentFn>,
    pub thinker: Option<Thinker>,
    pub on_photo: Option<OnPhoto>,
    pub on_sound: Option<OnSound>,
    pub muted: BoolFn,
    pub on_caption: Option<OnCaption>,
    pub on_explore: Option<OnExplore>,
    pub mic: eve_ears::PcmQueue,
    pub speaker: Arc<dyn VoiceSpeaker>,
    pub agent_factory: AgentFactory,
    pub on_state: OnState,
    pub config: VoiceConfig,
    pub brief: String,
    pub memory: Option<Arc<dyn BrainMemory>>,
    pub conv: String,
    pub today: String,
    pub backend_today: String,
    pub(crate) clock: Clock,
    /// `config.output == "captions" and on_caption is not None`
    pub(crate) captions: bool,
    pub(crate) caption_tick: f64,
    pub(crate) watchdog_secs: f64,
    pub agent_enabled: bool,
    pub(crate) ended: Flag,
    /// eve's apps' hubs by slot (`self.<slot>`)
    pub hubs: Mutex<AppHubs>,
    /// What Python reassigns after `__init__`, behind one lock.
    pub(crate) st: Mutex<VoiceSessionState>,
}

/// `VoiceSession(...)`'s keyword arguments; `Default` is Python's defaults (a `None` callable with a Python
/// default, `clock`, `muted`, `mac_busy`, is that default).
pub struct VoiceSessionOptions {
    pub config: Option<VoiceConfig>,
    pub clock: Option<Clock>,
    pub agent_enabled: bool,
    pub on_caption: Option<OnCaption>,
    pub caption_tick_secs: f64,
    pub watchdog_secs: f64,
    pub on_explore: Option<OnExplore>,
    pub turn_gap_secs: f64,
    pub scene: Option<Arc<dyn SceneWatch>>,
    pub head: Option<Arc<eve_head::Head>>,
    pub intent: Option<IntentFn>,
    pub on_sound: Option<OnSound>,
    pub muted: Option<BoolFn>,
    pub thinker: Option<Thinker>,
    pub on_photo: Option<OnPhoto>,
    pub brief: String,
    pub memory: Option<Arc<dyn BrainMemory>>,
    pub conv: String,
    pub today: String,
    pub backend_today: String,
    pub learning: Option<LearningFn>,
    pub think_aloud: Option<Obj>,
    pub on_spoken_idea: Option<SpokenIdeaFn>,
    pub on_think_aloud: Option<ThinkAloudFn>,
    pub lesson_wake: bool,
    pub head_pose: Option<HeadPoseFn>,
    pub gate: Option<Arc<dyn SpeakerGate>>,
    pub on_expression: Option<ExpressionFn>,
    pub mac_busy: Option<BoolFn>,
    pub meeter: Option<Arc<eve_meet::Meeter>>,
    /// `**app_hubs`
    pub app_hubs: AppHubs,
}

impl Default for VoiceSessionOptions {
    fn default() -> Self {
        Self {
            config: None,
            clock: None,
            agent_enabled: true,
            on_caption: None,
            caption_tick_secs: 0.05,
            watchdog_secs: 0.5,
            on_explore: None,
            turn_gap_secs: TURN_GAP_SECS,
            scene: None,
            head: None,
            intent: None,
            on_sound: None,
            muted: None,
            thinker: None,
            on_photo: None,
            brief: String::new(),
            memory: None,
            conv: String::new(),
            today: String::new(),
            backend_today: String::new(),
            learning: None,
            think_aloud: None,
            on_spoken_idea: None,
            on_think_aloud: None,
            lesson_wake: false,
            head_pose: None,
            gate: None,
            on_expression: None,
            mac_busy: None,
            meeter: None,
            app_hubs: AppHubs::default(),
        }
    }
}

/// `VoiceSession`'s mutable state (the attributes Python reassigns or mutates after `__init__`).
pub(crate) struct VoiceSessionState {
    pub(crate) expression_chars: i64,
    /// (the turn it answered, what the head did)
    pub(crate) fast_head: Option<(f64, Obj)>,
    pub(crate) think_aloud: Option<Obj>,
    pub(crate) lesson_setup: bool,
    pub(crate) idea_saves: Vec<Job>,
    pub(crate) end_reason: String,
    pub(crate) farewell: bool,
    pub(crate) farewell_since: f64,
    pub(crate) task_done_at: f64,
    pub(crate) last_reply_at: f64,
    pub(crate) slow_tasks: Vec<Job>,
    pub(crate) user_turn_started_at: f64,
    pub(crate) delegated_at: f64,
    pub(crate) last_head_tool_at: f64,
    pub(crate) explore_requested: bool,
    /// tool_gate.carried(...): "" or the kind of outside text the conversation holds
    pub(crate) taint: String,
    /// the acting call asked about, at which owner turn
    pub(crate) held: Option<(String, i64)>,
    pub(crate) owner_turns: i64,
    /// the owner turn that was a bare spoken yes (-1: none)
    pub(crate) owner_yes: i64,
    pub(crate) decision: Option<(i64, String, String)>,
    /// exact latest owner words; never a model interpretation
    pub(crate) answer_words: String,
    pub(crate) question_turn: i64,
    /// "gate" or "task": the question asked most recently
    pub(crate) asked_last: String,
    pub(crate) held_act: String,
    pub(crate) held_said: bool,
    /// the owner turn whose yes or no has been used (-1: none)
    pub(crate) used_turn: i64,
    pub(crate) memory_tool_names: HashSet<String>,
    pub(crate) opened_wall: Option<f64>,
    /// transcript lines this conversation wrote
    pub(crate) captured: i64,
    pub(crate) close_written: bool,
    /// the turn boundaries (`self._turns`, a `_Turns`)
    pub(crate) turn_marks: _Turns,
    pub(crate) turns: Vec<(String, String)>,
    pub(crate) caption: String,
    pub(crate) pager: eve_caption_pager::CaptionPager,
    pub(crate) state_after_captions: Option<String>,
    pub(crate) state: String,
    pub(crate) agent: Option<Arc<dyn TaskAgent>>,
    pub(crate) agent_task: Option<Job>,
    pub(crate) pending_answer: Option<Pending>,
    pub(crate) last_activity: f64,
    pub(crate) started_at: f64,
    pub(crate) tool_calls: Vec<(String, Obj)>,
    pub(crate) transcript: Vec<String>,
    pub(crate) response_active: bool,
    pub(crate) response_wanted: bool,
    pub(crate) reply_open: bool,
    pub(crate) speaking_until: f64,
    pub(crate) last_progress_at: f64,
}

impl VoiceSession {
    /// py: voice_agent.py:761 `def apps(self) -> dict[str, Any]:`
    ///
    /// eve's apps' hubs by slot (app_registry), read off this session's attributes (self.lights, ...) now.
    pub fn apps(self: &Arc<Self>) -> Vec<(String, Arc<dyn AppHub>)> {
        todo!("voice_agent.py:761 VoiceSession.apps")
    }

    /// py: voice_agent.py:765 `def __init__( self, connection: Any,                                   # Live connection (async ctx manager already entered) mic: "asyncio.Queue[bytes]",                        # from Ears.subscribe() speaker: Any,                                       # Speaker-like: play/stop/busy agent_factory: Callable[[Callable[[AgentEvent], None], Callable[[str], Awaitable[str]]], ComputerAgent], on_state: Callable[[str], None], config: Optional[VoiceConfig] = None, clock: Callable[[], float] = time.monotonic, agent_enabled: bool = True, on_caption: Optional[Callable[[dict], None]] = None, caption_tick_secs: float = 0.05, watchdog_secs: float = 0.5,                          # how often the idle / cap watchdog looks on_explore: Optional[Callable[[], None]] = None, turn_gap_secs: float = TURN_GAP_SECS, scene: Any = None,                                  # scene.SceneWatcher-like: start/stop/look/locate head: Any = None,                                   # head.Head-like: move/yaw/pitch/clock/sleep intent: Optional[Callable[[str], Awaitable[Optional[str]]]] = None,   # intent.make_classifier(...) on_sound: Optional[Callable[[bool], None]] = None,  # the owner muted (False) / unmuted (True) muted: Callable[[], bool] = lambda: False, thinker: Optional[Callable[..., Awaitable[dict[str, Any]]]] = None,   # think.make_thinker(...) on_photo: Optional[Callable[[str], Awaitable[dict[str, Any]]]] = None,   # Daemon._photo_for_owner brief: str = "",                                    # the opening brief, "" when memory is off memory: Optional["Memory"] = None,                  # memory.Memory, None when memory is off conv: str = "",                                     # this conversation's transcript id ("" mints one) today: str = "",                                    # today's talk for the Live front (voice budget) backend_today: str = "",                            # today's talk for the backend (backend budget) learning: Optional[Callable[..., dict[str, Any]]] = None, think_aloud: Optional[dict[str, Any]] = None,       # the lesson, when opened for think out loud on_spoken_idea: Optional[Callable[[str, str], Any]] = None,   # LearningApp.append_spoken (blocking) on_think_aloud: Optional[Callable[[bool, Optional[str]], None]] = None,   # (listening, lesson id) lesson_wake: bool = False,                          # opened by the "lesson" wake word head_pose: Optional[Callable[[str], Awaitable[str]]] = None,   # typed_ask: utterance -> a pose label, or "none" gate: Any = None,              # voice_gate.SpeakerGate for this conversation, or None (today's behaviour) on_expression: Optional[Callable[[str, str], None]] = None, mac_busy: Callable[[], bool] = lambda: False,       # a task from another door (telegram.py) has the Mac meeter: Any = None,                                 # meet.Meeter, or None: "join my meeting" out loud **app_hubs: Any,                                    # eve's apps by slot (app_registry): "lights off" out loud ) -> None:`
    /// (`**app_hubs` is `options.app_hubs`.)
    pub fn new(connection: Arc<dyn LiveConnection>, mic: eve_ears::PcmQueue, speaker: Arc<dyn VoiceSpeaker>, agent_factory: AgentFactory, on_state: OnState, options: VoiceSessionOptions) -> Arc<Self> {
        todo!("voice_agent.py:765 VoiceSession.__init__")
    }

    /// py: voice_agent.py:928 `def _set(self, state: str) -> None:`
    pub(crate) fn set(self: &Arc<Self>, state: &str) {
        todo!("voice_agent.py:928 VoiceSession._set")
    }

    /// py: voice_agent.py:937 `def task_running(self) -> bool:`
    pub fn task_running(self: &Arc<Self>) -> bool {
        todo!("voice_agent.py:937 VoiceSession.task_running")
    }

    /// py: voice_agent.py:943 `async def run(self) -> None:`
    pub async fn run(self: &Arc<Self>) {
        todo!("voice_agent.py:943 VoiceSession.run")
    }

    /// py: voice_agent.py:1038 `def _note_live_usage(self, event: Any) -> None:`
    ///
    /// Keep the Live session's cumulative audio seconds (a total, not an increment: the largest one wins).
    pub(crate) fn note_live_usage(self: &Arc<Self>, event: &Value) {
        todo!("voice_agent.py:1038 VoiceSession._note_live_usage")
    }

    /// py: voice_agent.py:1044 `def _bill_live(self) -> None:`
    ///
    /// The session's minutes, once, when it closes. Billed seconds come from the server's usage events; a
    /// session that never sent one is metered by its own clock and says so in the note.
    pub(crate) fn bill_live(self: &Arc<Self>) {
        todo!("voice_agent.py:1044 VoiceSession._bill_live")
    }

    /// py: voice_agent.py:1058 `def _bill_backend(self, response: Any) -> None:`
    ///
    /// One delegated Responses call: its usage at the backend model's rates, plus its hosted searches.
    pub(crate) fn bill_backend(self: &Arc<Self>, response: &Value) {
        todo!("voice_agent.py:1058 VoiceSession._bill_backend")
    }

    /// py: voice_agent.py:1068 `def _wall_now(self) -> Optional[datetime]:`
    ///
    /// When this conversation opened, for recent_conversation. Floored to the second: transcript lines
    /// carry whole seconds, so a text sent in the opening second is still newer than this.
    pub(crate) fn wall_now(self: &Arc<Self>) -> Option<chrono::DateTime<chrono::FixedOffset>> {
        todo!("voice_agent.py:1068 VoiceSession._wall_now")
    }

    /// py: voice_agent.py:1076 `def _profiles(self) -> tuple[str, str]:`
    ///
    /// (voice profile, full profile), off the loop: a few file reads.
    pub(crate) fn profiles(self: &Arc<Self>) -> (String, String) {
        todo!("voice_agent.py:1076 VoiceSession._profiles")
    }

    /// py: voice_agent.py:1087 `def _memory_tools(self) -> list[dict[str, Any]]:`
    ///
    /// The backend's memory tools, in memory.py's stable order. Their names route in _tool.
    pub(crate) fn memory_tools(self: &Arc<Self>) -> Vec<Obj> {
        todo!("voice_agent.py:1087 VoiceSession._memory_tools")
    }

    /// py: voice_agent.py:1102 `def _capture(self, who: str, kind: str, text: str, tool: str = "", *, outside: bool = False) -> None:`
    ///
    /// One transcript line for this conversation. Never while a learner thinks out loud; never raises.
    /// ``outside``: words eve did not write (a reply after a read, a task's sentence), kept marked
    /// (transcripts ``out``), so a restart cannot launder them into eve's own (v3 safety review).
    pub(crate) fn capture(self: &Arc<Self>, who: &str, kind: &str, text: &str, tool: &str, outside: bool) {
        todo!("voice_agent.py:1102 VoiceSession._capture")
    }

    /// py: voice_agent.py:1116 `def _write_close(self) -> None:`
    ///
    /// The close marker, once, when this conversation wrote anything.
    pub(crate) fn write_close(self: &Arc<Self>) {
        todo!("voice_agent.py:1116 VoiceSession._write_close")
    }

    /// py: voice_agent.py:1127 `def _capture_result(self, name: str, result: Any) -> None:`
    ///
    /// Keep what an answer-bearing tool returned, as one tool line of at most TOOL_LINE_CHARS.
    pub(crate) fn capture_result(self: &Arc<Self>, name: &str, result: &Value) {
        todo!("voice_agent.py:1127 VoiceSession._capture_result")
    }

    /// py: voice_agent.py:1142 `async def _memory_tool(self, name: str, args: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// A memory lookup in a worker thread, waited on for MEMORY_TIMEOUT_SECS at most.
    ///
    /// On a timeout the thread finishes on its own; the backend hears MEMORY_TIMEOUT_LINE and the
    /// conversation goes on. The log carries the tool, the hit count and the milliseconds, never the query.
    pub(crate) async fn memory_tool(self: &Arc<Self>, name: &str, args: &Obj) -> Obj {
        todo!("voice_agent.py:1142 VoiceSession._memory_tool")
    }

    /// py: voice_agent.py:1165 `def _think_context(self) -> str:`
    ///
    /// The owner's profile and today's talk, for think_hard (think.py bounds it). Off the loop.
    pub(crate) fn think_context(self: &Arc<Self>) -> String {
        todo!("voice_agent.py:1165 VoiceSession._think_context")
    }

    /// py: voice_agent.py:1177 `async def _await_started(self, timeout: float = 15.0) -> None:`
    ///
    /// Audio sent before `session.started` is discarded, so wait for it.
    pub(crate) async fn await_started(self: &Arc<Self>, timeout: f64) {
        todo!("voice_agent.py:1177 VoiceSession._await_started")
    }

    /// py: voice_agent.py:1192 `def end(self) -> None:`
    pub fn end(self: &Arc<Self>) {
        todo!("voice_agent.py:1192 VoiceSession.end")
    }

    /// py: voice_agent.py:1195 `def _finish(self, reason: str) -> None:`
    ///
    /// Close the session, remembering the first reason (cap, silence, goodbye, stopped, ...).
    pub(crate) fn finish(self: &Arc<Self>, reason: &str) {
        todo!("voice_agent.py:1195 VoiceSession._finish")
    }

    /// py: voice_agent.py:1203 `def ended(self) -> bool:`
    pub fn ended(self: &Arc<Self>) -> bool {
        todo!("voice_agent.py:1203 VoiceSession.ended")
    }

    /// py: voice_agent.py:1207 `def think_aloud_lesson_id(self) -> Optional[str]:`
    pub fn think_aloud_lesson_id(self: &Arc<Self>) -> Option<String> {
        todo!("voice_agent.py:1207 VoiceSession.think_aloud_lesson_id")
    }

    /// py: voice_agent.py:1210 `def _notify_think_aloud(self, on: bool) -> None:`
    pub(crate) fn notify_think_aloud(self: &Arc<Self>, on: bool) {
        todo!("voice_agent.py:1210 VoiceSession._notify_think_aloud")
    }

    /// py: voice_agent.py:1218 `async def enter_think_aloud(self, lesson: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// Switch an open conversation to think out loud (a voice request, or a toggle while talking).
    ///
    /// The voice's instructions are fixed at session.start, so its listening rules and the lesson go in
    /// as silent context; the backend's instructions are replaced through session.update.
    pub async fn enter_think_aloud(self: &Arc<Self>, lesson: &Obj) -> Obj {
        todo!("voice_agent.py:1218 VoiceSession.enter_think_aloud")
    }

    /// py: voice_agent.py:1250 `def prepare_think_aloud(self, lesson: dict[str, Any]) -> None:`
    ///
    /// Before run(): open this session for think out loud (asked for while it was connecting).
    pub fn prepare_think_aloud(self: &Arc<Self>, lesson: &Obj) {
        todo!("voice_agent.py:1250 VoiceSession.prepare_think_aloud")
    }

    /// py: voice_agent.py:1255 `def stop_think_aloud(self, reason: str = "stopped") -> None:`
    ///
    /// The toggle or the command stopped listening: the microphone closes now.
    pub fn stop_think_aloud(self: &Arc<Self>, reason: &str) {
        todo!("voice_agent.py:1255 VoiceSession.stop_think_aloud")
    }

    /// py: voice_agent.py:1261 `def _save_spoken_idea(self, text: str) -> None:`
    ///
    /// Append one finished learner turn to the lesson's ideas, off the event loop and after the ones before it.
    ///
    /// A dismissal ("I'm done", "stop listening") is a command, not thinking, and is not saved.
    pub(crate) fn save_spoken_idea(self: &Arc<Self>, text: &str) {
        todo!("voice_agent.py:1261 VoiceSession._save_spoken_idea")
    }
}
