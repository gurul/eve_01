//! test_phone_call_turns.py: the push-to-talk wire supplies turn boundaries; a fake realtime server exercises the
//! live ears' actual event handling (the seam is [`RealtimeConnect`], where Python patched `openai.AsyncOpenAI`).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::test_support::*;
use super::*;

/// The realtime server: what was configured, appended, committed and cleared, and the events it sends back.
pub struct RealtimeServer {
    me: std::sync::Weak<RealtimeServer>,
    tx: mpsc::UnboundedSender<Value>,
    rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Value>>,
    pub texts: Vec<String>,
    sessions: Mutex<Vec<Value>>,
    audio: Mutex<Vec<u8>>,
    commits: Mutex<Vec<Vec<u8>>>,
    pub clears: AtomicUsize,
    pub auto_ack: AtomicBool,
    pub auto_complete: AtomicBool,
    pub clear_fails: AtomicBool,
    pub closed: AtomicBool,
}

impl RealtimeServer {
    pub fn new(texts: &[&str]) -> Arc<Self> {
        let texts = if texts.is_empty() { vec!["remind me to call the dentist", "hello, cancel that"] } else { texts.to_vec() };
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            tx,
            rx: tokio::sync::Mutex::new(rx),
            texts: texts.into_iter().map(String::from).collect(),
            sessions: Mutex::new(vec![]),
            audio: Mutex::new(vec![]),
            commits: Mutex::new(vec![]),
            clears: AtomicUsize::new(0),
            auto_ack: AtomicBool::new(true),
            auto_complete: AtomicBool::new(true),
            clear_fails: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        })
    }

    pub fn emit(&self, event: Value) {
        let _ = self.tx.send(event);
    }

    pub fn complete(&self, item: &str, transcript: &str) {
        self.emit(json!({"type": "conversation.item.input_audio_transcription.completed", "item_id": item,
                         "transcript": transcript}));
    }

    pub fn sessions(&self) -> Vec<Value> {
        self.sessions.lock().clone()
    }

    pub fn commits(&self) -> Vec<Vec<u8>> {
        self.commits.lock().clone()
    }

    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl RealtimeConnect for RealtimeServer {
    async fn connect(&self, _api_key: &str) -> anyhow::Result<Arc<dyn RealtimeConn>> {
        Ok(Arc::new(Conn(self.me.upgrade().expect("the server"))))
    }
}

pub fn server(texts: &[&str]) -> Arc<RealtimeServer> {
    RealtimeServer::new(texts)
}

struct Conn(Arc<RealtimeServer>);

#[async_trait]
impl RealtimeConn for Conn {
    async fn send(&self, event: Value) -> anyhow::Result<()> {
        let s = &self.0;
        match event["type"].as_str().unwrap_or("") {
            "session.update" => {
                s.sessions.lock().push(event["session"].clone());
                if s.auto_ack.load(Ordering::SeqCst) {
                    s.emit(json!({"type": "session.updated", "session": event["session"]}));
                }
            }
            "input_audio_buffer.clear" => {
                if s.clear_fails.load(Ordering::SeqCst) {
                    return Err(EarsError::Connection("clear failed".into()).into());
                }
                s.clears.fetch_add(1, Ordering::SeqCst);
                s.audio.lock().clear();
            }
            "input_audio_buffer.append" => {
                let audio = base64::engine::general_purpose::STANDARD.decode(event["audio"].as_str().unwrap())?;
                s.audio.lock().extend(audio);
            }
            "input_audio_buffer.commit" => {
                let n = {
                    let mut commits = s.commits.lock();
                    commits.push(std::mem::take(&mut *s.audio.lock()));
                    commits.len()
                };
                let item = format!("press-{n}");
                s.emit(json!({"type": "input_audio_buffer.committed", "item_id": item}));
                if s.auto_complete.load(Ordering::SeqCst) {
                    s.complete(&item, &s.texts[n - 1]);
                }
            }
            other => anyhow::bail!("unexpected event {other}"),
        }
        Ok(())
    }

    async fn recv(&self) -> Option<anyhow::Result<Value>> {
        self.0.rx.lock().await.recv().await.map(Ok)
    }

