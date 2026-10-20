//! A port of `voice_agent.py`, lines 744-1287: VoiceSession: its state, run, billing, memory capture, think-aloud.
//!
//! A hush (Python's `task.cancel()` on the running `run()`) is dropping the `run()` future (aborting its task):
//! the cleanup Python's `finally` runs on a `CancelledError` runs from a drop guard then, without the awaits a
//! drop cannot make (see [`VoiceSession::run`]).

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::voice_agent::*;
use eve_computer_agent::py::py_strip;

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
    /// spend's ledger (`spend.record_live` / `spend.record_response`): the configured folder unless a test lends one
    pub ledger: eve_spend::Ledger,
    /// `system_context.context()`: the clock and place block both prompts carry (a test pins it)
    pub system_context: StrFn,
    /// the environment the session's switchable tools are read from (None: the process's, Python's `os.environ`)
    pub environ: Option<Environ>,
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
    /// Rust seam: where the spend lines go (None: spend's configured folder, as Python's module functions).
    pub ledger: Option<eve_spend::Ledger>,
    /// Rust seam: `system_context.context()` (None: the real clock and place block, read when asked).
    pub system_context: Option<StrFn>,
    /// Rust seam: the environment the switchable tools read (None: the process's).
    pub environ: Option<Environ>,
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
            ledger: None,
            system_context: None,
            environ: None,
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
    /// when this conversation opened, floored to the second (recent_conversation's `since`)
    pub(crate) opened_wall: Option<eve_transcripts::Dt>,
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
    /// The spend meter (spend.py): the Live session's billed seconds, from session.usage.updated and
    /// session.closed (cumulative, never summed), ...
    pub(crate) live_secs: Option<f64>,
    pub(crate) live_billed: bool,
    /// ... and the hosted web searches of the backend response in flight.
    pub(crate) backend_searches: i64,
}

/// `TRANSCRIPT_WHO[who]`: the transcript's name for a turn's speaker, None for one it does not keep.
fn transcript_who(who: &str) -> Option<&'static str> {
    TRANSCRIPT_WHO.iter().find(|(k, _)| *k == who).map(|(_, v)| *v)
}

/// `try: f() except Exception: log.exception("voice: <what> failed")`: a lent callback never breaks the session.
fn guarded(what: &str, f: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
        tracing::error!("voice: {} failed", what);
    }
}

/// Where `run()`'s cleanup has got to, for the drop guard: what Python's `finally` still owes when a hush lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// `_until_ended()` is being awaited: a hush runs the whole `CancelledError` path
    Serving,
    /// the ordinary cleanup is under way: a hush skips the rest of it (Python's inner `try`)
    Cleaning,
    /// the last caption page is being held: a hush clears it at once
    Draining,
}

/// Runs the cleanup a hush owes when `run()`'s future is dropped before it finished.
struct RunGuard {
    session: Arc<VoiceSession>,
    jobs: Vec<Job>,
    stage: Stage,
    armed: bool,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let s = &self.session;
        match self.stage {
            Stage::Serving => s.hushed(&self.jobs),
            Stage::Draining => {
                let now = (s.clock)();
                let events = s.st.lock().pager.reset(now);
                s.emit_events(&events);
            }
            Stage::Cleaning => {}
        }
        // every exit path, a second hush in the drain too
        s.write_close();
        s.bill_live();
    }
}

impl VoiceSession {
    /// py: voice_agent.py:761 `def apps(self) -> dict[str, Any]:`
    ///
    /// eve's apps' hubs by slot (app_registry), read off this session's attributes (self.lights, ...) now.
    /// (Rust: only the slots with a hub; Python's dict holds None for the others.)
    pub fn apps(self: &Arc<Self>) -> Vec<(String, Arc<dyn AppHub>)> {
        let hubs = self.hubs.lock();
        eve_app_registry::with_tools()
            .into_iter()
            .filter_map(|app| hubs.get(&app.slot).map(|hub| (app.slot.clone(), hub)))
            .collect()
    }

