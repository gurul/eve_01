//! "hey eve": the ears and the wake word, the conversation (`_converse`), the board's hold-to-talk desk calls,
//! the speaker gate, the head-pose asker, the agent's resync and keepalive, the room notes and the phone's notes,
//! and the conversation's hands (the terminal, the Mac's busy flags).

use std::sync::Arc;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::daemon::Daemon;
use crate::lesson::{learning_error_name, py_repr};
use crate::seams::{HeadAsker, Inlet, Mic, Obj};

/// The speaker gate as the voice session takes it: `eve_voice_gate::SpeakerGate` behind a lock, logging its
/// summary when the conversation ends (Python logged `gate.summary()` in `_converse`'s finally).
pub struct VoiceGateHandle(pub Arc<Mutex<eve_voice_gate::SpeakerGate>>);

impl eve_brain::seams::SpeakerGate for VoiceGateHandle {
    fn set_bypass(&self, on: bool) {
        self.0.lock().bypass = on;
    }
    fn set_lenient(&self, on: bool) {
        self.0.lock().lenient = on;
    }
    fn process(&self, raw: &[u8]) -> Vec<Vec<u8>> {
        self.0.lock().process(raw)
    }
}

/// The speaker model loaded once for the daemon's life (`self._voice_embedder`), lent to each conversation's gate.
struct SharedEmbedder(Arc<Mutex<eve_voice_gate::SherpaEmbedder>>);

impl eve_voice_gate::Embedder for SharedEmbedder {
    fn embed(&mut self, pcm: &[u8]) -> Result<Option<Vec<f64>>, eve_voice_gate::GateError> {
        eve_voice_gate::Embedder::embed(&mut *self.0.lock(), pcm)
    }
}

/// scene.SceneWatcher as the voice session takes it (`scene=self._scene`): look and locate for the head and the
/// look tool, start and stop with the conversation.
pub struct SceneHandle(pub Arc<eve_scene::SceneWatcher>);

#[async_trait::async_trait]
impl eve_head::Scene for SceneHandle {
    async fn look(&self, fresh_secs: f64, newer_than: Option<f64>) -> Value {
        Value::Object(self.0.look(Some(fresh_secs), newer_than).await)
    }
    async fn locate(&self, target: &str, newer_than: Option<f64>) -> Value {
        Value::Object(self.0.locate(target, newer_than).await)
    }
}

#[async_trait::async_trait]
impl eve_brain::seams::SceneWatch for SceneHandle {
    fn start(&self) {
        self.0.start(true);
    }
    async fn stop(&self) {
        self.0.stop().await;
    }
}

/// The wake word's microphone as a room-notes audio source (`RoomNotes(..., self._ears, ...)`): each subscriber's
/// queue is the ears' own, handed back to the ears on unsubscribe.
pub struct MicSource {
    mic: Arc<dyn Mic>,
    subs: Mutex<Vec<(Arc<dyn eve_notes::MicQueue>, eve_ears::PcmQueue)>>,
}

impl MicSource {
    pub fn new(mic: Arc<dyn Mic>) -> Self {
        MicSource { mic, subs: Mutex::new(Vec::new()) }
    }
}

struct PcmMicQueue(eve_ears::PcmQueue);

impl eve_notes::MicQueue for PcmMicQueue {
    fn get(&self) -> BoxFuture<'_, Vec<u8>> {
        Box::pin(self.0.get())
    }
}

impl eve_notes::AudioSource for MicSource {
    fn listening(&self) -> bool {
        self.mic.listening()
    }
    fn subscribe(&self) -> Arc<dyn eve_notes::MicQueue> {
        let q = self.mic.subscribe();
        let handed: Arc<dyn eve_notes::MicQueue> = Arc::new(PcmMicQueue(q.clone()));
        self.subs.lock().push((handed.clone(), q));
        handed
    }
    fn unsubscribe(&self, q: &Arc<dyn eve_notes::MicQueue>) {
        let found = {
            let mut subs = self.subs.lock();
            subs.iter().position(|(h, _)| eve_notes::same_queue(h, q)).map(|i| subs.remove(i).1)
        };
        if let Some(pcm) = found {
            self.mic.unsubscribe(&pcm);
        }
    }
}

/// `time.perf_counter()`.
fn perf_counter() -> f64 {
    use std::sync::LazyLock;
    use std::time::Instant;
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_secs_f64()
}

