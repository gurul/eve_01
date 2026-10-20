//! `phone_call.py`: calling eve from the phone, push to talk, with everything texting can do.
//!
//! The owner asked on 2026-09-25 for voice calls with eve over Telegram, then for push to talk "instead of live",
//! and for "full functionality like i am texting it". A Telegram bot cannot take a Telegram call, so the call lives
//! in eve's Mini App (miniapp.py): **Call eve** opens the microphone in Telegram's in-app browser, and a WebSocket
//! carries it to the daemon through the same Cloudflare tunnel the Mini App already uses.
//!
//! * **It is the chat, spoken.** Each press of the button is one message to the same brain the owner texts
//!   (telegram.py): the audio is transcribed and handed to the chat exactly as if it had been typed, so every tool
//!   the chat has works on a call. Everything eve then says in the chat is also spoken on the call, and the chat
//!   keeps the whole call in writing.
//! * **Push to talk.** The phone sends audio only while the button is held (`talk`, the audio, `done`). A press
//!   while eve is speaking interrupts it: the phone drops what it was playing and eve stops reading.
//! * **The wire.** `GET /api/call` upgrades to a WebSocket (RFC 6455, written here: the Mini App server is a small
//!   hand-written HTTP server). Audio is 24 kHz mono 16-bit PCM both ways, in binary frames. Text frames carry
//!   JSON: the phone's first message is its Telegram `initData`; then `talk`, `done` and `end`; eve sends `state`,
//!   `heard` (what it transcribed), `flush` (drop the queued audio) and `ended`.
//! * **Only the owner.** Nothing starts until the first message carries initData signed by this bot for an owner
//!   (miniapp.check_init_data), within [`AUTH_SECS`]. One call at a time.
//! * **Eyes.** On an Eve Link call whose hello lists `"eyes"`, what the owner said and each sentence eve reads out
//!   go to `PhoneCalls.on_text` (the daemon's expression picker), and a pick reaches the phone as
//!   `{"type": "expression", "id", "label", "ttl_ms"}` (or `{"type": "expression", "id", "clear": true}`). A
//!   sentence is offered when the phone starts to play it, not when its audio is made (speech is made faster than
//!   it plays), and its pick carries the sentence as `"said"`. Nothing is sent to any other call, or while the owner
//!   holds the button.
//! * **Notes on the go.** (owner, 2026-10-04: "when i say start taking notes the phone should hold the port until i
//!   click action button to turn it off".) On an Eve Link call whose hello says it can (`"can": ["notes"]`),
//!   take_notes records the phone's microphone instead of the desk's ([`PhoneNotes`]): the call stops being push to
//!   talk, every audio frame goes to the notes, and the next press of the Action Button (`notes_stop`) writes them
//!   up, texts them to the owner and says "Notes saved." A call that drops keeps what was heard, and the same phone
//!   calling back within [`NOTES_GRACE_SECS`] carries on.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use futures::future::{BoxFuture, FutureExt, Shared};
use futures::{SinkExt, StreamExt};
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::{AbortHandle, JoinHandle};
use tracing::{info, warn};

use eve_notes::{NotesSdk, PushedAudio, Row};
use eve_openrouter::Part;
use eve_spend::{Ledger, RecordOpts};

use crate::py;

pub const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// What the phone sends and OpenAI's "pcm" speech format is.
pub const SAMPLE_RATE: u32 = 24000;
pub const BYTES_PER_SEC: usize = SAMPLE_RATE as usize * 2;
/// The first message (initData) must come this soon.
pub const AUTH_SECS: f64 = 10.0;
/// A 100 ms block is 4800 bytes; anything this big is not audio.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
/// One press holds at most this much audio; the rest is dropped.
pub const MAX_PRESS_SECS: f64 = 120.0;
/// Shorter than this is a tap, not words.
pub const MIN_PRESS_SECS: f64 = 0.35;
/// A call this long is ended.
pub const MAX_CALL_SECS: f64 = 60.0 * 60.0;
/// Nothing from the phone this long (it sends a ping every 10 s): it is gone.
pub const QUIET_SECS: f64 = 30.0;
pub const MAX_SPOKEN_CHARS: usize = 1500;
/// A call holds the chat (its listener) from its first press, not from when it opens, and an idle one gives it back
/// to the desk's button. An open call is often an unused one (Eve Link keeps it up for 45 s after a turn): on
/// 2026-09-30 one blocked the Voice PE ("desk call: the phone is on a call; the button waits") and would have read
/// the owner's texted replies out on the phone. Idle: no press, transcription, reply or speech for this long.
pub const IDLE_YIELD_SECS: f64 = 8.0;
/// The call's ears (owner, 2026-09-30: "transcription is terrible"). The full model, not the mini;
/// EVE_CALL_STT_MODEL overrides it.
pub const DEFAULT_STT_MODEL: &str = "gpt-4o-transcribe";
/// Push-to-talk already supplies the turn boundary. Keep each press together, including its pauses, instead of
/// asking server VAD to create additional turns. Near-field reduction is for a microphone held to the mouth.
pub const LIVE_NOISE_REDUCTION: &str = "near_field";
/// The live ears ([`LiveEars`]) are OpenAI's realtime transcription WebSocket, which OpenRouter does not carry: an
/// opt-in that needs this key (EVE_CALL_LIVE_STT=1). Off by default since 2026-10-08 (docs/openrouter.md).
pub const LIVE_EARS_KEY_ENV: &str = "OPENAI_API_KEY";
/// The language the owner speaks, ISO-639-1. Left to guess, the model heard "Spotify mode" as Chinese on a short
/// Voice PE press and eve answered in Chinese (2026-09-27). EVE_CALL_LANGUAGE sets it; an empty value lets the
/// model guess again.
pub const DEFAULT_LANGUAGE: &str = "en";
/// Even told "en", the transcriber can answer near-silence with a syllable in another script (2026-10-08; owner:
/// "english only remember"). With English set, words whose letters are mostly not Latin are treated as not caught.
pub const LATIN_LANGUAGES: &[&str] = &["en"];

/// The call's voice, on OpenRouter (owner, 2026-09-25). deepgram/aura-2 had the first sound in 0.31 s, the fastest
/// measured. Since the owner moved every model call to his OpenRouter key (docs/openrouter.md), the voice is
/// OpenRouter's alone and the ears upload each press to OpenRouter's /audio/transcriptions.
pub const OPENROUTER_SPEECH_URL: &str = "https://openrouter.ai/api/v1/audio/speech";
pub const DEFAULT_OR_TTS_MODEL: &str = "deepgram/aura-2";
pub const DEFAULT_OR_TTS_VOICE: &str = "aura-2-thalia-en";
/// deepgram/aura-2's listed price on OpenRouter (/api/v1/models, 2026-09-25).
pub const OR_TTS_USD_PER_CHAR: f64 = 0.00003;
/// OpenAI's realtime endpoint, as the SDK builds it for `realtime.connect(extra_query={"intent": "transcription"})`.
pub const OPENAI_REALTIME_URL: &str = "wss://api.openai.com/v1/realtime?intent=transcription";

/// Notes on the phone ([`PhoneNotes`]). A call that drops while they are on is waited for this long before the
/// notes are written up with what was heard: long enough for the phone to change networks or come back from a lift,
/// short enough that the owner who walked away still gets their notes the same minute.
pub const NOTES_GRACE_SECS: f64 = 60.0;
/// The capability an Eve Link hello lists to be able to take notes.
pub const NOTES_CAN: &str = "notes";
/// Eve's expressions in the phone's eyes (owner, 2026-10-12: "route the expressions into the eyes").
pub const EYES_CAN: &str = "eyes";
/// Picks' sentences remembered for their `said` (a pick is sent within 4 s of its offer).
pub const EYES_SAID_KEEP: usize = 32;
/// The longest a sentence's offer waits for the audio queued before it to play.
pub const EYES_MAX_WAIT_SECS: f64 = 60.0;
/// Why a call ended. "you hung up" is the End button (`end`): with notes on it means stop and write them up. A
/// connection that broke is not the owner hanging up, and gives the phone the grace to call back.
pub const HUNG_UP: &str = "you hung up";
/// A stop waits this long for the write-up (notes.py's own waits, and the text).
pub const SUMMARY_WAIT_SECS: f64 = 240.0;
pub const DROPPED: &str = "the connection dropped";
pub const QUIET: &str = "the phone went quiet";
/// The daemon stopping while the phone takes notes: the session is written up with no summary, within this many
/// seconds, so the words are kept.
pub const NOTES_SHUTDOWN_SECS: f64 = 30.0;

pub const OP_CONT: u8 = 0x0;
pub const OP_TEXT: u8 = 0x1;
pub const OP_BIN: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xA;

/// One Telegram message, with room under the Bot API's 4096 for the title and markup.
pub const NOTES_PIECE_CHARS: usize = 3500;
/// Past this the notes point to the file, which has everything.
pub const NOTES_MAX_PIECES: usize = 8;

const NOTES_STOP_HINT: &str = "press the Action Button, or say stop taking notes; the notes are texted when they stop";

/// A monotonic clock in seconds (`time.monotonic`).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

fn default_clock() -> Clock {
    Arc::new(eve_notes::monotonic)
}

fn wall_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn secs(x: f64) -> Duration {
    Duration::from_secs_f64(if x.is_finite() && x > 0.0 { x } else { 0.0 })
}

fn same<T: ?Sized>(a: &Arc<T>, b: &Arc<T>) -> bool {
    std::ptr::eq(Arc::as_ptr(a) as *const (), Arc::as_ptr(b) as *const ())
}

// ---- errors -------------------------------------------------------------------------------------------------------

/// The other side closed the WebSocket, or it broke.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct Closed(pub String);

/// The live ears' failures, by the Python exception each one is.
#[derive(Debug, thiserror::Error)]
pub enum EarsError {
    /// `ConnectionError`
    #[error("{0}")]
    Connection(String),
    /// `TimeoutError` (`asyncio.timeout`)
    #[error("timed out")]
    Timeout,
}

/// `type(e).__name__` for the log lines (the type only: an error can quote what was said).
pub fn error_name(e: &anyhow::Error) -> String {
    for cause in e.chain() {
        if let Some(x) = cause.downcast_ref::<EarsError>() {
            return match x {
                EarsError::Connection(_) => "ConnectionError",
                EarsError::Timeout => "TimeoutError",
            }
            .into();
        }
        if cause.downcast_ref::<Closed>().is_some() {
            return "Closed".into();
        }
        if cause.downcast_ref::<tokio::time::error::Elapsed>().is_some() {
            return "TimeoutError".into();
        }
    }
    eve_records::py::error_name(e)
}

fn io_name(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        UnexpectedEof => "IncompleteReadError",
        ConnectionReset => "ConnectionResetError",
        ConnectionAborted => "ConnectionAbortedError",
        ConnectionRefused => "ConnectionRefusedError",
        BrokenPipe => "BrokenPipeError",
        _ => "ConnectionError",
    }
    .into()
}

// ---- the WebSocket ------------------------------------------------------------------------------------------------

/// The WebSocket handshake's answer: base64 of SHA-1 over the key and the GUID.
pub fn accept_key(key: &str) -> String {
    let mut h = Sha1::new();
    h.update(key.as_bytes());
    h.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(h.finalize())
}

/// Whether a request's headers (lower-cased names) ask for a WebSocket.
pub fn is_upgrade(headers: &HashMap<String, String>) -> bool {
    let get = |k: &str| headers.get(k).map(String::as_str).unwrap_or("");
    get("upgrade").to_lowercase().contains("websocket")
        && get("connection").to_lowercase().contains("upgrade")
        && !get("sec-websocket-key").is_empty()
}

