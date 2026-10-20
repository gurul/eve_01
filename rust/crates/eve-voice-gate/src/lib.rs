//! The voice gate: in a conversation, only the person who said "hey eve" reaches the model.
//!
//! Port of `bridge/src/eve_bridge/voice_gate.py`.
//!
//! eve's live voice is OpenAI's Live API (gpt-live-1), which has no turn detection, noise reduction or
//! speaker setting a client can touch: it hears whatever the client appends. So a TV, a call or someone else
//! in the room were answered, kept the conversation open (any transcribed speech resets the idle timer) and
//! wiped eve's caption mid-reply (any transcribed speech is a barge-in). Whatever isolates a speaker has to
//! happen here, on the Mac, before `input_audio.append`.
//!
//! HOW. The last seconds of microphone audio before the wake word are kept in RAM (ears), so the wake
//! utterance is an enrolment sample. Speech is cut into segments by energy; each segment's first HOLD of
//! audio is held back, embedded with a speaker-embedding model (sherpa-onnx — TitaNet-small, 192 numbers,
//! ~10 ms on this Mac) and compared with the enrolled voice by cosine. A match is forwarded — the held audio
//! in one burst, then live; anything else is forwarded as SILENCE of the same length, because the Live model
//! wants a continuous stream and needs trailing quiet to close a turn. Non-speech blocks pass untouched.
//!
//! ```text
//!     SEED         the wake utterance, trimmed to its speech. Never overwritten.
//!     PROVISIONAL  until ENROL_MIN seconds are enrolled NOTHING IS REJECTED. "hey eve" is ~0.7 s, and a voice
//!                  embedding of that is not evidence: replayed on sherpa-onnx's sample speakers, one speaker's
//!                  own later speech scored 0.06–0.26 against his 0.6 s seed and most of it was silenced. So the
//!                  speech that starts within FIRST_TURN of the wake — the owner's query, by construction — is
//!                  forwarded and enrolled whatever it scores; later speech is forwarded too, and enrolled only
//!                  when it already resembles the voice so far (T_LOOSE).
//!     LOCKED       ≥ ENROL_MIN seconds enrolled: a segment is forwarded when its cosine clears T_ACCEPT,
//!                  re-scored as it goes on. This is the only state that silences anything.
//!     ADAPT        only a segment close to the centroid AND to the seed moves the centroid: a TV cannot walk it.
//!     RESET        at the end of the conversation everything is dropped. Nothing about a voice touches the disk.
//! ```
//!
//! IT FAILS OPEN, except where it is sure. No model file, an embedder that raised, no usable seed, a segment
//! too short to judge, nothing accepted at all since the wake (the enrolment itself is suspect): all of those
//! forward the audio as today. It closes only on a confident mismatch — which is the TV and the other person,
//! the long, plainly different segments. A robot that stops hearing its owner cannot be debugged by talking
//! to it.
//!
//! ONLY FOR QUERIES. In a think-aloud lesson eve is a listener taking notes on whoever is speaking, so the
//! session sets `bypass` and every block passes unjudged. A conversation opened without a wake word (the
//! lesson toggle) has no seed, so it is ungated too.
//!
//! THE NUMBERS BELOW ARE NOT FITTED. They come from a three-speaker probe on sherpa-onnx's sample recordings
//! (2026-09-21: TitaNet-small separated 3 s enrolments from 2 s tests cleanly, lowest same-speaker cosine
//! 0.58, highest different-speaker 0.39; two other popular models did not separate at all). That is why the
//! switch ships `off`, why `shadow` exists — every decision computed and logged, no audio changed — and why
//! tools/voice_gate_eval.py scores a labelled set of the owner's own room before anything is believed.

use std::collections::VecDeque;
use std::ffi::{CStr, CString, OsStr};
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Instant;

use serde_json::{json, Value};

/// The sherpa-onnx binding this crate links (statically). Re-exported so another eve crate that needs
/// sherpa-onnx (the ears keyword spotter) links the same build instead of a second copy of its C library.
pub use sherpa_onnx;
use tracing::{info, warn};

/// ears' capture rate; the extractor resamples to 16 kHz itself.
pub const SAMPLE_RATE: u32 = 24000;
/// ears' block: 2400 samples.
pub const BLOCK_SECS: f64 = 0.1;
pub const MODES: [&str; 3] = ["off", "shadow", "on"];
pub const DEFAULT_MODE: &str = "off";
pub const DEFAULT_MODEL: &str = "~/.config/eve-bridge/models/nemo_en_titanet_small.onnx";
pub const MODEL_URL: &str = concat!(
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/",
    "nemo_en_titanet_small.onnx"
);
/// The env var that picks the mode (`off` | `shadow` | `on`).
pub const MODE_VAR: &str = "EVE_VOICE_GATE";
/// The env var that points at the speaker model.
pub const MODEL_VAR: &str = "EVE_VOICE_GATE_MODEL";

