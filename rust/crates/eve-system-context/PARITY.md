# eve-system-context
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/system_context.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `context` | `context(environ: Option<&Env>, now: Option<DateTime<Utc>>) -> String` | `None` environ is the process environment (Python's `os.environ`); `now` is the instant, which is all `astimezone` uses. Text verbatim, lines joined by `\n`, leading `\n\nSystem context:` |
| (`ZoneInfo(name)` with its ZoneInfoNotFoundError/ValueError catch) | private `zone_info` | chrono-tz, then case-insensitive (zoneinfo opens a file on the Mac's case-insensitive disk), plus `posixrules` (New York's rules); the key keeps the owner's spelling, as `ZoneInfo.key` does |
| (`instant.astimezone(None)`, `tzname()`) | private `local_offset_and_name` | `tzset` + `localtime_r`: the offset (`tm_gmtoff`) and abbreviation (`tm_zone`) at that instant, as CPython's `_local_timezone` |
| (`isoformat(timespec="seconds")`) | private `iso_offset` + `%Y-%m-%dT%H:%M:%S` | sub-second dropped; an offset with seconds (LMT) prints `+05:53:28` |
| (`" ".join(s.split())[:300]`) | private `one_line` | Python's whitespace: Unicode White_Space plus `\x1c`..`\x1f`; 300 characters, not bytes |
| (`places.here_line(environ)`) | `eve_places::places::here_line` | upstream, not re-implemented |

`Env` is re-exported from `eve-places` (`HashMap<String, String>`).

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_system_context.py` | `test_clock_uses_configured_timezone_and_daylight_saving` (both parameter rows), `test_missing_location_and_invalid_timezone_use_local_clock` | same names in `tests/system_context.rs`, plus `test_the_whole_block_is_python_s_text`, `test_without_a_zone_the_clock_is_this_mac_s`, `test_a_zone_in_the_wrong_case_loads_and_keeps_its_spelling`, `test_an_old_offset_keeps_its_seconds_and_the_location_is_capped`; unit tests in `src/lib.rs`: `test_one_line_splits_like_python`, `test_iso_offset_matches_isoformat`, `test_zone_info_keeps_the_key_and_refuses_what_zoneinfo_refuses` | `test_context_reaches_each_brain_and_refreshes_for_new_requests` asserts on voice_agent, telegram and think prompts; it is ported with those crates. The Rust tests lend `EVE_PLACES_FILE`/`EVE_PHONE_LOCATION_FILE` in a temp folder so they never read this Mac's real places history (the Python tests would) |
| `bridge/tests/test_places.py` (tests_ref) | `test_where_the_owner_is_now_is_said_by_name_in_the_system_context` (its `system_context.context()` asserts) | same name in `tests/system_context.rs`; also asserts the line sits just before the snapshot warning | the `here_line` / `reading()` asserts are eve-places' |
| `test_link_chat.py`, `test_memory_eval.py`, `test_menu_off_bytes.py`, `test_telegram.py`, `test_telegram_chief.py`, `test_think.py`, `test_voice_agent.py` (tests_ref) | they patch `system_context.context` with a fixed string | none here | they test the callers' prompt assembly; ported with those crates (the seam is theirs) |

Cross-check run during the port (not committed, it needs the Python daemon): 18 cases (six EVE_TIMEZONE values
including empty, an unknown zone, Lord Howe's half-hour DST and UTC, times three instants across DST changes, with a
location holding `\x1f` and runs of spaces) were rendered by `system_context.context()` and by this crate; all 18
strings were identical, including the local-clock fallback's abbreviation (`PST`/`PDT`).

## Boundaries
None

## Differences
- Off unix (no `localtime_r`), the local fallback names the zone `UTC±hh:mm` from chrono's offset; Python on
  Windows would give the C library's long zone name. The daemon runs on macOS, where the two match.
- A zone name chrono-tz does not carry but the Mac's zone folder does (other than `posixrules`) falls back to the
  local clock, where Python would load it; and the reverse for a name the Mac's tzdata lacks. Both read IANA data, so
  this only bites across tzdata versions.
