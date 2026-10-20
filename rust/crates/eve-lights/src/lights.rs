//! The owner's lights: Govee and WiZ over the LAN, HappyLighting over Bluetooth, Tuya — one controller.
//!
//! The owner asked on 2026-09-27 to control their lights from eve (WiZ added 2026-09-28). Each speaks its own
//! protocol, and none needs a cloud or a hub once set up:
//!
//! | Brand | How eve talks to it | What setup needs |
//! |---|---|---|
//! | Govee | Govee's LAN API: JSON over UDP (scan on multicast 239.255.255.250:4001, replies to :4002, commands to :4003) | "LAN Control" on for the light in the Govee app. No key. |
//! | WiZ | WiZ's local API: JSON over UDP port 38899 (`getPilot`, `setPilot`); found by a broadcast `getPilot` | Newer firmware refuses unsigned changes: the home's signing key, from the WiZ app's local-integration export (`lights wiz-import`, docs/lights.md). |
//! | HappyLighting | Bluetooth LE, the "Triones" protocol QH-tek's controllers speak (`QHM-…` names): write `ffd9`, status on `ffd4` | Within Bluetooth range of the Mac. No key. |
//! | Sylvania Smart+ (Wi-Fi) | Tuya's local protocol through tinytuya (the `lights` extra), on the LAN | The device's local key, once, from Tuya's developer platform (docs/lights.md). |
//!
//! The lights live in `~/.config/eve-bridge/lights.json` (mode 600: a Tuya key is a secret), which
//! `eve-bridge lights scan` fills and `lights name` edits. Names, rooms and addresses stay on the Mac, never in
//! the repo.
//!
//! Three doors use one [`Lights`]:
//!
//! * **Telegram** (and everything that rides its brain: the chat window, Mini App calls, the Voice PE's hold to
//!   talk). A message that is **only** a light command ("lights off", "floor lamp blue", "dim the bedroom to
//!   30%") is done by code, no model turn ([`Lights::match_command`]), like quick_answers. Anything else ("make
//!   it cozy in here", "turn the lights off in ten minutes") goes to the model, which has `lights_set` and
//!   `lights_status`.
//! * **The robot's live voice** (voice_agent) gets the same two tools.
//!
//! `EVE_LIGHTS=0` turns it all off. With no lights file, or an empty one, it is off too, and no tool is lent.
//!
//! HappyLighting over Bluetooth from the daemon: macOS grants Bluetooth to the launchd job's process itself. A
//! terminal that has no Bluetooth permission is killed by macOS the moment it scans (exit 134), so `lights scan`
//! from such a terminal takes `--no-ble`, and the daemon (or a terminal with the permission) finds the
//! controller.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::join_all;
use regex::Regex;
use serde_json::{json, Map, Value};
use tracing::{info, warn};

use crate::govee::GoveeDriver;
use crate::py;
use crate::triones::TrionesDriver;
use crate::tuya::TuyaDriver;
use crate::wiz::WizDriver;

pub const CONFIG_PATH: &str = "~/.config/eve-bridge/lights.json";
pub const OFF_REASON: &str = "eve has no lights on this computer (EVE_LIGHTS, ~/.config/eve-bridge/lights.json)";
pub const KINDS: [&str; 4] = ["govee", "wiz", "triones", "tuya"];
/// One light's whole change; a Bluetooth connect alone can take several seconds.
pub const LIGHT_TIMEOUT_SECS: f64 = 12.0;

// ---- errors ---------------------------------------------------------------------------------------------------

/// What a light's driver raises. Each variant is the Python exception it stands for, so a failure reads the
/// same: the reason is the message, or the exception's name when the message is empty.
#[derive(Debug, Clone, thiserror::Error)]
pub enum LightError {
    /// `ConnectionError`
    #[error("{0}")]
    Connection(String),
    /// `TimeoutError` (and `socket.timeout`, whose message is "timed out")
    #[error("{0}")]
    Timeout(String),
    /// `WizRefused`: the bulb answered but refused a change.
    #[error("{0}")]
    Refused(String),
    /// `PermissionError`
    #[error("{0}")]
    Permission(String),
    /// `RuntimeError`
    #[error("{0}")]
    Runtime(String),
    /// `OSError` from a socket, worded `[Errno N] text` as Python words it.
    #[error("{0}")]
    Os(String),
    /// Any other Python exception, by name: `ValueError`, `KeyError`, `JSONDecodeError`, `BleakError`, or an
    /// error the Tuya helper reported.
    #[error("{message}")]
    Python { kind: String, message: String },
}

impl LightError {
    /// The Python exception's name.
    pub fn kind_name(&self) -> &str {
        match self {
            LightError::Connection(_) => "ConnectionError",
            LightError::Timeout(_) => "TimeoutError",
            LightError::Refused(_) => "WizRefused",
            LightError::Permission(_) => "PermissionError",
            LightError::Runtime(_) => "RuntimeError",
            LightError::Os(_) => "OSError",
            LightError::Python { kind, .. } => kind,
        }
    }

    /// `isinstance(e, (OSError, TimeoutError))`: what a LAN driver answers by looking for the light again.
    pub fn is_os(&self) -> bool {
        matches!(
            self,
            LightError::Connection(_)
                | LightError::Timeout(_)
                | LightError::Refused(_)
                | LightError::Permission(_)
                | LightError::Os(_)
        )
    }

    /// `str(e) or type(e).__name__`.
    pub fn reason(&self) -> String {
        let s = self.to_string();
        if s.is_empty() {
            self.kind_name().to_string()
        } else {
            s
        }
    }

    pub(crate) fn py(kind: &str, message: impl Into<String>) -> Self {
        LightError::Python { kind: kind.to_string(), message: message.into() }
    }
}

impl From<py::PyError> for LightError {
    fn from(e: py::PyError) -> Self {
        LightError::py(e.kind, e.message)
    }
}

impl From<std::io::Error> for LightError {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind::*;
        match e.kind() {
            WouldBlock | TimedOut => LightError::Timeout("timed out".into()),
            _ => LightError::Os(os_error_text(&e)),
        }
    }
}

/// An `io::Error` as Python's `str(OSError)` words it: `[Errno 65] No route to host`.
pub(crate) fn os_error_text(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(n) => {
            let text = e.to_string();
            let text = text.strip_suffix(&format!(" (os error {n})")).unwrap_or(&text).to_string();
            format!("[Errno {n}] {text}")
        }
        None => e.to_string(),
    }
}

// ---- colour ---------------------------------------------------------------------------------------------------

pub type Rgb = [i64; 3];

