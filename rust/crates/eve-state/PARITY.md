# eve-state
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/state.py` | `crates/eve-state/src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `NON_WAITING_NOTIFICATION_KINDS` (frozenset) | `NON_WAITING_NOTIFICATION_KINDS: [&str; 4]` | Same four kinds. |
| `notification_waits(kind)` | `notification_waits(Option<&str>) -> bool` | `None`/empty/unknown wait. |
| `Session` (dataclass) | `Session` struct, `Session::new(id, started_at)` | Same fields and defaults; `started_at` is `time.time()` epoch f64, `needs_input_at` is `time.monotonic()` f64 with 0.0 = not waiting. |
| `Entry` (dataclass) | `Entry { at, text }` | |
| `State` | `State` | `sessions` is an `IndexMap` (dict insertion order; `session_end` uses `shift_remove` so the rest keep their order). Constants `MAX_ENTRIES`, `NEEDS_INPUT_TTL_SECS`, `MAX_AGENT_ROWS`, `AGENT_NAME_CHARS` are associated consts. |
| `State.session_start` | `State::session_start(id, Option<&str>, Option<&str>)` | No-op when the id exists. |
| `State.session_end` | `State::session_end` | |
| `State.turn_begin` / `turn_end` | same | |
| `State.needs_input` | `State::needs_input` | Conjures a Session when unknown. |
| `State.note_tool` | `State::note_tool` | Never conjures; ignores empty tool name. |
| `State.input_received` | same | |
| `State._needs_input_live` | `State::needs_input_live` (pub) | |
| `State.attention_cwd` | `State::attention_cwd -> String` | Newest live flag; ties keep the first, as `max`. |
| `State.pulse_completed(duration_secs=5.0)` | `State::pulse_completed(f64)` + `State::DEFAULT_PULSE_SECS` | |
| `State.is_celebrating` (property) | `State::is_celebrating()` | |
| `State.add_entry(text, at=None)` | `State::add_entry(&str, Option<f64>)` | Strips with `str.strip()` semantics (Unicode whitespace plus U+001C..U+001F); prepends; caps at 8. |
| `State.set_tokens(cum, today, cost_cumulative=0.0, cost_today=0.0)` | `State::set_tokens(i64, i64, f64, f64)` | Callers pass 0.0 for the Python defaults. |
| `State.total` / `running_count` / `waiting_count` (properties) | methods returning `usize` | |
| `State.agent_rows(limit=None)` | `State::agent_rows(Option<usize>) -> Vec<AgentRow>` | `AgentRow` serializes to `{"n","s"}` plus `"t"` only when a tool is known; clipping counts code points like Python slicing; sorted by (rank, name). |
| `time.monotonic` / `time.time` | `monotonic()` (CLOCK_UPTIME_RAW on Apple, CLOCK_MONOTONIC elsewhere), `wall_time()` | `State::with_clock(Clock)` is the seam where Python tests patch `time.monotonic`. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_state.py` | all 17 | `src/tests.rs`: same names | None |
| `bridge/tests/test_agent_rows.py` | `test_row_names_come_from_cwd_basename`, `test_trailing_slash_does_not_blank_the_name`, `test_name_falls_back_to_session_prefix_without_cwd`, `test_long_name_is_clipped_to_the_column_width`, `test_states_and_current_tool`, `test_ordering_is_waiting_then_running_then_idle`, `test_ordering_is_stable_by_name_within_a_rank`, `test_rows_are_capped`, `test_note_tool_never_conjures_a_session` | same names | `test_heartbeat_carries_agents_and_stays_small`, `test_empty_agents_key_is_always_present` test `protocol.build_heartbeat` (the protocol crate) |
| `bridge/tests/test_protocol.py` | celebrate pulse behavior | `not_celebrating_when_idle`, `celebrating_during_pulse`, `pulse_already_expired`, `default_pulse_lasts_five_seconds` | heartbeat shape belongs to the protocol crate |
| `bridge/tests/test_claude_live.py`, `test_telegram.py`, `test_work_sessions.py` | use State as a fixture only (public `sessions`, `started_at`, `entries`) | covered by public fields | — |

Extra Rust tests: TTL expiry, conjuring, idempotent start, order after end, tie-breaking, strip semantics, char-based clipping, `?` fallback.

## Boundaries
None

## Differences
- `agent_rows` takes `Option<usize>`: a negative `limit` (Python slice from the end) is not expressible. No caller passes one.
- Token counters are `i64` rather than unbounded Python ints.
