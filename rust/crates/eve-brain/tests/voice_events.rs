//! Unit `voice_events`: voice_agent.py 1288-2240 (voice_agent/events, tools); tests: test_voice_agent.py go explore (831-960), eyes and head (961-1578), think out loud (1579-1870), take_photo (1871-1944), Meet (2383-2633).
//!
//! The shared fakes are in `support` (architect-owned); this unit's own go here. Python's tests read the session's
//! private attributes; these read them through `probe_state()` and drive its private methods through `probe_*`.
//! The voice's route delays (LOOK_ROUTE_DELAY_SECS, LESSON_ROUTE_DELAY_SECS) are the real 0.6 s here: Python's
//! tests patched the module globals down to 0.05 s.

mod support;

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_agent_contract::AgentEvent;
use eve_brain::prelude::{
    AgentFactory, AppHub, AppHubs, AskUser, Flag, LearningFn, OnEvent, SceneWatch, TaskAgent,
};
use eve_brain::voice_agent::{
    session_config, session_config_with, VoiceConfig, VoiceSession, VoiceSessionOptions, BACKEND_INSTRUCTIONS,
    FAREWELL_MAX_SECS, FAREWELL_QUIET_SECS, INSTRUCTIONS, LESSON_ROUTE_DELAY_SECS, LOOK_ROUTE_DELAY_SECS, MEET_TOOLS,
    PHOTO_TIMEOUT_SECS, TOOLS, TURN_GAP_SECS, VOICE_FIRETV_NOTE,
};
use support::{FakeClock, FakeConn, FakeSpeaker, Obj};

// ---- fakes ---------------------------------------------------------------------------------------------------------

/// test_voice_agent.py's FakeAgent: runs until released; reports events through on_event like the real one.
struct TestAgent {
    on_event: OnEvent,
    ask_user: AskUser,
    final_text: String,
    ask_q: Option<String>,
    running: AtomicBool,
    steers: Mutex<Vec<String>>,
    cancelled: AtomicBool,
    reason: Mutex<String>,
    release: Flag,
    finished: Mutex<Option<String>>,
}

#[async_trait]
impl TaskAgent for TestAgent {
    async fn run(&self, goal: &str, _note: &str) -> anyhow::Result<String> {
        self.running.store(true, Ordering::SeqCst);
        (self.on_event)(&AgentEvent::with_text("started", goal));
        (self.on_event)(&AgentEvent::with_turn("exec", "display(...)", 1));
        let mut answer = None;
        if let Some(q) = &self.ask_q {
            (self.on_event)(&AgentEvent::with_turn("ask", q.clone(), 1));
            answer = Some((self.ask_user)(q.clone()).await?);
        }
        self.release.wait().await;
        self.running.store(false, Ordering::SeqCst);
        if self.cancelled.load(Ordering::SeqCst) {
            (self.on_event)(&AgentEvent::with_text("cancelled", "Stopped."));
            *self.finished.lock() = Some("Stopped.".into());
            return Ok("Stopped.".into());
        }
        let out = match answer {
            Some(a) => format!("{} (answer={a})", self.final_text),
            None => self.final_text.clone(),
        };
        *self.finished.lock() = Some(out.clone());
        (self.on_event)(&AgentEvent::with_turn("final", out.clone(), 2));
        Ok(out)
    }

    fn cancel(&self, reason: &str) {
        self.cancelled.store(true, Ordering::SeqCst);
        *self.reason.lock() = reason.to_string();
        self.release.set();
    }

    fn steer(&self, text: &str) -> bool {
        if !self.running.load(Ordering::SeqCst) {
            return false;
        }
        self.steers.lock().push(text.to_string());
        true
    }

    fn status(&self) -> Value {
        json!({"running": self.running.load(Ordering::SeqCst), "turn": 1, "goal": "g", "last": "looking",
               "final": *self.finished.lock()})
    }

    fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

/// The agents a factory made, newest last.
struct Agents {
    made: Mutex<Vec<Arc<TestAgent>>>,
    final_text: String,
}

impl Agents {
    fn new(final_text: &str) -> Arc<Self> {
        Arc::new(Self { made: Mutex::new(Vec::new()), final_text: final_text.into() })
    }

    fn factory(self: &Arc<Self>) -> AgentFactory {
        let me = self.clone();
        Arc::new(move |on_event: OnEvent, ask_user: AskUser, _floor: Option<String>| {
            let agent = Arc::new(TestAgent {
                on_event,
                ask_user,
                final_text: me.final_text.clone(),
                ask_q: None,
                running: AtomicBool::new(false),
                steers: Mutex::new(Vec::new()),
                cancelled: AtomicBool::new(false),
                reason: Mutex::new(String::new()),
                release: Flag::new(),
                finished: Mutex::new(None),
            });
            me.made.lock().push(agent.clone());
            agent as Arc<dyn TaskAgent>
        })
    }

    fn last(&self) -> Arc<TestAgent> {
        self.made.lock().last().cloned().expect("no agent was made")
    }
}

/// test_voice_agent.py's FakeScene: start/stop counters, a fixed view, and the targets located.
struct TestScene {
    started: AtomicI64,
    stopped: AtomicI64,
    view: Value,
    located: Mutex<Vec<String>>,
    /// SlowScene: look waits for this
    gate: Option<Arc<Flag>>,
}

impl TestScene {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            started: AtomicI64::new(0),
            stopped: AtomicI64::new(0),
            view: json!({"ok": true, "view": "A person holding a green mug.", "seen_at": "16:43:05",
                         "age_secs": 1.0, "stale": false}),
            located: Mutex::new(Vec::new()),
            gate: None,
        })
    }

    fn slow(gate: Arc<Flag>) -> Arc<Self> {
        let mut scene = Arc::try_unwrap(Self::new()).ok().expect("fresh");
        scene.gate = Some(gate);
        Arc::new(scene)
    }
}

#[async_trait]
impl eve_head::Scene for TestScene {
    async fn look(&self, _fresh_secs: f64, _newer_than: Option<f64>) -> Value {
        match &self.gate {
            Some(gate) => gate.wait().await,
            None => tokio::task::yield_now().await,
        }
        self.view.clone()
    }

    async fn locate(&self, target: &str, _newer_than: Option<f64>) -> Value {
        self.located.lock().push(target.to_string());
        json!({"ok": true, "visible": true, "x": 0.0, "y": 0.0, "what": target, "yaw": 0, "pitch": 45})
    }
}

#[async_trait]
impl SceneWatch for TestScene {
    fn start(&self) {
        self.started.fetch_add(1, Ordering::SeqCst);
    }

    async fn stop(&self) {
        self.stopped.fetch_add(1, Ordering::SeqCst);
    }
}

/// The robot head over a recording wire (Python's FakeHead recorded the moves; the real Head sends them).
fn test_head() -> (Arc<eve_head::Head>, Arc<Mutex<Vec<Value>>>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let s = sent.clone();
    let send: eve_head::Sender = Arc::new(move |cmd: Value| {
        s.lock().push(cmd);
        async { true }.boxed()
    });
    let head = eve_head::Head::with(
        send,
        Arc::new(|| true),
        Arc::new(|| 0.0),
        Arc::new(|_secs: f64| async { tokio::task::yield_now().await }.boxed()),
    );
    (Arc::new(head), sent)
}

/// One of eve's apps' hubs, faked: its tools, its instructions, and the calls it got.
struct TestHub {
    tools: Vec<Value>,
    instructions: String,
    /// the instructions when the voice passes screenshot=False (the TV's)
    no_screenshot: Option<String>,
    reply: Value,
    calls: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl AppHub for TestHub {
    fn tools(&self) -> Vec<Value> {
        self.tools.clone()
    }

    fn instructions(&self, kwargs: &Obj) -> String {
        match (&self.no_screenshot, kwargs.get("screenshot")) {
            (Some(text), Some(Value::Bool(false))) => text.clone(),
            _ => self.instructions.clone(),
        }
    }

    async fn handle(&self, name: &str, args: &Value) -> Value {
        self.calls.lock().push((name.to_string(), args.clone()));
        self.reply.clone()
    }
}

/// Make the app's module known to the registry (the daemon does it at startup), so its tools route.
fn register_app(slot: &str, names: &[&str]) {
    let app = eve_app_registry::get(slot).expect("an app");
    eve_app_registry::register_module(eve_app_registry::ModuleInfo {
        tool_names: Some(names.iter().map(|n| n.to_string()).collect()),
        off_reason: Some(format!("{slot} is off")),
        ..eve_app_registry::ModuleInfo::new(&app.module)
    });
}

fn hubs(slot: &str, hub: Arc<TestHub>) -> AppHubs {
    AppHubs::new(vec![(slot.to_string(), hub as Arc<dyn AppHub>)]).expect("a known slot")
}

/// test_voice_agent.py's FakeMemory: memory.py's surface over a REAL transcript store in a tmp folder.
struct TestMemory {
    transcripts: Arc<eve_transcripts::Transcripts>,
    voice_profile: String,
    starred: Mutex<Vec<String>>,
}

impl TestMemory {
    fn new(dir: &tempfile::TempDir, voice_profile: &str) -> Arc<Self> {
        let mut config = eve_transcripts::TranscriptConfig::new(true, dir.path().join("transcripts"));
        config.home = Some(dir.path().to_path_buf());
        let transcripts = Arc::new(eve_transcripts::Transcripts::new(config, None, None));
        assert!(transcripts.enabled);
        Arc::new(Self { transcripts, voice_profile: voice_profile.into(), starred: Mutex::new(Vec::new()) })
    }