    /// py: voice_agent.py:765 `def __init__( self, connection: Any,                                   # Live connection (async ctx manager already entered) mic: "asyncio.Queue[bytes]",                        # from Ears.subscribe() speaker: Any,                                       # Speaker-like: play/stop/busy agent_factory: Callable[[Callable[[AgentEvent], None], Callable[[str], Awaitable[str]]], ComputerAgent], on_state: Callable[[str], None], config: Optional[VoiceConfig] = None, clock: Callable[[], float] = time.monotonic, agent_enabled: bool = True, on_caption: Optional[Callable[[dict], None]] = None, caption_tick_secs: float = 0.05, watchdog_secs: float = 0.5,                          # how often the idle / cap watchdog looks on_explore: Optional[Callable[[], None]] = None, turn_gap_secs: float = TURN_GAP_SECS, scene: Any = None,                                  # scene.SceneWatcher-like: start/stop/look/locate head: Any = None,                                   # head.Head-like: move/yaw/pitch/clock/sleep intent: Optional[Callable[[str], Awaitable[Optional[str]]]] = None,   # intent.make_classifier(...) on_sound: Optional[Callable[[bool], None]] = None,  # the owner muted (False) / unmuted (True) muted: Callable[[], bool] = lambda: False, thinker: Optional[Callable[..., Awaitable[dict[str, Any]]]] = None,   # think.make_thinker(...) on_photo: Optional[Callable[[str], Awaitable[dict[str, Any]]]] = None,   # Daemon._photo_for_owner brief: str = "",                                    # the opening brief, "" when memory is off memory: Optional["Memory"] = None,                  # memory.Memory, None when memory is off conv: str = "",                                     # this conversation's transcript id ("" mints one) today: str = "",                                    # today's talk for the Live front (voice budget) backend_today: str = "",                            # today's talk for the backend (backend budget) learning: Optional[Callable[..., dict[str, Any]]] = None, think_aloud: Optional[dict[str, Any]] = None,       # the lesson, when opened for think out loud on_spoken_idea: Optional[Callable[[str, str], Any]] = None,   # LearningApp.append_spoken (blocking) on_think_aloud: Optional[Callable[[bool, Optional[str]], None]] = None,   # (listening, lesson id) lesson_wake: bool = False,                          # opened by the "lesson" wake word head_pose: Optional[Callable[[str], Awaitable[str]]] = None,   # typed_ask: utterance -> a pose label, or "none" gate: Any = None,              # voice_gate.SpeakerGate for this conversation, or None (today's behaviour) on_expression: Optional[Callable[[str, str], None]] = None, mac_busy: Callable[[], bool] = lambda: False,       # a task from another door (telegram.py) has the Mac meeter: Any = None,                                 # meet.Meeter, or None: "join my meeting" out loud **app_hubs: Any,                                    # eve's apps by slot (app_registry): "lights off" out loud ) -> None:`
    /// (`**app_hubs` is `options.app_hubs`, already checked against the registry when it was built.)
    pub fn new(connection: Arc<dyn LiveConnection>, mic: eve_ears::PcmQueue, speaker: Arc<dyn VoiceSpeaker>, agent_factory: AgentFactory, on_state: OnState, options: VoiceSessionOptions) -> Arc<Self> {
        let VoiceSessionOptions {
            config,
            clock,
            agent_enabled,
            on_caption,
            caption_tick_secs,
            watchdog_secs,
            on_explore,
            turn_gap_secs,
            scene,
            head,
            intent,
            on_sound,
            muted,
            thinker,
            on_photo,
            brief,
            memory,
            conv,
            today,
            backend_today,
            learning,
            think_aloud,
            on_spoken_idea,
            on_think_aloud,
            lesson_wake,
            head_pose,
            gate,
            on_expression,
            mac_busy,
            meeter,
            app_hubs,
            ledger,
            system_context,
            environ,
        } = options;
        let config = config.unwrap_or_default();
        let clock: Clock = clock.unwrap_or_else(|| Arc::new(monotonic));
        // Memory (owner, 2026-09-23): every word is kept on this Mac. Each closed turn goes to the day's
        // transcript through memory.transcripts the moment it closes, so a crash or a restart loses nothing
        // (the 2026-09-11 RAM-only choice is reversed). "Remember that" stars through memory.star: in the
        // owner's memory system only a human may star, and out loud is how the owner does it.
        let conv = match &memory {
            Some(m) if conv.is_empty() => m.transcripts().new_conv("voice"),
            _ => conv,
        };
        // The taint gate out loud (tool_gate.py): what outside text this conversation's prompt holds. It starts
        // tainted when today's blocks already carry a read's line or a marked line, and a read's result taints it
        // for the rest of the conversation (_slow_tool). Consequential native calls require a spoken yes
        // (_gate). Never reset by a new turn of the owner's: the words read stay in the conversation's context.
        let taint = eve_tool_gate::carried([&Value::String(today.clone()), &Value::String(backend_today.clone())], None)
            .to_string();
        // Captions: the pager decides which page is up and for how long; the board only draws.
        let pager = eve_caption_pager::CaptionPager::new(eve_caption_pager::PagerConfig::with_read_cps(config.caption_cps));
        let captions = config.output == "captions" && on_caption.is_some();
        let now = clock();
        let st = VoiceSessionState {
            expression_chars: 0,
            fast_head: None,
            // Think out loud: the lesson the learner is talking through, or None. While it is set the
            // session waits through long thinking pauses, saves what the learner says into the lesson's
            // ideas, and never logs or remembers the learner's words.
            think_aloud,
            // Lesson setup: the lesson window is open and no lesson has started. Until one does, every
            // owner turn the voice does not delegate goes to the backend as [lesson setup], so "fractions,
            // grade four" becomes a lesson start and not chit-chat.
            lesson_setup: lesson_wake,
            idea_saves: Vec::new(),
            end_reason: String::new(),
            // "Go away" was heard: the mic stops, and the session closes once eve's
            // goodbye (or a running task's result) has been said.
            farewell: false,
            farewell_since: f64::NEG_INFINITY,
            task_done_at: f64::NEG_INFINITY,
            last_reply_at: f64::NEG_INFINITY,
            slow_tasks: Vec::new(),
            // Head requests: the voice model often says "Looking." without delegating
            // (log, 2026-09-10 17:43), so the session routes them itself unless the
            // voice delegated that turn or a head tool already ran.
            user_turn_started_at: f64::NEG_INFINITY,
            delegated_at: f64::NEG_INFINITY,
            last_head_tool_at: f64::NEG_INFINITY,
            explore_requested: false,
            taint,
            held: None,
            owner_turns: 0,
            owner_yes: -1,
            // the owner's last turn, its bare yes/no ("": neither) and the question it answers: the one asked last
            // before it, "gate" (the taint gate's) or "task" (a computer task's), "" for none (round-4 review: one
            // spoken yes approved both a held call and a pending task question). A decision is used once.
            decision: None,
            answer_words: String::new(),
            question_turn: 0,
            asked_last: String::new(),
            // the held call's question, as code wrote it for the voice to say, and whether the voice's output
            // transcript has since held it whole (tool_gate.said_whole): a yes approves the held call only after it
            // was said
            held_act: String::new(),
            held_said: false,
            used_turn: -1,
            memory_tool_names: HashSet::new(),
            opened_wall: None,
            captured: 0,
            close_written: false,
            // Live has no transcript-done event; _Turns infers the boundary.
            turn_marks: _Turns::new(turn_gap_secs),
            // Both sides of the conversation, in order, as (who, text): the on_closed hand-off and "remember
            // that" read it. Think out loud keeps nothing here.
            turns: Vec::new(),
            caption: String::new(),
            pager,
            state_after_captions: None,
            state: "idle".into(),
            agent: None,
            agent_task: None,
            pending_answer: None,
            last_activity: now,
            started_at: now,
            tool_calls: Vec::new(),
            transcript: Vec::new(),
            // The Responses backend runs one response at a time. A function call is
            // reported before that response completes, so a response.create sent right
            // after a tool result would be rejected. Track the in-flight backend
            // response and defer our creates until it completes.
            response_active: false,
            response_wanted: false,
            reply_open: false,
            speaking_until: 0.0,
            last_progress_at: f64::NEG_INFINITY,
            live_secs: None,
            live_billed: false,
            backend_searches: 0,
        };
        Arc::new(Self {
            meeter,
            on_expression,
            mac_busy: mac_busy.unwrap_or_else(|| Arc::new(|| false)),
            head_pose,
            gate,
            learning,
            lesson_wake,
            on_spoken_idea,
            on_think_aloud,
            started: Flag::new(),
            conn: connection,
            scene,
            head,
            intent,
            thinker,
            on_photo,
            on_sound,
            muted: muted.unwrap_or_else(|| Arc::new(|| false)),
            on_caption,
            on_explore,
            mic,
            speaker,
            agent_factory,
            on_state,
            config,
            brief,
            memory,
            conv,
            today,
            backend_today,
            clock,
            captions,
            caption_tick: caption_tick_secs,
            watchdog_secs,
            agent_enabled,
            ended: Flag::new(),
            ledger: ledger.unwrap_or_default(),
            system_context: system_context
                .unwrap_or_else(|| Arc::new(|| eve_system_context::context(None, None))),
            environ,
            hubs: Mutex::new(app_hubs),
            st: Mutex::new(st),
        })
    }

