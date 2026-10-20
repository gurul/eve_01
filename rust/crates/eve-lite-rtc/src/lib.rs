//! eve lite's WebRTC: the web app's chat and push-to-talk voice, from a phone or laptop on the same Wi-Fi straight to
//! this Mac (docs/plans/eve-lite.md, M6). Port of `lite_rtc.py`.
//!
//! The owner, 2026-10-13: "make the eve app a web app so that it can just be accessed from same wifi through web rtc".
//! The page (ios/EveLink/Web/src/web) offers one peer per device through `POST /api/web/rtc` (lite_web.py, behind the
//! device cookie), and [`RtcHub::answer`] answers it with every candidate in the answer, no trickle (as pika_rtc.py
//! does). There is no STUN or TURN server: [`configuration`] has no ICE server at all, and both ends are on the same
//! LAN. One peer per device, the newest offer wins, at most [`MAX_PEERS`]; a peer that has not connected
//! [`CONNECT_SECS`] after its answer is closed.
//!
//! * **Candidates.** A browser hides its address behind an mDNS name (`<uuid>.local`), and an ICE agent resolves each
//!   with a single short query and drops the candidate when it gets no answer; with no pair to check, ICE fails at
//!   once. So before the offer is applied, each such host candidate is pointed at the signaling request's own peer
//!   address when that is IPv4 and not loopback ([`rewrite_local_candidates`]). Peer-reflexive discovery stays as the
//!   backstop.
//! * **The `link` channel** carries the unchanged LinkChat (link_chat.py) as HTTP over messages ([`LinkChannel`]).
//!   The page sends `{"t":"req","id":<u32>,"m":"GET|POST","p":<path>,"q":<query>,"h":{...},"b":<body>}` and
//!   `{"t":"cancel","id":n}`. The Mac answers with `{"t":"head","id":n,"s":<status>,"h":{...}}`, binary frames (the
//!   id as 4 bytes big-endian, then at most [`BODY_FRAME_BYTES`] of the de-chunked body) and `{"t":"end","id":n}`,
//!   or `{"t":"err","id":n}` when the answer broke off. Only `/api/link/turn` and `/api/link/messages` pass; every
//!   other path is a 404 here (uploads and files go over HTTPS). A request's body is at most [`MAX_BODY_BYTES`] (an
//!   SCTP message is at most 64 KiB), and at most [`MAX_REQUESTS`] run at once on a channel. The door drops the page's
//!   `Origin` (LinkChat refuses any) and sets `Authorization: Bearer <the peer's secret>`. The body is fed to a reader
//!   that reaches EOF only on a cancel or when the channel closes: LinkChat reads EOF as the client hanging up, and a
//!   turn's lines nobody reads go elsewhere.
//! * **The `call` channel** and the peer's audio carry a push-to-talk call into the unchanged PhoneCalls
//!   (phone_call.py, `serve_socket`), through [`RtcCallSocket`]. Its hello is built here (`{"token": <the peer's
//!   secret>, "can": []}`), so a later call on the same peer needs no fresh token. The page's microphone track is
//!   decoded to 24 kHz mono 16-bit PCM and passed on in 100 ms blocks, only between `talk` and `done`, with the 300 ms
//!   before `talk` replayed after it (`talk` can arrive after the first words, since SCTP and RTP race) and `done`
//!   held back 250 ms (SCTP can overtake the last RTP packets). eve's speech plays on the peer's audio track
//!   ([`VoiceTrack`]): 20 ms frames in real time, silence when nothing is queued. A `said` caption waits in the audio
//!   queue and goes out when its sentence starts to play; `flush` empties the queue; everything else goes at once.
//!
//! The WebRTC stack (webrtc-rs) is compiled in; the Opus codec is the system's libopus, opened when an offer comes
//! ([`opus`]), as aiortc's native libraries were imported then: without it the daemon starts, every offer is refused
//! with [`RtcError`] and the page chats over HTTPS. Logs carry counts and types, never what was said.

pub mod call;
pub mod channel;
pub mod hub;
pub mod link;
pub mod opus;
pub mod voice;

use std::net::IpAddr;

use serde_json::Value;
use tracing::warn;

pub use call::RtcCallSocket;
pub use channel::{Channel, ChannelEvent, Data, Listener, ReadyState, WebrtcChannel};
pub use hub::{CallDoor, HubOptions, LinkDoor, Peer, RtcAnswer, RtcHub};
pub use link::{BodyReader, ChannelWriter, LinkChannel, LinkRequest};
pub use voice::{media_track, InboundAudio, MediaStreamError, Marker, VoiceFrame, VoiceMediaTrack, VoiceTrack};