    fn lines(&self, conv: &str) -> usize {
        if conv.is_empty() {
            0
        } else {
            self.transcripts.conv_lines(conv).len()
        }
    }
}

impl eve_brain::prelude::BrainMemory for TestMemory {
    fn transcripts(&self) -> Arc<eve_transcripts::Transcripts> {
        self.transcripts.clone()
    }
    fn profile(&self) -> String {
        String::new()
    }
    fn profile_for_voice(&self) -> String {
        self.voice_profile.clone()
    }
    fn today(&self, max_chars: i64, options: &eve_transcripts::Today) -> String {
        self.transcripts.today_block_with(max_chars, options)
    }
    fn star(&self, text: &str) -> Option<String> {
        self.starred.lock().push(text.to_string());
        Some(format!("- {text} (2026-09-23)"))
    }
    fn tools(&self, voice: bool) -> Vec<Value> {
        let mut names = vec!["memory_search", "memory_read", "forget_preview", "forget_apply"];
        if voice {
            names.push("recent_conversation");
        }
        names
            .into_iter()
            .map(|n| {
                json!({"type": "function", "name": n, "description": "d", "strict": true,
                       "parameters": {"type": "object", "properties": {}, "required": [],
                                      "additionalProperties": false}})
            })
            .collect()
    }
    fn handle_tool(&self, _name: &str, _args: &Value, _since: Option<eve_transcripts::Dt>, _channel: &str) -> Value {
        json!({"ok": true, "hits": []})
    }
}

/// Log lines written on this thread while it is alive (pytest's caplog at INFO). One global subscriber writes each
/// line to the capturing thread's buffer: a scoped (`set_default`) subscriber loses lines once another test's
/// callsite caches "nobody listens" for the whole process.
struct Logs {
    buf: Arc<Mutex<Vec<u8>>>,
}

thread_local! {
    static CAPTURE: std::cell::RefCell<Option<Arc<Mutex<Vec<u8>>>>> = const { std::cell::RefCell::new(None) };
}

struct LogWriter;

impl std::io::Write for LogWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        CAPTURE.with(|c| {
            if let Some(buf) = &*c.borrow() {
                buf.lock().extend_from_slice(data);
            }
        });
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn capture() -> Self {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::INFO)
                .with_ansi(false)
                .with_writer(|| LogWriter)
                .finish();
            tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
        });
        let buf = Arc::new(Mutex::new(Vec::new()));
        CAPTURE.with(|c| *c.borrow_mut() = Some(buf.clone()));
        Self { buf }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock()).into_owned()
    }
}

impl Drop for Logs {
    fn drop(&mut self) {
        CAPTURE.with(|c| *c.borrow_mut() = None);
    }
}

// ---- the session rig -----------------------------------------------------------------------------------------------

/// The watchdog looks every WATCHDOG_SECS in these tests (0.5 s in production); a wait of WATCHDOG_WAIT lets it
/// look at least twice. The decisions ride the fake clock, so only the number of looks matters, not real time.
const WATCHDOG_SECS: f64 = 0.02;
const WATCHDOG_WAIT: f64 = 0.05;

fn audio_config(idle: f64, cap: f64) -> VoiceConfig {
    VoiceConfig {
        model: "gpt-live-1".into(),
        backend_model: "gpt-6-astra".into(),
        backend_effort: "low".into(),
        voice: "marin".into(),
        idle_timeout_secs: idle,
        max_session_secs: cap,
        output: "audio".into(),
        caption_cps: 12.0,
        web_search: true,
        search: eve_web::websearch::SearchConfig::default(),
        about: String::new(),
    }
}

/// `VoiceConfig()`: Python's defaults (captions out).
fn plain_config() -> VoiceConfig {
    VoiceConfig { output: "captions".into(), ..audio_config(20.0, 600.0) }
}

struct Rig {
    conn: Arc<FakeConn>,
    s: Arc<VoiceSession>,
    states: Arc<Mutex<Vec<String>>>,
    mic: eve_ears::PcmQueue,
    clock: FakeClock,
    agents: Arc<Agents>,
    speaker: Arc<FakeSpeaker>,
}

/// test_voice_agent.py's `_session(FakeConnection(events), [FakeAgent(...)], **kw)`: `tweak` sets the keywords.
fn rig_with(events: Vec<Value>, final_text: &str, tweak: impl FnOnce(&mut VoiceSessionOptions)) -> Rig {
    let conn = FakeConn::new();
    conn.push(json!({"type": "session.started"})); // run() waits for this
    for e in events {
        conn.push(e);
    }
    let clock = FakeClock::new(0.0);
    let states = Arc::new(Mutex::new(Vec::new()));
    let agents = Agents::new(final_text);
    let speaker = Arc::new(FakeSpeaker::default());
    let mic = eve_ears::PcmQueue::new(0);
    let st = states.clone();
    let mut options = VoiceSessionOptions {
        config: Some(audio_config(20.0, 600.0)),
        clock: Some(clock.clock()),
        watchdog_secs: WATCHDOG_SECS,
        ..VoiceSessionOptions::default()
    };
    tweak(&mut options);
    let s = VoiceSession::new(
        conn.clone(),
        mic.clone(),
        speaker.clone(),
        agents.factory(),
        Arc::new(move |state: &str| st.lock().push(state.to_string())),
        options,
    );
    Rig { conn, s, states, mic, clock, agents, speaker }
}

fn rig(events: Vec<Value>, tweak: impl FnOnce(&mut VoiceSessionOptions)) -> Rig {
    rig_with(events, "Mail is open.", tweak)
}

impl Rig {
    fn start(&self) -> tokio::task::JoinHandle<()> {
        let s = self.s.clone();
        tokio::spawn(async move { s.run().await })
    }

    /// `conn.feed(...)`; Python's None closes the stream.
    fn feed(&self, events: Vec<Value>) {
        for e in events {
            self.conn.push(e);
        }
    }

    fn close(&self) {
        self.conn.push(Value::Null);
    }

    fn outputs(&self) -> Vec<Value> {
        self.conn
            .of("response.item.create")
            .into_iter()
            .filter(|item| item["type"] == "function_call_output")
            .map(|item| serde_json::from_str(item["output"].as_str().unwrap()).unwrap())
            .collect()
    }

    fn user_messages(&self) -> Vec<String> {
        self.conn
            .of("response.item.create")
            .into_iter()
            .filter(|item| item["type"] == "message")
            .map(|item| item["content"][0]["text"].as_str().unwrap().to_string())
            .collect()
    }

    fn commentary(&self) -> Vec<String> {
        self.conn.of("commentary.append").into_iter().map(|c| c["content"].as_str().unwrap().to_string()).collect()
    }

    fn thinking(&self) -> Vec<String> {
        self.conn.of("thinking.append").into_iter().map(|c| c["content"].as_str().unwrap().to_string()).collect()
    }

    fn kinds(&self) -> Vec<String> {
        self.conn.sent.lock().iter().map(|(k, _)| k.clone()).collect()
    }

    fn states(&self) -> Vec<String> {
        self.states.lock().clone()
    }

    fn farewell(&self) -> bool {
        self.s.probe_state().farewell
    }
}

fn tool_call(name: &str, call_id: &str, args: Value) -> Value {
    json!({"type": "response.event", "event": {
        "type": "response.output_item.done",
        "item": {"type": "function_call", "name": name, "call_id": call_id,
                 "arguments": serde_json::to_string(&args).unwrap()}}})
}

fn end_call(call_id: &str) -> Value {
    tool_call("end_conversation", call_id, json!({}))
}

fn delegated() -> Value {
    json!({"type": "session.delegation.created", "delegation": {"id": "d1", "target": "responses", "type": "delegation"}})
}

fn done() -> Value {
    json!({"type": "response.event", "event": {"type": "response.completed"}})
}

fn heard(text: &str) -> Value {
    json!({"type": "session.input_transcript.delta", "delta": text})
}

fn spoke(text: &str) -> Value {
    json!({"type": "session.output_transcript.delta", "delta": text})
}

async fn sleep(secs: f64) {
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
}

/// Wait for a state transition (Python's `_eventually`).
async fn eventually(mut predicate: impl FnMut() -> bool, timeout: f64) {
    let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout);
    while !predicate() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting");
        sleep(0.001).await;
    }
}

async fn finish(rig: &Rig, task: tokio::task::JoinHandle<()>) {
    tokio::time::timeout(Duration::from_secs(10), task).await.expect("the session ended").expect("run");
    let _ = rig;
}

fn obj(v: Value) -> Obj {
    v.as_object().cloned().unwrap()
}

fn lesson() -> Obj {
    obj(json!({"id": "L1", "topic": "Algebra", "level": "Grade 5", "problem": "Solve 2x + 3 = 11.",
               "stage": "working", "ideas": "typed first", "events": [{"action": "step", "step": "2x = 11 - 3"}]}))
}

fn learning_fn(calls: Arc<Mutex<Vec<Obj>>>, answer: Value) -> LearningFn {
    Arc::new(move |r: &eve_learning::LessonRequest| {
        calls.lock().push(r.to_map());
        Ok(obj(answer.clone()))
    })
}

// ---- "go explore" (test_voice_agent.py 831-960) ------------------------------------------------------------------

#[tokio::test]
async fn test_go_explore_ends_the_conversation_and_fires_the_callback() {
    let fired = Arc::new(AtomicI64::new(0));
    let f = fired.clone();
    let r = rig(vec![tool_call("go_explore", "c1", json!({})), Value::Null], |o| {
        o.on_explore = Some(Arc::new(move || {
            f.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }.boxed()
        }));
    });
    r.s.run().await;
    assert_eq!(r.outputs(), vec![json!({"ok": true})]);
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    assert!(r.s.probe_state().explore_requested);
    assert_eq!(r.kinds().last().unwrap(), "response.item.create"); // no further response is requested
    assert_eq!(r.states().last().unwrap(), "idle");
}