/// One unmasked, final frame (a server never masks).
pub fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let n = payload.len();
    let mut out = Vec::with_capacity(n + 10);
    out.push(0x80 | opcode);
    if n < 126 {
        out.push(n as u8);
    } else if n < 1 << 16 {
        out.push(126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

/// What a call runs on: the WebSocket here, eve lite's WebRTC channel (lite_rtc.RtcCallSocket), or the desk's
/// board link (desk_call.DeskLink). `recv` gives the next text or binary message; a close raises [`Closed`].
#[async_trait]
pub trait CallSocket: Send + Sync {
    async fn recv(&self) -> Result<(u8, Vec<u8>), Closed>;
    async fn send_json(&self, obj: &Value) -> Result<(), Closed>;
    async fn send_bytes(&self, data: &[u8]) -> Result<(), Closed>;
    async fn close(&self, code: u16);
    fn closed(&self) -> bool;
}

type Outgoing = (Vec<u8>, oneshot::Sender<std::io::Result<()>>);

/// The server side of one WebSocket, over a stream the HTTP server already read the request from.
///
/// Frames are written by one writer task, whole, in the order they were sent: a send that is cancelled while it
/// waits (a barge-in cancelling the reader mid-sentence) still leaves a whole frame on the wire, as asyncio's
/// buffered `writer.write` does.
pub struct WebSocket {
    reader: tokio::sync::Mutex<Box<dyn AsyncRead + Send + Unpin>>,
    out: mpsc::UnboundedSender<Outgoing>,
    closed: AtomicBool,
}

impl WebSocket {
    /// A WebSocket over an already upgraded stream. Must be called inside a tokio runtime.
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<Outgoing>();
        let mut writer = writer;
        tokio::spawn(async move {
            while let Some((buf, ack)) = rx.recv().await {
                let result = async {
                    writer.write_all(&buf).await?;
                    writer.flush().await
                }
                .await;
                let _ = ack.send(result);
            }
            let _ = writer.shutdown().await;
        });
        Self { reader: tokio::sync::Mutex::new(Box::new(reader)), out: tx, closed: AtomicBool::new(false) }
    }

    /// Answer the upgrade (101 Switching Protocols) and hand back the socket.
    pub async fn accept<R, W>(reader: R, writer: W, headers: &HashMap<String, String>) -> anyhow::Result<Self>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let key = headers.get("sec-websocket-key").ok_or_else(|| anyhow::anyhow!("KeyError: 'sec-websocket-key'"))?;
        let ws = Self::new(reader, writer);
        let head = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
            accept_key(key)
        );
        ws.write(head.into_bytes()).await?;
        Ok(ws)
    }

    async fn write(&self, buf: Vec<u8>) -> Result<(), Closed> {
        let (ack, done) = oneshot::channel();
        if self.out.send((buf, ack)).is_err() {
            self.closed.store(true, Ordering::SeqCst);
            return Err(Closed("BrokenPipeError".into()));
        }
        match done.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                self.closed.store(true, Ordering::SeqCst);
                Err(Closed(io_name(&e)))
            }
            Err(_) => {
                self.closed.store(true, Ordering::SeqCst);
                Err(Closed("ConnectionError".into()))
            }
        }
    }

    async fn read_frame(&self, reader: &mut (dyn AsyncRead + Send + Unpin)) -> Result<(bool, u8, Vec<u8>), Closed> {
        let wrap = |e: std::io::Error| Closed(io_name(&e));
        let mut head = [0u8; 2];
        reader.read_exact(&mut head).await.map_err(wrap)?;
        let (b1, b2) = (head[0], head[1]);
        let mut n = (b2 & 0x7F) as u64;
        if n == 126 {
            let mut ext = [0u8; 2];
            reader.read_exact(&mut ext).await.map_err(wrap)?;
            n = u16::from_be_bytes(ext) as u64;
        } else if n == 127 {
            let mut ext = [0u8; 8];
            reader.read_exact(&mut ext).await.map_err(wrap)?;
            n = u64::from_be_bytes(ext);
        }
        if n > MAX_FRAME_BYTES as u64 {
            return Err(Closed("frame too large".into()));
        }
        if b2 & 0x80 == 0 {
            return Err(Closed("a client frame must be masked".into()));
        }
        let mut mask = [0u8; 4];
        reader.read_exact(&mut mask).await.map_err(wrap)?;
        let mut data = vec![0u8; n as usize];
        reader.read_exact(&mut data).await.map_err(wrap)?;
        // unmask the whole payload: XOR with the 4-byte mask repeated to its length
        for (i, b) in data.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
        Ok((b1 & 0x80 != 0, b1 & 0x0F, data))
    }

    /// The next text or binary message (fragments joined). Pings are answered; a close raises Closed.
    pub async fn recv(&self) -> Result<(u8, Vec<u8>), Closed> {
        let mut reader = self.reader.lock().await;
        let mut parts: Vec<Vec<u8>> = Vec::new();
        let mut first = 0u8;
        loop {
            let (fin, op, payload) = self.read_frame(&mut **reader).await?;
            if op == OP_PING {
                self.send(OP_PONG, &payload).await?;
                continue;
            }
            if op == OP_PONG {
                continue;
            }
            if op == OP_CLOSE {
                let _ = self.send(OP_CLOSE, &payload[..payload.len().min(2)]).await;
                self.closed.store(true, Ordering::SeqCst);
                return Err(Closed("closed by the phone".into()));
            }
            if op == OP_TEXT || op == OP_BIN {
                first = op;
                parts = vec![payload];
            } else if op == OP_CONT && !parts.is_empty() {
                parts.push(payload);
            } else {
                return Err(Closed("bad frame".into()));
            }
            if parts.iter().map(Vec::len).sum::<usize>() > MAX_FRAME_BYTES {
                return Err(Closed("message too large".into()));
            }
            if fin {
                return Ok((first, parts.concat()));
            }
        }
    }

    async fn send(&self, opcode: u8, payload: &[u8]) -> Result<(), Closed> {
        if self.closed.load(Ordering::SeqCst) && opcode != OP_CLOSE {
            return Err(Closed("already closed".into()));
        }
        self.write(frame(opcode, payload)).await
    }

    pub async fn send_bytes(&self, data: &[u8]) -> Result<(), Closed> {
        self.send(OP_BIN, data).await
    }

    pub async fn send_json(&self, obj: &Value) -> Result<(), Closed> {
        self.send(OP_TEXT, py::dumps(obj).as_bytes()).await
    }

    pub async fn close(&self, code: u16) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.send(OP_CLOSE, &code.to_be_bytes()).await;
        self.closed.store(true, Ordering::SeqCst);
    }

    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CallSocket for WebSocket {
    async fn recv(&self) -> Result<(u8, Vec<u8>), Closed> {
        WebSocket::recv(self).await
    }
    async fn send_json(&self, obj: &Value) -> Result<(), Closed> {
        WebSocket::send_json(self, obj).await
    }
    async fn send_bytes(&self, data: &[u8]) -> Result<(), Closed> {
        WebSocket::send_bytes(self, data).await
    }
    async fn close(&self, code: u16) {
        WebSocket::close(self, code).await
    }
    fn closed(&self) -> bool {
        WebSocket::closed(self)
    }
}

// ---- the speech on both ends ----------------------------------------------------------------------------------------

static MARKS: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"[*_`#>|~]+|\[([^\]]*)\]\([^)]*\)").expect("marks"));
// Python's `\s` is `str.isspace`, which also takes U+001C..U+001F; Rust's `\s` (White_Space) does not.
static URL: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"https?://[^\s\x1c-\x1f]+").expect("url"));
static BULLET: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"(?m)^[\s\x1c-\x1f]*[-•][\s\x1c-\x1f]+").expect("bullet"));

/// A chat message as words to read out: no Markdown marks, a link read as its words, a bare URL as "a link", and a
/// long message cut at a sentence with a pointer to the chat.
pub fn speakable(text: &str) -> String {
    let t = MARKS.replace_all(text, |c: &regex::Captures| match c.get(1).map(|m| m.as_str()) {
        Some(words) if !words.is_empty() => words.to_string(),
        _ => " ".to_string(),
    });
    let t = URL.replace_all(&t, "a link");
    let t = BULLET.replace_all(&t, "");
    let t = py::split_ws(&t).join(" ");
    if py::len(&t) > MAX_SPOKEN_CHARS {
        let chars: Vec<char> = t.chars().collect();
        // t.rfind(". ", 0, MAX_SPOKEN_CHARS): the last ". " wholly inside the first MAX_SPOKEN_CHARS characters
        let cut = (0..MAX_SPOKEN_CHARS.saturating_sub(1)).rev().find(|&i| chars[i] == '.' && chars[i + 1] == ' ');
        let end = match cut {
            Some(c) if c > 200 => c + 1,
            _ => MAX_SPOKEN_CHARS,
        };
        return chars[..end].iter().collect::<String>() + " The rest is in the chat.";
    }
    t
}

/// `re.split(r"(?<=[.!?])\s+", text)`: split at each run of whitespace that follows a sentence's end.
fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut prev: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if py::is_space(c) && matches!(prev, Some('.' | '!' | '?')) {
            while chars.peek().is_some_and(|n| py::is_space(*n)) {
                chars.next();
            }
            out.push(std::mem::take(&mut current));
            prev = None;
            continue;
        }
        current.push(c);
        prev = Some(c);
    }
    out.push(current);
    out
}

/// Read in sentence-sized pieces, so the first one plays while the next is still being made.
pub fn sentences(text: &str, limit: usize) -> Vec<String> {
    let parts: Vec<String> =
        split_sentences(text).iter().map(|p| py::strip(p).to_string()).filter(|p| !p.is_empty()).collect();
    let mut out: Vec<String> = Vec::new();
    for p in parts {
        if let Some(last) = out.last_mut() {
            if py::len(last) + py::len(&p) < 90 {
                // short sentences ride together: fewer, fuller requests
                last.push(' ');
                last.push_str(&p);
                continue;
            }
        }
        out.push(py::head(&p, limit * 4));
    }
    out
}

/// Whether `text` cannot be what the owner said in `language`: under half its letters are Latin. Only for a
/// Latin-script language ([`LATIN_LANGUAGES`]); "" (the model guesses) or any other language keeps every text.
pub fn foreign(text: &str, language: &str) -> bool {
    if !LATIN_LANGUAGES.contains(&language) {
        return false;
    }
    let letters: Vec<char> = text.chars().filter(|c| py::is_alpha(*c)).collect();
    if letters.is_empty() {
        return false;
    }
    let latin = letters.iter().filter(|c| c.is_ascii() || ('\u{c0}'..='\u{24f}').contains(*c)).count();
    latin * 2 < letters.len()
}

/// The words' audio as it is made (a blocking iterator): the first chunk plays while the rest arrive.
pub type SpeechStream<'a> = Box<dyn Iterator<Item = anyhow::Result<Vec<u8>>> + Send + 'a>;

/// The call's ears and voice. `transcribe` and `speak_stream` block: the call runs them on threads.
pub trait Voice: Send + Sync {
    fn transcribe(&self, pcm: &[u8]) -> anyhow::Result<String>;
    fn speak_stream(&self, text: &str) -> SpeechStream<'_>;
    fn live_ears(&self) -> Option<Arc<dyn Ears>>;
    /// The language the ears are told (`getattr(voice, "language", DEFAULT_LANGUAGE)`).
    fn language(&self) -> String {
        DEFAULT_LANGUAGE.to_string()
    }
}

/// The live ears as the call drives them: one session per call, one press at a time ([`LiveEars`] is the real one).
#[async_trait]
pub trait Ears: Send + Sync {
    async fn open(&self) -> anyhow::Result<()>;
    /// A new press: nothing left over from the last one.
    async fn begin(&self) -> anyhow::Result<()>;
    /// A short tap has no transcript; clear its audio and let the next press begin.
    async fn discard(&self) -> anyhow::Result<()>;
    async fn feed(&self, pcm: &[u8]) -> anyhow::Result<()>;
    /// One release commits one complete turn, then waits for that turn's final transcript.
    async fn finish(&self, timeout: f64) -> anyhow::Result<String>;
    async fn close(&self);
    /// `close` for a task that was cancelled and cannot await: let the session go now.
    fn close_now(&self);
    fn broken(&self) -> bool;
    fn set_broken(&self);
}

// ---- the live ears ------------------------------------------------------------------------------------------------

/// One realtime connection: JSON events out, server events in (`None` once it closed cleanly).
#[async_trait]
pub trait RealtimeConn: Send + Sync {
    async fn send(&self, event: Value) -> anyhow::Result<()>;
    async fn recv(&self) -> Option<anyhow::Result<Value>>;
    /// `__aexit__` of the SDK's connection manager: close the connection.
    fn close(&self);
}

/// Opens a [`RealtimeConn`] (`AsyncOpenAI(api_key=…).realtime.connect(extra_query={"intent": "transcription"})`).
#[async_trait]
pub trait RealtimeConnect: Send + Sync {
    async fn connect(&self, api_key: &str) -> anyhow::Result<Arc<dyn RealtimeConn>>;
}

/// OpenAI's realtime WebSocket, over tokio-tungstenite.
pub struct OpenAIRealtime {
    pub url: String,
}

impl Default for OpenAIRealtime {
    fn default() -> Self {
        Self { url: OPENAI_REALTIME_URL.to_string() }
    }
}

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures::stream::SplitSink<WsStream, tokio_tungstenite::tungstenite::Message>;

struct TungsteniteConn {
    sink: Arc<tokio::sync::Mutex<WsSink>>,
    stream: tokio::sync::Mutex<futures::stream::SplitStream<WsStream>>,
}

#[async_trait]
impl RealtimeConnect for OpenAIRealtime {
    async fn connect(&self, api_key: &str) -> anyhow::Result<Arc<dyn RealtimeConn>> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut req = self.url.as_str().into_client_request()?;
        req.headers_mut().insert("Authorization", format!("Bearer {api_key}").parse()?);
        let (ws, _) = tokio_tungstenite::connect_async(req).await?;
        let (sink, stream) = ws.split();
        Ok(Arc::new(TungsteniteConn {
            sink: Arc::new(tokio::sync::Mutex::new(sink)),
            stream: tokio::sync::Mutex::new(stream),
        }))
    }
}

#[async_trait]
impl RealtimeConn for TungsteniteConn {
    async fn send(&self, event: Value) -> anyhow::Result<()> {
        use tokio_tungstenite::tungstenite::Message;
        self.sink.lock().await.send(Message::text(event.to_string())).await?;
        Ok(())
    }

    async fn recv(&self) -> Option<anyhow::Result<Value>> {
        use tokio_tungstenite::tungstenite::{protocol::frame::coding::CloseCode, Error, Message};
        let mut stream = self.stream.lock().await;
        loop {
            match stream.next().await? {
                Ok(Message::Text(t)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                        return Some(Ok(v));
                    }
                }
                Ok(Message::Close(frame)) => {
                    // the SDK ends its iteration on a normal close (ConnectionClosedOK) and raises on any other
                    let ok = frame.as_ref().is_none_or(|f| matches!(f.code, CloseCode::Normal | CloseCode::Away));
                    return if ok { None } else { Some(Err(anyhow::anyhow!("ConnectionClosedError"))) };
                }
                Ok(_) => {}
                Err(Error::ConnectionClosed | Error::AlreadyClosed) => return None,
                Err(e) => return Some(Err(e.into())),
            }
        }
    }

    fn close(&self) {
        let sink = self.sink.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = sink.lock().await.close().await;
            });
        }
    }
}

/// A set-once flag that can be awaited (`asyncio.Event` that is never cleared).
#[derive(Default)]
struct Flag {
    set: AtomicBool,
    notify: Notify,
}

impl Flag {
    fn set(&self) {
        self.set.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            let n = self.notify.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if self.set.load(Ordering::SeqCst) {
                return;
            }
            n.await;
        }
    }
}

#[derive(Default)]
struct EarsState {
    order: Vec<String>,
    done: HashMap<String, String>,
    commit_resolved: bool,
    awaiting_commit: bool,
    fed: usize,
}

#[derive(Default)]
struct EarsShared {
    st: Mutex<EarsState>,
    broken: AtomicBool,
    changed: Notify,
    configured: Flag,
}

impl EarsShared {
    fn changed(&self) {
        self.changed.notify_waiters();
    }