    /// py: voice_agent.py:928 `def _set(self, state: str) -> None:`
    pub(crate) fn set(self: &Arc<Self>, state: &str) {
        {
            let mut st = self.st.lock();
            if st.state == state {
                return;
            }
            st.state = state.to_string();
        }
        guarded("on_state", || (self.on_state)(state));
    }

    /// py: voice_agent.py:937 `def task_running(self) -> bool:`
    ///
    /// The agent's own `running` flips inside run(); between start_task and
    /// the first yield the task exists but has not started — count that too.
    pub fn task_running(self: &Arc<Self>) -> bool {
        self.st.lock().agent_task.as_ref().is_some_and(|t| !t.done())
    }

    /// py: voice_agent.py:943 `async def run(self) -> None:`
    ///
    /// The conversation, start to close. A failure to open it (session.start refused, the connection gone)
    /// is logged and ends the call, where Python's `run()` raises before its `try`.
    ///
    /// A hush is dropping this future: the drop guard then does what Python's `finally` does after a
    /// `CancelledError` — the board cleared at once, the cut-off turn kept, the task cancelled, the close
    /// marker and the minutes written — except the awaits (the jobs and the agent are cancelled, not waited
    /// for; the scene is stopped in the background; queued audio is dropped rather than drained).
    pub async fn run(self: &Arc<Self>) {
        let listening = self.st.lock().think_aloud.is_some();
        self.set(if listening { "listening" } else { "wake" });
        {
            let now = (self.clock)();
            let mut st = self.st.lock();
            st.started_at = now;
            st.last_activity = now;
        }
        let mut profile = String::new();
        let mut backend_profile = String::new();
        let mut memory_tools: Vec<Obj> = Vec::new();
        if self.memory.is_some() {
            let opened = self.wall_now();
            self.st.lock().opened_wall = opened;
            if !listening {
                let me = self.clone();
                match tokio::task::spawn_blocking(eve_spend::carry(move || me.profiles())).await {
                    Ok((voice, full)) => {
                        profile = voice;
                        backend_profile = full;
                    }
                    Err(_) => tracing::warn!("voice: no profile this conversation (Exception)"),
                }
                memory_tools = self.memory_tools();
            }
        }
        let think_aloud = self.st.lock().think_aloud.clone();
        let meet_tools: Option<Vec<Obj>> = self
            .meeter
            .as_ref()
            .map(|_| MEET_TOOLS.as_array().into_iter().flatten().filter_map(|t| t.as_object().cloned()).collect());
        let hubs = self.hubs.lock().clone();
        let cfg = session_config_with(
            &self.config,
            &self.brief,
            think_aloud.as_ref(),
            &profile,
            &backend_profile,
            &self.today,
            &self.backend_today,
            Some(&memory_tools),
            meet_tools.as_deref(),
            &hubs,
            &(self.system_context)(),
            self.environ.as_ref(),
        );
        let front_chars = cfg.get("instructions").and_then(Value::as_str).map_or(0, |s| s.chars().count());
        let back_chars = cfg
            .get("delegation")
            .and_then(|d| d.get("responses"))
            .and_then(|r| r.get("instructions"))
            .and_then(Value::as_str)
            .map_or(0, |s| s.chars().count());
        tracing::info!("voice: prompt front {} chars, backend {} chars", front_chars, back_chars);
        if let Err(e) = self.conn.session_start(Value::Object(cfg)).await {
            tracing::warn!("voice: session.start failed: {}", e);
            return;
        }
        if let Err(e) = self.await_started(15.0).await {
            tracing::warn!("voice: {}", e);
            return;
        }
        self.started.set();
        // A fixed greeting needs no backend round-trip: commentary is context the
        // Live model speaks itself.
        let greeting = if listening {
            "The learner asked you to listen while they think out loud. Say 'I'm listening' in two or three words, then stay quiet while they think."
        } else if self.lesson_wake {
            "The owner just said your lesson word and the lesson window is already open. Say exactly: 'Lesson time. Learn a topic, or bring a problem?' and nothing else."
        } else {
            "The owner just said your wake word. Greet them in one or two words, like 'Yeah?'."
        };
        if let Err(e) = self.conn.commentary_append(greeting, None).await {
            tracing::warn!("voice: the greeting was not sent: {}", e);
            return;
        }
        if listening {
            self.notify_think_aloud(true);
        }
        if (self.muted)() {
            self.quiet(MUTED_NOTE).await;
        }
        if self.lesson_wake {
            // The daemon opened the window. The owner's answers reach the backend as [lesson setup].
            self.backend_note("[lesson setup] The owner said the lesson word. The lesson window is open and eve asked: learn a topic, or bring a problem? Their next words answer that.").await;
        }
        if let Some(scene) = &self.scene {
            scene.start(); // watches quietly; the look tool reads it on request
        }
        let pump = {
            let me = self.clone();
            Job::spawn("voice-mic", async move { me.pump_mic().await })
        };
        let watchdog = {
            let me = self.clone();
            Job::spawn("voice-watchdog", async move { me.watchdog().await })
        };
        let ticker = {
            let me = self.clone();
            Job::spawn("voice-ticks", async move { me.tick_loop().await })
        };
        let mut guard = RunGuard {
            session: self.clone(),
            jobs: vec![pump.clone(), watchdog.clone(), ticker.clone()],
            stage: Stage::Serving,
            armed: true,
        };
        self.until_ended().await;
        guard.stage = Stage::Cleaning;
        self.ended.set(); // every exit path: nothing may speak into a closing session
        pump.cancel();
        watchdog.cancel();
        let slow: Vec<Job> = self.st.lock().slow_tasks.clone();
        for t in &slow {
            t.cancel();
        }
        pump.join().await;
        watchdog.join().await;
        for t in &slow {
            t.join().await;
        }
        self.stop_scene().await; // the camera stops being looked at the moment we close
        if self.task_running() {
            let agent = self.st.lock().agent.clone();
            if let Some(agent) = agent {
                agent.cancel("the conversation closed");
            }
        }
        let agent_task = self.st.lock().agent_task.clone();
        if let Some(t) = agent_task {
            t.join().await;
        }
        ticker.cancel(); // one poller at a time: the loop stops before the drain
        ticker.join().await;
        let closing = self.st.lock().turn_marks.flush(); // the last turn never went quiet; close it now
        if let Some((who, text)) = closing {
            self.close_turn(&who, &text);
        }
        self.write_close(); // nothing more is said: mark it before the caption drain
        if self.captions {
            guard.stage = Stage::Draining;
            self.drain_captions(15.0).await;
            guard.stage = Stage::Cleaning;
        }
        self.drain_speaker(15.0).await;
        self.speaker.stop();
        let saves: Vec<Job> = self.st.lock().idea_saves.clone();
        // A line the learner said just before the close is still being written to the lesson.
        for t in &saves {
            t.join().await;
        }
        let was_listening = self.st.lock().think_aloud.take().is_some();
        if was_listening {
            self.notify_think_aloud(false);
        }
        self.set("idle");
        guard.armed = false;
        self.write_close();
        self.bill_live();
    }

