//! Port of `bridge/tests/test_voice_gate.py`.
//!
//! No microphone and no model: a FakeEmbedder reads the speaker off a block's samples, so every test says
//! exactly whose voice each 100 ms block carries. Block sizes are small on purpose.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;

const BLOCK: usize = 8; // samples per fake block
const OWNER: i16 = 1000;
const OTHER: i16 = 2000;
const THIRD: i16 = 3000;

fn vec_of(v: i16) -> Option<[f64; 3]> {
    match v {
        OWNER => Some([1.0, 0.0, 0.0]),
        OTHER => Some([0.0, 1.0, 0.0]),
        THIRD => Some([0.0, 0.0, 1.0]),
        _ => None,
    }
}

/// `n` loud blocks whose samples all carry the speaker's number (rms ≈ who/32768 ≥ speech_rms).
fn speech(who: i16, n: usize) -> Vec<Vec<u8>> {
    (0..n).map(|_| (0..BLOCK).flat_map(|_| who.to_le_bytes()).collect()).collect()
}

fn quiet(n: usize) -> Vec<Vec<u8>> {
    (0..n).map(|_| vec![0u8; BLOCK * 2]).collect()
}

fn cat(parts: &[Vec<Vec<u8>>]) -> Vec<Vec<u8>> {
    parts.iter().flatten().cloned().collect()
}

/// The mean voice of the audio: a mix of speakers embeds between them, as a real mixture does.
struct FakeEmbedder {
    calls: Arc<AtomicUsize>,
    fail: bool,
}

impl FakeEmbedder {
    fn new() -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (Self { calls: calls.clone(), fail: false }, calls)
    }

    fn failing() -> Self {
        Self { calls: Arc::new(AtomicUsize::new(0)), fail: true }
    }
}

impl Embedder for FakeEmbedder {
    fn embed(&mut self, pcm: &[u8]) -> Result<Option<Vec<f64>>, GateError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(GateError::new("RuntimeError", "onnx went away"));
        }
        let mut total = [0.0f64; 3];
        for c in pcm.chunks_exact(2) {
            if let Some(v) = vec_of(i16::from_le_bytes([c[0], c[1]])) {
                for (t, x) in total.iter_mut().zip(v) {
                    *t += x;
                }
            }
        }
        Ok(if total.iter().any(|&x| x != 0.0) { Some(total.to_vec()) } else { None })
    }
}

#[derive(Clone)]
struct TestClock(Arc<AtomicU64>);

impl TestClock {
    fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0f64.to_bits())))
    }

    fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::SeqCst))
    }

    fn advance(&self, by: f64) {
        self.0.store((self.get() + by).to_bits(), Ordering::SeqCst);
    }

    fn boxed(&self) -> Clock {
        let c = self.clone();
        Box::new(move || c.get())
    }
}

fn test_config(mode: &str) -> GateConfig {
    GateConfig { speech_rms: 0.01, ..GateConfig::with_mode(mode) }
}

fn gate_with(mode: &str, tweak: impl FnOnce(&mut GateConfig)) -> (SpeakerGate, TestClock, Arc<AtomicUsize>) {
    let clock = TestClock::new();
    let mut config = test_config(mode);
    tweak(&mut config);
    let (embedder, calls) = FakeEmbedder::new();
    let mut g = SpeakerGate::with_clock(config, Some(Box::new(embedder)), clock.boxed(), BLOCK * 2);
    let seed = cat(&[quiet(5), speech(OWNER, 7), quiet(2)]).concat();
    assert!(g.enrol_seed(&seed).unwrap());
    (g, clock, calls)
}

fn gate(mode: &str) -> (SpeakerGate, TestClock) {
    let (g, clock, _) = gate_with(mode, |_| {});
    (g, clock)
}

fn feed(g: &mut SpeakerGate, clock: &TestClock, blocks: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for b in blocks {
        clock.advance(BLOCK_SECS);
        out.extend(g.process(b));
    }
    out
}

/// The owner finishes their first sentence: 3 s, enough to leave PROVISIONAL.
fn lock(g: &mut SpeakerGate, clock: &TestClock) {
    feed(g, clock, &cat(&[speech(OWNER, 30), quiet(5)]));
    assert_eq!(g.state(), "locked", "{}", g.summary());
}

fn silent(blocks: &[Vec<u8>]) -> usize {
    blocks.iter().filter(|b| b.iter().all(|&x| x == 0)).count()
}

