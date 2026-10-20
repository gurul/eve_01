# eve-obsbot
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/obsbot.py` | `src/lib.rs` (Python behaviors it leans on in `src/py.rs`, tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `HOST`, `PORT`, `APP`, `APP_PATH`, `INI_PATH`, `ARRIVE_WAIT`, `G`, `T` | same names | `APP_PATH`/`INI_PATH` are `&str` (`INI_PATH` keeps its `~`, expanded by `expand_user`) |
| `PAN_RANGE`, `PITCH_RANGE` | same names, `(f64, f64)` | `PITCH_RANGE` is `±eve_obsbot_usb::TILT_LIMIT` |
| `DIRECTIONS`, `FOV`, `AI_MODES`, `FRAMING`, `TRACK_SPEED` | same names, ordered `[(key, value)]` arrays; `keys()` is Python's `list(...)` | order kept: the refusal lines join the keys |
| `OFF_REASON`, `APP_NEEDED_LINE` | same names | verbatim |
| `_pad` | `pad` (private) | always 1-4 nulls, as Python |
| `encode` | `encode(address, &[OscValue]) -> Result<Vec<u8>, OscError>` | `OscValue::Int` packs `>i`, `Float` packs `>f`, `Str` padded UTF-8; an int outside int32 or a finite float too big for f32 is `OscError` with struct's message (Python raises `struct.error`/`OverflowError`) |
| `decode` | `decode(&[u8]) -> Result<(String, Vec<OscValue>), OscError>` | lossy UTF-8 (`"replace"`), floats rounded to 2 places like `round(x, 2)`, an unknown tag stops the read; Python's `ValueError`/`struct.error` are `OscError::NoTerminator`/`Short` |
| (Python int/float/str) | `OscValue`, `values_repr` | `values_repr` is Python's `repr` of a value list, as `raw` prints replies: built from the values themselves, so a NaN or infinity prints `nan`/`inf`, and strings escape what `str.isprintable()` refuses (`py::str_repr`, by Unicode general category) |
| `_app_running` | `app_running()` | `pgrep -x OBSBOT_Main`; a pgrep that can't run is false |
| `Obsbot` | `Obsbot`, `Options` (`Obsbot::options().port(..).app_running(..).usb(..).clock(..).open()`) | `usb` is any `eve_obsbot_usb::UsbControl` (the tests' fake); the `Clock` seam stands for the tests' patched `time.sleep`; `_ready`/`_osc_ready` are the public fields `ready`/`osc_ready` (the panel resets `ready`) |
| `Obsbot.__init__` | `Obsbot::with(Options)`, `Obsbot::new()` | binds 127.0.0.1 for a local host, else all interfaces, port 0; returns the bind error |
| `Obsbot.close` | `Obsbot::close` | `Disconnected 0` only after OSC answered; send errors ignored |
| `Obsbot.send` | `Obsbot::send(name, &[OscValue]) -> io::Result<()>` | a name without `/` goes under `G`; resolves the host to IPv4 per send, as an AF_INET socket does |
| `Obsbot._recv_until`, `_recv_raw`, `_drain` | `recv_until`, `recv_raw`, `drain` (private) | 65536-byte reads; undecodable packets skipped / end the raw read |
| `Obsbot.ask` | `Obsbot::ask` | drains, sends the device index alone, waits for the reply suffix |
| `Obsbot.ensure` | `Obsbot::ensure -> Option<String>` | |
| `Obsbot.osc` | `Obsbot::osc -> Option<String>` | `Connected 0`, 0.5 s for `ConnectedResp`, else `OFF_REASON` when the app runs, else `APP_NEEDED_LINE` |
| `Obsbot.info` | `Obsbot::info -> Option<Res>` | |
| `Obsbot.position` | `Obsbot::position -> Option<(i64, i64)>` | `round` is half to even, as Python's |
| `Obsbot.zoom_level` | `Obsbot::zoom_level` | `int()` truncates a float |
| `Obsbot.presets` | `Obsbot::presets -> Vec<Preset>` | `Preset { n, set, name }` serializes as Python's dict |
| `Obsbot.position_reply`, `zoom_reply`, `locked` | same names | |
| `Obsbot.status` | `Obsbot::status -> Res` | same keys in the same order, same line |
| `Obsbot.wake` | `Obsbot::wake(bool)` | wake over USB; sleep over OSC, sent twice at most, polled every 0.5 s for 5 s, "crashed" when the app dies |
| `Obsbot.move` | `Obsbot::move_to(Option<f64>, Option<f64>)` | `move` is a Rust keyword; clamps like Python's `max(lo, min(hi, v))` (NaN goes to the top) and sends tilt `-pitch` as an int (never `-0.00`) |
| `Obsbot._settle` | `settle` (private) | |
| `Obsbot.nudge`, `stop`, `reset`, `zoom` | same names | `reset` replaces `line` in place (key order kept) |
| `Obsbot._app`, `_tiny3` | `app`, `tiny3` (private) | |
| `Obsbot.fov`, `preset`, `track`, `mode`, `framing`, `track_speed`, `focus`, `mirror` | same names | same checks in the same order, same lines |
| `setup` | `setup(ini, out, usb)`, `setup_with(ini, out, usb, app_path, app_running)`, `expand_user` | the tests patch `APP_PATH` and `_app_running`, so `setup_with` takes them; the ini edit (replace in place, insert the missing keys after `[SoftSetting]` or append the section, `"\n".join + "\n"`) is line for line Python's |
| `_number` | `number(&str) -> OscValue` | Python's `int()` then `float()` string rules (whitespace, sign, underscores, inf/nan) |
| `cli` | `cli(argv, out, make) -> i32`, `main_with_args(argv)` | clap builder with argparse's subcommands, choices, help text and defaults; `--json` anywhere and `--device N` after the first word are moved to the front exactly as Python does; `make(device)` opens the camera; prints `json.dumps(res, ensure_ascii=False)` (", " and ": " separators, Python float repr) or the line; exit 0 when ok, 1 when not, clap's 0/2 for help/usage errors like argparse |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_obsbot.py` | `test_osc_round_trip`, `test_status_over_usb_alone`, `test_status_asleep_and_unplugged`, `test_move_turns_pitch_into_tilt_and_reads_back`, `test_move_wakes_a_sleeping_camera_first`, `test_move_keeps_the_axis_not_given_and_clamps`, `test_nudge_turns_then_stops`, `test_zoom_and_center`, `test_app_features_say_the_app_is_needed`, `test_app_features_go_to_obsbot_center_when_it_runs`, `test_ai_mode_refuses_another_model`, `test_a_running_app_with_osc_off_is_named`, `test_close_says_disconnected_only_to_an_app_it_reached`, `test_cli_json_after_the_subcommand`, `test_cli_move_nudge_and_exit_codes`, `test_cli_raw_needs_an_obsbot_address_and_the_app`, `test_setup_reads_the_camera_and_turns_osc_on_while_the_app_is_closed`, `test_setup_leaves_a_running_app_alone`, `test_setup_without_a_camera_fails` | same names in `src/tests.rs` (FakeUsb implements `UsbControl`, FakeCenter is a UDP thread answering like the real app), plus `test_osc_bytes_padding_and_errors`, `test_move_rounds_half_to_even_and_refuses_when_it_wont_wake`, `test_status_with_the_app_lists_presets_and_info`, `test_cli_device_after_the_subcommand_goes_to_the_camera`, `test_cli_raw_lists_what_came_back`, `test_setup_adds_the_section_when_it_is_missing`, `test_setup_without_the_app_is_fine_for_moving`, `test_python_helpers`, `test_raw_repr_of_values_python_cannot_put_in_json` | none |
| `bridge/tests/test_obsbot_brain.py` | `test_handle_status_turn_point_zoom_center`, `test_handle_refusals`, `test_run_is_the_code_path_line` (the Obsbot reads they rest on) | `test_brain_status_turn_point_zoom_center_reads`, `test_brain_refusals` | the tests drive `obsbot_panel.ObsbotPanel` (`handle`, `run`, `match`, `where`, `TOOLS`, `INSTRUCTIONS`): that module is the `eve-obsbot-panel` crate, whose port carries them; here only the `Obsbot` calls they make (status, position_reply, move, zoom_reply, zoom, reset, track, ensure) are tested |
| `bridge/tests/test_obsbot_panel.py` (tests_ref) | uses `obsbot.AI_MODES`, `obsbot.APP_PATH` | `test_panel_constants` | the panel's own behavior belongs to `eve-obsbot-panel` |

## Boundaries
- `pgrep -x OBSBOT_Main` is a subprocess, as in Python.
- The USB side is `eve-obsbot-usb` (its C helper subprocess and ffmpeg waker are recorded there).

## Differences
- Where Python raises out of a method, Rust returns a value: an OSC send that fails in a command (fov, preset, track, mode, framing, track speed, focus, mirror, sleep, raw) is `{"ok": false, "line": <the error>}`; a failed send in `ask`/`osc` reads as no answer; a USB read without a numeric `pan` is "Couldn't read where the camera points." (Python: KeyError); `setup` that can't read or write the ini prints the error and returns 1 (Python: traceback); `make` failing to bind its socket is the result line, exit 1.
- `raw` arguments and `--device` past int32 fail with struct's message as the result line, where Python crashes with `struct.error`.
- Usage errors (bad choice, missing subcommand, bad number) are clap's wording on stderr with exit 2; argparse's wording differs, the exit code is the same. Ints in the CLI saturate at i64 where Python's are unbounded (every such value is clamped or refused anyway).
- A decoded float that is NaN or infinite becomes JSON `null` in `raw --json` output (serde_json can't hold them; Python prints `NaN`/`Infinity`). The plain line prints them as Python does (`nan`, `inf`).
