//! The owner's OBSBOT Tiny 3 webcam: point the gimbal, zoom, wake and sleep, presets and AI tracking — through
//! OBSBOT Center's OSC server on this Mac. Port of `obsbot.py`.
//!
//! The owner asked on 2026-10-11 for a CLI so eve can control the camera. How OBSBOT Center talks to the camera
//! was read out of the app (v2.0.15) with REA (github.com/morluto/rea) and `strings`/`nm`: the camera itself is
//! driven by `Frameworks/libdev.dylib` (OBSBOT's device SDK, C++ `Device::ai*`/`camera*` methods over USB), and the
//! app wraps a liblo `OSCServer` around it with 76 addresses under `/OBSBOT/...`. OBSBOT documents that OSC surface
//! (obsbot.com/explore/obsbot-center/osc, `obsbot_center_osc_definition20260508.xlsx`), so eve speaks OSC rather
//! than re-implementing the USB protocol: it keeps working across firmware updates, and the app stays usable by
//! hand. The cost is that OBSBOT Center must be running with OSC on; `eve-bridge obsbot setup` turns OSC on in its
//! `global.ini`.
//!
//! The wire, measured against a Tiny 3 on 2026-10-11:
//!
//! * UDP to 127.0.0.1:16284 (`OSCReceivePort`). Replies come back to the sender's address and port, from 16284.
//!   OBSBOT Center binds `*:16284`, so anything on the LAN can send it commands too; that is the app's choice.
//! * Every command's first int is the device index (0 = the first camera). It is optional in OBSBOT's sheet; eve
//!   always sends it. A query (`Get...`) takes the index **alone**: the sheet's reserved second int makes
//!   `GetDeviceInfo` go unanswered.
//! * `/General/Connected` is answered with `ConnectedResp 1` and then a push of DeviceInfo, ZoomInfo, the gimbal
//!   position, the presets and the AI lock state. That is how eve asks "is the app there" (~5 ms).
//! * `GetGimbalPosInfoResp` carries **two** ints, `[pan, pitch]` in degrees, not the three (roll, pitch, yaw) the
//!   sheet says. Measured by commanding (40, 0), (0, 25) and reading them back. Positive pan turns the view left
//!   (checked on the picture: the scene's left edge came into frame); positive pitch tilts it **down** (holding
//!   "up" reads negative, and asleep the lens parks at +79).
//! * `ZoomInfo` is `[zoom 0-100, fov]`; the Tiny 3 answers fov -1, so the field of view is set but never read.
//! * `DeviceInfo` is `[connected, name] x 4, selected, run state (0 sleep, 1 awake), type]`; a Tiny 3 is type 18.
//! * `AiTrackingInfo` is the target *lock*, not whether tracking is on: it stayed 0 after SetAiMode with no one in
//!   the frame. So a tracking change is sent and not claimed as read back.
//! * Commands whose value the sheet marks "x" (`ResetGimbal`, like the queries) act only when sent the device
//!   index alone; with the extra 0 the reset was ignored. Even alone it was ignored one time in three, so "center"
//!   is a move to (0, 0).
//! * `SetGimMotorDegree` (ints) is the move. Its float twin `SetGimMotorDegreeEx` moved pan but left pitch where it
//!   was. The speed argument decides whether a move lands: at 60 every move under ~20° on an axis was dropped, at
//!   90 small moves overshot, at 40 two of six missed; at 30 all landed within 2°, at about 25°/s (a 180° pan in
//!   7.2 s). So eve sends 30, waits for the distance, and resends a move that stopped short once at 20.
//! * `SetZoomSpeed` did nothing on the Tiny 3 (any argument shape); `SetZoom` works, ~2 s end to end.
//! * `WakeSleep 1` sent while the camera is still parking from a sleep is ignored; from rest it wakes in ~0.6 s.
//!   Asleep, the gimbal reads pitch 79 (parked lens-down).
//!
//! The moves, turns, zoom, status and waking go over USB (`eve-obsbot-usb`), always; the commands that move the
//! camera, `move` and `zoom`, are read back until the camera arrives and the reply says where it ended up. The Tiny
//! 3's AI modes are its own numbering (single, group, voice, desk, hand, whiteboard), which differs from the Tiny
//! 2's; `mode` refuses a camera that is not a Tiny 3 rather than guess.

pub mod py;

use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::time::Duration;

use clap::{Arg, ArgAction, ArgMatches, Command};
use serde::Serialize;
use serde_json::{json, Map, Value};

pub use eve_obsbot_usb::{
    Clock, Direction, Reply, SystemClock, UsbCamera, UsbControl, NO_CAMERA_LINE, TILT_LIMIT,
    TURN_SPEED,
};

use py::{py_str, truthy};

pub const HOST: &str = "127.0.0.1";
pub const PORT: u16 = 16284;
pub const APP: &str = "OBSBOT_Center";
pub const APP_PATH: &str = "/Applications/OBSBOT_Center.app";
pub const INI_PATH: &str = "~/Library/Application Support/OBSBOT_Center/global.ini";
/// zoom: 0 to 100% takes ~2 s
pub const ARRIVE_WAIT: f64 = 4.0;

pub const G: &str = "/OBSBOT/WebCam/General/";
pub const T: &str = "/OBSBOT/WebCam/Tiny/";

pub const PAN_RANGE: (f64, f64) = (-129.0, 129.0);
/// Above the parked tilt: asleep stays telling.
pub const PITCH_RANGE: (f64, f64) = (-TILT_LIMIT, TILT_LIMIT);
pub const DIRECTIONS: [(&str, &str); 4] = [
    ("up", "SetGimbalUp"),
    ("down", "SetGimbalDown"),
    ("left", "SetGimbalLeft"),
    ("right", "SetGimbalRight"),
];
pub const FOV: [(i64, i64); 3] = [(86, 0), (78, 1), (65, 2)];
/// Tiny 3 / Tiny 3 Lite numbering for SetAiMode (OBSBOT's sheet, 2026-05-08)
pub const AI_MODES: [(&str, i64); 6] = [
    ("single", 0),
    ("group", 1),
    ("voice", 2),
    ("desk", 3),
    ("hand", 4),
    ("whiteboard", 5),
];
pub const FRAMING: [(&str, i64); 3] = [("headroom", 0), ("standard", 1), ("motion", 2)];
pub const TRACK_SPEED: [(&str, i64); 3] = [("slow", 0), ("standard", 1), ("fast", 2)];

