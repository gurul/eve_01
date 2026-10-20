//! obsbot_panel.py's tests (bridge/tests/test_obsbot_panel.py and test_obsbot_brain.py): the camera's Eve Link
//! panel (hold-to-turn with a heartbeat, the reads), its picture (a fake ffmpeg that lists an OBSBOT camera and
//! writes JPEGs, or is refused the camera), and the models' camera (strict tools, the code path's matcher, and
//! handle/run over a real Obsbot with a fake USB camera).

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar};
use std::time::Duration;

use bytes::Bytes;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_obsbot::{Clock, Direction, Obsbot, Reply, Res, SystemClock, UsbControl, NO_CAMERA_LINE};

use super::*;

// ---- fakes -------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Sent {
    Wake,
    Turn(&'static str),
    Zoom(i64),
    Move(Option<f64>, Option<f64>),
}

type Log = Arc<Mutex<Vec<Sent>>>;

/// A one-shot flag a thread can wait on (Python's `threading.Event`).
#[derive(Default)]
struct Flag(std::sync::Mutex<bool>, Condvar);

impl Flag {
    fn set(&self) {
        *self.0.lock().unwrap() = true;
        self.1.notify_all();
    }

    fn wait(&self, secs: f64) -> bool {
        let g = self.0.lock().unwrap();
        let (g, _) = self.1.wait_timeout_while(g, Duration::from_secs_f64(secs), |set| !*set).unwrap();
        *g
    }
}

/// UsbCamera's turn and wake, recorded into the shared list; the wake can be held at a gate.
struct FakeUsbTurns {
    sent: Log,
    gate: Mutex<Option<(Arc<Flag>, Arc<Flag>)>>,
}

impl Turner for FakeUsbTurns {
    fn wake(&self) -> bool {
        let gate = self.gate.lock().clone();
        if let Some((entered, release)) = gate {
            entered.set();
            release.wait(5.0);
        }
        self.sent.lock().push(Sent::Wake);
        true
    }

    fn turn(&self, direction: Direction, _speed: i64) -> bool {
        self.sent.lock().push(Sent::Turn(direction.as_str()));
        true
    }

    fn why(&self) -> String {
        String::new()
    }
}

fn obj(v: Value) -> Res {
    v.as_object().cloned().unwrap_or_default()
}

/// Obsbot's surface: moves and reads answered, USB turns recorded.
struct FakeCam {
    sent: Log,
    ready: bool,
    answers: bool,
    usb: Arc<FakeUsbTurns>,
}

impl Camera for FakeCam {
    fn ensure(&mut self) -> Option<String> {
        if !self.answers {
            return Some(NO_CAMERA_LINE.to_string());
        }
        self.ready = true;
        None
    }
    fn ready(&self) -> bool {
        self.ready
    }
    fn set_ready(&mut self, ready: bool) {
        self.ready = ready;
    }
    fn status(&mut self) -> Res {
        obj(json!({"ok": true, "line": "Tiny 3 awake; pan 0°, pitch 0°; zoom 0%.", "pan": 0}))
    }
    fn position_reply(&mut self) -> Res {
        obj(json!({"ok": true, "pan": 0, "pitch": 0, "line": ""}))
    }
    fn zoom_reply(&mut self) -> Res {
        obj(json!({"ok": true, "zoom": 0, "line": ""}))
    }
    fn move_to(&mut self, pan: Option<f64>, pitch: Option<f64>) -> Res {
        self.sent.lock().push(Sent::Move(pan, pitch));
        let f = |v: Option<f64>| v.map(|x| x.to_string()).unwrap_or_default();
        obj(json!({"ok": true, "line": format!("Pan {}°, pitch {}°.", f(pan), f(pitch)), "pan": pan, "pitch": pitch}))
    }
    fn reset(&mut self) -> Res {
        obj(json!({"ok": true, "line": "Centered.", "pan": 0, "pitch": 0}))
    }
    fn zoom(&mut self, level: i64) -> Res {
        self.sent.lock().push(Sent::Zoom(level));
        obj(json!({"ok": true, "line": format!("Zoom {level}%."), "zoom": level}))
    }
    fn wake(&mut self, awake: bool) -> Res {
        obj(json!({"ok": true, "line": if awake { "Tiny 3 awake." } else { "Tiny 3 asleep." }}))
    }
    fn track(&mut self, on: bool) -> Res {
        obj(json!({"ok": true, "line": if on { "Tracking on." } else { "Tracking off." }}))
    }
    fn mode(&mut self, name: &str) -> Res {
        obj(json!({"ok": true, "line": format!("AI mode {name}.")}))
    }
    fn preset(&mut self, n: i64) -> Res {
        obj(json!({"ok": true, "line": format!("Going to preset {n}.")}))
    }
    fn close(&mut self) {}
    fn turner(&mut self) -> Arc<dyn Turner> {
        self.usb.clone()
    }
}

struct Rig {
    panel: ObsbotPanel,
    sent: Log,
    usb: Arc<FakeUsbTurns>,
}

fn rig_with(ready: bool, mut o: PanelOptions) -> Rig {
    let sent: Log = Arc::default();
    let usb = Arc::new(FakeUsbTurns { sent: sent.clone(), gate: Mutex::new(None) });
    let (s, u) = (sent.clone(), usb.clone());
    o.make = Arc::new(move || {
        Ok(Box::new(FakeCam { sent: s.clone(), ready: false, answers: ready, usb: u.clone() }) as Box<dyn Camera>)
    });
    Rig { panel: ObsbotPanel::new(o), sent, usb }
}

fn rig() -> Rig {
    rig_with(true, PanelOptions::default())
}

impl Rig {
    fn turns(&self) -> Vec<&'static str> {
        self.sent.lock().iter().filter_map(|s| if let Sent::Turn(d) = s { Some(*d) } else { None }).collect()
    }

    fn names(&self) -> Vec<&'static str> {
        self.sent
            .lock()
            .iter()
            .map(|s| match s {
                Sent::Wake => "wake",
                Sent::Turn(_) => "turn",
                Sent::Zoom(_) => "zoom",
                Sent::Move(..) => "move",
            })
            .collect()
    }
}

