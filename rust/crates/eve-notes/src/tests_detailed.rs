//! `tests/test_notes_detailed.py`: detailed notes (owner, 2026-10-04): the write-up's shape, the full transcript
//! first, and who spoke.
//!
//! Every model here is a fake: no audio leaves the machine and nothing is called.

use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, LazyLock, OnceLock};

use base64::Engine as _;
use chrono::NaiveDateTime;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use crate::tests::{cfg, read, run_to_end, taker_with, tmp, when, FakeEars, FakeTranscriber};
use crate::*;

static WHEN: LazyLock<NaiveDateTime> = LazyLock::new(|| when(2026, 10, 4, 9, 5));

fn lines() -> Vec<Line> {
    vec![
        Line::new(3.0, "Good morning. I think we should ship on Friday the twelfth.", "Speaker 1"),
        Line::new(9.0, "Friday works, but the right servo still sticks under load.", "Speaker 2"),
        Line::new(15.0, "Then order two spare servos today for forty dollars.", "Speaker 1"),
    ]
}

fn conversation() -> Value {
    json!({
        "kind": "conversation",
        "title": "Shipping the robot Friday",
        "summary": "Two people agreed to ship on Friday the twelfth once spare servos are ordered.",
        "speakers": [{"label": "Speaker 1", "name": null, "about": "led and set the date"},
                     {"label": "Speaker 2", "name": null, "about": "raised the servo"}],
        "topics": [{"heading": "Ship date", "points": ["Friday the twelfth", "the right servo sticks under load"]}],
        "decisions": ["ship on Friday the twelfth"],
        "actions": [{"who": "Speaker 1", "what": "order two spare servos", "when": "today", "at": "00:15"}],
        "questions": ["is the sticking mechanical"],
        "quotes": [{"at": "00:15", "speaker": "Speaker 1", "text": "order two spare servos today for forty dollars"},
                   {"at": "00:09", "speaker": "Speaker 2", "text": "the servo is broken beyond repair"}],
        "ideas": [],
        "todos": [],
        "follow_ups": ["check the servo's datasheet"],
        "unclear": [],
    })
}

fn memo() -> Value {
    json!({
        "kind": "memo",
        "title": "Ideas for the robot case",
        "summary": "Thinking out loud about the case.",
        "speakers": [{"label": "Speaker 1", "name": null, "about": null}],
        "topics": [{"heading": "The case", "points": ["print it in PETG"]}],
        "decisions": [],
        "actions": [],
        "questions": [],
        "quotes": [],
        "ideas": ["a magnetic lid"],
        "todos": ["buy PETG"],
        "follow_ups": [],
        "unclear": [],
    })
}

/// `{**base, **extra}`.
fn with(base: Value, extra: Value) -> Value {
    let mut m = base.as_object().unwrap().clone();
    for (k, v) in extra.as_object().unwrap() {
        m.insert(k.clone(), v.clone());
    }
    Value::Object(m)
}

