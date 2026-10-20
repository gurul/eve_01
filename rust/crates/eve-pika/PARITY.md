# eve-pika
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/pika.py` | `src/pika.rs` |
| `eve_bridge/pika_rtc.py` | `src/pika_rtc.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `pika.PikaConfig` | `pika::PikaConfig` | Same fields and defaults (`Default`). Ports are `u16`, speeds `i64`. |
| `pika.enabled(environ)` | `pika::enabled(Option<&HashMap>)` | `None` reads the process env. Same truthy set, Python `strip()` white space. |
| `pika.load(path)` | `pika::load(Option<&Path>)` | Same file (`~/.config/eve-bridge/pika.json`), same fallbacks: a float, bool or out-of-range number takes the default; the host is stripped; `stun` keeps Python's `$` (one trailing newline allowed). |
| `pika.make_pika(path, environ)` | `pika::make_pika` | Same log lines; attaches `PikaRtc` when `rtc` is on. |
| `pika.frame(tag, **fields)` | `pika::frame(tag, &[(k, v)])` | Compact JSON, `"H"` first, byte-identical. |
| `pika.drive_frame` | `pika::drive_frame(tag, dir, speed, ms)` | `ms` explicit (Python default `PULSE_MS`). |
| `pika.stop_frame` | `pika::stop_frame` | |
| `pika.split_frames` | `pika::split_frames` | |
| `pika.split_jpegs` | `pika::split_jpegs` | |
| `pika.stamp_ms` | `pika::stamp_ms` | |
| `pika.video_part` | `pika::video_part(jpeg, Option<stamp>)` | |
| `pika._body` | `pika::Body` (`Body::new(reader, chunked).next()`) | De-chunks like the generator; chunk sizes parse as `int(x, 16)`. |
| `pika._open` | `pika::open` (+ `pika::resolve_ipv4`, `pika::default_opener`) | IPv4 only through `getaddrinfo(AF_INET)`, as `family=socket.AF_INET`. |
| `pika._chunk` | `pika::write_chunk` | |
| `pika._json` | `pika::write_json` (+ `pika::py_json_dumps`) | Body formatted as `json.dumps` writes it (`", "`, `": "`, ASCII escapes). |
| `pika.PikaError` | `pika::PikaError` | `Display` is the owner's line. |
| `pika.Pika` | `pika::Pika` (`Pika::new`, `Pika::with_options(cfg, PikaOptions)`) | Clones share one panel. `opener`, `clock`, `heartbeat_secs`, `idle_secs`, `hello_secs` and `tick_secs` are `PikaOptions` fields; `drive_framer` is the seam the tests patched `pika.drive_frame` through. `Pika.available` is `Pika::AVAILABLE`. |
| `Pika.connected` | `Pika::connected` | True only after the hello was answered. |
| `Pika._next_tag`, `Pika._end`, `Pika._write`, `Pika._drop` | private `State` methods | Same wrap at 99999, same 128 ended holds, same "not connected" error. |
| `Pika.panel` | `Pika::panel(&Value) -> Value` | Same actions, replies (key order kept) and lines. |
| `Pika._rtc` | private `Pika::rtc_answer` | Same 8 s limit and refusals. |
| `Pika._stats` | private `Pika::stats` (+ `pika::stats_line`) | Same log line format. |
| `Pika._drive` | private `Pika::drive` | |
| `Pika.stop` | `Pika::stop` | A plain `fn`: it never awaits. |
| `Pika._connect`, `Pika._dial` | private `Pika::connect`, `Pika::dial` | One shared dial (`Shared` future over a spawned task) as `asyncio.shield(ensure_future)`. |
| `Pika._read` | private `Pika::read` | Answers `{Heartbeat}`; a hang-up ends the press with LOST_LINE. |
| `Pika._watchdog` | private `Pika::watchdog` | |
| `Pika._release` | private `Pika::release` | Waits up to 0.5 s for the stop to be written before closing. |
| `Pika.close` | `Pika::close(why)` | |
| `Pika.frames` | `Pika::frames() -> Frames` (`Frames::next`) | Newest JPEG only; `Some(Err)` first when the camera is off or busy; dropping it closes the camera. |
| `Pika._take_video`, `Pika._left_video`, `Pika._end_video` | crate-private `take_video` (returns a `VideoSlot` guard), `left_video`, `end_video` | Task cancellation becomes a cancel signal the holder selects on; dropping the slot gives it back. `Pika::video_active` reads `_video is not None`. |
| `Pika.stream(writer)` | `Pika::stream(&mut impl AsyncWrite)` | Same head, chunks, `X-Stamp` and 503 JSON. Returns when a newer viewer takes the camera. |
| `pika_rtc.frame_message` | `pika_rtc::frame_message` | 8-byte big-endian stamp, then the JPEG. |
| `pika_rtc.parse_message` | `pika_rtc::parse_message -> Option` | `None` where Python raised `struct.error`. |
| `pika_rtc.available` | `pika_rtc::available` | Always true: the stack is compiled in. |
| `pika_rtc.PikaRtc` | `pika_rtc::PikaRtc` (`new`, `with_udp_addrs`) | Holds a weak handle to the panel. `open` is `is_open`; `sent`/`dropped` are counters. |
| `PikaRtc.answer` | `PikaRtc::answer` | Non-trickle answer: waits for ICE gathering to complete. |
| `PikaRtc._send` | private `PikaRtc::send` / `relay` | Same 10 s open wait, drop rule, text line on a camera error, 3 s retry. |
| `PikaRtc.close` | `PikaRtc::close` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_pika.py` | test_frames_are_the_car_repos_bytes_exactly, test_a_held_drive_then_release_is_hello_drive_stop_byte_for_byte, test_beats_renew_the_timed_move_but_not_faster_than_resend, test_a_missed_heartbeat_stops_and_the_late_beat_never_restarts_it, test_a_beat_that_crossed_the_release_in_the_tunnel_is_refused, test_a_release_while_connecting_means_no_drive_frame_at_all, test_the_car_hanging_up_ends_the_press, test_an_error_mid_drive_sends_a_stop, test_close_stops_and_lets_go_of_the_slot, test_the_slot_is_let_go_after_idle, test_a_busy_car_says_so_in_one_line_and_lets_go, test_a_second_beat_during_the_hello_writes_no_drive_frame, test_an_unreachable_car_says_so, test_off_by_default_and_without_config, test_the_video_is_relayed_one_viewer_at_a_time, test_a_camera_that_is_off_is_one_plain_line, test_split_jpegs_keeps_a_partial_frame, test_the_car_is_dialled_over_ipv4_only | `tests/pika.rs`: same names, plus test_the_app_hanging_up_lets_go_of_the_camera, test_a_camera_that_answers_no_is_busy, test_the_chunked_body_is_dechunked, test_the_stream_head_is_the_pythons_bytes | test_the_control_and_the_video_need_the_link_token and the listing half of test_pika_is_listed_only_when_it_is_on test link_chat's door (token, origin, `/api/link/controls` listing); they belong to the link_chat port. `tests/pika.rs::test_pika_is_listed_only_when_it_is_on` keeps this crate's half (a panel sends nothing until the first drive). The video tests call `Pika::stream` directly over a pipe instead of through the door. |
| `bridge/tests/test_pika_rtc.py` | test_frames_arrive_byte_for_byte_with_the_macs_stamp, test_the_slot_is_shared_with_the_tunnel_stream_and_the_newest_wins, test_close_ends_the_peer_and_the_camera_socket, test_a_newer_offer_replaces_the_older_peer, test_a_camera_that_is_off_is_a_line_on_the_channel, test_a_bad_offer_or_no_webrtc_sends_the_app_to_the_tunnel, test_stats_are_logged_per_path_and_bad_ones_refused, test_the_config_turns_webrtc_off_and_checks_the_stun_server, test_the_frame_message_is_stamp_then_jpeg | `tests/pika_rtc.rs`: same names (a real webrtc-rs peer plays the phone, both on loopback), plus `#[ignore]` live test_the_default_bind_reaches_a_phone_on_the_lan | None. |
| `bridge/tests/test_obsbot_panel.py` (tests_ref) | uses `pika._body` and `pika.split_jpegs` as stream-reading helpers | covered by test_the_chunked_body_is_dechunked and test_split_jpegs_keeps_a_partial_frame | The OBSBOT panel tests themselves belong to the obsbot_panel port. |

