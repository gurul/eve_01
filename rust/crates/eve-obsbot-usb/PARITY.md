# eve-obsbot-usb
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/obsbot_usb.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `SOURCE` | `SOURCE`, `SOURCE_BYTES` | `SOURCE` is the checkout path of `native/obsbot_uvc.c`; `SOURCE_BYTES` embeds the same file with `include_bytes!`, so the hash (and the binary name) is the one Python computes |
| `BIN_DIR` | `bin_dir()` | `~/.config/eve-bridge/bin`; tests pass a directory instead of patching the global |
| `VENDOR`, `PARKED_TILT`, `TILT_LIMIT`, `TURN_SPEED`, `WAKE_SECS`, `NO_CLANG_LINE`, `NO_CAMERA_LINE` | same names | values verbatim |
| `Runner` | `trait Runner`, `SubprocessRunner`, `RunOutput` | argv + timeout; stdin closed, output captured as text; a timeout is `io::ErrorKind::TimedOut`. Closures `Fn(&[String], Duration) -> io::Result<RunOutput>` implement it |
| `helper_path` | `helper_path()`, `helper_path_in(dir)` | `obsbot-uvc-<sha256(source)[:10]>` |
| `build` | `build(runner)`, `build_in(dir, clang, runner)`, `find_clang()` | same clang argv (`-O2 -framework IOKit -framework CoreFoundation -o <tmp> <source>`), `<name>.tmp` then rename, older `obsbot-uvc-*` builds removed, same log lines and reasons |
| `present` | `present(runner)` | `ioreg -p IOUSB -l -w0`, looks for `"idVendor" = 13668`; any run error is false |
| `UsbCamera` | `UsbCamera` (+ `trait UsbControl` for fakes) | builder: `with_runner`, `with_waker`, `with_helper`, `with_clock`, `with_bin_dir`; `why` is a public field |
| `UsbCamera.__init__` | `UsbCamera::new()` + builders | defaults: subprocesses, `open_video` waker, helper built on first call |
| `UsbCamera._call` | `UsbCamera::call` (private) | build on first use, run with a 5 s timeout, parse the last line of stdout as JSON; failures are `{"ok": false, "error": "the camera helper failed (<Python exception name>)"}` with the names Python gives (`TimeoutExpired`, `JSONDecodeError`, `IndexError`, `FileNotFoundError`, `PermissionError`, `OSError`); a helper `ok: false` sets `why` (`NO_CAMERA_LINE` for "no camera") |
| `UsbCamera.read` | `UsbCamera::read` | the helper's object with `asleep` appended (`float(tilt) < PARKED_TILT`), key order kept |
| `UsbCamera.wake` | `UsbCamera::wake` | waker only when asleep, then polls every 0.3 s for `WAKE_SECS` through the `Clock` seam |
| `UsbCamera.point` | `UsbCamera::point` | args formatted as Python's `:.2f` (incl. `nan`, `-0.00`) |
| `UsbCamera.turn` | `UsbCamera::turn(Direction, speed)`, `Direction` (`FromStr`) | left (-1,0), right (1,0), up (0,1), down (0,-1), stop (0,0); an unknown name fails to parse with `UnknownDirection` where Python's dict lookup raises `KeyError` |
| `UsbCamera.zoom` | `UsbCamera::zoom(i64)` | `str(int(level))`: the caller passes an integer |
| `time.monotonic` / `time.sleep` (patched in tests) | `trait Clock`, `SystemClock` | seam for the tests' patches |
| `_open_video` | `open_video()` | same ffmpeg lookup and argv, 10 s timeout, every failure ignored |
| `_video_device` | `video_device(ffmpeg)`, `video_device_in(listing)` | same regex `\[(\d+)\] .*OBSBOT`, stops at "audio devices", falls back to "0" |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_obsbot_usb.py` | `test_read_and_asleep_by_the_parked_tilt`, `test_no_camera_is_a_plain_line`, `test_turn_directions_and_speed`, `test_point_and_zoom_arguments`, `test_wake_opens_the_video_only_when_asleep`, `test_wake_gives_up_when_the_camera_stays_parked`, `test_build_compiles_once_per_source`, `test_build_failure_is_a_reason`, `test_present_reads_the_usb_bus`, `test_the_helper_source_builds_with_this_macs_clang` | same names in `src/tests.rs`, plus `test_read_keeps_the_helpers_fields_in_order`, `test_other_helper_errors_are_passed_through`, `test_unknown_direction_is_refused`, `test_point_formats_like_python`, `test_wake_fails_when_the_camera_cannot_be_read`, `test_helper_name_is_the_source_hash_python_uses`, `test_build_without_clang_says_so`, `test_call_builds_the_helper_and_reports_why`, `test_helper_failures_name_the_python_exception`, `test_reply_is_the_last_line`, `test_video_device_by_name`, `test_python_string_helpers`, `test_build_for_real_and_read_without_a_camera` | none |
| `bridge/tests/test_obsbot.py` (tests_ref) | uses `FakeUsb` with `UsbCamera`'s surface, `NO_CAMERA_LINE`, `TURN_SPEED`, `TILT_LIMIT` | constants exported; `UsbControl` trait gives the panel crate the fake seam | the panel's behavior belongs to the `obsbot` crate |

## Boundaries
- The UVC requests go through the existing C helper `native/obsbot_uvc.c`, compiled with clang and run as a subprocess, exactly as the Python does (one process per call). The source is embedded, so the binary name is the same hash and both daemons share `~/.config/eve-bridge/bin/obsbot-uvc-<hash>`.
- `ioreg` (presence) and `ffmpeg` (waking by opening the video) are subprocesses, as in Python.

## Differences
- clang compiles the embedded source written to `~/.config/eve-bridge/bin/.obsbot_uvc-<hash>.c` (removed after the build; the `obsbot-uvc-*` sweep never matches it) rather than the package file path.
- Where Python would raise out of the module, Rust returns a value instead of panicking: a clang run that errors or times out, or a bin directory that can't be created, is the "clang failed" reason; a reply whose last line is JSON but not an object is `the camera helper failed (AttributeError)`; a `get` reply with no numeric `tilt` reads as None (Python: KeyError/ValueError).
- clang runs with stdin closed (Python inherits it); clang never reads stdin.
- A non-string, non-scalar `error` in a helper reply becomes `why` as JSON text rather than Python's `repr` (the helper only prints strings).