use eve_link::link_chat::{MESSAGES_PATH, TURN_PATH};

// ---- signaling and peers --------------------------------------------------------------------------------------

/// An offer longer than this is refused before the WebRTC stack reads it.
pub const MAX_SDP_CHARS: usize = 20000;
/// One per device; past this the oldest peer is closed for the new one.
pub const MAX_PEERS: usize = 4;
/// A peer not connected this long after its answer is closed.
pub const CONNECT_SECS: f64 = 30.0;
/// How long a closing peer waits for the WebRTC stack, and for its call to end.
pub const CLOSE_SECS: f64 = 5.0;
pub const LINK_LABEL: &str = "link";
pub const CALL_LABEL: &str = "call";
pub const NO_AUDIO: &str = "This page connected without audio. Reload it to talk.";

// ---- the link channel ----------------------------------------------------------------------------------------

pub const LINK_PATHS: [&str; 2] = [TURN_PATH, MESSAGES_PATH];
/// Requests running at once on one channel.
pub const MAX_REQUESTS: usize = 8;
/// A request's body, UTF-8; the whole message must fit SCTP's 64 KiB.
pub const MAX_BODY_BYTES: usize = 60_000;
/// A message longer than this is not read at all.
pub const MAX_MESSAGE_CHARS: usize = 1 << 20;
pub const MAX_QUERY_CHARS: usize = 2048;
pub const MAX_HEADERS: usize = 32;
pub const MAX_HEADER_CHARS: usize = 2048;
/// LinkChat's heads are a few hundred bytes.
pub const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Body bytes in one binary frame, after the 4-byte id.
pub const BODY_FRAME_BYTES: usize = 16_384;
/// A writer's drain waits while more than this waits in the channel.
pub const BUFFER_HIGH: usize = 256 * 1024;
/// And looks again this often, woken or not.
pub const DRAIN_POLL_SECS: f64 = 0.5;
pub const MAX_ID: u64 = 0xFFFF_FFFF;
/// `^[a-z0-9][a-z0-9-]{0,63}$`
pub const HEADER_NAME_RE: &str = r"^[a-z0-9][a-z0-9-]{0,63}$";
/// The page's headers that never reach LinkChat: the door sets its own authorization and length, and a browser's
/// Origin would be refused (link_chat.py: any Origin is a web page).
pub const DROPPED_REQUEST_HEADERS: [&str; 9] = [
    "origin",
    "authorization",
    "cookie",
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "x-filename",
];
/// The body is de-chunked.
pub const DROPPED_RESPONSE_HEADERS: [&str; 3] = ["transfer-encoding", "connection", "keep-alive"];

// ---- the call --------------------------------------------------------------------------------------------------

/// 24 kHz mono 16-bit, both ways, as phone_call.Call.
pub const SAMPLE_RATE: u32 = eve_phone_call::SAMPLE_RATE;
/// 20 ms: 480 samples.
pub const VOICE_FRAME_SAMPLES: usize = SAMPLE_RATE as usize / 50;
pub const VOICE_FRAME_BYTES: usize = VOICE_FRAME_SAMPLES * 2;
/// 100 ms of the microphone per binary message: 4800.
pub const BLOCK_BYTES: usize = SAMPLE_RATE as usize / 10 * 2;
/// 300 ms kept before each talk, replayed after it.
pub const PREROLL_BYTES: usize = SAMPLE_RATE as usize * 3 / 10 * 2;
/// How long a done waits for the last RTP packets.
pub const POSTROLL_SECS: f64 = 0.25;
pub const MAX_PRESS_BYTES: usize =
    (eve_phone_call::MAX_PRESS_SECS * eve_phone_call::BYTES_PER_SEC as f64) as usize + BLOCK_BYTES;
/// Speech queued past ten minutes is dropped.
pub const MAX_VOICE_BYTES: usize = 600 * eve_phone_call::BYTES_PER_SEC;
/// A voice track this far behind starts again from now.
pub const MAX_LAG_SECS: f64 = 0.2;
/// A call message longer than this is not a control.
pub const MAX_CONTROL_CHARS: usize = 4096;

/// An offer this Mac will not or cannot answer. lite_web.py answers 400, and the page chats over HTTPS.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RtcError(pub String);

impl RtcError {
    pub fn new(message: impl Into<String>) -> Self {
        RtcError(message.into())
    }
}

/// Whether the WebRTC stack can answer here: webrtc-rs is compiled in, so whether libopus loads. Not only a missing
/// library: one without the calls fails too, and either way the page chats over HTTPS instead.
pub fn available() -> bool {
    match opus::lib() {
        Ok(_) => true,
        Err(e) => {
            warn!("lite rtc: libopus does not load ({}); the web app chats over HTTPS", opus_error_name(&e));
            false
        }
    }
}

