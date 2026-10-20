//! Host vision — frame decode, face geometry, the drop governor (test_vision.py),
//! and the native Vision boundary on macOS (test_vision_native.py).
//!
//! No Vision framework in the core tests: the detector is a plain closure.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use parking_lot::Mutex;
use serde_json::{json, Value};

use super::*;

// A real 4x2 baseline JPEG (488 bytes, written by ImageIO) so the jpeg
// branch is exercised with genuine bytes.
const TINY_JPEG_B64: &str = concat!(
    "/9j/4AAQSkZJRgABAQAASABIAAD/4QBARXhpZgAATU0AKgAAAAgAAYdpAAQAAAABAAAAGgAAAAAAAqACAAQAAAABAAAABKAD",
    "AAQAAAABAAAAAgAAAAD/7QA4UGhvdG9zaG9wIDMuMAA4QklNBAQAAAAAAAA4QklNBCUAAAAAABDUHYzZjwCyBOmACZjs+EJ+",
    "/8AACwgAAgAEAQERAP/EAB8AAAEFAQEBAQEBAAAAAAAAAAABAgMEBQYHCAkKC//EALUQAAIBAwMCBAMFBQQEAAABfQECAwAE",
    "EQUSITFBBhNRYQcicRQygZGhCCNCscEVUtHwJDNicoIJChYXGBkaJSYnKCkqNDU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVm",
    "Z2hpanN0dXZ3eHl6g4SFhoeIiYqSk5SVlpeYmZqio6Slpqeoqaqys7S1tre4ubrCw8TFxsfIycrS09TV1tfY2drh4uPk5ebn",
    "6Onq8fLz9PX29/j5+v/bAEMABgYGBgYGCgYGCg4KCgoOEg4ODg4SFxISEhISFxwXFxcXFxccHBwcHBwcHCIiIiIiIicnJycn",
    "LCwsLCwsLCwsLP/dAAQAAf/aAAgBAQAAPwCz4Tvr210eNLaeSIHGQjlQdqqg6HsqgD2AHQV//9k="
);

fn tiny_jpeg() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(TINY_JPEG_B64).unwrap()
}

const W: i64 = 160;
const H: i64 = 120;

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn with(mut obj: Value, extra: Value) -> Value {
    if let (Value::Object(m), Value::Object(e)) = (&mut obj, extra) {
        for (k, v) in e {
            m.insert(k, v);
        }
    }
    obj
}

fn gray_obj_wh(seq: i64, w: i64, h: i64, extra: Value) -> Value {
    with(json!({"seq": seq, "w": w, "h": h, "fmt": "gray", "b64": b64(&vec![0u8; (w * h) as usize])}), extra)
}

fn gray_obj(seq: i64, extra: Value) -> Value {
    gray_obj_wh(seq, W, H, extra)
}

fn jpeg_obj(seq: i64, extra: Value) -> Value {
    with(json!({"seq": seq, "w": 4, "h": 2, "fmt": "jpeg", "b64": TINY_JPEG_B64}), extra)
}

fn frame(seq: i64) -> Frame {
    Frame::new(seq, W, H, "gray", vec![0u8; (W * H) as usize])
}

// ---- decode_frame -----------------------------------------------------------

#[test]
fn test_decode_gray() {
    let f = decode_frame(&gray_obj(7, json!({"yaw": 12.5, "pitch": -3}))).unwrap();
    assert_eq!((f.seq, f.w, f.h, f.fmt.as_str()), (7, W, H, "gray"));
    assert_eq!(f.data.len() as i64, W * H);
    assert_eq!((f.yaw, f.pitch), (Some(12.5), Some(-3.0)));
    assert!(!f.snap);
}

#[test]
fn test_decode_jpeg_keeps_file_bytes() {
    let f = decode_frame(&jpeg_obj(1, json!({}))).unwrap();
    assert_eq!(f.fmt, "jpeg");
    assert_eq!(f.data, tiny_jpeg());
    assert!(f.data.starts_with(b"\xff\xd8") && f.data.ends_with(b"\xff\xd9"));
    assert!(f.yaw.is_none() && f.pitch.is_none());
}

