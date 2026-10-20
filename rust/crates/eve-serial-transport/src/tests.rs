//! Ports of bridge/tests/test_serial_transport.py, the transport half of
//! test_serial_lean.py and test_serial_skip.py, plus extra cover.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

use super::*;

// ---- fakes ------------------------------------------------------------------------------------

/// Minimal pyserial stand-in.
///
/// `lines` are returned one per read() call (read_chunk asks for one byte and then whatever is
/// waiting; here a whole line comes back at once and `in_waiting` stays 0). Once exhausted every
/// subsequent call returns empty — exactly what a stale handle looks like, since pyserial returns
/// empty on its read timeout rather than raising.
#[derive(Default)]
struct FakeSerial {
    lines: Mutex<VecDeque<Vec<u8>>>,
    closed: AtomicBool,
    cancelled: AtomicBool,
    dtr: AtomicBool,
    rts: AtomicBool,
    input_resets: AtomicUsize,
    written: Mutex<Vec<Vec<u8>>>,
    write_err: Mutex<Option<SerialError>>,
    dtr_err: Mutex<Option<SerialError>>,
    port: Option<String>,
    modem_log: Mutex<Vec<(&'static str, bool, ThreadId)>>,
}

impl FakeSerial {
    fn new(lines: Vec<&[u8]>) -> FakeSerial {
        FakeSerial { lines: Mutex::new(lines.into_iter().map(<[u8]>::to_vec).collect()), ..Default::default() }
    }