fn wav_secs(data: &[u8]) -> f64 {
    let rate = u32::from_le_bytes(data[24..28].try_into().unwrap());
    let frames = u32::from_le_bytes(data[40..44].try_into().unwrap()) / 2;
    assert_eq!(wav_seconds(data), frames as f64 / rate as f64);
    frames as f64 / rate as f64
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// ---- the shape --------------------------------------------------------------------------------------------------

#[test]
fn test_schema_and_render_round_trip() {
    // What the model returns survives JSON, the cleaning and the file: every section, every number.
    let back = clean_summary(&serde_json::from_str(&conversation().to_string()).unwrap(), &lines());
    assert_eq!(back["kind"], json!("conversation"));
    assert_eq!(back["title"], json!("Shipping the robot Friday"));
    assert_eq!(back["actions"], conversation()["actions"]);
    let state = NotesState { started_wall: Some(*WHEN), words: 30, stopped_by: "asked".into(), ..Default::default() };
    let opts = RenderOpts { speakers: Some(strs(&["Speaker 1", "Speaker 2"])), diarized: "yes".into(), ..Default::default() };
    let text = render(&back, &render_lines(&lines()), &state, &opts);
    assert!(text.contains("status: notes") && text.contains("kind: conversation") && text.contains("speakers: 2"));
    assert!(text.contains("# Shipping the robot Friday") && text.contains("Friday the twelfth once spare servos"));
    for heading in ["## Speakers", "## Topics", "### Ship date", "## Decisions", "## Actions", "## Open questions",
                    "## Quotes", "## Follow-ups", "## Transcript"] {
        assert!(text.contains(heading), "{heading}");
    }
    assert!(text.contains("- Speaker 1: led and set the date"));
    assert!(text.contains("- Speaker 1: order two spare servos (by today) [00:15]"));
    assert!(text.contains("[00:09] Speaker 2: Friday works"));
    assert!(text.contains("forty dollars"));
}

#[test]
fn render_writes_the_bytes_python_writes() {
    // The whole file for one known input, as notes.render printed it.
    let back = clean_summary(&conversation(), &lines());
    let state = NotesState { started_wall: Some(*WHEN), words: 30, stopped_by: String::new(), ..Default::default() };
    let opts = RenderOpts { speakers: Some(strs(&["Speaker 1", "Speaker 2"])), diarized: "yes".into(), ..Default::default() };
    let want = "---\nstatus: notes\nsource: eve-notes\nstarted: 2026-10-04 09:05\nminutes: 0.3\nwords: 30\n\
stopped_by: asked\nkind: conversation\nspeakers: 2\ndiarized: yes\n---\n\n# Shipping the robot Friday\n\n\
Two people agreed to ship on Friday the twelfth once spare servos are ordered.\n\n## Speakers\n\n\
- Speaker 1: led and set the date\n- Speaker 2: raised the servo\n\n## Topics\n\n### Ship date\n\n\
- Friday the twelfth\n- the right servo sticks under load\n\n## Decisions\n\n- ship on Friday the twelfth\n\n\
## Actions\n\n- Speaker 1: order two spare servos (by today) [00:15]\n\n## Open questions\n\n\
- is the sticking mechanical\n\n## Quotes\n\n\
- \"order two spare servos today for forty dollars\" (Speaker 1, 00:15)\n\n## Follow-ups\n\n\
- check the servo's datasheet\n\n## Transcript\n\n<!-- automatic transcription, split by voice: a speaker label \
is one voice, not a known person; expect wrong words -->\n\n[00:03] Speaker 1: Good morning. I think we should ship \
on Friday the twelfth.\n[00:09] Speaker 2: Friday works, but the right servo still sticks under load.\n[00:15] \
Speaker 1: Then order two spare servos today for forty dollars.\n";
    assert_eq!(render(&back, &render_lines(&lines()), &state, &opts), want);
    let empty = NotesState { started_wall: Some(*WHEN), ..Default::default() };
    let pending = RenderOpts { pending: true, live: "[00:00] x".into(), diarized: "partial".into(), ..Default::default() };
    assert_eq!(
        render(&Map::new(), "[00:00] x", &empty, &pending),
        "---\nstatus: transcript\nsource: eve-notes\nstarted: 2026-10-04 09:05\nminutes: 0\nwords: 0\n\
stopped_by: asked\ndiarized: partial\n---\n\n# Notes\n\n> Writing these up. The notes appear above the transcript \
when they are done.\n\n## Transcript\n\n<!-- automatic transcription: expect wrong words and no speaker labels -->\n\n\
[00:00] x\n\n## Live transcript\n\n<!-- the transcript taken while recording, kept because the speaker pass missed \
part of it -->\n\n[00:00] x\n"
    );
}

#[test]
fn test_a_conversation_and_a_memo_get_their_own_sections() {
    let heads = |s: &Map<String, Value>, sp: &[&str]| -> Vec<&'static str> {
        note_sections(s, Some(&strs(sp))).into_iter().map(|(h, _)| h).collect()
    };
    let conv = heads(&clean_summary(&conversation(), &lines()), &["Speaker 1", "Speaker 2"]);
    assert_eq!(conv[..5], ["Speakers", "Topics", "Decisions", "Actions", "Open questions"]);
    let m = heads(&clean_summary(&memo(), &lines()), &["Speaker 1"]);
    assert_eq!(m[..3], ["Topics", "Ideas", "To-dos"], "a memo leads with its ideas and to-dos");
    assert!(!m.contains(&"Speakers"), "one voice talking to itself needs no speaker list");
    // a voice the pass heard is listed even when the write-up says nothing about it
    let s = clean_summary(&conversation(), &lines());
    let rows: HashMap<&str, Vec<Row>> =
        note_sections(&s, Some(&strs(&["Speaker 1", "Speaker 2", "Speaker 3"]))).into_iter().collect();
    assert_eq!(rows["Speakers"].last(), Some(&Row::Text("Speaker 3".into())));
}

