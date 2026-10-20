//! Tests for the voice_lesson fill unit: test_daemon_task_cancel.py, test_daemon_explore.py and the daemon half
//! of test_daemon_lesson.py. No board, no mic, no network: a recording board link, fake microphones, sessions
//! and agents, and the daemon built from [`DaemonParts::for_test`].
//!
//! Python built a `SimpleNamespace` with only the attributes a method reads; here every test builds the real
//! daemon from inert parts, so a test that reaches a method of another fill unit waits on that unit (ignored with
//! its name until the integrator fills it).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_daemon::seams::{BoardLink, Job, Mic, Obj, TaskAgent, VoiceSessionHandle};
use eve_daemon::{Daemon, DaemonParts};

// ---- fakes ------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct Board {
    connected: AtomicBool,
    sent: Mutex<Vec<Value>>,
}

impl Board {
    fn new(connected: bool) -> Arc<Board> {
        Arc::new(Board { connected: AtomicBool::new(connected), sent: Mutex::new(Vec::new()) })
    }
    fn sent(&self) -> Vec<Value> {
        self.sent.lock().clone()
    }
    fn set_connected(&self, on: bool) {
        self.connected.store(on, Ordering::SeqCst);
    }
}

#[async_trait]
impl BoardLink for Board {
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

struct Agent {
    running: AtomicBool,
    reason: Mutex<Option<String>>,
}

impl Agent {
    fn new(running: bool) -> Arc<Agent> {
        Arc::new(Agent { running: AtomicBool::new(running), reason: Mutex::new(None) })
    }
}

#[async_trait]
impl TaskAgent for Agent {
    async fn run(&self, _goal: &str, _note: &str) -> anyhow::Result<String> {
        Ok(String::new())
    }
    fn cancel(&self, reason: &str) {
        *self.reason.lock() = Some(reason.to_string());
        self.running.store(false, Ordering::SeqCst);
    }
    fn steer(&self, _text: &str) -> bool {
        false
    }
    fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

/// A microphone that is always open and hears nothing.
#[derive(Default)]
struct FakeMic {
    unsubscribed: AtomicBool,
}

impl Mic for FakeMic {
    fn listening(&self) -> bool {
        true
    }
    fn prepare(&self) -> bool {
        true
    }
    fn start(&self) -> bool {
        true
    }
    fn stop(&self) {}
    fn subscribe(&self) -> eve_ears::PcmQueue {
        eve_ears::PcmQueue::new(0)
    }
    fn unsubscribe(&self, _q: &eve_ears::PcmQueue) {
        self.unsubscribed.store(true, Ordering::SeqCst);
    }
    fn take_last_wake_audio(&self) -> Vec<u8> {
        Vec::new()
    }
}

#[derive(Default)]
struct FakeSession {
    lesson_id: Mutex<Option<String>>,
    ended: AtomicBool,
    entered: Mutex<Vec<String>>,
    prepared: Mutex<Vec<String>>,
    stopped: Mutex<Vec<String>>,
    /// None: enter_think_aloud answers ok; Some(reason): it refuses.
    refuse: Option<String>,
}

fn id_of(lesson: &Obj) -> String {
    lesson.get("id").and_then(Value::as_str).unwrap_or_default().to_string()
}

#[async_trait]
impl VoiceSessionHandle for FakeSession {
    fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }
    fn think_aloud_lesson_id(&self) -> Option<String> {
        self.lesson_id.lock().clone()
    }
    async fn enter_think_aloud(&self, lesson: &Obj) -> Obj {
        if let Some(reason) = &self.refuse {
            return obj(json!({"ok": false, "reason": reason}));
        }
        self.entered.lock().push(id_of(lesson));
        *self.lesson_id.lock() = Some(id_of(lesson));
        obj(json!({"ok": true}))
    }
    fn prepare_think_aloud(&self, lesson: &Obj) {
        self.prepared.lock().push(id_of(lesson));
    }
    fn stop_think_aloud(&self, reason: &str) {
        self.stopped.lock().push(reason.to_string());
        self.ended.store(true, Ordering::SeqCst);
    }
}

/// `server.app.voice` as a test's function, the rest of the learning server inert.
struct FakeLearning {
    #[allow(clippy::type_complexity)]
    voice: Box<dyn Fn(&eve_learning::LessonRequest) -> Result<Obj, eve_learning::LearningError> + Send + Sync>,
    calls: Mutex<Vec<Obj>>,
}

impl eve_daemon::seams::Learning for FakeLearning {
    fn voice(&self, req: &eve_learning::LessonRequest) -> Result<Obj, eve_learning::LearningError> {
        self.calls.lock().push(req.to_map());
        (self.voice)(req)
    }
    fn append_spoken(&self, _lesson_id: &str, _text: &str) -> Result<bool, eve_learning::LearningError> {
        Ok(true)
    }
    fn url(&self) -> String {
        String::new()
    }
    fn active_summary(&self) -> Option<Obj> {
        None
    }
    fn shutdown(&self) {}
}

fn learning(
    voice: impl Fn(&eve_learning::LessonRequest) -> Result<Obj, eve_learning::LearningError> + Send + Sync + 'static,
) -> Arc<FakeLearning> {
    Arc::new(FakeLearning { voice: Box::new(voice), calls: Mutex::new(Vec::new()) })
}

fn answers(answer: &'static str) -> Arc<FakeLearning> {
    learning(move |_| Ok(obj(json!({"ok": true, "answer": answer}))))
}

/// The real demo `LearningApp` as the daemon's learning server, its listener the daemon's own.
struct AppLearning(eve_learning::LearningApp);

impl eve_daemon::seams::Learning for AppLearning {
    fn voice(&self, req: &eve_learning::LessonRequest) -> Result<Obj, eve_learning::LearningError> {
        self.0.voice(req)
    }
    fn append_spoken(&self, lesson_id: &str, text: &str) -> Result<bool, eve_learning::LearningError> {
        self.0.append_spoken(lesson_id, text)
    }
    fn url(&self) -> String {
        self.0.url()
    }
    fn active_summary(&self) -> Option<Obj> {
        None
    }
    fn shutdown(&self) {}
}

// ---- helpers ----------------------------------------------------------------------------------------------------

fn obj(v: Value) -> Obj {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn explore_cfg(enabled: bool) -> eve_explore::ExploreConfig {
    eve_explore::ExploreConfig {
        enabled,
        after_secs: 600.0,
        notes_per_hour: 0.0,
        model: "fake".into(),
        notes_dir: "/nonexistent".into(),
        cycle_wait_secs: 900.0,
    }
}

/// `_daemon(...)`: a daemon on `board` whose explorer is `Explorer(_cfg(enabled), now=0.0, notes_enabled=False)`.
fn daemon_with(board: &Arc<Board>, enabled: bool, adjust: impl FnOnce(&mut DaemonParts)) -> (Arc<Daemon>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut parts = DaemonParts::for_test(board.clone(), dir.path());
    parts.explore_cfg = explore_cfg(enabled);
    adjust(&mut parts);
    let d = Daemon::assemble(parts);
    *d.explorer.lock() = eve_explore::Explorer::with_options(
        explore_cfg(enabled),
        0.0,
        eve_explore::ExplorerOptions { notes_enabled: false, ..Default::default() },
    );
    d.st.lock().last_activity_at = 0.0;
    (d, dir)
}

fn daemon(board: &Arc<Board>) -> (Arc<Daemon>, tempfile::TempDir) {
    daemon_with(board, true, |_| {})
}

const MODE_ON: fn() -> Value = || json!({"cmd": "mode", "explore": true});
const MODE_OFF: fn() -> Value = || json!({"cmd": "mode", "explore": false});
const THINKING: fn() -> Value = || json!({"cmd": "agent", "state": "thinking"});
const LISTEN_ON: fn() -> Value = || json!({"cmd": "listen", "on": true});
const LISTEN_OFF: fn() -> Value = || json!({"cmd": "listen", "on": false});

/// The look the explorer opens a pan with, whatever the pan plan is.
fn first_look() -> Value {
    let (yaw, pitch) = eve_explore::WAYPOINTS[0];
    json!({"cmd": "look", "yaw": yaw, "pitch": pitch, "hold": 6000})
}

fn captions(board: &Board) -> Vec<Value> {
    board.sent().into_iter().filter(|m| m["cmd"] == "caption").collect()
}

fn shown_captions(board: &Board) -> Vec<Value> {
    captions(board).into_iter().filter(|m| m.get("clear").is_none()).collect()
}

fn states(board: &Board) -> Vec<String> {
    board
        .sent()
        .into_iter()
        .filter(|m| m["cmd"] == "agent")
        .map(|m| m["state"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn index_of(sent: &[Value], want: &Value) -> usize {
    sent.iter().position(|m| m == want).expect("sent")
}

/// `_settle()`: the caption and agent-state sends are tasks of their own.
async fn settle() {
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
}

/// A conversation that stays open until cancelled (`_Talking`).
fn talking() -> Job {
    Job::spawn("voice-conversation", futures::future::pending::<()>())
}

fn thought(text: &str, tags: &[&str], photographed: bool) -> eve_diary::Thought {
    eve_diary::Thought {
        text: text.into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        written: true,
        photographed,
        cool: 0.5,
        novelty: 8,
        importance: 7,
    }
}

const JPEG_B64: &str = "/9j/4AAAAAAAAAAAAAAAAAAAAAD/2Q==";
const JPEG: &[u8] = b"\xff\xd8\xff\xe0\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xff\xd9";

fn snap_line(snap: bool, w: i64, h: i64) -> Obj {
    let mut f = json!({"seq": 7, "w": w, "h": h, "fmt": "jpeg",
                       "b64": JPEG_B64, "yaw": 20, "pitch": 40});
    if snap {
        f["snap"] = json!(true);
    }
    obj(json!({"frame": f}))
}

// ---- test_daemon_task_cancel.py -----------------------------------------------------------------------------

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_cancel_active_task_cancels_and_captions() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    let agent = Agent::new(true);
    d.st.lock().active_agent = Some(agent.clone());
    d.cancel_active_task("the daemon is restarting").await;
    assert_eq!(agent.reason.lock().as_deref(), Some("the daemon is restarting"));
    assert_eq!(
        board.sent(),
        vec![json!({"cmd": "caption", "lines": ["stopped: the daem"], "page": 0, "of": 1, "hold_ms": 4000,
                    "final": true, "chirp": false})]
    );
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_cancel_active_task_is_a_noop_without_a_running_task() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.st.lock().active_agent = Some(Agent::new(false));
    d.cancel_active_task("x").await;
    assert!(board.sent().is_empty());
    let board2 = Board::new(true);
    let (d2, _dir2) = daemon(&board2);
    d2.cancel_active_task("x").await;
    assert!(board2.sent().is_empty());
}

// ---- test_daemon_explore.py: IPC (eve-bridge explore [start|stop|status]) -----------------------------------

async fn ipc(d: &Arc<Daemon>, req: Value) -> Obj {
    d.handle_ipc(obj(req)).await.expect("the IPC handler answers")
}

#[tokio::test]
#[ignore = "needs units board, ipc_hooks"]
async fn test_ipc_start_puts_the_board_in_explore_mode_without_counting_as_activity() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.st.lock().last_activity_at = 123.0;
    let resp = ipc(&d, json!({"evt": "explore", "action": "start"})).await;
    assert!(resp["ok"].as_bool().unwrap() && resp["connected"].as_bool().unwrap());
    assert_eq!(resp["explore"]["state"], "exploring");
    assert_eq!(resp["explore"]["manual"], true);
    assert_eq!(resp["explore"]["reason"], "requested by cli");
    assert_eq!(board.sent(), vec![MODE_ON(), first_look()]);
    assert_eq!(d.st.lock().last_activity_at, 123.0); // the request itself is not "the human is back"
}

#[tokio::test]
#[ignore = "needs units board, ipc_hooks"]
async fn test_ipc_stop_ends_it_and_status_reports() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    let st = ipc(&d, json!({"evt": "explore", "action": "status"})).await;
    assert_eq!(Value::Object(st.clone()), json!({"ok": true, "connected": true, "explore": d.explorer.lock().status()}));
    assert_eq!(st["explore"]["state"], "off");
    ipc(&d, json!({"evt": "explore", "action": "start"})).await;
    let st = ipc(&d, json!({"evt": "explore", "action": "status"})).await;
    let (yaw, pitch) = eve_explore::WAYPOINTS[0];
    assert_eq!(st["explore"]["state"], "exploring");
    assert_eq!(st["explore"]["waypoint"], json!([yaw, pitch]));
    let resp = ipc(&d, json!({"evt": "explore", "action": "stop"})).await;
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["explore"]["state"], "off");
    assert_eq!(resp["explore"]["manual"], false);
    assert_eq!(board.sent().last(), Some(&MODE_OFF()));
    assert_eq!(board.sent().iter().filter(|m| **m == MODE_OFF()).count(), 1);
    // stop when nothing is running is a harmless no-op on the wire
    let n = board.sent().len();
    let resp = ipc(&d, json!({"evt": "explore", "action": "stop"})).await;
    assert_eq!(resp["ok"], true);
    assert_eq!(board.sent().len(), n);
}

#[tokio::test]
#[ignore = "needs units board, ipc_hooks"]
async fn test_ipc_start_is_refused_while_disconnected_or_dictating() {
    for (connected, listen_sent, why) in [(false, None, "board disconnected"), (true, Some(true), "listen key")] {
        let board = Board::new(connected);
        let (d, _dir) = daemon(&board);
        d.st.lock().listen_sent = listen_sent;
        let resp = ipc(&d, json!({"evt": "explore", "action": "start"})).await;
        assert_eq!(resp["ok"], false, "{why}");
        assert_eq!(resp["error"], why);
        assert_eq!(resp["explore"]["state"], "off");
        assert!(board.sent().is_empty());
    }
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    let resp = ipc(&d, json!({"evt": "explore", "action": "dance"})).await;
    assert_eq!(resp["ok"], false);
    assert!(resp["error"].as_str().unwrap().contains("unknown explore action"));
}

#[tokio::test]
#[ignore = "needs units board, ipc_hooks"]
async fn test_ipc_start_works_when_the_idle_start_is_disabled() {
    let board = Board::new(true);
    let (d, _dir) = daemon_with(&board, false, |_| {});
    d.explore_step(5000.0).await; // never starts on its own
    assert!(board.sent().is_empty());
    let resp = ipc(&d, json!({"evt": "explore"})).await; // action defaults to start
    assert_eq!(resp["ok"], true);
    assert_eq!(board.sent()[0], MODE_ON());
}

// ---- the explorer driven directly (what the IPC route calls) ------------------------------------------------

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_request_explore_starts_a_pan_and_a_busy_tick_keeps_it_going() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.st.lock().last_activity_at = 123.0;
    d.request_explore("requested by cli").await.expect("not refused");
    assert_eq!(board.sent(), vec![MODE_ON(), first_look()]);
    assert_eq!(d.st.lock().last_activity_at, 123.0); // the request itself is not "the human is back"
    // a tick with zero idle time (a session is running) keeps exploring
    {
        let mut state = d.state.lock();
        state.session_start("s1", None, None);
        state.turn_begin("s1");
        assert!(state.running_count() > 0);
    }
    d.explore_step(7.0).await;
    assert!(d.explorer.lock().active());
    assert_eq!(board.sent().last().unwrap()["cmd"], "look");
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_request_explore_is_refused_while_disconnected_or_dictating() {
    for (connected, listen_sent, why) in [(false, None, "board disconnected"), (true, Some(true), "listen key")] {
        let board = Board::new(connected);
        let (d, _dir) = daemon(&board);
        d.st.lock().listen_sent = listen_sent;
        let refused = d.request_explore("requested by cli").await.expect_err("refused");
        assert_eq!(refused.0, why);
        assert_eq!(d.explorer.lock().status()["state"], "off");
        assert!(board.sent().is_empty());
    }
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_the_idle_start_disabled_never_starts_on_its_own_and_dismiss_is_quiet_when_off() {
    let board = Board::new(true);
    let (d, _dir) = daemon_with(&board, false, |_| {});
    d.explore_step(5000.0).await;
    assert!(board.sent().is_empty());
    d.dismiss_explore("explore stop").await; // nothing on the board: nothing on the wire
    assert!(board.sent().is_empty());
    d.request_explore("requested by cli").await.unwrap();
    d.dismiss_explore("explore stop").await;
    assert_eq!(board.sent().last(), Some(&MODE_OFF()));
    assert!(!d.explorer.lock().manual);
}

// ---- the board and the mic --------------------------------------------------------------------------------------

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_a_touch_ends_a_manual_explore_at_once() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.request_explore("requested by cli").await.unwrap();
    let _ = d.handle_ble(obj(json!({"cmd": "key", "name": "enter"}))).await;
    assert_eq!(d.explorer.lock().state, eve_explore::Explorer::OFF);
    assert!(!d.explorer.lock().manual);
    assert!(board.sent().contains(&MODE_OFF()));
    // the idle clock restarted: the next tick does not resume
    d.explore_step(1.0).await;
    assert_eq!(d.explorer.lock().state, eve_explore::Explorer::OFF);
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_a_touch_during_a_conversation_ends_the_explore_but_not_the_conversation() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    let conv = talking();
    d.st.lock().conversation = Some(conv.clone());
    d.request_explore("requested by cli").await.unwrap();
    let _ = d.handle_ble(obj(json!({"cmd": "key", "name": "enter"}))).await;
    assert!(!conv.cancelled());
    assert_eq!(d.explorer.lock().state, eve_explore::Explorer::OFF);
    assert!(board.sent().contains(&MODE_OFF()));
    conv.cancel();
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_the_wake_word_ends_a_manual_explore_before_the_conversation() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.st.lock().ears = None; // converse returns at once without ears
    d.request_explore("requested by cli").await.unwrap();
    d.on_wake("hey eve");
    settle().await; // let the dismiss task run
    assert_eq!(d.explorer.lock().state, eve_explore::Explorer::OFF);
    assert!(!d.explorer.lock().manual);
    assert_eq!(board.sent().last(), Some(&MODE_OFF()));
    let conversation = d.st.lock().conversation.clone().expect("a conversation task");
    assert_eq!(conversation.name(), "voice-conversation");
    conversation.join().await;
}

/// The open_session seam: records each conversation's options, says `listening`, asks to explore when told to,
/// and either closes, hangs until hushed, or fails.
type Opened = Arc<Mutex<Vec<(bool, Option<String>)>>>;

fn open_session_seam(explore: bool, hang: bool) -> (eve_daemon::seams::OpenSession, Opened) {
    let opened: Opened = Arc::new(Mutex::new(Vec::new()));
    let seen = opened.clone();
    let seam: eve_daemon::seams::OpenSession = Arc::new(move |_mic, on_state, _factory, _on_open, options| {
        let seen = seen.clone();
        Box::pin(async move {
            seen.lock().push((options.lesson_wake, options.think_aloud.as_ref().map(id_of)));
            on_state("listening");
            if explore {
                if let Some(on_explore) = &options.on_explore {
                    on_explore().await?;
                }
            }
            if hang {
                futures::future::pending::<()>().await;
            }
            Ok(())
        })
    });
    (seam, opened)
}

#[tokio::test]
#[ignore = "needs units board, run_memory, agents, door"]
async fn test_the_lesson_word_opens_a_conversation_flagged_as_a_lesson_wake() {
    let board = Board::new(true);
    let (seam, opened) = open_session_seam(false, false);
    let (d, _dir) = daemon_with(&board, true, |p| {
        p.seams.open_session = seam;
        p.ears_cfg = eve_ears::configured_from(|_| None);
    });
    d.st.lock().ears = Some(Arc::new(FakeMic::default()));
    d.on_wake("lesson");
    let conversation = d.st.lock().conversation.clone().unwrap();
    conversation.join().await;
    d.on_wake("hey_eve");
    let conversation = d.st.lock().conversation.clone().unwrap();
    conversation.join().await;
    let flags: Vec<bool> = opened.lock().iter().map(|(wake, _)| *wake).collect();
    assert_eq!(flags, vec![true, false]);
}

#[tokio::test]
#[ignore = "needs units board, run_memory, agents, door"]
async fn test_voice_go_explore_starts_once_the_conversation_has_closed() {
    let board = Board::new(true);
    let (seam, _opened) = open_session_seam(true, false);
    let (d, _dir) = daemon_with(&board, true, |p| p.seams.open_session = seam);
    let mic = Arc::new(FakeMic::default());
    d.st.lock().ears = Some(mic.clone());
    d.converse(None, false).await;
    settle().await; // the agent-state send is a task of its own
    assert!(mic.unsubscribed.load(Ordering::SeqCst));
    let explorer = d.explorer.lock().clone();
    assert!(explorer.active() && explorer.manual);
    assert_eq!(explorer.started_reason, "requested by voice");
    assert!(d.st.lock().explore_after_conversation.is_none());
    let sent = board.sent();
    assert!(sent.contains(&MODE_ON()));
    assert!(sent.contains(&first_look()));
    assert!(sent.contains(&json!({"cmd": "agent", "state": "idle"})));
    assert!(index_of(&sent, &MODE_ON()) > index_of(&sent, &json!({"cmd": "agent", "state": "listening"})));
}

#[tokio::test]
#[ignore = "needs units board, run_memory, agents, door"]
async fn test_voice_go_explore_is_dropped_when_the_conversation_is_hushed() {
    let board = Board::new(true);
    let (seam, opened) = open_session_seam(true, true);
    let (d, _dir) = daemon_with(&board, true, |p| p.seams.open_session = seam);
    d.st.lock().ears = Some(Arc::new(FakeMic::default()));
    let me = d.clone();
    let conversation = Job::spawn("voice-conversation", async move { me.converse(None, false).await });
    while opened.lock().is_empty() {
        tokio::task::yield_now().await;
    }
    conversation.cancel(); // the hush
    conversation.join().await;
    settle().await;
    assert_eq!(d.explorer.lock().state, eve_explore::Explorer::OFF);
    assert!(d.st.lock().explore_after_conversation.is_none());
    assert!(!board.sent().contains(&MODE_ON()));
}

// ---- photos: {"cmd":"snap"} and the one frame line that answers it ------------------------------------------

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_take_snapshot_sends_snap_and_returns_the_answering_frame() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    let (b, dd) = (board.clone(), d.clone());
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(b.sent().last(), Some(&json!({"cmd": "snap"})));
        let _ = dd.handle_ble(snap_line(true, 320, 240)).await;
    });
    let frame = d.take_snapshot().await.expect("the answering frame");
    assert!(frame.snap && (frame.w, frame.h) == (320, 240));
    assert_eq!(frame.data, JPEG);
    assert_eq!(frame.yaw, Some(20.0));
    assert!(d.st.lock().snap_waiter.is_none());
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_take_snapshot_times_out_on_old_firmware_and_leaves_no_waiter() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    let frame = d.take_snapshot().await; // SNAP_TIMEOUT_SECS: the board never answers
    assert!(frame.is_none());
    assert_eq!(board.sent(), vec![json!({"cmd": "snap"})]);
    assert!(d.st.lock().snap_waiter.is_none());
    // a late answer after the timeout is dropped, not an error
    d.handle_ble(snap_line(true, 320, 240)).await.expect("a late snap is dropped");
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_take_snapshot_refuses_while_disconnected_or_busy() {
    let away = Board::new(false);
    let (d0, _dir0) = daemon(&away);
    assert!(d0.take_snapshot().await.is_none());
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    let me = d.clone();
    let first = tokio::spawn(async move { me.take_snapshot().await });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert!(d.take_snapshot().await.is_none()); // one at a time
    let _ = d.handle_ble(snap_line(true, 320, 240)).await;
    assert!(first.await.unwrap().is_some());
    assert_eq!(board.sent().iter().filter(|m| **m == json!({"cmd": "snap"})).count(), 1);
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_stream_frames_still_go_to_the_tracker_and_bad_snaps_are_none() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    // a stream frame is not a snap: no waiter takes it
    let _ = d.handle_ble(snap_line(false, 160, 120)).await;
    assert!(d.st.lock().snap_waiter.is_none());
    let dd = d.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut bad = snap_line(true, 320, 240);
        bad["frame"]["b64"] = json!("not base64!");
        let _ = dd.handle_ble(bad).await;
    });
    assert!(d.take_snapshot().await.is_none());
    assert!(d.st.lock().snap_waiter.is_none());
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_every_other_board_touch_still_ends_it() {
    // focus / key: either one is the human, so either ends the explore.
    for cmd in [json!({"cmd": "focus"}), json!({"cmd": "key", "name": "enter"})] {
        let board = Board::new(true);
        let (d, _dir) = daemon(&board);
        d.request_explore("requested by cli").await.unwrap();
        // The rest of the handler may need state this daemon does not carry; the explore must already have been
        // dismissed by then.
        let _ = d.handle_ble(obj(cmd.clone())).await;
        assert_eq!(d.explorer.lock().state, eve_explore::Explorer::OFF, "{cmd}");
        assert!(board.sent().contains(&MODE_OFF()), "{cmd}");
    }
}

