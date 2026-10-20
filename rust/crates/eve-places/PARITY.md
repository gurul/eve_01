# eve-places
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/phone_location.py` | `src/phone_location.rs` |
| `bridge/src/eve_bridge/places.py` | `src/places.rs` |
| (Python built-ins the two lean on: `json.dumps`, `float.__repr__`, `round`, `'%g'`, `statistics.median`, `datetime.fromisoformat`, `isoformat`) | `src/py.rs` |

## API

### phone_location
| Python | Rust | Notes |
|---|---|---|
| `SWITCH`, `FILE_ENV`, `DEFAULT_FILE`, `STALE_MIN`, `FUTURE_SECS`, `MAX_ACCURACY_M`, `MAX_SPEED_MPS`, `MAX_PLACE_CHARS`, `COORD_DECIMALS`, `NO_FIX`, `OFF_REASON`, `TOOL_NAME` | same names | same values |
| `_CONTROL` | `py::one_line` | `[\x00-\x1f\x7f-\x9f  ]` to a space, then whitespace collapsed |
| `TOOL`, `TOOLS` | `TOOL`, `TOOLS` (`LazyLock<Value>`) | byte-identical to Python's `json.dumps` (checked) |
| `PLACES_TOOL_NAMES`, `TOOL_NAMES` | same names (arrays) | same order |
| `_lock` | private `LOCK` (`parking_lot::Mutex<()>`) | |
| `enabled` | `enabled(Option<&Env>)` | `None` is the process environment |
| `state_path` | `state_path` | lent env, then the process's, then `DEFAULT_FILE`, `~` expanded; empty is `.` like `Path("")` |
| `Fix` | `Fix` (+ `Fix::new`, `as_json`, `age_secs`) | `at` is `DateTime<Utc>` held to the microsecond; `as_json` keeps the key order |
| `BadFix` | `BadFix(String)` (thiserror) | message is the route's 400 reason |
| `_number` | `number` (crate-private) | JSON number not bool; `{low:g}`/`{high:g}` in the message via `py::fmt_g` |
| `_place` | private `place_of` | |
| `_when` | `when` (crate-private) | 10..40 characters, strip, trailing Z/z → +00:00, CPython 3.13's `fromisoformat` (ported in `py::fromisoformat`), must carry a zone, to UTC |
| `parse` | `parse(&Value, Option<DateTime<Utc>>)` | same checks in the same order, same reasons |
| `load` | `load(Option<&Path>)` | never fails: None for a missing/broken file; stored fixes are never "future" |
| `store` | `store(&Fix, Option<&Path>) -> io::Result<bool>` | temp file `.phone-location-*.tmp` in the folder, 0600, fsync, rename, chmod 0600; a folder it makes is 0700; the file is Python's `json.dump` byte for byte |
| `fresh` | `fresh` and `fresh_within(env, max_age_min, now)` | the keyword arguments became `fresh_within` |
| `offered` | `offered` | |
| `tools` | `tools` | + `places::tools` |
| `reading` | `reading(env, now) -> Map` | `round(x, 5)`, `round(acc)` half-even, `age // 60`; `"at"` from `Places::here` when places are on and the history exists |
| `handle` | `async fn handle(name, &Map, env) -> Map` | blocking work in `spawn_blocking` (Python `asyncio.to_thread`); places' names go to `places::call` |
| `describe` | `describe` | "age N s, accuracy N m" |

