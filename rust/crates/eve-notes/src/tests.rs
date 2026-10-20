//! `tests/test_notes.py`: eve recording the room.
//!
//! No audio leaves the machine and no model is called. The transcriber is a fake that returns scripted text, so the
//! tests are about the parts that decide behaviour: when it stops, what reaches disk, and what happens when the
//! transcription fails.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use chrono::{NaiveDate, NaiveDateTime};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use crate::*;
use eve_records::RecallConfig;

pub(crate) fn when(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, 0).unwrap()
}

pub(crate) static WHEN: LazyLock<NaiveDateTime> = LazyLock::new(|| when(2026, 9, 11, 14, 30));

pub(crate) fn summary() -> Value {
    json!({
        "kind": "conversation",
        "title": "The servo order and the case",
        "summary": "Two people worked out what to buy and who does it.",
        "speakers": [],
        "topics": [{"heading": "The servo", "points": ["the right servo sticks under load",
                                                      "the case needs reprinting either way"]}],
        "decisions": ["order two spare SCS0009 servos"],
        "actions": [{"who": null, "what": "place the order", "when": "Friday", "at": null}],
        "questions": ["is the sticking mechanical or electrical"],
        "quotes": [],
        "ideas": [],
        "todos": [],
        "follow_ups": [],
        "unclear": ["a part number around 12 minutes"],
    })
}

type SummaryFn = Arc<dyn Fn(&str) -> anyhow::Result<String> + Send + Sync>;
type DiarizeFn = Arc<dyn Fn(&str, &[String]) -> anyhow::Result<Value> + Send + Sync>;

/// The scripted transcriber. `summarize_with` and `diarize_with` stand in for the Python subclasses that override
/// those methods.
pub(crate) struct FakeTranscriber {
    pub texts: Vec<String>,
    pub calls: Mutex<Vec<Vec<u8>>>,
    pub prompts: Mutex<Vec<String>>,
    pub summaries: Mutex<Vec<String>>,
    pub diarized: Mutex<Vec<(String, Vec<String>)>>,
    pub raise_on_transcribe: bool,
    pub raise_on_summary: bool,
    pub summarize_with: Option<SummaryFn>,
    pub diarize_with: Option<DiarizeFn>,
}

impl FakeTranscriber {
    pub fn new(texts: &[&str]) -> Self {
        let texts = if texts.is_empty() { vec!["they talked about the servos"] } else { texts.to_vec() };
        Self {
            texts: texts.into_iter().map(String::from).collect(),
            calls: Mutex::new(Vec::new()),
            prompts: Mutex::new(Vec::new()),
            summaries: Mutex::new(Vec::new()),
            diarized: Mutex::new(Vec::new()),
            raise_on_transcribe: false,
            raise_on_summary: false,
            summarize_with: None,
            diarize_with: None,
        }
    }
}

impl TranscribeClient for FakeTranscriber {
    fn transcribe(&self, wav: &[u8], prompt: &str) -> anyhow::Result<String> {
        let mut calls = self.calls.lock();
        calls.push(wav.to_vec());
        self.prompts.lock().push(prompt.to_string());
        if self.raise_on_transcribe {
            anyhow::bail!("the model refused");
        }
        Ok(self.texts[(calls.len() - 1).min(self.texts.len() - 1)].clone())
    }

    fn summarize(&self, transcript: &str) -> anyhow::Result<String> {
        self.summaries.lock().push(transcript.to_string());
        if let Some(f) = &self.summarize_with {
            return f(transcript);
        }
        if self.raise_on_summary {
            anyhow::bail!("the model refused");
        }
        Ok(summary().to_string())
    }

    fn has_diarize(&self) -> bool {
        self.diarize_with.is_some()
    }

    fn diarize(&self, _audio: &[u8], _f: &str, mime: &str, names: &[String], _refs: &[String]) -> anyhow::Result<Value> {
        self.diarized.lock().push((mime.to_string(), names.to_vec()));
        match &self.diarize_with {
            Some(f) => f(mime, names),
            None => Err(NotesError::NoMethod("diarize").into()),
        }
    }
}

/// Hands out blocks of loud audio, like ears' 100 ms subscription.
pub(crate) struct FakeEars {
    pub blocks: usize,
    pub unsubscribed: AtomicUsize,
}