    fn repeated(line: &[u8], n: usize) -> FakeSerial {
        FakeSerial { lines: Mutex::new(std::iter::repeat_n(line.to_vec(), n).collect()), ..Default::default() }
    }

    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl SerialHandle for FakeSerial {
    fn read(&self, _size: usize) -> Result<Vec<u8>, SerialError> {
        Ok(self.lines.lock().pop_front().unwrap_or_default())
    }
    fn in_waiting(&self) -> Result<usize, SerialError> {
        Ok(0)
    }
    fn write(&self, data: &[u8]) -> Result<usize, SerialError> {
        if let Some(e) = self.write_err.lock().clone() {
            return Err(e);
        }
        self.written.lock().push(data.to_vec());
        Ok(data.len())
    }
    fn is_open(&self) -> bool {
        !self.closed()
    }
    fn cancel_read(&self) -> Result<(), SerialError> {
        self.cancelled.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn close(&self) -> Result<(), SerialError> {
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn set_dtr(&self, on: bool) -> Result<(), SerialError> {
        if let Some(e) = self.dtr_err.lock().clone() {
            return Err(e);
        }
        self.dtr.store(on, Ordering::SeqCst);
        self.modem_log.lock().push(("dtr", on, std::thread::current().id()));
        Ok(())
    }
    fn set_rts(&self, on: bool) -> Result<(), SerialError> {
        self.rts.store(on, Ordering::SeqCst);
        self.modem_log.lock().push(("rts", on, std::thread::current().id()));
        Ok(())
    }
    fn reset_input_buffer(&self) -> Result<(), SerialError> {
        self.input_resets.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn port(&self) -> Option<String> {
        self.port.clone()
    }
}

type Opened = Arc<Mutex<Vec<Arc<FakeSerial>>>>;

fn noop() -> IncomingHandler {
    Arc::new(|_obj| Box::pin(async {}))
}

/// Shrink the timeouts so these run in milliseconds, not half a minute.
fn fast_watchdog() -> Timing {
    Timing { rx_silence_secs: 0.05, reconnect_secs: 0.01, ..Timing::default() }
}

/// `_patch_port`: every open hands out a fresh fake from `make`; the port always resolves.
fn patched(make: impl Fn() -> FakeSerial + Send + Sync + 'static) -> (Seams, Opened) {
    let opened: Opened = Arc::new(Mutex::new(Vec::new()));
    let sink = opened.clone();
    let seams = Seams {
        open: Arc::new(move |_port, _baud| {
            let s = Arc::new(make());
            sink.lock().push(s.clone());
            Ok(s as Arc<dyn SerialHandle>)
        }),
        resolve_port: Arc::new(|_p, _skip| Some("/dev/fake".to_string())),
        ..Seams::default()
    };
    (seams, opened)
}

fn transport(on_message: IncomingHandler, seams: Seams, timing: Timing) -> EveSerial {
    EveSerial::with_seams(on_message, "/dev/fake", 115200, BTreeSet::new(), seams, timing)
}

/// Run the transport loop for a bounded time, then shut it down cleanly.
async fn drive(bs: &EveSerial, seconds: f64) {
    let b = bs.clone();
    let task = tokio::spawn(async move { b.run().await });
    tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
    bs.stop().await;
    task.abort();
    let _ = task.await;
}

// ---- log capture ------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<(Level, String)>>>);

struct Message(String);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl<S: Subscriber> Layer<S> for Logs {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut m = Message(String::new());
        event.record(&mut m);
        self.0.lock().push((*event.metadata().level(), m.0));
    }
}

impl Logs {
    /// Capture this thread's events (the current-thread runtime's tasks run here).
    fn install(&self) -> tracing::subscriber::DefaultGuard {
        tracing::subscriber::set_default(tracing_subscriber::registry().with(self.clone()))
    }

    fn matching(&self, needle: &str) -> Vec<(Level, String)> {
        self.0.lock().iter().filter(|(_, m)| m.contains(needle)).cloned().collect()
    }
}

// ---- receive-silence watchdog -----------------------------------------------------------------

/// A port that goes quiet is reopened rather than held forever.
#[tokio::test]
async fn test_silence_triggers_reconnect() {
    let (seams, opened) = patched(|| FakeSerial::new(vec![b"[alive] up=1s\n"])); // one line, then silence
    let bs = transport(noop(), seams, fast_watchdog());
    drive(&bs, 0.4).await;
    let opened = opened.lock();
    // Without the watchdog this stays at 1 forever.
    assert!(opened.len() > 1, "stale handle was never dropped");
    assert!(opened[0].closed(), "the dead handle should be closed on teardown");
}

/// A chatty board is never reconnected — the watchdog must not flap.
#[tokio::test]
async fn test_traffic_keeps_link_open() {
    // Far more lines than the test will consume: the board keeps talking.
    let (seams, opened) = patched(|| FakeSerial::repeated(b"[alive] up=1s\n", 100_000));
    let bs = transport(noop(), seams, fast_watchdog());
    drive(&bs, 0.4).await;
    assert_eq!(opened.lock().len(), 1, "a live link was needlessly reconnected");
}

/// Non-JSON boot chatter is skipped; JSON is parsed and dispatched.
#[tokio::test]
async fn test_json_lines_reach_the_handler() {
    let seen: Arc<Mutex<Vec<Map<String, Value>>>> = Arc::default();
    let sink = seen.clone();
    let capture: IncomingHandler = Arc::new(move |obj| {
        sink.lock().push(obj);
        Box::pin(async {})
    });
    let (seams, _opened) = patched(|| {
        FakeSerial::new(vec![b"[alive] up=1s\n", br#"{"cmd":"permission","id":"abc","decision":"once"}"# as &[u8], b"\n"])
    });
    let bs = transport(capture, seams, fast_watchdog());
    drive(&bs, 0.2).await;
    let want = json!({"cmd": "permission", "id": "abc", "decision": "once"});
    assert!(seen.lock().iter().any(|o| Value::Object(o.clone()) == want), "{:?}", seen.lock());
}

/// force_reconnect never closes the fd itself — it flags the reader, which closes and reopens.
/// (Closing under the reader's blocked readline raced: paired EBADF after every forced reconnect,
/// fd-reuse hazard.)
#[tokio::test]
async fn test_force_reconnect_defers_close_to_reader() {
    let (seams, opened) = patched(|| FakeSerial::repeated(b"[alive] up=1s\n", 100_000));
    let timing = Timing { forced_holdoff_secs: 0.01, ..fast_watchdog() };
    let bs = transport(noop(), seams, timing);
    let b = bs.clone();
    let task = tokio::spawn(async move { b.run().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let first = opened.lock()[0].clone();
    bs.force_reconnect("test wedge");
    assert!(!first.closed(), "event-loop side must not close the fd");
    assert!(first.cancelled(), "reader must be kicked out of readline");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(first.closed(), "reader should close the condemned handle");
    assert!(opened.lock().len() > 1, "port should reopen after the holdoff");
    bs.stop().await;
    task.abort();
    let _ = task.await;
}

/// A write that fails after a reconnect must not tear down the NEW handle.
#[tokio::test]
async fn test_send_failure_does_not_condemn_fresh_handle() {
    let stale = Arc::new(FakeSerial::new(vec![]));
    *stale.write_err.lock() = Some(SerialError::Serial("late failure".into()));
    let bs = transport(noop(), Seams::default(), Timing::default());
    bs.adopt(stale);
    let ok = bs.send(&json!({"a": 1}), None).await.unwrap();
    assert!(!ok);
    assert!(bs.drop_reason().is_some()); // stale handle condemned
    // Simulate the reader reopening:
    bs.adopt(Arc::new(FakeSerial::new(vec![])));
    assert!(bs.connected());
}

/// Enumerated-but-silent reopens of the same port arm and consume the RTS pulse instead of
/// looping reopens forever.
#[tokio::test]
async fn test_repeated_silence_arms_rts_pulse() {
    let (seams, opened) = patched(|| FakeSerial::new(vec![])); // born silent
    let bs = transport(noop(), seams, fast_watchdog());
    drive(&bs, 0.8).await;
    let opened = opened.lock();
    assert!(opened.len() > RX_TRIP_PULSE_COUNT);
    assert!(!bs.pulse_on_open()); // armed then consumed, not left dangling
    assert!(
        opened.iter().any(|s| s.modem_log.lock().iter().any(|(line, on, _)| *line == "rts" && *on)),
        "a consumed pulse must have raised RTS on a fresh handle"
    );
}

/// `count` fresh handles whose first write fails with `err`, as the reader's reopen loop would
/// hand them over.
async fn deaf_reopens(err: SerialError, count: usize) -> EveSerial {
    let bs = transport(noop(), Seams::default(), Timing::default());
    for _ in 0..count {
        let fresh = FakeSerial { port: Some("/dev/cu.usbmodem101".into()), ..FakeSerial::new(vec![]) };
        *fresh.write_err.lock() = Some(err.clone());
        bs.adopt(Arc::new(fresh));
        assert!(!bs.send(&json!({"time": 1}), None).await.unwrap());
    }
    bs
}

/// A board that stops draining its OUT endpoint (the Voice PE, 2026-10-01) gets the RTS reset
/// too, instead of a reopen every 5 s forever: a write timeout counts as a trip.
#[tokio::test]
async fn test_repeated_write_timeouts_arm_rts_pulse() {
    let bs = deaf_reopens(SerialError::WriteTimeout, RX_TRIP_PULSE_COUNT).await;
    assert!(bs.pulse_on_open());
}

/// Control: only a timeout is the deaf board; a write that raises anything else is not counted.
#[tokio::test]
async fn test_other_write_failures_do_not_arm_rts_pulse() {
    let bs = deaf_reopens(SerialError::Serial("late failure".into()), RX_TRIP_PULSE_COUNT).await;
    assert!(!bs.pulse_on_open());
}

// ---- port choice: the board, or nothing -------------------------------------------------------

fn port(device: &str, vid: Option<u16>, serial: Option<&str>) -> PortInfo {
    PortInfo { device: device.into(), vid, serial_number: serial.map(str::to_string) }
}

fn board() -> PortInfo {
    port("/dev/cu.usbmodem101", Some(0x303A), None)
}
fn camera() -> PortInfo {
    port("/dev/cu.usbmodem1105", Some(0x3564), None)
}
fn dock() -> PortInfo {
    port("/dev/cu.usbmodemSN1", Some(0x291A), None)
}
fn robot() -> PortInfo {
    port("/dev/cu.usbmodem31201", Some(0x303A), Some("0A:00:00:00:00:01"))
}
fn controller() -> PortInfo {
    port("/dev/cu.usbmodem101", Some(0x303A), Some("0A:00:00:00:00:02"))
}

/// `_ports`: glob and the USB list both see exactly `ports`.
fn resolve_among(ports: &[PortInfo], pattern: &str, skip: &[&str]) -> Option<String> {
    let skip: BTreeSet<String> = skip.iter().map(|s| s.to_string()).collect();
    let devices: Vec<String> = ports.iter().map(|p| p.device.clone()).collect();
    let listed = ports.to_vec();
    resolve_port_with(pattern, &skip, &move |_| devices.clone(), &move || Ok(listed.clone()))
}

fn resolve_broken_list(globbed: &[PortInfo], pattern: &str, skip: &[&str]) -> Option<String> {
    let skip: BTreeSet<String> = skip.iter().map(|s| s.to_string()).collect();
    let devices: Vec<String> = globbed.iter().map(|p| p.device.clone()).collect();
    resolve_port_with(pattern, &skip, &move |_| devices.clone(), &|| Err("IOKit mid-enumeration".into()))
}

#[test]
fn test_the_board_is_chosen_among_other_usb_devices() {
    assert_eq!(resolve_among(&[camera(), board(), dock()], "/dev/cu.usbmodem*", &[]), Some(board().device));
}

/// The StackChan and the Voice PE controller are both ESP32-S3s (2026-09-26): the robot's glob
/// skips the controller's serial, and the controller is named by its serial, in either case.
#[test]
fn test_two_esp32_boards_are_told_apart_by_usb_serial() {
    let all = [camera(), controller(), robot(), dock()];
    let skip = ["0a:00:00:00:00:02"];
    assert_eq!(resolve_among(&all, "/dev/cu.usbmodem*", &[]), Some(controller().device)); // sorted first
    assert_eq!(resolve_among(&all, "/dev/cu.usbmodem*", &skip), Some(robot().device));
    assert_eq!(resolve_among(&all, "usbsn:0A:00:00:00:00:02", &[]), Some(controller().device));
    assert_eq!(resolve_among(&all, "usbsn:0a:00:00:00:00:01", &[]), Some(robot().device));
}

#[test]
fn test_with_only_the_controller_on_the_bus_the_robot_waits() {
    let ports = [camera(), controller()];
    assert_eq!(resolve_among(&ports, "/dev/cu.usbmodem*", &["0A:00:00:00:00:02"]), None);
    assert_eq!(resolve_among(&ports, "usbsn:00:00:00:00:00:00", &[]), None);
}

/// The robot power-cycling left a webcam and a dock matching the glob (2026-09-23); the daemon
/// opened the webcam. Every match has a vendor id and none is Espressif: wait for the board.
#[test]
fn test_with_the_board_off_the_bus_a_camera_is_never_opened() {
    assert_eq!(resolve_among(&[camera(), dock()], "/dev/cu.usbmodem*", &[]), None);
    assert_eq!(resolve_among(&[camera()], "/dev/cu.usbmodem*", &[]), None);
}

/// A board behind a hub or a USB-serial chip may report no vendor id: then the glob is all there
/// is.
#[test]
fn test_without_vendor_ids_the_glob_still_decides() {
    let ports = [port("/dev/cu.usbmodem9", None, None)];
    assert_eq!(resolve_among(&ports, "/dev/cu.usbmodem*", &[]), Some("/dev/cu.usbmodem9".into()));
}

#[test]
fn test_an_enumeration_error_falls_back_to_the_glob() {
    assert_eq!(resolve_broken_list(&[board()], "/dev/cu.usbmodem*", &[]), Some(board().device));
}

/// With the board off the bus, the USB listing took 2-3 s on the daemon's own loop (1,543+ stall
/// reports, 2026-09-23/24) and froze every other door, Telegram included. The enumeration runs off
/// the loop: it never runs on the loop's thread, and a slow enumerator does not hold back a
/// concurrent loop task.
#[tokio::test]
async fn test_port_enumeration_never_blocks_the_event_loop() {
    let loop_thread = std::thread::current().id();
    let threads: Arc<Mutex<Vec<ThreadId>>> = Arc::default();
    let seen = threads.clone();
    let resolve: PortResolver = Arc::new(move |pattern, skip| {
        let seen = seen.clone();
        let slow_comports = move || -> Result<Vec<PortInfo>, String> {
            seen.lock().push(std::thread::current().id());
            std::thread::sleep(Duration::from_millis(200));
            Ok(vec![camera(), dock()]) // the board is off the bus: run() keeps waiting
        };
        resolve_port_with(pattern, skip, &|_| vec![camera().device, dock().device], &slow_comports)
    });
    let seams = Seams { resolve_port: resolve, ..Seams::default() };
    let bs = EveSerial::with_seams(noop(), "/dev/cu.usbmodem*", 115200, BTreeSet::new(), seams, fast_watchdog());
    let b = bs.clone();
    let runner = tokio::spawn(async move { b.run().await });
    let (mut worst, mut last) = (Duration::ZERO, Instant::now());
    for _ in 0..40 {
        // a concurrent loop task ticking every 10 ms
        tokio::time::sleep(Duration::from_millis(10)).await;
        let now = Instant::now();
        worst = worst.max(now - last);
        last = now;
    }
    bs.stop().await;
    runner.abort();
    let _ = runner.await;
    let threads = threads.lock();
    assert!(!threads.is_empty(), "the enumerator was never called");
    assert!(threads.iter().all(|t| *t != loop_thread), "comports() ran on the event loop's thread");
    assert!(worst < Duration::from_millis(150), "a loop task was held back {worst:?} by the port enumeration");
}

/// Eve/PortPick.lean's gap: the USB listing failing skipped the skip filter and fell back to the
/// glob's first node, which can be the controller. With a skip set, the robot waits for the next
/// open instead.
#[test]
fn test_when_the_usb_list_fails_a_skip_is_never_guessed_past() {
    let globbed = [controller(), robot()];
    assert_eq!(resolve_broken_list(&globbed, "/dev/cu.usbmodem*", &["0A:00:00:00:00:02"]), None);
    assert_eq!(resolve_broken_list(&globbed, "/dev/cu.usbmodem*", &[]), Some(controller().device)); // no skip: as before
}

/// With the board off the bus for hours, the warning comes at most every 10 minutes. The back-off
/// compared total time missing with the capped gap, so after 600 s every 3 s pass warned: 70k of
/// the log's 106k lines by 2026-10-13.
#[tokio::test]
async fn test_unplugged_board_warning_backs_off_to_ten_minutes() {
    let logs = Logs::default();
    let _guard = logs.install();
    let clock = Arc::new(Mutex::new(0.0_f64));
    let passes = Arc::new(AtomicUsize::new(0));
    let me: Arc<Mutex<Option<std::sync::Weak<Inner>>>> = Arc::default();
    let (c, p, m) = (clock.clone(), passes.clone(), me.clone());
    let no_port: PortResolver = Arc::new(move |_pattern, _skip| {
        p.fetch_add(1, Ordering::SeqCst);
        let mut now = c.lock();
        *now += 3.0; // one RECONNECT_SECS pass of real time
        if *now >= 6000.0 {
            if let Some(inner) = m.lock().as_ref().and_then(std::sync::Weak::upgrade) {
                EveSerial { inner }.request_stop();
            }
        }
        None
    });
    let c2 = clock.clone();
    let seams = Seams { resolve_port: no_port, monotonic: Arc::new(move || *c2.lock()), ..Seams::default() };
    let timing = Timing { reconnect_secs: 0.0, ..Timing::default() };
    let bs = transport(noop(), seams, timing);
    *me.lock() = Some(Arc::downgrade(&bs.inner));
    tokio::time::timeout(Duration::from_secs(10), bs.run()).await.expect("run() must end on stop()");
    let warned = logs.matching("no port matching");
    assert!(passes.load(Ordering::SeqCst) >= 1990);
    // 30 s, then doubling to the 600 s cap: 30, 90, 210, 450, 930, 1530 … 5730 → 13 warnings.
    assert_eq!(warned.len(), 13, "{warned:?}");
    assert!(warned.iter().all(|(level, _)| *level == Level::WARN));
    assert_eq!(warned[0].1, "serial: no port matching /dev/fake for 30s — board unplugged?");
}

/// Each line's on_message task is held by the transport (review D2): every handler is counted
/// until it finishes.
#[tokio::test]
async fn test_handler_tasks_are_held_until_done() {
    let (release_tx, release_rx) = tokio::sync::watch::channel(false);
    let slow: IncomingHandler = Arc::new(move |_obj| {
        let mut rx = release_rx.clone();
        Box::pin(async move {
            let _ = rx.wait_for(|r| *r).await;
        })
    });
    let (seams, _opened) = patched(|| {
        let mut lines: Vec<&[u8]> = vec![b"{\"cmd\":\"a\"}\n", b"{\"cmd\":\"b\"}\n"];
        lines.extend(std::iter::repeat_n(b"[alive]\n" as &[u8], 100_000));
        FakeSerial::new(lines)
    });
    let bs = transport(slow, seams, fast_watchdog());
    let b = bs.clone();
    let task = tokio::spawn(async move { b.run().await });
    let mut held = Vec::new();
    tokio::time::sleep(Duration::from_millis(100)).await;
    held.push(bs.pending_tasks());
    release_tx.send_replace(true);
    tokio::time::sleep(Duration::from_millis(50)).await;
    held.push(bs.pending_tasks());
    bs.stop().await;
    task.abort();
    let _ = task.await;
    assert_eq!(held, vec![2, 0]);
}

/// Opening the port and the RTS pulse's 0.1 s sleep block; they run on a blocking thread, not
/// the runtime thread every door shares (review D2).
#[tokio::test]
async fn test_port_open_and_rts_pulse_run_off_the_loop() {
    let loop_thread = std::thread::current().id();
    let open_threads: Arc<Mutex<Vec<ThreadId>>> = Arc::default();
    let sleep_threads: Arc<Mutex<Vec<ThreadId>>> = Arc::default();
    let (ot, st) = (open_threads.clone(), sleep_threads.clone());
    let seams = Seams {
        open: Arc::new(move |_port, _baud| {
            ot.lock().push(std::thread::current().id());
            Ok(Arc::new(FakeSerial::repeated(b"[alive]\n", 100_000)) as Arc<dyn SerialHandle>)
        }),
        resolve_port: Arc::new(|_p, _s| Some("/dev/fake".into())),
        sleep: Arc::new(move |secs| {
            st.lock().push(std::thread::current().id());
            std::thread::sleep(Duration::from_secs_f64(secs));
        }),
        ..Seams::default()
    };
    let bs = transport(noop(), seams, fast_watchdog());
    bs.set_pulse_on_open(true);
    drive(&bs, 0.3).await;
    let (open_threads, sleep_threads) = (open_threads.lock(), sleep_threads.lock());
    assert!(!open_threads.is_empty() && !open_threads.contains(&loop_thread));
    assert!(!sleep_threads.is_empty() && !sleep_threads.contains(&loop_thread));
}

// ---- test_serial_lean.py, A: one write on the port at a time ----------------------------------

/// A port whose write blocks until released, counting how many writes are in it at once.
#[derive(Default)]
struct SlowSerial {
    released: std::sync::Mutex<bool>,
    release_cv: Condvar,
    entered: AtomicBool,
    inflight: AtomicUsize,
    max_inflight: AtomicUsize,
    wire: Mutex<Vec<Vec<u8>>>,
}

impl SlowSerial {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.release_cv.notify_all();
    }
}

impl SerialHandle for SlowSerial {
    fn read(&self, _size: usize) -> Result<Vec<u8>, SerialError> {
        Ok(Vec::new())
    }
    fn in_waiting(&self) -> Result<usize, SerialError> {
        Ok(0)
    }
    fn write(&self, data: &[u8]) -> Result<usize, SerialError> {
        let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_inflight.fetch_max(now, Ordering::SeqCst);
        self.entered.store(true, Ordering::SeqCst);
        let guard = self.released.lock().unwrap();
        let _ = self.release_cv.wait_timeout_while(guard, Duration::from_secs(5), |r| !*r).unwrap();
        self.wire.lock().push(data.to_vec());
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        Ok(data.len())
    }
    fn is_open(&self) -> bool {
        true
    }
    fn cancel_read(&self) -> Result<(), SerialError> {
        Ok(())
    }
    fn close(&self) -> Result<(), SerialError> {
        Ok(())
    }
    fn set_dtr(&self, _on: bool) -> Result<(), SerialError> {
        Ok(())
    }
    fn set_rts(&self, _on: bool) -> Result<(), SerialError> {
        Ok(())
    }
    fn reset_input_buffer(&self) -> Result<(), SerialError> {
        Ok(())
    }
    fn port(&self) -> Option<String> {
        None
    }
}

#[tokio::test]
async fn test_a_cancelled_sender_keeps_the_port_until_its_write_returns() {
    let bs = transport(noop(), Seams::default(), Timing::default());
    let port = Arc::new(SlowSerial::default());
    bs.adopt(port.clone());
    let b1 = bs.clone();
    let first = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_millis(50), b1.send(&json!({"first": 1}), None)).await
    });
    let p = port.clone();
    tokio::task::spawn_blocking(move || {
        let t0 = Instant::now();
        while !p.entered.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(1));
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await; // the timeout has cancelled the first sender by now
    let b2 = bs.clone();
    let second = tokio::spawn(async move { b2.send(&json!({"second": 2}), None).await });
    tokio::time::sleep(Duration::from_millis(150)).await; // give the second sender every chance to write
    port.release();
    let r1 = first.await.unwrap();
    let r2 = second.await.unwrap();
    assert_eq!(port.max_inflight.load(Ordering::SeqCst), 1, "writes overlapped on the port");
    assert!(r1.is_err(), "the caller still sees its timeout");
    assert!(r2.unwrap());
    let heads: Vec<Vec<u8>> =
        port.wire.lock().iter().map(|w| w.split(|b| *b == b':').next().unwrap().to_vec()).collect();
    assert_eq!(heads, vec![b"{\"first\"".to_vec(), b"{\"second\"".to_vec()]);
}

// ---- test_serial_skip.py ----------------------------------------------------------------------

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn test_the_setting_is_a_comma_list_of_serials_any_case() {
    assert_eq!(serial_skip(Some(&env(&[]))), BTreeSet::new());
    assert_eq!(serial_skip(Some(&env(&[("EVE_SERIAL_SKIP", "")]))), BTreeSet::new());
    assert_eq!(
        serial_skip(Some(&env(&[("EVE_SERIAL_SKIP", " 0A:00:00:00:00:03 , ,0a:00 ")]))),
        set(&["0a:00:00:00:00:03", "0a:00"])
    );
}

fn skip_controller() -> PortInfo {
    port("/dev/cu.usbmodem101", Some(0x303A), Some("0A:00:00:00:00:02"))
}
fn skip_stick() -> PortInfo {
    port("/dev/cu.usbmodem31201", Some(0x303A), Some("0A:00:00:00:00:03"))
}
fn skip_robot() -> PortInfo {
    port("/dev/cu.usbmodem4101", Some(0x303A), Some("0A:00:00:00:00:01"))
}

/// What happened: the controller skipped, the stick the only other Espressif node, so it became
/// the robot.
#[test]
fn test_with_the_robot_unplugged_the_stick_is_not_opened() {
    let ports = [camera(), skip_controller(), skip_stick()];
    let devices: Vec<String> = ports.iter().map(|p| p.device.clone()).collect();
    let listed = ports.to_vec();
    let resolve = |skip: &BTreeSet<String>| {
        let (d, l) = (devices.clone(), listed.clone());
        resolve_port_with("/dev/cu.usbmodem*", skip, &move |_| d.clone(), &move || Ok(l.clone()))
    };
    let ctl_only = set(&["0a:00:00:00:00:02"]);
    assert_eq!(resolve(&ctl_only), Some(skip_stick().device)); // the bug (control)
    let mut skip = ctl_only.clone();
    skip.extend(serial_skip(Some(&env(&[("EVE_SERIAL_SKIP", "0A:00:00:00:00:03")]))));
    assert_eq!(resolve(&skip), None); // waits instead
}

#[test]
fn test_with_the_robot_plugged_in_it_is_still_found() {
    let ports = [camera(), skip_controller(), skip_stick(), skip_robot()];
    assert_eq!(
        resolve_among(&ports, "/dev/cu.usbmodem*", &["0a:00:00:00:00:02", "0a:00:00:00:00:03"]),
        Some(skip_robot().device)
    );
}

// ---- extra cover ------------------------------------------------------------------------------

#[test]
fn a_concrete_port_is_used_as_given() {
    assert_eq!(resolve_among(&[], "/dev/cu.usbmodem101", &[]), Some("/dev/cu.usbmodem101".into()));
    assert_eq!(resolve_among(&[], "/dev/cu.usbmodem*", &[]), None);
}

#[test]
fn usbsn_needs_a_callout_node_and_a_working_list() {
    let tty = port("/dev/tty.usbmodem31201", Some(0x303A), Some("0A:00:00:00:00:01"));
    assert_eq!(resolve_among(&[tty], "usbsn:0A:00:00:00:00:01", &[]), None);
    assert_eq!(resolve_broken_list(&[robot()], "usbsn:0A:00:00:00:00:01", &[]), None);
    assert_eq!(resolve_among(&[robot()], "usbsn: 0a:00:00:00:00:01 ", &[]), Some(robot().device));
}

#[test]
fn several_unknown_vendor_nodes_pick_the_first_sorted() {
    let ports = [port("/dev/cu.usbmodemB", None, None), port("/dev/cu.usbmodemA", Some(0x1234), None)];
    let logs = Logs::default();
    let _guard = logs.install();
    assert_eq!(resolve_among(&ports, "/dev/cu.usbmodem*", &[]), Some("/dev/cu.usbmodemA".into()));
    assert_eq!(
        logs.matching("nodes match"),
        vec![(Level::WARN, "serial: 2 nodes match /dev/cu.usbmodem* — picking /dev/cu.usbmodemA".to_string())]
    );
}

#[test]
fn two_espressif_nodes_prefer_the_first_and_say_so() {
    let logs = Logs::default();
    let _guard = logs.install();
    assert_eq!(resolve_among(&[robot(), board()], "/dev/cu.usbmodem*", &[]), Some(board().device));
    assert_eq!(
        logs.matching("nodes match")[0].1,
        "serial: 2 nodes match /dev/cu.usbmodem* — preferring ESP32-S3 /dev/cu.usbmodem101"
    );
}

#[test]
fn read_chunk_drains_what_is_waiting() {
    struct Waiting(Mutex<Vec<u8>>);
    impl SerialHandle for Waiting {
        fn read(&self, size: usize) -> Result<Vec<u8>, SerialError> {
            let mut b = self.0.lock();
            let n = size.min(b.len());
            Ok(b.drain(..n).collect())
        }
        fn in_waiting(&self) -> Result<usize, SerialError> {
            Ok(self.0.lock().len())
        }
        fn write(&self, d: &[u8]) -> Result<usize, SerialError> {
            Ok(d.len())
        }
        fn is_open(&self) -> bool {
            true
        }
        fn cancel_read(&self) -> Result<(), SerialError> {
            Ok(())
        }
        fn close(&self) -> Result<(), SerialError> {
            Ok(())
        }
        fn set_dtr(&self, _: bool) -> Result<(), SerialError> {
            Ok(())
        }
        fn set_rts(&self, _: bool) -> Result<(), SerialError> {
            Ok(())
        }
        fn reset_input_buffer(&self) -> Result<(), SerialError> {
            Ok(())
        }
        fn port(&self) -> Option<String> {
            None
        }
    }
    let w = Waiting(Mutex::new(b"{\"a\":1}\n{\"b\"".to_vec()));
    assert_eq!(read_chunk(&w).unwrap(), b"{\"a\":1}\n{\"b\"");
    assert_eq!(read_chunk(&w).unwrap(), b"");
}

#[test]
fn open_port_raises_dtr_pulses_and_flushes() {
    let made: Arc<Mutex<Option<Arc<FakeSerial>>>> = Arc::default();
    let m = made.clone();
    let opener: PortOpener = Arc::new(move |_p, _b| {
        let s = Arc::new(FakeSerial::new(vec![]));
        *m.lock() = Some(s.clone());
        Ok(s as Arc<dyn SerialHandle>)
    });
    let sleeps: Arc<Mutex<Vec<f64>>> = Arc::default();
    let sl = sleeps.clone();
    let sleeper: Sleeper = Arc::new(move |s| sl.lock().push(s));
    open_port(&opener, &sleeper, "/dev/fake", 115200, false).unwrap();
    let s = made.lock().clone().unwrap();
    let lines: Vec<(&str, bool)> = s.modem_log.lock().iter().map(|(l, on, _)| (*l, *on)).collect();
    assert_eq!(lines, vec![("dtr", true)]);
    assert_eq!(s.input_resets.load(Ordering::SeqCst), 1);
    assert!(sleeps.lock().is_empty());

    open_port(&opener, &sleeper, "/dev/fake", 115200, true).unwrap();
    let s = made.lock().clone().unwrap();
    let lines: Vec<(&str, bool)> = s.modem_log.lock().iter().map(|(l, on, _)| (*l, *on)).collect();
    assert_eq!(lines, vec![("dtr", true), ("rts", true), ("rts", false), ("dtr", true)]);
    assert_eq!(*sleeps.lock(), vec![0.1]);
}

#[test]
fn open_port_closes_the_handle_when_dtr_fails() {
    let made: Arc<Mutex<Option<Arc<FakeSerial>>>> = Arc::default();
    let m = made.clone();
    let opener: PortOpener = Arc::new(move |_p, _b| {
        let s = Arc::new(FakeSerial::new(vec![]));
        *s.dtr_err.lock() = Some(SerialError::Serial("[Errno 25] Inappropriate ioctl for device".into()));
        *m.lock() = Some(s.clone());
        Ok(s as Arc<dyn SerialHandle>)
    });
    let sleeper: Sleeper = Arc::new(|_| {});
    let err = open_port(&opener, &sleeper, "/dev/fake", 115200, false).err().unwrap();
    assert_eq!(err.to_string(), "[Errno 25] Inappropriate ioctl for device");
    assert!(made.lock().clone().unwrap().closed());
}

#[tokio::test]
async fn board_chatter_is_logged_at_its_level() {
    let logs = Logs::default();
    let _guard = logs.install();
    let bs = transport(noop(), Seams::default(), Timing::default());
    bs.dispatch_line(b"[alive] up=5s");
    bs.dispatch_line(b"Guru Meditation Error: Core 1 panic'ed");
    bs.dispatch_line(b"E (1234) task_wdt: oops");
    bs.dispatch_line(b"[body] motion admitted");
    bs.dispatch_line(b"   ");
    bs.dispatch_line(b"{not json");
    let stick: Vec<(Level, String)> = logs.matching("stick:");
    assert_eq!(
        stick,
        vec![
            (Level::DEBUG, "stick: [alive] up=5s".to_string()),
            (Level::WARN, "stick: Guru Meditation Error: Core 1 panic'ed".to_string()),
            (Level::WARN, "stick: E (1234) task_wdt: oops".to_string()),
            (Level::INFO, "stick: [body] motion admitted".to_string()),
        ]
    );
    assert_eq!(logs.matching("bad json"), vec![(Level::DEBUG, "serial: bad json: '{not json'".to_string())]);
}

#[tokio::test]
async fn the_boot_hook_fires_once_per_boot() {
    let clock = Arc::new(Mutex::new(1000.0_f64));
    let c = clock.clone();
    let seams = Seams { monotonic: Arc::new(move || *c.lock()), ..Seams::default() };
    let bs = transport(noop(), seams, Timing::default());
    let fired = Arc::new(AtomicUsize::new(0));
    let f = fired.clone();
    bs.set_on_boot(Some(Arc::new(move || {
        f.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    })));
    bs.dispatch_line(b"rst:0x1 (POWERON),boot:0x8 (SPI_FAST_FLASH_BOOT)");
    bs.dispatch_line(b"[boot] stackchan 1.2");
    *clock.lock() += 6.0;
    bs.dispatch_line(b"[boot] stackchan 1.2");
    bs.dispatch_line(b"not a [boot] line");
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(fired.load(Ordering::SeqCst), 2);
    assert_eq!(bs.pending_tasks(), 0);
}

#[tokio::test]
async fn a_newline_less_spew_is_discarded() {
    let logs = Logs::default();
    let _guard = logs.install();
    let seen = Arc::new(AtomicUsize::new(0));
    let s = seen.clone();
    let handler: IncomingHandler = Arc::new(move |_| {
        s.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    });
    let (seams, _opened) = patched(|| {
        let mut lines: Vec<Vec<u8>> = vec![vec![b'x'; 200 * 1024], vec![b'y'; 100 * 1024]];
        lines.push(b"{\"ok\":1}\n".to_vec());
        lines.extend(std::iter::repeat_n(b"[alive]\n".to_vec(), 100_000));
        FakeSerial { lines: Mutex::new(lines.into()), ..Default::default() }
    });
    let bs = transport(handler, seams, fast_watchdog());
    drive(&bs, 0.2).await;
    assert_eq!(
        logs.matching("without a newline"),
        vec![(Level::WARN, format!("serial: {} bytes without a newline — discarding", 300 * 1024))]
    );
    assert_eq!(seen.load(Ordering::SeqCst), 1, "the line after the discard still parses");
}

#[tokio::test]
async fn pulse_reset_pulses_the_live_handle_and_hands_the_drop_to_the_reader() {
    let bs = transport(noop(), Seams { sleep: Arc::new(|_| {}), ..Seams::default() }, Timing::default());
    let ser = Arc::new(FakeSerial::new(vec![]));
    bs.adopt(ser.clone());
    assert!(bs.connected());
    bs.pulse_reset("deaf");
    let lines: Vec<(&str, bool)> = ser.modem_log.lock().iter().map(|(l, on, _)| (*l, *on)).collect();
    assert_eq!(lines, vec![("dtr", false), ("rts", true), ("rts", false)]);
    assert_eq!(bs.drop_reason().as_deref(), Some("RTS reset: deaf"));
    assert!(ser.cancelled() && !ser.closed());
    assert!(!bs.connected());
    // A second request keeps the first reason.
    bs.force_reconnect("again");
    assert_eq!(bs.drop_reason().as_deref(), Some("RTS reset: deaf"));
}

#[tokio::test]
async fn send_writes_one_encoded_line() {
    let bs = transport(noop(), Seams::default(), Timing::default());
    assert!(!bs.send(&json!({"a": 1}), None).await.unwrap(), "no link: nothing sent");
    let ser = Arc::new(FakeSerial::new(vec![]));
    bs.adopt(ser.clone());
    assert!(bs.send(&json!({"msg": "héllo", "n": 1}), None).await.unwrap());
    assert_eq!(*ser.written.lock(), vec!["{\"msg\":\"héllo\",\"n\":1}\n".as_bytes().to_vec()]);
    assert!(bs.send(&json!({"m": "x"}), Some("no-such-codec")).await.is_err());
}

#[tokio::test]
async fn wait_connected_returns_once_the_port_opens() {
    let (seams, _opened) = patched(|| FakeSerial::repeated(b"[alive]\n", 100_000));
    let bs = transport(noop(), seams, fast_watchdog());
    let b = bs.clone();
    let task = tokio::spawn(async move { b.run().await });
    tokio::time::timeout(Duration::from_secs(2), bs.wait_connected()).await.expect("connected");
    assert!(bs.connected());
    bs.stop().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), task).await.expect("run() returns after stop()");
    assert!(!bs.connected());
    assert!(bs.handle().is_none(), "the reader closed and released the handle");
}

#[tokio::test]
async fn a_failed_open_is_logged_and_retried() {
    let logs = Logs::default();
    let _guard = logs.install();
    let tries = Arc::new(AtomicUsize::new(0));
    let t = tries.clone();
    let seams = Seams {
        open: Arc::new(move |_p, _b| {
            t.fetch_add(1, Ordering::SeqCst);
            Err(SerialError::Serial("[Errno 16] Resource busy".into()))
        }),
        resolve_port: Arc::new(|_p, _s| Some("/dev/fake".into())),
        ..Seams::default()
    };
    let bs = transport(noop(), seams, fast_watchdog());
    drive(&bs, 0.1).await;
    assert!(tries.load(Ordering::SeqCst) > 1);
    assert_eq!(logs.matching("open")[0].1, "serial open /dev/fake failed: [Errno 16] Resource busy");
}

#[test]
fn glob_ports_sorts_and_skips_dot_files() {
    let dir = std::env::temp_dir().join(format!("eve-serial-glob-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["cu.usbmodem2", "cu.usbmodem10", ".cu.hidden"] {
        std::fs::write(dir.join(name), b"").unwrap();
    }
    let base = dir.to_string_lossy().into_owned();
    assert_eq!(
        glob_ports(&format!("{base}/cu.usbmodem*")),
        vec![format!("{base}/cu.usbmodem10"), format!("{base}/cu.usbmodem2")]
    );
    assert_eq!(glob_ports(&format!("{base}/*")).len(), 2);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_link_is_wired() {
    let bs = transport(noop(), Seams::default(), Timing::default());
    assert!(bs.wired());
    assert_eq!(bs.baud(), 115200);
    assert_eq!(bs.port_pattern(), "/dev/fake");
    assert!(bs.skip_serials().is_empty());
}

#[test]
fn the_regexes_match_what_python_matches() {
    assert!(IS_CRASH.is_match("task WATCHDOG got triggered"));
    assert!(IS_CRASH.is_match("ets Jul 29 2019 rst:0xc (SW_CPU_RESET)"));
    assert!(!IS_CRASH.is_match("E (x) not an idf line"));
    assert!(IS_BOOT.is_match("[boot] v1"));
    assert!(!IS_BOOT.is_match(" [boot] v1"));
    assert!(IS_MOTION.is_match("[gaze] host look 10,20"));
    assert!(!IS_MOTION.is_match("x [body] motion"));
}

#[test]
fn the_usb_listing_names_callout_nodes_only() {
    // The real IOKit/udev listing: whatever is plugged in, it must answer, and on macOS list the
    // `/dev/cu.*` node of each port only (pyserial's list_ports_osx).
    let ports = comports().expect("listing serial ports");
    if cfg!(target_os = "macos") {
        assert!(ports.iter().all(|p| p.device.starts_with("/dev/cu.")), "{ports:?}");
    }
}