pub const OFF_REASON: &str =
    "OBSBOT Center is running but isn't answering on OSC. Run: eve-bridge obsbot setup";
pub const APP_NEEDED_LINE: &str = "This needs OBSBOT Center running (tracking, AI modes, presets and sleep are OBSBOT's own; eve doesn't start it).";

/// A result as Python's dicts: `ok`, a plain `line`, and what the command read back, in insertion order.
pub type Res = Map<String, Value>;

fn lookup<V: Copy>(table: &[(&str, V)], key: &str) -> Option<V> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn names<V>(table: &[(&str, V)]) -> String {
    table.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
}

/// The names of a table, in order (Python's `list(AI_MODES)`).
pub fn keys<V>(table: &[(&'static str, V)]) -> Vec<&'static str> {
    table.iter().map(|(k, _)| *k).collect()
}

fn obj(v: Value) -> Res {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn fail(line: impl Into<String>) -> Res {
    obj(json!({"ok": false, "line": line.into()}))
}

fn why_or(why: &str, fallback: &str) -> String {
    if why.is_empty() {
        fallback.to_string()
    } else {
        why.to_string()
    }
}

// --- OSC 1.0, the subset OBSBOT uses: int32, float32, string ------------------------------------------------------

/// An OSC argument or a decoded value: Python's int, float or str.
#[derive(Debug, Clone, PartialEq)]
pub enum OscValue {
    Int(i64),
    Float(f64),
    Str(String),
}

impl OscValue {
    pub fn to_json(&self) -> Value {
        match self {
            OscValue::Int(n) => Value::from(*n),
            OscValue::Float(f) => Value::from(*f),
            OscValue::Str(s) => Value::String(s.clone()),
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            OscValue::Int(n) => *n != 0,
            OscValue::Float(f) => *f != 0.0,
            OscValue::Str(s) => !s.is_empty(),
        }
    }
}

impl From<i64> for OscValue {
    fn from(n: i64) -> Self {
        OscValue::Int(n)
    }
}

impl From<i32> for OscValue {
    fn from(n: i32) -> Self {
        OscValue::Int(i64::from(n))
    }
}

impl From<bool> for OscValue {
    fn from(b: bool) -> Self {
        OscValue::Int(i64::from(b))
    }
}

impl From<f64> for OscValue {
    fn from(f: f64) -> Self {
        OscValue::Float(f)
    }
}

impl From<&str> for OscValue {
    fn from(s: &str) -> Self {
        OscValue::Str(s.to_string())
    }
}

impl From<String> for OscValue {
    fn from(s: String) -> Self {
        OscValue::Str(s)
    }
}

/// Python's `repr` of a decoded value list, as the raw command prints it: `[1, 'Desk', 0.5]`.
/// Each value is repr'd from itself, not through JSON, so a float NaN or infinity prints `nan`/`inf` as Python does.
pub fn values_repr(values: &[OscValue]) -> String {
    let items: Vec<String> = values
        .iter()
        .map(|v| match v {
            OscValue::Int(n) => n.to_string(),
            OscValue::Float(f) => py::float_repr(*f),
            OscValue::Str(s) => py::str_repr(s),
        })
        .collect();
    format!("[{}]", items.join(", "))
}

/// What Python's struct and bytes.index raise for a value or packet OSC can't carry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OscError {
    #[error("'i' format requires -2147483648 <= number <= 2147483647")]
    IntRange,
    #[error("float too large to pack with f format")]
    FloatRange,
    #[error("subsection not found")]
    NoTerminator,
    #[error("unpack requires a buffer of 4 bytes")]
    Short,
}

fn pad(mut b: Vec<u8>) -> Vec<u8> {
    let n = 4 - b.len() % 4;
    b.extend(std::iter::repeat_n(0u8, n));
    b
}

pub fn encode(address: &str, args: &[OscValue]) -> Result<Vec<u8>, OscError> {
    let mut tags = String::from(",");
    let mut body = Vec::new();
    for a in args {
        match a {
            OscValue::Int(n) => {
                let n = i32::try_from(*n).map_err(|_| OscError::IntRange)?;
                tags.push('i');
                body.extend_from_slice(&n.to_be_bytes());
            }
            OscValue::Float(f) => {
                let g = *f as f32;
                if f.is_finite() && g.is_infinite() {
                    return Err(OscError::FloatRange);
                }
                tags.push('f');
                body.extend_from_slice(&g.to_be_bytes());
            }
            OscValue::Str(s) => {
                tags.push('s');
                body.extend(pad(s.as_bytes().to_vec()));
            }
        }
    }
    let mut out = pad(address.as_bytes().to_vec());
    out.extend(pad(tags.into_bytes()));
    out.extend(body);
    Ok(out)
}

/// Python's `round(x, 2)`: correctly rounded on the exact binary value.
fn round2(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.2}").parse().unwrap_or(x)
}

pub fn decode(data: &[u8]) -> Result<(String, Vec<OscValue>), OscError> {
    let string = |i: usize| -> Result<(String, usize), OscError> {
        let j = data
            .get(i..)
            .and_then(|s| s.iter().position(|&b| b == 0))
            .map(|p| i + p)
            .ok_or(OscError::NoTerminator)?;
        Ok((
            String::from_utf8_lossy(&data[i..j]).into_owned(),
            (j / 4 + 1) * 4,
        ))
    };
    let word = |i: usize| -> Result<[u8; 4], OscError> {
        data.get(i..i + 4)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .ok_or(OscError::Short)
    };
    let (address, mut i) = string(0)?;
    let (tags, next) = if i < data.len() {
        string(i)?
    } else {
        (",".to_string(), i)
    };
    i = next;
    let mut values = Vec::new();
    for t in tags.chars().skip(1) {
        match t {
            'i' => {
                values.push(OscValue::Int(i64::from(i32::from_be_bytes(word(i)?))));
                i += 4;
            }
            'f' => {
                values.push(OscValue::Float(round2(f64::from(f32::from_be_bytes(
                    word(i)?,
                )))));
                i += 4;
            }
            's' => {
                let (s, n) = string(i)?;
                values.push(OscValue::Str(s));
                i = n;
            }
            _ => break, // a type OBSBOT never sends: stop rather than misread
        }
    }
    Ok((address, values))
}

// --- the camera ----------------------------------------------------------------------------------------------------

/// `pgrep -x OBSBOT_Main`: OBSBOT Center's process is running.
pub fn app_running() -> bool {
    Process::new("pgrep")
        .args(["-x", "OBSBOT_Main"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A preset slot as OBSBOT Center lists it: 1-based, saved or not, and its name ("" when not saved).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Preset {
    pub n: i64,
    pub set: bool,
    pub name: Value,
}

impl Preset {
    /// `p["name"] or f"preset {n}"`.
    pub fn label(&self) -> String {
        if truthy(Some(&self.name)) {
            py_str(&self.name)
        } else {
            format!("preset {}", self.n)
        }
    }
}

type AppRunning = Box<dyn Fn() -> bool + Send + Sync>;

/// `Obsbot(host=, port=, device=, app_running=, usb=)`, plus the clock the Python tests patch.
pub struct Options {
    pub host: String,
    pub port: u16,
    pub device: i64,
    pub app_running: AppRunning,
    pub usb: Option<Box<dyn UsbControl>>,
    pub clock: Box<dyn Clock>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            host: HOST.to_string(),
            port: PORT,
            device: 0,
            app_running: Box::new(app_running),
            usb: None,
            clock: Box::new(SystemClock),
        }
    }
}

impl Options {
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    pub fn device(mut self, device: i64) -> Self {
        self.device = device;
        self
    }

    pub fn app_running(mut self, f: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.app_running = Box::new(f);
        self
    }

    pub fn usb(mut self, usb: impl UsbControl + 'static) -> Self {
        self.usb = Some(Box::new(usb));
        self
    }

    pub fn clock(mut self, clock: impl Clock + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    pub fn open(self) -> io::Result<Obsbot> {
        Obsbot::with(self)
    }
}

/// The camera: moves, turns, zoom, status and waking over USB (obsbot_usb), always; tracking, the AI mode,
/// presets, sleep and the picture settings over OBSBOT Center's OSC, only while the app is running. eve never
/// starts the app (owner, 2026-10-11: direct, with the app for the AI features).
pub struct Obsbot {
    host: String,
    port: u16,
    pub device: i64,
    app_running: AppRunning,
    pub usb: Box<dyn UsbControl>,
    sock: Option<UdpSocket>,
    clock: Box<dyn Clock>,
    /// the camera answered over USB
    pub ready: bool,
    /// OBSBOT Center answered over OSC
    pub osc_ready: bool,
}

impl std::fmt::Debug for Obsbot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Obsbot")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("device", &self.device)
            .field("ready", &self.ready)
            .field("osc_ready", &self.osc_ready)
            .finish()
    }
}

fn duration(secs: f64) -> Duration {
    Duration::from_secs_f64(secs.max(0.0)).max(Duration::from_micros(1))
}

impl Obsbot {
    /// `Obsbot()`: OBSBOT Center on 127.0.0.1:16284, device 0, the real USB camera.
    pub fn new() -> io::Result<Self> {
        Self::with(Options::default())
    }

    pub fn options() -> Options {
        Options::default()
    }

    pub fn with(o: Options) -> io::Result<Self> {
        let bind = if o.host == "127.0.0.1" || o.host == "localhost" {
            "127.0.0.1"
        } else {
            "0.0.0.0"
        };
        let sock = UdpSocket::bind((bind, 0))?;
        Ok(Self {
            host: o.host,
            port: o.port,
            device: o.device,
            app_running: o.app_running,
            usb: o.usb.unwrap_or_else(|| Box::new(UsbCamera::new())),
            sock: Some(sock),
            clock: o.clock,
            ready: false,
            osc_ready: false,
        })
    }

    /// Tell OBSBOT Center this client is gone (its sheet's optional `Disconnected`), then close. Every source port
    /// that ever sent is a client it keeps and refreshes on each state change.
    pub fn close(&mut self) {
        if self.osc_ready {
            let _ = self.send(&format!("{G}Disconnected"), &[OscValue::Int(0)]);
        }
        self.sock = None;
    }

    fn sock(&self) -> io::Result<&UdpSocket> {
        self.sock
            .as_ref()
            .ok_or_else(|| io::Error::from_raw_os_error(9))
    }

    fn target(&self) -> io::Result<SocketAddr> {
        (self.host.as_str(), self.port)
            .to_socket_addrs()?
            .find(SocketAddr::is_ipv4)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "no IPv4 address for the OSC host")
            })
    }

    // the OSC wire (OBSBOT Center)
    pub fn send(&self, name: &str, args: &[OscValue]) -> io::Result<()> {
        let address = if name.starts_with('/') {
            name.to_string()
        } else {
            format!("{G}{name}")
        };
        let data =
            encode(&address, args).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let to = self.target()?;
        self.sock()?.send_to(&data, to)?;
        Ok(())
    }

    fn recv_until(&self, want: &str, timeout: f64) -> Option<Vec<OscValue>> {
        let sock = self.sock().ok()?;
        let end = self.clock.monotonic() + timeout;
        let mut buf = vec![0u8; 65536];
        loop {
            let left = end - self.clock.monotonic();
            if left <= 0.0 {
                return None;
            }
            sock.set_read_timeout(Some(duration(left))).ok()?;
            let n = match sock.recv_from(&mut buf) {
                Ok((n, _)) => n,
                Err(_) => return None,
            };
            let Ok((address, values)) = decode(&buf[..n]) else {
                continue;
            };
            if address.ends_with(want) {
                return Some(values);
            }
        }
    }

    fn recv_raw(&self, timeout: f64) -> Option<(String, Vec<OscValue>)> {
        let sock = self.sock().ok()?;
        sock.set_read_timeout(Some(duration(timeout))).ok()?;
        let mut buf = vec![0u8; 65536];
        let (n, _) = sock.recv_from(&mut buf).ok()?;
        decode(&buf[..n]).ok()
    }

    fn drain(&self) {
        let Ok(sock) = self.sock() else {
            return;
        };
        if sock.set_nonblocking(true).is_err() {
            return;
        }
        let mut buf = vec![0u8; 65536];
        while sock.recv_from(&mut buf).is_ok() {}
        let _ = sock.set_nonblocking(false);
    }

    pub fn ask(&self, name: &str, reply: &str, timeout: f64) -> Option<Vec<OscValue>> {
        self.drain();
        // one int: GetDeviceInfo with a second (the sheet's "x") never answers
        self.send(name, &[OscValue::Int(self.device)]).ok()?;
        self.recv_until(reply, timeout)
    }

    /// None when the camera answers over USB; otherwise the reason it can't (no camera, no clang).
    pub fn ensure(&mut self) -> Option<String> {
        if self.ready {
            return None;
        }
        if self.usb.read().is_none() {
            return Some(why_or(self.usb.why(), NO_CAMERA_LINE));
        }
        self.ready = true;
        None
    }

    /// None when OBSBOT Center answers on OSC; otherwise the line for a feature that needs it. Never starts it.
    pub fn osc(&mut self) -> Option<String> {
        if self.osc_ready {
            return None;
        }
        self.drain();
        let _ = self.send(&format!("{G}Connected"), &[OscValue::Int(0)]);
        if self.recv_until("ConnectedResp", 0.5).is_some() {
            self.osc_ready = true;
            return None;
        }
        if (self.app_running)() {
            return Some(OFF_REASON.to_string());
        }
        Some(APP_NEEDED_LINE.to_string())
    }

    // reads
    /// OBSBOT Center's device list (OSC); None when the app isn't there.
    pub fn info(&mut self) -> Option<Res> {
        if self.osc().is_some() {
            return None;
        }
        let v = self.ask(&format!("{G}GetDeviceInfo"), "DeviceInfo", 1.0)?;
        if v.len() < 11 {
            return None;
        }
        let devices: Vec<Value> = (0..4)
            .filter(|&n| v[2 * n].truthy() || v[2 * n + 1].truthy())
            .map(|n| json!({"index": n, "connected": v[2 * n].truthy(), "name": v[2 * n + 1].to_json()}))
            .collect();
        Some(obj(
            json!({"devices": devices, "selected": v[8].to_json(), "awake": v[9].truthy(),
                        "type": v[10].to_json()}),
        ))
    }

    /// (pan, pitch) in degrees: pan + left, pitch + down (the camera's tilt + up, turned over).
    pub fn position(&mut self) -> Option<(i64, i64)> {
        let st = self.usb.read()?;
        pan_pitch(&st)
    }

    pub fn zoom_level(&mut self) -> Option<i64> {
        let st = self.usb.read()?;
        match st.get("zoom") {
            None | Some(Value::Null) => None,
            Some(z) => py::int_value(z),
        }
    }

    pub fn presets(&mut self) -> Vec<Preset> {
        if self.osc().is_some() {
            return Vec::new();
        }
        let v = self
            .ask(
                &format!("{T}GetPresetPositionInfo"),
                "PresetPositionInfo",
                1.0,
            )
            .unwrap_or_default();
        (0..v.len() / 2)
            .map(|n| {
                let set = v[2 * n].truthy();
                Preset {
                    n: n as i64 + 1,
                    set,
                    name: if set {
                        v[2 * n + 1].to_json()
                    } else {
                        Value::String(String::new())
                    },
                }
            })
            .collect()
    }

    pub fn position_reply(&mut self) -> Res {
        match self.position() {
            None => fail(why_or(
                self.usb.why(),
                "Couldn't read where the camera points.",
            )),
            Some((pan, pitch)) => obj(json!({"ok": true, "pan": pan, "pitch": pitch, "line": ""})),
        }
    }

    pub fn zoom_reply(&mut self) -> Res {
        match self.zoom_level() {
            Some(z) => obj(json!({"ok": true, "zoom": z, "line": ""})),
            None => fail("Couldn't read the zoom."),
        }
    }

    pub fn locked(&mut self) -> Option<bool> {
        if self.osc().is_some() {
            return None;
        }
        let v = self.ask(&format!("{T}GetAiTrackingInfo"), "AiTrackingInfo", 1.0)?;
        v.first().map(OscValue::truthy)
    }

    // the doors
    pub fn status(&mut self) -> Res {
        let Some(st) = self.usb.read() else {
            return fail(why_or(self.usb.why(), NO_CAMERA_LINE));
        };
        let raw = st
            .get("name")
            .filter(|v| truthy(Some(v)))
            .map(py_str)
            .unwrap_or_else(|| "OBSBOT camera".into());
        let name = py::strip(raw.strip_prefix("OBSBOT ").unwrap_or(&raw)).to_string();
        let name = if name.is_empty() {
            "OBSBOT camera".to_string()
        } else {
            name
        };
        let awake = !truthy(st.get("asleep"));
        let Some((pan, pitch)) = pan_pitch(&st) else {
            return fail("Couldn't read where the camera points.");
        };
        let zoom = st.get("zoom").cloned().unwrap_or(Value::Null);
        let presets = self.presets();
        let lock = self.locked();
        let mut parts = vec![format!("{name} {}", if awake { "awake" } else { "asleep" })];
        if awake {
            parts.push(format!("pan {pan}°, pitch {pitch}°"));
        }
        if !zoom.is_null() {
            parts.push(format!("zoom {}%", py_str(&zoom)));
        }
        if lock == Some(true) {
            parts.push("tracking a locked target".into());
        }
        let saved: Vec<String> = presets
            .iter()
            .filter(|p| p.set)
            .map(Preset::label)
            .collect();
        if !saved.is_empty() {
            parts.push(format!("presets: {}", saved.join(", ")));
        }
        obj(json!({
            "ok": true, "line": format!("{}.", parts.join("; ")), "camera": name, "awake": awake,
            "pan": if awake { Some(pan) } else { None }, "pitch": if awake { Some(pitch) } else { None },
            "zoom": zoom, "locked": lock, "presets": presets, "app": self.osc_ready,
        }))
    }

    fn short_name(&mut self) -> String {
        match self.usb.read().and_then(|st| st.get("name").cloned()) {
            None => "The camera".to_string(),
            Some(v) => {
                let s = py_str(&v);
                s.strip_prefix("OBSBOT ").unwrap_or(&s).to_string()
            }
        }
    }

    pub fn wake(&mut self, awake: bool) -> Res {
        let name = self.short_name();
        if awake {
            if self.usb.wake() {
                return obj(json!({"ok": true, "line": format!("{name} awake.")}));
            }
            return fail(why_or(
                self.usb.why(),
                "The camera didn't wake. Is eve allowed to use the camera?",
            ));
        }
        if let Some(why) = self.osc() {
            return fail(why); // sleep is OBSBOT's own command: no UVC control for it
        }
        for _ in 0..2 {
            // a sleep sent mid-unpark is ignored: send it again once
            if let Err(e) = self.send(
                &format!("{G}WakeSleep"),
                &[OscValue::Int(self.device), OscValue::Int(0)],
            ) {
                return fail(e.to_string());
            }
            let end = self.clock.monotonic() + 5.0;
            while self.clock.monotonic() < end {
                self.clock.sleep(0.5);
                if self.usb.read().is_some_and(|st| truthy(st.get("asleep"))) {
                    return obj(json!({"ok": true, "line": format!("{name} asleep.")}));
                }
                if !(self.app_running)() {
                    self.osc_ready = false; // its sleep handler can crash the app (docs/obsbot.md)
                    return obj(
                        json!({"ok": false, "line": "OBSBOT Center crashed putting the camera to sleep.",
                                      "crashed": true}),
                    );
                }
            }
        }
        fail("Sent sleep; the camera didn't confirm it.")
    }

    /// Python's `move(pan, pitch)`: point the gimbal (pan + left, pitch + down), keeping an axis not given.
    pub fn move_to(&mut self, pan: Option<f64>, pitch: Option<f64>) -> Res {
        if !self.usb.wake() {
            return fail(why_or(
                self.usb.why(),
                "The camera is asleep and didn't wake.",
            ));
        }
        let Some(now) = self.position() else {
            return fail("Couldn't read where the camera points.");
        };
        let clamp = |range: (f64, f64), v: f64| -> i64 {
            let v = if v < range.1 { v } else { range.1 }; // Python's min(hi, v), then max(lo, ...)
            let v = if v > range.0 { v } else { range.0 };
            py::round_even(v).unwrap_or(range.1 as i64)
        };
        let pan = clamp(PAN_RANGE, pan.unwrap_or(now.0 as f64));
        let pitch = clamp(PITCH_RANGE, pitch.unwrap_or(now.1 as f64));
        if !self.usb.point(pan as f64, (-pitch) as f64) {
            return fail("The camera refused the move.");
        }
        let far = (pan - now.0).abs().max((pitch - now.1).abs());
        self.settle(pan, pitch, (1.0 + far as f64 / 15.0).min(12.0))
    }

    fn settle(&mut self, pan: i64, pitch: i64, wait: f64) -> Res {
        let start = self.clock.monotonic();
        let (mut last, mut pos, mut still) = (None, None, 0);
        while self.clock.monotonic() < start + wait + 1.5 {
            self.clock.sleep(0.25);
            pos = self.position();
            still = if pos.is_some() && pos == last {
                still + 1
            } else {
                0
            };
            last = pos;
            // still for three reads (~0.75 s): arrived, or stopped short (an end stop). Within a few degrees is
            // not enough: the last degree creeps in, and the reply should say where the camera ended up
            if still >= 3 || (still >= 1 && self.clock.monotonic() > start + wait) {
                break;
            }
        }
        let Some((p, q)) = pos else {
            return fail("Sent the move; couldn't read the camera's position back.");
        };
        let arrived = (p - pan).abs() <= 3 && (q - pitch).abs() <= 3;
        let line = if arrived {
            format!("Pan {p}°, pitch {q}°.")
        } else {
            format!("Asked for pan {pan}°, pitch {pitch}°; it stopped at pan {p}°, pitch {q}°.")
        };
        obj(json!({"ok": arrived, "line": line, "pan": p, "pitch": q}))
    }

    pub fn nudge(&mut self, direction: &str, speed: i64, seconds: f64) -> Res {
        let dir = match direction.parse::<Direction>() {
            Ok(d) if lookup(&DIRECTIONS, direction).is_some() => d,
            _ => return fail(format!("Direction is one of: {}.", names(&DIRECTIONS))),
        };
        if !self.usb.wake() {
            return fail(why_or(
                self.usb.why(),
                "The camera is asleep and didn't wake.",
            ));
        }
        self.usb.turn(dir, speed.clamp(1, 127));
        let hold = if seconds < 10.0 { seconds } else { 10.0 };
        self.clock.sleep(if hold > 0.05 { hold } else { 0.05 });
        self.usb.turn(Direction::Stop, TURN_SPEED);
        self.clock.sleep(0.4);
        let pos = self.position();
        let tail = pos
            .map(|(p, q)| format!(" Pan {p}°, pitch {q}°."))
            .unwrap_or_default();
        obj(
            json!({"ok": true, "line": format!("Moved {direction}.{tail}"), "pan": pos.map(|p| p.0),
                   "pitch": pos.map(|p| p.1)}),
        )
    }

    pub fn stop(&mut self) -> Res {
        self.usb.turn(Direction::Stop, TURN_SPEED);
        obj(json!({"ok": true, "line": "Stopped."}))
    }

    /// Back to straight ahead: a move to (0, 0).
    pub fn reset(&mut self) -> Res {
        let mut res = self.move_to(Some(0.0), Some(0.0));
        if truthy(res.get("ok")) {
            res.insert("line".into(), Value::String("Centered.".into()));
        }
        res
    }

    pub fn zoom(&mut self, level: i64) -> Res {
        let level = level.clamp(0, 100);
        if !self.usb.wake() {
            return fail(why_or(
                self.usb.why(),
                "The camera is asleep and didn't wake.",
            ));
        }
        if !self.usb.zoom(level) {
            return fail("The camera refused the zoom.");
        }
        let end = self.clock.monotonic() + ARRIVE_WAIT;
        let mut got = None;
        while self.clock.monotonic() < end {
            self.clock.sleep(0.3);
            got = self.zoom_level();
            if let Some(g) = got {
                if (g - level).abs() <= 1 {
                    return obj(json!({"ok": true, "line": format!("Zoom {g}%."), "zoom": g}));
                }
            }
        }
        let reads = got.map(|g| g.to_string()).unwrap_or_else(|| "None".into());
        obj(
            json!({"ok": false, "line": format!("Asked for zoom {level}%; it reads {reads}%."), "zoom": got}),
        )
    }

    // OBSBOT's own features: OBSBOT Center, while it runs
    fn app(&mut self) -> Option<Res> {
        self.osc().map(fail)
    }

    /// A fire-and-forget command; a send that fails is the result (Python raises it).
    fn command(&self, address: String, value: i64, line: String) -> Res {
        match self.send(
            &address,
            &[OscValue::Int(self.device), OscValue::Int(value)],
        ) {
            Ok(()) => obj(json!({"ok": true, "line": line})),
            Err(e) => fail(e.to_string()),
        }
    }

    pub fn fov(&mut self, degrees: i64) -> Res {
        let Some(v) = FOV.iter().find(|(d, _)| *d == degrees).map(|(_, v)| *v) else {
            return fail("Field of view is 86, 78 or 65.");
        };
        if let Some(refused) = self.app() {
            return refused;
        }
        self.command(
            format!("{G}SetView"),
            v,
            format!("Field of view {degrees}°."),
        )
    }

    pub fn preset(&mut self, n: i64) -> Res {
        if !(1..=3).contains(&n) {
            return fail("Preset is 1, 2 or 3.");
        }
        if let Some(refused) = self.app() {
            return refused;
        }
        let saved = self.presets();
        let slot = saved.iter().find(|p| p.n == n);
        if !saved.is_empty() && !slot.is_some_and(|p| p.set) {
            return fail(format!(
                "Preset {n} isn't saved. Save it in OBSBOT Center first."
            ));
        }
        let name = slot
            .filter(|p| truthy(Some(&p.name)))
            .map(|p| py_str(&p.name));
        let name = name.unwrap_or_else(|| format!("preset {n}"));
        self.command(
            format!("{T}TriggerPreset"),
            n - 1,
            format!("Going to {name}."),
        )
    }

    pub fn track(&mut self, on: bool) -> Res {
        if let Some(refused) = self.app() {
            return refused;
        }
        let line = if on { "Tracking on." } else { "Tracking off." };
        self.command(format!("{T}ToggleAILock"), i64::from(on), line.into())
    }

    fn tiny3(&mut self) -> Option<String> {
        let name = self
            .usb
            .read()
            .and_then(|st| st.get("name").filter(|v| truthy(Some(v))).map(py_str))
            .unwrap_or_default();
        if !name.is_empty() && !name.to_lowercase().contains("tiny 3") {
            return Some(format!(
                "These settings use the Tiny 3's numbering; this camera is a {name}."
            ));
        }
        None
    }

    pub fn mode(&mut self, name: &str) -> Res {
        let Some(v) = lookup(&AI_MODES, name) else {
            return fail(format!("AI mode is one of: {}.", names(&AI_MODES)));
        };
        if let Some(why) = self.tiny3() {
            return fail(why);
        }
        if let Some(refused) = self.app() {
            return refused;
        }
        self.command(format!("{T}SetAiMode"), v, format!("AI mode {name}."))
    }

    pub fn framing(&mut self, name: &str) -> Res {
        let Some(v) = lookup(&FRAMING, name) else {
            return fail(format!("Framing is one of: {}.", names(&FRAMING)));
        };
        if let Some(refused) = self.app() {
            return refused;
        }
        self.command(format!("{T}SetTrackingMode"), v, format!("Framing {name}."))
    }

    pub fn track_speed(&mut self, name: &str) -> Res {
        let Some(v) = lookup(&TRACK_SPEED, name) else {
            return fail(format!(
                "Tracking speed is one of: {}.",
                names(&TRACK_SPEED)
            ));
        };
        if let Some(why) = self.tiny3() {
            return fail(why);
        }
        if let Some(refused) = self.app() {
            return refused;
        }
        self.command(
            format!("{T}SetTrackingSpeed"),
            v,
            format!("Tracking speed {name}."),
        )
    }

    pub fn focus(&mut self, value: &str) -> Res {
        let manual = if value == "auto" {
            None
        } else {
            match py::parse_int(value) {
                Some(n) => Some(n),
                None => return fail("Focus is auto or 0-100."),
            }
        };
        if let Some(refused) = self.app() {
            return refused;
        }
        let Some(n) = manual else {
            return self.command(format!("{G}SetAutoFocus"), 1, "Autofocus on.".into());
        };
        let level = n.clamp(0, 100) as i64;
        let off = self.command(format!("{G}SetAutoFocus"), 0, String::new());
        if !truthy(off.get("ok")) {
            return off;
        }
        self.command(
            format!("{G}SetManualFocus"),
            level,
            format!("Manual focus {level}."),
        )
    }

    pub fn mirror(&mut self, on: bool) -> Res {
        if let Some(refused) = self.app() {
            return refused;
        }
        let line = if on { "Mirrored." } else { "Not mirrored." };
        self.command(format!("{G}SetMirror"), i64::from(on), line.into())
    }
}

