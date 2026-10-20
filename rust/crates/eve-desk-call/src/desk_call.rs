//! Hold to talk on the board: the Telegram Mini App's push-to-talk call, on the desk.
//!
//! The owner asked on 2026-09-26 for the Voice PE's button to work "like telegram": hold it, talk, let go, and
//! eve answers. That is phone_call's [`Call`] (each press is one message to the same brain the owner texts, every
//! reply read back) with the phone swapped for the desk:
//!
//! * **The button** (`{"cmd":"ptt","on":bool}` from the board) is the phone's `talk` / `done`.
//! * **The Mac microphone** (the same device the wake word uses, `EVE_MIC`) is the phone's audio. It is opened
//!   only while the button is held.
//! * **The speaker** plays eve's reply, where the phone played it: the Voice PE's own speaker when it is the
//!   connected controller ([`BoardSpeaker`], streamed as `{"cmd":"pcm"}` lines; owner, 2026-09-26: "route the
//!   sound of this through voice pe"), else the Mac's ([`Speaker`]).
//! * **The board's ring** shows the call's state through `{"cmd":"agent","state":..}`: listening, thinking,
//!   speaking.
//! * **The head follower** (follow.py) hears the call as a conversation through `on_phase`: `listening` from the
//!   call's start (between presses too, since the person is still there), `thinking` and `speaking` from the
//!   ring, `idle` when the call ends. Without it eve never looked at whoever was talking on a desk call: the
//!   ring's states went straight to the board and never reached the follower.
//!
//! [`DeskLink`] is the stand-in for phone_call's WebSocket: the only change to the call itself is where its bytes
//! come from and go to. A call starts on the first press and ends after `phone_call::QUIET_SECS` without one, like
//! a phone that went quiet; the next press starts a new one. One call at a time across phone and desk: the chat
//! has a single listener.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex as StdMutex};
use std::thread::JoinHandle as ThreadHandle;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use async_trait::async_trait;
use base64::Engine;
use eve_ears::{find_input_device, input_device_name, open_input, AudioBackend, CpalBackend, InputSpec, InputStream};
use eve_phone_call::{Brain, Call, CallSocket, Closed, Limits, Voice, BYTES_PER_SEC, OP_BIN, OP_TEXT, SAMPLE_RATE};
use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::audio_out::{open_default_output, OutputOpener, RawOutput};

/// Mic block handed to the call: 50 ms, 1200 samples (2400 bytes).
pub const BLOCK_SECS: f64 = 0.05;

/// 100 ms of 24 kHz mono int16 per `{"cmd":"pcm"}` line (6.4 KB of base64).
pub const PCM_PIECE_BYTES: usize = 4800;
/// How far ahead of the board's playback the daemon may be; its buffer holds 12 s.
pub const LEAD_SECS: f64 = 3.0;

/// The phone pings every 10 s; the desk pings while eve is still working on a reply.
pub const PING_SECS: f64 = 10.0;

/// The call's states on the ring (firmware agent states). The call's "listening" means waiting for the next
/// press, so the ring goes quiet; it turns blue only while the button is held (`DeskCalls::press`).
pub const RING: &[(&str, &str)] = &[("thinking", "thinking"), ("speaking", "speaking")];

/// `RING.get(state, "idle")`.
pub fn ring_state(state: &str) -> &'static str {
    RING.iter().find(|(k, _)| *k == state).map(|(_, v)| *v).unwrap_or("idle")
}

/// A line to the board (`Send` in the Python): the controller's send. Its error reaches the caller.
pub type BoardSend = Arc<dyn Fn(Value) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// The head follower's phase hook (`Phase`).
pub type Phase = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// A monotonic clock in seconds (`time.monotonic`).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
/// `asyncio.sleep`.
pub type Sleep = Arc<dyn Fn(f64) -> BoxFuture<'static, ()> + Send + Sync>;
/// A block of microphone audio (24 kHz mono int16 bytes), called from any thread.
pub type OnBlock = Arc<dyn Fn(Vec<u8>) + Send + Sync>;
/// `mic_factory(device, on_block)`.
pub type MicFactory = Arc<dyn Fn(Option<String>, OnBlock) -> Box<dyn DeskMic> + Send + Sync>;
/// `speaker_factory()`.
pub type SpeakerFactory = Arc<dyn Fn() -> Arc<dyn DeskSpeaker> + Send + Sync>;
/// Whether the chat already has a listener (a phone call).
pub type Busy = Arc<dyn Fn() -> bool + Send + Sync>;

