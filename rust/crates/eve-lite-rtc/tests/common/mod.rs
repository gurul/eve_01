//! The fakes the Python tests share (test_lite_link_channel.py's FakeChannel, answers, req, until and door;
//! test_link_chat.py's FakeInlet, store_at, two_lines and events; test_phone_call.py's Brain and Voice), for every
//! test file of this crate.

#![allow(dead_code, clippy::type_complexity)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::Value;

use eve_lite_rtc::{py, Channel, ChannelEvent, Data, Listener, ReadyState, BODY_FRAME_BYTES};
use eve_link::link_chat::door::LinkChatOptions;
use eve_link::link_chat::{HiddenJobs, LinkChat};
use eve_link::seams::{AppOut, HearOptions, Inlet};
use eve_link_uploads::UploadStore;
use eve_phone_call::{Brain, Captured, Ears, Reader, SpeechStream, Voice};
use eve_transcripts::{TranscriptConfig, Transcripts};

pub const SECRET: &str = "peer-secret-CANARY-NOT-A-KEY-7f3c";

// ---- the page's side -----------------------------------------------------------------------------------------

/// aiortc's RTCDataChannel as lite_rtc uses it. What is sent is kept in order; `page` is a message from the page;
/// `close` closes it the way aiortc does (readyState, then the "close" event).
pub struct FakeChannel {
    pub label: String,
    pub ordered: bool,
    pub max_packet_life_time: Mutex<Option<u16>>,
    pub max_retransmits: Mutex<Option<u16>>,
    pub state: Mutex<ReadyState>,
    pub buffered: AtomicUsize,
    pub low_threshold: AtomicUsize,
    pub sent: Mutex<Vec<Data>>,
    listeners: Mutex<Vec<Listener>>,
}

impl FakeChannel {
    pub fn new(label: &str) -> Arc<FakeChannel> {
        Self::with(label, true)
    }

    pub fn with(label: &str, ordered: bool) -> Arc<FakeChannel> {
        Arc::new(FakeChannel {
            label: label.into(),
            ordered,
            max_packet_life_time: Mutex::new(None),
            max_retransmits: Mutex::new(None),
            state: Mutex::new(ReadyState::Open),
            buffered: AtomicUsize::new(0),
            low_threshold: AtomicUsize::new(0),
            sent: Mutex::new(Vec::new()),
            listeners: Mutex::new(Vec::new()),
        })
    }

    pub fn emit(&self, event: ChannelEvent) {
        let listeners = self.listeners.lock().clone();
        for l in listeners {
            l(event.clone());
        }
    }

    /// A message from the page: a JSON value is sent as `json.dumps` writes it.
    pub fn page(&self, msg: Value) {
        let data = match msg {
            Value::String(s) => Data::Text(s),
            other => Data::Text(py::dumps(&other, false)),
        };
        self.emit(ChannelEvent::Message(data));
    }

    pub fn page_bytes(&self, b: &[u8]) {
        self.emit(ChannelEvent::Message(Data::Binary(b.to_vec())));
    }

    pub fn sent(&self) -> Vec<Data> {
        self.sent.lock().clone()
    }

    /// `close()` as the page or aiortc does it.
    pub fn close(&self) {
        Channel::close(self)
    }

    pub fn is_closed(&self) -> bool {
        *self.state.lock() == ReadyState::Closed
    }

    /// The text messages, as JSON.
    pub fn got(&self) -> Vec<Value> {
        self.sent()
            .into_iter()
            .filter_map(|d| match d {
                Data::Text(s) => Some(serde_json::from_str(&s).unwrap()),
                Data::Binary(_) => None,
            })
            .collect()
    }
}

