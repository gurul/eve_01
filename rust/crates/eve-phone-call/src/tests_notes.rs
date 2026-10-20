//! test_phone_notes.py: "start taking notes" said on an Eve Link call records the phone until the Action Button.
//! The loopback server, the Eve Link door (`/api/stick`, a token instead of initData), a real WebSocket client as
//! the phone, a chat that runs take_notes the way telegram.py does, the real notes session (eve-notes' RoomNotes)
//! and a fake transcriber (no audio leaves the machine, no model is called). Plus test_notes_detailed.py's texted
//! notes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use eve_notes::{AudioSource, Line, NotesConfig, RoomNotes, RoomNotesOptions, TranscribeClient};
use eve_records::RecallConfig;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use super::test_support::*;
use super::*;

fn summary() -> Value {
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

/// 100 ms at 24 kHz, well above SILENCE_RMS.
fn loud() -> Vec<u8> {
    (0..2400).flat_map(|i| if i % 2 == 0 { 6000i16 } else { -6000i16 }.to_le_bytes()).collect()
}

struct FakeTranscriber {
    texts: Vec<String>,
    calls: AtomicUsize,
    no_summary: bool,
    summarized: AtomicBool,
}

impl FakeTranscriber {
    fn new(texts: &[&str], no_summary: bool) -> Arc<Self> {
        Arc::new(Self {
            texts: texts.iter().map(|s| s.to_string()).collect(),
            calls: AtomicUsize::new(0),
            no_summary,
            summarized: AtomicBool::new(false),
        })
    }
}

impl TranscribeClient for FakeTranscriber {
    fn transcribe(&self, _wav: &[u8], _prompt: &str) -> anyhow::Result<String> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.texts[n.min(self.texts.len() - 1)].clone())
    }

    fn summarize(&self, _transcript: &str) -> anyhow::Result<String> {
        self.summarized.store(true, Ordering::SeqCst);
        if self.no_summary {
            anyhow::bail!("a shutdown must not wait for a summary");
        }
        Ok(summary().to_string())
    }
}

/// The chat: "start taking notes" runs take_notes with the call's reader, as telegram._robot_tool does, and the reply
/// is read out through the reader the turn began with.
struct NotesBrain {
    heard: Mutex<Vec<String>>,
    say: Mutex<Option<Reader>>,
    calls: OnceLock<Weak<PhoneCalls>>,
    results: Mutex<Vec<Option<Value>>>,
    me: Weak<NotesBrain>,
}

impl NotesBrain {
    fn new() -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            heard: Mutex::new(vec![]),
            say: Mutex::new(None),
            calls: OnceLock::new(),
            results: Mutex::new(vec![]),
            me: me.clone(),
        })
    }

    fn results(&self) -> Vec<Option<Value>> {
        self.results.lock().clone()
    }
}

impl Brain for NotesBrain {
    fn hear(&self, text: &str, _call: Option<&Reader>, _answer_context: Captured) {
        self.heard.lock().push(text.to_string());
        let say = self.say.lock().clone(); // fixed for the turn, as telegram.py's call_say
        let action = if text.contains("stop") { "stop" } else { "start" };
        let calls = self.calls.get().and_then(Weak::upgrade).expect("the endpoint");
        let me = self.me.clone();
        tokio::spawn(async move {
            let result = calls.notes_action(action, say.as_ref(), "asked from Telegram", None).await;
            let ok = result.as_ref().is_some_and(|r| r["ok"] == true);
            if let Some(brain) = me.upgrade() {
                brain.results.lock().push(result);
            }
            if let Some(say) = say {
                say.say(if ok { "Taking notes." } else { "I can't." });
            }
        });
    }

    fn listen(&self, say: Option<Reader>, owner: Option<&Reader>) -> bool {
        listen_slot(&self.say, say, owner)
    }

    fn chat_ready(&self) -> Option<bool> {
        Some(true)
    }
}

type Told = Arc<Mutex<Vec<(String, String)>>>;

struct H {
    calls: Arc<PhoneCalls>,
    base: String,
    voice: Arc<FakeVoice>,
    told: Told,
    recall: RecallConfig,
    _dir: tempfile::TempDir,
}

fn notes_of(h: &H) -> Arc<PhoneNotes> {
    h.calls.notes().expect("phone notes")
}

