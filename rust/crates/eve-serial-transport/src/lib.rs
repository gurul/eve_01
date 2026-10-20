//! USB CDC serial transport — drop-in replacement for EveBLE.
//!
//! Port of `eve_bridge/serial_transport.py`.
//!
//! The claude-desktop-buddy firmware feeds the same newline-delimited JSON
//! parser from USB serial as from BLE NUS (see firmware data.h), so the only
//! difference is the pipe. On the ESP32-S3's native USB-Serial/JTAG port,
//! opening the port does NOT reset the sketch, but the host must assert DTR
//! or the firmware's HWCDC TX is silently dropped.
//!
//! Duck-types EveBLE's surface used by Daemon: `connected`,
//! `wait_connected()`, `send(obj, codec)`, `run()`, `stop()`.
//!
//! Threading model: `run()`'s reader is the SOLE owner of the port handle's
//! close. Event-loop-side paths (force_reconnect, pulse_reset, send-failure,
//! stop) never close the fd — closing it under the reader's blocked read()
//! raced (paired EBADF on every forced reconnect, and an fd-reuse hazard).
//! They set a drop reason and cancel_read() instead; the reader notices,
//! breaks, and closes on its way out.
//!
//! Nothing that can block runs on the async runtime: reads, writes and the
//! port enumeration (`resolve_port`, whose USB listing takes 2-3 s while the
//! board is off the bus) all run on blocking threads, because the runtime is
//! shared with every other door (Telegram, the voice, the web page).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use eve_protocol::{encode, LookupError};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{Map, Value};
use tokio::sync::watch;
use tracing::{debug, info, warn};

#[cfg(unix)]
mod posix;
mod pystr;

#[cfg(unix)]
pub use posix::PosixSerial;
use pystr::{py_repr, py_strip, take_chars};

/// The future a handler returns.
pub type HandlerFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
/// `IncomingHandler`: one JSON object off the wire.
pub type IncomingHandler = Arc<dyn Fn(Map<String, Value>) -> HandlerFuture + Send + Sync>;
/// The `on_boot` hook.
pub type BootHook = Arc<dyn Fn() -> HandlerFuture + Send + Sync>;

pub const RECONNECT_SECS: f64 = 3.0;
/// After an ack-loss forced reconnect, hold the port CLOSED this long before
/// reopening. Field evidence: the only things that ever revived the deaf-board
/// wedge short of a reflash were daemon restarts — i.e. tens of seconds of
/// port-closed time — while 3s in-process reopens never did.
pub const FORCED_HOLDOFF_SECS: f64 = 15.0;
/// A board that is enumerated-but-silent (fresh opens, zero bytes) this many
/// times within the window gets an RTS hardware reset on the next open. The
/// silent-board crash loops of 2026-08-05 ran 20+ minutes of reopen loops that
/// could never help; the RTS pulse reboots the chip and ends it in ~3 minutes.
pub const RX_TRIP_PULSE_COUNT: usize = 3;
pub const RX_TRIP_WINDOW_SECS: f64 = 600.0;
/// ESP32-S3 native USB-Serial/JTAG. (TinyUSB CDC builds present 303a:0002 —
/// this firmware uses the built-in peripheral.)
pub const ESP32S3_VID: u16 = 0x303A;

/// Board output worth surfacing at WARNING: ESP-IDF error lines, panic dumps,
/// watchdog trips, the diag ring's died-in-phase line, and the ROM banner that
/// means it just reset underneath us.
pub static IS_CRASH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)watchdog|Backtrace|Guru Meditation|abort\(\)|assert failed|DIED IN|StoreProhibited|LoadProhibited|rst:0x|E \(\d+\)|CORRUPT HEAP|stack overflow",
    )
    .expect("IS_CRASH regex")
});

/// The board rebooted underneath an unbroken link. A CH340-bridged board reset
/// does NOT re-enumerate USB, so no reconnect fires and the on-connect resync
/// (time + forced heartbeat) never runs — the reborn board sits at "--:--" and
/// "No Claude connected" until state happens to change. The ROM banner and the
/// firmware's own boot line are the tell; the daemon hooks them via `on_boot`.
pub static IS_BOOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"rst:0x|^\[boot\] ").expect("IS_BOOT regex"));

/// What the board says when a host motion is admitted, reduced, refused or
/// stopped (body.cpp / main.cpp). Promoted to INFO: see `dispatch_line`.
pub static IS_MOTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[body\] motion|^\[gaze\] host look").expect("IS_MOTION regex"));

/// Treat the link as dead after this long with no bytes read at all.
///
/// A USB re-enumeration — which every board reset causes, including the one
/// esptool triggers on flash — leaves the host holding a file descriptor that
/// no longer reaches the device. Writes succeed into the void, reads return
/// empty, and nothing ever raises, so `connected` stays True forever: the
/// daemon looks healthy, the log is clean, and the board sits there showing
/// "No Claude connected". Only an exception used to trigger reconnect, and a
/// stale fd never produces one.
///
/// The firmware prints "[alive] ..." every 5s unconditionally WHILE loop()
/// runs (main.cpp) — so silence this long means either a stale fd or a
/// hung/crashed board (the USB peripheral is autonomous silicon and keeps
/// enumerating while the CPU is stuck). Keep in sync with the firmware
/// interval.
pub const RX_SILENCE_SECS: f64 = 20.0;