// ---- thoughts on the robot's own screen while it explores -------------------------------------------------

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_a_thought_goes_onto_the_screen_while_exploring() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.request_explore("requested by cli").await.unwrap();
    d.show_thought(&thought("Someone brought a plant to my desk.", &["plant"], false));
    settle().await;
    let pages = captions(&board);
    assert!(!pages.is_empty(), "nothing was drawn");
    let first = &pages[0];
    assert_eq!(first["page"], 0);
    let lines = first["lines"].as_array().unwrap();
    assert!(!lines.is_empty() && lines.len() <= 4);
    assert!(lines.iter().all(|l| l.as_str().unwrap().chars().count() <= 17));
    let text: Vec<String> = pages
        .iter()
        .flat_map(|p| p["lines"].as_array().cloned().unwrap_or_default())
        .map(|l| l.as_str().unwrap().to_string())
        .collect();
    assert!(text.join(" ").contains("plant"));
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_a_photographed_thought_says_so_on_the_screen() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.request_explore("requested by cli").await.unwrap();
    d.show_thought(&thought("A plant arrived.", &["plant"], true));
    settle().await;
    let text: Vec<String> = captions(&board)
        .iter()
        .flat_map(|p| p["lines"].as_array().cloned().unwrap_or_default())
        .map(|l| l.as_str().unwrap().to_string())
        .collect();
    assert!(text.join(" ").contains("photo"));
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_nothing_is_drawn_when_the_screen_is_not_eves_to_use() {
    // not exploring at all
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.show_thought(&thought("A thought nobody asked for.", &[], false));
    settle().await;
    assert!(captions(&board).is_empty());

    // a conversation owns the screen
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.request_explore("requested by cli").await.unwrap();
    let conv = talking();
    d.st.lock().conversation = Some(conv.clone());
    d.show_thought(&thought("A thought.", &[], false));
    settle().await;
    assert!(captions(&board).is_empty());
    conv.cancel();

    // the owner is dictating
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.request_explore("requested by cli").await.unwrap();
    d.st.lock().listen_down = true;
    d.show_thought(&thought("A thought.", &[], false));
    settle().await;
    assert!(captions(&board).is_empty());

    // the board is away
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.request_explore("requested by cli").await.unwrap();
    board.set_connected(false);
    d.show_thought(&thought("A thought.", &[], false));
    settle().await;
    assert!(captions(&board).is_empty());
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_the_screen_is_cleared_when_the_explore_ends() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.request_explore("requested by cli").await.unwrap();
    d.show_thought(&thought("Half-read when the human walks in.", &[], false));
    settle().await;
    assert!(!captions(&board).is_empty());
    let _ = d.handle_ble(obj(json!({"cmd": "key", "name": "enter"}))).await; // a touch ends it
    settle().await; // on_caption sends on a task
    let caps = captions(&board);
    assert!(caps.contains(&json!({"cmd": "caption", "clear": true})));
    assert_eq!(caps.last(), Some(&json!({"cmd": "caption", "clear": true})));
}