const RING_SECS: f64 = 3.0;

// ---- errors ------------------------------------------------------------------------------------------

/// What Python raises inside the gate: the exception's class name (it reaches the log and `stats.opened`)
/// and its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateError {
    pub kind: String,
    pub message: String,
}

impl GateError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind: kind.into(), message: message.into() }
    }
}

impl fmt::Display for GateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GateError {}

// ---- configuration -----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct GateConfig {
    pub mode: String,
    pub model: String,
    /// A block at or above this is speech (ears' SILENCE_RMS is the dead-mic floor).
    pub speech_rms: f64,
    /// Quiet blocks that end a segment (0.4 s).
    pub hang_blocks: u32,
    /// Audio held back before the first decision on a segment.
    pub hold_secs: f64,
    /// …and when that decision is "not them": hold on, and judge again on this much. 0.6 s is nearly
    /// undecidable (one speaker's own speech: 0.20–0.41), so nothing is REJECTED on it. Only unconvincing
    /// openings pay the extra wait.
    pub hold_more_secs: f64,
    /// A fragment too short to judge, this soon after a judged one, gets its verdict.
    pub continue_secs: f64,
    /// A shorter segment cannot be judged: it passes.
    pub min_decide_secs: f64,
    pub rescore_secs: f64,
    /// What a re-score embeds: the last this-many seconds of the segment.
    pub window_secs: f64,
    pub enrol_min_secs: f64,
    /// Speech starting this soon after the wake is the owner's first sentence.
    pub first_turn_secs: f64,
    pub enrol_cap_secs: f64,
    // SET, NOT FITTED — from tools/voice_gate_eval.py replaying sherpa-onnx's three sample speakers (clean,
    // read, Mandarin, 2026-09-21) with each in turn as the owner: every OTHER voice scored ≤ 0.27, two owners
    // scored 0.45–0.59, and the third — a halting speaker — only 0.26–0.39 against his own enrolment. One
    // number cannot be right for every voice, and going deaf to the owner is the worse mistake, so the cut
    // sits just above the highest stranger rather than in the middle. Fit these on the owner's own room
    // before trusting them.
    pub t_loose: f64,
    pub t_accept: f64,
    pub t_drop: f64,
    pub t_adapt: f64,
    pub t_seed_floor: f64,
    /// This much speech rejected with NOTHING accepted yet: open up.
    pub open_after_rejected_secs: f64,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            mode: DEFAULT_MODE.to_string(),
            model: DEFAULT_MODEL.to_string(),
            speech_rms: 0.012,
            hang_blocks: 4,
            hold_secs: 0.6,
            hold_more_secs: 1.5,
            continue_secs: 1.0,
            min_decide_secs: 0.5,
            rescore_secs: 0.5,
            window_secs: 2.0,
            enrol_min_secs: 3.0,
            first_turn_secs: 10.0,
            enrol_cap_secs: 20.0,
            t_loose: 0.30,
            t_accept: 0.33,
            t_drop: 0.27,
            t_adapt: 0.50,
            t_seed_floor: 0.20,
            open_after_rejected_secs: 12.0,
        }
    }
}

impl GateConfig {
    /// The defaults with another mode: Python's `GateConfig(mode=...)`.
    pub fn with_mode(mode: &str) -> Self {
        Self { mode: mode.to_string(), ..Self::default() }
    }
}

/// The configuration from the process environment: Python's `configured()`.
pub fn configured() -> GateConfig {
    // A value that is not UTF-8 is read lossily rather than as unset: Python sees it (surrogateescape), so an
    // unreadable mode still warns and falls back to off.
    configured_from(|k| std::env::var_os(k).map(|v| v.to_string_lossy().into_owned()))
}

/// The configuration from any environment lookup: Python's `configured(environ)`.
pub fn configured_from<F: Fn(&str) -> Option<String>>(env: F) -> GateConfig {
    let raw = env(MODE_VAR).unwrap_or_default();
    let mut mode = py_strip(&raw).to_lowercase();
    if mode.is_empty() {
        mode = DEFAULT_MODE.to_string();
    }
    if !MODES.contains(&mode.as_str()) {
        warn!("voice gate: {}={} is not one of {}; off", MODE_VAR, py_repr(&mode), MODES.join("|"));
        mode = "off".to_string();
    }
    let model_raw = env(MODEL_VAR).filter(|v| !v.is_empty()).unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let model = py_strip(&model_raw);
    let model = if model.is_empty() { DEFAULT_MODEL.to_string() } else { model.to_string() };
    GateConfig { mode, model, ..GateConfig::default() }
}