/// Longest NDJSON line we will assemble before declaring the stream garbage.
/// Camera frames are the big ones: 160x120 gray is 19.2 KB raw → 25.6 KB of
/// base64 in one line; JPEG frames are a few KB. 256 KB leaves room for larger
/// frame sizes without letting a newline-less firmware spew grow the buffer
/// without bound.
pub const MAX_LINE_BYTES: usize = 256 * 1024;

pub const USB_SERIAL_PREFIX: &str = "usbsn:";

/// A port error, as the transport tells them apart: pyserial's
/// `SerialTimeoutException` (a write that found no room within
/// `write_timeout`) against everything else.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SerialError {
    #[error("Write timeout")]
    WriteTimeout,
    #[error("{0}")]
    Serial(String),
}

/// The part of pyserial's `serial.Serial` the transport uses. Every method
/// takes `&self`: the reader thread reads while a writer thread writes and the
/// runtime pulses modem lines, as with pyserial.
pub trait SerialHandle: Send + Sync + 'static {
    /// `read(size)`: block up to the port timeout; empty on timeout or after
    /// `cancel_read`.
    fn read(&self, size: usize) -> Result<Vec<u8>, SerialError>;
    /// `in_waiting`.
    fn in_waiting(&self) -> Result<usize, SerialError>;
    /// `write(data)`, bounded by the port's write timeout.
    fn write(&self, data: &[u8]) -> Result<usize, SerialError>;
    /// `is_open`.
    fn is_open(&self) -> bool;
    /// `cancel_read()`: unblock a reader at once.
    fn cancel_read(&self) -> Result<(), SerialError>;
    /// `close()`.
    fn close(&self) -> Result<(), SerialError>;
    /// `ser.dtr = on`.
    fn set_dtr(&self, on: bool) -> Result<(), SerialError>;
    /// `ser.rts = on`.
    fn set_rts(&self, on: bool) -> Result<(), SerialError>;
    /// `reset_input_buffer()`.
    fn reset_input_buffer(&self) -> Result<(), SerialError>;
    /// `ser.port`: the device path, when there is one.
    fn port(&self) -> Option<String>;
}

/// `serial.Serial(port, baud, timeout=1, write_timeout=2, exclusive=True)`.
pub type PortOpener = Arc<dyn Fn(&str, u32) -> Result<Arc<dyn SerialHandle>, SerialError> + Send + Sync>;
/// `_resolve_port(pattern, skip_serials)`.
pub type PortResolver = Arc<dyn Fn(&str, &BTreeSet<String>) -> Option<String> + Send + Sync>;
/// `time.monotonic()`.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
/// `time.sleep(secs)` for the RTS pulse (blocking).
pub type Sleeper = Arc<dyn Fn(f64) + Send + Sync>;

/// One entry of `serial.tools.list_ports.comports()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    pub device: String,
    pub vid: Option<u16>,
    pub serial_number: Option<String>,
}

/// One blocking read of whatever the board has sent. Runs on a blocking thread.
///
/// Not a byte-at-a-time readline: that pulled ONE byte per read() — a select()
/// plus a syscall each — so a 25 KB camera-frame line cost tens of thousands
/// of round trips and the reader fell behind at 5 fps. Block up to the port
/// timeout for the first byte, then drain what is already waiting. Returns
/// empty on the timeout (or a cancel_read), exactly like readline did.
pub fn read_chunk(ser: &dyn SerialHandle) -> Result<Vec<u8>, SerialError> {
    let mut first = ser.read(1)?;
    if first.is_empty() {
        return Ok(first);
    }
    let waiting = ser.in_waiting()?;
    if waiting > 0 {
        first.extend(ser.read(waiting)?);
    }
    Ok(first)
}

