# eve-events
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/events.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `TOOL_NAME`, `KEY_ENV`, `TIMEOUT_S`, `NEAR_RADIUS_MI`, `NEAR_DECIMALS`, `MAX_EVENTS`, `OFF_REASON`, `NO_HERE` | same names | same values |
| `TOOL_NAMES` | `TOOL_NAMES: [&str; 1]` | |
| `CATEGORIES` | `CATEGORIES: [&str; 6]` | same order (the tool's enum) |
| `_DAY`, `_STATE` | private `DAY`, `STATE` (`regex`) | `\d` is `[0-9]`: a Unicode digit passed Python's pattern and then failed `date.fromisoformat`, the same ValueError |
| `TOOL` | `TOOL: LazyLock<Value>` | `json.dumps` of both is byte-identical (test) |
| `INSTRUCTIONS`, `WATCH_HINT` | same names | verbatim (test against the Python's printout) |
| `Fetch` | `Fetch` (`Arc<dyn Fn(&str, &[(String, String)]) -> anyhow::Result<(u16, String)>>`) | an error that downcasts to `eve_web::FetchError` carries its status / retry_after; an `io::Error` of kind `TimedOut` is Python's TimeoutError |
| `available` | `available(Option<&Env>)` | `None` is the process environment; the key is stripped as Python strips |
| `instructions` | `instructions(bool) -> String` | |
| `_utc` | `utc(&DateTime<Utc>)` | `%Y-%m-%dT%H:%M:%SZ` |
| `_day` | `day(Option<&Value>) -> Result<Option<NaiveDate>, BadQuery>` | `str(raw or "")` via `eve_places::py::truthy_str`; years 1..=9999 as Python's `date` |
| (ValueError) | `BadQuery` | |
| `midnight` | `midnight(NaiveDate, Option<Zone>)`, `midnight_in(.., &L: TimeZone)` | a fixed offset (`Zone::Fixed`, Python's `timezone`) is read in the Mac's local zone (`chrono::Local`, lent in `_in` for tests); a real zone (`Zone::Named(Tz)`) keeps its rules. Python's fold=0: an ambiguous midnight is the earlier instant, a midnight in a gap takes the offset before the gap (ZoneInfo, and `_mktime`'s `max(u1, u2)`) |
| aware `datetime` `now` | `OwnerNow { at, zone }` (`OwnerNow::local()` is `datetime.now().astimezone()`, `named`, `fixed`) | |
| `here` | `here(Option<&Env>)` | `eve_places::phone_location::fresh`, else `EVE_WEATHER_LAT`/`LON` read as Python's `float()` (strip, `_` between digits, inf/nan); range check; `round(x, 2)` via `eve_places::py::round_to` |
| `wants_near` | `wants_near(&Map)` | `near` must be JSON `true` (`is True`) |
| `query` | `query(&Map, &OwnerNow, Option<(f64, f64)>) -> Result<IndexMap<String, String>, BadQuery>`, `query_in(.., &L)` | same keys in Python's insertion order; `city[:80]`, `keyword[:120]` by code point; whitespace collapsed by Python's `split()` (`eve_places::py::split`) |
| `urllib.parse.urlencode` | `urlencode` | `quote_plus`: `A-Za-z0-9_.-~` kept, space `+`, the rest `%XX`; `apikey` first as `{"apikey": key, **q}` |
| `_first` | private `first` | |
| `_name` | private `name` | |
| `_money` | `money(Option<&Value>)` | Python's `float()` of a JSON value; `{:.0}` / `{:.2}` round the exact binary value ties-to-even as Python does (`0.125` is `0.12`, test) |
| `best_image` | `best_image(Option<&Value>)` | same filter, `int(width or 0)` as Python reads it; of equal widths the first, as `max` keeps it |
| `trim` | `trim(&Map) -> Map` | same keys, same order; `image` and `price` only when present |
| `parse` | `parse(&Value) -> Value` | `watch::ticketmaster::tm_events`; `total` only for a JSON integer |
| `_reason` | `reason(&anyhow::Error)` (and private `Failure`) | same order of checks, same words |
| `Events` | `Events`, `EventsOptions` (Python's keyword arguments; `where` is `r#where`) | `Default` is Python's defaults: `watch::http_request` with `timeout_s`, no limiter, `OwnerNow::local`, the process environment, offloaded, `here(environ)` |
| `Events.enabled`, `tools`, `handle`, `search` | same names (`handle`/`search` async) | `search` never fails; same results, log lines and limiter calls |
| `Events._limiter_now` | private `limiter_now` | the limiter callable returns `anyhow::Result<Option<Arc<dyn Limiter>>>`; an error is no limiter |
| the limiter's duck type (`backed_off`, `take`, `penalize`, `clear`) | trait `Limiter` (each call fallible, each failure swallowed where Python swallows it), implemented for the Watcher's `parking_lot::Mutex<RateLimiter>`; `limiter_of(Arc<Mutex<RateLimiter>>)` | |
| `_seconds` | private `seconds` | 0 for an error, NaN or ≤ 0 |
| `_round` | private `round_up` | `max(1, int(secs + 0.999))` |
| `_now` | the `offload: false` branch of `search` | the fetch runs on the task inside the deadline, as Python's coroutine does inside `wait_for` |
| `asyncio.to_thread` + `wait_for(timeout + 0.5)` | `spawn_blocking` + `tokio::time::timeout` | the worker thread finishes on its own, as Python's does |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_events.py` | all 21 (parametrized cases looped in one Rust test each) | `tests/test_events.rs`, same names | in `test_the_tool_is_strict_and_sits_in_the_always_carried_events_toolset`, the `toolset_router.ALWAYS` / `always_toolset` asserts: they test eve-toolset-router, which this crate does not depend on |
| `bridge/tests/test_find_image.py` | `test_an_event_keeps_ticketmasters_widest_16_9_picture` | same name in `tests/test_events.rs` | the rest test find_image |
| `bridge/tests/test_menu_tools.py` | the `events` lines (lent with the key, the line, the inlet handler) | covered here by `test_no_key_is_a_clear_reason_and_no_tool`, `test_handle_answers_only_its_own_tool` | the menu/telegram wiring belongs to those crates |
| (new) | | `test_the_tool_is_pythons_json_byte_for_byte`, `test_a_500_backs_off_only_when_it_said_how_long`, `test_handle_answers_only_its_own_tool`, `unit::*` (money, quote_plus, DST gap/ambiguous midnights, Python's int/float, calendar ends) | |

`test_a_fixed_offset_now_reads_each_day_in_the_mac_s_own_zone` checks the fixed-offset path with the zone lent, and
then re-runs itself (`fixed_offset_child`) in a child process of the test binary with `TZ=America/Los_Angeles`, so
`chrono::Local` itself is checked without the test setting TZ in its own process.

## Boundaries
None

## Differences
- A Discovery answer is read with `serde_json`, not Python's `json.loads`: a body carrying `NaN`/`Infinity`
  literals or a lone surrogate escape is "Ticketmaster sent an unreadable answer" here where Python read it.
  Ticketmaster sends neither. Two more edges of the same reader: a body nested deeper than serde_json's 128 levels
  is "unreadable" here, where Python parsed it (or, past its recursion limit, said "could not be reached"); and a
  `totalElements` integer past 64 bits is read as a float, so `total` is left out where Python kept it.
- Where Python raised out of `search` on a shape or value it did not expect (a truthy non-object where an object
  belongs, a width `int()` cannot read, a non-finite price, a last day of 9999-12-31, an infinite back-off), the port
  reads the field as missing, skips that picture, leaves the price out, answers the bad-dates reason, or saturates
  the seconds. `search` never fails, as its docstring promises.