#[tokio::test]
async fn test_go_explore_is_refused_while_a_task_runs() {
    let fired = Arc::new(AtomicI64::new(0));
    let f = fired.clone();
    let r = rig(vec![tool_call("start_task", "c1", json!({"goal": "open mail"}))], |o| {
        o.on_explore = Some(Arc::new(move || {
            f.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }.boxed()
        }));
    });
    let task = r.start();
    sleep(0.02).await;
    r.feed(vec![tool_call("go_explore", "c2", json!({}))]);
    sleep(0.02).await;
    r.agents.last().release.set();
    sleep(0.02).await;
    r.feed(vec![end_call("c3"), Value::Null]);
    finish(&r, task).await;
    let outs = r.outputs();
    assert_eq!(outs[1], json!({"ok": false, "reason": "a task is running; stop it first"}));
    assert_eq!(fired.load(Ordering::SeqCst), 0);
    assert!(!r.s.probe_state().explore_requested);
}

#[tokio::test]
async fn test_go_explore_without_a_callback_still_ends() {
    let r = rig(vec![tool_call("go_explore", "c1", json!({})), Value::Null], |_| {});
    r.s.run().await;
    assert!(r.s.probe_state().explore_requested);
    assert_eq!(r.states().last().unwrap(), "idle");
}

#[test]
fn test_instructions_mention_go_explore() {
    // the voice half only has to know to delegate; the tool workflow is the backend's
    assert!(INSTRUCTIONS.contains("explore"));
    assert!(BACKEND_INSTRUCTIONS.contains("Go explore") && BACKEND_INSTRUCTIONS.contains("go_explore"));
}

#[test]
fn test_prompts_do_not_let_a_started_task_read_as_done() {
    // 2026-09-10: the backend said "Spotify playback requested." and the voice turned it into
    // "It's playing now." 12 s before the task finished.
    assert!(INSTRUCTIONS.contains("Starting a task is not finishing it"));
    assert!(INSTRUCTIONS.contains("INSTEAD: \"On it.\""));
    // the backend stays silent after start_task (its "On it." doubled the voice's own), and
    // never asks a clarifying question (the "what city?" round trip cost 10.4 s)
    assert!(BACKEND_INSTRUCTIONS.contains("reply with an empty message"));
    assert!(!BACKEND_INSTRUCTIONS.contains("return exactly: On it"));
    assert!(BACKEND_INSTRUCTIONS.contains("Outside lessons, never ask the owner a clarifying question"));
}

#[tokio::test]
async fn test_idle_timeout_closes_when_the_server_goes_quiet() {
    // 3/3 logged idle closes hung until the next Live event (+91 s, +129 s, +93 min).
    let r = rig(vec![], |_| {}); // nothing arrives after session.started
    let task = r.start();
    sleep(0.05).await;
    r.clock.set(30.0); // past the 20 s idle timeout
    tokio::time::timeout(Duration::from_secs(2), task).await.expect("ends with NO further event").unwrap();
    assert!(r.s.ended());
    assert_eq!(r.states().last().unwrap(), "idle");
}

#[tokio::test]
async fn test_started_task_goes_working_at_once_and_tells_the_voice_quietly() {
    // "listening" first: task events never set "working" from there, so only start_task can
    let r = rig(vec![heard("open my mail"), tool_call("start_task", "c1", json!({"goal": "open mail"}))], |_| {});
    let task = r.start();
    sleep(0.02).await;
    assert_eq!(r.states().last().unwrap(), "working");
    let quiet = r.thinking();
    assert_eq!(quiet.len(), 1);
    assert!(quiet[0].contains("open mail") && quiet[0].contains("Nothing is done yet"));
    r.agents.last().release.set();
    sleep(0.02).await;
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
}

#[tokio::test]
async fn test_stopped_task_is_not_done_and_gives_the_owners_reason() {
    let r = rig(vec![tool_call("start_task", "c1", json!({"goal": "open mail"}))], |_| {});
    let task = r.start();
    sleep(0.02).await;
    r.feed(vec![tool_call("stop_task", "c2", json!({}))]);
    sleep(0.02).await;
    let agent = r.agents.last();
    assert!(agent.cancelled.load(Ordering::SeqCst));
    assert_eq!(*agent.reason.lock(), "you asked me to stop");
    r.s.probe_agent_event(&AgentEvent::with_text("cancelled", "Stopped."));
    assert_eq!(r.states().last().unwrap(), "listening"); // no done nod for a stopped task
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
}

#[tokio::test]
async fn test_closing_session_speaks_no_result() {
    // A hush used Task.cancel() but the result path checked only `_ended`, so a hushed
    // conversation still tried to speak its result (15:50:04.623).
    let r = rig(vec![tool_call("start_task", "c1", json!({"goal": "open mail"}))], |_| {});
    let task = r.start();
    sleep(0.02).await;
    task.abort(); // the hush path
    assert!(task.await.unwrap_err().is_cancelled());
    sleep(0.05).await;
    // the greeting is the only thing the voice was handed: no result of any kind after the hush
    let said = r.commentary();
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("Greet"));
}

// ---- eyes, head, standing orders (2026-09-10; test_voice_agent.py 961-1578) -------------------------------------

#[tokio::test]
async fn test_scene_watches_with_the_session_and_nothing_it_sees_is_pushed_to_the_voice() {
    // Owner report 2026-09-10: pushed [vision] lines made eve narrate the room.
    // The watcher runs for the conversation, but a view reaches the voice only
    // through the look tool.
    let scene = TestScene::new();
    let sc = scene.clone();
    let r = rig(vec![], move |o| o.scene = Some(sc));
    let task = r.start();
    sleep(0.05).await;
    assert_eq!(scene.started.load(Ordering::SeqCst), 1);
    assert!(!r.thinking().iter().any(|t| t.contains("[vision")));
    r.feed(vec![end_call("c1"), Value::Null]);
    finish(&r, task).await;
    assert_eq!(scene.stopped.load(Ordering::SeqCst), 1);
    assert!(!r.thinking().iter().any(|t| t.contains("[vision")));
}

#[tokio::test]
async fn test_look_tool_answers_from_a_background_task() {
    let scene = TestScene::new();
    let r = rig(vec![tool_call("look", "c1", json!({}))], move |o| o.scene = Some(scene));
    let task = r.start();
    sleep(0.05).await;
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
    assert_eq!(r.outputs()[0]["view"], "A person holding a green mug.");
}

#[tokio::test]
async fn test_think_hard_answers_from_a_background_task_and_keeps_the_voice_company() {
    let asked = Arc::new(Mutex::new(Vec::<String>::new()));
    let release = Arc::new(Flag::new());
    let (a, rel) = (asked.clone(), release.clone());
    let r = rig(vec![tool_call("think_hard", "c1", json!({"question": "what is six times seven, and why?"}))], |o| {
        o.thinker = Some(Arc::new(move |question: String, _context: String| {
            a.lock().push(question);
            let rel = rel.clone();
            async move {
                rel.wait().await;
                Ok(obj(json!({"ok": true, "answer": "Forty-two, because the question was the easy part."})))
            }
            .boxed()
        }));
    });
    let task = r.start();
    eventually(|| !asked.lock().is_empty(), 2.0).await;
    // while the brain works: the voice was told to keep the owner company, nothing answered yet
    assert!(r.thinking().iter().any(|t| t.contains("worked on in the background")));
    assert!(r.outputs().is_empty());
    release.set();
    eventually(|| !r.outputs().is_empty(), 2.0).await;
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
    assert_eq!(*asked.lock(), vec!["what is six times seven, and why?".to_string()]);
    assert_eq!(r.outputs()[0], json!({"ok": true, "answer": "Forty-two, because the question was the easy part."}));
}

#[tokio::test]
async fn test_think_hard_without_a_thinker_or_when_it_fails_tells_the_backend_to_answer_itself() {
    async fn go(r: &Rig) {
        let task = r.start();
        sleep(0.05).await;
        r.feed(vec![end_call("c9"), Value::Null]);
        finish(r, task).await;
    }
    let r = rig(vec![tool_call("think_hard", "c1", json!({"question": "why?"}))], |_| {});
    go(&r).await;
    let out = &r.outputs()[0];
    assert_eq!(out["ok"], false);
    assert!(out["reason"].as_str().unwrap().contains("answer as best you can"));

    let r = rig(vec![tool_call("think_hard", "c1", json!({"question": "why?"}))], |o| {
        o.thinker = Some(Arc::new(|_q: String, _c: String| async { Err(anyhow::anyhow!("90 s")) }.boxed()));
    });
    go(&r).await;
    let out = &r.outputs()[0];
    assert_eq!(out["ok"], false);
    assert!(out["reason"].as_str().unwrap().contains("did not answer in time"));
}

#[tokio::test]
async fn test_look_without_vision_says_why() {
    let r = rig(vec![tool_call("look", "c1", json!({}))], |_| {});
    let task = r.start();
    sleep(0.05).await;
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
    assert_eq!(r.outputs()[0], json!({"ok": false, "reason": "vision is not set up on this computer"}));
}

#[tokio::test]
async fn test_move_head_passes_the_backend_numbers_through() {
    let (head, sent) = test_head();
    let r = rig(
        vec![
            tool_call("move_head", "c1", json!({"yaw": -15, "relative": true})),
            tool_call("move_head", "c2", json!({"yaw": 120, "pitch": 45, "relative": false, "hold_secs": 30})),
            tool_call("move_head", "c3", json!({"yaw": "left", "relative": true})),
            end_call("c9"),
            Value::Null,
        ],
        move |o| o.head = Some(head),
    );
    r.s.run().await;
    let sent = sent.lock().clone();
    // (-15, None, True, 15): an offset from straight ahead, level, the default hold
    assert_eq!(sent[0], json!({"cmd": "look", "yaw": -15, "pitch": 45, "hold": 15000}));
    // (120, 45, False, 30)
    assert_eq!(sent[1], json!({"cmd": "look", "yaw": 120, "pitch": 45, "hold": 30000}));
    // (None, None, True, 15): a non-number is dropped, not guessed
    assert_eq!(sent[2], json!({"cmd": "look", "yaw": 120, "pitch": 45, "hold": 15000}));
    assert_eq!(r.outputs()[1]["yaw"], 120);
}