/// `EVE_SERIAL_SKIP`: USB serial numbers, comma-separated, that the robot's
/// glob never opens.
///
/// Other ESP32-S3 boards share the robot's `/dev/cu.usbmodem*` glob. On
/// 2026-09-30, with the StackChan unplugged, the daemon opened another board
/// on its charging cable as the robot, sent it status polls and RTS-reset it
/// every two minutes. A board named here is left alone.
///
/// `environ` is the environment to read; `None` reads the process's.
pub fn serial_skip(environ: Option<&HashMap<String, String>>) -> BTreeSet<String> {
    let raw = match environ {
        Some(env) => env.get("EVE_SERIAL_SKIP").cloned().unwrap_or_default(),
        None => std::env::var_os("EVE_SERIAL_SKIP")
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    raw.split(',')
        .map(py_strip)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// `sorted(glob.glob(pattern))`. Like Python's glob, a wildcard never matches
/// a leading dot.
pub fn glob_ports(pattern: &str) -> Vec<String> {
    let options = glob::MatchOptions { require_literal_leading_dot: true, ..glob::MatchOptions::new() };
    let mut hits: Vec<String> = match glob::glob_with(pattern, options) {
        Ok(paths) => paths
            .filter_map(Result::ok)
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    hits.sort();
    hits
}

/// `serial.tools.list_ports.comports()`. On macOS pyserial lists the callout
/// (`/dev/cu.*`) node of each port only, so the dial-in twin is dropped here.
pub fn comports() -> Result<Vec<PortInfo>, String> {
    let ports = serialport::available_ports().map_err(|e| e.to_string())?;
    Ok(ports
        .into_iter()
        .filter(|p| !cfg!(target_os = "macos") || p.port_name.starts_with("/dev/cu."))
        .map(|p| {
            let (vid, serial_number) = match p.port_type {
                serialport::SerialPortType::UsbPort(usb) => (Some(usb.vid), usb.serial_number),
                _ => (None, None),
            };
            PortInfo { device: p.port_name, vid, serial_number }
        })
        .collect())
}

/// `_resolve_port` against the real glob and USB listing.
pub fn resolve_port(pattern: &str, skip_serials: &BTreeSet<String>) -> Option<String> {
    resolve_port_with(pattern, skip_serials, &glob_ports, &comports)
}

fn lower_serial(s: &Option<String>) -> String {
    s.as_deref().unwrap_or("").to_lowercase()
}

/// Expand a glob like /dev/cu.usbmodem* to a concrete port (macOS
/// re-enumerates the number when the board changes USB sockets). With
/// multiple matches, prefer the node whose USB VID is the ESP32-S3's, and say
/// so — silently picking sorted()[0] grabbed whatever enumerated first.
///
/// `usbsn:<serial>` names a board by its USB serial number (an ESP32-S3's is
/// its MAC) instead: with two ESP32-S3 boards on the bus — the StackChan and
/// the Voice PE controller (controller.py) — the VID alone cannot tell them
/// apart. A glob skips the nodes whose serial is in `skip_serials`.
///
/// `glob_fn` is `sorted(glob.glob(..))`'s source and `list_ports` is
/// `comports()`; an `Err` from it is IOKit throwing mid-enumeration.
pub fn resolve_port_with(
    pattern: &str,
    skip_serials: &BTreeSet<String>,
    glob_fn: &dyn Fn(&str) -> Vec<String>,
    list_ports: &dyn Fn() -> Result<Vec<PortInfo>, String>,
) -> Option<String> {
    if let Some(rest) = pattern.strip_prefix(USB_SERIAL_PREFIX) {
        let want = py_strip(rest).to_lowercase();
        // IOKit can throw mid-enumeration; try again on the next open.
        return match list_ports() {
            Ok(ports) => ports
                .into_iter()
                .find(|p| lower_serial(&p.serial_number) == want && p.device.contains("/cu."))
                .map(|p| p.device),
            Err(_) => None,
        };
    }
    if !pattern.contains('*') {
        return Some(pattern.to_string());
    }
    let mut hits = glob_fn(pattern);
    hits.sort();
    if hits.is_empty() {
        return None;
    }
    let mut vids: HashMap<String, Option<u16>> = HashMap::new();
    match list_ports() {
        Ok(all) => {
            let ports: Vec<PortInfo> = all.into_iter().filter(|p| hits.contains(&p.device)).collect();
            let skip: HashSet<String> = skip_serials.iter().map(|s| s.to_lowercase()).collect();
            hits.retain(|h| !ports.iter().any(|p| &p.device == h && skip.contains(&lower_serial(&p.serial_number))));
            if hits.is_empty() {
                return None;
            }
            for p in ports {
                if hits.contains(&p.device) {
                    vids.insert(p.device, p.vid);
                }
            }
        }
        Err(_) => {
            // IOKit can throw mid-enumeration; fall back to the glob alone.
            if !skip_serials.is_empty() {
                // Without the serials there is no telling the robot from the controller
                // (Eve/PortPick.lean): wait for the next open rather than guess, which could open
                // the controller as the robot.
                return None;
            }
        }
    }
    let mut s3: Vec<&String> = vids
        .iter()
        .filter(|(_, vid)| **vid == Some(ESP32S3_VID))
        .map(|(d, _)| d)
        .collect();
    s3.sort();
    if let Some(first) = s3.first() {
        if hits.len() > 1 {
            warn!("serial: {} nodes match {} — preferring ESP32-S3 {}", hits.len(), pattern, first);
        }
        return Some((*first).clone());
    }
    // Every match names a USB vendor and none is Espressif: the board is off the bus and what is
    // left is a camera or a dock (2026-09-23: with the robot power-cycling, the daemon opened a
    // webcam's node, wrote status polls into it and waited for acks). Wait for the board instead
    // of guessing.
    if hits.iter().all(|d| matches!(vids.get(d), Some(Some(_)))) {
        return None;
    }
    if hits.len() > 1 {
        warn!("serial: {} nodes match {} — picking {}", hits.len(), pattern, hits[0]);
    }
    Some(hits[0].clone())
}

/// Runs on a blocking thread. Open the port with DTR up, RTS-pulse the fresh
/// handle when asked, and flush what the board printed before we were
/// listening. A failed open errors; a failed pulse is logged.
pub fn open_port(
    opener: &PortOpener,
    sleep: &Sleeper,
    port: &str,
    baud: u32,
    pulse: bool,
) -> Result<Arc<dyn SerialHandle>, SerialError> {
    let ser = opener(port, baud)?;
    // firmware HWCDC drops TX without DTR
    if let Err(e) = ser.set_dtr(true) {
        let _ = ser.close();
        return Err(e);
    }
    if pulse {
        let pulsed = (|| {
            ser.set_rts(true)?;
            sleep(0.1);
            ser.set_rts(false)?;
            ser.set_dtr(true)
        })();
        if let Err(e) = pulsed {
            warn!("serial: RTS pulse failed: {}", e);
        }
    }
    let _ = ser.reset_input_buffer();
    Ok(ser)
}

/// `time.monotonic()` in seconds, on the clock CPython reads: on macOS
/// `mach_absolute_time()` (`CLOCK_UPTIME_RAW`), which stands still while the
/// Mac sleeps, so a lid closed for an hour is not 3600 s of board silence
/// (CLOCK_MONOTONIC there counts the sleep, and every wake would trip the
/// RX_SILENCE_SECS watchdog and count toward the RTS reset); elsewhere
/// `CLOCK_MONOTONIC`.
pub fn monotonic() -> f64 {
    #[cfg(unix)]
    {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        let clock = libc::CLOCK_UPTIME_RAW;
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        let clock = libc::CLOCK_MONOTONIC;
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: clock_gettime fills the timespec.
        unsafe { libc::clock_gettime(clock, &mut ts) };
        ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
    }
    #[cfg(not(unix))]
    {
        static START: LazyLock<std::time::Instant> = LazyLock::new(std::time::Instant::now);
        START.elapsed().as_secs_f64()
    }
}

/// `serial.Serial(...)` for real.
pub fn open_real(port: &str, baud: u32) -> Result<Arc<dyn SerialHandle>, SerialError> {
    #[cfg(unix)]
    {
        PosixSerial::open(port, baud).map(|s| Arc::new(s) as Arc<dyn SerialHandle>)
    }
    #[cfg(not(unix))]
    {
        let _ = baud;
        Err(SerialError::Serial(format!("could not open port {port}: serial ports need a POSIX host")))
    }
}

/// What the transport reaches the outside world through: the port open, the
/// port pick, the clock and the blocking sleep. `Default` is the real thing;
/// tests swap pieces (where the Python tests monkeypatch the module).
#[derive(Clone)]
pub struct Seams {
    pub open: PortOpener,
    pub resolve_port: PortResolver,
    pub monotonic: Clock,
    pub sleep: Sleeper,
}

impl Default for Seams {
    fn default() -> Self {
        Seams {
            open: Arc::new(open_real),
            resolve_port: Arc::new(resolve_port),
            monotonic: Arc::new(monotonic),
            sleep: Arc::new(|secs| std::thread::sleep(Duration::from_secs_f64(secs))),
        }
    }
}

/// The module's timing constants, per transport so tests can shrink them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    pub reconnect_secs: f64,
    pub forced_holdoff_secs: f64,
    pub rx_silence_secs: f64,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            reconnect_secs: RECONNECT_SECS,
            forced_holdoff_secs: FORCED_HOLDOFF_SECS,
            rx_silence_secs: RX_SILENCE_SECS,
        }
    }
}

