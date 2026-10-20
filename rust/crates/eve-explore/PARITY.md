# eve-explore
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/explore.py` | `src/lib.rs` (pure core, note client, note taker, notes file, CLI), `src/mac.rs` (ImageIO + Core Graphics thumbnailer), tests in `src/tests.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `log` | `tracing` | Same messages: `explore: X='v' is not a number; using N`, `… is negative; using N`, `explore: OPENROUTER_API_KEY not set — …`, `explore: note -> PATH`, `explore: note failed — WHAT (further failures muted for 10 min)` |
| `DEFAULT_AFTER_MIN`, `DEFAULT_NOTES_PER_HOUR`, `DEFAULT_MODEL`, `DEFAULT_NOTES_DIR`, `DEFAULT_CYCLE_MIN` | same names | Values verbatim (`gpt-6-luna`, `~/.config/eve-bridge/notes`) |
| `PAN_YAWS`, `PAN_PITCHES`, `WAYPOINTS` | `PAN_YAWS`, `PAN_PITCHES` (arrays), `WAYPOINTS` (`LazyLock<Vec<(i64, i64)>>`) | Same serpentine, 36 waypoints |
| `LOOK_INTERVAL_SECS`, `LOOK_HOLD_MS`, `SETTLE_SECS`, `YAW_RANGE`, `PITCH_RANGE` | same names | |
| `THUMB_W`, `THUMB_H`, `CHANGE_THRESHOLD`, `BANK_SIZE`, `BANK_MIN`, `NOISE_FLOOR` | same names | |
| `NOTE_TIMEOUT_SECS`, `NOTE_MAX_OUTPUT_TOKENS`, `ERROR_LOG_INTERVAL_SECS`, `PROMPT` | same names | Prompt text verbatim |
| `ExploreConfig` | `ExploreConfig` (`Default` = the dataclass defaults) | `notes_dir` default expanded with `eve_vision::expanduser` |
| `_env_float` | private `env_float` (+ `py_float`, `py_g`, `py_repr_str`) | Python `float()` parsing (underscores, `inf`/`nan` any case), `%r` and `%g` in the warning |
| `configured` | `configured(Option<&HashMap<String, String>>)` | `None` reads the process environment, as `environ=None` does |
| `Mode` | `Mode { explore, reason }` (`Mode::new`) | |
| `Look` | `Look { yaw, pitch, hold_ms }` (`Look::new` with the default hold, `Look::with_hold`) | |
| `Note` | `Note { frame, yaw, pitch, thumb, diff, surprise }` (`Note::new` with the measurements None) | |
| `Rest` | `Rest { reason }` | |
| `Action` (Union) | `enum Action { Mode, Look, Note, Rest }` | |
| `ExploreRefused` | `ExploreRefused(String)` (thiserror) | Message is the blocker |
| `build_look_cmd` | `build_look_cmd(yaw, pitch, hold_ms) -> Value` | `eve_vision::clamp` (round half to even), `hold` floored at 0; key order `cmd, yaw, pitch, hold` |
| `build_mode_cmd` | `build_mode_cmd(bool) -> Value` | |
| `TokenBucket` | `TokenBucket::new(per_hour, now)`, `with_capacity(per_hour, now, Option<f64>)`, `available`, `take` | pub `rate`, `capacity`, `tokens` |
| `downscale_gray` | `downscale_gray(w, h, &[u8], tw, th) -> Result<Vec<u8>, ValueError>` | Same box bounds, integer floor average, same error messages |
| `mean_abs_diff` | `mean_abs_diff(&[u8], &[u8]) -> Result<f64, ValueError>` | |
| `Thumbnailer` | `type Thumbnailer = Arc<dyn Fn(&Frame) -> Option<Vec<u8>>>` | |
| `frame_thumb` | `frame_thumb(&Frame)` | Gray: `downscale_gray` (errors → None); else `quartz_thumb` |
| `_quartz_thumb` | `quartz_thumb(&[u8], tw, th)` (macOS body in `mac.rs`) | `CGImageSource` → 8-bit DeviceGray `CGBitmapContextCreate` (alpha none), `kCGInterpolationLow`, draw, `CGBitmapContextCreateImage`, rows de-strided; None off macOS |
| `normalise` | `normalise(&[u8]) -> Vec<u8>` | `int()` truncation then clamp |
| `robust_z` | `robust_z(&[u8], &VecDeque<Vec<u8>>, noise_floor) -> f64` | Same upper median (`col[len//2]`) and MAD |
| `ChangeDetector` | `ChangeDetector` (`default`, `new(threshold)`, `with_thumb`, `with_options`), `measure`, `keep`, `surprise`, `banked`, `thumb_of` | Bank is a `VecDeque` capped at `bank_size` (deque `maxlen`) |
| `Explorer` | `Explorer::new(config, now)`, `with_options(config, now, ExplorerOptions)` | `OFF`/`EXPLORING`/`RESTING` are `State` values (`as_str` gives `off`/`exploring`/`resting`); `active()`, `on_board()`, `waypoint()` are methods for the properties; `wants_frame`, `reset`, `status` (same JSON keys and order), `request -> Result<Vec<Action>, ExploreRefused>`, `dismiss`, `tick(now, idle_secs, listening, Option<&Frame>, connected)`; counters pub |
| `NoteClient` (Protocol) | trait `NoteClient::describe(&[u8], &str) -> anyhow::Result<String>` | Blocking |
| `frame_image` | `frame_image(&Frame) -> Result<(Vec<u8>, &str), ValueError>` | |
| `data_url` | `data_url(&[u8], &str)` | |
| `OpenAINoteClient` | `OpenAINoteClient::new(model, api_key, timeout)`, `with_options`, `with_responses`, `with_ledger`; `NoteClient` impl | Same Responses body (`model, input, max_output_tokens, reasoning`), no retries, metered as `exploring` via `eve_spend::Ledger::record_response`; `output_text` collapsed with Python `split()`; empty → `RuntimeError("empty answer (status=…, incomplete=…)")` printed as the SDK prints `IncompleteDetails(reason='…')` |
| (SDK `Response.output_text`) | `output_text(&Value)` | Joined `output_text` parts of `message` items |
| (Python exception names in logs) | `error_name`, `error_text`, `RuntimeError` | SDK class names by status; connection/timeout texts as the SDK prints them |
| `make_note_client` | `make_note_client(&ExploreConfig, Option<&HashMap>) -> Option<Arc<dyn NoteClient>>` | Same warning when the key is missing |
| `note_line` | `note_line(&NaiveDateTime, yaw, pitch, text)` | `- HH:MM yaw=+N pitch=N — text` |
| `notes_path` | `notes_path(&Path, &NaiveDateTime)` | `YYYY-MM-DD.md` |
| `append_note` | `append_note(dir, when, yaw, pitch, text, Option<&str>) -> io::Result<PathBuf>` | Creates the folder 0700 only when missing; empty `extra` is skipped as falsy |
| `read_notes` | `read_notes(&Path, last: i64) -> io::Result<Vec<String>>` | `*.md` sorted by name, newest files first until `last` lines, `str.splitlines()`, blank lines dropped, date prefix; Python slicing for any sign of `last` |
| `NoteTaker` | `NoteTaker::new(client, notes_dir)` + `with_timeout`, `with_clock`, `with_wall`; `async take(&Note) -> anyhow::Result<Option<PathBuf>>`, `stop`, `taken()`, `failed()` | One worker thread `notes_0` (ThreadPoolExecutor max_workers=1), started lazily and again after `stop`; `tokio::time::timeout`; errors logged at most once per 10 min |
| `run_notes_cli` | `run_notes_cli(last, Option<&ExploreConfig>) -> i32`, `run_notes_cli_to(…, out, err)` | |
| `run_notes_test` | `run_notes_test(path, Option<&ExploreConfig>) -> i32`, `run_notes_test_with(…, make_client, out, err)` | Same lines and exit codes (2 unreadable / no client, 1 call failed, 0); `[Errno N] msg: 'path'` like Python's OSError |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `tests/test_explore.py` | `test_build_look_clamps_to_wire_ranges`, `test_build_mode`, `test_configured_defaults`, `test_configured_env_knobs`, `test_configured_bad_number_falls_back`, `test_starts_only_when_idle_and_unblocked`, `test_disabled_config_never_starts`, `test_activity_stops_exploring` (3 params), `test_waypoint_cadence_one_look_per_six_seconds`, `test_cycle_end_rests_with_the_board_still_exploring`, `test_activity_while_resting_leaves_explore_mode` (3 params), `test_on_board_tracks_exploring_and_resting`, `test_frame_sampled_two_seconds_after_look_once_per_waypoint`, `test_unchanged_view_spends_no_note_on_the_next_cycle`, `test_changed_view_spends_a_note`, `test_budget_exhausted_skips_note_but_keeps_reference_unchanged`, `test_notes_disabled_pans_without_notes`, `test_downscale_gray_box_average`, `test_mean_abs_diff`, `test_change_detector_first_visit_then_threshold`, `test_change_detector_undecodable_counts_as_changed`, `test_frame_thumb_jpeg_without_imageio_is_none`, `test_request_starts_at_once_and_ignores_idle`, `test_manual_explore_survives_rest_and_pans_again`, `test_hard_blockers_end_a_manual_explore_and_clear_manual` (2 params), `test_request_is_refused_on_a_hard_blocker` (2 params), `test_request_while_resting_sends_only_a_look`, `test_request_while_exploring_restarts_the_pan_without_mode`, `test_dismiss_sends_mode_off_only_when_on_board`, `test_request_works_when_idle_start_is_disabled`, `test_status_when_off`, `test_token_bucket_starts_full_then_refills_at_rate`, `test_token_bucket_zero_rate_never_grants`, `test_note_line_format`, `test_append_note_creates_private_dir_and_dated_file`, `test_read_notes_last_n_across_days`, `test_notes_cli_prints`, `test_frame_image_formats`, `test_data_url`, `test_note_taker_appends_and_logs`, `test_note_taker_failure_skips_and_logs_once_per_ten_minutes`, `test_note_taker_timeout`, `test_make_note_client_without_key_is_none`, `test_surprise_is_none_until_the_bank_has_five_frames`, `test_surprise_stays_low_for_the_usual_view_and_jumps_for_a_change`, `test_a_flickering_cell_stops_counting_as_surprise`, `test_surprise_ignores_the_lights_going_down`, `test_banks_are_per_waypoint`, `test_normalise_centres_on_mid_grey`, `test_every_considered_frame_is_banked_even_when_no_note_is_spent`, `test_the_pan_plan_stays_inside_the_servos_own_limit`, `test_the_pan_sweeps_the_whole_range_in_small_steps`, `test_a_cycle_is_still_a_sane_length`, `test_the_rows_straddle_wherever_level_turns_out_to_be` | same names in `src/tests.rs` (parametrized ones loop over their cases). Added: `test_configured_parses_like_python`, `test_python_number_formats`, `test_native_quartz_thumb_decodes_an_image_file` (macOS), `test_append_note_writes_extra_under_the_line`, `test_read_notes_skips_blank_lines_and_other_files`, `test_note_taker_stop_cancels_a_queued_call_and_restarts_lazily`, `test_note_taker_bad_frame_escapes_take`, `test_make_note_client_with_key_builds_inside_a_runtime`, `test_empty_answer_is_a_runtime_error`, `test_the_note_request_goes_to_openrouter_with_the_prefixed_model`, `test_notes_test_cli`, `test_bank_keeps_only_the_newest_bank_size`, `test_waypoints_are_the_serpentine` | `test_daemon_sends_mode_and_look_when_idle`, `test_daemon_idle_clock_resets_while_a_session_runs`, `test_daemon_stops_on_activity_and_sends_mode_off`, `test_daemon_sends_nothing_while_disconnected`, `test_daemon_samples_frame_and_takes_note`, `test_daemon_stop_explore_leaves_mode_on_shutdown`: they drive `Daemon` methods (`_explore_step`, `_stop_explore`, …) and belong to the eve-daemon port |
| `tests/test_spend_wiring.py` (ref) | `test_exploring_notes_and_the_diary_are_metered` | `test_exploring_notes_are_metered` (the explore half) | The diary half belongs to eve-diary |
| `tests/test_model_memory_roles.py` (ref) | uses `explore.DEFAULT_MODEL` | `test_configured_defaults` pins it | The diary client belongs to eve-diary |
| `tests/test_diary.py`, `test_daemon_explore.py`, `test_daemon_lesson.py`, `test_daemon_membus.py`, `test_daemon_touch.py` (ref) | use `Note`, `THUMB_W`/`THUMB_H`, `ExploreConfig`, `Explorer` as fixtures | covered above | The behaviour under test is in eve-diary and eve-daemon |