pub fn model_present(config: &GateConfig) -> bool {
    Path::new(&expanduser(&config.model)).is_file()
}

// ---- the embedder ------------------------------------------------------------------------------------

/// A voice embedding of int16 mono PCM at [`SAMPLE_RATE`], `Ok(None)` when it cannot make one. An `Err` is
/// what the Python embedder raised: the gate opens for the rest of the conversation.
pub trait Embedder: Send {
    fn embed(&mut self, pcm: &[u8]) -> Result<Option<Vec<f64>>, GateError>;
}

/// sherpa-onnx's speaker-embedding extractor. One thread, CPU: ~10 ms for two seconds on this Mac.
///
/// The official `sherpa-onnx` crate, the same release (1.13.8) as the Python wheel the daemon runs.
pub struct SherpaEmbedder {
    extractor: sherpa_onnx::SpeakerEmbeddingExtractor,
}

impl SherpaEmbedder {
    pub fn new(model: &str) -> Result<Self, GateError> {
        let path = expanduser(model);
        if !Path::new(&path).is_file() {
            return Err(GateError::new(
                "FileNotFoundError",
                format!("no speaker model at {path} (download {MODEL_URL})"),
            ));
        }
        let config = sherpa_onnx::SpeakerEmbeddingExtractorConfig {
            model: Some(path.clone()),
            num_threads: 1,
            debug: false,
            provider: Some("cpu".to_string()),
        };
        let extractor = sherpa_onnx::SpeakerEmbeddingExtractor::create(&config)
            .ok_or_else(|| GateError::new("RuntimeError", format!("cannot load the speaker model at {path}")))?;
        Ok(Self { extractor })
    }
}

impl Embedder for SherpaEmbedder {
    fn embed(&mut self, pcm: &[u8]) -> Result<Option<Vec<f64>>, GateError> {
        if !pcm.len().is_multiple_of(2) {
            // numpy.frombuffer's refusal
            return Err(GateError::new("ValueError", "buffer size must be a multiple of element size"));
        }
        let samples: Vec<f32> = pcm
            .chunks_exact(2)
            .map(|c| f32::from(i16::from_ne_bytes([c[0], c[1]])) / 32768.0)
            .collect();
        if samples.len() < (SAMPLE_RATE / 5) as usize {
            return Ok(None);
        }
        let stream = self
            .extractor
            .create_stream()
            .ok_or_else(|| GateError::new("RuntimeError", "sherpa-onnx could not create a stream"))?;
        stream.accept_waveform(SAMPLE_RATE as i32, &samples);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            return Ok(None);
        }
        let v = self
            .extractor
            .compute(&stream)
            .ok_or_else(|| GateError::new("RuntimeError", "sherpa-onnx returned no embedding"))?;
        Ok(Some(v.into_iter().map(f64::from).collect()))
    }
}

// ---- arithmetic --------------------------------------------------------------------------------------

/// RMS of int16 PCM as a fraction of full scale.
pub fn rms(pcm: &[u8]) -> f64 {
    let n = pcm.len() / 2;
    if n == 0 {
        return 0.0;
    }
    let total: u128 = pcm
        .chunks_exact(2)
        .map(|c| {
            let v = i64::from(i16::from_ne_bytes([c[0], c[1]]));
            (v * v) as u128
        })
        .sum();
    (py_int_div(total, n as u128)).sqrt() / 32768.0
}

/// `dot / (|a| |b|)`, 0.0 when either is the zero vector. Vectors of different lengths are Python's
/// `zip(strict=True)` ValueError.
pub fn cosine(a: &[f64], b: &[f64]) -> Result<f64, GateError> {
    check_same_len(a, b)?;
    let dot = py_sum(a.iter().zip(b).map(|(x, y)| x * y));
    let na = py_sum(a.iter().map(|x| x * x)).sqrt();
    let nb = py_sum(b.iter().map(|y| y * y)).sqrt();
    Ok(if na > 0.0 && nb > 0.0 { dot / (na * nb) } else { 0.0 })
}

fn unit(v: &[f64]) -> Vec<f64> {
    let n = py_sum(v.iter().map(|x| x * x)).sqrt();
    if n > 0.0 {
        v.iter().map(|x| x / n).collect()
    } else {
        v.to_vec()
    }
}

/// The span from the first loud block to the last: the wake snapshot is mostly the room before the word.
pub fn trim_to_speech(pcm: &[u8], speech_rms: f64, block_bytes: usize) -> Vec<u8> {
    let blocks: Vec<&[u8]> = pcm.chunks(block_bytes.max(1)).collect();
    let loud: Vec<usize> = (0..blocks.len()).filter(|&i| rms(blocks[i]) >= speech_rms).collect();
    match (loud.first(), loud.last()) {
        (Some(&first), Some(&last)) => blocks[first..=last].concat(),
        _ => Vec::new(),
    }
}