#[test]
fn test_the_prompt_states_its_values_as_sets_and_carries_no_banned_title() {
    assert!(SUMMARY_PROMPT.contains("exactly one of two values"));
    assert!(SUMMARY_PROMPT.contains("conversation:") && SUMMARY_PROMPT.contains("memo:"));
    assert!(!SUMMARY_PROMPT.contains("Not \"") && !SUMMARY_PROMPT.contains("Meeting notes"));
    assert!(SUMMARY_PROMPT.contains("instruction") && SUMMARY_PROMPT.contains("Never invent"));
}

#[test]
fn the_prompts_are_byte_identical_to_the_python() {
    let py = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/notes.py"))
        .expect("the Python source beside the port");
    for (name, prompt) in [("SUMMARY_PROMPT", SUMMARY_PROMPT), ("COMBINE_PROMPT", COMBINE_PROMPT)] {
        let start = py.find(&format!("{name} = \"\"\"")).expect(name) + name.len() + 6;
        let end = start + py[start..].find("\"\"\"").unwrap();
        assert_eq!(&py[start..end], prompt, "{name}");
    }
}

#[test]
fn test_a_quote_that_is_not_in_its_line_is_dropped() {
    let mut quotes = conversation()["quotes"].as_array().unwrap().clone();
    quotes.extend([
        json!({"at": "00:03", "speaker": "Speaker 2", "text": "we should ship on Friday"}), // the wrong voice
        json!({"at": "00:09", "speaker": null, "text": "ship on Friday the twelfth"}),     // the wrong line
        json!({"at": "[00:03]", "speaker": null, "text": "“I think we should ship on Friday”"}),
        json!({"at": "00:15", "speaker": "Speaker 1", "text": "order three spare servos"}), // a changed word
        json!("not a quote"),
    ]);
    let kept = check_quotes(Some(&Value::Array(quotes)), &lines());
    let texts: Vec<&str> = kept.iter().map(|q| q["text"].as_str().unwrap()).collect();
    assert_eq!(texts, ["order two spare servos today for forty dollars", "I think we should ship on Friday"]);
    assert_eq!(kept[1]["speaker"], json!("Speaker 1"), "the line's own speaker, not the model's");
    // an undiarized transcript: the quote carries no speaker
    assert_eq!(
        check_quotes(Some(&json!([{"at": "00:01", "speaker": "Speaker 1", "text": "hello there"}])),
                     &[Line::new(1.0, "Hello there, everyone.", "")]),
        vec![json!({"at": "00:01", "speaker": null, "text": "hello there"})]
    );
}

// ---- long recordings --------------------------------------------------------------------------------------------

struct WindowClient {
    inputs: Mutex<Vec<String>>,
    combined: Mutex<Vec<String>>,
    combine_fails: bool,
}

impl WindowClient {
    fn new(combine_fails: bool) -> Self {
        Self { inputs: Mutex::default(), combined: Mutex::default(), combine_fails }
    }
}

impl TranscribeClient for WindowClient {
    fn transcribe(&self, _wav: &[u8], _prompt: &str) -> anyhow::Result<String> {
        anyhow::bail!("this client only writes up")
    }

