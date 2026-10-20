# eve-motion
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/motion.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `log` | none | declared but never used in Python |
| `KEEP` | `KEEP` | 127 (`i64`) |
| `MAX_KEYS` | `MAX_KEYS` | 8 (`usize`) |
| `SPEED_MIN`, `SPEED_MAX` | `SPEED_MIN`, `SPEED_MAX` | 0.5, 2.0 |
| `OSC` | `OSC: &[(name, &[(field, i64)])]`, plus `osc_preset`, `osc_field`, `osc_names` (`sorted(OSC)`) | same seven rhythms, same values, same field order |
| `KEYS` | `KEYS: &[(name, &[[i64; 4]])]`, plus `keys_preset`, `keys_names` (`sorted(KEYS)`) | same seven gestures, same keys |
| `_clamp` | private `clamp` | NaN falls through, as Python's comparisons do |
| `osc_command` | `osc_command(Option<&str>, speed: f64, &OscParams) -> Result<Command, MotionError>` | keyword overrides are the `OscParams` struct (`None` = Python `None`); `OscParams::from_request` reads the ten fields from an IPC request the way the daemon's `_ipc_move` does (present and not null, through `int()`); period scaling uses `round_ties_even` (Python `round`); output keys in the same order |
| `keys_command` | `keys_command(Option<&str>, Option<&[Value]>) -> Result<Command, MotionError>`, plus `keys_command_rows(&[Vec<i64>])` | raw keys are the JSON list a request carries, each number through `int()`; capped at `MAX_KEYS` before validation; default speed 500; same ValueError texts |
| `stop_command` | `stop_command() -> Command` | `{"cmd":"move","kind":"stop"}` |
| `predict` | `predict(&Command) -> Result<Command, MotionError>` | same keys and order; `round(x, 1)` reproduced exactly (`{:.1}` on the exact binary value, ties to even); defaults 4 cycles, 900 ms |
| `describe` | `describe(&Command) -> Result<String, MotionError>` | same text; floats printed as Python `repr(float)` (`0.0`, `174.5`) |
| `ValueError` / `TypeError` / `IndexError` / `OverflowError` | `MotionError::{Value, Type, Index, Overflow}` | `python_type()` names the class; the daemon catches only `Value`, as it catches only ValueError |
| `int(x)` on request values | `py_int(&Value)` | ints, truncated floats, bools, base-10 strings with whitespace, sign and single underscores; Python's error texts |
| `repr(str)` | `py_repr_str` | for the `'name' is not a ...` messages |

`Command` is `serde_json::Map<String, Value>` (insertion-ordered), so a command
has the same keys, values and key order as the Python dict; the board
transport that sends it chooses the separators, as in Python.

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_motion.py` | all 13: `test_every_preset_makes_one_wire_line_of_the_right_shape`, `test_the_dance_is_two_axes_a_quarter_cycle_apart`, `test_faster_means_a_quicker_beat_not_a_refusal`, `test_speed_is_bounded_so_a_model_cannot_ask_for_a_blur`, `test_no_centre_means_centre_on_wherever_the_head_is`, `test_the_host_table_asks_for_what_the_board_actually_runs`, `test_overrides_beat_the_preset_and_a_bad_name_is_refused`, `test_a_gesture_keeps_an_axis_alone_with_the_sentinel`, `test_raw_keys_are_accepted_capped_and_defaulted`, `test_stop_is_one_short_line`, `test_predict_reports_the_speed_the_board_budgets_against`, `test_describe_says_something_a_person_can_read`, `test_every_preset_stays_inside_the_periods_and_cycles_the_board_accepts` | same names in `src/tests.rs`, plus `test_wire_lines_match_the_python_bytes`, `test_predict_and_describe_match_the_python_text`, `test_error_messages_match_the_python_text`, `test_raw_keys_after_the_cap_are_never_read`, `test_request_numbers_are_read_the_way_python_int_reads_them`, `test_python_float_text` (expected values produced by running the Python module) | none |

## Boundaries
None. The module is pure computation; sending the line to the board is the
daemon's job.

## Differences
- Integers are `i64` rather than Python's unbounded ints: a product or a
  rounded period that would exceed `i64` saturates instead of growing.
- `preset` is a string. Python would accept any JSON value a request carried
  (a number named as a preset just fails the name check); converting a
  non-string preset is the caller's job.
- A raw key that is not a JSON list (for example a string) is a `TypeError`
  here. Python would index into a string's characters; no real request sends
  one. The same holds for a non-list, truthy `keys` value handed to `predict`.
