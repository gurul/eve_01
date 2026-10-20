//! The OBSBOT webcam's standard USB video (UVC) controls, straight from this Mac: pan, tilt, a continuous turn,
//! zoom, and waking it. No OBSBOT Center (docs/obsbot.md, "Direct USB"). Port of `obsbot_usb.py`.
//!
//! Owner, 2026-10-11, after OBSBOT Center crashed on an OSC sleep, left an orphaned helper holding its OSC port,
//! showed "already running" and turned its own OSC setting off: "can't u just interface with [the camera]
//! directly". The camera's pan/tilt and zoom are UVC camera-terminal controls, so they need no OBSBOT protocol. The
//! Mac's UVC driver owns the camera and libusb is refused, but IOKit's DeviceRequest reaches the default pipe, and
//! the Tiny 3 answers. That is `native/obsbot_uvc.c`, built with clang on first use into BIN_DIR (keyed by the
//! source's hash, so an edit rebuilds it, and the Python and Rust daemons share one binary). One call is one
//! process, ~7 ms.
//!
//! Measured on a Tiny 3, 2026-10-11:
//!
//! * Ranges: pan ±130°, tilt ±90°, zoom 0-100. Absolute pan + turns the view left (checked on the picture), tilt +
//!   is up. Absolute moves land within ~1°, small ones too (a 20° pan took ~2 s).
//! * The continuous turn's "+1" turns pan the other way (absolute pan goes down): `turn` maps directions here so
//!   callers never see it. At the camera's top speed (80) it turns ~32°/s.
//! * Asleep, the camera takes commands and ignores them, and reads tilt -79 (parked lens-down). Opening the video
//!   wakes it: ~1 s of ffmpeg reading AVFoundation, then the gimbal unparks. After the video closed it stayed
//!   awake for at least a minute.

use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Where the helper's C source lives in the checkout (Python's `SOURCE`). The build itself uses
/// [`SOURCE_BYTES`], the same file embedded at compile time, so a binary away from the checkout still builds it.
pub const SOURCE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/native/obsbot_uvc.c");
/// The bytes of `native/obsbot_uvc.c`; their hash names the binary, exactly as Python hashes the file.
pub const SOURCE_BYTES: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/native/obsbot_uvc.c"));

/// OBSBOT (13668), as native/obsbot_uvc.c.
pub const VENDOR: u32 = 0x3564;
/// Below this the camera is asleep, lens parked down (it reads -79).
pub const PARKED_TILT: f64 = -70.0;
/// Moves stay above the parked tilt, so asleep and pointed-down never mix.
pub const TILT_LIMIT: f64 = 59.0;
/// The continuous turn: half the Tiny 3's top speed, ~16°/s, to stop on target.
pub const TURN_SPEED: i64 = 40;
pub const WAKE_SECS: f64 = 6.0;
pub const NO_CLANG_LINE: &str =
    "eve needs the Xcode command line tools to reach the camera directly (xcode-select --install).";
pub const NO_CAMERA_LINE: &str = "No OBSBOT camera is plugged into this Mac.";
const BUILD_FAILED_LINE: &str = "eve couldn't build its camera helper (clang failed); see the daemon log.";

/// A helper reply: the JSON object the helper printed (`ok`, `name`, `pan`, `tilt`, `zoom`, ranges, ...).
pub type Reply = Map<String, Value>;

/// `~/.config/eve-bridge/bin` (Python's `BIN_DIR`).
pub fn bin_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("~")).join(".config/eve-bridge/bin")
}

// ---- the subprocess seam --------------------------------------------------------------------------------------

/// What a finished run left: the `CompletedProcess` fields this module reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutput {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The seam Python's `runner=subprocess.run` parameter is: argv and a timeout; stdin is always closed and output
/// captured as text. A timeout is an `io::Error` of kind `TimedOut` (Python's `TimeoutExpired`).
pub trait Runner: Send + Sync {
    fn run(&self, argv: &[String], timeout: Duration) -> io::Result<RunOutput>;
}

impl<F> Runner for F
where
    F: Fn(&[String], Duration) -> io::Result<RunOutput> + Send + Sync,
{
    fn run(&self, argv: &[String], timeout: Duration) -> io::Result<RunOutput> {
        self(argv, timeout)
    }
}

