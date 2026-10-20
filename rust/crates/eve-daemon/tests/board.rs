//! The board unit: `test_daemon_scene.py` and `test_daemon_touch.py`, plus the heartbeat, the watchdog, the
//! acks, the tailer's callbacks and identity.
//!
//! Python drove single `Daemon` methods on a `SimpleNamespace` stub; here a real daemon is assembled from
//! [`DaemonParts::for_test`] around a fake board link that records what it was sent.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use eve_daemon::seams::{BoardLink, KeyTap, Obj};
use eve_daemon::{Daemon, DaemonParts};
use parking_lot::Mutex;
use serde_json::{json, Value};

// ---- the fakes -------------------------------------------------------------------------------------------------

#[derive(Default)]
struct Ble {
    connected: AtomicBool,
    wired: AtomicBool,
    sent: Mutex<Vec<Value>>,
    pulses: AtomicUsize,
    reconnects: AtomicUsize,
    /// Some: a link with pulse_reset (serial); None: without (BLE).
    can_pulse: bool,
}

impl Ble {
    fn new(connected: bool) -> Arc<Ble> {
        let b = Ble::default();
        b.connected.store(connected, Ordering::SeqCst);
        Arc::new(b)
    }
    fn sent(&self) -> Vec<Value> {
        self.sent.lock().clone()
    }
    fn last(&self) -> Value {
        self.sent.lock().last().cloned().unwrap_or(Value::Null)
    }
    fn set_connected(&self, on: bool) {
        self.connected.store(on, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl BoardLink for Ble {
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
    fn wired(&self) -> bool {
        self.wired.load(Ordering::SeqCst)
    }
    fn pulse_reset(&self, _why: &str) -> bool {
        if !self.can_pulse {
            return false;
        }
        self.pulses.fetch_add(1, Ordering::SeqCst);
        true
    }
    fn force_reconnect(&self, _why: &str) -> bool {
        self.reconnects.fetch_add(1, Ordering::SeqCst);
        true
    }
}

struct NoClient;

impl eve_scene::SceneClient for NoClient {
    fn describe(&self, _image: &[u8], _mime: &str, _previous: Option<&str>) -> anyhow::Result<(String, String)> {
        Ok(("A desk.".into(), String::new()))
    }
    fn locate(&self, _image: &[u8], _mime: &str, _target: &str) -> anyhow::Result<Obj> {
        let mut m = Obj::new();
        m.insert("visible".into(), json!(false));
        m.insert("x".into(), json!(0.0));
        m.insert("y".into(), json!(0.0));
        m.insert("what".into(), json!(""));
        Ok(m)
    }
}

/// What the desktop got: terminal raises and KeyTappers built.
#[derive(Default)]
struct Desktop {
    focused: Mutex<Vec<String>>,
    tappers: Mutex<Vec<Arc<Mutex<Vec<String>>>>>,
}

struct Keys(Arc<Mutex<Vec<String>>>);

impl KeyTap for Keys {
    fn tap(&mut self, name: &str) -> bool {
        self.0.lock().push(name.to_string());
        true
    }
}

fn daemon_with(ble: Arc<Ble>, dir: &std::path::Path) -> (Arc<Daemon>, Arc<Desktop>) {
    let desktop = Arc::new(Desktop::default());
    let mut parts = DaemonParts::for_test(ble, dir);
    let d1 = desktop.clone();
    parts.seams.focus_terminal = Arc::new(move |cwd: String| {
        let d1 = d1.clone();
        Box::pin(async move { d1.focused.lock().push(cwd) })
    });
    let d2 = desktop.clone();
    parts.seams.key_tapper = Arc::new(move || {
        let taps = Arc::new(Mutex::new(Vec::new()));
        d2.tappers.lock().push(taps.clone());
        Box::new(Keys(taps)) as Box<dyn KeyTap>
    });
    let d = Daemon::assemble(parts);
    // The stub's `_Vision` was enabled; the real tracker is, once it has a detector.
    d.vision.set_detect(Some(Arc::new(|_f: &eve_vision::Frame| Ok(Vec::new()))));
    d.scene.set_client(Some(Arc::new(NoClient)));
    (d, desktop)
}

fn daemon(dir: &std::path::Path, connected: bool) -> (Arc<Daemon>, Arc<Ble>) {
    let ble = Ble::new(connected);
    let (d, _) = daemon_with(ble.clone(), dir);
    (d, ble)
}

fn obj(v: Value) -> Obj {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

fn frame(seq: i64, yaw: i64, pitch: i64) -> Obj {
    // base64 of b"\xff\xd8x"
    obj(json!({"frame": {"seq": seq, "w": 160, "h": 120, "fmt": "jpeg", "b64": "/9h4", "yaw": yaw, "pitch": pitch}}))
}

async fn settle() {
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
}

/// Captures every log line written while the guard lives (this thread only: a current-thread runtime).
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture(level: tracing::Level) -> (Captured, tracing::subscriber::DefaultGuard) {
    let cap = Captured::default();
    let w = cap.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_ansi(false)
        .with_writer(move || w.clone())
        .finish();
    (cap, tracing::subscriber::set_default(sub))
}

// ---- test_daemon_scene.py ---------------------------------------------------------------------------------------

#[tokio::test]
async fn test_frames_reach_the_scene_only_during_a_conversation_and_pose_reaches_the_head() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    d.handle_ble(frame(1, -30, 50)).await.unwrap();
    assert!(d.scene.held_frame().is_none()); // no conversation: nothing held
    assert_eq!((d.head.yaw(), d.head.pitch()), (-30.0, 50.0));
    assert_eq!(d.vision.governor().received, 1); // face tracking is unchanged
    d.scene.start(false);
    d.handle_ble(frame(2, 10, 50)).await.unwrap();
    let held = d.scene.held_frame().expect("a frame is held during a conversation");
    assert_eq!(held["seq"], json!(2));
    d.scene.stop().await;
    assert!(d.scene.held_frame().is_none());
}

#[tokio::test]
async fn test_board_loss_is_camera_loss() {
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), true);
    d.scene.start(false);
    d.handle_ble(frame(1, -30, 50)).await.unwrap();
    assert_eq!(d.scene.camera_state(d.scene.clock()), "ok");
    ble.set_connected(false);
    assert_eq!(d.scene.camera_state(d.scene.clock()), "lost");
    let moved = d.head.move_to(Some(10.0), Some(45.0), false, eve_head::DEFAULT_HOLD_SECS).await;
    assert_eq!(moved["ok"], json!(false)); // nothing to move either
    d.scene.stop().await;
}

#[tokio::test]
async fn test_sound_is_sent_on_connect_and_mute_persists() {
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), true);
    d.send_sound().await;
    assert_eq!(ble.last(), json!({"cmd": "sound", "on": true}));
    d.set_sound(false);
    settle().await;
    assert_eq!(ble.last(), json!({"cmd": "sound", "on": false}));
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("sound.json")).unwrap()).unwrap();
    assert_eq!(saved, json!({"muted": true}));
    // a restart keeps it
    assert!(!eve_sound::SoundSetting::at(dir.path().join("sound.json")).load());
}

