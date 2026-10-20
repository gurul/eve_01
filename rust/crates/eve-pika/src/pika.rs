//! Pika, the owner's ELEGOO robot car, driven from the Eve Link app through this Mac (docs/pika.md).
//!
//! Owner, 2026-10-10: "can we build a controller for pika as an app", "Inside Eve Link, via the Mac", "very
//! minimalist". The app's Pika screen is live video with a hold-to-drive pad over it. It reaches the car only through
//! this daemon: `POST /api/link/controls/pika` with one JSON body (link_chat.LinkChat._controls), and the video at
//! `GET /api/link/controls/pika/stream`, both behind the link token.
//!
//! The car (github.com/gurul/pika). An ESP32-S3 camera board on the home Wi-Fi relays JSON frames between one TCP
//! client on port 100 and the UNO, and serves MJPEG at `http://<host>:81/stream`. What this file knows of it is ported
//! here, not imported: eve must not depend on that checkout's path. Each piece names its source.
//!
//! * A frame is compact JSON with a string `"H"` tag first (tools/car.py `Car.send`). Drive is the stock timed move
//!   `{"H":"<n>","N":2,"D1":<dir>,"D2":<speed>,"T":<ms>}` with direction 1 left, 2 right, 3 forward, 4 back
//!   (tools/car.py `DIR`; README "Wire protocol"). Stop is `{"H":"<n>","N":100}`, and the UNO answers it `{ok}`.
//! * The camera board sends `{Heartbeat}` once a second and drops a client that misses three; the client answers
//!   `{Heartbeat}` (tools/car.py `Car._reader`; firmware/camera_s3/camera_s3.ino).
//! * It serves ONE client at a time. A second connection is accepted by the network stack but never read until the
//!   first one leaves (camera_s3.ino `SocketServer_Test`), so a busy car is a connection that answers nothing. Eve
//!   sends a stop as its hello and waits HELLO_SECS for any frame back; silence means another controller has the car.
//! * Its video serves one viewer at a time (tools/controller.py; README "Browser controller"). JPEGs are cut out of the
//!   stream between the start and end markers, as tools/follow.py `Stream` does.
//!
//! The firmware's own deadman. `N=2` is a timed move: the UNO stops by itself `T` ms after the last drive frame
//! (firmware/uno_v4_mod `CMD_CarControlTimeLimit_xxx0`), and the camera board sends the UNO a stop when its client
//! disconnects. Eve never sends the untimed moves (`N=3`, or `N=4` wheel speeds with their 1.5 s deadline), so if
//! the Mac or the network dies mid-drive the car stops within PULSE_MS.
//!
//! Safety, on top of that. Hold to drive only:
//!
//! * While a direction is held the app sends `{"action": "drive", "dir", "hold"}` about every 150 ms. `hold` is one
//!   id per press. Each beat renews the timed move (at most every RESEND_SECS) and the heartbeat deadline.
//! * A stop is sent on release (`{"action": "stop", "hold"}`), when no beat came for HEARTBEAT_SECS, when the panel
//!   closes (`{"action": "close"}`), when the car's connection fails, and on any error. A stop is written to the
//!   socket at once: nothing is ever queued in front of it, and no lock is waited on.
//! * A press that was stopped stays stopped: a late beat of an ended hold (the phone's requests cross in the tunnel)
//!   is refused, never restarts the car. The owner lifts and presses again.
//!
//! The slot. The car takes one controller, so eve holds it only while the panel is used: it connects on the first
//! drive, lets go IDLE_SECS after the last command with nothing held, and at once on `close`. The video is the same:
//! one viewer, the newest wins, and `close` or the app hanging up ends it.
//!
//! Config. `~/.config/eve-bridge/pika.json` (no secrets): `{"host": "elegoo-car.local"}`, the name tools/car.py
//! uses (mDNS), or an address; optional `port` (100), `video_port` (81), `speed` (120) and `turn_speed` (140)
//! (calibration.json's `forward_speed` and `turn_speed`). It is off unless `EVE_PIKA=1` and the file is there;
//! then the Apps tab has no Pika. The log keeps actions and outcomes, never the address.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Weak};
use std::time::Duration;

use futures::future::{BoxFuture, FutureExt, Shared};
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::pika_rtc::{self, PikaRtc};

pub const CONFIG_PATH: &str = "~/.config/eve-bridge/pika.json";
pub const DEFAULT_HOST: &str = "elegoo-car.local"; // tools/car.py --host default
pub const DEFAULT_PORT: u16 = 100; // the camera board's JSON relay (tools/car.py)
pub const DEFAULT_VIDEO_PORT: u16 = 81; // its MJPEG server (tools/controller.py --video-port)
pub const DEFAULT_SPEED: i64 = 120; // calibration.json forward_speed; tools/car.py's default speed
pub const DEFAULT_TURN_SPEED: i64 = 140; // calibration.json turn_speed
/// pika_rtc.rs: the Mac's public address, for a phone off the home Wi-Fi
pub const DEFAULT_STUN: &str = "stun:stun.cloudflare.com:3478";

/// tools/car.py DIR (its "fwd" is "forward" here), in the Python dict's order.
pub const DIRS: [(&str, i64); 4] = [("left", 1), ("right", 2), ("forward", 3), ("back", 4)];
/// Each drive frame's timed move: the firmware deadman (tools/safety.py MAX_PULSE_MS; tools/controller.html's held
/// pulse).
pub const PULSE_MS: i64 = 450;
pub const RESEND_SECS: f64 = 0.2; // a held direction's frame is renewed at most this often
pub const HEARTBEAT_SECS: f64 = 0.45; // no beat from the app for this long: stop
pub const IDLE_SECS: f64 = 5.0; // nothing held and no command for this long: let go of the car's slot
pub const HELLO_SECS: f64 = 1.5; // after connecting, how long the car gets to answer the hello stop
pub const CONNECT_SECS: f64 = 3.0;
pub const TICK_SECS: f64 = 0.05; // the watchdog's period
pub const ENDED_KEPT: usize = 128; // ended holds remembered, so their late beats are refused
pub const MAX_HOLD_CHARS: usize = 64;

pub const VIDEO_HEAD_SECS: f64 = 4.0; // the car's answer to the video request
pub const VIDEO_FRAME_SECS: f64 = 5.0; // no new frame for this long: the video ended
pub const MAX_VIDEO_BUFFER: usize = 1 << 20; // bytes kept while looking for a JPEG's end
pub const VIDEO_HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\n\
Cache-Control: no-store\r\nX-Accel-Buffering: no\r\nTransfer-Encoding: chunked\r\n\
Connection: close\r\n\r\n";

pub const BUSY_LINE: &str = "Pika is busy: another controller has it. Close that one and try again.";
pub const UNREACHABLE_LINE: &str = "I can't reach Pika. Is it on, and on the home Wi-Fi?";
pub const LOST_LINE: &str = "Lost the connection to Pika. It stopped.";
pub const LATE_LINE: &str = "Stopped: the phone's signal came late. Press again to drive.";
pub const VIDEO_BUSY_LINE: &str = "Pika's video is in use by another viewer.";
pub const VIDEO_UNREACHABLE_LINE: &str = "I can't reach Pika's camera. Is it on, and on the home Wi-Fi?";

/// `^[A-Za-z0-9.-]{1,253}$`, matched against the stripped host.
pub static HOST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9.-]{1,253}$").expect("HOST_RE"));
/// Python's `re.match(r"^stun:...$")`: its `$` also matches before one final newline, so that is allowed here too.
pub static STUN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^stun:[A-Za-z0-9.-]{1,253}(:[0-9]{1,5})?\n?$").expect("STUN_RE"));
pub const STAT_PATHS: [&str; 2] = ["webrtc", "tunnel"];

/// asyncio's StreamReader default limit: a header line longer than this is an error.
const READ_LIMIT: usize = 1 << 16;