    fn handle(&self, ev: &Value) {
        let kind = ev.get("type").and_then(Value::as_str).unwrap_or("");
        let item = || ev.get("item_id").and_then(Value::as_str).unwrap_or("").to_string();
        match kind {
            "session.updated" => self.configured.set(),
            "input_audio_buffer.committed" => {
                let mut st = self.st.lock();
                if !st.awaiting_commit {
                    drop(st);
                    self.broken.store(true, Ordering::SeqCst); // an unexpected turn must never become the owner's words
                    self.changed();
                    return;
                }
                st.order.push(item());
                st.commit_resolved = true;
                st.awaiting_commit = false;
            }
            "conversation.item.input_audio_transcription.completed" => {
                let id = item();
                let mut st = self.st.lock();
                if st.order.contains(&id) {
                    let text = ev.get("transcript").and_then(Value::as_str).unwrap_or("");
                    st.done.insert(id, py::strip(text).to_string());
                }
            }
            "conversation.item.input_audio_transcription.failed" => {
                let id = item();
                if self.st.lock().order.contains(&id) {
                    self.broken.store(true, Ordering::SeqCst); // recover with the complete PCM instead
                }
            }
            "error" => {
                let code = py::str_or_empty(ev.get("error").and_then(|e| e.get("code")));
                warn!("call: live transcription error {}", if code.is_empty() { "?" } else { &code });
                self.broken.store(true, Ordering::SeqCst);
                self.configured.set();
            }
            _ => {}
        }
        self.changed();
    }
}

async fn read_events(shared: Arc<EarsShared>, conn: Arc<dyn RealtimeConn>) {
    loop {
        match conn.recv().await {
            Some(Ok(ev)) => shared.handle(&ev),
            Some(Err(e)) => {
                info!("call: the live transcription session closed ({})", error_name(&e));
                break;
            }
            None => break,
        }
    }
    shared.broken.store(true, Ordering::SeqCst);
    shared.configured.set();
    shared.changed();
}

/// Stream the held button's audio, with one manually committed transcript at release.
///
/// One OpenAI realtime transcription session per call, opened when the call starts and reused for every press.
/// Audio is appended as it arrives, and the server receives the entire press as one manually committed turn. The
/// next press cannot clear or reuse that turn's bookkeeping until its transcript is complete. This preserves
/// sentence context and prevents delayed events from a previous press appearing at the start of the next one. Any
/// failure is the caller's cue to fall back to uploading the press ([`Voice::transcribe`]).
pub struct LiveEars {
    pub api_key: String,
    pub model: String,
    pub language: String,
    /// How long `open` waits for the server to accept the session (5 s).
    pub open_timeout: f64,
    /// The spend ledger the live transcription is metered in.
    pub ledger: Ledger,
    connector: Arc<dyn RealtimeConnect>,
    shared: Arc<EarsShared>,
    conn: Mutex<Option<Arc<dyn RealtimeConn>>>,
    reader: Mutex<Option<JoinHandle<()>>>,
    press: Arc<Semaphore>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
}

impl LiveEars {
    pub fn new(api_key: &str, model: &str, language: &str) -> Self {
        Self::with_connector(api_key, model, language, Arc::new(OpenAIRealtime::default()))
    }

    pub fn with_connector(api_key: &str, model: &str, language: &str, connector: Arc<dyn RealtimeConnect>) -> Self {
        let shared = EarsShared::default();
        shared.st.lock().commit_resolved = true;
        Self {
            api_key: api_key.into(),
            model: model.into(),
            language: language.into(),
            open_timeout: 5.0,
            ledger: Ledger::default(),
            connector,
            shared: Arc::new(shared),
            conn: Mutex::new(None),
            reader: Mutex::new(None),
            press: Arc::new(Semaphore::new(1)),
            permit: Mutex::new(None),
        }
    }

    /// The press's committed items, in order.
    pub fn order(&self) -> Vec<String> {
        self.shared.st.lock().order.clone()
    }

    /// The press's finished transcripts by item.
    pub fn done(&self) -> HashMap<String, String> {
        self.shared.st.lock().done.clone()
    }

    /// Whether a press holds the session (`_press_lock.locked()`).
    pub fn press_locked(&self) -> bool {
        self.press.available_permits() == 0
    }

    /// Whether the event reader has ended (`_reader.done()`); true when it never started.
    pub fn reader_done(&self) -> bool {
        self.reader.lock().as_ref().is_none_or(|h| h.is_finished())
    }

    /// Whether the reader was started.
    pub fn reader_started(&self) -> bool {
        self.reader.lock().is_some()
    }

    /// Wait until the server accepted the session (`configured.wait()`).
    pub async fn configured(&self) {
        self.shared.configured.wait().await
    }

    fn conn(&self) -> anyhow::Result<Arc<dyn RealtimeConn>> {
        self.conn.lock().clone().ok_or_else(|| EarsError::Connection("the live transcription session is not open".into()).into())
    }

    fn release(&self) {
        self.permit.lock().take();
    }

    fn session(&self) -> Value {
        let mut transcription = Map::new();
        transcription.insert("model".into(), json!(self.model));
        if !self.language.is_empty() {
            transcription.insert("language".into(), json!(self.language));
        }
        json!({"type": "transcription", "audio": {"input": {
            "format": {"type": "audio/pcm", "rate": SAMPLE_RATE},
            "transcription": transcription,
            "noise_reduction": {"type": LIVE_NOISE_REDUCTION},
            "turn_detection": null}}})
    }
}

/// On a dropped (cancelled) or failed press: the session is never reused, and the press lets go.
struct PressGuard<'a> {
    ears: &'a LiveEars,
    ok: bool,
}

impl Drop for PressGuard<'_> {
    fn drop(&mut self) {
        if !self.ok {
            self.ears.shared.broken.store(true, Ordering::SeqCst);
        }
        self.ears.release();
    }
}

#[async_trait]
impl Ears for LiveEars {
    async fn open(&self) -> anyhow::Result<()> {
        let conn = self.connector.connect(&self.api_key).await?;
        *self.conn.lock() = Some(conn.clone());
        conn.send(json!({"type": "session.update", "session": self.session()})).await?;
        let handle = tokio::spawn(read_events(self.shared.clone(), conn));
        *self.reader.lock() = Some(handle);
        // Sending session.update does not prove the server accepted it. Audio must not reach the default VAD session
        // while the update is still pending, or it can produce an unsolicited startup turn.
        tokio::time::timeout(secs(self.open_timeout), self.shared.configured.wait())
            .await
            .map_err(|_| EarsError::Timeout)?;
        if self.shared.broken.load(Ordering::SeqCst) {
            return Err(EarsError::Connection("the live transcription session was not configured".into()).into());
        }
        Ok(())
    }

    async fn begin(&self) -> anyhow::Result<()> {
        let permit = self.press.clone().acquire_owned().await?;
        if self.shared.broken.load(Ordering::SeqCst) {
            return Err(EarsError::Connection("the live transcription session closed".into()).into());
        }
        {
            let mut st = self.shared.st.lock();
            st.order.clear();
            st.done.clear();
            st.fed = 0;
            st.commit_resolved = true;
            st.awaiting_commit = false;
        }
        let conn = self.conn()?;
        conn.send(json!({"type": "input_audio_buffer.clear"})).await?;
        *self.permit.lock() = Some(permit);
        Ok(())
    }

    async fn discard(&self) -> anyhow::Result<()> {
        let mut guard = PressGuard { ears: self, ok: false };
        let conn = self.conn()?;
        conn.send(json!({"type": "input_audio_buffer.clear"})).await?;
        guard.ok = true;
        Ok(())
    }

    async fn feed(&self, pcm: &[u8]) -> anyhow::Result<()> {
        self.shared.st.lock().fed += pcm.len();
        let audio = base64::engine::general_purpose::STANDARD.encode(pcm);
        self.conn()?.send(json!({"type": "input_audio_buffer.append", "audio": audio})).await
    }

    async fn finish(&self, timeout: f64) -> anyhow::Result<String> {
        let mut guard = PressGuard { ears: self, ok: false };
        let shared = self.shared.clone();
        if shared.broken.load(Ordering::SeqCst) {
            return Err(EarsError::Connection("the live transcription session closed".into()).into());
        }
        {
            let mut st = shared.st.lock();
            st.commit_resolved = false;
            st.awaiting_commit = true;
        }
        let conn = self.conn()?;
        let waited = tokio::time::timeout(secs(timeout), async {
            conn.send(json!({"type": "input_audio_buffer.commit"})).await?;
            loop {
                let n = shared.changed.notified();
                tokio::pin!(n);
                n.as_mut().enable();
                if shared.broken.load(Ordering::SeqCst) {
                    return Err(anyhow::Error::from(EarsError::Connection(
                        "the live transcription session closed".into(),
                    )));
                }
                {
                    let st = shared.st.lock();
                    if st.commit_resolved && !st.order.is_empty() && st.order.iter().all(|i| st.done.contains_key(i)) {
                        return Ok(());
                    }
                }
                n.await;
            }
        })
        .await;
        match waited {
            Err(_) => return Err(EarsError::Timeout.into()),
            Ok(Err(e)) => return Err(e),
            Ok(Ok(())) => {}
        }
        let (fed, text) = {
            let st = shared.st.lock();
            let words: Vec<&str> =
                st.order.iter().filter_map(|i| st.done.get(i)).filter(|t| !t.is_empty()).map(String::as_str).collect();
            (st.fed, py::strip(&words.join(" ")).to_string())
        };
        self.ledger.record(
            "openai",
            &self.model,
            eve_spend::CALLS,
            None,
            RecordOpts::new().note(&format!("live transcription, {:.1} s", fed as f64 / BYTES_PER_SEC as f64)),
        );
        guard.ok = true;
        Ok(text)
    }

    async fn close(&self) {
        let handle = self.reader.lock().take();
        if let Some(mut h) = handle {
            h.abort();
            let _ = (&mut h).await;
            *self.reader.lock() = Some(h);
        }
        let conn = self.conn.lock().clone();
        if let Some(c) = conn {
            c.close();
        }
    }

    fn close_now(&self) {
        if let Some(h) = self.reader.lock().as_ref() {
            h.abort();
        }
        if let Some(c) = self.conn.lock().clone() {
            c.close();
        }
    }

    fn broken(&self) -> bool {
        self.shared.broken.load(Ordering::SeqCst)
    }

    fn set_broken(&self) {
        self.shared.broken.store(true, Ordering::SeqCst);
    }
}

// ---- the real voice -----------------------------------------------------------------------------------------------

/// Speech to text and text to speech on OpenRouter, with the owner's OPENROUTER_API_KEY (blocking: run on a
/// thread). The name is kept from when both were OpenAI's; the transcriber is still an OpenAI model.
pub struct OpenAIVoice {
    /// The transcription client (`openrouter.client(api_key)`); a test swaps in a fake.
    pub client: Arc<dyn NotesSdk>,
    pub stt_model: String,
    /// EVE_CALL_LANGUAGE; "" = the model guesses.
    pub language: String,
    /// LiveEars: EVE_CALL_LIVE_STT=1 and an OpenAI key.
    pub live: bool,
    /// The OpenAI key the live ears need (opt-in only).
    pub live_key: String,
    pub or_key: String,
    pub or_model: String,
    pub or_voice: String,
    /// Where the speech is asked for ([`OPENROUTER_SPEECH_URL`]; a test points it at a local server).
    pub speech_url: String,
    /// The spend ledger the speech and transcriptions are metered in.
    pub ledger: Ledger,
    /// How the live ears connect (OpenAI's realtime WebSocket).
    pub realtime: Arc<dyn RealtimeConnect>,
}

impl OpenAIVoice {
    pub fn new(api_key: &str, stt_model: &str) -> Self {
        Self {
            client: Arc::new(eve_notes::OpenRouterSdk::new(api_key.to_string(), Default::default())),
            stt_model: stt_model.to_string(),
            language: DEFAULT_LANGUAGE.to_string(),
            live: false,
            live_key: String::new(),
            or_key: api_key.to_string(),
            or_model: DEFAULT_OR_TTS_MODEL.to_string(),
            or_voice: DEFAULT_OR_TTS_VOICE.to_string(),
            speech_url: OPENROUTER_SPEECH_URL.to_string(),
            ledger: Ledger::default(),
            realtime: Arc::new(OpenAIRealtime::default()),
        }
    }
}

/// The speech request, made on the first chunk asked for (a generator runs nothing until iterated).
struct OpenRouterSpeech<'a> {
    voice: &'a OpenAIVoice,
    text: String,
    resp: Option<reqwest::blocking::Response>,
    started: bool,
    done: bool,
}

impl OpenRouterSpeech<'_> {
    fn request(&self) -> anyhow::Result<reqwest::blocking::Response> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        let resp = http
            .post(&self.voice.speech_url)
            .header("Authorization", format!("Bearer {}", self.voice.or_key))
            .json(&json!({"model": self.voice.or_model, "voice": self.voice.or_voice, "input": self.text,
                          "response_format": "pcm"}))
            .send()?;
        if resp.status().as_u16() != 200 {
            anyhow::bail!("OpenRouter speech {}", resp.status().as_u16());
        }
        Ok(resp)
    }
}

impl Iterator for OpenRouterSpeech<'_> {
    type Item = anyhow::Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if !self.started {
            self.started = true;
            match self.request() {
                Ok(r) => self.resp = Some(r),
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
        let resp = self.resp.as_mut()?;
        // httpx's iter_bytes(4800): whole 4800-byte chunks, the last one shorter
        let mut buf = vec![0u8; 4800];
        let mut got = 0;
        while got < buf.len() {
            match resp.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) => {
                    self.done = true;
                    return Some(Err(e.into()));
                }
            }
        }
        if got == 0 {
            self.done = true;
            self.resp = None;
            let chars = py::len(&self.text);
            self.voice.ledger.record(
                "openrouter",
                &self.voice.or_model,
                eve_spend::CALLS,
                Some(chars as f64 * OR_TTS_USD_PER_CHAR),
                RecordOpts::new().note(&format!("speech, {chars} chars")),
            );
            return None;
        }
        buf.truncate(got);
        Some(Ok(buf))
    }
}