#[test]
fn test_decode_yaw_pitch_ignore_junk() {
    let f = decode_frame(&gray_obj(1, json!({"yaw": "12", "pitch": true}))).unwrap();
    assert!(f.yaw.is_none() && f.pitch.is_none());
}

#[test]
fn test_decode_snap_only_when_true() {
    assert!(decode_frame(&gray_obj(1, json!({"snap": true}))).unwrap().snap);
    assert!(!decode_frame(&gray_obj(1, json!({"snap": 1}))).unwrap().snap);
}

#[test]
fn test_decode_rejects_malformed() {
    let mut no_seq = gray_obj(1, json!({}));
    no_seq.as_object_mut().unwrap().remove("seq");
    let bad = [
        with(gray_obj(1, json!({})), json!({"fmt": "rgb"})),
        with(gray_obj(1, json!({})), json!({"b64": "not base64!"})),
        with(gray_obj(1, json!({})), json!({"b64": ""})),
        with(gray_obj(1, json!({})), json!({"w": 0})),
        with(gray_obj_wh(1, 10, 10, json!({})), json!({"w": 11})), // gray length mismatch
        with(jpeg_obj(1, json!({})), json!({"b64": b64(b"PNG.....")})), // no SOI
        no_seq,
    ];
    for b in &bad {
        assert!(decode_frame(b).is_err(), "accepted {b}");
    }
}

#[test]
fn test_decode_error_messages_match_python() {
    let mut no_seq = gray_obj(1, json!({}));
    no_seq.as_object_mut().unwrap().remove("seq");
    let msg = |v: &Value| decode_frame(v).unwrap_err().0;
    assert_eq!(msg(&no_seq), "frame: bad seq/w/h: 'seq'");
    assert_eq!(
        msg(&with(gray_obj(1, json!({})), json!({"w": "x"}))),
        "frame: bad seq/w/h: invalid literal for int() with base 10: 'x'"
    );
    assert_eq!(
        msg(&with(gray_obj(1, json!({})), json!({"h": null}))),
        "frame: bad seq/w/h: int() argument must be a string, a bytes-like object or a real number, not 'NoneType'"
    );
    assert_eq!(msg(&with(gray_obj(1, json!({})), json!({"w": 0}))), "frame: bad size 0x120");
    assert_eq!(msg(&with(gray_obj(1, json!({})), json!({"fmt": "rgb"}))), "frame: unknown fmt 'rgb'");
    assert_eq!(msg(&with(gray_obj(1, json!({})), json!({"fmt": null}))), "frame: unknown fmt ''");
    assert_eq!(msg(&with(gray_obj(1, json!({})), json!({"b64": 5}))), "frame: missing b64");
    assert_eq!(
        msg(&with(gray_obj_wh(1, 10, 10, json!({})), json!({"w": 11}))),
        "frame: gray payload is 100 bytes, expected 110"
    );
    assert_eq!(
        msg(&with(jpeg_obj(1, json!({})), json!({"b64": b64(b"PNG.....")}))),
        "frame: jpeg payload lacks SOI marker"
    );
    assert!(msg(&with(gray_obj(1, json!({})), json!({"b64": "QQ"}))).starts_with("frame: bad base64: "));
}

#[test]
fn test_decode_ints_like_python_int() {
    // int("7"), int(7.9), int(True) all succeed in Python.
    let f = decode_frame(&with(gray_obj(1, json!({})), json!({"seq": " 7 "}))).unwrap();
    assert_eq!(f.seq, 7);
    let f = decode_frame(&with(gray_obj(1, json!({})), json!({"seq": 7.9}))).unwrap();
    assert_eq!(f.seq, 7);
    let f = decode_frame(&with(gray_obj(1, json!({})), json!({"seq": true}))).unwrap();
    assert_eq!(f.seq, 1);
}

