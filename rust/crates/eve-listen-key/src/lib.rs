//! Listen key: mirror a held modifier on the Mac to the board.
//!
//! Port of `eve_bridge/listen_key.py`.
//!
//! The user's dictation app records while a global hotkey (Option by default)
//! is held. While that key is down the board should turn to face the user and
//! show its listening pose, and drop it on release. This module watches the
//! physical key; the daemon turns the transitions into wire messages:
//!
//! ```text
//! {"cmd":"listen","on":true}    key down
//! {"cmd":"listen","on":false}   key up
//! ```
//!
//! Two halves, kept apart so the state machine is testable without a Mac:
//!
//! * [`ListenKeyTracker`] — pure. Feed it the event flags and a clock; it
//!   reports a transition, or holds one back while the debounce window is open.
//! * [`start_listen_key`] — Quartz. A listen-only CGEventTap for flagsChanged
//!   runs on its own thread and marshals flags to the tokio runtime.
//!
//! key_tap (swipe-to-key) only taps Return and the arrow keys; it no longer
//! holds Option, so no synthetic modifier reaches this tap.
//!
//! The tap needs macOS Input Monitoring for the daemon's binary. Without it
//! CGEventTapCreate returns NULL; we log one warning naming the fix and the
//! feature is simply off — the daemon never crashes over it.

use std::fmt;
use std::time::Duration;

use eve_key_tap::{FLAG_ALTERNATE, FLAG_SECONDARY_FN};
use tokio::runtime::Handle;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

/// Minimum spacing between two transitions sent to the board. A flagsChanged
/// storm (key bounce, an app re-posting modifiers) collapses to the state that
/// holds once the window closes, so a quick tap still produces a clean
/// down/up pair instead of losing its release.
pub const DEBOUNCE_SECS: f64 = 0.15;

/// `EVE_LISTEN_KEY` → event-flag mask, in Python's dict order. "off" disables
/// the watcher.
pub const LISTEN_KEYS: &[(&str, u64)] = &[
    ("option", FLAG_ALTERNATE),
    ("fn", FLAG_SECONDARY_FN),
    ("off", 0),
];

/// Python's `Stopper = Callable[[], None]`: stops the watcher; safe to call
/// more than once.
pub type Stopper = Box<dyn Fn() + Send + Sync>;

/// The mask for a listen-key name (`LISTEN_KEYS[name]`), or `None` when the
/// name is not a key of `LISTEN_KEYS`.
pub fn listen_key_mask(name: &str) -> Option<u64> {
    LISTEN_KEYS.iter().find(|(k, _)| *k == name).map(|(_, m)| *m)
}

/// Python's `sys.platform` for the running build: "darwin" on macOS, the
/// Rust OS name elsewhere ("linux", ...). Only the "darwin" comparison matters.
pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        std::env::consts::OS
    }
}

/// Option on macOS; off elsewhere, where no tap can exist anyway.
pub fn default_listen_key() -> &'static str {
    default_listen_key_for(platform())
}

/// [`default_listen_key`] for an explicit `sys.platform` value (test seam).
pub fn default_listen_key_for(platform: &str) -> &'static str {
    if platform == "darwin" {
        "option"
    } else {
        "off"
    }
}

/// `EVE_LISTEN_KEY`, stripped and lowercased; an unset or empty value means
/// the platform default, and an unknown name logs a warning and falls back to
/// the default too.
pub fn configured_listen_key() -> String {
    let raw = std::env::var_os("EVE_LISTEN_KEY").map(|v| v.to_string_lossy().into_owned());
    configured_listen_key_from(raw.as_deref(), platform())
}

/// [`configured_listen_key`] over an explicit env value and platform, so
/// tests never touch the process environment.
pub fn configured_listen_key_from(value: Option<&str>, platform: &str) -> String {
    // `os.environ.get(...) or default`: an empty string is falsy.
    let chosen = match value {
        Some(v) if !v.is_empty() => v,
        _ => default_listen_key_for(platform),
    };
    let name = chosen.trim_matches(py_isspace).to_lowercase();
    if listen_key_mask(&name).is_none() {
        let fallback = default_listen_key_for(platform);
        tracing::warn!(
            "listen key: unknown EVE_LISTEN_KEY={}; using {}",
            py_repr(&name),
            fallback
        );
        return fallback.to_string();
    }
    name
}

/// Python's `ValueError(f"unknown listen key {key!r}")` from the tracker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownListenKey(pub String);

impl fmt::Display for UnknownListenKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown listen key {}", py_repr(&self.0))
    }
}

impl std::error::Error for UnknownListenKey {}

