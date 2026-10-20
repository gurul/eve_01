//! One peer per device, and the hub that answers their offers ([`Peer`], [`RtcHub`]).

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use bytes::Bytes;
use indexmap::IndexMap;
use parking_lot::Mutex;
use rand::RngCore;
use serde_json::{json, Map, Value};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::media_engine::MIME_TYPE_OPUS;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};
use rtc::rtp_transceiver::RTCRtpTransceiverDirection;
use webrtc::data_channel::DataChannel;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState,
    RTCPeerConnectionState, RTCSessionDescription,
};

use eve_link::link_chat::{LinkChat, TURN_PATH};
use eve_link::seams::{DynRead, DynWrite, Headers};
use eve_phone_call::{CallSocket, PhoneCalls, TokenCheck};

use crate::call::RtcCallSocket;
use crate::channel::{Channel, Data, WebrtcChannel};
use crate::link::LinkChannel;
use crate::voice::{media_track, InboundAudio, VoiceEncoder, VoiceMediaTrack, VoiceTrack};
use crate::{
    available, configuration, py, reliable, rewrite_local_candidates, text, RtcError, CALL_LABEL, CLOSE_SECS,
    CONNECT_SECS, LINK_LABEL, MAX_PEERS, MAX_SDP_CHARS, NO_AUDIO,
};

// ---- what the hub serves ----------------------------------------------------------------------------------------

/// The peer's token check, as LinkChat takes it.
pub type TokenFn = dyn for<'s> Fn(&'s str) -> bool + Send + Sync;

/// LinkChat as the `link` channel calls it: `serve(method, path, query, headers, reader, writer, token_check)`.
#[async_trait]
pub trait LinkDoor: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    async fn serve(
        &self,
        method: &str,
        path: &str,
        query: &str,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
        token_check: &TokenFn,
    ) -> io::Result<()>;
}

#[async_trait]
impl LinkDoor for LinkChat {
    async fn serve(
        &self,
        method: &str,
        path: &str,
        query: &str,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
        token_check: &TokenFn,
    ) -> io::Result<()> {
        LinkChat::serve(self, method, path, query, headers, reader, writer, Some(token_check), None).await
    }
}

/// PhoneCalls as the `call` channel uses it.
#[async_trait]
pub trait CallDoor: Send + Sync {
    /// `calls.voice is not None and calls.brain is not None`.
    fn voice_ready(&self) -> bool;
    async fn serve_socket(&self, sock: Arc<dyn CallSocket>, token_check: TokenCheck) -> anyhow::Result<()>;
}

#[async_trait]
impl CallDoor for PhoneCalls {
    fn voice_ready(&self) -> bool {
        self.voice().is_some() && self.brain().is_some()
    }

    async fn serve_socket(&self, sock: Arc<dyn CallSocket>, token_check: TokenCheck) -> anyhow::Result<()> {
        PhoneCalls::serve_socket(self, sock, Some(token_check)).await
    }
}

/// `secrets.token_urlsafe(nbytes)`.
fn token_urlsafe(nbytes: usize) -> String {
    let mut raw = vec![0u8; nbytes];
    rand::rng().fill_bytes(&mut raw);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

fn secs(x: f64) -> Duration {
    Duration::from_secs_f64(if x.is_finite() && x > 0.0 { x } else { 0.0 })
}

/// The codec eve speaks and hears: Opus at RTP's 48 kHz stereo clock, as every browser offers it.
fn opus_codec() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: MIME_TYPE_OPUS.to_owned(),
        clock_rate: 48000,
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
        rtcp_feedback: vec![],
    }
}

/// The media the peers negotiate: Opus only (the voice); video is always inactive.
fn media_engine() -> anyhow::Result<MediaEngine> {
    let mut m = MediaEngine::default();
    m.register_codec(RTCRtpCodecParameters { rtp_codec: opus_codec(), payload_type: 111 }, RtpCodecKind::Audio)?;
    Ok(m)
}