#[derive(Default)]
struct LinkState {
    drop_reason: Option<String>,    // reader-owned teardown request
    reopen_holdoff: f64,            // extra close-time before next open
    rx_trips: Vec<(f64, String)>,   // (monotonic, port) silence trips
    pulse_on_open: bool,            // RTS-reset the board on next open
    last_boot_hook: f64,
}

struct Inner {
    on_message: IncomingHandler,
    on_boot: Mutex<Option<BootHook>>,
    port_pattern: String,
    baud: u32,
    skip_serials: BTreeSet<String>, // USB serials a glob must not open (resolve_port)
    seams: Seams,
    timing: Timing,
    ser: Mutex<Option<Arc<dyn SerialHandle>>>,
    connected_tx: watch::Sender<bool>,
    send_lock: Arc<tokio::sync::Mutex<()>>,
    stop_tx: watch::Sender<bool>,
    st: Mutex<LinkState>,
    tasks: Arc<AtomicUsize>, // on_message / on_boot handlers still running
}

fn same(a: &Arc<dyn SerialHandle>, b: &Arc<dyn SerialHandle>) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
}

/// Decrements the live-handler count however the handler ends.
struct TaskSlot(Arc<AtomicUsize>);

impl Drop for TaskSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `run()`'s `finally: self._close_current()`, also when the run future is
/// dropped (Python's cancellation runs the finally too).
struct CloseOnExit {
    inner: Arc<Inner>,
    armed: bool,
}