fn check_same_len(a: &[f64], b: &[f64]) -> Result<(), GateError> {
    if a.len() == b.len() {
        return Ok(());
    }
    let which = if b.len() < a.len() { "shorter" } else { "longer" };
    Err(GateError::new("ValueError", format!("zip() argument 2 is {which} than argument 1")))
}

// ---- decisions and stats -----------------------------------------------------------------------------

/// One judgement of one segment, for the log and the eval. Never words, never audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub at: f64,
    pub secs: f64,
    /// provisional | locked
    pub state: String,
    pub score: Option<f64>,
    pub threshold: f64,
    pub accepted: bool,
    /// match | mismatch | too_short | continues | embed_failed | lenient | first_turn | enrolling
    pub why: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub segments: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub secs_forwarded: f64,
    pub secs_silenced: f64,
    pub enrolled_secs: f64,
    /// Why the gate stopped judging for this conversation, "" if it did not.
    pub opened: String,
    pub decisions: Vec<Decision>,
}

/// A clock in seconds: Python's `clock: Callable[[], float]`.
pub type Clock = Box<dyn Fn() -> f64 + Send>;

static MONOTONIC_ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `time.monotonic()`: seconds on a clock that never goes back. Only differences are used.
pub fn monotonic() -> f64 {
    MONOTONIC_ORIGIN.elapsed().as_secs_f64()
}

// ---- the gate ----------------------------------------------------------------------------------------

/// `process(block)` returns the blocks to append now — the block itself, silence of its length, nothing
/// while a decision is pending, or a held burst. In `shadow` mode the decisions are made and the audio that
/// comes out is exactly the audio that went in.
pub struct SpeakerGate {
    pub config: GateConfig,
    pub embedder: Option<Box<dyn Embedder>>,
    /// A think-aloud lesson: eve takes notes on whoever speaks.
    pub bypass: bool,
    /// A yes/no is awaited or a task runs: a short "stop" must never be lost.
    pub lenient: bool,
    pub stats: Stats,
    clock: Clock,
    block_bytes: usize,
    seed: Option<Vec<f64>>,
    centroid: Option<Vec<f64>>,
    woke_at: f64,
    /// The current segment's audio so far.
    segment: Vec<Vec<u8>>,
    /// Blocks not yet released (the first HOLD of a segment).
    held: Vec<Vec<u8>>,
    /// Whether the block being processed is the last one in `held` (Python's `self._held[-1] is block`).
    held_has_block: bool,
    quiet: u32,
    /// The current segment: None undecided, true forward, false silence.
    verdict: Option<bool>,
    /// Segment seconds at the last score.
    scored_at: f64,
    segment_started: f64,
    rejected_secs: f64,
    low_streak: u32,
    /// Loud blocks in the current segment.
    voiced: u64,
    /// The previous judged segment, for a fragment that follows it.
    last_verdict: Option<bool>,
    last_ended: f64,
}

impl fmt::Debug for SpeakerGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpeakerGate")
            .field("config", &self.config)
            .field("state", &self.state())
            .field("bypass", &self.bypass)
            .field("lenient", &self.lenient)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

/// ears' block size in bytes: `int(SAMPLE_RATE * BLOCK_SECS) * 2`.
pub fn default_block_bytes() -> usize {
    (f64::from(SAMPLE_RATE) * BLOCK_SECS) as usize * 2
}

impl SpeakerGate {
    /// Python's `SpeakerGate(config, embedder)` with `time.monotonic` and ears' block size.
    pub fn new(config: GateConfig, embedder: Option<Box<dyn Embedder>>) -> Self {
        Self::with_clock(config, embedder, Box::new(monotonic), default_block_bytes())
    }

    pub fn with_clock(
        config: GateConfig,
        embedder: Option<Box<dyn Embedder>>,
        clock: Clock,
        block_bytes: usize,
    ) -> Self {
        let woke_at = clock();
        let mut stats = Stats::default();
        if embedder.is_none() {
            stats.opened = "no embedder".to_string();
        }
        Self {
            config,
            embedder,
            bypass: false,
            lenient: false,
            stats,
            clock,
            block_bytes,
            seed: None,
            centroid: None,
            woke_at,
            segment: Vec::new(),
            held: Vec::new(),
            held_has_block: false,
            quiet: 0,
            verdict: None,
            scored_at: 0.0,
            segment_started: 0.0,
            rejected_secs: 0.0,
            low_streak: 0,
            voiced: 0,
            last_verdict: None,
            last_ended: -1e9,
        }
    }