/// `(round(st["pan"]), -round(st["tilt"]))` of a USB read.
fn pan_pitch(st: &Reply) -> Option<(i64, i64)> {
    let pan = py::round_value(st.get("pan")?)?;
    let tilt = py::round_value(st.get("tilt")?)?;
    Some((pan, -tilt))
}

// --- setup: the USB helper, and OSC for OBSBOT's own features -----------------------------------------------------

/// `Path.expanduser()`.
pub fn expand_user(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s == "~" || s.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(s.trim_start_matches('~').trim_start_matches('/'));
        }
    }
    p.to_path_buf()
}

/// Build the USB helper and read the camera (all eve needs to move it). Then, when OBSBOT Center is installed and
/// not running, turn its OSC on (UDP, 127.0.0.1, port 16284) in global.ini, for tracking, AI modes, presets and
/// sleep. A running app is left alone: it rewrites the file when it quits, and asking it to quit shows a dialog.
pub fn setup(ini: &Path, out: &mut dyn FnMut(&str), usb: Option<Box<dyn UsbControl>>) -> i32 {
    setup_with(ini, out, usb, Path::new(APP_PATH), &app_running)
}

/// `setup` with the app's path and the running check given (the Python tests patch both).
pub fn setup_with(
    ini: &Path,
    out: &mut dyn FnMut(&str),
    usb: Option<Box<dyn UsbControl>>,
    app_path: &Path,
    app_running: &dyn Fn() -> bool,
) -> i32 {
    let mut usb = usb.unwrap_or_else(|| Box::new(UsbCamera::new()));
    let Some(st) = usb.read() else {
        out(&why_or(usb.why(), NO_CAMERA_LINE));
        return 1;
    };
    let name = st
        .get("name")
        .map(py_str)
        .unwrap_or_else(|| "The camera".into());
    out(&format!(
        "{name} answers over USB: eve can move, zoom and wake it without OBSBOT Center."
    ));
    let ini = expand_user(ini);
    if !app_path.exists() || !ini.exists() {
        out("OBSBOT Center isn't set up, so tracking, AI modes, presets and sleep are off. That's fine for moving it.");
        return 0;
    }
    if app_running() {
        out(&format!(
            "OBSBOT Center is running: for tracking and AI modes from eve, turn on OSC in its settings (UDP, port \
             {PORT}), or quit it and run this again."
        ));
        return 0;
    }
    let port = PORT.to_string();
    let want: [(&str, &str); 4] = [
        ("OSC", "true"),
        ("OSCConnectMethod", "0"),
        ("OSCHostIp", HOST),
        ("OSCReceivePort", port.as_str()),
    ];
    let text = match std::fs::read_to_string(&ini) {
        Ok(t) => t,
        Err(e) => {
            out(&e.to_string());
            return 1;
        }
    };
    let mut lines: Vec<String> = py::splitlines(&text)
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut seen: Vec<&str> = Vec::new();
    for line in lines.iter_mut() {
        let key = line.split('=').next().unwrap_or("").to_string();
        if let Some(v) = lookup(&want, &key) {
            if line.contains('=') {
                *line = format!("{key}={v}");
                if let Some((k, _)) = want.iter().find(|(k, _)| *k == key) {
                    seen.push(k);
                }
            }
        }
    }
    let missing: Vec<&str> = want
        .iter()
        .map(|(k, _)| *k)
        .filter(|k| !seen.contains(k))
        .collect();
    if !missing.is_empty() {
        let at = match lines.iter().position(|ln| py::strip(ln) == "[SoftSetting]") {
            Some(i) => i + 1,
            None => {
                lines.push("[SoftSetting]".into());
                lines.len()
            }
        };
        let add: Vec<String> = missing
            .iter()
            .map(|k| format!("{k}={}", lookup(&want, k).unwrap_or("")))
            .collect();
        lines.splice(at..at, add);
    }
    if let Err(e) = std::fs::write(&ini, format!("{}\n", lines.join("\n"))) {
        out(&e.to_string());
        return 1;
    }
    out("OSC is on in OBSBOT Center's settings: open the app when you want tracking or AI modes from eve.");
    0
}

