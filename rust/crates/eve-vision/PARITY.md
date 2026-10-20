# eve-vision
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/vision.py` | `src/lib.rs` (pure core, tracker, frame dump, CLI), `src/mac.rs` (macOS Vision / ImageIO / Core Graphics adapter), tests in `src/tests.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `log` | `tracing` | Same messages: `vision: bad frame from board — … (further ones counted, not logged)`, `vision: first frame WxH fmt N.N KB`, `vision: detector failed on frame N`, `vision: on_faces failed on frame N`, `vision: could not save frame to … — disabling frame dump`, `vision: saving frames to … (first: …)`, `vision: host face tracking needs macOS Vision — board keeps on-board tracking` |
| `CAM_FPS`, `CAM_W`, `CAM_H`, `SAVE_FRAMES_MIN_INTERVAL`, `STATS_INTERVAL_SECS`, `FRAME_FORMATS`, `WHO_UNKNOWN` | same names | Values verbatim (5, 160, 120, 1.0, 30.0, jpeg/gray, `unknown`) |
| `Frame` | `Frame` (`Frame::new(seq, w, h, fmt, data)`) | Same fields; `seq`/`w`/`h` are `i64` (Python int), `data: Vec<u8>`, `yaw`/`pitch: Option<f64>`, `snap: bool` |
| `Rect` | `Rect` (`Rect::new` with `conf = 1.0`, `Rect::with_conf`) | |
| `FaceResult` | `FaceResult` | `bx`, `by`, `size`, `conf` as `i64` |
| `Detector` | `type Detector = Arc<dyn Fn(&Frame) -> anyhow::Result<Vec<Rect>>>` | `Err` is the Python exception |
| `Sender` | `type Sender = Arc<dyn Fn(Value) -> BoxFuture<bool>>` | |
| `Identity` (Protocol) | trait `Identity::process(&Frame, &[Rect]) -> anyhow::Result<String>` | `Err` is an exception inside `_work`: logged, frame answers no face |
| `build_cam_cmd` | `build_cam_cmd(on)`, `build_cam_cmd_with(on, fps, w, h)` | Same keys in the same order |
| `decode_frame` | `decode_frame(&Value) -> Result<Frame, ValueError>` | Python `int()` semantics for seq/w/h (int, float truncation, bool, numeric strings with whitespace/underscores) and the messages of the `KeyError`/`TypeError`/`ValueError` it wraps; `str(obj.get("fmt") or "")` with Python's `str`/`repr`; `b64decode(validate=True)` as CPython 3.13 (the daemon's interpreter) runs it: the ASCII check, then `binascii.a2b_base64(strict_mode=True)` character by character with its messages (`Leading padding not allowed`, `Excess padding not allowed`, `Excess data after padding`, `Discontinuous padding not allowed`, `Only base64 data is allowed`, `Incorrect padding`, the 1-more-than-a-multiple-of-4 count); subscripting a list/str/other gives Python's `TypeError` text; gray length and JPEG SOI checks verbatim; `snap` only for JSON `true` |
| `build_snap_cmd` | `build_snap_cmd()` | |
| `_opt_float` | `opt_float(Option<&Value>)` | bools and strings are None |
| `_clamp` | `clamp(v, lo, hi)` (pub: explore uses `_clamp`) | `int(round(v))` is half-to-even (`round_ties_even`) |
| `largest_rect` | `largest_rect(&[Rect]) -> Rect` | First of equal areas wins, as `max`; panics on empty like `max` raises |
| `pick_face` | `pick_face(&[Rect], w, h) -> Option<FaceResult>` | |
| `build_face_cmd` | `build_face_cmd(seq, Option<FaceResult>, yaw, pitch, who)` | Key order `cmd, seq, bx, by, size, conf, yaw, pitch, who` / `cmd, seq, conf, who` |
| `FrameRateGovernor` | `FrameRateGovernor` | pub `busy`, `pending`, `received`, `processed`, `dropped`; `offer`, `done` identical |
| `_Window` | private `Window` | |
| `FaceTracker` | `FaceTracker` (cheap `Clone`, shared state) | `new(detect, send)`, `with_options(detect, send, save_dir, clock, identity, on_faces)`; the daemon's attribute assignments are `set_detect`, `set_identity`, `set_on_faces`, `set_save_dir`; counters via `faces()`, `detect_secs()`, `bad_frames()`, `governor()` (a copy) |
| `FaceTracker.enabled` (property) | `enabled()` | |
| `FaceTracker.stop` | `stop()` | Shuts the worker down: the running detect finishes, queued ones are dropped (`cancel_futures=True`) |
| `FaceTracker.on_frame` | `async on_frame(&Value)` | Decode, first-frame log, rate-limited save, governor offer; a run task is `tokio::spawn`ed |
| `FaceTracker._run` | private `run` | Single named worker thread (`vision_0`) per tracker, created lazily (again after `stop()` if a drain is still going); detector errors and panics are logged and answer "no face"; `on_faces` gets every face via `pick_face([r])`, its errors and panics are logged |
| `FaceTracker._work` | private `work` | Detect, then identity on the same thread only when there is a face |
| `FaceTracker.stats_line` | `stats_line() -> Option<String>` | Same window arithmetic and format (`{:.1}` / `{:.0}` round like Python's) |
| `FaceTracker._maybe_save` | private `maybe_save` | At most one frame per `SAVE_FRAMES_MIN_INTERVAL`; an I/O error disables the dump |
| `save_frame` | `save_frame(&Frame, &Path) -> io::Result<PathBuf>` | `frame_{seq:06}.jpg` verbatim / `.png` via `encode_gray_png`; creates parents |
| `encode_gray_png` | `encode_gray_png(w, h, &[u8]) -> Result<Vec<u8>, ValueError>` | Same chunk layout (IHDR 8-bit gray, one IDAT, IEND), filter 0 per row, zlib level 6 (flate2), CRC32 (crc32fast); same mismatch message |
| `configured_save_dir` | `configured_save_dir(Option<&str>)` (process env) and `configured_save_dir_from(cli, env)` | `cli or EVE_SAVE_FRAMES or ""`, stripped, `expanduser` (HOME, else passwd; `~user`) |
| `make_detector` | `make_detector() -> Option<Detector>` | Off-mac: the same warning and None. On mac: checks the Vision classes exist, else warns and returns None |
| `_cgimage_from_frame` | `cgimage_from_frame(&Frame) -> Option<CFRetained<CGImage>>` (macOS, pub for identity) | Gray: `CGImageCreate` 8 bpc/8 bpp DeviceGray, no alpha; otherwise `CGImageSourceCreateWithData` + `CreateImageAtIndex(0)` |
| `vision_detect` | `vision_detect(&Frame) -> anyhow::Result<Vec<Rect>>` | Inside an autorelease pool; `VNDetectFaceRectanglesRequest` with default options; y flipped from bottom-left to top-left with the CGImage's own size; errors `could not decode {fmt} frame {seq}` and `Vision request failed: <NSError -description>` (Python's `str(err)`) |
| `probe_image_size` | `probe_image_size(&[u8]) -> Option<(i64, i64)>` | ImageIO on macOS |
| `run_vision_test` | `run_vision_test(path) -> i32` | Same stdout/stderr lines and exit codes (2 for no detector / unreadable / undecodable, 0 on success), `[Errno N] msg: 'path'` like Python's OSError, compact JSON for `send:` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `tests/test_vision.py` | `test_decode_gray`, `test_decode_jpeg_keeps_file_bytes`, `test_decode_yaw_pitch_ignore_junk`, `test_decode_rejects_malformed` (7 params), `test_pick_face_centre`, `test_pick_face_right_is_positive_bx`, `test_pick_face_down_is_positive_by`, `test_pick_face_top_left_is_negative`, `test_pick_face_largest_wins`, `test_pick_face_clamps_out_of_frame`, `test_pick_face_none_without_rects`, `test_face_cmd_with_face_echoes_pose`, `test_face_cmd_without_pose_omits_it`, `test_face_cmd_no_face_is_conf_zero`, `test_face_cmd_who_override`, `test_cam_cmds`, `test_governor_runs_first_frame_immediately`, `test_governor_keeps_only_newest_while_busy`, `test_tracker_replies_face_for_each_processed_frame`, `test_tracker_drops_to_newest_while_detector_busy`, `test_tracker_disabled_ignores_frames`, `test_tracker_counts_bad_frames_without_replying`, `test_tracker_stats_line_and_window_reset`, `test_gray_png_is_valid_png`, `test_tracker_saves_at_most_one_frame_per_second`, `test_configured_save_dir`, `test_make_detector_none_off_darwin` | same names in `src/tests.rs` (`test_make_detector_none_off_darwin` is `cfg(not(macos))`, with `test_make_detector_some_on_darwin` on macOS). Added: `test_decode_snap_only_when_true`, `test_decode_error_messages_match_python`, `test_decode_ints_like_python_int`, `test_base64_validate_like_python`, `test_clamp_rounds_half_to_even`, `test_largest_rect_first_of_equals`, `test_tracker_detector_error_answers_no_face`, `test_tracker_identity_fills_who_and_on_faces_gets_every_face`, `test_tracker_on_faces_failure_never_stops_tracker`, `test_gray_png_decodes_to_the_same_pixels`, `test_tracker_save_failure_disables_dump`, `test_configured_save_dir_expands_user`, `test_tracker_stop_mid_drain_still_answers_parked_frame`, `test_stats_number_format_rounds_like_python`; `tests/decode_differential.rs` `test_decode_frame_matches_python_cases` checks 135 inputs against `tests/fixtures/decode_frame.json`, written by CPython 3.13 running the Python `decode_frame` (`tests/fixtures/gen_decode.py`) | `test_daemon_resync_sends_cam_on_after_time_sync`, `test_daemon_no_cam_on_without_detector`, `test_daemon_no_cam_while_disconnected`, `test_daemon_frame_object_gets_a_face_reply`: they drive `Daemon` methods (`_resync_board`, `_send_cam`, `_handle_ble`) and belong to the eve-daemon port |
| `tests/test_vision_native.py` | `test_native_face_request_accepts_default_options`, `test_native_identity_request_accepts_default_options` | `test_native_face_request_accepts_default_options` (macOS only); added `test_native_jpeg_frame_decodes_and_probes`, `test_native_vision_test_cli` | `test_native_identity_request_accepts_default_options` tests `identity.vision_feature_print` (eve-identity) |
| `tests_ref` (`test_daemon_membus`, `test_desk_call`, `test_diary`, `test_explore`, `test_follow`, `test_identity`, `test_photo_request`) | use `Frame`, `Rect`, `decode_frame`, `encode_gray_png`, `FaceTracker` as fixtures | covered by the tests above | the behaviour under test is in those crates |

