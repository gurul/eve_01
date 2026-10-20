//! Pika's video over WebRTC: the car's JPEGs straight from this Mac to the phone, past the tunnel (docs/pika.md).
//!
//! Owner, 2026-10-10: "can we setup webrtc so that the camera stream is lower latency". Measured that day from the Mac,
//! the tunnel stream's first frame took 0.79 s against 0.14 s from the camera direct, its frames came in bursts (gap p95
//! 564 ms against 209 ms) and a request's round trip through it was 145 ms median. On the home Wi-Fi WebRTC connects the
//! phone to this Mac directly; off it, through each side's public address (STUN). It never relays through a server: when
//! neither works the app falls back to the tunnel stream (pika::Pika::stream).
//!
//! How. The app offers one data channel labelled `video`, unordered and with a short packet lifetime, so a lost frame
//! is skipped rather than holding up the ones behind it. The offer goes through the link's own authenticated control
//! (`{"action": "rtc", "sdp": <offer>}`, pika::Pika::panel), and the answer comes back in the reply with every candidate
//! in it (no trickle). Each binary message is one frame: the Mac's send stamp in ms (8 bytes, big-endian) and the
//! camera's JPEG exactly as it came, never re-encoded, so WebRTC adds no encoder delay. A text message is a line for the
//! owner (the camera is off, or busy). Frames are dropped, never queued, while the channel's buffer is behind.
//!
//! The camera serves one viewer. This takes the same slot as the tunnel stream (newest wins), and gives it back when
//! the channel or the peer closes, when the panel closes, or when a tunnel stream or a newer offer takes over. While the
//! channel is open and the camera can't be reached, it tries again every RETRY_SECS, so a car switched on later
//! appears by itself.
//!
//! The Python daemon imported aiortc only when an offer came; here the WebRTC stack (webrtc-rs) is compiled in, so
//! `available()` is always true.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::anyhow;
use bytes::BytesMut;
use parking_lot::Mutex;
use tokio::task::JoinHandle;
use tracing::info;
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelState};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder, RTCIceGatheringState,
    RTCIceServer, RTCPeerConnectionState, RTCSessionDescription,
};

use crate::pika::{self as pika_mod, Event, Pika, WeakPika};

pub const HEADER_SIZE: usize = 8; // the Mac's send stamp, ms since the epoch (">Q")
pub const MAX_MESSAGE: usize = 65536; // aiortc's SCTP maximum message size (RTCSctpTransport.getCapabilities)
pub const MAX_JPEG: usize = MAX_MESSAGE - HEADER_SIZE;
pub const BUFFER_LIMIT: usize = 96 * 1024; // bytes waiting in the channel: past this, a frame is dropped
pub const OPEN_SECS: f64 = 10.0; // the channel must open this long after the answer
pub const RETRY_SECS: f64 = 3.0; // the camera could not be reached: try again after this
pub const MAX_SDP_CHARS: usize = 20000;

/// Where the peer's UDP sockets bind: every IPv4 interface (webrtc-rs binds one socket per interface address).
pub const DEFAULT_UDP_ADDRS: [&str; 1] = ["0.0.0.0:0"];

pub fn frame_message(stamp: u64, jpeg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_SIZE + jpeg.len());
    out.extend_from_slice(&stamp.to_be_bytes());
    out.extend_from_slice(jpeg);
    out
}

/// The stamp and the JPEG of one frame message (the tests read frames with it, as the app does). None when the
/// message is shorter than the stamp (Python's `struct.error`).
pub fn parse_message(data: &[u8]) -> Option<(u64, Vec<u8>)> {
    if data.len() < HEADER_SIZE {
        return None;
    }
    let mut stamp = [0u8; HEADER_SIZE];
    stamp.copy_from_slice(&data[..HEADER_SIZE]);
    Some((u64::from_be_bytes(stamp), data[HEADER_SIZE..].to_vec()))
}

/// Whether WebRTC can be offered. The stack is compiled in, so always.
pub fn available() -> bool {
    true
}

struct RtcState {
    pc: Option<(u64, Arc<dyn PeerConnection>)>,
    sender: Option<JoinHandle<()>>,
}

struct RtcInner {
    pika: WeakPika,
    stun: String,
    udp_addrs: Vec<String>,
    state: Mutex<RtcState>,
    next_pc: AtomicU64,
    sent: AtomicU64,
    dropped: AtomicU64,
}