#[test]
fn test_base64_validate_like_python() {
    // CPython 3.13's base64.b64decode(s, validate=True), case by case.
    let ok = |s: &str| b64decode_validate(s).unwrap();
    let err = |s: &str| b64decode_validate(s).unwrap_err();
    assert_eq!(ok("QUJD"), b"ABC");
    assert_eq!(ok("QUJ="), b"AB");
    assert_eq!(ok("QR=="), b"A"); // non-canonical trailing bits accepted
    assert_eq!(err("QUJD="), "Excess padding not allowed");
    assert_eq!(err("QUJD=="), "Excess padding not allowed");
    assert_eq!(err("QUI=="), "Excess padding not allowed");
    assert_eq!(err("QQ="), "Incorrect padding");
    assert_eq!(err("QQ"), "Incorrect padding");
    assert_eq!(
        err("QUJDQ="),
        "Invalid base64-encoded string: number of data characters (5) cannot be 1 more than a multiple of 4"
    );
    assert_eq!(err("="), "Leading padding not allowed");
    assert_eq!(err("QQ==QQ=="), "Excess data after padding");
    assert_eq!(err("QQ==\n"), "Only base64 data is allowed");
    assert_eq!(
        err("Q==="),
        "Invalid base64-encoded string: number of data characters (1) cannot be 1 more than a multiple of 4"
    );
    assert_eq!(err("AA=A"), "Discontinuous padding not allowed");
    assert_eq!(err("QUJD\u{e9}"), "string argument should contain only ASCII characters");
}

// ---- pick_face geometry -----------------------------------------------------

#[test]
fn test_pick_face_centre() {
    let r = pick_face(&[Rect::with_conf(60.0, 40.0, 40.0, 40.0, 0.9)], W, H); // centre (80, 60)
    assert_eq!(r, Some(FaceResult { bx: 0, by: 0, size: 25, conf: 90 }));
}

#[test]
fn test_pick_face_right_is_positive_bx() {
    let r = pick_face(&[Rect::new(120.0, 40.0, 40.0, 40.0)], W, H).unwrap(); // centre x = 140
    assert!(r.bx == 75 && r.by == 0);
}

#[test]
fn test_pick_face_down_is_positive_by() {
    let r = pick_face(&[Rect::new(60.0, 90.0, 40.0, 30.0)], W, H).unwrap(); // centre y = 105
    assert!(r.by == 75 && r.bx == 0);
}

#[test]
fn test_pick_face_top_left_is_negative() {
    let r = pick_face(&[Rect::new(0.0, 0.0, 16.0, 12.0)], W, H).unwrap(); // centre (8, 6)
    assert!(r.bx == -90 && r.by == -90 && r.size == 10);
}

#[test]
fn test_pick_face_largest_wins() {
    let small = Rect::with_conf(0.0, 0.0, 10.0, 10.0, 0.99);
    let big = Rect::with_conf(100.0, 20.0, 50.0, 50.0, 0.5);
    let r = pick_face(&[small, big], W, H);
    assert_eq!(r, Some(FaceResult { bx: 56, by: -25, size: 31, conf: 50 }));
}

#[test]
fn test_pick_face_clamps_out_of_frame() {
    let r = pick_face(&[Rect::new(150.0, 110.0, 60.0, 60.0)], W, H).unwrap();
    assert!(r.bx == 100 && r.by == 100 && r.size == 38);
}

#[test]
fn test_pick_face_none_without_rects() {
    assert_eq!(pick_face(&[], W, H), None);
}

#[test]
fn test_clamp_rounds_half_to_even() {
    assert_eq!(clamp(2.5, -100, 100), 2);
    assert_eq!(clamp(3.5, -100, 100), 4);
    assert_eq!(clamp(-2.5, -100, 100), -2);
}

#[test]
fn test_largest_rect_first_of_equals() {
    let a = Rect::with_conf(0.0, 0.0, 10.0, 10.0, 0.1);
    let b = Rect::with_conf(5.0, 5.0, 10.0, 10.0, 0.2);
    assert_eq!(largest_rect(&[a, b]), a);
}

// ---- wire shapes ------------------------------------------------------------