/// `subprocess.run(argv, capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=...)`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SubprocessRunner;

impl Runner for SubprocessRunner {
    fn run(&self, argv: &[String], timeout: Duration) -> io::Result<RunOutput> {
        let (program, args) = argv.split_first().ok_or_else(|| io::Error::other("empty argv"))?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let out_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = stdout.as_mut() {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });
        let err_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = stderr.as_mut() {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("Command {:?} timed out after {} seconds", argv, timeout.as_secs_f64()),
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let stdout = out_reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();
        Ok(RunOutput {
            returncode: status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

/// The seam for `time.monotonic` and `time.sleep`, which the tests patch.
pub trait Clock: Send + Sync {
    fn monotonic(&self) -> f64;
    fn sleep(&self, secs: f64);
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

impl Clock for SystemClock {
    fn monotonic(&self) -> f64 {
        EPOCH.elapsed().as_secs_f64()
    }

    fn sleep(&self, secs: f64) {
        if secs > 0.0 {
            std::thread::sleep(Duration::from_secs_f64(secs));
        }
    }
}

// ---- the helper -----------------------------------------------------------------------------------------------

fn digest() -> String {
    let hex = hex::encode(Sha256::digest(SOURCE_BYTES));
    hex[..10].to_string()
}

/// The helper binary for this source, in `~/.config/eve-bridge/bin`.
pub fn helper_path() -> PathBuf {
    helper_path_in(&bin_dir())
}

/// The helper binary for this source in `bin_dir` (Python's `helper_path` with `BIN_DIR` patched).
pub fn helper_path_in(bin_dir: &Path) -> PathBuf {
    bin_dir.join(format!("obsbot-uvc-{}", digest()))
}

/// `shutil.which("clang")`, else `/usr/bin/clang` when it exists.
pub fn find_clang() -> Option<PathBuf> {
    which::which("clang").ok().or_else(|| {
        let p = PathBuf::from("/usr/bin/clang");
        p.exists().then_some(p)
    })
}

/// The helper, built if this source has no binary yet. `(Some(path), "")` or `(None, the reason)`.
pub fn build(runner: &dyn Runner) -> (Option<PathBuf>, String) {
    let dir = bin_dir();
    let path = helper_path_in(&dir);
    if path.exists() {
        return (Some(path), String::new());
    }
    build_in(&dir, find_clang().as_deref(), runner)
}

/// [`build`] against an explicit bin directory and clang (`None`: no clang on this Mac).
pub fn build_in(bin_dir: &Path, clang: Option<&Path>, runner: &dyn Runner) -> (Option<PathBuf>, String) {
    let path = helper_path_in(bin_dir);
    if path.exists() {
        return (Some(path), String::new());
    }
    let Some(clang) = clang else {
        return (None, NO_CLANG_LINE.to_string());
    };
    if let Err(e) = std::fs::create_dir_all(bin_dir) {
        tracing::warn!("obsbot: the USB helper didn't build: {e}");
        return (None, BUILD_FAILED_LINE.to_string());
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = bin_dir.join(format!("{name}.tmp"));
    // clang needs the source as a file: the embedded bytes, written beside the build under a name that the
    // "obsbot-uvc-*" sweep (here and in Python) never matches, and removed once clang is done.
    let src = bin_dir.join(format!(".obsbot_uvc-{}.c", digest()));
    if let Err(e) = std::fs::write(&src, SOURCE_BYTES) {
        tracing::warn!("obsbot: the USB helper didn't build: {e}");
        return (None, BUILD_FAILED_LINE.to_string());
    }
    let argv: Vec<String> = vec![
        clang.to_string_lossy().into_owned(),
        "-O2".into(),
        "-framework".into(),
        "IOKit".into(),
        "-framework".into(),
        "CoreFoundation".into(),
        "-o".into(),
        tmp.to_string_lossy().into_owned(),
        src.to_string_lossy().into_owned(),
    ];
    let out = runner.run(&argv, Duration::from_secs(120));
    let _ = std::fs::remove_file(&src);
    let built = match &out {
        Ok(o) => o.returncode == 0 && tmp.exists(),
        Err(_) => false,
    };
    if !built {
        let stderr = match out {
            Ok(o) => o.stderr,
            Err(e) => e.to_string(),
        };
        tracing::warn!("obsbot: the USB helper didn't build: {}", last_chars(&stderr, 300));
        return (None, BUILD_FAILED_LINE.to_string());
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        tracing::warn!("obsbot: the USB helper didn't build: {e}");
        return (None, BUILD_FAILED_LINE.to_string());
    }
    if let Ok(entries) = std::fs::read_dir(bin_dir) {
        for entry in entries.flatten() {
            let old = entry.path();
            let is_build = entry.file_name().to_string_lossy().starts_with("obsbot-uvc-");
            if is_build && old != path {
                // earlier builds of an older source
                let _ = std::fs::remove_file(&old);
            }
        }
    }
    tracing::info!("obsbot: built the USB helper {}", name);
    (Some(path), String::new())
}

/// Python's `s[-n:]`, by characters.
fn last_chars(s: &str, n: usize) -> &str {
    let count = s.chars().count();
    if count <= n {
        return s;
    }
    let skip = count - n;
    let idx = s.char_indices().nth(skip).map(|(i, _)| i).unwrap_or(s.len());
    &s[idx..]
}

/// An OBSBOT USB device is plugged in (ioreg, ~50 ms; no helper, no clang).
pub fn present(runner: &dyn Runner) -> bool {
    let argv: Vec<String> = ["ioreg", "-p", "IOUSB", "-l", "-w0"].iter().map(|s| s.to_string()).collect();
    match runner.run(&argv, Duration::from_secs(5)) {
        Ok(out) => out.stdout.contains(&format!("\"idVendor\" = {VENDOR}")),
        Err(_) => false,
    }
}

// ---- Python string helpers ------------------------------------------------------------------------------------

fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.splitlines()`: every line boundary Python knows, the ends dropped.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if boundary {
            lines.push(&s[start..i]);
            let mut next = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = chars.peek() {
                    chars.next();
                    next = j + 1;
                }
            }
            start = next;
        }
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str(value)` for a JSON value that came from `json.loads`.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            if n.is_f64() {
                py_float_repr(n.as_f64().unwrap_or(0.0))
            } else {
                n.to_string()
            }
        }
        other => other.to_string(),
    }
}

fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let s = format!("{f:?}");
    if s.contains('e') {
        // Rust: 1e20 / 1.5e-7; Python: 1e+20 / 1.5e-07.
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let (sign, digits) = match exp.strip_prefix('-') {
            Some(d) => ("-", d),
            None => ("+", exp),
        };
        let mant = mant.strip_suffix(".0").unwrap_or(mant);
        return format!("{mant}e{sign}{digits:0>2}");
    }
    s
}

/// `float(x)` on a JSON value; `None` where Python would raise.
fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => s.trim_matches(py_space).parse::<f64>().ok(),
        _ => None,
    }
}