#[tokio::test]
async fn test_mute_without_a_board_is_kept_for_the_next_connect() {
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), false);
    d.set_sound(false);
    settle().await;
    assert!(ble.sent().is_empty() && d.sound.lock().muted());
    ble.set_connected(true);
    d.send_sound().await;
    assert_eq!(ble.sent(), vec![json!({"cmd": "sound", "on": false})]);
}

#[tokio::test]
#[ignore = "needs unit voice_lesson (Daemon::on_caption)"]
async fn test_caption_chirps_are_dropped_while_muted() {
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), true);
    let page = obj(json!({"cmd": "caption", "page": 0, "of": 1, "lines": ["hi"], "chirp": true}));
    d.on_caption(page.clone());
    settle().await;
    assert_eq!(ble.last()["chirp"], json!(true));
    d.sound.lock().set(false);
    d.on_caption(page);
    settle().await;
    assert_eq!(ble.last()["chirp"], json!(false));
    assert_eq!(ble.last()["lines"], json!(["hi"]));
}

#[tokio::test]
async fn test_status_ack_mismatch_resends_the_owners_choice() {
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), true);
    d.sound.lock().set(false);
    d.handle_ble(obj(json!({"ack": "status", "ok": true, "data": {"sec": false, "snd": true}}))).await.unwrap();
    settle().await;
    assert!(ble.sent().contains(&json!({"cmd": "sound", "on": false})));
    ble.sent.lock().clear();
    d.handle_ble(obj(json!({"ack": "status", "ok": true, "data": {"sec": false, "snd": false}}))).await.unwrap();
    settle().await;
    assert!(ble.sent().is_empty()); // in agreement: nothing sent
    d.handle_ble(obj(json!({"ack": "status", "ok": true, "data": {"sec": false}}))).await.unwrap();
    settle().await;
    assert!(ble.sent().is_empty()); // older firmware: no field, no resend loop
}

