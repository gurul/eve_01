# eve-loop-watchdog
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/loop_watchdog.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `PET_SECS`, `DEFAULT_THRESHOLD`, `DEFAULT_REPEAT` | same `pub const`s | 0.25, 2.0, 10.0 |
| `poll=0.5` default | `DEFAULT_POLL` | |
| `register_dump_signal`'s `sig=signal.SIGUSR1` | `DUMP_SIGNAL` | |
| `time.monotonic` (default clock) | `monotonic()`, `Clock` | Seconds since first use; only differences are read |
| `format_stack` | `format_stack(&LoopThread) -> String` | Innermost call last, one `  File "<file>", line <n>, in <name>` line per frame, `rstrip`ped; `  (no frame for that thread)` when the stack cannot be read. Python's thread ident becomes `LoopThread` (`LoopThread::current()`, `.ident()` is the `pthread_t`). The calling thread is read with `backtrace::trace`; another thread by Mach suspend + frame-pointer walk (see module docs) |
| `LoopWatchdog` | `LoopWatchdog` | `new(Handle)` = Python defaults; `with_options(handle, threshold, repeat, clock, poll)` = the full constructor. The asyncio loop becomes a tokio `runtime::Handle` (time driver required) |
| `LoopWatchdog._pet` | `Inner::pet` + the pet task spawned by `start` | `call_soon` + `call_later(PET_SECS)` chain becomes one task: pet, stop if stopped, sleep `PET_SECS` |
| `LoopWatchdog._watch` | `Inner::watch` + `WatchState::tick` | The branch logic (`quiet > threshold`, first look counts the stall, report when never reported or `now - reported_at >= repeat`, resume measures `last_pet - stalled_since`) is a pure `tick` so it is tested without timing. Messages verbatim: `event loop stalled for %.1f s — the loop thread is at:\n<stack>` and `event loop resumed after a %.1f s stall` at WARN; `  (loop never ticked)` before the first pet |
| `LoopWatchdog.start` | `start(self) -> io::Result<Self>` | Python returns `self` and raises if the thread cannot start; Rust returns the error. Thread name `loop-watchdog` |
| `LoopWatchdog.stop` | `stop(&self)` | Sets stop, waits up to `poll * 4` for the thread, joins it only if it finished (Python's `join(timeout)`) |
| `stalls`, `longest`, `threshold`, `repeat` attributes | `stalls()`, `longest()`, `threshold()`, `repeat()` | `loop_threads()` also lists the threads the loop has ticked on |
| `register_dump_signal` | `register_dump_signal(sig) -> io::Result<bool>` | `faulthandler.register(sig, all_threads=True, chain=True)`: `Ok(true)` installed; `Ok(false)` where Python caught `ValueError`/`RuntimeError` (SIGSEGV, SIGFPE, SIGABRT, SIGBUS, SIGILL; `sig < 1` or `>= NSIG`) or off macOS; `Err` where Python's `OSError` escaped (sigaction refused, e.g. SIGKILL) |
| (faulthandler's dump) | `dump_all_threads(Option<&LoopThread>) -> String` | faulthandler's layout: `Thread 0x%016x (most recent call first):` / `Current thread 0x…` for the thread that took the signal, `  File "<file>", line <n> in <name>` frames, a blank line between threads |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_loop_watchdog.py` | `test_a_stall_logs_the_loop_threads_stack`, `test_a_healthy_loop_logs_nothing`, `test_a_long_stall_is_reported_again_while_it_lasts`, `test_dump_signal_registers_on_this_platform` | same names in `src/tests.rs`, plus `test_watch_state_follows_the_python_branches`, `test_a_watchdog_that_never_ticked_says_so`, `test_an_injected_clock_drives_the_stall`, `test_format_stack_of_the_calling_thread_names_this_test`, `test_format_stack_reads_another_thread`, `test_dump_signal_rejects_what_faulthandler_rejects`, `test_dump_all_threads_shows_every_thread_most_recent_first`, `test_dump_signal_dumps_then_chains_to_the_earlier_handler` | None. `caplog` is a tracing layer installed with `with_default`; the watchdog logs through the subscriber in effect where it was made, so the watching thread's records reach it. The stack assertion checks for `hold_the_loop` and `sleep` (Python: `_hold_the_loop` and `time.sleep`) |

## Boundaries
- Reading another thread's stack and the dump signal are macOS-only (the daemon's
  only platform). Off macOS, `format_stack` of another thread returns
  `  (no frame for that thread)` and `register_dump_signal` returns `Ok(false)`;
  Python could read stacks on Linux too.

## Differences
- Stacks are native frames (Rust functions, tokio internals, system libraries), not
  Python frames, and carry no source line text. Frames without debug info show the
  image path and `line 0`.
- On a multi-thread tokio runtime the pet can run on several workers; every worker
  it has run on is kept, and a stall logs each one's stack under
  `  thread 0x<pthread_t>:`. A current-thread runtime (asyncio's shape) logs the
  single stack exactly as Python does.
- The dump is written by a helper thread (`loop-watchdog-dump`) that the signal
  handler wakes through a pipe and waits for (up to 3 s), because symbolizing is
  not async-signal-safe. It covers every thread of the process, not only those
  running Python code. Every thread's stack is read before any is symbolized, so a
  slow first symbolization (seconds, while debug info loads) cannot let a thread exit
  before its turn; the text is the same. After the dump the signal is chained exactly as faulthandler
  does: the previous handler is restored, the signal re-raised, and ours put back;
  with no previous handler SIGUSR1's default action ends the process, as the
  Python daemon does (`kill -USR1` dumps, then exits with status 158).
- The handler is installed with `SA_NODEFER | SA_RESTART`; faulthandler's chain mode
  sets only `SA_NODEFER`. Python retries `EINTR` everywhere (PEP 475); Rust code does
  not, so restarting interrupted system calls keeps a dump from turning into I/O
  errors elsewhere in the daemon.