#[tokio::test]
async fn test_look_around_and_find_run_through_head_and_scene() {
    let (head, _) = test_head();
    let scene = TestScene::new();
    let sc = scene.clone();
    let r = rig(vec![tool_call("look_around", "c1", json!({}))], move |o| {
        o.head = Some(head);
        o.scene = Some(sc);
    });
    let task = r.start();
    sleep(0.1).await;
    r.feed(vec![tool_call("find", "c2", json!({"target": "the door"}))]);
    sleep(0.1).await;
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
    let outs = r.outputs();
    let around = outs.iter().find(|o| o.get("views").is_some()).expect("views");
    assert_eq!(around["ok"], true);
    assert_eq!(around["views"].as_array().unwrap().len(), 5);
    let found = outs.iter().find(|o| o.get("found").is_some()).expect("found");
    assert_eq!(found["found"], true);
    assert_eq!(*scene.located.lock(), vec!["the door".to_string()]);
}

#[tokio::test]
async fn test_goodbye_in_the_owners_words_ends_the_session() {
    for words in ["Okay, bye eve.", "I want you to leave now.", "Stop listening."] {
        let r = rig(vec![], |_| {});
        let task = r.start();
        sleep(0.01).await;
        r.feed(vec![heard(words)]);
        sleep(0.01).await;
        r.feed(vec![spoke("Bye!")]); // eve's goodbye closes the user turn
        sleep(0.01).await;
        assert!(r.farewell() && !r.s.ended(), "{words}: not before the goodbye has been said");
        r.clock.advance(TURN_GAP_SECS + 0.1); // the goodbye goes quiet...
        sleep(0.1).await;
        r.clock.advance(1.0);
        sleep(WATCHDOG_WAIT).await; // ...and the watchdog closes the session
        assert!(r.s.ended(), "{words}");
        r.close();
        finish(&r, task).await;
        assert_eq!(r.states().last().unwrap(), "idle");
        assert!(r.thinking().iter().any(|t| t.contains("[leaving]")));
        assert!(r.s.probe_state().tool_calls.is_empty()); // it never depended on the model calling a tool
    }
}

#[tokio::test]
async fn test_goodbye_closes_even_if_eve_says_nothing_back() {
    let r = rig(vec![], |_| {});
    let task = r.start();
    sleep(0.01).await;
    r.feed(vec![heard("goodbye")]);
    sleep(0.01).await;
    r.clock.advance(TURN_GAP_SECS + 0.1); // the user turn closes on the gap
    sleep(0.1).await;
    assert!(r.farewell() && !r.s.ended());
    r.clock.advance(FAREWELL_MAX_SECS);
    sleep(WATCHDOG_WAIT).await;
    assert!(r.s.ended());
    r.close();
    finish(&r, task).await;
}

fn classifier(f: impl Fn(&str) -> Option<String> + Send + Sync + 'static, seen: Arc<Mutex<Vec<String>>>) -> eve_intent::Classifier {
    let f = Arc::new(f);
    Arc::new(move |text: String| {
        seen.lock().push(text.clone());
        let f = f.clone();
        Box::pin(async move { f(&text) })
    })
}

#[tokio::test]
async fn test_the_classifier_catches_a_goodbye_the_phrase_table_does_not_know() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let intent = classifier(|t| t.contains("heading out").then(|| "leave".to_string()), seen.clone());
    let r = rig(vec![], move |o| o.intent = Some(intent));
    let task = r.start();
    sleep(0.01).await;
    r.feed(vec![heard("alright little guy, I'm heading out"), spoke("See ya.")]);
    sleep(0.05).await;
    assert!(r.farewell());
    r.clock.advance(TURN_GAP_SECS + FAREWELL_QUIET_SECS + 0.5);
    sleep(WATCHDOG_WAIT).await;
    assert!(r.s.ended());
    r.close();
    finish(&r, task).await;
    assert_eq!(*seen.lock(), vec!["alright little guy, I'm heading out".to_string()]);
}

#[tokio::test]
async fn test_leave_the_tab_open_does_not_end_anything() {
    let intent = classifier(|_| None, Arc::new(Mutex::new(Vec::new()))); // the model says: not about eve
    let r = rig(vec![], move |o| o.intent = Some(intent));
    let task = r.start();
    sleep(0.01).await;
    r.feed(vec![heard("leave the tab open"), spoke("On it.")]);
    sleep(0.05).await;
    r.clock.advance(8.0);
    sleep(WATCHDOG_WAIT).await;
    assert!(!r.farewell() && !r.s.ended());
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
}

#[tokio::test]
async fn test_goodbye_during_a_task_stops_the_mic_and_waits_for_the_result() {
    let r = rig_with(vec![tool_call("start_task", "c1", json!({"goal": "play music"}))], "Spotify is playing.", |_| {});
    let task = r.start();
    sleep(0.02).await;
    r.feed(vec![done(), heard("thanks, bye"), spoke("Bye!")]);
    sleep(0.02).await;
    let agent = r.agents.last();
    assert!(r.farewell() && r.s.task_running() && !agent.cancelled.load(Ordering::SeqCst)); // the task is not killed
    let before = r.conn.of("input_audio.append").len();
    r.mic.put_nowait([0x00u8, 0x01].repeat(50)).unwrap();
    sleep(0.02).await;
    assert_eq!(r.conn.of("input_audio.append").len(), before); // eve stopped listening
    r.clock.advance(FAREWELL_MAX_SECS + 1.0);
    sleep(WATCHDOG_WAIT).await;
    assert!(!r.s.ended()); // still waiting on the task
    agent.release.set();
    sleep(0.02).await;
    assert!(r.commentary().last().unwrap().contains("Spotify is playing.")); // the result is said first
    r.feed(vec![spoke("Spotify is playing.")]);
    sleep(0.01).await;
    r.clock.advance(TURN_GAP_SECS + FAREWELL_QUIET_SECS + 0.2);
    sleep(WATCHDOG_WAIT).await;
    assert!(r.s.ended());
    r.close();
    finish(&r, task).await;
    assert!(!agent.cancelled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_mute_and_unmute_by_voice_keep_the_conversation_going() {
    use base64::Engine as _;
    let sound = Arc::new(AtomicBool::new(true));
    let calls = Arc::new(Mutex::new(Vec::<bool>::new()));
    let (snd, cl, snd2) = (sound.clone(), calls.clone(), sound.clone());
    let pcm = base64::engine::general_purpose::STANDARD.encode([0x00u8, 0x10].repeat(50));
    let r = rig(vec![], move |o| {
        o.on_sound = Some(Arc::new(move |on: bool| {
            cl.lock().push(on);
            snd.store(on, Ordering::SeqCst);
        }));
        o.muted = Some(Arc::new(move || !snd2.load(Ordering::SeqCst)));
    });
    let task = r.start();
    sleep(0.01).await;
    r.feed(vec![heard("mute"), spoke("Okay.")]);
    sleep(0.02).await;
    assert_eq!(*calls.lock(), vec![false]);
    assert!(!r.s.ended() && !r.farewell());
    r.feed(vec![json!({"type": "session.output_audio.delta", "delta": pcm})]);
    sleep(0.01).await;
    assert!(r.speaker.played.lock().is_empty()); // muted: the Mac plays nothing
    r.clock.advance(TURN_GAP_SECS + 0.1);
    r.feed(vec![heard("unmute yourself"), spoke("Back.")]);
    sleep(0.02).await;
    assert_eq!(*calls.lock(), vec![false, true]);
    r.feed(vec![json!({"type": "session.output_audio.delta", "delta": pcm})]);
    sleep(0.01).await;
    assert_eq!(r.speaker.played.lock().concat(), [0x00u8, 0x10].repeat(50));
    r.feed(vec![tool_call("set_sound", "c1", json!({"on": false})), tool_call("set_sound", "c2", json!({"on": "loud"}))]);
    sleep(0.02).await;
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
    let notes = r.thinking();
    assert!(notes.iter().any(|n| n.starts_with("[sound] You are muted")));
    assert!(notes.iter().any(|n| n == "[sound] Your sound is back on."));
    assert_eq!(*calls.lock(), vec![false, true, false]);
    assert_eq!(r.outputs()[0], json!({"ok": true, "sound": "off"}));
    assert_eq!(r.outputs()[1], json!({"ok": false, "reason": "on must be true or false"}));
}

#[tokio::test]
async fn test_a_slow_look_holds_off_the_idle_close_and_never_logs_the_view() {
    let logs = Logs::capture();
    let gate = Arc::new(Flag::new());
    let scene = TestScene::slow(gate.clone());
    let r = rig(vec![tool_call("look", "c1", json!({}))], move |o| o.scene = Some(scene));
    let task = r.start();
    sleep(0.02).await;
    r.clock.advance(60.0); // far past the 20 s idle timeout
    sleep(WATCHDOG_WAIT).await;
    assert!(!r.s.ended()); // a look in flight is not idleness
    gate.set();
    sleep(0.05).await;
    assert_eq!(r.outputs()[0]["ok"], true);
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
    let text = logs.text();
    assert!(text.contains("voice: look → ok=True"), "positive control: the look line is logged: {text}");
    assert!(!text.contains("green mug"));
}

#[tokio::test]
async fn test_a_head_request_the_voice_did_not_delegate_is_routed_to_the_backend() {
    // 17:43 on the bench: "look a bit up and to your left" got "Okay, looking." and no tool call.
    let r = rig(vec![], |_| {});
    let task = r.start();
    sleep(0.01).await;
    r.feed(vec![heard("Look a bit up and to your left."), spoke("Okay, looking.")]);
    sleep(LOOK_ROUTE_DELAY_SECS + 0.15).await;
    assert_eq!(r.user_messages(), vec!["[look request] Look a bit up and to your left.".to_string()]);
    assert_eq!(r.kinds().last().unwrap(), "response.create"); // the backend is asked to act on it
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
}

#[tokio::test]
async fn test_a_lesson_request_the_voice_did_not_delegate_is_routed_to_the_backend() {
    // 11:44 on the bench: "teach me about Rust ownership" got a spoken explanation and no lesson.
    let r = rig(vec![], |_| {});
    let task = r.start();
    sleep(0.01).await;
    r.feed(vec![heard("Teach me about Rust ownership."), spoke("Sure. What have you done in Rust so far?")]);
    sleep(LESSON_ROUTE_DELAY_SECS + 0.15).await;
    assert_eq!(r.user_messages(), vec!["[lesson request] Teach me about Rust ownership.".to_string()]);
    assert_eq!(r.kinds().last().unwrap(), "response.create"); // the backend is asked to open the lesson
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
}

#[tokio::test]
async fn test_a_lesson_request_the_voice_delegated_is_not_routed_twice() {
    let r = rig(vec![], |_| {});
    let task = r.start();
    sleep(0.01).await;
    r.clock.set(5.0);
    r.feed(vec![heard("quiz me on French verbs")]);
    sleep(0.01).await;
    r.clock.set(5.3);
    r.feed(vec![delegated(), spoke("Let's do it.")]); // the voice delegated it itself
    sleep(LESSON_ROUTE_DELAY_SECS + 0.15).await;
    assert!(r.user_messages().is_empty());
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
}

#[test]
fn test_the_backend_is_told_what_a_lesson_request_tag_means() {
    assert!(BACKEND_INSTRUCTIONS.contains("[lesson request]"));
    assert!(BACKEND_INSTRUCTIONS.contains("never answer it without the tool"));
}

#[tokio::test]
async fn test_the_lesson_wake_word_opens_in_lesson_setup_and_routes_the_owner_answer() {
    // Say "lesson": the daemon opened the window, eve asks learn-or-bring, and the answer must reach
    // the backend even when the voice just chats back.
    let r = rig(vec![], |o| o.lesson_wake = true);
    let task = r.start();
    sleep(0.05).await;
    let notes = r.user_messages();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].starts_with("[lesson setup] The owner said"));
    assert!(!r.kinds().iter().any(|k| k == "response.create")); // the greeting asks; no backend turn yet
    r.feed(vec![heard("Learn a topic. Fractions, grade four."), spoke("Fractions, nice choice!")]);
    sleep(LESSON_ROUTE_DELAY_SECS + 0.15).await;
    assert_eq!(r.user_messages().last().unwrap(), "[lesson setup] Learn a topic. Fractions, grade four.");
    assert_eq!(r.kinds().last().unwrap(), "response.create");
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
    assert_eq!(r.states()[0], "wake");
}