    fn summarize(&self, transcript: &str) -> anyhow::Result<String> {
        let mut inputs = self.inputs.lock();
        inputs.push(transcript.to_string());
        let n = inputs.len();
        Ok(with(memo(), json!({"kind": "conversation", "title": format!("Part {n}"), "summary": format!("part {n}"),
                                "topics": [{"heading": format!("Topic {n}"), "points": [format!("point {n}")]}],
                                "decisions": [format!("decision {n}"), "shared decision"]}))
        .to_string())
    }

    fn has_combine(&self) -> bool {
        true
    }

    fn combine(&self, partials: &str) -> anyhow::Result<String> {
        self.combined.lock().push(partials.to_string());
        if self.combine_fails {
            anyhow::bail!("the model refused");
        }
        Ok(with(conversation(), json!({"quotes": []})).to_string())
    }
}

fn long(minutes: usize) -> Vec<Line> {
    (0..minutes * 3).map(|t| Line::new(t as f64 * 20.0, format!("line {t}"), "Speaker 1")).collect()
}

#[test]
fn test_a_long_recording_is_written_up_in_overlapping_windows_then_merged() {
    let ls = long(55); // past the 40 minute threshold
    let client = WindowClient::new(false);
    let out = write_up(&client, &ls).unwrap();
    let inputs = client.inputs.lock().clone();
    assert_eq!(inputs.len(), 6, "ten-minute windows over 55 minutes");
    assert_eq!(windows(&ls, WINDOW_SECS, WINDOW_OVERLAP_SECS).len(), 6);
    assert!(inputs[1].starts_with("(Part 2 of 6 of one longer recording, "));
    // 30 s of overlap: the last lines of a window open the next one
    assert!(inputs[0].contains("[09:40] Speaker 1: line 29") && inputs[1].contains("[09:40] Speaker 1: line 29"));
    assert!(!inputs[0].contains("[10:00]"));
    let combined: Value = serde_json::from_str(&client.combined.lock()[0]).unwrap();
    assert_eq!(combined.as_array().unwrap().len(), 6);
    assert_eq!(out["title"], json!("Shipping the robot Friday"));
}

#[test]
fn test_a_failed_merge_is_done_in_code_and_a_short_recording_is_one_call() {
    let client = WindowClient::new(true);
    let out = write_up(&client, &long(45)).unwrap();
    let headings: Vec<&str> = out["topics"].as_array().unwrap().iter().map(|t| t["heading"].as_str().unwrap()).collect();
    assert_eq!(headings, (1..6).map(|n| format!("Topic {n}")).collect::<Vec<_>>());
    let shared = out["decisions"].as_array().unwrap().iter().filter(|d| *d == "shared decision").count();
    assert_eq!(shared, 1, "the overlap's repeats are merged away");
    assert_eq!(out["title"], json!("Part 1"));
    let short = WindowClient::new(false);
    write_up(&short, &long(39)).unwrap();
    assert!(short.inputs.lock().len() == 1 && short.combined.lock().is_empty());
    assert!(!short.inputs.lock()[0].starts_with("(Part"));
}

#[test]
fn the_combine_input_is_pythons_json_dumps() {
    // json.dumps(parts, ensure_ascii=False): ", " and ": " separators, non-ASCII kept as it is.
    let client = WindowClient::new(false);
    write_up(&client, &long(45)).unwrap();
    let body = client.combined.lock()[0].clone();
    assert!(body.starts_with("[{\"kind\": \"conversation\", \"title\": \"Part 1\", "), "{}", &body[..80]);
    let parts = [json!({"title": "Café", "decisions": ["Ölçü", "x"]}).as_object().unwrap().clone(),
                 json!({"decisions": ["ölçü", "y"], "summary": "  "}).as_object().unwrap().clone()];
    let merged = merge_partials(&parts);
    // the repeat key is ASCII-escaped before it is case-folded, so a non-ASCII letter's case still tells rows apart
    assert_eq!(merged["decisions"], json!(["Ölçü", "x", "ölçü", "y"]));
    assert_eq!(merged["title"], json!("Café"));
    assert_eq!(merged["summary"], json!(""));
    assert_eq!(merged["kind"], json!("memo"));
}

