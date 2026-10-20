//! pika_rtc.rs: Pika's video over a WebRTC data channel, against a real WebRTC peer on this Mac and the fake camera
//! (port of bridge/tests/test_pika_rtc.py).
//!
//! The peer plays the app: it offers one unordered `video` channel, sends the offer through the panel as the app
//! does, and reads frames as Shared/PikaDrive.swift does (8-byte stamp, then the JPEG). Both peers bind loopback, so
//! the tests need no network.

mod common;

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use eve_pika::pika::{self, Pika, PikaConfig, PikaOptions};
use eve_pika::pika_rtc::{self, PikaRtc};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::Notify;
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder, RTCIceGatheringState,
    RTCPeerConnectionState, RTCSessionDescription,
};

fn loopback() -> Vec<String> {
    vec!["127.0.0.1:0".to_string()]
}

fn with_rtc(p: Pika) -> Pika {
    p.set_rtc(Some(PikaRtc::with_udp_addrs(&p, "", loopback())));
    p
}

fn car_with(camera: &FakeCamera) -> Pika {
    with_rtc(Pika::new(PikaConfig { host: "127.0.0.1".into(), video_port: camera.port, ..Default::default() }))
}

#[derive(Default)]
struct Seen {
    frames: Mutex<Vec<(u64, Vec<u8>)>>,
    lines: Mutex<Vec<String>>,
    got: Notify,
    gathered: AtomicBool,
    gathered_now: Notify,
    channel_closed: AtomicBool,
    state: Mutex<String>,
}

struct PhoneHandler(Arc<Seen>);

#[async_trait::async_trait]
impl PeerConnectionEventHandler for PhoneHandler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.0.gathered.store(true, Ordering::SeqCst);
            self.0.gathered_now.notify_waiters();
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        *self.0.state.lock() = state.to_string();
    }
}

/// The app's side: one peer, one unordered `video` channel, frames and lines as they arrive.
struct Phone {
    pc: Arc<dyn PeerConnection>,
    seen: Arc<Seen>,
}

impl Phone {
    async fn new() -> Phone {
        Phone::bound(loopback()).await
    }