#[tokio::test]
async fn test_a_started_lesson_ends_lesson_setup_so_later_turns_are_not_routed() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let learning = learning_fn(calls, json!({"ok": true, "answer": "Here is your first problem."}));
    let r = rig(vec![], move |o| {
        o.lesson_wake = true;
        o.learning = Some(learning);
    });
    let task = r.start();
    sleep(0.05).await;
    r.feed(vec![tool_call(
        "lesson",
        "c2",
        json!({"action": "start", "mode": "learn", "topic": "Fractions", "level": "Grade 4"}),
    )]);
    sleep(0.05).await;
    r.s.probe_slow_tasks().await;
    assert!(!r.s.probe_state().lesson_setup);
    let before = r.user_messages().len();
    r.feed(vec![heard("I think I add the tops."), spoke("Hmm, try it.")]);
    sleep(LESSON_ROUTE_DELAY_SECS + 0.15).await;
    assert_eq!(r.user_messages().len(), before); // not a setup answer any more
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
}

#[tokio::test]
async fn test_a_head_request_the_voice_delegated_is_not_routed_twice() {
    let r = rig(vec![], |_| {});
    let task = r.start();
    sleep(0.01).await;
    r.clock.set(5.0);
    r.feed(vec![heard("look a bit more left")]);
    sleep(0.01).await;
    r.clock.set(5.3);
    r.feed(vec![delegated(), spoke("Turning.")]); // the voice delegated it itself
    sleep(LOOK_ROUTE_DELAY_SECS + 0.15).await;
    assert!(r.user_messages().is_empty());
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
}

#[tokio::test]
async fn test_the_classifier_routes_a_head_request_the_table_does_not_know() {
    let intent = classifier(|t| t.contains("keys").then(|| "look".to_string()), Arc::new(Mutex::new(Vec::new())));
    let r = rig(vec![], move |o| o.intent = Some(intent));
    let task = r.start();
    sleep(0.01).await;
    r.feed(vec![heard("where did I leave my keys"), spoke("Let me see.")]);
    sleep(LOOK_ROUTE_DELAY_SECS + 0.15).await;
    assert_eq!(r.user_messages(), vec!["[look request] where did I leave my keys".to_string()]);
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
}

#[tokio::test]
async fn test_a_muted_session_starts_by_telling_the_voice() {
    let r = rig(vec![end_call("c1"), Value::Null], |o| o.muted = Some(Arc::new(|| true)));
    r.s.run().await;
    assert!(r.thinking()[0].starts_with("[sound] You are muted"));
}

/// test_voice_agent.py's `_run_lesson(learning, arguments)`: one lesson call, its output.
async fn run_lesson(learning: Option<LearningFn>, arguments: &str) -> Value {
    let r = rig(vec![], move |o| o.learning = learning);
    r.s.probe_tool("lesson", "math1", arguments).await;
    r.s.probe_slow_tasks().await;
    r.outputs()[0].clone()
}

#[tokio::test]
async fn test_lesson_dispatch_uses_existing_voice_and_returns_saved_step() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let learning = learning_fn(calls.clone(), json!({"ok": true, "answer": "2x = 8. Subtract three from both sides."}));
    let out = run_lesson(Some(learning), r#"{"action":"step"}"#).await;
    assert_eq!(*calls.lock(), vec![obj(json!({"action": "step"}))]);
    assert_eq!(out["ok"], true);
}

#[tokio::test]
async fn test_lesson_without_service_returns_an_actionable_error() {
    let out = run_lesson(None, r#"{"action":"open"}"#).await;
    assert_eq!(out["ok"], false);
}

#[tokio::test]
async fn test_lesson_drops_unknown_arguments() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let learning = learning_fn(calls.clone(), json!({"ok": true, "answer": "Try the ones place."}));
    let out = run_lesson(Some(learning), r#"{"action":"step","bogus":1}"#).await;
    assert_eq!(*calls.lock(), vec![obj(json!({"action": "step"}))]);
    assert_eq!(out["ok"], true);
}

#[tokio::test]
async fn test_lesson_with_unreadable_arguments_does_not_open_the_whiteboard() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    for arguments in ["not json", "[1, 2]"] {
        let learning = learning_fn(calls.clone(), json!({"ok": true}));
        let out = run_lesson(Some(learning), arguments).await;
        assert_eq!(out["ok"], false);
        assert!(out["reason"].as_str().unwrap().contains("Unknown lesson action"));
    }
    assert!(calls.lock().is_empty());
}

#[tokio::test]
async fn test_lesson_stale_lesson_is_actionable() {
    let learning: LearningFn =
        Arc::new(|_r: &eve_learning::LessonRequest| Err(eve_learning::LearningError::NotFound("gone".into())));
    let out = run_lesson(Some(learning), r#"{"action":"hint"}"#).await;
    assert_eq!(out["ok"], false);
    let reason = out["reason"].as_str().unwrap();
    assert!(reason.contains("no longer there") && !reason.contains("lesson failed"));
}

#[test]
fn test_lesson_schema_uses_the_shared_action_set() {
    let tool = TOOLS.as_array().unwrap().iter().find(|t| t["name"] == "lesson").unwrap();
    assert_eq!(tool["parameters"]["properties"]["action"]["enum"], json!(eve_learning::LESSON_ACTIONS));
    assert_eq!(tool["parameters"]["additionalProperties"], false);
}

#[test]
fn test_the_lesson_tool_is_named_lesson_and_teaches_any_subject() {
    // The voice model routes by tool name, so the name must not narrow the subject.
    let names: Vec<&str> = TOOLS.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"lesson") && !names.contains(&"math_lesson"));
    let tool = TOOLS.as_array().unwrap().iter().find(|t| t["name"] == "lesson").unwrap();
    let description = tool["description"].as_str().unwrap();
    assert!(description.contains("any subject") && !description.to_lowercase().contains("math"));
    for text in [INSTRUCTIONS, BACKEND_INSTRUCTIONS] {
        assert!(text.contains("teach me") && text.contains("any subject"));
        assert!(text.contains("lesson") && !text.contains("math_lesson"));
    }
}

// ---- think out loud (test_voice_agent.py 1579-1870) -----------------------------------------------------------------

/// The think-out-loud notices: (on, lesson id).
type Notices = Arc<Mutex<Vec<(bool, Option<String>)>>>;

/// `_listening(conn, clock, **kw)`: a session opened for think out loud on LESSON, with its notices and saves.
struct Listening {
    rig: Rig,
    notices: Notices,
    saved: Arc<Mutex<Vec<(String, String)>>>,
}

fn listening(tweak: impl FnOnce(&mut VoiceSessionOptions)) -> Listening {
    let notices = Arc::new(Mutex::new(Vec::new()));
    let saved = Arc::new(Mutex::new(Vec::new()));
    let (n, sv) = (notices.clone(), saved.clone());
    let rig = rig(vec![], move |o| {
        o.think_aloud = Some(lesson());
        o.on_spoken_idea = Some(Arc::new(move |lesson_id: &str, text: &str| {
            sv.lock().push((lesson_id.to_string(), text.to_string()));
            Ok(())
        }));
        o.on_think_aloud = Some(Arc::new(move |on: bool, lid: Option<&str>| n.lock().push((on, lid.map(str::to_string)))));
        tweak(o);
    });
    Listening { rig, notices, saved }
}