// ---- the transcript reaches disk first --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_transcript_is_on_disk_before_the_summary_and_survives_its_crash() {
    let d = tmp();
    let seen: Arc<Mutex<(String, u32)>> = Arc::default();
    let handle: Arc<OnceLock<RoomNotes>> = Arc::default();
    let mut client = FakeTranscriber::new(&["the right servo sticks", "stop taking notes"]);
    let (s2, h2) = (seen.clone(), handle.clone());
    client.summarize_with = Some(Arc::new(move |_t: &str| {
        let path = h2.get().unwrap().state().path;
        *s2.lock() = match path {
            Some(p) => (read(&p), std::fs::metadata(&p).unwrap().permissions().mode() & 0o777),
            None => (String::new(), 0),
        };
        anyhow::bail!("the summary crashed")
    }));
    let nc = NotesConfig { segment_secs: 0.5, ..Default::default() };
    let t = taker_with(d.path(), Arc::new(client), nc, FakeEars::new(), *WHEN);
    let _ = handle.set(t.clone());
    run_to_end(&t, 10).await;
    let (text, mode) = seen.lock().clone();
    assert!(text.contains("status: transcript") && text.contains("the right servo sticks"));
    assert!(text.contains("Writing these up") && mode == 0o600);
    let path = t.state().path.unwrap();
    let fin = read(&path);
    assert!(fin.contains("the right servo sticks") && fin.contains("write-up failed"));
    let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with('.') && n.ends_with(".tmp")
        })
        .collect();
    assert!(leftovers.is_empty(), "no temporary file is left behind");
}

// ---- who spoke --------------------------------------------------------------------------------------------------

fn pcm(secs: f64) -> Vec<u8> {
    crate::tests::loud().repeat((secs * 10.0) as usize)
}

/// Scripted pieces: each call returns the next list of segments, or fails when it is an error.
struct DiarizeClient {
    script: Mutex<VecDeque<Result<Value, String>>>,
    calls: Mutex<Vec<(Vec<u8>, String, String, Vec<String>, Vec<String>)>>,
}

impl DiarizeClient {
    fn new(script: Vec<Result<Value, String>>) -> Self {
        Self { script: Mutex::new(script.into()), calls: Mutex::default() }
    }

    fn names(&self) -> Vec<Vec<String>> {
        self.calls.lock().iter().map(|c| c.3.clone()).collect()
    }
}

impl TranscribeClient for DiarizeClient {
    fn transcribe(&self, _wav: &[u8], _prompt: &str) -> anyhow::Result<String> {
        anyhow::bail!("this client only diarizes")
    }

    fn summarize(&self, _transcript: &str) -> anyhow::Result<String> {
        anyhow::bail!("this client only diarizes")
    }

    fn has_diarize(&self) -> bool {
        true
    }

    fn diarize(&self, audio: &[u8], filename: &str, mime: &str, names: &[String], refs: &[String]) -> anyhow::Result<Value> {
        self.calls.lock().push((audio.to_vec(), filename.into(), mime.into(), names.to_vec(), refs.to_vec()));
        match self.script.lock().pop_front().expect("a scripted reply for every call") {
            Ok(v) => Ok(v),
            Err(e) => Err(anyhow::anyhow!(e)),
        }
    }
}

fn seg(speaker: &str, start: f64, end: f64, text: &str) -> Value {
    json!({"speaker": speaker, "start": start, "end": end, "text": text})
}

fn no_afconvert(_: &str) -> Option<std::path::PathBuf> {
    None
}

fn opts_wav(piece_secs: f64, max_bytes: usize) -> DiarizeOpts<'static> {
    DiarizeOpts { piece_secs, max_bytes, which: &no_afconvert, ..DiarizeOpts::default() }
}

fn ref_secs(url: &str) -> f64 {
    assert!(url.starts_with("data:audio/wav;base64,"));
    wav_secs(&base64::engine::general_purpose::STANDARD.decode(url.split_once(',').unwrap().1).unwrap())
}

