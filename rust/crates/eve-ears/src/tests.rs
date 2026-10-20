//! Ports of bridge/tests/test_ears.py and the Ears parts of bridge/tests/test_mic.py, plus parity checks
//! against values printed by Python (numpy 2, sentencepiece 0.2, CPython 3.13).

use super::*;
use std::collections::HashMap;
use std::io::Write as _;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---- helpers ----------------------------------------------------------------------

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    move |k: &str| map.get(k).cloned()
}

#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }
}

/// caplog: run `f` with WARNING-and-above (or `level`) captured on this thread.
fn capture<R>(level: tracing::Level, f: impl FnOnce() -> R) -> (R, String) {
    let buf = LogBuf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let r = tracing::subscriber::with_default(subscriber, f);
    (r, buf.text())
}

struct FakeSpotter {
    hits_at_blocks: Vec<usize>,
    fed: Arc<AtomicUsize>,
}

impl FakeSpotter {
    fn new(hits_at_blocks: &[usize]) -> (Self, Arc<AtomicUsize>) {
        let fed = Arc::new(AtomicUsize::new(0));
        (FakeSpotter { hits_at_blocks: hits_at_blocks.to_vec(), fed: fed.clone() }, fed)
    }
}

impl Spotter for FakeSpotter {
    fn feed(&mut self, _samples: &[f32], rate: u32) -> Option<String> {
        let n = self.fed.fetch_add(1, Ordering::SeqCst) + 1;
        assert_eq!(rate, SAMPLE_RATE);
        self.hits_at_blocks.contains(&n).then(|| "hey_eve".to_string())
    }
}

/// A loud, noise-like 2400-sample block (the Python test's seeded normal noise × level).
fn block(level: i32) -> Vec<i16> {
    let mut x: u64 = 1;
    (0..2400)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let u = ((x >> 33) as f64 / (1u64 << 31) as f64) * 2.0 - 1.0;
            (u * 1.7 * f64::from(level)) as i16
        })
        .collect()
}

fn to_ne(b: &[i16]) -> Vec<u8> {
    b.iter().flat_map(|s| s.to_ne_bytes()).collect()
}

fn new_ears(spotter: FakeSpotter, on_wake: OnWake) -> Ears {
    Ears::new(EarsConfig::default(), on_wake, tokio::runtime::Handle::current())
        .with_spotter(Box::new(spotter))
        .with_backend(Arc::new(FakeSd::default()))
}

// ---- a sounddevice stand-in ------------------------------------------------------

#[derive(Default)]
struct SdCounts {
    opened: AtomicUsize,
    closed: AtomicUsize,
    made: AtomicUsize,
    refreshed: AtomicUsize,
}

#[derive(Default)]
struct FakeSd {
    counts: Arc<SdCounts>,
    unavailable: bool,
    /// How many `make_input` calls fail before one succeeds.
    fail_first: usize,
    devices: Vec<DeviceInfo>,
}

struct FakeStream {
    counts: Arc<SdCounts>,
}

