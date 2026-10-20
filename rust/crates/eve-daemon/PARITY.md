# eve-daemon
Status: complete

## Modules
| Python | Rust |
|---|---|
| `daemon.py` | `src/lib.rs` (constants), `src/daemon.rs` (the struct, its parts, `_spawn`, `_work_note`), `src/seams.rs`, `src/link.rs`, `src/board.rs`, `src/ble_handler.rs`, `src/run.rs`, `src/voice.rs`, `src/lesson.rs`, `src/explore.rs`, `src/ipc.rs`, `src/hooks.rs`, `src/door.rs`, `src/inlet.rs`, `src/agents.rs`, `src/apps.rs`, `src/startup.rs` |

Python's `Daemon` is one class; here it is one struct, `Daemon`, held as an `Arc`, with its methods split over one
file per section of the class. Every method keeps its Python name without the leading underscore.

## API
| Python | Rust | Notes |
|---|---|---|
| `_ENTRY_PAYLOAD_MAX_BYTES` | `ENTRY_PAYLOAD_MAX_BYTES` (lib.rs) | `ENTRY_MAX_BYTES - 2` |
| `log` | `tracing` | same messages |
| `THINK_ALOUD_CALL_SECS`, `THINK_ALOUD_TASK` | `THINK_ALOUD_CALL_SECS`, `THINK_ALOUD_TASK` (lib.rs) | |
| `TODAY_BUILD_SECS`, `CLOSE_MARKERS_SECS`, `DRAIN_SECS` | same names (lib.rs) | |
| `CHAT_MAX_CHARS`, `MCP_CALL_MARGIN_SECS`, `MCP_CALL_BOUND_SECS`, `MCP_STILL_RUNNING` | same names (lib.rs) | text verbatim |
| `Daemon` | `Daemon` (daemon.rs), `DaemonParts`, `DaemonState` (`Daemon.st`) | the mutable attributes live in one `parking_lot::Mutex<DaemonState>`; `state` is an `Arc<Mutex<State>>` lent as is to the Work sessions |
| `Daemon.IPC_HANDLERS` | `IPC_EVENTS` + the `match` in `Daemon::handle_ipc` (ipc.rs) | the same 29 events |
| `_spawn` | `Daemon::spawn` (daemon.rs) | a `Job` held in `Daemon.background` until done |
| `_work_note` | `Daemon::work_note` (daemon.rs) | a panic stands in for Python's exception and is logged |
| `_mcp_late_done` | `mcp_late_done` (ipc.rs) | |
| `_mcp_refusal` | `Daemon::mcp_refusal` (ipc.rs) | |
| `code_origin` | `startup::code_origin` | |
| `_log_permission_config_summary` | `startup::log_permission_config_summary` | |
| `Daemon.__init__` | `Daemon::new`, `Daemon::assemble`, `DaemonParts::for_test` (daemon.rs) | the parts are built first (`DaemonParts`), then assembled with `Arc::new_cyclic` so callbacks hold a `Weak<Daemon>` |
| `Daemon.run` | `Daemon::run` (agents.rs) | |
| `Daemon.shutdown` | `Daemon::shutdown` (run.rs) | |
| `Daemon._close_notes` | `Daemon::close_notes` (run.rs) | |
| `Daemon._build_memory` | `Daemon::build_memory` (run.rs) | |
| `Daemon._open_memory` | `Daemon::open_memory` (run.rs) | |
| `Daemon._start_dream` | `Daemon::start_dream` (run.rs) | |
| `Daemon._close_open_conversations` | `Daemon::close_open_conversations` (run.rs) | |
| `Daemon._drain_background` | `Daemon::drain_background` (run.rs) | |
| `Daemon._voice_memory` | `Daemon::voice_memory` (run.rs) | |
| `Daemon._start_memory_sinks` | `Daemon::start_memory_sinks` (run.rs) | |
| `Daemon._stop_memory_sinks` | `Daemon::stop_memory_sinks` (run.rs) | |
| `Daemon._publish_observation` | `Daemon::publish_observation` (run.rs) | |
| `Daemon._publish_lesson` | `Daemon::publish_lesson` (run.rs) | |
| `Daemon._on_remember` | `Daemon::on_remember` (run.rs) | |
| `Daemon._recall_service` | `Daemon::recall_service` (run.rs) | |
| `Daemon._heartbeat_loop` | `Daemon::heartbeat_loop` (board.rs) | |
| `Daemon._push_heartbeat` | `Daemon::push_heartbeat` (board.rs) | |
| `Daemon._resync_board` | `Daemon::resync_board` (board.rs) | |
| `Daemon._send_resync` | `Daemon::send_resync` (board.rs) | |
| `Daemon._on_ble_connected` | `Daemon::on_ble_connected` (board.rs) | |
| `Daemon._mic_wanted` | `Daemon::mic_wanted` (board.rs) | |
| `Daemon._apply_mic` | `Daemon::apply_mic` (board.rs) | |
| `Daemon._mic_status` | `Daemon::mic_status` (board.rs) | |
| `Daemon._status_poller` | `Daemon::status_poller` (board.rs) | |
| `Daemon._send_cam` | `Daemon::send_cam` (board.rs) | |
| `Daemon._send_sound` | `Daemon::send_sound` (board.rs) | |
| `Daemon._set_sound` | `Daemon::set_sound` (board.rs) | |
| `Daemon._vision_stats_loop` | `Daemon::vision_stats_loop` (board.rs) | |
| `Daemon._make_identity` | `Daemon::make_identity` (board.rs) | |
| `Daemon._handle_identity` | `Daemon::handle_identity` (board.rs) | |
| `Daemon._start_ears` | `Daemon::start_ears` (voice.rs) | |
| `Daemon._wake_suppressed` | `Daemon::wake_suppressed` (voice.rs) | |
| `Daemon._desk_calls_get` | `Daemon::desk_calls_get` (voice.rs) | |
| `Daemon._on_wake` | `Daemon::on_wake` (voice.rs) | |
| `Daemon._voice_gate_for` | `Daemon::voice_gate_for` (voice.rs) | |
| `Daemon._head_pose_asker` | `Daemon::head_pose_asker` (voice.rs) | |
| `Daemon._resync_agent` | `Daemon::resync_agent` (voice.rs) | |
| `Daemon._agent_keepalive` | `Daemon::agent_keepalive` (voice.rs) | |
| `Daemon._converse` | `Daemon::converse` (voice.rs) | |
| `Daemon._room_notes_taker` | `Daemon::room_notes_taker` (voice.rs) | |
| `Daemon._phone_notes` | `Daemon::phone_notes` (voice.rs) | |
| `Daemon._on_voice_explore` | `Daemon::on_voice_explore` (voice.rs) | |
| `Daemon._type_into_terminal` | `Daemon::type_into_terminal` (voice.rs) | |
| `Daemon._texted_task_running` | `Daemon::texted_task_running` (voice.rs) | |
| `Daemon._desk_has_the_mac` | `Daemon::desk_has_the_mac` (voice.rs) | |
| `Daemon._make_apps` | `Daemon::make_apps` (door.rs) | |
| `Daemon._make_direct` | `Daemon::make_direct` (door.rs) | |
| `Daemon._make_telegram` | `Daemon::make_telegram` (door.rs) | |
| `Daemon._make_chief` | `Daemon::make_chief` (door.rs) | |
| `Daemon._warn_removed_computer` | `Daemon::warn_removed_computer` (agents.rs) | |
| `Daemon._make_agent` | `Daemon::make_agent` (agents.rs) | |
| `Daemon._start_miniapp` | `Daemon::start_miniapp` (door.rs) | |
| `Daemon._make_chrome_lane` | `Daemon::make_chrome_lane` (agents.rs) | |
| `Daemon._app_hub` | `Daemon::app_hub` (apps.rs) | |
| `Daemon._app_hubs` | `Daemon::app_hubs` (apps.rs) | |
| `Daemon._lights_hub` | `Daemon::lights_hub` (apps.rs) | |
| `Daemon._spotify_hub` | `Daemon::spotify_hub` (apps.rs) | |
| `Daemon._firetv_hub` | `Daemon::firetv_hub` (apps.rs) | |
| `Daemon._make_canvas` | `Daemon::make_canvas` (door.rs) | |
| `Daemon._make_share` | `Daemon::make_share` (door.rs) | |
| `Daemon._make_agentcard` | `Daemon::make_agentcard` (door.rs) | |
| `Daemon._make_america` | `Daemon::make_america` (door.rs) | |
| `Daemon._board_say` | `Daemon::board_say` (apps.rs) | |
| `Daemon._make_meeter` | `Daemon::make_meeter` (door.rs) | |
| `Daemon._ask_owner_on_phone` | `Daemon::ask_owner_on_phone` (door.rs) | |
| `Daemon._tell_owner_on_phone` | `Daemon::tell_owner_on_phone` (door.rs) | |
| `Daemon._desk` | `Daemon::desk` (agents.rs) | |
| `Daemon._bodies` | `Daemon::bodies` (agents.rs) | |
| `Daemon._reader_route` | `Daemon::reader_route` (agents.rs) | |
| `Daemon._chrome_body` | `Daemon::chrome_body` (agents.rs) | |
| `Daemon._cancel_active_task` | `Daemon::cancel_active_task` (lesson.rs) | |
| `Daemon._learning_notice` | `Daemon::learning_notice` (lesson.rs) | |
| `Daemon._learning_idle` | `Daemon::learning_idle` (lesson.rs) | |
| `Daemon._make_listener` | `Daemon::make_listener` (lesson.rs) | |
| `Daemon._think_aloud_start` | `Daemon::think_aloud_start` (lesson.rs) | |
| `Daemon._think_aloud_stop` | `Daemon::think_aloud_stop` (lesson.rs) | |
| `Daemon._start_think_aloud_conversation` | `Daemon::start_think_aloud_conversation` (lesson.rs) | |
| `Daemon._on_voice_session` | `Daemon::on_voice_session` (lesson.rs) | |
| `Daemon._on_think_aloud` | `Daemon::on_think_aloud` (lesson.rs) | |
| `Daemon._start_phone_expressions` | `Daemon::start_phone_expressions` (door.rs) | |
| `Daemon._on_expression_text` | `Daemon::on_expression_text` (lesson.rs) | |
| `Daemon._on_caption` | `Daemon::on_caption` (lesson.rs) | |
| `Daemon._on_agent_state` | `Daemon::on_agent_state` (lesson.rs) | |
| `Daemon._sync_listen_pose` | `Daemon::sync_listen_pose` (lesson.rs) | |
| `Daemon._note_activity` | `Daemon::note_activity` (explore.rs) | |
| `Daemon._idle_secs` | `Daemon::idle_secs` (explore.rs) | |
| `Daemon._explore_loop` | `Daemon::explore_loop` (explore.rs) | |
| `Daemon._explore_step` | `Daemon::explore_step` (explore.rs) | |
| `Daemon._run_explore_action` | `Daemon::run_explore_action` (explore.rs) | |
| `Daemon._show_thought` | `Daemon::show_thought` (explore.rs) | |
| `Daemon._flush_thought_pager` | `Daemon::flush_thought_pager` (explore.rs) | |
| `Daemon._thought_caption_loop` | `Daemon::thought_caption_loop` (explore.rs) | |
| `Daemon._clear_thought` | `Daemon::clear_thought` (explore.rs) | |
| `Daemon._send_emote` | `Daemon::send_emote` (explore.rs) | |
| `Daemon._take_snapshot` | `Daemon::take_snapshot` (explore.rs) | |
| `Daemon._photo_for_owner` | `Daemon::photo_for_owner` (explore.rs) | |
| `Daemon._mac_photo` | `Daemon::mac_photo` (explore.rs) | |
| `Daemon._request_explore` | `Daemon::request_explore` (explore.rs) | |
| `Daemon._dismiss_explore` | `Daemon::dismiss_explore` (explore.rs) | |
| `Daemon._stop_explore` | `Daemon::stop_explore` (explore.rs) | |
| `Daemon._on_listen_key` | `Daemon::on_listen_key` (explore.rs) | |
| `Daemon._reset_listen` | `Daemon::reset_listen` (explore.rs) | |
| `Daemon._update_check_loop` | `Daemon::update_check_loop` (explore.rs) | |
| `Daemon._handle_lesson` | `Daemon::handle_lesson` (lesson.rs) | |
| `Daemon._handle_ipc` | `Daemon::handle_ipc` (ipc.rs) | |
| `Daemon._ipc_expressions` | `Daemon::ipc_expressions` (ipc.rs) | |
| `Daemon._ipc_explore` | `Daemon::ipc_explore` (ipc.rs) | |
| `Daemon._ipc_lesson` | `Daemon::ipc_lesson` (ipc.rs) | |
| `Daemon._ipc_notes` | `Daemon::ipc_notes` (ipc.rs) | |
| `Daemon._ipc_trace` | `Daemon::ipc_trace` (ipc.rs) | |
| `Daemon._ipc_pose` | `Daemon::ipc_pose` (ipc.rs) | |
| `Daemon._ipc_follow` | `Daemon::ipc_follow` (ipc.rs) | |
| `Daemon._ipc_axis` | `Daemon::ipc_axis` (ipc.rs) | |
| `Daemon._ipc_move` | `Daemon::ipc_move` (ipc.rs) | |
| `Daemon._ipc_celebrate` | `Daemon::ipc_celebrate` (ipc.rs) | |
| `Daemon._ipc_species` | `Daemon::ipc_species` (ipc.rs) | |
| `Daemon._ipc_diag` | `Daemon::ipc_diag` (ipc.rs) | |
| `Daemon._ipc_identity` | `Daemon::ipc_identity` (ipc.rs) | |
| `Daemon._ipc_session_start` | `Daemon::ipc_session_start` (ipc.rs) | |
| `Daemon._ipc_session_end` | `Daemon::ipc_session_end` (ipc.rs) | |
| `Daemon._ipc_turn_begin` | `Daemon::ipc_turn_begin` (ipc.rs) | |
| `Daemon._ipc_turn_end` | `Daemon::ipc_turn_end` (ipc.rs) | |
| `Daemon._ipc_pretooluse` | `Daemon::ipc_pretooluse` (ipc.rs) | |
| `Daemon._ipc_permissionrequest` | `Daemon::ipc_permissionrequest` (ipc.rs) | |
| `Daemon._ipc_push_character` | `Daemon::ipc_push_character` (ipc.rs) | |
| `Daemon._ipc_unpair` | `Daemon::ipc_unpair` (ipc.rs) | |
| `Daemon._ipc_sound` | `Daemon::ipc_sound` (ipc.rs) | |
| `Daemon._ipc_chat` | `Daemon::ipc_chat` (ipc.rs) | |
| `Daemon._ipc_mcp_tools` | `Daemon::ipc_mcp_tools` (ipc.rs) | |
| `Daemon._ipc_mcp_call` | `Daemon::ipc_mcp_call` (ipc.rs) | |
| `Daemon._ipc_mic` | `Daemon::ipc_mic` (ipc.rs) | |
| `Daemon._ipc_get_state` | `Daemon::ipc_get_state` (ipc.rs) | |
| `Daemon._ipc_posttooluse` | `Daemon::ipc_posttooluse` (ipc.rs) | |
| `Daemon._ipc_notification` | `Daemon::ipc_notification` (ipc.rs) | |
| `Daemon._handle_permission_request` | `Daemon::handle_permission_request` (hooks.rs) | |
| `Daemon._handle_pretooluse` | `Daemon::handle_pretooluse` (hooks.rs) | |
| `Daemon._command_risk` | `Daemon::command_risk` (hooks.rs) | |
| `Daemon._shadow_command_risk` | `Daemon::shadow_command_risk` (hooks.rs) | |
| `Daemon._ensure_session` | `Daemon::ensure_session` (hooks.rs) | |
| `Daemon._handle_read_pretooluse` | `Daemon::handle_read_pretooluse` (hooks.rs) | |
| `Daemon._handle_ble` | `Daemon::handle_ble` (ble_handler.rs) | |
| `Daemon._on_tokens` | `Daemon::on_tokens` (ble_handler.rs) | |
| `Daemon._on_assistant_text` | `Daemon::on_assistant_text` (ble_handler.rs) | |
| `Daemon._turn_end_side_effects` | `Daemon::turn_end_side_effects` (ble_handler.rs) | |
| `Daemon._heartbeat_after` | `Daemon::heartbeat_after` (ble_handler.rs) | |
| `Daemon._deferred_turn_end` | `Daemon::deferred_turn_end` (ble_handler.rs) | |
| `Daemon.expect_ack` | `Daemon::expect_ack` (ble_handler.rs) | |
| `Daemon.wait_for_ack` | `Daemon::wait_for_ack` (ble_handler.rs) | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `test_daemon_background.py` | 2 | `tests/ipc_hooks.rs` (2) | none. The real side effects run here, so the job left after the side-effects task is the follow-up `heartbeat-after` push (Python stubbed the side effects) |
| `test_daemon_explore.py` | 21 | `tests/voice_lesson.rs`, `tests/board.rs` (music input) | none |
| `test_daemon_lesson.py` | 32 | `tests/voice_lesson.rs` (the 22 daemon tests) | the 10 `cli.main lesson ...` tests: they test `cli.py`, ported in eve-bridge |
| `test_daemon_membus.py` | 17 | `tests/run_memory.rs`, `tests/voice_lesson.rs` (`/eve/state`, the IPC lesson action) | `test_the_diary_only_calls_on_written_for_records_it_wrote` (eve-diary's own tests cover the gate); `test_missing_modules_are_a_warning_not_a_crash` and `test_the_bus_is_optional_on_old_stubs` (Python import failures and stub daemons without a bus; the Rust sinks are compiled in and every daemon has a bus) |
| `test_daemon_scene.py` | 9 | `tests/board.rs` | none |
| `test_daemon_task_cancel.py` | 2 | `tests/voice_lesson.rs` | none |
| `test_daemon_touch.py` | 5 | `tests/board.rs` | none |
| `test_daemon_transcripts.py` | 17 | `tests/run_memory.rs` | `test_with_memory_on_the_inlet_gets_the_same_memory`: building the full text door reads process-wide configuration (the watcher's `Ctx::global`, the Responses creator's key) and opens the owner's files (the watch list, the chat log); the brief it checks is `eve_records::opening_brief`, tested there |
| `tests_ref`: `test_ipc_dispatch.py`, `test_ipc_chat.py`, `test_command_risk.py`, `test_permission_matrix.py` (the 2923-case golden master), `test_relay_questions.py`, `test_eve_mcp.py`, `test_telegram.py` (the phone's decision, the relay) | the daemon's parts | `tests/ipc_hooks.rs` | Python's monkeypatch-only dispatch test; the matrix test leaves out the shadow gate's `jev_shadow` row, which Python's test stubbed away |
| `tests_ref`: `test_chrome_lane.py`, `test_chrome_consent.py`, `test_phone_expressions.py`, `test_telegram.py`, `test_telegram_chief.py` | the daemon's parts | `tests/door.rs` | the parts that build a working text door (they need the process environment; see above). Python's positive control of the chief switch (it would write the owner's real chief ledger) |
| `tests_ref`: `test_telegram_chief.py`, `test_web_reader.py`, `test_chrome_lane.py`, `test_codex_computer.py`, `test_desk_lane.py`, `test_app_registry.py` | the daemon's parts | `tests/agents.rs` | none |
| other `tests_ref` files | | | they exercise their own crates; the daemon's part of each is covered by the rows above |

Plus 7 unit tests in `src/` (Python's `repr`, `float`, `round` and truthiness helpers, `textwrap.wrap`, the transcript stem).

## Boundaries
None. The daemon calls the upstream crates; the subprocess boundaries are theirs (osascript in `type_into_terminal`
and the terminal raise, `git` in `code_origin`).

## Upstream edits
- eve-web: `Watcher::set_notify` (Python's `watcher.notify = ...`), so the daemon can lend the text door's
  `tell_owner` after the watcher is made.
- eve-brain: `ChatMaker.building` is `Arc<Mutex<HashSet<String>>>` and `ChatMaker::sharing` builds one around a
  given set (Python's `door.building = self._miniapp.changing`), so the chat and the Mini App share one set of apps
  being changed. `ChatMaker::new` is unchanged for callers.
- eve-brain (door unit): `telegram/inlet/door.rs`, the door's accessors the daemon's `Inlet` mapping reads.

## Differences
- Construction: `Daemon::new` checks the IPC address and the transcript roots first, since `IpcServer::new` and
  `JsonlTailer::new` can fail; inside `assemble` a failure is logged and the field is left `None`.
- A `build_heartbeat` timestamp error is logged and that push is skipped (Python raised).
- An error from a serial or controller `on_message` handler is logged as "on_message handler crashed" (Python left
  the task's exception unretrieved). A BLE line that is not an object is logged at debug and dropped.
- `handle_ipc`: a request that is not an object answers `{ok: false, error: "AttributeError: ..."}`. An `OSError`
  from the identity reset answers `{ok: false, error: "OSError: ..."}`, the IPC server's error shape.
- With no Claude config home, the permission summary logs a warning and returns.
- `_ipc_pose`'s `at` is JSON `null` before the board echoes a pose, where Python wrote `-Infinity` (a JSON value
  cannot hold a non-finite float).
- `mcp_late_done` cannot see an exception class (a `Job` carries none), so it logs "done" for any call that was not
  cancelled; `ipc_mcp_call` logs the real error itself when a late call fails.
- The `finally` blocks of the expression audition, the follow test and the voice conversation run from drop guards.
  The conversation's close (unsubscribe, keepalive cancel) is armed before the speaker gate loads, so a hush during
  the gate load does not leak the mic as Python's did; the follow test's async cleanup is spawned when cancelled.
- `open_session` errors are logged as "voice: conversation failed: {err}" without Python's exception type name; an
  `ExploreRefused` after a conversation is logged at info instead of escaping the task.
- `photo_for_owner`: a `DiaryTaker.keep` or note-append IO error is logged and answered "the picture could not be
  saved" (Python raised). A failed osascript spawn in `type_into_terminal` returns the refusal line.
- A `LearningError::Other` is logged as "Exception", not by a Python type name.
- The Work sessions see the daemon's own `State` (shared `Arc`), as Python lent `lambda: self.state`.
- The watcher's and the Meet notetaker's notify are set when they are made (in `make_telegram`/`make_meeter`) and
  read the text door at call time; Python set them in `run()` after the door was made. With no door at call time
  the watcher's send did not go (`false`) and the notetaker logs "meet: (no chat to tell)".
- `make_direct` starts the AppsBridge synchronously before it is shared (`AppsBridge::start` takes `&mut self`);
  Python started it on a background thread.
- A `ChatLog` config error (an infinite `EVE_CHAT_LOG_LINES`) is logged and the log stays off; Python raised out of
  `_make_telegram`. `ask_owner_on_phone` returns "" and logs where Python raised; `ask_owner_checked` keeps the error
  for the Chrome consent broker. `_start_miniapp`'s anthropic-SDK import check is dropped (no SDK to import).
- Python exception names in the agentcard, America and direct-apps log lines are approximated: a panic is
  `PanicException`, an invalid transport `ValueError`, anything else `RuntimeError`. An unknown create or ask error
  reaches the chief as `Raised("Exception", text)`.
- App hubs: an error Python's `hub.handle` or `screenshot_for_model` raised (Spotify's, the Fire TV's IO error)
  comes back as the tool result `{ok: false, reason: <error>}`; an error from Spotify's code-path run is logged and
  answers ""; `board_say` logs a speaking failure instead of raising (Spotify mode gets the result through
  `board_say_result`).
- `chrome_body`: a failing `make_response_creator` logs and returns no bodies, where Python raised.
  `make_chrome_lane` keeps the lane in the daemon's state and returns it on later calls.
- `agent_factory` holds the daemon weakly; once the daemon is gone it builds a plain Codex agent.
- The places loop runs on a blocking thread of the same runtime (its future is not `Send`) and ends on the shutdown
  flag.