impl Voice for OpenAIVoice {
    fn transcribe(&self, pcm: &[u8]) -> anyhow::Result<String> {
        let mut fields = Map::new();
        fields.insert("model".into(), json!(self.stt_model));
        if !self.language.is_empty() {
            fields.insert("language".into(), json!(self.language));
        }
        let file = Part::file("file", "call.wav", eve_notes::to_wav(pcm, SAMPLE_RATE), "audio/wav");
        let resp = self.client.transcriptions_create(fields, file)?;
        self.ledger.record_transcription(
            eve_spend::CALLS,
            &self.stt_model,
            &resp,
            pcm.len() as f64 / BYTES_PER_SEC as f64,
            "openrouter",
        );
        let text = resp.get("text").and_then(Value::as_str).unwrap_or("");
        Ok(py::strip(text).to_string())
    }

    /// The spoken words as they are made: the first bytes play while the rest arrive. OpenRouter's voice
    /// (deepgram/aura-2). A failure is an error item: that sentence is not heard (the chat has it in writing). There
    /// is no second voice since OpenAI's was removed with the OpenAI key (2026-10-08).
    fn speak_stream(&self, text: &str) -> SpeechStream<'_> {
        Box::new(OpenRouterSpeech { voice: self, text: text.to_string(), resp: None, started: false, done: false })
    }

    fn live_ears(&self) -> Option<Arc<dyn Ears>> {
        if self.live && !self.live_key.is_empty() {
            let mut ears = LiveEars::with_connector(&self.live_key, &self.stt_model, &self.language, self.realtime.clone());
            ears.ledger = self.ledger.clone();
            Some(Arc::new(ears))
        } else {
            None
        }
    }

    fn language(&self) -> String {
        self.language.clone()
    }
}

/// A truthy switch: "1", "true", "yes" or "on".
pub fn on(raw: Option<&str>) -> bool {
    matches!(py::strip(raw.unwrap_or("")).to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// The call's ears and voice on OPENROUTER_API_KEY, or None (calls off) without it. EVE_CALL_STT_MODEL,
/// EVE_CALL_LANGUAGE, EVE_CALL_OR_TTS_MODEL and EVE_CALL_OR_VOICE tune it. The live ears are opt-in:
/// EVE_CALL_LIVE_STT=1 with an OPENAI_API_KEY (OpenAI's realtime transcription; OpenRouter has none). `None` reads
/// the process environment.
pub fn make_voice(environ: Option<&HashMap<String, String>>) -> Option<OpenAIVoice> {
    let process;
    let env = match environ {
        Some(e) => e,
        None => {
            process = eve_records::py::environ();
            &process
        }
    };
    let key = eve_openrouter::key(Some(env));
    if key.is_empty() {
        warn!("call: OPENROUTER_API_KEY is not set — calls are off");
        return None;
    }
    let or_default = |name: &str, default: &str| -> String {
        let raw = env.get(name).map(String::as_str).filter(|v| !v.is_empty()).unwrap_or(default);
        py::strip(raw).to_string()
    };
    let mut voice = OpenAIVoice::new(&key, &or_default("EVE_CALL_STT_MODEL", DEFAULT_STT_MODEL));
    voice.language =
        py::strip(env.get("EVE_CALL_LANGUAGE").map(String::as_str).unwrap_or(DEFAULT_LANGUAGE)).to_lowercase();
    voice.or_model = or_default("EVE_CALL_OR_TTS_MODEL", DEFAULT_OR_TTS_MODEL);
    voice.or_voice = or_default("EVE_CALL_OR_VOICE", DEFAULT_OR_TTS_VOICE);
    if on(env.get("EVE_CALL_LIVE_STT").map(String::as_str)) {
        voice.live_key = py::strip(env.get(LIVE_EARS_KEY_ENV).map(String::as_str).unwrap_or("")).to_string();
        voice.live = !voice.live_key.is_empty();
        if !voice.live {
            warn!("call: EVE_CALL_LIVE_STT=1 but OPENAI_API_KEY is not set; each press is uploaded instead");
        }
    }
    Some(voice)
}

// ---- the call -----------------------------------------------------------------------------------------------------

/// A reader of what eve says (Python's bound `Call.say`): two are equal when they are the same reader.
#[derive(Clone)]
pub struct Reader(Arc<dyn Fn(&str) + Send + Sync>);

impl Reader {
    pub fn new(f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    pub fn say(&self, text: &str) {
        (self.0)(text)
    }
}

impl PartialEq for Reader {
    fn eq(&self, other: &Self) -> bool {
        same(&self.0, &other.0)
    }
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Reader({:p})", Arc::as_ptr(&self.0) as *const ())
    }
}

/// The question a press started under (telegram.TelegramInlet's open question, or none).
pub type AnswerContext = Option<Arc<dyn Any + Send + Sync>>;

/// What a press carries to `hear`: nothing for a brain with no capture hook, else what it captured.
#[derive(Clone, Default)]
pub enum Captured {
    #[default]
    Uncaptured,
    Context(AnswerContext),
}

/// The chat, lent by the daemon (telegram.TelegramInlet): `hear` hands it the owner's words as a message;
/// `listen(say)` has everything it then says to the owner also passed to `say` until `listen(None)`.
/// `listen(None, owner=say)` clears it only while `say` is still the listener.
///
/// The optional parts of Python's protocol are the defaulted methods: `chat_ready` (`None` when the brain has no
/// such method), `answer_context` (CaptureBrain's hook; `None` when there is none) and `takes_call` (whether
/// `hear` takes `call=`, the reader of the call that heard the words).
pub trait Brain: Send + Sync {
    /// The owner's words. `call` is the hearing call's reader when [`Brain::takes_call`]; `answer_context` is the
    /// question the press started under, when the brain captured one.
    fn hear(&self, text: &str, call: Option<&Reader>, answer_context: Captured);
    fn listen(&self, say: Option<Reader>, owner: Option<&Reader>) -> bool;
    fn chat_ready(&self) -> Option<bool> {
        None
    }
    fn answer_context(&self) -> Option<AnswerContext> {
        None
    }
    fn takes_call(&self) -> bool {
        false
    }
}

/// The phone's initData → the owner's id, or None (miniapp.check_init_data).
pub type CheckOwner = Arc<dyn Fn(&str) -> Option<i64> + Send + Sync>;
/// Eve Link's token check (stick_link.py).
pub type TokenCheck = Arc<dyn Fn(&str) -> bool + Send + Sync>;
/// The expression picker's offer: `(who, text)` → the event id it queued, if any.
pub type OnText = Arc<dyn Fn(&str, &str) -> anyhow::Result<Option<i64>> + Send + Sync>;

/// The waits that tests shorten (Python patches the module constants). Read on every use, so a change reaches a
/// call already running.
pub struct Limits {
    auth: AtomicU64,
    quiet: AtomicU64,
    max_call: AtomicU64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            auth: AtomicU64::new(AUTH_SECS.to_bits()),
            quiet: AtomicU64::new(QUIET_SECS.to_bits()),
            max_call: AtomicU64::new(MAX_CALL_SECS.to_bits()),
        }
    }
}

impl Limits {
    pub fn auth_secs(&self) -> f64 {
        f64::from_bits(self.auth.load(Ordering::SeqCst))
    }
    pub fn quiet_secs(&self) -> f64 {
        f64::from_bits(self.quiet.load(Ordering::SeqCst))
    }
    pub fn max_call_secs(&self) -> f64 {
        f64::from_bits(self.max_call.load(Ordering::SeqCst))
    }
    pub fn set_auth_secs(&self, v: f64) {
        self.auth.store(v.to_bits(), Ordering::SeqCst)
    }
    pub fn set_quiet_secs(&self, v: f64) {
        self.quiet.store(v.to_bits(), Ordering::SeqCst)
    }
    pub fn set_max_call_secs(&self, v: f64) {
        self.max_call.store(v.to_bits(), Ordering::SeqCst)
    }
}

/// `asyncio.Queue[str]` of what to read out.
#[derive(Default)]
struct Lines {
    q: Mutex<VecDeque<String>>,
    notify: Notify,
}

impl Lines {
    fn put(&self, s: String) {
        self.q.lock().push_back(s);
        self.notify.notify_one();
    }

    fn pop(&self) -> Option<String> {
        self.q.lock().pop_front()
    }

    fn is_empty(&self) -> bool {
        self.q.lock().is_empty()
    }

    async fn get(&self) -> String {
        loop {
            let n = self.notify.notified();
            if let Some(s) = self.pop() {
                return s;
            }
            n.await;
        }
    }
}

/// The call's own tasks (`self.tasks`): each leaves the set when it ends, however it ends.
#[derive(Default)]
struct TaskSet {
    next: AtomicU64,
    map: Mutex<HashMap<u64, JoinHandle<()>>>,
}

struct Leave(Weak<TaskSet>, u64);

impl Drop for Leave {
    fn drop(&mut self) {
        if let Some(set) = self.0.upgrade() {
            set.map.lock().remove(&self.1);
        }
    }
}

impl TaskSet {
    fn spawn(self: &Arc<Self>, fut: impl Future<Output = ()> + Send + 'static) {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let me = Arc::downgrade(self);
        let mut map = self.map.lock();
        let handle = tokio::spawn(async move {
            let _leave = Leave(me, id);
            fut.await;
        });
        map.insert(id, handle);
    }

    fn is_empty(&self) -> bool {
        self.map.lock().is_empty()
    }

    fn take_all(&self) -> Vec<JoinHandle<()>> {
        self.map.lock().drain().map(|(_, h)| h).collect()
    }
}

/// The stages of one press, release to first sound (seconds).
#[derive(Debug, Clone, Default)]
struct Timing {
    press: f64,
    stt: f64,
    live: bool,
    brain: Option<f64>,
    first_sound: f64,
}

struct CallState {
    talking: bool,
    press: Vec<u8>,
    answer_context: Captured,
    presses: usize,
    ears_live: bool,
    released_at: Option<f64>,
    timing: Timing,
    listening: bool,
    speaking: bool,
    last_activity: f64,
    from_app: bool,
    can_notes: bool,
    can_eyes: bool,
    play_end: f64,
    said_for: IndexMap<i64, String>,
}

/// One call: presses in, speech out. Each press is timed from release to eve's first sound, per stage, in the log
/// (the owner asked for less latency: measure it, do not guess).
pub struct Call {
    pub ws: Arc<dyn CallSocket>,
    brain: Arc<dyn Brain>,
    voice: Arc<dyn Voice>,
    clock: Clock,
    limits: Arc<Limits>,
    reader: Reader,
    me: Weak<Call>,
    st: Mutex<CallState>,
    lines: Lines,
    tasks: Arc<TaskSet>,
    reading: Mutex<Option<JoinHandle<()>>>,
    opening: Mutex<Option<JoinHandle<()>>>,
    ears: Mutex<Option<Arc<dyn Ears>>>,
    offers: Mutex<HashMap<u64, AbortHandle>>,
    next_offer: AtomicU64,
    notes: Mutex<Option<Arc<PhoneNotes>>>,
    on_text: Mutex<Option<OnText>>,
}

/// An ears session being opened by a task that may be cancelled: closed if the task is dropped.
struct CloseOnDrop(Option<Arc<dyn Ears>>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if let Some(ears) = self.0.take() {
            ears.close_now();
        }
    }
}

/// `run`'s `finally` when the run itself is dropped (cancelled) before it ends.
struct RunGuard(Option<Arc<Call>>);

impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Some(call) = self.0.take() {
            call.brain.listen(None, Some(&call.reader));
            for h in call.tasks.take_all() {
                h.abort();
            }
            for slot in [&call.reading, &call.opening] {
                if let Some(h) = slot.lock().take() {
                    h.abort();
                }
            }
            if let Some(ears) = call.ears.lock().clone() {
                ears.close_now();
            }
        }
    }
}

impl Call {
    pub fn new(ws: Arc<dyn CallSocket>, brain: Arc<dyn Brain>, voice: Arc<dyn Voice>) -> Arc<Self> {
        Self::with_clock(ws, brain, voice, default_clock())
    }

    pub fn with_clock(ws: Arc<dyn CallSocket>, brain: Arc<dyn Brain>, voice: Arc<dyn Voice>, clock: Clock) -> Arc<Self> {
        Self::build(ws, brain, voice, clock, Arc::new(Limits::default()))
    }

    /// A call whose waits (`QUIET_SECS`, the hour) are read from `limits`: desk_call.py's calls read this module's
    /// constants, which its tests patch.
    pub fn with_limits(ws: Arc<dyn CallSocket>, brain: Arc<dyn Brain>, voice: Arc<dyn Voice>, limits: Arc<Limits>) -> Arc<Self> {
        Self::build(ws, brain, voice, default_clock(), limits)
    }