/// The car's direction number for a name (`DIRS.get`).
pub fn dir_number(name: &str) -> Option<i64> {
    DIRS.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

// ---- the seams ------------------------------------------------------------------------------------------------

/// Dials the car (or its camera): host and port to an open TCP stream. The default is [`open`], IPv4 only.
pub type Opener = Arc<dyn Fn(String, u16) -> BoxFuture<'static, io::Result<TcpStream>> + Send + Sync>;
/// Seconds on a monotonic clock (Python's `time.monotonic`).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
/// Builds one drive frame from (tag, direction, speed). The default is [`drive_frame`] with PULSE_MS; the tests put a
/// failing one here, as the Python tests patched `pika.drive_frame`.
pub type DriveFramer = Arc<dyn Fn(u32, i64, i64) -> io::Result<Vec<u8>> + Send + Sync>;

/// The default opener: [`open`].
pub fn default_opener() -> Opener {
    Arc::new(|host: String, port: u16| async move { open(&host, port).await }.boxed())
}

/// `time.monotonic()`: seconds since boot on a Mac, never 0 near start (so a fresh press always sends its frame).
pub fn monotonic() -> f64 {
    #[cfg(unix)]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: clock_gettime writes one timespec into the valid pointer we pass.
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        if rc == 0 {
            return ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9;
        }
    }
    static START: LazyLock<std::time::Instant> = LazyLock::new(std::time::Instant::now);
    1e6 + START.elapsed().as_secs_f64()
}

fn default_clock() -> Clock {
    Arc::new(monotonic)
}

fn default_drive_framer() -> DriveFramer {
    Arc::new(|tag, direction, speed| Ok(drive_frame(tag, direction, speed, PULSE_MS)))
}

// ---- config ---------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PikaConfig {
    pub host: String,
    pub port: u16,
    pub video_port: u16,
    pub speed: i64,
    pub turn_speed: i64,
    /// offer the video over WebRTC (pika_rtc.rs); false: the tunnel stream only
    pub rtc: bool,
    /// "" for none: then WebRTC works on the home Wi-Fi only
    pub stun: String,
}

impl Default for PikaConfig {
    fn default() -> Self {
        PikaConfig {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            video_port: DEFAULT_VIDEO_PORT,
            speed: DEFAULT_SPEED,
            turn_speed: DEFAULT_TURN_SPEED,
            rtc: true,
            stun: DEFAULT_STUN.to_string(),
        }
    }
}

/// Python's `str.isspace` set: Unicode white space plus the four separators U+001C..U+001F.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.strip()`.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// Off unless EVE_PIKA is on: a car that moves is never offered by default. `environ` None reads the process env.
pub fn enabled(environ: Option<&HashMap<String, String>>) -> bool {
    let raw = match environ {
        Some(env) => env.get("EVE_PIKA").cloned().unwrap_or_default(),
        None => std::env::var("EVE_PIKA").unwrap_or_default(),
    };
    matches!(py_strip(&raw).to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// `Path.expanduser()`: a leading `~` is the home directory.
pub(crate) fn expand_user(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s == "~" || s.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return if s == "~" { home } else { home.join(&s[2..]) };
        }
    }
    p.to_path_buf()
}

/// The config file, or None when it is missing or not a JSON object. Bad values fall back to the defaults.
pub fn load(path: Option<&Path>) -> Option<PikaConfig> {
    let p = expand_user(path.unwrap_or(Path::new(CONFIG_PATH)));
    let text = std::fs::read_to_string(&p).ok()?;
    let raw: Value = serde_json::from_str(&py_json_text(&text)).ok()?;
    let raw = raw.as_object()?;

    let number = |key: &str, default: i64, lo: i64, hi: i64| -> i64 {
        match raw.get(key) {
            None => default,
            Some(v) => match v.as_i64() {
                Some(i) if (lo..=hi).contains(&i) => i,
                _ => default,
            },
        }
    };

    let host = match raw.get("host") {
        None => DEFAULT_HOST.to_string(),
        Some(Value::String(h)) if HOST_RE.is_match(py_strip(h)) => py_strip(h).to_string(),
        Some(_) => DEFAULT_HOST.to_string(),
    };
    let rtc = match raw.get("rtc") {
        Some(Value::Bool(b)) => *b,
        _ => true,
    };
    let stun = match raw.get("stun") {
        None => DEFAULT_STUN.to_string(),
        Some(Value::String(s)) if s.is_empty() || STUN_RE.is_match(s) => s.clone(),
        Some(_) => DEFAULT_STUN.to_string(),
    };
    Some(PikaConfig {
        host,
        port: number("port", DEFAULT_PORT as i64, 1, 65535) as u16,
        rtc,
        stun,
        video_port: number("video_port", DEFAULT_VIDEO_PORT as i64, 1, 65535) as u16,
        speed: number("speed", DEFAULT_SPEED, 0, 255),
        turn_speed: number("turn_speed", DEFAULT_TURN_SPEED, 0, 255),
    })
}

