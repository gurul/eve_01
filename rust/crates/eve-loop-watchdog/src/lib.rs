//! A watchdog for the daemon's event loop (port of `loop_watchdog.py`).
//!
//! Every stall of the loop looks the same from the outside: the log goes quiet, the
//! robot sits still, then everything that queued up lands in one burst. The 2026-09-21
//! wake-word stall (heard 'hey eve' → 19 s of nothing → "Yeah?") and the 41 s
//! hang right after the serial port opened both had that shape, and the log could not
//! say which call held the loop, because the loop is what writes the log.
//!
//! So a thread watches the loop from outside. The loop pets the watchdog every
//! quarter second from a task on the runtime; when the pets stop for longer than
//! `threshold`, the thread logs the loop thread's stack at WARNING — the one fact a
//! stall leaves nowhere else — and again every `repeat` seconds while it lasts. When
//! the loop comes back it logs how long it was gone.
//!
//! A dump handler is registered on SIGUSR1 as well, for a stall this thread cannot
//! see: `kill -USR1 <pid>` dumps every thread to stderr, which launchd points at the
//! daemon log.
//!
//! Reading another thread's stack: on macOS the watchdog suspends the loop thread
//! (Mach `thread_suspend`), reads its registers and walks the frame-pointer chain
//! into a preallocated buffer, resumes it, and only then symbolizes. Nothing that
//! allocates or locks runs while the thread is suspended, so a loop thread caught
//! holding the malloc lock cannot deadlock the watchdog.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, LazyLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

/// How often the loop reports in.
pub const PET_SECS: f64 = 0.25;
/// Silence longer than this is a stall.
pub const DEFAULT_THRESHOLD: f64 = 2.0;
/// While it lasts, log the stack again this often.
pub const DEFAULT_REPEAT: f64 = 10.0;
/// How often the watching thread looks (Python's `poll=0.5`).
pub const DEFAULT_POLL: f64 = 0.5;
/// The signal `register_dump_signal` uses by default (Python's `sig=signal.SIGUSR1`).
#[cfg(unix)]
pub const DUMP_SIGNAL: i32 = libc::SIGUSR1;
#[cfg(not(unix))]
pub const DUMP_SIGNAL: i32 = 10;

/// Deepest stack read from a thread.
const MAX_FRAMES: usize = 256;

const NO_FRAME: &str = "  (no frame for that thread)";
const NEVER_TICKED: &str = "  (loop never ticked)";

/// A clock in seconds; the watchdog's default is [`monotonic`].
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Monotonic seconds (Python's `time.monotonic`; only differences matter).
pub fn monotonic() -> f64 {
    static BASE: LazyLock<Instant> = LazyLock::new(Instant::now);
    BASE.elapsed().as_secs_f64()
}

// ---------------------------------------------------------------------------
// Threads and their stacks
// ---------------------------------------------------------------------------

/// A thread whose stack can be read from another thread (Python passes a
/// `threading.get_ident()` to `format_stack`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopThread {
    ident: usize,
    #[cfg(target_os = "macos")]
    port: u32,
    #[cfg(target_os = "macos")]
    stack_lo: usize,
    #[cfg(target_os = "macos")]
    stack_hi: usize,
}

impl LoopThread {
    /// The calling thread.
    pub fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: plain queries about the calling thread.
            unsafe {
                let me = libc::pthread_self();
                let hi = libc::pthread_get_stackaddr_np(me) as usize;
                let size = libc::pthread_get_stacksize_np(me);
                LoopThread {
                    ident: me as usize,
                    port: libc::pthread_mach_thread_np(me),
                    stack_lo: hi.saturating_sub(size),
                    stack_hi: hi,
                }
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            // SAFETY: pthread_self has no preconditions.
            LoopThread { ident: unsafe { libc::pthread_self() } as usize }
        }
        #[cfg(not(unix))]
        {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::thread::current().id().hash(&mut h);
            LoopThread { ident: h.finish() as usize }
        }
    }

    /// The thread's identity (the `pthread_t`, like Python's `threading.get_ident()`).
    pub fn ident(&self) -> usize {
        self.ident
    }

    fn is_current(&self) -> bool {
        Self::current().ident == self.ident
    }
}

