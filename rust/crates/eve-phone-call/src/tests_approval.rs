//! test_phone_call_approval.py, the call's half: each press carries the question it was recorded under, whatever the
//! chat asks meanwhile. Python drove the real Telegram inlet and purchase adapter (the brain crate's); here a chat
//! that only records which question each heard press carried, with a transcriber held until the test lets it go.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use super::test_support::*;
use super::*;

/// One open question (telegram's `_open_question`), told apart by name.
struct Question(&'static str);

/// What one press reached the chat with.
#[derive(Debug, Clone, PartialEq)]
enum Carried {
    Uncaptured,
    NoQuestion,
    Question(&'static str),
}

/// The chat: its open question is whatever the test set last; `hear` records the question the press carried.
struct CaptureBrain {
    question: Mutex<Option<Arc<Question>>>,
    heard: Mutex<Vec<(String, Carried, bool)>>,
    say: Mutex<Option<Reader>>,
    capture: bool,
    takes_call: bool,
    /// The question the chat moves to when the call takes its listener (a question that changes while
    /// `begin_press` yields).
    on_listen: Mutex<Option<&'static str>>,
}

impl CaptureBrain {
    fn new(capture: bool) -> Arc<Self> {
        Arc::new(Self {
            question: Mutex::new(None),
            heard: Mutex::new(vec![]),
            say: Mutex::new(None),
            capture,
            takes_call: false,
            on_listen: Mutex::new(None),
        })
    }

    fn ask(&self, name: &'static str) {
        *self.question.lock() = Some(Arc::new(Question(name)));
    }

    fn heard(&self) -> Vec<(String, Carried, bool)> {
        self.heard.lock().clone()
    }
}

impl Brain for CaptureBrain {
    fn hear(&self, text: &str, call: Option<&Reader>, answer_context: Captured) {
        let carried = match answer_context {
            Captured::Uncaptured => Carried::Uncaptured,
            Captured::Context(None) => Carried::NoQuestion,
            Captured::Context(Some(q)) => {
                Carried::Question(q.downcast_ref::<Question>().expect("a question").0)
            }
        };
        let same_call = call.is_some() && call == self.say.lock().as_ref();
        self.heard.lock().push((text.to_string(), carried, same_call));
    }

    fn listen(&self, say: Option<Reader>, owner: Option<&Reader>) -> bool {
        if say.is_some() {
            if let Some(next) = self.on_listen.lock().take() {
                self.ask(next);
            }
        }
        listen_slot(&self.say, say, owner)
    }

    fn chat_ready(&self) -> Option<bool> {
        Some(true)
    }

    fn answer_context(&self) -> Option<AnswerContext> {
        if !self.capture {
            return None;
        }
        Some(self.question.lock().clone().map(|q| q as Arc<dyn Any + Send + Sync>))
    }

    fn takes_call(&self) -> bool {
        self.takes_call
    }
}

/// The upload transcriber, held per press (the press's first byte names it) until the test releases it.
struct HeldVoice {
    entered: [Gate; 3],
    release: [Gate; 3],
    finished: [Gate; 3],
    words: Mutex<[&'static str; 3]>,
    ears: Mutex<Option<Arc<dyn Ears>>>,
}

impl HeldVoice {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Default::default(),
            release: Default::default(),
            finished: Default::default(),
            words: Mutex::new(["", "yes", "yes"]),
            ears: Mutex::new(None),
        })
    }

    fn unblock(&self) {
        for g in &self.release {
            g.set();
        }
    }
}

impl Voice for HeldVoice {
    fn transcribe(&self, pcm: &[u8]) -> anyhow::Result<String> {
        let i = pcm[0] as usize;
        self.entered[i].set();
        let released = self.release[i].wait(3.0);
        self.finished[i].set();
        assert!(released, "test did not release STT");
        Ok(self.words.lock()[i].to_string())
    }

