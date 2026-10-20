//! Ears: the "hey eve" wake word, and the "lesson" word, on the Mac's microphone.
//!
//! The daemon keeps one microphone stream open while the robot is connected
//! (24 kHz mono int16, 100 ms blocks; `EVE_MIC_ALWAYS=1` keeps it open from
//! boot) and runs every block through a sherpa-onnx keyword spotter — a
//! 3.3M-parameter streaming zipformer that needs no training for a new
//! phrase: the phrase is written as BPE tokens into a keywords file and the
//! spotter fires when the decoded lattice contains it. Bench 2026-09-06 on an
//! M-series Mac: a 1.9 s clip decodes in 0.02 s, so this costs ~1% of a core.
//!
//! Two consumers share the stream. The spotter listens all the time; while a
//! voice session is open (voice_agent) the same PCM is forwarded to the
//! realtime model through `subscribe()`, and the spotter is muted so eve
//! does not wake itself on its own replies.
//!
//! Everything that touches hardware or the model is behind two small seams —
//! `Spotter` (the model) and `AudioBackend` (sounddevice) — so the gating
//! logic (`WakeGate`) and the stream plumbing are unit-testable without a mic.
//!
//! Why sherpa-onnx and not the usual suspects (research 2026-09-06):
//! openWakeWord's ONNX path returns ~0 scores on Apple Silicon (issue #336)
//! and is 2.5 years stale; Porcupine dropped its free tier on 2026-06-30;
//! livekit-wakeword needs a GPU training run per phrase (a later upgrade —
//! see docs/stackchan/voice.md).

pub mod audio;
pub mod py;
pub mod sentencepiece;

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use regex::Regex;
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

pub use audio::{AudioBackend, BlockCallback, CpalBackend, DeviceInfo, InputSpec, InputStream, StatusCallback};
use eve_voice_gate::WakeRing;

pub const DEFAULT_WAKE_WORD: &str = "hey eve";
/// A second phrase in the same spotter. Heard on its own it opens a conversation that
/// starts a lesson at once, with no "teach me" to route. EVE_LESSON_WORD=off disables it.
pub const DEFAULT_LESSON_WORD: &str = "lesson";
pub const DEFAULT_LESSON_THRESHOLD: f64 = 0.35;
pub const DEFAULT_MODEL_DIR: &str =
    "~/.config/eve-bridge/models/sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01";
pub const MODEL_URL: &str = concat!(
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models/",
    "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01.tar.bz2"
);
/// What the realtime model wants; the spotter resamples to 16 k itself.
pub const SAMPLE_RATE: u32 = 24000;
pub const BLOCK_SECS: f64 = 0.1;
/// A keyword fires at most once per COOLDOWN after a hit; a spoken "hey eve"
/// can otherwise trigger twice as the lattice settles.
pub const DEFAULT_COOLDOWN_SECS: f64 = 2.0;
/// Boosting score / threshold from the sherpa-onnx KWS docs; 0.25 fired on a
/// synthesized clip and stayed quiet on a control sentence (bench).
pub const DEFAULT_BOOST: f64 = 2.0;
pub const DEFAULT_THRESHOLD: f64 = 0.25;
/// RMS of int16 samples below this for SILENCE_WARN_SECS → the mic is dead
/// (no TCC grant, wrong device). Ambient on the bench read ~90.
pub const SILENCE_RMS: f64 = 3.0;
pub const SILENCE_WARN_SECS: f64 = 10.0;

const ENCODER: &str = "encoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx";
const DECODER: &str = "decoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx";
const JOINER: &str = "joiner-epoch-12-avg-2-chunk-16-left-64.int8.onnx";

/// `int(SAMPLE_RATE * BLOCK_SECS)`: 2400 samples.
pub fn block_frames() -> usize {
    (f64::from(SAMPLE_RATE) * BLOCK_SECS) as usize
}

