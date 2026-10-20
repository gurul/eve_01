//! The picture over WebRTC (pika_rtc's wire: one data channel "video", stamp + JPEG per message). Python's
//! `ObsbotPanel._rtc`, `_answer`, `_send` and `_close_peer`. Unlike Pika's one peer, any number of screens watch
//! (up to MAX_PEERS), each with its own channel and all sharing one capture.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use bytes::BytesMut;
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::info;
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelState};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder, RTCIceGatheringState,
    RTCIceServer, RTCPeerConnectionState, RTCSessionDescription,
};

use eve_pika::pika::stamp_ms;
use eve_pika::pika_rtc;

use crate::panel::{Inner, ObsbotPanel};
use crate::MAX_PEERS;

/// One WebRTC viewer: its connection and the task sending it frames.
pub(crate) struct Peer {
    pc: Arc<dyn PeerConnection>,
    sender: Option<JoinHandle<()>>,
}

fn refused() -> Value {
    json!({"ok": false, "rtc": false, "line": ""})
}

impl ObsbotPanel {
    pub(crate) async fn rtc(&self, sdp: Option<&Value>) -> Value {
        let sdp = match sdp {
            Some(Value::String(s))
                if !s.is_empty() && s.chars().count() <= pika_rtc::MAX_SDP_CHARS && pika_rtc::available() =>
            {
                s.clone()
            }
            _ => return refused(),
        };
        match tokio::time::timeout(Duration::from_secs_f64(8.0), self.answer(&sdp)).await {
            Ok(Ok(answer)) => json!({"ok": true, "rtc": true, "sdp": answer, "line": ""}),
            Ok(Err(e)) => {
                // the WebRTC stack raises its own kinds; the tunnel still works
                info!("obsbot: webrtc answer failed ({})", error_name(&e));
                refused()
            }
            Err(_) => {
                info!("obsbot: webrtc answer failed (TimeoutError)");
                refused()
            }
        }
    }

    async fn answer(&self, offer_sdp: &str) -> anyhow::Result<String> {
        loop {
            // the oldest goes: a phone that vanished without closing
            let oldest = {
                let peers = self.inner.peers.lock();
                if peers.len() < MAX_PEERS {
                    break;
                }
                peers.keys().next().copied()
            };
            match oldest {
                Some(id) => close_peer(&self.inner, id).await,
                None => break,
            }
        }
        let id = self.inner.next_peer.fetch_add(1, Ordering::SeqCst) + 1;
        let gathered = Arc::new(Notify::new());
        let handler = Arc::new(Handler { panel: Arc::downgrade(&self.inner), id, gathered: gathered.clone() });
        let mut config = RTCConfigurationBuilder::new();
        if !self.inner.stun.is_empty() {
            config = config
                .with_ice_servers(vec![RTCIceServer { urls: vec![self.inner.stun.clone()], ..Default::default() }]);
        }
        let pc = PeerConnectionBuilder::new()
            .with_configuration(config.build())
            .with_handler(handler)
            .with_udp_addrs(self.inner.udp_addrs.clone())
            .build()
            .await?;
        let pc: Arc<dyn PeerConnection> = Arc::new(pc);
        self.inner.peers.lock().insert(id, Peer { pc: pc.clone(), sender: None });
        let answered: anyhow::Result<String> = async {
            pc.set_remote_description(RTCSessionDescription::offer(offer_sdp.to_string())?).await?;
            let answer = pc.create_answer(None).await?;
            pc.set_local_description(answer).await?;
            gathered.notified().await; // gathers every candidate first
            pc.local_description().await.map(|d| d.sdp).ok_or_else(|| anyhow::anyhow!("no local description"))
        }
        .await;
        if answered.is_err() {
            close_peer(&self.inner, id).await;
        }
        answered
    }

    pub(crate) async fn close_peers(&self) {
        let ids: Vec<u64> = self.inner.peers.lock().keys().copied().collect();
        for id in ids {
            close_peer(&self.inner, id).await;
        }
    }

    /// WebRTC viewers connected now.
    pub fn peers(&self) -> usize {
        self.inner.peers.lock().len()
    }
}