/// `_converse`'s finally: what closes a conversation however it ends, including a hush (the task cancelled, which
/// in Rust drops the conversation's future). `finished` is false on a hush: then the explore wish and a pending
/// listen are dropped first, as Python's `except asyncio.CancelledError` did.
struct ConverseClose {
    daemon: Arc<Daemon>,
    ears: Arc<dyn Mic>,
    mic: eve_ears::PcmQueue,
    keepalive: crate::seams::Job,
    gate: Option<Arc<VoiceGateHandle>>,
    finished: bool,
}

impl Drop for ConverseClose {
    fn drop(&mut self) {
        let d = &self.daemon;
        if !self.finished {
            let mut st = d.st.lock();
            st.explore_after_conversation = None; // hushed: stay put
            st.think_aloud_wish = None; // a hush or a stop cancels a pending listen too
        }
        self.keepalive.cancel();
        self.ears.unsubscribe(&self.mic); // the microphone stops reaching the session here
        if let Some(gate) = &self.gate {
            let mut g = gate.0.lock();
            info!("voice gate: {}", py_repr(&g.summary())); // counts and seconds only: never words, never audio
            g.reset(); // the enrolled voice does not outlive the conversation
        }
        let (listening, inlet) = {
            let mut st = d.st.lock();
            st.voice_session = None;
            (st.think_aloud_state != "off", st.telegram.clone())
        };
        if listening {
            d.on_think_aloud(false, None);
        }
        if let Some(chief) = inlet.and_then(|i| i.chief()) {
            // a held reminder or a queued Mac step may go now
            if chief.breakpoint("wake_end", None).is_err() {
                // the chief never costs the conversation its close
                warn!("chief: the wake breakpoint failed ({})", "ChiefError");
            }
        }
        d.on_agent_state("idle");
        let wish = d.st.lock().think_aloud_wish.clone();
        if let Some(wish) = wish {
            if !d.shutting_down() {
                // Listening was asked for while this conversation was closing: open it now.
                d.st.lock().think_aloud_wish = None;
                d.start_think_aloud_conversation(wish);
            }
        }
    }
}

impl Daemon {
    /// `_start_ears(loop)`: the wake word's microphone, when voice is on and the model loads; stored as `ears`.
    pub fn start_ears(self: &Arc<Self>, handle: &tokio::runtime::Handle) {
        if !self.ears_cfg.enabled {
            info!("ears: disabled (EVE_VOICE=0)");
            return;
        }
        let wake = Arc::downgrade(self);
        let quiet = Arc::downgrade(self);
        let ears = eve_ears::Ears::new(
            self.ears_cfg.clone(),
            Arc::new(move |keyword: String| {
                if let Some(d) = wake.upgrade() {
                    d.on_wake(&keyword);
                }
            }),
            handle.clone(),
        )
        .with_suppressed(Arc::new(move || quiet.upgrade().is_some_and(|d| d.wake_suppressed())))
        // sounddevice is one module for the whole daemon: the desk calls' microphone shares its device table
        .with_backend(eve_desk_call::audio_backend());
        // Load the model now so a missing one shows at boot; the mic itself opens
        // and closes in _apply_mic, to the policy in _mic_wanted.
        if !ears.prepare() {
            self.st.lock().ears = None;
            return;
        }
        self.st.lock().ears = Some(Arc::new(crate::seams::EarsMic(Arc::new(ears))));
        let (mic_on, mic_path) = {
            let mic = self.mic.lock();
            (mic.on, mic.path.clone())
        };
        if !mic_on {
            info!(
                "ears: microphone off (owner's choice, {}) — `eve-bridge mic on` turns it back on",
                mic_path.display()
            );
        } else if !self.ears_cfg.always {
            info!(
                "ears: the microphone opens when the robot connects and closes when it leaves (EVE_MIC_ALWAYS=1 keeps it open)"
            );
        }
        self.apply_mic("boot");
        if !eve_brain::voice_agent::live_available(self.environ()) {
            warn!(
                "voice: OPENAI_API_KEY not set — eve will hear its name but the desk voice (OpenAI's Live API, not yet on OpenRouter) cannot talk back; everything else runs on OPENROUTER_API_KEY"
            );
        }
        info!(
            "agent: Codex Computer Use {}; app permissions are requested by Codex per task",
            if self.agent_cfg.enabled { "ready" } else { "disabled (EVE_COMPUTER_CONTROL=0)" }
        );
    }