impl InputStream for FakeStream {
    fn start(&mut self) -> Result<(), String> {
        self.counts.opened.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn stop(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn close(&mut self) -> Result<(), String> {
        self.counts.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl AudioBackend for FakeSd {
    fn available(&self) -> Result<(), String> {
        if self.unavailable {
            Err("No module named 'sounddevice'".into())
        } else {
            Ok(())
        }
    }
    fn query_devices(&self) -> Result<Vec<DeviceInfo>, String> {
        Ok(self.devices.clone())
    }
    fn device_name(&self, device: Option<usize>) -> Result<String, String> {
        match device {
            None => Ok("Fake Mic".into()),
            Some(i) => self.devices.get(i).map(|d| d.name.clone()).ok_or_else(|| "no device".into()),
        }
    }
    fn make_input(
        &self,
        _spec: InputSpec,
        _on_block: BlockCallback,
        _on_status: StatusCallback,
    ) -> Result<Box<dyn InputStream>, String> {
        let n = self.counts.made.fetch_add(1, Ordering::SeqCst);
        if n < self.fail_first {
            return Err("Internal PortAudio error [PaErrorCode -9986]".into());
        }
        Ok(Box::new(FakeStream { counts: self.counts.clone() }))
    }
    fn refresh(&self) -> Result<(), String> {
        self.counts.refreshed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// ---- config -----------------------------------------------------------------

#[test]
fn test_configured_defaults() {
    let c = configured_from(env_of(&[]));
    assert!(c.enabled && c.wake_word == "hey eve" && c.threshold == 0.25 && c.device.is_none());
}

#[test]
fn test_configured_env_knobs() {
    let c = configured_from(env_of(&[
        ("EVE_VOICE", "0"),
        ("EVE_WAKE_WORD", " hey robot "),
        ("EVE_WAKE_THRESHOLD", "0.4"),
        ("EVE_MIC", "Studio"),
    ]));
    assert!(!c.enabled && c.wake_word == "hey robot" && c.threshold == 0.4 && c.device.as_deref() == Some("Studio"));
}

#[test]
fn test_configured_bad_threshold_falls_back() {
    let (c, logs) = capture(tracing::Level::WARN, || configured_from(env_of(&[("EVE_WAKE_THRESHOLD", "hot")])));
    assert!(c.threshold == 0.25 && logs.contains("not a number"));
    assert!(logs.contains("ears: EVE_WAKE_THRESHOLD='hot' is not a number; using 0.25"), "{logs}");
}

#[test]
fn test_keyword_id_and_line() {
    assert_eq!(keyword_id("Hey, Eve!"), "hey_eve");
    let tokens: Vec<String> = ["▁HE", "Y", "▁BU", "D", "D", "Y"].iter().map(|s| s.to_string()).collect();
    assert_eq!(keyword_line(&tokens, "hey eve", 2.0, 0.25), "▁HE Y ▁BU D D Y :2 #0.25 @hey_eve");
}

// ---- gate -------------------------------------------------------------------

#[test]
fn test_gate_passes_first_hit_and_cools_down() {
    let mut g = WakeGate { cooldown_secs: 2.0, ..WakeGate::default() };
    assert_eq!(g.consider(Some("hey_eve"), 10.0).as_deref(), Some("hey_eve"));
    assert_eq!(g.consider(Some("hey_eve"), 11.0), None); // inside the window
    assert_eq!(g.consider(Some("hey_eve"), 12.5).as_deref(), Some("hey_eve"));
    assert_eq!(g.consider(None, 20.0), None);
    assert_eq!(g.hits, 3);
}

#[test]
fn test_gate_suppressed_hits_are_counted_not_raised() {
    let mut g = WakeGate { suppressed: Arc::new(|| true), ..WakeGate::default() };
    assert_eq!(g.consider(Some("hey_eve"), 1.0), None);
    assert!(g.suppressed_hits == 1 && g.hits == 1);
}

// ---- stream plumbing (fake spotter, no mic) -----------------------------------

#[tokio::test]
async fn test_wake_callback_runs_on_the_loop() {
    let got = Arc::new(Mutex::new(Vec::<String>::new()));
    let g = got.clone();
    let (spot, _) = FakeSpotter::new(&[2]);
    let e = new_ears(spot, Arc::new(move |k| g.lock().push(k))).with_clock(Arc::new(|| 0.0));
    assert_eq!(e.on_block(&block(500), None), None);
    assert_eq!(e.on_block(&block(500), None).as_deref(), Some("hey_eve"));
    tokio::task::yield_now().await;
    assert_eq!(*got.lock(), vec!["hey_eve".to_string()]);
}

#[tokio::test]
async fn test_subscribers_get_pcm_and_mute_the_spotter() {
    let (spot, fed) = FakeSpotter::new(&[1, 2, 3]);
    let e = new_ears(spot, Arc::new(|_k| {}));
    let q = e.subscribe();
    assert!(e.muted());
    let b = block(500);
    assert_eq!(e.on_block(&b, None), None);
    tokio::task::yield_now().await;
    let raw = q.get_nowait().expect("a block");
    assert!(raw == to_ne(&b) && fed.load(Ordering::SeqCst) == 0);
    e.unsubscribe(&q);
    assert!(!e.muted());
    assert!(e.on_block(&b, None).as_deref() == Some("hey_eve") && fed.load(Ordering::SeqCst) == 1);
}

#[tokio::test]
async fn test_slow_subscriber_drops_oldest_block() {
    let (spot, _) = FakeSpotter::new(&[]);
    let e = new_ears(spot, Arc::new(|_k| {}));
    let q = e.subscribe();
    for i in 0..105i16 {
        e.on_block(&vec![i; 2400], None);
    }
    tokio::task::yield_now().await;
    assert_eq!(q.qsize(), 100);
    let first = q.get_nowait().unwrap();
    let first = i16::from_ne_bytes([first[0], first[1]]);
    assert_eq!(first, 5); // blocks 0..4 were dropped
}

#[tokio::test]
async fn test_silence_warns_once_after_ten_seconds() {
    let t = Arc::new(Mutex::new(0.0f64));
    let tc = t.clone();
    let (spot, _) = FakeSpotter::new(&[]);
    let e = new_ears(spot, Arc::new(|_k| {})).with_clock(Arc::new(move || *tc.lock()));
    let ((), logs) = capture(tracing::Level::WARN, || {
        for i in 0..120 {
            *t.lock() = f64::from(i) * 0.1;
            e.on_block(&[0i16; 2400], None);
        }
    });
    assert_eq!(logs.matches("silent for").count(), 1);
    assert!(logs.contains("ears: the microphone has been silent for 10 s (rms 0.0) — is Microphone granted to "));
    // sound again resets the timer; silence must run the full window again
    *t.lock() = 13.0;
    e.on_block(&block(500), None);
    assert_eq!(e.silent_since(), None);
}

#[test]
fn test_rms() {
    assert_eq!(rms_int16(&[0i16; 10]), 0.0);
    assert!((rms_int16(&[100i16; 10]) - 100.0).abs() < 1e-6);
}

#[tokio::test]
async fn test_start_without_sounddevice_is_a_clean_false() {
    let (spot, _) = FakeSpotter::new(&[]);
    let e = Ears::new(EarsConfig::default(), Arc::new(|_k| {}), tokio::runtime::Handle::current())
        .with_spotter(Box::new(spot))
        .with_backend(Arc::new(FakeSd { unavailable: true, ..FakeSd::default() }));
    let (ok, logs) = capture(tracing::Level::WARN, || e.start());
    assert!(!ok);
    assert!(logs.contains("audio input not available (No module named 'sounddevice')"), "{logs}");
    e.stop();
}

// ---- the real model on synthesized speech ---------------------------------------

fn real_model_available() -> bool {
    let ok = model_present(&EarsConfig::default().model_dir)
        && cfg!(target_os = "macos")
        && which("say")
        && which("afconvert");
    if !ok {
        eprintln!("skipped: needs the KWS model, macOS `say` and `afconvert`");
    }
    ok
}

fn which(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

fn synth(tmp: &Path, name: &str, text: &str) -> Vec<i16> {
    let aiff = tmp.join(format!("{name}.aiff"));
    let wav = tmp.join(format!("{name}.wav"));
    let ok = Command::new("say").args(["-v", "Samantha", "-o"]).arg(&aiff).arg(text).status().unwrap();
    assert!(ok.success());
    let ok = Command::new("afconvert")
        .args(["-f", "WAVE", "-d", &format!("LEI16@{SAMPLE_RATE}"), "-c", "1"])
        .arg(&aiff)
        .arg(&wav)
        .status()
        .unwrap();
    assert!(ok.success());
    read_wav_pcm(&std::fs::read(&wav).unwrap())
}

/// The `data` chunk of a 16-bit PCM WAVE file.
fn read_wav_pcm(bytes: &[u8]) -> Vec<i16> {
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &bytes[pos + 8..(pos + 8 + size).min(bytes.len())];
        if id == b"data" {
            return body.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        }
        pos += 8 + size + (size & 1);
    }
    panic!("no data chunk");
}

fn run_clip(spot: &mut WakeSpotter, pcm: &[i16]) -> Vec<(f64, String)> {
    let mut hits = Vec::new();
    let step = block_frames();
    for (n, chunk) in pcm.chunks(step).enumerate() {
        let samples: Vec<f32> = chunk.iter().map(|&s| f32::from(s) / 32768.0).collect();
        if let Some(r) = spot.feed(&samples, SAMPLE_RATE) {
            let at = ((n * step) as f64 / f64::from(SAMPLE_RATE) * 10.0).round() / 10.0;
            hits.push((at, r));
        }
    }
    if let Some(r) = spot.feed(&vec![0.0f32; SAMPLE_RATE as usize], SAMPLE_RATE) {
        // flush the lattice
        hits.push((-1.0, r));
    }
    hits
}

fn keys(hits: &[(f64, String)]) -> Vec<String> {
    hits.iter().map(|(_, k)| k.clone()).collect()
}

// The rename to eve (2026-10-10) made the default phrase "hey eve", and the shipped spotter does not hear it in the
// synthesized clip. The owner does not use a wake word, so this stays a known gap rather than a tuned threshold.
// Python marks these xfail(strict=True): the day the spotter hears it, they turn red and the marks come off. The
// Rust tests assert the gap itself, so they turn red on the same day.
#[test]
fn test_real_model_hears_hey_eve_and_ignores_a_control_sentence() {
    if !real_model_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let cfg = EarsConfig { model_dir: EarsConfig::default().model_dir, ..EarsConfig::default() };
    let kw = tmp.path().join("kw-default.txt");
    let mut spot = WakeSpotter::new(&cfg, Some(&kw)).unwrap();
    let listed = std::fs::read_to_string(&spot.keywords_file).unwrap().contains("@hey_eve"); // also okay/ok eve
    let pos = keys(&run_clip(&mut spot, &synth(tmp.path(), "pos", "hey eve, open my email")));
    let neg = run_clip(&mut spot, &synth(tmp.path(), "neg", "the weather is nice today, nothing to see here"));
    let passes = listed && pos == vec!["hey_eve".to_string()] && neg.is_empty();
    assert!(!passes, "the spotter now hears \"hey eve\": drop the xfail-gap assertion");
}

#[test]
fn test_real_model_custom_phrase() {
    if !real_model_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let cfg = EarsConfig { wake_word: "hello robot".into(), ..EarsConfig::default() };
    let kw = tmp.path().join("kw.txt");
    let mut spot = WakeSpotter::new(&cfg, Some(&kw)).unwrap();
    assert!(std::fs::read_to_string(&kw).unwrap().contains("@hello_robot"));
    let hits = run_clip(&mut spot, &synth(tmp.path(), "pos2", "hello robot, what time is it"));
    assert_eq!(keys(&hits), vec!["hello_robot".to_string()], "{hits:?}");
    // the default phrase must not fire on the custom-phrase spotter
    assert_eq!(run_clip(&mut spot, &synth(tmp.path(), "neg2", "hey eve, open my email")), vec![]);
}

/// The whole path: 24 kHz int16 blocks → Ears → spotter → on_wake. A strict xfail in Python (see above).
#[tokio::test]
async fn test_real_model_through_ears_fanout() {
    if !real_model_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let got = Arc::new(Mutex::new(Vec::<String>::new()));
    let g = got.clone();
    let cfg = EarsConfig::default();
    let spot = WakeSpotter::new(&cfg, Some(&tmp.path().join("kw.txt"))).unwrap();
    let e = Ears::new(cfg, Arc::new(move |k| g.lock().push(k)), tokio::runtime::Handle::current())
        .with_spotter(Box::new(spot))
        .with_clock(Arc::new(|| 0.0))
        .with_backend(Arc::new(FakeSd::default()));
    let pcm = synth(tmp.path(), "pos3", "hey eve");
    let step = block_frames();
    let mut i = 0;
    while i < pcm.len() + SAMPLE_RATE as usize {
        if i < pcm.len() {
            e.on_block(&pcm[i..(i + step).min(pcm.len())], None);
        } else {
            e.on_block(&vec![0i16; step], None);
        }
        i += step;
    }
    tokio::task::yield_now().await;
    assert_ne!(*got.lock(), vec!["hey_eve".to_string()], "the spotter now hears \"hey eve\": drop the xfail-gap");
}

#[test]
fn test_model_present_negative_control() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!model_present(tmp.path()));
    assert!(!model_present(&tmp.path().join("nope")));
}

#[test]
fn test_default_wake_word_includes_okay_eve_aliases() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = |phrase: &str, _: &Path| -> Result<Vec<String>> {
        Ok(phrase.to_uppercase().split_whitespace().map(String::from).collect())
    };
    let base = EarsConfig { model_dir: tmp.path().to_path_buf(), ..EarsConfig::default() };
    let out = write_keywords_file_with(&base, Some(&tmp.path().join("keywords.txt")), fake).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4);
    assert!(lines[1].ends_with("@okay_eve"));
    assert!(lines[2].ends_with("@ok_eve"));
    assert!(lines[3].ends_with("#0.35 @lesson")); // the lesson word rides in the same spotter, stricter
    assert!(lines[0].ends_with("#0.25 @hey_eve"));
    let robot = EarsConfig { wake_word: "hello robot".into(), ..base.clone() };
    let out = write_keywords_file_with(&robot, Some(&out), fake).unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap().lines().count(), 2);
    let quiet = EarsConfig { wake_word: "hello robot".into(), lesson_word: String::new(), ..base };
    let out = write_keywords_file_with(&quiet, Some(&out), fake).unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap().lines().count(), 1);
}