    /// Python's `run()` after a `CancelledError` (the daemon's touch hush), without its awaits: see [`Self::run`].
    fn hushed(self: &Arc<Self>, jobs: &[Job]) {
        self.ended.set();
        for j in jobs {
            j.cancel();
        }
        let slow: Vec<Job> = self.st.lock().slow_tasks.clone();
        for t in &slow {
            t.cancel();
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let me = self.clone();
            handle.spawn(async move { me.stop_scene().await });
        }
        if self.task_running() {
            let agent = self.st.lock().agent.clone();
            if let Some(agent) = agent {
                agent.cancel("the conversation closed");
            }
        }
        let closing = {
            let mut st = self.st.lock();
            let closing = st.turn_marks.flush();
            match closing {
                // A hush cut it off: nothing is shown or acted on, but what was said is still kept.
                Some((who, text)) if transcript_who(&who).is_some() && st.think_aloud.is_none() => {
                    st.turns.push((who.clone(), text.clone()));
                    Some((who, text))
                }
                _ => None,
            }
        };
        if let Some((who, text)) = closing {
            self.capture(transcript_who(&who).unwrap_or("eve"), "say", &text, "", false);
        }
        self.write_close();
        if self.captions {
            let now = (self.clock)();
            let events = self.st.lock().pager.reset(now);
            self.emit_events(&events); // hush (a touch on the robot): clear the board at once
        }
        self.speaker.stop();
        let was_listening = self.st.lock().think_aloud.take().is_some();
        if was_listening {
            self.notify_think_aloud(false);
        }
        self.set("idle");
    }

    /// py: voice_agent.py:1038 `def _note_live_usage(self, event: Any) -> None:`
    ///
    /// Keep the Live session's cumulative audio seconds (a total, not an increment: the largest one wins).
    pub(crate) fn note_live_usage(self: &Arc<Self>, event: &Value) {
        let usage = attr(event, "usage");
        let usage = if eve_computer_agent::py::truthy(&usage) { usage } else { Value::Object(Obj::new()) };
        if let Some(seconds) = attr(&usage, "seconds").as_f64() {
            let mut st = self.st.lock();
            let so_far = st.live_secs.unwrap_or(0.0);
            st.live_secs = Some(if so_far > seconds { so_far } else { seconds });
        }
    }