pub const COLORS: &[(&str, Rgb)] = &[
    ("red", [255, 0, 0]),
    ("orange", [255, 100, 0]),
    ("amber", [255, 150, 0]),
    ("yellow", [255, 210, 0]),
    ("gold", [255, 180, 0]),
    ("lime", [160, 255, 0]),
    ("green", [0, 255, 0]),
    ("mint", [60, 255, 140]),
    ("teal", [0, 200, 160]),
    ("cyan", [0, 255, 255]),
    ("turquoise", [0, 220, 200]),
    ("sky blue", [80, 180, 255]),
    ("blue", [0, 0, 255]),
    ("navy", [0, 0, 140]),
    ("indigo", [75, 0, 255]),
    ("purple", [140, 0, 255]),
    ("violet", [170, 60, 255]),
    ("lavender", [190, 150, 255]),
    ("magenta", [255, 0, 255]),
    ("pink", [255, 80, 160]),
    ("hot pink", [255, 20, 120]),
    ("rose", [255, 60, 110]),
    ("coral", [255, 110, 80]),
    ("salmon", [255, 120, 100]),
    ("peach", [255, 170, 110]),
];

/// White, by colour temperature. "white" alone is a neutral white.
pub const WHITES: &[(&str, i64)] = &[
    ("candle", 2000),
    ("candlelight", 2000),
    ("warm", 2700),
    ("warm white", 2700),
    ("soft white", 3000),
    ("white", 4000),
    ("neutral", 4000),
    ("neutral white", 4000),
    ("cool", 5500),
    ("cool white", 5500),
    ("daylight", 6500),
];

pub const KELVIN_MIN: i64 = 1500;
pub const KELVIN_MAX: i64 = 9000;

static HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#?([0-9a-f]{6})$").unwrap());
static KELVIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{4,5})\s*k$").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

fn color_rgb(name: &str) -> Option<Rgb> {
    COLORS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

fn white_kelvin(name: &str) -> Option<i64> {
    WHITES.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
}

/// What the owner asked for: an RGB colour, or a white at a colour temperature.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Color {
    pub rgb: Option<Rgb>,
    pub kelvin: Option<i64>,
    pub word: String,
}

impl Color {
    pub fn as_rgb(&self) -> Rgb {
        match self.rgb {
            Some(rgb) => rgb,
            None => kelvin_to_rgb(match self.kelvin {
                Some(k) if k != 0 => k,
                _ => 4000,
            }),
        }
    }
}

/// A colour name, a `#rrggbb`, a white ("warm white", "daylight") or a temperature ("2700K"). None otherwise.
pub fn parse_color(text: &str) -> Option<Color> {
    let lowered = text.trim().to_lowercase();
    let mut t = SPACES.replace_all(&lowered, " ").into_owned();
    if t.ends_with(" light") {
        t.truncate(t.len() - " light".len());
    }
    if let Some(rgb) = color_rgb(&t) {
        return Some(Color { rgb: Some(rgb), kelvin: None, word: t });
    }
    if let Some(k) = white_kelvin(&t) {
        return Some(Color { rgb: None, kelvin: Some(k), word: t });
    }
    if let Some(m) = HEX.captures(&t) {
        let h = &m[1];
        let ch = |a: usize| i64::from_str_radix(&h[a..a + 2], 16).unwrap_or(0);
        return Some(Color { rgb: Some([ch(0), ch(2), ch(4)]), kelvin: None, word: format!("#{h}") });
    }
    if let Some(m) = KELVIN.captures(&t) {
        let digits = &m[1];
        let k: i64 = digits.parse().ok()?;
        return Some(Color { rgb: None, kelvin: Some(k.clamp(KELVIN_MIN, KELVIN_MAX)), word: format!("{digits}K") });
    }
    None
}

/// An RGB that looks like a white of this temperature (Tanner Helland's fit), for lights with no white LED.
pub fn kelvin_to_rgb(kelvin: i64) -> Rgb {
    let t = kelvin.clamp(KELVIN_MIN, KELVIN_MAX) as f64 / 100.0;
    let (r, g, b) = if t <= 66.0 {
        let g = 99.4708025861 * t.ln() - 161.1195681661;
        let b = if t <= 19.0 { 0.0 } else { 138.5177312231 * (t - 10.0).ln() - 305.0447927307 };
        (255.0, g, b)
    } else {
        (329.698727446 * (t - 60.0).powf(-0.1332047592), 288.1221695283 * (t - 60.0).powf(-0.0755148492), 255.0)
    };
    let one = |v: f64| py::round_half_even(v).clamp(0.0, 255.0) as i64;
    [one(r), one(g), one(b)]
}

/// The colour's hue at `percent` brightness: the brightest channel becomes 255 * percent / 100. Black stays
/// black-free: a colour with no light in it is treated as white.
pub fn scale_rgb(rgb: &[i64], percent: i64) -> Rgb {
    let top = rgb.iter().copied().max().unwrap_or(0);
    let base: Vec<f64> =
        if top <= 0 { vec![255.0; 3] } else { rgb.iter().map(|&c| c as f64 * 255.0 / top as f64).collect() };
    let k = percent.clamp(0, 100) as f64 / 100.0;
    let one = |i: usize| base.get(i).map(|c| py::round_half_even(c * k) as i64).unwrap_or(0);
    [one(0), one(1), one(2)]
}

pub fn rgb_percent(rgb: &[i64]) -> i64 {
    match rgb.iter().copied().max() {
        Some(top) => py::round_half_even(top as f64 * 100.0 / 255.0) as i64,
        None => 0,
    }
}

pub fn hex_of(rgb: &[i64]) -> String {
    let mut out = String::from("#");
    for &c in rgb {
        if c < 0 {
            // f"{-1:02x}" is "-1": the sign counts toward the width
            out.push_str(&format!("-{:x}", c.unsigned_abs()));
        } else {
            out.push_str(&format!("{:02x}", c));
        }
    }
    out
}

/// Python's `colorsys.rgb_to_hsv`.
fn rgb_to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let maxc = r.max(g).max(b);
    let minc = r.min(g).min(b);
    let v = maxc;
    if minc == maxc {
        return (0.0, 0.0, v);
    }
    let range = maxc - minc;
    let s = range / maxc;
    let rc = (maxc - r) / range;
    let gc = (maxc - g) / range;
    let bc = (maxc - b) / range;
    let h = if r == maxc {
        bc - gc
    } else if g == maxc {
        2.0 + rc - bc
    } else {
        4.0 + gc - rc
    };
    ((h / 6.0).rem_euclid(1.0), s, v)
}

/// The nearest colour name, by hue, for a status line.
pub fn color_word(rgb: &[i64]) -> String {
    let c = |i: usize| rgb.get(i).copied().unwrap_or(0) as f64 / 255.0;
    let (h, s, v) = rgb_to_hsv(c(0), c(1), c(2));
    if v < 0.02 {
        return "off".into();
    }
    if s < 0.18 {
        return "white".into();
    }
    let (mut best, mut dist) = ("", 9.0);
    for (name, col) in COLORS {
        let (h2, s2, _) = rgb_to_hsv(col[0] as f64 / 255.0, col[1] as f64 / 255.0, col[2] as f64 / 255.0);
        let d = (h - h2).abs().min(1.0 - (h - h2).abs()) + 0.2 * (s - s2).abs();
        if d < dist {
            best = name;
            dist = d;
        }
    }
    best.to_string()
}