fn hold(dir: &str, id: &str) -> Value {
    json!({"action": "hold", "dir": dir, "hold": id})
}

fn ok(v: &Value) -> bool {
    v["ok"] == json!(true)
}

// ---- hold to turn ----------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_press_wakes_and_turns_once_and_a_stop_ends_it() {
    let r = rig();
    let p = &r.panel;
    for _ in 0..3 {
        assert_eq!(p.panel(&hold("left", "a")).await, json!({"ok": true, "line": ""}));
    }
    // the first beat wakes the camera and starts the USB turn; the rest only renew it
    assert_eq!(r.names(), ["wake", "turn"]);
    assert_eq!(r.turns(), ["left"]);
    let res = p.panel(&json!({"action": "stop", "hold": "a"})).await;
    assert_eq!(res["stopped"], json!(true));
    assert_eq!(r.turns(), ["left", "stop"]);
    let late = p.panel(&hold("left", "a")).await; // a beat that crossed the stop
    assert!(late["stopped"] == json!(true) && !ok(&late));
    assert_eq!(r.turns(), ["left", "stop"]);
    p.close().await;
}

#[tokio::test]
async fn test_a_new_direction_replaces_the_turn() {
    let r = rig();
    let p = &r.panel;
    p.panel(&hold("up", "a")).await;
    p.panel(&hold("right", "b")).await;
    assert_eq!(r.turns(), ["up", "right"]);
    assert_eq!(p.panel(&hold("up", "a")).await["stopped"], json!(true)); // the replaced press
    p.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_stop_during_the_first_beats_wake_never_turns() {
    // The wake takes up to about 7 s; a stop in it used to be undone when the turn went out after it.
    let r = rig();
    let p = r.panel.clone();
    assert_eq!(p.ready().await, None);
    let (entered, release) = (Arc::new(Flag::default()), Arc::new(Flag::default()));
    *r.usb.gate.lock() = Some((entered.clone(), release.clone()));
    let beat = tokio::spawn({
        let p = p.clone();
        async move { p.panel(&hold("left", "a")).await }
    });
    let e = entered.clone();
    assert!(tokio::task::spawn_blocking(move || e.wait(5.0)).await.unwrap());
    assert_eq!(p.panel(&json!({"action": "stop", "hold": "a"})).await["stopped"], json!(true));
    release.set();
    let res = beat.await.unwrap();
    assert!(res["stopped"] == json!(true) && !ok(&res));
    assert_eq!(r.turns(), ["stop"]); // no turn after the stop
    p.close().await;
}

#[tokio::test]
async fn test_no_beat_stops_the_gimbal() {
    let r = rig();
    let p = &r.panel;
    p.panel(&hold("down", "a")).await;
    tokio::time::sleep(Duration::from_secs_f64(HEARTBEAT_SECS + 0.3)).await;
    assert_eq!(r.turns(), ["down", "stop"]);
    assert_eq!(p.holding(), None);
    assert_eq!(p.panel(&hold("down", "a")).await["stopped"], json!(true));
    p.close().await;
}

#[tokio::test]
async fn test_beats_keep_it_turning_past_the_heartbeat() {
    let r = rig();
    let p = &r.panel;
    for _ in 0..8 {
        // 8 x 0.15 s > 0.45 s
        p.panel(&hold("down", "a")).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert_eq!(r.turns(), ["down"]); // no stop between
    p.panel(&json!({"action": "close"})).await;
    assert_eq!(r.turns(), ["down", "stop"]);
    p.close().await;
}

#[tokio::test]
async fn test_ended_presses_are_remembered_up_to_the_limit() {
    let r = rig();
    let p = &r.panel;
    for n in 0..ENDED_KEEP + 1 {
        p.panel(&json!({"action": "stop", "hold": format!("p{n}")})).await;
    }
    // the oldest was forgotten, so its beat turns again; a newer one stays refused
    assert!(ok(&p.panel(&hold("left", "p0")).await));
    assert_eq!(p.panel(&hold("left", "p1")).await["stopped"], json!(true));
    p.close().await;
}

// ---- the reads -------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_reads_and_refusals() {
    let r = rig();
    let p = &r.panel;
    let st = p.panel(&json!({"action": "status"})).await;
    assert!(ok(&st));
    assert_eq!(st["modes"], json!(eve_obsbot::keys(&eve_obsbot::AI_MODES)));
    assert_eq!(p.panel(&json!({"action": "zoom", "level": "40"})).await["line"], json!("Zoom 40%."));
    assert!(!ok(&p.panel(&json!({"action": "zoom", "level": true})).await));
    assert_eq!(p.panel(&json!({"action": "move", "pan": 10, "pitch": -5})).await["pan"], json!(10.0));
    assert!(!ok(&p.panel(&json!({"action": "fly"})).await));
    assert!(!ok(&p.panel(&hold("sideways", "x")).await));
    let down = rig_with(false, PanelOptions::default());
    let res = down.panel.panel(&json!({"action": "status"})).await;
    assert_eq!(res, json!({"ok": false, "line": "No OBSBOT camera is plugged into this Mac.", "stopped": false}));
    p.close().await;
    down.panel.close().await;
}

#[tokio::test]
async fn test_panel_lines_and_numbers_as_python_reads_them() {
    let r = rig();
    let p = &r.panel;
    // no action is a status read; zoom truncates a float level, refuses what Python can't make an int of
    assert!(p.panel(&json!({})).await["modes"].is_array());
    assert_eq!(p.panel(&json!({"action": "zoom", "level": 40.9})).await["zoom"], json!(40));
    assert_eq!(p.panel(&json!({"action": "zoom", "level": " 1_0 "})).await["zoom"], json!(10));
    assert_eq!(p.panel(&json!({"action": "zoom"})).await, json!({"ok": false, "line": "Zoom is 0 to 100."}));
    assert_eq!(p.panel(&json!({"action": "zoom", "level": "inf"})).await["line"], json!("Zoom is 0 to 100."));
    assert_eq!(p.panel(&json!({"action": "preset", "n": "2"})).await["line"], json!("Going to preset 2."));
    assert_eq!(p.panel(&json!({"action": "preset"})).await["line"], json!("Going to preset 0."));
    assert_eq!(p.panel(&json!({"action": "mode", "name": "desk"})).await["line"], json!("AI mode desk."));
    assert_eq!(p.panel(&json!({"action": "track", "on": 1})).await["line"], json!("Tracking on."));
    assert_eq!(p.panel(&json!({"action": "sleep"})).await["line"], json!("Tiny 3 asleep."));
    assert_eq!(p.panel(&json!({"action": "move", "pan": "x"})).await["line"], json!("Pan °, pitch °."));
    assert_eq!(
        p.panel(&json!({"action": "hold", "dir": "left"})).await,
        json!({"ok": false, "line": "Hold an arrow.", "stopped": true})
    );
    assert_eq!(p.panel(&json!({"action": "rtc", "sdp": ""})).await, json!({"ok": false, "rtc": false, "line": ""}));
    assert_eq!(p.panel(&json!({"action": "rtc", "sdp": 5})).await["rtc"], json!(false));
    let big = "v".repeat(eve_pika::pika_rtc::MAX_SDP_CHARS + 1);
    assert_eq!(p.panel(&json!({"action": "rtc", "sdp": big})).await["rtc"], json!(false));
    p.close().await;
}

#[test]
fn test_it_is_listed_only_when_it_is_on() {
    // link_chat's listing and token belong to its own port; this crate's half is `available` and make_panel
    let off: HashMap<String, String> = [("EVE_OBSBOT".to_string(), " 0 ".to_string())].into();
    assert!(!enabled(Some(&off)));
    assert!(make_panel(Some(&off)).is_none());
    let r = rig_with(true, PanelOptions { environ: Some(off), ..Default::default() });
    assert!(!r.panel.available());
    let on: HashMap<String, String> = [("EVE_OBSBOT".to_string(), "1".to_string())].into();
    let here = Path::new(eve_obsbot::APP_PATH).exists()
        || eve_obsbot_usb::present(&eve_obsbot_usb::SubprocessRunner);
    assert_eq!(enabled(Some(&on)), here);
    assert_eq!(make_panel(Some(&on)).is_some(), here);
    assert!(r.sent.lock().is_empty()); // nothing reached the camera
}

// ---- the picture -------------------------------------------------------------------------------------------------

const FAKE_FFMPEG: &str = r#"#!/bin/sh
mode=MODE
for a in "$@"; do
  if [ "$a" = "-list_devices" ]; then
    printf '[AVFoundation indev @ 0x1] AVFoundation video devices:\n[AVFoundation indev @ 0x1] [0] MacBook Pro Camera\n' >&2
    if [ "$mode" != none ]; then printf '[AVFoundation indev @ 0x1] [1] OBSBOT Tiny 3 StreamCamera\n' >&2; fi
    printf '[AVFoundation indev @ 0x1] AVFoundation audio devices:\n[AVFoundation indev @ 0x1] [0] OBSBOT Tiny 3 Microphone\n' >&2
    exit 1
  fi
done
if [ "$mode" = denied ]; then
  echo "Failed to create AV capture input device: Cannot use OBSBOT (not authorized to capture video)" >&2
  exit 1
fi
prev=""
for a in "$@"; do
  if [ "$prev" = "-i" ] && [ "$a" != "1:none" ]; then exit 3; fi
  prev="$a"
done
n=0
while :; do
  n=$(( n % 249 + 1 ))
  b=$(printf '\\%03o' "$n")
  printf '\377\330'
  i=0
  while [ $i -lt 50 ]; do printf "$b"; i=$((i+1)); done
  printf '\377\331'
  sleep 0.05
done
"#;

fn fake_ffmpeg(dir: &Path, mode: &str) -> String {
    let f = dir.join(format!("ffmpeg-{mode}"));
    std::fs::write(&f, FAKE_FFMPEG.replace("MODE", mode)).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    f.to_string_lossy().into_owned()
}

/// One screen reading the tunnel stream: the status, whole JPEGs as they arrive.
struct Screen {
    head: Vec<u8>,
    jpegs: Arc<Mutex<Vec<Vec<u8>>>>,
    got: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
    reader: tokio::task::JoinHandle<()>,
}

impl Screen {
    async fn open(panel: &ObsbotPanel) -> Screen {
        use tokio::io::AsyncBufReadExt;
        let (mut server, client) = tokio::io::duplex(1 << 20);
        let p = panel.clone();
        let task = tokio::spawn(async move { p.stream(&mut server).await });
        let mut reader = tokio::io::BufReader::new(client);
        let mut head = Vec::new();
        loop {
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).await.unwrap();
            head.extend_from_slice(&line);
            if line == b"\r\n" || line.is_empty() {
                break;
            }
        }
        let jpegs: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let got = Arc::new(tokio::sync::Notify::new());
        let (j, g) = (jpegs.clone(), got.clone());
        let reader = tokio::spawn(async move {
            let mut body = eve_pika::pika::Body::new(reader, true);
            let mut buf = Vec::new();
            while let Ok(Some(data)) = body.next().await {
                buf.extend_from_slice(&data);
                let (found, rest) = eve_pika::pika::split_jpegs(&buf);
                buf = rest;
                j.lock().extend(found);
                g.notify_waiters();
            }
        });
        Screen { head, jpegs, got, task, reader }
    }

    fn status(&self) -> u16 {
        String::from_utf8_lossy(&self.head).split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    async fn wait_for(&self, want: usize) -> usize {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let notified = self.got.notified();
            let n = self.jpegs.lock().len();
            if n >= want || tokio::time::Instant::now() > deadline {
                return n;
            }
            let _ = tokio::time::timeout(Duration::from_millis(100), notified).await;
        }
    }

    /// The app hangs up.
    async fn close(self) {
        self.reader.abort();
        let _ = self.reader.await;
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_two_viewers_share_one_capture_and_ffmpeg_stops_after_both() {
    // Two open screens both get frames; neither ends the other (2026-10-11: a phone and a simulator took the
    // camera from each other every 3 s under the old one-viewer rule).
    let dir = tempfile::tempdir().unwrap();
    let r = rig_with(
        true,
        PanelOptions { ffmpeg: Some(fake_ffmpeg(dir.path(), "ok")), linger_secs: 0.2, ..Default::default() },
    );
    let p = &r.panel;
    let first = Screen::open(p).await;
    let head = String::from_utf8_lossy(&first.head).to_string();
    assert!(head.contains("multipart/x-mixed-replace; boundary=frame") && !head.contains("X-Stamp"));
    assert_eq!(first.status(), 200);
    assert!(first.wait_for(3).await >= 3);
    assert!(first.jpegs.lock().iter().all(|g| g.starts_with(b"\xff\xd8")));
    let second = Screen::open(p).await; // a second screen opens
    assert_eq!(second.status(), 200);
    assert!(second.wait_for(2).await >= 2);
    let cap = p.capture().clone();
    assert!(cap.viewers() == 2 && cap.running());
    let had = first.jpegs.lock().len();
    assert!(first.wait_for(had + 1).await > had); // the first still gets frames
    first.close().await;
    second.close().await;
    p.panel(&json!({"action": "close"})).await; // stops turning, not the picture
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !cap.running() {
            break;
        }
    }
    assert!(cap.viewers() == 0 && !cap.running()); // ffmpeg ended after the linger
    p.close().await;
}

#[tokio::test]
async fn test_the_capture_hands_every_viewer_the_newest_frame() {
    let made = Arc::new(AtomicUsize::new(0));
    let m = made.clone();
    let frames: FramesFn = Arc::new(move || {
        m.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::unfold(1u8, |n| async move {
            if n >= 50 {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            Some((Ok(Bytes::from(vec![n])), n + 1))
        }))
    });
    let cap = Capture::new(frames);
    let (mut a, mut b) = (cap.watch(), cap.watch());
    let mut got_a = Vec::new();
    for _ in 0..3 {
        got_a.push(a.next().await.unwrap().unwrap());
    }
    let mut got_b = Vec::new();
    for _ in 0..3 {
        got_b.push(b.next().await.unwrap().unwrap());
    }
    assert_eq!(made.load(Ordering::SeqCst), 1); // one ffmpeg for both
    let sorted = |v: &Vec<Bytes>| v.windows(2).all(|w| w[0] < w[1]);
    assert!(sorted(&got_a) && sorted(&got_b));
    a.close();
    b.close();
    cap.stop().await;

    let broken: FramesFn = Arc::new(|| Box::pin(futures::stream::iter([Err(VideoError::new("no camera"))])));
    let cap = Capture::new(broken);
    let mut w = cap.watch();
    assert_eq!(w.next().await, Some(Err(VideoError::new("no camera"))));
    assert_eq!(w.next().await, None);
    w.close();
    assert_eq!(cap.viewers(), 0);
}

#[tokio::test]
async fn test_a_capture_that_ends_says_the_picture_is_off_and_lingers_then_stops() {
    let frames: FramesFn = Arc::new(|| Box::pin(futures::stream::iter([Ok(Bytes::from_static(b"\xff\xd8\xff\xd9"))])));
    let cap = Capture::with_linger(frames, 0.1);
    let mut w = cap.watch();
    let first = w.next().await;
    // the run may have ended before the viewer looked: either the frame, or straight to the line
    match first {
        Some(Ok(j)) => {
            assert_eq!(&j[..], b"\xff\xd8\xff\xd9");
            assert_eq!(w.next().await, Some(Err(VideoError::new(VIDEO_OFF_LINE))));
        }
        other => assert_eq!(other, Some(Err(VideoError::new(VIDEO_OFF_LINE)))),
    }
    assert_eq!(cap.error().as_deref(), Some(VIDEO_OFF_LINE));
    assert_eq!(cap.jpeg(), None);
    drop(w);
    assert_eq!(cap.viewers(), 0);
}

#[tokio::test]
async fn test_the_picture_says_why_it_cant_start() {
    use futures::StreamExt;
    let dir = tempfile::tempdir().unwrap();
    for (mode, line) in [("denied", VIDEO_DENIED_LINE), ("none", VIDEO_OFF_LINE)] {
        let r = rig_with(true, PanelOptions { ffmpeg: Some(fake_ffmpeg(dir.path(), mode)), ..Default::default() });
        let mut frames = r.panel.frames();
        assert_eq!(frames.next().await, Some(Err(VideoError::new(line))), "{mode}");
        assert!(frames.next().await.is_none());
        r.panel.close().await;
    }
    // through the tunnel the line is a 503 with the owner's line
    let r = rig_with(true, PanelOptions { ffmpeg: Some(fake_ffmpeg(dir.path(), "denied")), ..Default::default() });
    let (mut server, mut client) = tokio::io::duplex(1 << 16);
    r.panel.stream(&mut server).await;
    drop(server);
    let mut out = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut client, &mut out).await.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
    assert!(out.ends_with(&eve_pika::pika::py_json_dumps(&json!({"ok": false, "line": VIDEO_DENIED_LINE}))));
    r.panel.close().await;
}