    /// py: voice_agent.py:1044 `def _bill_live(self) -> None:`
    ///
    /// The session's minutes, once, when it closes. Billed seconds come from the server's usage events; a
    /// session that never sent one is metered by its own clock and says so in the note.
    pub(crate) fn bill_live(self: &Arc<Self>) {
        let (seconds, note) = {
            let mut st = self.st.lock();
            if st.live_billed {
                return;
            }
            st.live_billed = true;
            match st.live_secs {
                Some(s) => (s, ""),
                None => {
                    let elapsed = (self.clock)() - st.started_at;
                    (if elapsed > 0.0 { elapsed } else { 0.0 }, "clock-timed: no usage event")
                }
            }
        };
        if seconds > 0.0 {
            self.ledger.record_live(eve_spend::VOICE, &self.config.model, seconds, note);
        }
    }

    /// py: voice_agent.py:1058 `def _bill_backend(self, response: Any) -> None:`
    ///
    /// One delegated Responses call: its usage at the backend model's rates, plus its hosted searches.
    pub(crate) fn bill_backend(self: &Arc<Self>, response: &Value) {
        let searches = std::mem::take(&mut self.st.lock().backend_searches);
        let body = response.as_object().cloned().unwrap_or_default();
        if !body.get("usage").is_some_and(Value::is_object) {
            return; // a lifecycle snapshot without usage (a failure before any tokens)
        }
        let model = match body.get("model") {
            Some(m) if eve_computer_agent::py::truthy(m) => eve_computer_agent::py::value_str(m),
            _ => self.config.backend_model.clone(),
        };
        // the Live API's backend: still OpenAI's
        self.ledger.record_response(eve_spend::VOICE, &Value::Object(body), &model, "openai", Some(searches));
    }

    /// py: voice_agent.py:1068 `def _wall_now(self) -> Optional[datetime]:`
    ///
    /// When this conversation opened, for recent_conversation. Floored to the second: transcript lines
    /// carry whole seconds, so a text sent in the opening second is still newer than this.
    pub(crate) fn wall_now(self: &Arc<Self>) -> Option<chrono::DateTime<chrono::FixedOffset>> {
        use chrono::Timelike;
        let memory = self.memory.as_ref()?;
        let now = memory.transcripts().now();
        Some(now.with_nanosecond(0).unwrap_or(now))
    }

    /// py: voice_agent.py:1076 `def _profiles(self) -> tuple[str, str]:`
    ///
    /// (voice profile, full profile), off the loop: a few file reads.
    pub(crate) fn profiles(self: &Arc<Self>) -> (String, String) {
        match &self.memory {
            None => (String::new(), String::new()),
            Some(memory) => (memory.profile_for_voice(), memory.profile()),
        }
    }

