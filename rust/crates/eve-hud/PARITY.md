# eve-hud
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/hud.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `BAR_WIDTH`, `BAR_FULL`, `BAR_EMPTY` | same names | 8, `█`, `░` |
| `_ANSI_RESET`, `_ANSI_RED`, `_ANSI_YELLOW`, `_ANSI_GREEN` | `ANSI_RESET`, `ANSI_RED`, `ANSI_YELLOW`, `ANSI_GREEN` | same escapes |
| `_bar` | `bar(pct: i128, width)` | clamp 0..=100, `(pct*width+50)//100` cells |
| `_battery_color` | `battery_color` | red ≤15, yellow ≤40, else green |
| `_format_tokens` | `format_tokens(n: i128)` | `12.3K` below 100K, `850K` from 100K, `1.2M` from 1M; `{:.1}` rounds the binary value like Python's `.1f` |
| `_battery_segment` | `battery_segment(Option<&Value>, ascii_only)` | `None` unless the value is a Python int (an integer or a bool, which renders as `True%`) |
| `_query_state` | `query_state(socket_path, timeout)`; `QUERY_TIMEOUT` = 0.5 s | `eve_ipc::make_transport(...).sync_connect`, sends `{"evt":"get_state"}\n`, reads 4096-byte chunks to the first newline or EOF; any connect/read/spec/UTF-8/JSON failure, a falsy `ok`, or a missing/null `state` → `None` |
| `format_line` | `format_line(Option<&Map<String, Value>>, ascii_only)` | same segments in the same order with Python's type rules (`isinstance(int)` includes bool, `sec is True/False`, truthy `running` printed with `str()`); plus `format_state(Option<&Value>, ascii_only)` for whatever `query_state` returned |
| `run` | `run(ascii_only, socket_path) -> i32` | drains stdin when it is not a TTY and is readable within 100 ms (`poll`, as Python's `select`), queries, prints the line + `\n`, returns 0. Called by the CLI's `hud` subcommand (`--ascii`, `--socket`) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_hud.py` | `test_format_none_state`, `test_format_disconnected`, `test_format_full_state`, `test_format_low_battery_icon`, `test_bar_rendering_endpoints`, `test_bar_rendering_middle`, `test_bar_clamps_out_of_range`, `test_ascii_bar_uses_ascii_chars_only`, `test_format_unencrypted_warns`, `test_format_no_running_omits_count`, `test_format_cost_shows_when_above_one_cent`, `test_format_cost_hidden_below_one_cent`, `test_format_cost_two_decimals_for_double_digits`, `test_token_formatter_thresholds`, `test_format_tokens_shown_when_above_1k`, `test_format_tokens_hidden_below_1k`, `test_format_update_segment_appears_at_end`, `test_format_update_segment_ascii`, `test_format_no_update_omits_segment`, `test_format_tokens_and_cost_appear_together` | same names in `src/tests.rs` | none |
| (new) | | `test_exact_lines_match_python`, `test_python_type_edges`, `test_format_tokens_rounds_like_python` (expected strings taken from running the Python `format_line`), `test_format_state_only_formats_a_dict`, `ipc::test_query_state_returns_the_state`, `ipc::test_query_state_refusal_is_none`, `ipc::test_query_state_unreachable_is_none`, `ipc::test_query_state_bad_reply_is_none`, `ipc::test_query_state_reads_only_the_first_line`, `ipc::test_query_state_silent_daemon_times_out` | |

## Boundaries
None

## Differences
- A reply that is valid JSON but not an object, or a `state` that is not an object (`[1]`, `"x"`, `false`), makes the Python raise `AttributeError` out of `run` (a traceback, exit 1, empty statusLine). The Rust renders such a state as off (`🐾 off` / `eve: off`) and returns 0. The daemon always sends an object, so this only differs for a foreign server.
- `json.loads` accepts `NaN`/`Infinity` and integers past 64 bits; serde_json refuses the first two (the line reads as off) and reads big integers as floats, so a past-u64 `battery_pct`/`tokens_today` is not treated as an int.
- `format_tokens` divides `n as f64`; Python's int true division is correctly rounded for counts above 2^53. Token counts never get near that.
- A failed write to stdout is ignored; the Python would raise.