/// Whether an SDP has a data channel's media section (`m=application`).
fn has_application(sdp: &str) -> bool {
    sdp.split('\n').any(|ln| ln.starts_with("m=application"))
}

/// Each media section's kind by its mid (`m=audio ...` then `a=mid:0`), from an SDP.
fn media_kinds(sdp: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let mut kind: Option<String> = None;
    for line in sdp.split('\n') {
        let line = line.trim_end_matches('\r');
        if let Some(m) = line.strip_prefix("m=") {
            kind = m.split(' ').next().map(str::to_string);
        } else if let (Some(mid), Some(k)) = (line.strip_prefix("a=mid:"), kind.as_ref()) {
            out.insert(mid.trim().to_string(), k.clone());
        }
    }
    out
}

fn state_name(state: RTCPeerConnectionState) -> &'static str {
    match state {
        RTCPeerConnectionState::New => "new",
        RTCPeerConnectionState::Connecting => "connecting",
        RTCPeerConnectionState::Connected => "connected",
        RTCPeerConnectionState::Disconnected => "disconnected",
        RTCPeerConnectionState::Failed => "failed",
        RTCPeerConnectionState::Closed => "closed",
        _ => "unspecified",
    }
}

/// A one-shot flag that can be waited on.
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

/// The task running now is this one.
fn is_me(task: &JoinHandle<()>) -> bool {
    tokio::task::try_id() == Some(task.id())
}

// ---- one peer -----------------------------------------------------------------------------------------------------

struct PeerState {
    pc: Option<Arc<dyn PeerConnection>>,
    voice_on: bool,
    connected: bool,
    link: Option<LinkChannel>,
    call: Option<RtcCallSocket>,
    /// eve's voice on the peer's track: the track, its SSRC and the payload type the answer gave it
    speaker: Option<(Arc<TrackLocalStaticSample>, u32, u8)>,
    speaking: bool,
    /// the microphone's pump is running
    hearing: bool,
    /// the microphone pump, the voice and the connect deadline
    tasks: Vec<JoinHandle<()>>,
    /// PhoneCalls.serve_socket, one per call channel
    calls: Vec<JoinHandle<()>>,
}

/// One device's WebRTC peer: its connection, its secret, its `link` and `call` channels (the newest of each wins),
/// eve's voice on its audio track, and the pump that hears its microphone.
pub struct Peer {
    hub: Weak<HubInner>,
    pub device_id: String,
    secret: String,
    pub voice: Arc<VoiceTrack>,
    closed: AtomicBool,
    st: Mutex<PeerState>,
    me: Weak<Peer>,
}

impl Peer {
    pub fn new(hub: &RtcHub, device_id: &str) -> Arc<Peer> {
        Arc::new_cyclic(|me| Peer {
            hub: Arc::downgrade(&hub.inner),
            device_id: device_id.to_string(),
            secret: token_urlsafe(32),
            voice: Arc::new(VoiceTrack::new()),
            closed: AtomicBool::new(false),
            st: Mutex::new(PeerState {
                pc: None,
                voice_on: false,
                connected: false,
                link: None,
                call: None,
                speaker: None,
                speaking: false,
                hearing: false,
                tasks: Vec::new(),
                calls: Vec::new(),
            }),
            me: me.clone(),
        })
    }

    fn hub(&self) -> Option<RtcHub> {
        self.hub.upgrade().map(|inner| RtcHub { inner })
    }

    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn connected(&self) -> bool {
        self.st.lock().connected
    }

    pub fn voice_on(&self) -> bool {
        self.st.lock().voice_on
    }

    /// Whether the call can use the peer's audio (set by `start`; the tests set it).
    pub fn set_voice_on(&self, on: bool) {
        self.st.lock().voice_on = on;
    }

    /// The connection, until the peer closes.
    pub fn pc(&self) -> Option<Arc<dyn PeerConnection>> {
        self.st.lock().pc.clone()
    }