impl Drop for CloseOnExit {
    fn drop(&mut self) {
        if self.armed {
            // The reader thread may still be inside read(): kick it out first so the close does
            // not wait out the port timeout.
            if let Some(ser) = self.inner.ser.lock().clone() {
                let _ = ser.cancel_read();
            }
            self.inner.close_current();
        }
    }
}

impl Inner {
    fn now(&self) -> f64 {
        (self.seams.monotonic)()
    }

    fn stopped(&self) -> bool {
        *self.stop_tx.borrow()
    }

    /// `asyncio.sleep(secs)`, cut short by stop().
    async fn sleep(&self, secs: f64) {
        let mut rx = self.stop_tx.subscribe();
        let secs = if secs.is_finite() && secs > 0.0 { secs } else { 0.0 };
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs_f64(secs)) => {}
            _ = rx.wait_for(|s| *s) => {}
        }
    }

    fn connected(&self) -> bool {
        let ser = self.ser.lock().clone();
        match ser {
            Some(ser) => ser.is_open() && self.st.lock().drop_reason.is_none(),
            None => false,
        }
    }

    fn spawn(&self, fut: HandlerFuture) {
        self.tasks.fetch_add(1, Ordering::SeqCst);
        let slot = TaskSlot(self.tasks.clone());
        tokio::spawn(async move {
            let _slot = slot;
            fut.await;
        });
    }

    /// One complete line off the wire: JSON to on_message, anything else to
    /// the log (crash output at WARNING, see IS_CRASH).
    fn dispatch_line(&self, line: &[u8]) {
        let decoded = String::from_utf8_lossy(line);
        let text = py_strip(&decoded);
        if !text.starts_with('{') {
            if !text.is_empty() {
                // Crash output must survive at the default log level. A panic backtrace logged
                // at DEBUG is invisible exactly when it matters, and asking someone to reproduce
                // a freeze under a hand-started DEBUG daemon is a bad trade.
                // The board's answer to a motion request is the only way to know whether it
                // admitted, reduced or refused it, and a bench run that needs a DEBUG daemon to
                // see that is a bench run nobody does. So the motion echo rides at INFO while the
                // rest of the board's chatter stays at DEBUG.
                if IS_CRASH.is_match(text) {
                    warn!("stick: {}", text);
                } else if IS_MOTION.is_match(text) {
                    info!("stick: {}", text);
                } else {
                    debug!("stick: {}", text);
                }
                let hook = self.on_boot.lock().clone();
                if let Some(hook) = hook {
                    if IS_BOOT.is_match(text) {
                        let now = self.now();
                        let fire = {
                            let mut st = self.st.lock();
                            if now - st.last_boot_hook > 5.0 {
                                st.last_boot_hook = self.now();
                                true
                            } else {
                                false
                            }
                        };
                        if fire {
                            self.spawn(hook());
                        }
                    }
                }
            }
            return;
        }
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(obj)) => self.spawn((self.on_message)(obj)),
            Ok(_) | Err(_) => debug!("serial: bad json: {}", py_repr(take_chars(text, 120))),
        }
    }

    /// Close the current handle. Reader-side only (run()'s exit).
    fn close_current(&self) {
        self.connected_tx.send_replace(false);
        let ser = self.ser.lock().take();
        if let Some(ser) = ser {
            let _ = ser.close();
        }
    }

    /// Ask the reader to drop and reopen. Never closes the fd here — the
    /// reader owns the close (see the crate docs).
    fn request_drop(&self, why: &str) {
        {
            let mut st = self.st.lock();
            if st.drop_reason.is_none() {
                st.drop_reason = Some(why.to_string());
            }
        }
        self.connected_tx.send_replace(false);
        let ser = self.ser.lock().clone();
        if let Some(ser) = ser {
            let _ = ser.cancel_read(); // unblocks the reader's blocking read immediately
        }
    }

    /// Sliding-window counter of enumerated-but-silent episodes; arms the RTS
    /// pulse when the same port trips repeatedly.
    fn note_rx_trip(&self, port: &str) {
        let now = self.now();
        let mut st = self.st.lock();
        st.rx_trips.retain(|(t, _)| now - t < RX_TRIP_WINDOW_SECS);
        st.rx_trips.push((now, port.to_string()));
        if st.rx_trips.iter().filter(|(_, p)| p == port).count() >= RX_TRIP_PULSE_COUNT {
            st.pulse_on_open = true;
            st.rx_trips.clear();
        }
    }

    /// A write timeout is the deaf half of the wedge: the board has stopped
    /// draining the OUT endpoint. It counts as a trip like a silent open, so
    /// repeated ones RTS-reset the board instead of reopening it every 5 s
    /// forever (the Voice PE, 2026-10-01 to 10-03: ~35,000 reopens, every one
    /// timing out).
    fn note_deaf(&self, ser: &Arc<dyn SerialHandle>, err: &SerialError) {
        if matches!(err, SerialError::WriteTimeout) {
            if let Some(port) = ser.port().filter(|p| !p.is_empty()) {
                self.note_rx_trip(&port);
            }
        }
    }

    /// A write on `ser` failed: log it and, if `ser` is still the live handle,
    /// condemn it. Only the handle the failure belongs to — a write that
    /// raises late must not tear down a fresh reopened connection.
    fn write_failed(&self, ser: &Arc<dyn SerialHandle>, err: &SerialError) {
        warn!("serial send failed: {}", err);
        let current = self.ser.lock().clone();
        if current.is_some_and(|c| same(&c, ser)) {
            self.note_deaf(ser, err);
            self.request_drop(&format!("send failed: {err}"));
        }
    }
}

