# eve-mac-camera
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/mac_camera.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `ENV`, `DEFAULT_DEVICE`, `WARMUP_FRAMES`, `FRAMERATE`, `MAX_WIDTH`, `TIMEOUT_SECS`, `FFMPEG_PATHS` | same `pub const`s | Values verbatim (`EVE_MAC_CAMERA`, `default`, 15, 30, 1920, 12.0, the two Homebrew paths) |
| `log` | `tracing::warn!` | Messages verbatim: `mac camera: cannot start ffmpeg: …`, `mac camera: no picture (exit N): …` (exit is the Popen-style returncode, negative for a signal; last stderr line, 200 chars) |
| `device` | `device(Option<&HashMap<String, String>>) -> Option<String>` | `None` reads the process env (`os.environ`). Trim, lowercase off-words `off/0/false/no/none` → `None`; blank → `default` |
| `ffmpeg_path` | `ffmpeg_path() -> Option<String>` | `which::which` (as `shutil.which`), then the first `FFMPEG_PATHS` entry passing `access(X_OK)` (as `os.access`) |
| `command` | `command(&str, &str) -> Vec<String>` | Same argv, element for element |
| `snap` | `snap(environ, timeout_secs) -> Snap`; seam `snap_with(environ, timeout_secs, find_ffmpeg)` | `{"ok": True, "jpeg": bytes}` / `{"ok": False, "reason": …}` is the enum `Snap::{Ok{jpeg}, Failed{reason}}` with `ok()`, `jpeg()`, `reason()`. stdin null, stdout/stderr piped; on timeout the child is killed and reaped. Reasons verbatim (`REASON_*` consts). JPEG check: exit 0 and stdout starts `FF D8` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| (none in `tests`) | - | 13 crate tests in `src/tests.rs`: full argv, off-words, blank/trimmed device, fallback paths, no ffmpeg, success with fake ffmpeg (argv checked), non-JPEG output, failed exit, timeout kill, spawn failure, reasons verbatim, stderr tail, signal returncode | - |
| `bridge/tests/test_photo_request.py` (ref) | `test_the_camera_command_reads_one_settled_frame_without_a_microphone`, `test_a_switched_off_camera_never_starts_ffmpeg` | same names in `src/tests.rs` (the off test proves neither the ffmpeg lookup nor a fake ffmpeg runs) | `test_without_the_robot_the_macs_camera_takes_the_photo`, `test_the_robot_still_takes_it_when_it_is_connected`, `test_a_mac_camera_that_gives_nothing_is_its_reason` and the rest test `daemon._photo_for_owner`; they belong to the daemon crate's port |

## Boundaries
ffmpeg runs as a subprocess, exactly as in Python (AVFoundation capture through `ffmpeg -f avfoundation`). This is the Python design, not a new boundary.

## Differences
- `snap` takes the timeout explicitly (Python defaults it to `TIMEOUT_SECS`); a NaN or negative timeout times out at once, as `asyncio.wait_for` does with a non-positive one.
- If waiting on the started process itself fails (an OS error Python's `communicate` would raise), Rust logs `cannot start ffmpeg` and returns the "could not be started" reason instead of raising, in keeping with the module's "a reason, never an exception" contract.