    pub fn link(&self) -> Option<LinkChannel> {
        self.st.lock().link.clone()
    }

    pub fn call(&self) -> Option<RtcCallSocket> {
        self.st.lock().call.clone()
    }

    pub fn check(&self, token: &str) -> bool {
        py::compare_digest(token.as_bytes(), self.secret.as_bytes())
    }

    fn token_check(&self) -> TokenCheck {
        let secret = self.secret.clone();
        Arc::new(move |t: &str| py::compare_digest(t.as_bytes(), secret.as_bytes()))
    }

    fn spawn_task<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut st = self.st.lock();
        st.tasks.retain(|t| !t.is_finished());
        st.tasks.push(tokio::spawn(fut));
    }

    /// Apply the offer and answer it, with every candidate in the answer.
    pub async fn start(&self, offer: &str) -> anyhow::Result<String> {
        let me = self.me.upgrade().ok_or_else(|| anyhow::anyhow!("the peer is gone"))?;
        let hub = self.hub().ok_or_else(|| anyhow::anyhow!("the hub is gone"))?;
        let gathered = Arc::new(Flag::default());
        let handler = Arc::new(Handler { peer: self.me.clone(), gathered: gathered.clone() });
        let pc = PeerConnectionBuilder::new()
            .with_configuration(configuration())
            .with_media_engine(media_engine()?)
            .with_handler(handler)
            .with_udp_addrs(hub.inner.opts.udp_addrs.clone())
            .build()
            .await?;
        let pc: Arc<dyn PeerConnection> = Arc::new(pc);
        self.st.lock().pc = Some(pc.clone());
        let applied = match RTCSessionDescription::offer(offer.to_string()) {
            Ok(desc) => pc.set_remote_description(desc).await.map_err(anyhow::Error::from),
            Err(e) => Err(e.into()),
        };
        if let Err(e) = applied {
            if !has_application(offer) {
                return Err(RtcError::new("the offer has no data channel").into());
            }
            return Err(e);
        }
        let remote = pc.remote_description().await.map(|d| d.sdp).unwrap_or_default();
        if !has_application(&remote) {
            // the page makes its link channel before the offer; the stack would answer anything, even no media at all
            return Err(RtcError::new("the offer has no data channel").into());
        }
        // a transceiver's kind is its media section's (`m=audio`): the stack makes its receiver only when the first
        // packet's track arrives
        let kinds = media_kinds(&remote);
        let mut audio = None;
        for t in pc.get_transceivers().await {
            let mid = t.mid().await.ok().flatten();
            let is_audio = mid.as_ref().and_then(|m| kinds.get(m)).is_some_and(|k| k == "audio");
            if audio.is_none() && is_audio {
                audio = Some(t);
                continue;
            }
            let _ = t.set_direction(RTCRtpTransceiverDirection::Inactive).await; // only the voice's audio flows
        }
        if let Some(t) = audio {
            if hub.voice_ready() {
                let ssrc = rand::random::<u32>();
                let track = Arc::new(TrackLocalStaticSample::new(
                    Instant::now(),
                    MediaStreamTrack::new(
                        format!("eve-{}", token_urlsafe(6)),
                        "eve-voice".to_owned(),
                        "eve's voice".to_owned(),
                        RtpCodecKind::Audio,
                        vec![RTCRtpEncodingParameters {
                            rtp_coding_parameters: RTCRtpCodingParameters { ssrc: Some(ssrc), ..Default::default() },
                            codec: opus_codec(),
                            ..Default::default()
                        }],
                    ),
                )?);
                // onto the page's sendrecv transceiver: no renegotiation
                let sender = pc.add_track(Arc::clone(&track) as Arc<dyn TrackLocal>).await?;
                {
                    let mut st = self.st.lock();
                    st.voice_on = true;
                    st.speaker = Some((track, ssrc, 0));
                }
                pc.set_local_description(pc.create_answer(None).await?).await?;
                let pt = sender
                    .get_parameters()
                    .await
                    .ok()
                    .and_then(|p| p.rtp_parameters.codecs.first().map(|c| c.payload_type))
                    .unwrap_or(111);
                if let Some(s) = self.st.lock().speaker.as_mut() {
                    s.2 = pt;
                }
            } else {
                let _ = t.set_direction(RTCRtpTransceiverDirection::Inactive).await;
                pc.set_local_description(pc.create_answer(None).await?).await?;
            }
        } else {
            pc.set_local_description(pc.create_answer(None).await?).await?;
        }
        gathered.wait().await; // every candidate goes in the answer
        let peer = me.clone();
        self.spawn_task(async move { peer.deadline().await });
        pc.local_description().await.map(|d| d.sdp).ok_or_else(|| anyhow::anyhow!("no local description"))
    }

    /// The page's microphone arrived (its first packet): with eve's voice on this peer, the pump hears it. One pump
    /// per peer; any other track is left to the stack.
    async fn on_track(self: &Arc<Self>, track: Arc<dyn TrackRemote>) {
        if track.kind().await != RtpCodecKind::Audio {
            return;
        }
        let start = {
            let mut st = self.st.lock();
            let start = st.voice_on && !st.hearing && !self.closed();
            if start {
                st.hearing = true;
            }
            start
        };
        if start {
            let peer = self.clone();
            self.spawn_task(async move { peer.hear(track).await });
        }
    }

    /// Read the page's microphone for as long as the track lives, so nothing piles up in the stack's queue; a call
    /// socket gets it as 24 kHz PCM.
    async fn hear(self: Arc<Self>, track: Arc<dyn TrackRemote>) {
        let mut inbound = InboundAudio::new();
        while let Some(event) = track.poll().await {
            let packet = match event {
                TrackRemoteEvent::OnRtpPacket(p) => p,
                TrackRemoteEvent::OnEnded => return,
                _ => continue,
            };
            let call = self.call();
            let Some(call) = call.filter(|c| !c.is_closed()) else { continue };
            match inbound.convert(&packet.payload) {
                Ok(pcm) => call.audio(&pcm),
                Err(_) => info!("lite rtc: a microphone frame could not be read (OpusError)"),
            }
        }
    }

    /// eve's voice onto the peer's track: each 20 ms frame encoded to Opus, from when the connection is up.
    async fn speak(media: VoiceMediaTrack, track: Arc<TrackLocalStaticSample>, ssrc: u32, payload_type: u8) {
        let mut encoder = match VoiceEncoder::new() {
            Ok(e) => e,
            Err(_) => {
                warn!("lite rtc: eve's voice could not be encoded (OSError)");
                return;
            }
        };
        while let Ok(frame) = media.recv().await {
            let Ok(packet) = encoder.encode(&frame) else { continue };
            let sample = Sample { data: Bytes::from(packet), duration: Duration::from_millis(20), ..Sample::new(Instant::now()) };
            let _ = track.write_sample(ssrc, payload_type, &sample, &[]).await;
        }
    }

    async fn deadline(self: Arc<Self>) {
        let connect = self.hub().map(|h| h.inner.opts.connect_secs).unwrap_or(CONNECT_SECS);
        tokio::time::sleep(secs(connect)).await;
        if !self.closed() && !self.connected() {
            if let Some(hub) = self.hub() {
                hub.forget(&self, "it did not connect");
            }
        }
    }

    fn on_state(self: &Arc<Self>, state: RTCPeerConnectionState) {
        if self.closed() || self.pc().is_none() {
            return;
        }
        if state == RTCPeerConnectionState::Connected {
            let (first, speak) = {
                let mut st = self.st.lock();
                let first = !st.connected;
                st.connected = true;
                let speak = if st.voice_on && !st.speaking { st.speaker.clone() } else { None };
                if speak.is_some() {
                    st.speaking = true;
                }
                (first, speak)
            };
            if first {
                info!("lite rtc: a peer connected (voice {})", if self.voice_on() { "on" } else { "off" });
            }
            if let Some((track, ssrc, pt)) = speak {
                let media = media_track(self.voice.clone());
                self.spawn_task(Self::speak(media, track, ssrc, pt));
            }
        } else if matches!(state, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
            if let Some(hub) = self.hub() {
                hub.forget(self, &format!("its connection {}", state_name(state)));
            }
        }
    }

    /// A data channel the page opened. Never fails into the stack: a channel that cannot be taken is closed.
    pub fn on_channel(self: &Arc<Self>, channel: Arc<dyn Channel>) {
        let run = std::panic::AssertUnwindSafe(|| self.take_channel(channel.clone()));
        if std::panic::catch_unwind(run).is_err() {
            warn!("lite rtc: a data channel could not be taken (Exception)");
            channel.close();
        }
    }

    fn take_channel(self: &Arc<Self>, channel: Arc<dyn Channel>) {
        let label = channel.label();
        let known = label == LINK_LABEL || label == CALL_LABEL;
        if self.closed() || !known || !reliable(channel.as_ref()) {
            info!("lite rtc: a data channel was refused ({})", if known { label.as_str() } else { "an unknown label" });
            channel.close();
            return;
        }
        let Some(hub) = self.hub() else {
            channel.close();
            return;
        };
        if label == LINK_LABEL {
            let served = {
                let weak = Arc::downgrade(&hub.inner);
                Arc::new(move |path: &str| {
                    if let Some(inner) = weak.upgrade() {
                        RtcHub { inner }.served(path);
                    }
                })
            };
            let lc = LinkChannel::with(channel, hub.inner.link.clone(), &self.secret, Some(served), crate::MAX_REQUESTS);
            let old = self.st.lock().link.replace(lc);
            if let Some(old) = old {
                old.close();
            }
            return;
        }
        let Some(calls) = hub.inner.calls.clone() else {
            channel.close();
            return;
        };
        let old_call = self.st.lock().call.clone();
        if let Some(old) = old_call {
            old.shut(); // a new call on this peer: the old one ends first
        }
        if !self.voice_on() && hub.voice_ready() {
            crate::send(channel.as_ref(), Data::Text(text(&json!({"type": "ended", "reason": NO_AUDIO}))));
            channel.close();
            return;
        }
        let weak = self.me.clone();
        let on_close: crate::call::OnClose = Arc::new(move |sock: &RtcCallSocket| {
            if let Some(peer) = weak.upgrade() {
                peer.call_closed(sock);
            }
        });
        let sock = RtcCallSocket::with(channel, self.voice.clone(), &self.secret, Some(on_close), crate::POSTROLL_SECS);
        self.st.lock().call = Some(sock.clone());
        let peer = self.clone();
        let task = tokio::spawn(async move { peer.serve_call(calls, sock).await });
        let mut st = self.st.lock();
        st.calls.retain(|t| !t.is_finished());
        st.calls.push(task);
    }

    async fn serve_call(self: Arc<Self>, calls: Arc<dyn CallDoor>, sock: RtcCallSocket) {
        if let Some(hub) = self.hub() {
            hub.inner.counts.lock().calls += 1;
        }
        info!("lite rtc: a call opened on a peer");
        let socket: Arc<dyn CallSocket> = Arc::new(sock.clone());
        if let Err(e) = calls.serve_socket(socket, self.token_check()).await {
            warn!("lite rtc: a call ended with an error ({})", eve_phone_call::error_name(&e));
        }
        sock.shut();
    }

    fn call_closed(&self, sock: &RtcCallSocket) {
        let mut st = self.st.lock();
        if st.call.as_ref().is_some_and(|c| c.same(sock)) {
            st.call = None;
        }
    }

    /// End everything on this peer: requests are cancelled, the call ends (PhoneCalls sees it drop), the connection
    /// closes.
    pub async fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let (link, call) = {
            let st = self.st.lock();
            (st.link.clone(), st.call.clone())
        };
        if let Some(link) = link {
            link.close();
        }
        if let Some(call) = call {
            call.shut();
        }
        self.voice.flush();
        let (tasks, calls, pc) = {
            let mut st = self.st.lock();
            // the connection leaves first (`pc, self.pc = self.pc, None`): its own "closed" event is not a forget
            (std::mem::take(&mut st.tasks), std::mem::take(&mut st.calls), st.pc.take())
        };
        for task in &tasks {
            if !is_me(task) {
                task.abort();
            }
        }
        if let Some(pc) = pc {
            let _ = tokio::time::timeout(secs(CLOSE_SECS), pc.close()).await;
        }
        let pending: Vec<JoinHandle<()>> =
            tasks.into_iter().chain(calls).filter(|t| !is_me(t) && !t.is_finished()).collect();
        if !pending.is_empty() {
            let aborts: Vec<_> = pending.iter().map(|t| t.abort_handle()).collect();
            let all = futures::future::join_all(pending);
            if tokio::time::timeout(secs(CLOSE_SECS), all).await.is_err() {
                for a in aborts {
                    a.abort();
                }
            }
        }
    }
}