// ---- the change asked for, and a light's state ----------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    /// None: leave it (a colour or a brightness turns an off light on)
    pub power: Option<bool>,
    /// 1-100
    pub brightness: Option<i64>,
    pub color: Option<Color>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.power.is_none() && self.brightness.is_none() && self.color.is_none()
    }

    /// brightness 0 means off; anything above 100 is 100.
    pub fn normalised(&self) -> Change {
        if matches!(self.brightness, Some(b) if b <= 0) {
            return Change { power: Some(false), ..Change::default() };
        }
        Change { power: self.power, brightness: self.brightness.map(|b| b.min(100)), color: self.color.clone() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub on: Option<bool>,
    pub brightness: Option<i64>,
    pub rgb: Option<Rgb>,
    pub kelvin: Option<i64>,
}

impl State {
    pub fn public(&self) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert("on".into(), self.on.map(Value::Bool).unwrap_or(Value::Null));
        if let Some(b) = self.brightness {
            out.insert("brightness".into(), json!(b));
        }
        if let Some(k) = self.kelvin.filter(|k| *k != 0) {
            out.insert("white_kelvin".into(), json!(k));
        } else if let Some(rgb) = self.rgb {
            out.insert("color".into(), json!(color_word(&rgb)));
            out.insert("rgb".into(), json!(hex_of(&rgb)));
        }
        out
    }
}

/// The change that puts another light in this state: "match the lights" copies one light onto the rest.
pub fn change_of(state: &State) -> Change {
    if state.on == Some(false) {
        return Change { power: Some(false), ..Change::default() };
    }
    let mut color = None;
    if let Some(k) = state.kelvin.filter(|k| *k != 0) {
        color = Some(Color { rgb: None, kelvin: Some(k), word: format!("{k}K") });
    } else if let Some(rgb) = state.rgb.filter(|rgb| rgb.iter().any(|c| *c != 0)) {
        // a Bluetooth controller's level is in its colour: take the hue at full
        let rgb = scale_rgb(&rgb, 100);
        color = Some(Color { rgb: Some(rgb), kelvin: None, word: color_word(&rgb) });
    }
    Change { power: Some(true), brightness: state.brightness.filter(|b| *b != 0), color }
}

// ---- a light, and the lights file ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Light {
    pub name: String,
    pub kind: String,
    pub room: String,
    pub aliases: Vec<String>,
    /// govee and wiz (a WiZ light's device is its MAC, its sku the module name)
    pub ip: String,
    pub device: String,
    pub sku: String,
    /// triones (a CoreBluetooth UUID on macOS, a MAC elsewhere)
    pub address: String,
    /// tuya (and a WiZ home's UDP signing key, hex)
    pub id: String,
    pub key: String,
    pub version: String,
}

impl Default for Light {
    fn default() -> Self {
        Light {
            name: String::new(),
            kind: String::new(),
            room: String::new(),
            aliases: Vec::new(),
            ip: String::new(),
            device: String::new(),
            sku: String::new(),
            address: String::new(),
            id: String::new(),
            key: String::new(),
            version: "3.3".into(),
        }
    }
}

/// `str(d.get(key) or default)`.
fn field(d: &Map<String, Value>, key: &str, default: &str) -> String {
    match d.get(key) {
        Some(v) if py::truthy(v) => py::str_of(v),
        _ => default.to_string(),
    }
}

impl Light {
    /// A light of this kind and name, the rest empty (`Light(name, kind)`).
    pub fn new(name: impl Into<String>, kind: impl Into<String>) -> Self {
        Light { name: name.into(), kind: kind.into(), ..Light::default() }
    }

    /// A light from its lights-file entry; the error says why the entry is skipped.
    pub fn from_dict(d: &Value) -> Result<Light, String> {
        let Some(d) = d.as_object() else {
            return Err(format!("'{}' object has no attribute 'get'", py::type_name(d)));
        };
        let kind = field(d, "kind", "").trim().to_lowercase();
        if !KINDS.contains(&kind.as_str()) {
            return Err(format!("unknown light kind {}", py::repr_str(&kind)));
        }
        let name = field(d, "name", "").trim().to_string();
        if name.is_empty() {
            return Err("a light needs a name".into());
        }
        let aliases: Vec<String> = match d.get("aliases") {
            Some(v) if py::truthy(v) => match v {
                Value::Array(items) => items.iter().map(py::str_of).collect(),
                Value::String(s) => s.chars().map(String::from).collect(),
                Value::Object(m) => m.keys().cloned().collect(),
                other => return Err(format!("'{}' object is not iterable", py::type_name(other))),
            },
            _ => Vec::new(),
        };
        let aliases = aliases.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
        Ok(Light {
            name,
            kind,
            room: field(d, "room", "").trim().to_string(),
            aliases,
            ip: field(d, "ip", ""),
            device: field(d, "device", ""),
            sku: field(d, "sku", ""),
            address: field(d, "address", ""),
            id: field(d, "id", ""),
            key: field(d, "key", ""),
            version: field(d, "version", "3.3"),
        })
    }

    pub fn to_dict(&self) -> Value {
        let mut out = Map::new();
        out.insert("name".into(), json!(self.name));
        out.insert("kind".into(), json!(self.kind));
        if !self.room.is_empty() {
            out.insert("room".into(), json!(self.room));
        }
        if !self.aliases.is_empty() {
            out.insert("aliases".into(), json!(self.aliases));
        }
        let fields: &[&str] = match self.kind.as_str() {
            "govee" => &["ip", "device", "sku"],
            "wiz" => &["ip", "device", "sku", "key"],
            "triones" => &["address"],
            "tuya" => &["id", "ip", "key", "version"],
            _ => &[],
        };
        for f in fields {
            let v = match *f {
                "ip" => &self.ip,
                "device" => &self.device,
                "sku" => &self.sku,
                "address" => &self.address,
                "id" => &self.id,
                "key" => &self.key,
                _ => &self.version,
            };
            if !v.is_empty() {
                out.insert(f.to_string(), json!(v));
            }
        }
        Value::Object(out)
    }
}

/// `Path.expanduser()`: a leading `~` is the home directory.
pub fn expand_user(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s == "~" || s.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from).or_else(dirs::home_dir) {
            return if s == "~" { home } else { home.join(&s[2..]) };
        }
    }
    p.to_path_buf()
}

/// The lights file's path.
pub fn config_path() -> PathBuf {
    expand_user(Path::new(CONFIG_PATH))
}

fn resolve_path(path: Option<&Path>) -> PathBuf {
    match path {
        Some(p) => expand_user(p),
        None => config_path(),
    }
}

/// The Python exception name a failed read of a JSON file raises.
fn read_error_name(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind::*;
    match e.kind() {
        PermissionDenied => "PermissionError",
        IsADirectory => "IsADirectoryError",
        NotADirectory => "NotADirectoryError",
        InvalidData => "UnicodeDecodeError",
        _ => "OSError",
    }
}