// ---- being called back by hand -------------------------------------------------------------------------

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_a_double_tap_on_the_screen_calls_eve_back() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.st.lock().last_activity_at = 0.0;
    d.request_explore("requested by cli").await.unwrap();
    let _ = d.handle_ble(obj(json!({"cmd": "explore", "stop": true}))).await;
    assert_eq!(d.explorer.lock().state, eve_explore::Explorer::OFF);
    assert!(!d.explorer.lock().manual);
    assert_eq!(board.sent().last(), Some(&MODE_OFF()));
    assert!(d.st.lock().last_activity_at > 0.0); // the human is here
    // ... and it does not resume on the next tick
    d.explore_step(1.0).await;
    assert_eq!(d.explorer.lock().state, eve_explore::Explorer::OFF);
}

// ---- test_daemon_lesson.py: the IPC handler (`_handle_lesson`) ---------------------------------------------

async fn lesson(d: &Arc<Daemon>, req: Value) -> Obj {
    d.handle_lesson(obj(req)).await
}

fn with_learning(board: &Arc<Board>, server: Option<Arc<dyn eve_daemon::seams::Learning>>) -> (Arc<Daemon>, tempfile::TempDir) {
    let (d, dir) = daemon(board);
    d.st.lock().learning_server = server;
    (d, dir)
}