fn audible(b: &[u8]) -> bool {
    b.iter().any(|&x| x != 0)
}

// ---- configuration ---------------------------------------------------------------------------------

#[test]
fn test_it_ships_off_and_off_means_no_gate_at_all() {
    let env = |pairs: &[(&str, &str)]| {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        configured_from(move |k| m.get(k).cloned())
    };
    assert!(DEFAULT_MODE == "off" && env(&[]).mode == "off");
    assert_eq!(env(&[("EVE_VOICE_GATE", "shadow")]).mode, "shadow");
    assert_eq!(env(&[("EVE_VOICE_GATE", "loud")]).mode, "off");
    let (fake, _) = FakeEmbedder::new();
    assert!(build(GateConfig::with_mode("off"), &[b'x'; 64], Box::new(monotonic), Some(Box::new(fake))).is_none());
    // no wake audio — a conversation the lesson toggle opened — is ungated whatever the mode
    let (fake, _) = FakeEmbedder::new();
    assert!(build(GateConfig::with_mode("on"), b"", Box::new(monotonic), Some(Box::new(fake))).is_none());
}

#[test]
fn test_the_seed_is_the_speech_in_the_wake_audio_not_the_room_before_it() {
    let (g, _) = gate("on");
    assert!(g.state() == "provisional" && (g.stats.enrolled_secs - 0.7).abs() < 1e-6);
    assert_eq!(trim_to_speech(&quiet(6).concat(), 0.01, BLOCK * 2), Vec::<u8>::new());
}

// ---- the stream --------------------------------------------------------------------------------------

#[test]
fn test_the_owner_is_forwarded_whole_in_order_after_a_short_hold() {
    let (mut g, clock) = gate("on");
    let said = cat(&[speech(OWNER, 20), quiet(5)]);
    let out = feed(&mut g, &clock, &said);
    assert_eq!(out, said); // every sample, in order: held, then burst, then live
    assert!(g.stats.accepted == 1 && g.stats.secs_silenced == 0.0);
    let first: Vec<Vec<Vec<u8>>> = speech(OWNER, 5).iter().map(|b| g.process(b)).collect();
    assert!(first.iter().all(Vec::is_empty) && first.len() == 5); // nothing leaves while the first 0.6 s is judged
}

#[test]
fn test_another_voice_becomes_silence_of_the_same_length() {
    let (mut g, clock) = gate("on");
    lock(&mut g, &clock);
    let tv = cat(&[speech(OTHER, 30), quiet(5)]);
    let out = feed(&mut g, &clock, &tv);
    assert!(out.len() == tv.len() && out.iter().zip(&tv).all(|(a, b)| a.len() == b.len()));
    assert_eq!(silent(&out), out.len()); // not one sample of it reaches the model
    assert!(g.stats.rejected == 1 && g.stats.decisions.last().unwrap().why == "mismatch");
}

#[test]
fn test_the_positive_control_the_same_audio_with_the_gate_off_is_forwarded() {
    let (mut g, clock) = gate("off");
    let tv = cat(&[speech(OTHER, 30), quiet(5)]);
    assert_eq!(feed(&mut g, &clock, &tv), tv);
}

#[test]
fn test_shadow_judges_everything_and_changes_nothing() {
    let (mut g, clock) = gate("shadow");
    lock(&mut g, &clock);
    let tv = cat(&[speech(OTHER, 30), quiet(5)]);
    assert_eq!(feed(&mut g, &clock, &tv), tv); // heard exactly as the mic heard it
    assert!(g.stats.rejected == 1 && g.stats.secs_silenced > 2.5); // …and counted as `on` would have
}

#[test]
fn test_quiet_room_noise_passes_untouched() {
    let (mut g, clock) = gate("on");
    lock(&mut g, &clock);
    let hum: Vec<Vec<u8>> = (0..10).map(|_| (0..BLOCK).flat_map(|_| 40i16.to_le_bytes()).collect()).collect();
    assert!(feed(&mut g, &clock, &hum) == hum && g.stats.segments == 1); // only the owner's segment so far
}