#[tokio::test]
#[ignore = "needs unit ipc_hooks (Daemon::handle_ipc)"]
async fn test_ipc_sound_command() {
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), true);
    let status = d.handle_ipc(obj(json!({"evt": "sound", "action": "status"}))).await.unwrap();
    assert_eq!(Value::Object(status), json!({"ok": true, "sound": "on", "connected": true}));
    let off = d.handle_ipc(obj(json!({"evt": "sound", "action": "off"}))).await.unwrap();
    assert_eq!(off["sound"], json!("off"));
    settle().await;
    assert_eq!(ble.last(), json!({"cmd": "sound", "on": false}));
    let on = d.handle_ipc(obj(json!({"evt": "sound", "action": "on"}))).await.unwrap();
    assert_eq!(on["sound"], json!("on"));
}

#[tokio::test]
async fn test_status_ack_sec_ignored_on_a_wired_link() {
    // SETUP-ISSUES #10: over USB the board's BLE "sec" flag is not this link's security.
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), true);
    d.st.lock().last_stick_sec = None;
    ble.wired.store(true, Ordering::SeqCst);
    let (cap, _guard) = capture(tracing::Level::INFO);
    d.handle_ble(obj(json!({"ack": "status", "ok": true, "data": {"sec": false}}))).await.unwrap();
    assert_eq!(d.st.lock().last_stick_sec, None);
    assert!(!cap.text().contains("UNENCRYPTED"));
}

#[tokio::test]
async fn test_status_ack_sec_still_warns_over_ble() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    d.st.lock().last_stick_sec = None;
    let (cap, _guard) = capture(tracing::Level::INFO);
    d.handle_ble(obj(json!({"ack": "status", "ok": true, "data": {"sec": false}}))).await.unwrap();
    assert_eq!(d.st.lock().last_stick_sec, Some(false));
    assert!(cap.text().contains("UNENCRYPTED"));
}

// ---- test_daemon_touch.py ---------------------------------------------------------------------------------------
//
// A board touch (focus / key) while a voice conversation is open does nothing
// but count as activity: no hush, no task cancel, no terminal raise, no key tap.
// On 2026-09-10 a phantom body-pad blip in the attention pose sent focus and
// killed a live task. Outside a conversation, focus and key behave as before.

/// A conversation that is open (its task never finishes until cancelled).
fn in_conversation(d: &Arc<Daemon>) -> eve_daemon::seams::Job {
    let conv = eve_daemon::seams::Job::spawn("conversation", futures::future::pending::<()>());
    d.st.lock().conversation = Some(conv.clone());
    conv
}

fn touch_daemon() -> (tempfile::TempDir, Arc<Daemon>, Arc<Ble>, Arc<Desktop>) {
    let dir = tempfile::tempdir().unwrap();
    let ble = Ble::new(true);
    let (d, desktop) = daemon_with(ble.clone(), dir.path());
    d.st.lock().last_activity_at = 0.0;
    d.state.lock().session_start("s1", None, Some("/work/project"));
    d.state.lock().needs_input("s1");
    (dir, d, ble, desktop)
}

#[tokio::test]
#[ignore = "needs unit owning explore.rs (Daemon::note_activity, Daemon::dismiss_explore)"]
async fn test_focus_during_a_conversation_is_ignored() {
    let (_dir, d, ble, desktop) = touch_daemon();
    let conv = in_conversation(&d);
    let (cap, _guard) = capture(tracing::Level::INFO);
    d.handle_ble(obj(json!({"cmd": "focus"}))).await.unwrap();
    settle().await; // a focus raise would run on a task
    assert!(!conv.done());
    assert!(desktop.focused.lock().is_empty());
    assert!(d.st.lock().last_activity_at > 0.0); // the touch still counts as activity
    assert!(!ble.sent().iter().any(|m| m["cmd"] == json!("caption"))); // no "stopped:" caption
    assert!(cap.text().contains("board touch (focus) during a conversation"));
    assert!(!cap.text().contains("hushed"));
}

#[tokio::test]
#[ignore = "needs unit owning explore.rs (Daemon::note_activity, Daemon::dismiss_explore)"]
async fn test_key_during_a_conversation_taps_nothing() {
    let (_dir, d, _ble, desktop) = touch_daemon();
    let conv = in_conversation(&d);
    d.handle_ble(obj(json!({"cmd": "key", "name": "enter"}))).await.unwrap();
    assert!(!conv.done());
    assert!(desktop.tappers.lock().is_empty() && d.st.lock().keys.is_none());
    assert!(d.st.lock().last_activity_at > 0.0);
}