/// Turn a stream of modifier-flag snapshots into debounced down/up edges.
///
/// [`feed`](Self::feed) returns `Some(true)` (down) or `Some(false)` (up) when
/// the watched key changed state and the debounce window since the last
/// reported edge has closed. An edge that lands inside the window is held as
/// *pending*; [`due_in`](Self::due_in) says when it may go out and
/// [`flush`](Self::flush) releases it. A pending edge that is cancelled by a
/// return to the reported state (chatter) is dropped.
///
/// Times are seconds on a monotonic clock (Python's `time.monotonic()`).
#[derive(Debug, Clone)]
pub struct ListenKeyTracker {
    pub key: String,
    pub mask: u64,
    pub debounce: f64,
    down: bool,          // last edge reported
    reported_at: f64,    // first edge always passes (-inf)
    pending: Option<bool>,
}

impl ListenKeyTracker {
    /// `ListenKeyTracker(key, debounce)`. `key: None` reads `EVE_LISTEN_KEY`
    /// through [`configured_listen_key`]. The key is matched exactly (no
    /// stripping or lowercasing), as in Python.
    pub fn new(key: Option<&str>, debounce: f64) -> Result<Self, UnknownListenKey> {
        let key = match key {
            Some(k) => k.to_string(),
            None => configured_listen_key(),
        };
        let mask = listen_key_mask(&key).ok_or_else(|| UnknownListenKey(key.clone()))?;
        Ok(Self {
            key,
            mask,
            debounce,
            down: false,
            reported_at: f64::NEG_INFINITY,
            pending: None,
        })
    }

    /// `ListenKeyTracker(key=key)` with the default debounce.
    pub fn for_key(key: &str) -> Result<Self, UnknownListenKey> {
        Self::new(Some(key), DEBOUNCE_SECS)
    }