#[async_trait]
impl Channel for FakeChannel {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn ordered(&self) -> bool {
        self.ordered
    }
    fn max_packet_life_time(&self) -> Option<u16> {
        *self.max_packet_life_time.lock()
    }
    fn max_retransmits(&self) -> Option<u16> {
        *self.max_retransmits.lock()
    }
    fn ready_state(&self) -> ReadyState {
        *self.state.lock()
    }
    fn send(&self, data: Data) -> Result<(), eve_lite_rtc::channel::ChannelClosed> {
        if *self.state.lock() != ReadyState::Open {
            return Err(eve_lite_rtc::channel::ChannelClosed);
        }
        self.sent.lock().push(data);
        Ok(())
    }
    async fn buffered_amount(&self) -> usize {
        self.buffered.load(Ordering::SeqCst)
    }
    fn set_buffered_amount_low_threshold(&self, n: usize) {
        self.low_threshold.store(n, Ordering::SeqCst);
    }
    fn close(&self) {
        let was_open = {
            let mut s = self.state.lock();
            let open = *s == ReadyState::Open;
            if open {
                *s = ReadyState::Closed;
            }
            open
        };
        if was_open {
            self.emit(ChannelEvent::Close);
        }
    }
    fn on(&self, listener: Listener) {
        self.listeners.lock().push(listener);
    }
}

/// One request's answer as the page reassembles it.
#[derive(Debug, Clone, Default)]
pub struct Answer {
    pub head: Option<Value>,
    pub body: Vec<u8>,
    pub frames: usize,
    pub end: Option<String>,
}

impl Answer {
    pub fn status(&self) -> i64 {
        self.head.as_ref().expect("a head")["s"].as_i64().unwrap()
    }
    pub fn h(&self) -> &serde_json::Map<String, Value> {
        self.head.as_ref().expect("a head")["h"].as_object().unwrap()
    }
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
    pub fn ended(&self) -> bool {
        self.end.is_some()
    }
}

/// Each request's answer as the page reassembles it: `head` (the message), `body` (its frames joined), `frames` and
/// `end` ("end" or "err"). The order is checked as it is read.
pub fn answers(sent: &[Data]) -> HashMap<u32, Answer> {
    let mut out: HashMap<u32, Answer> = HashMap::new();
    for m in sent {
        match m {
            Data::Binary(b) => {
                assert!(b.len() > 4, "a frame with no body");
                let rid = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                assert!(b.len() - 4 <= BODY_FRAME_BYTES);
                let a = out.get_mut(&rid).expect("a frame for a request with a head");
                assert!(a.head.is_some() && a.end.is_none(), "a frame outside head..end");
                a.body.extend_from_slice(&b[4..]);
                a.frames += 1;
            }
            Data::Text(s) => {
                let msg: Value = serde_json::from_str(s).unwrap();
                let rid = msg["id"].as_u64().unwrap() as u32;
                let a = out.entry(rid).or_default();
                assert!(a.end.is_none(), "nothing may follow end or err");
                let keys: Vec<&str> = msg.as_object().unwrap().keys().map(String::as_str).collect();
                if msg["t"] == "head" {
                    assert!(a.head.is_none());
                    let mut k = keys.clone();
                    k.sort();
                    assert_eq!(k, vec!["h", "id", "s", "t"]);
                    a.head = Some(msg);
                } else {
                    let t = msg["t"].as_str().unwrap();
                    assert!(t == "end" || t == "err");
                    let mut k = keys.clone();
                    k.sort();
                    assert_eq!(k, vec!["id", "t"]);
                    a.end = Some(t.to_string());
                }
            }
        }
    }
    out
}

pub fn answer(ch: &FakeChannel, rid: u32) -> Option<Answer> {
    answers(&ch.sent()).get(&rid).cloned()
}

/// `{"t":"req","id":rid,"m":method,"p":path,"q":query,"h":headers}` and `b`: a string body as is, else json.dumps.
pub fn req(rid: Value, method: &str, path: &str, body: Option<Value>, query: &str, headers: &[(&str, &str)]) -> Value {
    let mut h = serde_json::Map::new();
    for (k, v) in headers {
        h.insert((*k).into(), Value::String((*v).into()));
    }
    let mut msg = serde_json::json!({"t": "req", "id": rid, "m": method, "p": path, "q": query, "h": h});
    if let Some(body) = body {
        let b = match body {
            Value::String(s) => s,
            other => py::dumps(&other, false),
        };
        msg["b"] = Value::String(b);
    }
    msg
}