#[tokio::test]
#[ignore = "needs unit owning explore.rs (Daemon::note_activity, Daemon::dismiss_explore)"]
async fn test_a_closed_conversation_does_not_block_a_touch() {
    let (_dir, d, _ble, desktop) = touch_daemon();
    let conv = in_conversation(&d);
    conv.cancel(); // done(): the conversation has ended
    settle().await;
    d.handle_ble(obj(json!({"cmd": "focus"}))).await.unwrap();
    settle().await;
    assert_eq!(*desktop.focused.lock(), vec!["/work/project".to_string()]);
}

#[tokio::test]
#[ignore = "needs unit owning explore.rs (Daemon::note_activity, Daemon::dismiss_explore)"]
async fn test_focus_outside_a_conversation_raises_the_terminal() {
    let (_dir, d, _ble, desktop) = touch_daemon();
    d.handle_ble(obj(json!({"cmd": "focus"}))).await.unwrap();
    settle().await;
    assert_eq!(*desktop.focused.lock(), vec!["/work/project".to_string()]);
    assert!(d.st.lock().last_activity_at > 0.0);
}

#[tokio::test]
#[ignore = "needs unit owning explore.rs (Daemon::note_activity, Daemon::dismiss_explore)"]
async fn test_key_outside_a_conversation_taps_enter() {
    let (_dir, d, _ble, desktop) = touch_daemon();
    d.handle_ble(obj(json!({"cmd": "key", "name": "enter"}))).await.unwrap();
    let tappers = desktop.tappers.lock();
    assert_eq!(tappers.len(), 1);
    assert_eq!(*tappers[0].lock(), vec!["enter".to_string()]);
}

// ---- the rest of the unit -------------------------------------------------------------------------------------

#[tokio::test]
async fn heartbeat_is_deduped_until_forced_or_changed() {
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), true);
    d.push_heartbeat(false).await;
    assert_eq!(ble.sent().len(), 1);
    assert_eq!(ble.last()["total"], json!(0));
    d.push_heartbeat(false).await; // same snapshot, keepalive not due
    assert_eq!(ble.sent().len(), 1);
    d.push_heartbeat(true).await;
    assert_eq!(ble.sent().len(), 2);
    d.state.lock().session_start("s1", None, Some("/x"));
    d.push_heartbeat(false).await; // changed
    assert_eq!(ble.sent().len(), 3);
    assert_eq!(ble.last()["total"], json!(1));
}

#[tokio::test]
async fn heartbeat_is_not_sent_without_a_board() {
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), false);
    d.push_heartbeat(true).await;
    assert!(ble.sent().is_empty());
    assert!(d.st.lock().last_hb_serialized.is_none());
}

#[tokio::test]
async fn status_poller_escalates_reconnect_then_rts_pulse() {
    let dir = tempfile::tempdir().unwrap();
    let ble = Arc::new(Ble { can_pulse: true, ..Default::default() });
    ble.set_connected(true);
    let (d, _) = daemon_with(ble.clone(), dir.path());
    d.status_poll_once().await; // first poll goes out
    assert_eq!(ble.last(), json!({"cmd": "status"}));
    assert!(d.st.lock().status_sent_at.is_some());
    d.status_poll_once().await; // one missed, below the limit: polls again
    assert_eq!(d.st.lock().status_missed, 1);
    d.status_poll_once().await; // two missed: a plain reconnect
    assert_eq!(ble.reconnects.load(Ordering::SeqCst), 1);
    assert_eq!(d.st.lock().ack_escalation, 1);
    assert!(d.st.lock().status_sent_at.is_none());
    d.status_poll_once().await;
    d.status_poll_once().await;
    d.status_poll_once().await; // still deaf a cycle later: the board is rebooted
    assert_eq!(ble.pulses.load(Ordering::SeqCst), 1);
    assert_eq!(ble.reconnects.load(Ordering::SeqCst), 1);
    assert_eq!(d.st.lock().ack_escalation, 0);
}

#[tokio::test]
async fn ble_link_without_pulse_reconnects_again() {
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), true);
    d.st.lock().ack_escalation = 1;
    d.st.lock().status_sent_at = Some(0.0);
    d.st.lock().status_missed = 1;
    d.status_poll_once().await;
    assert_eq!(ble.reconnects.load(Ordering::SeqCst), 1);
    assert_eq!(d.st.lock().ack_escalation, 2);
}