/// The USB serial link to the board.
#[derive(Clone)]
pub struct EveSerial {
    inner: Arc<Inner>,
}

impl EveSerial {
    /// A USB cable, not a radio link: the board's BLE "sec" flag says nothing
    /// about this link, so the daemon ignores it here (docs/SETUP-ISSUES.md #10).
    pub const WIRED: bool = true;

    /// `EveSerial(on_message, port, baud=115200, skip_serials=frozenset())`.
    pub fn new(on_message: IncomingHandler, port: &str, baud: u32, skip_serials: BTreeSet<String>) -> EveSerial {
        Self::with_seams(on_message, port, baud, skip_serials, Seams::default(), Timing::default())
    }

    pub fn with_seams(
        on_message: IncomingHandler,
        port: &str,
        baud: u32,
        skip_serials: BTreeSet<String>,
        seams: Seams,
        timing: Timing,
    ) -> EveSerial {
        let (connected_tx, _) = watch::channel(false);
        let (stop_tx, _) = watch::channel(false);
        EveSerial {
            inner: Arc::new(Inner {
                on_message,
                on_boot: Mutex::new(None),
                port_pattern: port.to_string(),
                baud,
                skip_serials,
                seams,
                timing,
                ser: Mutex::new(None),
                connected_tx,
                send_lock: Arc::new(tokio::sync::Mutex::new(())),
                stop_tx,
                st: Mutex::new(LinkState::default()),
                tasks: Arc::new(AtomicUsize::new(0)),
            }),
        }
    }

    /// `wired`.
    pub fn wired(&self) -> bool {
        Self::WIRED
    }

    pub fn port_pattern(&self) -> &str {
        &self.inner.port_pattern
    }

    pub fn baud(&self) -> u32 {
        self.inner.baud
    }

    pub fn skip_serials(&self) -> &BTreeSet<String> {
        &self.inner.skip_serials
    }

    /// Optional async hook fired when the board's boot banner scrolls past on
    /// an already-open link (see IS_BOOT). Debounced: a boot emits several
    /// matching lines back to back.
    pub fn set_on_boot(&self, hook: Option<BootHook>) {
        *self.inner.on_boot.lock() = hook;
    }

    /// The live handle, if any (`_ser`).
    pub fn handle(&self) -> Option<Arc<dyn SerialHandle>> {
        self.inner.ser.lock().clone()
    }

    /// Handlers spawned for incoming lines that have not finished (`len(_tasks)`).
    pub fn pending_tasks(&self) -> usize {
        self.inner.tasks.load(Ordering::SeqCst)
    }

    /// The reason the reader was asked to drop the link, if it was (`_drop_reason`).
    pub fn drop_reason(&self) -> Option<String> {
        self.inner.st.lock().drop_reason.clone()
    }

    /// Whether the next open RTS-resets the board (`_pulse_on_open`).
    pub fn pulse_on_open(&self) -> bool {
        self.inner.st.lock().pulse_on_open
    }

    /// Arm (or disarm) the RTS reset on the next open.
    pub fn set_pulse_on_open(&self, on: bool) {
        self.inner.st.lock().pulse_on_open = on;
    }

    /// Hand the transport a handle as the reader's open would: the link's
    /// current handle, with no drop pending.
    pub fn adopt(&self, ser: Arc<dyn SerialHandle>) {
        self.inner.st.lock().drop_reason = None;
        *self.inner.ser.lock() = Some(ser);
    }

    /// `connected`.
    pub fn connected(&self) -> bool {
        self.inner.connected()
    }

    /// `wait_connected()`.
    pub async fn wait_connected(&self) {
        let mut rx = self.inner.connected_tx.subscribe();
        let _ = rx.wait_for(|c| *c).await;
    }