#[derive(Clone, Debug, PartialEq)]
pub struct EarsConfig {
    pub enabled: bool,
    pub wake_word: String,
    /// "" = no lesson word.
    pub lesson_word: String,
    pub model_dir: PathBuf,
    pub threshold: f64,
    /// The lesson word's own threshold, written per keyword. Bench 2026-09-15, synthesized voices:
    /// at 0.25 "listen" fired 1 in 3, at 0.35 never, and "lesson" alone fired 3 in 3 at both.
    pub lesson_threshold: f64,
    pub boost: f64,
    pub cooldown_secs: f64,
    /// sounddevice input name substring; None = default.
    pub device: Option<String>,
    /// Open the mic at boot, not only while the robot is connected.
    pub always: bool,
}

impl Default for EarsConfig {
    fn default() -> Self {
        EarsConfig {
            enabled: true,
            wake_word: DEFAULT_WAKE_WORD.into(),
            lesson_word: DEFAULT_LESSON_WORD.into(),
            model_dir: py::expand_path(DEFAULT_MODEL_DIR),
            threshold: DEFAULT_THRESHOLD,
            lesson_threshold: DEFAULT_LESSON_THRESHOLD,
            boost: DEFAULT_BOOST,
            cooldown_secs: DEFAULT_COOLDOWN_SECS,
            device: None,
            always: false,
        }
    }
}

/// `configured()` over the process environment.
pub fn configured() -> EarsConfig {
    configured_from(|k| std::env::var(k).ok())
}

/// `configured(environ)`: `env` returns a variable's value, None when unset.
pub fn configured_from<F: Fn(&str) -> Option<String>>(env: F) -> EarsConfig {
    // Python's `env.get(k) or ""`: an empty value counts as unset.
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    let raw = py::strip(&get("EVE_VOICE").unwrap_or_default()).to_lowercase();
    let enabled = !matches!(raw.as_str(), "0" | "false" | "no" | "off");
    let word = get("EVE_WAKE_WORD").unwrap_or_else(|| DEFAULT_WAKE_WORD.into());
    let word = match py::strip(&word) {
        "" => DEFAULT_WAKE_WORD.to_string(),
        w => w.to_string(),
    };
    let mut lesson_word = match env("EVE_LESSON_WORD") {
        None => DEFAULT_LESSON_WORD.to_string(),
        Some(v) => py::strip(&v).to_string(),
    };
    if matches!(lesson_word.to_lowercase().as_str(), "0" | "off" | "false" | "no" | "none") {
        lesson_word = String::new();
    }
    let model_dir = py::expand_path(&get("EVE_KWS_MODEL_DIR").unwrap_or_else(|| DEFAULT_MODEL_DIR.into()));
    let threshold = float_knob(&get("EVE_WAKE_THRESHOLD").unwrap_or_default(), "EVE_WAKE_THRESHOLD", DEFAULT_THRESHOLD);
    let lesson_threshold = float_knob(
        &get("EVE_LESSON_THRESHOLD").unwrap_or_default(),
        "EVE_LESSON_THRESHOLD",
        DEFAULT_LESSON_THRESHOLD,
    );
    let device = Some(py::strip(&get("EVE_MIC").unwrap_or_default()).to_string()).filter(|d| !d.is_empty());
    let always = matches!(
        py::strip(&get("EVE_MIC_ALWAYS").unwrap_or_default()).to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    );
    EarsConfig {
        enabled,
        wake_word: word,
        lesson_word,
        model_dir,
        threshold,
        lesson_threshold,
        device,
        always,
        ..EarsConfig::default()
    }
}

fn float_knob(raw: &str, name: &str, default: f64) -> f64 {
    let raw = py::strip(raw);
    if raw.is_empty() {
        return default;
    }
    match py::parse_float(raw) {
        Some(v) => v,
        None => {
            warn!("ears: {}={} is not a number; using {}", name, py::repr(raw), py::float_str(default));
            default
        }
    }
}

// ---- keyword file ---------------------------------------------------------------

static NON_ALNUM: LazyLock<Regex> = LazyLock::new(|| Regex::new("[^a-z0-9]+").expect("valid regex"));

/// "hey eve" -> "hey_eve": the tag the spotter returns on a hit.
pub fn keyword_id(phrase: &str) -> String {
    let lowered = py::strip(phrase).to_lowercase();
    let id = NON_ALNUM.replace_all(&lowered, "_");
    match id.trim_matches('_') {
        "" => "wake".to_string(),
        s => s.to_string(),
    }
}