/// `f"{x:.2f}"`.
fn fmt2(x: f64) -> String {
    if x.is_nan() {
        "nan".into()
    } else {
        format!("{x:.2}")
    }
}

/// The Python exception name `type(e).__name__` gives for a runner error.
fn io_error_name(e: &io::Error) -> &'static str {
    match e.kind() {
        io::ErrorKind::TimedOut => "TimeoutExpired",
        io::ErrorKind::NotFound => "FileNotFoundError",
        io::ErrorKind::PermissionDenied => "PermissionError",
        _ => "OSError",
    }
}

fn reply(pairs: &[(&str, Value)]) -> Reply {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

// ---- the camera -----------------------------------------------------------------------------------------------

/// A continuous turn's direction, or stop. Parsing an unknown name fails, where Python's lookup raises KeyError.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
    Stop,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Up => "up",
            Direction::Down => "down",
            Direction::Stop => "stop",
        }
    }

    /// The helper's relative (pan, tilt): the camera's "+1" pans the other way, so left is -1.
    fn axes(self) -> (i32, i32) {
        match self {
            Direction::Left => (-1, 0),
            Direction::Right => (1, 0),
            Direction::Up => (0, 1),
            Direction::Down => (0, -1),
            Direction::Stop => (0, 0),
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A direction that isn't "left", "right", "up", "down" or "stop" (Python's KeyError).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("'{0}'")]
pub struct UnknownDirection(pub String);