fn cancel_idle(d: &Daemon) {
    if let Some(h) = d.st.lock().learning_idle.take() {
        h.abort();
    }
}

#[tokio::test]
#[ignore = "needs units board, run_memory"]
async fn test_step_thinks_then_shows_one_caption_that_fits_the_screen() {
    let board = Board::new(true);
    let app = answers("Subtract three from both sides to get the x part alone.");
    let (d, _dir) = with_learning(&board, Some(app.clone()));
    let resp = lesson(&d, json!({"evt": "lesson", "action": "step"})).await;
    settle().await;
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["action"], "step");
    assert!(resp["answer"].as_str().is_some_and(|a| !a.is_empty()));
    assert_eq!(*app.calls.lock(), vec![obj(json!({"action": "step"}))]);
    let sent = board.sent();
    assert!(sent.contains(&THINKING()));
    let caps = shown_captions(&board);
    assert_eq!(caps.len(), 1);
    assert!(index_of(&sent, &THINKING()) < index_of(&sent, &caps[0]));
    let lines = caps[0]["lines"].as_array().unwrap();
    assert!((1..=4).contains(&lines.len()) && lines.iter().all(|l| l.as_str().unwrap().chars().count() <= 17));
    assert_eq!(states(&board).last().map(String::as_str), Some("speaking"));
    serde_json::to_string(&resp).unwrap();
    cancel_idle(&d);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs units board, run_memory"]