    /// `_wake_suppressed()`: a wake now would be ignored (a conversation is open, the mic is off, a lesson is
    /// listening, a desk call holds the mic).
    ///
    /// Reasons not to wake: the human is dictating, a conversation is already open, or the board's button is
    /// held to talk. The button is the board's last ptt edge, not the desk call's `held`: a call that ends
    /// mid-hold drops `held`, and a press during a conversation never reaches the desk
    /// (verification/Eve/DeskCall.lean).
    pub fn wake_suppressed(&self) -> bool {
        let st = self.st.lock();
        st.listen_down || st.conversation.as_ref().is_some_and(|c| !c.done()) || st.ptt_held
    }

    /// `_desk_calls_get()`: the board's hold-to-talk calls, made on the first press (needs the text door).
    ///
    /// They borrow the Telegram chat as the brain and the phone call's OpenAI voice, and wait while the phone is
    /// on a call. Each call speaks on the Voice PE when the controller is connected as it starts, else on the Mac.
    pub fn desk_calls_get(self: &Arc<Self>) -> Option<Arc<eve_desk_call::DeskCalls>> {
        if let Some(calls) = self.st.lock().desk_calls.clone() {
            return Some(calls);
        }
        let busy_of = Arc::downgrade(self);
        let phone_busy: eve_desk_call::Busy = Arc::new(move || {
            // Only a phone call that has the chat and is using it (phone_call.IDLE_YIELD_SECS): an open, unused
            // call must not hold the Voice PE's button.
            let Some(d) = busy_of.upgrade() else { return false };
            let miniapp = d.st.lock().miniapp.clone();
            let Some(miniapp) = miniapp else { return false };
            let calls = miniapp.server.calls.lock().clone();
            match calls {
                Some(calls) if calls.active().is_some() => calls.holding_chat(),
                _ => false,
            }
        });
        let controller = self.controller.clone();
        let speaker: eve_desk_call::SpeakerFactory = Arc::new(move || {
            // eve's reply on the Voice PE when it is the connected controller, else on the Mac.
            if let Some(ctl) = controller.as_ref().filter(|c| c.connected()) {
                let ctl = ctl.clone();
                let send: eve_desk_call::BoardSend = Arc::new(move |obj: Value| {
                    let ctl = ctl.clone();
                    Box::pin(async move { ctl.send_audio(&obj).await })
                });
                return Arc::new(eve_desk_call::BoardSpeaker::new(send)) as Arc<dyn eve_desk_call::DeskSpeaker>;
            }
            Arc::new(eve_desk_call::Speaker::new()) as Arc<dyn eve_desk_call::DeskSpeaker>
        });
        let link = self.ble.clone();
        let send_board: eve_desk_call::BoardSend = Arc::new(move |obj: Value| {
            let link = link.clone();
            Box::pin(async move {
                link.send(&obj, None).await;
                Ok(())
            })
        });
        let follower = self.follower.clone();
        // a desk call is a conversation: eve follows whoever is talking
        let on_phase: eve_desk_call::Phase = Arc::new(move |phase: String| {
            let follower = follower.clone();
            Box::pin(async move {
                follower.on_phase(&phase).await;
                Ok(())
            })
        });
        let brain = self.st.lock().telegram_inlet.clone().map(|t| t as Arc<dyn eve_phone_call::Brain>);
        let voice = eve_phone_call::make_voice(self.environ()).map(|v| Arc::new(v) as Arc<dyn eve_phone_call::Voice>);
        let calls = Arc::new(eve_desk_call::DeskCalls::new(
            brain,
            voice,
            send_board,
            eve_desk_call::DeskOptions {
                mic_device: self.ears_cfg.device.clone(),
                busy: phone_busy,
                speaker_factory: speaker,
                on_phase: Some(on_phase),
                ..eve_desk_call::DeskOptions::default()
            },
        ));
        let mut st = self.st.lock();
        Some(st.desk_calls.get_or_insert(calls).clone())
    }