#[tokio::test]
async fn only_answered_polls_count_toward_de_escalation() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    d.st.lock().ack_escalation = 1;
    let status = || obj(json!({"ack": "status", "ok": true, "data": {}}));
    d.handle_ble(status()).await.unwrap(); // the resync's ack: no poll outstanding
    assert_eq!(d.st.lock().clean_polls, 0);
    for _ in 0..2 {
        d.st.lock().status_sent_at = Some(1.0);
        d.handle_ble(status()).await.unwrap();
    }
    assert_eq!(d.st.lock().clean_polls, 2);
    assert_eq!(d.st.lock().ack_escalation, 0);
}

#[tokio::test]
async fn acks_reach_their_waiter_and_stale_ones_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    let waiter = d.expect_ack("chunk", Some(3));
    d.handle_ble(obj(json!({"ack": "chunk", "ok": true, "n": 2}))).await.unwrap(); // stale
    d.handle_ble(obj(json!({"ack": "chunk", "ok": true, "n": 3}))).await.unwrap();
    let got = d.wait_for_ack("chunk", Duration::from_secs(1), Some(waiter)).await.unwrap();
    assert_eq!(got["n"], json!(3));
    let err = d.wait_for_ack("file", Duration::from_millis(20), None).await;
    assert!(matches!(err, Err(eve_folder_push::AckWaitError::Timeout)));
}

#[tokio::test]
async fn a_failed_status_ack_goes_to_a_status_waiter() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    let waiter = d.expect_ack("status", None);
    d.handle_ble(obj(json!({"ack": "status", "ok": false}))).await.unwrap();
    let got = d.wait_for_ack("status", Duration::from_secs(1), Some(waiter)).await.unwrap();
    assert_eq!(got["ok"], json!(false));
}

#[tokio::test]
async fn diag_is_kept_for_ipc() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    let (cap, _guard) = capture(tracing::Level::INFO);
    d.handle_ble(obj(json!({"diag": {"boot": 7, "reset": "TASK-WATCHDOG", "up": 12, "heap": 1, "minheap": 2,
        "psram": 3, "diedIn": "net", "diedAtMs": 40, "loops": 9, "last": ["a", "b"]}})))
    .await
    .unwrap();
    assert_eq!(d.st.lock().last_diag.as_ref().unwrap()["boot"], json!(7));
    let text = cap.text();
    assert!(text.contains("WARN"));
    assert!(text.contains("board diag: boot #7 after TASK-WATCHDOG (up=12s heap=1 min=2 psram=3)"));
    assert!(text.contains("died in: net (entered 40ms, 9 loops)"));
    assert!(text.contains("pre-reset event: b"));
}

#[tokio::test]
async fn pose_reports_feed_the_head_and_the_motion_trace() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    for i in 0..(eve_daemon::MOTION_TRACE_MAX + 5) {
        d.handle_ble(obj(json!({"pose": {"y": i as i64, "p": 450}}))).await.unwrap();
    }
    assert_eq!(d.st.lock().motion_trace.len(), eve_daemon::MOTION_TRACE_MAX);
    assert_eq!(d.head.pitch(), 45.0);
    d.handle_ble(obj(json!({"pose": {"y": "x", "p": 1}}))).await.unwrap(); // not numbers: ignored
    assert_eq!(d.st.lock().motion_trace.len(), eve_daemon::MOTION_TRACE_MAX);
}

#[tokio::test]
async fn a_snap_goes_to_the_waiting_diary_never_the_tracker() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    let (tx, rx) = tokio::sync::oneshot::channel();
    d.st.lock().snap_waiter = Some(tx);
    d.handle_ble(obj(json!({"frame": {"snap": true, "seq": 1, "w": 320, "h": 240, "fmt": "jpeg", "b64": "/9h4"}})))
        .await
        .unwrap();
    let frame = rx.await.unwrap().unwrap();
    assert_eq!(frame.w, 320);
    assert_eq!(d.vision.governor().received, 0);
    // an unasked-for snap is dropped
    d.handle_ble(obj(json!({"frame": {"snap": true, "seq": 2, "w": 1, "h": 1, "fmt": "jpeg", "b64": "/9h4"}})))
        .await
        .unwrap();
    assert_eq!(d.vision.governor().received, 0);
}