    // -- enrolment --

    /// The audio before the wake word fired. False, and an open gate, when it holds no usable voice. An
    /// `Err` is what the embedder raised (Python lets it propagate to `build`).
    pub fn enrol_seed(&mut self, pcm: &[u8]) -> Result<bool, GateError> {
        let speech = trim_to_speech(pcm, self.config.speech_rms, self.block_bytes);
        let vector = if speech.is_empty() { None } else { self.embed(&speech)? };
        let Some(vector) = vector else {
            if self.stats.opened.is_empty() {
                self.stats.opened = "no seed".to_string();
            }
            return Ok(false);
        };
        let seed = unit(&vector);
        self.centroid = Some(seed.clone());
        self.seed = Some(seed);
        self.stats.enrolled_secs = speech.len() as f64 / self.block_bytes as f64 * BLOCK_SECS;
        Ok(true)
    }

    /// `open` | `provisional` | `locked`.
    pub fn state(&self) -> &'static str {
        if !self.stats.opened.is_empty() || self.seed.is_none() {
            return "open";
        }
        if self.stats.enrolled_secs >= self.config.enrol_min_secs {
            "locked"
        } else {
            "provisional"
        }
    }

    pub fn reset(&mut self) {
        self.seed = None;
        self.centroid = None;
        self.segment.clear();
        self.held.clear();
        self.stats = Stats { opened: "reset".to_string(), ..Stats::default() };
    }

    // -- the stream --

    pub fn process(&mut self, block: &[u8]) -> Vec<Vec<u8>> {
        if self.bypass || self.state() == "open" || self.config.mode == "off" {
            // Blocks held for a decision that will not come now go out first: _end_segment drops them, and
            // that was up to the first 1.5 s of a sentence. In shadow they were heard as they came.
            let held = std::mem::take(&mut self.held);
            let mut out = if self.config.mode == "shadow" { Vec::new() } else { held };
            self.end_segment();
            out.push(block.to_vec());
            return out;
        }
        self.held_has_block = false;
        let out = match self.process_inner(block) {
            Ok(out) => out,
            Err(e) => {
                // a gate that breaks must never deafen eve
                warn!("voice gate: {}: {} — open for the rest of this conversation", e.kind, e.message);
                self.stats.opened = format!("error: {}", e.kind);
                let mut out = std::mem::take(&mut self.held);
                if !self.held_has_block || out.is_empty() {
                    out.push(block.to_vec());
                }
                self.segment.clear();
                out
            }
        };
        if self.config.mode == "on" {
            out
        } else {
            self.shadow(block)
        }
    }

    fn shadow(&self, block: &[u8]) -> Vec<Vec<u8>> {
        vec![block.to_vec()] // shadow: judged and counted as `on` would, heard exactly as the mic heard it
    }

    fn process_inner(&mut self, block: &[u8]) -> Result<Vec<Vec<u8>>, GateError> {
        let loud = rms(block) >= self.config.speech_rms;
        if self.segment.is_empty() {
            if !loud {
                return Ok(vec![block.to_vec()]);
            }
            self.segment_started = (self.clock)();
            self.verdict = None;
            self.scored_at = 0.0;
            self.quiet = 0;
            self.low_streak = 0;
            self.voiced = 0;
            self.stats.segments += 1;
        }
        self.quiet = if loud { 0 } else { self.quiet + 1 };
        self.voiced += u64::from(loud);
        self.segment.push(block.to_vec());
        let secs = self.segment.len() as f64 * BLOCK_SECS;
        let ended = self.quiet >= self.config.hang_blocks;

        if self.verdict.is_none() {
            self.held.push(block.to_vec());
            self.held_has_block = true;
            // Judged on VOICED time: the quiet after a one-word answer must not count toward the hold, or
            // "yes" plus its own trailing silence gets embedded and judged like a sentence.
            let voiced = self.voiced as f64 * BLOCK_SECS;
            if voiced >= self.config.hold_secs || ended {
                let last_word = ended || voiced >= self.config.hold_more_secs;
                let Some(verdict) = self.judge(secs, ended, last_word)? else {
                    return Ok(Vec::new()); // not them on 0.6 s: keep holding, judge on more
                };
                self.verdict = Some(verdict);
                self.scored_at = secs;
                let burst = std::mem::take(&mut self.held);
                let out: Vec<Vec<u8>> =
                    if verdict { burst } else { burst.iter().map(|b| vec![0u8; b.len()]).collect() };
                self.count(&out, verdict);
                if ended {
                    self.end_segment();
                }
                return Ok(out);
            }
            return Ok(Vec::new());
        }

        if !ended && secs - self.scored_at >= self.config.rescore_secs {
            self.verdict = Some(self.rejudge(secs)?);
        }
        let forward = self.verdict == Some(true);
        let out = if forward { vec![block.to_vec()] } else { vec![vec![0u8; block.len()]] };
        self.count(&out, forward);
        if ended {
            self.end_segment();
        }
        Ok(out)
    }

    fn count(&mut self, blocks: &[Vec<u8>], forwarded: bool) {
        let total: usize = blocks.iter().map(Vec::len).sum();
        let secs = total as f64 / self.block_bytes as f64 * BLOCK_SECS;
        if forwarded {
            self.stats.secs_forwarded += secs;
        } else {
            self.stats.secs_silenced += secs;
        }
    }

    fn end_segment(&mut self) {
        if self.segment.is_empty() {
            return;
        }
        let secs = self.segment.len() as f64 * BLOCK_SECS;
        match self.verdict {
            Some(false) => {
                self.stats.rejected += 1;
                self.rejected_secs += secs;
                if self.stats.accepted == 0 && self.rejected_secs >= self.config.open_after_rejected_secs {
                    // Nothing has EVER matched since the wake: the enrolment is what is wrong (the word was
                    // said over the TV, a bad seed), not the room. Once something has matched this never
                    // fires — a TV that talks for a minute is rejected for a minute, the conversation idles
                    // out, and the next "hey eve" enrols afresh.
                    self.stats.opened = "nothing matched the wake voice".to_string();
                    info!(
                        "voice gate: {:.0} s of speech rejected and none accepted — open for this conversation",
                        self.rejected_secs
                    );
                }
            }
            Some(true) => self.stats.accepted += 1,
            None => {}
        }
        if self.verdict.is_some() {
            self.last_verdict = self.verdict;
            self.last_ended = (self.clock)();
        }
        self.segment.clear();
        self.held.clear();
        self.verdict = None;
    }

    // -- judging --

    fn embed(&mut self, pcm: &[u8]) -> Result<Option<Vec<f64>>, GateError> {
        match self.embedder.as_mut() {
            Some(e) if !pcm.is_empty() => e.embed(pcm),
            _ => Ok(None),
        }
    }

    /// Some(true) forward, Some(false) silence, None "not convinced yet" (only when `commit` is false).
    fn judge(&mut self, secs: f64, is_final: bool, commit: bool) -> Result<Option<bool>, GateError> {
        let state = self.state();
        let voiced = self.voiced as f64 * BLOCK_SECS;
        if is_final && voiced < self.config.min_decide_secs {
            let recent = self.last_verdict.is_some()
                && self.segment_started - self.last_ended <= self.config.continue_secs;
            if recent && !self.lenient && state == "locked" {
                // the same speaker going on after a breath: a halting voice, the owner's or the TV's
                let v = self.last_verdict == Some(true);
                return Ok(Some(self.decide(secs, state, None, 0.0, v, "continues")));
            }
            let why = if self.lenient { "lenient" } else { "too_short" };
            return Ok(Some(self.decide(secs, state, None, 0.0, true, why)));
        }
        let audio = self.segment.concat();
        let vector = self.embed(&audio)?;
        let (Some(vector), Some(centroid)) = (vector, self.centroid.as_ref()) else {
            return Ok(Some(self.decide(secs, state, None, 0.0, true, "embed_failed")));
        };
        let u = unit(&vector);
        let score = cosine(&u, centroid)?;
        let first_turn =
            state == "provisional" && self.segment_started - self.woke_at <= self.config.first_turn_secs;
        if state == "provisional" {
            if first_turn || score >= self.config.t_loose {
                self.adapt(&u, score, voiced, true)?;
            }
            let why = if first_turn { "first_turn" } else { "enrolling" };
            return Ok(Some(self.decide(secs, state, Some(score), self.config.t_loose, true, why)));
        }
        let threshold = self.config.t_accept;
        let accepted = score >= threshold;
        if !accepted && !commit && !self.lenient {
            return Ok(None);
        }
        if !accepted && self.lenient && secs < 1.5 {
            return Ok(Some(self.decide(secs, state, Some(score), threshold, true, "lenient")));
        }
        if accepted {
            self.adapt(&u, score, voiced, false)?;
        }
        let why = if accepted { "match" } else { "mismatch" };
        Ok(Some(self.decide(secs, state, Some(score), threshold, accepted, why)))
    }

    /// As a segment goes on: the last `window_secs` against the centroid. Two low scores in a row stop a
    /// forwarded segment (someone else took over); one good score starts a silenced one (the owner spoke up).
    fn rejudge(&mut self, secs: f64) -> Result<bool, GateError> {
        self.scored_at = secs;
        if self.state() == "provisional" {
            // Still enrolling: keep forwarding, and keep folding the owner's first sentence in as it goes on.
            if self.segment_started - self.woke_at <= self.config.first_turn_secs {
                let tail = py_tail(&self.segment, (self.config.rescore_secs / BLOCK_SECS) as i64).concat();
                let vector = self.embed(&tail)?;
                if let (Some(vector), true) = (vector, self.centroid.is_some()) {
                    self.adapt(&unit(&vector), 1.0, self.config.rescore_secs, true)?;
                }
            }
            return Ok(true);
        }
        let window = py_tail(&self.segment, (self.config.window_secs / BLOCK_SECS) as i64).concat();
        let vector = self.embed(&window)?;
        let current = self.verdict == Some(true);
        let (Some(vector), Some(centroid)) = (vector, self.centroid.as_ref()) else {
            return Ok(current);
        };
        let u = unit(&vector);
        let score = cosine(&u, centroid)?;
        if current {
            self.low_streak = if score < self.config.t_drop { self.low_streak + 1 } else { 0 };
            let keep = self.low_streak < 2 || self.lenient; // an awaited answer is never cut off mid-word
            if keep && score >= self.config.t_accept {
                self.adapt(&u, score, self.config.rescore_secs, false)?;
            }
            if !keep {
                let state = self.state();
                self.decide(secs, state, Some(score), self.config.t_drop, false, "mismatch");
            }
            return Ok(keep);
        }
        if score >= self.config.t_accept {
            let state = self.state();
            self.decide(secs, state, Some(score), self.config.t_accept, true, "match");
            return Ok(true);
        }
        Ok(false)
    }

    fn adapt(&mut self, u: &[f64], score: f64, secs: f64, first_turn: bool) -> Result<(), GateError> {
        let (Some(seed), Some(centroid)) = (self.seed.as_ref(), self.centroid.as_ref()) else {
            return Err(GateError::new("AssertionError", ""));
        };
        if self.stats.enrolled_secs >= self.config.enrol_cap_secs {
            return Ok(());
        }
        if !first_turn && (score < self.config.t_adapt || cosine(u, seed)? < self.config.t_seed_floor) {
            return Ok(());
        }
        check_same_len(centroid, u)?;
        let have = self.stats.enrolled_secs;
        let mixed: Vec<f64> = centroid.iter().zip(u).map(|(c, x)| c * have + x * secs).collect();
        self.centroid = Some(unit(&mixed));
        self.stats.enrolled_secs = self.config.enrol_cap_secs.min(have + secs);
        Ok(())
    }

    fn decide(&mut self, secs: f64, state: &str, score: Option<f64>, threshold: f64, accepted: bool, why: &str) -> bool {
        let d = Decision {
            at: (self.clock)() - self.woke_at,
            secs: py_round(secs, 2),
            state: state.to_string(),
            score: score.map(|s| py_round(s, 3)),
            threshold,
            accepted,
            why: why.to_string(),
        };
        self.stats.decisions.push(d);
        info!(
            "voice gate[{}]: {} {:.1} s {} score={} cut={:.2} ({}, enrolled {:.1} s)",
            self.config.mode,
            state,
            secs,
            if accepted { "forward" } else { "silence" },
            score.map_or_else(|| "-".to_string(), |s| format!("{s:.2}")),
            threshold,
            why,
            self.stats.enrolled_secs
        );
        accepted
    }

    /// The conversation's numbers, in Python's key order.
    pub fn summary(&self) -> Value {
        let s = &self.stats;
        json!({
            "mode": self.config.mode,
            "segments": s.segments,
            "accepted": s.accepted,
            "rejected": s.rejected,
            "secs_forwarded": py_round(s.secs_forwarded, 1),
            "secs_silenced": py_round(s.secs_silenced, 1),
            "enrolled_secs": py_round(s.enrolled_secs, 1),
            "opened": s.opened,
        })
    }
}