/// The text `json.loads` accepts, made readable by serde_json without changing what `load` decides. Outside strings,
/// the bare `NaN`, `Infinity` and `-Infinity` tokens become `null`; inside strings, a `\uXXXX` surrogate escape that
/// is not half of a pair becomes `�`. Every key `load` reads treats a float and a null alike (the default), and
/// no host or STUN value with either character passes its check, so the result is Python's for every file.
pub(crate) fn py_json_text(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let hex4 = |at: usize| -> Option<u32> {
        let s = text.get(at..at + 4)?;
        if s.bytes().all(|c| c.is_ascii_hexdigit()) {
            u32::from_str_radix(s, 16).ok()
        } else {
            None
        }
    };
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'"' {
                in_str = false;
            } else if c == b'\\' && i + 1 < b.len() {
                if b[i + 1] == b'u' {
                    if let Some(u) = hex4(i + 2) {
                        let pair_low = |at: usize| {
                            text.get(at..at + 2) == Some("\\u") && hex4(at + 2).is_some_and(|l| (0xdc00..0xe000).contains(&l))
                        };
                        if (0xd800..0xdc00).contains(&u) && pair_low(i + 6) {
                            out.push_str(&text[i..i + 12]);
                            i += 12;
                            continue;
                        }
                        if (0xd800..0xe000).contains(&u) {
                            out.push_str("\\ufffd");
                            i += 6;
                            continue;
                        }
                    }
                }
                out.push('\\');
                i += 1;
                // the escaped character is copied below, whole
                let ch = text[i..].chars().next().unwrap_or('\u{fffd}');
                out.push(ch);
                i += ch.len_utf8();
                continue;
            }
        } else if c == b'"' {
            in_str = true;
        } else {
            let rest = &text[i..];
            let token = ["-Infinity", "Infinity", "NaN"].into_iter().find(|t| rest.starts_with(t));
            if let Some(t) = token {
                out.push_str("null");
                i += t.len();
                continue;
            }
        }
        // copy one whole character (the text is valid UTF-8)
        let ch = text[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The car's panel, or None when EVE_PIKA is off or there is no config (then the app shows no Pika). Making it
/// sends nothing: the first drive connects.
pub fn make_pika(path: Option<&Path>, environ: Option<&HashMap<String, String>>) -> Option<Pika> {
    if !enabled(environ) {
        info!("pika: off (EVE_PIKA)");
        return None;
    }
    let Some(cfg) = load(path) else {
        info!("pika: no config ({})", CONFIG_PATH);
        return None;
    };
    let car = Pika::new(cfg.clone());
    if cfg.rtc && pika_rtc::available() {
        car.set_rtc(Some(PikaRtc::new(&car, &cfg.stun)));
    }
    info!(
        "pika: on (video over {})",
        if car.rtc().is_some() { "webrtc, tunnel fallback" } else { "the tunnel" }
    );
    Some(car)
}

// ---- the wire (tools/car.py) ---------------------------------------------------------------------------------

/// One frame as tools/car.py `Car.send` writes it: `{"H":"<tag>",...}`, compact.
pub fn frame(tag: u32, fields: &[(&str, i64)]) -> Vec<u8> {
    let mut m = Map::new();
    m.insert("H".to_string(), Value::String(tag.to_string()));
    for (k, v) in fields {
        m.insert((*k).to_string(), json!(v));
    }
    serde_json::to_vec(&Value::Object(m)).unwrap_or_default()
}

pub fn drive_frame(tag: u32, direction: i64, speed: i64, ms: i64) -> Vec<u8> {
    frame(tag, &[("N", 2), ("D1", direction), ("D2", speed), ("T", ms)])
}

pub fn stop_frame(tag: u32) -> Vec<u8> {
    frame(tag, &[("N", 100)])
}

pub const HEARTBEAT: &[u8] = b"{Heartbeat}";

/// Complete `{...}` frames in `buf` and what is left (tools/car.py `Car._reader`).
pub fn split_frames(buf: &str) -> (Vec<String>, String) {
    let mut out = Vec::new();
    let mut buf = buf.to_string();
    while let Some(i) = buf.find('}') {
        let mut part = buf[..i].to_string();
        buf = buf[i + 1..].to_string();
        part.push('}');
        match part.rfind('{') {
            Some(j) => out.push(part[j..].to_string()),
            None => out.push(part),
        }
    }
    (out, buf)
}

// ---- the video (tools/follow.py Stream) ----------------------------------------------------------------------

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// Whole JPEGs (start marker to end marker) in `buf`, and the bytes to keep for the next read.
pub fn split_jpegs(buf: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut out = Vec::new();
    let mut buf = buf;
    loop {
        let a = find(buf, b"\xff\xd8", 0);
        let z = a.and_then(|a| find(buf, b"\xff\xd9", a + 2));
        match (a, z) {
            (Some(a), Some(z)) => {
                out.push(buf[a..z + 2].to_vec());
                buf = &buf[z + 2..];
            }
            (Some(a), None) => return (out, buf[a..].to_vec()),
            (None, _) => return (out, buf[buf.len().saturating_sub(1)..].to_vec()),
        }
    }
}

/// The Mac's wall clock in ms, put on every frame so the phone can measure how late it arrives.
pub fn stamp_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One part of the app's multipart stream, stamped with when it left the Mac.
pub fn video_part(jpeg: &[u8], stamp: Option<i64>) -> Vec<u8> {
    let mut out = format!(
        "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nX-Stamp: {}\r\n\r\n",
        jpeg.len(),
        stamp.unwrap_or_else(stamp_ms)
    )
    .into_bytes();
    out.extend_from_slice(jpeg);
    out.extend_from_slice(b"\r\n");
    out
}

/// asyncio's `LimitOverrunError`: `readuntil` found no separator within its limit.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct LimitOverrun(pub &'static str);

/// asyncio's `StreamReader.readuntil(sep)`: the bytes up to and including `sep`. EOF first is UnexpectedEof
/// (IncompleteReadError); no separator within `limit` bytes is InvalidData (LimitOverrunError).
pub async fn read_until<R: AsyncBufRead + Unpin>(reader: &mut R, sep: &[u8], limit: usize) -> io::Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    loop {
        let avail = reader.fill_buf().await?;
        if avail.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete read"));
        }
        let take = avail.len();
        let start = out.len().saturating_sub(sep.len() - 1);
        out.extend_from_slice(avail);
        if let Some(pos) = find(&out, sep, start) {
            let end = pos + sep.len();
            let extra = out.len() - end;
            reader.consume(take - extra);
            out.truncate(end);
            if end > limit {
                return Err(io::Error::new(io::ErrorKind::InvalidData, LimitOverrun("Separator is found, but chunk is longer than limit")));
            }
            return Ok(out);
        }
        reader.consume(take);
        if out.len() > limit {
            return Err(io::Error::new(io::ErrorKind::InvalidData, LimitOverrun("Separator is not found, and chunk exceed the limit")));
        }
    }
}

/// Python's `int(text, 16)` for a chunk size: blanks around it, an optional sign and `0x`, underscores between digits.
fn parse_hex(text: &[u8]) -> io::Result<u64> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "invalid literal for int() with base 16");
    let s = std::str::from_utf8(text).map_err(|_| bad())?;
    let s = py_strip(s);
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).map(|r| r.strip_prefix('_').unwrap_or(r)).unwrap_or(s);
    if s.is_empty() || s.starts_with('_') || s.ends_with('_') || s.contains("__") {
        return Err(bad());
    }
    let digits: String = s.chars().filter(|c| *c != '_').collect();
    let v = u64::from_str_radix(&digits, 16).map_err(|_| bad())?;
    if neg && v != 0 {
        // readexactly(-n) raises ValueError in Python
        return Err(io::Error::new(io::ErrorKind::InvalidData, "readexactly size can not be less than zero"));
    }
    Ok(v)
}

/// The response body's bytes: de-chunked when the camera's server sends it chunked (ESP-IDF's httpd does).
pub struct Body<R> {
    reader: R,
    chunked: bool,
    done: bool,
}

impl<R: AsyncBufRead + Unpin> Body<R> {
    pub fn new(reader: R, chunked: bool) -> Self {
        Body { reader, chunked, done: false }
    }

    /// The next piece of the body; None at its end. An error ends the body (as the Python generator raising).
    pub async fn next(&mut self) -> io::Result<Option<Vec<u8>>> {
        if self.done {
            return Ok(None);
        }
        if !self.chunked {
            let mut buf = vec![0u8; 16384];
            let n = self.reader.read(&mut buf).await?;
            if n == 0 {
                self.done = true;
                return Ok(None);
            }
            buf.truncate(n);
            return Ok(Some(buf));
        }
        let size_line = read_until(&mut self.reader, b"\r\n", READ_LIMIT).await?;
        let head = size_line.split(|b| *b == b';').next().unwrap_or(b"");
        let head = trim_bytes(head);
        let size = if head.is_empty() { 0 } else { parse_hex(head)? };
        if size == 0 {
            self.done = true;
            return Ok(None);
        }
        // Read what arrives rather than allocating the declared size up front: a bad size line (say `ffffffffffff`)
        // must end the body as Python's readexactly does at EOF, not exhaust memory.
        let mut data = Vec::with_capacity((size as usize).min(1 << 20));
        (&mut self.reader).take(size).read_to_end(&mut data).await?;
        if (data.len() as u64) < size {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete read"));
        }
        let mut crlf = [0u8; 2];
        self.reader.read_exact(&mut crlf).await?;
        Ok(Some(data))
    }
}

/// `bytes.strip()`: ASCII white space at both ends.
fn trim_bytes(b: &[u8]) -> &[u8] {
    let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c');
    let start = b.iter().position(|c| !ws(c)).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !ws(c)).map(|i| i + 1).unwrap_or(start);
    &b[start..end.max(start)]
}

/// getaddrinfo failed: Python's `socket.gaierror`, with its EAI code.
#[derive(Debug, thiserror::Error)]
#[error("[Errno {code}] {message}")]
pub struct GaiError {
    pub code: i32,
    pub message: String,
}

/// The host's IPv4 addresses (getaddrinfo with AF_INET), in the resolver's order.
pub async fn resolve_ipv4(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Ok(vec![SocketAddr::from((ip, port))]);
    }
    let host = host.to_string();
    tokio::task::spawn_blocking(move || getaddrinfo_v4(&host, port))
        .await
        .map_err(io::Error::other)?
}

