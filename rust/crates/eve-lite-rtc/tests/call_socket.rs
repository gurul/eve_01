//! lite_rtc.RtcCallSocket and InboundAudio: a push-to-talk call on the web app's `call` data channel and the peer's
//! audio, as the WebSocket PhoneCalls drives (port of bridge/tests/test_rtc_call_socket.py).
//!
//! The channel is the fake of the link tests; the microphone is fed the way the peer's pump feeds it
//! (`RtcCallSocket::audio`, after InboundAudio); eve's voice is a real VoiceTrack, taken a frame at a time as the
//! sender would. The last tests run the unchanged PhoneCalls over it with test_phone_call.py's fake chat and voice.
//!
//! Python fed InboundAudio aiortc's decoded 48 kHz stereo frames; here the page's microphone arrives as the Opus
//! packets a browser sends (48 kHz stereo, encoded by libopus), which is what the peer's pump hands it.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_lite_rtc::opus::{Encoder, APPLICATION_VOIP};
use eve_lite_rtc::{InboundAudio, RtcCallSocket, VoiceTrack, BLOCK_BYTES, MAX_CONTROL_CHARS, POSTROLL_SECS};
use eve_lite_rtc::{PREROLL_BYTES, SAMPLE_RATE, VOICE_FRAME_BYTES};
use eve_phone_call::{CallSocket, PhoneCalls, OP_BIN, OP_TEXT};

const RATE: usize = SAMPLE_RATE as usize;

/// What the call read: text as JSON (or as the page sent it, when it is not JSON), binary as bytes.
#[derive(Debug, Clone, PartialEq)]
enum Got {
    Json(Value),
    Text(Vec<u8>),
    Bin(Vec<u8>),
}

impl Got {
    fn kind(&self) -> String {
        match self {
            Got::Json(v) => v["type"].as_str().unwrap_or("").to_string(),
            _ => String::new(),
        }
    }
}

fn read(op: u8, data: Vec<u8>) -> Got {
    if op == OP_TEXT {
        match serde_json::from_slice(&data) {
            Ok(v) => Got::Json(v),
            Err(_) => Got::Text(data), // passed on as the page sent it
        }
    } else {
        assert_eq!(op, OP_BIN);
        Got::Bin(data)
    }
}

/// What the call would read now, until nothing comes for `secs`.
async fn received(sock: &RtcCallSocket, secs: f64) -> Vec<Got> {
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs_f64(secs), sock.recv()).await {
            Err(_) => return out,
            Ok(Ok((op, data))) => out.push(read(op, data)),
            Ok(Err(e)) => panic!("the socket closed: {e:?}"),
        }
    }
}

async fn opened(ch: &Arc<FakeChannel>, voice: Arc<VoiceTrack>, on_close: Option<eve_lite_rtc::call::OnClose>, postroll: f64) -> RtcCallSocket {
    let sock = RtcCallSocket::with(ch.clone(), voice, SECRET, on_close, postroll);
    let (op, _hello) = sock.recv().await.unwrap();
    assert_eq!(op, OP_TEXT);
    sock
}

/// 20 ms of a tone as a browser sends the page's microphone: 48 kHz stereo, one Opus packet.
struct Mic {
    enc: Encoder,
}

impl Mic {
    fn new() -> Mic {
        Mic { enc: Encoder::new(48000, 2, APPLICATION_VOIP).unwrap() }
    }

    fn packet(&mut self, freq: f64, start: usize) -> Vec<u8> {
        let mut pcm = Vec::with_capacity(1920);
        for i in 0..960 {
            let t = (i + start) as f64 / 48000.0;
            let v = ((2.0 * std::f64::consts::PI * freq * t).sin() * 12000.0) as i16;
            pcm.push(v);
            pcm.push(v);
        }
        self.enc.encode(&pcm).unwrap()
    }
}

fn page_got(ch: &FakeChannel) -> Vec<Value> {
    ch.got()
}