    fn build(
        ws: Arc<dyn CallSocket>,
        brain: Arc<dyn Brain>,
        voice: Arc<dyn Voice>,
        clock: Clock,
        limits: Arc<Limits>,
    ) -> Arc<Self> {
        let now = clock();
        Arc::new_cyclic(|me: &Weak<Call>| {
            let weak = me.clone();
            let reader = Reader::new(move |text| {
                if let Some(call) = weak.upgrade() {
                    call.say(text);
                }
            });
            Call {
                ws,
                brain,
                voice,
                clock,
                limits,
                reader,
                me: me.clone(),
                st: Mutex::new(CallState {
                    talking: false,
                    press: Vec::new(),
                    answer_context: Captured::Uncaptured,
                    presses: 0,
                    ears_live: false,
                    released_at: None,
                    timing: Timing::default(),
                    listening: false,
                    speaking: false,
                    last_activity: now,
                    from_app: false,
                    can_notes: false,
                    can_eyes: false,
                    play_end: 0.0,
                    said_for: IndexMap::new(),
                }),
                lines: Lines::default(),
                tasks: Arc::new(TaskSet::default()),
                reading: Mutex::new(None),
                opening: Mutex::new(None),
                ears: Mutex::new(None),
                offers: Mutex::new(HashMap::new()),
                next_offer: AtomicU64::new(0),
                notes: Mutex::new(None),
                on_text: Mutex::new(None),
            }
        })
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    /// This call's reader (`call.say`): what a brain is lent as its listener.
    pub fn reader(&self) -> Reader {
        self.reader.clone()
    }

    pub fn talking(&self) -> bool {
        self.st.lock().talking
    }
    pub fn set_talking(&self, v: bool) {
        self.st.lock().talking = v;
    }
    pub fn press(&self) -> Vec<u8> {
        self.st.lock().press.clone()
    }
    pub fn set_press(&self, pcm: Vec<u8>) {
        self.st.lock().press = pcm;
    }
    pub fn presses(&self) -> usize {
        self.st.lock().presses
    }
    pub fn listening(&self) -> bool {
        self.st.lock().listening
    }
    pub fn speaking(&self) -> bool {
        self.st.lock().speaking
    }
    pub fn set_speaking(&self, v: bool) {
        self.st.lock().speaking = v;
    }
    /// Eve Link's call (`token` hello). Set by `PhoneCalls::serve`.
    pub fn from_app(&self) -> bool {
        self.st.lock().from_app
    }
    pub fn set_from_app(&self, v: bool) {
        self.st.lock().from_app = v;
    }
    /// The hello said it can take notes: only such a call records notes on the phone.
    pub fn can_notes(&self) -> bool {
        self.st.lock().can_notes
    }
    pub fn set_can_notes(&self, v: bool) {
        self.st.lock().can_notes = v;
    }
    /// The hello listed "eyes": the face can draw expressions.
    pub fn can_eyes(&self) -> bool {
        self.st.lock().can_eyes
    }
    pub fn set_can_eyes(&self, v: bool) {
        self.st.lock().can_eyes = v;
    }
    /// The picker's offer, set only for a call that can draw expressions.
    pub fn set_on_text(&self, f: Option<OnText>) {
        *self.on_text.lock() = f;
    }
    /// The notes this call is the microphone of, while they are on.
    pub fn notes(&self) -> Option<Arc<PhoneNotes>> {
        self.notes.lock().clone()
    }
    pub fn set_notes(&self, n: Option<Arc<PhoneNotes>>) {
        *self.notes.lock() = n;
    }
    /// When the phone's queued audio of eve's ends, on this call's clock.
    pub fn play_end(&self) -> f64 {
        self.st.lock().play_end
    }
    /// Sentences waiting to be offered when their sound plays.
    pub fn pending_offers(&self) -> usize {
        self.offers.lock().len()
    }
    /// Whether the call has tasks still running (a press being heard).
    pub fn has_tasks(&self) -> bool {
        !self.tasks.is_empty()
    }
    /// Queue words to read out (`call.lines.put_nowait`).
    pub fn queue_line(&self, text: &str) {
        self.lines.put(text.to_string());
    }
    /// Take one queued line back (`call.lines.get_nowait`).
    pub fn take_line(&self) -> Option<String> {
        self.lines.pop()
    }
    /// The live ears this call uses, once open.
    pub fn ears(&self) -> Option<Arc<dyn Ears>> {
        self.ears.lock().clone()
    }

    /// A turn's words to the expression picker. Its fault is never the call's: the call goes on without eyes.
    pub fn text(&self, who: &str, text: &str) {
        let Some(on_text) = self.on_text.lock().clone() else { return };
        if self.ws.closed() {
            return;
        }
        let event = match on_text(who, text) {
            Ok(e) => e,
            Err(e) => {
                warn!("call: expression offer failed ({})", error_name(&e));
                return;
            }
        };
        if who == "assistant" {
            if let Some(id) = event {
                let mut st = self.st.lock();
                st.said_for.insert(id, text.to_string());
                while st.said_for.len() > EYES_SAID_KEEP {
                    st.said_for.shift_remove_index(0);
                }
            }
        }
    }

    /// Offers one of eve's sentences when the phone starts to play it: now if nothing of eve's is still queued
    /// there, else when what is queued has played. Call it just before the sentence's first audio is sent.
    pub fn offer_when_heard(&self, text: &str) {
        if self.on_text.lock().is_none() {
            return;
        }
        let delay = (self.play_end() - self.now()).clamp(0.0, EYES_MAX_WAIT_SECS);
        if delay <= 0.0 {
            self.text("assistant", text);
            return;
        }
        let id = self.next_offer.fetch_add(1, Ordering::SeqCst);
        let me = self.me.clone();
        let text = text.to_string();
        let mut offers = self.offers.lock();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(secs(delay)).await;
            if let Some(call) = me.upgrade() {
                call.offers.lock().remove(&id);
                call.text("assistant", &text);
            }
        });
        offers.insert(id, handle.abort_handle());
    }

    /// `nbytes` of eve's audio went to the phone: it plays after what is already queued there.
    pub fn sent_audio(&self, nbytes: usize) {
        let now = self.now();
        let mut st = self.st.lock();
        st.play_end = now.max(st.play_end) + nbytes as f64 / BYTES_PER_SEC as f64;
    }

    /// The phone dropped eve's queued audio (a barge-in) or the call ended: offer none of its sentences.
    pub fn drop_offers(&self) {
        for (_, h) in self.offers.lock().drain() {
            h.abort();
        }
        self.st.lock().play_end = 0.0;
    }

    /// One `expression` message to the phone, with the sentence its pick was made for as `said`. False when the
    /// socket is gone.
    pub async fn send_expression(&self, msg: Map<String, Value>) -> bool {
        if self.ws.closed() {
            return false;
        }
        let mut msg = msg;
        let labelled = msg.get("label").is_some_and(|l| !l.is_null());
        let said = if labelled {
            msg.get("id").and_then(py::as_int).and_then(|id| self.st.lock().said_for.get(&id).cloned())
        } else {
            None
        };
        if let Some(said) = said {
            msg.insert("said".into(), json!(said));
        }
        self.ws.send_json(&Value::Object(msg)).await.is_ok()
    }

    fn touch(&self) {
        let now = self.now();
        self.st.lock().last_activity = now;
    }

    /// Take the chat's listener, unless this call has it already.
    pub fn take_chat(&self) -> bool {
        if !self.st.lock().listening {
            let got = self.brain.listen(Some(self.reader.clone()), None);
            self.st.lock().listening = got;
        }
        self.st.lock().listening
    }

    /// No press, transcription, reply or speech for [`IDLE_YIELD_SECS`].
    pub fn idle(&self) -> bool {
        let now = self.now();
        let st = self.st.lock();
        !st.talking
            && !st.speaking
            && self.tasks.is_empty()
            && self.lines.is_empty()
            && now - st.last_activity >= IDLE_YIELD_SECS
    }

    /// The desk's button wants the chat: an idle call lets go of it (and stays open, for its next press).
    pub fn release_if_idle(&self) -> bool {
        let listening = self.st.lock().listening;
        if listening && self.idle() {
            self.brain.listen(None, Some(&self.reader));
            self.st.lock().listening = false;
            info!("call: idle, so the desk's button takes the chat");
            return true;
        }
        !listening
    }

    /// The turn that started the notes is over (its reply read out): now let the chat's listener go.
    pub fn release_for_notes(&self) {
        if self.notes().is_some() {
            self.release_chat();
        }
    }

    /// A press the notes cut off (a press made while eve was still answering, then notes started): it is nobody's
    /// question now. The phone drops its turn too (Relay.notesOn), so both sides forget it, and the next press after
    /// the notes is a fresh one, not stuck behind `talking`.
    pub fn drop_press(&self) {
        let live = {
            let mut st = self.st.lock();
            st.talking = false;
            st.press.clear();
            st.answer_context = Captured::Uncaptured;
            std::mem::replace(&mut st.ears_live, false)
        };
        if live {
            if let Some(ears) = self.ears() {
                self.tasks.spawn(async move {
                    let _ = ears.discard().await;
                });
            }
        }
    }

    /// Give the chat's listener up, so nothing texted is read out on the call (notes recording the phone would
    /// write eve's own voice down). The call's next press takes it back.
    pub fn release_chat(&self) {
        if self.st.lock().listening {
            self.brain.listen(None, Some(&self.reader));
            self.st.lock().listening = false;
        }
    }

    /// The state the phone waits in: taking notes, while they are on; listening for the next press otherwise.
    pub async fn rest(&self) {
        match self.notes() {
            Some(notes) => self.state("notes", notes.progress()).await,
            None => self.state("listening", Map::new()).await,
        }
    }

    /// A `state` message, if the socket is still there (a closed one is not an error).
    pub async fn state(&self, state: &str, extra: Map<String, Value>) {
        if !self.ws.closed() {
            let mut msg = Map::new();
            msg.insert("type".into(), json!("state"));
            msg.insert("state".into(), json!(state));
            msg.extend(extra);
            let _ = self.ws.send_json(&Value::Object(msg)).await;
        }
    }

    async fn state_note(&self, state: &str, note: &str) {
        let mut extra = Map::new();
        extra.insert("note".into(), json!(note));
        self.state(state, extra).await
    }

    /// Something eve said in the chat (or a sentence of it, streamed): read it out after what is playing.
    pub fn say(&self, text: &str) {
        let words = speakable(text);
        if !words.is_empty() {
            let now = self.now();
            {
                let mut st = self.st.lock();
                if let Some(released) = st.released_at {
                    if st.timing.brain.is_none() {
                        st.timing.brain = Some(now - released);
                    }
                }
            }
            self.lines.put(words);
        }
    }

    /// The words' audio as it is made: the blocking stream runs on a thread and hands chunks over. The thread runs
    /// the stream to its end even when nobody is listening any more (a barge-in), as the executor's worker does.
    fn speech(&self, text: String) -> mpsc::UnboundedReceiver<Vec<u8>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let voice = self.voice.clone();
        std::thread::spawn(move || {
            for chunk in voice.speak_stream(&text) {
                match chunk {
                    Ok(c) => {
                        let _ = tx.send(c);
                    }
                    Err(e) => {
                        // one sentence unheard, the call goes on
                        warn!("call: speech failed ({e}); a sentence was not read out");
                        return;
                    }
                }
            }
        });
        rx
    }

    /// The reader: one message at a time, a sentence at a time, streamed to the phone as it is made.
    async fn read_out(self: Arc<Self>) {
        let mut carry: Vec<u8> = Vec::new();
        loop {
            let text = self.lines.get().await;
            self.st.lock().speaking = true;
            self.touch();
            self.state("speaking", Map::new()).await;
            for piece in sentences(&text, 220) {
                // The sentence's words go just before its sound, so the phone's voice mode captions what it plays
                // (owner, 2026-10-09: "the stackchan face with captions"); the phone shows it when it is heard.
                if !self.ws.closed() {
                    let _ = self.ws.send_json(&json!({"type": "said", "text": piece})).await;
                }
                let mut offered = false;
                let mut chunks = self.speech(piece.clone());
                while let Some(chunk) = chunks.recv().await {
                    let mut data = std::mem::take(&mut carry);
                    data.extend_from_slice(&chunk);
                    let even = data.len() - data.len() % 2;
                    carry = data.split_off(even);
                    if !data.is_empty() && !self.ws.closed() {
                        if self.st.lock().released_at.is_some() {
                            self.first_sound();
                        }
                        if !offered {
                            // to the picker when the phone starts to play it, so the eyes match what is heard
                            self.offer_when_heard(&piece);
                            offered = true;
                        }
                        if self.ws.send_bytes(&data).await.is_err() {
                            return;
                        }
                        self.sent_audio(data.len());
                    }
                }
            }
            if self.lines.is_empty() {
                self.st.lock().speaking = false;
                self.touch();
                self.rest().await;
            }
        }
    }

    fn first_sound(&self) {
        let now = self.now();
        let t = {
            let mut st = self.st.lock();
            let released = st.released_at.unwrap_or(now);
            st.timing.first_sound = now - released;
            st.released_at = None;
            st.timing.clone()
        };
        info!(
            "call: press {:.1} s → heard in {:.2} s ({}) → first reply {:.2} s → first sound {:.2} s after release",
            t.press,
            t.stt,
            if t.live { "live" } else { "upload" },
            t.brain.unwrap_or(0.0),
            t.first_sound
        );
    }

    fn start_reader(&self) {
        if let Some(me) = self.me.upgrade() {
            *self.reading.lock() = Some(tokio::spawn(me.read_out()));
        }
    }

    /// A press while eve talks: stop reading, drop what is queued here and on the phone.
    pub async fn interrupt(&self) {
        while self.lines.pop().is_some() {}
        let reading = self.reading.lock().take();
        if let Some(h) = reading {
            if !h.is_finished() {
                h.abort();
                let _ = h.await;
            }
        }
        self.start_reader();
        self.drop_offers();
        let _ = self.ws.send_json(&json!({"type": "flush"})).await;
    }

    /// Open the live ears while the owner reaches for the button; without them each press is uploaded.
    pub async fn open_ears(&self) {
        let Some(ears) = self.voice.live_ears() else { return };
        let mut guard = CloseOnDrop(Some(ears.clone()));
        let opened = ears.open().await;
        guard.0 = None;
        match opened {
            Ok(()) => *self.ears.lock() = Some(ears),
            Err(e) => {
                // no live session: each press is uploaded after release
                warn!("call: live transcription unavailable ({}); uploading presses instead", error_name(&e));
                ears.close().await;
            }
        }
    }

    fn drop_ears_if(&self, ears: &Arc<dyn Ears>) -> bool {
        let mut slot = self.ears.lock();
        if slot.as_ref().is_some_and(|e| same(e, ears)) {
            *slot = None;
            return true;
        }
        false
    }

    /// `talk`: a new press (a duplicate `talk` is not one).
    pub async fn begin_press(&self) {
        if self.st.lock().talking {
            return; // duplicate talk frames are not a new press
        }
        // Capture before yielding or taking the listener: a question can change during either operation.
        let captured = match self.brain.answer_context() {
            Some(ctx) => Captured::Context(ctx),
            None => Captured::Uncaptured,
        };
        {
            let mut st = self.st.lock();
            st.answer_context = captured;
            st.talking = true;
            st.press.clear();
            st.ears_live = false;
        }
        self.touch();
        self.take_chat(); // the first press, or the first after the desk took it
        self.interrupt().await;
        let ears = self.ears();
        if let Some(ears) = ears {
            if !ears.broken() {
                match ears.begin().await {
                    Ok(()) => self.st.lock().ears_live = true,
                    Err(e) => {
                        info!("call: live transcription dropped ({})", error_name(&e));
                        if self.drop_ears_if(&ears) {
                            ears.close().await;
                        }
                    }
                }
            }
        }
    }

    /// One binary frame of a press.
    pub async fn feed(&self, data: &[u8]) {
        let cap = (MAX_PRESS_SECS * BYTES_PER_SEC as f64) as usize;
        let (data, live) = {
            let mut st = self.st.lock();
            if !st.talking || st.press.len() >= cap {
                return;
            }
            let remaining = cap - st.press.len();
            let take = (data.len() - data.len() % 2).min(remaining);
            let data = data[..take].to_vec();
            st.press.extend_from_slice(&data);
            (data, st.ears_live)
        };
        if live {
            if let Some(ears) = self.ears() {
                if ears.feed(&data).await.is_err() {
                    ears.set_broken(); // the press is still kept whole, for the upload
                }
            }
        }
    }

    async fn transcribe(&self, pcm: Vec<u8>, live: bool) -> anyhow::Result<(String, bool)> {
        if live {
            if let Some(ears) = self.ears() {
                match ears.finish(6.0).await {
                    Ok(text) => return Ok((text, true)),
                    Err(e) => {
                        info!("call: live transcription failed ({}); uploading the press", error_name(&e));
                        if self.drop_ears_if(&ears) {
                            ears.close().await;
                        }
                    }
                }
            }
        }
        let voice = self.voice.clone();
        let text = eve_spend::to_thread(move || voice.transcribe(&pcm)).await?;
        Ok((text, false))
    }

    /// One press, done: its words, handed to the chat as the owner's.
    pub async fn heard(&self, pcm: Vec<u8>, live: bool, released: f64, answer_context: Captured) {
        if (pcm.len() as f64) < MIN_PRESS_SECS * BYTES_PER_SEC as f64 {
            if live {
                if let Some(ears) = self.ears() {
                    let _ = ears.discard().await; // nothing of a tap is left in the live session
                }
            }
            self.state_note("listening", "Hold the button while you talk.").await;
            return;
        }
        self.state("thinking", Map::new()).await;
        let seconds = pcm.len() as f64 / BYTES_PER_SEC as f64;
        let (mut text, was_live) = match self.transcribe(pcm, live).await {
            Ok(x) => x,
            Err(e) => {
                // the type only: the error can quote what was said
                warn!("call: transcription failed: {}", error_name(&e));
                self.state_note("listening", "I couldn't make that out. Try again?").await;
                return;
            }
        };
        if !text.is_empty() && foreign(&text, &self.voice.language()) {
            info!("call: heard {} character(s) not in the call's language; dropped", py::len(&text));
            text.clear();
        }
        if text.is_empty() {
            self.state_note("listening", "I didn't catch anything. Try again?").await;
            return;
        }
        let now = self.now();
        {
            let mut st = self.st.lock();
            st.presses += 1;
            st.timing = Timing { press: seconds, stt: now - released, live: was_live, ..Default::default() };
            st.released_at = Some(released);
        }
        let _ = self.ws.send_json(&json!({"type": "heard", "text": text})).await;
        self.text("user", &text);
        // The words are this call's, whichever call holds the chat's listener now: a desk call's `listening` stays
        // true after a phone call took the listener, so the brain is told which call heard them, marks the work with
        // this call's reader and gives this call the listener back (verification/Eve/CallScope.lean, the hearing
        // call). A brain whose hear takes no `call` is asked the old way.
        let call = if self.brain.takes_call() { Some(&self.reader) } else { None };
        self.brain.hear(&text, call, answer_context);
    }

    async fn run_loop(self: &Arc<Self>) -> String {
        let mut started = self.now();
        loop {
            if self.notes().is_some() {
                started = self.now(); // notes run to their own cap; the hour counts from when they end
            }
            let left = self.limits.max_call_secs() - (self.now() - started);
            if left <= 0.0 {
                return "the call reached its hour".into();
            }
            let quiet = self.limits.quiet_secs();
            let wait = if self.notes().is_some() { quiet } else { left.min(quiet) };
            let (op, data) = match tokio::time::timeout(secs(wait), self.ws.recv()).await {
                Err(_) => {
                    // the phone slept or the page closed without a goodbye (live 2026-09-25: a call left open that
                    // way refused the next one with "already on a call")
                    return if wait >= self.limits.quiet_secs() { QUIET.into() } else { "the call reached its hour".into() };
                }
                Ok(Err(_)) => return DROPPED.into(),
                Ok(Ok(x)) => x,
            };
            if op == OP_BIN {
                match self.notes() {
                    Some(notes) => notes.feed(&data), // notes: every frame is theirs, never a press
                    None => self.feed(&data).await,
                }
                continue;
            }
            let Some(msg) = std::str::from_utf8(&data).ok().and_then(|s| serde_json::from_str::<Value>(s).ok()) else {
                continue;
            };
            let kind = msg.as_object().and_then(|m| m.get("type")).and_then(Value::as_str).unwrap_or("").to_string();
            let notes = self.notes();
            if kind == "ping" {
                // While notes record, eve sends nothing else for an hour, so the phone cannot tell a live connection
                // from a half-open one (review, 2026-10-04): each ping gets a pong, and a phone that hears none for a
                // while takes it as a drop, holds its audio and calls back (CallSocket).
                if notes.is_some() && !self.ws.closed() {
                    let _ = self.ws.send_json(&json!({"type": "pong"})).await;
                }
                continue;
            }
            if let (Some(notes), "notes_stop" | "talk") = (&notes, kind.as_str()) {
                // The Action Button while notes are on stops them (a `talk` from a build that does not know
                // `notes_stop` too). The write-up then says "Notes saved." here and texts the notes.
                if notes.request_stop("you pressed stop on the phone") {
                    self.state_note("thinking", "Writing up your notes.").await;
                }
            } else if kind == "talk" {
                self.begin_press().await;
            } else if kind == "done" && self.talking() {
                // Freeze this press's context in its task, before another press overwrites it.
                let (pcm, live, ctx) = {
                    let mut st = self.st.lock();
                    st.talking = false;
                    let ctx = std::mem::take(&mut st.answer_context);
                    (std::mem::take(&mut st.press), st.ears_live, ctx)
                };
                let released = self.now();
                let me = self.clone();
                self.tasks.spawn(async move { me.heard(pcm, live, released, ctx).await });
            } else if kind == "end" {
                return HUNG_UP.into();
            }
        }
    }

    /// Until the phone hangs up or the hour is up. Returns why it ended.
    pub async fn run(self: &Arc<Self>) -> String {
        self.start_reader();
        {
            let me = self.clone();
            // while the owner reaches for the button
            *self.opening.lock() = Some(tokio::spawn(async move { me.open_ears().await }));
        }
        // Is there a chat to be in? Asked without taking the listener, which a desk call may hold: the call takes it
        // at its first press. (A brain without chat_ready is asked the old way, by taking it.)
        let ready = match self.brain.chat_ready() {
            Some(r) => r,
            None => self.take_chat(),
        };
        if !ready {
            self.stop_tasks().await;
            return "eve's chat isn't running".into();
        }
        let mut guard = RunGuard(Some(self.clone()));
        let reason = self.run_loop().await;
        guard.0 = None;
        // Only this call's own reader: another call may have taken the chat since (a phone call started during a
        // desk call), and its reader must outlive this call (verification/Eve/DeskCall.lean).
        self.brain.listen(None, Some(&self.reader));
        self.stop_tasks().await;
        reason
    }

    async fn stop_tasks(&self) {
        let mut pending = self.tasks.take_all();
        pending.extend(self.reading.lock().take());
        pending.extend(self.opening.lock().take());
        for h in &pending {
            h.abort();
        }
        for h in pending {
            let _ = h.await;
        }
        let ears = self.ears();
        if let Some(ears) = ears {
            ears.close().await;
        }
    }
}