#[test]
fn test_face_cmd_with_face_echoes_pose() {
    let cmd = build_face_cmd(
        42,
        Some(FaceResult { bx: 10, by: -20, size: 30, conf: 95 }),
        Some(15.0),
        Some(-5.0),
        WHO_UNKNOWN,
    );
    assert_eq!(
        cmd,
        json!({"cmd": "face", "seq": 42, "bx": 10, "by": -20, "size": 30, "conf": 95,
               "yaw": 15.0, "pitch": -5.0, "who": "unknown"})
    );
    // Key order is the wire order.
    assert_eq!(
        serde_json::to_string(&cmd).unwrap(),
        r#"{"cmd":"face","seq":42,"bx":10,"by":-20,"size":30,"conf":95,"yaw":15.0,"pitch":-5.0,"who":"unknown"}"#
    );
}

#[test]
fn test_face_cmd_without_pose_omits_it() {
    let cmd = build_face_cmd(1, Some(FaceResult { bx: 0, by: 0, size: 5, conf: 50 }), None, None, WHO_UNKNOWN);
    assert!(cmd.get("yaw").is_none() && cmd.get("pitch").is_none());
    assert_eq!(cmd["who"], "unknown");
}

#[test]
fn test_face_cmd_no_face_is_conf_zero() {
    assert_eq!(
        build_face_cmd(9, None, Some(1.0), Some(2.0), WHO_UNKNOWN),
        json!({"cmd": "face", "seq": 9, "conf": 0, "who": "unknown"})
    );
}

#[test]
fn test_face_cmd_who_override() {
    assert_eq!(build_face_cmd(1, None, None, None, "owner")["who"], "owner");
}