#[cfg(unix)]
fn getaddrinfo_v4(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    use std::ffi::{CStr, CString};
    let c_host = CString::new(host)?;
    let c_port = CString::new(port.to_string())?;
    // SAFETY: an all-zero addrinfo is the documented "no hints" value; we then set family and socktype.
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = libc::AF_INET;
    hints.ai_socktype = libc::SOCK_STREAM;
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: valid NUL-terminated strings, a valid hints struct, and an out pointer we free below.
    let rc = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
    if rc != 0 {
        // SAFETY: gai_strerror returns a static NUL-terminated message for any code.
        let message = unsafe { CStr::from_ptr(libc::gai_strerror(rc)) }.to_string_lossy().into_owned();
        return Err(io::Error::other(GaiError { code: rc, message }));
    }
    let mut out = Vec::new();
    let mut cur = res;
    while !cur.is_null() {
        // SAFETY: cur walks the list getaddrinfo returned; each AF_INET entry's ai_addr is a sockaddr_in.
        unsafe {
            let ai = &*cur;
            if ai.ai_family == libc::AF_INET && !ai.ai_addr.is_null() {
                let sin = &*(ai.ai_addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                let addr = SocketAddr::from((ip, u16::from_be(sin.sin_port)));
                if !out.contains(&addr) {
                    out.push(addr);
                }
            }
            cur = ai.ai_next;
        }
    }
    // SAFETY: res came from a successful getaddrinfo and is freed once.
    unsafe { libc::freeaddrinfo(res) };
    Ok(out)
}

#[cfg(not(unix))]
fn getaddrinfo_v4(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    use std::net::ToSocketAddrs;
    Ok((host, port).to_socket_addrs()?.filter(|a| a.is_ipv4()).collect())
}

/// Dial the car. IPv4 only: the car has no IPv6, and macOS spends 5 s on the mDNS AAAA lookup before giving up,
/// longer than CONNECT_SECS. Measured 2026-10-10: getaddrinfo("elegoo-car.local") 5.06 s, with AF_INET 0.02 s.
pub async fn open(host: &str, port: u16) -> io::Result<TcpStream> {
    let mut last: Option<io::Error> = None;
    for addr in resolve_ipv4(host, port).await? {
        match TcpStream::connect(addr).await {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        io::Error::other(GaiError { code: 8, message: "nodename nor servname provided, or not known".into() })
    }))
}

/// The Python exception class name an I/O error would have been, for the log lines that name it.
pub fn py_error_name(e: &io::Error) -> &'static str {
    if e.get_ref().is_some_and(|inner| inner.is::<GaiError>()) {
        return "gaierror";
    }
    if e.get_ref().is_some_and(|inner| inner.is::<LimitOverrun>()) {
        return "LimitOverrunError";
    }
    match e.kind() {
        io::ErrorKind::ConnectionRefused => "ConnectionRefusedError",
        io::ErrorKind::ConnectionReset => "ConnectionResetError",
        io::ErrorKind::ConnectionAborted => "ConnectionAbortedError",
        io::ErrorKind::BrokenPipe => "BrokenPipeError",
        io::ErrorKind::TimedOut => "TimeoutError",
        io::ErrorKind::PermissionDenied => "PermissionError",
        io::ErrorKind::NotFound => "FileNotFoundError",
        io::ErrorKind::UnexpectedEof => "IncompleteReadError",
        io::ErrorKind::NotConnected if e.raw_os_error().is_none() => "ConnectionError", // `_write`'s error
        _ => "OSError",
    }
}

/// `getattr(e, "errno", None) or ""`.
fn py_errno(e: &io::Error) -> String {
    if let Some(g) = e.get_ref().and_then(|inner| inner.downcast_ref::<GaiError>()) {
        return g.code.to_string();
    }
    match e.raw_os_error() {
        Some(n) if n != 0 => n.to_string(),
        _ => String::new(),
    }
}

/// One HTTP chunk.
pub async fn write_chunk<W: AsyncWrite + Unpin>(writer: &mut W, data: &[u8]) -> io::Result<()> {
    let mut out = format!("{:x}\r\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
    writer.write_all(&out).await?;
    writer.flush().await
}

/// A whole JSON response, `json.dumps` formatted, closing the connection.
pub async fn write_json<W: AsyncWrite + Unpin>(writer: &mut W, status: u16, obj: &Value) -> io::Result<()> {
    let body = py_json_dumps(obj).into_bytes();
    let reason = match status {
        503 => "Service Unavailable",
        _ => "OK",
    };
    let mut out = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(&body);
    writer.write_all(&out).await?;
    writer.flush().await
}

/// Python's `json.dumps(obj)` with its defaults: `", "` and `": "` separators, non-ASCII escaped.
pub fn py_json_dumps(v: &Value) -> String {
    let mut out = String::new();
    dump(v, &mut out);
    out
}

fn dump(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                out.push_str(&py_float_repr(n.as_f64().unwrap_or(0.0)));
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::String(s) => dump_str(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump(x, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_str(k, out);
                out.push_str(": ");
                dump(x, out);
            }
            out.push('}');
        }
    }
}

fn dump_str(s: &str, out: &mut String) {
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
            // ensure_ascii escapes everything outside ' '..='~', DEL included
            c if (c as u32) < 0x20 || (c as u32) >= 0x7f => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Python's `repr(float)`: shortest round-trip digits, exponent past 1e16 or under 1e-4 as `1e+16` / `1e-05`.
fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let s = format!("{:?}", f);
    match s.split_once('e') {
        Some((mant, exp)) => {
            let (sign, digits) = match exp.strip_prefix('-') {
                Some(d) => ('-', d),
                None => ('+', exp),
            };
            let mant = mant.strip_suffix(".0").unwrap_or(mant);
            format!("{mant}e{sign}{digits:0>2}")
        }
        None => s,
    }
}

/// A plain line for the owner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PikaError(pub String);

impl PikaError {
    pub fn new(line: &str) -> Self {
        PikaError(line.to_string())
    }
}

// ---- small async primitives -----------------------------------------------------------------------------------

/// `asyncio.Event`: set stays set until cleared; waiters wake on set.
#[derive(Default)]
pub(crate) struct Event {
    flag: AtomicBool,
    notify: Notify,
}

impl Event {
    pub(crate) fn set(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub(crate) fn clear(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }

    pub(crate) fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    pub(crate) async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_set() {
                return;
            }
            notified.await;
        }
    }
}

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s.max(0.0))
}

fn is_current(handle: &JoinHandle<()>) -> bool {
    tokio::task::try_id() == Some(handle.id())
}

// ---- the car's socket -----------------------------------------------------------------------------------------

enum WriteCmd {
    Data(Vec<u8>),
    Drain(oneshot::Sender<()>),
    Close,
}

/// The car's TCP writer: `write` queues at once and never awaits (asyncio's `writer.write`); a task writes them out
/// in order. `close` flushes what is queued, then closes the write side.
#[derive(Clone)]
struct Conn {
    id: u64,
    tx: mpsc::UnboundedSender<WriteCmd>,
    closing: Arc<AtomicBool>,
}

impl Conn {
    fn spawn(id: u64, mut w: OwnedWriteHalf) -> Conn {
        let (tx, mut rx) = mpsc::unbounded_channel::<WriteCmd>();
        let closing = Arc::new(AtomicBool::new(false));
        let flag = closing.clone();
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    WriteCmd::Data(d) => {
                        if w.write_all(&d).await.is_err() {
                            break;
                        }
                    }
                    WriteCmd::Drain(done) => {
                        let _ = w.flush().await;
                        let _ = done.send(());
                    }
                    WriteCmd::Close => break,
                }
            }
            flag.store(true, Ordering::SeqCst);
            let _ = w.shutdown().await;
        });
        Conn { id, tx, closing }
    }

    fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst) || self.tx.is_closed()
    }

    fn write(&self, data: &[u8]) -> io::Result<()> {
        if self.is_closing() {
            return Err(not_connected());
        }
        self.tx.send(WriteCmd::Data(data.to_vec())).map_err(|_| not_connected())
    }

    fn close(&self) {
        if !self.closing.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(WriteCmd::Close);
        }
    }
}

fn not_connected() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "not connected")
}

// ---- the panel -----------------------------------------------------------------------------------------------