#[test]
fn test_the_lesson_word_is_configured_and_can_be_turned_off() {
    assert_eq!(configured_from(env_of(&[])).lesson_word, "lesson");
    assert_eq!(configured_from(env_of(&[("EVE_LESSON_WORD", "eve lesson")])).lesson_word, "eve lesson");
    assert_eq!(configured_from(env_of(&[("EVE_LESSON_WORD", "off")])).lesson_word, "");
    assert_eq!(configured_from(env_of(&[("EVE_LESSON_WORD", "")])).lesson_word, "");
    assert_eq!(configured_from(env_of(&[])).lesson_threshold, 0.35);
    assert_eq!(configured_from(env_of(&[("EVE_LESSON_THRESHOLD", "0.3")])).lesson_threshold, 0.3);
    assert_eq!(configured_from(env_of(&[("EVE_LESSON_THRESHOLD", "high")])).lesson_threshold, 0.35);
}

// ---- test_mic.py: Ears prepare, start, stop -----------------------------------------

fn mic_ears(sd: FakeSd) -> Ears {
    struct Quiet;
    impl Spotter for Quiet {
        fn feed(&mut self, _samples: &[f32], _rate: u32) -> Option<String> {
            None
        }
    }
    // Kept for the test's life: the handle must outlive the ears.
    let rt: &'static tokio::runtime::Runtime =
        Box::leak(Box::new(tokio::runtime::Builder::new_current_thread().build().unwrap()));
    Ears::new(EarsConfig::default(), Arc::new(|_k| {}), rt.handle().clone())
        .with_spotter(Box::new(Quiet))
        .with_backend(Arc::new(sd))
}