#[test]
fn test_the_owner_speaking_up_mid_segment_is_heard_and_a_takeover_is_cut() {
    let (mut g, clock) = gate("on");
    lock(&mut g, &clock);
    let out = feed(&mut g, &clock, &cat(&[speech(OTHER, 15), speech(OWNER, 40), quiet(5)]));
    // silenced, then forwarded once it is the owner
    assert!(silent(&out[..15]) == 15 && out[out.len() - 25..].iter().any(|b| audible(b)));
    let (mut g2, clock2) = gate("on");
    lock(&mut g2, &clock2);
    let out2 = feed(&mut g2, &clock2, &cat(&[speech(OWNER, 15), speech(OTHER, 50), quiet(5)]));
    // forwarded, then cut when someone took over
    assert!(audible(&out2[0]) && silent(&out2[out2.len() - 20..]) == 20);
}

// ---- what must never be lost -----------------------------------------------------------------------

#[test]
fn test_a_word_too_short_to_judge_passes() {
    let (mut g, clock) = gate("on");
    lock(&mut g, &clock);
    clock.advance(5.0); // long after anyone last spoke
    let word = cat(&[speech(OTHER, 3), quiet(5)]);
    assert!(feed(&mut g, &clock, &word) == word && g.stats.decisions.last().unwrap().why == "too_short");
}

#[test]
fn test_a_fragment_right_after_judged_speech_gets_that_verdict() {
    let (mut g, clock) = gate("on");
    lock(&mut g, &clock);
    // a halting voice: the fragment is the TV's too
    let tv = cat(&[speech(OTHER, 20), quiet(5), speech(OTHER, 3), quiet(5)]);
    let out = feed(&mut g, &clock, &tv);
    assert!(silent(&out) == out.len() && g.stats.decisions.last().unwrap().why == "continues");
    let owner = cat(&[speech(OWNER, 20), quiet(5), speech(OWNER, 3), quiet(5)]);
    assert_eq!(feed(&mut g, &clock, &owner), owner);
}

#[test]
fn test_nothing_is_rejected_on_the_first_fraction_of_a_second() {
    let (mut g, clock) = gate("on");
    lock(&mut g, &clock);
    // 0.6 s of someone else, then the owner: the first judgement is "not them", so the gate holds on and
    // judges again on 1.5 s — by then mostly the owner — instead of silencing on too little.
    let out = feed(&mut g, &clock, &cat(&[speech(OTHER, 6), speech(OWNER, 24), quiet(5)]));
    assert!(out.len() == 35 && audible(&out[0]) && g.stats.decisions.last().unwrap().accepted);
}

#[test]
fn test_while_an_answer_is_awaited_a_short_reply_is_never_held_back() {
    let (mut g, clock) = gate("on");
    lock(&mut g, &clock);
    g.lenient = true;
    // 0.9 s, and it does not sound like the owner (a cold, a cough)
    let reply = cat(&[speech(OTHER, 9), quiet(5)]);
    assert!(feed(&mut g, &clock, &reply) == reply && g.stats.decisions.last().unwrap().why == "lenient");
    g.lenient = false;
    // without the question pending, it is judged as usual
    assert_eq!(silent(&feed(&mut g, &clock, &reply)), reply.len());
}

#[test]
fn test_notes_are_not_gated_a_think_aloud_lesson_hears_everyone() {
    let (mut g, clock, calls) = gate_with("on", |_| {});
    lock(&mut g, &clock);
    g.bypass = true;
    let room = cat(&[speech(OTHER, 20), speech(THIRD, 20), quiet(5)]);
    let before = calls.load(Ordering::SeqCst);
    // forwarded whole, and nobody was judged
    assert!(feed(&mut g, &clock, &room) == room && calls.load(Ordering::SeqCst) == before);
}

#[test]
fn test_audio_held_for_a_decision_is_forwarded_when_the_gate_stops_judging() {
    // bypass set (a lesson starts) or mode off while the first 0.6 s of a segment was held: _end_segment
    // cleared the held blocks, and up to 1.5 s of what was said never reached the ears
    for stop in ["bypass", "off"] {
        let (mut g, clock) = gate("on");
        lock(&mut g, &clock);
        let start = speech(OWNER, 3);
        assert_eq!(feed(&mut g, &clock, &start), Vec::<Vec<u8>>::new()); // held, not judged yet
        if stop == "bypass" {
            g.bypass = true;
        } else {
            g.config = test_config("off");
        }
        let more = speech(OWNER, 2);
        assert_eq!(feed(&mut g, &clock, &more), cat(&[start, more]), "{stop}"); // nothing said is lost
    }
}

#[test]
fn test_shadow_never_repeats_the_held_audio_when_bypassed() {
    let (mut g, clock) = gate("shadow");
    lock(&mut g, &clock);
    let start = speech(OWNER, 3);
    assert_eq!(feed(&mut g, &clock, &start), start); // shadow hears it as it comes
    g.bypass = true;
    let more = speech(OWNER, 2);
    assert_eq!(feed(&mut g, &clock, &more), more);
}