/// The one dial in flight: its shared result, and the task's handle (Python's `self._connecting.done()`).
type Connecting = (Shared<BoxFuture<'static, Result<(), PikaError>>>, tokio::task::AbortHandle);

struct VideoHolder {
    id: u64,
    task: Option<tokio::task::Id>,
    cancel: Arc<Event>,
    done: Arc<Event>,
}

/// The one viewer slot, held by the tunnel stream or the WebRTC sender. Dropping it gives the slot back.
pub(crate) struct VideoSlot {
    pika: Pika,
    id: u64,
    cancel: Arc<Event>,
    done: Arc<Event>,
}

impl VideoSlot {
    /// Resolves when a newer viewer (or `close`) takes the camera.
    pub(crate) async fn cancelled(&self) {
        self.cancel.wait().await
    }
}

impl Drop for VideoSlot {
    fn drop(&mut self) {
        self.pika.left_video(self.id);
        self.done.set();
    }
}

struct State {
    conn: Option<Conn>,
    next_conn: u64,
    hello_ok: bool, // the car answered this connection's hello: it is ours
    rx: Option<JoinHandle<()>>,
    watch: Option<JoinHandle<()>>,
    connecting: Option<Connecting>,
    answered: Option<Arc<Event>>,
    tag: u32,
    held: Option<String>, // the hold driving now
    dir: i64,
    deadline: f64, // the held direction's heartbeat deadline
    sent_at: f64,  // when its frame was last written
    used: f64,     // the last command
    epoch: u64,    // bumped by every stop: a drive that waited on a connect re-checks
    ended: IndexMap<String, ()>,
    note: String, // why the last hold was stopped, said to its next beat
    video: Option<VideoHolder>,
    next_video: u64,
    stops: Vec<String>, // why each stop was sent (the tests read it)
}

impl State {
    fn new() -> State {
        State {
            conn: None,
            next_conn: 0,
            hello_ok: false,
            rx: None,
            watch: None,
            connecting: None,
            answered: None,
            tag: 0,
            held: None,
            dir: 0,
            deadline: 0.0,
            sent_at: 0.0,
            used: 0.0,
            epoch: 0,
            ended: IndexMap::new(),
            note: String::new(),
            video: None,
            next_video: 0,
            stops: Vec::new(),
        }
    }

    // Only once the car has answered the hello: during the wait a second drive beat must wait on the same
    // connect (_connect) rather than write a drive frame to a car that has not shown it is free.
    fn connected(&self) -> bool {
        self.hello_ok && self.conn.as_ref().is_some_and(|c| !c.is_closing())
    }

    fn next_tag(&mut self) -> u32 {
        self.tag = self.tag % 99999 + 1;
        self.tag
    }

    fn end(&mut self, hold: Option<&str>) {
        if let Some(h) = hold {
            if !h.is_empty() {
                if !self.ended.contains_key(h) {
                    self.ended.insert(h.to_string(), ());
                }
                while self.ended.len() > ENDED_KEPT {
                    self.ended.shift_remove_index(0);
                }
            }
        }
    }

    fn write(&self, data: &[u8]) -> io::Result<()> {
        match &self.conn {
            Some(c) if !c.is_closing() => c.write(data),
            _ => Err(not_connected()),
        }
    }

    /// Stop now. Ends the held press and writes a stop if the car is connected; never awaits.
    fn stop(&mut self, why: &str) {
        self.epoch += 1;
        if let Some(h) = self.held.take() {
            self.end(Some(&h));
            self.dir = 0;
        }
        if self.connected() {
            self.stops.push(why.to_string());
            let tag = self.next_tag();
            if self.write(&stop_frame(tag)).is_err() {
                self.drop_conn();
            }
            info!("pika: stop ({})", why);
        }
    }

    fn drop_conn(&mut self) {
        if let Some(c) = self.conn.take() {
            c.close();
        }
        self.hello_ok = false;
        if let Some(rx) = self.rx.take() {
            if !is_current(&rx) {
                rx.abort();
            }
        }
    }
}

struct Inner {
    cfg: PikaConfig,
    opener: Opener,
    clock: Clock,
    heartbeat_secs: f64,
    idle_secs: f64,
    hello_secs: f64,
    tick_secs: f64,
    drive_framer: DriveFramer,
    state: Mutex<State>,
    rtc: Mutex<Option<PikaRtc>>, // the video over WebRTC (pika_rtc.rs)
}

/// The knobs `Pika(cfg, *, opener, clock, heartbeat_secs, idle_secs, hello_secs, tick_secs)` takes, plus the drive
/// frame seam.
#[derive(Clone)]
pub struct PikaOptions {
    pub opener: Opener,
    pub clock: Clock,
    pub heartbeat_secs: f64,
    pub idle_secs: f64,
    pub hello_secs: f64,
    pub tick_secs: f64,
    pub drive_framer: DriveFramer,
}

impl Default for PikaOptions {
    fn default() -> Self {
        PikaOptions {
            opener: default_opener(),
            clock: default_clock(),
            heartbeat_secs: HEARTBEAT_SECS,
            idle_secs: IDLE_SECS,
            hello_secs: HELLO_SECS,
            tick_secs: TICK_SECS,
            drive_framer: default_drive_framer(),
        }
    }
}

enum DriveError {
    Pika(PikaError),
    Io(io::Error),
}

/// The Pika screen's `panel(body)` and video `stream(writer)`. One per daemon; clones share it.
#[derive(Clone)]
pub struct Pika {
    inner: Arc<Inner>,
}

/// A handle that does not keep the panel alive (PikaRtc holds one, so the two do not keep each other).
#[derive(Clone)]
pub struct WeakPika(Weak<Inner>);

impl WeakPika {
    pub fn upgrade(&self) -> Option<Pika> {
        self.0.upgrade().map(|inner| Pika { inner })
    }
}

impl Pika {
    /// `Pika.available`: the app lists Pika whenever a panel exists.
    pub const AVAILABLE: bool = true;

    pub fn new(cfg: PikaConfig) -> Pika {
        Pika::with_options(cfg, PikaOptions::default())
    }

    pub fn with_options(cfg: PikaConfig, opts: PikaOptions) -> Pika {
        Pika {
            inner: Arc::new(Inner {
                cfg,
                opener: opts.opener,
                clock: opts.clock,
                heartbeat_secs: opts.heartbeat_secs,
                idle_secs: opts.idle_secs,
                hello_secs: opts.hello_secs,
                tick_secs: opts.tick_secs,
                drive_framer: opts.drive_framer,
                state: Mutex::new(State::new()),
                rtc: Mutex::new(None),
            }),
        }
    }

    pub fn downgrade(&self) -> WeakPika {
        WeakPika(Arc::downgrade(&self.inner))
    }

    pub fn cfg(&self) -> &PikaConfig {
        &self.inner.cfg
    }

    /// The WebRTC video, when it is on.
    pub fn rtc(&self) -> Option<PikaRtc> {
        self.inner.rtc.lock().clone()
    }

    pub fn set_rtc(&self, rtc: Option<PikaRtc>) {
        *self.inner.rtc.lock() = rtc;
    }

    /// Why each stop was sent, in order (the tests read it).
    pub fn stops(&self) -> Vec<String> {
        self.inner.state.lock().stops.clone()
    }

    /// Whether a viewer holds the camera now (Python's `_video is not None`).
    pub fn video_active(&self) -> bool {
        self.inner.state.lock().video.is_some()
    }

    // -- state --
    pub fn connected(&self) -> bool {
        self.inner.state.lock().connected()
    }

    fn now(&self) -> f64 {
        (self.inner.clock)()
    }

    // -- the panel --
    pub async fn panel(&self, body: &Value) -> Value {
        let empty = Map::new();
        let body = body.as_object().unwrap_or(&empty);
        let action = match body.get("action") {
            Some(v) if truthy(v) => match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            },
            _ => "status".to_string(),
        };
        let hold: String = match body.get("hold") {
            Some(Value::String(h)) => py_strip(h).chars().take(MAX_HOLD_CHARS).collect(),
            _ => String::new(),
        };
        match action.as_str() {
            "status" => {
                let connected = self.connected();
                return json!({"ok": true, "connected": connected, "rtc": self.rtc().is_some(), "line": ""});
            }
            "rtc" => return self.rtc_answer(body.get("sdp")).await,
            "stats" => return self.stats(body),
            "stop" => {
                let now = self.now();
                let mut s = self.inner.state.lock();
                s.end(Some(&hold));
                s.stop("release");
                s.used = now;
                return json!({"ok": true, "line": ""});
            }
            "close" => {
                self.inner.state.lock().end(Some(&hold));
                self.close("panel closed").await;
                return json!({"ok": true, "line": ""});
            }
            "drive" => {}
            _ => return json!({"ok": false, "line": "That isn't something Pika does."}),
        }
        let direction = match body.get("dir") {
            Some(Value::String(d)) => dir_number(d),
            _ => None,
        };
        let Some(direction) = direction.filter(|_| !hold.is_empty()) else {
            return json!({"ok": false, "line": "Which way?"});
        };
        match self.drive(&hold, direction).await {
            Ok(v) => v,
            Err(DriveError::Pika(e)) => {
                let mut s = self.inner.state.lock();
                s.end(Some(&hold));
                s.stop("error");
                json!({"ok": false, "stopped": true, "line": e.0})
            }
            Err(DriveError::Io(e)) => {
                warn!("pika: drive failed ({})", py_error_name(&e));
                let mut s = self.inner.state.lock();
                s.end(Some(&hold));
                s.stop("error");
                s.drop_conn();
                json!({"ok": false, "stopped": true, "line": LOST_LINE})
            }
        }
    }

    /// Answer the app's WebRTC offer for the video (pika_rtc.rs). `rtc: false`: use the tunnel stream.
    async fn rtc_answer(&self, sdp: Option<&Value>) -> Value {
        let refused = json!({"ok": false, "rtc": false, "line": ""});
        let Some(rtc) = self.rtc() else {
            return refused;
        };
        let sdp = match sdp {
            Some(Value::String(s)) if s.starts_with("v=0") && s.chars().count() <= 20000 => s.clone(),
            _ => return refused,
        };
        match tokio::time::timeout(Duration::from_secs_f64(8.0), rtc.answer(&sdp)).await {
            Ok(Ok(answer)) => json!({"ok": true, "rtc": true, "sdp": answer, "line": ""}),
            // the WebRTC stack raises its own kinds; the tunnel still works
            Ok(Err(e)) => {
                info!("pika: webrtc answer failed ({})", rtc_error_name(&e));
                refused
            }
            Err(_) => {
                info!("pika: webrtc answer failed (TimeoutError)");
                refused
            }
        }
    }

    /// The phone's latency summary for one video path: logged, so the two paths can be compared.
    fn stats(&self, body: &Map<String, Value>) -> Value {
        let path = match body.get("path") {
            Some(Value::String(p)) if STAT_PATHS.contains(&p.as_str()) => Some(p.as_str()),
            _ => None,
        };
        let num = |key: &str| -> Option<f64> {
            match body.get(key) {
                Some(Value::Number(n)) => n.as_f64().filter(|v| v.abs() < 1e7),
                _ => None,
            }
        };
        match (path, num("n"), num("p50"), num("p95"), num("fps")) {
            (Some(path), Some(n), Some(p50), Some(p95), Some(fps)) => {
                info!("{}", stats_line(path, n, p50, p95, fps));
                json!({"ok": true, "line": ""})
            }
            _ => json!({"ok": false, "line": ""}),
        }
    }

    async fn drive(&self, hold: &str, direction: i64) -> Result<Value, DriveError> {
        let epoch;
        let connected;
        {
            let mut s = self.inner.state.lock();
            if s.ended.contains_key(hold) {
                return Ok(json!({"ok": false, "stopped": true, "line": s.note}));
            }
            epoch = s.epoch;
            s.used = self.now();
            connected = s.connected();
        }
        if !connected {
            self.connect().await.map_err(DriveError::Pika)?;
        }
        let mut s = self.inner.state.lock();
        if epoch != s.epoch || s.ended.contains_key(hold) {
            // a stop came while connecting: this press is over
            s.end(Some(hold));
            return Ok(json!({"ok": false, "stopped": true, "line": s.note}));
        }
        let now = self.now();
        if s.held.as_deref() != Some(hold) {
            let old = s.held.take();
            s.end(old.as_deref()); // a new press replaces the old one
            s.held = Some(hold.to_string());
            s.dir = direction;
            s.sent_at = 0.0;
            s.note.clear();
        } else if s.dir != direction {
            s.dir = direction;
            s.sent_at = 0.0;
        }
        s.deadline = now + self.inner.heartbeat_secs;
        s.used = now;
        if now - s.sent_at >= RESEND_SECS {
            let speed = if direction == 3 || direction == 4 { self.inner.cfg.speed } else { self.inner.cfg.turn_speed };
            let tag = s.next_tag();
            let data = (self.inner.drive_framer)(tag, direction, speed).map_err(DriveError::Io)?;
            s.write(&data).map_err(DriveError::Io)?;
            s.sent_at = now;
        }
        Ok(json!({"ok": true, "line": ""}))
    }

    // -- stop: written at once, never behind anything --
    /// Stop now. Ends the held press and writes a stop if the car is connected; never awaits.
    pub fn stop(&self, why: &str) {
        self.inner.state.lock().stop(why);
    }

    // -- the connection --
    async fn connect(&self) -> Result<(), PikaError> {
        let fut = {
            let mut s = self.inner.state.lock();
            // A finished dial is never reused, even when no caller has read its result yet (a caller that gave up):
            // Python makes a new task once `_connecting.done()`.
            match &s.connecting {
                Some((f, task)) if !task.is_finished() => f.clone(),
                _ => {
                    let me = self.clone();
                    let task = tokio::spawn(async move { me.dial().await });
                    let handle = task.abort_handle();
                    let f = async move { task.await.unwrap_or_else(|_| Err(PikaError::new(UNREACHABLE_LINE))) }
                        .boxed()
                        .shared();
                    s.connecting = Some((f.clone(), handle));
                    f
                }
            }
        };
        fut.await
    }

    async fn dial(&self) -> Result<(), PikaError> {
        let cfg = &self.inner.cfg;
        let opened = tokio::time::timeout(secs(CONNECT_SECS), (self.inner.opener)(cfg.host.clone(), cfg.port)).await;
        let stream = match opened {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                info!("pika: car unreachable ({} {})", py_error_name(&e), py_errno(&e));
                return Err(PikaError::new(UNREACHABLE_LINE));
            }
            Err(_) => {
                info!("pika: car unreachable (TimeoutError )");
                return Err(PikaError::new(UNREACHABLE_LINE));
            }
        };
        let (rd, wr) = stream.into_split();
        let answered = Arc::new(Event::default());
        {
            let mut s = self.inner.state.lock();
            s.hello_ok = false;
            if let Some(old) = s.conn.take() {
                old.close();
            }
            s.next_conn += 1;
            let conn = Conn::spawn(s.next_conn, wr);
            s.conn = Some(conn.clone());
            s.answered = Some(answered.clone());
            let me = self.clone();
            s.rx = Some(tokio::spawn(async move { me.read(rd, conn).await }));
            let tag = s.next_tag();
            if s.write(&stop_frame(tag)).is_err() {
                // the hello: harmless, and the UNO answers it
                s.drop_conn();
                return Err(PikaError::new(UNREACHABLE_LINE));
            }
        }
        if tokio::time::timeout(secs(self.inner.hello_secs), answered.wait()).await.is_err() {
            info!("pika: the car did not answer (another controller has it)");
            self.inner.state.lock().drop_conn();
            return Err(PikaError::new(BUSY_LINE));
        }
        {
            let mut s = self.inner.state.lock();
            s.hello_ok = true;
            if s.watch.as_ref().is_none_or(|w| w.is_finished()) {
                let me = self.clone();
                s.watch = Some(tokio::spawn(async move { me.watchdog().await }));
            }
        }
        info!("pika: connected");
        Ok(())
    }

    async fn read(&self, mut rd: OwnedReadHalf, conn: Conn) {
        let mut buf = String::new();
        let mut data = [0u8; 1024];
        loop {
            match rd.read(&mut data).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.push_str(&String::from_utf8_lossy(&data[..n]));
                    let (frames, rest) = split_frames(&buf);
                    buf = rest;
                    if buf.chars().count() > 4096 {
                        buf.clear();
                    }
                    let answered = self.inner.state.lock().answered.clone();
                    for f in frames {
                        if let Some(a) = &answered {
                            a.set();
                        }
                        if f == "{Heartbeat}" && !conn.is_closing() {
                            let _ = conn.write(HEARTBEAT);
                        }
                    }
                }
            }
        }
        let mut s = self.inner.state.lock();
        if s.conn.as_ref().map(|c| c.id) == Some(conn.id) {
            // the car is gone; its camera board stops the UNO when its client leaves (camera_s3.ino)
            info!("pika: the car's connection ended");
            if s.held.is_some() {
                s.note = LOST_LINE.to_string();
            }
            let held = s.held.take();
            s.end(held.as_deref());
            s.epoch += 1;
            s.drop_conn();
        }
    }

    async fn watchdog(&self) {
        while self.connected() {
            tokio::time::sleep(secs(self.inner.tick_secs)).await;
            let now = self.now();
            let release = {
                let mut s = self.inner.state.lock();
                if s.held.is_some() && now > s.deadline {
                    s.note = LATE_LINE.to_string();
                    s.stop("heartbeat");
                    false
                } else if s.held.is_none() && now - s.used > self.inner.idle_secs {
                    info!("pika: idle, letting go of the car");
                    s.stop("idle");
                    true
                } else {
                    false
                }
            };
            if release {
                self.release().await;
                return;
            }
        }
    }

    async fn release(&self) {
        let conn = self.inner.state.lock().conn.clone();
        if let Some(c) = conn {
            let (tx, rx) = oneshot::channel();
            if c.tx.send(WriteCmd::Drain(tx)).is_ok() {
                // the stop goes out before the socket closes
                let _ = tokio::time::timeout(Duration::from_millis(500), rx).await;
            }
        }
        self.inner.state.lock().drop_conn();
    }

    /// Stop, let go of the car's slot and end the video (the panel closed, or the daemon stops).
    pub async fn close(&self, why: &str) {
        self.stop(why);
        self.release().await;
        let watch = self.inner.state.lock().watch.take();
        if let Some(w) = watch {
            if !is_current(&w) {
                w.abort();
            }
        }
        self.end_video().await;
        if let Some(rtc) = self.rtc() {
            rtc.close().await;
        }
    }

    // -- the video --
    /// The car's camera as whole JPEGs, the newest only: a reader that falls behind skips frames, never queues
    /// them. Yields PikaError with the owner's line when the camera can't be reached or is busy; ends when the
    /// camera ends or goes quiet for VIDEO_FRAME_SECS. The caller holds the one viewer slot (`take_video`).
    pub fn frames(&self) -> Frames {
        Frames {
            opener: self.inner.opener.clone(),
            host: self.inner.cfg.host.clone(),
            port: self.inner.cfg.video_port,
            started: false,
            finished: false,
            puller: None,
            latest: Arc::new(Mutex::new(None)),
            fresh: Arc::new(Event::default()),
        }
    }

    /// The camera serves one viewer: the newest, over either path, ends the one before it.
    pub(crate) async fn take_video(&self, keep_rtc: bool) -> VideoSlot {
        self.end_video().await;
        if !keep_rtc {
            if let Some(rtc) = self.rtc() {
                rtc.close().await;
            }
        }
        let mut s = self.inner.state.lock();
        s.next_video += 1;
        let id = s.next_video;
        let cancel = Arc::new(Event::default());
        let done = Arc::new(Event::default());
        s.video = Some(VideoHolder {
            id,
            task: tokio::task::try_id(),
            cancel: cancel.clone(),
            done: done.clone(),
        });
        VideoSlot { pika: self.clone(), id, cancel, done }
    }

    fn left_video(&self, id: u64) {
        let mut s = self.inner.state.lock();
        if s.video.as_ref().is_some_and(|v| v.id == id) {
            s.video = None;
        }
        info!("pika: video off");
    }

    /// The car's video to the app as multipart JPEGs through the tunnel, each part stamped `X-Stamp` with the
    /// Mac's clock in ms (the phone measures latency with it). WebRTC's fallback (pika_rtc.rs). Returns when the
    /// video ends, the app hangs up, or a newer viewer takes the camera.
    pub async fn stream<W: AsyncWrite + Unpin + Send>(&self, writer: &mut W) {
        let slot = self.take_video(false).await;
        tokio::select! {
            _ = slot.cancelled() => {}
            _ = self.stream_to(writer) => {}
        }
        drop(slot);
    }

    async fn stream_to<W: AsyncWrite + Unpin + Send>(&self, writer: &mut W) {
        // an I/O error is the app hanging up
        let _ = async {
            let mut frames = self.frames();
            let first = match frames.next().await {
                Some(Ok(jpeg)) => jpeg,
                Some(Err(e)) => {
                    write_json(writer, 503, &json!({"ok": false, "line": e.0})).await?;
                    return Ok(());
                }
                None => {
                    write_json(writer, 503, &json!({"ok": false, "line": VIDEO_BUSY_LINE})).await?;
                    return Ok(());
                }
            };
            writer.write_all(VIDEO_HEAD).await?;
            writer.flush().await?;
            info!("pika: video on (tunnel)");
            write_chunk(writer, &video_part(&first, None)).await?;
            let sent: io::Result<()> = async {
                while let Some(Ok(jpeg)) = frames.next().await {
                    write_chunk(writer, &video_part(&jpeg, None)).await?;
                }
                Ok(())
            }
            .await;
            drop(frames);
            sent?;
            let _ = write_chunk(writer, b"").await;
            Ok::<(), io::Error>(())
        }
        .await;
    }

    async fn end_video(&self) {
        let holder = self.inner.state.lock().video.take();
        if let Some(h) = holder {
            h.cancel.set();
            let me = tokio::task::try_id();
            if h.task.is_none() || h.task != me {
                h.done.wait().await;
            }
        }
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// The log line for one video path's stats, as `"pika: video stats %s n=%d p50=%.0f p95=%.0f fps=%.1f"` prints it.
pub fn stats_line(path: &str, n: f64, p50: f64, p95: f64, fps: f64) -> String {
    format!("pika: video stats {} n={} p50={:.0} p95={:.0} fps={:.1}", path, n.trunc() as i64, p50, p95, fps)
}

fn rtc_error_name(e: &anyhow::Error) -> String {
    if let Some(io) = e.downcast_ref::<io::Error>() {
        return py_error_name(io).to_string();
    }
    // the root cause's Debug starts with its variant name (`ErrSessionDescriptionMissingIceUfrag`, `Sdp(..)`)
    let dbg = format!("{:?}", e.root_cause());
    match dbg.split(|c: char| !c.is_alphanumeric() && c != '_').next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "Error".to_string(),
    }
}