    fn speak_stream(&self, _text: &str) -> SpeechStream<'_> {
        Box::new(std::iter::empty())
    }

    fn live_ears(&self) -> Option<Arc<dyn Ears>> {
        self.ears.lock().clone()
    }
}

/// Hold live STT too: capture context must be independent of the transcription provider.
struct HeldEars {
    voice: std::sync::Weak<HeldVoice>,
    i: Mutex<usize>,
}

#[async_trait::async_trait]
impl Ears for HeldEars {
    async fn open(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn begin(&self) -> anyhow::Result<()> {
        *self.i.lock() = 0;
        Ok(())
    }
    async fn discard(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn feed(&self, pcm: &[u8]) -> anyhow::Result<()> {
        *self.i.lock() = pcm[0] as usize;
        Ok(())
    }
    async fn finish(&self, _timeout: f64) -> anyhow::Result<String> {
        let i = *self.i.lock();
        let voice = self.voice.upgrade().unwrap();
        voice.entered[i].set();
        until("the test to release live STT", || voice.release[i].is_set()).await;
        voice.finished[i].set();
        let w = voice.words.lock()[i];
        Ok(w.to_string())
    }
    async fn close(&self) {}
    fn close_now(&self) {}
    fn broken(&self) -> bool {
        false
    }
    fn set_broken(&self) {}
}

struct Harness {
    brain: Arc<CaptureBrain>,
    voice: Arc<HeldVoice>,
    ws: Arc<MemorySocket>,
    call: Arc<Call>,
    run: tokio::task::JoinHandle<String>,
}

impl Harness {
    fn new(live: bool, brain: Arc<CaptureBrain>) -> Self {
        let voice = HeldVoice::new();
        if live {
            *voice.ears.lock() = Some(Arc::new(HeldEars { voice: Arc::downgrade(&voice), i: Mutex::new(0) }));
        }
        let ws = MemorySocket::new();
        let call = Call::new(ws.clone(), brain.clone(), voice.clone());
        let c = call.clone();
        let run = tokio::spawn(async move { c.run().await });
        Self { brain, voice, ws, call, run }
    }

    fn flushes(&self) -> usize {
        self.ws.sent().iter().filter(|m| m.is("flush")).count()
    }

    async fn begin(&self) {
        let before = self.flushes();
        self.ws.command("talk");
        until("the press to begin", || self.flushes() > before).await;
    }

    async fn release(&self, i: u8) {
        self.ws.page_bytes([i, 0].repeat(SAMPLE_RATE as usize));
        self.ws.command("done");
        until("STT to start", || self.voice.entered[i as usize].is_set()).await;
    }

    async fn finish(&self, i: usize) {
        let before = self.call.presses();
        self.voice.release[i].set();
        until("the press to be heard", || self.call.presses() > before).await;
        until("the press's task to end", || !self.call.has_tasks()).await;
    }

    async fn close(self) -> String {
        self.voice.unblock();
        if !self.run.is_finished() {
            self.ws.command("end");
        }
        tokio::time::timeout(Duration::from_secs(3), self.run).await.unwrap().unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_old_spoken_yes_cannot_approve_new_purchase() {
    for initial_question in [true, false] {
        for change_before_release in [true, false] {
            for live in [true, false] {
                let case = format!("initial={initial_question} change_first={change_before_release} live={live}");
                let h = Harness::new(live, CaptureBrain::new(true));
                if initial_question {
                    h.brain.ask("A");
                }
                h.begin().await; // capture A, or capture the absence of any question
                if change_before_release {
                    h.brain.ask("B");
                    h.release(1).await;
                } else {
                    h.release(1).await; // STT is running, but has no text yet
                    h.brain.ask("B");
                }
                h.finish(1).await;
                let first = if initial_question { Carried::Question("A") } else { Carried::NoQuestion };
                assert_eq!(h.brain.heard()[0].1, first, "{case}");
                // Positive control: a fresh press under B really carries B.
                h.begin().await;
                h.release(2).await;
                h.finish(2).await;
                assert_eq!(h.brain.heard()[1].1, Carried::Question("B"), "{case}");
                assert_eq!(h.brain.heard().len(), 2, "{case}");
                h.close().await;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_context_is_captured_before_begin_press_yields() {
    let brain = CaptureBrain::new(true);
    let h = Harness::new(false, brain.clone());
    brain.ask("A");
    *brain.on_listen.lock() = Some("B"); // the question changes as the press takes the chat's listener
    h.begin().await;
    assert!(matches!(&*brain.question.lock(), Some(q) if q.0 == "B"));
    h.release(1).await;
    h.finish(1).await;
    assert_eq!(brain.heard()[0].1, Carried::Question("A"));
    h.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_overlapping_transcriptions_keep_each_presses_question_identity() {
    let h = Harness::new(false, CaptureBrain::new(true));
    h.brain.ask("A");
    h.begin().await;
    h.release(1).await;
    h.brain.ask("B");
    h.begin().await;
    h.release(2).await;
    // The first transcript arrives while the second press is held in STT. It must still carry A.
    h.voice.release[1].set();
    until("the first press to be heard", || h.call.presses() == 1).await;
    assert_eq!(h.brain.heard()[0].1, Carried::Question("A"));
    h.voice.release[2].set();
    until("the second press to be heard", || h.call.presses() == 2).await;
    assert_eq!(h.brain.heard()[1].1, Carried::Question("B"));
    h.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_duplicate_talk_does_not_rebind_recording_to_a_new_question() {
    let h = Harness::new(false, CaptureBrain::new(true));
    h.brain.ask("A");
    h.begin().await;
    h.brain.ask("B");
    h.ws.command("talk"); // no new press; this frame arrives before the audio and done below
    h.release(1).await;
    h.finish(1).await;
    assert_eq!(h.brain.heard()[0].1, Carried::Question("A"));
    h.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_hangup_cancels_a_pending_transcript_without_approving_purchase() {
    let h = Harness::new(false, CaptureBrain::new(true));
    h.brain.ask("B");
    h.begin().await;
    h.release(1).await;
    h.ws.command("end");
    let (brain, voice, call) = (h.brain.clone(), h.voice.clone(), h.call.clone());
    assert_eq!(tokio::time::timeout(Duration::from_secs(3), h.run).await.unwrap().unwrap(), HUNG_UP);
    voice.release[1].set(); // the upload worker cannot be stopped; its result must never enter hear
    until("STT to finish", || voice.finished[1].is_set()).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(brain.heard().is_empty());
    assert!(call.presses() == 0 && !call.has_tasks());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_legacy_brain_keeps_one_argument_hear() {
    let h = Harness::new(false, CaptureBrain::new(false));
    h.ws.command("talk");
    h.ws.page_bytes(b"\x01\x00".repeat(SAMPLE_RATE as usize));
    h.ws.command("done");
    until("STT to start", || h.voice.entered[1].is_set()).await;
    h.voice.release[1].set();
    until("the words to be heard", || !h.brain.heard().is_empty()).await;
    // legacy brains have no capture hook or `call` keyword argument
    assert_eq!(h.brain.heard(), vec![("yes".to_string(), Carried::Uncaptured, false)]);
    h.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_brain_that_takes_call_is_told_which_call_heard_the_words() {
    let brain = Arc::new(CaptureBrain {
        question: Mutex::new(None),
        heard: Mutex::new(vec![]),
        say: Mutex::new(None),
        capture: false,
        takes_call: true,
        on_listen: Mutex::new(None),
    });
    let h = Harness::new(false, brain.clone());
    h.begin().await;
    h.release(1).await;
    h.finish(1).await;
    assert_eq!(brain.heard(), vec![("yes".to_string(), Carried::Uncaptured, true)]);
    h.close().await;
}