#[test]
fn test_labels_stay_one_voice_across_pieces_through_known_speakers() {
    let audio = pcm(75.0);
    let client = DiarizeClient::new(vec![
        Ok(json!([seg("A", 0.0, 4.0, "first words"), seg("B", 4.5, 6.0, "short"), seg("B", 6.2, 9.0, "and more"),
                  seg("A", 10.0, 12.0, "back to A")])),
        Ok(json!([seg("Speaker 2", 0.0, 3.0, "B again"), seg("Speaker 1", 3.0, 6.0, "A again"),
                  seg("A", 7.0, 9.0, "a new voice")])), // labels reset: this A is not the first A
        Err("down".into()),
        Err("still down".into()),
    ]);
    let mut read = |start: f64, end: f64| -> std::io::Result<Vec<u8>> {
        let (a, b) = ((start * SAMPLE_RATE as f64) as usize * 2, (end * SAMPLE_RATE as f64) as usize * 2);
        Ok(audio[a.min(audio.len())..b.min(audio.len())].to_vec())
    };
    let d = diarize_recording(&mut read, 75.0, &client, None, &opts_wav(30.0, DIARIZE_MAX_BYTES)).unwrap();
    let s12 = strs(&["Speaker 1", "Speaker 2"]);
    assert_eq!(client.names(), vec![vec![], s12.clone(), s12.clone(), vec![]]);
    let refs = client.calls.lock()[1].4.clone();
    for r in &refs {
        let secs = ref_secs(r);
        assert!((2.0..=10.0).contains(&secs));
    }
    // B's clip is its two consecutive segments together (4.5 to 9.0 s): the longest run, at least 2 s
    assert!((ref_secs(&refs[1]) - 4.5).abs() < 0.01);
    assert_eq!(d.speakers, strs(&["Speaker 1", "Speaker 2", "Speaker 3"]));
    let got: Vec<(i64, &str)> = d.lines.iter().map(|ln| (ln.at.round_ties_even() as i64, ln.speaker.as_str())).collect();
    assert_eq!(
        got,
        [(0, "Speaker 1"), (4, "Speaker 2"), (10, "Speaker 1"), (30, "Speaker 2"), (33, "Speaker 1"), (37, "Speaker 3")]
    );
    assert_eq!(d.lines[1].text, "short and more", "one voice's consecutive segments are one line");
    assert!(d.status == "partial" && d.failed == vec![(60.0, 75.0)]);
}

#[test]
fn test_the_owner_is_reference_one_on_every_piece() {
    let audio = pcm(50.0);
    let client = DiarizeClient::new(vec![
        Ok(json!([seg("Ada", 0.0, 3.0, "mine"), seg("A", 3.0, 8.0, "theirs")])),
        Ok(json!([seg("Ada", 0.0, 2.0, "mine again"), seg("Speaker 1", 2.0, 5.0, "theirs again")])),
    ]);
    let mut read = |start: f64, end: f64| -> std::io::Result<Vec<u8>> {
        Ok(audio[(start * SAMPLE_RATE as f64) as usize * 2..(end * SAMPLE_RATE as f64) as usize * 2].to_vec())
    };
    let owner = ("Ada".to_string(), "data:audio/wav;base64,AAAA".to_string());
    let d = diarize_recording(&mut read, 50.0, &client, Some(&owner), &opts_wav(25.0, DIARIZE_MAX_BYTES)).unwrap();
    let calls = client.calls.lock();
    assert!(calls[0].3 == strs(&["Ada"]) && calls[0].4 == strs(&["data:audio/wav;base64,AAAA"]));
    assert_eq!(calls[1].3, strs(&["Ada", "Speaker 1"]));
    assert!(d.speakers == strs(&["Ada", "Speaker 1"]) && d.status == "yes");
}

