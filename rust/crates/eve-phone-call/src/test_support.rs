//! The fakes of test_phone_call.py (a chat that answers through the call's reader, a voice that needs no model, the
//! live ears), an in-memory socket, and a loopback server that answers the upgrade as the Mini App server does and
//! hands the stream to `PhoneCalls::serve` (the `/api/call` door with initData, `/api/stick` with Eve Link's token).

#![allow(clippy::type_complexity)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar};
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::*;

pub const OWNER: i64 = 4242;
pub const SIGNED: &str = "query_id=AAH&user=%7B%22id%22%3A4242%7D&hash=signed-by-the-bot";
pub const LINK: &str = "link-token";

/// miniapp.check_init_data for these tests: only the owner's signed initData is an owner.
pub fn check_owner() -> CheckOwner {
    Arc::new(|d: &str| (d == SIGNED).then_some(OWNER))
}

// ---- the chat --------------------------------------------------------------------------------------------------

/// The chat: hears the owner's words and answers through the call's reader, as telegram.TelegramInlet does.
pub struct FakeBrain {
    pub reply: String,
    pub heard: Mutex<Vec<String>>,
    pub say: Mutex<Option<Reader>>,
    /// ReadyBrain: asked whether it is there without taking its listener.
    pub ready: Option<bool>,
}

impl FakeBrain {
    pub fn new(reply: &str) -> Arc<Self> {
        Arc::new(Self { reply: reply.into(), heard: Mutex::new(vec![]), say: Mutex::new(None), ready: None })
    }

    pub fn default_reply() -> Arc<Self> {
        Self::new("You have **two** meetings: [standup](https://x.example) and lunch.")
    }

    pub fn ready(reply: &str) -> Arc<Self> {
        Arc::new(Self { reply: reply.into(), heard: Mutex::new(vec![]), say: Mutex::new(None), ready: Some(true) })
    }

    pub fn heard(&self) -> Vec<String> {
        self.heard.lock().clone()
    }

    pub fn say(&self) -> Option<Reader> {
        self.say.lock().clone()
    }
}

/// `TelegramInlet.listen`: `listen(None, owner)` clears only its own listener.
pub fn listen_slot(slot: &Mutex<Option<Reader>>, say: Option<Reader>, owner: Option<&Reader>) -> bool {
    let mut cur = slot.lock();
    if say.is_some() || owner.is_none() || cur.as_ref() == owner {
        *cur = say;
    }
    true
}

impl Brain for FakeBrain {
    fn hear(&self, text: &str, _call: Option<&Reader>, _answer_context: Captured) {
        self.heard.lock().push(text.to_string());
        let say = self.say();
        if let Some(say) = say {
            if !self.reply.is_empty() {
                say.say(&self.reply);
            }
        }
    }

    fn listen(&self, say: Option<Reader>, owner: Option<&Reader>) -> bool {
        listen_slot(&self.say, say, owner)
    }

    fn chat_ready(&self) -> Option<bool> {
        self.ready
    }
}

// ---- the voice -------------------------------------------------------------------------------------------------

pub enum Speech {
    /// 1200 samples, an odd byte, then the rest: 4800 bytes in all.
    Odd,
    /// One sentence is half a second of sound (12000 samples), made at once.
    HalfSecond,
}

pub struct FakeVoice {
    pub text: String,
    pub spoken: Mutex<Vec<String>>,
    pub transcribed: Mutex<Vec<usize>>,
    pub delay: Duration,
    pub speech: Speech,
    pub ears: Option<Arc<dyn Ears>>,
}

impl FakeVoice {
    pub fn new(text: &str) -> Arc<Self> {
        Arc::new(Self::plain(text))
    }

    pub fn plain(text: &str) -> Self {
        Self {
            text: text.into(),
            spoken: Mutex::new(vec![]),
            transcribed: Mutex::new(vec![]),
            delay: Duration::ZERO,
            speech: Speech::Odd,
            ears: None,
        }
    }

    pub fn calendar() -> Arc<Self> {
        Self::new("what's on my calendar")
    }

    /// LiveVoice: the uploaded words differ from the live ones.
    pub fn live(ears: Option<Arc<dyn Ears>>) -> Arc<Self> {
        Arc::new(Self { ears, ..Self::plain("uploaded words") })
    }

    pub fn spoken(&self) -> Vec<String> {
        self.spoken.lock().clone()
    }