impl FromStr for Direction {
    type Err = UnknownDirection;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "left" => Ok(Direction::Left),
            "right" => Ok(Direction::Right),
            "up" => Ok(Direction::Up),
            "down" => Ok(Direction::Down),
            "stop" => Ok(Direction::Stop),
            other => Err(UnknownDirection(other.to_string())),
        }
    }
}

/// `UsbCamera`'s surface, so the panel (`obsbot`) can take a fake in tests as Python does by duck typing.
pub trait UsbControl: Send {
    fn read(&mut self) -> Option<Reply>;
    fn wake(&mut self) -> bool;
    fn point(&mut self, pan: f64, tilt: f64) -> bool;
    fn turn(&mut self, direction: Direction, speed: i64) -> bool;
    fn zoom(&mut self, level: i64) -> bool;
    /// Why the last call failed, for the owner ("" when nothing is known).
    fn why(&self) -> &str;
}

type Waker = Box<dyn Fn() + Send + Sync>;

/// The camera over USB. Every method is quick and blocking (the panel runs them in a thread).
pub struct UsbCamera {
    run: Box<dyn Runner>,
    waker: Waker,
    helper: Option<PathBuf>,
    clock: Box<dyn Clock>,
    bin_dir: Option<PathBuf>,
    pub why: String,
}

impl Default for UsbCamera {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for UsbCamera {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsbCamera").field("helper", &self.helper).field("why", &self.why).finish()
    }
}

impl UsbCamera {
    /// `UsbCamera()`: real subprocesses, the ffmpeg waker, the helper built on first use.
    pub fn new() -> Self {
        Self {
            run: Box::new(SubprocessRunner),
            waker: Box::new(open_video),
            helper: None,
            clock: Box::new(SystemClock),
            bin_dir: None,
            why: String::new(),
        }
    }

    pub fn with_runner(mut self, runner: impl Runner + 'static) -> Self {
        self.run = Box::new(runner);
        self
    }

    pub fn with_waker(mut self, waker: impl Fn() + Send + Sync + 'static) -> Self {
        self.waker = Box::new(waker);
        self
    }

    /// A helper binary already at hand (no build).
    pub fn with_helper(mut self, helper: impl Into<PathBuf>) -> Self {
        self.helper = Some(helper.into());
        self
    }

    pub fn with_clock(mut self, clock: impl Clock + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    /// Build the helper into this directory instead of `~/.config/eve-bridge/bin`.
    pub fn with_bin_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.bin_dir = Some(dir.into());
        self
    }

    fn call(&mut self, args: &[String]) -> Reply {
        let helper = match &self.helper {
            Some(h) => h.clone(),
            None => {
                let (path, why) = match &self.bin_dir {
                    Some(dir) => build_in(dir, find_clang().as_deref(), self.run.as_ref()),
                    None => build(self.run.as_ref()),
                };
                match path {
                    Some(p) => {
                        self.helper = Some(p.clone());
                        p
                    }
                    None => {
                        self.why = why.clone();
                        return reply(&[("ok", Value::Bool(false)), ("error", Value::String(why))]);
                    }
                }
            }
        };
        let mut argv = vec![helper.to_string_lossy().into_owned()];
        argv.extend(args.iter().cloned());
        let failed = |name: &str| {
            reply(&[
                ("ok", Value::Bool(false)),
                ("error", Value::String(format!("the camera helper failed ({name})"))),
            ])
        };
        let out = match self.run.run(&argv, Duration::from_secs(5)) {
            Ok(out) => out,
            Err(e) => return failed(io_error_name(&e)),
        };
        let stripped = out.stdout.trim_matches(py_space);
        let Some(last) = py_splitlines(stripped).last().copied() else {
            return failed("IndexError");
        };
        let value: Value = match serde_json::from_str(last) {
            Ok(v) => v,
            Err(_) => return failed("JSONDecodeError"),
        };
        let Value::Object(reply) = value else {
            return failed("AttributeError");
        };
        if !truthy(reply.get("ok")) {
            let err = reply.get("error");
            self.why = if err.and_then(Value::as_str) == Some("no camera") {
                NO_CAMERA_LINE.to_string()
            } else if truthy(err) {
                err.map(py_str).unwrap_or_default()
            } else {
                String::new()
            };
        }
        reply
    }