async fn test_a_dispatch_notice_is_the_only_caption() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    let rt = tokio::runtime::Handle::current();
    let weak = Arc::downgrade(&d);
    let app = learning(move |_| {
        // the learning server's notify, handed to the loop from the worker thread
        let weak = weak.clone();
        rt.spawn(async move {
            if let Some(d) = weak.upgrade() {
                d.learning_notice("Well done, that is the answer.", "complete");
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok(obj(json!({"ok": true, "answer": "Well done, that is the answer."})))
    });
    d.st.lock().learning_server = Some(app);
    let resp = lesson(&d, json!({"evt": "lesson", "action": "check"})).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(resp["ok"], true);
    assert_eq!(shown_captions(&board).len(), 1);
    assert!(states(&board).contains(&"done".to_string()));
    cancel_idle(&d);
}

#[tokio::test]
#[ignore = "needs units board, run_memory"]
async fn test_open_speaks_without_thinking() {
    let board = Board::new(true);
    let app = answers("Would you like to learn a topic?");
    let (d, _dir) = with_learning(&board, Some(app.clone()));
    let resp = lesson(&d, json!({"evt": "lesson", "action": "open"})).await;
    settle().await;
    assert_eq!(resp["ok"], true);
    assert_eq!(*app.calls.lock(), vec![obj(json!({"action": "open"}))]);
    assert_eq!(shown_captions(&board).len(), 1);
    assert!(!states(&board).contains(&"thinking".to_string()));
    assert_eq!(states(&board).last().map(String::as_str), Some("speaking"));
    cancel_idle(&d);
}

#[tokio::test]
#[ignore = "needs units board, run_memory"]
async fn test_start_options_are_forwarded_and_unknown_keys_dropped() {
    let board = Board::new(true);
    let app = answers("What is 3/4 + 1/4?");
    let (d, _dir) = with_learning(&board, Some(app.clone()));
    let resp = lesson(
        &d,
        json!({"evt": "lesson", "action": "start", "mode": "learn", "topic": "Fractions", "level": "Grade 4",
               "extra": "x"}),
    )
    .await;
    settle().await;
    assert_eq!(resp["ok"], true);
    assert_eq!(
        *app.calls.lock(),
        vec![obj(json!({"action": "start", "mode": "learn", "topic": "Fractions", "level": "Grade 4"}))]
    );
    cancel_idle(&d);
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_unknown_action_and_bad_mode_are_refused_before_anything_happens() {
    let board = Board::new(true);
    let app = answers("x");
    let (d, _dir) = with_learning(&board, Some(app.clone()));
    let resp = lesson(&d, json!({"evt": "lesson", "action": "dance"})).await;
    assert_eq!(resp["ok"], false);
    assert!(resp["error"].as_str().unwrap().contains("Unknown lesson action"));
    let resp = lesson(&d, json!({"evt": "lesson", "action": "start", "mode": "play"})).await;
    assert_eq!(resp["ok"], false);
    assert!(resp["error"].as_str().unwrap().contains("learn or help"));
    settle().await;
    assert!(app.calls.lock().is_empty());
    assert!(board.sent().is_empty());
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_learning_disabled_is_a_clear_error_without_a_caption() {
    let board = Board::new(true);
    let (d, _dir) = with_learning(&board, None);
    let resp = lesson(&d, json!({"evt": "lesson", "action": "hint"})).await;
    settle().await;
    assert_eq!(resp["ok"], false);
    assert!(resp["error"].as_str().unwrap().to_lowercase().contains("unavailable"));
    assert_eq!(resp["reason"], resp["error"]);
    assert!(shown_captions(&board).is_empty());
    assert!(!states(&board).contains(&"thinking".to_string()));
    serde_json::to_string(&resp).unwrap();
}

#[tokio::test]
#[ignore = "needs units board, run_memory"]
async fn test_a_refusal_shows_the_error_state_with_the_reason() {
    let board = Board::new(true);
    let refuse = learning(|_| Err(eve_learning::LearningError::value("Open a lesson first.")));
    let (d, _dir) = with_learning(&board, Some(refuse));
    let resp = lesson(&d, json!({"evt": "lesson", "action": "hint"})).await;
    settle().await;
    assert_eq!(resp["ok"], false);
    assert_eq!(resp["error"], "Open a lesson first.");
    assert_eq!(states(&board).last().map(String::as_str), Some("error"));
    let caps = shown_captions(&board);
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0]["chirp"], false);
    cancel_idle(&d);

    let board = Board::new(true);
    let stale = learning(|_| Err(eve_learning::LearningError::NotFound("gone".into())));
    let (d, _dir) = with_learning(&board, Some(stale));
    let resp = lesson(&d, json!({"evt": "lesson", "action": "status"})).await;
    assert_eq!(resp["ok"], false);
    assert!(resp["error"].as_str().unwrap().contains("no longer there"));
    cancel_idle(&d);
}

#[tokio::test]
#[ignore = "needs units board, run_memory"]
async fn test_an_unexpected_error_ends_thinking_with_an_error_caption() {
    let board = Board::new(true);
    let broken = learning(|_| Err(eve_learning::LearningError::Other(anyhow::anyhow!("disk full near 2x+3=11"))));
    let (d, _dir) = with_learning(&board, Some(broken));
    let resp = lesson(&d, json!({"evt": "lesson", "action": "check", "text": "2x+3=11"})).await;
    settle().await;
    assert_eq!(resp["ok"], false);
    assert_eq!(resp["action"], "check");
    assert_eq!(resp["error"], resp["reason"]);
    let sent = board.sent();
    assert!(sent.contains(&THINKING()));
    assert_eq!(states(&board).last().map(String::as_str), Some("error"));
    let caps = shown_captions(&board);
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0]["chirp"], false);
    assert!(index_of(&sent, &THINKING()) < index_of(&sent, &caps[0]));
    assert!(d.st.lock().learning_idle.is_some());
    serde_json::to_string(&resp).unwrap();
    cancel_idle(&d);
}