// --- the CLI -------------------------------------------------------------------------------------------------------

/// `_number`: an int, else a float, else the string, as the raw command sends its arguments.
pub fn number(s: &str) -> OscValue {
    if let Some(n) = py::parse_int(s) {
        return OscValue::Int(py::saturate(n));
    }
    if let Some(f) = py::parse_float(s) {
        return OscValue::Float(f);
    }
    OscValue::Str(s.to_string())
}

fn int_arg(s: &str) -> Result<i64, String> {
    py::parse_int(s)
        .map(py::saturate)
        .ok_or_else(|| format!("invalid int value: '{s}'"))
}

fn float_arg(s: &str) -> Result<f64, String> {
    py::parse_float(s).ok_or_else(|| format!("invalid float value: '{s}'"))
}

fn sub(name: &'static str, help: impl Into<String>) -> Command {
    Command::new(name).about(help.into()).infer_long_args(true)
}

fn int_pos(name: &'static str) -> Arg {
    Arg::new(name)
        .required(true)
        .allow_negative_numbers(true)
        .value_parser(int_arg)
}

fn choice(name: &'static str, choices: Vec<&'static str>) -> Arg {
    Arg::new(name).required(true).value_parser(choices)
}

fn parser() -> Command {
    Command::new("eve-bridge obsbot")
        .about("Control the OBSBOT camera: moves over USB, OBSBOT's own features through OBSBOT Center while it runs.")
        .infer_long_args(true)
        .subcommand_required(true)
        .arg(Arg::new("json").long("json").action(ArgAction::SetTrue).help("print the result as JSON"))
        .arg(
            Arg::new("device")
                .long("device")
                .allow_negative_numbers(true)
                .value_parser(int_arg)
                .default_value("0")
                .help("camera index in OBSBOT Center (default 0)"),
        )
        .subcommand(sub(
            "setup",
            "Build the USB helper, read the camera, and turn on OBSBOT Center's OSC if it's closed",
        ))
        .subcommand(sub("status", "Camera, awake or asleep, where it points, zoom, presets"))
        .subcommand(sub("wake", "Wake the camera"))
        .subcommand(sub("sleep", "Put the camera to sleep (needs OBSBOT Center running)"))
        .subcommand(
            sub("move", "Point the gimbal: pan -129..129° (+ is left), pitch -59..59° (+ is down)")
                .arg(Arg::new("pan").long("pan").allow_negative_numbers(true).value_parser(float_arg))
                .arg(Arg::new("pitch").long("pitch").allow_negative_numbers(true).value_parser(float_arg)),
        )
        .subcommand(
            sub("nudge", "Turn the gimbal for a moment: up, down, left, right")
                .arg(choice("direction", keys(&DIRECTIONS)))
                .arg(
                    Arg::new("speed")
                        .long("speed")
                        .allow_negative_numbers(true)
                        .value_parser(int_arg)
                        .help(format!("1-80 on a Tiny 3 (default {TURN_SPEED})")),
                )
                .arg(
                    Arg::new("seconds")
                        .long("seconds")
                        .allow_negative_numbers(true)
                        .value_parser(float_arg)
                        .default_value("0.5"),
                ),
        )
        .subcommand(sub("stop", "Stop a gimbal move"))
        .subcommand(sub("center", "Reset the gimbal to its home position"))
        .subcommand(sub("zoom", "Zoom 0-100%").arg(int_pos("level")))
        .subcommand(sub("fov", "Field of view: 86, 78 or 65°").arg(int_pos("degrees")))
        .subcommand(sub("preset", "Go to a preset saved in OBSBOT Center: 1, 2 or 3").arg(int_pos("n")))
        .subcommand(sub("track", "AI tracking lock on or off").arg(choice("state", vec!["on", "off"])))
        .subcommand(sub("mode", format!("AI mode: {}", names(&AI_MODES))).arg(choice("name", keys(&AI_MODES))))
        .subcommand(
            sub("framing", format!("Tracking framing: {}", names(&FRAMING))).arg(choice("name", keys(&FRAMING))),
        )
        .subcommand(
            sub("track-speed", format!("Tracking speed: {}", names(&TRACK_SPEED)))
                .arg(choice("name", keys(&TRACK_SPEED))),
        )
        .subcommand(
            sub("focus", "auto, or manual 0-100")
                .arg(Arg::new("value").required(true).allow_negative_numbers(true)),
        )
        .subcommand(sub("mirror", "Mirror the picture: on or off").arg(choice("state", vec!["on", "off"])))
        .subcommand(
            sub("raw", "Send any OBSBOT OSC address; ints, floats and strings as given")
                .arg(Arg::new("address").required(true))
                .arg(Arg::new("args").num_args(0..).allow_negative_numbers(true)),
        )
}