    /// `send(obj, codec=None)`. `Ok(false)` when there is no live link or the
    /// write failed; `Err` when `codec` is unknown (Python's `LookupError`
    /// from `encode`).
    ///
    /// The send lock is held until the blocking write has RETURNED, even if
    /// the sender is cancelled meanwhile: a cancelled sender (every
    /// `timeout(send(..))` that expires) used to free the lock while its
    /// thread was still inside write, so the next sender's line went out
    /// alongside it and broke the NDJSON framing on the wire. The wait is
    /// bounded by the port's write timeout (verification/Eve/Serial.lean, A).
    /// A write that fails after its sender was cancelled is still logged and
    /// still condemns its handle.
    pub async fn send(&self, obj: &Value, codec: Option<&str>) -> Result<bool, LookupError> {
        // Capture once: the live handle can be swapped/dropped across the await
        // points below.
        let Some(ser) = self.usable_handle() else {
            return Ok(false);
        };
        let data = encode(obj, codec)?;
        let guard = self.inner.send_lock.clone().lock_owned().await;
        if !self.still_live(&ser) {
            return Ok(false);
        }
        let inner = self.inner.clone();
        // write_timeout bounds this (set at open). No flush(): that was tcdrain, which no timeout
        // covers — one board that stopped draining the OUT endpoint would block a sender forever
        // while every other sender queued on the lock.
        let job = tokio::task::spawn_blocking(move || {
            let _held = guard;
            match ser.write(&data) {
                Ok(_) => true,
                Err(e) => {
                    inner.write_failed(&ser, &e);
                    false
                }
            }
        });
        match job.await {
            Ok(ok) => Ok(ok),
            Err(e) => {
                warn!("serial send failed: {}", e);
                Ok(false)
            }
        }
    }

    fn usable_handle(&self) -> Option<Arc<dyn SerialHandle>> {
        let ser = self.inner.ser.lock().clone()?;
        (ser.is_open() && self.inner.st.lock().drop_reason.is_none()).then_some(ser)
    }

    fn still_live(&self, ser: &Arc<dyn SerialHandle>) -> bool {
        let current = self.inner.ser.lock().clone();
        current.is_some_and(|c| same(&c, ser)) && ser.is_open() && self.inner.st.lock().drop_reason.is_none()
    }

    /// `run()`: resolve, open, read and dispatch, reconnecting forever until
    /// stop().
    pub async fn run(&self) {
        let inner = &self.inner;
        let timing = inner.timing;
        let mut missing_since: Option<f64> = None;
        let mut next_missing_warn = 30.0_f64;
        let mut warn_at = 0.0_f64;
        while !inner.stopped() {
            // Off the runtime: with the board off the bus, IOKit's enumeration takes 2-3 s, and
            // on the loop it froze every door, Telegram included (1,543+ stall reports,
            // 2026-09-23/24).
            let port = {
                let i = inner.clone();
                match tokio::task::spawn_blocking(move || (i.seams.resolve_port)(&i.port_pattern, &i.skip_serials)).await {
                    Ok(p) => p,
                    Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                    Err(_) => None,
                }
            };
            let Some(port) = port else {
                // Device off the bus: say so, loudly then backing off. The 52-minute silent log
                // hole of 2026-08-05 04:43 was this path having no log line at all.
                let now = inner.now();
                match missing_since {
                    None => {
                        missing_since = Some(now);
                        next_missing_warn = 30.0;
                        warn_at = now + next_missing_warn;
                    }
                    Some(since) if now >= warn_at => {
                        // A deadline, not total time missing: compared with the capped gap, every
                        // pass after 600 s warned, every 3 s (two thirds of the log by 2026-10-13).
                        warn!(
                            "serial: no port matching {} for {:.0}s — board unplugged?",
                            inner.port_pattern,
                            now - since
                        );
                        next_missing_warn = (next_missing_warn * 2.0).min(600.0);
                        warn_at = now + next_missing_warn;
                    }
                    Some(_) => {}
                }
                inner.sleep(timing.reconnect_secs).await;
                continue;
            };
            if let Some(since) = missing_since.take() {
                info!("serial: device reappeared at {} after {:.0}s away", port, inner.now() - since);
            }
            // Opening the port, the RTS pulse's 0.1 s sleep and the buffer reset all block, so
            // they run on a blocking thread: on the runtime they would stall every door for as
            // long as the open takes.
            let pulse = std::mem::take(&mut inner.st.lock().pulse_on_open);
            if pulse {
                // Escalation for the enumerated-but-silent board: pulse RTS on the FRESH handle
                // (pulsing before reopen is a no-op — the old handle is already gone). Logs
                // unconditionally: this line's absence from a log is proof the pulse never ran.
                warn!(
                    "serial: RTS-resetting the board — {} silent or deaf reopens of {} within {:.0} min",
                    RX_TRIP_PULSE_COUNT,
                    port,
                    RX_TRIP_WINDOW_SECS / 60.0
                );
            }
            let opened = {
                let (opener, sleeper, p, baud) =
                    (inner.seams.open.clone(), inner.seams.sleep.clone(), port.clone(), inner.baud);
                match tokio::task::spawn_blocking(move || open_port(&opener, &sleeper, &p, baud, pulse)).await {
                    Ok(r) => r,
                    Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                    Err(e) => Err(SerialError::Serial(e.to_string())),
                }
            };
            let ser = match opened {
                Ok(ser) => ser,
                Err(e) => {
                    warn!("serial open {} failed: {}", port, e);
                    inner.sleep(timing.reconnect_secs).await;
                    continue;
                }
            };

            inner.st.lock().drop_reason = None;
            *inner.ser.lock() = Some(ser.clone());
            inner.connected_tx.send_replace(true);
            info!("serial connected: {} @ {}", port, inner.baud);
            let mut close_guard = CloseOnExit { inner: inner.clone(), armed: true };
            if let Err(e) = self.read_loop(&ser, &port).await {
                warn!("serial read error ({}) — reconnecting", e);
            }
            close_guard.armed = false;
            inner.close_current();
            let holdoff = {
                let mut st = inner.st.lock();
                let h = st.reopen_holdoff;
                st.reopen_holdoff = 0.0;
                h
            };
            let holdoff = if holdoff != 0.0 { holdoff } else { timing.reconnect_secs };
            inner.sleep(holdoff).await;
        }
    }