    async fn bound(udp_addrs: Vec<String>) -> Phone {
        let seen = Arc::new(Seen::default());
        let pc = PeerConnectionBuilder::new()
            .with_configuration(RTCConfigurationBuilder::new().build())
            .with_handler(Arc::new(PhoneHandler(seen.clone())))
            .with_udp_addrs(udp_addrs)
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
                match event {
                    DataChannelEvent::OnMessage(m) => {
                        if m.is_string {
                            s.lines.lock().push(String::from_utf8_lossy(&m.data).to_string());
                        } else {
                            s.frames.lock().push(pika_rtc::parse_message(&m.data).unwrap());
                        }
                        s.got.notify_waiters();
                    }
                    DataChannelEvent::OnClosing | DataChannelEvent::OnClose => {
                        s.channel_closed.store(true, Ordering::SeqCst)
                    }
                    _ => {}
                }
            }
            s.channel_closed.store(true, Ordering::SeqCst);
        });
        Phone { pc, seen }
    }

    async fn connect(&self, car: &Pika) -> Value {
        let offer = self.pc.create_offer(None).await.unwrap();
        self.pc.set_local_description(offer).await.unwrap();
        while !self.seen.gathered.load(Ordering::SeqCst) {
            let _ = tokio::time::timeout(Duration::from_millis(50), self.seen.gathered_now.notified()).await;
        }
        let sdp = self.pc.local_description().await.unwrap().sdp;
        let reply = car.panel(&json!({"action": "rtc", "sdp": sdp})).await;
        if reply["ok"] == json!(true) {
            let answer = RTCSessionDescription::answer(reply["sdp"].as_str().unwrap().to_string()).unwrap();
            self.pc.set_remote_description(answer).await.unwrap();
        }
        reply
    }

    fn frames(&self) -> Vec<(u64, Vec<u8>)> {
        self.seen.frames.lock().clone()
    }

    fn lines(&self) -> Vec<String> {
        self.seen.lines.lock().clone()
    }

    async fn wait_for(&self, frames: usize, lines: usize) {
        let end = Instant::now() + Duration::from_secs(10);
        while self.seen.frames.lock().len() < frames || self.seen.lines.lock().len() < lines {
            assert!(
                Instant::now() < end,
                "got {} frames and {:?} in 10 s",
                self.seen.frames.lock().len(),
                self.lines()
            );
            let _ = tokio::time::timeout(Duration::from_millis(50), self.seen.got.notified()).await;
        }
    }

    async fn close(&self) {
        let _ = self.pc.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_frames_arrive_byte_for_byte_with_the_macs_stamp() {
    let camera = FakeCamera::start(vec![jpeg(1), jpeg(2), jpeg(3)]).await;
    let car = car_with(&camera);
    let phone = Phone::new().await;
    let before = pika::stamp_ms() as u64;
    let reply = phone.connect(&car).await;
    assert_eq!(reply["ok"], json!(true));
    assert_eq!(reply["rtc"], json!(true));
    assert!(reply["sdp"].as_str().unwrap().starts_with("v=0"));
    phone.wait_for(5, 0).await;
    let after = pika::stamp_ms() as u64;
    assert!(phone.frames().iter().all(|(_, j)| [jpeg(1), jpeg(2), jpeg(3)].contains(j))); // never re-encoded
    assert!(phone.frames().iter().all(|(s, _)| before <= *s && *s <= after));
    assert!(car.video_active());
    assert_eq!(camera.viewers(), 1);
    assert!(car.rtc().unwrap().sent() >= 5);
    car.close("closed").await;
    phone.close().await;
}

/// The daemon's own bind: every IPv4 interface, as a phone on the home Wi-Fi reaches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: needs a non-loopback IPv4 interface (the home Wi-Fi)"]
async fn test_the_default_bind_reaches_a_phone_on_the_lan() {
    let camera = FakeCamera::start(vec![jpeg(1)]).await;
    let car = Pika::new(PikaConfig { host: "127.0.0.1".into(), video_port: camera.port, stun: String::new(), ..Default::default() });
    car.set_rtc(Some(PikaRtc::new(&car, "")));
    let phone = Phone::bound(vec!["0.0.0.0:0".to_string()]).await;
    assert_eq!(phone.connect(&car).await["ok"], json!(true));
    phone.wait_for(2, 0).await;
    car.close("closed").await;
    phone.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_the_slot_is_shared_with_the_tunnel_stream_and_the_newest_wins() {
    let camera = FakeCamera::start(vec![jpeg(1), jpeg(2)]).await;
    let car = car_with(&camera);
    let phone = Phone::new().await;
    phone.connect(&car).await;
    phone.wait_for(2, 0).await;
    let (status, got, tunnel) = read_parts(&car, 2).await; // the tunnel takes the camera
    assert_eq!(status, 200);
    assert!(got.len() >= 2);
    settle(0.2).await;
    let rtc = car.rtc().unwrap();
    assert!(!rtc.is_open()); // the peer and its upstream ended
    assert_eq!(camera.ended(), 1);
    let again = Phone::new().await;
    again.connect(&car).await; // and WebRTC takes it back
    again.wait_for(2, 0).await;
    settle(0.2).await;
    assert_eq!(camera.ended(), 2);
    assert!(rtc.is_open());
    assert!(tunnel.task.is_finished());
    drop(tunnel);
    car.close("closed").await;
    phone.close().await;
    again.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_close_ends_the_peer_and_the_camera_socket() {
    let camera = FakeCamera::start(vec![jpeg(1)]).await;
    let car = car_with(&camera);
    let phone = Phone::new().await;
    phone.connect(&car).await;
    phone.wait_for(2, 0).await;
    car.panel(&json!({"action": "close"})).await; // the Pika screen closed
    settle(0.3).await;
    assert!(!car.rtc().unwrap().is_open());
    assert!(!car.video_active());
    assert_eq!(camera.ended(), 1);
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let state = phone.seen.state.lock().clone();
        if ["closed", "failed", "disconnected"].contains(&state.as_str())
            || phone.seen.channel_closed.load(Ordering::SeqCst)
        {
            break;
        }
        assert!(Instant::now() < end, "the phone still sees {state}");
        settle(0.05).await;
    }
    phone.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_newer_offer_replaces_the_older_peer() {
    let camera = FakeCamera::start(vec![jpeg(1)]).await;
    let car = car_with(&camera);
    let (first, second) = (Phone::new().await, Phone::new().await);
    first.connect(&car).await;
    first.wait_for(1, 0).await;
    second.connect(&car).await;
    second.wait_for(2, 0).await;
    let n = first.frames().len();
    settle(0.3).await;
    assert_eq!(first.frames().len(), n); // the old peer gets nothing more
    car.close("closed").await;
    first.close().await;
    second.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_camera_that_is_off_is_a_line_on_the_channel() {
    let car = with_rtc(Pika::with_options(
        PikaConfig { host: "127.0.0.1".into(), ..Default::default() },
        PikaOptions { opener: refuse(), ..Default::default() },
    ));
    let phone = Phone::new().await;
    assert_eq!(phone.connect(&car).await["ok"], json!(true));
    phone.wait_for(0, 1).await;
    assert_eq!(phone.lines()[0], pika::VIDEO_UNREACHABLE_LINE);
    assert!(phone.frames().is_empty());
    car.close("closed").await;
    phone.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_bad_offer_or_no_webrtc_sends_the_app_to_the_tunnel() {
    let car = Pika::new(PikaConfig { host: "127.0.0.1".into(), ..Default::default() });
    let refused = json!({"ok": false, "rtc": false, "line": ""});
    assert_eq!(car.panel(&json!({"action": "rtc", "sdp": "v=0\r\n"})).await, refused);
    let car = with_rtc(car);
    let long = format!("v=0{}", "x".repeat(30000));
    for sdp in [Value::Null, json!(42), json!("not sdp"), json!(long)] {
        assert_eq!(car.panel(&json!({"action": "rtc", "sdp": sdp})).await, refused);
    }
    assert!(!car.rtc().unwrap().is_open());
    assert_eq!(car.panel(&json!({"action": "status"})).await["rtc"], json!(true));
    // an offer that is SDP-shaped but not one is refused too, and leaves no peer behind
    assert_eq!(car.panel(&json!({"action": "rtc", "sdp": "v=0\r\nnonsense"})).await, refused);
    assert!(!car.rtc().unwrap().is_open());
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn test_stats_are_logged_per_path_and_bad_ones_refused() {
    let log = Captured::default();
    let sink = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || sink.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let car = Pika::new(PikaConfig { host: "127.0.0.1".into(), ..Default::default() });
            let ok = json!({"action": "stats", "path": "webrtc", "n": 60, "p50": 41.4, "p95": 88, "fps": 11.2});
            assert_eq!(car.panel(&ok).await, json!({"ok": true, "line": ""}));
            let text = String::from_utf8_lossy(&log.0.lock()).to_string();
            assert!(text.contains("pika: video stats webrtc n=60 p50=41 p95=88 fps=11.2"), "{text}");
            let with = |k: &str, v: Value| {
                let mut b = ok.clone();
                b[k] = v;
                b
            };
            for bad in [
                with("path", json!("carrier pigeon")),
                with("p50", json!("fast")),
                with("n", json!(true)),
                with("fps", json!(1e9)),
            ] {
                assert_eq!(car.panel(&bad).await["ok"], json!(false));
            }
        });
    });
}

#[test]
fn test_the_config_turns_webrtc_off_and_checks_the_stun_server() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("pika.json");
    std::fs::write(&f, json!({"host": "elegoo-car.local"}).to_string()).unwrap();
    let cfg = pika::load(Some(&f)).unwrap();
    assert!(cfg.rtc);
    assert_eq!(cfg.stun, pika::DEFAULT_STUN);
    std::fs::write(&f, json!({"rtc": false, "stun": ""}).to_string()).unwrap();
    let cfg = pika::load(Some(&f)).unwrap();
    assert!(!cfg.rtc);
    assert_eq!(cfg.stun, "");
    std::fs::write(&f, json!({"rtc": "yes", "stun": "turn:evil.example:3478?transport=tcp"}).to_string()).unwrap();
    let cfg = pika::load(Some(&f)).unwrap();
    assert!(cfg.rtc);
    assert_eq!(cfg.stun, pika::DEFAULT_STUN);
    let on: HashMap<String, String> = [("EVE_PIKA".to_string(), "1".to_string())].into();
    std::fs::write(&f, json!({"rtc": false}).to_string()).unwrap();
    assert!(pika::make_pika(Some(&f), Some(&on)).unwrap().rtc().is_none());
    std::fs::write(&f, json!({}).to_string()).unwrap();
    let made = pika::make_pika(Some(&f), Some(&on)).unwrap();
    assert_eq!(made.rtc().unwrap().stun(), pika::DEFAULT_STUN);
}

#[test]
fn test_the_frame_message_is_stamp_then_jpeg() {
    let m = pika_rtc::frame_message(1760100000123, &jpeg(7));
    assert_eq!(&m[..8], &1760100000123u64.to_be_bytes());
    assert_eq!(&m[8..], &jpeg(7)[..]);
    assert_eq!(pika_rtc::parse_message(&m), Some((1760100000123, jpeg(7))));
}