### places
| Python | Rust | Notes |
|---|---|---|
| `SWITCH`, `FILE_ENV`, `DAYS_ENV`, `DEFAULT_FILE`, `KEEP_DAYS`, `FIX_DAYS`, `STAY_RADIUS_M`, `MIN_STAY_S`, `TRAVEL_MPS`, `PLACE_RADIUS_M`, `MAX_MATCH_RADIUS_M`, `POI_MATCH_M`, `VAGUE_FIX_M`, `VISIT_SAME_S`, `LABEL_DAYS`, `LABEL_MIN_DAYS`, `LABEL_MIN_HOURS`, `WORK_MIN_HOURS`, `PATTERN_WEEKS`, `MAX_TIMELINE`, `MAX_THINGS`, `MAX_NAME_CHARS`, `MAX_NOTE_CHARS`, `LABEL_TITLES`, `WEEKDAYS`, `OFF_REASON`, `NO_HERE` | same names | same values (`LABEL_TITLES` as pairs) |
| `_CONTROL`, `_DAY` | `py::one_line`, private `DAY` | |
| `_lock` | private `LOCK` | one connection at a time in the process |
| `enabled` | `enabled` | and `phone_location::enabled` |
| `state_path` | `state_path` | as phone_location's |
| `keep_days` | `keep_days` | Python `int()` syntax (sign, `_` between digits), clamped to 30..3650, bad text → 365 |
| `zone` | `zone(env) -> Zone` | `Zone::Named(chrono_tz::Tz)` for EVE_TIMEZONE, else `Zone::Fixed` (this Mac's offset now, as Python's `datetime.now().astimezone().tzinfo`); `Zone::timestamp` follows Python's fold=0 for ambiguous and missing wall times |
| `distance_m` | `distance_m` | haversine, r = 6 371 008.8 m |
| `clean` | `clean(Option<&str>, limit) -> Result<Option<String>, String>` | Err carries the ValueError message; a private `clean_value` refuses non-strings from a JSON body |
| `duration` | `duration` | round half-even like Python |
| `clock` | `clock` | |
| `distance_words` | `distance_words` | |
| `days_words` | `days_words` | |
| `category_words` | `category_words` | lower→Upper boundary gets a space, then lowercase |
| `Visit` | `Visit` | |
| `BadVisit` | `BadVisit(String)` | |
| `_when` | private `when` | |
| `parse_visit` | `parse_visit(&Value, Option<DateTime<Utc>>)` | same order and reasons |
| `SCHEMA` | `SCHEMA` | verbatim |
| `Places` | `Places` | `Places::new(path, environ, now)`; methods below; errors are `PlacesError` (Value / Db / Io / Other) |
| `Places._db` | private `Places::db` | module lock, 0700 folder and 0600 file when new, busy timeout 5 s, schema, one transaction, rollback on error |
| `Places._ts`, `_local`, `_day_bounds`, `_meta`, `_set_meta` | private `ts`, `local`, `day_bounds`, `meta`, `set_meta` | `meta` values are Python's `json.dumps` text |
| `Places._match`, `_place_for` | private `match_place`, `place_for` | |
| `Places.title` | `Places::title(&PlaceRow)` | |
| `Places._describe` | private `describe` | |
| `Places.record_fix` | `record_fix(&Fix) -> Res<Option<Map>>` | the stay in `meta` keeps Python's keys and order |
| `Places._new_stay`, `_insert_visit`, `_drop_fix_stays` | private `new_stay`, `insert_visit`, `drop_fix_stays` | |
| `Places.record_visit` | `record_visit(&Visit) -> Res<&'static str>` | "arrival", "visit", "departure", "repeat", "ignored" |
| `Places._visits`, `_overlap_hours` | private `visits_between`, `overlap_hours` | insertion order kept (IndexMap) so ties break as Python's `max` |
| `Places.infer_labels` | `infer_labels() -> Res<Labels>` | `Labels { home, work }` in place of the dict |
| `Places.here` | `here() -> Res<Option<Map>>` | |
| `Places.timeline` | `timeline(first, last)` | |
| `Places._pattern_rows`, `_habit`, `_routine`, `_maybe_infer` | private `pattern_rows`, `habit`, `routine`, `maybe_infer` | medians as `statistics.median` |
| `Places.patterns` | `patterns()` | |
| `Places.digest`, `_days_of`, `_digest_text` | `digest(day)`, private `days_of`, `digest_text` | |
| `Places.week` | `week(day)` | |
| `Places._find_place`, `name_place` | private `find_place`, `name_place(name, which)` | |
| `Places._key`, `remember_thing`, `_thing_out`, `find_thing`, `list_things`, `forget_thing` | private `key`, `thing_out`; `remember_thing(thing, note, which)`, `find_thing`, `list_things`, `forget_thing` | |
| `Places.prune`, `summary`, `forget` | `prune() -> IndexMap`, `summary() -> Map`, `forget() -> IndexMap` | counts keep Python's key order; `forget` removes the phone's latest fix and logs counts only |
| `RECALL`, `NAME`, `THING`, `TOOL_NAMES`, `TOOLS` | same names | `TOOLS` byte-identical to Python's (checked) |
| `offered` | `offered` | |
| `tools` | `tools` | |
| `_day` | private `day_arg` | "dates are YYYY-MM-DD", then `date.fromisoformat`'s own messages ("month must be in 1..12", "day is out of range for month", "year 0 is out of range") |
| `call` | `call(name, &Map, Option<&Places>, env) -> Map` | never fails; ValueError → its message, store errors → "the places history could not be read" with a warning naming the error class |
| `here_line` | `here_line` | |
| `loop` | `async fn run_loop(watch::Receiver<bool>, Duration)` | `loop` is a Rust keyword; the asyncio.Event is a `watch` channel that turns true on shutdown |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_phone_location.py` | `test_a_good_body_is_a_fix`, `test_garbage_is_refused_with_a_reason`, `test_a_little_clock_skew_is_allowed`, `test_only_the_newest_fix_is_kept_atomically_and_owner_only`, `test_a_failed_write_leaves_the_old_fix_and_no_temp_file`, `test_a_broken_file_is_no_fix_and_is_replaced`, `test_the_file_is_where_the_environment_says`, `test_the_tool_is_strict_with_no_arguments_and_says_when_to_use_it`, `test_the_answer_fresh_stale_and_missing`, `test_the_switch_turns_off_the_tool_and_its_answer`, `test_the_tool_is_offered_only_once_the_phone_shared_a_fix`, `test_the_log_words_carry_age_and_accuracy_only` | same names in `tests/phone_location.rs` (the failed write in `src/phone_location.rs`, with the rename lent through `store_with`), plus `test_the_file_is_what_python_json_dump_writes`, `test_another_tool_name_is_refused` | NaN, infinity and `10**400` bodies cannot exist in a `serde_json::Value` (the JSON reader refuses them before `parse`); `1e300` stands in for out of range. The surface tests (`test_the_chat_brain_*`, `test_its_result_is_not_written_to_the_transcript`, `test_out_loud_*`, `test_the_taint_gate_*`, `test_the_intent_eval_*`, the quick-answers "here" tests, the coordinate log filter tests, `test_the_check_*`) exercise telegram, voice_agent, tool_gate, quick_answers and tools/ scripts, ported with those crates |
| `bridge/tests/test_places.py` | `test_an_arrival_and_its_departure_are_one_visit_at_a_named_place`, `test_nearby_visits_share_a_place_and_a_far_one_starts_another`, `test_a_stay_is_read_from_fixes_and_an_ios_visit_replaces_it`, `test_a_short_stop_and_a_vague_fix_are_not_visits`, `test_a_body_that_is_not_a_visit_is_refused`, `test_a_visit_body_parses_and_drops_an_odd_category`, `test_home_and_work_are_inferred_and_named`, `test_habits_are_the_usual_days_times_and_stays`, `test_a_day_digest_says_the_day_in_order_and_what_was_missed`, `test_today_s_digest_ends_now_and_a_week_digest_ranks_places`, `test_new_places_show_in_their_day`, `test_the_owner_names_where_they_are_and_renames_it`, `test_things_are_kept_where_the_owner_is_and_found_with_how_far`, `test_a_thing_at_a_known_place_needs_no_fix`, `test_where_the_owner_is_now_is_said_by_name_in_the_system_context`, `test_the_tools_ride_with_phone_location_once_the_phone_has_shared`, `test_the_recall_tool_answers_each_kind_and_refuses_bad_dates`, `test_the_file_is_owner_only_and_old_evidence_is_pruned`, `test_forget_wipes_everything_with_the_latest_fix` | same names in `tests/places.rs`, plus `test_the_stay_is_kept_as_python_json_in_meta`, `test_a_day_in_a_dst_change_is_bounded_like_python` | the `system_context.context()` assertions in `test_where_the_owner_is_now_*` belong to system_context's crate (`here_line` and `reading()["at"]` are asserted here) |
| `bridge/tests/test_events.py`, `test_link_chat.py`, `test_updates.py` (tests_ref) | their uses of `parse`, `store`, `load`, `state_path`, `Places().summary()` | covered by the tests above | the routes and digests themselves are those crates' |
| (Python built-ins) | | `src/py.rs`: `test_float_repr_matches_python`, `test_dumps_matches_python`, `test_round_and_g`, `test_fromisoformat_matches_cpython` (29 strings run through CPython 3.13), `test_one_line_and_isoformat` | |

Cross-checks run during the port (not committed, they need the Python daemon): a four-week history built by
`places.py` was read by this crate and 42 answers (31 day digests, 5 weeks, habits, places, a 22-day timeline,
labels, summary, prune) matched Python's JSON exactly, key order included; the same history built by this crate
dumped (`sqlite3 .dump`) identically to Python's; 49 fix and visit bodies parsed to the same results and reasons;
the four tool schemas serialize byte for byte as Python's.

## Boundaries
None

## Differences
- A time whose UTC conversion leaves datetime's range (e.g. `0001-01-01T00:00:00+01:00`) makes Python raise
  OverflowError out of `parse`; here it is a BadFix with the usual "at must be an ISO 8601 time with a zone".
- `call` answers "the places history could not be read" for a malformed `stay` in `meta` (missing keys); Python
  would raise KeyError out of the tool call.
- A corrupt JSON value in `meta` gives serde_json's message as the tool's reason, where Python gives
  `json.JSONDecodeError`'s.
- The thing key's `[^\w\s]` uses Rust regex's Unicode `\w`, which leaves out a few numeric symbols (e.g. `½`)
  that Python's `\w` keeps.
- `keep_days` does not read non-ASCII digits as Python's `int()` does.