Unit tests in `src/`: `py_json_dumps` matches `json.dumps`, chunk sizes parse as `int(x, 16)`, the stats line formats as `%d`/`%.0f`/`%.1f`, `STUN_RE` keeps Python's `$`, `strip()` uses Python's white-space set, and the frame message layout.

## Boundaries
None. WebRTC uses `webrtc` 0.21.1 (webrtc-rs) in process, where the Python imported aiortc.

## Differences
- WebRTC host candidates: the peer binds every IPv4 interface (`0.0.0.0:0`, one socket per interface). aiortc also gathered IPv6 host candidates. The car's video reaches the phone on the home Wi-Fi over IPv4 and off it through STUN (IPv4), so this changes no working path.
- The channel's backlog is read with webrtc-rs `outstanding_bytes()` (bytes handed to the channel and not yet acknowledged), the closest measure to aiortc's `bufferedAmount`; the 96 KiB drop rule is unchanged.
- `PikaRtc` is always available (the stack is compiled in), so with `rtc` on the panel always offers WebRTC. In Python it was offered only when aiortc was installed.
- A new connection to the car closes any previous, already-failed socket explicitly; Python left it to garbage collection. No working path changes.
- The log lines naming an error class use the Python class name mapped from the I/O error kind (`ConnectionRefusedError`, `gaierror`, `TimeoutError`, ...); a WebRTC answer error names the webrtc-rs error variant.