fn heard_lines(h: &H) -> usize {
    notes_of(h).taker().map(|t| t.lines().len()).unwrap_or(0)
}

async fn setup(brain: &Arc<NotesBrain>, client: Option<Arc<FakeTranscriber>>, writer: Option<eve_notes::NotesWriter>) -> H {
    let dir = tempfile::tempdir().unwrap();
    let voice = FakeVoice::new("start taking notes");
    let calls = Arc::new(PhoneCalls::new(check_owner(), Some(brain.clone()), Some(voice.clone())));
    let told: Told = Arc::new(Mutex::new(vec![]));
    let recall = RecallConfig::new(dir.path().join("debrief"), dir.path().join("notes"));
    let client = client.unwrap_or_else(|| FakeTranscriber::new(&["first we talk about the servo", "then the case gets reprinted"], false));
    let r = recall.clone();
    let make: MakeTaker = Arc::new(move |mic: Arc<eve_notes::PushedAudio>| {
        let mut opts = RoomNotesOptions { source: "eve-link".into(), ..Default::default() };
        if let Some(w) = &writer {
            opts.writer = w.clone();
        }
        let cfg = NotesConfig { segment_secs: 0.5, ..Default::default() };
        let client: Arc<dyn TranscribeClient> = client.clone();
        let ears: Arc<dyn AudioSource> = mic;
        Arc::new(RoomNotes::with_options(r.clone(), cfg, Some(client), Some(ears), opts)) as Arc<dyn NotesTaker>
    });
    let t = told.clone();
    let notify: NotesNotify = Arc::new(move |text, about| {
        t.lock().push((text, about));
        Box::pin(async { Ok(()) })
    });
    calls.set_notes(Some(PhoneNotes::new(make, Some(notify))));
    let _ = brain.calls.set(Arc::downgrade(&calls));
    let base = serve(calls.clone()).await;
    H { calls, base, voice, told, recall, _dir: dir }
}

async fn hello(ws: &Phone, can: &[&str]) {
    ws.send_json(json!({"token": LINK, "can": can})).await;
    ws.until(is_state("connected")).await;
}

/// The owner presses and says "start taking notes"; eve answers and the phone is told notes are on.
async fn say_start(ws: &Phone) -> Vec<Msg> {
    ws.press(1.0).await;
    ws.until(is_state("notes")).await
}

async fn talk_for(ws: &Phone, secs: f64) {
    for _ in 0..(secs * 10.0) as usize {
        ws.send_bytes(loud()).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn files(recall: &RecallConfig) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
    }
    let mut out = vec![];
    walk(&recall.transcripts_dir().join("meetings"), &mut out);
    out.sort();
    out
}