/// One line of a sherpa-onnx keywords file: tokens, then :boost #threshold @id.
pub fn keyword_line(tokens: &[String], phrase: &str, boost: f64, threshold: f64) -> String {
    format!("{} :{} #{} @{}", tokens.join(" "), py::format_g(boost), py::format_g(threshold), keyword_id(phrase))
}

/// BPE tokens of the phrase in the KWS model's vocabulary (upper-case, as gigaspeech was trained).
pub fn encode_phrase(phrase: &str, bpe_model: &Path) -> Result<Vec<String>> {
    let sp = sentencepiece::SentencePieceProcessor::open(bpe_model)?;
    Ok(sp.encode(&py::strip(phrase).to_uppercase()))
}

pub fn write_keywords_file(config: &EarsConfig, path: Option<&Path>) -> Result<PathBuf> {
    write_keywords_file_with(config, path, encode_phrase)
}

/// `write_keywords_file` with the phrase encoder as a parameter (the Python tests patch `encode_phrase`).
pub fn write_keywords_file_with<E>(config: &EarsConfig, path: Option<&Path>, encode: E) -> Result<PathBuf>
where
    E: Fn(&str, &Path) -> Result<Vec<String>>,
{
    let bpe = config.model_dir.join("bpe.model");
    let mut phrases = vec![config.wake_word.clone()];
    if py::strip(&config.wake_word).to_lowercase() == "hey eve" {
        phrases.push("okay eve".into());
        phrases.push("ok eve".into());
    }
    let mut lines = Vec::new();
    for phrase in &phrases {
        lines.push(keyword_line(&encode(phrase, &bpe)?, phrase, config.boost, config.threshold));
    }
    let lesson = py::strip(&config.lesson_word);
    if !lesson.is_empty() && !phrases.iter().any(|p| keyword_id(p) == keyword_id(lesson)) {
        // Its own threshold: a per-keyword "#threshold" overrides the spotter's global one.
        lines.push(keyword_line(&encode(lesson, &bpe)?, lesson, config.boost, config.lesson_threshold));
    }
    let out = match path {
        Some(p) => p.to_path_buf(),
        None => config.model_dir.join(format!("keywords-{}.txt", keyword_id(&config.wake_word))),
    };
    std::fs::write(&out, lines.join("\n") + "\n").with_context(|| format!("could not write {}", out.display()))?;
    Ok(out)
}

pub fn model_present(model_dir: &Path) -> bool {
    ["tokens.txt", "bpe.model", ENCODER, DECODER, JOINER].iter().all(|f| model_dir.join(f).exists())
}

// ---- the spotter (model seam) ---------------------------------------------------

/// What `Ears` needs of a spotter: `feed(samples, rate)` returns the keyword id when the phrase completes
/// in this block, else None. `samples` is float32 in [-1, 1].
pub trait Spotter: Send {
    fn feed(&mut self, samples: &[f32], rate: u32) -> Option<String>;
}

