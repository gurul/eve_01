//! A data channel as lite_rtc uses one: aiortc's RTCDataChannel surface (`label`, `ordered`, `maxPacketLifeTime`,
//! `maxRetransmits`, `readyState`, `send`, `bufferedAmount` and its low threshold, `close`, and the `message`,
//! `close` and `bufferedamountlow` events), as a trait so the tests drive the channels with a fake as the Python
//! tests did. [`WebrtcChannel`] is the real one, over webrtc-rs.
//!
//! aiortc's `send` queued the message and returned; webrtc-rs's is async. [`WebrtcChannel::send`] puts each message
//! on one writer task, so messages leave in the order they were sent and a sender never waits; a `close` goes on the
//! same queue, after every message sent before it.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::BytesMut;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelState};

/// One message: text (a JSON string here) or binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Data {
    Text(String),
    Binary(Vec<u8>),
}

impl Data {
    pub fn len(&self) -> usize {
        match self {
            Data::Text(s) => s.len(),
            Data::Binary(b) => b.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `readyState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyState {
    Connecting,
    Open,
    Closing,
    Closed,
}

impl ReadyState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadyState::Connecting => "connecting",
            ReadyState::Open => "open",
            ReadyState::Closing => "closing",
            ReadyState::Closed => "closed",
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => ReadyState::Connecting,
            1 => ReadyState::Open,
            2 => ReadyState::Closing,
            _ => ReadyState::Closed,
        }
    }

    fn to_u8(self) -> u8 {
        self as u8
    }
}

/// What a channel tells its listeners (pyee's `channel.on(...)`).
#[derive(Debug, Clone)]
pub enum ChannelEvent {
    /// `message`: one message from the page.
    Message(Data),
    /// `close`: the channel closed.
    Close,
    /// `bufferedamountlow`: the buffer fell to the low threshold.
    BufferedAmountLow,
}

/// A listener, called in the order events happen. It must not block.
pub type Listener = Arc<dyn Fn(ChannelEvent) + Send + Sync>;

/// The channel was closed, or the stack would not take the message (aiortc's InvalidStateError).
#[derive(Debug, Clone, thiserror::Error)]
#[error("the data channel is closed")]
pub struct ChannelClosed;

#[async_trait]
pub trait Channel: Send + Sync {
    fn label(&self) -> String;
    fn ordered(&self) -> bool;
    fn max_packet_life_time(&self) -> Option<u16>;
    fn max_retransmits(&self) -> Option<u16>;
    fn ready_state(&self) -> ReadyState;
    /// Queue one message; an error when the channel is not open.
    fn send(&self, data: Data) -> Result<(), ChannelClosed>;
    /// `bufferedAmount`: bytes queued and not yet handed to the network.
    async fn buffered_amount(&self) -> usize;
    /// `bufferedAmountLowThreshold = n`.
    fn set_buffered_amount_low_threshold(&self, n: usize);
    /// Close the channel; its `close` event follows.
    fn close(&self);
    /// `channel.on(...)` for every event.
    fn on(&self, listener: Listener);
}

/// How long a close waits for the messages sent before it to leave.
pub const CLOSE_DRAIN_SECS: f64 = 1.0;

/// What the writer task does next, in order.
enum Out {
    Data(Data),
    Close,
}

/// A webrtc-rs data channel behind [`Channel`].
pub struct WebrtcChannel {
    dc: Arc<dyn DataChannel>,
    label: String,
    ordered: bool,
    max_packet_life_time: Option<u16>,
    max_retransmits: Option<u16>,
    state: AtomicU8,
    /// Bytes handed to `send` and not yet to the stack.
    queued: Arc<AtomicUsize>,
    out: mpsc::UnboundedSender<Out>,
    listeners: Mutex<Vec<Listener>>,
    pumping: AtomicBool,
    closed_told: AtomicBool,
    me: std::sync::Weak<WebrtcChannel>,
}

impl WebrtcChannel {
    /// Wrap a channel the stack announced (`on_data_channel`) or made (`create_data_channel`). Must be called inside
    /// a tokio runtime. Its events are read once the first listener is added, so none is lost before.
    pub async fn new(dc: Arc<dyn DataChannel>) -> Arc<WebrtcChannel> {
        let label = dc.label().await.unwrap_or_default();
        let ordered = dc.ordered().await.unwrap_or(false);
        let max_packet_life_time = dc.max_packet_life_time().await.unwrap_or(None);
        let max_retransmits = dc.max_retransmits().await.unwrap_or(None);
        let state = match dc.ready_state().await {
            Ok(RTCDataChannelState::Open) => ReadyState::Open,
            Ok(RTCDataChannelState::Closing) => ReadyState::Closing,
            Ok(RTCDataChannelState::Closed) | Err(_) => ReadyState::Closed,
            Ok(_) => ReadyState::Connecting,
        };
        let (tx, mut rx) = mpsc::unbounded_channel::<Out>();
        let queued = Arc::new(AtomicUsize::new(0));
        let writer_dc = dc.clone();
        let writer_queued = queued.clone();
        let channel = Arc::new_cyclic(|me: &std::sync::Weak<WebrtcChannel>| WebrtcChannel {
            dc,
            label,
            ordered,
            max_packet_life_time,
            max_retransmits,
            state: AtomicU8::new(state.to_u8()),
            queued,
            out: tx,
            listeners: Mutex::new(Vec::new()),
            pumping: AtomicBool::new(false),
            closed_told: AtomicBool::new(false),
            me: me.clone(),
        });
        let me = Arc::downgrade(&channel);
        tokio::spawn(async move {
            while let Some(out) = rx.recv().await {
                match out {
                    Out::Data(data) => {
                        let n = data.len();
                        let _ = match data {
                            Data::Text(s) => writer_dc.send_text(&s).await,
                            Data::Binary(b) => writer_dc.send(BytesMut::from(&b[..])).await,
                        };
                        writer_queued.fetch_sub(n, Ordering::SeqCst);
                    }
                    // after every message sent before it, as aiortc's close came after its queued sends
                    Out::Close => {
                        // the stack's close resets the stream, which drops what SCTP has not sent yet: what was sent
                        // first leaves first (an `ended` before the close), for at most CLOSE_DRAIN_SECS
                        let end = std::time::Instant::now() + std::time::Duration::from_secs_f64(CLOSE_DRAIN_SECS);
                        while std::time::Instant::now() < end
                            && writer_dc.outstanding_bytes().await.is_ok_and(|n| n > 0)
                        {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                        let _ = writer_dc.close().await;
                        // a channel nobody listened to has no pump to see its close
                        if let Some(me) = me.upgrade() {
                            if !me.pumping.load(Ordering::SeqCst) {
                                me.set_state(ReadyState::Closed);
                            }
                        }
                        break;
                    }
                }
            }
        });
        channel
    }

    /// The stack's own channel.
    pub fn inner(&self) -> &Arc<dyn DataChannel> {
        &self.dc
    }

    fn set_state(&self, s: ReadyState) {
        self.state.store(s.to_u8(), Ordering::SeqCst);
    }

    fn emit(&self, event: ChannelEvent) {
        let listeners = self.listeners.lock().clone();
        for l in listeners {
            l(event.clone());
        }
    }

    fn told_closed(&self) {
        self.set_state(ReadyState::Closed);
        if !self.closed_told.swap(true, Ordering::SeqCst) {
            self.emit(ChannelEvent::Close);
        }
    }

    fn pump(&self) {
        if self.pumping.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(me) = self.me.upgrade() else { return };
        tokio::spawn(async move {
            while let Some(event) = me.dc.poll().await {
                match event {
                    DataChannelEvent::OnOpen => {
                        if me.ready_state() == ReadyState::Connecting {
                            me.set_state(ReadyState::Open);
                        }
                    }
                    DataChannelEvent::OnMessage(m) => {
                        if me.closed_told.load(Ordering::SeqCst) {
                            continue;
                        }
                        let data = if m.is_string {
                            match String::from_utf8(m.data.to_vec()) {
                                Ok(s) => Data::Text(s),
                                Err(_) => continue,
                            }
                        } else {
                            Data::Binary(m.data.to_vec())
                        };
                        me.emit(ChannelEvent::Message(data));
                    }
                    DataChannelEvent::OnClosing => {
                        if me.ready_state() != ReadyState::Closed {
                            me.set_state(ReadyState::Closing);
                        }
                    }
                    DataChannelEvent::OnClose => {
                        me.told_closed();
                        break;
                    }
                    DataChannelEvent::OnBufferedAmountLow => me.emit(ChannelEvent::BufferedAmountLow),
                    _ => {}
                }
            }
            me.told_closed();
        });
    }
}

#[async_trait]
impl Channel for WebrtcChannel {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn ordered(&self) -> bool {
        self.ordered
    }

    fn max_packet_life_time(&self) -> Option<u16> {
        self.max_packet_life_time
    }

    fn max_retransmits(&self) -> Option<u16> {
        self.max_retransmits
    }

    fn ready_state(&self) -> ReadyState {
        ReadyState::from_u8(self.state.load(Ordering::SeqCst))
    }

    fn send(&self, data: Data) -> Result<(), ChannelClosed> {
        if self.ready_state() != ReadyState::Open {
            return Err(ChannelClosed);
        }
        let n = data.len();
        self.queued.fetch_add(n, Ordering::SeqCst);
        if self.out.send(Out::Data(data)).is_err() {
            self.queued.fetch_sub(n, Ordering::SeqCst);
            return Err(ChannelClosed);
        }
        Ok(())
    }

    async fn buffered_amount(&self) -> usize {
        self.queued.load(Ordering::SeqCst) + self.dc.outstanding_bytes().await.unwrap_or(0)
    }

    fn set_buffered_amount_low_threshold(&self, n: usize) {
        let dc = self.dc.clone();
        let n = u32::try_from(n).unwrap_or(u32::MAX);
        tokio::spawn(async move {
            let _ = dc.set_buffered_amount_low_threshold(n).await;
        });
    }

    fn close(&self) {
        let s = self.ready_state();
        if s == ReadyState::Closed || s == ReadyState::Closing {
            return;
        }
        self.set_state(ReadyState::Closing);
        let _ = self.out.send(Out::Close);
    }

    fn on(&self, listener: Listener) {
        self.listeners.lock().push(listener);
        self.pump();
    }
}
