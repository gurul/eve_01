//! lite_rtc.VoiceTrack: eve's voice on the web app's audio track (port of bridge/tests/test_voice_track.py). 20 ms
//! frames of 24 kHz mono 16-bit with rising pts, paced in real time on a fake clock, silence while nothing is
//! queued; markers (the `said` captions) firing when their audio is taken; and the peer's track and Opus encoder
//! taking the frames.

mod common;

use std::sync::Arc;

use common::*;
use futures::FutureExt;
use parking_lot::Mutex;

use eve_lite_rtc::voice::VoiceEncoder;
use eve_lite_rtc::{media_track, VoiceTrack, MAX_VOICE_BYTES, SAMPLE_RATE, VOICE_FRAME_BYTES, VOICE_FRAME_SAMPLES};

/// A clock that moves only when the track sleeps (or a test says so).
struct Clock {
    t: Mutex<f64>,
    slept: Mutex<Vec<f64>>,
}

impl Clock {
    fn add(&self, s: f64) {
        *self.t.lock() += s;
    }
    fn slept(&self) -> Vec<f64> {
        self.slept.lock().clone()
    }
}

fn track(max_bytes: usize) -> (VoiceTrack, Arc<Clock>) {
    let clock = Arc::new(Clock { t: Mutex::new(100.0), slept: Mutex::new(Vec::new()) });
    let (now, sleep) = (clock.clone(), clock.clone());
    let voice = VoiceTrack::with(
        Arc::new(move || *now.t.lock()),
        Arc::new(move |secs: f64| {
            sleep.slept.lock().push(secs);
            *sleep.t.lock() += secs;
            async {}.boxed()
        }),
        max_bytes,
    );
    (voice, clock)
}

fn ramp(n: usize, start: i16) -> Vec<u8> {
    bytes_of(&(0..n).map(|i| start + i as i16).collect::<Vec<_>>())
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[tokio::test]
async fn test_frames_are_20_ms_of_24k_mono_with_rising_pts_and_silence_when_nothing_is_queued() {
    let (voice, clock) = track(MAX_VOICE_BYTES);
    let mut frames = Vec::new();
    for _ in 0..6 {
        frames.push(voice.recv().await);
    }
    for (i, f) in frames.iter().enumerate() {
        assert_eq!((f.samples, f.sample_rate), (VOICE_FRAME_SAMPLES, SAMPLE_RATE)); // s16 mono, 1/24000 time base
        assert_eq!(f.pts, (i * VOICE_FRAME_SAMPLES) as u64);
        assert!(f.samples_i16().iter().all(|s| *s == 0)); // silence
    }
    assert_eq!(VOICE_FRAME_SAMPLES, 480);
    let slept = clock.slept();
    assert!(slept.len() == 5 && slept.iter().all(|s| approx(*s, 0.02))); // the first at once, then real time
    assert_eq!(voice.frames(), 6);
}

#[tokio::test]
async fn test_queued_speech_plays_in_order_across_frames_then_silence() {
    let (voice, _) = track(MAX_VOICE_BYTES);
    let data = ramp(700, 1); // 1,400 bytes: a frame and a half
    for piece in data.chunks(101) {
        voice.push(piece); // odd pieces: a sample split in two
    }
    assert_eq!(voice.queued(), data.len());
    let mut out = Vec::new();
    for _ in 0..3 {
        out.extend(voice.recv().await.samples_i16());
    }
    assert_eq!(out[..700].to_vec(), (1..=700).collect::<Vec<i16>>());
    assert!(out[700..].iter().all(|s| *s == 0));
    assert_eq!(voice.queued(), 0);
    voice.push(b"\x05"); // half a sample waits for the rest
    assert_eq!(voice.queued(), 0);
    voice.push(b"\x00");
    assert_eq!(voice.recv().await.samples_i16()[0], 5);
}

#[test]
fn test_a_marker_fires_when_the_audio_after_it_is_taken() {
    let fired: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let mark = |name: &'static str| {
        let f = fired.clone();
        Box::new(move || {
            f.lock().push(name);
            Ok(())
        }) as eve_lite_rtc::Marker
    };
    let (voice, _) = track(MAX_VOICE_BYTES);
    voice.mark(mark("first"));
    voice.push(&ramp(VOICE_FRAME_SAMPLES, 1));
    voice.mark(mark("second"));
    voice.push(&ramp(VOICE_FRAME_SAMPLES, 600));
    assert!(fired.lock().is_empty()); // queued is not playing
    voice.take();
    assert_eq!(*fired.lock(), vec!["first"]);
    voice.take();
    assert_eq!(*fired.lock(), vec!["first", "second"]);
    voice.push(&ramp(100, 1));
    voice.mark(mark("no sound of its own")); // its speech failed: no audio
    voice.take();
    assert_eq!(*fired.lock().last().unwrap(), "no sound of its own"); // it goes where its sound would
    voice.mark(mark("flushed"));
    voice.push(&ramp(VOICE_FRAME_SAMPLES, 1));
    voice.flush();
    assert!(i16s(&voice.take()).iter().all(|s| *s == 0) && !fired.lock().contains(&"flushed"));
    voice.mark(Box::new(|| anyhow::bail!("the channel is gone")));
    voice.push(&ramp(VOICE_FRAME_SAMPLES, 1));
    assert_eq!(i16s(&voice.take())[0], 1); // the voice goes on
}