    /// `_on_wake(keyword)`: the ears heard the wake word: open a conversation unless one is open or suppressed.
    pub fn on_wake(self: &Arc<Self>, keyword: &str) {
        info!("ears: heard {}", eve_focus_terminal::py_repr(keyword));
        self.note_activity(); // a conversation is activity: the explorer stops
        if self.conversation_open() {
            return;
        }
        // A manual explore ignores activity, so end it here: the board
        // cannot pan the room and hold the conversation pose at once.
        let me = self.clone();
        self.spawn("dismiss-explore", async move { me.dismiss_explore("wake word").await });
        // "lesson" on its own: the conversation opens with the lesson already requested.
        let lesson_word = &self.ears_cfg.lesson_word;
        let lesson = !lesson_word.is_empty() && keyword == eve_ears::keyword_id(lesson_word);
        let me = self.clone();
        let job = crate::seams::Job::spawn("voice-conversation", async move { me.converse(None, lesson).await });
        self.st.lock().conversation = Some(job);
    }

    /// `_voice_gate_for(think_aloud)`: the speaker gate for this conversation, None when off or the model does
    /// not load (the embedder is loaded once, off the loop).
    ///
    /// EVE_VOICE_GATE=shadow|on: this conversation's speaker gate, enrolled from the audio that woke eve
    /// (voice_gate.py). None — today's behaviour — when it is off, when the conversation was opened without a
    /// wake word, or for a think-aloud lesson, where eve takes notes on whoever speaks. Anything wrong with the
    /// model is simply "no gate".
    pub async fn voice_gate_for(self: &Arc<Self>, think_aloud: Option<&Obj>) -> Option<Arc<VoiceGateHandle>> {
        let ears = self.st.lock().ears.clone();
        // one conversation's enrolment, then gone
        let wake = ears.map(|e| e.take_last_wake_audio()).unwrap_or_default();
        let cfg = eve_voice_gate::configured_from(|k| self.env(k));
        if cfg.mode == "off" || think_aloud.is_some() || wake.is_empty() {
            return None;
        }
        let me = self.clone();
        let make = move || -> Result<Option<eve_voice_gate::SpeakerGate>, eve_voice_gate::GateError> {
            let existing = me.st.lock().voice_embedder.clone();
            let embedder = match existing {
                Some(e) => e,
                None => {
                    let made = Arc::new(Mutex::new(eve_voice_gate::SherpaEmbedder::new(&cfg.model)?));
                    me.st.lock().voice_embedder = Some(made.clone());
                    made
                }
            };
            Ok(eve_voice_gate::build(
                cfg,
                &wake,
                Box::new(eve_voice_gate::monotonic),
                Some(Box::new(SharedEmbedder(embedder))),
            ))
        };
        match tokio::task::spawn_blocking(make).await {
            Ok(Ok(gate)) => gate.map(|g| Arc::new(VoiceGateHandle(Arc::new(Mutex::new(g))))),
            Ok(Err(e)) => {
                // no model file, a bad one: eve hears as it always has
                warn!("voice gate: off for this conversation ({}: {})", e.kind, e.message);
                None
            }
            Err(e) => {
                warn!("voice gate: off for this conversation ({}: {})", "JoinError", e);
                None
            }
        }
    }

    /// `_head_pose_asker()`: Jev's head-pose asker, made once; None when off.
    ///
    /// EVE_HEAD_MODEL=jev: "look left" is decided by Jev, asked in its own idiom (typed_ask.py), in a quarter of
    /// a second instead of ~3.2 s through the backend. Off by default: the words of a turn that mentions a
    /// direction leave the Mac. Anything wrong with it is simply "off".
    pub fn head_pose_asker(&self) -> Option<HeadAsker> {
        if let Some(made) = self.st.lock().head_asker.clone() {
            return made;
        }
        let mut made: Option<HeadAsker> = None;
        if self.env("EVE_HEAD_MODEL").unwrap_or_default().trim().to_lowercase() == "jev" {
            let route = match self.environ() {
                Some(map) => eve_jev::route_config(map, ""),
                None => eve_jev::route_config(&eve_jev::ProcessEnv, ""),
            };
            match route {
                Ok((url, key, model)) => {
                    let predict = eve_jev::make_predict(
                        &url,
                        &key,
                        &model,
                        eve_jev::PredictOptions { timeout_s: 1.5, ..eve_jev::PredictOptions::default() },
                    );
                    let sync = Arc::new(eve_typed_ask::make_jev_head_asker(
                        move |state: &Value, questions: &Value| {
                            predict.predict(state, questions).map_err(anyhow::Error::from)
                        },
                        perf_counter,
                    ));
                    // a network call: never on the loop
                    let ask: HeadAsker = Arc::new(move |text: String| {
                        let sync = sync.clone();
                        Box::pin(async move {
                            tokio::task::spawn_blocking(move || sync(&text)).await.map_err(anyhow::Error::from)
                        })
                    });
                    made = Some(ask);
                    info!("head: poses are chosen by Jev (EVE_HEAD_MODEL); the backend remains the fallback");
                }
                Err(e) => warn!("head: the pose model is off ({}: {})", "JevError", e),
            }
        }
        let mut st = self.st.lock();
        st.head_asker.get_or_insert(made).clone()
    }