fn opus_error_name(e: &opus::OpusError) -> &'static str {
    match e {
        opus::OpusError::Load(_) => "OSError",
        opus::OpusError::Code(_) => "OpusError",
    }
}

/// The peers' configuration: no ICE server at all. The phone and the Mac share a LAN, and nothing here may ask a
/// server elsewhere for an address (aiortc's `iceServers=None` would have added stun.l.google.com).
pub fn configuration() -> webrtc::peer_connection::RTCConfiguration {
    webrtc::peer_connection::RTCConfigurationBuilder::new().with_ice_servers(vec![]).build()
}

// ---- candidates ------------------------------------------------------------------------------------------------

/// The signaling request's peer as a plain IPv4 address (an IPv4-mapped IPv6 one unwrapped), or None for an IPv6
/// peer, a loopback one, or anything that is not an address.
pub fn peer_ipv4(addr: &str) -> Option<String> {
    let s = py::strip(addr);
    let s = s.trim_matches(|c| c == '[' || c == ']');
    let s = s.split('%').next().unwrap_or("");
    let ip: IpAddr = s.parse().ok()?;
    let v4 = match ip {
        IpAddr::V4(v4) => v4,
        IpAddr::V6(v6) => v6.to_ipv4_mapped()?,
    };
    if v4.is_loopback() || v4.is_unspecified() || v4.is_multicast() {
        return None;
    }
    Some(v4.to_string())
}

/// The offer with each host candidate whose address is an mDNS name (`….local`) pointed at `peer_addr`, the
/// signaling request's peer, when that is IPv4 and not loopback → (the SDP, how many were rewritten). The port stays:
/// it is the browser's own socket. Every other line, and each line ending, is kept as it was.
pub fn rewrite_local_candidates(sdp: &str, peer_addr: &str) -> (String, usize) {
    let Some(ip) = peer_ipv4(peer_addr) else { return (sdp.to_string(), 0) };
    if !sdp.to_lowercase().contains(".local") {
        return (sdp.to_string(), 0);
    }
    let mut lines: Vec<String> = sdp.split('\n').map(str::to_string).collect();
    let mut rewritten = 0;
    for line in lines.iter_mut() {
        if !line.starts_with("a=candidate:") {
            continue;
        }
        let (body, end) = match line.strip_suffix('\r') {
            Some(body) => (body.to_string(), "\r"),
            None => (line.clone(), ""),
        };
        // a=candidate:<foundation> <component> <transport> <priority> <address> <port> typ <type> ...
        let mut bits: Vec<&str> = body.split(' ').collect();
        if bits.len() >= 8 && bits[6] == "typ" && bits[7] == "host" && bits[4].to_lowercase().ends_with(".local") {
            bits[4] = &ip;
            *line = format!("{}{}", bits.join(" "), end);
            rewritten += 1;
        }
    }
    (lines.join("\n"), rewritten)
}

/// An ordered channel that retransmits until delivered: both channels' messages depend on their order.
pub fn reliable(channel: &dyn Channel) -> bool {
    channel.ordered() && channel.max_packet_life_time().is_none() && channel.max_retransmits().is_none()
}

/// One message on a data channel → whether it was queued. A closed channel is false, never an error.
pub fn send(channel: &dyn Channel, data: Data) -> bool {
    if channel.ready_state() != ReadyState::Open {
        return false;
    }
    channel.send(data).is_ok()
}

/// `json.dumps(obj, separators=(",", ":"))`: compact, non-ASCII escaped, as the page reads it.
pub fn text(obj: &Value) -> String {
    py::dumps(obj, true)
}

/// What the Python module did with its own small helpers: whitespace as `str.isspace`, `json.dumps`, `int()`,
/// `hmac.compare_digest`.
pub mod py {
    use serde_json::Value;

    /// `str.isspace` for one character: Unicode White_Space plus the four information separators.
    pub fn is_space(c: char) -> bool {
        c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
    }

    /// `str.strip()`.
    pub fn strip(s: &str) -> &str {
        s.trim_matches(is_space)
    }

    /// `bytes.strip()`: ASCII whitespace only.
    pub fn strip_bytes(b: &[u8]) -> &[u8] {
        let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
        let start = b.iter().position(|c| !ws(c)).unwrap_or(b.len());
        let end = b.iter().rposition(|c| !ws(c)).map_or(start, |i| i + 1);
        &b[start..end.max(start)]
    }