pub fn get(rid: u32, path: &str) -> Value {
    req(rid.into(), "GET", path, None, "", &[])
}

pub async fn until(pred: impl Fn() -> bool, secs: f64) {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while !pred() {
        assert!(Instant::now() < end, "timed out");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub fn ended(ch: &FakeChannel, rid: u32) -> bool {
    answer(ch, rid).is_some_and(|a| a.ended())
}

// ---- the chat's brain (test_link_chat.py) -----------------------------------------------------------------------

pub type Script = Arc<dyn Fn(Arc<FakeInlet>, String) -> BoxFuture<'static, ()> + Send + Sync>;

/// The inlet as link_chat uses it: hear, owner_busy, app_out, transcripts and the chat id. `script` runs as the turn
/// would, in a task, and calls app_out the way TelegramInlet._to_app does.
pub struct FakeInlet {
    me: Weak<FakeInlet>,
    app_out: Mutex<Option<AppOut>>,
    transcripts: Option<Arc<Transcripts>>,
    pub heard: Mutex<Vec<(String, bool, Option<String>)>>,
    pub busy: AtomicBool,
    script: Option<Script>,
    pub delivered: Mutex<Vec<bool>>,
}

impl FakeInlet {
    pub fn new(store: Option<Arc<Transcripts>>, script: Option<Script>) -> Arc<FakeInlet> {
        Arc::new_cyclic(|me| FakeInlet {
            me: me.clone(),
            app_out: Mutex::new(None),
            transcripts: store,
            heard: Mutex::new(Vec::new()),
            busy: AtomicBool::new(false),
            script,
            delivered: Mutex::new(Vec::new()),
        })
    }

    /// One of eve's lines for app turn `app`, through app_out; whether a stream read it is kept in `delivered`.
    pub fn out(&self, app: &str, text: &str) {
        let out = self.app_out.lock().clone().expect("app_out is set");
        let read = out(app, text, "say");
        self.delivered.lock().push(read);
    }

    pub fn heard(&self) -> Vec<(String, bool, Option<String>)> {
        self.heard.lock().clone()
    }

    pub fn delivered(&self) -> Vec<bool> {
        self.delivered.lock().clone()
    }
}

impl Inlet for FakeInlet {
    fn chat_ready(&self) -> bool {
        true
    }

    fn hear(&self, text: &str, opts: HearOptions) -> anyhow::Result<()> {
        self.heard.lock().push((text.to_string(), opts.on_call, opts.app.clone()));
        if let Some(script) = self.script.clone() {
            self.busy.store(true, Ordering::SeqCst);
            let me = self.me.upgrade().expect("the inlet");
            tokio::spawn(script(me, opts.app.unwrap_or_default()));
        }
        Ok(())
    }

    fn owner_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    fn set_app_out(&self, out: AppOut) {
        *self.app_out.lock() = Some(out);
    }

    fn transcripts(&self) -> Option<Arc<Transcripts>> {
        self.transcripts.clone()
    }
}

pub fn store_at(tmp: &Path) -> Arc<Transcripts> {
    let store = Transcripts::new(TranscriptConfig::new(true, tmp.join("transcripts")), None, None);
    assert!(store.enabled);
    Arc::new(store)
}

pub fn script<F, Fut>(f: F) -> Script
where
    F: Fn(Arc<FakeInlet>, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    Arc::new(move |inlet, app| f(inlet, app).boxed())
}

pub fn two_lines() -> Script {
    script(|inlet: Arc<FakeInlet>, app: String| async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        inlet.out(&app, "First line.");
        tokio::time::sleep(Duration::from_millis(20)).await;
        inlet.out(&app, "Second line.");
        inlet.busy.store(false, Ordering::SeqCst);
    })
}

/// Busy, never done: one line and the turn stays open.
pub fn forever() -> Script {
    script(|inlet: Arc<FakeInlet>, app: String| async move {
        inlet.out(&app, "Working on it.");
    })
}

/// LinkChat with this inlet and its stores in `tmp` (test_lite_link_channel.door).
pub fn door(tmp: &Path, inlet: Arc<FakeInlet>, mut opts: LinkChatOptions) -> LinkChat {
    std::fs::create_dir_all(tmp).unwrap();
    opts.uploads = Some(Arc::new(UploadStore::new(Some(tmp.join("uploads")))));
    opts.hidden = Some(Arc::new(HiddenJobs::new(Some(&tmp.join("hidden-jobs.json")))));
    let inlet: Arc<dyn Inlet> = inlet;
    LinkChat::new(Arc::new(move || Some(inlet.clone())), opts)
}

pub fn fast() -> LinkChatOptions {
    LinkChatOptions { settle_secs: 0.01, ..LinkChatOptions::default() }
}

/// (each chunk's content, whether [DONE] came last, how many pings). Every event must be well formed.
pub fn events(body: &[u8]) -> (Vec<String>, bool, usize) {
    let text = std::str::from_utf8(body).unwrap();
    assert!(text.ends_with("\n\n"));
    let (mut contents, mut done, mut pings) = (Vec::new(), false, 0);
    for event in text[..text.len() - 2].split("\n\n") {
        assert!(!done, "nothing may follow [DONE]");
        if event == ": ping" {
            pings += 1;
        } else if event == "data: [DONE]" {
            done = true;
        } else {
            assert!(event.starts_with("data: ") && !event.contains('\n'));
            let chunk: Value = serde_json::from_str(&event["data: ".len()..]).unwrap();
            contents.push(chunk["choices"][0]["delta"]["content"].as_str().unwrap().to_string());
            assert_eq!(chunk["choices"][0]["index"], 0);
        }
    }
    (contents, done, pings)
}

// ---- the call's chat and voice (test_phone_call.py) --------------------------------------------------------------

/// The chat: hears the owner's words and answers through the call's reader, as telegram.TelegramInlet does.
pub struct FakeBrain {
    pub reply: String,
    pub heard: Mutex<Vec<String>>,
    pub say: Mutex<Option<Reader>>,
}

impl FakeBrain {
    pub fn new(reply: &str) -> Arc<FakeBrain> {
        Arc::new(FakeBrain { reply: reply.into(), heard: Mutex::new(Vec::new()), say: Mutex::new(None) })
    }

    pub fn default_reply() -> Arc<FakeBrain> {
        Self::new("You have **two** meetings: [standup](https://x.example) and lunch.")
    }

    pub fn heard(&self) -> Vec<String> {
        self.heard.lock().clone()
    }
}

impl Brain for FakeBrain {
    fn hear(&self, text: &str, _call: Option<&Reader>, _answer_context: Captured) {
        self.heard.lock().push(text.to_string());
        let say = self.say.lock().clone();
        if let Some(say) = say {
            if !self.reply.is_empty() {
                say.say(&self.reply);
            }
        }
    }

    fn listen(&self, say: Option<Reader>, owner: Option<&Reader>) -> bool {
        let mut cur = self.say.lock();
        if say.is_some() || owner.is_none() || cur.as_ref() == owner {
            *cur = say;
        }
        true
    }
}

/// The voice: a fixed transcript; 4,800 bytes of speech per sentence (1,200 samples, an odd byte, then the rest),
/// or with `tone`, 0.6 s of 440 Hz (ToneVoice). `pcm` keeps the press it was given.
pub struct FakeVoice {
    pub text: String,
    pub tone: bool,
    pub spoken: Mutex<Vec<String>>,
    pub transcribed: Mutex<Vec<usize>>,
    pub pcm: Mutex<Option<Vec<u8>>>,
}

impl FakeVoice {
    pub fn new(text: &str) -> Arc<FakeVoice> {
        Self::make(text, false)
    }

    fn make(text: &str, tone: bool) -> Arc<FakeVoice> {
        Arc::new(FakeVoice {
            text: text.into(),
            tone,
            spoken: Mutex::new(Vec::new()),
            transcribed: Mutex::new(Vec::new()),
            pcm: Mutex::new(None),
        })
    }

    pub fn calendar() -> Arc<FakeVoice> {
        Self::new("what's on my calendar")
    }

    pub fn tone() -> Arc<FakeVoice> {
        Self::make("what time is it", true)
    }

    pub fn transcribed(&self) -> Vec<usize> {
        self.transcribed.lock().clone()
    }

    pub fn pcm(&self) -> Option<Vec<u8>> {
        self.pcm.lock().clone()
    }
}

impl Voice for FakeVoice {
    fn transcribe(&self, pcm: &[u8]) -> anyhow::Result<String> {
        self.transcribed.lock().push(pcm.len());
        *self.pcm.lock() = Some(pcm.to_vec());
        Ok(self.text.clone())
    }

    fn speak_stream(&self, text: &str) -> SpeechStream<'_> {
        self.spoken.lock().push(text.to_string());
        let chunks: Vec<anyhow::Result<Vec<u8>>> = if self.tone {
            let rate = eve_lite_rtc::SAMPLE_RATE as f64;
            let n = (0.6 * rate) as usize;
            let data: Vec<u8> = (0..n)
                .flat_map(|i| (((2.0 * std::f64::consts::PI * 440.0 * i as f64 / rate).sin() * 8000.0) as i16).to_le_bytes())
                .collect();
            data.chunks(4800).map(|c| Ok(c.to_vec())).collect()
        } else {
            let mut third = vec![0u8];
            third.extend(b"\x01\x00".repeat(1199));
            vec![Ok(b"\x01\x00".repeat(1200)), Ok(vec![1u8]), Ok(third)]
        };
        Box::new(chunks.into_iter())
    }

    fn live_ears(&self) -> Option<Arc<dyn Ears>> {
        None
    }
}

