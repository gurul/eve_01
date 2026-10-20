# eve-lights
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/lights.py` | `src/lights.rs` (colour, change, state, the lights file, the drivers' trait, targets, tools, the code path, `Lights`), `src/govee.rs`, `src/wiz.rs`, `src/triones.rs`, `src/tuya.rs` (one file per brand), `src/cli.rs` (`scan`, `merge`, `cli`), `src/py.rs` (Python-compatible `json.dumps`, `repr`, `int()`, `bytes.fromhex`, `round`, and Python's `str.strip`, `\s`, `\w`, `\b` and `\d` digit values) |

## API
| Python | Rust | Notes |
|---|---|---|
| `CONFIG_PATH`, `OFF_REASON`, `KINDS`, `LIGHT_TIMEOUT_SECS` | same names in `lights` | `config_path()` expands `~` |
| `COLORS`, `WHITES`, `KELVIN_MIN`, `KELVIN_MAX` | same | ordered slices, same order as the dicts |
| `Color` (`as_rgb`) | `Color`, `Color::as_rgb` | `rgb: Option<[i64; 3]>` |
| `parse_color` | `parse_color` | |
| `kelvin_to_rgb` | `kelvin_to_rgb` | Python's round-half-to-even |
| `scale_rgb` | `scale_rgb` | |
| `rgb_percent` | `rgb_percent` | |
| `hex_of` | `hex_of` | |
| `color_word` | `color_word` | `colorsys.rgb_to_hsv` ported exactly |
| `Change` (`empty`, `normalised`) | `Change`, `Change::is_empty`, `Change::normalised` | |
| `State` (`public`) | `State`, `State::public` | key order kept |
| `change_of` | `change_of` | |
| `Light` (`from_dict`, `to_dict`) | `Light`, `Light::from_dict`, `Light::to_dict`, `Light::new` | `from_dict` returns `Err(reason)` where Python raised |
| `load` | `load` | |
| `save` | `save` | same bytes as `json.dumps(indent=2)` + `\n`, `ensure_ascii`, mode 600, via `lights.tmp` |
| `GOVEE_GROUP`, `GOVEE_SCAN_PORT`, `GOVEE_LISTEN_PORT`, `GOVEE_CMD_PORT`, `GOVEE_KELVIN`, `_GOVEE_LISTEN_LOCK` | same constants in `govee`; `GOVEE_LISTEN_LOCK` static | |
| `_govee_listening` | `govee::govee_listening` | holds the process-wide listen lock |
| `_govee_listener` | `govee::govee_listener` | socket2: `SO_REUSEADDR` + `SO_REUSEPORT` |
| `govee_scan` | `govee_scan` → `Vec<GoveeFound>` | |
| `govee_commands` | `govee_commands` → `Vec<Value>` | sent as `json.dumps` bytes |
| `GoveeDriver` (`_status_blocking`, `_rediscover`, `state`, `apply`, `close`) | `GoveeDriver` (`status_blocking`, `rediscover`, `Driver::state/apply/close`), `GoveePorts` | ports are a field so a test can move them |
| `WIZ_PORT`, `WIZ_KELVIN`, `WIZ_MIN_DIMMING` | same in `wiz` | |
| `wiz_params` | `wiz_params` | |
| `wiz_parse_pilot` | `wiz_parse_pilot` | |
| `wiz_sign` | `wiz_sign` | HMAC-SHA256, base64 |
| `wiz_message` | `wiz_message` | compact JSON bytes |
| `WizRefused` | `LightError::Refused` | |
| `_wiz_call` | `wiz_call`, `wiz_call_at` (port) | |
| `wiz_call_signed` | `wiz_call_signed`, `wiz_call_signed_at` | |
| `wiz_import` | `wiz_import` | error is the Python exception's name, for the CLI line |
| `_broadcast_addresses` | `wiz::broadcast_addresses` | |
| `wiz_scan` | `wiz_scan` → `Vec<WizFound>` | |
| `WizDriver` (`_rediscover`, `_call`, `state`, `apply`, `close`) | `WizDriver` (`rediscover`, `call`, `Driver` impl), `WizDriver::with_port` | |
| `TRIONES_WRITE`, `TRIONES_NOTIFY`, `TRIONES_ON`, `TRIONES_OFF`, `TRIONES_STATUS`, `TRIONES_NAMES`, `TRIONES_IDLE_SECS`, `TRIONES_CONNECT_SECS` | same in `triones` | |
| `triones_color` | `triones_color` | |
| `triones_parse_status` | `triones_parse_status` | |
| `triones_frames` | `triones_frames` | |
| `TrionesDriver` (`_connect`, `_on_notify`, `_touch`, `_read`, `state`, `apply`, `close`) | `TrionesDriver` (`Inner::connect`, the notification task, `touch`, `Inner::read`, `Driver` impl) | btleplug in place of bleak |
| `triones_scan` | `triones_scan` → `Vec<TrionesFound>` | |
| `TUYA_KELVIN` | `TUYA_KELVIN` | |
| `tuya_temp_percent` | `tuya_temp_percent` | |
| `TuyaDriver` (`_dev`, `_state_blocking`, `_apply_blocking`, `state`, `apply`, `close`) | `TuyaDriver` (`device`, `Driver` impl), `tuya_calls` | see Boundaries |
| `_tuya_ok` | the helper's `_tuya_ok` | verbatim in the helper |
| `tuya_scan` | `tuya_scan` → `Vec<TuyaFound>` | see Boundaries |
| `DRIVERS` | `drivers()` → `HashMap<String, DriverFactory>` | |
| `ALL_WORDS` | `ALL_WORDS` | |
| `_norm` | `lights::norm` | Python's `\w` (letters and numbers, no combining marks) and whitespace (U+001C..U+001F too) |
| `TOOLS`, `TOOL_NAMES`, `INSTRUCTIONS_HEAD` | `TOOLS` (`LazyLock<Value>`), `TOOL_NAMES`, `INSTRUCTIONS_HEAD` | verbatim; `json.dumps` of each tool is byte-identical (tested) |
| `Command` | `Command` | `names: Vec<String>` |
| `_FILLER`, `_VERB_DIM`, `_VERB_BRIGHT`, `_POWER`, `DIM_PERCENT`, `_NAME_STOPWORDS`, `_PCT`, `_COPY_ALL`, `_COPY_AS`, `_COPY_TO`, `_COPY_LEAD`, `_COPY_TAIL`, `_OTHERS`, `_SOURCE_ORDER` | `FILLER`, `VERB_DIM`, `VERB_BRIGHT`, `power_word`, `DIM_PERCENT`, `NAME_STOPWORDS`, `PCT`, `COPY_ALL`, `COPY_AS`, `COPY_TO`, `COPY_LEAD`, `COPY_TAIL`, `OTHERS`, `SOURCE_ORDER` | same patterns |
| `Lights` (`__init__`, `driver`, `_words_of`, `resolve`, `names_line`, `_one`, `set`, `status`, `copy`, `copy_line`, `close`, `tools`, `instructions`, `handle`, `match`, `_match_copy`, `run`, `line`, `panel`, `listing`) | `Lights` (`new`/`with`, `driver`, `words_of`, `resolve`, `names_line`, `one`, `set`, `status`, `copy`, `copy_line`, `close`, `tools`, `instructions`, `handle`, `match_command`, `match_copy`, `run`, `line`, `panel`, `listing`) | `match` is a Rust keyword: `match_command`. `one`: a driver's own `TimeoutError` reads "it did not answer in time", as `except asyncio.TimeoutError` catches it on Python 3.11+. Results are `serde_json::Value` with Python's key order |
| `describe` | `describe` | |
| `_join` | `join_names` | |
| `enabled` | `enabled(Option<&HashMap>)` | None reads the process environment |
| `make_lights` | `make_lights(path, environ)` | |
| `scan` | `cli::scan` | |
| `merge` | `cli::merge` | returns (all, new) as values; the new ones are the tail of all |
| `cli` | `cli(argv, path, out) -> i32` | clap in place of argparse; same subcommands, options and lines |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `tests/test_lights.py` | all 47: `test_parse_color_names_whites_hex_kelvin`, `test_kelvin_to_rgb_is_warm_to_cool`, `test_scale_rgb_keeps_hue_and_sets_level`, `test_describe_names_a_picked_colour`, `test_color_word`, `test_govee_off_is_one_turn`, `test_govee_colour_and_level_turn_on_first`, `test_govee_white_uses_kelvin_clamped`, `test_govee_brightness_zero_means_off`, `test_wiz_off_is_setstate_false`, `test_wiz_colour_and_level_in_one_pilot`, `test_wiz_white_is_temp_clamped_and_dimming_floored`, `test_wiz_parse_pilot`, `test_wiz_driver_sends_setpilot_and_reads_reply`, `test_wiz_error_reply_raises`, `test_wiz_sign_is_hmac_sha256_of_compact_params`, `test_a_signed_change_is_accepted_and_an_unsigned_one_names_the_fix`, `test_a_clock_the_bulb_disagrees_with_is_retried_on_the_bulbs_clock`, `test_wiz_import_takes_the_key_by_mac`, `test_wiz_light_roundtrips_and_merges`, `test_triones_frames_bytes`, `test_triones_status_parse`, `test_triones_colour_keeps_current_level`, `test_triones_level_keeps_current_colour`, `test_triones_colour_on_an_off_light_is_full`, `test_tuya_temp_percent`, `test_tuya_without_key_says_so`, `test_load_save_roundtrip_is_private`, `test_load_skips_bad_entries`, `test_merge_keeps_names_and_takes_new_address`, `test_make_lights_off_switch_and_empty`, `test_resolve`, `test_match` (15 cases), `test_match_leaves_the_rest_to_the_model` (12 cases), `test_match_without_lights_is_none`, `test_run_by_code_reports_each`, `test_tools_are_strict_and_complete`, `test_handle_set_and_status`, `test_change_of_state`, `test_match_copy` (7 cases), `test_match_copy_needs_real_lights`, `test_copy_reads_a_wifi_light_first_and_sets_the_rest`, `test_copy_skips_a_source_that_does_not_answer`, `test_copy_by_code_and_by_tool`, `test_a_slow_light_times_out_alone`, `test_cli_name_sets_room_and_key`, `test_parallel_govee_reads_each_get_their_own_reply` | same names in `src/tests.rs`, plus `kelvin_to_rgb_matches_python_values`, `save_writes_what_python_writes`, `from_dict_reads_like_python`, `tools_and_instructions_are_byte_for_byte_python`, `tuya_calls_are_the_bulbdevice_calls`, `copy_with_no_answer_says_who_was_tried`, `cli_without_a_file_says_scan`, `cli_set_reports_an_unknown_colour`, `line_names_everyone_only_when_all_answered`, `tuya_helper_without_tinytuya_says_how_to_install_it`, `tuya_key_never_on_the_command_line`, `a_drivers_own_timeout_is_did_not_answer_in_time_for_a_change`, `norm_and_colours_use_pythons_whitespace_words_and_digits`, `a_partial_name_needs_pythons_word_boundary`, `cli_brightness_is_pythons_int_and_options_take_a_prefix`, and `py::tests` (3) | none. `test_parallel_govee_reads_each_get_their_own_reply` uses real sockets on localhost instead of a fake kernel; it fails with the listen lock removed (checked) |
| `tests/test_miniapp.py` (ref) | `test_the_lights_card_is_the_owners_only_and_drives_the_lights`: the hub's part | `panel_drives_the_lights_and_says_what_happened` | the HTTP routes and owner check belong to the miniapp crate |
| `tests/test_intent_eval.py` (ref) | "lights off" matched by code | `intent_eval_lights_off_is_a_command` | the eval rig belongs to its crate |
| `tests/test_telegram.py`, `tests/test_telegram_chief.py`, `tests/test_voice_agent.py` (ref) | use `Lights`, `TOOLS`, `TOOL_NAMES`, `save` | covered by `test_run_by_code_reports_each`, `test_handle_set_and_status`, `test_load_save_roundtrip_is_private` | the Telegram, self-context and voice parts belong to those crates |
| live (`#[ignore]`) | none in Python | `tuya_helper_reaches_tinytuya`, `lan_scans_run`, `live_lan_lights_read`, `bluetooth_scan_runs` | run on 2026-10-19 except the Bluetooth one: the LAN scan found the owner's Govee and WiZ light and read both; the Tuya helper reached tinytuya |