#[test]
fn test_a_bad_reference_is_dropped_and_the_piece_tried_again() {
    let client = DiarizeClient::new(vec![Err("bad reference".into()), Ok(json!([seg("A", 0.0, 3.0, "hello")]))]);
    let owner = ("Ada".to_string(), "data:audio/wav;base64,AAAA".to_string());
    let mut read = |s: f64, e: f64| -> std::io::Result<Vec<u8>> { Ok(pcm(e - s)) };
    let d = diarize_recording(&mut read, 10.0, &client, Some(&owner), &opts_wav(DIARIZE_PIECE_SECS, DIARIZE_MAX_BYTES))
        .unwrap();
    assert_eq!(client.names(), vec![strs(&["Ada"]), vec![]]);
    assert!(d.speakers == strs(&["Speaker 1"]) && d.status == "yes");
}

#[test]
fn test_speaker_names_follow_first_appearance() {
    let mut names = SpeakerNames::new(&["Ada"]);
    let none: [&str; 0] = [];
    assert_eq!(
        [names.name(0, "B", &none), names.name(0, "A", &none), names.name(0, "B", &none), names.name(1, "B", &none),
         names.name(1, "Ada", &["Ada"])],
        ["Speaker 1", "Speaker 2", "Speaker 1", "Speaker 3", "Ada"]
    );
}

fn installed(_: &str) -> Option<std::path::PathBuf> {
    Some("/usr/bin/afconvert".into())
}

fn broken(_cmd: &[String]) -> Result<(), RunError> {
    Err(RunError::Failed(1))
}

fn fake_afconvert(cmd: &[String]) -> Result<(), RunError> {
    std::fs::write(cmd.last().unwrap(), b"m4a bytes")?;
    Ok(())
}

#[test]
fn test_without_afconvert_or_when_it_fails_the_pieces_go_as_wav_under_the_limit() {
    let audio = pcm(30.0);
    let limit = 500_000; // about 10 s of 24 kHz WAV
    let ways: [(Which, Run); 2] = [(&no_afconvert, &default_run), (&installed, &broken)];
    for (which, run) in ways {
        let pieces = encode_pieces(&audio, 100.0, SAMPLE_RATE, limit, which, run);
        assert_eq!(pieces.len(), 3);
        assert!(pieces.iter().all(|p| p.mime == "audio/wav" && p.data.len() <= limit));
        assert!(pieces[0].start == 100.0 && (pieces[2].end - 130.0).abs() < 0.01);
        assert!((pieces.iter().map(|p| wav_secs(&p.data)).sum::<f64>() - 30.0).abs() < 0.01);
    }
    let pieces = encode_pieces(&audio, 0.0, SAMPLE_RATE, DIARIZE_MAX_BYTES, &installed, &fake_afconvert);
    assert_eq!(pieces.len(), 1);
    assert!(pieces[0].data == b"m4a bytes" && pieces[0].filename == "piece.m4a" && pieces[0].mime == "audio/mp4");
    // without afconvert the pieces are planned short enough for WAV
    let client = DiarizeClient::new(vec![Ok(json!([seg("A", 0.0, 1.0, "x")])); 3]);
    let mut read = |s: f64, e: f64| -> std::io::Result<Vec<u8>> { Ok(pcm(e - s)) };
    diarize_recording(&mut read, 25.0, &client, None, &opts_wav(DIARIZE_PIECE_SECS, limit)).unwrap();
    assert_eq!(client.calls.lock().len(), 3);
}

#[test]
fn test_afconvert_makes_a_small_aac_piece() {
    if default_which("afconvert").is_none() {
        return; // macOS afconvert
    }
    let pieces = encode_pieces(&pcm(20.0), 0.0, SAMPLE_RATE, DIARIZE_MAX_BYTES, &default_which, &default_run);
    assert_eq!(pieces.len(), 1);
    assert!(pieces[0].mime == "audio/mp4" && !pieces[0].data.is_empty() && pieces[0].data.len() < pcm(20.0).len() / 5);
}