#[test]
fn test_mic_always_is_off_by_default_and_reads_the_env() {
    assert!(!configured_from(env_of(&[])).always);
    assert!(configured_from(env_of(&[("EVE_MIC_ALWAYS", "1")])).always);
    assert!(!configured_from(env_of(&[("EVE_MIC_ALWAYS", "off")])).always);
}

#[test]
fn test_prepare_loads_nothing_into_the_mic() {
    let sd = FakeSd::default();
    let counts = sd.counts.clone();
    let e = mic_ears(sd);
    assert!(e.prepare());
    assert!(!e.listening() && counts.opened.load(Ordering::SeqCst) == 0);
}

#[test]
fn test_start_is_idempotent_and_stop_closes() {
    let sd = FakeSd::default();
    let c = sd.counts.clone();
    let e = mic_ears(sd);
    assert!(e.start() && e.listening());
    assert!(e.start()); // second call: still one stream
    assert_eq!(c.opened.load(Ordering::SeqCst), 1);
    e.stop();
    assert!(!e.listening() && c.closed.load(Ordering::SeqCst) == 1);
    e.stop(); // idempotent too
    assert_eq!(c.closed.load(Ordering::SeqCst), 1);
    assert!(e.start() && c.opened.load(Ordering::SeqCst) == 2); // reopens after a stop
}