/// One symbolized frame.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    name: String,
    file: String,
    line: u32,
}

/// Return addresses of `thread`, innermost first; `None` when its stack cannot be read.
fn capture(thread: &LoopThread) -> Option<Vec<usize>> {
    if thread.is_current() {
        let mut ips = Vec::with_capacity(MAX_FRAMES);
        backtrace::trace(|f| {
            ips.push(f.ip() as usize);
            ips.len() < MAX_FRAMES
        });
        return Some(ips);
    }
    #[cfg(target_os = "macos")]
    {
        mach::capture(thread.port, thread.stack_lo, thread.stack_hi)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Symbolize return addresses (innermost first); inlined calls become their own frames.
fn symbolize(ips: &[usize]) -> Vec<Frame> {
    let mut frames = Vec::new();
    for (i, &ip) in ips.iter().enumerate() {
        // A return address points after the call; step back into it for the right line.
        let addr = if i == 0 { ip } else { ip.saturating_sub(1) };
        let mut found: Vec<Frame> = Vec::new();
        let mut has_file = false;
        let mut sym_start: Option<usize> = None;
        backtrace::resolve(addr as *mut std::ffi::c_void, |sym| {
            if let Some(a) = sym.addr() {
                sym_start = Some(sym_start.map_or(a as usize, |s: usize| s.max(a as usize)));
            }
            let name = sym.name().map(|n| format!("{n:#}")).unwrap_or_else(|| format!("0x{ip:x}"));
            let file = match sym.filename() {
                Some(p) => {
                    has_file = true;
                    p.display().to_string()
                }
                None => "???".to_string(),
            };
            found.push(Frame { name, file, line: sym.lineno().unwrap_or(0) });
        });
        // Without debug info (system libraries) the symbol table can name the
        // wrong neighbour. Both answers are "the nearest symbol below": the one
        // that starts closer to the address is the right one, and the dynamic
        // linker's also names the image.
        if !has_file {
            if let Some((start, frame)) = dladdr_frame(addr) {
                if sym_start.is_none_or(|s| start > s) {
                    found = vec![frame];
                }
            }
        }
        if found.is_empty() {
            found.push(Frame { name: format!("0x{ip:x}"), file: "???".to_string(), line: 0 });
        }
        frames.extend(found);
    }
    frames
}

/// The exported symbol and image `dladdr` reports for an address.
#[cfg(unix)]
fn dladdr_frame(addr: usize) -> Option<(usize, Frame)> {
    // SAFETY: dladdr only reads loader tables; the strings it returns live as long as the image.
    unsafe {
        let mut info: libc::Dl_info = std::mem::zeroed();
        if libc::dladdr(addr as *const std::ffi::c_void, &mut info) == 0 || info.dli_sname.is_null() {
            return None;
        }
        let raw = std::ffi::CStr::from_ptr(info.dli_sname).to_string_lossy();
        let name = format!("{:#}", backtrace::SymbolName::new(raw.as_bytes()));
        let file = if info.dli_fname.is_null() {
            "???".to_string()
        } else {
            std::ffi::CStr::from_ptr(info.dli_fname).to_string_lossy().into_owned()
        };
        Some((info.dli_saddr as usize, Frame { name, file, line: 0 }))
    }
}

#[cfg(not(unix))]
fn dladdr_frame(_addr: usize) -> Option<(usize, Frame)> {
    None
}

/// The stack of one thread, innermost call last (like a traceback).
pub fn format_stack(thread: &LoopThread) -> String {
    let ips = match capture(thread) {
        Some(ips) if !ips.is_empty() => ips,
        _ => return NO_FRAME.to_string(),
    };
    let lines: Vec<String> = symbolize(&ips)
        .iter()
        .rev()
        .map(|f| format!("  File \"{}\", line {}, in {}", f.file, f.line, f.name))
        .collect();
    lines.join("\n").trim_end().to_string()
}

#[cfg(target_os = "macos")]
mod mach {
    //! Read a suspended thread's registers and walk its frame-pointer chain.
    use std::ffi::c_int;

    use libc::{kern_return_t, mach_port_t};

    use super::MAX_FRAMES;

    extern "C" {
        fn thread_suspend(target: mach_port_t) -> kern_return_t;
        fn thread_resume(target: mach_port_t) -> kern_return_t;
        fn thread_get_state(target: mach_port_t, flavor: c_int, state: *mut u32, count: *mut u32) -> kern_return_t;
        pub(crate) fn mach_port_deallocate(task: mach_port_t, name: mach_port_t) -> kern_return_t;
    }

    // ARM_THREAD_STATE64: x0..x28, fp, lr, sp, pc, cpsr+flags (68 u32 words).
    #[cfg(target_arch = "aarch64")]
    const FLAVOR: c_int = 6;
    #[cfg(target_arch = "aarch64")]
    const COUNT: u32 = 68;
    #[cfg(target_arch = "aarch64")]
    fn pc_fp(s: &[u64; 34]) -> (usize, usize) {
        (s[32] as usize, s[29] as usize)
    }
    #[cfg(target_arch = "aarch64")]
    const ALIGN: usize = 16;

    // x86_THREAD_STATE64: rax rbx rcx rdx rdi rsi rbp rsp r8..r15 rip … (42 u32 words).
    #[cfg(target_arch = "x86_64")]
    const FLAVOR: c_int = 4;
    #[cfg(target_arch = "x86_64")]
    const COUNT: u32 = 42;
    #[cfg(target_arch = "x86_64")]
    fn pc_fp(s: &[u64; 34]) -> (usize, usize) {
        (s[16] as usize, s[6] as usize)
    }
    #[cfg(target_arch = "x86_64")]
    const ALIGN: usize = 8;

    /// Strip pointer-authentication bits: user addresses fit in 47 bits.
    fn strip(addr: usize) -> usize {
        addr & 0x0000_7fff_ffff_ffff
    }

    /// Return addresses of the thread behind `port`, innermost first. The thread
    /// is suspended only while its registers and frames are copied into a buffer
    /// allocated beforehand: no allocation, no lock, no logging in that window.
    pub(crate) fn capture(port: mach_port_t, lo: usize, hi: usize) -> Option<Vec<usize>> {
        let mut ips: Vec<usize> = Vec::with_capacity(MAX_FRAMES);
        let mut state = [0u64; 34];
        let mut count = COUNT;
        // SAFETY: `port` names a thread of this task (or a dead name, which makes the
        // calls fail). The thread is not the caller (checked by `LoopThread::is_current`
        // and skipped in `dump_all_threads`). Memory is read only inside the thread's
        // own stack range, which stays mapped while it is suspended.
        unsafe {
            if thread_suspend(port) != 0 {
                return None;
            }
            let ok = thread_get_state(port, FLAVOR, state.as_mut_ptr().cast::<u32>(), &mut count) == 0;
            if ok {
                let (pc, mut fp) = pc_fp(&state);
                ips.push(strip(pc));
                while ips.len() < MAX_FRAMES
                    && fp >= lo
                    && fp.checked_add(16).is_some_and(|end| end <= hi)
                    && fp % ALIGN == 0
                {
                    let next = std::ptr::read_volatile(fp as *const usize);
                    let ret = strip(std::ptr::read_volatile((fp + 8) as *const usize));
                    if ret == 0 {
                        break;
                    }
                    ips.push(ret);
                    if next <= fp {
                        break;
                    }
                    fp = next;
                }
            }
            thread_resume(port);
            if !ok {
                return None;
            }
        }
        Some(ips)
    }
}

// ---------------------------------------------------------------------------
// The watchdog
// ---------------------------------------------------------------------------

/// One look by the watching thread.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Tick {
    /// The loop is ticking.
    Quiet,
    /// Silent for `quiet` seconds; `new_stall` on the first look of a stall,
    /// `report` when the stack is due to be logged.
    Stalled { quiet: f64, new_stall: bool, report: bool },
    /// The loop came back after `gone` seconds.
    Resumed { gone: f64 },
}

/// The watching thread's memory between looks (Python `_watch`'s locals).
#[derive(Debug, Default)]
struct WatchState {
    /// When the running stall was last logged.
    reported_at: Option<f64>,
    stalled_since: Option<f64>,
}

impl WatchState {
    fn tick(&mut self, now: f64, last_pet: f64, threshold: f64, repeat: f64) -> Tick {
        let quiet = now - last_pet;
        if quiet > threshold {
            let mut new_stall = false;
            if self.stalled_since.is_none() {
                self.stalled_since = Some(last_pet);
                new_stall = true;
            }
            let report = match self.reported_at {
                None => true,
                Some(at) => now - at >= repeat,
            };
            if report {
                self.reported_at = Some(now);
            }
            Tick::Stalled { quiet, new_stall, report }
        } else if let Some(since) = self.stalled_since {
            self.stalled_since = None;
            self.reported_at = None;
            Tick::Resumed { gone: last_pet - since }
        } else {
            Tick::Quiet
        }
    }
}

fn load_f64(a: &AtomicU64) -> f64 {
    f64::from_bits(a.load(Ordering::SeqCst))
}

fn store_f64(a: &AtomicU64, v: f64) {
    a.store(v.to_bits(), Ordering::SeqCst);
}

struct Inner {
    handle: tokio::runtime::Handle,
    threshold: f64,
    repeat: f64,
    clock: Clock,
    poll: f64,
    last_pet: AtomicU64,
    /// Every thread a pet has run on, first seen first. A current-thread runtime
    /// (asyncio's shape) has exactly one; a multi-thread runtime stalls only when
    /// all its workers are held, so each one seen is worth a stack.
    loop_threads: Mutex<Vec<LoopThread>>,
    stop: Mutex<bool>,
    stop_cv: Condvar,
    stopped: AtomicBool,
    stalls: AtomicU64,
    longest: AtomicU64,
    /// The tracing subscriber in effect where the watchdog was made; the
    /// watching thread logs through it.
    dispatch: tracing::Dispatch,
    thread: Mutex<Option<(JoinHandle<()>, mpsc::Receiver<()>)>>,
}

impl Inner {
    // -- loop side --
    fn pet(&self) {
        store_f64(&self.last_pet, (self.clock)());
        let me = LoopThread::current();
        let mut threads = self.loop_threads.lock();
        if !threads.iter().any(|t| t.ident == me.ident) {
            threads.push(me);
        }
    }

    // -- thread side --
    fn loop_stack(&self) -> String {
        let threads = self.loop_threads.lock().clone();
        match threads.as_slice() {
            [] => NEVER_TICKED.to_string(),
            [one] => format_stack(one),
            many => many
                .iter()
                .map(|t| format!("  thread 0x{:016x}:\n{}", t.ident, format_stack(t)))
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// Wait up to `poll` seconds for stop; true when stop is set (`Event.wait`).
    fn wait_stop(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs_f64(self.poll.max(0.0));
        let mut stop = self.stop.lock();
        while !*stop {
            if self.stop_cv.wait_until(&mut stop, deadline).timed_out() {
                break;
            }
        }
        *stop
    }

    fn watch(&self) {
        let mut state = WatchState::default();
        while !self.wait_stop() {
            let now = (self.clock)();
            match state.tick(now, load_f64(&self.last_pet), self.threshold, self.repeat) {
                Tick::Quiet => {}
                Tick::Stalled { quiet, new_stall, report } => {
                    if new_stall {
                        self.stalls.fetch_add(1, Ordering::SeqCst);
                    }
                    if report {
                        let stack = self.loop_stack();
                        tracing::warn!("event loop stalled for {quiet:.1} s — the loop thread is at:\n{stack}");
                    }
                }
                Tick::Resumed { gone } => {
                    store_f64(&self.longest, load_f64(&self.longest).max(gone));
                    tracing::warn!("event loop resumed after a {gone:.1} s stall");
                }
            }
        }
    }
}

/// Watch a tokio runtime from a thread; log its stack when it stops ticking.
///
/// The runtime must have its time driver enabled (`enable_all`/`enable_time`):
/// the pet task sleeps between pets.
#[derive(Clone)]
pub struct LoopWatchdog {
    inner: Arc<Inner>,
}

impl LoopWatchdog {
    /// A watchdog with Python's defaults: threshold 2 s, repeat 10 s, poll 0.5 s.
    pub fn new(handle: tokio::runtime::Handle) -> Self {
        Self::with_options(handle, DEFAULT_THRESHOLD, DEFAULT_REPEAT, Arc::new(monotonic), DEFAULT_POLL)
    }

    /// Python's `LoopWatchdog(loop, threshold, repeat, clock, poll)`.
    pub fn with_options(
        handle: tokio::runtime::Handle,
        threshold: f64,
        repeat: f64,
        clock: Clock,
        poll: f64,
    ) -> Self {
        let first = clock();
        LoopWatchdog {
            inner: Arc::new(Inner {
                handle,
                threshold,
                repeat,
                clock,
                poll,
                last_pet: AtomicU64::new(first.to_bits()),
                loop_threads: Mutex::new(Vec::new()),
                stop: Mutex::new(false),
                stop_cv: Condvar::new(),
                stopped: AtomicBool::new(false),
                stalls: AtomicU64::new(0),
                longest: AtomicU64::new(0f64.to_bits()),
                dispatch: tracing::dispatcher::get_default(|d| d.clone()),
                thread: Mutex::new(None),
            }),
        }
    }

    /// Start petting from the runtime and watching from a thread named `loop-watchdog`.
    pub fn start(self) -> std::io::Result<Self> {
        let inner = Arc::clone(&self.inner);
        self.inner.handle.spawn(async move {
            loop {
                inner.pet();
                if inner.stopped.load(Ordering::SeqCst) {
                    break;
                }
                tokio::time::sleep(Duration::from_secs_f64(PET_SECS)).await;
            }
        });
        let (done_tx, done_rx) = mpsc::channel();
        let inner = Arc::clone(&self.inner);
        let handle = std::thread::Builder::new().name("loop-watchdog".to_string()).spawn(move || {
            let dispatch = inner.dispatch.clone();
            tracing::dispatcher::with_default(&dispatch, || inner.watch());
            let _ = done_tx.send(());
        })?;
        *self.inner.thread.lock() = Some((handle, done_rx));
        Ok(self)
    }

    /// Stop petting and watching; waits up to four polls for the thread.
    pub fn stop(&self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        *self.inner.stop.lock() = true;
        self.inner.stop_cv.notify_all();
        let taken = self.inner.thread.lock().take();
        if let Some((handle, done)) = taken {
            if done.recv_timeout(Duration::from_secs_f64((self.inner.poll * 4.0).max(0.0))).is_ok() {
                let _ = handle.join();
            }
        }
    }

    /// Stalls seen.
    pub fn stalls(&self) -> u64 {
        self.inner.stalls.load(Ordering::SeqCst)
    }

    /// The longest stall, seconds.
    pub fn longest(&self) -> f64 {
        load_f64(&self.inner.longest)
    }

    pub fn threshold(&self) -> f64 {
        self.inner.threshold
    }

    pub fn repeat(&self) -> f64 {
        self.inner.repeat
    }

    /// The threads the loop has ticked on so far.
    pub fn loop_threads(&self) -> Vec<LoopThread> {
        self.inner.loop_threads.lock().clone()
    }
}

// ---------------------------------------------------------------------------
// The dump signal
// ---------------------------------------------------------------------------

/// `kill -<sig> <pid>` dumps every thread's stack to stderr, then hands the
/// signal to the handler that was there before (faulthandler's `chain=True`:
/// with no earlier handler, SIGUSR1's default action ends the process, exactly
/// as the Python daemon does).
///
/// `Ok(false)` where Python's `faulthandler.register` raised `ValueError` or
/// `RuntimeError` (a fatal signal, a number out of range) or where signals are
/// unsupported; `Err` where it raised `OSError` (sigaction refused, e.g. SIGKILL).
pub fn register_dump_signal(sig: i32) -> std::io::Result<bool> {
    #[cfg(target_os = "macos")]
    {
        dump::register(sig)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = sig;
        Ok(false)
    }
}

/// Every thread's stack, most recent call first, in faulthandler's layout.
/// `current` marks the thread that took the signal.
pub fn dump_all_threads(current: Option<&LoopThread>) -> String {
    #[cfg(target_os = "macos")]
    {
        dump::dump_all_threads(current.map(|t| t.port))
    }
    #[cfg(not(target_os = "macos"))]
    {
        match current {
            Some(t) => format!(
                "Current thread 0x{:016x} (most recent call first):\n{}",
                t.ident,
                faulthandler_frames(&capture(t).unwrap_or_default())
            ),
            None => String::new(),
        }
    }
}

/// Frames in faulthandler's layout, most recent call first.
fn faulthandler_frames(ips: &[usize]) -> String {
    if ips.is_empty() {
        return "  <no frame>\n".to_string();
    }
    symbolize(ips)
        .iter()
        .map(|f| format!("  File \"{}\", line {} in {}\n", f.file, f.line, f.name))
        .collect()
}

#[cfg(target_os = "macos")]
mod dump {
    use std::ffi::{c_int, c_void};
    use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
    use std::sync::OnceLock;

    use libc::mach_port_t;

    use super::mach::mach_port_deallocate;

    const NSIG: usize = 32;
    /// Signals faulthandler keeps for `enable()` and refuses to `register`.
    const FATAL: [c_int; 5] = [libc::SIGSEGV, libc::SIGFPE, libc::SIGABRT, libc::SIGBUS, libc::SIGILL];
    /// How long the handler waits for the dump before chaining.
    const WAIT_MS: u32 = 3000;

    static PREVIOUS: [OnceLock<libc::sigaction>; NSIG] = [const { OnceLock::new() }; NSIG];
    static PIPE: OnceLock<(c_int, c_int)> = OnceLock::new();
    static INSTALL: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    static REQUESTS: AtomicU64 = AtomicU64::new(0);
    static DONE: AtomicU64 = AtomicU64::new(0);
    static CURRENT: AtomicU32 = AtomicU32::new(0);
    /// Where the dump goes: stderr (tests point it elsewhere).
    pub(crate) static DUMP_FD: AtomicI32 = AtomicI32::new(2);

    fn our_action() -> libc::sigaction {
        // SAFETY: an all-zero sigaction is a valid starting value.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = on_signal as extern "C" fn(c_int) as usize;
        // SA_NODEFER as faulthandler's chain mode sets (the re-raise must land
        // inside the handler); SA_RESTART kept, see PARITY.md.
        action.sa_flags = libc::SA_NODEFER | libc::SA_RESTART;
        action
    }

    /// The handler: wake the dumper thread, wait for the dump, then chain.
    /// Only async-signal-safe calls (write, nanosleep, sigaction, raise).
    extern "C" fn on_signal(sig: c_int) {
        // SAFETY: async-signal-safe libc calls on statics initialised before install.
        unsafe {
            let saved = *libc::__error();
            if let Some(&(_, w)) = PIPE.get() {
                CURRENT.store(libc::pthread_mach_thread_np(libc::pthread_self()), Ordering::SeqCst);
                let gen = REQUESTS.fetch_add(1, Ordering::SeqCst) + 1;
                let byte = 1u8;
                if libc::write(w, (&byte as *const u8).cast::<c_void>(), 1) == 1 {
                    let pause = libc::timespec { tv_sec: 0, tv_nsec: 1_000_000 };
                    for _ in 0..WAIT_MS {
                        if DONE.load(Ordering::SeqCst) >= gen {
                            break;
                        }
                        libc::nanosleep(&pause, std::ptr::null_mut());
                    }
                }
            }
            if let Some(prev) = PREVIOUS.get(sig as usize).and_then(|p| p.get()) {
                libc::sigaction(sig, prev, std::ptr::null_mut());
                libc::raise(sig);
                let ours = our_action();
                libc::sigaction(sig, &ours, std::ptr::null_mut());
            }
            *libc::__error() = saved;
        }
    }

    fn write_all(fd: c_int, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            // SAFETY: writing from a live slice.
            let n = unsafe { libc::write(fd, bytes.as_ptr().cast::<c_void>(), bytes.len()) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            bytes = &bytes[n as usize..];
        }
    }

    /// The dumper thread: never takes a signal itself, dumps on each byte.
    fn dumper(read_fd: c_int) {
        // SAFETY: masking signals for this thread only.
        unsafe {
            let mut all: libc::sigset_t = std::mem::zeroed();
            libc::sigfillset(&mut all);
            libc::pthread_sigmask(libc::SIG_BLOCK, &all, std::ptr::null_mut());
        }
        let mut byte = [0u8; 1];
        loop {
            // SAFETY: reading into a live buffer.
            let n = unsafe { libc::read(read_fd, byte.as_mut_ptr().cast::<c_void>(), 1) };
            if n <= 0 {
                if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            let gen = REQUESTS.load(Ordering::SeqCst);
            let current = CURRENT.load(Ordering::SeqCst);
            let text = dump_all_threads(Some(current));
            write_all(DUMP_FD.load(Ordering::SeqCst), text.as_bytes());
            DONE.store(gen, Ordering::SeqCst);
        }
    }

    fn ensure_dumper() -> std::io::Result<()> {
        if PIPE.get().is_some() {
            return Ok(());
        }
        let mut fds = [0 as c_int; 2];
        // SAFETY: pipe fills a two-int array.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        for fd in fds {
            // SAFETY: fds just opened.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        let read_fd = fds[0];
        if let Err(e) = std::thread::Builder::new().name("loop-watchdog-dump".to_string()).spawn(move || dumper(read_fd)) {
            // SAFETY: closing the fds this call opened.
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(e);
        }
        let _ = PIPE.set((fds[0], fds[1]));
        Ok(())
    }

    pub(crate) fn register(sig: c_int) -> std::io::Result<bool> {
        if FATAL.contains(&sig) || sig < 1 || sig as usize >= NSIG {
            return Ok(false);
        }
        let _guard = INSTALL.lock();
        ensure_dumper()?;
        // SAFETY: querying then installing a signal action with valid pointers.
        unsafe {
            let mut prev: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, std::ptr::null(), &mut prev) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let ours = our_action();
            // Re-registering keeps the handler from before the first registration.
            if prev.sa_sigaction != ours.sa_sigaction {
                let _ = PREVIOUS[sig as usize].set(prev);
            }
            if libc::sigaction(sig, &ours, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(true)
    }

    #[allow(deprecated)] // libc points at the mach2 crate for mach_task_self; the call is the same
    pub(crate) fn dump_all_threads(current: Option<mach_port_t>) -> String {
        let mut out = String::new();
        // SAFETY: task_threads hands back an array of `count` ports that this
        // function owns: each port and the array are released below.
        unsafe {
            let task = libc::mach_task_self();
            let mut list: libc::thread_act_array_t = std::ptr::null_mut();
            let mut count: libc::mach_msg_type_number_t = 0;
            if libc::task_threads(task, &mut list, &mut count) != 0 || list.is_null() {
                return out;
            }
            let ports: Vec<mach_port_t> = std::slice::from_raw_parts(list, count as usize).to_vec();
            let me = libc::pthread_mach_thread_np(libc::pthread_self());
            // Read every stack before symbolizing any: the first symbolization loads debug
            // info and can take seconds, long enough for a short-lived thread to exit before
            // its turn and leave "<no frame>" (2026-10-20, full-workspace test run).
            let mut snapshot: Vec<(mach_port_t, usize, Vec<usize>)> = Vec::with_capacity(ports.len());
            for &port in &ports {
                if port == me {
                    continue;
                }
                let pt = libc::pthread_from_mach_thread_np(port);
                let (ident, lo, hi) = if pt == 0 {
                    (port as usize, 0, 0)
                } else {
                    let hi = libc::pthread_get_stackaddr_np(pt) as usize;
                    (pt as usize, hi.saturating_sub(libc::pthread_get_stacksize_np(pt)), hi)
                };
                let ips = super::mach::capture(port, lo, hi).unwrap_or_default();
                snapshot.push((port, ident, ips));
            }
            let mut first = true;
            for (port, ident, ips) in &snapshot {
                if !first {
                    out.push('\n');
                }
                first = false;
                let label = if Some(*port) == current { "Current thread" } else { "Thread" };
                out.push_str(&format!("{label} 0x{ident:016x} (most recent call first):\n"));
                out.push_str(&super::faulthandler_frames(ips));
            }
            for &port in &ports {
                mach_port_deallocate(task, port);
            }
            libc::vm_deallocate(
                task,
                list as libc::vm_address_t,
                (count as usize * std::mem::size_of::<mach_port_t>()) as libc::vm_size_t,
            );
        }
        out
    }
}

#[cfg(test)]
mod tests;