    /// pan, tilt (degrees, tilt + up), zoom, their ranges, and asleep; None when the camera can't be read.
    pub fn read(&mut self) -> Option<Reply> {
        let mut r = self.call(&["get".to_string()]);
        if !truthy(r.get("ok")) {
            return None;
        }
        let tilt = r.get("tilt").and_then(py_float)?;
        r.insert("asleep".into(), Value::Bool(tilt < PARKED_TILT));
        Some(r)
    }

    /// Open the video for a moment; true once the gimbal has left its parked tilt.
    pub fn wake(&mut self) -> bool {
        let Some(st) = self.read() else {
            return false;
        };
        if !asleep(&st) {
            return true;
        }
        (self.waker)();
        let end = self.clock.monotonic() + WAKE_SECS;
        while self.clock.monotonic() < end {
            self.clock.sleep(0.3);
            if let Some(st) = self.read() {
                if !asleep(&st) {
                    return true;
                }
            }
        }
        false
    }

    pub fn point(&mut self, pan: f64, tilt: f64) -> bool {
        truthy(self.call(&["pantilt".into(), fmt2(pan), fmt2(tilt)]).get("ok"))
    }

    /// Start a continuous turn (left, right, up, down), or stop with `Direction::Stop`.
    pub fn turn(&mut self, direction: Direction, speed: i64) -> bool {
        let (pan, tilt) = direction.axes();
        truthy(self.call(&["turn".into(), pan.to_string(), tilt.to_string(), speed.to_string()]).get("ok"))
    }

    pub fn zoom(&mut self, level: i64) -> bool {
        truthy(self.call(&["zoom".into(), level.to_string()]).get("ok"))
    }
}

fn asleep(st: &Reply) -> bool {
    st.get("asleep").and_then(Value::as_bool).unwrap_or(false)
}

impl UsbControl for UsbCamera {
    fn read(&mut self) -> Option<Reply> {
        UsbCamera::read(self)
    }

    fn wake(&mut self) -> bool {
        UsbCamera::wake(self)
    }

    fn point(&mut self, pan: f64, tilt: f64) -> bool {
        UsbCamera::point(self, pan, tilt)
    }

    fn turn(&mut self, direction: Direction, speed: i64) -> bool {
        UsbCamera::turn(self, direction, speed)
    }

    fn zoom(&mut self, level: i64) -> bool {
        UsbCamera::zoom(self, level)
    }

    fn why(&self) -> &str {
        &self.why
    }
}

// ---- waking by opening the video ------------------------------------------------------------------------------

fn find_ffmpeg() -> Option<PathBuf> {
    which::which("ffmpeg").ok().or_else(|| {
        ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"].iter().map(PathBuf::from).find(|p| p.exists())
    })
}

/// Read about a second of the camera's video: that wakes a sleeping OBSBOT.
pub fn open_video() {
    let Some(ffmpeg) = find_ffmpeg() else {
        return;
    };
    let ffmpeg = ffmpeg.to_string_lossy().into_owned();
    let device = format!("{}:none", video_device(&ffmpeg));
    let argv: Vec<String> = [
        ffmpeg.as_str(),
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "avfoundation",
        "-pixel_format",
        "nv12",
        "-framerate",
        "30",
        "-video_size",
        "1280x720",
        "-i",
        device.as_str(),
        "-frames:v",
        "30",
        "-f",
        "null",
        "-",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let _ = SubprocessRunner.run(&argv, Duration::from_secs(10));
}

/// The OBSBOT camera's AVFoundation index, by name (the list's order changes); "0" when it isn't named.
pub fn video_device(ffmpeg: &str) -> String {
    let argv: Vec<String> = [ffmpeg, "-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""]
        .iter()
        .map(|s| s.to_string())
        .collect();
    match SubprocessRunner.run(&argv, Duration::from_secs(10)) {
        Ok(out) => video_device_in(&out.stderr),
        Err(_) => "0".into(),
    }
}

static DEVICE_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(\d+)\] .*OBSBOT").expect("valid regex"));

/// The OBSBOT's index in ffmpeg's `-list_devices` output (video devices only); "0" when it isn't named.
pub fn video_device_in(listing: &str) -> String {
    for line in py_splitlines(listing) {
        if line.contains("audio devices") {
            break;
        }
        if let Some(m) = DEVICE_LINE.captures(line) {
            return m[1].to_string();
        }
    }
    "0".into()
}

#[cfg(test)]
mod tests;
