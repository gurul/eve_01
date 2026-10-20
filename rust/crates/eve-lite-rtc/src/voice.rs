//! eve's voice (the outbound audio track) and the page's microphone (the inbound one).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::Mutex;
use tracing::warn;

use crate::opus::{Decoder, Encoder, OpusError, APPLICATION_VOIP};
use crate::{MAX_LAG_SECS, MAX_VOICE_BYTES, SAMPLE_RATE, VOICE_FRAME_BYTES, VOICE_FRAME_SAMPLES};

/// A marker: fires when the audio queued after it starts to play. An error is logged; the voice goes on.
pub type Marker = Box<dyn FnOnce() -> anyhow::Result<()> + Send>;

/// Monotonic seconds (`time.monotonic`).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
/// `asyncio.sleep`.
pub type Sleep = Arc<dyn Fn(f64) -> BoxFuture<'static, ()> + Send + Sync>;

fn monotonic() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

fn real_sleep(secs: f64) -> BoxFuture<'static, ()> {
    tokio::time::sleep(std::time::Duration::from_secs_f64(secs.max(0.0))).boxed()
}

enum Item {
    Audio(Vec<u8>),
    Mark(Marker),
}

struct VoiceState {
    items: VecDeque<Item>,
    /// how much of the first queued audio is already in a frame
    offset: usize,
    /// an odd byte, waiting for the rest of its sample
    carry: Vec<u8>,
    /// bytes of audio waiting
    queued: usize,
    /// bytes refused past max_bytes
    dropped: usize,
    frames: u64,
    pts: Option<u64>,
    start: f64,
}

/// One frame of eve's voice (aiortc's `av.AudioFrame`): `samples` of 24 kHz mono signed 16-bit PCM, little-endian,
/// at `pts` in a time base of 1/`sample_rate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceFrame {
    pub pcm: Vec<u8>,
    pub pts: u64,
    pub samples: usize,
    pub sample_rate: u32,
}

impl VoiceFrame {
    /// The samples as numbers.
    pub fn samples_i16(&self) -> Vec<i16> {
        self.pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
    }
}

/// eve's voice on one peer: the call's 24 kHz mono PCM (phone_call.Call.read_out, through
/// RtcCallSocket.send_bytes) as 20 ms frames of [`VOICE_FRAME_SAMPLES`], paced in real time, and silence while
/// nothing is queued. A marker ([`VoiceTrack::mark`]) waits in the queue between the audio and fires when the audio
/// after it is taken for a frame, that is when that audio starts to leave the Mac: speech is made faster than it
/// plays, so a `said` caption sent with it would run ahead of the voice. [`VoiceTrack::flush`] drops the audio and the
/// markers. The peer encodes each frame to Opus ([`media_track`] is what it reads).
pub struct VoiceTrack {
    clock: Clock,
    sleep: Sleep,
    pub max_bytes: usize,
    st: Mutex<VoiceState>,
}

impl Default for VoiceTrack {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceTrack {
    pub fn new() -> Self {
        Self::with(Arc::new(monotonic), Arc::new(real_sleep), MAX_VOICE_BYTES)
    }

    pub fn with(clock: Clock, sleep: Sleep, max_bytes: usize) -> Self {
        VoiceTrack {
            clock,
            sleep,
            max_bytes,
            st: Mutex::new(VoiceState {
                items: VecDeque::new(),
                offset: 0,
                carry: Vec::new(),
                queued: 0,
                dropped: 0,
                frames: 0,
                pts: None,
                start: 0.0,
            }),
        }
    }

    /// Bytes of audio waiting.
    pub fn queued(&self) -> usize {
        self.st.lock().queued
    }

    /// Bytes refused past `max_bytes`.
    pub fn dropped(&self) -> usize {
        self.st.lock().dropped
    }

    /// Frames handed out by [`VoiceTrack::recv`].
    pub fn frames(&self) -> u64 {
        self.st.lock().frames
    }

    pub fn push(&self, pcm: &[u8]) {
        let mut st = self.st.lock();
        let mut data = std::mem::take(&mut st.carry);
        data.extend_from_slice(pcm);
        let cut = data.len() - data.len() % 2;
        st.carry = data[cut..].to_vec();
        if cut == 0 {
            return;
        }
        if st.queued + cut > self.max_bytes {
            if st.dropped == 0 {
                warn!(
                    "lite rtc: eve's speech is queued past {} s; the rest is dropped",
                    self.max_bytes / eve_phone_call::BYTES_PER_SEC
                );
            }
            st.dropped += cut;
            return;
        }
        data.truncate(cut);
        st.items.push_back(Item::Audio(data));
        st.queued += cut;
    }

    pub fn mark(&self, fire: Marker) {
        self.st.lock().items.push_back(Item::Mark(fire));
    }

    pub fn flush(&self) {
        let mut st = self.st.lock();
        st.items.clear();
        st.offset = 0;
        st.carry.clear();
        st.queued = 0;
    }