/// The gate for one conversation, enrolled from the wake audio — or None when it is off or there is no wake
/// audio (a conversation the lesson toggle opened). Never fails: a gate that cannot be built is no gate.
/// `embedder` None loads [`SherpaEmbedder`] from `config.model`.
pub fn build(
    config: GateConfig,
    wake_audio: &[u8],
    clock: Clock,
    embedder: Option<Box<dyn Embedder>>,
) -> Option<SpeakerGate> {
    if config.mode == "off" || wake_audio.is_empty() {
        return None;
    }
    let mode = config.mode.clone();
    let attempt = || -> Result<SpeakerGate, GateError> {
        let embedder: Box<dyn Embedder> = match embedder {
            Some(e) => e,
            None => Box::new(SherpaEmbedder::new(&config.model)?),
        };
        let mut gate = SpeakerGate::with_clock(config, Some(embedder), clock, default_block_bytes());
        let enrolled = gate.enrol_seed(wake_audio)?;
        if enrolled {
            info!("voice gate[{}]: enrolled {:.1} s from the wake", mode, gate.stats.enrolled_secs);
        } else {
            info!("voice gate[{}]: no usable wake voice — open", mode);
        }
        Ok(gate)
    };
    match attempt() {
        Ok(gate) => Some(gate),
        Err(e) => {
            warn!("voice gate: off for this conversation ({}: {})", e.kind, e.message);
            None
        }
    }
}