#[test]
fn test_stop_resets_the_silence_watch() {
    let e = mic_ears(FakeSd::default());
    e.start();
    e.on_block(&[0i16; 2400], Some(0.0));
    assert_eq!(e.silent_since(), Some(0.0));
    e.stop();
    assert!(e.silent_since().is_none() && !e.silence_warned());
}

// ---- open_input, device lookup, start's log ------------------------------------------

#[test]
fn open_input_refreshes_the_device_table_and_retries_once() {
    let sd = FakeSd { fail_first: 1, ..FakeSd::default() };
    let c = sd.counts.clone();
    let make = || sd.make_input(InputSpec { sample_rate: 24000, block_frames: 1200, device: None }, Box::new(|_| {}), Box::new(|_| {})).map_err(|e| anyhow!(e));
    let (r, logs) = capture(tracing::Level::INFO, || open_input(&sd, make, "desk call"));
    assert!(r.is_ok());
    assert_eq!(c.refreshed.load(Ordering::SeqCst), 1);
    assert_eq!(c.opened.load(Ordering::SeqCst), 1);
    assert!(logs.contains("desk call: could not open the microphone (Internal PortAudio error [PaErrorCode -9986]); refreshing the audio device list"), "{logs}");
    assert!(logs.contains("desk call: microphone opened after the refresh"));
}

#[test]
fn open_input_gives_up_after_the_second_failure() {
    let sd = FakeSd { fail_first: 2, ..FakeSd::default() };
    let make = || sd.make_input(InputSpec { sample_rate: 24000, block_frames: 2400, device: None }, Box::new(|_| {}), Box::new(|_| {})).map_err(|e| anyhow!(e));
    let r = open_input(&sd, make, "ears");
    assert!(r.is_err());
    assert_eq!(sd.counts.refreshed.load(Ordering::SeqCst), 1);
    assert_eq!(sd.counts.made.load(Ordering::SeqCst), 2);
}

#[test]
fn find_input_device_matches_inputs_by_substring_any_case() {
    let sd = FakeSd {
        devices: vec![
            DeviceInfo { name: "Studio Display Speakers".into(), max_input_channels: 0 },
            DeviceInfo { name: "Studio Display Microphone".into(), max_input_channels: 1 },
            DeviceInfo { name: "MacBook Pro Microphone".into(), max_input_channels: 1 },
        ],
        ..FakeSd::default()
    };
    assert_eq!(find_input_device(&sd, "studio"), Some(1));
    assert_eq!(find_input_device(&sd, "MACBOOK"), Some(2));
    assert_eq!(find_input_device(&sd, "jbl"), None);
    assert_eq!(input_device_name(&sd, Some(9)), "?");
    assert_eq!(input_device_name(&sd, None), "Fake Mic");
}

#[test]
fn start_logs_the_wake_word_device_and_threshold_and_warns_on_an_unknown_device() {
    let e = Ears { config: EarsConfig { device: Some("Studio".into()), ..EarsConfig::default() }, ..mic_ears(FakeSd::default()) };
    let (ok, logs) = capture(tracing::Level::INFO, || e.start());
    assert!(ok);
    assert!(logs.contains("ears: no input device matching 'Studio'; using the default"), "{logs}");
    assert!(logs.contains("ears: listening for 'hey eve' on Fake Mic (24000 Hz, threshold 0.25)"), "{logs}");
}

#[tokio::test]
async fn the_wake_audio_is_the_ring_before_the_wake_and_is_taken_once() {
    let (spot, _) = FakeSpotter::new(&[3]);
    let e = new_ears(spot, Arc::new(|_k| {})).with_clock(Arc::new(|| 0.0));
    let blocks: Vec<Vec<i16>> = (1..=3i16).map(|i| vec![i * 100; 2400]).collect();
    for b in &blocks {
        e.on_block(b, None);
    }
    let want: Vec<u8> = blocks.iter().flat_map(|b| to_ne(b)).collect();
    assert_eq!(e.last_wake_audio(), want);
    assert_eq!(e.take_last_wake_audio(), want);
    assert!(e.last_wake_audio().is_empty());
}

