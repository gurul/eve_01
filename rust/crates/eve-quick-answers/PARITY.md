# eve-quick-answers
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/quick_answers.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `FORECAST_URL`, `AIR_URL`, `GEOCODE_URL`, `TIMEOUT_SECS`, `PHONE_FRESH_MIN`, `PHONE_DECIMALS`, `PHONE_PLACE`, `HIDDEN`, `MAX_EXPONENT` | same names | same values |
| `_COORD` | private `COORD` | same pattern |
| `CoordinateFilter` | `CoordinateFilter::filter(&str) -> String` | rewrites only a line that names `open-meteo.com` and carries `latitude=`/`longitude=`; the values read `<hidden>` |
| `hide_coordinates` | `hide_coordinates()` (+ `remove_coordinate_filter()`, Python's `logger.removeFilter`) | the "httpx" request lines go through the filter while it is installed; installed from the start, as Python's import does, and again by `QuickAnswers::new` and `get_json_with` |
| `Ask` | `Ask { kind: Kind, place, day, args: Args }` (+ `Ask::new`, `with_place`, `with_day`, `with_args`) | `kind` is an enum whose `as_str()` is Python's string; the free-form `args` dict is the typed `Args` (`Sun{which}`, `Countdown{what}`, `Convert(Conversion)`, `Math(Sum)`) with the same keys as fields |
| `_LEAD`, `_POLITE`, `_TAIL`, `_PLACE`, `_WHEN` | private `LEAD`, `POLITE`, `TAIL`, `PLACE`, `WHEN` | verbatim; every pattern goes through `pyre`, so `\s` also matches `\x1c`..`\x1f` as Python's does |
| `normalize` | `normalize(&str) -> String` | same steps in the same order |
| `_day` | private `day_of` | |
| `_TIME`, `_DATE`, `_WEATHER`, `_SUN`, `_AIR`, `_BATTERY`, `_COUNTDOWN` | private statics, anchored `^(?:…)$` for `fullmatch` | verbatim; the `regex` crate's leftmost-first order picks the same groups as Python's backtracking |
| `match` | `match_text(&str) -> Option<Ask>` | `match` is a Rust keyword; same checks in the same order (90 code points, time, date, weather, sun, air, battery, countdown, convert, math) |
| `_MATH_WORDS`, `_OPS` | private `MATH_WORDS` (fancy-regex for the look-behinds), `Op` | |
| `_eval` | private `evaluate` (tokenizer + recursive-descent parser over Python's grammar) | ints are exact (`BigInt`), `/` of two ints is correctly rounded like `long_true_divide`, `**` refuses `abs(right) > 64` or `abs(left) > 1e6`, `//`, calls and `()` are not arithmetic, a float `**` past the range is OverflowError; checked against Python on 20,000 random sums |
| `arithmetic` | `arithmetic(&str) -> Option<Sum>` | `Sum { said, value: Num }`; `Num` is `Int(BigInt)` or `Float(f64)` like Python's result |
| `_num` | `num_text(&Num) -> String` | whole numbers with commas below 1e15, else `,.4f` with zeros stripped (ints past 1e15 format through float, as Python's `int.__format__` does) |
| `UNITS`, `_TEMPS`, `_UNIT` | `UNITS` (`LazyLock<HashMap>`), private `TEMPS`, `unit_group` | same table |
| `_unit` | the `unit` closure that fills `UNITS` | |
| `_convert` | `convert(f64, &str, &str) -> Option<f64>` | |
| `conversion` | `conversion(&str) -> Option<Conversion>` | `Conversion { value, src, dst, result }` |
| `_nth_weekday` | `nth_weekday(year, month, weekday, n)` | Monday = 0 |
| `HOLIDAYS` | `HOLIDAYS` (`LazyLock<HashMap<&str, fn(i32) -> Option<NaiveDate>>>`) | same keys |
| `holiday` | `holiday(&str, NaiveDate) -> Option<(String, NaiveDate)>` | |
| `time_line` | `time_line(&NaiveDateTime, &str)` | `%-I:%M %p`; the place is "" for here |
| `date_line` | `date_line(&NaiveDateTime)` | |
| `WMO` | `wmo(i128) -> Option<&str>` | same codes and words |
| `weather_line` | `weather_line(&str, &Value, usize) -> Option<String>` | Python's `float()`/`int()`/`round()` on JSON values (strings with any Unicode decimal digit, bools, ties to even), list or string indexing; None where Python catches |
| `sun_line` | `sun_line(&str, &Value, &str, usize)` | `datetime.fromisoformat` via `eve_places::py::fromisoformat` |
| `AQI_BANDS` | `AQI_BANDS` | |
| `air_line` | `air_line(&str, &Value)` | |
| `math_line` | `math_line(&str, &Num)` | |
| `convert_line` | `convert_line(&Conversion)` | |
| `countdown_line` | `countdown_line(&str, NaiveDate, NaiveDate)` | |
| `battery_line` | `battery_line(&str)` | |
| `zone` | `zone(Option<&Env>) -> Option<Tz>` | `EVE_TIMEZONE`, stripped; a key in the wrong case loads (the Mac's file system); the same warning with `%r` quoting. `ZoneInfo(key)` is private `zone_info`: ValueError for an absolute, unnormalized or escaping key (zoneinfo's `_validate_tzfile_path`), else ZoneInfoNotFoundError |
| `Getter` | `Getter` (`Arc<dyn Fn(String, Map) -> BoxFuture<anyhow::Result<Value>>>`) + `getter(async closure)` | the params map keeps Python's key order |
| `_get_json` | `get_json(url, params)`, `get_json_with(&Client, …)` (Python's `transport`), `fetch_json` (the request without re-installing the filter), `client()` | httpx's 5 s connect/read timeouts, no redirects, any non-2xx is an error (`raise_for_status`), query values written as httpx writes them (`repr` of floats, `true`/`false`) and encoded as httpx 0.28 encodes them (`quote_plus`, `~` kept, `*` encoded: private `query_string`); logs httpx's `HTTP Request: GET <url> "HTTP/1.1 200 OK"` at INFO, target "httpx", through the filter |
| `_pmset` | `pmset()` (+ `Pmset`, `pmset_fn`) | `pmset -g batt`, stderr to null, 5 s timeout, UTF-8 with replacement |
| `Place` | `Place { name, lat, lon, tz: Option<String> }` | a non-string `timezone` from the geocoder is None (see Differences); `float()` of the coordinates fails as ValueError for a string and TypeError for anything else, a hit that is not a dict as AttributeError (private `float_of`, `geocoded`) |
| `phone_here` | `phone_here(Option<&Env>) -> Option<Place>` | `eve_places::phone_location::fresh_within(…, PHONE_FRESH_MIN)`, coordinates `round(x, 2)` via `eve_places::py::round_to`; an empty place name is `PHONE_PLACE` (`fix.place or PHONE_PLACE`) |
| `QuickAnswers` | `QuickAnswers::new(Option<Env>)` + `with_get`, `with_now`, `with_pmset`, `with_phone`; `tz`, `home_name`, `home()`, `knows_here()`, `match_text`, `answer(&Ask)`; private `phone_place` (`_phone`), `answer_inner` (`_answer`), `here` (`_here`), `place` (`_place`) | `home` and the geocode cache sit behind mutexes so `answer` takes `&self`; a failed geocode is not cached, a miss is |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_quick_answers.py` | `test_standard_questions_are_answered_by_code`, `test_anything_more_goes_to_the_model`, `test_a_place_and_a_day_are_read`, `test_the_sums_are_right`, `test_no_sum_runs_away_or_runs_code`, `test_a_sum_too_big_for_a_float_is_left_to_the_model`, `test_the_conversions_are_right`, `test_a_conversion_across_dimensions_is_not_one`, `test_countdowns_land_on_the_next_date`, `test_the_words`, `test_weather_words`, `test_here_is_looked_up_once_and_answers_come_from_code`, `test_without_a_home_place_weather_is_the_models`, `test_a_failed_source_hands_the_question_back_to_the_model`, `test_time_elsewhere_uses_the_places_zone`, `test_the_battery_is_the_macs` | `tests/quick_answers.rs`: the same names (parametrized cases looped), plus `the_weather_params_are_pythons`, `bad_coordinates_and_a_bad_zone_are_ignored`, `a_place_the_geocoder_does_not_know_is_the_models_and_is_remembered`, `the_tomorrow_and_air_answers`, `the_words_take_pythons_types`; `src/lib.rs` unit tests for the evaluator, `_num`, `float()`/`int()`, the whitespace dialect, `%r`, httpx's query encoding (`the_query_is_written_as_httpx_writes_it`), `ZoneInfo`'s errors (`a_zone_key_fails_as_zoneinfo_fails`), the logged error names (`the_error_names_are_pythons`) and Unicode digits (`any_decimal_digit_is_a_number_as_in_python`) | — |
| `bridge/tests/test_phone_location.py` (tests_ref: the "quick answers' here" section) | `test_a_fresh_phone_fix_is_here_rounded_and_named`, `test_with_no_place_name_it_is_where_you_are`, `test_a_stale_fix_or_the_switch_off_falls_back_to_the_configured_place`, `test_a_named_place_still_beats_the_phone`, `test_a_broken_phone_reader_never_breaks_an_answer`, `test_the_phones_coordinates_never_reach_the_log`, `test_the_filter_only_touches_open_meteo_urls` | `tests/phone_location.rs` (same names, plus `a_phone_fix_is_here_for_phone_here_only_while_fresh`), `tests/coordinates.rs` (the two log tests: a real reqwest client against a local stand-in for api.open-meteo.com, the INFO log captured, with the positive control) | the last lines of the stale test set `EVE_PHONE_LOCATION=0` in `os.environ`; Rust tests do not mutate the process environment, so the switch is checked through the lent environment (the same `phone_location::enabled` path). The rest of that file tests eve-places. |

## Boundaries
- `pmset -g batt` is run as a subprocess, as Python does.

## Differences
- Python's httpx logs every request line itself; reqwest does not, so this crate logs httpx's line (same text, target "httpx") for its own Open-Meteo calls and applies the filter there. The filter is a global switch rather than a filter object on a logger.
- A sum whose result would be complex (`1 * (-8) ** 0.5`) raises TypeError out of Python's `match` (an unanswered message); here it is not arithmetic and `match_text` returns None.
- Where a line function would raise something Python does not catch there (OverflowError on an infinite number, StopIteration past the AQI table, an unbound name for a day past tomorrow), the Rust function returns None. `answer` already turned those into None in Python.
- Errors are logged by kind as Python does (`type(e).__name__`), with names mapped from the Rust error (`KeyError`, `TypeError`, `ValueError`, `AttributeError`, `ZoneInfoNotFoundError`, httpx's `ConnectTimeout`, `ReadTimeout`, `ConnectError`, `ReadError`, `RemoteProtocolError`, `HTTPStatusError`, `JSONDecodeError`, the OSError subclasses by `io::ErrorKind` such as `FileNotFoundError`, else `OSError` / `Error`); never the message, which could carry a URL with coordinates. reqwest's error kinds are coarser than httpx's classes, so a rare transport failure can be named differently (for example a write timeout logs `ReadTimeout`).
- A geocoder hit whose `timezone` is truthy but not a string: Python keeps it and `ZoneInfo(5)` raises TypeError, logged as "time unavailable (TypeError)"; here the zone is None and the time question goes to the model without that log line. The answer is None in both.
- Time zones come from chrono-tz's bundled database rather than the Mac's `/usr/share/zoneinfo`.
- A `pmset` that outlives the timeout is killed (Python leaves it running).