fn speaking_client() -> FakeTranscriber {
    let mut c = FakeTranscriber::new(&["we ship on friday", "the servo sticks", "stop taking notes"]);
    c.diarize_with = Some(Arc::new(|_mime: &str, _names: &[String]| {
        Ok(json!([seg("A", 0.0, 3.0, "we ship on friday"), seg("B", 3.0, 6.0, "the servo sticks"),
                  seg("A", 6.0, 8.0, "stop taking notes")]))
    }));
    c.summarize_with = Some(Arc::new(|_t: &str| {
        Ok(with(conversation(), json!({"quotes": [{"at": "00:03", "speaker": "Speaker 2", "text": "the servo sticks"}]}))
            .to_string())
    }));
    c
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_session_ends_with_the_speaker_pass_and_its_count() {
    let d = tmp();
    let client = Arc::new(speaking_client());
    let nc = NotesConfig { segment_secs: 0.5, ..Default::default() };
    let t = taker_with(d.path(), client.clone(), nc, FakeEars::new(), *WHEN);
    run_to_end(&t, 20).await;
    assert_eq!(client.diarized.lock().len(), 1);
    assert!(client.summaries.lock()[0].starts_with("[00:00] Speaker 1: we ship on friday"), "notes from the diarized text");
    let path = t.state().path.unwrap();
    let text = read(&path);
    assert!(text.contains("speakers: 2") && text.contains("diarized: yes") && text.contains("status: notes"));
    assert!(text.contains("[00:03] Speaker 2: the servo sticks"));
    assert!(text.contains("## Speakers") && text.contains("## Quotes") && text.contains("\"the servo sticks\" (Speaker 2, 00:03)"));
    assert!(t.speakers() == strs(&["Speaker 1", "Speaker 2"]) && t.diarized() == "yes");
    assert!(path.file_name().unwrap().to_string_lossy().ends_with("-shipping-the-robot-friday.md"), "named for its title at the end");
    assert!(t.spool_dir().is_none(), "the kept audio is gone once the notes are written");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_pass_can_be_turned_off_and_the_owner_named() {
    let env = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    let nc = configured(Some(&env(&[("EVE_NOTES_DIARIZE", "0")])));
    assert!(!nc.diarize);
    let d = tmp();
    let client = Arc::new(speaking_client());
    let t = taker_with(d.path(), client.clone(), nc, FakeEars::new(), *WHEN);
    t.start();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    t.stop("asked").await;
    assert!(client.diarized.lock().is_empty() && !read(&t.state().path.unwrap()).contains("speakers:"));
    let on = configured(Some(&env(&[("EVE_NOTES_OWNER_VOICE", "~/me.wav"), ("EVE_NOTES_OWNER_NAME", "  Ada  ")])));
    assert!(on.diarize && on.owner_voice == "~/me.wav" && on.owner_name == "Ada");
    assert_eq!(configured(Some(&env(&[]))).owner_name, "Owner");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_failed_pass_keeps_the_live_transcript() {
    let d = tmp();
    let mut client = speaking_client();
    client.diarize_with = Some(Arc::new(|_m: &str, _n: &[String]| anyhow::bail!("the pass failed")));
    let nc = NotesConfig { segment_secs: 0.5, ..Default::default() };
    let t = taker_with(d.path(), Arc::new(client), nc, FakeEars::new(), *WHEN);
    run_to_end(&t, 20).await;
    let text = read(&t.state().path.unwrap());
    assert!(text.contains("diarized: no") && !text.contains("speakers:"));
    assert!(text.contains("] we ship on friday") && text.contains("no speaker labels"));
}

#[test]
fn the_owner_clip_is_read_as_a_data_url_by_its_suffix() {
    let d = tmp();
    let clip = d.path().join("me.M4A");
    std::fs::write(&clip, b"abc").unwrap();
    let nc = NotesConfig { owner_voice: clip.to_string_lossy().into_owned(), owner_name: "Ada".into(), ..Default::default() };
    assert_eq!(owner_reference(&nc), Some(("Ada".into(), "data:audio/mp4;base64,YWJj".into())));
    let missing = NotesConfig { owner_voice: d.path().join("none.wav").to_string_lossy().into_owned(), ..Default::default() };
    assert_eq!(owner_reference(&missing), None);
    assert_eq!(owner_reference(&NotesConfig::default()), None);
    let _ = cfg(Path::new("/"));
}