// ---- the hello and the microphone ------------------------------------------------------------------------------

#[tokio::test]
async fn test_the_hello_is_built_here_and_comes_first() {
    let ch = FakeChannel::new("call");
    let sock = RtcCallSocket::new(ch.clone(), Arc::new(VoiceTrack::new()), SECRET);
    ch.page(json!({"type": "ping"})); // sent before the call read
    let (op, data) = sock.recv().await.unwrap();
    assert!(op == OP_TEXT && serde_json::from_slice::<Value>(&data).unwrap() == json!({"token": SECRET, "can": []}));
    assert_eq!(data, br#"{"token":"peer-secret-CANARY-NOT-A-KEY-7f3c","can":[]}"#.to_vec());
    assert_eq!(received(&sock, 0.05).await, vec![Got::Json(json!({"type": "ping"}))]);
}

#[test]
fn test_a_48k_stereo_microphone_becomes_24k_mono_16_bit() {
    let mut inbound = InboundAudio::new();
    let mut mic = Mic::new();
    let mut out = Vec::new();
    for i in 0..50 {
        out.extend(inbound.convert(&mic.packet(1000.0, i * 960)).unwrap()); // one second
    }
    assert!(out.len() % 2 == 0 && (out.len() as i64 - 2 * RATE as i64).abs() <= 64); // 24,000 samples
    assert!((dominant_hz(&out[4800..], SAMPLE_RATE) - 1000.0).abs() <= 10.0); // the tone, not a copy of it
    let level = i16s(&out[4800..]).into_iter().map(|s| (s as i32).abs()).max().unwrap();
    assert!((10000..=13000).contains(&level), "{level}"); // mono, not a sum of channels
    // another codec's stream mid-call (8 kHz mono): still read
    let mut other = Encoder::new(8000, 1, APPLICATION_VOIP).unwrap();
    let packet = other.encode(&[0i16; 160]).unwrap();
    assert_eq!(inbound.convert(&packet).unwrap().len() % 2, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_microphone_reaches_the_call_only_between_talk_and_done() {
    let ch = FakeChannel::new("call");
    let sock = opened(&ch, Arc::new(VoiceTrack::new()), None, 0.05).await;
    let mut inbound = InboundAudio::new();
    let mut mic = Mic::new();
    for i in 0..50 {
        sock.audio(&inbound.convert(&mic.packet(1000.0, i * 960)).unwrap()); // before the press
    }
    assert_eq!(received(&sock, 0.05).await, vec![]);
    ch.page(json!({"type": "talk"}));
    for i in 50..100 {
        sock.audio(&inbound.convert(&mic.packet(1000.0, i * 960)).unwrap()); // held for a second
    }
    ch.page(json!({"type": "done"}));
    let got = received(&sock, 0.3).await;
    assert_eq!(got[0], Got::Json(json!({"type": "talk"})));
    assert_eq!(got[got.len() - 1], Got::Json(json!({"type": "done"})));
    let press: Vec<Vec<u8>> = got[1..got.len() - 1]
        .iter()
        .map(|g| match g {
            Got::Bin(b) => b.clone(),
            other => panic!("not audio: {other:?}"),
        })
        .collect();
    assert!(press[..press.len() - 1].iter().all(|d| d.len() == BLOCK_BYTES));
    assert!(!press.last().unwrap().is_empty() && press.last().unwrap().len() <= BLOCK_BYTES);
    let audio = press.concat();
    assert!((audio.len() as i64 - (PREROLL_BYTES + 2 * RATE) as i64).abs() <= 64); // the pre-roll, then the press
    assert!((dominant_hz(&audio, SAMPLE_RATE) - 1000.0).abs() <= 10.0);
    for i in 100..120 {
        sock.audio(&inbound.convert(&mic.packet(1000.0, i * 960)).unwrap()); // after the release: not the call's
    }
    assert_eq!(received(&sock, 0.05).await, vec![]);
}

#[tokio::test]
async fn test_the_300_ms_before_talk_are_replayed_after_it() {
    let ch = FakeChannel::new("call");
    let sock = opened(&ch, Arc::new(VoiceTrack::new()), None, POSTROLL_SECS).await;
    for value in 1..=10 {
        sock.audio(&pcm(100, value)); // one second, 100 ms per value
    }
    ch.page(json!({"type": "talk"}));
    let got = received(&sock, 0.05).await;
    assert_eq!(got[0], Got::Json(json!({"type": "talk"})));
    let blocks: Vec<Vec<i16>> = got[1..]
        .iter()
        .map(|g| match g {
            Got::Bin(b) => values(b),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(blocks, vec![vec![8], vec![9], vec![10]]); // the last 300 ms, in order
    assert_eq!(PREROLL_BYTES, 3 * BLOCK_BYTES);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_done_waits_for_the_post_roll_and_carries_its_audio() {
    let ch = FakeChannel::new("call");
    let sock = opened(&ch, Arc::new(VoiceTrack::new()), None, POSTROLL_SECS).await;
    ch.page(json!({"type": "talk"}));
    sock.audio(&pcm(250, 5));
    let t0 = Instant::now();
    ch.page(json!({"type": "done"}));
    sock.audio(&pcm(170, 6)); // the last RTP, after the done
    let mut got: Vec<Got> = Vec::new();
    while got.last() != Some(&Got::Json(json!({"type": "done"}))) {
        let (op, data) = tokio::time::timeout(Duration::from_secs(2), sock.recv()).await.unwrap().unwrap();
        got.push(read(op, data));
    }
    assert!(t0.elapsed().as_secs_f64() >= POSTROLL_SECS - 0.01); // held for the post-roll
    let audio: Vec<Vec<u8>> = got
        .iter()
        .filter_map(|g| match g {
            Got::Bin(b) => Some(b.clone()),
            _ => None,
        })
        .collect();
    let whole = pcm(420, 0).len();
    assert_eq!(audio.iter().map(Vec::len).sum::<usize>(), whole); // every byte of the press
    assert_eq!(audio.last().unwrap().len(), whole % BLOCK_BYTES); // its last part block
    assert_eq!(values(&audio.concat()), vec![5, 6]);
}

#[tokio::test]
async fn test_a_new_press_or_an_end_during_the_post_roll_releases_the_done_first() {
    let ch = FakeChannel::new("call");
    let sock = opened(&ch, Arc::new(VoiceTrack::new()), None, 5.0).await;
    ch.page(json!({"type": "talk"}));
    sock.audio(&pcm(100, 3));
    ch.page(json!({"type": "done"}));
    ch.page(json!({"type": "done"})); // twice: one done
    ch.page(json!({"type": "talk"})); // pressed again at once
    sock.audio(&pcm(100, 4));
    ch.page(json!({"type": "done"}));
    ch.page(json!({"type": "end"}));
    let got = received(&sock, 0.05).await;
    let shape: Vec<String> = got
        .iter()
        .map(|g| match g {
            Got::Bin(b) => format!("{:?}", values(b)),
            other => other.kind(),
        })
        .collect();
    assert_eq!(shape, vec!["talk", "[3]", "done", "talk", "[4]", "done", "end"]);
}

#[tokio::test]
async fn test_a_stray_done_and_other_messages_pass_as_they_came() {
    let ch = FakeChannel::new("call");
    let sock = opened(&ch, Arc::new(VoiceTrack::new()), None, POSTROLL_SECS).await;
    ch.page(json!({"type": "done"}));
    ch.page(json!({"type": "ping"}));
    ch.page(Value::String("not json".into()));
    ch.page_bytes(b"\x00binary on the call channel"); // audio is the track, never this
    ch.page(Value::String(eve_lite_rtc::py::dumps(&json!({"type": "ping", "pad": "x".repeat(MAX_CONTROL_CHARS)}), false)));
    let got = received(&sock, 0.05).await;
    assert_eq!(
        got,
        vec![Got::Json(json!({"type": "done"})), Got::Json(json!({"type": "ping"})), Got::Text(b"not json".to_vec())]
    );
}

// ---- eve's side: captions, flush and the rest ------------------------------------------------------------------

#[tokio::test]
async fn test_said_goes_out_when_its_audio_is_taken_and_flush_drops_the_rest() {
    let ch = FakeChannel::new("call");
    let voice = Arc::new(VoiceTrack::new());
    let sock = opened(&ch, voice.clone(), None, POSTROLL_SECS).await;
    sock.send_json(&json!({"type": "said", "text": "First sentence."})).await.unwrap();
    sock.send_bytes(&pcm(30, 9)).await.unwrap();
    sock.send_json(&json!({"type": "said", "text": "Second sentence."})).await.unwrap();
    sock.send_bytes(&pcm(20, 8)).await.unwrap();
    assert_eq!(page_got(&ch), Vec::<Value>::new()); // made, not yet playing
    assert_eq!(values(&voice.take()), vec![9]);
    assert_eq!(page_got(&ch), vec![json!({"type": "said", "text": "First sentence."})]); // with its first sound
    assert_eq!(values(&voice.take()), vec![8, 9]); // the frame where it changes
    let texts: Vec<Value> = page_got(&ch).iter().map(|m| m["text"].clone()).collect();
    assert_eq!(texts, vec![json!("First sentence."), json!("Second sentence.")]);
    sock.send_json(&json!({"type": "said", "text": "Third sentence."})).await.unwrap();
    sock.send_bytes(&pcm(500, 7)).await.unwrap();
    sock.send_json(&json!({"type": "flush"})).await.unwrap(); // a barge-in
    assert!(page_got(&ch).last().unwrap() == &json!({"type": "flush"}) && voice.queued() == 0);
    assert_eq!(values(&voice.take()), vec![0]); // silence, and no caption
    assert!(!page_got(&ch).iter().any(|m| m["text"] == "Third sentence."));
}

#[tokio::test]
async fn test_heard_state_and_ended_go_out_at_once() {
    let ch = FakeChannel::new("call");
    let voice = Arc::new(VoiceTrack::new());
    let sock = opened(&ch, voice.clone(), None, POSTROLL_SECS).await;
    sock.send_bytes(&pcm(1000, 2)).await.unwrap(); // a second of speech queued
    for msg in [
        json!({"type": "heard", "text": "what time is it"}),
        json!({"type": "state", "state": "thinking"}),
        json!({"type": "ended", "reason": "you hung up"}),
    ] {
        sock.send_json(&msg).await.unwrap();
        assert_eq!(page_got(&ch).last().unwrap(), &msg);
    }
    assert_eq!(voice.queued(), pcm(1000, 2).len()); // and the voice is untouched
}

#[tokio::test]
async fn test_a_channel_the_page_closed_is_read_to_its_end_then_closed() {
    let closed: Arc<Mutex<Vec<RtcCallSocket>>> = Arc::new(Mutex::new(Vec::new()));
    let ch = FakeChannel::new("call");
    let voice = Arc::new(VoiceTrack::new());
    let seen = closed.clone();
    let on_close: eve_lite_rtc::call::OnClose = Arc::new(move |s: &RtcCallSocket| seen.lock().push(s.clone()));
    let sock = opened(&ch, voice.clone(), Some(on_close), POSTROLL_SECS).await;
    sock.send_bytes(&pcm(100, 1)).await.unwrap();
    ch.page(json!({"type": "end"}));
    ch.close();
    assert!(sock.closed() && voice.queued() == 0);
    assert!(closed.lock().len() == 1 && closed.lock()[0].same(&sock));
    assert_eq!(sock.recv().await.unwrap(), (OP_TEXT, br#"{"type": "end"}"#.to_vec())); // what it sent first counts
    for _ in 0..2 {
        assert!(sock.recv().await.is_err());
    }
    assert!(sock.send_json(&json!({"type": "state", "state": "listening"})).await.is_err());
    assert!(sock.send_bytes(b"\x00\x00").await.is_err());
    CallSocket::close(&sock, 1000).await;
    assert_eq!(closed.lock().len(), 1); // once
}

#[tokio::test]
async fn test_this_side_closing_drops_what_is_unread_and_closes_the_channel() {
    let ch = FakeChannel::new("call");
    let sock = opened(&ch, Arc::new(VoiceTrack::new()), None, POSTROLL_SECS).await;
    ch.page(json!({"type": "talk"}));
    sock.audio(&pcm(300, 1));
    CallSocket::close(&sock, 1000).await;
    assert!(ch.is_closed());
    assert!(tokio::time::timeout(Duration::from_secs(1), sock.recv()).await.unwrap().is_err());
    sock.audio(&pcm(100, 1)); // late audio: nobody's
    ch.page(json!({"type": "talk"}));
}

// ---- the unchanged PhoneCalls over it ----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_press_on_the_call_channel_is_heard_and_answered_in_eves_voice() {
    let brain = FakeBrain::new("It is **noon**.");
    let voice = FakeVoice::new("what time is it");
    let calls = Arc::new(PhoneCalls::new(Arc::new(|_: &str| None), Some(brain.clone()), Some(voice.clone())));
    let ch = FakeChannel::new("call");
    let track = Arc::new(VoiceTrack::new());
    let sock = RtcCallSocket::new(ch.clone(), track.clone(), SECRET);
    let (c, s) = (calls.clone(), sock.clone());
    let task = tokio::spawn(async move { c.serve_socket(Arc::new(s), Some(Arc::new(|t: &str| t == SECRET))).await });
    until(|| page_got(&ch).contains(&json!({"type": "state", "state": "connected"})), 5.0).await;
    ch.page(json!({"type": "talk"}));
    sock.audio(&pcm(1000, 3));
    ch.page(json!({"type": "done"}));
    until(|| !voice.transcribed().is_empty() && track.queued() == 4800, 5.0).await; // the fake voice's 100 ms
    assert_eq!(voice.transcribed(), vec![pcm(1000, 3).len()]); // the press, whole
    assert_eq!(brain.heard(), vec!["what time is it".to_string()]);
    let got = page_got(&ch);
    assert!(got.contains(&json!({"type": "heard", "text": "what time is it"})));
    assert!(!got.iter().any(|m| m["type"] == "said")); // its sound has not started
    track.take();
    let said: Vec<Value> = page_got(&ch).into_iter().filter(|m| m["type"] == "said").collect();
    assert_eq!(said, vec![json!({"type": "said", "text": eve_phone_call::speakable(&brain.reply)})]); // read as words
    assert_eq!(track.queued(), 4800 - VOICE_FRAME_BYTES);
    ch.page(json!({"type": "end"}));
    tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap().unwrap();
    assert_eq!(page_got(&ch).last().unwrap(), &json!({"type": "ended", "reason": eve_phone_call::HUNG_UP}));
    assert!(sock.closed() && ch.is_closed() && calls.active().is_none() && brain.say.lock().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_hello_that_is_not_the_peers_is_refused() {
    let brain = FakeBrain::default_reply();
    let calls = PhoneCalls::new(Arc::new(|_: &str| None), Some(brain.clone()), Some(FakeVoice::calendar()));
    let ch = FakeChannel::new("call");
    let sock = RtcCallSocket::new(ch.clone(), Arc::new(VoiceTrack::new()), SECRET);
    let check: eve_phone_call::TokenCheck = Arc::new(|t: &str| t == "another secret");
    tokio::time::timeout(Duration::from_secs(5), calls.serve_socket(Arc::new(sock.clone()), Some(check)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page_got(&ch), vec![json!({"type": "ended", "reason": "Only eve's owner can call."})]);
    assert!(sock.closed() && brain.say.lock().is_none() && calls.active().is_none());
}