/// [`build`] with `time.monotonic` and sherpa-onnx: what the session calls.
pub fn build_default(config: GateConfig, wake_audio: &[u8]) -> Option<SpeakerGate> {
    build(config, wake_audio, Box::new(monotonic), None)
}

// ---- the wake ring -----------------------------------------------------------------------------------

/// The last few seconds of microphone audio, in RAM only, so the wake utterance can enrol a voice.
#[derive(Debug, Clone)]
pub struct WakeRing {
    blocks: VecDeque<Vec<u8>>,
    maxlen: usize,
}

impl Default for WakeRing {
    fn default() -> Self {
        Self::new(RING_SECS)
    }
}

impl WakeRing {
    pub fn new(secs: f64) -> Self {
        let n = (secs / BLOCK_SECS).round_ties_even();
        let maxlen = if n >= 1.0 { n as usize } else { 1 };
        Self { blocks: VecDeque::with_capacity(maxlen), maxlen }
    }

    pub fn add(&mut self, raw: &[u8]) {
        if self.blocks.len() == self.maxlen {
            self.blocks.pop_front();
        }
        self.blocks.push_back(raw.to_vec());
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.blocks.iter().flatten().copied().collect()
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
    }
}

// ---- Python semantics --------------------------------------------------------------------------------

/// CPython 3.12+'s `sum()` of floats: Neumaier-compensated, starting from the int 0.
fn py_sum<I: IntoIterator<Item = f64>>(it: I) -> f64 {
    let mut f = 0.0f64;
    let mut c = 0.0f64;
    for x in it {
        let t = f + x;
        if f.abs() >= x.abs() {
            c += (f - t) + x;
        } else {
            c += (x - t) + f;
        }
        f = t;
    }
    if c != 0.0 && c.is_finite() {
        f += c;
    }
    f
}