/// The lights file, or [] when it is missing. A bad entry is skipped with a warning, never fatal.
pub fn load(path: Option<&Path>) -> Vec<Light> {
    let p = resolve_path(path);
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            warn!("lights: {} is unreadable ({})", p.display(), read_error_name(&e));
            return Vec::new();
        }
    };
    let raw: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => {
            warn!("lights: {} is unreadable (JSONDecodeError)", p.display());
            return Vec::new();
        }
    };
    let entries: Vec<Value> = match raw.as_object().and_then(|o| o.get("lights")) {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::Object(m)) if !m.is_empty() => m.keys().map(|k| json!(k)).collect(),
        Some(Value::String(s)) => s.chars().map(|c| json!(c.to_string())).collect(),
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for d in &entries {
        match Light::from_dict(d) {
            Ok(lt) => out.push(lt),
            Err(e) => warn!("lights: skipping an entry in {} ({})", p.display(), e),
        }
    }
    out
}

/// Write the lights file (mode 600, through a temporary file).
pub fn save<'a>(lights: impl IntoIterator<Item = &'a Light>, path: Option<&Path>) -> std::io::Result<PathBuf> {
    let p = resolve_path(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = p.with_extension("tmp");
    let body = json!({"lights": lights.into_iter().map(Light::to_dict).collect::<Vec<_>>()});
    std::fs::write(&tmp, py::dumps_indent(&body, 2) + "\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

// ---- the drivers ------------------------------------------------------------------------------------------------

/// One brand's way of reading and changing a light.
#[async_trait]
pub trait Driver: Send + Sync {
    async fn state(&self) -> Result<State, LightError>;
    async fn apply(&self, change: &Change) -> Result<(), LightError>;
    async fn close(&self) -> Result<(), LightError>;
}

/// Makes a light's driver (the Python `DRIVERS` values).
pub type DriverFactory = Arc<dyn Fn(&Light) -> Arc<dyn Driver> + Send + Sync>;

/// The real drivers, by kind.
pub fn drivers() -> HashMap<String, DriverFactory> {
    let mut out: HashMap<String, DriverFactory> = HashMap::new();
    out.insert("govee".into(), Arc::new(|lt: &Light| Arc::new(GoveeDriver::new(lt.clone())) as Arc<dyn Driver>));
    out.insert("wiz".into(), Arc::new(|lt: &Light| Arc::new(WizDriver::new(lt.clone())) as Arc<dyn Driver>));
    out.insert("triones".into(), Arc::new(|lt: &Light| Arc::new(TrionesDriver::new(lt.clone())) as Arc<dyn Driver>));
    out.insert("tuya".into(), Arc::new(|lt: &Light| Arc::new(TuyaDriver::new(lt.clone())) as Arc<dyn Driver>));
    out
}

/// Run blocking socket work off the async threads (`asyncio.to_thread`).
pub(crate) async fn blocking<T, F>(f: F) -> Result<T, LightError>
where
    F: FnOnce() -> Result<T, LightError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r,
        Err(e) => Err(LightError::Runtime(e.to_string())),
    }
}

// ---- which lights a request means -------------------------------------------------------------------------------

pub const ALL_WORDS: &[&str] = &[
    "", "all", "lights", "light", "the lights", "all lights", "all the lights", "every light", "all of them",
    "everything", "them", "it",
];

static NORM_DROP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\w#%\s-]").unwrap());

pub(crate) fn norm(text: &str) -> String {
    let lowered = text.to_lowercase().replace('\u{2019}', "'");
    let dropped = NORM_DROP.replace_all(&lowered, " ");
    SPACES.replace_all(&dropped, " ").trim().to_string()
}

// ---- the tools --------------------------------------------------------------------------------------------------

pub static TOOLS: LazyLock<Value> = LazyLock::new(|| {
    json!([
        {"type": "function", "name": "lights_set", "strict": true,
         "description": "Change the owner's lights: on or off, brightness, colour or a white. Only what you pass changes; a colour or a brightness turns an off light on.",
         "parameters": {"type": "object", "additionalProperties": false,
                        "required": ["target", "power", "brightness", "color"],
                        "properties": {
                            "target": {"type": "string",
                                       "description": "\"all\", a light's name, or a room, as listed in your instructions."},
                            "power": {"type": ["boolean", "null"], "description": "true on, false off, null to leave."},
                            "brightness": {"type": ["integer", "null"], "description": "1-100 percent; null to leave."},
                            "color": {"type": ["string", "null"],
                                      "description": "A colour name (red, blue, purple, pink, orange...), a #rrggbb, a white (\"warm white\", \"white\", \"cool white\", \"daylight\") or a temperature like \"2700K\"; null to leave."}}}},
        {"type": "function", "name": "lights_match", "strict": true,
         "description": "Make lights match another light: the same on or off, colour or white, and brightness. Use it for \"make X the same as the others\", \"match the lights\", \"sync the lights\".",
         "parameters": {"type": "object", "additionalProperties": false, "required": ["target", "source"],
                        "properties": {
                            "target": {"type": "string", "description": "The lights to change: \"all\", a light's name, or a room."},
                            "source": {"type": ["string", "null"],
                                       "description": "The light to copy, by name; null to copy the other lights."}}}},
        {"type": "function", "name": "lights_status", "strict": true,
         "description": "Whether the owner's lights are on, and their brightness and colour.",
         "parameters": {"type": "object", "additionalProperties": false, "required": ["target"],
                        "properties": {"target": {"type": "string", "description": "\"all\", a light's name, or a room."}}}}
    ])
});
pub const TOOL_NAMES: [&str; 3] = ["lights_set", "lights_match", "lights_status"];

pub const INSTRUCTIONS_HEAD: &str = "You can control the owner's lights with lights_set (on, off, brightness, colour, white),
make lights match another light with lights_match, and read them with lights_status. For a mood, pick the settings yourself: cozy is warm white around 30%, reading is
white at 100%, a movie is dim, a party is a colour. Say what you did in a few words.";

/// A light command the code understood: which lights, and what to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub target: String,
    pub change: Change,
    pub names: Vec<String>,
    /// set: copy this light ("" for the other lights) instead of change
    pub source: Option<String>,
}