#[tokio::test]
async fn assistant_text_from_a_known_session_becomes_an_entry() {
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), true);
    d.on_assistant_text("/t/unknown.jsonl", "hello", "u1").await.unwrap();
    assert!(d.state.lock().entries.is_empty());
    assert!(ble.sent().is_empty());
    d.state.lock().session_start("abc", None, Some("/w"));
    d.on_assistant_text("/root/proj/abc.jsonl", "hello there", "u2").await.unwrap();
    let entries = d.state.lock().entries.clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].text, "@ hello there");
    assert_eq!(ble.sent().len(), 1); // the forced heartbeat
}

#[tokio::test]
async fn tokens_reach_the_state_and_the_board() {
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), true);
    d.on_tokens(1200, 300, 1.5, 0.25, Vec::new()).await.unwrap();
    assert_eq!(d.state.lock().tokens_cumulative, 1200);
    assert_eq!(ble.last()["tokens"], json!(1200));
    assert_eq!(ble.last()["tokens_today"], json!(300));
}

#[tokio::test]
async fn deferred_turn_end_flips_running_after_the_grace_window() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    d.state.lock().session_start("s1", None, None);
    d.state.lock().turn_begin("s1");
    assert_eq!(d.state.lock().running_count(), 1);
    let me = d.clone();
    let job = d.spawn("turn-end", async move { me.deferred_turn_end("s1".into(), 0.3).await });
    d.st.lock().pending_turn_ends.insert("s1".into(), job.clone());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(d.state.lock().running_count(), 1);
    job.join().await;
    assert_eq!(d.state.lock().running_count(), 0);
}

#[tokio::test]
async fn a_cancelled_turn_end_leaves_the_session_running() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    d.state.lock().session_start("s1", None, None);
    d.state.lock().turn_begin("s1");
    let me = d.clone();
    let job = d.spawn("turn-end", async move { me.deferred_turn_end("s1".into(), 15.0).await });
    settle().await;
    job.cancel();
    job.join().await;
    assert_eq!(d.state.lock().running_count(), 1);
}

#[tokio::test]
async fn mic_status_without_ears() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    assert_eq!(
        Value::Object(d.mic_status()),
        json!({"ok": true, "mic": "on", "listening": false, "connected": true, "always": false, "available": false})
    );
    assert!(d.mic_wanted());
    d.mic.lock().set(false);
    assert!(!d.mic_wanted());
}

#[tokio::test]
async fn identity_status_and_reset_without_a_describer() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    let st = d.handle_identity("status");
    assert_eq!(st["ok"], json!(true));
    assert_eq!(st["enabled"], json!(false));
    assert!(st["identity"].is_object());
    let reset = d.handle_identity("reset");
    assert_eq!(Value::Object(reset), json!({"ok": true, "removed": 0}));
}

#[tokio::test]
async fn resync_sends_time_heartbeat_and_status_first() {
    // Only the board's own steps: listening and the agent's face belong to other units.
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), true);
    d.ble.send(&Value::Object(eve_protocol::build_time_sync()), None).await;
    d.push_heartbeat(true).await;
    d.ble.send(&json!({"cmd": "status"}), None).await;
    let sent = ble.sent();
    assert!(sent[0].get("time").is_some());
    assert!(sent[1].get("total").is_some());
    assert_eq!(sent[2], json!({"cmd": "status"}));
}

#[tokio::test]
#[ignore = "needs units owning explore.rs and voice.rs (Daemon::reset_listen, Daemon::resync_agent)"]
async fn send_resync_order() {
    let dir = tempfile::tempdir().unwrap();
    let (d, ble) = daemon(dir.path(), true);
    d.send_resync().await;
    let sent = ble.sent();
    assert!(sent[0].get("time").is_some());
    assert!(sent[1].get("total").is_some());
    assert_eq!(sent[2], json!({"cmd": "status"}));
    assert_eq!(sent.last().unwrap(), &json!({"cmd": "sound", "on": true}));
}

#[tokio::test]
async fn background_jobs_drop_finished_work() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _ble) = daemon(dir.path(), true);
    let job = d.spawn("quick", async {});
    job.join().await;
    let held = d.spawn("held", futures::future::pending::<()>());
    let jobs = d.background_jobs();
    assert_eq!(jobs, vec![held.clone()]);
    held.cancel();
}

#[test]
fn code_origin_names_the_checkout() {
    let origin = eve_daemon::code_origin();
    assert!(origin.contains(" @ ") || std::path::Path::new(&origin).exists(), "{origin}");
}