#[tokio::test]
async fn test_the_track_keeps_real_time_and_starts_again_after_a_stall() {
    let (voice, clock) = track(MAX_VOICE_BYTES);
    for _ in 0..3 {
        voice.recv().await;
    }
    clock.add(0.01); // a little late: catch up
    voice.recv().await;
    assert!(approx(*clock.slept().last().unwrap(), 0.01));
    clock.add(1.0); // the loop stalled for a second
    let n = clock.slept().len();
    let f = voice.recv().await;
    assert_eq!(clock.slept().len(), n); // this one at once
    voice.recv().await;
    assert!(approx(*clock.slept().last().unwrap(), 0.02)); // then 20 ms, not a burst of 50
    assert_eq!(f.pts, (4 * VOICE_FRAME_SAMPLES) as u64); // pts never jumps or repeats
}

#[test]
fn test_speech_queued_past_the_cap_is_dropped() {
    let (voice, _) = track(4 * VOICE_FRAME_BYTES);
    voice.push(&vec![0u8; 4 * VOICE_FRAME_BYTES]);
    voice.push(&[0u8; 2]);
    assert!(voice.queued() == 4 * VOICE_FRAME_BYTES && voice.dropped() == 2);
    assert_eq!(MAX_VOICE_BYTES, 600 * 2 * SAMPLE_RATE as usize);
}

/// aiortc's track and Opus encoder: here the peer's media track and the libopus encoder it speaks through.
#[tokio::test]
async fn test_aiortc_takes_the_track_and_its_opus_encoder_the_frames() {
    let (voice, _) = track(MAX_VOICE_BYTES);
    let voice = Arc::new(voice);
    let media = media_track(voice.clone());
    assert!(media.kind() == "audio" && media.ready_state() == "live");
    let other = media_track(voice.clone()); // one per peer
    assert!(!std::ptr::eq(&media, &other));
    voice.push(&bytes_of(&vec![3000i16; 10 * VOICE_FRAME_SAMPLES]));
    let mut encoder = VoiceEncoder::new().unwrap();
    let mut stamps: Vec<u64> = Vec::new();
    for _ in 0..10 {
        let frame = media.recv().await.unwrap();
        let packet = encoder.encode(&frame).unwrap();
        if !packet.is_empty() {
            stamps.push(frame.pts * 2); // on Opus's 48 kHz RTP clock
        }
    }
    assert!(stamps.len() >= 8 && stamps.windows(2).all(|w| w[0] < w[1]));
    assert!(stamps.windows(2).all(|w| w[1] - w[0] == 960)); // 20 ms at Opus's 48 kHz
    media.stop();
    assert!(media.recv().await.is_err());
}
