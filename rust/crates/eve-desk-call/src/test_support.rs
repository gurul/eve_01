//! The fakes of `test_desk_call.py`: a chat, a voice (no OpenAI), a microphone the test feeds, and a speaker that
//! records what it played.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eve_phone_call::{Captured, Ears, Reader, SpeechStream};
use parking_lot::Mutex;
use serde_json::Value;

use crate::{BoardSend, DeskCalls, DeskMic, DeskOptions, DeskSpeaker, MicFactory, OnBlock, SpeakerFactory};

pub struct Brain {
    pub reply: String,
    pub heard: Mutex<Vec<String>>,
    pub say: Mutex<Option<Reader>>,
}

impl Brain {
    pub fn new(reply: &str) -> Arc<Self> {
        Arc::new(Brain { reply: reply.into(), heard: Mutex::new(Vec::new()), say: Mutex::new(None) })
    }

    pub fn default_reply() -> Arc<Self> {
        Self::new("It is **three** o'clock.")
    }

    pub fn heard(&self) -> Vec<String> {
        self.heard.lock().clone()
    }
}

impl eve_phone_call::Brain for Brain {
    fn listen(&self, say: Option<Reader>, owner: Option<&Reader>) -> bool {
        let mut cur = self.say.lock();
        if say.is_some() || owner.is_none() || cur.as_ref() == owner {
            // as TelegramInlet.listen
            *cur = say;
        }
        true
    }

    fn hear(&self, text: &str, _call: Option<&Reader>, _answer_context: Captured) {
        self.heard.lock().push(text.to_string());
        let say = self.say.lock().clone();
        if let Some(say) = say {
            say.say(&self.reply);
        }
    }
}

#[derive(Default)]
pub struct Voice {
    pub transcribed: Mutex<Vec<usize>>,
    pub spoken: Mutex<Vec<String>>,
}

impl Voice {
    pub fn new() -> Arc<Self> {
        Arc::new(Voice::default())
    }
}

impl eve_phone_call::Voice for Voice {
    fn transcribe(&self, pcm: &[u8]) -> anyhow::Result<String> {
        self.transcribed.lock().push(pcm.len());
        Ok("what time is it".into())
    }

    fn speak_stream(&self, text: &str) -> SpeechStream<'_> {
        self.spoken.lock().push(text.to_string());
        Box::new(std::iter::once(Ok([1u8, 0].repeat(2400))))
    }

    fn live_ears(&self) -> Option<Arc<dyn Ears>> {
        None
    }
}

/// One microphone the desk made.
pub struct MicRec {
    pub device: Option<String>,
    pub on_block: OnBlock,
    pub opened: AtomicBool,
    pub closed: AtomicBool,
}

impl MicRec {
    pub fn opened(&self) -> bool {
        self.opened.load(Ordering::SeqCst)
    }
    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
    pub fn block(&self, pcm: &[u8]) {
        (self.on_block)(pcm.to_vec());
    }
}

struct FakeMic(Arc<MicRec>);

impl DeskMic for FakeMic {
    fn open(&mut self) -> bool {
        self.0.opened.store(true, Ordering::SeqCst);
        true
    }
    fn close(&mut self) {
        self.0.closed.store(true, Ordering::SeqCst);
    }
}

/// `Mic.made`, per test (the tests run in parallel).
pub type Made = Arc<Mutex<Vec<Arc<MicRec>>>>;

pub fn mic_factory() -> (MicFactory, Made) {
    let made: Made = Arc::default();
    let m = made.clone();
    let factory: MicFactory = Arc::new(move |device, on_block| {
        let rec = Arc::new(MicRec { device, on_block, opened: AtomicBool::new(false), closed: AtomicBool::new(false) });
        m.lock().push(rec.clone());
        Box::new(FakeMic(rec)) as Box<dyn DeskMic>
    });
    (factory, made)
}

pub fn last(made: &Made) -> Arc<MicRec> {
    made.lock().last().cloned().expect("a microphone was made")
}

pub fn open_mics(made: &Made) -> usize {
    made.lock().iter().filter(|m| m.opened() && !m.closed()).count()
}

#[derive(Default)]
pub struct FakeSpeaker {
    pub played: Mutex<Vec<u8>>,
    pub flushes: AtomicUsize,
    pub closed: AtomicBool,
}

impl FakeSpeaker {
    pub fn played(&self) -> usize {
        self.played.lock().len()
    }
    pub fn flushes(&self) -> usize {
        self.flushes.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl DeskSpeaker for FakeSpeaker {
    async fn play(&self, pcm: &[u8]) -> anyhow::Result<()> {
        self.played.lock().extend_from_slice(pcm);
        Ok(())
    }
    async fn flush(&self) -> anyhow::Result<()> {
        self.flushes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

pub type Speakers = Arc<Mutex<Vec<Arc<FakeSpeaker>>>>;

pub fn speaker_factory() -> (SpeakerFactory, Speakers) {
    let speakers: Speakers = Arc::default();
    let s = speakers.clone();
    let factory: SpeakerFactory = Arc::new(move || {
        let spk = Arc::new(FakeSpeaker::default());
        s.lock().push(spk.clone());
        spk as Arc<dyn DeskSpeaker>
    });
    (factory, speakers)
}

pub type Sent = Arc<Mutex<Vec<Value>>>;

/// A board that records every line.
pub fn recorder() -> (BoardSend, Sent) {
    let sent: Sent = Arc::default();
    let s = sent.clone();
    let send: BoardSend = Arc::new(move |obj: Value| {
        s.lock().push(obj);
        Box::pin(async { Ok(()) })
    });
    (send, sent)
}

pub struct Rig {
    pub desk: DeskCalls,
    pub sent: Sent,
    pub speakers: Speakers,
    pub made: Made,
}

/// `calls(brain, voice, busy)`.
pub fn calls(
    brain: Option<Arc<Brain>>,
    voice: Option<Arc<Voice>>,
    busy: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
) -> Rig {
    let (send, sent) = recorder();
    let (speaker_factory, speakers) = speaker_factory();
    let (mic_factory, made) = mic_factory();
    let desk = DeskCalls::new(
        brain.map(|b| b as Arc<dyn eve_phone_call::Brain>),
        voice.map(|v| v as Arc<dyn eve_phone_call::Voice>),
        send,
        DeskOptions {
            mic_device: Some("yeti".into()),
            busy: busy.unwrap_or_else(|| Arc::new(|| false)),
            mic_factory,
            speaker_factory,
            on_phase: None,
        },
    );
    Rig { desk, sent, speakers, made }
}

pub fn rings(sent: &Sent) -> Vec<String> {
    sent.lock()
        .iter()
        .filter(|m| m.get("cmd").and_then(Value::as_str) == Some("agent"))
        .map(|m| m["state"].as_str().unwrap_or("").to_string())
        .collect()
}

pub async fn until(pred: impl Fn() -> bool, secs: f64) {
    let r = tokio::time::timeout(Duration::from_secs_f64(secs), async {
        while !pred() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(r.is_ok(), "timed out waiting");
}

pub async fn sleep(secs: f64) {
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
}

/// A few turns of the loop (test_telegram's `settle`).
pub async fn settle() {
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    sleep(0.02).await;
}