/// sherpa-onnx KeywordSpotter over one streaming session. Any sample rate is fine — sherpa resamples to
/// the model's 16 kHz.
pub struct WakeSpotter {
    pub keywords_file: PathBuf,
    kws: sherpa_onnx::KeywordSpotter,
    stream: sherpa_onnx::OnlineStream,
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl WakeSpotter {
    pub fn new(config: &EarsConfig, keywords_file: Option<&Path>) -> Result<Self> {
        let m = &config.model_dir;
        if !model_present(m) {
            bail!("KWS model missing under {} — download {}", m.display(), MODEL_URL);
        }
        // Always (re)write the file: sherpa-onnx exit()s the whole process on a
        // missing keywords file rather than raising.
        let kw = write_keywords_file(config, keywords_file)?;
        if !kw.exists() {
            bail!("could not write keywords file {}", kw.display());
        }
        let mut c = sherpa_onnx::KeywordSpotterConfig::default();
        c.model_config.tokens = Some(path_string(&m.join("tokens.txt")));
        c.model_config.transducer.encoder = Some(path_string(&m.join(ENCODER)));
        c.model_config.transducer.decoder = Some(path_string(&m.join(DECODER)));
        c.model_config.transducer.joiner = Some(path_string(&m.join(JOINER)));
        c.model_config.num_threads = 1;
        c.model_config.provider = Some("cpu".into());
        c.max_active_paths = 4;
        c.keywords_file = Some(path_string(&kw));
        c.keywords_score = 1.0;
        c.keywords_threshold = config.threshold as f32;
        c.num_trailing_blanks = 1;
        let kws = sherpa_onnx::KeywordSpotter::create(&c)
            .ok_or_else(|| anyhow!("could not create the keyword spotter from {}", m.display()))?;
        let stream = kws.create_stream();
        Ok(WakeSpotter { keywords_file: kw, kws, stream })
    }
}

impl Spotter for WakeSpotter {
    fn feed(&mut self, samples: &[f32], rate: u32) -> Option<String> {
        self.stream.accept_waveform(rate as i32, samples);
        let mut hit = None;
        while self.kws.is_ready(&self.stream) {
            self.kws.decode(&self.stream);
            let r = self.kws.get_result(&self.stream).map(|r| py::strip(&r.keyword).to_string()).unwrap_or_default();
            if !r.is_empty() {
                hit = Some(r);
                self.kws.reset(&self.stream);
            }
        }
        hit
    }
}

// ---- gating (pure) --------------------------------------------------------------

pub type Suppressed = Arc<dyn Fn() -> bool + Send + Sync>;

/// Decides whether a spotter hit becomes a wake event.
///
/// suppressed(): the daemon's reasons not to wake — the listen key is down
/// (the human is dictating), a voice session is already open (eve would
/// hear itself). cooldown: one wake per window.
pub struct WakeGate {
    pub cooldown_secs: f64,
    pub suppressed: Suppressed,
    pub last_wake_at: f64,
    pub hits: u64,
    pub suppressed_hits: u64,
}

impl Default for WakeGate {
    fn default() -> Self {
        WakeGate {
            cooldown_secs: DEFAULT_COOLDOWN_SECS,
            suppressed: Arc::new(|| false),
            last_wake_at: f64::NEG_INFINITY,
            hits: 0,
            suppressed_hits: 0,
        }
    }
}

impl WakeGate {
    pub fn consider(&mut self, hit: Option<&str>, now: f64) -> Option<String> {
        let hit = hit.filter(|h| !h.is_empty())?;
        self.hits += 1;
        if (self.suppressed)() {
            self.suppressed_hits += 1;
            return None;
        }
        if now - self.last_wake_at < self.cooldown_secs {
            return None;
        }
        self.last_wake_at = now;
        Some(hit.to_string())
    }
}

/// numpy's pairwise summation of float32 (`np.add.reduce` on a contiguous array).
fn pairwise_sum_f32(a: &[f32]) -> f32 {
    let n = a.len();
    if n < 8 {
        let mut res = 0.0f32;
        for &x in a {
            res += x;
        }
        res
    } else if n <= 128 {
        let mut r = [0.0f32; 8];
        r.copy_from_slice(&a[..8]);
        let mut i = 8;
        while i < n - (n % 8) {
            for (k, rk) in r.iter_mut().enumerate() {
                *rk += a[i + k];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        pairwise_sum_f32(&a[..n2]) + pairwise_sum_f32(&a[n2..])
    }
}

/// `sqrt(mean(a * a))` with numpy's float32 arithmetic, as a float.
pub fn rms_int16(block: &[i16]) -> f64 {
    if block.is_empty() {
        return 0.0;
    }
    let sq: Vec<f32> = block.iter().map(|&s| f32::from(s) * f32::from(s)).collect();
    let mean = pairwise_sum_f32(&sq) / block.len() as f32;
    f64::from(mean).sqrt()
}

fn to_bytes(block: &[i16]) -> Vec<u8> {
    block.iter().flat_map(|s| s.to_ne_bytes()).collect()
}

// ---- subscriber queue -------------------------------------------------------------

struct QueueInner {
    items: Mutex<VecDeque<Vec<u8>>>,
    maxsize: usize,
    notify: Notify,
}

/// `asyncio.Queue(maxsize=100)` of raw int16 24 kHz blocks; any thread may put, one task gets.
#[derive(Clone)]
pub struct PcmQueue {
    inner: Arc<QueueInner>,
}

impl PcmQueue {
    /// `maxsize` 0 is unbounded, as in asyncio.
    pub fn new(maxsize: usize) -> Self {
        PcmQueue { inner: Arc::new(QueueInner { items: Mutex::new(VecDeque::new()), maxsize, notify: Notify::new() }) }
    }

    pub fn qsize(&self) -> usize {
        self.inner.items.lock().len()
    }

    pub fn empty(&self) -> bool {
        self.qsize() == 0
    }

    pub fn full(&self) -> bool {
        self.inner.maxsize > 0 && self.qsize() >= self.inner.maxsize
    }

    /// `put_nowait`: Err(raw) back when the queue is full (`asyncio.QueueFull`).
    pub fn put_nowait(&self, raw: Vec<u8>) -> Result<(), Vec<u8>> {
        {
            let mut items = self.inner.items.lock();
            if self.inner.maxsize > 0 && items.len() >= self.inner.maxsize {
                return Err(raw);
            }
            items.push_back(raw);
        }
        self.inner.notify.notify_one();
        Ok(())
    }

    /// `get_nowait`: None when empty (`asyncio.QueueEmpty`).
    pub fn get_nowait(&self) -> Option<Vec<u8>> {
        self.inner.items.lock().pop_front()
    }

    /// `await q.get()`.
    pub async fn get(&self) -> Vec<u8> {
        loop {
            let notified = self.inner.notify.notified();
            if let Some(x) = self.get_nowait() {
                return x;
            }
            notified.await;
        }
    }

    /// The same queue (Python's `q in self._subs` is identity for queues).
    pub fn same(&self, other: &PcmQueue) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

/// Drop the oldest block rather than block the audio thread on a slow consumer.
fn offer(q: &PcmQueue, raw: Vec<u8>) {
    let mut items = q.inner.items.lock();
    if q.inner.maxsize > 0 && items.len() >= q.inner.maxsize {
        items.pop_front();
    }
    items.push_back(raw);
    drop(items);
    q.inner.notify.notify_one();
}

// ---- the stream ------------------------------------------------------------------

pub type OnWake = Arc<dyn Fn(String) + Send + Sync>;
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

struct Core {
    on_wake: OnWake,
    handle: tokio::runtime::Handle,
    gate: WakeGate,
    spotter: Option<Box<dyn Spotter>>,
    clock: Clock,
    subs: Vec<PcmQueue>,
    silent_since: Option<f64>,
    silence_warned: bool,
    ring: WakeRing,
    last_wake_audio: Vec<u8>,
    last_rms: f64,
}

impl Core {
    fn on_block(&mut self, block: &[i16], now: Option<f64>) -> Option<String> {
        let now = now.unwrap_or_else(|| (self.clock)());
        self.last_rms = rms_int16(block);
        self.watch_silence(now);
        if !self.subs.is_empty() {
            let raw = to_bytes(block);
            for q in &self.subs {
                offer(q, raw.clone());
            }
            return None; // muted: never wake on our own voice
        }
        let spotter = self.spotter.as_mut()?;
        self.ring.add(&to_bytes(block));
        let samples: Vec<f32> = block.iter().map(|&s| f32::from(s) / 32768.0).collect();
        let fed = spotter.feed(&samples, SAMPLE_RATE);
        let hit = self.gate.consider(fed.as_deref(), now);
        if let Some(h) = &hit {
            self.last_wake_audio = self.ring.snapshot();
            self.ring.clear();
            let on_wake = self.on_wake.clone();
            let h = h.clone();
            self.handle.spawn(async move { on_wake(h) });
        }
        hit
    }

    fn watch_silence(&mut self, now: f64) {
        if self.last_rms >= SILENCE_RMS {
            self.silent_since = None;
            return;
        }
        match self.silent_since {
            None => self.silent_since = Some(now),
            Some(since) => {
                if !self.silence_warned && now - since >= SILENCE_WARN_SECS {
                    self.silence_warned = true;
                    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
                    warn!(
                        "ears: the microphone has been silent for {:.0} s (rms {:.1}) — is Microphone granted to {} \
                         in System Settings > Privacy & Security?",
                        SILENCE_WARN_SECS, self.last_rms, exe
                    );
                }
            }
        }
    }
}

/// Own the mic stream; fan PCM out to the spotter and to subscribers.
///
/// on_wake(keyword_id) is called on the tokio runtime given at construction. subscribe() returns a
/// `PcmQueue` that receives every raw int16 24 kHz block (bytes) until unsubscribe(); while any
/// subscriber exists the spotter is muted.
///
/// `suppressed` is called with the ears' lock held: it must not call back into this `Ears`.
pub struct Ears {
    pub config: EarsConfig,
    core: Arc<Mutex<Core>>,
    backend: Arc<dyn AudioBackend>,
    stream: Mutex<Option<Box<dyn InputStream>>>,
}

impl Ears {
    /// `Ears(config, on_wake, loop)` with the real model (loaded by `prepare`), CoreAudio and
    /// `time.monotonic`.
    pub fn new(config: EarsConfig, on_wake: OnWake, handle: tokio::runtime::Handle) -> Self {
        let gate = WakeGate { cooldown_secs: config.cooldown_secs, ..WakeGate::default() };
        let core = Core {
            on_wake,
            handle,
            gate,
            spotter: None,
            clock: Arc::new(eve_voice_gate::monotonic),
            subs: Vec::new(),
            silent_since: None,
            silence_warned: false,
            // The seconds before a wake word, in RAM only: the wake utterance is what voice_gate enrols a
            // conversation's speaker from. Cleared once taken; never written anywhere.
            ring: WakeRing::default(),
            last_wake_audio: Vec::new(),
            last_rms: 0.0,
        };
        Ears {
            config,
            core: Arc::new(Mutex::new(core)),
            backend: Arc::new(CpalBackend::new()),
            stream: Mutex::new(None),
        }
    }

    /// The daemon's reasons not to wake (`suppressed=`).
    pub fn with_suppressed(self, suppressed: Suppressed) -> Self {
        self.core.lock().gate.suppressed = suppressed;
        self
    }

    /// A spotter other than the real model (`spotter=`).
    pub fn with_spotter(self, spotter: Box<dyn Spotter>) -> Self {
        self.core.lock().spotter = Some(spotter);
        self
    }

    /// The clock (`clock=`).
    pub fn with_clock(self, clock: Clock) -> Self {
        self.core.lock().clock = clock;
        self
    }

    /// The audio library (the Python tests swap `sounddevice` in `sys.modules`).
    pub fn with_backend(mut self, backend: Arc<dyn AudioBackend>) -> Self {
        self.backend = backend;
        self
    }

    // -- subscribers (the voice session) --

    pub fn subscribe(&self) -> PcmQueue {
        let q = PcmQueue::new(100);
        self.core.lock().subs.push(q.clone());
        q
    }

    pub fn unsubscribe(&self, q: &PcmQueue) {
        let mut core = self.core.lock();
        if let Some(i) = core.subs.iter().position(|s| s.same(q)) {
            core.subs.remove(i);
        }
    }

    pub fn muted(&self) -> bool {
        !self.core.lock().subs.is_empty()
    }

    /// Process one int16 block (the audio thread calls this). Returns the keyword id on a wake (tests).
    pub fn on_block(&self, block: &[i16], now: Option<f64>) -> Option<String> {
        self.core.lock().on_block(block, now)
    }

    /// The audio before the last wake (`last_wake_audio`), empty when none.
    pub fn last_wake_audio(&self) -> Vec<u8> {
        self.core.lock().last_wake_audio.clone()
    }

    /// Read and clear `last_wake_audio`: one conversation's enrolment, then gone.
    pub fn take_last_wake_audio(&self) -> Vec<u8> {
        std::mem::take(&mut self.core.lock().last_wake_audio)
    }

    pub fn set_last_wake_audio(&self, audio: Vec<u8>) {
        self.core.lock().last_wake_audio = audio;
    }

    pub fn last_rms(&self) -> f64 {
        self.core.lock().last_rms
    }

    /// The gate's (hits, suppressed_hits).
    pub fn gate_counts(&self) -> (u64, u64) {
        let core = self.core.lock();
        (core.gate.hits, core.gate.suppressed_hits)
    }

    /// When the current silence began (`_silent_since`).
    pub fn silent_since(&self) -> Option<f64> {
        self.core.lock().silent_since
    }

    /// Whether the dead-mic warning has been given for this silence (`_silence_warned`).
    pub fn silence_warned(&self) -> bool {
        self.core.lock().silence_warned
    }

    // -- lifecycle --

    /// True while the microphone stream is open.
    pub fn listening(&self) -> bool {
        self.stream.lock().is_some()
    }

    /// Load the wake-word model without opening the mic. False (with one warning)
    /// when audio or the model is unavailable, so the daemon can say so at boot.
    pub fn prepare(&self) -> bool {
        if let Err(e) = self.backend.available() {
            warn!("ears: audio input not available ({}) — no wake word", e);
            return false;
        }
        if self.core.lock().spotter.is_none() {
            match WakeSpotter::new(&self.config, None) {
                Ok(s) => {
                    let mut core = self.core.lock();
                    if core.spotter.is_none() {
                        core.spotter = Some(Box::new(s));
                    }
                }
                Err(e) => {
                    warn!("ears: wake-word model unavailable ({:#}) — no wake word", e);
                    return false;
                }
            }
        }
        true
    }

    /// Open the mic. Idempotent: a second call while open is a no-op. False (with
    /// one warning) when audio or the model is unavailable.
    pub fn start(&self) -> bool {
        if self.stream.lock().is_some() {
            return true;
        }
        if !self.prepare() {
            return false;
        }
        let backend = self.backend.as_ref();
        let mut device = None;
        if let Some(needle) = &self.config.device {
            device = find_input_device(backend, needle);
            if device.is_none() {
                warn!("ears: no input device matching {}; using the default", py::repr(needle));
            }
        }
        let spec = InputSpec { sample_rate: SAMPLE_RATE, block_frames: block_frames(), device };
        let make = || {
            let core = self.core.clone();
            let on_block: BlockCallback = Box::new(move |block: &[i16]| {
                let r = std::panic::catch_unwind(AssertUnwindSafe(|| core.lock().on_block(block, None)));
                if r.is_err() {
                    error!("ears: block failed");
                }
            });
            let on_status: StatusCallback = Box::new(|status: String| debug!("ears: stream status {}", status));
            backend.make_input(spec, on_block, on_status).map_err(|e| anyhow!(e))
        };
        match open_input(backend, make, "ears") {
            Ok(stream) => {
                *self.stream.lock() = Some(stream);
            }
            Err(e) => {
                warn!("ears: could not open the microphone ({:#}) — no wake word", e);
                return false;
            }
        }
        info!(
            "ears: listening for {} on {} ({} Hz, threshold {:.2})",
            py::repr(&self.config.wake_word),
            input_device_name(backend, device),
            SAMPLE_RATE,
            self.config.threshold
        );
        true
    }

    /// Close the mic. The model stays loaded, so start() again is cheap.
    pub fn stop(&self) {
        let stream = self.stream.lock().take();
        if let Some(mut s) = stream {
            if s.stop().is_ok() {
                let _ = s.close();
            }
            let mut core = self.core.lock();
            core.silent_since = None;
            core.silence_warned = false;
        }
    }
}

/// Open and start an input stream; on failure, refresh the audio device table and try once more.
///
/// PortAudio lists the audio devices when it is initialised and never again, so a device that appeared
/// after the daemon started (a Bluetooth speaker with a microphone, a USB mic) is not in its table, and
/// when macOS makes that device the default input, opening "the default" hits a stale entry and fails with
/// paInternalError (-9986). Seen live 2026-09-28 15:24: the desk call heard nothing twice, then could not
/// open the microphone at all, after a JBL Flip 4 joined. The Rust backend keeps a device table the same
/// way, and `refresh()` rebuilds it.
pub fn open_input<F>(backend: &dyn AudioBackend, make: F, what: &str) -> Result<Box<dyn InputStream>>
where
    F: Fn() -> Result<Box<dyn InputStream>>,
{
    let attempt = |make: &F| -> Result<Box<dyn InputStream>> {
        let mut stream = make()?;
        stream.start().map_err(|e| anyhow!(e))?;
        Ok(stream)
    };
    match attempt(&make) {
        Ok(s) => Ok(s),
        Err(first) => {
            warn!("{}: could not open the microphone ({:#}); refreshing the audio device list", what, first);
            if let Err(e) = backend.refresh() {
                warn!("{}: audio device refresh failed ({})", what, e);
                return Err(first);
            }
            let stream = attempt(&make)?;
            info!("{}: microphone opened after the refresh", what);
            Ok(stream)
        }
    }
}

/// The device's name for the log, "?" when it cannot be read.
pub fn input_device_name(backend: &dyn AudioBackend, device: Option<usize>) -> String {
    backend.device_name(device).unwrap_or_else(|_| "?".into())
}

/// The first input device whose name contains `needle` (any case).
pub fn find_input_device(backend: &dyn AudioBackend, needle: &str) -> Option<usize> {
    let n = needle.to_lowercase();
    let devices = backend.query_devices().ok()?;
    devices.iter().position(|d| d.max_input_channels > 0 && d.name.to_lowercase().contains(&n))
}

// ---- `eve-bridge ears-check` -------------------------------------------------

/// Print why the wake word is or isn't working; listen for it for a few seconds.
pub fn diagnose(seconds: f64) -> i32 {
    diagnose_with(&configured(), &CpalBackend::new(), seconds)
}

fn py_bool(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

pub fn diagnose_with(cfg: &EarsConfig, backend: &dyn AudioBackend, seconds: f64) -> i32 {
    println!("wake word:      {}  (EVE_WAKE_WORD)", py::repr(&cfg.wake_word));
    println!("model dir:      {}  present={}", cfg.model_dir.display(), py_bool(model_present(&cfg.model_dir)));
    if !model_present(&cfg.model_dir) {
        let parent = cfg.model_dir.parent().map(|p| p.display().to_string()).unwrap_or_else(|| ".".into());
        println!("  download: curl -L {} | tar xj -C {}", MODEL_URL, if parent.is_empty() { "." } else { &parent });
        return 2;
    }
    if backend.available().is_err() {
        println!("audio input:    NOT AVAILABLE");
        return 2;
    }
    println!("input device:   {}", input_device_name(backend, None));
    let spot = match WakeSpotter::new(cfg, None) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ears-check: {e:#}");
            return 1;
        }
    };
    let kw_text = std::fs::read_to_string(&spot.keywords_file).unwrap_or_default();
    println!("keywords file:  {} -> {}", spot.keywords_file.display(), py::strip(&kw_text));
    println!("\nSay {} within {:.0} s ...", py::repr(&cfg.wake_word), seconds);

    struct Heard {
        hits: Vec<f64>,
        silent: bool,
    }
    let heard = Arc::new(Mutex::new(Heard { hits: Vec::new(), silent: true }));
    let t0 = Instant::now();
    let mut spot = spot;
    let h = heard.clone();
    let on_block: BlockCallback = Box::new(move |block: &[i16]| {
        if rms_int16(block) >= SILENCE_RMS {
            h.lock().silent = false;
        }
        let samples: Vec<f32> = block.iter().map(|&s| f32::from(s) / 32768.0).collect();
        if spot.feed(&samples, SAMPLE_RATE).is_some() {
            h.lock().hits.push(t0.elapsed().as_secs_f64());
        }
    });
    let spec = InputSpec { sample_rate: SAMPLE_RATE, block_frames: block_frames(), device: None };
    let mut stream = match backend.make_input(spec, on_block, Box::new(|_| {})) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ears-check: could not open the microphone ({e})");
            return 1;
        }
    };
    if let Err(e) = stream.start() {
        eprintln!("ears-check: could not open the microphone ({e})");
        return 1;
    }
    while t0.elapsed().as_secs_f64() < seconds {
        std::thread::sleep(Duration::from_millis(100));
        if !heard.lock().hits.is_empty() {
            break;
        }
    }
    let _ = stream.stop();
    let _ = stream.close();
    let heard = heard.lock();
    if let Some(first) = heard.hits.first() {
        println!("HEARD IT at {first:.1} s — ears are working.");
        return 0;
    }
    if heard.silent {
        let exe = std::env::current_exe()
            .and_then(|p| p.canonicalize())
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        println!("mic was silent the whole time: grant Microphone to {exe}");
        return 1;
    }
    println!("mic is live but the phrase was not heard — try again closer, or lower EVE_WAKE_THRESHOLD (0.25).");
    1
}

#[cfg(test)]
mod tests;
