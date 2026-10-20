//! The call channel: PhoneCalls' socket ([`RtcCallSocket`]).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::warn;

use eve_phone_call::{CallSocket, Closed, OP_BIN, OP_TEXT};

use crate::channel::{Channel, ChannelEvent, Data};
use crate::voice::VoiceTrack;
use crate::{text, BLOCK_BYTES, MAX_CONTROL_CHARS, MAX_PRESS_BYTES, POSTROLL_SECS, PREROLL_BYTES};

/// Told once when a call socket closes, by either side.
pub type OnClose = Arc<dyn Fn(&RtcCallSocket) + Send + Sync>;

/// One message for the call: its opcode (OP_TEXT or OP_BIN) and its bytes.
type Message = (u8, Vec<u8>);

struct Press {
    talking: bool,
    /// the last PREROLL_BYTES heard while not talking
    ring: Vec<u8>,
    /// the press's audio not yet a whole block
    block: Vec<u8>,
    press_bytes: usize,
    done: Option<Vec<u8>>,
    post: Option<JoinHandle<()>>,
}

struct CallInner {
    channel: Arc<dyn Channel>,
    voice: Arc<VoiceTrack>,
    on_close: Option<OnClose>,
    postroll_secs: f64,
    closed: AtomicBool,
    hello: Mutex<Option<Vec<u8>>>,
    /// what the call reads next; None is the end
    inbox: Mutex<VecDeque<Option<Message>>>,
    arrived: Notify,
    eof: AtomicBool,
    press: Mutex<Press>,
}

fn closed_error() -> Closed {
    Closed("the call channel closed".into())
}

/// One call on a peer: the `call` data channel and the peer's audio, as the WebSocket that PhoneCalls.serve_socket
/// drives (`recv`, `send_json`, `send_bytes`, `close`, `closed`). Clones share the socket.
///
/// * `recv`: first the hello, built here (`{"token": <the peer's secret>, "can": []}`); then the page's messages
///   (`talk`, `done`, `end`, `ping`) as text, and the microphone ([`RtcCallSocket::audio`], from the peer's track) as
///   [`BLOCK_BYTES`] binary blocks, only between `talk` and `done`. The [`PREROLL_BYTES`] heard just before a `talk`
///   follow it; a `done` waits [`POSTROLL_SECS`] with the audio still arriving, then the press's last part block,
///   then the `done`.
/// * `send_json`: `said` waits in eve's voice queue as a marker ([`VoiceTrack::mark`]) and goes out when its sentence
///   starts to play; `flush` empties that queue and goes out at once; everything else (`heard`, `state`, `ended`)
///   goes out at once.
/// * `send_bytes`: eve's speech, queued on the peer's VoiceTrack.
#[derive(Clone)]
pub struct RtcCallSocket {
    inner: Arc<CallInner>,
}

impl RtcCallSocket {
    pub fn new(channel: Arc<dyn Channel>, voice: Arc<VoiceTrack>, token: &str) -> Self {
        Self::with(channel, voice, token, None, POSTROLL_SECS)
    }

    pub fn with(
        channel: Arc<dyn Channel>,
        voice: Arc<VoiceTrack>,
        token: &str,
        on_close: Option<OnClose>,
        postroll_secs: f64,
    ) -> Self {
        let hello = text(&json!({"token": token, "can": []})).into_bytes();
        let sock = RtcCallSocket {
            inner: Arc::new(CallInner {
                channel: channel.clone(),
                voice,
                on_close,
                postroll_secs,
                closed: AtomicBool::new(false),
                hello: Mutex::new(Some(hello)),
                inbox: Mutex::new(VecDeque::new()),
                arrived: Notify::new(),
                eof: AtomicBool::new(false),
                press: Mutex::new(Press {
                    talking: false,
                    ring: Vec::new(),
                    block: Vec::new(),
                    press_bytes: 0,
                    done: None,
                    post: None,
                }),
            }),
        };
        let weak = Arc::downgrade(&sock.inner);
        channel.on(Arc::new(move |event| {
            let Some(inner) = weak.upgrade() else { return };
            let sock = RtcCallSocket { inner };
            match event {
                ChannelEvent::Message(m) => sock.on_message(m),
                ChannelEvent::Close => sock.on_channel_close(),
                ChannelEvent::BufferedAmountLow => {}
            }
        }));
        sock
    }