    pub fn transcribed(&self) -> Vec<usize> {
        self.transcribed.lock().clone()
    }
}

impl Voice for FakeVoice {
    fn transcribe(&self, pcm: &[u8]) -> anyhow::Result<String> {
        self.transcribed.lock().push(pcm.len());
        Ok(self.text.clone())
    }

    fn speak_stream(&self, text: &str) -> SpeechStream<'_> {
        std::thread::sleep(self.delay);
        self.spoken.lock().push(text.to_string());
        let chunks: Vec<anyhow::Result<Vec<u8>>> = match self.speech {
            Speech::Odd => {
                let mut third = vec![0u8];
                third.extend(b"\x01\x00".repeat(1199));
                vec![Ok(b"\x01\x00".repeat(1200)), Ok(vec![1u8]), Ok(third)]
            }
            Speech::HalfSecond => vec![Ok(b"\x01\x00".repeat(12000))],
        };
        Box::new(chunks.into_iter())
    }

    fn live_ears(&self) -> Option<Arc<dyn Ears>> {
        self.ears.clone()
    }
}

/// test_phone_call.FakeEars.
pub struct FakeEars {
    pub text: String,
    pub fail: bool,
    pub fed: AtomicUsize,
    pub begun: AtomicUsize,
    pub broken: AtomicBool,
}