/// The camera's JPEGs (see [`Pika::frames`]). It opens the camera on the first `next`; dropping it closes the camera.
pub struct Frames {
    opener: Opener,
    host: String,
    port: u16,
    started: bool,
    finished: bool,
    puller: Option<JoinHandle<()>>,
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    fresh: Arc<Event>,
}

impl Frames {
    /// The next JPEG; `Some(Err)` once, first, when the camera can't be reached or is busy; None when it ended.
    pub async fn next(&mut self) -> Option<Result<Vec<u8>, PikaError>> {
        if self.finished {
            return None;
        }
        if !self.started {
            self.started = true;
            if let Err(e) = self.start().await {
                self.finished = true;
                return Some(Err(e));
            }
        }
        loop {
            let got = if self.fresh.is_set() {
                true
            } else {
                match self.puller.as_mut() {
                    None => false,
                    Some(puller) => {
                        let mut puller_done = false;
                        let got = tokio::select! {
                            biased;
                            _ = self.fresh.wait() => true,
                            _ = puller => { puller_done = true; self.fresh.is_set() }
                            _ = tokio::time::sleep(secs(VIDEO_FRAME_SECS)) => false,
                        };
                        if puller_done {
                            self.puller = None;
                        }
                        got
                    }
                }
            };
            if !got {
                // the camera ended, failed or went quiet
                self.finish();
                return None;
            }
            self.fresh.clear();
            if let Some(jpeg) = self.latest.lock().take() {
                return Some(Ok(jpeg));
            }
        }
    }