#[test]
fn test_cam_cmds() {
    assert_eq!(build_cam_cmd(true), json!({"cmd": "cam", "on": true, "fps": 5, "w": 160, "h": 120}));
    assert_eq!(build_cam_cmd(false), json!({"cmd": "cam", "on": false}));
    assert_eq!(serde_json::to_string(&build_cam_cmd(true)).unwrap(), r#"{"cmd":"cam","on":true,"fps":5,"w":160,"h":120}"#);
    assert_eq!(build_snap_cmd(), json!({"cmd": "snap"}));
}

// ---- governor ---------------------------------------------------------------

#[test]
fn test_governor_runs_first_frame_immediately() {
    let mut g = FrameRateGovernor::new();
    let f = frame(1);
    assert_eq!(g.offer(f.clone()), Some(f));
    assert!(g.busy);
    assert_eq!(g.done(), None);
    assert!(!g.busy);
    assert_eq!((g.received, g.processed, g.dropped), (1, 1, 0));
}

#[test]
fn test_governor_keeps_only_newest_while_busy() {
    let mut g = FrameRateGovernor::new();
    assert!(g.offer(frame(1)).is_some());
    assert!(g.offer(frame(2)).is_none()); // parked
    assert!(g.offer(frame(3)).is_none()); // displaces 2
    assert!(g.offer(frame(4)).is_none()); // displaces 3
    assert_eq!(g.dropped, 2);
    let nxt = g.done();
    assert_eq!(nxt.map(|f| f.seq), Some(4));
    assert!(g.busy); // still running (frame 4)
    assert_eq!(g.done(), None);
    assert!(!g.busy);
    assert_eq!((g.received, g.processed, g.dropped), (4, 2, 2));
}

// ---- FaceTracker ------------------------------------------------------------

fn sender() -> (Sender, Arc<Mutex<Vec<Value>>>) {
    let items = Arc::new(Mutex::new(Vec::new()));
    let sink = items.clone();
    let send: Sender = Arc::new(move |obj: Value| {
        let sink = sink.clone();
        Box::pin(async move {
            sink.lock().push(obj);
            true
        })
    });
    (send, items)
}

fn detector(f: impl Fn(&Frame) -> Vec<Rect> + Send + Sync + 'static) -> Option<Detector> {
    Some(Arc::new(move |fr: &Frame| Ok(f(fr))))
}

fn fake_clock(now: Arc<Mutex<f64>>) -> Clock {
    Arc::new(move || *now.lock())
}

async fn sleep(secs: f64) {
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tracker_replies_face_for_each_processed_frame() {
    let (send, sent) = sender();
    let t = FaceTracker::new(
        detector(|f| if f.seq % 2 != 0 { vec![Rect::with_conf(120.0, 40.0, 40.0, 40.0, 0.8)] } else { vec![] }),
        send,
    );
    t.on_frame(&gray_obj(1, json!({"yaw": 10, "pitch": 2}))).await;
    sleep(0.05).await;
    t.on_frame(&gray_obj(2, json!({}))).await;
    sleep(0.05).await;
    t.stop();
    assert_eq!(
        *sent.lock(),
        vec![
            json!({"cmd": "face", "seq": 1, "bx": 75, "by": 0, "size": 25, "conf": 80,
                   "yaw": 10.0, "pitch": 2.0, "who": "unknown"}),
            json!({"cmd": "face", "seq": 2, "conf": 0, "who": "unknown"}),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tracker_drops_to_newest_while_detector_busy() {
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let gate_rx = std::sync::Mutex::new(gate_rx);
    let seen = Arc::new(Mutex::new(Vec::<i64>::new()));
    let seen2 = seen.clone();
    let slow: Detector = Arc::new(move |f: &Frame| {
        seen2.lock().push(f.seq);
        // gate.wait(timeout=2.0): once set, every later wait returns at once.
        let _ = gate_rx.lock().unwrap().recv_timeout(Duration::from_secs(2));
        Ok(vec![])
    });
    let (send, sent) = sender();
    let t = FaceTracker::new(Some(slow), send);
    for seq in [1, 2, 3, 4] {
        t.on_frame(&gray_obj(seq, json!({}))).await;
    }
    sleep(0.05).await; // frame 1 is inside the detector
    drop(gate_tx); // gate.set()
    sleep(0.2).await;
    t.stop();
    assert_eq!(*seen.lock(), vec![1, 4]); // 2 and 3 were never queued
    assert_eq!(sent.lock().iter().map(|o| o["seq"].as_i64().unwrap()).collect::<Vec<_>>(), vec![1, 4]);
    assert_eq!(t.governor().dropped, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tracker_stop_mid_drain_still_answers_parked_frame() {
    // Python: stop() lets the running detect finish; the parked frame then runs
    // on the loop's default executor. Rust gives the drain a fresh worker.
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let gate_rx = std::sync::Mutex::new(gate_rx);
    let slow: Detector = Arc::new(move |_f: &Frame| {
        let _ = gate_rx.lock().unwrap().recv_timeout(Duration::from_secs(2));
        Ok(vec![])
    });
    let (send, sent) = sender();
    let t = FaceTracker::new(Some(slow), send);
    t.on_frame(&gray_obj(1, json!({}))).await;
    t.on_frame(&gray_obj(2, json!({}))).await;
    sleep(0.05).await;
    t.stop();
    drop(gate_tx);
    sleep(0.2).await;
    assert_eq!(sent.lock().iter().map(|o| o["seq"].as_i64().unwrap()).collect::<Vec<_>>(), vec![1, 2]);
    assert!(!t.governor().busy);
    // And the tracker keeps working afterwards.
    t.on_frame(&gray_obj(3, json!({}))).await;
    sleep(0.1).await;
    t.stop();
    assert_eq!(sent.lock().len(), 3);
}

#[tokio::test]
async fn test_tracker_disabled_ignores_frames() {
    let (send, sent) = sender();
    let t = FaceTracker::new(None, send);
    assert!(!t.enabled());
    t.on_frame(&gray_obj(1, json!({}))).await;
    sleep(0.01).await;
    assert!(sent.lock().is_empty());
    assert_eq!(t.governor().received, 0);
}

#[tokio::test]
async fn test_tracker_counts_bad_frames_without_replying() {
    let (send, sent) = sender();
    let t = FaceTracker::new(detector(|_| vec![]), send);
    t.on_frame(&with(gray_obj(1, json!({})), json!({"fmt": "rgb"}))).await;
    assert_eq!((t.bad_frames(), sent.lock().len()), (1, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tracker_stats_line_and_window_reset() {
    let now = Arc::new(Mutex::new(100.0f64));
    let n2 = now.clone();
    let detect: Detector = Arc::new(move |f: &Frame| {
        *n2.lock() += 0.02; // 20 ms per detect on the fake clock
        Ok(if f.seq == 1 { vec![Rect::new(0.0, 0.0, 10.0, 10.0)] } else { vec![] })
    });
    let (send, _) = sender();
    let t = FaceTracker::with_options(Some(detect), send, None, fake_clock(now.clone()), None, None);
    assert_eq!(t.stats_line(), None); // nothing yet
    for seq in [1, 2] {
        t.on_frame(&gray_obj(seq, json!({}))).await;
        sleep(0.05).await;
    }
    *now.lock() += 10.0;
    let line = t.stats_line();
    let again = t.stats_line(); // window reset: nothing new
    t.stop();
    assert_eq!(line.as_deref(), Some("vision: 2 frames, 0 dropped, 0.2 fps, 20 ms/detect, faces 50%"));
    assert_eq!(again, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tracker_detector_error_answers_no_face() {
    let (send, sent) = sender();
    let t = FaceTracker::new(Some(Arc::new(|_: &Frame| anyhow::bail!("boom"))), send);
    t.on_frame(&gray_obj(3, json!({}))).await;
    sleep(0.05).await;
    // and a panicking detector does not kill the worker
    t.set_detect(Some(Arc::new(|_: &Frame| panic!("detector blew up"))));
    t.on_frame(&gray_obj(4, json!({}))).await;
    sleep(0.05).await;
    t.set_detect(detector(|_| vec![Rect::new(60.0, 40.0, 40.0, 40.0)]));
    t.on_frame(&gray_obj(5, json!({}))).await;
    sleep(0.05).await;
    t.stop();
    let seqs: Vec<(i64, i64)> =
        sent.lock().iter().map(|o| (o["seq"].as_i64().unwrap(), o["conf"].as_i64().unwrap())).collect();
    assert_eq!(seqs, vec![(3, 0), (4, 0), (5, 100)]);
}

struct Owner;

impl Identity for Owner {
    fn process(&self, _frame: &Frame, rects: &[Rect]) -> anyhow::Result<String> {
        assert!(!rects.is_empty());
        Ok("owner".into())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tracker_identity_fills_who_and_on_faces_gets_every_face() {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g2 = got.clone();
    let on_faces: OnFaces = Arc::new(move |faces, yaw, pitch| {
        let g2 = g2.clone();
        Box::pin(async move {
            g2.lock().push((faces, yaw, pitch));
            Ok(())
        })
    });
    let (send, sent) = sender();
    let t = FaceTracker::new(
        detector(|f| {
            if f.seq == 1 {
                vec![Rect::new(0.0, 0.0, 16.0, 12.0), Rect::with_conf(60.0, 40.0, 40.0, 40.0, 0.9)]
            } else {
                vec![]
            }
        }),
        send,
    );
    t.set_identity(Some(Arc::new(Owner)));
    t.set_on_faces(Some(on_faces));
    t.on_frame(&gray_obj(1, json!({"yaw": 5.5}))).await;
    sleep(0.05).await;
    t.on_frame(&gray_obj(2, json!({}))).await;
    sleep(0.05).await;
    t.stop();
    let sent = sent.lock();
    assert_eq!(sent[0]["who"], "owner");
    assert_eq!(sent[0]["bx"], 0); // the largest face
    assert_eq!(sent[1]["who"], "unknown"); // no face: identity never asked
    let got = got.lock();
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0],
        (
            vec![FaceResult { bx: -90, by: -90, size: 10, conf: 100 }, FaceResult { bx: 0, by: 0, size: 25, conf: 90 }],
            Some(5.5),
            None
        )
    );
    assert_eq!(got[1], (vec![], None, None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tracker_on_faces_failure_never_stops_tracker() {
    let on_faces: OnFaces = Arc::new(|_, _, _| Box::pin(async { anyhow::bail!("follower broke") }));
    let (send, sent) = sender();
    let t = FaceTracker::with_options(detector(|_| vec![]), send, None, Arc::new(monotonic), None, Some(on_faces));
    for seq in [1, 2] {
        t.on_frame(&gray_obj(seq, json!({}))).await;
        sleep(0.05).await;
    }
    t.stop();
    assert_eq!(sent.lock().len(), 2);
    assert!(!t.governor().busy);
}

// ---- frame dump -------------------------------------------------------------

#[test]
fn test_gray_png_is_valid_png() {
    let png = encode_gray_png(3, 2, &[0, 128, 255, 10, 20, 30]).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    let has = |n: &[u8]| png.windows(n.len()).any(|w| w == n);
    assert!(has(b"IHDR") && has(b"IDAT") && png.ends_with(b"IEND\xaeB`\x82"));
    assert!(encode_gray_png(3, 2, b"short").is_err());
    assert_eq!(encode_gray_png(3, 2, b"short").unwrap_err().0, "luma is 5 bytes, expected 6");
}

#[test]
fn test_gray_png_decodes_to_the_same_pixels() {
    use std::io::Read as _;
    let luma = [0u8, 128, 255, 10, 20, 30];
    let png = encode_gray_png(3, 2, &luma).unwrap();
    // IHDR: width, height, depth 8, colour type 0.
    assert_eq!(&png[16..29], &[0, 0, 0, 3, 0, 0, 0, 2, 8, 0, 0, 0, 0]);
    let idat_len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
    assert_eq!(&png[37..41], b"IDAT");
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&png[41..41 + idat_len]).read_to_end(&mut raw).unwrap();
    assert_eq!(raw, vec![0, 0, 128, 255, 0, 10, 20, 30]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tracker_saves_at_most_one_frame_per_second() {
    let tmp = tempfile::tempdir().unwrap();
    let now = Arc::new(Mutex::new(0.0f64));
    let (send, _) = sender();
    let t = FaceTracker::with_options(
        detector(|_| vec![]),
        send,
        Some(tmp.path().to_path_buf()),
        fake_clock(now.clone()),
        None,
        None,
    );
    t.on_frame(&gray_obj(1, json!({}))).await;
    *now.lock() += 0.5;
    t.on_frame(&jpeg_obj(2, json!({}))).await; // inside the 1 s window: skipped
    *now.lock() += 0.6;
    t.on_frame(&jpeg_obj(3, json!({}))).await;
    sleep(0.05).await;
    t.stop();
    let mut names: Vec<String> =
        std::fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, vec!["frame_000001.png", "frame_000003.jpg"]);
    assert_eq!(std::fs::read(tmp.path().join("frame_000003.jpg")).unwrap(), tiny_jpeg());
}

#[tokio::test]
async fn test_tracker_save_failure_disables_dump() {
    let tmp = tempfile::tempdir().unwrap();
    let blocker = tmp.path().join("file");
    std::fs::write(&blocker, b"x").unwrap();
    let (send, _) = sender();
    let t = FaceTracker::with_options(
        detector(|_| vec![]),
        send,
        Some(blocker.join("frames")),
        Arc::new(monotonic),
        None,
        None,
    );
    t.on_frame(&gray_obj(1, json!({}))).await;
    assert_eq!(t.save_dir(), None);
    t.stop();
}

#[test]
fn test_configured_save_dir() {
    assert_eq!(configured_save_dir_from(None, None), None);
    assert_eq!(configured_save_dir_from(None, Some("/tmp/frames ")), Some(PathBuf::from("/tmp/frames")));
    assert_eq!(configured_save_dir_from(Some("/cli/wins"), Some("/tmp/frames ")), Some(PathBuf::from("/cli/wins")));
    // An empty CLI value falls through to the environment, as `or` does.
    assert_eq!(configured_save_dir_from(Some(""), Some("/env")), Some(PathBuf::from("/env")));
    assert_eq!(configured_save_dir_from(None, Some("   ")), None);
}

#[test]
fn test_configured_save_dir_expands_user() {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
    assert_eq!(configured_save_dir_from(Some("~/frames"), None), Some(home.join("frames")));
    assert_eq!(configured_save_dir_from(Some("~//frames"), None), Some(home.join("frames")));
    assert_eq!(configured_save_dir_from(Some("~"), None), Some(home.clone()));
    // Spelled as pathlib spells it: str(Path("~/a//b/./c/").expanduser()).
    let want = format!("{}/a/b/c", home.to_string_lossy().trim_end_matches('/'));
    assert_eq!(configured_save_dir_from(Some("~/a//b/./c/"), None).unwrap().to_str(), Some(want.as_str()));
    assert_eq!(expanduser("/tmp//frames/").to_str(), Some("/tmp/frames"));
    assert_eq!(expanduser("//net/x").to_str(), Some("//net/x"));
    assert_eq!(expanduser("///x").to_str(), Some("/x"));
    assert_eq!(expanduser("./rel/").to_str(), Some("rel"));
    assert_eq!(expanduser("a/~b").to_str(), Some("a/~b"));
    assert_eq!(expanduser("/~").to_str(), Some("/~"));
    // An unknown user keeps the path (pathlib raises RuntimeError).
    assert_eq!(expanduser("~no_such_user_eve_xyz/f").to_str(), Some("~no_such_user_eve_xyz/f"));
}

// ---- adapter availability ----------------------------------------------------

#[cfg(not(target_os = "macos"))]
#[test]
fn test_make_detector_none_off_darwin() {
    assert!(make_detector().is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn test_make_detector_some_on_darwin() {
    assert!(make_detector().is_some());
}

// ---- native boundary (test_vision_native.py) -----------------------------------

#[cfg(target_os = "macos")]
fn native_frame() -> Frame {
    Frame::new(1, 160, 120, "gray", vec![128u8; 160 * 120])
}

#[cfg(target_os = "macos")]
#[test]
fn test_native_face_request_accepts_default_options() {
    // A blank image has no face, but must still construct and run the request.
    assert_eq!(vision_detect(&native_frame()).unwrap(), vec![]);
}

#[cfg(target_os = "macos")]
#[test]
fn test_native_jpeg_frame_decodes_and_probes() {
    let f = Frame::new(1, 4, 2, "jpeg", tiny_jpeg());
    assert_eq!(vision_detect(&f).unwrap(), vec![]);
    assert_eq!(probe_image_size(&tiny_jpeg()), Some((4, 2)));
    assert_eq!(probe_image_size(b"not an image"), None);
    let bad = Frame::new(9, 4, 2, "jpeg", b"\xff\xd8garbage".to_vec());
    assert_eq!(vision_detect(&bad).unwrap_err().to_string(), "could not decode jpeg frame 9");
}

#[cfg(target_os = "macos")]
#[test]
fn test_native_vision_test_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("tiny.jpg");
    std::fs::write(&p, tiny_jpeg()).unwrap();
    assert_eq!(run_vision_test(p.to_str().unwrap()), 0);
    assert_eq!(run_vision_test(tmp.path().join("missing.jpg").to_str().unwrap()), 2);
    let junk = tmp.path().join("junk.bin");
    std::fs::write(&junk, b"junk").unwrap();
    assert_eq!(run_vision_test(junk.to_str().unwrap()), 2);
}

#[test]
fn test_stats_number_format_rounds_like_python() {
    // f"{12.5:.0f}", f"{2.5:.0f}", f"{0.5:.0f}", f"{0.25:.1f}", f"{0.35:.1f}" in CPython.
    let got = [
        format!("{:.0}", 12.5),
        format!("{:.0}", 2.5),
        format!("{:.0}", 0.5),
        format!("{:.1}", 0.25),
        format!("{:.1}", 0.35),
    ];
    assert_eq!(got, ["12", "2", "0", "0.2", "0.3"]);
}
