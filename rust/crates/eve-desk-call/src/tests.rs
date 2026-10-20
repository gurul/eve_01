//! Port of `bridge/tests/test_desk_call.py`: the board's hold-to-talk button runs the Mini App's push-to-talk call
//! on the desk. A fake chat, a fake voice (no OpenAI), a fake microphone that the test feeds, and a fake speaker
//! that records what it played.

use std::cell::RefCell;
use std::io::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use base64::Engine;
use eve_ears::{AudioBackend, BlockCallback, DeviceInfo, InputSpec, InputStream, StatusCallback};
use eve_phone_call::{BYTES_PER_SEC, SAMPLE_RATE};
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::test_support::*;
use crate::*;
use eve_phone_call::CallSocket;

fn b64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(s).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_hold_talk_release_is_one_message_to_the_chat_and_the_reply_plays_on_the_mac() {
    let (brain, voice) = (Brain::default_reply(), Voice::new());
    let rig = calls(Some(brain.clone()), Some(voice.clone()), None);

    rig.desk.press(true).await;
    let mic = last(&rig.made);
    assert!(mic.opened() && mic.device.as_deref() == Some("yeti"));
    // one second of the owner talking, from the mic thread
    for _ in 0..20 {
        mic.block(&[2u8, 0].repeat(1200));
    }
    sleep(0.05).await;
    rig.desk.press(false).await;
    assert!(mic.closed());

    let speakers = rig.speakers.clone();
    until(|| speakers.lock().first().is_some_and(|s| s.played() > 0), 5.0).await;
    assert_eq!(brain.heard(), vec!["what time is it"]);
    assert_eq!(*voice.transcribed.lock(), vec![BYTES_PER_SEC]); // the whole press, uploaded
    let spoken = voice.spoken.lock().clone();
    assert!(!spoken.is_empty() && spoken[0].contains("three") && !spoken[0].contains("**"));
    let sent = rig.sent.clone();
    until(|| rings(&sent).iter().any(|r| r == "speaking"), 5.0).await;
    assert_eq!(rings(&rig.sent)[..3], ["listening", "thinking", "speaking"]);

    rig.desk.stop().await;
    assert_eq!(rings(&rig.sent).last().map(String::as_str), Some("idle"));
    assert!(rig.speakers.lock()[0].closed.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_tap_is_not_words() {
    let (brain, voice) = (Brain::default_reply(), Voice::new());
    let rig = calls(Some(brain.clone()), Some(voice.clone()), None);
    rig.desk.press(true).await;
    last(&rig.made).block(&[0u8, 0].repeat(1200)); // 50 ms
    sleep(0.05).await;
    rig.desk.press(false).await;
    sleep(0.1).await;
    assert!(brain.heard().is_empty() && voice.transcribed.lock().is_empty());
    rig.desk.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_press_while_eve_talks_interrupts_it() {
    let (brain, voice) = (Brain::default_reply(), Voice::new());
    let rig = calls(Some(brain), Some(voice), None);
    rig.desk.press(true).await;
    for _ in 0..10 {
        last(&rig.made).block(&[2u8, 0].repeat(1200));
    }
    sleep(0.05).await;
    rig.desk.press(false).await;
    let speakers = rig.speakers.clone();
    until(|| speakers.lock().first().is_some_and(|s| s.played() > 0), 5.0).await;
    let flushes = rig.speakers.lock()[0].flushes();
    rig.desk.press(true).await; // same call, a second press
    until(|| speakers.lock()[0].flushes() > flushes, 5.0).await;
    assert!(rig.made.lock().len() == 2 && rig.speakers.lock().len() == 1);
    rig.desk.press(false).await;
    rig.desk.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_desk_waits_while_the_phone_is_on_a_call() {
    let (brain, voice) = (Brain::default_reply(), Voice::new());
    let rig = calls(Some(brain), Some(voice), Some(Arc::new(|| true)));
    rig.desk.press(true).await;
    assert!(rig.made.lock().is_empty() && !rig.desk.active() && rig.sent.lock().is_empty());
    rig.desk.press(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_without_the_chat_or_voice_the_button_does_nothing() {
    let rig = calls(None, None, None);
    rig.desk.press(true).await;
    rig.desk.press(false).await;
    assert!(rig.made.lock().is_empty() && rig.sent.lock().is_empty() && !rig.desk.active());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_call_stays_open_while_eve_is_still_thinking() {
    // A chat that has not answered yet (a slow tool): the ring says thinking, the desk keeps pinging, and the
    // call outlives QUIET_SECS instead of ending as "the phone went quiet".
    let (brain, voice) = (Brain::new(""), Voice::new());
    let rig = calls(Some(brain.clone()), Some(voice), None);
    rig.desk.limits().set_quiet_secs(0.3);
    rig.desk.set_ping_secs(0.1);
    rig.desk.press(true).await;
    for _ in 0..10 {
        last(&rig.made).block(&[2u8, 0].repeat(1200));
    }
    sleep(0.05).await;
    rig.desk.press(false).await;
    let b = brain.clone();
    until(|| b.heard() == vec!["what time is it"], 5.0).await;
    sleep(1.0).await;
    assert!(rig.desk.active() && rings(&rig.sent).last().map(String::as_str) == Some("thinking"));
    rig.desk.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_once_eve_is_done_the_call_ends_when_quiet() {
    let rig = calls(Some(Brain::default_reply()), Some(Voice::new()), None);
    rig.desk.limits().set_quiet_secs(0.3);
    rig.desk.set_ping_secs(0.1);
    rig.desk.press(true).await;
    for _ in 0..10 {
        last(&rig.made).block(&[2u8, 0].repeat(1200));
    }
    sleep(0.05).await;
    rig.desk.press(false).await;
    let desk = rig.desk.clone();
    until(|| !desk.active(), 5.0).await;
    assert_eq!(rings(&rig.sent).last().map(String::as_str), Some("idle"));
}

// ---- the head follows whoever is talking on a desk call (follow.py) ---------------------------------

/// 2026-09-27: "Eve doesn't follow person anymore". Voice had moved to the Voice PE's button, and the call's states
/// went only to the ring, so the follower (which needs a conversation phase) never woke.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_desk_call_is_a_conversation_to_the_head_follower() {
    let phases: Arc<Mutex<Vec<String>>> = Arc::default();
    let p = phases.clone();
    let on_phase: Phase = Arc::new(move |phase: String| {
        p.lock().push(phase);
        Box::pin(async { Ok(()) })
    });
    let (send, _sent) = recorder();
    let (mic_factory, made) = mic_factory();
    let (speaker_factory, _speakers) = speaker_factory();
    let desk = DeskCalls::new(
        Some(Brain::default_reply()),
        Some(Voice::new()),
        send,
        DeskOptions { mic_factory, speaker_factory, on_phase: Some(on_phase), ..DeskOptions::default() },
    );
    desk.limits().set_quiet_secs(0.3);
    desk.set_ping_secs(0.1);
    desk.press(true).await;
    assert_eq!(*phases.lock(), vec!["listening"]);
    for _ in 0..10 {
        last(&made).block(&[2u8, 0].repeat(1200));
    }
    sleep(0.05).await;
    desk.press(false).await;
    let d = desk.clone();
    until(|| !d.active(), 5.0).await;
    let phases = phases.lock().clone();
    assert_eq!(phases[..3], ["listening", "thinking", "speaking"]);
    // after the reply the ring goes quiet, but the person is still there until the call ends
    assert_eq!(phases[phases.len() - 2..], ["listening", "idle"]);
    assert!(!phases[..phases.len() - 1].iter().any(|p| p == "idle"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_during_a_desk_call_eve_turns_to_the_face_it_sees() {
    use eve_follow::{Face, FollowerOptions, SpeakerFollower};

    let t = Arc::new(Mutex::new(100.0f64));
    let looks: Arc<Mutex<Vec<Value>>> = Arc::default();
    let l = looks.clone();
    let board: eve_follow::Sender = Arc::new(move |obj: Value| {
        if obj.get("cmd").and_then(Value::as_str) == Some("look") {
            l.lock().push(obj);
        }
        Box::pin(async { true })
    });
    let t2 = t.clone();
    let follower = Arc::new(SpeakerFollower::with(
        board.clone(),
        FollowerOptions { clock: Arc::new(move || *t2.lock()), ..FollowerOptions::default() },
    ));
    let to_board = board.clone();
    let send: BoardSend = Arc::new(move |obj: Value| {
        let sent = to_board(obj);
        Box::pin(async move {
            sent.await;
            Ok(())
        })
    });
    let f = follower.clone();
    let on_phase: Phase = Arc::new(move |phase: String| {
        let f = f.clone();
        Box::pin(async move {
            f.on_phase(&phase).await;
            Ok(())
        })
    });
    let (mic_factory, _made) = mic_factory();
    let (speaker_factory, _speakers) = speaker_factory();
    let desk = DeskCalls::new(
        Some(Brain::default_reply()),
        Some(Voice::new()),
        send,
        DeskOptions { mic_factory, speaker_factory, on_phase: Some(on_phase), ..DeskOptions::default() },
    );
    desk.press(true).await;
    assert!(follower.active());
    for _ in 0..12 {
        // the owner sits well to eve's left of where the head points
        *t.lock() += 0.25;
        follower.on_faces(&[Face::new(-60, 10, 22, 90)], Some(0.0), Some(45.0)).await;
    }
    {
        let looks = looks.lock();
        assert!(!looks.is_empty() && looks[0]["yaw"].as_f64().unwrap() < -10.0);
    }
    desk.stop().await;
    assert!(!follower.active() && follower.phase() == "idle");
}

// ---- eve's voice on the Voice PE (BoardSpeaker) ---------------------------------------------------

struct FakeClock {
    t: Mutex<f64>,
    slept: Mutex<Vec<f64>>,
}

fn board() -> (BoardSpeaker, Sent, Arc<FakeClock>) {
    let clock = Arc::new(FakeClock { t: Mutex::new(100.0), slept: Mutex::new(Vec::new()) });
    let (send, lines) = recorder();
    let (c1, c2) = (clock.clone(), clock.clone());
    let spk = BoardSpeaker::with_clock(
        send,
        Arc::new(move || *c1.t.lock()),
        Arc::new(move |secs| {
            c2.slept.lock().push(secs);
            *c2.t.lock() += secs;
            Box::pin(async {})
        }),
    );
    (spk, lines, clock)
}

fn pieces(lines: &Sent) -> Vec<Vec<u8>> {
    lines.lock().iter().filter(|l| l["cmd"] == "pcm").map(|l| b64(l["d"].as_str().unwrap())).collect()
}

#[tokio::test]
async fn test_the_voice_goes_to_the_board_in_decodable_pieces() {
    let (spk, lines, _clock) = board();
    let pcm: Vec<u8> = (0..=255u8).collect::<Vec<u8>>().repeat(40); // 10240 bytes: 4800 + 4800 + 640
    spk.play_pcm(&pcm).await.unwrap();
    let got = pieces(&lines);
    assert_eq!(got.iter().map(Vec::len).collect::<Vec<_>>(), vec![4800, 4800, 640]);
    assert_eq!(got.concat(), pcm);
    assert!(lines.lock().iter().all(|l| l["cmd"] == "pcm"));
}

#[tokio::test]
async fn test_an_odd_byte_never_reaches_the_board_and_the_samples_stay_aligned() {
    // The range stopped at an even length but the last slice ran to i + PCM_PIECE_BYTES, so a 4803-byte reply
    // sent a 3-byte piece and every int16 after it on the board was split across two samples
    let (spk, lines, _clock) = board();
    let mut first: Vec<u8> = (0..200u8).collect::<Vec<u8>>().repeat(24);
    first.extend_from_slice(&[1, 2, 3]); // 4803 bytes, then 3
    let second = vec![4u8, 5, 6];
    spk.play_pcm(&first).await.unwrap();
    spk.play_pcm(&second).await.unwrap();
    let got = pieces(&lines);
    assert!(got.iter().all(|p| p.len() % 2 == 0), "{:?}", got.iter().map(Vec::len).collect::<Vec<_>>());
    assert_eq!(got.concat(), [first, second].concat()); // the odd byte waited for its pair
    spk.play_pcm(&[7]).await.unwrap(); // held: half a sample
    spk.flush_board().await.unwrap(); // a flush drops a held byte with the rest
    spk.play_pcm(&[8, 9]).await.unwrap();
    let last_line = lines.lock().last().cloned().unwrap();
    assert_eq!(b64(last_line["d"].as_str().unwrap()), vec![8, 9]);
}

#[tokio::test]
async fn test_the_daemon_stays_at_most_three_seconds_ahead_of_the_board() {
    let (spk, lines, clock) = board();
    let ten_secs = vec![0u8; 2 * SAMPLE_RATE as usize * 10];
    spk.play_pcm(&ten_secs).await.unwrap();
    assert_eq!(lines.lock().len(), 100);
    assert!(spk.ahead() <= LEAD_SECS + 1e-9);
    // 10 s sent with a 3 s lead: about 7 s of waiting, never a burst the 12 s board buffer cannot hold
    let slept: f64 = clock.slept.lock().iter().sum();
    assert!((6.8..=7.1).contains(&slept), "{slept}");
}

#[tokio::test]
async fn test_after_a_pause_the_board_has_played_out_and_a_new_burst_starts() {
    let (spk, _lines, clock) = board();
    spk.play_pcm(&vec![0u8; 2 * SAMPLE_RATE as usize]).await.unwrap(); // 1 s
    *clock.t.lock() += 5.0; // the board played it out long ago
    assert_eq!(spk.ahead(), 0.0);
    spk.play_pcm(&vec![0u8; 2 * SAMPLE_RATE as usize]).await.unwrap();
    assert!(clock.slept.lock().is_empty() && (spk.ahead() - 1.0).abs() < 1e-9);
}

#[tokio::test]
async fn test_a_flush_empties_the_board() {
    let (spk, lines, _clock) = board();
    spk.play_pcm(&vec![0u8; 2 * 4800]).await.unwrap();
    spk.flush_board().await.unwrap();
    assert_eq!(lines.lock().last().cloned().unwrap(), json!({"cmd": "pcm_flush"}));
    assert_eq!(spk.ahead(), 0.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_an_interrupt_on_a_board_call_flushes_the_board() {
    let (send, sent) = recorder();
    let (mic_factory, made) = mic_factory();
    let board_send = send.clone();
    let desk = DeskCalls::new(
        Some(Brain::default_reply()),
        Some(Voice::new()),
        send,
        DeskOptions {
            mic_factory,
            speaker_factory: Arc::new(move || Arc::new(BoardSpeaker::new(board_send.clone())) as Arc<dyn DeskSpeaker>),
            ..DeskOptions::default()
        },
    );
    desk.press(true).await;
    for _ in 0..10 {
        last(&made).block(&[2u8, 0].repeat(1200));
    }
    sleep(0.05).await;
    desk.press(false).await;
    let s = sent.clone();
    until(|| s.lock().iter().any(|m| m["cmd"] == "pcm"), 5.0).await;
    desk.press(true).await;
    until(|| s.lock().contains(&json!({"cmd": "pcm_flush"})), 5.0).await;
    desk.press(false).await;
    desk.stop().await;
}

// -- the microphone: a device table gone stale is refreshed once (live 2026-09-28 15:24, a JBL joined) --

thread_local! {
    static CAPTURED: RefCell<Option<Arc<Mutex<Vec<u8>>>>> = const { RefCell::new(None) };
}

struct ThreadBuf;

impl std::io::Write for ThreadBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        CAPTURED.with(|c| {
            if let Some(b) = c.borrow().as_ref() {
                b.lock().extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `caplog`: what this thread logged while `f` ran.
fn capture<R>(f: impl FnOnce() -> R) -> (R, String) {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(|| ThreadBuf)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
    let buf = Arc::new(Mutex::new(Vec::new()));
    CAPTURED.with(|c| *c.borrow_mut() = Some(buf.clone()));
    let r = f();
    CAPTURED.with(|c| *c.borrow_mut() = None);
    let _ = std::io::stdout().flush();
    let text = String::from_utf8_lossy(&buf.lock()).into_owned();
    (r, text)
}

#[derive(Default)]
struct FakeSd {
    calls: Mutex<Vec<&'static str>>,
    started: AtomicUsize,
    fails: AtomicUsize,
}

struct OwnedStream(Arc<FakeSd>);

impl InputStream for OwnedStream {
    fn start(&mut self) -> Result<(), String> {
        self.0.started.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn stop(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn close(&mut self) -> Result<(), String> {
        Ok(())
    }
}

struct SdHandle(Arc<FakeSd>);

impl AudioBackend for SdHandle {
    fn available(&self) -> Result<(), String> {
        Ok(())
    }
    fn query_devices(&self) -> Result<Vec<DeviceInfo>, String> {
        Ok(vec![DeviceInfo { name: "Yeti Stereo Microphone".into(), max_input_channels: 2 }])
    }
    fn device_name(&self, _device: Option<usize>) -> Result<String, String> {
        Ok("Yeti Stereo Microphone".into())
    }
    fn make_input(
        &self,
        spec: InputSpec,
        _on_block: BlockCallback,
        _on_status: StatusCallback,
    ) -> Result<Box<dyn InputStream>, String> {
        assert_eq!((spec.sample_rate, spec.block_frames), (24000, 1200));
        self.0.calls.lock().push("open");
        if self.0.fails.load(Ordering::SeqCst) > 0 {
            self.0.fails.fetch_sub(1, Ordering::SeqCst);
            return Err("Error opening RawInputStream: Internal PortAudio error [PaErrorCode -9986]".into());
        }
        Ok(Box::new(OwnedStream(self.0.clone())))
    }
    fn refresh(&self) -> Result<(), String> {
        let mut calls = self.0.calls.lock();
        calls.push("terminate");
        calls.push("initialize");
        Ok(())
    }
}

fn fake_sounddevice(fail_times: usize) -> Arc<FakeSd> {
    let sd = Arc::new(FakeSd::default());
    sd.fails.store(fail_times, Ordering::SeqCst);
    sd
}

#[test]
fn test_a_stale_audio_device_table_is_refreshed_and_the_mic_opens() {
    let sd = fake_sounddevice(1);
    let mut mic = Mic::with_backend(None, Arc::new(|_b| {}), Arc::new(SdHandle(sd.clone())));
    let (opened, text) = capture(|| mic.open());
    assert!(opened);
    assert_eq!(*sd.calls.lock(), vec!["open", "terminate", "initialize", "open"]);
    assert_eq!(sd.started.load(Ordering::SeqCst), 1);
    assert!(text.contains("refreshing the audio device list") && text.contains("microphone Yeti"), "{text}");
    mic.close();
    assert!(!mic.is_open());
}

#[test]
fn test_a_mic_that_fails_after_the_refresh_is_a_clean_false() {
    let sd = fake_sounddevice(2);
    let mut mic = Mic::with_backend(None, Arc::new(|_b| {}), Arc::new(SdHandle(sd.clone())));
    let (opened, text) = capture(|| mic.open());
    assert!(!opened);
    assert_eq!(*sd.calls.lock(), vec!["open", "terminate", "initialize", "open"]);
    assert!(text.contains("could not open the microphone"), "{text}");
}

// ---- Rust-only checks of the parity details ----

#[test]
fn the_mic_hands_blocks_to_the_call_as_int16_bytes_and_finds_its_device() {
    struct Feeding(Arc<FakeSd>, Mutex<Option<InputSpec>>);
    impl AudioBackend for Feeding {
        fn available(&self) -> Result<(), String> {
            Ok(())
        }
        fn query_devices(&self) -> Result<Vec<DeviceInfo>, String> {
            Ok(vec![
                DeviceInfo { name: "Studio Display Speakers".into(), max_input_channels: 0 },
                DeviceInfo { name: "Yeti Stereo Microphone".into(), max_input_channels: 2 },
            ])
        }
        fn device_name(&self, device: Option<usize>) -> Result<String, String> {
            Ok(format!("device {device:?}"))
        }
        fn make_input(
            &self,
            spec: InputSpec,
            mut on_block: BlockCallback,
            _on_status: StatusCallback,
        ) -> Result<Box<dyn InputStream>, String> {
            *self.1.lock() = Some(spec);
            on_block(&[1, -2, 300]);
            Ok(Box::new(OwnedStream(self.0.clone())))
        }
        fn refresh(&self) -> Result<(), String> {
            Ok(())
        }
    }
    let backend = Arc::new(Feeding(Arc::new(FakeSd::default()), Mutex::new(None)));
    let got: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let g = got.clone();
    let mut mic = Mic::with_backend(Some("YETI".into()), Arc::new(move |b| g.lock().push(b)), backend.clone());
    let (opened, text) = capture(|| mic.open());
    assert!(opened && mic.open()); // a second open is a no-op
    assert_eq!(backend.1.lock().unwrap().device, Some(1));
    assert!(text.contains("desk call: microphone device Some(1)"), "{text}");
    let expect: Vec<u8> = [1i16, -2, 300].iter().flat_map(|s| s.to_ne_bytes()).collect();
    assert_eq!(*got.lock(), vec![expect]);
    mic.close();
    mic.close(); // closing twice is harmless
}

#[tokio::test]
async fn the_link_maps_the_calls_messages_to_the_ring_and_the_speaker() {
    let (send, sent) = recorder();
    let spk = Arc::new(FakeSpeaker::default());
    let link = DeskLink::new(send, spk.clone(), None);
    link.send_json(&json!({"type": "state", "state": "thinking"})).await.unwrap();
    link.send_json(&json!({"type": "state", "state": "thinking"})).await.unwrap(); // unchanged: not resent
    link.send_json(&json!({"type": "state", "state": "connected", "note": "hi"})).await.unwrap();
    link.send_json(&json!({"type": "state"})).await.unwrap();
    link.send_json(&json!({"type": "flush"})).await.unwrap();
    link.send_json(&json!({"type": "heard", "text": "hello"})).await.unwrap();
    link.send_bytes(&[1, 2]).await.unwrap();
    assert_eq!(rings(&sent), vec!["thinking", "idle"]);
    assert_eq!(link.phase(), "listening");
    assert_eq!((spk.flushes(), spk.played()), (1, 2));
    link.talk();
    link.audio(vec![9, 9]);
    assert_eq!(link.recv().await.unwrap(), (eve_phone_call::OP_TEXT, br#"{"type": "talk"}"#.to_vec()));
    assert_eq!(link.recv().await.unwrap(), (eve_phone_call::OP_BIN, vec![9, 9]));
    link.ping();
    link.close(1000).await;
    assert!(link.closed());
    assert!(link.recv().await.is_err()); // closed: the queued ping is not read
    assert!(link.send_bytes(&[1, 2]).await.is_err() && link.send_json(&json!({"type": "flush"})).await.is_err());
    assert_eq!(ring_state("speaking"), "speaking");
    assert_eq!(ring_state("listening"), "idle");
}

#[tokio::test]
async fn a_failing_phase_hook_or_board_never_stops_the_link() {
    let send: BoardSend = Arc::new(|_obj| Box::pin(async { Err(anyhow::anyhow!("serial link down")) }));
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let on_phase: Phase = Arc::new(move |_p| {
        c.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(anyhow::anyhow!("head unplugged")) })
    });
    let link = DeskLink::new(send, Arc::new(FakeSpeaker::default()), Some(on_phase));
    link.set_ring("speaking").await;
    link.set_ring("idle").await;
    assert_eq!((link.ring(), link.phase(), calls.load(Ordering::SeqCst)), ("idle".into(), "listening".into(), 2));
}

#[test]
fn the_mac_speaker_plays_in_order_and_a_flush_drops_what_is_queued() {
    use crate::audio_out::RawOutput;
    struct Out(Arc<Mutex<Vec<Vec<u8>>>>, Arc<std::sync::Barrier>, Arc<AtomicUsize>);
    impl RawOutput for Out {
        fn write(&mut self, pcm: &[u8]) -> Result<(), String> {
            if pcm == [0xFF, 0xFF] {
                self.1.wait(); // the test queues behind this write, then flushes
                self.1.wait();
            }
            self.0.lock().push(pcm.to_vec());
            Ok(())
        }
        fn stop_close(&mut self) {
            self.2.fetch_add(1, Ordering::SeqCst);
        }
    }
    let written: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let stopped = Arc::new(AtomicUsize::new(0));
    let (w, b, s) = (written.clone(), barrier.clone(), stopped.clone());
    let spk = Speaker::with_output(Arc::new(move |rate| {
        assert_eq!(rate, 24000);
        Ok(Box::new(Out(w.clone(), b.clone(), s.clone())) as Box<dyn RawOutput>)
    }));
    spk.play_pcm(&[1, 0]);
    spk.play_pcm(&[0xFF, 0xFF]);
    barrier.wait(); // the player is inside the blocking write
    spk.play_pcm(&[2, 0]);
    spk.flush_now();
    spk.play_pcm(&[3, 0]);
    barrier.wait();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while written.lock().len() < 3 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    spk.close_now(); // a close is a flush, then the end
    while stopped.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(stopped.load(Ordering::SeqCst), 1);
    assert_eq!(*written.lock(), vec![vec![1, 0], vec![0xFF, 0xFF], vec![3, 0]]);
}

#[test]
fn the_resampler_keeps_the_rate_and_shape() {
    use crate::audio_out::{int16_to_float, Resampler};
    let mut same = Resampler::new(24000, 24000);
    let mut out = Vec::new();
    same.push(&[0.5, -0.5], &mut out);
    assert_eq!(out, vec![0.5, -0.5]);
    let mut up = Resampler::new(24000, 48000);
    let mut out = Vec::new();
    let input: Vec<f32> = (0..2400).map(|i| (i as f32) / 2400.0).collect();
    up.push(&input[..1000], &mut out);
    up.push(&input[1000..], &mut out);
    assert_eq!(out.len(), 4800);
    assert!(out.windows(2).all(|w| w[1] >= w[0]));
    assert_eq!(int16_to_float(i16::MIN), -1.0);
    assert_eq!(int16_to_float(16384), 0.5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_desk_call_is_not_active_the_moment_stop_returns() {
    // test_desk_call_lean's P3 trace reads `active` straight after `stop()`, with no turn of the loop between.
    for _ in 0..20 {
        let rig = calls(Some(Brain::new("")), Some(Voice::new()), None);
        rig.desk.press(true).await;
        assert!(rig.desk.active());
        rig.desk.stop().await;
        assert!(!rig.desk.active() && !rig.desk.held());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_board_that_fails_a_flush_ends_the_call() {
    // Python: the flush's error is not Closed, so it left Call.run and the desk call ended ("ended (?)").
    let (rec, sent) = recorder();
    let failing: BoardSend = Arc::new(move |obj: Value| {
        let flush = obj == json!({"cmd": "pcm_flush"});
        let r = rec(obj);
        Box::pin(async move {
            r.await?;
            if flush {
                anyhow::bail!("serial link down");
            }
            Ok(())
        })
    });
    let (mic_factory, made) = mic_factory();
    let board_send = failing.clone();
    let desk = DeskCalls::new(
        Some(Brain::default_reply()),
        Some(Voice::new()),
        failing,
        DeskOptions {
            mic_factory,
            speaker_factory: Arc::new(move || Arc::new(BoardSpeaker::new(board_send.clone())) as Arc<dyn DeskSpeaker>),
            ..DeskOptions::default()
        },
    );
    desk.press(true).await;
    // the first press's interrupt already flushes the board, and that flush fails
    let d = desk.clone();
    until(|| !d.active(), 5.0).await;
    assert!(desk.link().unwrap().failed() && desk.link().unwrap().closed());
    assert!(sent.lock().contains(&json!({"cmd": "pcm_flush"})));
    assert_eq!(rings(&sent).last().map(String::as_str), Some("idle"));
    assert!(!desk.held() && made.lock().iter().all(|m| m.closed()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ring_lines_reach_the_board_in_the_order_the_ring_changed() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (rec, sent) = recorder();
    let g = gate.clone();
    let send: BoardSend = Arc::new(move |obj: Value| {
        let slow = obj == json!({"cmd": "agent", "state": "listening"});
        let (g, rec) = (g.clone(), rec.clone());
        Box::pin(async move {
            if slow {
                let _ = g.acquire().await; // the serial link is busy with the first line
            }
            rec(obj).await // recorded as it lands on the board
        })
    });
    let link = DeskLink::new(send, Arc::new(FakeSpeaker::default()), None);
    let l = link.clone();
    let first = tokio::spawn(async move { l.set_ring("listening").await });
    settle().await;
    let l = link.clone();
    let second = tokio::spawn(async move { l.set_ring("thinking").await });
    settle().await;
    gate.add_permits(1);
    first.await.unwrap();
    second.await.unwrap();
    assert_eq!(link.ring(), "thinking");
    // the second change waited for the first line to land
    assert_eq!(rings(&sent), vec!["listening", "thinking"]);
}