## Boundaries
None. The Vision, ImageIO and Core Graphics calls go through `objc2-vision`, `objc2-image-io` and `objc2-core-graphics` 0.3.2 on macOS; off macOS `make_detector` returns None exactly as Python does.

## Differences
- Python's `_run` catches every `RuntimeError` from `run_in_executor` as "executor shut down", which also swallows a `RuntimeError` raised by the detector itself (`Vision request failed: …`) and leaves the governor busy forever, so tracking stops for the rest of the run. Rust treats only a stopped worker as shutdown; a detector or identity error (or panic) is logged and the frame answers "no face", as Python does for every other exception.
- After `stop()`, a drain loop that is still running gets a fresh worker thread for its parked frame, where Python's `run_in_executor(None, …)` falls back to asyncio's default executor; both keep answering frames.
- `_clamp` on a NaN (a detector reporting a NaN box) gives 0 where Python's `round` raises `ValueError` on the loop thread and wedges the tracker; Vision never reports one.
- `encode_gray_png` compresses with flate2 (miniz_oxide) at level 6, so the IDAT bytes can differ from CPython's zlib; the PNG decodes to the same pixels.
- `seq`/`w`/`h` beyond 64-bit range are rejected as bad frames (Python would build a big int).
- `configured_save_dir` with `~user` for an unknown user keeps the path as given (pathlib raises `RuntimeError`).
- `run_vision_test`: a detector error prints `vision-test: <error>` and returns 1, where Python lets the exception escape as a traceback.
- `make_detector` on macOS checks the Vision classes at run time instead of importing pyobjc; the framework is linked, so the pyobjc install hint does not apply.