    /// py: voice_agent.py:1087 `def _memory_tools(self) -> list[dict[str, Any]]:`
    ///
    /// The backend's memory tools, in memory.py's stable order. Their names route in _tool.
    pub fn memory_tools(self: &Arc<Self>) -> Vec<Obj> {
        let Some(memory) = &self.memory else {
            return Vec::new();
        };
        let named = |t: &Obj| t.get("name").is_some_and(eve_computer_agent::py::truthy);
        let tools: Vec<Obj> = memory.tools(true).into_iter().filter_map(|t| t.as_object().cloned()).filter(named).collect();
        let mut taken: HashSet<String> = TOOLS
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string))
            .collect();
        taken.insert(eve_web::websearch::TOOL_NAME.to_string());
        let name_of = |t: &Obj| t.get("name").map(eve_computer_agent::py::value_str).unwrap_or_default();
        let tools: Vec<Obj> = tools.into_iter().filter(|t| !taken.contains(&name_of(t))).collect();
        self.st.lock().memory_tool_names = tools.iter().map(name_of).collect();
        tools
    }

    /// py: voice_agent.py:1102 `def _capture(self, who: str, kind: str, text: str, tool: str = "", *, outside: bool = False) -> None:`
    ///
    /// One transcript line for this conversation. Never while a learner thinks out loud; never raises.
    /// ``outside``: words eve did not write (a reply after a read, a task's sentence), kept marked
    /// (transcripts ``out``), so a restart cannot launder them into eve's own (v3 safety review).
    pub(crate) fn capture(self: &Arc<Self>, who: &str, kind: &str, text: &str, tool: &str, outside: bool) {
        let Some(memory) = &self.memory else {
            return;
        };
        if self.st.lock().think_aloud.is_some() || self.conv.is_empty() || py_strip(text).is_empty() {
            return;
        }
        let opts = eve_transcripts::Append { tool: tool.to_string(), outside, ..Default::default() };
        if memory.transcripts().append_with("voice", &self.conv, who, kind, text, opts) {
            self.st.lock().captured += 1;
        }
    }

    /// py: voice_agent.py:1116 `def _write_close(self) -> None:`
    ///
    /// The close marker, once, when this conversation wrote anything.
    pub(crate) fn write_close(self: &Arc<Self>) {
        let Some(memory) = &self.memory else {
            return;
        };
        {
            let mut st = self.st.lock();
            if st.close_written || st.captured == 0 {
                return;
            }
            st.close_written = true;
        }
        memory.transcripts().close("voice", &self.conv, None);
    }

    /// py: voice_agent.py:1127 `def _capture_result(self, name: str, result: Any) -> None:`
    ///
    /// Keep what an answer-bearing tool returned, as one tool line of at most TOOL_LINE_CHARS.
    pub(crate) fn capture_result(self: &Arc<Self>, name: &str, result: &Value) {
        let Some(result) = result.as_object() else {
            return;
        };
        if !ANSWER_TOOLS.contains(&name) || result.get("ok") == Some(&Value::Bool(false)) {
            return;
        }
        let mut text = String::new();
        for key in ["answer", "caption", "view", "text", "summary"] {
            if let Some(Value::String(value)) = result.get(key) {
                let stripped = py_strip(value);
                if !stripped.is_empty() {
                    text = stripped.to_string();
                    break;
                }
            }
        }
        if text.is_empty() {
            let rest: Obj = result.iter().filter(|(k, _)| k.as_str() != "ok").map(|(k, v)| (k.clone(), v.clone())).collect();
            if !rest.is_empty() {
                text = eve_learning::py::dumps(&Value::Object(rest), false);
            }
        }
        let clipped: String = text.chars().take(TOOL_LINE_CHARS).collect();
        self.capture("eve", "tool", &clipped, name, false);
    }

    /// py: voice_agent.py:1142 `async def _memory_tool(self, name: str, args: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// A memory lookup in a worker thread, waited on for MEMORY_TIMEOUT_SECS at most.
    ///
    /// On a timeout the thread finishes on its own; the backend hears MEMORY_TIMEOUT_LINE and the
    /// conversation goes on. The log carries the tool, the hit count and the milliseconds, never the query.
    pub async fn memory_tool(self: &Arc<Self>, name: &str, args: &Obj) -> Obj {
        let Some(memory) = self.memory.clone() else {
            return obj(json!({"ok": false, "reason": "memory is off on this computer"}));
        };
        let t0 = std::time::Instant::now();
        let since = self.st.lock().opened_wall;
        let (tool, given) = (name.to_string(), Value::Object(args.clone()));
        let work = tokio::task::spawn_blocking(eve_spend::carry(move || memory.handle_tool(&tool, &given, since, "voice")));
        let result = match tokio::time::timeout(std::time::Duration::from_secs_f64(MEMORY_TIMEOUT_SECS), work).await {
            Err(_) => {
                tracing::warn!("voice: {} → timed out after {:.1} s", name, MEMORY_TIMEOUT_SECS);
                return obj(json!({"ok": false, "reason": MEMORY_TIMEOUT_LINE}));
            }
            Ok(Ok(Value::Object(m))) => m,
            Ok(_) => obj(json!({"ok": false, "reason": format!("{name} failed")})),
        };
        let hits: usize = result.values().filter_map(Value::as_array).map(Vec::len).sum();
        let ok = result.get("ok").map_or_else(|| "None".to_string(), eve_computer_agent::py::value_repr);
        tracing::info!("voice: {} → ok={} hits={} in {} ms", name, ok, hits, t0.elapsed().as_millis());
        result
    }

    /// py: voice_agent.py:1165 `def _think_context(self) -> str:`
    ///
    /// The owner's profile and today's talk, for think_hard (think.py bounds it). Off the loop.
    pub(crate) fn think_context(self: &Arc<Self>) -> String {
        let Some(memory) = &self.memory else {
            return String::new();
        };
        let profile = memory.profile();
        let today = memory.today(THINK_TODAY_CHARS as i64, &eve_transcripts::Today::default());
        [py_strip(&profile), py_strip(&today)].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n\n")
    }

    /// py: voice_agent.py:1177 `async def _await_started(self, timeout: float = 15.0) -> None:`
    ///
    /// Audio sent before `session.started` is discarded, so wait for it. An `error` event first is the session
    /// refused (Python raises RuntimeError out of `run()`); the connection failing is passed on the same way.
    pub(crate) async fn await_started(self: &Arc<Self>, timeout: f64) -> anyhow::Result<()> {
        let wait = async {
            while let Some(event) = self.conn.next_event().await {
                let event = event?;
                let t = attr(&event, "type");
                if t == "session.started" {
                    return Ok(());
                }
                if t == "error" {
                    anyhow::bail!("live: session start rejected: {}", eve_computer_agent::py::value_str(&attr(&event, "error")));
                }
                self.handle_event(&event).await;
            }
            Ok(())
        };
        match tokio::time::timeout(std::time::Duration::from_secs_f64(timeout), wait).await {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!("voice: no session.started within {:.0} s — continuing anyway", timeout);
                Ok(())
            }
        }
    }

    /// py: voice_agent.py:1192 `def end(self) -> None:`
    pub fn end(self: &Arc<Self>) {
        self.finish("ended");
    }

    /// py: voice_agent.py:1195 `def _finish(self, reason: str) -> None:`
    ///
    /// Close the session, remembering the first reason (cap, silence, goodbye, stopped, ...).
    pub(crate) fn finish(self: &Arc<Self>, reason: &str) {
        if !self.ended.is_set() {
            self.st.lock().end_reason = reason.to_string();
        }
        self.ended.set();
    }

    /// py: voice_agent.py:1203 `def ended(self) -> bool:`
    pub fn ended(self: &Arc<Self>) -> bool {
        self.ended.is_set()
    }

    /// py: voice_agent.py:1207 `def think_aloud_lesson_id(self) -> Optional[str]:`
    pub fn think_aloud_lesson_id(self: &Arc<Self>) -> Option<String> {
        let st = self.st.lock();
        st.think_aloud.as_ref().map(|l| l.get("id").map_or_else(|| "None".to_string(), eve_computer_agent::py::value_str))
    }

    /// py: voice_agent.py:1210 `def _notify_think_aloud(self, on: bool) -> None:`
    pub(crate) fn notify_think_aloud(self: &Arc<Self>, on: bool) {
        let Some(callback) = &self.on_think_aloud else {
            return;
        };
        let lesson_id = if on { self.think_aloud_lesson_id() } else { None };
        guarded("on_think_aloud", || callback(on, lesson_id.as_deref()));
    }

    /// py: voice_agent.py:1218 `async def enter_think_aloud(self, lesson: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// Switch an open conversation to think out loud (a voice request, or a toggle while talking).
    ///
    /// The voice's instructions are fixed at session.start, so its listening rules and the lesson go in
    /// as silent context; the backend's instructions are replaced through session.update.
    pub async fn enter_think_aloud(self: &Arc<Self>, lesson: &Obj) -> Obj {
        if self.ended.is_set() {
            return obj(json!({"ok": false, "reason": "the conversation is closing"}));
        }
        if tokio::time::timeout(std::time::Duration::from_secs_f64(8.0), self.started.wait()).await.is_err() {
            return obj(json!({"ok": false, "reason": "the conversation is not connected yet"}));
        }
        if self.ended.is_set() {
            return obj(json!({"ok": false, "reason": "the conversation is closing"}));
        }
        if self.task_running() {
            return obj(json!({"ok": false, "reason": "eve is busy with a computer task. Stop it first, then think out loud."}));
        }
        let same = {
            let now = (self.clock)();
            let mut st = self.st.lock();
            let same = st.think_aloud.as_ref().is_some_and(|l| l.get("id") == lesson.get("id"));
            st.think_aloud = Some(lesson.clone());
            st.last_activity = now;
            same
        };
        if same {
            return obj(json!({"ok": true, "already": true}));
        }
        self.quiet(&eve_learning::think_aloud::voice_instructions(Some(lesson))).await;
        let instructions = format!(
            "{}{}{}{}",
            BACKEND_INSTRUCTIONS,
            self.config.about,
            (self.system_context)(),
            eve_learning::think_aloud::backend_instructions(Some(lesson))
        );
        let update = json!({"delegation": {"type": "responses", "responses": {"instructions": instructions}}});
        if let Err(e) = self.conn.session_update(update).await {
            // The voice still has the rules; the backend keeps its lesson rules from BACKEND_INSTRUCTIONS.
            tracing::warn!("voice: could not give the backend the listening rules ({})", e);
        }
        tracing::info!("voice: think out loud on");
        self.notify_think_aloud(true);
        obj(json!({"ok": true}))
    }

    /// py: voice_agent.py:1250 `def prepare_think_aloud(self, lesson: dict[str, Any]) -> None:`
    ///
    /// Before run(): open this session for think out loud (asked for while it was connecting).
    pub fn prepare_think_aloud(self: &Arc<Self>, lesson: &Obj) {
        if !self.started.is_set() {
            self.st.lock().think_aloud = Some(lesson.clone());
        }
    }

    /// py: voice_agent.py:1255 `def stop_think_aloud(self, reason: str = "stopped") -> None:`
    ///
    /// The toggle or the command stopped listening: the microphone closes now.
    pub fn stop_think_aloud(self: &Arc<Self>, reason: &str) {
        if self.st.lock().think_aloud.is_some() {
            tracing::info!("voice: think out loud off ({})", reason);
        }
        self.finish(reason);
    }

    /// py: voice_agent.py:1261 `def _save_spoken_idea(self, text: str) -> None:`
    ///
    /// Append one finished learner turn to the lesson's ideas, off the event loop and after the ones before it.
    ///
    /// A dismissal ("I'm done", "stop listening") is a command, not thinking, and is not saved.
    pub(crate) fn save_spoken_idea(self: &Arc<Self>, text: &str) {
        let lesson_id = self.think_aloud_lesson_id().unwrap_or_default();
        let Some(save) = self.on_spoken_idea.clone() else {
            return;
        };
        if lesson_id.is_empty() {
            return;
        }
        if eve_learning::think_aloud::said_done(&eve_intent::normalize(text))
            || eve_intent::fast_intent(text).is_some_and(|label| LISTENING_INTENTS.contains(&label))
        {
            return;
        }
        let me = self.clone();
        let text = text.to_string();
        let mut st = self.st.lock();
        st.idea_saves.retain(|t| !t.done());
        let before: Vec<Job> = st.idea_saves.clone();
        let task = Job::spawn("voice-idea", async move {
            // One save at a time, in the order spoken: parallel threads could append the ideas out of order.
            for t in &before {
                t.join().await;
            }
            let ok = match tokio::task::spawn_blocking(eve_spend::carry(move || save(&lesson_id, &text))).await {
                Ok(Ok(())) => true,
                Ok(Err(e)) => {
                    tracing::warn!("voice: spoken idea not saved ({})", e);
                    false
                }
                Err(e) => {
                    tracing::warn!("voice: spoken idea not saved ({})", e);
                    false
                }
            };
            tracing::info!("voice: spoken idea saved ok={}", if ok { "True" } else { "False" }); // never the words
            me.st.lock().idea_saves.retain(|t| !t.is_current());
        });
        st.idea_saves.push(task);
    }

    // ---- what Python's callers read as plain attributes ------------------------------------------------------

    /// `session.state`: the board's phase now.
    pub fn state(&self) -> String {
        self.st.lock().state.clone()
    }

    /// `session.end_reason`: why it closed ("" while it runs).
    pub fn end_reason(&self) -> String {
        self.st.lock().end_reason.clone()
    }

    /// `session.turns`: both sides of the conversation, in order, as (who, text).
    pub fn turns(&self) -> Vec<(String, String)> {
        self.st.lock().turns.clone()
    }

    /// `session.transcript`: what eve said, a line per closed turn.
    pub fn transcript(&self) -> Vec<String> {
        self.st.lock().transcript.clone()
    }

    /// `session.tool_calls`: every tool the backend called, with its arguments.
    pub fn tool_calls(&self) -> Vec<(String, Obj)> {
        self.st.lock().tool_calls.clone()
    }

    /// `session.explore_requested`: the owner sent eve off to explore.
    pub fn explore_requested(&self) -> bool {
        self.st.lock().explore_requested
    }
}