    async fn read_loop(&self, ser: &Arc<dyn SerialHandle>, port: &str) -> Result<(), String> {
        let inner = &self.inner;
        let mut buf: Vec<u8> = Vec::new();
        // Grace period starts now: a board mid-boot hasn't printed yet.
        let mut last_rx = inner.now();
        while !inner.stopped() {
            let reader = ser.clone();
            let chunk = match tokio::task::spawn_blocking(move || read_chunk(&*reader)).await {
                Ok(Ok(chunk)) => chunk,
                Ok(Err(e)) => return Err(e.to_string()),
                Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                Err(e) => return Err(e.to_string()),
            };
            let reason = inner.st.lock().drop_reason.clone();
            if let Some(reason) = reason {
                warn!("serial: dropping link — {}", reason);
                break;
            }
            if chunk.is_empty() {
                // read returned on its 1s timeout (or a cancel_read). Silence past the watchdog
                // means a stale fd or a hung board — drop and let the outer loop re-resolve the
                // glob and reopen.
                if inner.now() - last_rx > inner.timing.rx_silence_secs {
                    warn!(
                        "serial: no data from board for {:.0}s — assuming stale handle (USB re-enumeration?), reconnecting",
                        inner.timing.rx_silence_secs
                    );
                    inner.note_rx_trip(port);
                    break;
                }
                continue;
            }
            last_rx = inner.now();
            buf.extend_from_slice(&chunk);
            // A chunk may hold several lines, or the tail of a long one (a camera frame spans
            // many USB packets).
            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let rest = buf.split_off(nl + 1);
                buf.truncate(nl);
                let line = std::mem::replace(&mut buf, rest);
                inner.dispatch_line(&line);
            }
            if buf.len() > MAX_LINE_BYTES {
                warn!("serial: {} bytes without a newline — discarding", buf.len());
                buf.clear();
            }
        }
        Ok(())
    }

    /// One complete line off the wire (`_dispatch_line`). Must run inside a
    /// tokio runtime: handlers are spawned as tasks.
    pub fn dispatch_line(&self, line: &[u8]) {
        self.inner.dispatch_line(line);
    }

    /// Hardware-reset the board via the RTS line, then reconnect.
    ///
    /// The escalation for a board-side USB wedge: the S3 keeps transmitting
    /// but stops receiving, and reopening the host port does nothing because
    /// the dead half is in the chip. The pet reboots (~5s, diag ring reports
    /// the reset as external-pin) — a short outage against an indefinitely
    /// deaf board. Pulses the LIVE handle first (ioctls under a concurrent
    /// poll are safe; the fd must be open to pulse), then hands the drop to
    /// the reader. Blocks the caller for the 0.1 s pulse, as in Python.
    pub fn pulse_reset(&self, why: &str) {
        let ser = self.inner.ser.lock().clone();
        warn!("serial: RTS-resetting the board — {}", why);
        if let Some(ser) = ser {
            let pulsed = (|| {
                ser.set_dtr(false)?;
                ser.set_rts(true)?;
                (self.inner.seams.sleep)(0.1);
                ser.set_rts(false)
            })();
            if let Err(e) = pulsed {
                warn!("serial: RTS pulse failed: {}", e);
            }
        }
        self.inner.request_drop(&format!("RTS reset: {why}"));
    }

    /// Drop the handle so run() reopens the port — after a deliberate
    /// closed-port holdoff. Field evidence: daemon restarts (tens of seconds
    /// closed) revived the deaf wedge; 3s in-process reopens never did.
    ///
    /// For the half-dead link: writes silently go nowhere while reads keep
    /// working, so RX_SILENCE_SECS never trips and the daemon looks healthy
    /// while the board sits there showing "No Claude connected". Only the
    /// absence of *replies* reveals it — see the daemon's status-ack watchdog.
    pub fn force_reconnect(&self, why: &str) {
        let holdoff = self.inner.timing.forced_holdoff_secs;
        warn!("serial: forcing reconnect (holding port closed {:.0}s) — {}", holdoff, why);
        self.inner.st.lock().reopen_holdoff = holdoff;
        self.inner.request_drop(why);
    }

    /// `stop()`.
    pub async fn stop(&self) {
        self.request_stop();
    }

    /// `stop()` from synchronous code (a signal handler, a blocking thread):
    /// the same two steps, which never await.
    pub fn request_stop(&self) {
        self.inner.stop_tx.send_replace(true);
        self.inner.request_drop("stopping");
    }
}

#[cfg(test)]
mod tests;