// ---- the endpoint -------------------------------------------------------------------------------------------------

/// The `/api/call` endpoint: authenticates the phone, then runs one Call at a time.
pub struct PhoneCalls {
    check_owner: CheckOwner,
    brain: Option<Arc<dyn Brain>>,
    voice: Option<Arc<dyn Voice>>,
    clock: Clock,
    /// The waits (AUTH_SECS, QUIET_SECS, MAX_CALL_SECS), shared with every call this endpoint runs.
    pub limits: Arc<Limits>,
    active: Mutex<Option<Arc<Call>>>,
    /// Notes on the phone, when the daemon lends a note-taker (daemon._start_miniapp). They belong to the endpoint,
    /// not to one call, so that a call which drops and comes back carries on with the same notes.
    notes: Mutex<Option<Arc<PhoneNotes>>>,
    /// The expression picker's offer (daemon._start_miniapp): given to each call whose hello lists "eyes".
    on_text: Mutex<Option<OnText>>,
}

/// `_serve`'s `finally`, also when the serve is dropped (cancelled) mid-call.
struct ServeGuard<'a> {
    calls: &'a PhoneCalls,
    call: Arc<Call>,
    started: f64,
    reason: String,
    armed: bool,
}

impl ServeGuard<'_> {
    fn finish(&mut self) -> bool {
        self.armed = false;
        let call = &self.call;
        let taking_notes = call.notes().is_some_and(|n| n.recording());
        call.drop_offers();
        if let Some(notes) = self.calls.notes() {
            notes.detach(call, &self.reason);
        }
        *self.calls.active.lock() = None;
        info!("call: ended after {:.0} s, {} press(es)", self.calls.now() - self.started, call.presses());
        taking_notes
    }
}

impl Drop for ServeGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.finish();
        }
    }
}

fn has_can(can: Option<&Value>, what: &str) -> bool {
    can.and_then(Value::as_array).is_some_and(|items| items.iter().any(|v| v.as_str() == Some(what)))
}

impl PhoneCalls {
    pub fn new(check_owner: CheckOwner, brain: Option<Arc<dyn Brain>>, voice: Option<Arc<dyn Voice>>) -> Self {
        Self::with_clock(check_owner, brain, voice, default_clock())
    }

    pub fn with_clock(
        check_owner: CheckOwner,
        brain: Option<Arc<dyn Brain>>,
        voice: Option<Arc<dyn Voice>>,
        clock: Clock,
    ) -> Self {
        Self {
            check_owner,
            brain,
            voice,
            clock,
            limits: Arc::new(Limits::default()),
            active: Mutex::new(None),
            notes: Mutex::new(None),
            on_text: Mutex::new(None),
        }
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    /// The call on now, if any.
    pub fn active(&self) -> Option<Arc<Call>> {
        self.active.lock().clone()
    }

    pub fn set_active(&self, call: Option<Arc<Call>>) {
        *self.active.lock() = call;
    }

    pub fn notes(&self) -> Option<Arc<PhoneNotes>> {
        self.notes.lock().clone()
    }

    pub fn set_notes(&self, notes: Option<Arc<PhoneNotes>>) {
        *self.notes.lock() = notes;
    }

    pub fn on_text(&self) -> Option<OnText> {
        self.on_text.lock().clone()
    }

    pub fn set_on_text(&self, f: Option<OnText>) {
        *self.on_text.lock() = f;
    }

    /// The live Eve Link call that can draw expressions, while the owner is not holding the button; else None.
    pub fn eyes_call(&self) -> Option<Arc<Call>> {
        let call = self.active()?;
        if !call.can_eyes() || call.talking() || call.ws.closed() {
            return None;
        }
        Some(call)
    }

    pub fn eyes_live(&self) -> bool {
        self.eyes_call().is_some()
    }

    /// The picker's send (live_expressions.LiveExpressions): its `{"cmd": "expression", ...}` as the phone's
    /// `{"type": "expression", "id", "label", "ttl_ms"}`, or `{"type": "expression", "id", "clear": true}`. False
    /// when no call can take it, so the pick counts as dropped and nothing else is sent.
    pub async fn send_expression(&self, cmd: &Value) -> bool {
        let Some(call) = self.eyes_call() else { return false };
        if !py::is_int(cmd.get("id")) {
            return false;
        }
        let id = cmd["id"].clone();
        let msg = if cmd.get("clear").is_some_and(eve_records::py::truthy) {
            json!({"type": "expression", "id": id, "clear": true})
        } else if cmd.get("label").is_some_and(Value::is_string) && py::is_int(cmd.get("ttl_ms")) {
            json!({"type": "expression", "id": id, "label": cmd["label"], "ttl_ms": cmd["ttl_ms"]})
        } else {
            return false;
        };
        match msg {
            Value::Object(m) => call.send_expression(m).await,
            _ => false,
        }
    }

    /// take_notes, when it is the phone's to answer (telegram.py and the `notes` command); None when it is the
    /// desk's note-taker's (daemon._room_notes_taker).
    ///
    /// `start` is the phone's when it was said on the Eve Link call that holds the chat (`call_say` is that call's
    /// reader): a Mini App call, the desk or a text start the room's notes as before. `stop` and `status` are the
    /// phone's while its notes are on, from anywhere.
    ///
    /// `later` runs a callable once the turn that asked is over (telegram.py's end of turn). The call lets the chat's
    /// listener go only then: let go inside the turn, the turn's own reply ("Taking notes.") found no call to be read
    /// on and was texted instead (review, 2026-10-04). Without `later` the listener goes at once. Python's default
    /// `reason` is "asked from Telegram".
    pub async fn notes_action(
        &self,
        action: &str,
        call_say: Option<&Reader>,
        reason: &str,
        later: Option<Later>,
    ) -> Option<Value> {
        let notes = self.notes()?;
        if action == "start" {
            let call = self.active()?;
            let call_say = call_say?;
            if !call.from_app() || call.reader() != *call_say {
                return None;
            }
            return Some(notes.start(&call, later).await);
        }
        if !notes.active() {
            return None;
        }
        if action == "stop" {
            return Some(notes.stop(reason, false).await);
        }
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.extend(notes.status());
        Some(Value::Object(out))
    }

    /// Whether a phone call has the chat and is using it; an idle one gives it back first
    /// ([`Call::release_if_idle`]). The desk's button waits only on this (daemon._desk_calls_get's phone_busy).
    pub fn holding_chat(&self) -> bool {
        self.active().is_some_and(|call| !call.release_if_idle())
    }

    /// One call. With `token_check` it is the Eve Link app's (stick_link.py): its first message carries `token`
    /// instead of `initData`, and only that is accepted.
    pub async fn serve<R, W>(
        &self,
        reader: R,
        writer: W,
        headers: &HashMap<String, String>,
        token_check: Option<TokenCheck>,
    ) -> anyhow::Result<()>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let ws = Arc::new(WebSocket::accept(reader, writer, headers).await?);
        let result = self.serve_inner(ws.clone(), token_check).await;
        ws.close(1000).await;
        match result {
            Err(e) if e.downcast_ref::<Closed>().is_some() => Ok(()),
            other => other,
        }
    }