// The words a command may carry besides a target, an on/off, a colour and a level. Anything else — "in ten
// minutes", "are", "why" — and the message is not only a command: the model gets it.
const FILLER: &[&str] = &[
    "please", "pls", "eve", "hey", "ok", "okay", "can", "could", "would", "will", "you", "u", "the", "my", "to",
    "at", "in", "light", "lights", "lamp", "lamps", "turn", "switch", "set", "make", "put", "change", "brightness",
    "level", "now", "and", "a", "bit", "all", "of", "them", "it", "thanks", "thank", "for", "me", "go", "up",
    "percent",
];
const VERB_DIM: &[&str] = &["dim", "darken", "lower"];
const VERB_BRIGHT: &[&str] = &["brighten", "brighter", "raise", "full", "max"];
fn power_word(w: &str) -> Option<bool> {
    match w {
        "on" => Some(true),
        "off" | "out" => Some(false),
        _ => None,
    }
}
pub const DIM_PERCENT: i64 = 20;
const NAME_STOPWORDS: &[&str] = &["the", "my", "a", "of", "and", "in", "on", "off", "to", "at"];
static PCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{1,3})%?$").unwrap());
// "match the lights", "sync the lights", "make wiz match the lamp", "wiz same as the others", "match wiz to the lamp"
static COPY_ALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:match|sync)(?: up)?(?: all)?(?: the| my)? lights?(?: up)?$").unwrap());
static COPY_AS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?:make |set |turn |put |change )?(?P<target>.+?) (?:to )?(?:match(?:es)?|",
        r"(?:the )?same(?: (?:colou?rs?|lights?|settings?|way))? as|like) (?P<source>.+)$"
    ))
    .unwrap()
});
static COPY_TO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:match|sync) (?P<target>.+?)(?: (?:to|with) (?P<source>.+))?$").unwrap());
static COPY_LEAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:(?:hey|ok|okay|eve|please|pls|can you|could you|would you) )+").unwrap());
static COPY_TAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?: (?:please|pls|now|rn|right now|thanks|thank you|too|as well))+$").unwrap());
const OTHERS: &[&str] = &[
    "others", "the others", "other lights", "the other lights", "the other ones", "the rest", "rest of the lights",
    "the rest of the lights", "everything else", "everyone else", "the other light",
];
// a source is read before it is copied: a LAN light answers in milliseconds, Bluetooth has to connect
const SOURCE_ORDER: [&str; 4] = ["govee", "wiz", "tuya", "triones"];
static THE_MY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(the|my)\s+").unwrap());
static LIGHTS_TAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+(lights?|lamps?)$").unwrap());

/// One light's outcome: done, or why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub name: String,
    pub ok: bool,
    pub reason: String,
}

/// The owner's lights, their drivers, the tools and the code path.
pub struct Lights {
    pub lights: Vec<Light>,
    make: HashMap<String, DriverFactory>,
    cache: parking_lot::Mutex<HashMap<String, Arc<dyn Driver>>>,
    pub timeout: Duration,
}

impl Lights {
    /// The lights with the real drivers and the default timeout.
    pub fn new(lights: Vec<Light>) -> Self {
        Self::with(lights, None, Duration::from_secs_f64(LIGHT_TIMEOUT_SECS))
    }

    /// The lights with these drivers (by kind; None: the real ones) and this timeout per light.
    pub fn with(lights: Vec<Light>, drivers_by_kind: Option<HashMap<String, DriverFactory>>, timeout: Duration) -> Self {
        Lights {
            lights,
            make: drivers_by_kind.unwrap_or_else(drivers),
            cache: parking_lot::Mutex::new(HashMap::new()),
            timeout,
        }
    }

    pub fn driver(&self, light: &Light) -> Result<Arc<dyn Driver>, LightError> {
        let mut cache = self.cache.lock();
        if let Some(d) = cache.get(&light.name) {
            return Ok(d.clone());
        }
        let make = self.make.get(&light.kind).ok_or_else(|| LightError::py("KeyError", py::repr_str(&light.kind)))?;
        let d = make(light);
        cache.insert(light.name.clone(), d.clone());
        Ok(d)
    }

    // -- targets --
    fn words_of(light: &Light) -> Vec<String> {
        std::iter::once(norm(&light.name)).chain(light.aliases.iter().map(|a| norm(a))).filter(|w| !w.is_empty()).collect()
    }

    /// (lights, "") or ([], why not). "all"/"the lights" is every light; then a name or alias; then a room;
    /// then a single word that is in exactly one name ("lamp" for "floor lamp") or names a brand.
    pub fn resolve(&self, target: &str) -> (Vec<&Light>, String) {
        match self.resolve_idx(target) {
            Ok(idx) => (idx.into_iter().map(|i| &self.lights[i]).collect(), String::new()),
            Err(why) => (Vec::new(), why),
        }
    }

    fn resolve_idx(&self, target: &str) -> Result<Vec<usize>, String> {
        let t = norm(target);
        let t = THE_MY.replace(&t, "").into_owned();
        let t = if t != "lights" && t != "light" { LIGHTS_TAIL.replace(&t, "").into_owned() } else { t };
        let all: Vec<usize> = (0..self.lights.len()).collect();
        if ALL_WORDS.contains(&t.as_str()) {
            return Ok(all);
        }
        let pick = |f: &dyn Fn(&Light) -> bool| -> Vec<usize> {
            all.iter().copied().filter(|&i| f(&self.lights[i])).collect()
        };
        let by_name = pick(&|lt| Self::words_of(lt).contains(&t));
        if !by_name.is_empty() {
            return Ok(by_name);
        }
        let by_room = pick(&|lt| !lt.room.is_empty() && norm(&lt.room) == t);
        if !by_room.is_empty() {
            return Ok(by_room);
        }
        let by_kind = pick(&|lt| {
            t == lt.kind
                || match lt.kind.as_str() {
                    "triones" => t == "happylighting",
                    "tuya" => t == "sylvania",
                    _ => false,
                }
        });
        if !by_kind.is_empty() {
            return Ok(by_kind);
        }
        if let Ok(re) = Regex::new(&format!(r"\b{}\b", regex::escape(&t))) {
            let partial = pick(&|lt| Self::words_of(lt).iter().any(|w| re.is_match(w)));
            if !partial.is_empty() {
                return Ok(partial);
            }
        }
        Err(format!("no light called {}; the lights are: ", py::repr_str(target)) + &self.names_line())
    }

    pub fn names_line(&self) -> String {
        let line = self
            .lights
            .iter()
            .map(|lt| if lt.room.is_empty() { lt.name.clone() } else { format!("{} ({})", lt.name, lt.room) })
            .collect::<Vec<_>>()
            .join(", ");
        if line.is_empty() {
            "none".into()
        } else {
            line
        }
    }

    // -- doing it --
    async fn one(&self, i: usize, change: &Change) -> Outcome {
        let light = &self.lights[i];
        let work = async {
            let d = self.driver(light)?;
            d.apply(change).await
        };
        match tokio::time::timeout(self.timeout, work).await {
            Ok(Ok(())) => Outcome { name: light.name.clone(), ok: true, reason: String::new() },
            Err(_) => Outcome { name: light.name.clone(), ok: false, reason: "it did not answer in time".into() },
            Ok(Err(e)) => {
                // one light failing never stops the others
                warn!("lights: {} failed: {}: {}", light.name, e.kind_name(), e);
                Outcome { name: light.name.clone(), ok: false, reason: e.reason() }
            }
        }
    }