impl FakeEars {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { blocks: 400, unsubscribed: AtomicUsize::new(0) })
    }
}

pub(crate) fn loud() -> Vec<u8> {
    // 100 ms at 24 kHz
    (0..1200).flat_map(|_| [6000i16, -6000]).flat_map(|s| s.to_le_bytes()).collect()
}

impl AudioSource for FakeEars {
    fn subscribe(&self) -> Arc<dyn MicQueue> {
        let q = Arc::new(BlockQueue::new());
        for _ in 0..self.blocks {
            q.put_nowait(loud());
        }
        q
    }

    fn unsubscribe(&self, _q: &Arc<dyn MicQueue>) {
        self.unsubscribed.fetch_add(1, Ordering::SeqCst);
    }
}

pub(crate) fn cfg(tmp: &Path) -> RecallConfig {
    RecallConfig::new(tmp.join("debrief"), tmp.join("notes"))
}

pub(crate) fn taker_with(
    tmp: &Path,
    client: Arc<FakeTranscriber>,
    notes_cfg: NotesConfig,
    ears: Arc<dyn AudioSource>,
    wall: NaiveDateTime,
) -> RoomNotes {
    let opts = RoomNotesOptions { wall: Arc::new(move || wall), ..Default::default() };
    RoomNotes::with_options(cfg(tmp), notes_cfg, Some(client), Some(ears), opts)
}

fn taker(tmp: &Path, client: Arc<FakeTranscriber>, secs: f64) -> RoomNotes {
    let nc = NotesConfig { segment_secs: secs, ..Default::default() };
    taker_with(tmp, client, nc, FakeEars::new(), *WHEN)
}

pub(crate) async fn run_to_end(t: &RoomNotes, secs: u64) {
    assert_eq!(t.start()["started"], json!(true));
    let task = t.task().expect("a running session");
    tokio::time::timeout(Duration::from_secs(secs), task).await.expect("the session ended in time");
}

pub(crate) fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

pub(crate) fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ---- stopping ---------------------------------------------------------------------------------------------------