    fn finish(&mut self) {
        self.finished = true;
        if let Some(p) = self.puller.take() {
            p.abort();
        }
    }

    async fn start(&mut self) -> Result<(), PikaError> {
        let unreachable = |e: &io::Error| {
            info!("pika: camera unreachable ({} {})", py_error_name(e), py_errno(e));
            PikaError::new(VIDEO_UNREACHABLE_LINE)
        };
        let opened = tokio::time::timeout(secs(CONNECT_SECS), (self.opener)(self.host.clone(), self.port)).await;
        let mut stream = match opened {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(unreachable(&e)),
            Err(_) => return Err(PikaError::new(VIDEO_UNREACHABLE_LINE)),
        };
        let request = format!("GET /stream HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", self.host);
        if let Err(e) = async {
            stream.write_all(request.as_bytes()).await?;
            stream.flush().await
        }
        .await
        {
            return Err(unreachable(&e));
        }
        let mut reader = BufReader::new(stream);
        let head = match tokio::time::timeout(secs(VIDEO_HEAD_SECS), read_until(&mut reader, b"\r\n\r\n", READ_LIMIT)).await
        {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => return Err(unreachable(&e)),
            Err(_) => return Err(PikaError::new(VIDEO_BUSY_LINE)),
        };
        // latin-1: one char per byte
        let head: String = head.iter().map(|b| *b as char).collect();
        let lines: Vec<&str> = head.split("\r\n").collect();
        // str.split(): Python's white-space set, which for latin-1 also holds \x1c-\x1f, \x85 and \xa0
        let status: Vec<&str> = lines[0].split(py_space).filter(|w| !w.is_empty()).collect();
        if status.len() < 2 || status[1] != "200" {
            return Err(PikaError::new(VIDEO_BUSY_LINE));
        }
        let chunked = lines.iter().any(|ln| {
            let l = ln.to_lowercase();
            l.starts_with("transfer-encoding:") && l.contains("chunked")
        });
        let latest = self.latest.clone();
        let fresh = self.fresh.clone();
        self.puller = Some(tokio::spawn(async move {
            let mut body = Body::new(reader, chunked);
            let mut buf: Vec<u8> = Vec::new();
            while let Ok(Some(data)) = body.next().await {
                buf.extend_from_slice(&data);
                let (mut jpegs, rest) = split_jpegs(&buf);
                buf = rest;
                if buf.len() > MAX_VIDEO_BUFFER {
                    buf.clear();
                }
                if let Some(last) = jpegs.pop() {
                    *latest.lock() = Some(last);
                    fresh.set();
                }
            }
        }));
        Ok(())
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        if let Some(p) = self.puller.take() {
            p.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_json_dumps_matches_python() {
        let v = json!({"ok": false, "line": "can't — \"x\"\n", "n": 1, "f": 1.5, "a": [null, true]});
        assert_eq!(
            py_json_dumps(&v),
            r#"{"ok": false, "line": "can't \u2014 \"x\"\n", "n": 1, "f": 1.5, "a": [null, true]}"#
        );
        assert_eq!(py_float_repr(1e16), "1e+16");
        assert_eq!(py_float_repr(1.5e-5), "1.5e-05");
        assert_eq!(py_float_repr(2.0), "2.0");
        assert_eq!(py_json_dumps(&json!("😀")), r#""\ud83d\ude00""#);
    }

    #[test]
    fn ensure_ascii_escapes_del_too() {
        assert_eq!(py_json_dumps(&json!("a\u{7f}~")), "\"a\\u007f~\"");
    }

    #[tokio::test]
    async fn a_huge_chunk_size_ends_the_body_without_allocating_it() {
        let raw = b"ffffffffffff\r\nonly a few bytes".to_vec();
        let mut body = Body::new(&raw[..], true);
        let err = body.next().await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn a_header_past_the_limit_is_a_limit_overrun() {
        let raw = vec![b'x'; READ_LIMIT + 10];
        let mut r = &raw[..];
        let err = read_until(&mut r, b"\r\n\r\n", READ_LIMIT).await.unwrap_err();
        assert_eq!(py_error_name(&err), "LimitOverrunError");
        assert_eq!(py_error_name(&not_connected()), "ConnectionError");
    }

    #[test]
    fn the_status_line_splits_on_pythons_white_space() {
        let line: String = b"HTTP/1.1\x1f200".iter().map(|b| *b as char).collect();
        let words: Vec<&str> = line.split(py_space).filter(|w| !w.is_empty()).collect();
        assert_eq!(words, vec!["HTTP/1.1", "200"]);
    }

    #[test]
    fn hex_sizes_parse_as_python_int_base_16() {
        assert_eq!(parse_hex(b"1f").unwrap(), 31);
        assert_eq!(parse_hex(b" 0x1F ").unwrap(), 31);
        assert_eq!(parse_hex(b"1_0").unwrap(), 16);
        assert!(parse_hex(b"zz").is_err());
        assert!(parse_hex(b"-5").is_err());
    }

    #[test]
    fn stats_line_formats_as_python_percent() {
        assert_eq!(stats_line("webrtc", 60.0, 41.4, 88.0, 11.2), "pika: video stats webrtc n=60 p50=41 p95=88 fps=11.2");
        assert_eq!(stats_line("tunnel", 9.9, 0.5, 1.5, 0.05), "pika: video stats tunnel n=9 p50=0 p95=2 fps=0.1");
    }

    #[test]
    fn the_stun_check_keeps_pythons_dollar() {
        assert!(STUN_RE.is_match("stun:stun.cloudflare.com:3478"));
        assert!(STUN_RE.is_match("stun:host\n"));
        assert!(!STUN_RE.is_match("stun:host\n\n"));
        assert!(!STUN_RE.is_match("turn:evil.example:3478?transport=tcp"));
        assert!(HOST_RE.is_match("elegoo-car.local") && !HOST_RE.is_match("bad host!"));
    }

    #[test]
    fn the_config_reads_what_json_loads_reads() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("pika.json");
        // json.loads takes NaN and Infinity; each is a float, so that key falls back
        std::fs::write(&cfg, r#"{"host": "car.lan", "speed": NaN, "turn_speed": -Infinity, "port": Infinity, "video_port": 8081}"#)
            .unwrap();
        assert_eq!(
            load(Some(&cfg)),
            Some(PikaConfig { host: "car.lan".into(), video_port: 8081, ..Default::default() })
        );
        // a lone surrogate is a str that fails the host check; a real pair or a text "NaN" is kept as is
        std::fs::write(&cfg, r#"{"host": "\ud800x", "stun": "", "rtc": false, "speed": 7}"#).unwrap();
        assert_eq!(
            load(Some(&cfg)),
            Some(PikaConfig { stun: String::new(), rtc: false, speed: 7, ..Default::default() })
        );
        assert_eq!(py_json_text(r#"["NaN \ud83d\ude00 \\ é\"", NaN]"#), r#"["NaN \ud83d\ude00 \\ é\"", null]"#);
        std::fs::write(&cfg, "NaN").unwrap();
        assert_eq!(load(Some(&cfg)), None);
    }

    #[test]
    fn strip_matches_python_isspace() {
        assert_eq!(py_strip("\u{1c} h1 \t"), "h1");
        assert_eq!(py_strip("\u{a0}x\u{3000}"), "x");
    }
}