fn monotonic() -> f64 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_secs_f64()
}

fn secs(x: f64) -> Duration {
    Duration::from_secs_f64(if x.is_finite() && x > 0.0 { x } else { 0.0 })
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str(value)` for the log: a string as it is, anything else as JSON.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => other.to_string(),
    }
}

// ---- the Mac microphone ---------------------------------------------------------------------------------------

/// What the desk needs of a microphone (`Mic`'s duck type in the tests).
pub trait DeskMic: Send {
    fn open(&mut self) -> bool;
    fn close(&mut self);
}

/// sounddevice is one module for the whole daemon: one device table, refreshed in place.
static AUDIO: LazyLock<Arc<dyn AudioBackend>> = LazyLock::new(|| Arc::new(CpalBackend::new()));

/// The process's one audio backend (sounddevice's module state): the daemon hands the same one to the wake-word
/// ears (`eve_ears::Ears::with_backend`), so a refresh here is a refresh there, as in Python.
pub fn audio_backend() -> Arc<dyn AudioBackend> {
    AUDIO.clone()
}

/// The Mac microphone while the button is held: 24 kHz mono int16 blocks to `on_block` (any thread).
pub struct Mic {
    pub device: Option<String>,
    pub on_block: OnBlock,
    backend: Arc<dyn AudioBackend>,
    stream: Option<Box<dyn InputStream>>,
}

impl Mic {
    pub fn new(device: Option<String>, on_block: OnBlock) -> Self {
        Self::with_backend(device, on_block, AUDIO.clone())
    }

    pub fn with_backend(device: Option<String>, on_block: OnBlock, backend: Arc<dyn AudioBackend>) -> Self {
        Mic { device, on_block, backend, stream: None }
    }

    /// The default factory: `Mic(device, on_block)`.
    pub fn factory() -> MicFactory {
        Arc::new(|device, on_block| Box::new(Mic::new(device, on_block)) as Box<dyn DeskMic>)
    }

    pub fn is_open(&self) -> bool {
        self.stream.is_some()
    }

    fn try_open(&self) -> anyhow::Result<Box<dyn InputStream>> {
        let backend = self.backend.as_ref();
        backend.available().map_err(|e| anyhow!(e))?;
        let dev = match self.device.as_deref().filter(|d| !d.is_empty()) {
            Some(needle) => {
                // _find_input_device raises when the device list cannot be read
                backend.query_devices().map_err(|e| anyhow!(e))?;
                find_input_device(backend, needle)
            }
            None => None,
        };
        let spec = InputSpec { sample_rate: SAMPLE_RATE, block_frames: (f64::from(SAMPLE_RATE) * BLOCK_SECS) as usize, device: dev };
        let make = || {
            let on_block = self.on_block.clone();
            backend
                .make_input(
                    spec,
                    Box::new(move |block: &[i16]| {
                        on_block(block.iter().flat_map(|s| s.to_ne_bytes()).collect());
                    }),
                    Box::new(|_status: String| {}),
                )
                .map_err(|e| anyhow!(e))
        };
        // open_input refreshes the device table and retries once when the open fails: a device that joined after
        // the daemon started (a Bluetooth speaker's mic made the default) is otherwise a stale entry and
        // paInternalError (live 2026-09-28 15:24).
        let stream = open_input(backend, make, "desk call")?;
        info!("desk call: microphone {}", input_device_name(backend, dev));
        Ok(stream)
    }
}

impl DeskMic for Mic {
    fn open(&mut self) -> bool {
        if self.stream.is_some() {
            return true;
        }
        match self.try_open() {
            Ok(stream) => {
                self.stream = Some(stream);
                true
            }
            Err(e) => {
                // no mic: the press is empty and the call says so
                warn!("desk call: could not open the microphone ({:#})", e);
                self.stream = None;
                false
            }
        }
    }

    fn close(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            if stream.stop().is_ok() {
                let _ = stream.close();
            }
        }
    }
}

// ---- the speakers ---------------------------------------------------------------------------------------------

/// Where the call's audio plays: the Mac's [`Speaker`] or the board's [`BoardSpeaker`].
#[async_trait]
pub trait DeskSpeaker: Send + Sync {
    async fn play(&self, pcm: &[u8]) -> anyhow::Result<()>;
    /// Drop what is queued.
    async fn flush(&self) -> anyhow::Result<()>;
    fn close(&self);
}

type Item = Option<(u64, Vec<u8>)>;

struct PlayQueue {
    q: StdMutex<VecDeque<Item>>,
    ready: Condvar,
}

impl PlayQueue {
    fn put(&self, item: Item) {
        self.q.lock().unwrap_or_else(|e| e.into_inner()).push_back(item);
        self.ready.notify_one();
    }

    fn get(&self) -> Item {
        let mut q = self.q.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(item) = q.pop_front() {
                return item;
            }
            q = self.ready.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn clear(&self) {
        self.q.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// eve's reply on the Mac speaker. Writes block on a player thread; `flush` drops what is queued.
pub struct Speaker {
    q: Arc<PlayQueue>,
    gen: Arc<AtomicU64>,
    thread: Mutex<Option<ThreadHandle<()>>>,
    opener: OutputOpener,
}

impl Default for Speaker {
    fn default() -> Self {
        Self::new()
    }
}

impl Speaker {
    pub fn new() -> Self {
        Self::with_output(Arc::new(open_default_output))
    }

    /// A speaker that opens its output with `opener` (on the player thread).
    pub fn with_output(opener: OutputOpener) -> Self {
        Speaker {
            q: Arc::new(PlayQueue { q: StdMutex::new(VecDeque::new()), ready: Condvar::new() }),
            gen: Arc::new(AtomicU64::new(0)),
            thread: Mutex::new(None),
            opener,
        }
    }

    /// The default factory: a new Mac speaker per call.
    pub fn factory() -> SpeakerFactory {
        Arc::new(|| Arc::new(Speaker::new()) as Arc<dyn DeskSpeaker>)
    }

    pub fn play_pcm(&self, pcm: &[u8]) {
        {
            let mut slot = self.thread.lock();
            if slot.as_ref().is_none_or(|t| t.is_finished()) {
                let (q, gen, opener) = (self.q.clone(), self.gen.clone(), self.opener.clone());
                match std::thread::Builder::new().name("desk-call-speaker".into()).spawn(move || run(q, gen, opener)) {
                    Ok(t) => *slot = Some(t),
                    Err(e) => warn!("desk call: could not open the speaker ({})", e),
                }
            }
        }
        self.q.put(Some((self.gen.load(Ordering::SeqCst), pcm.to_vec())));
    }

    pub fn flush_now(&self) {
        self.gen.fetch_add(1, Ordering::SeqCst);
        self.q.clear();
    }

    pub fn close_now(&self) {
        self.flush_now();
        self.q.put(None);
    }
}

fn run(q: Arc<PlayQueue>, gen: Arc<AtomicU64>, opener: OutputOpener) {
    let mut out: Box<dyn RawOutput> = match opener(SAMPLE_RATE) {
        Ok(out) => out,
        Err(e) => {
            warn!("desk call: could not open the speaker ({})", e);
            return;
        }
    };
    while let Some((g, pcm)) = q.get() {
        if g == gen.load(Ordering::SeqCst) && out.write(&pcm).is_err() {
            break;
        }
    }
    out.stop_close();
}

#[async_trait]
impl DeskSpeaker for Speaker {
    async fn play(&self, pcm: &[u8]) -> anyhow::Result<()> {
        self.play_pcm(pcm);
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        self.flush_now();
        Ok(())
    }

    fn close(&self) {
        self.close_now();
    }
}

struct BoardState {
    started: Option<f64>,
    queued: f64,
    odd: Vec<u8>,
}

/// eve's reply on the Voice PE's speaker (firmware chirp.cpp, the voice buffer). The audio goes as base64 lines of
/// at most [`PCM_PIECE_BYTES`], paced to stay at most [`LEAD_SECS`] ahead of real time: speech arrives faster than
/// it plays, and the board's buffer is finite. A flush drops what the board still holds.
pub struct BoardSpeaker {
    pub send: BoardSend,
    pub clock: Clock,
    pub sleep: Sleep,
    st: Mutex<BoardState>,
}

impl BoardSpeaker {
    pub fn new(send: BoardSend) -> Self {
        Self::with_clock(send, Arc::new(monotonic), Arc::new(|s| Box::pin(tokio::time::sleep(secs(s)))))
    }

    pub fn with_clock(send: BoardSend, clock: Clock, sleep: Sleep) -> Self {
        BoardSpeaker { send, clock, sleep, st: Mutex::new(BoardState { started: None, queued: 0.0, odd: Vec::new() }) }
    }

    /// When the current burst began playing, roughly.
    pub fn started(&self) -> Option<f64> {
        self.st.lock().started
    }

    /// Seconds of audio sent in the current burst.
    pub fn queued(&self) -> f64 {
        self.st.lock().queued
    }

    /// Seconds of audio the board has not played yet, by the clock.
    pub fn ahead(&self) -> f64 {
        let (started, queued) = {
            let st = self.st.lock();
            (st.started, st.queued)
        };
        let Some(started) = started else { return 0.0 };
        let left = queued - ((self.clock)() - started);
        if left > 0.0 {
            left
        } else {
            0.0
        }
    }

    pub async fn play_pcm(&self, pcm: &[u8]) -> anyhow::Result<()> {
        // Only whole int16 samples go out. An odd last byte is kept for the next call: sent, it split every later
        // sample on the board across two; dropped, the next reply would start half a sample in.
        let (pcm, end) = {
            let mut st = self.st.lock();
            let mut all = std::mem::take(&mut st.odd);
            all.extend_from_slice(pcm);
            let end = all.len() - all.len() % 2;
            st.odd = all[end..].to_vec();
            (all, end)
        };
        let mut i = 0;
        while i < end {
            let piece = &pcm[i..(i + PCM_PIECE_BYTES).min(end)];
            if self.ahead() == 0.0 {
                // the board ran dry: a new burst starts now
                let now = (self.clock)();
                let mut st = self.st.lock();
                st.started = Some(now);
                st.queued = 0.0;
            }
            let wait = self.ahead() + piece.len() as f64 / BYTES_PER_SEC as f64 - LEAD_SECS; // room for the whole piece
            if wait > 0.0 {
                (self.sleep)(wait).await;
            }
            let d = base64::engine::general_purpose::STANDARD.encode(piece);
            (self.send)(json!({"cmd": "pcm", "d": d})).await?;
            self.st.lock().queued += piece.len() as f64 / BYTES_PER_SEC as f64;
            i += PCM_PIECE_BYTES;
        }
        Ok(())
    }

    pub async fn flush_board(&self) -> anyhow::Result<()> {
        {
            let mut st = self.st.lock();
            st.started = None;
            st.queued = 0.0;
            st.odd.clear();
        }
        (self.send)(json!({"cmd": "pcm_flush"})).await
    }
}

#[async_trait]
impl DeskSpeaker for BoardSpeaker {
    async fn play(&self, pcm: &[u8]) -> anyhow::Result<()> {
        self.play_pcm(pcm).await
    }

    async fn flush(&self) -> anyhow::Result<()> {
        self.flush_board().await
    }

    /// What is on the board plays out; nothing is left here to drop.
    fn close(&self) {}
}

// ---- the link ---------------------------------------------------------------------------------------------------

/// The closed sentinel's opcode on the inbox (`(-1, b"")`).
const CLOSE_OP: i16 = -1;

/// What phone_call's [`Call`] needs from its WebSocket, served by the board, the Mac mic and the Mac speaker.
pub struct DeskLink {
    pub send_board: BoardSend,
    pub speaker: Arc<dyn DeskSpeaker>,
    pub on_phase: Option<Phase>,
    closed: AtomicBool,
    tx: mpsc::UnboundedSender<(i16, Vec<u8>)>,
    rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<(i16, Vec<u8>)>>,
    ring: Mutex<String>,
    phase: Mutex<String>,
    /// Phase hooks run one at a time, in order (Python's single loop thread never ran two at once).
    phase_turn: tokio::sync::Mutex<()>,
}

impl DeskLink {
    pub fn new(send_board: BoardSend, speaker: Arc<dyn DeskSpeaker>, on_phase: Option<Phase>) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new(DeskLink {
            send_board,
            speaker,
            on_phase,
            closed: AtomicBool::new(false),
            tx,
            rx: tokio::sync::Mutex::new(rx),
            ring: Mutex::new("idle".into()),
            phase: Mutex::new("idle".into()),
            phase_turn: tokio::sync::Mutex::new(()),
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// The ring's state as last sent.
    pub fn ring(&self) -> String {
        self.ring.lock().clone()
    }

    /// The follower's phase as last told.
    pub fn phase(&self) -> String {
        self.phase.lock().clone()
    }

    /// `inbox.put_nowait((op, data))`.
    pub fn push(&self, op: u8, data: Vec<u8>) {
        let _ = self.tx.send((i16::from(op), data));
    }

    // ---- the desk's side ----
    pub fn talk(&self) {
        self.push(OP_TEXT, br#"{"type": "talk"}"#.to_vec());
    }

    pub fn done(&self) {
        self.push(OP_TEXT, br#"{"type": "done"}"#.to_vec());
    }

    pub fn ping(&self) {
        self.push(OP_TEXT, br#"{"type": "ping"}"#.to_vec());
    }

    pub fn audio(&self, pcm: Vec<u8>) {
        self.push(OP_BIN, pcm);
    }

    pub async fn set_ring(&self, state: &str) {
        let changed = {
            let mut ring = self.ring.lock();
            if *ring != state {
                *ring = state.to_string();
                true
            } else {
                false
            }
        };
        if changed {
            let _ = (self.send_board)(json!({"cmd": "agent", "state": state})).await;
        }
        // The ring goes quiet between presses, but the call is still a conversation with someone at the desk.
        self.set_phase(if matches!(state, "thinking" | "speaking") { state } else { "listening" }).await;
    }

    pub async fn set_phase(&self, phase: &str) {
        let _turn = self.phase_turn.lock().await;
        {
            let mut current = self.phase.lock();
            if *current == phase {
                return;
            }
            *current = phase.to_string();
        }
        if let Some(hook) = &self.on_phase {
            // the head is an audience; the call goes on without it
            match AssertUnwindSafe(hook(phase.to_string())).catch_unwind().await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => error!("desk call: phase hook failed: {:#}", e),
                Err(_) => error!("desk call: phase hook failed"),
            }
        }
    }
}

#[async_trait]
impl CallSocket for DeskLink {
    async fn recv(&self) -> Result<(u8, Vec<u8>), Closed> {
        if self.is_closed() {
            return Err(Closed(String::new()));
        }
        let got = self.rx.lock().await.recv().await;
        match got {
            Some((op, data)) if op >= 0 => Ok((op as u8, data)),
            _ => Err(Closed(String::new())),
        }
    }

    async fn send_bytes(&self, data: &[u8]) -> Result<(), Closed> {
        if self.is_closed() {
            return Err(Closed(String::new()));
        }
        // BoardSpeaker awaits the board; the Mac Speaker queues and returns
        self.speaker.play(data).await.map_err(|e| {
            warn!("desk call: the speaker failed ({:#})", e);
            Closed(e.to_string())
        })
    }

    async fn send_json(&self, obj: &Value) -> Result<(), Closed> {
        if self.is_closed() {
            return Err(Closed(String::new()));
        }
        let kind = obj.get("type").and_then(Value::as_str);
        match kind {
            Some("flush") => {
                self.speaker.flush().await.map_err(|e| {
                    warn!("desk call: the speaker failed ({:#})", e);
                    Closed(e.to_string())
                })?;
            }
            Some("state") => {
                let state = obj.get("state").map(py_str).unwrap_or_else(|| "None".into());
                self.set_ring(ring_state(&state)).await;
                if truthy(obj.get("note")) {
                    info!("desk call: {}", py_str(&obj["note"]));
                }
            }
            Some("heard") => {
                let text = obj.get("text").filter(|t| truthy(Some(t))).map(py_str).unwrap_or_default();
                info!("desk call: heard {} chars", text.chars().count());
            }
            _ => {}
        }
        Ok(())
    }

    async fn close(&self, _code: u16) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send((CLOSE_OP, Vec::new()));
        }
    }

    fn closed(&self) -> bool {
        self.is_closed()
    }
}

// ---- the calls --------------------------------------------------------------------------------------------------

/// `DeskCalls`' keyword arguments, with Python's defaults.
pub struct DeskOptions {
    pub mic_device: Option<String>,
    pub busy: Busy,
    pub mic_factory: MicFactory,
    pub speaker_factory: SpeakerFactory,
    pub on_phase: Option<Phase>,
}

impl Default for DeskOptions {
    fn default() -> Self {
        DeskOptions {
            mic_device: None,
            busy: Arc::new(|| false),
            mic_factory: Mic::factory(),
            speaker_factory: Speaker::factory(),
            on_phase: None,
        }
    }
}

#[derive(Default)]
struct DeskState {
    link: Option<Arc<DeskLink>>,
    task: Option<JoinHandle<()>>,
    done: Option<watch::Receiver<bool>>,
    mic: Option<Box<dyn DeskMic>>,
    held: bool,
}

impl DeskState {
    fn active(&self) -> bool {
        self.task.as_ref().is_some_and(|t| !t.is_finished())
    }
}

struct Inner {
    brain: Option<Arc<dyn Brain>>,
    voice: Option<Arc<dyn Voice>>,
    send_board: BoardSend,
    on_phase: Option<Phase>,
    mic_device: Option<String>,
    busy: Busy,
    mic_factory: MicFactory,
    speaker_factory: SpeakerFactory,
    limits: Arc<Limits>,
    ping_secs: AtomicU64,
    st: Mutex<DeskState>,
}

/// Presses from the board in, one [`Call`] at a time. `busy` says whether the chat already has a listener (a phone
/// call), so the desk never takes it from the phone.
#[derive(Clone)]
pub struct DeskCalls {
    inner: Arc<Inner>,
}

impl DeskCalls {
    pub fn new(
        brain: Option<Arc<dyn Brain>>,
        voice: Option<Arc<dyn Voice>>,
        send_board: BoardSend,
        options: DeskOptions,
    ) -> Self {
        DeskCalls {
            inner: Arc::new(Inner {
                brain,
                voice,
                send_board,
                on_phase: options.on_phase,
                mic_device: options.mic_device,
                busy: options.busy,
                mic_factory: options.mic_factory,
                speaker_factory: options.speaker_factory,
                limits: Arc::new(Limits::default()),
                ping_secs: AtomicU64::new(PING_SECS.to_bits()),
                st: Mutex::new(DeskState::default()),
            }),
        }
    }

    /// The calls' waits (`phone_call.QUIET_SECS`, the hour), read by each call as it runs.
    pub fn limits(&self) -> Arc<Limits> {
        self.inner.limits.clone()
    }

    /// `PING_SECS`, settable (the tests shorten it).
    pub fn ping_secs(&self) -> f64 {
        f64::from_bits(self.inner.ping_secs.load(Ordering::SeqCst))
    }

    pub fn set_ping_secs(&self, v: f64) {
        self.inner.ping_secs.store(v.to_bits(), Ordering::SeqCst);
    }

    pub fn active(&self) -> bool {
        self.inner.st.lock().active()
    }

    /// Whether the button is held.
    pub fn held(&self) -> bool {
        self.inner.st.lock().held
    }

    /// The current (or last) call's link.
    pub fn link(&self) -> Option<Arc<DeskLink>> {
        self.inner.st.lock().link.clone()
    }

    pub async fn press(&self, on: bool) {
        let ring = {
            let mut st = self.inner.st.lock();
            if on == st.held {
                return;
            }
            st.held = on;
            if on {
                self.inner.down(&mut st)
            } else {
                Inner::up(&mut st);
                None
            }
        };
        if let Some(link) = ring {
            link.set_ring("listening").await;
        }
    }

    pub async fn stop(&self) {
        let (link, done) = {
            let st = self.inner.st.lock();
            (st.link.clone(), st.done.clone())
        };
        if let Some(link) = link {
            link.close(1000).await;
        }
        if let Some(mut done) = done {
            let _ = done.wait_for(|d| *d).await;
        }
    }
}

impl Inner {
    /// The press's synchronous part; the link whose ring then turns to listening.
    fn down(self: &Arc<Self>, st: &mut DeskState) -> Option<Arc<DeskLink>> {
        let (Some(brain), Some(voice)) = (self.brain.clone(), self.voice.clone()) else {
            warn!("desk call: hold to talk needs the Telegram chat and an OpenAI key; not set up");
            return None;
        };
        if !st.active() {
            if (self.busy)() {
                info!("desk call: the phone is on a call; the button waits");
                return None;
            }
            self.start(st, brain, voice);
        }
        let link = st.link.clone()?;
        let to_call = link.clone();
        let block: OnBlock = Arc::new(move |pcm| to_call.audio(pcm));
        link.talk();
        // The mic opens before the ring's await, in the same step as the press. Each line from the board is its own
        // task, so the release can run during that send: opened after it, the mic stayed open with the button up
        // until the call ended (verification/Eve/DeskCall.lean).
        let mut mic = (self.mic_factory)(self.mic_device.clone(), block);
        mic.open();
        st.mic = Some(mic);
        Some(link)
    }

    fn up(st: &mut DeskState) {
        if let Some(mut mic) = st.mic.take() {
            mic.close();
        }
        if st.active() {
            if let Some(link) = &st.link {
                link.done();
            }
        }
    }

    fn start(self: &Arc<Self>, st: &mut DeskState, brain: Arc<dyn Brain>, voice: Arc<dyn Voice>) {
        let speaker = (self.speaker_factory)();
        let link = DeskLink::new(self.send_board.clone(), speaker.clone(), self.on_phase.clone());
        st.link = Some(link.clone());
        let call = Call::with_limits(link.clone(), brain, voice, self.limits.clone());
        let (done_tx, done_rx) = watch::channel(false);
        let me = self.clone();
        st.done = Some(done_rx);
        st.task = Some(tokio::spawn(async move {
            info!("desk call: started from the board's button");
            link.set_phase("listening").await;
            let pinger = tokio::spawn(keepalive(me.clone(), link.clone()));
            let reason = AssertUnwindSafe(call.run()).catch_unwind().await.unwrap_or_else(|_| "?".to_string());
            pinger.abort();
            {
                let mut st = me.st.lock();
                if let Some(mut mic) = st.mic.take() {
                    mic.close();
                }
                st.held = false;
            }
            speaker.close();
            link.set_ring("idle").await;
            link.set_phase("idle").await;
            link.close(1000).await;
            info!("desk call: ended ({}), {} press(es)", reason, call.presses());
            let _ = done_tx.send(true);
        }));
    }
}

/// `Call::run` ends a call after `QUIET_SECS` without a message. A reply that takes longer (a tool, a long
/// read-out) must not be cut off, so ping while eve thinks or speaks; once it goes quiet the call ends `QUIET_SECS`
/// later, like a phone put down.
async fn keepalive(me: Arc<Inner>, link: Arc<DeskLink>) {
    while !link.is_closed() {
        tokio::time::sleep(secs(f64::from_bits(me.ping_secs.load(Ordering::SeqCst)))).await;
        let ring = link.ring();
        if ring == "thinking" || ring == "speaking" || me.st.lock().held {
            link.ping();
        }
    }
}