async fn eventually(what: &str, secs: f64, pred: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(secs);
    while !pred() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_start_on_the_phone_records_the_phone_and_the_action_button_writes_them_up() {
    let brain = NotesBrain::new();
    let h = setup(&brain, None, None).await;
    let ws = Phone::connect(&format!("{}/api/stick", h.base)).await;
    hello(&ws, &["notes"]).await;
    let notes_on = say_start(&ws).await.last().unwrap().json().unwrap().clone();
    assert!(notes_on["started"].as_f64().unwrap() > 0.0 && notes_on["words"] == 0);
    assert!(!h.calls.holding_chat() && brain.say.lock().is_none()); // texts are not read into it
    // eve's spoken confirmation, then back to notes (not "listening": the phone keeps recording)
    let after = ws.until(is_state("notes")).await;
    assert!(after.iter().any(|m| m.state() == Some("speaking")));
    talk_for(&ws, 1.2).await; // audio with no talk: the notes', not a press
    eventually("two lines heard", 5.0, || heard_lines(&h) >= 2).await;
    ws.send_json(json!({"type": "notes_stop"})).await;
    let got = ws.until(is_state("listening")).await;
    ws.until(|m| matches!(m, Msg::Bin(_))).await; // "Notes saved." spoken on the call
    eventually("the notes to be texted", 5.0, || !h.told.lock().is_empty()).await;
    ws.close().await;

    assert!(got.contains(&Msg::Json(json!({"type": "state", "state": "thinking", "note": "Writing up your notes."}))));
    assert_eq!(
        got.last().unwrap(),
        &Msg::Json(json!({"type": "state", "state": "listening", "note": "Notes saved: The servo order and the case"}))
    );
    assert_eq!(*brain.heard.lock(), vec!["start taking notes"]); // one turn: the notes were never presses
    let first = brain.results()[0].clone().unwrap();
    assert!(first["ok"] == true && first["on"] == "your phone");
    assert!(h.voice.spoken().iter().any(|s| s == "Notes saved."));
    let paths = files(&h.recall);
    assert_eq!(paths.len(), 1);
    let text = std::fs::read_to_string(&paths[0]).unwrap();
    assert!(text.contains("source: eve-link") && text.contains("first we talk about the servo"));
    assert!(text.contains("stopped_by: you pressed stop on the phone"));
    let (message, about) = h.told.lock()[0].clone();
    let name = paths[0].file_name().unwrap().to_string_lossy().to_string();
    assert!(message.contains("The servo order and the case") && message.contains("place the order") && message.contains(&name));
    assert!(about.contains(&name) && !about.contains("talk about")); // the history never gets the words heard
}

#[tokio::test(flavor = "multi_thread")]
async fn test_an_old_app_is_refused_and_the_mini_app_leaves_it_to_the_room() {
    let brain = NotesBrain::new();
    let h = setup(&brain, None, None).await;
    let ws = Phone::connect(&format!("{}/api/stick", h.base)).await;
    hello(&ws, &[]).await; // an Eve Link build from before notes
    ws.press(1.0).await;
    eventually("a result", 5.0, || !brain.results().is_empty()).await;
    ws.close().await;
    let last = brain.results().last().cloned().unwrap().unwrap();
    assert!(last["ok"] == false && last["reason"].as_str().unwrap().contains("update"));
    let ws = Phone::connect(&format!("{}/api/call", h.base)).await;
    ws.hello_owner().await;
    ws.until(is_state("connected")).await;
    ws.press(1.0).await;
    eventually("a second result", 5.0, || brain.results().len() == 2).await;
    ws.close().await;
    assert!(brain.results().last().unwrap().is_none()); // the room's note-taker answers
    assert!(!notes_of(&h).active());
    // and nothing said on a call at all (a text, the desk) is never the phone's start
    assert!(h.calls.notes_action("start", None, "asked from Telegram", None).await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_dropped_call_keeps_what_was_heard_and_the_phone_calling_back_carries_on() {
    let brain = NotesBrain::new();
    let h = setup(&brain, None, None).await;
    let ws = Phone::connect(&format!("{}/api/stick", h.base)).await;
    hello(&ws, &["notes"]).await;
    let first = say_start(&ws).await.last().unwrap().json().unwrap().clone();
    talk_for(&ws, 0.6).await;
    eventually("a line heard", 5.0, || heard_lines(&h) >= 1).await;
    ws.close().await;
    eventually("the call to end", 5.0, || h.calls.active().is_none()).await;
    let notes = notes_of(&h);
    assert!(notes.recording() && notes.waiting_for_phone()); // waiting for the phone
    let ws = Phone::connect(&format!("{}/api/stick", h.base)).await;
    ws.send_json(json!({"token": LINK, "can": ["notes"]})).await;
    let back = ws.until(is_state("notes")).await.last().unwrap().json().unwrap().clone();
    assert!(back["started"] == first["started"] && back["words"].as_u64().unwrap() > 0);
    assert!(!notes.waiting_for_phone());
    talk_for(&ws, 0.6).await;
    eventually("two lines heard", 5.0, || heard_lines(&h) >= 2).await;
    ws.send_json(json!({"type": "talk"})).await; // a build without notes_stop: talk stops
    ws.until(is_state("listening")).await;
    eventually("the notes to be texted", 5.0, || !h.told.lock().is_empty()).await;
    ws.close().await;
    let paths = files(&h.recall);
    assert_eq!(paths.len(), 1);
    let text = std::fs::read_to_string(&paths[0]).unwrap();
    assert!(text.contains("first we talk about the servo") && text.contains("then the case gets reprinted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_phone_that_never_calls_back_gets_its_notes_after_the_grace() {
    let brain = NotesBrain::new();
    let h = setup(&brain, None, None).await;
    notes_of(&h).set_grace_secs(0.3);
    let ws = Phone::connect(&format!("{}/api/stick", h.base)).await;
    hello(&ws, &["notes"]).await;
    say_start(&ws).await;
    talk_for(&ws, 0.6).await;
    eventually("a line heard", 5.0, || heard_lines(&h) >= 1).await;
    ws.close().await;
    eventually("the notes to be texted", 5.0, || !h.told.lock().is_empty()).await;
    eventually("the session to end", 5.0, || !notes_of(&h).active()).await;
    let paths = files(&h.recall);
    assert_eq!(paths.len(), 1);
    assert!(std::fs::read_to_string(&paths[0]).unwrap().contains("stopped_by: the phone dropped"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_the_end_button_stops_and_writes_them_up_at_once() {
    let brain = NotesBrain::new();
    let h = setup(&brain, None, None).await;
    let ws = Phone::connect(&format!("{}/api/stick", h.base)).await;
    hello(&ws, &["notes"]).await;
    say_start(&ws).await;
    talk_for(&ws, 0.6).await;
    eventually("a line heard", 5.0, || heard_lines(&h) >= 1).await;
    ws.send_json(json!({"type": "end"})).await;
    assert_eq!(ws.until(is_kind("ended")).await.last().unwrap().json().unwrap()["reason"], HUNG_UP);
    eventually("the notes to be texted", 3.0, || !h.told.lock().is_empty()).await;
    ws.close().await;
    assert!(std::fs::read_to_string(&files(&h.recall)[0]).unwrap().contains("stopped_by: you ended the call"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_notes_outlast_the_calls_hour_and_the_command_stops_them() {
    let brain = NotesBrain::new();
    let h = setup(&brain, None, None).await;
    let ws = Phone::connect(&format!("{}/api/stick", h.base)).await;
    hello(&ws, &["notes"]).await;
    say_start(&ws).await;
    h.calls.limits.set_max_call_secs(0.3); // the call's "hour", from here
    talk_for(&ws, 1.0).await; // past the call's cap: still recording
    let notes = notes_of(&h);
    assert!(h.calls.active().is_some() && notes.recording());
    assert!(!h.calls.holding_chat()); // the desk's button is free meanwhile
    let status = h.calls.notes_action("status", None, "asked from Telegram", None).await.unwrap();
    assert!(status["on"] == "your phone" && status["attached"] == true && status["active"] == true);
    assert!(status.get("last").is_none());
    let out = h.calls.notes_action("stop", None, "asked", None).await.unwrap(); // `eve-bridge notes stop`
    assert!(out["ok"] == true && out["path"].is_string() && out["texted"] == true);
    ws.until(is_state("listening")).await;
    ws.close().await;
    // nothing on: the room's taker answers
    assert!(h.calls.notes_action("stop", None, "asked from Telegram", None).await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_nothing_said_is_said_and_texted_plainly() {
    let brain = NotesBrain::new();
    let h = setup(&brain, None, None).await;
    let ws = Phone::connect(&format!("{}/api/stick", h.base)).await;
    hello(&ws, &["notes"]).await;
    say_start(&ws).await;
    ws.send_json(json!({"type": "notes_stop"})).await;
    let got = ws.until(is_state("listening")).await;
    eventually("the notes to be texted", 5.0, || !h.told.lock().is_empty()).await;
    ws.close().await;
    assert_eq!(got.last().unwrap().json().unwrap()["note"], "Nothing was said, so there are no notes.");
    assert!(h.told.lock()[0].0.contains("nothing was said") && files(&h.recall).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_quiet_call_during_notes_closes_without_an_end_so_the_phone_calls_back() {
    // Review, 2026-10-04: the 30 s quiet timeout used to send `ended`, which the phone treats as an end eve chose
    // (no call back). During notes it now just closes the socket: the phone sees a drop, and eve waits.
    let brain = NotesBrain::new();
    let h = setup(&brain, None, None).await;
    let ws = Phone::connect(&format!("{}/api/stick", h.base)).await;
    hello(&ws, &["notes"]).await;
    say_start(&ws).await;
    ws.until(is_state("notes")).await; // after the spoken confirmation
    talk_for(&ws, 0.6).await;
    eventually("a line heard", 5.0, || heard_lines(&h) >= 1).await;
    h.calls.limits.set_quiet_secs(0.3); // the phone stalls from here
    ws.send_json(json!({"type": "ping"})).await; // (the read now waiting began before)
    let mut rest = vec![];
    while let Some(m) = ws.recv_within(5.0).await {
        rest.push(m);
    }
    eventually("the call to end", 5.0, || h.calls.active().is_none()).await;
    assert!(!rest.iter().any(|m| m.is("ended")));
    let notes = notes_of(&h);
    assert!(notes.recording() && notes.waiting_for_phone()); // waiting for the phone, not written up
    let out = h.calls.notes_action("stop", None, "asked", None).await.unwrap();
    assert!(out["ok"] == true && out["path"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_ping_during_notes_gets_a_pong_and_an_ordinary_call_stays_as_it_was() {
    let brain = NotesBrain::new();
    let h = setup(&brain, None, None).await;
    let ws = Phone::connect(&format!("{}/api/stick", h.base)).await;
    hello(&ws, &["notes"]).await;
    ws.send_json(json!({"type": "ping"})).await;
    if let Some(before) = ws.recv_within(0.3).await {
        panic!("an ordinary call answered a ping: {before:?}");
    }
    say_start(&ws).await;
    ws.until(is_state("notes")).await;
    ws.send_json(json!({"type": "ping"})).await;
    ws.until(is_kind("pong")).await;
    ws.send_json(json!({"type": "notes_stop"})).await;
    ws.until(is_state("listening")).await;
    ws.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_notes_not_written_are_never_called_saved_and_the_text_carries_the_words() {
    let brain = NotesBrain::new();
    let full_disk: eve_notes::NotesWriter = Arc::new(|_, _, _, _, _| None);
    let h = setup(&brain, None, Some(full_disk)).await;
    let ws = Phone::connect(&format!("{}/api/stick", h.base)).await;
    hello(&ws, &["notes"]).await;
    say_start(&ws).await;
    talk_for(&ws, 0.6).await;
    eventually("a line heard", 5.0, || heard_lines(&h) >= 1).await;
    ws.send_json(json!({"type": "notes_stop"})).await;
    let got = ws.until(is_state("listening")).await;
    eventually("the notes to be texted", 5.0, || !h.told.lock().is_empty()).await;
    ws.close().await;
    let note = got.last().unwrap().json().unwrap()["note"].as_str().unwrap().to_lowercase();
    assert!(!note.contains("saved:") && note.contains("texted"));
    assert!(!h.voice.spoken().iter().any(|s| s == "Notes saved."));
    let (message, about) = h.told.lock()[0].clone();
    assert!(message.contains("only copy") && message.contains("first we talk about the servo") && !message.contains("is saved"));
    assert!(!about.contains("talk about")); // the history still never gets them
}

#[tokio::test(flavor = "multi_thread")]
async fn test_the_daemon_stopping_writes_the_phones_notes_at_once() {
    // Notes live in memory until written: a daemon stopping mid-notes writes them now, transcript only
    // (Daemon._close_notes: `phone.stop("the daemon stopped", quick=True)`).
    let brain = NotesBrain::new();
    let client = FakeTranscriber::new(&["first we talk about the servo"], true);
    let h = setup(&brain, Some(client.clone()), None).await;
    let ws = Phone::connect(&format!("{}/api/stick", h.base)).await;
    hello(&ws, &["notes"]).await;
    say_start(&ws).await;
    talk_for(&ws, 0.6).await;
    eventually("a line heard", 5.0, || heard_lines(&h) >= 1).await;
    let out = notes_of(&h).stop("the daemon stopped", true).await;
    ws.close().await;
    assert!(out["ok"] == true && !notes_of(&h).active());
    assert!(!client.summarized.load(Ordering::SeqCst), "a shutdown must not wait for a summary");
    let paths = files(&h.recall);
    assert_eq!(paths.len(), 1);
    let text = std::fs::read_to_string(&paths[0]).unwrap();
    assert!(text.contains("stopped_by: the daemon stopped") && text.contains("first we talk about the servo"));
    assert!(!h.told.lock().is_empty()); // and texted, as any stop is
}

#[tokio::test]
async fn test_notes_drop_a_press_they_cut_off_on_both_sides() {
    // A press still open when notes start (said while eve was answering) is nobody's question now: the phone drops
    // its turn (Relay.notesOn) and the call forgets it too, so the first press after the notes is a real one.
    let brain = NotesBrain::new();
    let call = Call::new(MemorySocket::fixed(true), brain, FakeVoice::new("x"));
    call.set_talking(true);
    call.set_press(vec![1, 2]);
    let notes = PhoneNotes::new(Arc::new(|_mic| unreachable!("never started")), None);
    notes.attach(&call, true).await;
    assert!(!call.talking() && call.press().is_empty() && call.notes().is_some_and(|n| Arc::ptr_eq(&n, &notes)));
    // positive control: with notes off a talk begins a press, which a stuck `talking` used to swallow
    call.set_notes(None);
    call.begin_press().await;
    assert!(call.talking());
    call.stop_tasks().await;
}

struct StatusOnly;

impl NotesTaker for StatusOnly {
    fn start(&self) -> Value {
        json!({"ok": false})
    }
    fn active(&self) -> bool {
        true
    }
    fn stopped_by(&self) -> String {
        String::new()
    }
    fn words(&self) -> usize {
        9
    }
    fn path(&self) -> Option<PathBuf> {
        None
    }
    fn status(&self) -> Value {
        json!({"active": true, "words": 9, "last": "the secret plan"})
    }
    fn request_stop(&self, _reason: &str) {}
    fn set_quick(&self, _quick: bool) {}
    fn minutes(&self) -> f64 {
        0.0
    }
    fn task(&self) -> Option<futures::future::Shared<futures::future::BoxFuture<'static, ()>>> {
        None
    }
    fn lines(&self) -> Vec<(f64, String)> {
        vec![]
    }
    fn summary(&self) -> Map<String, Value> {
        Map::new()
    }
    fn transcript(&self) -> String {
        String::new()
    }
    fn speakers(&self) -> Vec<String> {
        vec![]
    }
    fn diarized(&self) -> String {
        "no".into()
    }
    fn close(&self) {}
}

#[test]
fn test_the_phones_status_never_carries_the_words_heard() {
    let notes = PhoneNotes::new(Arc::new(|_mic| unreachable!("never started")), None);
    notes.set_taker(Some(Arc::new(StatusOnly)));
    let out = notes.status();
    assert!(!out.contains_key("last") && out["words"] == 9);
    assert!(!py::dumps(&Value::Object(out)).contains("the secret plan"));
}

#[tokio::test]
async fn a_start_while_on_says_already_and_stop_with_nothing_on_says_so() {
    let notes = PhoneNotes::new(Arc::new(|_mic| unreachable!("never started")), None);
    assert_eq!(notes.stop("asked", false).await, json!({"ok": false, "error": "eve is not taking notes on your phone"}));
    let call = Call::new(MemorySocket::fixed(true), NotesBrain::new(), FakeVoice::new("x"));
    let refused = notes.start(&call, None).await;
    assert_eq!(refused["ok"], false);
    assert!(refused["reason"].as_str().unwrap().starts_with("this Eve Link app cannot take notes yet"));
    assert!(notes.progress() == Map::from_iter([("started".to_string(), json!(0.0)), ("words".to_string(), json!(0))]));
}

// ---- the text to the owner (test_notes_detailed.py) ----------------------------------------------------------------

fn lines() -> Vec<Line> {
    vec![
        Line::new(3.0, "Good morning. I think we should ship on Friday the twelfth.", "Speaker 1"),
        Line::new(9.0, "Friday works, but the right servo still sticks under load.", "Speaker 2"),
        Line::new(15.0, "Then order two spare servos today for forty dollars.", "Speaker 1"),
    ]
}

fn conversation() -> Map<String, Value> {
    match json!({
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
    }) {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

#[test]
fn test_the_texted_notes_carry_every_section_in_messages_under_the_limit() {
    let mut raw = conversation();
    let topics: Vec<Value> = (0..12)
        .map(|n| json!({"heading": format!("Topic {n}"),
                        "points": (0..6).map(|k| format!("point {n}.{k} {}", "detail ".repeat(30))).collect::<Vec<_>>()}))
        .collect();
    raw.insert("topics".into(), Value::Array(topics));
    let big = eve_notes::clean_summary(&Value::Object(raw), &lines());
    let speakers = vec!["Speaker 1".to_string(), "Speaker 2".to_string()];
    let pieces =
        notes_messages(&big, 40, 63.0, Some(Path::new("0905-shipping.md")), "", Some(&speakers), NOTES_PIECE_CHARS);
    assert!(pieces.len() > 1 && pieces.iter().all(|p| p.chars().count() <= NOTES_PIECE_CHARS));
    assert!(pieces[0].starts_with("Notes from your phone: Shipping the robot Friday (63 min, 2 speakers)."));
    let joined = pieces.join("\n");
    for needle in ["Speakers:", "Topic 11:", "point 11.5", "Decisions:", "Actions:", "Open questions:", "Quotes:", "Follow-ups:"] {
        assert!(joined.contains(needle), "{needle}");
    }
    assert!(pieces.last().unwrap().ends_with("The full transcript is in 0905-shipping.md, in the meeting notes."));
}

#[test]
fn test_the_texted_notes_cap_out_at_the_file_and_carry_the_words_when_there_is_no_file() {
    let mut raw = conversation();
    let topics: Vec<Value> = (0..30).map(|n| json!({"heading": format!("T{n}"), "points": ["x ".repeat(1500)]})).collect();
    raw.insert("topics".into(), Value::Array(topics));
    let huge = eve_notes::clean_summary(&Value::Object(raw), &lines());
    let pieces = notes_messages(&huge, 9, 90.0, Some(Path::new("f.md")), "", None, NOTES_PIECE_CHARS);
    assert!(pieces.len() <= NOTES_MAX_PIECES + 1 && pieces.concat().contains("rest of the notes are in the file"));
    assert!(pieces.iter().all(|p| p.chars().count() <= NOTES_PIECE_CHARS));
    let alone = notes_messages(&Map::new(), 2, 3.0, None, "[00:00] first words\n[00:12] second words", None, NOTES_PIECE_CHARS);
    let joined = alone.join("\n");
    assert!(joined.contains("only copy") && joined.contains("first words") && joined.contains("second words"));
    assert!(!alone[0].contains("speakers"), "no count when there was no speaker pass");
}

#[test]
fn the_texted_notes_are_the_bytes_python_writes() {
    // python3 -c 'from eve_bridge.phone_call import notes_messages; ...' on the same inputs
    assert_eq!(
        notes_messages(&Map::new(), 0, 1.0, None, "", None, NOTES_PIECE_CHARS),
        vec!["I took notes on your phone, but nothing was said, so there are no notes."]
    );
    let alone = notes_messages(&Map::new(), 2, 0.4, Some(Path::new("/x/0905.md")), "", None, NOTES_PIECE_CHARS);
    assert_eq!(
        alone,
        vec!["Notes from your phone (1 min).\n\nThe write-up failed; the transcript (2 lines) is saved.\n\n\
              The full transcript is in 0905.md, in the meeting notes."]
    );
    let memo = match json!({"kind": "memo", "title": "Ideas.", "gist": "Out loud.", "topics": [
        {"heading": "Case", "points": ["PETG"]}, {"heading": "Lid", "points": []}], "ideas": ["magnets"],
        "todos": ["buy PETG"]}) {
        Value::Object(m) => m,
        _ => unreachable!(),
    };
    let one = speakers_one();
    let text = notes_message(&memo, 3, 2.5, Some(Path::new("m.md")), "", Some(&one));
    assert_eq!(text, MEMO_TEXT);
    // a paragraph longer than a message is cut between its lines, and a line longer than one at the limit
    let long = vec!["a\nbb\nccc".to_string(), "d".repeat(12)];
    assert_eq!(pack(&long, 5), vec!["a\nbb", "ccc", "ddddd", "ddddd", "dd"]);
}

fn speakers_one() -> Vec<String> {
    vec!["Speaker 1".to_string()]
}

// notes_message(memo, 3, 2.5, Path("m.md"), "", ["Speaker 1"]) in Python (round(2.5) is 2): a memo lists no speakers
const MEMO_TEXT: &str = "Notes from your phone: Ideas (2 min, 1 speaker).\n\nOut loud.\n\nCase:\n\u{2022} PETG\n\nLid\n\n\
Ideas:\n\u{2022} magnets\n\nTo-dos:\n\u{2022} buy PETG\n\nThe full transcript is in m.md, in the meeting notes.";