#[tokio::test]
async fn pcm_queue_get_waits_for_a_block() {
    let q = PcmQueue::new(100);
    let q2 = q.clone();
    let t = tokio::spawn(async move { q2.get().await });
    tokio::task::yield_now().await;
    offer(&q, vec![1, 2]);
    assert_eq!(t.await.unwrap(), vec![1, 2]);
    assert!(q.empty());
}

// ---- parity: numpy, sentencepiece, Python formatting ----------------------------------

#[test]
fn rms_matches_numpy_float32_to_the_last_bit() {
    let b: Vec<i16> = (0..2400i64).map(|i| ((i * 7919 + 13) % 6553 - 3276) as i16).collect();
    assert_eq!(rms_int16(&b), 1892.4414125673745);
    let b: Vec<i16> = (0..1000i64).map(|i| ((i * 31337 + 7) % 201 - 100) as i16).collect();
    assert_eq!(rms_int16(&b), 58.08106328321973);
    let b: Vec<i16> = (0..100i64).map(|i| ((i * 31 + 7) % 17 - 8) as i16).collect();
    assert_eq!(rms_int16(&b), 4.9183331052357175);
}

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn sentencepiece_bpe_matches_the_cpp_library() {
    let sp = sentencepiece::SentencePieceProcessor::open(&data("tiny_bpe.model")).unwrap();
    let cases: &[(&str, &[&str])] = &[
        ("hello robot", &["▁hello", "▁robot"]),
        ("hey eve, okay eve!", &["▁hey", "▁eve", ",", "▁okay", "▁eve", "!"]),
        ("the lazy  dog jumps", &["▁the", "▁l", "a", "z", "y", "▁", "d", "o", "g", "▁", "j", "u", "m", "p", "s"]),
        ("<blk>hello", &["▁", "<blk>", "he", "llo"]),
        ("xyzzy quick", &["▁", "x", "y", "z", "z", "y", "▁", "q", "u", "i", "c", "k"]),
        ("  lesson listen ", &["▁l", "esson", "▁l", "isten"]),
        ("zebra", &["▁", "z", "e", "b", "r", "a"]),
    ];
    for (text, want) in cases {
        assert_eq!(sp.encode(text), strs(want), "{text:?}");
    }
}

#[test]
fn sentencepiece_unigram_matches_the_cpp_library() {
    let sp = sentencepiece::SentencePieceProcessor::open(&data("tiny_unigram.model")).unwrap();
    let cases: &[(&str, &[&str])] = &[
        ("hello robot", &["▁hello", "▁robot"]),
        ("hey eve, okay eve!", &["▁hey", "▁eve", ",", "▁okay", "▁eve", "!"]),
        ("the lazy  dog jumps", &["▁the", "▁lazy", "▁dog", "▁jumps"]),
        ("<blk>hello", &["▁", "<blk>", "h", "e", "l", "l", "o"]),
        ("xyzzy quick", &["▁", "x", "y", "z", "z", "y", "▁quick"]),
        ("  lesson listen ", &["▁lesson", "▁listen"]),
        ("zebra", &["▁", "z", "e", "b", "r", "a"]),
    ];
    for (text, want) in cases {
        assert_eq!(sp.encode(text), strs(want), "{text:?}");
    }
}

#[test]
fn the_kws_bpe_model_encodes_like_the_python_wheel() {
    let bpe = EarsConfig::default().model_dir.join("bpe.model");
    if !bpe.exists() {
        eprintln!("skipped: needs the KWS model's bpe.model");
        return;
    }
    let sp = sentencepiece::SentencePieceProcessor::open(&bpe).unwrap();
    let cases: &[(&str, &[&str])] = &[
        ("HEY EVE", &["▁HE", "Y", "▁E", "VE"]),
        ("OKAY EVE", &["▁OKAY", "▁E", "VE"]),
        ("OK EVE", &["▁O", "K", "▁E", "VE"]),
        ("LESSON", &["▁LE", "S", "S", "ON"]),
        ("HELLO ROBOT", &["▁HE", "LL", "O", "▁RO", "B", "O", "T"]),
        ("EVE LESSON", &["▁E", "VE", "▁LE", "S", "S", "ON"]),
        ("  HEY   ÉVE! 123 ", &["▁HE", "Y", "▁", "É", "VE", "!", "▁", "123"]),
        ("ＡＢＣ", &["▁A", "B", "C"]),
        ("<blk> HEY", &["▁", "<blk>", "▁HE", "Y"]),
        ("HEY<sos/eos>X", &["▁HE", "Y", "<sos/eos>", "X"]),
        ("ﬁ ½ Ⅻ", &["▁", "fi", "▁", "1⁄2", "▁", "X", "I", "I"]),
        ("A\tB\nC", &["▁A", "▁B", "▁C"]),
        ("", &[]),
        ("   ", &[]),
        ("STRASSE", &["▁ST", "RA", "S", "SE"]),
        ("ÀÉÎ", &["▁", "ÀÉÎ"]),
        ("①②", &["▁", "12"]),
    ];
    for (text, want) in cases {
        assert_eq!(sp.encode(text), strs(want), "{text:?}");
    }
    // encode_phrase strips and upper-cases first, as the Python does.
    assert_eq!(encode_phrase(" hey eve ", &bpe).unwrap(), strs(&["▁HE", "Y", "▁E", "VE"]));
}