    async fn all(&self, idx: &[usize], change: &Change) -> Vec<Outcome> {
        join_all(idx.iter().map(|&i| self.one(i, change))).await
    }

    async fn read(&self, light: &Light) -> Result<State, LightError> {
        let work = async {
            let d = self.driver(light)?;
            d.state().await
        };
        match tokio::time::timeout(self.timeout, work).await {
            Ok(r) => r,
            Err(_) => Err(LightError::Timeout(String::new())),
        }
    }

    pub async fn set(&self, target: &str, change: &Change) -> Value {
        let idx = match self.resolve_idx(target) {
            Ok(idx) => idx,
            Err(why) => return json!({"ok": false, "reason": why}),
        };
        if change.is_empty() {
            return json!({"ok": false, "reason": "nothing to change: give power, brightness or a colour"});
        }
        let results = self.all(&idx, change).await;
        let done: Vec<&str> = results.iter().filter(|r| r.ok).map(|r| r.name.as_str()).collect();
        let failed = failed_list(&results);
        info!("lights: {} -> {} done, {} failed", describe(change), done.len(), results.len() - done.len());
        json!({"ok": !done.is_empty(), "done": done, "failed": failed, "change": describe(change)})
    }

    pub async fn status(&self, target: &str) -> Value {
        let idx = match self.resolve_idx(target) {
            Ok(idx) => idx,
            Err(why) => return json!({"ok": false, "reason": why}),
        };
        let one = |i: usize| async move {
            let lt = &self.lights[i];
            let mut out = Map::new();
            out.insert("name".into(), json!(lt.name));
            if !lt.room.is_empty() {
                out.insert("room".into(), json!(lt.room));
            }
            match self.read(lt).await {
                Ok(st) => {
                    out.insert("reachable".into(), json!(true));
                    out.extend(st.public());
                }
                Err(e) => {
                    out.insert("reachable".into(), json!(false));
                    out.insert("reason".into(), json!(e.reason()));
                }
            }
            Value::Object(out)
        };
        let lights = join_all(idx.into_iter().map(one)).await;
        json!({"ok": true, "lights": lights})
    }

    /// Put the target lights in the source light's state. No source: the first other light that answers, a
    /// Wi-Fi light before a Bluetooth one. With every light as the target, one light is copied onto the rest.
    pub async fn copy(&self, target: &str, source: Option<&str>) -> Value {
        let targets = match self.resolve_idx(target) {
            Ok(idx) => idx,
            Err(why) => return json!({"ok": false, "reason": why}),
        };
        let mut sources = match source {
            Some(s) if !s.is_empty() && !OTHERS.contains(&norm(s).as_str()) => match self.resolve_idx(s) {
                Ok(idx) => idx,
                Err(why) => return json!({"ok": false, "reason": why}),
            },
            _ => {
                let others: Vec<usize> = (0..self.lights.len()).filter(|i| !targets.contains(i)).collect();
                if others.is_empty() {
                    targets.clone()
                } else {
                    others
                }
            }
        };
        sources.sort_by_key(|&i| SOURCE_ORDER.iter().position(|k| *k == self.lights[i].kind).unwrap_or(9));
        let mut picked: Option<(usize, State)> = None;
        let mut tried: Vec<String> = Vec::new();
        for &i in &sources {
            let lt = &self.lights[i];
            match self.read(lt).await {
                Ok(st) => {
                    picked = Some((i, st));
                    break;
                }
                // try the next light
                Err(e) => tried.push(format!("{}: {}", lt.name, e.reason())),
            }
        }
        let Some((src, state)) = picked else {
            return json!({"ok": false, "reason": format!("no light to copy answered ({})", tried.join("; "))});
        };
        let change = change_of(&state);
        let to_set: Vec<usize> = targets.iter().copied().filter(|&i| i != src).collect();
        if to_set.is_empty() {
            return json!({"ok": false, "reason": format!("{} is the only light: nothing to match it to",
                                                         self.lights[src].name)});
        }
        let results = self.all(&to_set, &change).await;
        let done: Vec<&str> = results.iter().filter(|r| r.ok).map(|r| r.name.as_str()).collect();
        info!("lights: match {} -> {} done, {} failed", describe(&change), done.len(), results.len() - done.len());
        let to_set_lights: Vec<&Light> = to_set.iter().map(|&i| &self.lights[i]).collect();
        json!({"ok": !done.is_empty(), "source": self.lights[src].name, "change": describe(&change),
               "done": done, "failed": failed_list(&results),
               "line": self.copy_line(&self.lights[src], &to_set_lights, &results, &change)})
    }

    /// "Wiz matched to floor lamp: red at 100%." and each light that did not.
    pub fn copy_line(&self, source: &Light, _lights: &[&Light], results: &[Outcome], change: &Change) -> String {
        let done: Vec<&str> = results.iter().filter(|r| r.ok).map(|r| r.name.as_str()).collect();
        let mut parts = Vec::new();
        if !done.is_empty() {
            parts.push(format!("{} matched to {}: {}.", join_names(&done), source.name, describe(change)));
        }
        parts.extend(results.iter().filter(|r| !r.ok).map(|r| format!("{}: {}.", r.name, r.reason)));
        if parts.is_empty() {
            "No lights changed.".into()
        } else {
            parts.join(" ")
        }
    }

    pub async fn close(&self) {
        let drivers: Vec<Arc<dyn Driver>> = self.cache.lock().drain().map(|(_, d)| d).collect();
        for d in drivers {
            let _ = d.close().await;
        }
    }

    // -- the model's tools --
    pub fn tools(&self) -> Vec<Value> {
        TOOLS.as_array().cloned().unwrap_or_default()
    }