    fn close(&self) {
        self.0.closed.store(true, Ordering::SeqCst);
    }
}

fn ears_on(server: &Arc<RealtimeServer>) -> Arc<LiveEars> {
    let spend = tempfile::tempdir().unwrap().keep();
    let mut ears = LiveEars::with_connector("fake", "gpt-4o-transcribe", DEFAULT_LANGUAGE, server.clone());
    ears.ledger = Ledger::at(spend);
    Arc::new(ears)
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(20)).await;
}

fn is_connection(e: &anyhow::Error) -> bool {
    matches!(e.downcast_ref::<EarsError>(), Some(EarsError::Connection(_)))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_open_waits_for_the_manual_turn_configuration_ack() {
    let server = server(&[]);
    server.auto_ack.store(false, Ordering::SeqCst);
    let ears = ears_on(&server);
    let e = ears.clone();
    let opening = tokio::spawn(async move { e.open().await });
    settle().await;
    assert!(!opening.is_finished());
    assert_eq!(server.sessions()[0]["audio"]["input"]["turn_detection"], Value::Null);
    server.emit(json!({"type": "session.updated"}));
    opening.await.unwrap().unwrap();
    ears.close().await;
    assert!(server.closed() && ears.reader_done());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_session_configuration_failure_closes_the_unready_session() {
    for failure in ["rejected", "timeout"] {
        let server = server(&[]);
        server.auto_ack.store(false, Ordering::SeqCst);
        if failure == "rejected" {
            server.emit(json!({"type": "error", "error": {"code": "invalid_session_update"}}));
        }
        let mut ears = LiveEars::with_connector("fake", "gpt-4o-transcribe", DEFAULT_LANGUAGE, server.clone());
        ears.open_timeout = 0.02;
        let ears: Arc<LiveEars> = Arc::new(ears);
        let call = Call::new(MemorySocket::fixed(true), FakeBrain::default_reply(), FakeVoice::live(Some(ears.clone())));
        call.open_ears().await;
        assert!(call.ears().is_none() && server.closed() && ears.reader_done(), "{failure}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_unsolicited_committed_turn_breaks_the_session_instead_of_becoming_owner_text() {
    let server = server(&[]);
    let ears = ears_on(&server);
    ears.open().await.unwrap();
    ears.begin().await.unwrap();
    server.emit(json!({"type": "input_audio_buffer.committed", "item_id": "unsolicited"}));
    server.complete("unsolicited", "random startup words");
    until("the session to break", || ears.broken()).await;
    settle().await;
    assert!(ears.broken() && ears.order().is_empty() && ears.done().is_empty());
    let err = ears.finish(0.2).await.unwrap_err();
    assert!(is_connection(&err));
    ears.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_one_manual_commit_preserves_the_complete_press_and_ignores_unrelated_text() {
    let server = server(&["hello, remind me to call the dentist"]);
    let ears = ears_on(&server);
    ears.open().await.unwrap();
    ears.begin().await.unwrap();
    // A sentence with a quiet pause: there is no extra turn or discarded initial word.
    let chunks = [b"\x12\x00".repeat(2400), vec![0u8; 24000], b"\x34\x00".repeat(2400)];
    server.complete("old-item", "unrelated old words");
    for chunk in &chunks {
        ears.feed(chunk).await.unwrap();
    }
    assert_eq!(ears.finish(1.0).await.unwrap(), "hello, remind me to call the dentist");
    assert_eq!(server.commits(), vec![chunks.concat()]);
    assert!(!ears.done().contains_key("old-item"));
    ears.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_next_press_waits_for_previous_item_and_does_not_clear_its_audio() {
    let server = server(&[]);
    server.auto_complete.store(false, Ordering::SeqCst);
    let ears = ears_on(&server);
    ears.open().await.unwrap();
    ears.begin().await.unwrap();
    let first = b"\x12\x00".repeat(4800);
    ears.feed(&first).await.unwrap();
    let e = ears.clone();
    let finishing = tokio::spawn(async move { e.finish(1.0).await });
    settle().await;
    let e = ears.clone();
    let beginning = tokio::spawn(async move { e.begin().await });
    settle().await;
    assert!(!beginning.is_finished() && server.clears.load(Ordering::SeqCst) == 1);
    server.complete("press-1", "first words");
    assert_eq!(finishing.await.unwrap().unwrap(), "first words");
    beginning.await.unwrap().unwrap();
    let second = b"\x34\x00".repeat(4800);
    ears.feed(&second).await.unwrap();
    // A late duplicate completion from the first item cannot prefix the second.
    server.complete("press-1", "old words");
    let e = ears.clone();
    let finishing = tokio::spawn(async move { e.finish(1.0).await });
    settle().await;
    server.complete("press-2", "hello, second words");
    assert_eq!(finishing.await.unwrap().unwrap(), "hello, second words");
    assert_eq!(server.commits(), vec![first, second]);
    ears.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_failure_unlocks_and_marks_the_session_unusable() {
    for failure in ["timeout", "failed", "commit_error"] {
        let server = server(&[]);
        server.auto_complete.store(false, Ordering::SeqCst);
        let ears = ears_on(&server);
        ears.open().await.unwrap();
        ears.begin().await.unwrap();
        ears.feed(&b"\x12\x00".repeat(4800)).await.unwrap();
        let e = ears.clone();
        let finishing = tokio::spawn(async move { e.finish(0.05).await });
        settle().await;
        if failure == "failed" {
            server.emit(json!({"type": "conversation.item.input_audio_transcription.failed", "item_id": "press-1"}));
        } else if failure == "commit_error" {
            server.emit(json!({"type": "error", "error": {"code": "input_audio_buffer_commit_empty"}}));
        }
        let err = finishing.await.unwrap().unwrap_err();
        assert!(err.downcast_ref::<EarsError>().is_some(), "{failure}: {err}");
        assert!(ears.broken() && !ears.press_locked(), "{failure}");
        let again = tokio::time::timeout(Duration::from_millis(200), ears.begin()).await.unwrap();
        assert!(is_connection(&again.unwrap_err()), "{failure}");
        ears.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_short_tap_discard_unlocks_without_creating_a_turn() {
    let server = server(&[]);
    let ears = ears_on(&server);
    ears.open().await.unwrap();
    ears.begin().await.unwrap();
    ears.feed(&[0u8; 2400]).await.unwrap();
    ears.discard().await.unwrap();
    tokio::time::timeout(Duration::from_millis(200), ears.begin()).await.unwrap().unwrap();
    ears.feed(&b"\x12\x00".repeat(4800)).await.unwrap();
    assert_eq!(ears.finish(1.0).await.unwrap(), server.texts[0]);
    assert_eq!(server.commits(), vec![b"\x12\x00".repeat(4800)]);
    ears.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_failed_discard_cannot_reuse_the_short_taps_audio() {
    let server = server(&[]);
    let ears = ears_on(&server);
    ears.open().await.unwrap();
    ears.begin().await.unwrap();
    ears.feed(&[0u8; 2400]).await.unwrap();
    server.clear_fails.store(true, Ordering::SeqCst);
    assert!(is_connection(&ears.discard().await.unwrap_err()));
    assert!(ears.broken() && !ears.press_locked());
    assert!(is_connection(&ears.begin().await.unwrap_err()));
    ears.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cancelled_open_closes_the_session_that_the_call_never_owned() {
    let server = server(&[]);
    server.auto_ack.store(false, Ordering::SeqCst);
    let ears = ears_on(&server);
    let call = Call::new(MemorySocket::fixed(true), FakeBrain::default_reply(), FakeVoice::live(Some(ears.clone())));
    let c = call.clone();
    let opening = tokio::spawn(async move { c.open_ears().await });
    settle().await;
    assert!(ears.reader_started() && call.ears().is_none());
    opening.abort();
    assert!(opening.await.unwrap_err().is_cancelled());
    settle().await;
    assert!(server.closed() && ears.reader_done());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cancelled_finish_unlocks_and_never_reuses_a_delayed_transcript() {
    let server = server(&[]);
    server.auto_complete.store(false, Ordering::SeqCst);
    let ears = ears_on(&server);
    ears.open().await.unwrap();
    ears.begin().await.unwrap();
    ears.feed(&[0u8; 4800]).await.unwrap();
    let e = ears.clone();
    let finishing = tokio::spawn(async move { e.finish(1.0).await });
    settle().await;
    finishing.abort();
    assert!(finishing.await.unwrap_err().is_cancelled());
    assert!(ears.broken() && !ears.press_locked());
    assert!(is_connection(&ears.begin().await.unwrap_err()));
    ears.close().await;
}

fn live_calls(brain: Arc<FakeBrain>, voice: Arc<FakeVoice>) -> Arc<PhoneCalls> {
    Arc::new(PhoneCalls::new(check_owner(), Some(brain), Some(voice)))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_rapid_websocket_presses_keep_all_audio_and_initial_words() {
    let server = server(&[]);
    server.auto_complete.store(false, Ordering::SeqCst);
    let ears = ears_on(&server);
    let brain = FakeBrain::new("");
    let voice = FakeVoice::live(Some(ears.clone()));
    let base = serve(live_calls(brain.clone(), voice.clone())).await;
    let ws = Phone::connect(&format!("{base}/api/call")).await;
    ws.hello_owner().await;
    ws.until(is_state("connected")).await;
    ears.configured().await;
    settle().await; // the call holds the opened session
    ws.press(1.0).await;
    ws.until(is_state("thinking")).await;
    // Send a complete second press before the first transcript finishes.
    ws.press(1.0).await;
    ws.until(is_kind("flush")).await;
    server.complete("press-1", &server.texts[0]);
    assert_eq!(ws.until(is_kind("heard")).await.last().unwrap().json().unwrap()["text"], server.texts[0].as_str());
    ws.until(is_state("thinking")).await;
    server.complete("press-2", &server.texts[1]);
    assert_eq!(ws.until(is_kind("heard")).await.last().unwrap().json().unwrap()["text"], server.texts[1].as_str());
    ws.close().await;
    assert_eq!(server.commits(), vec![vec![0u8; 48000], vec![0u8; 48000]]);
    assert_eq!(brain.heard(), server.texts);
    assert!(voice.transcribed().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_failed_live_press_and_waiting_next_press_both_recover_with_complete_uploads() {
    let server = server(&[]);
    server.auto_complete.store(false, Ordering::SeqCst);
    let ears = ears_on(&server);
    let brain = FakeBrain::new("");
    let voice = FakeVoice::live(Some(ears.clone()));
    let c = live_calls(brain.clone(), voice.clone());
    let base = serve(c.clone()).await;
    let ws = Phone::connect(&format!("{base}/api/call")).await;
    ws.hello_owner().await;
    ws.until(is_state("connected")).await;
    ears.configured().await;
    settle().await;
    ws.press(1.0).await;
    ws.until(is_state("thinking")).await;
    ws.press(1.0).await;
    ws.until(is_kind("flush")).await;
    server.emit(json!({"type": "conversation.item.input_audio_transcription.failed", "item_id": "press-1"}));
    for _ in 0..2 {
        assert_eq!(ws.until(is_kind("heard")).await.last().unwrap().json().unwrap()["text"], "uploaded words");
    }
    ws.close().await;
    until("the call to end", || c.active().is_none()).await;
    assert_eq!(voice.transcribed(), vec![48000, 48000]);
    assert_eq!(brain.heard(), vec!["uploaded words", "uploaded words"]);
    assert!(server.closed() && server.commits().len() == 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_duplicate_talk_and_audio_crossing_the_press_limit_do_not_reset_or_overshoot() {
    let call = Call::new(MemorySocket::fixed(true), FakeBrain::new(""), FakeVoice::live(None));
    call.begin_press().await;
    call.feed(&b"\x12\x00".repeat(4800)).await;
    call.begin_press().await;
    assert_eq!(call.press(), b"\x12\x00".repeat(4800));
    let cap = (MAX_PRESS_SECS * BYTES_PER_SEC as f64) as usize;
    call.set_press(vec![0u8; cap - 2]);
    call.feed(&b"\x34\x00".repeat(2400)).await;
    let press = call.press();
    assert!(press.len() == cap && press[press.len() - 2..] == *b"\x34\x00");
    call.feed(b"\x56\x00").await;
    assert_eq!(call.press().len(), cap);
    call.stop_tasks().await;
}