/// The peer's events. Each returns at once (the driver waits on it); real work is spawned.
struct Handler {
    peer: Weak<Peer>,
    gathered: Arc<Flag>,
}

#[async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.set();
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if let Some(peer) = self.peer.upgrade() {
            peer.on_state(state);
        }
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        if let Some(peer) = self.peer.upgrade() {
            peer.on_track(track).await;
        }
    }

    async fn on_data_channel(&self, dc: Arc<dyn DataChannel>) {
        let peer = self.peer.clone();
        tokio::spawn(async move {
            let channel = WebrtcChannel::new(dc).await;
            match peer.upgrade() {
                Some(peer) => peer.on_channel(channel),
                None => channel.close(),
            }
        });
    }
}

// ---- the hub ------------------------------------------------------------------------------------------------------

/// What `RtcHub` takes beside the link and the calls; `Default` is production.
#[derive(Clone)]
pub struct HubOptions {
    pub max_peers: usize,
    pub connect_secs: f64,
    /// Where each peer's UDP sockets bind: every IPv4 interface (webrtc-rs binds one socket per interface address).
    pub udp_addrs: Vec<String>,
    /// Whether an offer can be answered at all ([`available`]); the tests replace it.
    pub available: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl Default for HubOptions {
    fn default() -> Self {
        HubOptions { max_peers: MAX_PEERS, connect_secs: CONNECT_SECS, udp_addrs: vec!["0.0.0.0:0".into()], available: None }
    }
}

#[derive(Default)]
struct Counts {
    answers: i64,
    link_requests: i64,
    link_turns: i64,
    calls: i64,
}

struct HubInner {
    link: Option<Arc<dyn LinkDoor>>,
    calls: Option<Arc<dyn CallDoor>>,
    opts: HubOptions,
    /// by device id, oldest first
    peers: Mutex<IndexMap<String, Arc<Peer>>>,
    closed: AtomicBool,
    counts: Mutex<Counts>,
    lock: tokio::sync::Mutex<()>,
    closing: Mutex<Vec<JoinHandle<()>>>,
}

/// The answer to a device's offer: `{"sdp": <answer>, "voice": <whether the call can be used>}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtcAnswer {
    pub sdp: String,
    pub voice: bool,
}