/// A `json!({...})` object as a map.
fn obj(value: Value) -> Obj {
    match value {
        Value::Object(m) => m,
        _ => Obj::new(),
    }
}

/// Test access: what Python's tests read and set on the session's underscored attributes, and the private steps
/// they call directly. Not the daemon's API.
impl VoiceSession {
    #[doc(hidden)]
    pub fn test_started(&self) -> bool {
        self.started.is_set()
    }

    #[doc(hidden)]
    pub fn test_open_turn(&self) -> String {
        self.st.lock().turn_marks.text.clone()
    }

    #[doc(hidden)]
    pub fn test_taint(&self) -> String {
        self.st.lock().taint.clone()
    }

    #[doc(hidden)]
    pub fn test_set_state(&self, state: &str) {
        self.st.lock().state = state.to_string();
    }

    #[doc(hidden)]
    pub fn test_set_think_aloud(&self, lesson: Option<Obj>) {
        self.st.lock().think_aloud = lesson;
    }

    #[doc(hidden)]
    pub fn test_set_agent(&self, agent: Option<Arc<dyn TaskAgent>>) {
        self.st.lock().agent = agent;
    }

    /// The computer task's job and the slow tools' jobs (`_agent_task`, `_slow_tasks`).
    #[doc(hidden)]
    pub fn test_jobs(&self) -> Vec<Job> {
        let st = self.st.lock();
        st.agent_task.iter().cloned().chain(st.slow_tasks.iter().cloned()).collect()
    }

