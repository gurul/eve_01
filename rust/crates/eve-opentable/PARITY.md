# eve-opentable
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/opentable.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `OPENTABLE_URL`, `OFF_REASON`, `HELP_LINE`, `MENU`, `MAX_REQUEST_CHARS`, `WIDE_DESCRIPTION` | same-named `pub const` | verbatim |
| `TOOL_NAMES` | `TOOL_NAMES: &[&str]` | frozenset of one name |
| `_ON` | `ON` (private) | |
| `TOOL` | `TOOL: LazyLock<Value>` | same keys in Python's dict order; golden fixture compares serialized bytes |
| `_INSTRUCTIONS`, `_WIDE_FIRST_LINE`, `_NARROW_FIRST_LINE` | `INSTRUCTIONS`, `WIDE_FIRST_LINE`, `NARROW_FIRST_LINE` (private) | verbatim |
| `COMMAND` | `COMMAND: LazyLock<Regex>`, `command_request(&str) -> Option<Option<&str>>` | `command_request` is `COMMAND.fullmatch(text)` + `group(1)`; `\s`/`\w` spelled as Python's `str` classes (`isspace` incl. U+001C..U+001F; letters, numbers, `_`) |
| `configured` | `configured(Option<&Environ>) -> bool` | `None` reads the process environment; Python `str.strip()` whitespace |
| `tools` | `tools(wide: bool) -> Vec<Value>` | fresh deep copy each call |
| `instructions` | `instructions(wide: bool) -> String` | first narrow line replaced once |
| `build_goal` | `build_goal(&Value) -> Result<String, InvalidRequest>`, `build_goal_str(&str)`, `build_goal_with(&Value, clock)` | `InvalidRequest` is the `ValueError` (same messages); `build_goal_with` is the clock seam the Python tests patched (`datetime`); the clock is read only after validation |
| (`datetime.now().astimezone()`) | `local_clock() -> ClockSnapshot` | unix: `tzset` + `localtime_r`, `tm_gmtoff`/`tm_zone`, as CPython does; `isoformat(timespec='seconds')` offset incl. seconds when present |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_opentable.py` | all 14 | `tests/opentable.rs`: same names; the contract's receipt/request validation uses a small in-test JSON Schema subset validator (no `jsonschema` crate in the workspace) | `("Seattle\ud800", "valid Unicode text")` case: a Rust string cannot hold a lone surrogate; `lone_surrogate_cannot_reach_build_goal` shows the JSON parser refuses it. `monkeypatch.setenv` of the process env replaced by explicit mappings (parallel tests) |
| `bridge/tests/test_opentable_integration.py` | 10 | none | every test drives the Telegram inlet (`_dispatch`, `_tool`, `_turn_parts`, the agent factory) or `Daemon._make_telegram`; they belong to the telegram/daemon crates. The receipt shapes they validate are covered by the contract test above |
| `bridge/tests/test_menu_off_bytes.py`, `bridge/tests/test_menu_tools.py` (ref) | OpenTable wording parts | `switch_off_keeps_narrow_wording_and_on_lends_the_wide_one`, `tools_and_instructions_are_python_s_bytes` | the origin/main comparison and the request-body parts are telegram's |
| (golden) | — | `goals_are_python_s_bytes_for_a_fixed_clock`, `configured_matches_python_on_whitespace_edges`, `command_matches_python_fullmatch_on_unicode_and_separator_cases` | fixtures in `tests/fixtures/` were generated from the Python module |

## Boundaries
None

## Differences
- `build_goal` takes a `serde_json::Value` (what a tool call carries) instead of Python's `Any`; any non-string is "OpenTable request must be text.". The "must be valid Unicode text." error cannot occur in Rust.
- Off unix, the clock zone name is `UTC±HH:MM` (Python's name for a nameless fixed offset) rather than the OS name.