#[test]
fn test_session_config_for_listening_carries_the_rules_and_the_lesson() {
    let plain = session_config(&plain_config(), "", None, "", "", "", "", None, None);
    let lesson = lesson();
    let on = session_config(&plain_config(), "", Some(&lesson), "", "", "", "", None, None);
    let plain_front = plain["instructions"].as_str().unwrap();
    let plain_back = plain["delegation"]["responses"]["instructions"].as_str().unwrap();
    assert!(!plain_front.contains("[think out loud]"));
    assert!(!plain_back.to_lowercase().contains("think out loud"));
    let front = on["instructions"].as_str().unwrap();
    assert!(front.starts_with(plain_front));
    assert!(front.contains("Never say the final answer"));
    assert!(front.contains("Solve 2x + 3 = 11."));
    let backend = on["delegation"]["responses"]["instructions"].as_str().unwrap();
    assert!(backend.contains("Never state the final answer") && backend.contains("Solve 2x + 3 = 11."));
}

#[tokio::test]
async fn test_a_listening_session_opens_without_the_wake_greeting_and_reports_on_and_off() {
    let l = listening(|_| {});
    let task = l.rig.start();
    sleep(0.02).await;
    assert_eq!(*l.notices.lock(), vec![(true, Some("L1".to_string()))]);
    l.rig.s.stop_think_aloud("stopped");
    l.rig.close();
    finish(&l.rig, task).await;
    let states = l.rig.states();
    assert_eq!(states[0], "listening");
    assert!(!states.iter().any(|s| s == "wake"));
    assert!(l.rig.commentary()[0].contains("I'm listening"));
    let started = l.rig.conn.of("session.start");
    assert!(started[0]["instructions"].as_str().unwrap().contains("Solve 2x + 3 = 11."));
    assert_eq!(*l.notices.lock(), vec![(true, Some("L1".to_string())), (false, None)]);
    assert_eq!(l.rig.s.probe_state().end_reason, "stopped");
}

#[tokio::test]
async fn test_listening_waits_through_pauses_and_stops_after_a_minute_of_silence() {
    let l = listening(|_| {});
    let task = l.rig.start();
    sleep(0.02).await;
    l.rig.clock.set(45.0); // past the 20 s idle timeout: a thinking pause, not the end
    sleep(WATCHDOG_WAIT).await;
    assert!(!l.rig.s.ended());
    l.rig.clock.set(61.5);
    sleep(WATCHDOG_WAIT).await;
    assert!(l.rig.s.ended());
    assert_eq!(l.rig.s.probe_state().end_reason, "silence");
    l.rig.close();
    finish(&l.rig, task).await;
    assert_eq!(*l.notices.lock().last().unwrap(), (false, None));
}

#[tokio::test]
async fn test_listening_still_ends_at_the_session_cap() {
    let l = listening(|o| o.config = Some(audio_config(20.0, 100.0)));
    let task = l.rig.start();
    sleep(0.02).await;
    for t in [40.0, 80.0, 101.0] {
        // someone keeps talking: silence never closes it
        l.rig.clock.set(t);
        l.rig.s.probe_set_last_activity(t);
        sleep(WATCHDOG_WAIT).await;
    }
    assert!(l.rig.s.ended());
    assert_eq!(l.rig.s.probe_state().end_reason, "cap");
    l.rig.close();
    finish(&l.rig, task).await;
}

#[tokio::test]
async fn test_spoken_thinking_is_saved_to_the_lesson_and_never_logged_or_remembered() {
    let logs = Logs::capture();
    let l = listening(|_| {});
    let task = l.rig.start();
    sleep(0.02).await;
    l.rig.feed(vec![heard("I think I take away three from both sides")]);
    sleep(0.02).await;
    l.rig.clock.set(2.0); // past the turn gap: the turn closes
    eventually(|| !l.saved.lock().is_empty(), 2.0).await;
    l.rig.feed(vec![spoke("Nice start. What is left on the right side?")]);
    sleep(0.02).await;
    l.rig.clock.set(4.0);
    sleep(0.1).await;
    l.rig.s.stop_think_aloud("stopped");
    l.rig.close();
    finish(&l.rig, task).await;
    assert_eq!(*l.saved.lock(), vec![("L1".to_string(), "I think I take away three from both sides".to_string())]);
    assert!(l.rig.s.probe_state().turns.is_empty()); // nothing reaches chat memory
    let text = logs.text();
    assert!(!text.contains("take away three") && !text.contains("What is left"));
    assert!(text.contains("spoken idea saved ok=True"));
}

#[tokio::test]
async fn test_saying_done_says_goodbye_and_is_not_saved_as_an_idea() {
    let l = listening(|_| {});
    let task = l.rig.start();
    sleep(0.02).await;
    l.rig.feed(vec![heard("okay I'm done thinking")]);
    sleep(0.02).await;
    l.rig.clock.set(2.0);
    eventually(|| l.rig.farewell(), 2.0).await;
    l.rig.clock.set(2.0 + FAREWELL_MAX_SECS + 1.0);
    eventually(|| l.rig.s.ended(), 2.0).await;
    l.rig.close();
    finish(&l.rig, task).await;
    assert!(l.saved.lock().is_empty());
    assert_eq!(l.rig.s.probe_state().end_reason, "goodbye");
}