// ---- failing open --------------------------------------------------------------------------------------

#[test]
fn test_a_gate_that_cannot_judge_is_todays_behaviour() {
    let tv = cat(&[speech(OTHER, 30), quiet(5)]);
    let no_embedder = SpeakerGate::with_clock(test_config("on"), None, TestClock::new().boxed(), BLOCK * 2);
    let (fake, _) = FakeEmbedder::new();
    let no_voice = build(test_config("on"), &quiet(10).concat(), Box::new(monotonic), Some(Box::new(fake)));
    for g in [Some(no_embedder), no_voice] {
        let mut g = g.expect("a gate");
        assert_eq!(g.state(), "open"); // no embedder; a wake snapshot with no voice in it
        assert_eq!(feed(&mut g, &TestClock::new(), &tv), tv);
    }
    let (mut broken, clock) = gate("on");
    lock(&mut broken, &clock);
    broken.embedder = Some(Box::new(FakeEmbedder::failing()));
    let out = feed(&mut broken, &clock, &tv);
    assert!(out == tv && broken.stats.opened.starts_with("error")); // nothing held back is lost either
    assert_eq!(broken.stats.opened, "error: RuntimeError");
}

#[test]
fn test_when_nothing_has_ever_matched_the_enrolment_is_what_is_wrong() {
    let (mut g, clock, _) = gate_with("on", |c| c.open_after_rejected_secs = 6.0);
    g.woke_at = -100.0; // long after the wake: no first-turn leniency
    g.stats.enrolled_secs = 5.0; // a seed that was really the TV
    feed(&mut g, &clock, &cat(&[speech(OTHER, 35), quiet(5)]));
    assert!(g.state() == "locked" && g.stats.rejected == 1);
    feed(&mut g, &clock, &cat(&[speech(OTHER, 35), quiet(5)]));
    assert!(g.state() == "open" && g.stats.opened.contains("nothing matched"));
    let again = cat(&[speech(OTHER, 10), quiet(5)]);
    assert_eq!(feed(&mut g, &clock, &again), again);
}

#[test]
fn test_a_tv_that_talks_for_a_minute_never_opens_a_gate_that_has_heard_its_owner() {
    let (mut g, clock, _) = gate_with("on", |c| c.open_after_rejected_secs = 6.0);
    lock(&mut g, &clock);
    for _ in 0..12 {
        let out = feed(&mut g, &clock, &cat(&[speech(OTHER, 50), quiet(5)]));
        assert_eq!(silent(&out), out.len());
    }
    assert!(g.state() == "locked" && g.stats.rejected == 12);
}

#[test]
fn test_another_voice_cannot_walk_the_enrolment_away_from_the_seed() {
    let (mut g, clock) = gate("on");
    lock(&mut g, &clock);
    let before = g.centroid.clone();
    feed(&mut g, &clock, &cat(&[speech(OTHER, 40), quiet(5)]));
    assert_eq!(g.centroid, before);
    g.reset();
    // nothing of a voice is kept
    assert!(g.seed.is_none() && g.centroid.is_none() && g.state() == "open");
}

// ---- the wake ring ---------------------------------------------------------------------------------------

#[test]
fn test_the_wake_ring_keeps_only_the_last_seconds_in_ram() {
    let mut ring = WakeRing::new(0.3);
    for n in 0..5u8 {
        ring.add(&[n; 4]);
    }
    assert_eq!(ring.snapshot(), [[2u8; 4], [3; 4], [4; 4]].concat());
    ring.clear();
    assert_eq!(ring.snapshot(), Vec::<u8>::new());
}

// ---- parity with the Python ------------------------------------------------------------------------------

#[test]
fn configured_strips_lowers_and_falls_back_like_python() {
    let one = |k: &'static str, v: &'static str| configured_from(move |q| (q == k).then(|| v.to_string()));
    assert_eq!(one(MODE_VAR, "  On \n").mode, "on");
    assert_eq!(one(MODE_VAR, "\u{1c}shadow\u{1f}").mode, "shadow"); // str.strip() takes the separators too
    assert_eq!(one(MODE_VAR, "   ").mode, "off");
    let c = one(MODEL_VAR, "  /m/x.onnx ");
    assert_eq!((c.mode.as_str(), c.model.as_str()), ("off", "/m/x.onnx"));
    assert_eq!(one(MODEL_VAR, "   ").model, DEFAULT_MODEL);
    assert_eq!(one(MODEL_VAR, "").model, DEFAULT_MODEL);
    assert_eq!(configured_from(|_| None), GateConfig::default());
}