    /// `ListenKeyTracker()`: the configured key and the default debounce.
    /// Cannot fail: [`configured_listen_key`] only returns known names.
    pub fn configured() -> Self {
        let key = configured_listen_key();
        let mask = listen_key_mask(&key).unwrap_or(0);
        Self {
            key,
            mask,
            debounce: DEBOUNCE_SECS,
            down: false,
            reported_at: f64::NEG_INFINITY,
            pending: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.mask != 0
    }

    pub fn down(&self) -> bool {
        self.down
    }

    pub fn feed(&mut self, flags: u64, now: f64) -> Option<bool> {
        if !self.enabled() {
            return None;
        }
        self.settle(flags & self.mask != 0, now)
    }

    /// Release an edge held back by the debounce window, if it is due.
    pub fn flush(&mut self, now: f64) -> Option<bool> {
        let pending = self.pending?;
        if now - self.reported_at < self.debounce {
            return None;
        }
        self.settle(pending, now)
    }

    /// Seconds until the pending edge may be flushed; `None` when nothing is
    /// pending.
    pub fn due_in(&self, now: f64) -> Option<f64> {
        self.pending?;
        Some(f64::max(0.0, self.debounce - (now - self.reported_at)))
    }

    fn settle(&mut self, down: bool, now: f64) -> Option<bool> {
        if down == self.down {
            self.pending = None; // chatter resolved back to what we reported
            return None;
        }
        if now - self.reported_at < self.debounce {
            self.pending = Some(down);
            return None;
        }
        self.pending = None;
        self.down = down;
        self.reported_at = now;
        Some(down)
    }
}

impl Default for ListenKeyTracker {
    fn default() -> Self {
        Self::configured()
    }
}

/// The loop half of the watcher (Python's `_feed` and `_flush`): takes raw
/// flag snapshots from the tap thread, runs them through the tracker on the
/// runtime's monotonic clock, and calls `on_change` with each edge — at once,
/// or when the debounce window that held it back closes.
///
/// Returns when the sender side closes, after releasing an edge that was
/// still pending (Python's armed `call_later` flush outlives the stopper too).
pub async fn drive_tracker<F>(mut rx: UnboundedReceiver<u64>, mut tracker: ListenKeyTracker, mut on_change: F)
where
    F: FnMut(bool),
{
    let base = Instant::now();
    let now = || base.elapsed().as_secs_f64();
    let mut deadline: Option<Instant> = None;
    let arm = |tracker: &ListenKeyTracker, t: f64, deadline: &mut Option<Instant>| {
        if let Some(delay) = tracker.due_in(t) {
            *deadline = Some(base + Duration::from_secs_f64(t + delay));
        }
    };
    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => match msg {
                Some(flags) => {
                    // Emit now, or arm a flush for the end of the window.
                    let t = now();
                    if let Some(change) = tracker.feed(flags, t) {
                        on_change(change);
                    }
                    arm(&tracker, t, &mut deadline);
                }
                None => break,
            },
            _ = sleep_until_opt(deadline), if deadline.is_some() => {
                deadline = None;
                let t = now();
                if let Some(change) = tracker.flush(t) {
                    on_change(change);
                }
                // Float rounding can wake us a hair before the window closes;
                // re-arm rather than strand the pending edge.
                arm(&tracker, t, &mut deadline);
            }
        }
    }
    while let Some(at) = deadline.take() {
        tokio::time::sleep_until(at).await;
        let t = now();
        if let Some(change) = tracker.flush(t) {
            on_change(change);
        }
        arm(&tracker, t, &mut deadline);
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Watch the listen key and call `on_change(down)` on the runtime.
///
/// Returns a stopper, or `None` when the watcher cannot run (disabled, not
/// macOS, or Input Monitoring not granted). Every `None` path logs exactly
/// once and never panics. `tracker: None` builds one from `EVE_LISTEN_KEY`.
pub fn start_listen_key<F>(on_change: F, handle: &Handle, tracker: Option<ListenKeyTracker>) -> Option<Stopper>
where
    F: FnMut(bool) + Send + 'static,
{
    start_listen_key_on(on_change, handle, tracker, platform())
}

/// [`start_listen_key`] with an explicit `sys.platform` value (test seam).
pub fn start_listen_key_on<F>(
    on_change: F,
    handle: &Handle,
    tracker: Option<ListenKeyTracker>,
    platform: &str,
) -> Option<Stopper>
where
    F: FnMut(bool) + Send + 'static,
{
    let tracker = tracker.unwrap_or_else(ListenKeyTracker::configured);
    if !tracker.enabled() {
        tracing::info!("listen key: disabled (EVE_LISTEN_KEY=off)");
        return None;
    }
    if platform != "darwin" {
        tracing::warn!("listen key: only supported on macOS — board will not get listen events");
        return None;
    }
    quartz::start(on_change, handle, tracker)
}

fn input_monitoring_warning() {
    let exe = std::env::current_exe()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    tracing::warn!(
        "listen key: could not create the event tap — grant Input Monitoring \
         to {} in System Settings > Privacy & Security > Input Monitoring, \
         then restart the daemon. Board will not get listen events.",
        exe
    );
}

#[cfg(target_os = "macos")]
mod quartz {
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::{mpsc as std_mpsc, OnceLock};
    use std::time::Duration;

    use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRetained, CFRunLoop};
    use objc2_core_graphics::{
        CGEvent, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy,
        CGEventType,
    };
    use tokio::runtime::Handle;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

    use super::{drive_tracker, input_monitoring_warning, ListenKeyTracker, Stopper};

    /// What the tap callback reads: the channel to the runtime and, once
    /// created, the tap itself (to re-enable it). Lives on the tap thread.
    struct TapContext {
        tx: UnboundedSender<u64>,
        tap: OnceLock<CFRetained<CFMachPort>>,
    }

    /// The tap and its run loop, handed to the stopper on another thread.
    /// CGEventTapEnable and CFRunLoopStop are safe to call from any thread
    /// (the Python stopper does exactly that).
    struct Handles {
        tap: CFRetained<CFMachPort>,
        runloop: CFRetained<CFRunLoop>,
    }
    unsafe impl Send for Handles {}
    unsafe impl Sync for Handles {}

    impl Handles {
        fn stop(&self) {
            CGEvent::tap_enable(&self.tap, false);
            self.runloop.stop();
        }
    }

    // Quartz calls this on the tap thread.
    unsafe extern "C-unwind" fn callback(
        _proxy: CGEventTapProxy,
        etype: CGEventType,
        event: NonNull<CGEvent>,
        refcon: *mut c_void,
    ) -> *mut CGEvent {
        // SAFETY: refcon is the TapContext the tap thread owns; it outlives the tap.
        let ctx = unsafe { &*(refcon as *const TapContext) };
        if etype == CGEventType::TapDisabledByTimeout || etype == CGEventType::TapDisabledByUserInput {
            // macOS disables a tap it thinks is slow; re-enable or go deaf.
            if let Some(tap) = ctx.tap.get() {
                CGEvent::tap_enable(tap, true);
            }
            return event.as_ptr();
        }
        // SAFETY: Quartz hands us a valid event for the duration of the call.
        let flags = CGEvent::flags(Some(unsafe { event.as_ref() })).0;
        let _ = ctx.tx.send(flags);
        event.as_ptr()
    }

    pub(super) fn start<F>(on_change: F, handle: &Handle, tracker: ListenKeyTracker) -> Option<Stopper>
    where
        F: FnMut(bool) + Send + 'static,
    {
        let key = tracker.key.clone();
        let (tx, rx) = unbounded_channel::<u64>();
        let (ready_tx, ready_rx) = std_mpsc::channel::<Option<Handles>>();

        let spawned = std::thread::Builder::new()
            .name("listen-key-tap".into())
            .spawn(move || tap_thread(tx, ready_tx));
        let handles = match spawned {
            Ok(_) => ready_rx.recv_timeout(Duration::from_secs(5)).ok().flatten(),
            Err(e) => {
                tracing::error!("listen key: tap setup failed: {e}");
                None
            }
        };
        let Some(handles) = handles else {
            input_monitoring_warning();
            return None;
        };
        handle.spawn(drive_tracker(rx, tracker, on_change));
        tracing::info!("listen key: watching {} (EVE_LISTEN_KEY)", key);

        Some(Box::new(move || handles.stop()))
    }

    fn tap_thread(tx: UnboundedSender<u64>, ready: std_mpsc::Sender<Option<Handles>>) {
        let ctx = Box::into_raw(Box::new(TapContext { tx, tap: OnceLock::new() }));
        // SAFETY: `callback` matches CGEventTapCallBack and `ctx` stays valid
        // until the tap is invalidated below.
        let tap = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::ListenOnly,
                1u64 << CGEventType::FlagsChanged.0,
                Some(callback),
                ctx.cast(),
            )
        };
        let release = |tap: Option<&CFMachPort>| {
            if let Some(tap) = tap {
                CGEvent::tap_enable(tap, false);
                tap.invalidate();
            }
            // SAFETY: the tap is gone (or never existed), so no callback can
            // read the context any more; it was created by Box::into_raw above.
            drop(unsafe { Box::from_raw(ctx) });
        };
        let Some(tap) = tap else {
            // No Input Monitoring: the caller logs the warning naming the fix.
            release(None);
            let _ = ready.send(None);
            return;
        };
        let source = CFMachPort::new_run_loop_source(None, Some(&tap), 0);
        let runloop = CFRunLoop::current();
        let (Some(source), Some(runloop)) = (source, runloop) else {
            tracing::error!("listen key: tap setup failed");
            release(Some(&tap));
            let _ = ready.send(None);
            return;
        };
        // SAFETY: reading an immutable CoreFoundation constant.
        let modes = unsafe { kCFRunLoopCommonModes };
        runloop.add_source(Some(&source), modes);
        CGEvent::tap_enable(&tap, true);
        // SAFETY: only this thread writes the context, before the run loop runs.
        let _ = unsafe { &*ctx }.tap.set(tap.clone());
        let _ = ready.send(Some(Handles { tap: tap.clone(), runloop: runloop.clone() }));
        CFRunLoop::run();
        // Stopped: tear the tap down so its channel closes and the loop half
        // finishes once any pending edge is out.
        runloop.remove_source(Some(&source), modes);
        release(Some(&tap));
    }
}

