# eve-scene
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/scene.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_MODEL`, `DEFAULT_INTERVAL_SECS`, `DEFAULT_STALE_SECS`, `DEFAULT_CAMERA_LOST_SECS`, `DEFAULT_TIMEOUT_SECS`, `DEFAULT_REFRESH_SECS`, `MAX_OUTPUT_TOKENS`, `NOTE_MAX_CHARS`, `LOOK_FRESH_SECS`, `TICK_SECS`, `FRAME_WAIT_POLL_SECS` | same-named `pub const` | values verbatim |
| `PROMPT`, `LOCATE_PROMPT`, `NO_CAMERA` | `PROMPT`, `LOCATE_PROMPT`, `NO_CAMERA` | text verbatim |
| `SCHEMA`, `LOCATE_SCHEMA` | `schema()`, `locate_schema()` | `serde_json::Value`, same key order |
| `SceneConfig` | `SceneConfig` (+ `Default`) | same fields and defaults |
| `_float_env` | `float_env` (private) | Python `float()` parsing, `min(hi, max(lo, v))` with Python's NaN semantics, same warning text |
| `configured` | `configured(Option<&HashMap>)` | `None` reads the process environment |
| `Observation` | `Observation` (+ `Observation::new`) | `seen_wall` is a naive local `NaiveDateTime` like `datetime.now()` |
| `SceneClient` | `trait SceneClient` | `describe` → `(scene, change)`, `locate` → JSON object; both blocking, both `anyhow::Result` |
| `parse_scene` | `parse_scene` | errs with `eve_vision::ValueError` ("empty answer", "answer has no scene"); `str()` / truthiness of non-string fields as Python |
| `parse_locate` | `parse_locate` | errs with `serde_json::Error` (JSONDecodeError) or `AttributeError` for non-object JSON |
| `_hms` | `hms` (private) | `%H:%M:%S` |
| `_clip` | `clip` (private) | counts characters (code points) as Python `len` does |
| `_num` | `eve_vision::opt_float` | same rule: JSON numbers only, bools and strings are None |
| `format_note` | `format_note` | |
| `unchanged_note` | `unchanged_note` | |
| `lost_note` | `lost_note` | |
| `blind_note` | `blind_note` | |
| `SceneWatcher` | `SceneWatcher` | `new(client, config)` + `with_clock` / `with_wall` / `with_camera_ok` / `with_tick_secs`; `start(self: &Arc<Self>, run_loop)`, `async stop`, `offer`, `newest_frame`, `camera_state`, `async tick`, `async look(fresh_secs, newer_than)`, `async locate(target, newer_than)`; accessors `enabled`, `active`, `latest`, `notes`, `describes`, `failures`, `client` / `set_client`, `held_frame` (Python `_frame`), `set_last_describe_at` (tests set `_last_describe_at`) |
| `SceneWatcher._executor` (`ThreadPoolExecutor(max_workers=1)`) | `Worker` (private) | one named thread `scene_0`; a timed-out job that has not started is skipped, as a cancelled pending future is; shutdown skips queued jobs (`cancel_futures=True`) |
| `OpenAISceneClient` | `OpenAISceneClient` | `new(model, api_key, timeout)`, `with_options`, `with_responses` (the tests' `_client` swap), `with_ledger`, `request`, `locate_request`, `SceneClient` impl; metered as `spend.CAMERA` with provider `openrouter` |
| `make_scene_client` | `make_scene_client(&SceneConfig, Option<&HashMap>)` | returns `Option<Arc<dyn SceneClient>>` |
| `_mime_for` | `mime_for` | |
| `run_scene_test` | `run_scene_test(path, find) -> i32` | same output lines and exit codes; called by the CLI crate's `scene-test` subcommand |
| (exception class names in logs) | `error_name`, `error_text`, `TimeoutError`, `AttributeError`, `AssertionError`, `Cancelled`, `Panicked` | the log lines print `type(e).__name__: e` as Python does; delegates to `eve_explore::error_name` / `error_text` for vision, JSON and OpenRouter errors |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `tests/test_scene.py` | `test_parse_scene_json_plain_and_none_change`, `test_format_note_carries_seen_time_and_change_and_is_clipped`, `test_configured_reads_env`, `test_frames_are_ignored_outside_a_conversation`, `test_disabled_watcher_never_starts`, `test_only_the_newest_frame_is_described_and_note_uses_seen_time`, `test_change_is_announced_and_sameness_is_repeated_on_refresh`, `test_camera_loss_is_said_once_after_a_grace_and_board_loss_counts`, `test_camera_never_delivering_a_frame_is_reported_after_the_grace`, `test_slow_describe_times_out_and_blindness_is_said_once`, `test_look_reuses_a_fresh_view_and_refreshes_a_stale_one`, `test_look_outside_a_conversation_says_so`, `test_a_full_cycle_writes_nothing_to_disk`, `test_request_is_not_stored_and_sends_one_low_detail_image`, `test_describe_parses_the_sdk_answer` | same names in `src/tests.rs` | none; `test_a_full_cycle_writes_nothing_to_disk` keeps every behavioral assertion but cannot `chdir` or set `HOME` (tests run in parallel threads); the watcher holds no paths at all |
| `tests/test_spend_wiring.py` | `test_the_camera_describe_and_locate_are_metered` | `test_the_camera_describe_and_locate_are_metered` | the other tests there belong to other crates |
| `tests/test_daemon_scene.py` | `test_frames_reach_the_scene_only_during_a_conversation_and_pose_reaches_the_head`, `test_board_loss_is_camera_loss` | `test_frames_reach_the_scene_only_during_a_conversation`, `test_board_loss_is_camera_loss` | the head pose, BLE and sound parts belong to the daemon and head crates |
| `tests/test_daemon_explore.py`, `tests/test_identity.py`, `tests/test_vision.py` | build `SceneWatcher(None)` as an inert stand-in | `test_scene_watcher_without_a_client_is_inert` | the rest belongs to other crates |
| (new) | — | `test_parse_scene_follows_python_str_and_truthiness`, `test_parse_locate_clamps_and_blanks_when_not_visible`, `test_note_lines`, `test_configured_edges`, `test_mime_for`, `test_a_describe_dropped_by_stop_is_not_a_failure`, `test_look_newer_than_waits_for_a_newer_frame`, `test_a_bad_frame_counts_as_a_failure`, `test_newest_frame_ages_out`, `test_locate_uses_the_newest_frame_and_echoes_the_pose`, `test_locate_with_a_bad_frame_cannot_check`, `test_the_loop_runs_until_stop`, `test_locate_request_names_the_target_tidied_and_capped`, `test_an_empty_answer_is_a_runtime_error`, `test_make_scene_client_needs_a_key_and_the_switch`, `test_py_repr_of_a_locate_answer`, `test_os_error_text_matches_python`, `test_live_scene_test` (ignored: live) | |

## Boundaries
None.

## Differences
- The OpenRouter HTTP client is built on the first describe or locate (on the watcher's worker thread), not in the constructor: reqwest's blocking client panics when built on an async runtime thread. The key is still checked at construction. Python's `ImportError` branch of `make_scene_client` has no Rust counterpart; a client that cannot be built logs `scene: client could not be built (…) — no scene descriptions`. `run_scene_test` runs its calls on a scoped thread for the same reason.
- `parse_scene` / `parse_locate` use `serde_json`, which rejects the non-standard literals `NaN`, `Infinity` and `-Infinity` that Python's `json.loads` accepts. For `parse_scene` an answer holding one is taken as plain text (Python would read the object); for `parse_locate` it is a JSON error, and the JSON error's message text differs from Python's `JSONDecodeError` text. The model answers under a strict schema, so neither occurs in practice.
- A client that panics counts as a failed describe (`Exception: <panic message>` in the log). A locate whose job is dropped by `stop()` returns `could not check the view`; in Python the cancelled future raises `CancelledError` out of `locate`. A describe whose job is dropped by `stop()` leaves the watcher untouched (no failure, no note, the busy flag kept), as Python's re-raised `CancelledError` does; a call that races `stop()` past the executor's shutdown fails as Python's `RuntimeError: cannot schedule new futures after shutdown`.
- Kept as Python behaves: when `stop()` cancels the loop in the middle of a describe, the busy flag stays set (Python's `_reset` does not clear `_busy`), so a watcher restarted after that does not describe again. This port reproduces it rather than changing behavior.
- `SceneWatcher.start(run_loop=True)` must be called inside a tokio runtime (Python's `asyncio.ensure_future` needs a running loop too).