impl FakeEars {
    pub fn new(fail: bool) -> Arc<Self> {
        Arc::new(Self {
            text: "live words".into(),
            fail,
            fed: AtomicUsize::new(0),
            begun: AtomicUsize::new(0),
            broken: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl Ears for FakeEars {
    async fn open(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn begin(&self) -> anyhow::Result<()> {
        self.begun.fetch_add(1, Ordering::SeqCst);
        self.fed.store(0, Ordering::SeqCst);
        Ok(())
    }
    async fn discard(&self) -> anyhow::Result<()> {
        self.begin().await
    }
    async fn feed(&self, pcm: &[u8]) -> anyhow::Result<()> {
        self.fed.fetch_add(pcm.len(), Ordering::SeqCst);
        Ok(())
    }
    async fn finish(&self, _timeout: f64) -> anyhow::Result<String> {
        if self.fail {
            return Err(EarsError::Timeout.into());
        }
        Ok(self.text.clone())
    }
    async fn close(&self) {}
    fn close_now(&self) {}
    fn broken(&self) -> bool {
        self.broken.load(Ordering::SeqCst)
    }
    fn set_broken(&self) {
        self.broken.store(true, Ordering::SeqCst)
    }
}

// ---- a gate a blocking fake waits on (threading.Event) ------------------------------------------------------------

#[derive(Default)]
pub struct Gate {
    set: std::sync::Mutex<bool>,
    cv: Condvar,
}

impl Gate {
    pub fn set(&self) {
        *self.set.lock().unwrap() = true;
        self.cv.notify_all();
    }
    pub fn is_set(&self) -> bool {
        *self.set.lock().unwrap()
    }
    pub fn wait(&self, secs: f64) -> bool {
        let guard = self.set.lock().unwrap();
        let (guard, _) = self.cv.wait_timeout_while(guard, Duration::from_secs_f64(secs), |s| !*s).unwrap();
        *guard
    }
}

pub async fn until(what: &str, pred: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !pred() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

// ---- an in-memory socket -----------------------------------------------------------------------------------------

/// What serve_socket drives. `page` feeds it a message; `sent` is everything the call sent, in order.
pub struct MemorySocket {
    inbox_tx: mpsc::UnboundedSender<Option<(u8, Vec<u8>)>>,
    inbox: tokio::sync::Mutex<mpsc::UnboundedReceiver<Option<(u8, Vec<u8>)>>>,
    out_tx: mpsc::UnboundedSender<Msg>,
    pub out: tokio::sync::Mutex<mpsc::UnboundedReceiver<Msg>>,
    pub sent: Mutex<Vec<Msg>>,
    pub closed: AtomicBool,
    pub closes: AtomicUsize,
    /// A socket whose `closed` is fixed (the tests' `SimpleNamespace(closed=...)`).
    pub fixed_closed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Json(Value),
    Bin(Vec<u8>),
}

impl Msg {
    pub fn json(&self) -> Option<&Value> {
        match self {
            Msg::Json(v) => Some(v),
            Msg::Bin(_) => None,
        }
    }

    pub fn is(&self, kind: &str) -> bool {
        self.json().is_some_and(|v| v.get("type").and_then(Value::as_str) == Some(kind))
    }

    pub fn state(&self) -> Option<&str> {
        if self.is("state") {
            self.json().and_then(|v| v.get("state")).and_then(Value::as_str)
        } else {
            None
        }
    }
}

impl MemorySocket {
    pub fn new() -> Arc<Self> {
        Self::build(None)
    }

    pub fn fixed(closed: bool) -> Arc<Self> {
        Self::build(Some(closed))
    }

    fn build(fixed_closed: Option<bool>) -> Arc<Self> {
        let (inbox_tx, inbox) = mpsc::unbounded_channel();
        let (out_tx, out) = mpsc::unbounded_channel();
        Arc::new(Self {
            inbox_tx,
            inbox: tokio::sync::Mutex::new(inbox),
            out_tx,
            out: tokio::sync::Mutex::new(out),
            sent: Mutex::new(vec![]),
            closed: AtomicBool::new(false),
            closes: AtomicUsize::new(0),
            fixed_closed,
        })
    }

    pub fn page_json(&self, v: Value) {
        let _ = self.inbox_tx.send(Some((OP_TEXT, v.to_string().into_bytes())));
    }

    pub fn page_text(&self, s: &str) {
        let _ = self.inbox_tx.send(Some((OP_TEXT, s.as_bytes().to_vec())));
    }

    pub fn page_bytes(&self, b: Vec<u8>) {
        let _ = self.inbox_tx.send(Some((OP_BIN, b)));
    }

    pub fn page_close(&self) {
        let _ = self.inbox_tx.send(None);
    }

    pub fn command(&self, kind: &str) {
        self.page_json(json!({"type": kind}));
    }

    pub fn sent(&self) -> Vec<Msg> {
        self.sent.lock().clone()
    }

    pub fn sent_json(&self) -> Vec<Value> {
        self.sent().into_iter().filter_map(|m| m.json().cloned()).collect()
    }

    pub async fn next_out(&self) -> Msg {
        tokio::time::timeout(Duration::from_secs(5), self.out.lock().await.recv())
            .await
            .expect("a message from the call")
            .expect("the socket's outbox")
    }

    pub async fn until(&self, pred: impl Fn(&Msg) -> bool) -> Vec<Msg> {
        let mut got = vec![];
        loop {
            let m = self.next_out().await;
            let done = pred(&m);
            got.push(m);
            if done {
                return got;
            }
        }
    }
}

#[async_trait]
impl CallSocket for MemorySocket {
    async fn recv(&self) -> Result<(u8, Vec<u8>), Closed> {
        match self.inbox.lock().await.recv().await {
            Some(Some(item)) => Ok(item),
            _ => Err(Closed("closed".into())),
        }
    }

    async fn send_json(&self, obj: &Value) -> Result<(), Closed> {
        if self.fixed_closed.is_none() && self.closed() {
            return Err(Closed("closed".into()));
        }
        self.sent.lock().push(Msg::Json(obj.clone()));
        let _ = self.out_tx.send(Msg::Json(obj.clone()));
        Ok(())
    }

    async fn send_bytes(&self, data: &[u8]) -> Result<(), Closed> {
        if self.fixed_closed.is_none() && self.closed() {
            return Err(Closed("closed".into()));
        }
        self.sent.lock().push(Msg::Bin(data.to_vec()));
        let _ = self.out_tx.send(Msg::Bin(data.to_vec()));
        Ok(())
    }

    async fn close(&self, _code: u16) {
        self.closes.fetch_add(1, Ordering::SeqCst);
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.inbox_tx.send(None); // its own close ends its recv
    }

    fn closed(&self) -> bool {
        self.fixed_closed.unwrap_or_else(|| self.closed.load(Ordering::SeqCst))
    }
}

// ---- the loopback server and the phone's WebSocket ------------------------------------------------------------------

/// The Mini App server's part in a call: read the request, check it is an upgrade, hand the stream to the endpoint.
/// `/api/stick` is Eve Link's door (its token), anything else the Mini App's (initData).
pub async fn serve(calls: Arc<PhoneCalls>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let calls = calls.clone();
            tokio::spawn(async move {
                let (read, write) = tokio::io::split(stream);
                let mut reader = BufReader::new(read);
                let mut request = String::new();
                if reader.read_line(&mut request).await.is_err() {
                    return;
                }
                let mut headers = HashMap::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        headers.insert(k.trim().to_lowercase(), v.trim().to_string());
                    }
                }
                assert!(is_upgrade(&headers), "not an upgrade: {headers:?}");
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let token: Option<TokenCheck> =
                    if path == "/api/stick" { Some(Arc::new(|t: &str| t == LINK)) } else { None };
                let _ = calls.serve(reader, write, &headers, token).await;
            });
        }
    });
    format!("ws://127.0.0.1:{port}")
}

/// The phone: a WebSocket client whose frames are read in the background, as the browser's are.
pub struct Phone {
    sink: tokio::sync::Mutex<
        futures::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
            Message,
        >,
    >,
    inbox: tokio::sync::Mutex<mpsc::UnboundedReceiver<Msg>>,
    pub ended: Arc<AtomicBool>,
}

impl Phone {
    pub async fn connect(url: &str) -> Phone {
        let (ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
        let (sink, mut stream) = ws.split();
        let (tx, inbox) = mpsc::unbounded_channel();
        let ended = Arc::new(AtomicBool::new(false));
        let flag = ended.clone();
        tokio::spawn(async move {
            while let Some(Ok(m)) = stream.next().await {
                match m {
                    Message::Text(t) => {
                        let _ = tx.send(Msg::Json(serde_json::from_str(t.as_str()).expect("json")));
                    }
                    Message::Binary(b) => {
                        let _ = tx.send(Msg::Bin(b.to_vec()));
                    }
                    _ => {}
                }
            }
            flag.store(true, Ordering::SeqCst);
        });
        Phone { sink: tokio::sync::Mutex::new(sink), inbox: tokio::sync::Mutex::new(inbox), ended }
    }

    pub async fn send_json(&self, v: Value) {
        self.send_text(&v.to_string()).await
    }

    pub async fn send_text(&self, s: &str) {
        let _ = self.sink.lock().await.send(Message::text(s.to_string())).await;
    }

    pub async fn send_bytes(&self, b: Vec<u8>) {
        let _ = self.sink.lock().await.send(Message::binary(b)).await;
    }

    pub async fn hello_owner(&self) {
        self.send_json(json!({"initData": SIGNED})).await;
    }

    /// The next message, or None when the connection ended (or nothing came within `secs`).
    pub async fn recv_within(&self, secs: f64) -> Option<Msg> {
        tokio::time::timeout(Duration::from_secs_f64(secs), self.inbox.lock().await.recv()).await.ok().flatten()
    }

    pub async fn until(&self, pred: impl Fn(&Msg) -> bool) -> Vec<Msg> {
        let mut got = vec![];
        loop {
            let m = self.recv_within(5.0).await.unwrap_or_else(|| panic!("no message; so far {got:?}"));
            let done = pred(&m);
            got.push(m);
            if done {
                return got;
            }
        }
    }

    /// One press: `talk`, `secs` of audio in 100 ms blocks, `done`.
    pub async fn press(&self, secs: f64) {
        self.send_json(json!({"type": "talk"})).await;
        for _ in 0..(secs * 10.0) as usize {
            self.send_bytes(vec![0u8; 4800]).await;
        }
        self.send_json(json!({"type": "done"})).await;
    }

    pub async fn close(&self) {
        let _ = self.sink.lock().await.send(Message::Close(None)).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while !self.ended.load(Ordering::SeqCst) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

pub fn is_state(state: &'static str) -> impl Fn(&Msg) -> bool {
    move |m: &Msg| m.state() == Some(state)
}

pub fn is_kind(kind: &'static str) -> impl Fn(&Msg) -> bool {
    move |m: &Msg| m.is(kind)
}

pub fn states(got: &[Msg]) -> Vec<String> {
    got.iter().filter_map(|m| m.state().map(String::from)).collect()
}

pub fn audio_bytes(got: &[Msg]) -> usize {
    got.iter().map(|m| if let Msg::Bin(b) = m { b.len() } else { 0 }).sum()
}

pub fn texts_of(got: &[Msg], kind: &str) -> Vec<String> {
    got.iter()
        .filter(|m| m.is(kind))
        .filter_map(|m| m.json().and_then(|v| v.get("text")).and_then(Value::as_str).map(String::from))
        .collect()
}