    /// The next frame's PCM: queued audio, then silence; each marker passed on the way fires.
    pub fn take(&self) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(VOICE_FRAME_BYTES);
        let mut fired: Vec<Marker> = Vec::new();
        {
            let mut st = self.st.lock();
            while out.len() < VOICE_FRAME_BYTES && !st.items.is_empty() {
                if matches!(st.items.front(), Some(Item::Mark(_))) {
                    if let Some(Item::Mark(m)) = st.items.pop_front() {
                        fired.push(m);
                    }
                    continue;
                }
                let offset = st.offset;
                let Some(Item::Audio(item)) = st.items.front() else { break };
                let n = (item.len() - offset).min(VOICE_FRAME_BYTES - out.len());
                out.extend_from_slice(&item[offset..offset + n]);
                let whole = offset + n == item.len();
                st.offset += n;
                st.queued -= n;
                if whole {
                    st.items.pop_front();
                    st.offset = 0;
                }
            }
        }
        // the markers fire in order, outside the lock: a caption is sent from here
        for m in fired {
            if let Err(e) = m() {
                // a caption that cannot go must not stop the voice
                warn!("lite rtc: a caption could not be sent ({})", eve_phone_call::error_name(&e));
            }
        }
        out.resize(VOICE_FRAME_BYTES, 0);
        out
    }

    /// The next frame, when it is due: the first at once, then one every 20 ms.
    pub async fn recv(&self) -> VoiceFrame {
        let (pts, wait) = {
            let mut st = self.st.lock();
            match st.pts {
                None => {
                    st.pts = Some(0);
                    st.start = (self.clock)();
                    (0, None)
                }
                Some(p) => {
                    let pts = p + VOICE_FRAME_SAMPLES as u64;
                    st.pts = Some(pts);
                    let wait = st.start + pts as f64 / SAMPLE_RATE as f64 - (self.clock)();
                    if wait > 0.0 {
                        (pts, Some(wait))
                    } else {
                        if wait < -MAX_LAG_SECS {
                            // far behind (the loop stalled): from now, not a burst the page's jitter buffer would drop
                            st.start = (self.clock)() - pts as f64 / SAMPLE_RATE as f64;
                        }
                        (pts, None)
                    }
                }
            }
        };
        if let Some(wait) = wait {
            (self.sleep)(wait).await;
        }
        self.st.lock().frames += 1;
        Self::frame(self.take(), pts)
    }

    pub fn frame(pcm: Vec<u8>, pts: u64) -> VoiceFrame {
        VoiceFrame { samples: pcm.len() / 2, pcm, pts, sample_rate: SAMPLE_RATE }
    }
}

/// The track ended (aiortc's MediaStreamError).
#[derive(Debug, Clone, thiserror::Error)]
#[error("the track ended")]
pub struct MediaStreamError;

/// `voice` as the peer's audio track: `recv` gives its frames while the track is live.
pub struct VoiceMediaTrack {
    pub source: Arc<VoiceTrack>,
    live: AtomicBool,
}

impl VoiceMediaTrack {
    pub fn kind(&self) -> &'static str {
        "audio"
    }

    /// `readyState`: "live" or "ended".
    pub fn ready_state(&self) -> &'static str {
        if self.live.load(Ordering::SeqCst) {
            "live"
        } else {
            "ended"
        }
    }

    pub async fn recv(&self) -> Result<VoiceFrame, MediaStreamError> {
        if !self.live.load(Ordering::SeqCst) {
            return Err(MediaStreamError);
        }
        Ok(self.source.recv().await)
    }

    pub fn stop(&self) {
        self.live.store(false, Ordering::SeqCst);
    }
}

/// `voice` as the peer's audio track, one per peer.
pub fn media_track(voice: Arc<VoiceTrack>) -> VoiceMediaTrack {
    VoiceMediaTrack { source: voice, live: AtomicBool::new(true) }
}

/// eve's voice to Opus: one packet per 20 ms frame, at the call's own 24 kHz mono (aiortc resampled to 48 kHz
/// stereo first; the packet plays the same in the page's decoder).
pub struct VoiceEncoder {
    enc: Encoder,
}

impl VoiceEncoder {
    pub fn new() -> Result<Self, OpusError> {
        Ok(Self { enc: Encoder::new(SAMPLE_RATE, 1, APPLICATION_VOIP)? })
    }

    pub fn encode(&mut self, frame: &VoiceFrame) -> Result<Vec<u8>, OpusError> {
        self.enc.encode(&frame.samples_i16())
    }
}

// ---- the page's microphone: the inbound audio track ---------------------------------------------------------------

/// The page's microphone as the call hears it: each Opus packet of the peer's audio track decoded to 24 kHz mono
/// signed 16-bit PCM, the format phone_call.Call takes (aiortc's decoder gave 48 kHz stereo and
/// `av.AudioResampler(format="s16", layout="mono", rate=24000)` made it this).
#[derive(Default)]
pub struct InboundAudio {
    decoder: Option<Decoder>,
}

impl InboundAudio {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn convert(&mut self, packet: &[u8]) -> Result<Vec<u8>, OpusError> {
        if self.decoder.is_none() {
            self.decoder = Some(Decoder::new(SAMPLE_RATE, 1)?);
        }
        let decoded = match self.decoder.as_mut().map(|d| d.decode(packet)) {
            Some(Ok(pcm)) => pcm,
            _ => {
                // the stream changed under the decoder: a fresh one from this packet on
                let mut fresh = Decoder::new(SAMPLE_RATE, 1)?;
                let pcm = fresh.decode(packet);
                self.decoder = Some(fresh);
                pcm?
            }
        };
        Ok(decoded.iter().flat_map(|s| s.to_le_bytes()).collect())
    }
}