/// The global options are accepted after the subcommand too: "obsbot status --json".
fn reorder(argv: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = argv.to_vec();
    let mut front: Vec<String> = Vec::new();
    for flag in ["--json"] {
        while let Some(i) = argv.iter().position(|a| a == flag) {
            argv.remove(i);
            front.push(flag.to_string());
        }
    }
    if let Some(i) = argv
        .iter()
        .skip(1)
        .position(|a| a == "--device")
        .map(|i| i + 1)
    {
        if i + 1 < argv.len() {
            front.extend(argv.drain(i..i + 2));
        }
    }
    front.extend(argv);
    front
}

fn string_of(m: &ArgMatches, id: &str) -> String {
    m.get_one::<String>(id).cloned().unwrap_or_default()
}

/// `eve-bridge obsbot setup | status | wake | sleep | move [--pan P] [--pitch P] | nudge DIR | stop | center |
/// zoom N | fov 86|78|65 | preset N | track on|off | mode NAME | framing NAME | track-speed NAME | focus auto|N |
/// mirror on|off | raw ADDRESS [ARGS...]`. `--json` prints the result. `make` opens the camera for a device index
/// (`Obsbot(device=...)`).
pub fn cli(
    argv: &[String],
    out: &mut dyn FnMut(&str),
    make: &dyn Fn(i64) -> io::Result<Obsbot>,
) -> i32 {
    let args = std::iter::once("eve-bridge obsbot".to_string()).chain(reorder(argv));
    let a = match parser().try_get_matches_from(args) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };
    let json_out = a.get_flag("json");
    let device = a.get_one::<i64>("device").copied().unwrap_or(0);
    let Some((act, m)) = a.subcommand() else {
        return 2;
    };
    if act == "setup" {
        return setup(Path::new(INI_PATH), out, None);
    }
    let res = match make(device) {
        Err(e) => fail(e.to_string()),
        Ok(mut cam) => {
            let res = run(&mut cam, act, m);
            cam.close();
            res
        }
    };
    let line = res.get("line").map(py_str).unwrap_or_default();
    out(&if json_out {
        py::dumps(&Value::Object(res.clone()))
    } else {
        line
    });
    if truthy(res.get("ok")) {
        0
    } else {
        1
    }
}