/// One WebRTC peer at a time for the Pika screen's video. Clones share it.
#[derive(Clone)]
pub struct PikaRtc {
    inner: Arc<RtcInner>,
}

impl PikaRtc {
    pub fn new(pika: &Pika, stun: &str) -> PikaRtc {
        PikaRtc::with_udp_addrs(pika, stun, DEFAULT_UDP_ADDRS.iter().map(|s| s.to_string()).collect())
    }

    /// As `new`, binding the peer's sockets to these addresses (the tests use loopback).
    pub fn with_udp_addrs(pika: &Pika, stun: &str, udp_addrs: Vec<String>) -> PikaRtc {
        PikaRtc {
            inner: Arc::new(RtcInner {
                pika: pika.downgrade(),
                stun: stun.to_string(),
                udp_addrs,
                state: Mutex::new(RtcState { pc: None, sender: None }),
                next_pc: AtomicU64::new(0),
                sent: AtomicU64::new(0),
                dropped: AtomicU64::new(0),
            }),
        }
    }

    pub fn stun(&self) -> &str {
        &self.inner.stun
    }

    /// A peer is up (Python's `open` property).
    pub fn is_open(&self) -> bool {
        self.inner.state.lock().pc.is_some()
    }

    /// Frames sent into the channel.
    pub fn sent(&self) -> u64 {
        self.inner.sent.load(Ordering::SeqCst)
    }

