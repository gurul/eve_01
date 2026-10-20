# eve-obsbot-panel
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/obsbot_panel.py` | `src/lib.rs` (constants, tools, `where`, `Command`, `match`, `enabled`), `src/panel.rs` (`ObsbotPanel`), `src/capture.rs` (`frames`, `Capture`), `src/rtc.rs` (the WebRTC half of `ObsbotPanel`); tests in `src/tests.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `HEARTBEAT_SECS`, `HOLD_SPEED`, `VIDEO_FIRST_SECS`, `VIDEO_FRAME_SECS`, `MAX_VIDEO_BUFFER`, `VIDEO_WIDTH`, `VIDEO_FPS`, `LINGER_SECS`, `MAX_PEERS`, `DEVICE_RE`, `ENDED_KEEP` | same names | values verbatim; `DEVICE_RE` is a `LazyLock<Regex>` |
| `VIDEO_OFF_LINE`, `VIDEO_DENIED_LINE`, `NO_FFMPEG_LINE`, `OFF_REASON`, `NOT_SET_UP_LINE` | same names | verbatim |
| `ACTIONS`, `TURN_DEGREES`, `BIT_DEGREES`, `LOT_DEGREES`, `ZOOM_STEP`, `TILT_RANGE` | same names | `TILT_RANGE` is `(-59, 59)` from `eve_obsbot::PITCH_RANGE` |
| `TOOLS`, `TOOL_NAMES` | `TOOLS: LazyLock<Vec<Value>>`, `TOOL_NAMES` | key order kept; `json.dumps` of it is byte-identical (tested against Python's output) |
| `INSTRUCTIONS` | `INSTRUCTIONS: LazyLock<String>` | byte-identical (tested) |
| `where` | `where_(Option<i64>, Option<i64>)` (+ crate `where_value` over reply values) | `where` is a Rust keyword; ints and floats format as Python's `str` |
| `Command` | `Command` (+ `new`, `turn`, `zoom_to`, `zoom_by`, `preset` constructors) | same fields and defaults; `PartialEq` as the frozen dataclass |
| `_turn` | `turn_of` (private) | 1-120 or None |
| `_zoom_step` | `zoom_step` (private) | |
| `_LEAD`, `_TAIL`, `_CAM`, `_TURN_VERB`, `_AMOUNT_NAMED`, `_DIR_NAMED`, `_ZOOM_AMOUNT`, `_PATTERNS` | `LEAD` ... `ZOOM_AMOUNT`, `PATTERNS` (private) | same patterns in the same order, compiled once as `^(?:LEAD(?:p)TAIL)$` (Python's `re.fullmatch`) |
| `_norm` | `norm` (private) | lower, `’` to `'`, Python's whitespace set (`\s` plus `\x1c-\x1f`) |
| `match` | `r#match(text)` | `match` is a Rust keyword |
| `enabled` | `enabled(Option<&HashMap>)` | None reads the process env; `strip()` with Python's whitespace; `APP_PATH` exists or `eve_obsbot_usb::present` (ioreg) |
| `find_ffmpeg` | `find_ffmpeg()` | `which ffmpeg`, then the two Homebrew paths |
| `ObsbotPanel` | `ObsbotPanel` (clones share one panel), `PanelOptions` | `make`, `ffmpeg`, `environ`, `clock` as Python; plus `linger_secs` (the tests patched `LINGER_SECS`), `stun` and `udp_addrs` (where WebRTC peers bind) |
| `ObsbotPanel.__init__` | `ObsbotPanel::new(PanelOptions)`, `ObsbotPanel::with_make` | default make is `Obsbot::new()` |
| (`obsbot.Obsbot` as the panel uses it) | `trait Camera` (implemented for `eve_obsbot::Obsbot`), `trait Turner`, `UsbTurner`, `MakeCamera` | the seam the Python tests filled with `FakeCam`; `Camera::turner` is `cam.usb` for hold-to-turn |
| `ObsbotPanel.available` | `available()` | |
| `ObsbotPanel._ready` | `ready()` (public: the tests call it) | fast path on an atomic flag mirroring `cam._ready`, no lock |
| `ObsbotPanel._read` | `read` (private) | under the camera lock on a blocking thread; a failed read clears ready |
| `ObsbotPanel.panel` | `panel(&Value) -> Value` | same actions, replies (key order) and lines |
| `ObsbotPanel.tools` | `tools()` | a copy |
| `ObsbotPanel.instructions` | `instructions()` | |
| `ObsbotPanel.match` | `r#match(&self, text)` | |
| `ObsbotPanel.handle` | `handle(name, &Value) -> Value` | |
| `ObsbotPanel.control` | `control(&Map) -> Value` | the keyword arguments as a map; a missing key is None |
| `ObsbotPanel._moved` | `moved` (crate) | |
| `ObsbotPanel.run` | `run(&Command) -> String` | same log line |
| `ObsbotPanel._turn` | `turn` (private) | |
| `ObsbotPanel._end` | `Hold::end` (private) | keeps the last `ENDED_KEEP` ids |
| `ObsbotPanel._stop_turning` | `stop_turning` (private) | |
| `ObsbotPanel._watchdog` | `watchdog` (private, spawned) | 0.1 s tick, same log line |
| `ObsbotPanel._device` | `capture::device` / `device_in` (crate) | same argv, regex, stop at "audio devices", same log line |
| `ObsbotPanel.frames` | `frames() -> FrameStream` | `Ok(jpeg)` items; `Err(VideoError)` is Python's `LookupError` with the owner's line; dropping the stream kills ffmpeg (`kill_on_drop`, and an explicit kill and reap where Python's `finally` ran) |
| `ObsbotPanel.stream` | `stream(&mut impl AsyncWrite)` | `VIDEO_HEAD`, chunked `video_part`s (eve-pika), 503 JSON via `eve_pika::pika::write_json`, empty chunk when the picture ends |
| `ObsbotPanel._rtc` | `rtc` (crate) | same checks (`MAX_SDP_CHARS` in characters), 8 s limit, `{"ok", "rtc", "sdp", "line"}` replies, log line with the error kind |
| `ObsbotPanel._answer` | `answer` (private) | webrtc-rs (as eve-pika's `PikaRtc`): oldest peer closed past `MAX_PEERS`, STUN server, non-trickle answer |
| `ObsbotPanel._answer._channel`, `._state` | `Handler::on_data_channel`, `on_connection_state_change` | a channel not labelled `video`, or for a closed peer, is closed; failed/closed ends the peer |
| `ObsbotPanel._send` | `send` (private) | 10 s open wait, `MAX_JPEG`/`BUFFER_LIMIT` drop rule (`outstanding_bytes` for `bufferedAmount`), the line as text on an error, `RETRY_SECS` retry, same log lines |
| `ObsbotPanel._close_peer` | `close_peer` (crate) | |
| `ObsbotPanel.close` | `close()` | stop turning, close peers, stop the capture, close the camera |
| (`len(self._peers)`) | `peers()` | for tests and callers |
| `ObsbotPanel._hold`, `_capture` | `holding()`, `capture()` | read accessors |
| `Capture` | `Capture` (clones share one), `Viewer`, `VideoError`, `FrameResult`, `FrameStream`, `FramesFn` | |
| `Capture.__init__` | `Capture::new(FramesFn)`, `Capture::with_linger` | |
| `Capture.running` | `running()` | |
| `Capture.jpeg`, `seq`, `error`, `viewers` | `jpeg()`, `seq()`, `error()`, `viewers()` | |
| `Capture._publish` | `CapInner::publish` (private) | a `watch` channel bump stands for set-and-replace of the `asyncio.Event` |
| `Capture._run` | `run` (private) | a drop guard is the `finally` (also on cancel) |
| `Capture.watch` | `watch() -> Viewer`, `Viewer::next`, `Viewer::close` | joins at the first `next` like the generator; `Drop`/`close` is `aclose` |
| `Capture._stop_later` | `stop_later` (private) | |
| `Capture.stop` | `stop()` | |
| `_num` | `num(Option<&Value>) -> Option<f64>` | bool and None are None; strings by Python's `float()` rules (`eve_obsbot::py::parse_float`) |
| `make_panel` | `make_panel(Option<&HashMap>) -> Option<ObsbotPanel>` | makes nothing until used |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_obsbot_panel.py` | `test_a_press_wakes_and_turns_once_and_a_stop_ends_it`, `test_a_new_direction_replaces_the_turn`, `test_a_stop_during_the_first_beats_wake_never_turns`, `test_no_beat_stops_the_gimbal`, `test_beats_keep_it_turning_past_the_heartbeat`, `test_reads_and_refusals`, `test_it_is_listed_and_its_panel_is_behind_the_link_token`, `test_two_viewers_share_one_capture_and_ffmpeg_stops_after_both`, `test_the_capture_hands_every_viewer_the_newest_frame`, `test_the_picture_says_why_it_cant_start` | same names in `src/tests.rs` except `test_it_is_listed_only_when_it_is_on` for the listing test; plus `test_ended_presses_are_remembered_up_to_the_limit`, `test_panel_lines_and_numbers_as_python_reads_them`, `test_a_capture_that_ends_says_the_picture_is_off_and_lingers_then_stops`, `test_ffmpeg_listing_finds_the_obsbot_by_name`, `test_webrtc_viewers_share_the_capture_with_the_macs_stamp`, `test_webrtc_says_why_the_picture_cant_start_on_the_channel` | The link token, the 401s and the `/api/link/controls` listing in `test_it_is_listed_and_its_panel_is_behind_the_link_token` test link_chat's door and belong to its port; this crate keeps `available`/`make_panel` and that nothing reaches the camera. The two-viewer test reads `stream` directly over in-memory pipes (with eve-pika's `Body` and `split_jpegs`) instead of through link_chat's server, so it runs without OBSBOT Center installed. The fake ffmpeg is a `/bin/sh` script with the Python one's listing, refusal and JPEG output. |
| `bridge/tests/test_obsbot_brain.py` | `test_tools_are_strict_and_complete`, `test_instructions_name_the_limits`, `test_match` (20 cases), `test_match_leaves_the_rest_to_the_model` (10 cases), `test_handle_status_turn_point_zoom_center`, `test_handle_refusals`, `test_run_is_the_code_path_line`, `test_where_reads_in_the_owners_words` | same names (every case, plus 12 more `match` cases checked against Python's output on 2026-10-19), plus `test_tools_and_instructions_are_pythons_bytes`, `test_moved_says_where_it_stopped`, `test_num_reads_like_pythons_float` | none; `FakeUsb` and the patched `time.sleep` are a `UsbControl` fake and a `Clock` over a real `eve_obsbot::Obsbot` on port 1 |
| `bridge/tests/test_telegram.py` (tests_ref) | `test_camera_by_code_and_by_the_brain`, `test_a_camera_command_without_a_camera`, `test_the_camera_tool_without_a_camera_is_a_reason` | `test_match` covers `match("camera left") == Command("turn", "left", 20)` | the chat's routing (by code, by the brain, `NOT_SET_UP_LINE`, `OFF_REASON` as the tool's reason) belongs to the telegram port; this crate exports `TOOLS`, `TOOL_NAMES`, `INSTRUCTIONS`, `Command`, `NOT_SET_UP_LINE`, `OFF_REASON`, `enabled` |
| `bridge/tests/test_voice_agent.py` (tests_ref) | reads `obsbot_panel.TOOLS` and `INSTRUCTIONS` | `test_tools_and_instructions_are_pythons_bytes` | the voice agent's use belongs to its port |

## Boundaries
- ffmpeg (device listing and the MJPEG capture) is a subprocess, as in Python.
- The camera itself is eve-obsbot (OSC) and eve-obsbot-usb (the C helper subprocess); WebRTC is webrtc-rs 0.21.1 in process (the version eve-pika uses), where the Python imported aiortc.

## Differences
- Hold-to-turn uses its own `UsbCamera` handles (`UsbTurner`: one for the wake, one for turns) instead of sharing the reading camera's `usb` object across threads, so a stop never waits behind a move's read-back or a wake. Both run the same helper binary; `why` after a failed wake is the waking handle's.
- Where Python would raise out of the panel, Rust answers: a camera that can't be made (Obsbot's socket bind) is the not-ready line with the error text; `zoom` with a level of inf or nan is "Zoom is 0 to 100." (Python: `OverflowError`/`ValueError`); `preset` with inf or nan sends 0, which the camera refuses with its own line; `control` called before the camera was made asks for it first (Python: `AssertionError`); a `track` reply without a line gets one appended rather than a `KeyError`.
- ffmpeg that can't be started (listing or capture) logs and reads as `VIDEO_OFF_LINE`; Python raised `OSError` out of `frames()`, which the shared capture also turned into `VIDEO_OFF_LINE`. A failed read of ffmpeg's stdout is treated as its end.
- `match`: Rust's `\w` is Unicode's word class, Python's is `isalnum()` plus `_` (they differ on combining marks, connector punctuation and No-category numerals like `²`), so a few non-Latin characters normalize differently. A degree amount in non-ASCII digits matches as in Python but is left to the model (Rust can't read it as a number).
- `find_ffmpeg` uses the `which` crate; with PATH unset Python's `shutil.which` falls back to the system default path, `which` finds nothing and the Homebrew paths are tried.
- The WebRTC error's log line names the webrtc-rs error variant (aiortc's exception class in Python); the peers bind every IPv4 interface (`0.0.0.0:0`), aiortc also gathered IPv6 host candidates (as eve-pika records).
- `close()` waits for an in-flight camera read before closing the camera; Python closed it at once from the event loop.