    /// One call on a socket that is already open: `serve` without the WebSocket handshake. eve lite's WebRTC call
    /// (lite_rtc.RtcCallSocket: a data channel for the messages, the peer's audio track for the sound) is one. Its
    /// first message is the hello. The hello's check, one call at a time and the end are `serve`'s own.
    pub async fn serve_socket(&self, ws: Arc<dyn CallSocket>, token_check: Option<TokenCheck>) -> anyhow::Result<()> {
        let result = self.serve_inner(ws.clone(), token_check).await;
        ws.close(1000).await;
        match result {
            Err(e) if e.downcast_ref::<Closed>().is_some() => Ok(()),
            other => other,
        }
    }

    async fn serve_inner(&self, ws: Arc<dyn CallSocket>, token_check: Option<TokenCheck>) -> anyhow::Result<()> {
        let hello: Value = match tokio::time::timeout(secs(self.limits.auth_secs()), ws.recv()).await {
            Err(_) => json!({}),
            Ok(Err(closed)) => return Err(closed.into()),
            Ok(Ok((op, data))) => {
                if op == OP_TEXT {
                    std::str::from_utf8(&data)
                        .ok()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or_else(|| json!({}))
                } else {
                    json!({})
                }
            }
        };
        let hello = hello.as_object();
        let can = hello.and_then(|h| h.get("can"));
        let user = match hello {
            Some(h) => match &token_check {
                Some(check) => check(&py::str_or_empty(h.get("token"))).then(|| "eve link".to_string()),
                None => (self.check_owner)(&py::str_or_empty(h.get("initData"))).map(|id| id.to_string()),
            },
            None => None,
        };
        if user.is_none() {
            ws.send_json(&json!({"type": "ended", "reason": "Only eve's owner can call."})).await?;
            return Ok(());
        }
        let (Some(brain), Some(voice)) = (self.brain.clone(), self.voice.clone()) else {
            ws.send_json(&json!({"type": "ended", "reason": "Calls aren't set up on this computer."})).await?;
            return Ok(());
        };
        if let Some(old) = self.active() {
            // the owner calling again means the old call is over (a closed page, a lost connection): end it
            info!("call: a new call replaces the one still open");
            old.ws.close(1000).await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while self.active().is_some_and(|a| Arc::ptr_eq(&a, &old)) {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .map_err(|_| EarsError::Timeout)?;
        }
        let call = Call::build(ws.clone(), brain, voice, self.clock.clone(), self.limits.clone());
        let from_app = token_check.is_some();
        call.set_from_app(from_app);
        call.set_can_notes(from_app && has_can(can, NOTES_CAN));
        call.set_can_eyes(from_app && has_can(can, EYES_CAN));
        call.set_on_text(if call.can_eyes() { self.on_text() } else { None });
        *self.active.lock() = Some(call.clone());
        info!("call: the owner is on a call from the phone");
        let mut guard =
            ServeGuard { calls: self, call: call.clone(), started: self.now(), reason: DROPPED.into(), armed: true };
        let result: Result<String, Closed> = async {
            ws.send_json(&json!({"type": "state", "state": "connected"})).await?;
            if let Some(notes) = self.notes() {
                if notes.recording() && call.can_notes() {
                    notes.attach(&call, true).await; // the phone called back: carry on with the same notes
                }
            }
            Ok(call.run().await)
        }
        .await;
        if let Ok(reason) = &result {
            guard.reason = reason.clone();
        }
        let taking_notes = guard.finish();
        drop(guard);
        let reason = result?;
        if taking_notes && reason != HUNG_UP {
            // A call that went quiet while notes record is a drop, not an end eve chose: the phone treats any `ended`
            // as final and would give the notes up (review, 2026-10-04). So the socket just closes, the phone sees a
            // lost connection, holds its audio and calls back within the grace (detach above).
            ws.close(1001).await;
            return Ok(());
        }
        if !ws.closed() {
            let _ = ws.send_json(&json!({"type": "ended", "reason": reason})).await;
        }
        Ok(())
    }
}

// ---- notes on the phone (owner, 2026-10-04) -------------------------------------------------------------------------

/// The owner's chat (telegram.tell_owner): `(text, about)`.
pub type NotesNotify = Arc<dyn Fn(String, String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// Runs a callable once the turn that asked is over (telegram.py's end of turn).
pub type Later = Box<dyn FnOnce(Box<dyn FnOnce() + Send + 'static>) + Send>;
/// notes.PushedAudio → the session recording it (notes.RoomNotes).
pub type MakeTaker = Arc<dyn Fn(Arc<PushedAudio>) -> Arc<dyn NotesTaker> + Send + Sync>;
/// The wall clock in epoch seconds (`time.time`).
pub type Wall = Arc<dyn Fn() -> f64 + Send + Sync>;

fn paragraphs(summary: &Map<String, Value>, speakers: Option<&[String]>) -> Vec<String> {
    let bullets = |rows: &[String]| rows.iter().map(|r| format!("• {r}")).collect::<Vec<_>>().join("\n");
    let mut out = Vec::new();
    for (heading, rows) in eve_notes::note_sections(summary, speakers) {
        if heading == "Topics" {
            for row in rows {
                match row {
                    Row::Topic(topic, points) if !points.is_empty() => out.push(format!("{topic}:\n{}", bullets(&points))),
                    Row::Topic(topic, _) => out.push(topic),
                    Row::Text(t) => out.push(t),
                }
            }
            continue;
        }
        let texts: Vec<String> = rows
            .into_iter()
            .map(|r| match r {
                Row::Text(t) => t,
                Row::Topic(t, _) => t,
            })
            .collect();
        out.push(format!("{heading}:\n{}", bullets(&texts)));
    }
    out
}

/// Paragraphs into messages of at most `limit` characters, cut between paragraphs, and between lines only when one
/// paragraph is longer than a message.
fn pack(paragraphs: &[String], limit: usize) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    let mut current = String::new();
    for para in paragraphs {
        let mut chunks = vec![para.clone()];
        if py::len(para) > limit {
            chunks.clear();
            let mut line_run = String::new();
            for line in para.split('\n') {
                let mut line = line.to_string();
                while py::len(&line) > limit {
                    if !line_run.is_empty() {
                        chunks.push(std::mem::take(&mut line_run));
                    }
                    chunks.push(py::head(&line, limit));
                    line = py::tail(&line, limit);
                }
                if !line_run.is_empty() && py::len(&line_run) + 1 + py::len(&line) > limit {
                    chunks.push(std::mem::take(&mut line_run));
                }
                line_run = if line_run.is_empty() { line } else { format!("{line_run}\n{line}") };
            }
            if !line_run.is_empty() {
                chunks.push(line_run);
            }
        }
        for chunk in chunks {
            if !current.is_empty() && py::len(&current) + 2 + py::len(&chunk) > limit {
                pieces.push(std::mem::take(&mut current));
            }
            current = if current.is_empty() { chunk } else { format!("{current}\n\n{chunk}") };
        }
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The notes as texts to the owner: the title with the length and the number of voices, the summary, and every
/// section of the notes (notes.note_sections), in as many messages as it takes, each under Telegram's limit. The
/// last one says where the full transcript is, since the point of a recording is being able to go back to what was
/// actually said. More than [`NOTES_MAX_PIECES`] messages of notes end in the file.
///
/// When the file was not written (a full disk, a folder it cannot write), the texts are the only copy: they say so
/// and carry the transcript itself, rather than claiming a saved file that does not exist (review, 2026-10-04).
/// Python's defaults: `transcript=""`, `speakers=None`, `limit=NOTES_PIECE_CHARS`.
pub fn notes_messages(
    summary: &Map<String, Value>,
    lines: usize,
    minutes: f64,
    path: Option<&Path>,
    transcript: &str,
    speakers: Option<&[String]>,
    limit: usize,
) -> Vec<String> {
    if lines == 0 {
        return vec!["I took notes on your phone, but nothing was said, so there are no notes.".into()];
    }
    let title = py::strip(&py::str_or_empty(summary.get("title"))).trim_end_matches('.').to_string();
    let mut length = format!("{} min", py::round_int(minutes).max(1));
    if let Some(sp) = speakers {
        length += &format!(", {} {}", sp.len(), if sp.len() == 1 { "speaker" } else { "speakers" });
    }
    let mut paras = vec![format!(
        "Notes from your phone{} ({length}).",
        if title.is_empty() { String::new() } else { format!(": {title}") }
    )];
    let gist_raw = match summary.get("summary") {
        Some(v) if eve_records::py::truthy(v) => Some(v),
        _ => summary.get("gist"),
    };
    let gist = py::strip(&py::str_or_empty(gist_raw)).to_string();
    if !gist.is_empty() {
        paras.push(gist);
    }
    paras.extend(paragraphs(summary, speakers));
    if summary.is_empty() {
        paras.push(format!(
            "The write-up failed; the transcript ({lines} lines) is {}",
            if path.is_some() { "saved." } else { "below." }
        ));
    }
    let Some(path) = path else {
        paras.push("I could not save them to a file, so these messages are the only copy. What was said:".into());
        paras.extend(transcript.split('\n').filter(|p| !py::strip(p).is_empty()).map(String::from));
        return pack(&paras, limit);
    };
    let place = format!("The full transcript is in {}, in the meeting notes.", file_name(path));
    let mut pieces = pack(&paras, limit);
    if pieces.len() > NOTES_MAX_PIECES {
        pieces.truncate(NOTES_MAX_PIECES);
        if let Some(last) = pieces.last_mut() {
            last.push_str("\n\nThere is more than fits here: the rest of the notes are in the file.");
        }
    }
    match pieces.last_mut() {
        Some(last) if py::len(last) + 2 + py::len(&place) <= limit => {
            last.push_str("\n\n");
            last.push_str(&place);
        }
        _ => pieces.push(place),
    }
    pieces
}

/// notes_messages as one text (the pieces, a blank line apart).
pub fn notes_message(
    summary: &Map<String, Value>,
    lines: usize,
    minutes: f64,
    path: Option<&Path>,
    transcript: &str,
    speakers: Option<&[String]>,
) -> String {
    notes_messages(summary, lines, minutes, path, transcript, speakers, NOTES_PIECE_CHARS).join("\n\n")
}

/// What [`PhoneNotes`] needs of a note-taking session (notes.RoomNotes is the real one).
pub trait NotesTaker: Send + Sync {
    fn start(&self) -> Value;
    fn active(&self) -> bool;
    fn stopped_by(&self) -> String;
    fn words(&self) -> usize;
    fn path(&self) -> Option<PathBuf>;
    fn status(&self) -> Value;
    fn request_stop(&self, reason: &str);
    fn set_quick(&self, quick: bool);
    fn minutes(&self) -> f64;
    /// The running session, done once the file is written.
    fn task(&self) -> Option<Shared<BoxFuture<'static, ()>>>;
    fn lines(&self) -> Vec<(f64, String)>;
    fn summary(&self) -> Map<String, Value>;
    fn transcript(&self) -> String;
    fn speakers(&self) -> Vec<String>;
    fn diarized(&self) -> String;
    fn close(&self);
}

impl NotesTaker for eve_notes::RoomNotes {
    fn start(&self) -> Value {
        eve_notes::RoomNotes::start(self)
    }
    fn active(&self) -> bool {
        eve_notes::RoomNotes::active(self)
    }
    fn stopped_by(&self) -> String {
        self.state().stopped_by
    }
    fn words(&self) -> usize {
        self.state().words
    }
    fn path(&self) -> Option<PathBuf> {
        self.state().path
    }
    fn status(&self) -> Value {
        eve_notes::RoomNotes::status(self)
    }
    fn request_stop(&self, reason: &str) {
        eve_notes::RoomNotes::request_stop(self, reason)
    }
    fn set_quick(&self, quick: bool) {
        eve_notes::RoomNotes::set_quick(self, quick)
    }
    fn minutes(&self) -> f64 {
        eve_notes::RoomNotes::minutes(self)
    }
    fn task(&self) -> Option<Shared<BoxFuture<'static, ()>>> {
        eve_notes::RoomNotes::task(self)
    }
    fn lines(&self) -> Vec<(f64, String)> {
        eve_notes::RoomNotes::lines(self)
    }
    fn summary(&self) -> Map<String, Value> {
        eve_notes::RoomNotes::summary(self)
    }
    fn transcript(&self) -> String {
        eve_notes::RoomNotes::transcript(self)
    }
    fn speakers(&self) -> Vec<String> {
        eve_notes::RoomNotes::speakers(self)
    }
    fn diarized(&self) -> String {
        eve_notes::RoomNotes::diarized(self)
    }
    fn close(&self) {
        eve_notes::RoomNotes::close(self)
    }
}

#[derive(Default)]
struct NotesSlots {
    taker: Option<Arc<dyn NotesTaker>>,
    mic: Option<Arc<PushedAudio>>,
    call: Option<Arc<Call>>,
    started_wall: f64,
    grace: Option<JoinHandle<()>>,
    watch: Option<(AbortHandle, Shared<BoxFuture<'static, ()>>)>,
}

/// Notes on the phone: one note-taking session (notes.RoomNotes) fed from an Eve Link call, not the desk.
///
/// * **Start.** take_notes said on an Eve Link call ([`PhoneCalls::notes_action`]) builds a session whose
///   microphone is a notes.PushedAudio, and attaches it to the call: the phone is told
///   `{"type":"state","state":"notes","started":<unix seconds>,"words":<so far>}`, every audio frame it then sends is
///   the notes' (not a press), and the call lets the chat's listener go, so a texted reply is not read out into the
///   recording. A call whose hello did not list `"notes"` (an Eve Link build from before) is refused with the
///   reason: it would record silence.
/// * **Stop.** `notes_stop` (the Action Button), a `talk`, "stop taking notes" said into the phone, the End button,
///   take_notes stop from the chat or `eve-bridge notes stop`. The phone is told
///   `{"type":"state","state":"thinking","note":"Writing up your notes."}`; once the file is written it is told
///   `{"type":"state","state":"listening","note":"Notes saved: <title>"}` and eve says "Notes saved." on the call,
///   and the notes are texted to the owner.
/// * **A dropped call.** The session is the endpoint's, not the call's, so it outlives the call: what was heard
///   stays heard, and the session waits on an empty microphone. The same phone calling back within
///   [`NOTES_GRACE_SECS`] is attached again and carries on; otherwise the notes are written up with what there is,
///   `stopped_by` "the phone dropped", and texted.
///
/// The session's tasks run in a fresh task (tokio carries no task-locals into `spawn`): take_notes runs inside the
/// call's turn, where telegram.py marks work as the call's so that it is read out, and the notes texted at the end
/// would otherwise be read out whole on the call, an hour after the turn was over.
pub struct PhoneNotes {
    /// (notes.PushedAudio) → notes.RoomNotes, recording that microphone.
    pub make_taker: MakeTaker,
    pub notify: Option<NotesNotify>,
    pub wall: Wall,
    grace_secs: AtomicU64,
    me: Weak<PhoneNotes>,
    slots: Mutex<NotesSlots>,
}

impl PhoneNotes {
    pub fn new(make_taker: MakeTaker, notify: Option<NotesNotify>) -> Arc<Self> {
        Self::with_wall(make_taker, notify, Arc::new(wall_now))
    }

    pub fn with_wall(make_taker: MakeTaker, notify: Option<NotesNotify>, wall: Wall) -> Arc<Self> {
        Arc::new_cyclic(|me| PhoneNotes {
            make_taker,
            notify,
            wall,
            grace_secs: AtomicU64::new(NOTES_GRACE_SECS.to_bits()),
            me: me.clone(),
            slots: Mutex::new(NotesSlots::default()),
        })
    }

    /// How long a dropped call is waited for ([`NOTES_GRACE_SECS`]).
    pub fn grace_secs(&self) -> f64 {
        f64::from_bits(self.grace_secs.load(Ordering::SeqCst))
    }

    pub fn set_grace_secs(&self, v: f64) {
        self.grace_secs.store(v.to_bits(), Ordering::SeqCst)
    }

    /// The session recording now, if any.
    pub fn taker(&self) -> Option<Arc<dyn NotesTaker>> {
        self.slots.lock().taker.clone()
    }

    pub fn set_taker(&self, taker: Option<Arc<dyn NotesTaker>>) {
        self.slots.lock().taker = taker;
    }

    /// The call the notes are attached to.
    pub fn call(&self) -> Option<Arc<Call>> {
        self.slots.lock().call.clone()
    }

    /// Whether a dropped call is being waited for (`grace is not None`).
    pub fn waiting_for_phone(&self) -> bool {
        self.slots.lock().grace.is_some()
    }

    /// A session is on: recording, or being written up.
    pub fn active(&self) -> bool {
        self.slots.lock().watch.as_ref().is_some_and(|(h, _)| !h.is_finished())
    }

    pub fn recording(&self) -> bool {
        let taker = self.taker();
        self.active() && taker.is_some_and(|t| t.active() && t.stopped_by().is_empty())
    }

    pub fn progress(&self) -> Map<String, Value> {
        let (taker, started) = {
            let s = self.slots.lock();
            (s.taker.clone(), s.started_wall)
        };
        let words = taker.map(|t| t.words()).unwrap_or(0);
        let mut out = Map::new();
        out.insert("started".into(), json!(py::round_to(started, 3)));
        out.insert("words".into(), json!(words));
        out
    }

    /// Counts only: the session's `last` (the words last heard) is left out, since this goes to the chat model as a
    /// tool result and the words are anyone's in a public place (the reason `about` never carries them).
    pub fn status(&self) -> Map<String, Value> {
        let (taker, attached) = {
            let s = self.slots.lock();
            (s.taker.clone(), s.call.is_some())
        };
        let raw = match taker {
            Some(t) => t.status(),
            None => json!({"active": false}),
        };
        let mut out: Map<String, Value> = match raw {
            Value::Object(m) => m.into_iter().filter(|(k, _)| k != "last").collect(),
            _ => Map::new(),
        };
        out.insert("on".into(), json!("your phone"));
        out.insert("attached".into(), json!(attached));
        out
    }

    pub async fn start(&self, call: &Arc<Call>, later: Option<Later>) -> Value {
        if self.active() {
            let mut out = Map::new();
            out.insert("ok".into(), json!(true));
            out.insert("already".into(), json!(true));
            out.insert("on".into(), json!("your phone"));
            out.extend(self.status());
            return Value::Object(out);
        }
        if !call.can_notes() {
            return json!({"ok": false, "reason": "this Eve Link app cannot take notes yet; update it, or text me to \
                                                  take notes on the Mac's microphone instead"});
        }
        let mic = Arc::new(PushedAudio::new());
        let taker = (self.make_taker)(mic.clone());
        let result = taker.start();
        if !result.get("ok").is_some_and(eve_records::py::truthy) {
            return result;
        }
        {
            let mut s = self.slots.lock();
            s.taker = Some(taker.clone());
            s.mic = Some(mic);
            s.started_wall = (self.wall)();
        }
        if let Some(me) = self.me.upgrade() {
            let handle = tokio::spawn(async move { me.watch(taker).await });
            let abort = handle.abort_handle();
            let shared: Shared<BoxFuture<'static, ()>> = handle.map(|_| ()).boxed().shared();
            self.slots.lock().watch = Some((abort, shared));
        }
        let release_now = later.is_none();
        self.attach(call, release_now).await;
        if let Some(later) = later {
            let call = call.clone();
            later(Box::new(move || call.release_for_notes())); // once this turn's reply has been read out
        }
        info!("call: taking notes on the phone");
        let mut out = match result {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        out.insert("on".into(), json!("your phone"));
        out.insert("stop".into(), json!(NOTES_STOP_HINT));
        Value::Object(out)
    }

    /// This call is the notes' microphone: from the start, or the phone calling back after a drop. A press the notes
    /// cut off is dropped here as the phone drops it ([`Call::drop_press`]).
    pub async fn attach(&self, call: &Arc<Call>, release: bool) {
        {
            let mut s = self.slots.lock();
            if let Some(g) = s.grace.take() {
                g.abort();
            }
            s.call = Some(call.clone());
        }
        call.set_notes(self.me.upgrade());
        call.drop_press();
        if release {
            call.release_chat();
        }
        call.rest().await;
    }

    /// The call ended. The End button stops the notes; anything else waits for the phone to call back.
    pub fn detach(&self, call: &Arc<Call>, reason: &str) {
        {
            let mut s = self.slots.lock();
            if !s.call.as_ref().is_some_and(|c| Arc::ptr_eq(c, call)) {
                return;
            }
            s.call = None;
        }
        call.set_notes(None);
        if !self.recording() {
            return;
        }
        if reason == HUNG_UP {
            self.request_stop("you ended the call");
            return;
        }
        let grace = self.grace_secs();
        info!("call: dropped while taking notes ({reason}); waiting {grace:.0} s for the phone");
        if let Some(me) = self.me.upgrade() {
            let handle = tokio::spawn(async move {
                tokio::time::sleep(secs(grace)).await;
                if me.call().is_none() {
                    me.request_stop("the phone dropped");
                }
            });
            self.slots.lock().grace = Some(handle);
        }
    }

    pub fn feed(&self, pcm: &[u8]) {
        let mic = self.slots.lock().mic.clone();
        if let Some(mic) = mic {
            mic.put(pcm);
        }
    }

    /// Stop recording now; the write-up follows on its own (`watch`). Returns whether it was recording.
    pub fn request_stop(&self, reason: &str) -> bool {
        if !self.recording() {
            return false;
        }
        if let Some(t) = self.taker() {
            t.request_stop(reason);
        }
        true
    }

    /// Stop and wait for the write-up and the text (take_notes stop, the `notes` command). `quick` (the daemon
    /// stopping) writes the transcript with no summary and waits only [`NOTES_SHUTDOWN_SECS`].
    pub async fn stop(&self, reason: &str, quick: bool) -> Value {
        if !self.active() {
            return json!({"ok": false, "error": "eve is not taking notes on your phone"});
        }
        let (taker, watch, call) = {
            let s = self.slots.lock();
            (s.taker.clone(), s.watch.clone(), s.call.clone())
        };
        if quick {
            if let Some(t) = &taker {
                t.set_quick(true);
            }
        }
        if self.request_stop(reason) {
            if let Some(call) = &call {
                call.state_note("thinking", "Writing up your notes.").await;
            }
        }
        if let Some((_, fut)) = &watch {
            let wait = if quick { NOTES_SHUTDOWN_SECS } else { SUMMARY_WAIT_SECS };
            let _ = tokio::time::timeout(secs(wait), fut.clone()).await;
        }
        let path = taker.as_ref().and_then(|t| t.path());
        let texted = self.notify.is_some() && watch.as_ref().is_some_and(|(h, _)| h.is_finished());
        json!({"ok": true, "on": "your phone",
               "path": path.map(|p| p.to_string_lossy().into_owned()),
               "minutes": py::round_to(taker.as_ref().map(|t| t.minutes()).unwrap_or(0.0), 1),
               "words": taker.as_ref().map(|t| t.words()).unwrap_or(0),
               "texted": texted})
    }

    /// Wait for the session to write its file, whatever stopped it, then tell the phone and the owner.
    async fn watch(&self, taker: Arc<dyn NotesTaker>) {
        if let Some(task) = taker.task() {
            task.await;
        }
        let lines = taker.lines();
        let path = taker.path();
        let (minutes, words) = (taker.minutes(), taker.words());
        let summary = taker.summary();
        let title = {
            let t = match summary.get("title") {
                Some(v) if eve_records::py::truthy(v) => eve_records::py::str_of(v),
                _ => "your notes".to_string(),
            };
            py::strip(&t).trim_end_matches('.').to_string()
        };
        let call = {
            let mut s = self.slots.lock();
            if let Some(g) = s.grace.take() {
                g.abort();
            }
            s.taker = None;
            s.mic = None;
            s.call.take()
        };
        if let Some(call) = call {
            call.set_notes(None);
            if !lines.is_empty() && path.is_none() {
                // The file was not written: never "saved". The text below carries the transcript instead.
                call.state_note("listening", "Notes not saved to a file; I texted them to you.").await;
                call.say("I couldn't save the notes to a file, so I texted them to you.");
            } else if !lines.is_empty() {
                call.state_note("listening", &format!("Notes saved: {title}")).await;
                call.say("Notes saved.");
            } else {
                call.state_note("listening", "Nothing was said, so there are no notes.").await;
                call.say("Nothing was said, so there are no notes.");
            }
        }
        if let Some(notify) = &self.notify {
            let mins = py::round_int(minutes).max(1);
            let about = format!("I texted the owner the notes they took on their phone ({mins} min, {words} words)")
                + &match &path {
                    Some(p) => format!(
                        "; they are saved as {} in the meeting notes, where memory_search finds them.",
                        file_name(p)
                    ),
                    None => ".".into(),
                };
            let transcript = if path.is_none() {
                let t = taker.transcript();
                if t.is_empty() {
                    lines.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>().join("\n")
                } else {
                    t
                }
            } else {
                String::new()
            };
            let speakers = if taker.diarized() != "no" { Some(taker.speakers()) } else { None };
            let pieces = notes_messages(
                &summary,
                lines.len(),
                minutes,
                path.as_deref(),
                &transcript,
                speakers.as_deref(),
                NOTES_PIECE_CHARS,
            );
            let n = pieces.len();
            for (i, piece) in pieces.into_iter().enumerate() {
                let about_i = if n == 1 { about.clone() } else { format!("{about} (message {} of {n})", i + 1) };
                if let Err(e) = notify(piece, about_i).await {
                    // the file is written either way
                    warn!("call: the phone's notes were not texted ({})", error_name(&e));
                    break;
                }
            }
        }
        taker.close();
    }
}

#[cfg(test)]
#[path = "test_support.rs"]
mod test_support;
#[cfg(test)]
#[path = "tests_call.rs"]
mod tests_call;
#[cfg(test)]
#[path = "tests_turns.rs"]
mod tests_turns;
#[cfg(test)]
#[path = "tests_serve_socket.rs"]
mod tests_serve_socket;
#[cfg(test)]
#[path = "tests_approval.rs"]
mod tests_approval;
#[cfg(test)]
#[path = "tests_notes.rs"]
mod tests_notes;
#[cfg(test)]
#[path = "tests_expressions.rs"]
mod tests_expressions;