fn run(cam: &mut Obsbot, act: &str, m: &ArgMatches) -> Res {
    if let Some(why) = cam.ensure() {
        return fail(why);
    }
    match act {
        "status" => cam.status(),
        "wake" | "sleep" => cam.wake(act == "wake"),
        "move" => cam.move_to(
            m.get_one::<f64>("pan").copied(),
            m.get_one::<f64>("pitch").copied(),
        ),
        "nudge" => cam.nudge(
            &string_of(m, "direction"),
            m.get_one::<i64>("speed").copied().unwrap_or(TURN_SPEED),
            m.get_one::<f64>("seconds").copied().unwrap_or(0.5),
        ),
        "stop" => cam.stop(),
        "center" => cam.reset(),
        "zoom" => cam.zoom(m.get_one::<i64>("level").copied().unwrap_or(0)),
        "fov" => cam.fov(m.get_one::<i64>("degrees").copied().unwrap_or(0)),
        "preset" => cam.preset(m.get_one::<i64>("n").copied().unwrap_or(0)),
        "track" => cam.track(string_of(m, "state") == "on"),
        "mode" => cam.mode(&string_of(m, "name")),
        "framing" => cam.framing(&string_of(m, "name")),
        "track-speed" => cam.track_speed(&string_of(m, "name")),
        "focus" => cam.focus(&string_of(m, "value")),
        "mirror" => cam.mirror(string_of(m, "state") == "on"),
        _ => raw(
            cam,
            &string_of(m, "address"),
            m.get_many::<String>("args").into_iter().flatten(),
        ),
    }
}