#[test]
fn test_ffmpeg_listing_finds_the_obsbot_by_name() {
    let listing = "[AVFoundation indev @ 0x1] AVFoundation video devices:\n\
[AVFoundation indev @ 0x1] [0] MacBook Pro Camera\n\
[AVFoundation indev @ 0x1] [2] OBSBOT Tiny 3 StreamCamera\n\
[AVFoundation indev @ 0x1] AVFoundation audio devices:\n\
[AVFoundation indev @ 0x1] [0] OBSBOT Tiny 3 Microphone\n";
    assert_eq!(capture::device_in(listing).as_deref(), Some("2"));
    let no_video = "[x] AVFoundation audio devices:\n[x] [0] OBSBOT Tiny 3 Microphone\n";
    assert_eq!(capture::device_in(no_video), None); // a microphone is not the camera
    assert_eq!(capture::device_in(""), None);
}

// ---- the picture over WebRTC -------------------------------------------------------------------------------------

mod phone {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use parking_lot::Mutex;
    use serde_json::{json, Value};
    use tokio::sync::Notify;
    use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
    use webrtc::peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
        RTCIceGatheringState, RTCSessionDescription,
    };

    use crate::ObsbotPanel;

    #[derive(Default)]
    pub struct Seen {
        pub frames: Mutex<Vec<(u64, Vec<u8>)>>,
        pub lines: Mutex<Vec<String>>,
        gathered: AtomicBool,
    }

    struct Handler(Arc<Seen>, Arc<Notify>);

    #[async_trait::async_trait]
    impl PeerConnectionEventHandler for Handler {
        async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
            if state == RTCIceGatheringState::Complete {
                self.0.gathered.store(true, Ordering::SeqCst);
                self.1.notify_waiters();
            }
        }
    }

    /// The app's side: one peer, one unordered `video` channel, frames and lines as they arrive.
    pub struct Phone {
        pub pc: Arc<dyn PeerConnection>,
        pub seen: Arc<Seen>,
        gathered: Arc<Notify>,
    }

    impl Phone {
        pub async fn new() -> Phone {
            let seen = Arc::new(Seen::default());
            let gathered = Arc::new(Notify::new());
            let pc = PeerConnectionBuilder::new()
                .with_configuration(RTCConfigurationBuilder::new().build())
                .with_handler(Arc::new(Handler(seen.clone(), gathered.clone())))
                .with_udp_addrs(vec!["127.0.0.1:0".to_string()])
                .build()
                .await
                .unwrap();
            let pc: Arc<dyn PeerConnection> = Arc::new(pc);
            let channel = pc
                .create_data_channel(
                    "video",
                    Some(RTCDataChannelInit { ordered: false, max_packet_life_time: Some(150), ..Default::default() }),
                )
                .await
                .unwrap();
            let s = seen.clone();
            tokio::spawn(async move {
                let channel: Arc<dyn DataChannel> = channel;
                while let Some(event) = channel.poll().await {
                    if let DataChannelEvent::OnMessage(m) = event {
                        if m.is_string {
                            s.lines.lock().push(String::from_utf8_lossy(&m.data).to_string());
                        } else if let Some(f) = eve_pika::pika_rtc::parse_message(&m.data) {
                            s.frames.lock().push(f);
                        }
                    }
                }
            });
            Phone { pc, seen, gathered }
        }

        pub async fn connect(&self, panel: &ObsbotPanel) -> Value {
            let offer = self.pc.create_offer(None).await.unwrap();
            self.pc.set_local_description(offer).await.unwrap();
            while !self.seen.gathered.load(Ordering::SeqCst) {
                let _ = tokio::time::timeout(Duration::from_millis(50), self.gathered.notified()).await;
            }
            let sdp = self.pc.local_description().await.unwrap().sdp;
            let reply = panel.panel(&json!({"action": "rtc", "sdp": sdp})).await;
            if reply["ok"] == json!(true) {
                let answer = RTCSessionDescription::answer(reply["sdp"].as_str().unwrap().to_string()).unwrap();
                self.pc.set_remote_description(answer).await.unwrap();
            }
            reply
        }

        pub async fn wait(&self, cond: impl Fn(&Seen) -> bool) -> bool {
            for _ in 0..200 {
                if cond(&self.seen) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            false
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_webrtc_viewers_share_the_capture_with_the_macs_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_with(
        true,
        PanelOptions {
            ffmpeg: Some(fake_ffmpeg(dir.path(), "ok")),
            stun: String::new(),
            udp_addrs: vec!["127.0.0.1:0".to_string()],
            linger_secs: 0.2,
            ..Default::default()
        },
    );
    let p = &r.panel;
    let before = eve_pika::pika::stamp_ms() as u64;
    let a = phone::Phone::new().await;
    let reply = a.connect(p).await;
    assert!(reply["ok"] == json!(true) && reply["rtc"] == json!(true) && reply["line"] == json!(""));
    assert!(a.wait(|s| s.frames.lock().len() >= 3).await);
    let b = phone::Phone::new().await;
    assert_eq!(b.connect(p).await["rtc"], json!(true));
    assert!(b.wait(|s| s.frames.lock().len() >= 2).await);
    assert_eq!(p.peers(), 2);
    assert_eq!(p.capture().viewers(), 2); // one ffmpeg for both screens
    for (stamp, jpeg) in a.seen.frames.lock().iter() {
        assert!(*stamp >= before && jpeg.starts_with(b"\xff\xd8") && jpeg.ends_with(b"\xff\xd9"));
    }
    p.close().await;
    assert_eq!(p.peers(), 0);
    assert!(!p.capture().running());
    let _ = a.pc.close().await;
    let _ = b.pc.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_webrtc_says_why_the_picture_cant_start_on_the_channel() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_with(
        true,
        PanelOptions {
            ffmpeg: Some(fake_ffmpeg(dir.path(), "denied")),
            stun: String::new(),
            udp_addrs: vec!["127.0.0.1:0".to_string()],
            ..Default::default()
        },
    );
    let a = phone::Phone::new().await;
    assert_eq!(a.connect(&r.panel).await["rtc"], json!(true));
    assert!(a.wait(|s| !s.lines.lock().is_empty()).await);
    assert_eq!(a.seen.lines.lock()[0], VIDEO_DENIED_LINE);
    assert!(a.seen.frames.lock().is_empty());
    r.panel.close().await;
    let _ = a.pc.close().await;
    // an offer that isn't SDP sends the app to the tunnel
    let bad = r.panel.panel(&json!({"action": "rtc", "sdp": "not an offer"})).await;
    assert_eq!(bad, json!({"ok": false, "rtc": false, "line": ""}));
    assert_eq!(r.panel.peers(), 0);
}

// ---- the models' camera (test_obsbot_brain.py) -----------------------------------------------------------------

/// UsbCamera's surface. Asleep, it reads tilt -79 and ignores moves, as the camera does (test_obsbot.FakeUsb).
#[derive(Debug)]
struct UsbState {
    pan: f64,
    tilt: f64,
    zoom_level: i64,
    asleep: bool,
    unplugged: bool,
    calls: Vec<(String, Vec<f64>)>,
}

#[derive(Clone)]
struct FakeUsb(Arc<Mutex<UsbState>>, String);

impl FakeUsb {
    fn new() -> FakeUsb {
        FakeUsb(
            Arc::new(Mutex::new(UsbState {
                pan: 0.0,
                tilt: 0.0,
                zoom_level: 0,
                asleep: false,
                unplugged: false,
                calls: Vec::new(),
            })),
            String::new(),
        )
    }
}

impl UsbControl for FakeUsb {
    fn read(&mut self) -> Option<Reply> {
        let s = self.0.lock();
        if s.unplugged {
            self.1 = NO_CAMERA_LINE.into();
            return None;
        }
        json!({"ok": true, "name": "OBSBOT Tiny 3", "pan": s.pan, "tilt": if s.asleep { -79.0 } else { s.tilt },
               "zoom": s.zoom_level, "asleep": s.asleep})
        .as_object()
        .cloned()
    }

    fn wake(&mut self) -> bool {
        let mut s = self.0.lock();
        s.calls.push(("wake".into(), vec![]));
        s.asleep = false;
        !s.unplugged
    }

    fn point(&mut self, pan: f64, tilt: f64) -> bool {
        let mut s = self.0.lock();
        s.calls.push(("point".into(), vec![pan, tilt]));
        if !s.asleep {
            s.pan = pan;
            s.tilt = tilt;
        }
        true
    }

    fn turn(&mut self, direction: Direction, _speed: i64) -> bool {
        let mut s = self.0.lock();
        s.calls.push(("turn".into(), vec![]));
        let d = match direction {
            Direction::Left => (5.0, 0.0),
            Direction::Right => (-5.0, 0.0),
            Direction::Up => (0.0, 5.0),
            Direction::Down => (0.0, -5.0),
            Direction::Stop => (0.0, 0.0),
        };
        s.pan += d.0;
        s.tilt += d.1;
        true
    }

    fn zoom(&mut self, level: i64) -> bool {
        let mut s = self.0.lock();
        s.calls.push(("zoom".into(), vec![level as f64]));
        s.zoom_level = level;
        true
    }

    fn why(&self) -> &str {
        &self.1
    }
}

/// `time.sleep` patched out; `time.monotonic` stays real, as in the Python tests.
struct Quick;

impl Clock for Quick {
    fn monotonic(&self) -> f64 {
        SystemClock.monotonic()
    }

    fn sleep(&self, _secs: f64) {
        std::thread::yield_now();
    }
}

fn brain(usb: &FakeUsb) -> ObsbotPanel {
    let usb = usb.clone();
    ObsbotPanel::with_make(Arc::new(move || {
        let cam = Obsbot::options().port(1).app_running(|| false).usb(usb.clone()).clock(Quick).open()?;
        Ok(Box::new(cam) as Box<dyn Camera>)
    }))
}

fn control(kw: Value) -> Value {
    let mut args = Map::new();
    for k in ["action", "direction", "degrees", "pan", "tilt", "zoom", "zoom_change", "mode", "preset"] {
        args.insert(k.into(), kw.get(k).cloned().unwrap_or(Value::Null));
    }
    Value::Object(args)
}

#[test]
fn test_tools_are_strict_and_complete() {
    let names: Vec<&str> = TOOLS.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOL_NAMES);
    assert_eq!(TOOL_NAMES, ["camera_status", "camera_control"]);
    for t in TOOLS.iter() {
        let params = &t["parameters"];
        assert!(t["strict"] == json!(true) && params["additionalProperties"] == json!(false));
        let mut required: Vec<&str> = params["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        let mut props: Vec<&str> = params["properties"].as_object().unwrap().keys().map(String::as_str).collect();
        required.sort();
        props.sort();
        assert_eq!(required, props);
    }
    let ctl = &TOOLS.iter().find(|t| t["name"] == "camera_control").unwrap()["parameters"]["properties"];
    assert_eq!(ctl["action"]["enum"], json!(ACTIONS));
    let mut modes = eve_obsbot::keys(&eve_obsbot::AI_MODES).into_iter().map(Value::from).collect::<Vec<_>>();
    modes.push(Value::Null);
    assert_eq!(ctl["mode"]["enum"], Value::Array(modes));
    // a deep copy each time
    let r = rig();
    let mut mine = r.panel.tools();
    mine[0]["name"] = json!("changed");
    assert_eq!(TOOLS[0]["name"], json!("camera_status"));
}

/// Python's `json.dumps(obsbot_panel.TOOLS)` and `json.dumps(obsbot_panel.INSTRUCTIONS)`, byte for byte.
const PY_TOOLS: &str = r#"[{"type": "function", "name": "camera_status", "strict": true, "description": "The owner's OBSBOT webcam on the Mac right now: awake or asleep, where it points (pan degrees left or right of straight ahead, tilt degrees up or down), the zoom in percent, and its saved presets.", "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}}, {"type": "function", "name": "camera_control", "strict": true, "description": "Move the owner's OBSBOT webcam on the Mac: turn it (a direction and degrees from where it points), point it (pan and tilt from straight ahead), center it, zoom (a level or a change), wake it or put it to sleep, turn AI tracking on or off, set the AI mode, or go to a saved preset. Pass null for what you don't use.", "parameters": {"type": "object", "additionalProperties": false, "required": ["action", "direction", "degrees", "pan", "tilt", "zoom", "zoom_change", "mode", "preset"], "properties": {"action": {"type": "string", "enum": ["turn", "point", "center", "zoom", "wake", "sleep", "track_on", "track_off", "mode", "preset"]}, "direction": {"type": ["string", "null"], "enum": ["left", "right", "up", "down", null], "description": "turn: which way, as the picture moves; or null."}, "degrees": {"type": ["integer", "null"], "description": "turn: how far, 1-120; null for 20. \"A bit\" is 10, \"a lot\" 45."}, "pan": {"type": ["integer", "null"], "description": "point: degrees from straight ahead, -129 to 129, + is left; or null to keep it."}, "tilt": {"type": ["integer", "null"], "description": "point: degrees from level, -59 to 59, + is up; or null to keep it."}, "zoom": {"type": ["integer", "null"], "description": "zoom: 0-100 percent; or null."}, "zoom_change": {"type": ["integer", "null"], "description": "zoom: a change such as 25 or -25, when no level was asked; or null."}, "mode": {"type": ["string", "null"], "enum": ["single", "group", "voice", "desk", "hand", "whiteboard", null], "description": "mode: single follows one person, group frames everyone, voice follows who speaks, desk looks down at the desk, hand follows a hand, whiteboard frames a whiteboard; or null."}, "preset": {"type": ["integer", "null"], "description": "preset: 1, 2 or 3, saved in OBSBOT Center; or null."}}}}]"#;
const PY_INSTRUCTIONS: &str = r#""The owner's OBSBOT webcam, the Mac's camera (take_photo takes its pictures).\nWhat you can do: camera_control turns it (a direction and degrees from where it points: a turn with no amount is\n20\u00b0, \"a bit\" 10\u00b0, \"a lot\" 45\u00b0), points it (pan + left, tilt + up, from straight\nahead), centers it, zooms (\"zoom in\" is zoom_change 25), wakes it or puts it to sleep, turns AI tracking on\nor off, sets the AI mode and goes to a saved preset. camera_status reads where it points, the zoom and whether it\nis awake.\nVerify: turns, points, centering and zoom are read back from the camera: say where it ended up, in plain words\n(\"a little to the left, level\"). Tracking and the AI mode are not read back: say what you set.\nTo aim at something by name (\"the door\", \"my desk\"), you need to see the room first: take_photo, then turn toward\nit, and say if you are guessing. While tracking is on, the camera follows a person and won't stay where it was\npointed: turn tracking off first when the owner wants it to hold still.\nLimits: moving, zoom and waking always work. Tracking, the AI mode, presets and sleep are OBSBOT's own features and\nwork only while the OBSBOT Center app runs on the Mac; when one can't, say so in one line.""#;

#[test]
fn test_tools_and_instructions_are_pythons_bytes() {
    assert_eq!(eve_pika::pika::py_json_dumps(&Value::Array(TOOLS.clone())), PY_TOOLS);
    assert_eq!(eve_pika::pika::py_json_dumps(&json!(*INSTRUCTIONS)), PY_INSTRUCTIONS);
    assert_eq!(TILT_RANGE, (-59, 59));
}

#[test]
fn test_instructions_name_the_limits() {
    let text = &*INSTRUCTIONS;
    assert!(text.contains("take_photo") && text.contains("OBSBOT Center") && text.contains("tracking"));
}

#[test]
fn test_match() {
    let cases: Vec<(&str, Command)> = vec![
        ("camera left", Command::turn("left", TURN_DEGREES)),
        ("turn the camera left a bit", Command::turn("left", BIT_DEGREES)),
        ("pan the webcam right 30 degrees", Command::turn("right", 30)),
        ("camera to the left", Command::turn("left", TURN_DEGREES)),
        ("camera up a lot", Command::turn("up", LOT_DEGREES)),
        ("hey eve can you turn the camera right please", Command::turn("right", 20)),
        ("center the camera", Command::new("center")),
        ("camera straight ahead", Command::new("center")),
        ("zoom the camera in", Command::zoom_by(ZOOM_STEP)),
        ("camera zoom out a bit", Command::zoom_by(-(ZOOM_STEP / 2))),
        ("zoom in all the way on the camera", Command::zoom_to(100)),
        ("camera zoom 50%", Command::zoom_to(50)),
        ("wake the camera", Command::new("wake")),
        ("camera off", Command::new("sleep")),
        ("put the camera to sleep", Command::new("sleep")),
        ("where is the camera pointing", Command::new("status")),
        ("is my webcam awake", Command::new("status")),
        ("camera track me", Command::new("track_on")),
        ("camera stop tracking", Command::new("track_off")),
        ("camera preset 2", Command::preset(2)),
        // and more, each as Python reads it (2026-10-19)
        ("Camera, LEFT!", Command::turn("left", 20)),
        ("turn left on the camera", Command::turn("left", 20)),
        ("zoom out with my cam", Command::zoom_by(-25)),
        ("camera zoom to 100", Command::zoom_to(100)),
        ("unzoom the camera", Command::zoom_to(0)),
        ("switch the camera off", Command::new("sleep")),
        ("stop the camera following me", Command::new("track_off")),
        ("preset 3 on the webcam", Command::preset(3)),
        ("okay eve please camera on thanks", Command::new("wake")),
        ("camera to preset 1", Command::preset(1)),
        ("camera right by 120°", Command::turn("right", 120)),
        ("where’s the camera aimed", Command::new("status")),
        // Python's `\w` drops a combining mark and connector punctuation and keeps numbers such as `²`
        ("camera left \u{301}", Command::turn("left", 20)),
        ("camera\u{203f}left", Command::turn("left", 20)),
        ("camera left\u{a0}please", Command::turn("left", 20)),
        // int() reads any Unicode decimal digit
        ("camera left ٣٠ degrees", Command::turn("left", 30)),
        ("camera zoom ٥٠%", Command::zoom_to(50)),
        ("camera right 𝟗𝟗°", Command::turn("right", 99)),
        ("camera left ０７ deg", Command::turn("left", 7)),
        ("camera zoom to ᠑", Command::zoom_to(1)),
        ("camera left ꘩ deg", Command::turn("left", 9)),
    ];
    for (text, cmd) in cases {
        assert_eq!(r#match(text), Some(cmd), "{text}");
    }
}

#[test]
fn test_match_leaves_the_rest_to_the_model() {
    for text in [
        "take a photo with the camera", // take_photo's
        "point the camera at the door", // needs eyes: the model
        "turn left",                    // names no camera
        "zoom in",
        "camera left in ten minutes", // words over
        "camera zoom 300%",           // out of range
        "camera left 500 degrees",
        "turn on the tv",
        "lights off",
        "what's the best camera for vlogging",
        "camera left 0 degrees",
        "² webcam wake",
        "camera left ²",
        "camera \u{216b} left",
        "",
    ] {
        assert_eq!(r#match(text), None, "{text}");
    }
    assert_eq!(r#match(&format!("camera left {}", "please ".repeat(20))), None); // past 100 characters
}

#[tokio::test]
async fn test_handle_status_turn_point_zoom_center() {
    let usb = FakeUsb::new();
    let p = brain(&usb);
    let st = p.handle("camera_status", &json!({})).await;
    assert!(ok(&st) && st["tilt"] == json!(0));
    assert_eq!(st["line"], json!("Tiny 3 is awake, pointing straight ahead, zoom 0%."));
    let res = p.handle("camera_control", &control(json!({"action": "turn", "direction": "left", "degrees": 15}))).await;
    assert_eq!(res, json!({"ok": true, "pan": 15, "tilt": 0, "line": "Camera pointing 15° left."}));
    let res = p.handle("camera_control", &control(json!({"action": "point", "pan": -30, "tilt": 15}))).await;
    assert!(usb.0.lock().calls.contains(&("point".to_string(), vec![-30.0, 15.0])));
    assert_eq!(res["line"], json!("Camera pointing 30° right, 15° up."));
    let res = p.handle("camera_control", &control(json!({"action": "turn", "direction": "down"}))).await;
    assert_eq!(res["tilt"], json!(15 - TURN_DEGREES));
    let res = p.handle("camera_control", &control(json!({"action": "zoom", "zoom_change": 25}))).await;
    assert_eq!(res["zoom"], json!(25));
    let res = p.handle("camera_control", &control(json!({"action": "zoom", "zoom_change": -50}))).await;
    assert_eq!(res["zoom"], json!(0)); // clamped
    let res = p.handle("camera_control", &control(json!({"action": "center"}))).await;
    assert_eq!(res["line"], json!("Camera pointing straight ahead."));
    p.close().await;
}

#[tokio::test]
async fn test_handle_refusals() {
    let usb = FakeUsb::new();
    let p = brain(&usb);
    assert!(!ok(&p.handle("camera_control", &control(json!({"action": "turn"}))).await)); // no direction
    assert!(!ok(&p.handle("camera_control", &control(json!({"action": "point"}))).await)); // no angles
    assert!(!ok(&p.handle("camera_control", &control(json!({"action": "zoom"}))).await));
    assert_eq!(p.handle("camera_fly", &json!({})).await["reason"], json!("unknown tool camera_fly"));
    let track = p.handle("camera_control", &control(json!({"action": "track_on"}))).await;
    assert!(track["ok"] == json!(false) && track["line"] == json!(eve_obsbot::APP_NEEDED_LINE));
    assert_eq!(
        p.handle("camera_control", &control(json!({"action": "fly"}))).await,
        json!({"ok": false, "reason": "unknown action fly"})
    );
    assert_eq!(
        p.handle("camera_control", &json!({})).await,
        json!({"ok": false, "reason": "unknown action None"})
    );
    usb.0.lock().unplugged = true;
    let gone = brain(&usb);
    assert_eq!(gone.handle("camera_status", &json!({})).await["reason"], json!(NO_CAMERA_LINE));
    assert_eq!(gone.run(&Command::new("center")).await, NO_CAMERA_LINE);
    p.close().await;
    gone.close().await;
}

#[tokio::test]
async fn test_run_is_the_code_path_line() {
    let usb = FakeUsb::new();
    let p = brain(&usb);
    assert_eq!(p.run(&r#match("camera left a bit").unwrap()).await, "Camera pointing 10° left.");
    assert_eq!(p.run(&r#match("camera zoom in").unwrap()).await, "Zoom 25%.");
    assert_eq!(
        p.run(&r#match("where is the camera pointing").unwrap()).await,
        "Tiny 3 is awake, pointing 10° left, zoom 25%."
    );
    assert_eq!(p.run(&r#match("camera track me").unwrap()).await, eve_obsbot::APP_NEEDED_LINE);
    p.close().await;
}

#[test]
fn test_where_reads_in_the_owners_words() {
    assert_eq!(where_(Some(0), Some(0)), "straight ahead");
    assert_eq!(where_(Some(20), Some(-10)), "20° left, 10° up");
    assert_eq!(where_(Some(-5), Some(30)), "5° right, 30° down");
    assert_eq!(where_(None, Some(0)), "somewhere I couldn't read");
    assert_eq!(where_(Some(3), Some(-3)), "3° left, 3° up");
    assert_eq!(where_(Some(-2), Some(2)), "straight ahead");
    assert_eq!(where_value(&json!(15.5), &json!(-0.0)), "15.5° left");
}

#[test]
fn test_moved_says_where_it_stopped() {
    let stopped = panel_moved(json!({"ok": false, "line": "Asked...", "pan": 129, "pitch": 0}), "turn left");
    assert_eq!(
        stopped,
        json!({"ok": false, "pan": 129, "tilt": 0,
               "line": "The camera stopped at 129° left (the end of its range, or it is asleep)."})
    );
    let unread = panel_moved(json!({"ok": false, "line": "Couldn't read where the camera points."}), "center");
    assert_eq!(unread, json!({"ok": false, "reason": "Couldn't read where the camera points."}));
    let bare = panel_moved(json!({"ok": true}), "point");
    assert_eq!(bare, json!({"ok": false, "reason": "the camera didn't confirm the point"}));
}

fn panel_moved(res: Value, what: &str) -> Value {
    panel::moved(res.as_object().unwrap(), what)
}

#[test]
fn test_num_reads_like_pythons_float() {
    assert_eq!(num(Some(&json!(3))), Some(3.0));
    assert_eq!(num(Some(&json!(" 2.5 "))), Some(2.5));
    assert_eq!(num(Some(&json!(true))), None);
    assert_eq!(num(Some(&json!(null))), None);
    assert_eq!(num(Some(&json!("x"))), None);
    assert_eq!(num(Some(&json!([1]))), None);
    assert_eq!(num(None), None);
    assert!(num(Some(&json!("nan"))).is_some_and(f64::is_nan));
}