/// Python's `int / int`: correctly rounded. Exact here whenever both fit in 53 bits, which every block does.
fn py_int_div(a: u128, b: u128) -> f64 {
    if a < (1 << 53) && b < (1 << 53) {
        return a as f64 / b as f64;
    }
    let q = a / b;
    let r = a % b;
    q as f64 + r as f64 / b as f64
}

/// Python's `round(x, n)`: correctly rounded to `n` decimals (Rust's fixed formatting rounds the same way).
fn py_round(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.n$}").parse().unwrap_or(x)
}

/// `seq[-k:]` with Python's slicing (k == 0 is the whole sequence).
fn py_tail<T>(seq: &[T], k: i64) -> &[T] {
    let len = seq.len() as i64;
    let start = -k;
    let start = if start < 0 { (len + start).max(0) } else { start.min(len) };
    &seq[start as usize..]
}

fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// Python's `repr()` of a str.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (0x80..0xa0).contains(&(c as u32)) => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `os.path.expanduser`: `~` and `~/…` from HOME (else the password database), `~user/…` from the password
/// database; anything it cannot resolve is returned as written.
fn expanduser(path: &str) -> String {
    if !path.starts_with('~') {
        return path.to_string();
    }
    let i = path[1..].find('/').map_or(path.len(), |j| j + 1);
    let home = if i == 1 {
        match std::env::var_os("HOME") {
            Some(h) => Some(h.to_string_lossy().into_owned()),
            None => passwd_home(None),
        }
    } else {
        passwd_home(Some(&path[1..i]))
    };
    let Some(home) = home else {
        return path.to_string();
    };
    let joined = format!("{}{}", home.trim_end_matches('/'), &path[i..]);
    if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    }
}

/// A home folder from the password database: the current user's, or a named one's.
fn passwd_home(name: Option<&str>) -> Option<String> {
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call; `buf` backs the strings `pwd` points into.
    let rc = unsafe {
        match name {
            None => libc::getpwuid_r(libc::getuid(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result),
            Some(n) => {
                let c = CString::new(n).ok()?;
                libc::getpwnam_r(c.as_ptr(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result)
            }
        }
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: pw_dir is a NUL-terminated string inside `buf`.
    let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
    Some(OsStr::from_bytes(dir.to_bytes()).to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests;