fn raw<'a>(cam: &mut Obsbot, address: &str, args: impl Iterator<Item = &'a String>) -> Res {
    if !address.starts_with("/OBSBOT/") {
        return fail("The address starts with /OBSBOT/.");
    }
    if let Some(why) = cam.osc() {
        return fail(why);
    }
    cam.drain();
    let values: Vec<OscValue> = args.map(|s| number(s)).collect();
    if let Err(e) = cam.send(address, &values) {
        return fail(e.to_string());
    }
    // everything that arrives in the next 0.7 s: the camera also pushes changes unasked
    let end = cam.clock.monotonic() + 0.7;
    let mut replies: Vec<(String, Vec<OscValue>)> = Vec::new();
    while let Some(got) = cam.recv_raw((end - cam.clock.monotonic()).max(0.01)) {
        if cam.clock.monotonic() >= end + 0.1 {
            break;
        }
        replies.push(got);
    }
    let lines: String = replies
        .iter()
        .map(|(a, v)| format!("\n  {a} {}", values_repr(v)))
        .collect();
    let line = format!(
        "Sent {address}.{}",
        if lines.is_empty() {
            " No reply."
        } else {
            lines.as_str()
        }
    );
    let replies: Vec<Value> = replies
        .iter()
        .map(|(a, v)| json!([a, Value::Array(v.iter().map(OscValue::to_json).collect())]))
        .collect();
    obj(json!({"ok": true, "line": line, "replies": replies}))
}

/// `cli` with the real camera, printing to stdout.
pub fn main_with_args(argv: &[String]) -> i32 {
    cli(argv, &mut |s| println!("{s}"), &|device| {
        Obsbot::options().device(device).open()
    })
}

#[cfg(test)]
mod tests;
