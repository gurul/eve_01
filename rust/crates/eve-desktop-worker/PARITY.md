# eve-desktop-worker
Status: complete

The desktop worker: the daemon's child process that runs the planner's `exec_py` code, plus the planner's
`lane_first`, `outline` and `run_plan` operations. `cargo test -p eve-desktop-worker`: 48 tests pass.

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/desktop_worker.py` | `src/lib.rs` (the port), `src/host.rs` + `src/exec_host.py` (the exec host, see Boundaries), `src/quartz.rs` (the macOS calls), `src/main.rs` (the `eve-desktop-worker` binary, Python's `python -m eve_bridge.desktop_worker`); tests in `src/tests.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `MAX_TEXT_BYTES`, `MAX_IMAGE_BYTES`, `MAX_ITEMS`, `MAX_CODE_BYTES`, `MAX_REQUEST_BYTES` | same `pub const`s | 60 KiB, 8 MiB, 250, 64 KiB, 512 KiB |
| `emit` | `emit(&Value)` | `json.dumps` bytes via `eve_jev::py_dumps` (Python separators, `ensure_ascii`), one line, flushed |
| `check_desktop` | `check_desktop(&Namespace) -> (w, h)` | Same messages. `pyautogui.size()` runs in the exec host; on macOS the Accessibility / Screen Recording checks run for the host interpreter (messages name its path, as Python's named `sys.executable`) and then for this process (messages name the worker binary), since both touch the screen |
| `normalize_screenshots` | `normalize_screenshots(&Namespace)` | Runs in the exec host on Pillow images: resize to `pyautogui.size()` with LANCZOS, then `region=`, then `imageFilename=` |
| `execute` | `execute(code, &Namespace, Option<&mut Helpers>) -> Result<Value>` | `{"output": …}` / `{"error": {"code": "failsafe", …}}`, `timing` and `fast_lane` as in Python. Err only when the exec host is gone (`HostDied`) |
| `execute.log` | `Output::log` (+ the host's `log`, which joins `str()` of its arguments) | ValueError `text output exceeds its size limit`; `text_bytes` counts before the check, as Python's did |
| `execute.display` | `Output::display_png` / `Output::display_image` (+ the host's `display`, which PNG-encodes a Pillow image) | TypeError `display() expects a Pillow image or PNG bytes`, ValueError `image output exceeds its size limit`; encode time counts under `timing.encode` |
| `execute.Writer` | the host's `_Writer` | stdout and stderr of the model's code become text items (`text.rstrip("\n")` when `text.strip()`) |
| `fast_lane_config` | `fast_lane_config(&Env) -> (bool, String)` | `Env` = `HashMap<String, String>` (Python's mapping); `process_env()` reads `os.environ` |
| `lane_modes` | `lane_modes(&Env) -> (String, bool)` | |
| `DECIDER_BACKENDS` | `DECIDER_BACKENDS` | `["jev"]` |
| `decider_backend` | `decider_backend(&Env) -> String` | |
| `start_fast_lane` | `start_fast_lane(&SharedHelpers, &Env, Option<Loader>, thread) -> String`; `start_fast_lane_on(platform, …)` | `SharedHelpers` = `Arc<Mutex<Helpers>>`: the loader thread sets `decider` and `fast_lane_status` on it as Python's thread set them on the object. `Loader` = `FnOnce(style) -> Result<Loaded, LaneError>`; `jev_loader(env, opener)` is `jev.load(env=env, style=style)` (opener = eve_jev's socket seam). Status strings verbatim, `[:200]` on failures |
| `start_jev_asker` | `start_jev_asker(&mut Helpers, &Env) -> String` | `eve_jev::route_config` / `timeout_from_env` / `make_predict` + `eve_typed_ask::make_step_asker(…, perf_counter, env)`; `off (…)` cut to 160 characters |
| `outline` | `outline(&Value, Option<&mut Helpers>) -> Value` | `{"outline": …}`; an error is `{"app": "", "lines": [], "error": "<Kind>: <msg>"[:200]}` |
| `run_plan` | `run_plan(&Value, Option<&mut Helpers>) -> Result<Value, LaneError>`; `run_plan_args(&Value) -> RunPlanArgs` | `str(req.get("request") or "")`, `int(req.get("start") or 0)` (`py_int_or_zero`, Python's `int()` on str / float / bool with its ValueError / TypeError texts), `approved` when a dict, `bool()` of `dry_run` / `success_rejected`. Err is the exception Python let escape (a `start` `int()` refuses raised again in the `except`), which ends the worker |
| `lane_first` | `lane_first(&Value, Option<&mut Helpers>) -> Value` | |
| `serve` | `serve(&Namespace, impl BufRead, Option<&SharedHelpers>, &mut dyn FnMut(&Value)) -> Result<()>` | `emit` is a parameter (the tests' monkeypatch); same order of checks (size, JSON, terminal fail-safe, operation, code) and the same error bodies |
| `release_inputs` | `release_inputs(&mut impl Quartz) -> usize`; trait `Quartz`; `SystemQuartz` (macOS) | 128 key-ups then left / right / other mouse-ups at the cursor, all on `kCGHIDEventTap`; returns 131. `K_CG_*` constants carry the Quartz values |
| `main` | `main_with(check)` + `src/main.rs` | Namespace `{"__builtins__", "pyautogui", "time"}`, FAILSAFE on, PAUSE 0.05, Helpers on the real backends, lane + asker, install, the ready line `{"ready", "platform", "width", "height", "fast_lane", "jev_asker"}`, `--check`, then serve. Any failure is `{"error": {"code": "startup", "message": "<Kind>: <msg>"[:4000]}}` and exit 1 |
| `main.watch_parent` | `watch_parent` (private) | SIGKILL itself when the parent pid changes, checked every second; the exec host watches the worker the same way |
| `__main__ --release` | `eve-desktop-worker --release` | exit 0 after posting; off macOS `ModuleNotFoundError: No module named 'Quartz'`, exit 1 (Python's import failure) |
| (`Helpers.install(namespace)`) | `install(&mut Helpers, &Namespace)` | Binds `Helpers::install()`'s names into the namespace, drops `delegate` when the lane is off, wraps every `ACTION_NAMES` PyAutoGUI call with the acting wrap (once) |
| (the helpers bound in the namespace) | `call_helper(&mut Helpers, name, kwargs)` + the host's stubs | The stubs carry the Python signatures (defaults, keyword-only `gone`, …) so a wrong call is Python's own TypeError; `delegate` makes Python's `str()` / `int()` conversions in the stub. Return values are the Python values (dicts for `frontmost`, `screen_text` items, `find_text`, `wait_for`'s hit; `True`/`False`/`None` for `wait_for`, `wait_settled`). A helper's error comes back as its Python class (builtins by name, others as a class of that name), and a PyAutoGUI exception raised under a helper as the original object |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_desktop_worker.py` | all 32 | the same 32 names in `src/tests.rs`. The model's code runs in a real exec host with the Python test's `FakeAutoGUI` as `pyautogui`; the helpers are the real Rust Helpers on fake backends. The `settled_screenshot` / `_settle` spies become the fake clock's recorded sleeps and the capture count (one 0.25 s interval = one settle on a still screen; a picture with no wait = one capture); `test_timed_out_settle_is_not_reused_when_the_frame_is_stale` lets the time pass with a `wait_for` that cannot succeed (the fake clock is Rust's, so a Python `pause()` cannot move it); the monkeypatched `jev.load` becomes `jev_loader` with eve_jev's opener seam (and the default loader with a missing key, which proves it reads the env's route); a raising `h.lane_first` becomes an AX backend that panics | none |
| `bridge/tests/test_plan_success_guard.py` (ref) | `test_the_desktop_worker_passes_the_rejection_to_the_helpers` | same name, over `run_plan_args` (a fake Helpers class cannot stand in for the Rust `Helpers`) | the rest test plan_executor / plan_contract / the agent (other crates) |
| (new) | — | `test_the_loader_thread_reports_when_it_is_done`, `test_the_lane_is_off_away_from_macos`, `test_a_request_over_512_kib_is_refused_and_serving_goes_on`, `test_code_over_64_kib_is_bad_code`, `test_a_bad_start_ends_the_worker_as_pythons_except_did`, `test_int_reads_like_python`, `test_helper_arguments_are_bound_as_python_binds_them`, `test_a_helpers_log_past_the_limit_raises_in_the_models_code`, `test_the_namespace_survives_a_failsafe_inside_a_helper`, `test_lone_surrogates_read_as_replacement_characters`, `test_a_dead_host_is_host_died`, `test_the_models_stdin_and_stdout_cannot_reach_the_pipe`, `test_threads_of_the_models_code_take_turns_on_the_pipe`, `test_a_thread_that_outlives_its_call_cannot_corrupt_the_next_one`, `test_a_script_delegate_ignores_the_objectives_arguments` | |

Live check on this Mac (2026-10-20): the release binary started with the real PyAutoGUI (ready line `darwin`,
1920x1080, `fast_lane` and `jev_asker` as configured), answered `execute` (`pyautogui.size()`, PAUSE 0.05, FAILSAFE
True), `outline`, `screen_text()` and `observe` (a 1920x1080 PNG, the context line, the `[after]` line). No input was
posted. Accurate Vision OCR took ~24 s on this machine in both the Rust and the Python (`vision_ocr` measured
side by side), so that is the machine, not the port.

## Boundaries
- **The model's code is Python.** `exec_py` is Python by contract (the planner writes Python against PyAutoGUI and
  the helpers), so it runs in a Python child of the worker: the exec host (`src/exec_host.py`, embedded with
  `include_str!` and run with `python -I -c`). It holds the namespace (globals persist), PyAutoGUI and Pillow, runs
  each call under Python's rules (`<exec_py>`, stdout / stderr captured, `traceback.format_exc()[-4000:]`, the
  fail-safe test), wraps PyAutoGUI's input calls with the acting wrap, and normalises screenshots. Everything else is
  Rust: the output list and its limits, the helpers (eve_desktop_lanes, called back over the pipe with the Python
  signatures), the auto-screenshot rule, the `[after]` line, timing, the fast lane, the operations and the daemon's
  protocol. The helpers' own input (`click_text`, `type_text`, the lane's clicks) runs through the host's PyAutoGUI
  (`HostGui`), so the fail-safe, PAUSE and PyAutoGUI's key map are PyAutoGUI's.
- The interpreter is `EVE_WORKER_PYTHON`, else the bridge checkout's `.venv/bin/python3`, else `python3` (the Python
  daemon used its own `sys.executable`). It needs PyAutoGUI and Pillow; pyobjc Quartz for the permission check.
- The host's protocol runs on private copies of its stdin / stdout; fd 0 is `/dev/null` and fd 1 is stderr inside the
  host, so a stray print or native write cannot corrupt the pipe (in Python the model's `input()` read the daemon's
  next request line and a native write to fd 1 broke the protocol).
- macOS: `AXIsProcessTrusted`, `CGPreflightScreenCaptureAccess` and the CGEvent calls of `--release` are direct
  ApplicationServices / CoreGraphics FFI (`src/quartz.rs`).

## Differences
- A helper's own `log` / `display` that exceeds a limit: Python raised ValueError at that line, inside the helper;
  here the helper finishes and its call then raises the same ValueError in the model's code.
- The acting wrap's `act` time is measured on the worker's monotonic clock, not on the Helpers' injected clock (the
  same clock in production). A Rust helper's own PyAutoGUI calls are marked once by the helper; in Python they also
  went through the wrapped module functions and were marked (and timed) a second time. The `[after]` line and the
  auto-screenshot read the same `acted` either way.
- An input call whose coordinates `int()` refuses (a click-family call with two arguments) leaves a `timing.act` of
  0.0 next to `acted`, where Python left no `act` key.
- Helper arguments cross as JSON: an int parameter (coordinates, `clicks`, `max_steps`) given a float is truncated
  (`int()`), a bool reads as 0 / 1; a `region` must unpack into four numbers (Python's ValueError texts for the wrong
  length), and float region values are truncated before the centre test. A string parameter given another type is a
  TypeError naming the parameter (Python failed later, with the operation's own message); a `delegate(steps=[…])`
  script reads only `approve` and `dry_run`, as Python's did, so the objective's arguments are not checked. A NaN passed to a helper is
  the host's ValueError (`allow_nan=False`) rather than a NaN in the helper.
- `pyautogui.size()` failing inside a helper reads as a (0, 0) screen (the Gui trait's `size` cannot fail); at startup
  `check_desktop` reports the failure as Python did.
- Text with lone surrogates (possible in a Python `str`) reaches the reply as U+FFFD; Rust strings cannot hold them.
- PNG encoding of Rust-side images (the auto-screenshot, `observe`, `zoom`) uses the image crate's fast deflate:
  about the same size as Pillow's level 6 (2.0 vs 1.9 MB for a 1920x1080 screen) in a tenth of the time. The pixels
  are the same.
- A panic inside the router, the outline or the plan walk answers as Python's caught exception did:
  `{"status": "none", "reason": "error:panic"}`, `"error": "panic: <msg>"`, `"reason": "error:panic: <msg>"`. A
  panicking or failing loader is `failed: panic: <msg>` / `failed: <Kind>: <msg>`.
- Request lines that are not UTF-8 are read lossily (Python's strict stdin raised `UnicodeDecodeError` and ended the
  worker); `NaN` / `Infinity` literals in a request are `bad_json` (Python's `json.loads` accepted them).
- The exec host dying after the ready line (the model's code calling `os._exit`, a crash) ends the worker with exit
  1 and no line, as the Python worker's own death did; before the ready line it is a `startup` error.
- A reply that cannot be written (the daemon gone) ends the worker with exit 1, as Python's raising `emit` did.
- Threads the model's code starts: their `log` / `print` / `display` / helper calls take turns with the exec thread
  on the host's pipe (one re-entrant lock), so while a call runs they land in its output as in Python. After the
  call has ended nobody reads the pipe: a late `log` / `print` / `display` goes nowhere (Python appended it to the
  finished call's list, which nobody read either), a late PyAutoGUI input still runs (unmarked; the next call's
  `begin()` would have cleared `acted`), and a late helper call raises `RuntimeError: <name>() ran after its exec_py
  call ended; the desktop helpers answer only while a call is running` (Python ran it outside any call).