    /// `_resync_agent()`: the agent's phase to the board again.
    ///
    /// Tell a (re)connected or rebooted board which conversation phase is live — idle when none. The board drops
    /// a phase that goes 30 s without a word from us, so a lost 'idle' can no longer pin the listening pose
    /// (bench 2026-09-06).
    pub async fn resync_agent(self: &Arc<Self>) {
        if self.ble.connected() {
            let state = self.st.lock().agent_state.clone();
            self.ble.send(&json!({"cmd": "agent", "state": state}), None).await;
        }
    }

    /// `_agent_keepalive()`: re-send the phase while a conversation is open, every 10 s.
    pub async fn agent_keepalive(self: &Arc<Self>) {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            self.resync_agent().await;
        }
    }

    /// `_converse(think_aloud=None, lesson_wake=False)`: one voice conversation, start to close: the memory's
    /// opening, the gate, the session with everything it is lent, the transcript, the explore wish afterwards.
    pub async fn converse(self: &Arc<Self>, think_aloud: Option<Obj>, lesson_wake: bool) {
        let Some(ears) = self.st.lock().ears.clone() else { return };
        if !ears.listening() {
            return;
        }
        let server = self.st.lock().learning_server.clone();
        if lesson_wake {
            if let Some(server) = &server {
                // The word itself is the request: open the lesson window now, with no model in the loop.
                // The conversation then asks learn-a-topic or bring-a-problem and starts the lesson.
                let s = server.clone();
                let opened = tokio::task::spawn_blocking(move || {
                    s.voice(&eve_learning::LessonRequest { action: "open".into(), ..Default::default() })
                })
                .await;
                match opened {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => warn!(
                        "lesson word: could not open the lesson window: {}: {}",
                        learning_error_name(&e),
                        e
                    ),
                    Err(e) => warn!("lesson word: could not open the lesson window: {}: {}", "JoinError", e),
                }
            }
        }
        // What eve remembers of talking with the owner (memory.py, recall.py), when memory is on: the
        // opening brief (one clause), this conversation's transcript id, and today's lines for the voice and
        // its backend. Read here because the prompt is built once, at session.start, before the owner has
        // finished their first sentence, and before the microphone is taken, so a hush during the read
        // leaves nothing to undo. The session reads the profile itself. Memory off: all empty.
        let memory = self.st.lock().memory.clone();
        let brief = eve_records::opening_brief(
            &self.recall_cfg,
            memory.as_ref().map(|m| m.transcripts.as_ref() as &dyn eve_records::BriefSource),
            None,
        );
        if !brief.is_empty() {
            info!("recall: an opening brief of {} chars", brief.chars().count());
        }
        let (conv, today, backend_today) = self.voice_memory(memory.clone()).await;
        self.st.lock().voice_conv = conv.clone();
        let mic = ears.subscribe();
        let me = self.clone();
        let keepalive = crate::seams::Job::spawn("agent-keepalive", async move { me.agent_keepalive().await });
        let mut close = ConverseClose {
            daemon: self.clone(),
            ears: ears.clone(),
            mic: mic.clone(),
            keepalive,
            gate: None,
            finished: false,
        };
        let gate = self.voice_gate_for(think_aloud.as_ref()).await;
        close.gate = gate.clone();

        let d = self.clone();
        let on_state: eve_brain::seams::OnState = Arc::new(move |state: &str| d.on_agent_state(state));
        let d = self.clone();
        let on_caption: eve_brain::seams::OnCaption = Arc::new(move |msg: Value| {
            if let Value::Object(msg) = msg {
                d.on_caption(msg);
            }
        });
        let d = self.clone();
        let on_explore: eve_brain::seams::OnExplore = Arc::new(move || {
            d.on_voice_explore();
            Box::pin(async { Ok(()) })
        });
        let d = self.clone();
        let on_expression: eve_brain::seams::ExpressionFn =
            Arc::new(move |who: &str, text: &str| d.on_expression_text(who, text));
        let d = self.clone();
        let on_sound: eve_brain::seams::OnSound = Arc::new(move |on: bool| d.set_sound(on));
        let d = self.clone();
        let muted: eve_brain::seams::BoolFn = Arc::new(move || d.sound.lock().muted());
        let d = self.clone();
        let on_photo: eve_brain::seams::OnPhoto = Arc::new(move |said: String| {
            let d = d.clone();
            Box::pin(async move { d.photo_for_owner(&said).await })
        });
        let learning: Option<eve_brain::seams::LearningFn> = server.clone().map(|s| {
            Arc::new(move |r: &eve_learning::LessonRequest| s.voice(r)) as eve_brain::seams::LearningFn
        });
        let on_spoken_idea: Option<eve_brain::seams::SpokenIdeaFn> = server.clone().map(|s| {
            Arc::new(move |lesson_id: &str, text: &str| {
                s.append_spoken(lesson_id, text).map_err(anyhow::Error::from)
            }) as eve_brain::seams::SpokenIdeaFn
        });
        let d = self.clone();
        let on_think_aloud: eve_brain::seams::ThinkAloudFn =
            Arc::new(move |on: bool, lesson_id: Option<&str>| d.on_think_aloud(on, lesson_id));
        let d = self.clone();
        let on_open: eve_brain::voice_agent::OnOpenSession =
            Arc::new(move |session: &Arc<eve_brain::voice_agent::VoiceSession>| {
                d.on_voice_session(Arc::new(session.clone()));
            });
        let d = self.clone();
        let mac_busy: eve_brain::seams::BoolFn = Arc::new(move || d.texted_task_running());
        let (intent, thinker, meeter) = {
            let st = self.st.lock();
            (st.intent.clone(), st.thinker.clone(), st.meeter.clone())
        };
        let options = eve_brain::voice_agent::VoiceSessionOptions {
            config: Some(self.voice_cfg.clone()),
            agent_enabled: self.agent_cfg.enabled,
            on_caption: Some(on_caption),
            on_explore: Some(on_explore),
            on_expression: Some(on_expression),
            scene: Some(Arc::new(SceneHandle(self.scene.clone())) as Arc<dyn eve_brain::seams::SceneWatch>),
            head: Some(self.head.clone()),
            intent,
            on_sound: Some(on_sound),
            muted: Some(muted),
            thinker,
            on_photo: Some(on_photo),
            brief,
            memory: memory.map(|m| m as Arc<dyn eve_brain::seams::BrainMemory>),
            conv,
            today,
            backend_today,
            learning,
            think_aloud,
            lesson_wake,
            head_pose: self.head_pose_asker(),
            gate: gate.map(|g| g as Arc<dyn eve_brain::seams::SpeakerGate>),
            on_spoken_idea,
            on_think_aloud: Some(on_think_aloud),
            mac_busy: Some(mac_busy),
            meeter,
            app_hubs: self.app_hubs(true),
            environ: self.environ.clone(),
            ..eve_brain::voice_agent::VoiceSessionOptions::default()
        };
        let opened = (self.seams.open_session)(mic, on_state, self.agent_factory(), Some(on_open), options).await;
        if let Err(e) = opened {
            warn!("voice: conversation failed: {:#}", e);
            self.on_agent_state("error");
        }
        close.finished = true;
        // `keepalive.cancel(); await asyncio.gather(keepalive, ...)`: the close yields to the loop here, so what the
        // session left queued (its last phase sends) goes out before the rest of the close.
        close.keepalive.cancel();
        close.keepalive.join().await;
        drop(close);
        let reason = self.st.lock().explore_after_conversation.take();
        if let Some(reason) = reason {
            if let Err(refused) = self.request_explore(&reason).await {
                info!("explore: not started after the conversation ({})", refused);
            }
        }
    }

    /// `_room_notes_taker()`: the room notes, made on the first ask.
    pub fn room_notes_taker(self: &Arc<Self>) -> Arc<eve_notes::RoomNotes> {
        if let Some(taker) = self.st.lock().room_notes.clone() {
            return taker;
        }
        let ears = self.st.lock().ears.clone();
        let source = ears.map(|e| Arc::new(MicSource::new(e)) as Arc<dyn eve_notes::AudioSource>);
        let weak = Arc::downgrade(self);
        let on_state: eve_notes::Callback = Arc::new(move |state: &str| {
            if let Some(d) = weak.upgrade() {
                d.on_agent_state(state);
            }
        });
        let taker = Arc::new(eve_notes::RoomNotes::with_options(
            self.recall_cfg.clone(),
            eve_notes::configured(self.environ()),
            eve_notes::make_notes_client(self.environ()),
            source,
            eve_notes::RoomNotesOptions { on_state: Some(on_state), ..eve_notes::RoomNotesOptions::default() },
        ));
        let mut st = self.st.lock();
        st.room_notes.get_or_insert(taker).clone()
    }

    /// `_phone_notes(inlet)`: the phone's notes for the Mini App's calls, made with the text door.
    ///
    /// Notes on the phone (phone_call.PhoneNotes): each session is a RoomNotes of its own, never the desk's, so
    /// the two never share a recording, fed from the call and written beside the desk's as `source: eve-link`.
    /// The notes are texted to the owner the way the Meet notetaker's are: the history gets `about`, never the
    /// words heard (the transcript is anyone's, and the chat reading it as its own would be a stored prompt
    /// injection).
    pub fn phone_notes(self: &Arc<Self>, inlet: &Arc<dyn Inlet>) -> Arc<eve_phone_call::PhoneNotes> {
        let inlet = inlet.clone();
        let notify: eve_phone_call::NotesNotify = Arc::new(move |text: String, about: String| {
            let inlet = inlet.clone();
            Box::pin(async move {
                inlet.tell_owner(&text, &about, eve_brain::telegram::NOTES_TITLE).await;
                Ok(())
            })
        });
        let recall_cfg = self.recall_cfg.clone();
        let environ = self.environ.clone();
        let make_taker: eve_phone_call::MakeTaker = Arc::new(move |mic: Arc<eve_notes::PushedAudio>| {
            Arc::new(eve_notes::RoomNotes::with_options(
                recall_cfg.clone(),
                eve_notes::configured(environ.as_ref()),
                eve_notes::make_notes_client(environ.as_ref()),
                Some(mic as Arc<dyn eve_notes::AudioSource>),
                eve_notes::RoomNotesOptions { source: "eve-link".into(), ..eve_notes::RoomNotesOptions::default() },
            )) as Arc<dyn eve_phone_call::NotesTaker>
        });
        eve_phone_call::PhoneNotes::new(make_taker, Some(notify))
    }

    /// `_on_voice_explore()`: "go explore" said by voice: granted once the conversation has closed (in
    /// `converse`).
    pub fn on_voice_explore(&self) {
        self.st.lock().explore_after_conversation = Some("requested by voice".to_string());
    }

    /// `_type_into_terminal(cwd, text)`: raise the session's terminal and type `text` into it; the outcome line.
    ///
    /// "claude: <text>" from the phone: raise the session's terminal (focus_terminal.py) and type the line with
    /// Return, through System Events. Real keystrokes into whatever is then frontmost — which is why it only runs
    /// while the owner has said "claude on".
    pub async fn type_into_terminal(&self, cwd: &str, text: &str) -> String {
        (self.seams.focus_terminal)(cwd.to_string()).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "tell application \"System Events\" to keystroke \"{escaped}\"\ntell application \"System Events\" to keystroke return"
        );
        match eve_focus_terminal::osascript(&script, &[]).await {
            Ok(Some(_)) => String::new(), // typed: the phone reacts to the owner's message
            Ok(None) | Err(_) => {
                "I couldn't type into the terminal (System Events refused — check Automation permissions).".to_string()
            }
        }
    }

    /// `_texted_task_running()`: the text door has a desktop task running.
    pub fn texted_task_running(&self) -> bool {
        let inlet = self.st.lock().telegram.clone();
        inlet.is_some_and(|i| i.task_running())
    }

    /// `_desk_has_the_mac()`: a desk call's task holds the Mac.
    ///
    /// For the text door: a spoken conversation is open, or a task it started is still running. One agent on the
    /// mouse at a time, and the person at the desk wins.
    pub fn desk_has_the_mac(&self) -> bool {
        let (open, agent, inlet) = {
            let st = self.st.lock();
            (st.conversation.as_ref().is_some_and(|c| !c.done()), st.active_agent.clone(), st.telegram.clone())
        };
        let spoken_task =
            agent.is_some_and(|a| a.running()) && !inlet.is_some_and(|i| i.task_running());
        open || spoken_task
    }
}