#[test]
fn the_default_keywords_file_matches_the_python_one() {
    let model_dir = EarsConfig::default().model_dir;
    if !model_dir.join("bpe.model").exists() {
        eprintln!("skipped: needs the KWS model's bpe.model");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let out = write_keywords_file(&EarsConfig::default(), Some(&tmp.path().join("k.txt"))).unwrap();
    // Printed by ears.write_keywords_file(EarsConfig(), path) under Python.
    assert_eq!(
        std::fs::read_to_string(out).unwrap(),
        "▁HE Y ▁E VE :2 #0.25 @hey_eve\n▁OKAY ▁E VE :2 #0.25 @okay_eve\n▁O K ▁E VE :2 #0.25 @ok_eve\n\
         ▁LE S S ON :2 #0.35 @lesson\n"
    );
}

#[test]
fn python_formatting_helpers_match() {
    for (x, g, s) in [
        (2.0, "2", "2.0"),
        (0.25, "0.25", "0.25"),
        (0.35, "0.35", "0.35"),
        (1e-5, "1e-05", "1e-05"),
        (1234567.0, "1.23457e+06", "1234567.0"),
        (0.0001, "0.0001", "0.0001"),
        (100000.0, "100000", "100000.0"),
        (1e6, "1e+06", "1000000.0"),
        (-2.5, "-2.5", "-2.5"),
        (0.1 + 0.2, "0.3", "0.30000000000000004"),
        (f64::INFINITY, "inf", "inf"),
        (f64::NAN, "nan", "nan"),
        (-0.0, "-0", "-0.0"),
        (123456.5, "123456", "123456.5"),
        (1.0 / 3.0, "0.333333", "0.3333333333333333"),
        (1e16, "1e+16", "1e+16"),
    ] {
        assert_eq!(py::format_g(x), g, "{x}");
        assert_eq!(py::float_str(x), s, "{x}");
    }
    for (raw, want) in [
        ("0.4", Some(0.4)),
        ("1_000", Some(1000.0)),
        ("1__0", None),
        (" 1e3", Some(1000.0)),
        ("+.5", Some(0.5)),
        ("inf", Some(f64::INFINITY)),
        ("-Infinity", Some(f64::NEG_INFINITY)),
        ("1.", Some(1.0)),
        (".", None),
        ("1e", None),
        ("--1", None),
        ("0x10", None),
        ("١٢", Some(12.0)),
        ("1_", None),
        ("hot", None),
    ] {
        assert_eq!(py::parse_float(raw), want, "{raw:?}");
    }
    assert!(py::parse_float("nan").unwrap().is_nan());
    assert_eq!(py::repr("hey eve"), "'hey eve'");
    assert_eq!(py::repr("it's"), "\"it's\"");
    assert_eq!(py::repr("a\"b'c"), "'a\"b\\'c'");
    assert_eq!(py::repr("tab\there\x01\x7f\u{a0}é"), "'tab\\there\\x01\\x7f\\xa0é'");
    assert_eq!(py::strip("\u{1c} x \u{3000}"), "x");
    assert_eq!(py::path_str("~/a//b/./c/"), "~/a/b/c");
    assert_eq!(py::path_str("//x"), "//x");
    assert_eq!(py::path_str("///x"), "/x");
}

#[test]
fn the_model_dir_expands_home_like_pathlib() {
    let home = std::env::var("HOME").unwrap();
    let c = configured_from(env_of(&[("EVE_KWS_MODEL_DIR", "~/models/kws/")]));
    assert_eq!(c.model_dir, PathBuf::from(format!("{}/models/kws", home.trim_end_matches('/'))));
    assert_eq!(
        EarsConfig::default().model_dir,
        PathBuf::from(format!(
            "{}/.config/eve-bridge/models/sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01",
            home.trim_end_matches('/')
        ))
    );
    assert_eq!(configured_from(env_of(&[("EVE_KWS_MODEL_DIR", "/opt/kws")])).model_dir, PathBuf::from("/opt/kws"));
    assert_eq!(py::expanduser("~nosuchuser-eve/x"), "~nosuchuser-eve/x");
}

#[test]
fn configured_reads_every_knob_like_python() {
    let c = configured_from(env_of(&[
        ("EVE_VOICE", " OFF "),
        ("EVE_WAKE_WORD", "   "),
        ("EVE_LESSON_WORD", " None "),
        ("EVE_LESSON_THRESHOLD", " 0.5 "),
        ("EVE_MIC", "   "),
        ("EVE_MIC_ALWAYS", " Yes "),
    ]));
    assert!(!c.enabled);
    assert_eq!(c.wake_word, "hey eve");
    assert_eq!(c.lesson_word, "");
    assert_eq!(c.lesson_threshold, 0.5);
    assert_eq!(c.device, None);
    assert!(c.always);
    assert_eq!(c.boost, 2.0);
    assert_eq!(c.cooldown_secs, 2.0);
    assert!(configured_from(env_of(&[("EVE_VOICE", "")])).enabled);
    assert!(configured_from(env_of(&[("EVE_VOICE", "yes")])).enabled);
    let (c, logs) = capture(tracing::Level::WARN, || configured_from(env_of(&[("EVE_LESSON_THRESHOLD", "high")])));
    assert_eq!(c.lesson_threshold, 0.35);
    assert!(logs.contains("ears: EVE_LESSON_THRESHOLD='high' is not a number; using 0.35"), "{logs}");
}

#[test]
fn the_lesson_word_is_left_out_when_it_is_the_wake_word() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = |phrase: &str, _: &Path| -> Result<Vec<String>> { Ok(vec![phrase.to_uppercase()]) };
    let cfg = EarsConfig { model_dir: tmp.path().into(), wake_word: "Lesson!".into(), ..EarsConfig::default() };
    let out = write_keywords_file_with(&cfg, None, fake).unwrap();
    assert_eq!(out, tmp.path().join("keywords-lesson.txt"));
    assert_eq!(std::fs::read_to_string(out).unwrap(), "LESSON! :2 #0.25 @lesson\n");
}