#[tokio::test]
#[ignore = "needs units board, run_memory"]
async fn test_during_a_conversation_live_keeps_the_screen() {
    let board = Board::new(true);
    let app = answers("x");
    let (d, _dir) = with_learning(&board, Some(app.clone()));
    let conv = talking();
    d.st.lock().conversation = Some(conv.clone());
    let resp = lesson(&d, json!({"evt": "lesson", "action": "step"})).await;
    settle().await;
    assert_eq!(resp["ok"], true);
    assert_eq!(*app.calls.lock(), vec![obj(json!({"action": "step"}))]);
    assert!(!states(&board).contains(&"thinking".to_string()));
    assert!(shown_captions(&board).is_empty());
    assert!(d.st.lock().learning_idle.is_none());
    conv.cancel();
}

#[tokio::test]
#[ignore = "needs units board, run_memory"]
async fn test_a_lesson_ends_a_manual_explore() {
    let board = Board::new(true);
    let (d, _dir) = with_learning(&board, Some(answers("x")));
    d.request_explore("requested by cli").await.unwrap();
    assert_eq!(d.explorer.lock().state, eve_explore::Explorer::EXPLORING);
    let resp = lesson(&d, json!({"evt": "lesson", "action": "step"})).await;
    settle().await;
    assert_eq!(resp["ok"], true);
    assert!(board.sent().contains(&MODE_OFF()));
    assert_eq!(d.explorer.lock().state, eve_explore::Explorer::OFF);
    cancel_idle(&d);
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_status_neither_ends_the_explore_nor_captions() {
    let board = Board::new(true);
    let status = learning(|_| {
        Ok(obj(json!({"ok": true, "lesson": {"topic": "Fractions", "mode": "learn", "stage": "practice"},
                      "answer": "Try the next one."})))
    });
    let (d, _dir) = with_learning(&board, Some(status));
    d.request_explore("requested by cli").await.unwrap();
    let n = board.sent().len();
    let resp = lesson(&d, json!({"evt": "lesson", "action": "status"})).await;
    settle().await;
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["lesson"]["topic"], "Fractions");
    assert_eq!(d.explorer.lock().state, eve_explore::Explorer::EXPLORING);
    assert!(!board.sent()[n..].contains(&MODE_OFF()));
    assert!(shown_captions(&board).is_empty());
}