    /// Frames dropped because the channel was behind or the JPEG too big for one message.
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::SeqCst)
    }

    fn is_current(&self, id: u64) -> bool {
        self.inner.state.lock().pc.as_ref().is_some_and(|(pid, _)| *pid == id)
    }

    /// Answer the app's offer. Ends the peer and the video before it: the newest wins.
    pub async fn answer(&self, offer_sdp: &str) -> anyhow::Result<String> {
        self.close().await;
        let id = self.inner.next_pc.fetch_add(1, Ordering::SeqCst) + 1;
        let gathered = Arc::new(Event::default());
        let handler = Arc::new(Handler { rtc: Arc::downgrade(&self.inner), id, gathered: gathered.clone() });
        let mut config = RTCConfigurationBuilder::new();
        if !self.inner.stun.is_empty() {
            config = config.with_ice_servers(vec![RTCIceServer {
                urls: vec![self.inner.stun.clone()],
                ..Default::default()
            }]);
        }
        let pc = PeerConnectionBuilder::new()
            .with_configuration(config.build())
            .with_handler(handler)
            .with_udp_addrs(self.inner.udp_addrs.clone())
            .build()
            .await?;
        let pc: Arc<dyn PeerConnection> = Arc::new(pc);
        self.inner.state.lock().pc = Some((id, pc.clone()));

        let answered: anyhow::Result<String> = async {
            pc.set_remote_description(RTCSessionDescription::offer(offer_sdp.to_string())?).await?;
            let answer = pc.create_answer(None).await?;
            pc.set_local_description(answer).await?;
            gathered.wait().await; // every candidate goes in the answer (no trickle)
            pc.local_description().await.map(|d| d.sdp).ok_or_else(|| anyhow!("no local description"))
        }
        .await;
        if answered.is_err() && self.is_current(id) {
            self.close().await;
        }
        answered
    }

    /// The camera's frames into the channel until it closes.
    async fn send(self, id: u64, channel: Arc<dyn DataChannel>) {
        let chan = Arc::new(ChannelState::default());
        if let Ok(RTCDataChannelState::Open) = channel.ready_state().await {
            chan.open.store(true, Ordering::SeqCst);
        }
        let pump = {
            let chan = chan.clone();
            let channel = channel.clone();
            AbortOnDrop(tokio::spawn(async move {
                while let Some(event) = channel.poll().await {
                    match event {
                        DataChannelEvent::OnOpen => chan.open.store(true, Ordering::SeqCst),
                        DataChannelEvent::OnClosing | DataChannelEvent::OnClose => {
                            chan.closed.store(true, Ordering::SeqCst)
                        }
                        _ => continue,
                    }
                    chan.changed.set();
                }
                chan.closed.store(true, Ordering::SeqCst);
                chan.changed.set();
            }))
        };
        if !chan.open.load(Ordering::SeqCst) {
            let opened = tokio::time::timeout(Duration::from_secs_f64(OPEN_SECS), async {
                while !chan.open.load(Ordering::SeqCst) {
                    chan.changed.wait().await;
                    chan.changed.clear();
                }
            })
            .await;
            if opened.is_err() {
                info!("pika: webrtc channel never opened");
                return;
            }
        }
        let Some(pika) = self.inner.pika.upgrade() else {
            return;
        };
        let slot = pika.take_video(true).await;
        info!("pika: video on (webrtc)");
        tokio::select! {
            _ = slot.cancelled() => {}
            _ = self.relay(&pika, id, &channel, &chan) => {}
        }
        drop(slot);
        drop(pump);
    }

    async fn relay(&self, pika: &Pika, id: u64, channel: &Arc<dyn DataChannel>, chan: &ChannelState) {
        while chan.is_open() && self.is_current(id) {
            let mut frames = pika.frames();
            loop {
                match frames.next().await {
                    None => break,
                    Some(Err(e)) => {
                        if chan.is_open() && channel.send_text(&e.0).await.is_err() {
                            return;
                        }
                        break;
                    }
                    Some(Ok(jpeg)) => {
                        if !chan.is_open() {
                            break;
                        }
                        let buffered = channel.outstanding_bytes().await.unwrap_or(0);
                        if jpeg.len() > MAX_JPEG || buffered > BUFFER_LIMIT {
                            self.inner.dropped.fetch_add(1, Ordering::SeqCst);
                            continue;
                        }
                        let message = frame_message(pika_mod::stamp_ms().max(0) as u64, &jpeg);
                        if channel.send(BytesMut::from(&message[..])).await.is_err() {
                            return;
                        }
                        self.inner.sent.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
            drop(frames);
            if chan.is_open() {
                tokio::time::sleep(Duration::from_secs_f64(RETRY_SECS)).await;
            }
        }
    }

    /// End the peer and give back the camera.
    pub async fn close(&self) {
        let (pc, sender) = {
            let mut s = self.inner.state.lock();
            (s.pc.take(), s.sender.take())
        };
        if let Some(sender) = sender {
            if tokio::task::try_id() != Some(sender.id()) && !sender.is_finished() {
                sender.abort();
                let _ = sender.await;
            }
        }
        if let Some((_, pc)) = pc {
            let _ = pc.close().await;
        }
    }
}

/// The channel's state as its events report it: `readyState == "open"` is opened and not closing.
#[derive(Default)]
struct ChannelState {
    open: AtomicBool,
    closed: AtomicBool,
    changed: Event,
}

impl ChannelState {
    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst) && !self.closed.load(Ordering::SeqCst)
    }
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The peer's events. Each handler returns at once (the driver waits on it); real work is spawned.
struct Handler {
    rtc: Weak<RtcInner>,
    id: u64,
    gathered: Arc<Event>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.set();
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        info!("pika: webrtc {}", state);
        if matches!(state, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
            if let Some(inner) = self.rtc.upgrade() {
                let rtc = PikaRtc { inner };
                if rtc.is_current(self.id) {
                    tokio::spawn(async move { rtc.close().await });
                }
            }
        }
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let rtc = self.rtc.upgrade().map(|inner| PikaRtc { inner });
        let id = self.id;
        tokio::spawn(async move {
            let label = channel.label().await.unwrap_or_default();
            let Some(rtc) = rtc.filter(|r| label == "video" && r.is_current(id)) else {
                let _ = channel.close().await;
                return;
            };
            let mut s = rtc.inner.state.lock();
            if let Some(old) = s.sender.take() {
                old.abort();
            }
            s.sender = Some(tokio::spawn(rtc.clone().send(id, channel)));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_message_is_stamp_then_jpeg() {
        let jpeg = [&b"\xff\xd8"[..], &[7u8; 40], &b"\xff\xd9"[..]].concat();
        let m = frame_message(1760100000123, &jpeg);
        assert_eq!(&m[..8], &1760100000123u64.to_be_bytes());
        assert_eq!(&m[8..], &jpeg[..]);
        assert_eq!(parse_message(&m), Some((1760100000123, jpeg)));
        assert_eq!(parse_message(b"short"), None);
    }
}