/// The error's kind for the log line (Python logged `type(e).__name__`): the root cause's variant name.
fn error_name(e: &anyhow::Error) -> String {
    let dbg = format!("{:?}", e.root_cause());
    match dbg.split(|c: char| !c.is_alphanumeric() && c != '_').next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "Error".to_string(),
    }
}

pub(crate) async fn close_peer(inner: &Arc<Inner>, id: u64) {
    let Some(peer) = inner.peers.lock().shift_remove(&id) else {
        return;
    };
    if let Some(task) = peer.sender {
        if tokio::task::try_id() != Some(task.id()) && !task.is_finished() {
            task.abort();
            let _ = task.await;
        }
    }
    let _ = peer.pc.close().await;
}

/// The channel's state as its events report it: `readyState == "open"` is opened and not closing.
#[derive(Default)]
struct ChannelState {
    open: AtomicBool,
    closed: AtomicBool,
    changed: Notify,
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

/// The shared capture's frames into one peer's channel until it closes; a lost frame is skipped.
async fn send(inner: Arc<Inner>, id: u64, channel: Arc<dyn DataChannel>) {
    let chan = Arc::new(ChannelState::default());
    if let Ok(RTCDataChannelState::Open) = channel.ready_state().await {
        chan.open.store(true, Ordering::SeqCst);
    }
    let _pump = {
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
                chan.changed.notify_waiters();
            }
            chan.closed.store(true, Ordering::SeqCst);
            chan.changed.notify_waiters();
        }))
    };
    if !chan.is_open() {
        let opened = tokio::time::timeout(Duration::from_secs_f64(pika_rtc::OPEN_SECS), async {
            loop {
                let notified = chan.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if chan.open.load(Ordering::SeqCst) {
                    return;
                }
                notified.await;
            }
        })
        .await;
        if opened.is_err() {
            return;
        }
    }
    let capture = inner.capture.clone();
    while chan.is_open() && inner.peers.lock().contains_key(&id) {
        let mut watch = capture.watch();
        info!("obsbot: picture on (webrtc; {} watching)", capture.viewers() + 1);
        let mut gone = false;
        while let Some(item) = watch.next().await {
            match item {
                Ok(jpeg) => {
                    if !chan.is_open() {
                        break;
                    }
                    let buffered = channel.outstanding_bytes().await.unwrap_or(0);
                    if jpeg.len() > pika_rtc::MAX_JPEG || buffered > pika_rtc::BUFFER_LIMIT {
                        continue;
                    }
                    let message = pika_rtc::frame_message(stamp_ms().max(0) as u64, &jpeg);
                    if channel.send(BytesMut::from(&message[..])).await.is_err() {
                        gone = true; // Python's send raised out of the sender
                        break;
                    }
                }
                Err(e) => {
                    if chan.is_open() && channel.send_text(&e.0).await.is_err() {
                        gone = true;
                    }
                    break;
                }
            }
        }
        watch.close();
        info!("obsbot: picture off (webrtc; {} watching)", capture.viewers());
        if gone {
            return;
        }
        if chan.is_open() {
            tokio::time::sleep(Duration::from_secs_f64(pika_rtc::RETRY_SECS)).await;
        }
    }
}

/// The peer's events. Each handler returns at once (the driver waits on it); real work is spawned.
struct Handler {
    panel: Weak<Inner>,
    id: u64,
    gathered: Arc<Notify>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if matches!(state, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
            if let Some(inner) = self.panel.upgrade() {
                let id = self.id;
                tokio::spawn(async move { close_peer(&inner, id).await });
            }
        }
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let inner = self.panel.upgrade();
        let id = self.id;
        tokio::spawn(async move {
            let label = channel.label().await.unwrap_or_default();
            if let Some(inner) = inner.filter(|_| label == "video") {
                // The check and the new sender go under one lock, as Python's synchronous handler did them in one
                // step: a closed peer must not be left a sender.
                let mut peers = inner.peers.lock();
                if let Some(peer) = peers.get_mut(&id) {
                    if let Some(old) = peer.sender.take() {
                        old.abort();
                    }
                    peer.sender = Some(tokio::spawn(send(inner.clone(), id, channel)));
                    return;
                }
            }
            let _ = channel.close().await;
        });
    }
}