#[test]
fn test_the_stop_phrase_is_narrow_enough_for_a_real_meeting() {
    // This fires on a transcript of people talking, where "stop" is a common word.
    for said in ["ok stop taking notes", "stop the notes please", "that's enough notes", "let's stop recording",
                 "eve, stop", "no more notes thanks"] {
        assert!(wants_stop(said), "{said}");
    }
    for said in ["we should stop the meeting soon", "stop the car", "I took notes last time",
                 "can you note that down", "the recording studio", ""] {
        assert!(!wants_stop(said), "{said}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_hearing_the_stop_ends_the_recording() {
    let d = tmp();
    let t = taker(d.path(), Arc::new(FakeTranscriber::new(&["alright that's enough notes"])), 0.5);
    run_to_end(&t, 10).await;
    assert!(!t.active());
    assert_eq!(t.state().stopped_by, "asked out loud");
    assert!(t.state().path.is_some_and(|p| p.exists()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stopping_by_hand_needs_no_transcript() {
    // The command and a tap on the robot are the stops with no latency.
    let d = tmp();
    let t = taker(d.path(), Arc::new(FakeTranscriber::new(&["a long meeting about nothing"])), 30.0); // no segment would land in time
    t.start();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let out = t.stop("a tap on the robot").await;
    assert!(out["ok"] == json!(true) && !t.active());
    assert_eq!(t.state().stopped_by, "a tap on the robot");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_time_cap_stops_it_even_if_nobody_does() {
    let d = tmp();
    let nc = NotesConfig { segment_secs: 0.4, max_minutes: 0.0001, ..Default::default() };
    let t = taker_with(d.path(), Arc::new(FakeTranscriber::new(&["and then we talked some more"])), nc, FakeEars::new(), *WHEN);
    run_to_end(&t, 10).await;
    assert!(t.state().stopped_by.contains("cap"));
}

// ---- what reaches disk ------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_file_holds_the_notes_and_the_transcript() {
    let d = tmp();
    let t = taker(d.path(), Arc::new(FakeTranscriber::new(&["the right servo sticks", "stop taking notes"])), 0.5);
    run_to_end(&t, 10).await;
    let path = t.state().path.unwrap();
    let text = read(&path);
    assert!(text.contains("source: eve-notes"));
    assert!(text.contains("# The servo order and the case"));
    assert!(text.contains("## Decisions") && text.contains("order two spare SCS0009 servos"));
    assert!(text.contains("## Actions") && text.contains("Friday"));
    assert!(text.contains("## Open questions"));
    assert!(text.contains("## Transcript") && text.contains("the right servo sticks"));
    assert!(text.contains("[00:"), "the transcript is stamped so you can find a moment");
    // what it could not make out is said, not guessed away
    assert!(text.contains("Not made out") && text.contains("part number"));
    assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_transcript_can_be_left_out() {
    let d = tmp();
    let nc = NotesConfig { segment_secs: 0.5, keep_transcript: false, ..Default::default() };
    let client = Arc::new(FakeTranscriber::new(&["something private", "stop taking notes"]));
    let t = taker_with(d.path(), client, nc, FakeEars::new(), *WHEN);
    run_to_end(&t, 10).await;
    let text = read(&t.state().path.unwrap());
    assert!(!text.contains("## Transcript") && !text.contains("something private"));
    assert!(text.contains("# The servo order"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_failed_write_up_still_keeps_every_word() {
    // The transcript is the irreplaceable half: it cannot be recomputed.
    let d = tmp();
    let mut client = FakeTranscriber::new(&["they said something important", "stop taking notes"]);
    client.raise_on_summary = true;
    let t = taker(d.path(), Arc::new(client), 0.5);
    run_to_end(&t, 10).await;
    let text = read(&t.state().path.unwrap());
    assert!(text.contains("they said something important"));
    assert!(text.contains("write-up failed"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_recording_with_nothing_said_leaves_no_file() {
    let d = tmp();
    let said: Arc<Mutex<Vec<String>>> = Arc::default();
    let heard = said.clone();
    let opts = RoomNotesOptions {
        wall: Arc::new(|| *WHEN),
        say: Some(Arc::new(move |s: &str| heard.lock().push(s.to_string()))),
        ..Default::default()
    };
    let nc = NotesConfig { segment_secs: 0.5, ..Default::default() };
    let t = RoomNotes::with_options(cfg(d.path()), nc, Some(Arc::new(FakeTranscriber::new(&[""]))), Some(FakeEars::new()), opts);
    t.start();
    tokio::time::sleep(Duration::from_millis(400)).await;
    t.stop("asked").await;
    assert!(t.state().path.is_none());
    assert!(recent(&cfg(d.path()), 20).unwrap().is_empty());
    assert_eq!(*said.lock(), vec!["nothing".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_failing_transcriber_loses_a_segment_not_the_session() {
    let d = tmp();
    let mut client = FakeTranscriber::new(&[]);
    client.raise_on_transcribe = true;
    let t = taker(d.path(), Arc::new(client), 0.5);
    t.start();
    tokio::time::sleep(Duration::from_millis(500)).await;
    t.stop("asked").await;
    assert!(t.state().failed >= 1);
    assert!(!t.active(), "a failed segment must not wedge the recording");
}

// ---- refusals and housekeeping ----------------------------------------------------------------------------------

#[test]
fn test_it_refuses_to_start_without_a_key_or_a_microphone() {
    let d = tmp();
    let no_client = RoomNotes::new(cfg(d.path()), NotesConfig::default(), None, Some(FakeEars::new()));
    assert_eq!(no_client.start()["ok"], json!(false));
    let no_ears = RoomNotes::new(cfg(d.path()), NotesConfig::default(), Some(Arc::new(FakeTranscriber::new(&[]))), None);
    assert_eq!(no_ears.start()["ok"], json!(false));
    assert!(make_notes_client(Some(&Default::default())).is_none());
    // a microphone that is not running is no microphone
    let off = Arc::new(PushedAudio::new());
    off.listening.store(false, Ordering::SeqCst);
    let no_mic = RoomNotes::new(cfg(d.path()), NotesConfig::default(), Some(Arc::new(FakeTranscriber::new(&[]))), Some(off));
    assert_eq!(no_mic.start(), json!({"ok": false, "error": "the microphone is not running"}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_starting_twice_does_not_start_twice() {
    let d = tmp();
    let t = taker(d.path(), Arc::new(FakeTranscriber::new(&[])), 30.0);
    let first = t.start();
    let second = t.start();
    t.stop("asked").await;
    assert_eq!(first["started"], json!(true));
    assert_eq!(second["already"], json!(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stopping_when_nothing_is_running_says_so() {
    let d = tmp();
    let t = taker(d.path(), Arc::new(FakeTranscriber::new(&[])), 0.5);
    assert_eq!(t.stop("asked").await["ok"], json!(false));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_microphone_is_always_handed_back() {
    // A subscription left behind would keep the wake word muted for good.
    let d = tmp();
    let ears = FakeEars::new();
    let nc = NotesConfig { segment_secs: 0.4, ..Default::default() };
    let t = taker_with(d.path(), Arc::new(FakeTranscriber::new(&[])), nc, ears.clone(), *WHEN);
    t.start();
    tokio::time::sleep(Duration::from_millis(300)).await;
    t.stop("asked").await;
    assert_eq!(ears.unsubscribed.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_segment_carries_the_tail_of_the_last_one() {
    // Context across a boundary is how names stay spelled the same way.
    let d = tmp();
    let client = Arc::new(FakeTranscriber::new(&["the SCS0009 servo", "is what sticks", "stop taking notes"]));
    let t = taker(d.path(), client.clone(), 0.3);
    run_to_end(&t, 10).await;
    let prompts = client.prompts.lock().clone();
    assert!(prompts[1..].iter().any(|p| p.contains("SCS0009")), "{prompts:?}");
}

// ---- audio helpers ----------------------------------------------------------------------------------------------

fn pack(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

#[test]
fn test_silence_is_not_uploaded() {
    assert_eq!(rms(&[0u8; 4000]), 0.0);
    let loud: Vec<i16> = (0..500).flat_map(|_| [8000, -8000]).collect();
    assert!(rms(&pack(&loud)) > 1000.0);
}

#[test]
fn test_wav_is_a_real_container_at_the_microphones_rate() {
    let pcm = pack(&[1234; 1000]);
    let wav = to_wav(&pcm, SAMPLE_RATE);
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "one channel");
    assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16, "two bytes a sample");
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), SAMPLE_RATE);
    assert_eq!(wav_seconds(&wav), 1000.0 / SAMPLE_RATE as f64);
    // the bytes Python's wave module writes, odd lengths too
    assert_eq!(
        to_wav(&[1, 2, 3], 24000),
        [&b"RIFF\x27\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\xc0\x5d\x00\x00\x80\xbb\x00\x00\x02\x00\x10\x00data\x03\x00\x00\x00"[..], &[1, 2, 3]].concat()
    );
    assert_eq!(wav_seconds(b"not a wav"), 0.0);
}

#[test]
fn test_render_survives_a_model_that_ignores_the_schema() {
    let state = NotesState { started_wall: Some(*WHEN), words: 12, stopped_by: "asked".into(), ..Default::default() };
    for junk in [json!({}), json!({"title": null, "points": "not a list"})] {
        let text = render(junk.as_object().unwrap(), "[00:00] something", &state, &RenderOpts::default());
        assert!(text.starts_with("---") && text.contains("## Transcript"));
    }
}

fn env(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn test_configured_reads_the_env() {
    assert_eq!(configured(Some(&env(&[]))).segment_secs, 12.0);
    assert_eq!(configured(Some(&env(&[("EVE_NOTES_SEGMENT_SECS", "30")]))).segment_secs, 30.0);
    assert_eq!(configured(Some(&env(&[("EVE_NOTES_SEGMENT_SECS", "9999")]))).segment_secs, 60.0);
    assert!(!configured(Some(&env(&[("EVE_NOTES_KEEP_TRANSCRIPT", "0")]))).keep_transcript);
    // float() as Python reads it: whitespace, underscores, nan (clamped to the floor), junk falls back
    assert_eq!(configured(Some(&env(&[("EVE_NOTES_SEGMENT_SECS", " 1_0 ")]))).segment_secs, 10.0);
    assert_eq!(configured(Some(&env(&[("EVE_NOTES_SEGMENT_SECS", "nan")]))).segment_secs, 4.0);
    assert_eq!(configured(Some(&env(&[("EVE_NOTES_SEGMENT_SECS", "twelve")]))).segment_secs, 12.0);
    assert_eq!(configured(Some(&env(&[("EVE_NOTES_MAX_MINUTES", "0")]))).max_minutes, 1.0);
    assert!(configured(Some(&env(&[("EVE_NOTES_KEEP_TRANSCRIPT", "")]))).keep_transcript);
}

// ---- the request the API actually receives ----------------------------------------------------------------------

/// The SDK's two calls, recorded, with scripted replies (the Python test's stub `openai` module).
pub(crate) struct FakeSdk {
    pub sent: Mutex<Vec<Value>>,
    pub fields: Mutex<Vec<Map<String, Value>>>,
    pub reply: Value,
}

impl FakeSdk {
    pub fn new(reply: Value) -> Arc<Self> {
        Arc::new(Self { sent: Mutex::default(), fields: Mutex::default(), reply })
    }
}

impl NotesSdk for FakeSdk {
    fn transcriptions_create(&self, fields: Map<String, Value>, _file: eve_openrouter::Part) -> anyhow::Result<Value> {
        self.fields.lock().push(fields);
        Ok(self.reply.clone())
    }

    fn responses_create(&self, body: Value) -> anyhow::Result<Value> {
        self.sent.lock().push(body);
        Ok(self.reply.clone())
    }
}

pub(crate) fn response(text: &str) -> Value {
    json!({"output": [{"type": "message", "content": [{"type": "output_text", "text": text}]}]})
}

#[test]
fn test_the_summary_asks_for_a_schema_and_not_a_bare_json_object() {
    // A bare `json_object` format is rejected unless the word "json" appears in the input, and the input here is a
    // transcript of whatever was said in the room. Live probe, 2026-09-11: every summary came back 400 until this
    // was a schema. A fake client cannot catch that, so the request shape is asserted.
    let d = tmp();
    let sdk = FakeSdk::new(response(
        r#"{"kind":"memo","title":"t","summary":"g","speakers":[],"topics":[],"decisions":[],"actions":[],"questions":[],"quotes":[],"ideas":[],"todos":[],"follow_ups":[],"unclear":[]}"#,
    ));
    let mut client = OpenAINotesClient::with_sdk(DEFAULT_MODEL, DEFAULT_SUMMARY_MODEL, sdk.clone());
    client.ledger = eve_spend::Ledger::at(d.path());
    client.summarize("we talked about the servos").unwrap();
    let sent = sdk.sent.lock()[0].clone();
    let fmt = &sent["text"]["format"];
    assert_eq!(fmt["type"], json!("json_schema"), "json_object needs the word json in the input");
    assert_eq!(fmt["strict"], json!(true));
    assert_eq!(fmt["schema"], *SUMMARY_SCHEMA);
    assert!(sent["max_output_tokens"].as_u64().unwrap() >= 8000, "detailed notes of a long meeting need the room");
    // every field the write-up renders must be required, or a model may omit it
    let required = SUMMARY_SCHEMA["required"].as_array().unwrap();
    for key in ["kind", "title", "summary", "speakers", "topics", "decisions", "actions", "questions", "quotes", "ideas",
                "todos", "follow_ups", "unclear"] {
        assert!(required.contains(&json!(key)), "{key}");
    }

    fn strict(node: &Value) {
        // strict mode: every object closed, every property required
        if node.get("type") == Some(&json!("object")) {
            assert_eq!(node["additionalProperties"], json!(false));
            let mut req: Vec<String> = node["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
            let mut props: Vec<String> = node["properties"].as_object().unwrap().keys().cloned().collect();
            req.sort();
            props.sort();
            assert_eq!(req, props);
            for child in node["properties"].as_object().unwrap().values() {
                strict(child);
            }
        }
        if node.get("type") == Some(&json!("array")) {
            strict(&node["items"]);
        }
    }
    strict(&SUMMARY_SCHEMA);
}

// ---- the seam between two segments ------------------------------------------------------------------------------

#[test]
fn test_the_overlap_is_not_transcribed_twice() {
    // The exact line a live recording produced: a segment whose whole content was the last word of the one before
    // it (2026-09-11).
    assert_eq!(stitch("Do we need the transcript kept as well as the summary?", "Summary?"), "");
    assert_eq!(stitch("...ship it on Friday.", "Friday. Guru will flash the board"), "Guru will flash the board");
    // several words of overlap, and punctuation and case are not the difference
    assert_eq!(stitch("we talked about the right servo", "The Right Servo, and it sticks"), "and it sticks");
}

#[test]
fn test_a_segment_that_only_continues_is_left_alone() {
    assert_eq!(stitch("the first thing", "a different thing entirely"), "a different thing entirely");
    assert_eq!(stitch("", "the very first segment"), "the very first segment");
    assert_eq!(stitch("something", ""), "");
    // a word repeated inside a line is not a seam, so nothing is dropped
    assert_eq!(stitch("the board", "the docs are written"), "the docs are written");
}

// ---- where the file goes (L6: one memory folder) ----------------------------------------------------------------

#[test]
fn test_meeting_notes_live_beside_the_transcripts_and_never_overwrite() {
    // Meeting notes are under transcripts/meetings/<date>/ so memory_search finds them; two meetings that start in
    // the same minute under the same title are two files (they used to be one, the second winning).
    let d = tmp();
    let c = cfg(d.path());
    assert_eq!(notes_dir(&c), c.transcripts_dir().join("meetings"));
    let state = NotesState { started_wall: Some(*WHEN), ..Default::default() };
    let s = summary();
    let s = s.as_object().unwrap();
    let first = write_notes(&c, s, "first meeting words", &state, &RenderOpts::default()).unwrap();
    let second = write_notes(&c, s, "second meeting words", &state, &RenderOpts::default()).unwrap();
    assert_ne!(first, second);
    assert_eq!(first.parent().unwrap(), c.transcripts_dir().join("meetings").join("2026-09-11"));
    assert!(read(&first).contains("first meeting words") && read(&second).contains("second meeting words"));
    assert_eq!(
        second.file_name().unwrap().to_string_lossy(),
        format!("{}-2.md", first.file_stem().unwrap().to_string_lossy())
    );
    assert_eq!(std::fs::metadata(&first).unwrap().permissions().mode() & 0o777, 0o600);
    let mut both = vec![first.clone(), second.clone()];
    both.sort();
    both.reverse();
    assert_eq!(recent(&c, 20).unwrap()[..2], both[..]);
}

// ---- smaller parity checks (no Python test of their own) -------------------------------------------------------

#[test]
fn stamps_slugs_and_the_pushed_audio_match_python() {
    assert_eq!(stamp(0.0), "00:00");
    assert_eq!(stamp(59.9), "00:59");
    assert_eq!(stamp(3725.0), "62:05");
    assert_eq!(stamp(-3.0), "00:00");
    assert_eq!(stamp(f64::NAN), "00:00");
    assert_eq!(slug("The Servo, order & the CASE! extra words"), "the-servo-order-the-case");
    assert_eq!(slug("!!!"), "notes");
    assert_eq!(key("Friday."), "friday");
    assert_eq!(words("Hello, there — everyone!"), "hello there everyone");
    assert_eq!(at(Some(&json!("[1:05]"))), Some("01:05".into()));
    assert_eq!(at(Some(&json!("noon"))), None);
    assert_eq!(items(Some(&json!(["a  b", "", 3, 1.5, true, null, ["x"]])), None), vec!["a b", "3", "1.5", "True"]);
    assert_eq!(items(Some(&json!(["a", "b", "c"])), Some(2)), vec!["a", "b"]);
    assert_eq!(items(Some(&json!("not a list")), None), Vec::<String>::new());
}

#[test]
fn the_pushed_audio_feeds_only_a_listening_session() {
    // tests/test_phone_notes.py::test_pushed_audio_feeds_only_a_listening_session
    let mic = PushedAudio::new();
    let l = loud();
    assert!(!mic.put(&l), "nobody subscribed: dropped");
    let q = mic.subscribe();
    let mut odd = l.clone();
    odd.push(1);
    assert!(mic.put(&odd));
    let mine = mic.subscribe_queue();
    assert!(mic.put(&odd));
    assert_eq!(mine.get_nowait().unwrap().len(), l.len(), "whole samples only");
    mic.unsubscribe(&q); // not the current queue: nothing changes
    assert!(mic.put(&l));
    let current: Arc<dyn MicQueue> = mine;
    mic.unsubscribe(&current);
    assert!(!mic.put(&l));
}
