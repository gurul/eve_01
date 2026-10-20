//! pika.rs: the Pika car's hold-to-drive panel and its video, against a fake car on loopback
//! (port of bridge/tests/test_pika.py).

mod common;

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use common::*;
use eve_pika::pika::{self, Pika, PikaConfig, PikaOptions};
use futures::FutureExt;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

// The car repo's own examples (README "Wire protocol"), with eve's pulse length: the bytes must be these exactly.
const README_FORWARD: &[u8] = br#"{"H":"4","N":2,"D1":3,"D2":120,"T":800}"#;
const README_STOP: &[u8] = br#"{"H":"6","N":100}"#;

fn b(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

// ---- the wire ------------------------------------------------------------------------------------------------

#[test]
fn test_frames_are_the_car_repos_bytes_exactly() {
    assert_eq!(pika::frame(4, &[("N", 2), ("D1", 3), ("D2", 120), ("T", 800)]), README_FORWARD);
    assert_eq!(pika::stop_frame(6), README_STOP);
    assert_eq!(pika::drive_frame(4, 3, 120, pika::PULSE_MS), br#"{"H":"4","N":2,"D1":3,"D2":120,"T":450}"#);
    assert_eq!(pika::DIRS, [("left", 1), ("right", 2), ("forward", 3), ("back", 4)]); // tools/car.py DIR
    const { assert!(pika::PULSE_MS <= 450) }; // tools/safety.py MAX_PULSE_MS
    assert_eq!(
        pika::split_frames("{Heartbeat}{3_ok}{T_1"),
        (vec!["{Heartbeat}".to_string(), "{3_ok}".to_string()], "{T_1".to_string())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_held_drive_then_release_is_hello_drive_stop_byte_for_byte() {
    let car = FakeCar::start(false, false).await;
    let p = make(&car);
    assert_eq!(drive(&p, "forward", "h1").await, json!({"ok": true, "line": ""}));
    assert_eq!(p.panel(&json!({"action": "stop", "hold": "h1"})).await["ok"], json!(true));
    settle(0.05).await;
    assert_eq!(
        car.frames(0),
        vec![
            b(r#"{"H":"1","N":100}"#), // the hello
            b(r#"{"H":"2","N":2,"D1":3,"D2":120,"T":450}"#),
            b(r#"{"H":"3","N":100}"#),
        ]
    );
    assert_eq!(p.stops(), vec!["release"]);
    // a turn uses the turn speed; back the drive speed
    assert_eq!(drive(&p, "left", "h2").await["ok"], json!(true));
    assert_eq!(drive(&p, "back", "h2").await["ok"], json!(true));
    p.panel(&json!({"action": "stop", "hold": "h2"})).await;
    settle(0.05).await;
    assert_eq!(
        car.frames(0)[3..].to_vec(),
        vec![
            b(r#"{"H":"4","N":2,"D1":1,"D2":140,"T":450}"#),
            b(r#"{"H":"5","N":2,"D1":4,"D2":120,"T":450}"#),
            b(r#"{"H":"6","N":100}"#),
        ]
    );
    p.close("closed").await;
    car.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_beats_renew_the_timed_move_but_not_faster_than_resend() {
    let car = FakeCar::start(false, false).await;
    let p = make(&car);
    for _ in 0..4 {
        // four beats in one burst: one frame
        assert_eq!(drive(&p, "forward", "h").await["ok"], json!(true));
    }
    settle(pika::RESEND_SECS + 0.05).await;
    assert_eq!(drive(&p, "forward", "h").await["ok"], json!(true)); // a beat after RESEND_SECS: renewed
    settle(0.05).await;
    assert_eq!(car.drives(0).len(), 2);
    p.close("closed").await;
    car.close();
}

// ---- the stops -----------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_missed_heartbeat_stops_and_the_late_beat_never_restarts_it() {
    let car = FakeCar::start(false, false).await;
    let p = make_with(&car, 1, PikaOptions { heartbeat_secs: 0.15, ..opts() });
    assert_eq!(drive(&p, "forward", "h").await["ok"], json!(true));
    settle(0.35).await; // no beat for longer than the heartbeat
    assert_eq!(p.stops(), vec!["heartbeat"]);
    assert_eq!(car.frames(0).last().unwrap(), &b(r#"{"H":"3","N":100}"#));
    let late = drive(&p, "forward", "h").await;
    assert_eq!(late, json!({"ok": false, "stopped": true, "line": pika::LATE_LINE}));
    settle(0.05).await;
    assert_eq!(car.drives(0), vec![b(r#"{"H":"2","N":2,"D1":3,"D2":120,"T":450}"#)]);
    assert_eq!(drive(&p, "forward", "h2").await["ok"], json!(true)); // a new press drives again
    p.close("closed").await;
    car.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_beat_that_crossed_the_release_in_the_tunnel_is_refused() {
    let car = FakeCar::start(false, false).await;
    let p = make(&car);
    assert_eq!(drive(&p, "right", "h").await["ok"], json!(true));
    p.panel(&json!({"action": "stop", "hold": "h"})).await;
    assert_eq!(drive(&p, "right", "h").await["stopped"], json!(true));
    settle(0.05).await;
    assert_eq!(car.drives(0).len(), 1);
    p.close("closed").await;
    car.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_release_while_connecting_means_no_drive_frame_at_all() {
    let car = FakeCar::start(false, false).await;
    let (gate, gate_rx) = tokio::sync::watch::channel(false);
    let slow_open: pika::Opener = Arc::new(move |host: String, port: u16| {
        let mut rx = gate_rx.clone();
        async move {
            let _ = rx.wait_for(|open| *open).await;
            TcpStream::connect((host.as_str(), port)).await
        }
        .boxed()
    });
    let p = make_with(&car, 1, PikaOptions { opener: slow_open, ..opts() });
    let pending = {
        let p = p.clone();
        tokio::spawn(async move { drive(&p, "forward", "h").await })
    };
    settle(0.05).await;
    // answered at once, nothing to wait on
    assert_eq!(p.panel(&json!({"action": "stop", "hold": "h"})).await["ok"], json!(true));
    gate.send(true).unwrap();
    assert_eq!(pending.await.unwrap()["stopped"], json!(true));
    settle(0.05).await;
    assert!(car.drives(0).is_empty());
    // a stop never awaits anything: Pika::stop is a plain fn (checked by its type)
    let _never_awaits: fn(&Pika, &str) = Pika::stop;
    p.close("closed").await;
    car.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_car_hanging_up_ends_the_press() {
    let car = FakeCar::start(false, false).await;
    let p = make(&car);
    assert_eq!(drive(&p, "forward", "h").await["ok"], json!(true));
    car.hang_up(0); // the camera board stops the UNO itself on a disconnect
    settle(0.1).await;
    assert!(!p.connected());
    assert_eq!(drive(&p, "forward", "h").await, json!({"ok": false, "stopped": true, "line": pika::LOST_LINE}));
    assert_eq!(drive(&p, "forward", "h2").await["ok"], json!(true)); // a new press connects again
    assert_eq!(car.clients().len(), 2);
    p.close("closed").await;
    car.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_an_error_mid_drive_sends_a_stop() {
    let car = FakeCar::start(false, false).await;
    let broken = Arc::new(AtomicBool::new(false));
    let flag = broken.clone();
    let framer: pika::DriveFramer = Arc::new(move |tag, direction, speed| {
        if flag.load(Ordering::SeqCst) {
            return Err(io::Error::other("boom"));
        }
        Ok(pika::drive_frame(tag, direction, speed, pika::PULSE_MS))
    });
    let p = make_with(&car, 1, PikaOptions { drive_framer: framer, ..opts() });
    assert_eq!(drive(&p, "forward", "h").await["ok"], json!(true));
    broken.store(true, Ordering::SeqCst);
    settle(pika::RESEND_SECS + 0.02).await;
    let out = drive(&p, "left", "h").await;
    assert_eq!(out, json!({"ok": false, "stopped": true, "line": pika::LOST_LINE}));
    assert_eq!(p.stops(), vec!["error"]);
    settle(0.05).await;
    // tag 3 failed
    assert_eq!(
        car.frames(0)[1..].to_vec(),
        vec![b(r#"{"H":"2","N":2,"D1":3,"D2":120,"T":450}"#), b(r#"{"H":"4","N":100}"#)]
    );
    assert!(!p.connected());
    broken.store(false, Ordering::SeqCst);
    p.close("closed").await;
    car.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_close_stops_and_lets_go_of_the_slot() {
    let car = FakeCar::start(false, false).await;
    let p = make(&car);
    assert_eq!(drive(&p, "back", "h").await["ok"], json!(true));
    assert_eq!(p.panel(&json!({"action": "close", "hold": "h"})).await["ok"], json!(true));
    settle(0.1).await;
    assert_eq!(car.frames(0).last().unwrap(), &b(r#"{"H":"3","N":100}"#));
    assert_eq!(p.stops(), vec!["panel closed"]);
    assert_eq!(car.open(), vec![false]);
    assert!(!p.connected());
    car.close();
}

// ---- the one slot --------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_slot_is_let_go_after_idle() {
    let car = FakeCar::start(false, true).await;
    let p = make_with(&car, 1, PikaOptions { idle_secs: 0.2, ..opts() });
    assert_eq!(drive(&p, "forward", "h").await["ok"], json!(true));
    p.panel(&json!({"action": "stop", "hold": "h"})).await;
    settle(0.1).await;
    assert_eq!(car.open(), vec![true]);
    assert!(contains(&car.clients()[0], b"{Heartbeat}")); // eve answered the board's heartbeat
    settle(0.4).await;
    assert_eq!(car.open(), vec![false]);
    assert!(!p.connected());
    assert_eq!(p.stops(), vec!["release", "idle"]);
    car.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_busy_car_says_so_in_one_line_and_lets_go() {
    let car = FakeCar::start(true, false).await;
    let p = make(&car);
    let out = drive(&p, "forward", "h").await;
    assert_eq!(out, json!({"ok": false, "stopped": true, "line": pika::BUSY_LINE}));
    assert!(!pika::BUSY_LINE.contains('\n'));
    settle(0.1).await;
    assert_eq!(car.open(), vec![false]);
    assert!(!p.connected());
    assert!(car.drives(0).is_empty());
    car.close();
}

/// Review D11: the writer was set before the car answered the hello, so `connected` was True during the wait
/// and a second drive beat wrote a drive frame to a car that had not shown it was free.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_second_beat_during_the_hello_writes_no_drive_frame() {
    let car = FakeCar::start(true, false).await;
    let p = make(&car);
    let first = {
        let p = p.clone();
        tokio::spawn(async move { drive(&p, "forward", "h").await })
    };
    settle(0.1).await; // inside the 0.5 s hello wait
    assert!(!p.connected());
    let second = {
        let p = p.clone();
        tokio::spawn(async move { drive(&p, "forward", "h").await })
    };
    let outs = [first.await.unwrap(), second.await.unwrap()];
    assert!(outs.iter().all(|o| o["ok"] == json!(false)));
    settle(0.1).await;
    assert!(car.drives(0).is_empty());
    car.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_an_unreachable_car_says_so() {
    let p = Pika::with_options(
        PikaConfig { host: "127.0.0.1".into(), port: 9, ..Default::default() },
        PikaOptions { opener: refuse(), ..Default::default() },
    );
    assert_eq!(drive(&p, "forward", "h").await, json!({"ok": false, "stopped": true, "line": pika::UNREACHABLE_LINE}));
    assert_eq!(
        p.panel(&json!({"action": "status"})).await,
        json!({"ok": true, "connected": false, "rtc": false, "line": ""})
    );
    assert_eq!(p.panel(&json!({"action": "drive", "dir": "up", "hold": "x"})).await["ok"], json!(false));
    assert_eq!(p.panel(&json!({"action": "spin"})).await["ok"], json!(false));
    // the exact lines the app shows
    assert_eq!(
        p.panel(&json!({"action": "spin"})).await,
        json!({"ok": false, "line": "That isn't something Pika does."})
    );
    assert_eq!(
        p.panel(&json!({"action": "drive", "dir": "forward"})).await,
        json!({"ok": false, "line": "Which way?"})
    );
    // no action (or an empty one) is a status
    assert_eq!(p.panel(&json!({})).await["connected"], json!(false));
    assert_eq!(p.panel(&json!({"action": ""})).await["connected"], json!(false));
}

// ---- the switch and the config -------------------------------------------------------------------------------

#[test]
fn test_off_by_default_and_without_config() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("pika.json");
    let env = |v: &[(&str, &str)]| -> HashMap<String, String> {
        v.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    assert!(pika::make_pika(Some(&cfg), Some(&env(&[]))).is_none()); // off by default
    assert!(pika::make_pika(Some(&cfg), Some(&env(&[("EVE_PIKA", "1")]))).is_none()); // on, but no config
    std::fs::write(&cfg, json!({"host": "192.168.1.50"}).to_string()).unwrap();
    assert!(pika::make_pika(Some(&cfg), Some(&env(&[("EVE_PIKA", "0")]))).is_none());
    let made = pika::make_pika(Some(&cfg), Some(&env(&[("EVE_PIKA", "1")])));
    assert!(made.is_some());
    assert_eq!(made.unwrap().cfg(), &PikaConfig { host: "192.168.1.50".into(), ..Default::default() });
    std::fs::write(
        &cfg,
        json!({"host": "bad host!", "port": "x", "speed": 999, "turn_speed": 90, "video_port": 8081}).to_string(),
    )
    .unwrap();
    assert_eq!(pika::load(Some(&cfg)), Some(PikaConfig { turn_speed: 90, video_port: 8081, ..Default::default() }));
    std::fs::write(&cfg, "[]").unwrap();
    assert_eq!(pika::load(Some(&cfg)), None);
    // the switch reads Python's truthy strings, blanks around them ignored
    for on in ["1", "true", " Yes ", "ON"] {
        assert!(pika::enabled(Some(&env(&[("EVE_PIKA", on)]))), "{on}");
    }
    for off in ["", "0", "no", "2"] {
        assert!(!pika::enabled(Some(&env(&[("EVE_PIKA", off)]))), "{off}");
    }
    // a host is stripped; a float, a bool or a number out of range falls back
    std::fs::write(&cfg, r#"{"host": "  car.lan \n", "speed": 120.0, "port": true, "video_port": 0}"#).unwrap();
    assert_eq!(pika::load(Some(&cfg)), Some(PikaConfig { host: "car.lan".into(), ..Default::default() }));
    std::fs::write(&cfg, "not json").unwrap();
    assert_eq!(pika::load(Some(&cfg)), None);
}

// ---- the video, as the link door serves it -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_pika_is_listed_only_when_it_is_on() {
    // the listing itself is link_chat's; here: a panel made from a config sends nothing until the first drive
    const { assert!(Pika::AVAILABLE) };
    let car = FakeCar::start(false, false).await;
    let p = make(&car);
    settle(0.05).await;
    assert!(car.clients().is_empty());
    assert_eq!(p.panel(&json!({"action": "status"})).await["ok"], json!(true));
    assert!(car.clients().is_empty());
    car.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_video_is_relayed_one_viewer_at_a_time() {
    let camera = FakeCamera::start(vec![jpeg(1), jpeg(2), jpeg(3)]).await;
    let car = FakeCar::start(false, false).await;
    let p = make_with(&car, camera.port, opts());
    let (status, got, first) = read_parts(&p, 3).await;
    assert_eq!(status, 200);
    assert!(got.iter().all(|g| [jpeg(1), jpeg(2), jpeg(3)].contains(g)));
    let (status, got, second) = read_parts(&p, 2).await; // the newest viewer wins
    assert_eq!(status, 200);
    assert!(got.len() >= 2);
    settle(0.1).await;
    assert_eq!(camera.ended(), 1); // the first upstream was let go
    assert!(first.task.is_finished());
    p.panel(&json!({"action": "close"})).await; // the panel closed
    settle(0.1).await;
    assert_eq!(camera.ended(), 2);
    assert!(!p.video_active());
    drop(first);
    drop(second);
    car.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_app_hanging_up_lets_go_of_the_camera() {
    let camera = FakeCamera::start(vec![jpeg(1)]).await;
    let car = FakeCar::start(false, false).await;
    let p = make_with(&car, camera.port, opts());
    let (status, _, viewer) = read_parts(&p, 2).await;
    assert_eq!(status, 200);
    let task = viewer.task;
    drop(viewer.reader); // the app hung up
    tokio::time::timeout(std::time::Duration::from_secs(2), task).await.expect("stream ends").unwrap();
    settle(0.1).await;
    assert_eq!(camera.ended(), 1);
    assert!(!p.video_active());
    car.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_camera_that_is_off_is_one_plain_line() {
    let p = Pika::with_options(
        PikaConfig { host: "127.0.0.1".into(), ..Default::default() },
        PikaOptions { opener: refuse(), ..Default::default() },
    );
    let mut out: Vec<u8> = Vec::new();
    p.stream(&mut out).await;
    let text = String::from_utf8(out).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("HTTP/1.1 503 Service Unavailable\r\n"), "{head}");
    assert!(head.contains(&format!("Content-Length: {}", body.len())));
    assert_eq!(serde_json::from_str::<Value>(body).unwrap(), json!({"ok": false, "line": pika::VIDEO_UNREACHABLE_LINE}));
    // json.dumps's own spacing, as the Python daemon wrote it
    assert_eq!(body, format!(r#"{{"ok": false, "line": "{}"}}"#, pika::VIDEO_UNREACHABLE_LINE));
    assert!(!p.video_active());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_camera_that_answers_no_is_busy() {
    // anything but a 200 from the camera is another viewer holding it
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = s.read(&mut buf).await;
        let _ = tokio::io::AsyncWriteExt::write_all(&mut s, b"HTTP/1.1 503 Busy\r\n\r\n").await;
    });
    let p = Pika::new(PikaConfig { host: "127.0.0.1".into(), video_port: port, ..Default::default() });
    let mut frames = p.frames();
    assert_eq!(frames.next().await, Some(Err(pika::PikaError::new(pika::VIDEO_BUSY_LINE))));
    assert_eq!(frames.next().await, None);
}

#[test]
fn test_the_stream_head_is_the_pythons_bytes() {
    assert_eq!(
        pika::VIDEO_HEAD,
        &b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\nCache-Control: no-store\r\nX-Accel-Buffering: no\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"[..]
    );
}

#[test]
fn test_split_jpegs_keeps_a_partial_frame() {
    let (a, bb) = (jpeg(1), jpeg(2));
    let input = [&b"junk"[..], &a, b"--x", &bb[..10]].concat();
    let (found, rest) = pika::split_jpegs(&input);
    assert_eq!(found, vec![a.clone()]);
    assert_eq!(rest, bb[..10].to_vec());
    let (found, rest) = pika::split_jpegs(&[&rest[..], &bb[10..]].concat());
    assert_eq!(found, vec![bb.clone()]);
    assert_eq!(rest, Vec::<u8>::new());
    // no start marker: only the last byte is kept (it may be the first half of one)
    assert_eq!(pika::split_jpegs(b"abc\xff"), (vec![], b"\xff".to_vec()));
    let mut want = b"--frame\r\nContent-Type: image/jpeg\r\nContent-Length: 44\r\nX-Stamp: 1760100000123\r\n\r\n".to_vec();
    want.extend_from_slice(&a);
    want.extend_from_slice(b"\r\n");
    assert_eq!(pika::video_part(&a, Some(1760100000123)), want);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_car_is_dialled_over_ipv4_only() {
    // macOS spends 5 s on the mDNS AAAA lookup for elegoo-car.local, longer than CONNECT_SECS; IPv4 answers at once.
    let addrs = pika::resolve_ipv4("localhost", 100).await.unwrap();
    assert!(!addrs.is_empty());
    assert!(addrs.iter().all(|a| a.is_ipv4() && a.port() == 100), "{addrs:?}");
    // and the default opener reaches a car listening on IPv4 by name
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let stream = pika::open("localhost", port).await.unwrap();
    assert!(stream.peer_addr().unwrap().is_ipv4());
    // a name that does not resolve is a gaierror, as the Python log names it
    let err = pika::resolve_ipv4("no-such-host.invalid", 100).await.unwrap_err();
    assert_eq!(pika::py_error_name(&err), "gaierror");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_chunked_body_is_dechunked() {
    let raw = b"7\r\n\r\n--abc\r\n3;ext=1\r\nxyz\r\n0\r\n\r\n".to_vec();
    let mut body = pika::Body::new(&raw[..], true);
    assert_eq!(body.next().await.unwrap(), Some(b"\r\n--abc".to_vec()));
    assert_eq!(body.next().await.unwrap(), Some(b"xyz".to_vec()));
    assert_eq!(body.next().await.unwrap(), None);
    let mut plain = pika::Body::new(&b"abc"[..], false);
    assert_eq!(plain.next().await.unwrap(), Some(b"abc".to_vec()));
    assert_eq!(plain.next().await.unwrap(), None);
    let mut cut = pika::Body::new(&b"5\r\nab"[..], true);
    assert!(cut.next().await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_finished_dial_is_never_reused() {
    // Python makes a new dial once `_connecting.done()`: a dial whose caller gave up before reading its result must
    // not answer the next press with that stale result.
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let slow_refuse: pika::Opener = Arc::new(move |_host: String, _port: u16| {
        seen.fetch_add(1, Ordering::SeqCst);
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            Err::<TcpStream, _>(io::Error::from(io::ErrorKind::ConnectionRefused))
        }
        .boxed()
    });
    let p = Pika::with_options(
        PikaConfig { host: "127.0.0.1".into(), port: 9, ..Default::default() },
        PikaOptions { opener: slow_refuse, ..Default::default() },
    );
    let gave_up = {
        let p = p.clone();
        tokio::spawn(async move { drive(&p, "forward", "h").await })
    };
    settle(0.01).await;
    gave_up.abort(); // the app's request went away mid-dial
    settle(0.1).await; // the dial has finished; nobody read its result
    assert_eq!(drive(&p, "forward", "h2").await, json!({"ok": false, "stopped": true, "line": pika::UNREACHABLE_LINE}));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