## Boundaries
- **Tuya (Sylvania Smart+ Wi-Fi) runs through tinytuya, in a Python subprocess.** Tuya's local protocol has
  five wire versions (3.1 to 3.5, each with its own framing, session-key negotiation and AES mode) and
  tinytuya's `BulbDevice` holds the per-bulb-type data points and colour encodings. The Python daemon uses
  exactly that library, so the port keeps it: `TuyaDriver` decides the `BulbDevice` calls (`tuya_calls`) and a
  short helper (embedded in `src/tuya.rs`) runs them with the same `set_socketTimeout(3)`,
  `set_socketRetryLimit(1)`, the same state reading and the same `_tuya_ok` check, printing one JSON line.
  The request (with the light's local key) goes to the helper on stdin, never on its command line, where `ps`
  would show the key to any user of the Mac.
  The interpreter is `EVE_LIGHTS_PYTHON` (new), else the checkout's `bridge/.venv/bin/python3` when it exists (where `install.sh` installs the extra, as the other Python helpers default), else `python3`; it needs the `lights` extra. A missing
  interpreter or tinytuya gives the Python daemon's message, `tinytuya is not installed: pip install -e ".[lights]"`,
  and `tuya_scan` returns no devices, as before.
- **HappyLighting runs on btleplug** (CoreBluetooth on macOS) in place of bleak. The address kept in
  `lights.json` is the same: on macOS the CoreBluetooth identifier in upper case, elsewhere the MAC.

## Differences
- Tuya: each read or change opens its own connection in a fresh helper process; the Python driver kept one
  `BulbDevice` per light. With no `ip` in the lights file ("Auto"), tinytuya looks for the device on every call.
- Error texts that come from the platform are Rust's: a socket error reads `[Errno 65] No route to host` as in
  Python, but a host-lookup failure or a malformed JSON reply is worded by Rust, and a Bluetooth failure by
  btleplug (named `BleakError`).
- A reply that is valid JSON but not an object (a list, a number) is skipped by the WiZ reader, where Python
  raised `AttributeError`; a `lights` value in the file that is a number reads as no lights, where Python
  raised `TypeError`.
- Ties between phrases of equal length in `match_command` are taken in the order the lights are listed;
  Python's came from set iteration, which varies by hash seed.
- The CLI's usage and parse-error text is clap's (exit code 2 as with argparse); options still take a unique
  prefix and `--brightness` is read as Python's `int()` reads it. A failed write of the lights
  file is a line and exit 1 rather than a traceback, and so is a WiZ export whose `devices` is a number
  (`Could not read a WiZ export from FILE: TypeError`), where Python's `TypeError` escaped as a traceback.
- `cli` builds its own tokio runtime, so it is called from outside one (Python's `asyncio.run` had the same
  rule).

## Added for eve-bridge
- `cli::clap_command()` returns the private clap parser, so eve-bridge can print `eve-bridge lights --help` in argparse's layout. Small and additive; nothing else changed, and this crate's tests were re-run after it (all pass).