#[test]
fn a_missing_model_is_a_clean_error() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = EarsConfig { model_dir: tmp.path().into(), ..EarsConfig::default() };
    let err = WakeSpotter::new(&cfg, None).err().expect("an error");
    assert_eq!(err.to_string(), format!("KWS model missing under {} — download {}", tmp.path().display(), MODEL_URL));
}

#[test]
fn the_converter_cuts_exact_blocks_from_any_device_format() {
    let got = Arc::new(Mutex::new(Vec::<Vec<i16>>::new()));
    let g = got.clone();
    let spec = InputSpec { sample_rate: 24000, block_frames: 2400, device: None };
    let mut push = audio::converter_for_tests(2, 48000, &spec, Box::new(move |b| g.lock().push(b.to_vec()))).unwrap();
    // One second of a stereo 48 kHz 440 Hz tone at half scale, delivered in uneven device buffers.
    let frames: Vec<f32> = (0..48000)
        .flat_map(|i| {
            let v = 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin();
            [v, -1.0]
        })
        .collect();
    for chunk in frames.chunks(2 * 512) {
        push(chunk);
    }
    let blocks = got.lock();
    assert!(blocks.len() >= 9, "{}", blocks.len());
    assert!(blocks.iter().all(|b| b.len() == 2400));
    // The first channel only, resampled: a half-scale tone, not the -1.0 of the second channel.
    let r = rms_int16(&blocks[5]);
    assert!((r - 0.5 * 32767.0 / 2f64.sqrt()).abs() < 300.0, "{r}");

    let got = Arc::new(Mutex::new(Vec::<Vec<i16>>::new()));
    let g = got.clone();
    let mut push = audio::converter_i16_for_tests(1, &spec, Box::new(move |b| g.lock().push(b.to_vec()))).unwrap();
    let pcm: Vec<i16> = (0..5000).map(|i| (i % 300) as i16).collect();
    push(&pcm[..1000]);
    push(&pcm[1000..]);
    let blocks = got.lock();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks.concat(), pcm[..4800].to_vec()); // passed through untouched
    assert_eq!(audio::float_to_int16(1.5), 32767);
    assert_eq!(audio::float_to_int16(-1.5), -32768);
    assert_eq!(audio::float_to_int16(0.99999), 32766);
    let _ = std::io::stderr().flush();
}

#[test]
#[ignore = "live: opens the Mac's microphone for one second"]
fn the_real_microphone_delivers_24k_blocks() {
    let sd = CpalBackend::new();
    let got = Arc::new(Mutex::new(Vec::<usize>::new()));
    let g = got.clone();
    let spec = InputSpec { sample_rate: SAMPLE_RATE, block_frames: block_frames(), device: None };
    let make = || sd.make_input(spec, Box::new({ let g = g.clone(); move |b| g.lock().push(b.len()) }), Box::new(|_| {})).map_err(|e| anyhow!(e));
    let mut s = open_input(&sd, make, "test").unwrap();
    std::thread::sleep(Duration::from_millis(1000));
    s.stop().unwrap();
    s.close().unwrap();
    let got = got.lock();
    assert!(got.len() >= 5 && got.iter().all(|&n| n == 2400), "{got:?}");
}

#[test]
fn the_ears_can_be_shared_across_threads() {
    fn shared<T: Send + Sync>() {}
    shared::<Ears>();
    shared::<PcmQueue>();
    fn sendable<T: Send>() {}
    sendable::<WakeSpotter>();
}