#[test]
fn defaults_match_the_python_dataclass() {
    let c = GateConfig::default();
    assert_eq!((c.mode.as_str(), c.model.as_str()), ("off", "~/.config/eve-bridge/models/nemo_en_titanet_small.onnx"));
    assert_eq!((c.speech_rms, c.hang_blocks, c.hold_secs, c.hold_more_secs), (0.012, 4, 0.6, 1.5));
    assert_eq!((c.continue_secs, c.min_decide_secs, c.rescore_secs, c.window_secs), (1.0, 0.5, 0.5, 2.0));
    assert_eq!((c.enrol_min_secs, c.first_turn_secs, c.enrol_cap_secs), (3.0, 10.0, 20.0));
    assert_eq!((c.t_loose, c.t_accept, c.t_drop, c.t_adapt, c.t_seed_floor), (0.30, 0.33, 0.27, 0.50, 0.20));
    assert_eq!(c.open_after_rejected_secs, 12.0);
    assert_eq!(default_block_bytes(), 4800);
    assert_eq!(
        MODEL_URL,
        "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/nemo_en_titanet_small.onnx"
    );
}

#[test]
fn rms_and_cosine_match_python() {
    assert_eq!(rms(b""), 0.0);
    assert_eq!(rms(&[0x01]), 0.0); // a lone byte is no sample
    // python: rms(struct.pack('<3h', 1000, -2000, 3000)) == 0.06592550352384298
    assert_eq!(rms(&[1000i16, -2000, 3000].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>()), 0.06592550352384298);
    assert_eq!(rms(&i16::MIN.to_le_bytes()), 1.0);
    assert_eq!(cosine(&[1.0, 0.0], &[0.0, 0.0]).unwrap(), 0.0);
    assert!((cosine(&[1.0, 1.0], &[1.0, 0.0]).unwrap() - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-15);
    let e = cosine(&[1.0, 2.0], &[1.0]).unwrap_err();
    assert_eq!((e.kind.as_str(), e.message.as_str()), ("ValueError", "zip() argument 2 is shorter than argument 1"));
    let e = cosine(&[1.0], &[1.0, 2.0]).unwrap_err();
    assert_eq!(e.message, "zip() argument 2 is longer than argument 1");
}

#[test]
fn sums_are_compensated_like_python_3_12() {
    // python 3.13: sum([1e16, 1.0, -1e16]) == 1.0 and sum([0.1]*10) == 1.0
    assert_eq!(py_sum([1e16, 1.0, -1e16]), 1.0);
    assert_eq!(py_sum([0.1; 10]), 1.0);
    assert_eq!(py_sum(std::iter::empty()), 0.0);
}

#[test]
fn rounding_and_slicing_match_python() {
    assert_eq!(py_round(0.125, 2), 0.12);
    assert_eq!(py_round(2.675, 2), 2.67);
    assert_eq!(py_round(0.3333333, 3), 0.333);
    let v = [1, 2, 3, 4];
    assert_eq!(py_tail(&v, 2), &[3, 4]);
    assert_eq!(py_tail(&v, 9), &v);
    assert_eq!(py_tail(&v, 0), &v); // seq[-0:] is the whole list
}

#[test]
fn repr_is_pythons() {
    assert_eq!(py_repr("loud"), "'loud'");
    assert_eq!(py_repr("it's"), "\"it's\"");
    assert_eq!(py_repr("a\tb\\"), "'a\\tb\\\\'");
    assert_eq!(py_repr("\u{1}"), "'\\x01'");
}

#[test]
fn expanduser_reads_home() {
    let home = std::env::var("HOME").unwrap_or_default();
    assert_eq!(expanduser("~/x"), format!("{}/x", home.trim_end_matches('/')));
    assert_eq!(expanduser("/abs/~"), "/abs/~");
    assert_eq!(expanduser("~no-such-user-eve-test/x"), "~no-such-user-eve-test/x");
}

#[test]
fn the_summary_has_pythons_keys_in_order() {
    let (mut g, clock) = gate("on");
    lock(&mut g, &clock);
    let s = g.summary();
    let keys: Vec<&str> = s.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["mode", "segments", "accepted", "rejected", "secs_forwarded", "secs_silenced", "enrolled_secs", "opened"]
    );
    assert_eq!(s["mode"], "on");
    assert_eq!(s["segments"], 1);
    assert_eq!(s["secs_forwarded"], 3.4);
    assert_eq!(s["enrolled_secs"], 3.8);
    assert_eq!(s["opened"], "");
}