    #[doc(hidden)]
    pub fn test_reset_pager(&self, now: f64) {
        let _ = self.st.lock().pager.reset(now);
    }

    #[doc(hidden)]
    pub fn test_close_turn(self: &Arc<Self>, who: &str, text: &str) {
        self.close_turn(who, text);
    }

    #[doc(hidden)]
    pub fn test_start_task(self: &Arc<Self>, goal: &str) -> Obj {
        self.start_task(goal)
    }

    #[doc(hidden)]
    pub async fn test_think_hard(self: &Arc<Self>, question: &str) -> Obj {
        self.think_hard(question).await
    }

    #[doc(hidden)]
    pub async fn test_tool(self: &Arc<Self>, name: &str, call_id: &str, arguments: &str) {
        self.tool(name, call_id, arguments).await;
    }

    #[doc(hidden)]
    pub fn test_on_agent_event(self: &Arc<Self>, ev: &eve_agent_contract::AgentEvent) {
        self.on_agent_event(ev);
    }

    #[doc(hidden)]
    pub fn test_capture(self: &Arc<Self>, who: &str, kind: &str, text: &str, tool: &str, outside: bool) {
        self.capture(who, kind, text, tool, outside);
    }

    #[doc(hidden)]
    pub fn test_capture_result(self: &Arc<Self>, name: &str, result: &Value) {
        self.capture_result(name, result);
    }

    #[doc(hidden)]
    pub fn test_write_close(self: &Arc<Self>) {
        self.write_close();
    }

    /// (`_captured`, `_close_written`)
    #[doc(hidden)]
    pub fn test_captured(&self) -> (i64, bool) {
        let st = self.st.lock();
        (st.captured, st.close_written)
    }

    #[doc(hidden)]
    pub fn test_note_live_usage(self: &Arc<Self>, event: &Value) {
        self.note_live_usage(event);
    }

    #[doc(hidden)]
    pub fn test_live_secs(&self) -> Option<f64> {
        self.st.lock().live_secs
    }

    #[doc(hidden)]
    pub fn test_bill_live(self: &Arc<Self>) {
        self.bill_live();
    }

    #[doc(hidden)]
    pub fn test_add_backend_search(&self) {
        self.st.lock().backend_searches += 1;
    }

    #[doc(hidden)]
    pub fn test_bill_backend(self: &Arc<Self>, response: &Value) {
        self.bill_backend(response);
    }

    /// `_started_at = _last_activity = clock()`, as `run()` does first.
    #[doc(hidden)]
    pub fn test_mark_started_at(&self) {
        let now = (self.clock)();
        let mut st = self.st.lock();
        st.started_at = now;
        st.last_activity = now;
    }

    #[doc(hidden)]
    pub async fn test_await_started(self: &Arc<Self>, timeout: f64) -> anyhow::Result<()> {
        self.await_started(timeout).await
    }

    #[doc(hidden)]
    pub fn test_finish(self: &Arc<Self>, reason: &str) {
        self.finish(reason);
    }

    #[doc(hidden)]
    pub fn test_think_context(self: &Arc<Self>) -> String {
        self.think_context()
    }

    #[doc(hidden)]
    pub fn test_profiles(self: &Arc<Self>) -> (String, String) {
        self.profiles()
    }

    #[doc(hidden)]
    pub fn test_set_opened_wall(self: &Arc<Self>) {
        let opened = self.wall_now();
        self.st.lock().opened_wall = opened;
    }

    #[doc(hidden)]
    pub fn test_think_aloud(&self) -> Option<Obj> {
        self.st.lock().think_aloud.clone()
    }

    #[doc(hidden)]
    pub fn test_memory_tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.st.lock().memory_tool_names.iter().cloned().collect();
        names.sort();
        names
    }

    #[doc(hidden)]
    pub fn test_notify_think_aloud(self: &Arc<Self>, on: bool) {
        self.notify_think_aloud(on);
    }

    #[doc(hidden)]
    pub fn test_set_started(&self) {
        self.started.set();
    }
}