    pub fn instructions(&self) -> String {
        let rows = self
            .lights
            .iter()
            .map(|lt| {
                let mut row = format!("- {}", lt.name);
                if !lt.room.is_empty() {
                    row += &format!(" (room: {})", lt.room);
                }
                if !lt.aliases.is_empty() {
                    row += &format!(", also called {}", lt.aliases.join(", "));
                }
                row
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("{INSTRUCTIONS_HEAD}\nThe lights:\n{rows}")
    }

    pub async fn handle(&self, name: &str, args: &Value) -> Value {
        let arg = |k: &str| args.get(k).cloned().unwrap_or(Value::Null);
        let target = || {
            let t = arg("target");
            if py::truthy(&t) {
                py::str_of(&t)
            } else {
                "all".to_string()
            }
        };
        match name {
            "lights_status" => self.status(&target()).await,
            "lights_match" => {
                let source = arg("source");
                let source = if py::truthy(&source) { Some(py::str_of(&source)) } else { None };
                let mut out = self.copy(&target(), source.as_deref()).await;
                if let Some(o) = out.as_object_mut() {
                    o.shift_remove("line");
                }
                out
            }
            "lights_set" => {
                let raw = arg("color");
                let mut color = None;
                if !raw.is_null() && raw != json!("") {
                    color = parse_color(&py::str_of(&raw));
                    if color.is_none() {
                        return json!({"ok": false, "reason": format!(
                            "unknown colour {}: use a colour name, #rrggbb, a white or a temperature like 2700K",
                            py::repr(&raw))});
                    }
                }
                let bright = arg("brightness");
                let change = Change {
                    power: arg("power").as_bool(),
                    brightness: if py::is_number(&bright) { py::int_of(&bright).ok() } else { None },
                    color,
                };
                self.set(&target(), &change).await
            }
            _ => json!({"ok": false, "reason": format!("unknown tool {name}")}),
        }
    }

    // -- the code path: a message that is only a light command --

    /// A Command when the whole message is a light command, else None (the model answers). Python's
    /// `Lights.match`.
    ///
    /// Every word must be accounted for: a light, a room or "lights"; on/off; a colour or white; a level ("40%",
    /// "dim"); or a filler word. A message must name a light ("lights", "lamp", or a configured name or room),
    /// and must ask for something. "Turn off the lights in ten minutes" leaves "ten minutes" over: model.
    pub fn match_command(&self, text: &str) -> Option<Command> {
        if self.lights.is_empty() {
            return None;
        }
        let t = norm(text);
        if t.is_empty() || t.chars().count() > 80 {
            return None;
        }
        if let Some(copy) = self.match_copy(&t) {
            return Some(copy);
        }
        let mut rest = format!(" {t} ");
        // the lights' own words first, longest first, so "bedroom lamp" wins over "lamp"
        let mut phrases: Vec<(String, String)> = Vec::new();
        let add = |p: (String, String), into: &mut Vec<(String, String)>| {
            if !into.contains(&p) {
                into.push(p);
            }
        };
        for lt in &self.lights {
            for w in Self::words_of(lt) {
                add((w, lt.name.clone()), &mut phrases);
            }
        }
        for lt in self.lights.iter().filter(|lt| !lt.room.is_empty()) {
            add((norm(&lt.room), format!("@{}", lt.room)), &mut phrases);
        }
        phrases.sort_by_key(|p| std::cmp::Reverse(p.0.chars().count()));
        // then a single word of exactly one light's name: "the lamp" is the floor lamp when only one is a lamp
        let mut seen: Vec<(String, Vec<String>)> = Vec::new();
        for lt in &self.lights {
            let mut words: Vec<String> = Vec::new();
            for phrase in Self::words_of(lt) {
                for x in phrase.split_whitespace() {
                    if !NAME_STOPWORDS.contains(&x) && !words.iter().any(|w| w == x) {
                        words.push(x.to_string());
                    }
                }
            }
            for w in words {
                match seen.iter_mut().find(|(s, _)| *s == w) {
                    Some((_, who)) => {
                        if !who.contains(&lt.name) {
                            who.push(lt.name.clone());
                        }
                    }
                    None => seen.push((w, vec![lt.name.clone()])),
                }
            }
        }
        phrases.extend(seen.into_iter().filter(|(_, who)| who.len() == 1).map(|(w, mut who)| (w, who.remove(0))));
        let mut targets: Vec<String> = Vec::new();
        for (phrase, who) in &phrases {
            let pat = format!(" {phrase} ");
            if !phrase.is_empty() && rest.contains(&pat) {
                rest = rest.replacen(&pat, " ", 1);
                targets.push(who.clone());
            }
        }
        // colours next, two words before one ("warm white" before "white")
        let mut color: Option<Color> = None;
        let mut names: Vec<&str> = COLORS.iter().map(|(n, _)| *n).chain(WHITES.iter().map(|(n, _)| *n)).collect();
        names.sort_by_key(|p| std::cmp::Reverse(p.chars().count()));
        for phrase in names {
            let pat = format!(" {phrase} ");
            if rest.contains(&pat) {
                if color.is_some() {
                    return None; // two colours: not a simple command
                }
                color = parse_color(phrase);
                rest = rest.replacen(&pat, " ", 1);
            }
        }
        let mut power: Option<bool> = None;
        let mut bright: Option<i64> = None;
        let mut light_word = !targets.is_empty();
        let mut leftover: Vec<&str> = Vec::new();
        for w in rest.split_whitespace() {
            let pct = PCT.captures(w).and_then(|m| m[1].parse::<i64>().ok());
            if let Some(p) = power_word(w) {
                if power.is_some() && power != Some(p) {
                    return None;
                }
                power = Some(p);
            } else if let Some(n) = pct.filter(|n| *n <= 100) {
                bright = Some(n);
            } else if KELVIN.is_match(w) {
                if color.is_some() {
                    return None;
                }
                color = parse_color(w);
            } else if VERB_DIM.contains(&w) {
                bright = bright.or(Some(DIM_PERCENT));
            } else if VERB_BRIGHT.contains(&w) {
                bright = bright.or(Some(100));
            } else if w.starts_with('#') && parse_color(w).is_some() {
                if color.is_some() {
                    return None;
                }
                color = parse_color(w);
            } else if matches!(w, "light" | "lights" | "lamp" | "lamps") {
                light_word = true;
            } else if FILLER.contains(&w) {
            } else {
                leftover.push(w);
            }
        }
        if !leftover.is_empty() || !light_word {
            return None;
        }
        let change = Change { power, brightness: bright, color };
        if change.is_empty() {
            return None;
        }
        if targets.is_empty() {
            return Some(Command {
                target: "all".into(),
                change,
                names: self.lights.iter().map(|lt| lt.name.clone()).collect(),
                source: None,
            });
        }
        let mut chosen: Vec<usize> = Vec::new();
        for who in &targets {
            let picked: Vec<usize> = match who.strip_prefix('@') {
                Some(room) => (0..self.lights.len()).filter(|&i| self.lights[i].room == room).collect(),
                None => (0..self.lights.len()).filter(|&i| &self.lights[i].name == who).collect(),
            };
            for i in picked {
                if !chosen.contains(&i) {
                    chosen.push(i);
                }
            }
        }
        Some(Command {
            target: targets.join(","),
            change,
            names: (0..self.lights.len()).filter(|i| chosen.contains(i)).map(|i| self.lights[i].name.clone()).collect(),
            source: None,
        })
    }

    /// "match the lights", "make wiz match the lamp", "wiz same as the others": a Command with a source.
    /// Target and source must both name lights, or it is not this command.
    fn match_copy(&self, t: &str) -> Option<Command> {
        let lead = COPY_LEAD.replace(t, "");
        let t = COPY_TAIL.replace(&lead, "").trim().to_string();
        if COPY_ALL.is_match(&t) {
            return Some(Command {
                target: "all".into(),
                change: Change::default(),
                names: self.lights.iter().map(|lt| lt.name.clone()).collect(),
                source: Some(String::new()),
            });
        }
        let m = COPY_AS.captures(&t).or_else(|| COPY_TO.captures(&t))?;
        let target = m.name("target").map(|g| g.as_str()).unwrap_or("").to_string();
        let source_raw = m.name("source").map(|g| g.as_str()).unwrap_or("");
        let source = COPY_TAIL.replace(source_raw, "").trim().to_string();
        let targets = self.resolve_idx(&target).ok()?;
        if !source.is_empty() && !OTHERS.contains(&source.as_str()) && self.resolve_idx(&source).is_err() {
            return None;
        }
        Some(Command {
            target,
            change: Change::default(),
            names: targets.into_iter().map(|i| self.lights[i].name.clone()).collect(),
            source: Some(source),
        })
    }

    /// Do a matched command; the line to say back.
    pub async fn run(&self, cmd: &Command) -> String {
        if let Some(source) = &cmd.source {
            let src = if source.is_empty() { None } else { Some(source.as_str()) };
            let out = self.copy(&cmd.target, src).await;
            for k in ["line", "reason"] {
                if let Some(v) = out.get(k).filter(|v| py::truthy(v)) {
                    return py::str_of(v);
                }
            }
            return "No lights changed.".into();
        }
        let idx: Vec<usize> = (0..self.lights.len()).filter(|&i| cmd.names.contains(&self.lights[i].name)).collect();
        let results = self.all(&idx, &cmd.change).await;
        let ok = results.iter().filter(|r| r.ok).count();
        info!("lights: by code, {} -> {} done, {} failed", describe(&cmd.change), ok, results.len() - ok);
        let lights: Vec<&Light> = idx.iter().map(|&i| &self.lights[i]).collect();
        self.line(&lights, &results, &cmd.change)
    }

    /// "Lights blue." / "Floor lamp off. strip: out of range.": what changed, and each light that did not.
    pub fn line(&self, lights: &[&Light], results: &[Outcome], change: &Change) -> String {
        let done: Vec<&str> = results.iter().filter(|r| r.ok).map(|r| r.name.as_str()).collect();
        let failed: Vec<&Outcome> = results.iter().filter(|r| !r.ok).collect();
        let everyone = lights.len() == self.lights.len() && lights.len() > 1;
        let mut parts = Vec::new();
        if !done.is_empty() {
            let who = if everyone && failed.is_empty() { "Lights".to_string() } else { join_names(&done) };
            parts.push(format!("{who} {}.", describe(change)));
        }
        for f in failed {
            parts.push(format!("{}: {}.", f.name, f.reason));
        }
        if parts.is_empty() {
            "No lights changed.".into()
        } else {
            parts.join(" ")
        }
    }

    /// The Mini App's lights card (miniapp /api/lights): `{"action": "status"}` reads every light;
    /// `{"action": "set", "target", "power", "brightness", "color"}` changes them and says what happened.
    pub async fn panel(&self, body: &Value) -> Value {
        if body.get("action") == Some(&json!("status")) {
            return self.status("all").await;
        }
        let get = |k: &str| body.get(k).cloned().unwrap_or(Value::Null);
        let target = get("target");
        let target = if py::truthy(&target) { py::str_of(&target) } else { "all".into() };
        let idx = match self.resolve_idx(&target) {
            Ok(idx) => idx,
            Err(why) => return json!({"ok": false, "line": why}),
        };
        let raw = get("color");
        let mut color = None;
        if py::truthy(&raw) {
            color = parse_color(&py::str_of(&raw));
            if color.is_none() {
                return json!({"ok": false, "line": format!("Unknown colour {}.", py::repr(&raw))});
            }
        }
        let bright = get("brightness");
        let change = Change {
            power: get("power").as_bool(),
            brightness: if bright.is_number() { py::int_of(&bright).ok() } else { None },
            color,
        };
        if change.is_empty() {
            return json!({"ok": false, "line": "Nothing to change."});
        }
        let results = self.all(&idx, &change).await;
        let ok = results.iter().filter(|r| r.ok).count();
        info!("lights: from the Mini App, {} -> {} done, {} failed", describe(&change), ok, results.len() - ok);
        let lights: Vec<&Light> = idx.iter().map(|&i| &self.lights[i]).collect();
        json!({"ok": ok > 0, "line": self.line(&lights, &results, &change),
               "failed": results.iter().filter(|r| !r.ok).map(|r| r.name.clone()).collect::<Vec<_>>()})
    }

    /// The lights' names and rooms, for the Mini App's card (no addresses, no keys).
    pub fn listing(&self) -> Vec<Value> {
        self.lights
            .iter()
            .map(|lt| {
                let mut o = Map::new();
                o.insert("name".into(), json!(lt.name));
                if !lt.room.is_empty() {
                    o.insert("room".into(), json!(lt.room));
                }
                Value::Object(o)
            })
            .collect()
    }
}

fn failed_list(results: &[Outcome]) -> Vec<Value> {
    results.iter().filter(|r| !r.ok).map(|r| json!({"name": r.name, "reason": r.reason})).collect()
}

pub fn describe(change: &Change) -> String {
    let c = change.normalised();
    if c.power == Some(false) {
        return "off".into();
    }
    let mut bits = Vec::new();
    if let Some(color) = &c.color {
        let mut word = if color.word.is_empty() { "that colour".to_string() } else { color.word.clone() };
        if let (true, Some(rgb)) = (word.starts_with('#'), color.rgb) {
            word = format!("{} ({word})", color_word(&rgb)); // a picked colour: "cyan (#19ffff)" says what it is
        }
        bits.push(word);
    }
    if let Some(b) = c.brightness {
        bits.push(format!("at {b}%"));
    }
    if bits.is_empty() {
        "on".into()
    } else {
        bits.join(" ")
    }
}

/// "Floor lamp and desk bulb": the names joined, the first letter capitalised. Python's `_join`.
pub fn join_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => py::capitalize_first(one),
        [head @ .., last] => py::capitalize_first(&format!("{} and {last}", head.join(", "))),
    }
}

// ---- construction -----------------------------------------------------------------------------------------------

/// `EVE_LIGHTS` from this environment (None: the process's).
pub fn enabled(environ: Option<&HashMap<String, String>>) -> bool {
    let v = match environ {
        Some(env) => env.get("EVE_LIGHTS").cloned().unwrap_or_default(),
        None => std::env::var("EVE_LIGHTS").unwrap_or_default(),
    };
    !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off")
}

/// The owner's Lights, or None when switched off or there are none (then no tool is lent).
pub fn make_lights(path: Option<&Path>, environ: Option<&HashMap<String, String>>) -> Option<Lights> {
    if !enabled(environ) {
        info!("lights: off (EVE_LIGHTS)");
        return None;
    }
    let found = load(path);
    if found.is_empty() {
        info!("lights: none configured (eve-bridge lights scan)");
        return None;
    }
    let mut kinds: Vec<&str> = found.iter().map(|lt| lt.kind.as_str()).collect();
    kinds.sort();
    kinds.dedup();
    info!("lights: {} ({})", found.len(), kinds.join(", "));
    Some(Lights::new(found))
}