impl RtcAnswer {
    pub fn to_value(&self) -> Value {
        json!({"sdp": self.sdp, "voice": self.voice})
    }
}

/// Every device's peer (`RtcHub(link, calls)`). `link` is the daemon's one LinkChat; `calls` its PhoneCalls, or
/// None when calls are off. lite_web answers `POST /api/web/rtc` with [`RtcHub::answer`] and closes a revoked
/// device's peer with [`RtcHub::close_device`]; the daemon calls [`RtcHub::close`] when it stops. Clones share the
/// hub.
#[derive(Clone)]
pub struct RtcHub {
    inner: Arc<HubInner>,
}

/// An answer that was dropped mid-start (its request went away) still takes its peer down.
struct StartGuard<'a> {
    hub: &'a RtcHub,
    peer: Arc<Peer>,
    armed: bool,
}

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.hub.drop_peer(&self.peer);
        }
    }
}

impl RtcHub {
    pub fn new(link: Option<Arc<dyn LinkDoor>>, calls: Option<Arc<dyn CallDoor>>) -> Self {
        Self::with_options(link, calls, HubOptions::default())
    }

    pub fn with_options(link: Option<Arc<dyn LinkDoor>>, calls: Option<Arc<dyn CallDoor>>, opts: HubOptions) -> Self {
        RtcHub {
            inner: Arc::new(HubInner {
                link,
                calls,
                opts,
                peers: Mutex::new(IndexMap::new()),
                closed: AtomicBool::new(false),
                counts: Mutex::new(Counts::default()),
                lock: tokio::sync::Mutex::new(()),
                closing: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn max_peers(&self) -> usize {
        self.inner.opts.max_peers
    }

    pub fn connect_secs(&self) -> f64 {
        self.inner.opts.connect_secs
    }

    pub fn closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// The device ids with a peer, oldest first.
    pub fn peer_ids(&self) -> Vec<String> {
        self.inner.peers.lock().keys().cloned().collect()
    }

    pub fn peer(&self, device_id: &str) -> Option<Arc<Peer>> {
        self.inner.peers.lock().get(device_id).cloned()
    }

    /// Whether this Mac can take a call: PhoneCalls with the chat and a voice (an OpenRouter key).
    pub fn voice_ready(&self) -> bool {
        self.inner.calls.as_ref().is_some_and(|c| c.voice_ready())
    }

    /// Counts for status pages and tests: peers, connected peers, answers, link requests and turns, calls.
    pub fn stats(&self) -> Map<String, Value> {
        let (peers, connected) = {
            let peers = self.inner.peers.lock();
            (peers.len(), peers.values().filter(|p| p.connected()).count())
        };
        let c = self.inner.counts.lock();
        let mut out = Map::new();
        out.insert("peers".into(), json!(peers));
        out.insert("connected".into(), json!(connected));
        out.insert("answers".into(), json!(c.answers));
        out.insert("link_requests".into(), json!(c.link_requests));
        out.insert("link_turns".into(), json!(c.link_turns));
        out.insert("calls".into(), json!(c.calls));
        out
    }

    fn served(&self, path: &str) {
        let mut c = self.inner.counts.lock();
        c.link_requests += 1;
        if path == TURN_PATH {
            c.link_turns += 1;
        }
    }

    /// `POST /api/web/rtc`: the answer to a device's offer. The device's previous peer is closed (the newest offer
    /// wins), and the oldest other peer when `max_peers` are open. `peer_addr` is the signaling request's peer address
    /// (the candidate rewrite). `sdp` None is an offer that is not a string. An error is an offer that is not
    /// answered.
    pub async fn answer(&self, device_id: &str, sdp: Option<&str>, peer_addr: &str) -> Result<RtcAnswer, RtcError> {
        if device_id.is_empty() {
            return Err(RtcError::new("no device"));
        }
        let sdp = match sdp {
            Some(s) if !py::strip(s).is_empty() && s.chars().count() <= MAX_SDP_CHARS => s,
            _ => return Err(RtcError::new(format!("the offer must be SDP of at most {MAX_SDP_CHARS} characters"))),
        };
        let can = match &self.inner.opts.available {
            Some(f) => f(),
            None => available(),
        };
        if !can {
            return Err(RtcError::new("WebRTC is not available on this Mac"));
        }
        let (offer, rewritten) = rewrite_local_candidates(sdp, peer_addr);
        let peer = {
            let _held = self.inner.lock.lock().await;
            if self.closed() {
                return Err(RtcError::new("eve is stopping"));
            }
            let mut retire = Vec::new();
            let peer = {
                let mut peers = self.inner.peers.lock();
                if let Some(old) = peers.shift_remove(device_id) {
                    retire.push(old);
                }
                while peers.len() >= self.inner.opts.max_peers.max(1) {
                    match peers.shift_remove_index(0) {
                        Some((_, oldest)) => retire.push(oldest),
                        None => break,
                    }
                }
                let peer = Peer::new(self, device_id);
                peers.insert(device_id.to_string(), peer.clone());
                peer
            };
            for old in retire {
                self.retire(old);
            }
            let mut guard = StartGuard { hub: self, peer: peer.clone(), armed: true };
            let started = peer.start(&offer).await;
            guard.armed = false;
            match started {
                Ok(answer) => (peer, answer),
                Err(e) => {
                    self.drop_peer(&peer);
                    let rtc = e.downcast_ref::<RtcError>().cloned();
                    info!(
                        "lite rtc: an offer could not be answered ({})",
                        if rtc.is_some() { "RtcError".to_string() } else { eve_phone_call::error_name(&e) }
                    );
                    return Err(rtc.unwrap_or_else(|| RtcError::new("that offer could not be answered")));
                }
            }
        };
        let (peer, answer) = peer;
        self.inner.counts.lock().answers += 1;
        let voice = peer.voice_on();
        info!(
            "lite rtc: answered a device's offer ({} of {} peers, voice {}, {} mDNS candidate(s) pointed at its address)",
            self.inner.peers.lock().len(),
            self.inner.opts.max_peers,
            if voice { "on" } else { "off" },
            rewritten
        );
        Ok(RtcAnswer { sdp: answer, voice })
    }

    fn drop_peer(&self, peer: &Arc<Peer>) {
        {
            let mut peers = self.inner.peers.lock();
            if peers.get(&peer.device_id).is_some_and(|p| Arc::ptr_eq(p, peer)) {
                peers.shift_remove(&peer.device_id);
            }
        }
        self.retire(peer.clone());
    }

    /// A revoked device: its peer closes now, with its requests and its call.
    pub async fn close_device(&self, device_id: &str) {
        let peer = {
            let _held = self.inner.lock.lock().await;
            self.inner.peers.lock().shift_remove(device_id)
        };
        if let Some(peer) = peer {
            peer.close().await;
        }
    }

    /// The daemon is stopping: every peer closes.
    pub async fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        let peers: Vec<Arc<Peer>> = std::mem::take(&mut *self.inner.peers.lock()).into_values().collect();
        let closing: Vec<JoinHandle<()>> = std::mem::take(&mut *self.inner.closing.lock());
        let mine = futures::future::join_all(peers.iter().map(|p| p.close()));
        let theirs = futures::future::join_all(closing);
        futures::join!(mine, theirs);
    }

    /// A peer that failed, closed or never connected leaves (if it is still its device's).
    fn forget(&self, peer: &Arc<Peer>, why: &str) {
        {
            let mut peers = self.inner.peers.lock();
            if peers.get(&peer.device_id).is_some_and(|p| Arc::ptr_eq(p, peer)) {
                peers.shift_remove(&peer.device_id);
            }
        }
        if !peer.closed() {
            info!("lite rtc: a peer closed: {}", why);
        }
        self.retire(peer.clone());
    }

    /// Close a peer in the background: a new offer is not kept waiting on an old connection's goodbye.
    fn retire(&self, peer: Arc<Peer>) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
        let task = rt.spawn(async move { peer.close().await });
        let mut closing = self.inner.closing.lock();
        closing.retain(|t| !t.is_finished());
        closing.push(task);
    }
}