    /// `int(s, radix)` for radix 10 or 16 over an already stripped string: a sign, `0x` for base 16, digits with
    /// single underscores between them. Values past i128 saturate (Python's ints have no ceiling; nothing here needs
    /// one that large to be exact).
    pub fn int(s: &str, radix: u32) -> Option<i128> {
        let s = strip(s);
        let (neg, rest) = match s.as_bytes().first() {
            Some(b'-') => (true, &s[1..]),
            Some(b'+') => (false, &s[1..]),
            _ => (false, s),
        };
        let rest = if radix == 16 {
            rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")).map(|r| r.strip_prefix('_').unwrap_or(r)).unwrap_or(rest)
        } else {
            rest
        };
        if rest.is_empty() || rest.starts_with('_') || rest.ends_with('_') || rest.contains("__") {
            return None;
        }
        let mut v: i128 = 0;
        for c in rest.chars() {
            if c == '_' {
                continue;
            }
            let d = c.to_digit(radix)? as i128;
            v = v.saturating_mul(radix as i128).saturating_add(d);
        }
        Some(if neg { -v } else { v })
    }

    /// `hmac.compare_digest` over two byte strings: no early exit on the first difference.
    pub fn compare_digest(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    /// `json.dumps(v)` (`compact`: with `separators=(",", ":")`): keys in order, non-ASCII as `\uXXXX`.
    pub fn dumps(v: &Value, compact: bool) -> String {
        let mut out = String::new();
        write(&mut out, v, compact);
        out
    }

    fn write(out: &mut String, v: &Value, compact: bool) {
        let (item, key) = if compact { (",", ":") } else { (", ", ": ") };
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
                (Some(i), _, _) => out.push_str(&i.to_string()),
                (_, Some(u), _) => out.push_str(&u.to_string()),
                (_, _, Some(f)) => out.push_str(&float_repr(f)),
                _ => out.push_str(&n.to_string()),
            },
            Value::String(s) => string(out, s),
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push_str(item);
                    }
                    write(out, x, compact);
                }
                out.push(']');
            }
            Value::Object(m) => {
                out.push('{');
                for (i, (k, x)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push_str(item);
                    }
                    string(out, k);
                    out.push_str(key);
                    write(out, x, compact);
                }
                out.push('}');
            }
        }
    }

    /// `float.__repr__`: the shortest form that reads back, with Python's exponent style.
    fn float_repr(f: f64) -> String {
        if f.is_nan() {
            return "NaN".into();
        }
        if f.is_infinite() {
            return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() };
        }
        if f == f.trunc() && f.abs() < 1e16 {
            return format!("{f:.1}");
        }
        let s = format!("{f:e}");
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let e: i32 = exp.parse().unwrap_or(0);
        if (-4..16).contains(&e) {
            return format!("{f}");
        }
        format!("{mant}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
    }

    fn string(out: &mut String, s: &str) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{08}' => out.push_str("\\b"),
                '\u{0c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                    let mut buf = [0u16; 2];
                    for unit in c.encode_utf16(&mut buf) {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dumps_matches_python_json_dumps() {
        let v = json!({"type": "said", "text": "Café \u{1F600} \"q\"\n", "n": 3, "f": 0.5, "x": null, "a": [true, 1]});
        assert_eq!(
            py::dumps(&v, true),
            r#"{"type":"said","text":"Caf\u00e9 \ud83d\ude00 \"q\"\n","n":3,"f":0.5,"x":null,"a":[true,1]}"#
        );
        assert_eq!(py::dumps(&json!({"ok": false, "error": "not found"}), false), r#"{"ok": false, "error": "not found"}"#);
        assert_eq!(py::dumps(&json!("\u{7f}\u{1f}"), true), "\"\\u007f\\u001f\"");
        assert_eq!(py::dumps(&json!([1e-5, 1e16, 2.0, 0.0001]), true), "[1e-05,1e+16,2.0,0.0001]");
    }

    #[test]
    fn int_parses_as_python_int() {
        assert_eq!(py::int("1a", 16), Some(26));
        assert_eq!(py::int("0x1A", 16), Some(26));
        assert_eq!(py::int("1_0", 10), Some(10));
        assert_eq!(py::int("-3", 10), Some(-3));
        assert_eq!(py::int("zz", 16), None);
        assert_eq!(py::int("many", 10), None);
        assert_eq!(py::int("", 10), None);
        assert_eq!(py::int("1__0", 10), None);
    }

    #[test]
    fn compare_digest_is_equality() {
        assert!(py::compare_digest(b"abc", b"abc"));
        assert!(!py::compare_digest(b"abc", b"abd"));
        assert!(!py::compare_digest(b"abc", b"ab"));
    }
}