#[tokio::test]
async fn test_while_listening_lesson_ideas_never_overwrites_and_stop_listening_says_goodbye() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let learning = learning_fn(calls.clone(), json!({"ok": true, "answer": "One step."}));
    let l = listening(move |o| o.learning = Some(learning));
    let s = &l.rig.s;
    s.probe_tool("lesson", "m1", r#"{"action":"ideas","text":"erase it all"}"#).await;
    s.probe_tool("lesson", "m2", r#"{"action":"stop-listening"}"#).await;
    s.probe_slow_tasks().await;
    s.probe_tool("start_task", "t1", r#"{"goal":"open Safari"}"#).await;
    s.probe_tool("go_explore", "t2", "{}").await;
    assert!(calls.lock().is_empty());
    let outs = l.rig.outputs();
    let first = outs.iter().find(|o| o.get("answer").is_some_and(|a| a.as_str().unwrap().contains("Already saved")));
    assert!(first.is_some_and(|o| o["ok"] == true));
    assert!(outs.iter().any(|o| o["ok"] == true && o["answer"] == "Say a two- or three-word goodbye."));
    assert!(l.rig.farewell());
    assert_eq!(outs[2]["ok"], false);
    assert_eq!(outs[3]["ok"], false);
    let probe = s.probe_state();
    assert!(!probe.has_agent && !probe.explore_requested);
}

#[tokio::test]
async fn test_a_step_while_listening_waits_for_what_the_learner_just_said() {
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let (o1, o2) = (order.clone(), order.clone());
    let r = rig(vec![], move |o| {
        o.think_aloud = Some(lesson());
        o.on_spoken_idea = Some(Arc::new(move |_lid: &str, _text: &str| {
            o1.lock().push("saved".into());
            Ok(())
        }));
        o.learning = Some(Arc::new(move |req: &eve_learning::LessonRequest| {
            o2.lock().push(req.to_map()["action"].as_str().unwrap().to_string());
            Ok(obj(json!({"ok": true, "answer": "2x = 8"})))
        }));
    });
    r.s.probe_transcript_delta("user", "can you show me one step"); // the turn is still open when the tool is called
    r.s.probe_tool("lesson", "m1", r#"{"action":"step"}"#).await;
    r.s.probe_slow_tasks().await;
    assert_eq!(*order.lock(), vec!["saved".to_string(), "step".to_string()]);
    assert_eq!(r.outputs()[0]["ok"], true);
}

#[tokio::test]
async fn test_an_open_conversation_switches_to_listening_without_a_second_session() {
    let notices = Arc::new(Mutex::new(Vec::new()));
    let n = notices.clone();
    let r = rig(vec![], move |o| {
        o.on_think_aloud = Some(Arc::new(move |on: bool, lid: Option<&str>| n.lock().push((on, lid.map(str::to_string)))));
    });
    let task = r.start();
    sleep(0.02).await;
    let out = r.s.enter_think_aloud(&lesson()).await;
    let again = r.s.enter_think_aloud(&lesson()).await;
    r.clock.set(30.0); // past the conversation's idle timeout
    sleep(WATCHDOG_WAIT).await;
    assert!(!r.s.ended());
    r.s.stop_think_aloud("stopped");
    r.close();
    finish(&r, task).await;
    assert_eq!(Value::Object(out), json!({"ok": true}));
    assert_eq!(again["already"], true);
    assert!(r.commentary()[0].contains("Greet them"));
    assert!(r.thinking().iter().any(|t| t.contains("[think out loud]") && t.contains("Solve 2x + 3 = 11.")));
    let updates = r.conn.of("session.update");
    assert_eq!(updates.len(), 1);
    assert!(updates[0]["delegation"]["responses"]["instructions"].as_str().unwrap().contains("Never state the final answer"));
    assert_eq!(*notices.lock(), vec![(true, Some("L1".to_string())), (false, None)]);
}

#[tokio::test]
async fn test_a_closing_conversation_refuses_to_switch() {
    let r = rig(vec![], |_| {});
    r.s.end();
    let out = r.s.enter_think_aloud(&lesson()).await;
    assert_eq!(out["ok"], false);
    assert!(r.s.think_aloud_lesson_id().is_none());
}

#[tokio::test]
async fn test_outside_listening_eves_words_are_logged_as_before() {
    // Positive control for the no-words log test: the same reply is logged when nobody is thinking out loud.
    let logs = Logs::capture();
    let r = rig(vec![], |_| {});
    r.s.probe_close_turn("assistant", "Nice start. What is left on the right side?");
    assert!(logs.text().contains("What is left"));
}

#[tokio::test]
async fn test_while_listening_only_goodbye_and_mute_act_and_nothing_is_starred() {
    let dir = support::tmp();
    let memory = TestMemory::new(&dir, "");
    let sounds = Arc::new(Mutex::new(Vec::<bool>::new()));
    let classified = Arc::new(Mutex::new(Vec::new()));
    let (m, snd) = (memory.clone(), sounds.clone());
    let intent = classifier(|_| Some("leave".to_string()), classified.clone());
    let l = listening(move |o| {
        o.memory = Some(m);
        o.on_sound = Some(Arc::new(move |on: bool| snd.lock().push(on)));
        o.intent = Some(intent);
    });
    let s = &l.rig.s;
    s.probe_close_turn("user", "I think the answer has a 3 in it");
    s.probe_close_turn("user", "remember that");
    s.probe_close_turn("user", "look to your left");
    s.probe_close_turn("user", "mute");
    sleep(0.05).await;
    s.probe_idea_saves().await;
    assert!(memory.starred.lock().is_empty());
    assert!(!l.rig.farewell());
    assert!(classified.lock().is_empty());
    assert_eq!(memory.lines(&s.conv), 0); // and nothing reaches the transcript
    assert_eq!(*sounds.lock(), vec![false]);
    let saved: Vec<String> = l.saved.lock().iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(saved, vec!["I think the answer has a 3 in it", "remember that", "look to your left"]);
}

#[tokio::test]
async fn test_spoken_ideas_are_saved_in_the_order_spoken_even_when_one_save_is_slow() {
    let saved = Arc::new(Mutex::new(Vec::<String>::new()));
    let sv = saved.clone();
    let r = rig(vec![], move |o| {
        o.think_aloud = Some(lesson());
        o.on_think_aloud = Some(Arc::new(|_on: bool, _lid: Option<&str>| {}));
        o.on_spoken_idea = Some(Arc::new(move |_lid: &str, text: &str| {
            if text == "first" {
                std::thread::sleep(Duration::from_millis(100)); // a slow disk write must not let the next idea jump ahead
            }
            sv.lock().push(text.to_string());
            Ok(())
        }));
    });
    r.s.probe_close_turn("user", "first");
    r.s.probe_close_turn("user", "second");
    r.s.probe_close_turn("user", "third");
    eventually(|| r.s.probe_state().idea_saves == 0 && saved.lock().len() == 3, 2.0).await;
    assert_eq!(*saved.lock(), vec!["first", "second", "third"]);
}

#[tokio::test]
async fn test_a_running_task_refuses_listening() {
    let r = rig(vec![], |_| {});
    r.s.probe_mark_started();
    r.s.probe_tool("start_task", "t1", r#"{"goal":"open Mail"}"#).await;
    let out = r.s.enter_think_aloud(&lesson()).await;
    r.agents.last().cancel("");
    r.s.probe_agent_task().await;
    assert_eq!(out["ok"], false);
    assert!(out["reason"].as_str().unwrap().contains("computer task"));
}

// ---- take_photo (test_voice_agent.py 1871-1944) --------------------------------------------------------------------

#[tokio::test]
async fn test_take_photo_hands_the_note_to_the_daemon_and_returns_the_caption() {
    let asked = Arc::new(Mutex::new(Vec::<String>::new()));
    let a = asked.clone();
    let r = rig(vec![tool_call("take_photo", "c1", json!({"note": "my bike"}))], move |o| {
        o.on_photo = Some(Arc::new(move |note: String| {
            a.lock().push(note);
            async {
                obj(json!({"ok": true, "path": "/n/photos/2026-09-13/120000-7.jpg", "caption": "a red bike by the door",
                           "answer": "Kept it: a red bike by the door"}))
            }
            .boxed()
        }));
    });
    let task = r.start();
    eventually(|| !r.outputs().is_empty(), 2.0).await;
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
    assert_eq!(*asked.lock(), vec!["my bike".to_string()]);
    assert_eq!(r.outputs()[0]["ok"], true);
    assert_eq!(r.outputs()[0]["caption"], "a red bike by the door");
}

#[tokio::test]
async fn test_take_photo_without_a_camera_or_when_it_stalls_says_why() {
    async fn go(r: &Rig) {
        let task = r.start();
        eventually(|| !r.outputs().is_empty(), 2.0).await;
        r.feed(vec![end_call("c9"), Value::Null]);
        finish(r, task).await;
    }
    let r = rig(vec![tool_call("take_photo", "c1", json!({"note": ""}))], |_| {});
    go(&r).await;
    let out = &r.outputs()[0];
    assert_eq!(out["ok"], false);
    assert!(out["reason"].as_str().unwrap().contains("no camera"));

    let r = rig(vec![tool_call("take_photo", "c1", json!({"note": ""}))], |o| {
        o.on_photo = Some(Arc::new(|_note: String| {
            async {
                sleep(5.0).await;
                obj(json!({"ok": true}))
            }
            .boxed()
        }));
    });
    let was = std::mem::replace(&mut *PHOTO_TIMEOUT_SECS.lock(), 0.02);
    go(&r).await;
    *PHOTO_TIMEOUT_SECS.lock() = was;
    let out = &r.outputs()[0];
    assert_eq!(out["ok"], false);
    assert!(out["reason"].as_str().unwrap().contains("did not answer in time"));
}

#[tokio::test]
async fn test_live_expression_callback_receives_streamed_reply_and_user_turn() {
    let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let sn = seen.clone();
    let r = rig(vec![], move |o| {
        o.on_expression = Some(Arc::new(move |who: &str, text: &str| sn.lock().push((who.into(), text.into()))));
    });
    r.s.probe_transcript_delta("assistant", "That is such wonderful news!");
    assert_eq!(*seen.lock(), vec![("assistant".to_string(), "That is such wonderful news!".to_string())]);
    r.s.probe_close_turn("user", "I found something interesting.");
    assert_eq!(*seen.lock().last().unwrap(), ("user".to_string(), "I found something interesting.".to_string()));
}

#[tokio::test]
async fn test_the_records_profile_reaches_the_voice_prompt_and_is_absent_without_one() {
    let plain = session_config(&plain_config(), "", None, "", "", "", "", None, None);
    let empty = session_config(&plain_config(), "", None, "", "", "", "", None, None);
    assert_eq!(empty["instructions"], plain["instructions"]);
    let page = "## Life context\n- Their name is Sam.";
    let with_page = session_config(&plain_config(), "", None, page, "", "", "", None, None);
    let with_page = with_page["instructions"].as_str().unwrap();
    assert!(with_page.contains(page));
    assert_eq!(with_page.replace(&eve_brain::voice_agent::profile_block(page), ""), plain["instructions"].as_str().unwrap());
    assert!(with_page.contains("without reciting it"));
    let dir = support::tmp();
    let memory = TestMemory::new(&dir, page);
    let r = rig(vec![end_call("c1"), Value::Null], move |o| o.memory = Some(memory));
    r.s.run().await;
    let started = r.conn.of("session.start");
    assert!(!started.is_empty() && started[0]["instructions"].as_str().unwrap().contains(page));
}

// ---- Meet: "join my meeting" out loud (meet.py; test_voice_agent.py 2383-2633) ------------------------------------

fn tool_names(cfg: &Obj) -> Vec<String> {
    cfg["delegation"]["responses"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn meet_tools() -> Vec<Obj> {
    MEET_TOOLS.as_array().unwrap().iter().map(|t| t.as_object().cloned().unwrap()).collect()
}

/// A Meeter whose browser never opens: joining is off, or the page cannot be made.
fn meeter(enabled: bool, dir: &tempfile::TempDir) -> Arc<eve_meet::Meeter> {
    let config = eve_meet::MeetConfig { enabled, ..eve_meet::MeetConfig::default() };
    let make_page: eve_meet::MakePage = Arc::new(|| Err(anyhow::anyhow!("no browser in a test")));
    Arc::new(eve_meet::Meeter::new(config, make_page, dir.path(), eve_meet::MeeterOptions::default()))
}

#[test]
fn test_the_meeting_tools_are_offered_only_with_a_meeter_and_never_in_a_lesson() {
    let cfg = plain_config();
    assert!(!tool_names(&session_config(&cfg, "", None, "", "", "", "", None, None)).contains(&"join_meeting".into()));
    let meet = meet_tools();
    let with_meet = tool_names(&session_config(&cfg, "", None, "", "", "", "", None, Some(&meet)));
    assert!(with_meet.contains(&"join_meeting".into()) && with_meet.contains(&"leave_meeting".into()));
    let lesson = obj(json!({"id": "l1", "topic": "fractions"}));
    let in_lesson = tool_names(&session_config(&cfg, "", Some(&lesson), "", "", "", "", None, Some(&meet)));
    assert!(!in_lesson.contains(&"join_meeting".into()));
}

#[tokio::test]
async fn test_join_meeting_out_loud_reaches_the_meeter_and_leave_too() {
    // Python's fake Meeter answered the join; the real one, with joining off, answers with its own reason, and its
    // leave says there is no call: both are the Meeter's words, so both calls reached it.
    let dir = support::tmp();
    let m = meeter(false, &dir);
    let r = rig(vec![tool_call("join_meeting", "c1", json!({"meeting": "my 3pm"}))], move |o| o.meeter = Some(m));
    let task = r.start();
    eventually(|| !r.outputs().is_empty(), 2.0).await;
    // leaving is routine, no question
    r.feed(vec![tool_call("leave_meeting", "c2", json!({}))]);
    eventually(|| r.outputs().len() >= 2, 2.0).await;
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
    let outs = r.outputs();
    assert_eq!(outs[0], json!({"ok": false, "reason": eve_meet::OFF_REASON}));
    assert_eq!(outs[1], json!({"ok": false, "reason": "I'm not in a meeting"}));

    // a join that raises (the page cannot be made) answers that the call failed
    let m = meeter(true, &dir);
    let r = rig(vec![tool_call("join_meeting", "c1", json!({"meeting": "meet.google.com/abc-defg-hij"}))], move |o| {
        o.meeter = Some(m)
    });
    let task = r.start();
    eventually(|| !r.outputs().is_empty(), 2.0).await;
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
    assert_eq!(r.outputs()[0], json!({"ok": false, "reason": "join_meeting failed"}));
}

#[tokio::test]
async fn test_join_meeting_without_a_meeter_says_why() {
    let r = rig(vec![tool_call("join_meeting", "c1", json!({"meeting": "now"}))], |_| {});
    let task = r.start();
    eventually(|| !r.outputs().is_empty(), 2.0).await;
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
    let out = &r.outputs()[0];
    assert_eq!(out["ok"], false);
    assert!(out["reason"].as_str().unwrap().contains("off"));
}

fn lights_hub() -> Arc<TestHub> {
    Arc::new(TestHub {
        tools: eve_lights::TOOLS.as_array().cloned().unwrap_or_default(),
        instructions: "The lights:\n- floor lamp".into(),
        no_screenshot: None,
        reply: json!({"ok": true, "done": ["floor lamp"], "failed": [], "change": "off"}),
        calls: Mutex::new(Vec::new()),
    })
}

fn spotify_hub() -> Arc<TestHub> {
    let tool = |name: &str| {
        json!({"type": "function", "name": name, "description": "d",
               "parameters": {"type": "object", "properties": {}, "additionalProperties": false}})
    };
    Arc::new(TestHub {
        tools: vec![tool("spotify_now_playing"), tool("spotify_control")],
        instructions: "You can control the owner's Spotify with spotify_control (play, pause, next, previous, volume)."
            .into(),
        no_screenshot: None,
        reply: json!({"ok": true, "line": "Paused."}),
        calls: Mutex::new(Vec::new()),
    })
}

fn tv_hub() -> Arc<TestHub> {
    Arc::new(TestHub {
        tools: eve_firetv::TOOLS.clone(),
        instructions: eve_firetv::INSTRUCTIONS.clone(),
        no_screenshot: Some(eve_firetv::INSTRUCTIONS_NO_SCREENSHOT.clone()),
        reply: json!({"ok": true, "line": "TV volume 30%."}),
        calls: Mutex::new(Vec::new()),
    })
}

fn camera_hub() -> Arc<TestHub> {
    Arc::new(TestHub {
        tools: eve_obsbot_panel::TOOLS.clone(),
        instructions: eve_obsbot_panel::INSTRUCTIONS.clone(),
        no_screenshot: None,
        reply: json!({"ok": true, "pan": 20, "tilt": 0, "line": "Camera pointing 20° left."}),
        calls: Mutex::new(Vec::new()),
    })
}

/// `voice_agent.session_config(cfg, think_aloud=..., **{slot: hub})`.
fn config_with(slot: Option<(&str, Arc<TestHub>)>, think_aloud: Option<&Obj>) -> Obj {
    let hubs = match slot {
        Some((slot, hub)) => hubs(slot, hub),
        None => AppHubs::default(),
    };
    session_config_with(&plain_config(), "", think_aloud, "", "", "", "", None, None, &hubs, "")
}

fn backend_text(cfg: &Obj) -> String {
    cfg["delegation"]["responses"]["instructions"].as_str().unwrap().to_string()
}

/// One app tool call out loud: the hub it reached and the output.
async fn out_loud(slot: &str, hub: Arc<TestHub>, name: &str, args: Value) -> Value {
    let h = hubs(slot, hub);
    let r = rig(vec![tool_call(name, "c1", args)], move |o| o.app_hubs = h);
    let task = r.start();
    eventually(|| !r.outputs().is_empty(), 2.0).await;
    r.feed(vec![end_call("c9"), Value::Null]);
    finish(&r, task).await;
    r.outputs()[0].clone()
}

#[test]
fn test_the_lights_are_offered_to_the_backend_only_with_lights_and_never_in_a_lesson() {
    assert!(!tool_names(&config_with(None, None)).contains(&"lights_set".into()));
    let on = config_with(Some(("lights", lights_hub())), None);
    let names = tool_names(&on);
    assert!(names.contains(&"lights_set".into()) && names.contains(&"lights_status".into()));
    assert!(backend_text(&on).contains("floor lamp"));
    let lesson = obj(json!({"id": "l1", "topic": "fractions"}));
    let in_lesson = config_with(Some(("lights", lights_hub())), Some(&lesson));
    assert!(!tool_names(&in_lesson).contains(&"lights_set".into()) && !backend_text(&in_lesson).contains("floor lamp"));
}

#[tokio::test]
async fn test_lights_out_loud_reach_the_lights() {
    register_app("lights", &eve_lights::TOOL_NAMES);
    let hub = lights_hub();
    let args = json!({"target": "all", "power": false, "brightness": null, "color": null});
    let out = out_loud("lights", hub.clone(), "lights_set", args.clone()).await;
    assert_eq!(*hub.calls.lock(), vec![("lights_set".to_string(), args)]);
    assert_eq!(out["done"], json!(["floor lamp"]));
}

#[test]
fn test_spotify_is_offered_to_the_backend_only_when_logged_in_and_never_in_a_lesson() {
    assert!(!tool_names(&config_with(None, None)).contains(&"spotify_control".into()));
    let on = config_with(Some(("spotify", spotify_hub())), None);
    let names = tool_names(&on);
    assert!(names.contains(&"spotify_now_playing".into()) && names.contains(&"spotify_control".into()));
    assert!(backend_text(&on).contains("spotify_control"));
    let lesson = obj(json!({"id": "l1", "topic": "fractions"}));
    assert!(!tool_names(&config_with(Some(("spotify", spotify_hub())), Some(&lesson))).contains(&"spotify_control".into()));
}

#[tokio::test]
async fn test_music_out_loud_reaches_spotify() {
    register_app("spotify", &["spotify_now_playing", "spotify_control"]);
    let hub = spotify_hub();
    let args = json!({"action": "pause", "query": null, "kind": null, "volume": null, "volume_change": null,
                      "device": null});
    let out = out_loud("spotify", hub.clone(), "spotify_control", args.clone()).await;
    assert_eq!(*hub.calls.lock(), vec![("spotify_control".to_string(), args)]);
    assert_eq!(out["line"], "Paused.");
}

#[test]
fn test_the_tv_is_offered_out_loud_only_when_paired_never_in_a_lesson_and_without_the_screenshot() {
    let tv = ["firetv_status", "firetv_control", "firetv_screenshot"];
    let off = config_with(None, None);
    assert!(!tool_names(&off).iter().any(|n| tv.contains(&n.as_str())));
    assert!(!backend_text(&off).contains("firetv_control"));
    let on = config_with(Some(("firetv", tv_hub())), None);
    let names = tool_names(&on);
    assert!(names.contains(&"firetv_status".into()) && names.contains(&"firetv_control".into()));
    assert!(!names.contains(&"firetv_screenshot".into())); // its picture goes to a chat; a voice has none
    let text = backend_text(&on);
    assert!(text.contains("firetv_control"));
    assert!(text.contains(VOICE_FIRETV_NOTE));
    assert!(!text.contains("firetv_screenshot")); // no tool it doesn't have
    let lesson = obj(json!({"id": "l1", "topic": "fractions"}));
    let in_lesson = config_with(Some(("firetv", tv_hub())), Some(&lesson));
    assert!(!tool_names(&in_lesson).iter().any(|n| tv.contains(&n.as_str())));
    assert!(!backend_text(&in_lesson).contains("firetv_control"));
}

#[tokio::test]
async fn test_the_tv_out_loud_reaches_the_tv() {
    register_app("firetv", &eve_firetv::TOOL_NAMES);
    let hub = tv_hub();
    let args = json!({"action": "volume", "app": null, "query": null, "text": null, "volume": 30,
                      "volume_change": null, "repeat": null});
    let out = out_loud("firetv", hub.clone(), "firetv_control", args.clone()).await;
    assert_eq!(*hub.calls.lock(), vec![("firetv_control".to_string(), args)]);
    assert_eq!(out["line"], "TV volume 30%.");
}

#[test]
fn test_the_camera_is_offered_out_loud_only_when_there_is_one_and_never_in_a_lesson() {
    let cam = ["camera_status", "camera_control"];
    assert!(!tool_names(&config_with(None, None)).iter().any(|n| cam.contains(&n.as_str())));
    let on = config_with(Some(("obsbot", camera_hub())), None);
    let names = tool_names(&on);
    assert!(names.contains(&"camera_status".into()) && names.contains(&"camera_control".into()));
    assert!(backend_text(&on).contains("camera_control"));
    let lesson = obj(json!({"id": "l1", "topic": "fractions"}));
    let in_lesson = config_with(Some(("obsbot", camera_hub())), Some(&lesson));
    assert!(!tool_names(&in_lesson).iter().any(|n| cam.contains(&n.as_str())));
}

#[tokio::test]
async fn test_the_camera_out_loud_reaches_the_camera() {
    register_app("obsbot", &eve_obsbot_panel::TOOL_NAMES);
    let hub = camera_hub();
    let args = json!({"action": "turn", "direction": "left", "degrees": 20, "pan": null, "tilt": null, "zoom": null,
                      "zoom_change": null, "mode": null, "preset": null});
    let out = out_loud("obsbot", hub.clone(), "camera_control", args.clone()).await;
    assert_eq!(*hub.calls.lock(), vec![("camera_control".to_string(), args)]);
    assert_eq!(out["line"], "Camera pointing 20° left.");
}