// ---- audio ---------------------------------------------------------------------------------------------------------

pub fn i16s(pcm: &[u8]) -> Vec<i16> {
    pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
}

pub fn bytes_of(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// The strongest frequency in `pcm` (Hann-windowed), on the rfft's own grid (`rate / n` apart), as numpy's argmax
/// of `abs(rfft(x * hanning(n)))` finds it.
pub fn dominant_hz(pcm: &[u8], rate: u32) -> f64 {
    let x: Vec<f64> = i16s(pcm).into_iter().map(f64::from).collect();
    let n = x.len();
    let w: Vec<f64> = (0..n)
        .map(|i| if n > 1 { 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos() } else { 1.0 })
        .zip(&x)
        .map(|(h, v)| h * v)
        .collect();
    let power = |k: usize| {
        // Goertzel at bin k
        let coeff = 2.0 * (2.0 * std::f64::consts::PI * k as f64 / n as f64).cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for v in &w {
            let s = v + coeff * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        s1 * s1 + s2 * s2 - coeff * s1 * s2
    };
    let step = rate as f64 / n as f64;
    // a coarse pass every ~10 Hz, then every bin around the best
    let stride = ((10.0 / step).floor() as usize).max(1);
    let half = n / 2;
    let mut best = (0usize, -1.0f64);
    let mut k = 0;
    while k <= half {
        let p = power(k);
        if p > best.1 {
            best = (k, p);
        }
        k += stride;
    }
    let lo = best.0.saturating_sub(stride);
    let hi = (best.0 + stride).min(half);
    for k in lo..=hi {
        let p = power(k);
        if p > best.1 {
            best = (k, p);
        }
    }
    best.0 as f64 * step
}

/// `ms` of 24 kHz mono PCM holding one value, so where each piece went can be read back.
pub fn pcm(ms: usize, value: i16) -> Vec<u8> {
    bytes_of(&vec![value; eve_lite_rtc::SAMPLE_RATE as usize * ms / 1000])
}

/// The distinct sample values, sorted.
pub fn values(data: &[u8]) -> Vec<i16> {
    let mut v = i16s(data);
    v.sort();
    v.dedup();
    v
}