#[tokio::test]
#[ignore = "needs units board, run_memory"]
async fn test_back_to_back_notices_keep_only_the_latest_idle_timer() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.learning_notice("First step.", "");
    let first = d.st.lock().learning_idle.clone().expect("a timer");
    d.learning_notice("Second step.", "");
    let second = d.st.lock().learning_idle.clone().expect("a timer");
    assert_ne!(first.id(), second.id());
    settle().await;
    assert!(first.is_finished() && !second.is_finished()); // the first was cancelled
    assert_eq!(d.st.lock().learning_notices, 2);
    second.abort();
}

#[tokio::test]
#[ignore = "needs units board, run_memory"]
async fn test_a_long_word_still_fits_the_screen() {
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.learning_notice(&"  supercalifragilisticexpialidocious\n and   more words here ".repeat(3), "");
    settle().await;
    let caps = shown_captions(&board);
    let lines = caps[0]["lines"].as_array().unwrap();
    assert!((1..=4).contains(&lines.len()) && lines.iter().all(|l| l.as_str().unwrap().chars().count() <= 17));
    cancel_idle(&d);
    let board = Board::new(true);
    let (d, _dir) = daemon(&board);
    d.learning_notice("", "");
    settle().await;
    assert_eq!(shown_captions(&board)[0]["lines"], json!(["..."]));
    cancel_idle(&d);
}

// ---- think out loud on the daemon ----------------------------------------------------------------------------

/// `_listening_daemon`: a daemon with a microphone and OPENAI_API_KEY (only its presence is checked; nothing
/// connects), lent the real demo LearningApp whose listener is the daemon's own, and a lesson generated in it.
fn listening_daemon(board: &Arc<Board>, ears: bool) -> (Arc<Daemon>, Arc<AppLearning>, Obj, tempfile::TempDir) {
    listening_daemon_with(board, ears, |_| {})
}