    /// The same socket.
    pub fn same(&self, other: &RtcCallSocket) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn channel(&self) -> &Arc<dyn Channel> {
        &self.inner.channel
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// A press is held.
    pub fn talking(&self) -> bool {
        self.inner.press.lock().talking
    }

    fn weak(&self) -> Weak<CallInner> {
        Arc::downgrade(&self.inner)
    }

    // -- the WebSocket PhoneCalls drives --

    async fn next(&self) -> Result<(u8, Vec<u8>), Closed> {
        if let Some(hello) = self.inner.hello.lock().take() {
            return Ok((OP_TEXT, hello));
        }
        if self.inner.eof.load(Ordering::SeqCst) {
            return Err(closed_error());
        }
        loop {
            let arrived = self.inner.arrived.notified();
            tokio::pin!(arrived);
            arrived.as_mut().enable();
            let item = self.inner.inbox.lock().pop_front();
            match item {
                Some(Some(item)) => return Ok(item),
                Some(None) => {
                    self.inner.eof.store(true, Ordering::SeqCst);
                    return Err(closed_error());
                }
                None => arrived.await,
            }
        }
    }

    async fn send_obj(&self, obj: &Value) -> Result<(), Closed> {
        if self.is_closed() {
            return Err(closed_error());
        }
        let kind = obj.get("type").and_then(Value::as_str);
        let line = text(obj);
        if kind == Some("said") {
            let weak = self.weak();
            self.inner.voice.mark(Box::new(move || {
                // with its sentence's first sound
                if let Some(inner) = weak.upgrade() {
                    RtcCallSocket { inner }.send_text(line);
                }
                Ok(())
            }));
            return Ok(());
        }
        if kind == Some("flush") {
            self.inner.voice.flush();
        }
        if !self.send_text(line) {
            return Err(closed_error());
        }
        Ok(())
    }

    // -- the page's side --

    /// Microphone PCM from the peer's track (24 kHz mono 16-bit): the press's while talking, else the ring.
    pub fn audio(&self, pcm: &[u8]) {
        if self.is_closed() || pcm.is_empty() {
            return;
        }
        let mut blocks: Vec<Vec<u8>> = Vec::new();
        {
            let mut p = self.inner.press.lock();
            if !p.talking {
                p.ring.extend_from_slice(pcm);
                let over = p.ring.len().saturating_sub(PREROLL_BYTES);
                if over > 0 {
                    p.ring.drain(..over);
                }
                return;
            }
            p.block.extend_from_slice(pcm);
            while p.block.len() >= BLOCK_BYTES {
                let block: Vec<u8> = p.block.drain(..BLOCK_BYTES).collect();
                if p.press_bytes < MAX_PRESS_BYTES {
                    // the call keeps MAX_PRESS_SECS of a press; so does this
                    p.press_bytes += BLOCK_BYTES;
                    blocks.push(block);
                }
            }
            // queued under the press's lock, so blocks keep their order against a done released meanwhile
            for block in blocks {
                self.put(OP_BIN, block);
            }
        }
    }

    fn on_message(&self, message: Data) {
        let run = std::panic::AssertUnwindSafe(|| self.message(message));
        if std::panic::catch_unwind(run).is_err() {
            // never into the stack's receive loop
            warn!("lite rtc: a call message failed (Exception)");
        }
    }

    fn message(&self, message: Data) {
        let Data::Text(message) = message else { return }; // the microphone is the audio track, never a message
        if self.is_closed() || message.chars().count() > MAX_CONTROL_CHARS {
            return;
        }
        let raw = message.as_bytes().to_vec();
        let msg: Option<Value> = serde_json::from_str(&message).ok();
        let kind = msg.as_ref().and_then(Value::as_object).and_then(|m| m.get("type")).and_then(Value::as_str);
        match kind {
            Some("talk") => {
                self.release(); // a done still in its post-roll is the last press's end
                self.put(OP_TEXT, raw);
                let pre = {
                    let mut p = self.inner.press.lock();
                    if p.talking {
                        None
                    } else {
                        p.talking = true;
                        p.press_bytes = 0;
                        Some(std::mem::take(&mut p.ring))
                    }
                };
                if let Some(pre) = pre {
                    self.audio(&pre); // the pre-roll: what was heard just before the press
                }
            }
            Some("done") => {
                let mut p = self.inner.press.lock();
                if !p.talking {
                    drop(p);
                    self.put(OP_TEXT, raw); // a stray done: the call ignores it, as before
                } else if p.post.is_none() {
                    p.done = Some(raw);
                    let weak = self.weak();
                    let secs = self.inner.postroll_secs;
                    p.post = Some(tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs_f64(secs.max(0.0))).await;
                        if let Some(inner) = weak.upgrade() {
                            RtcCallSocket { inner }.release_from_timer();
                        }
                    }));
                }
            }
            Some("end") => {
                self.release();
                self.put(OP_TEXT, raw);
            }
            _ => self.put(OP_TEXT, raw), // ping and anything else, as the page sent it
        }
    }

    fn release_from_timer(&self) {
        let mut p = self.inner.press.lock();
        if p.post.is_none() {
            return;
        }
        p.post = None; // this is the timer itself: nothing to cancel
        self.released(&mut p);
    }

    /// The post-roll is over (or cut short): the press's last part block, then its done.
    fn release(&self) {
        let mut p = self.inner.press.lock();
        let Some(post) = p.post.take() else { return };
        post.abort();
        self.released(&mut p);
    }

    fn released(&self, p: &mut Press) {
        let keep = p.block.len() - p.block.len() % 2;
        let tail: Vec<u8> = p.block[..keep].to_vec();
        p.block.clear();
        if !tail.is_empty() && p.press_bytes < MAX_PRESS_BYTES {
            self.put(OP_BIN, tail);
        }
        p.talking = false;
        if let Some(done) = p.done.take() {
            self.put(OP_TEXT, done);
        }
    }

    fn put(&self, op: u8, data: Vec<u8>) {
        if !self.is_closed() {
            self.inner.inbox.lock().push_back(Some((op, data)));
            self.inner.arrived.notify_one();
        }
    }

    fn send_text(&self, line: String) -> bool {
        !self.is_closed() && crate::send(self.inner.channel.as_ref(), Data::Text(line))
    }

    // -- the end --

    /// This side ends the call's socket: what the page sent and the call did not read yet is dropped.
    pub fn shut(&self) {
        if self.end(false) {
            self.inner.channel.close();
        }
    }

    fn on_channel_close(&self) {
        self.end(true); // the page closed it: what it sent first is still read
    }

    fn end(&self, keep: bool) -> bool {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return false;
        }
        if let Some(post) = self.inner.press.lock().post.take() {
            post.abort();
        }
        self.inner.voice.flush();
        {
            let mut inbox = self.inner.inbox.lock();
            if !keep {
                inbox.clear();
            }
            inbox.push_back(None);
        }
        self.inner.arrived.notify_one();
        if let Some(on_close) = &self.inner.on_close {
            on_close(self);
        }
        true
    }
}

#[async_trait]
impl CallSocket for RtcCallSocket {
    async fn recv(&self) -> Result<(u8, Vec<u8>), Closed> {
        self.next().await
    }

    async fn send_json(&self, obj: &Value) -> Result<(), Closed> {
        self.send_obj(obj).await
    }

    async fn send_bytes(&self, data: &[u8]) -> Result<(), Closed> {
        if self.is_closed() {
            return Err(closed_error());
        }
        self.inner.voice.push(data);
        Ok(())
    }

    async fn close(&self, _code: u16) {
        self.shut();
    }

    fn closed(&self) -> bool {
        self.is_closed()
    }
}