#[test]
fn a_decision_records_rounded_numbers() {
    let (mut g, clock) = gate("on");
    lock(&mut g, &clock);
    let d = g.stats.decisions.first().unwrap();
    assert_eq!((d.state.as_str(), d.why.as_str(), d.accepted), ("provisional", "first_turn", true));
    assert_eq!((d.secs, d.score, d.threshold), (0.6, Some(1.0), 0.30));
}

/// Expected values printed by the Python gate on the same scenario (Python 3.13).
#[test]
fn a_takeover_scenario_matches_python_number_for_number() {
    let (mut g, clock) = gate("on");
    lock(&mut g, &clock);
    feed(&mut g, &clock, &cat(&[speech(OTHER, 15), speech(OWNER, 40), quiet(5)]));
    assert_eq!(
        g.summary(),
        json!({"mode": "on", "segments": 2, "accepted": 2, "rejected": 0, "secs_forwarded": 6.9,
               "secs_silenced": 2.4, "enrolled_secs": 6.8, "opened": ""})
    );
    let got: Vec<(f64, f64, &str, Option<f64>, f64, bool, &str)> = g
        .stats
        .decisions
        .iter()
        .map(|d| (d.at, d.secs, d.state.as_str(), d.score, d.threshold, d.accepted, d.why.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (0.6, 0.6, "provisional", Some(1.0), 0.3, true, "first_turn"),
            (4.999999999999998, 1.5, "locked", Some(0.0), 0.33, false, "mismatch"),
            (5.999999999999995, 2.5, "locked", Some(0.707), 0.33, true, "match"),
        ]
    );
    assert_eq!(g.centroid, Some(vec![0.9997266458205615, 0.023380197526317502, 0.0]));
}

#[test]
fn an_error_mid_hold_forwards_the_held_block_once() {
    let (mut g, clock) = gate("on");
    lock(&mut g, &clock);
    let start = speech(OWNER, 5);
    assert!(feed(&mut g, &clock, &start).is_empty());
    g.embedder = Some(Box::new(FakeEmbedder::failing()));
    let last = speech(OWNER, 1); // the 0.6th second: the judgement raises
    let out = feed(&mut g, &clock, &last);
    assert_eq!(out, cat(&[start, last]));
    assert_eq!(g.state(), "open");
}

#[test]
fn a_missing_model_means_no_gate() {
    let e = SherpaEmbedder::new("/no/such/eve/model.onnx").err().unwrap();
    assert_eq!(e.kind, "FileNotFoundError");
    assert_eq!(
        e.message,
        format!("no speaker model at /no/such/eve/model.onnx (download {MODEL_URL})")
    );
    let config = GateConfig { model: "/no/such/eve/model.onnx".into(), ..GateConfig::with_mode("on") };
    assert!(!model_present(&config));
    assert!(build(config, &speech(OWNER, 3).concat(), Box::new(monotonic), None).is_none());
}

#[test]
#[ignore = "live: needs ~/.config/eve-bridge/models/nemo_en_titanet_small.onnx"]
fn the_sherpa_embedder_embeds_two_seconds() {
    let config = GateConfig::default();
    assert!(model_present(&config));
    let mut e = SherpaEmbedder::new(&config.model).unwrap();
    let tone: Vec<u8> = (0..48000)
        .flat_map(|i| (((i as f64 * 0.07).sin() * 6000.0) as i16).to_ne_bytes())
        .collect();
    let v = e.embed(&tone).unwrap().expect("an embedding");
    assert_eq!(v.len(), 192);
    // python: SherpaEmbedder(DEFAULT_MODEL).embed(<the same tone>)[:5] on sherpa-onnx 1.13.8
    let python = [0.06485257297754288, 0.013026864267885685, -0.00543045112863183, 0.1379084587097168, 0.01016321498900652];
    for (r, p) in v.iter().zip(python) {
        assert!((r - p).abs() < 1e-5, "{r} vs {p}");
    }
    assert_eq!(e.embed(&tone[..4000]).unwrap(), None); // under 0.2 s
}
