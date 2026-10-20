//! The loop watchdog: a stalled event loop gets its stack logged; a healthy one stays quiet.
//! Port of `bridge/tests/test_loop_watchdog.py`, plus the watch decisions and the dump signal.

use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

use super::*;

const LOGGER: &str = "eve_loop_watchdog";

/// Collects the messages this crate logs at WARNING or above (pytest's `caplog`).
#[derive(Clone, Default)]
struct Caplog(Arc<StdMutex<Vec<String>>>);

struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for Caplog {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if meta.target().starts_with(LOGGER) && *meta.level() <= tracing::Level::WARN {
            let mut v = MessageVisitor(String::new());
            event.record(&mut v);
            self.0.lock().unwrap().push(v.0);
        }
    }
}

impl Caplog {
    fn messages(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

fn with_caplog<T>(f: impl FnOnce() -> T) -> (T, Caplog) {
    let cap = Caplog::default();
    let subscriber = tracing_subscriber::registry().with(cap.clone());
    let out = tracing::subscriber::with_default(subscriber, f);
    (out, cap)
}

/// Symbolize once before a timed test. The first symbolization in a process loads the binary's
/// debug info and takes seconds in a debug build, so a stall's stack would be logged after
/// `stop()` had stopped waiting, and a short-lived thread would exit before a dump reached it.
/// The daemon pays that once, on its first stall; the tests pay it here, outside the timing.
fn warm_symbolizer() {
    static WARM: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARM.get_or_init(|| {
        let _ = format_stack(&LoopThread::current());
    });
}

fn current_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// The kind of call the watchdog exists to name: synchronous, on the loop thread.
#[inline(never)]
fn hold_the_loop(secs: f64) {
    std::thread::sleep(Duration::from_secs_f64(secs));
    std::hint::black_box(());
}

fn run<F, Fut>(body: F, threshold: f64, poll: f64) -> LoopWatchdog
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    // Well above PET_SECS (0.25 s), so an ordinary gap between pets under load is never a stall.
    let rt = current_thread_runtime();
    let wd = LoopWatchdog::with_options(rt.handle().clone(), threshold, DEFAULT_REPEAT, Arc::new(monotonic), poll);
    let wd2 = wd.clone();
    rt.block_on(async move {
        let wd = wd2.start().unwrap();
        tokio::time::sleep(Duration::from_secs_f64(0.3)).await; // a few pets land first
        body().await;
        tokio::time::sleep(Duration::from_secs_f64(0.3)).await; // and the loop is seen ticking again
        wd.stop();
    });
    wd
}

#[test]
fn test_a_stall_logs_the_loop_threads_stack() {
    warm_symbolizer();
    let (wd, caplog) = with_caplog(|| {
        run(
            || async {
                hold_the_loop(0.8);
            },
            0.45,
            0.05,
        )
    });
    let msgs = caplog.messages();
    let stalled: Vec<&String> = msgs.iter().filter(|m| m.contains("event loop stalled")).collect();
    assert!(!stalled.is_empty(), "{msgs:?}");
    // The stack names the call that held the loop, not just the fact of a stall.
    assert!(stalled[0].contains("hold_the_loop"), "{}", stalled[0]);
    assert!(stalled[0].contains("sleep"), "{}", stalled[0]);
    assert!(msgs.iter().any(|m| m.contains("resumed after")), "{msgs:?}");
    assert_eq!(wd.stalls(), 1);
    assert!((0.7..=1.5).contains(&wd.longest()), "{}", wd.longest());
}

#[test]
fn test_a_healthy_loop_logs_nothing() {
    let (wd, caplog) = with_caplog(|| {
        run(
            || async {
                for _ in 0..10 {
                    tokio::time::sleep(Duration::from_secs_f64(0.05)).await; // busy, but never holding the loop
                }
            },
            0.45,
            0.05,
        )
    });
    assert_eq!(wd.stalls(), 0);
    assert!(caplog.messages().is_empty(), "{:?}", caplog.messages());
}

#[test]
fn test_a_long_stall_is_reported_again_while_it_lasts() {
    warm_symbolizer();
    // The threshold must sit clearly above PET_SECS (0.25 s): at 0.2 an ordinary gap between two pets already
    // read as a stall, so a little scheduling jitter around the real one counted a second stall (1 run in 7,
    // 2026-09-23). A 1.0 s hold still crosses 0.4 and repeats every 0.3 s, so it is reported at least twice.
    const { assert!(PET_SECS < 0.4) };
    let (wd, caplog) = with_caplog(|| {
        let rt = current_thread_runtime();
        let wd = LoopWatchdog::with_options(rt.handle().clone(), 0.4, 0.3, Arc::new(monotonic), 0.05);
        let wd2 = wd.clone();
        rt.block_on(async move {
            let wd = wd2.start().unwrap();
            tokio::time::sleep(Duration::from_secs_f64(0.2)).await;
            hold_the_loop(1.0);
            tokio::time::sleep(Duration::from_secs_f64(0.2)).await;
            wd.stop();
        });
        wd
    });
    let stalled: Vec<String> =
        caplog.messages().into_iter().filter(|m| m.contains("event loop stalled")).collect();
    assert!(stalled.len() >= 2, "{:?}", stalled.iter().map(|m| m.chars().take(60).collect::<String>()).collect::<Vec<_>>());
    assert_eq!(wd.stalls(), 1);
}

#[test]
fn test_dump_signal_registers_on_this_platform() {
    assert!(register_dump_signal(DUMP_SIGNAL).unwrap());
    // Registering again keeps working (faulthandler re-registers the same way).
    assert!(register_dump_signal(DUMP_SIGNAL).unwrap());
}

// -- beyond the Python file --

#[test]
fn test_watch_state_follows_the_python_branches() {
    let mut s = WatchState::default();
    assert_eq!(s.tick(10.0, 9.9, 2.0, 10.0), Tick::Quiet);
    // Quiet for exactly the threshold is not a stall (`quiet > threshold`).
    assert_eq!(s.tick(12.0, 10.0, 2.0, 10.0), Tick::Quiet);
    assert_eq!(s.tick(12.5, 10.0, 2.0, 10.0), Tick::Stalled { quiet: 2.5, new_stall: true, report: true });
    assert_eq!(s.tick(13.0, 10.0, 2.0, 10.0), Tick::Stalled { quiet: 3.0, new_stall: false, report: false });
    // Reported again once `repeat` has passed since the last report (>=).
    assert_eq!(s.tick(22.5, 10.0, 2.0, 10.0), Tick::Stalled { quiet: 12.5, new_stall: false, report: true });
    // Back: gone is measured from the stall's last pet to the new one.
    assert_eq!(s.tick(23.0, 22.9, 2.0, 10.0), Tick::Resumed { gone: 12.899999999999999 });
    assert_eq!(s.tick(23.1, 23.0, 2.0, 10.0), Tick::Quiet);
    // A second stall counts afresh and reports at once.
    assert_eq!(s.tick(30.0, 23.0, 2.0, 10.0), Tick::Stalled { quiet: 7.0, new_stall: true, report: true });
}

#[test]
fn test_a_watchdog_that_never_ticked_says_so() {
    // A runtime that is never driven: the pet task never runs.
    let rt = current_thread_runtime();
    let wd = LoopWatchdog::with_options(rt.handle().clone(), 0.1, 10.0, Arc::new(monotonic), 0.05);
    assert_eq!(wd.inner.loop_stack(), NEVER_TICKED);
    let ((), caplog) = with_caplog(|| {
        let wd = LoopWatchdog::with_options(rt.handle().clone(), 0.1, 10.0, Arc::new(monotonic), 0.05).start().unwrap();
        std::thread::sleep(Duration::from_millis(400));
        wd.stop();
        assert_eq!(wd.stalls(), 1);
    });
    let msgs = caplog.messages();
    assert!(msgs.iter().any(|m| m.contains("(loop never ticked)")), "{msgs:?}");
}

#[test]
fn test_an_injected_clock_drives_the_stall() {
    // Python's `clock=` seam: a clock that jumps makes a stall without holding anything.
    let now = Arc::new(StdMutex::new(100.0f64));
    let clock_now = Arc::clone(&now);
    let clock: Clock = Arc::new(move || *clock_now.lock().unwrap());
    let rt = current_thread_runtime();
    let ((), caplog) = with_caplog(|| {
        let wd = LoopWatchdog::with_options(rt.handle().clone(), 2.0, 10.0, clock, 0.02).start().unwrap();
        *now.lock().unwrap() = 105.0;
        std::thread::sleep(Duration::from_millis(200));
        wd.stop();
        assert_eq!(wd.stalls(), 1);
    });
    let msgs = caplog.messages();
    assert!(msgs.iter().any(|m| m.starts_with("event loop stalled for 5.0 s — the loop thread is at:\n")), "{msgs:?}");
}

#[test]
fn test_format_stack_of_the_calling_thread_names_this_test() {
    let s = format_stack(&LoopThread::current());
    assert!(s.contains("test_format_stack_of_the_calling_thread_names_this_test"), "{s}");
    assert!(s.starts_with("  File \""), "{s}");
}

#[cfg(target_os = "macos")]
#[test]
fn test_format_stack_reads_another_thread() {
    warm_symbolizer();
    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || {
        tx.send(LoopThread::current()).unwrap();
        hold_the_loop(0.6);
    });
    let target = rx.recv().unwrap();
    std::thread::sleep(Duration::from_millis(150));
    let s = format_stack(&target);
    t.join().unwrap();
    assert!(s.contains("hold_the_loop"), "{s}");
    // Innermost call last: the sleep sits below hold_the_loop.
    let hold = s.find("hold_the_loop").unwrap();
    let sleep = s.rfind("sleep").unwrap();
    assert!(sleep > hold, "{s}");
}