## Boundaries
None. The JPEG thumbnailer uses ImageIO and Core Graphics through `objc2-image-io` / `objc2-core-graphics` 0.3.2 on macOS; off macOS it returns None as Python does.

## Differences
- The OpenRouter HTTP client is built on the first `describe` (on the note taker's worker thread), not in the constructor: reqwest's blocking client panics when built on an async runtime thread. The key is still checked at construction, so a missing key fails there as `openrouter.client` raises `NoKey`. Python's `ImportError` branch of `make_note_client` has no Rust counterpart; a client that cannot be built logs `explore: note client could not be built (…) — no notes`.
- `NoteTaker::take` returns `Err` for what Python lets escape `take` (a frame `encode_gray_png` rejects, a notes file that cannot be written, and `NoteCancelled` for a queued call dropped by `stop()`, Python's `CancelledError`). A panic inside a client's `describe` counts as a failed note. Dropping a `NoteTaker` stops its worker.
- `read_notes` returns an `io::Error` where Python raises (a non-UTF-8 notes file, a directory named `*.md`); `run_notes_cli` prints `notes: <error>` and returns 1 where Python would print a traceback. File names that are not valid UTF-8 are not listed.
- `normalise` of an empty thumbnail returns an empty one (Python raises ZeroDivisionError); `robust_z` panics where Python raises IndexError (empty bank, shorter banked thumbnail) — neither happens with the default thumbnailer and `BANK_MIN` 5.