#[cfg(not(target_os = "macos"))]
mod quartz {
    use tokio::runtime::Handle;

    use super::{input_monitoring_warning, ListenKeyTracker, Stopper};

    /// Only reachable when a caller passes platform "darwin" on a build that
    /// has no Quartz: no tap can be created, which is the tap-None path.
    pub(super) fn start<F>(_on_change: F, _handle: &Handle, _tracker: ListenKeyTracker) -> Option<Stopper>
    where
        F: FnMut(bool) + Send + 'static,
    {
        input_monitoring_warning();
        None
    }
}

/// Python's `str.isspace`, which `str.strip()` uses: Unicode White_Space plus
/// the information separators U+001C..U+001F.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python `repr()` of a str, for the "unknown EVE_LISTEN_KEY" log line and
/// the tracker's error text.
pub fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !py_isprintable(c) => {
                let n = c as u32;
                if n <= 0xff {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n <= 0xffff {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `str.isprintable` for the characters a stripped env value can
/// hold: control characters (Cc), separators other than space (Zs, Zl, Zp),
/// the common format characters (Cf), private use (Co) and noncharacters are
/// not printable.
fn py_isprintable(c: char) -> bool {
    let n = c as u32;
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    !matches!(
        n,
        0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x890..=0x891 | 0x8e2 | 0x180e
            | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x2064 | 0x2066..=0x206f
            | 0xfeff | 0xfff9..=0xfffb | 0x110bd | 0x110cd | 0x13430..=0x1343f
            | 0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a | 0xe0001 | 0xe0020..=0xe007f
            | 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd
            | 0xfdd0..=0xfdef
    ) && (n & 0xfffe) != 0xfffe
}

#[cfg(test)]
mod tests;