#[cfg(target_os = "macos")]
#[test]
fn test_dump_signal_rejects_what_faulthandler_rejects() {
    for sig in [libc::SIGSEGV, libc::SIGFPE, libc::SIGABRT, libc::SIGBUS, libc::SIGILL, 0, -1, 32, 999] {
        assert!(!register_dump_signal(sig).unwrap(), "{sig}");
    }
    // sigaction refuses SIGKILL: Python's OSError, which register_dump_signal does not catch.
    assert!(register_dump_signal(libc::SIGKILL).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn test_dump_all_threads_shows_every_thread_most_recent_first() {
    warm_symbolizer();
    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || {
        tx.send(LoopThread::current()).unwrap();
        hold_the_loop(0.6);
    });
    let target = rx.recv().unwrap();
    std::thread::sleep(Duration::from_millis(150));
    let text = dump_all_threads(Some(&target));
    t.join().unwrap();
    let current = format!("Current thread 0x{:016x} (most recent call first):\n", target.ident());
    let at = text.find(&current).unwrap_or_else(|| panic!("{text}"));
    let section = &text[at..];
    let section = &section[..section[1..].find("\nThread 0x").map(|i| i + 1).unwrap_or(section.len())];
    // Most recent call first: the sleep comes before hold_the_loop.
    let sleep = section.find("sleep").unwrap_or_else(|| panic!("{section}"));
    let hold = section.find("hold_the_loop").unwrap_or_else(|| panic!("{section}"));
    assert!(sleep < hold, "{section}");
    assert!(section.contains(" in "), "{section}");
}

#[cfg(target_os = "macos")]
#[test]
fn test_dump_signal_dumps_then_chains_to_the_earlier_handler() {
    warm_symbolizer();
    use std::sync::atomic::AtomicUsize;
    use std::io::Read;

    static EARLIER: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn earlier(_sig: libc::c_int) {
        EARLIER.fetch_add(1, Ordering::SeqCst);
    }
    let sig = libc::SIGUSR2;
    let mut file = tempfile_in_target();
    // SAFETY: installing a plain handler for SIGUSR2, which only this test raises.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = earlier as extern "C" fn(libc::c_int) as usize;
        assert_eq!(libc::sigaction(sig, &action, std::ptr::null_mut()), 0);
    }
    assert!(register_dump_signal(sig).unwrap());
    {
        use std::os::fd::AsRawFd;
        dump::DUMP_FD.store(file.as_raw_fd(), Ordering::SeqCst);
    }
    // SAFETY: raising a signal whose handler was installed above.
    unsafe { libc::raise(sig) };
    dump::DUMP_FD.store(2, Ordering::SeqCst);
    assert_eq!(EARLIER.load(Ordering::SeqCst), 1);
    let mut text = String::new();
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(0)).unwrap();
    file.read_to_string(&mut text).unwrap();
    assert!(text.contains("Current thread 0x"), "{text}");
    assert!(text.contains("(most recent call first):"), "{text}");
    assert!(text.contains("test_dump_signal_dumps_then_chains_to_the_earlier_handler"), "{text}");
}

#[cfg(target_os = "macos")]
fn tempfile_in_target() -> std::fs::File {
    let path = std::env::temp_dir().join(format!("eve-loop-watchdog-dump-{}.txt", std::process::id()));
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    file
}
