# eve-head
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/head.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `YAW_MAX`, `PITCH_MIN`, `PITCH_MAX`, `PITCH_LEVEL` | same-named `i64` consts | values verbatim |
| `CAMERA_HFOV_DEG`, `CAMERA_VFOV_DEG` | same-named `f64` consts | |
| `DEFAULT_HOLD_SECS`, `MAX_HOLD_SECS`, `SETTLE_SECS` | same-named `f64` consts | |
| `SWEEP` | `SWEEP: &[(i64, i64)]` | |
| `Sender` | `Sender` (`Arc<dyn Fn(Value) -> BoxFuture<bool>>`) | the command is a `serde_json::Value` dict |
| `POSE_MOVES` | `POSE_MOVES: &[(&str, PoseMove)]` + `pose_move(name)` | typed `PoseMove { yaw, pitch, relative }`; `None` = key absent in Python; same order and numbers |
| `clamp_pose` | `clamp_pose(f64, f64) -> (i64, i64, bool)` | `int(round(x))` is round-half-to-even, as in Python |
| `build_look_cmd` | `build_look_cmd(i64, i64, i64) -> Value` | `{"cmd","yaw","pitch","hold"}`, hold clamped to 0..65535 |
| `direction_word` | `direction_word(f64) -> String` | |
| `target_pose` | `target_pose(f64, f64, f64, f64)` | |
| `class Head` | `struct Head` | pose behind a `parking_lot::Mutex` so it is shared via `Arc<Head>` |
| `Head.__init__` | `Head::new(send)` / `Head::with(send, connected, clock, sleep)` | defaults: always connected, `monotonic()`, tokio sleep |
| `Head.yaw` / `.pitch` / `.pose_at` | `yaw()` / `pitch()` / `pose_at()` | `pose_at` starts at `-inf` |
| `Head.on_move` | `set_on_move(Option<OnMove>)` | called with the hold after each successful move |
| `Head.observe` | `Head::observe(&Value, &Value)` | ignores anything that is not a JSON number (bools are not numbers in JSON) |
| `Head.move` | `Head::move_to(yaw, pitch, relative, hold_secs)` | `move` is a Rust keyword; result dict and messages verbatim |
| `Head.face_owner` | `Head::face_owner` | yaw 0, pitch 45, hold 3 s |
| `look_around` | `look_around(&Head, &dyn Scene, stops, settle)` | |
| `find` | `find(&Head, &dyn Scene, target, stops, settle, hold_secs)` | |
| (scene duck type) | `trait Scene { look, locate }` | the seam scene.py's watcher implements |
| `time.monotonic` / `asyncio.sleep` defaults | `monotonic()` / `tokio_sleep()` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_head.py` | test_clamp_and_command, test_direction_words_follow_the_robot_frame, test_target_pose_centres_a_point_like_the_firmware, test_absolute_and_relative_moves_use_the_echoed_pose, test_move_refuses_without_a_board_and_reports_a_lost_send, test_hold_is_capped_and_zero_hands_the_head_back, test_look_around_describes_each_stop_after_it_settles_then_faces_the_owner, test_look_around_stops_when_the_camera_is_gone, test_find_checks_the_current_view_first_then_sweeps_and_centres, test_find_reports_not_found_and_faces_the_owner, test_behind_is_the_neck_limit | same names in `src/tests.rs`, plus test_clamp_rounds_half_to_even_like_python, test_observe_records_when_the_pose_was_echoed, test_on_move_is_told_the_hold, test_pose_moves_match_the_backend_numbers, test_look_around_without_a_board_returns_the_move_failure_with_views | none |
| `bridge/tests/test_follow.py` (ref) | test_an_asked_for_pose_reaches_the_follower_through_the_head_and_presence_is_a_bus_topic (head part) | test_on_move_is_told_the_hold | the follower and bus belong to other crates |
| `bridge/tests/test_daemon_scene.py`, `bridge/tests/test_voice_fast_head.py` (ref) | daemon / voice wiring | covered by the head part above (observe, move without a board, POSE_MOVES) | wiring belongs to the daemon and voice crates |

## Boundaries
None

## Differences
- `clamp_pose` with NaN or infinite input saturates (`as i64`) where Python raises `ValueError`/`OverflowError`.
- In `find`, a located `x`/`y` that is not a number or numeric string reads as 0 (Python's `float()` would raise), and a located `yaw`/`pitch` that is not a number falls back to the head's pose (Python would raise in the arithmetic).