fn listening_daemon_with(
    board: &Arc<Board>,
    ears: bool,
    adjust: impl FnOnce(&mut DaemonParts),
) -> (Arc<Daemon>, Arc<AppLearning>, Obj, tempfile::TempDir) {
    let (d, dir) = daemon_with(board, true, |p| {
        if let Some(env) = p.environ.as_mut() {
            env.insert("OPENAI_API_KEY".into(), "test-placeholder".into());
        }
        adjust(p);
    });
    let listener = d.make_listener(&tokio::runtime::Handle::current());
    let app = eve_learning::LearningApp::new(eve_learning::StartOptions {
        directory: Some(dir.path().join("learning")),
        demo: true,
        listener: Some(listener),
        ..Default::default()
    })
    .expect("a demo learning app");
    let s = app
        .dispatch(&obj(json!({"action": "create", "mode": "learn", "topic": "Algebra", "level": "Grade 5"})))
        .expect("created");
    let generated = app.dispatch(&obj(json!({"action": "generate", "id": s["id"]}))).expect("generated");
    let app = Arc::new(AppLearning(app));
    {
        let mut st = d.st.lock();
        st.learning_server = Some(app.clone());
        if ears {
            st.ears = Some(Arc::new(FakeMic::default()));
        }
    }
    (d, app, generated, dir)
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_a_wake_word_conversation_switches_instead_of_opening_another() {
    let board = Board::new(true);
    let (d, _app, lesson, _dir) = listening_daemon(&board, true);
    let conv = talking();
    let session = Arc::new(FakeSession::default());
    {
        let mut st = d.st.lock();
        st.conversation = Some(conv.clone());
        st.voice_session = Some(session.clone());
    }
    let out = d.think_aloud_start(lesson.clone()).await;
    assert_eq!(out["ok"], true);
    assert_eq!(*session.entered.lock(), vec![id_of(&lesson)]);
    assert!(d.st.lock().conversation.as_ref().is_some_and(|c| *c == conv)); // no second conversation
    conv.cancel();
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_listen_while_connecting_is_handed_to_the_session_when_it_opens() {
    let board = Board::new(true);
    let (d, _app, lesson, _dir) = listening_daemon(&board, true);
    let conv = talking(); // the wake word opened one; it is still connecting
    d.st.lock().conversation = Some(conv.clone());
    let out = d.think_aloud_start(lesson.clone()).await;
    assert_eq!(out["ok"], true);
    assert_eq!(d.st.lock().think_aloud_state, "starting");
    assert!(d.st.lock().conversation.as_ref().is_some_and(|c| *c == conv));
    let session = Arc::new(FakeSession::default());
    d.on_voice_session(session.clone());
    assert_eq!(*session.prepared.lock(), vec![id_of(&lesson)]);
    assert!(d.st.lock().think_aloud_wish.is_none());
    conv.cancel();
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_an_open_session_that_refuses_is_answered_not_queued() {
    let board = Board::new(true);
    let (d, _app, lesson, _dir) = listening_daemon(&board, true);
    let conv = talking();
    let session = Arc::new(FakeSession {
        refuse: Some("eve is busy with a computer task. Stop it first, then think out loud.".into()),
        ..Default::default()
    });
    {
        let mut st = d.st.lock();
        st.conversation = Some(conv.clone());
        st.voice_session = Some(session);
    }
    let out = d.think_aloud_start(lesson).await;
    assert_eq!(out["ok"], false);
    assert!(out["reason"].as_str().unwrap().contains("computer task"));
    let st = d.st.lock();
    assert!(st.think_aloud_wish.is_none() && st.think_aloud_state == "off");
    assert!(st.conversation.as_ref().is_some_and(|c| *c == conv));
    drop(st);
    conv.cancel();
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_listen_without_a_microphone_says_why() {
    let board = Board::new(true);
    let (d, app, _lesson, _dir) = listening_daemon(&board, false);
    let a = app.clone();
    let out = tokio::task::spawn_blocking(move || a.0.listen(None)).await.unwrap();
    assert_eq!(out["ok"], false);
    assert!(out["reason"].as_str().unwrap().contains("microphone is off"));
    assert!(d.st.lock().conversation.is_none());
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_the_listening_pose_drops_while_eve_thinks_or_speaks() {
    let board = Board::new(true);
    let (d, _app, lesson, _dir) = listening_daemon(&board, true);
    d.on_think_aloud(true, Some(&id_of(&lesson)));
    d.on_agent_state("thinking");
    d.on_agent_state("speaking");
    d.on_agent_state("listening");
    settle().await;
    let listens: Vec<Value> =
        board.sent().into_iter().filter(|m| m["cmd"] == "listen").map(|m| m["on"].clone()).collect();
    assert_eq!(listens, vec![json!(true), json!(false), json!(true)]);
    d.on_think_aloud(false, None);
    settle().await;
    assert_eq!(board.sent().last(), Some(&LISTEN_OFF()));
}

#[tokio::test]
#[ignore = "needs unit board"]
async fn test_a_listener_call_on_the_event_loop_is_refused_rather_than_deadlocking() {
    let board = Board::new(true);
    let (d, _app, _lesson, _dir) = listening_daemon(&board, true);
    let listener = d.make_listener(&tokio::runtime::Handle::current());
    let err = eve_learning::Listener::stop(listener.as_ref()).expect_err("refused on the loop");
    assert!(err.to_string().contains("worker thread"));
}

#[tokio::test]
#[ignore = "needs units board, run_memory, agents, door"]
async fn test_ipc_listen_opens_a_conversation_and_stop_listening_closes_it() {
    let board = Board::new(true);
    let (seam, opened) = open_session_seam(false, true);
    let (d, dir) = daemon_with(&board, true, |p| {
        p.seams.open_session = seam;
        if let Some(env) = p.environ.as_mut() {
            env.insert("OPENAI_API_KEY".into(), "test-placeholder".into());
        }
    });
    let listener = d.make_listener(&tokio::runtime::Handle::current());
    let app = eve_learning::LearningApp::new(eve_learning::StartOptions {
        directory: Some(dir.path().join("learning")),
        demo: true,
        listener: Some(listener),
        ..Default::default()
    })
    .unwrap();
    let s = app.dispatch(&obj(json!({"action": "create", "mode": "learn", "topic": "Algebra", "level": "Grade 5"}))).unwrap();
    let generated = app.dispatch(&obj(json!({"action": "generate", "id": s["id"]}))).unwrap();
    let app = Arc::new(AppLearning(app));
    {
        let mut st = d.st.lock();
        st.learning_server = Some(app.clone());
        st.ears = Some(Arc::new(FakeMic::default()));
    }
    let resp = lesson(&d, json!({"evt": "lesson", "action": "listen"})).await;
    settle().await;
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["action"], "listen");
    assert_eq!(resp["state"], "starting");
    while opened.lock().is_empty() {
        tokio::task::yield_now().await;
    }
    assert_eq!(opened.lock()[0].1, Some(id_of(&generated)));
    assert_eq!(d.st.lock().conversation.as_ref().unwrap().name(), "think-aloud-conversation");
    assert!(shown_captions(&board).is_empty() && !states(&board).contains(&"thinking".to_string()));

    let again = lesson(&d, json!({"evt": "lesson", "action": "listen"})).await; // a second press: no second session
    settle().await;
    assert_eq!(again["ok"], true);
    assert_eq!(opened.lock().len(), 1);

    let session = Arc::new(FakeSession::default()); // the session connected and listens
    *session.lesson_id.lock() = Some(id_of(&generated));
    d.on_voice_session(session.clone());
    d.on_think_aloud(true, Some(&id_of(&generated)));
    settle().await;
    assert!(board.sent().contains(&LISTEN_ON()));
    assert_eq!(
        app.0.listen_status(),
        obj(json!({"available": true, "state": "on", "lesson_id": id_of(&generated), "reason": ""}))
    );

    let resp = lesson(&d, json!({"evt": "lesson", "action": "stop-listening"})).await;
    settle().await;
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["state"], "off");
    assert_eq!(*session.stopped.lock(), vec!["stopped".to_string()]);
    assert_eq!(board.sent().last(), Some(&LISTEN_OFF()));
    serde_json::to_string(&resp).unwrap();
    if let Some(c) = d.st.lock().conversation.clone() {
        c.cancel();
    }
    cancel_idle(&d);
}

#[tokio::test]
#[ignore = "needs units board, run_memory"]
async fn test_ipc_listen_with_no_lesson_open_is_refused_with_a_caption() {
    let board = Board::new(true);
    let (d, dir) = daemon_with(&board, true, |p| {
        if let Some(env) = p.environ.as_mut() {
            env.insert("OPENAI_API_KEY".into(), "test-placeholder".into());
        }
    });
    let listener = d.make_listener(&tokio::runtime::Handle::current());
    // no lesson made: none is active
    let app = eve_learning::LearningApp::new(eve_learning::StartOptions {
        directory: Some(dir.path().join("learning")),
        demo: true,
        listener: Some(listener),
        ..Default::default()
    })
    .unwrap();
    {
        let mut st = d.st.lock();
        st.learning_server = Some(Arc::new(AppLearning(app)));
        st.ears = Some(Arc::new(FakeMic::default()));
    }
    let resp = lesson(&d, json!({"evt": "lesson", "action": "listen"})).await;
    settle().await;
    assert_eq!(resp["ok"], false);
    assert!(resp["error"].as_str().unwrap().contains("Open a lesson first"));
    assert!(d.st.lock().conversation.is_none());
    assert_eq!(d.st.lock().think_aloud_state, "off");
    assert_eq!(shown_captions(&board).len(), 1);
    assert_eq!(states(&board).last().map(String::as_str), Some("error"));
    cancel_idle(&d);
}

#[tokio::test]
#[ignore = "needs units board, run_memory, agents, door"]
async fn test_stop_while_a_listening_conversation_is_still_connecting_cancels_it() {
    let board = Board::new(true);
    let (seam, _opened) = open_session_seam(false, true);
    let (d, _app, lesson, _dir) = listening_daemon_with(&board, true, |p| p.seams.open_session = seam);
    d.think_aloud_start(lesson).await;
    tokio::task::yield_now().await;
    let conversation = d.st.lock().conversation.clone().expect("a conversation");
    let out = d.think_aloud_stop().await;
    conversation.join().await;
    assert_eq!(out["ok"], true);
    assert!(conversation.cancelled());
    assert_eq!(d.st.lock().think_aloud_state, "off");
    let out = d.think_aloud_stop().await; // stopping twice is harmless
    assert_eq!(out["ok"], true);
    assert!(out["answer"].as_str().unwrap().contains("not listening"));
}
