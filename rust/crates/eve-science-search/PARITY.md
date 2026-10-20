# eve-science-search
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/science_search.py` | `src/lib.rs` (with `src/transport.rs` for the HTTP seam, `src/py.rs` for Python value semantics, `src/html.rs` + `src/html5_entities.rs` for `html.unescape`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `SWITCH`, `TOOL_NAME`, `BASE`, `SEARCH_URL`, `SITE`, `HOST`, `OFF_REASON`, `USER_AGENT` | same-named `pub const`s | verbatim |
| `DEFAULT_K` … `MIN_YEAR`, `ATTEMPT_SECS` … `RATE_WINDOW_SECS`, `NOTE` | same-named `pub const`s | verbatim |
| `TOOL`, `TOOLS`, `TOOL_NAMES` | `TOOL: LazyLock<Value>`, `TOOLS: LazyLock<Vec<Value>>`, `TOOL_NAMES: [&str; 1]` | description and schema byte-identical, key order kept |
| `HIDDEN` | `HIDDEN` | |
| `class QueryFilter` | `struct QueryFilter` + `QueryFilter::filter(&str) -> String` | same regex (Python's `\s`, \x1c-\x1f included) and HOST check; returns the rewritten line (Python rewrites the record and never drops it) |
| `hide_queries` | `hide_queries(make_writer) -> HideQueries` | a `tracing_subscriber` `MakeWriter` wrapper that passes every log line through `QueryFilter` (Rust has no "httpx" logger to attach a filter to) |
| `enabled` | `enabled(Option<&HashMap<String,String>>)` | None reads the process environment |
| `tools` | `tools(environ)` | |
| `class BadArgs` | `struct BadArgs(String)` (thiserror) | |
| `clean_text` | `clean_text(Option<&Value>, limit)` and `clean_str(&str, limit)` | `str(raw or "")` via `py::str_or_empty` (Python `repr` for lists/dicts/floats/bools) |
| `_int` | `int_arg` (private) | int, finite float under 1e9 truncated, `\s*-?\d{1,9}\s*` string (Unicode digits read as `int()` does; see Differences for the \x1c-\x1f edge) |
| `_flag` | `flag` (private) | |
| `params` | `params(&Value, Option<DateTime<Utc>>) -> Result<Map, BadArgs>` | key order q, k, year_from, year_to, type, open_access |
| `_authors` | `authors` (private) | the count in i128, so a count past i64 still reads as Python's unbounded int |
| `_abstract` | `abstract_text` (private) | tag regex then full HTML5 `html.unescape` |
| `_site` | `site` (private) | `urllib.parse.quote(ref, safe="/:")` |
| `paper` | `paper(&Map) -> Map` | same keys, same order (`plain_words` before `abstract`); `smr == 1` true for 1, 1.0 and true |
| `shape` | `shape(&Value, &str) -> Value` | `matches` kept when an int or a bool, as `isinstance(…, int)` |
| `class _Limiter` | `struct Limiter` (`new(clock)`, `Default`, `take`, `reset`) | `parking_lot::Mutex<VecDeque<f64>>`; clock is a seam |
| `LIMITER` | `LIMITER: LazyLock<Arc<Limiter>>` | |
| `_retry_after` | `retry_after` (private) | header through `float()` semantics, then the JSON body's `retry_after`; clamped 1..3600 |
| `_busy` | `busy` (private) | |
| `_detail` | `detail` (private) | |
| `search` | `async fn search(&Value, &SearchOptions) -> Value` | `SearchOptions { transport, limiter, sleep, now, clock }` are Python's keywords plus the monotonic clock its tests patch |
| `handle` | `async fn handle(name, &Value, environ, &SearchOptions) -> Value` | |
| (httpx transport) | `trait Transport`, `ReqwestTransport`, `DEFAULT_TRANSPORT`, `HttpRequest`, `HttpResponse` (`header`, `joined_header`), `TransportError` | redirects not followed; query string written as httpx writes `params=` (`quote_plus`); header bytes decoded as httpx's `Headers.encoding` (ASCII/UTF-8, else ISO-8859-1); `joined_header` is httpx's `Headers.get` (repeated values joined by ", "), which `retry_after` reads |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_science_search.py` | the tool (2), the arguments (6), the answer (4), the request (8), the log (1), the switch (2), live (1) | `tests/science_search.rs`: same names (parametrized cases as loops, plus extra edge cases), `test_a_request_that_hangs_times_out_within_the_budget`, `test_an_answer_with_a_lone_surrogate_is_still_read`, `test_the_limiter_resets`; `tests/log.rs`: `test_the_owners_words_never_reach_the_log` (own binary: tracing callsite interest is process-wide); `src/py.rs` unit tests; live test `#[ignore = "live: ..."]` (run once by hand, passed) | `test_the_taint_gate_reads_it_as_outside_text_that_never_asks` (tool_gate crate), `test_its_results_are_kept_like_web_searchs_and_marked_as_outside_words` (telegram / voice_agent / transcripts crates), `test_the_chat_brain_offers_and_answers_it_and_the_turn_is_tainted`, `test_the_chat_brain_off_neither_offers_nor_answers`, `test_the_tool_choice_line_rides_only_with_both_searches` (telegram crate), `test_out_loud_it_is_offered_never_in_a_lesson_and_answered` (voice_agent crate), `test_the_intent_eval_has_its_label` (tools/intent_eval.py, not a crate): each tests another module's wiring of this tool |

## Boundaries
None.

## Differences
- Timeouts: Python passes `timeout=min(ATTEMPT_SECS, left)` to httpx, which applies it to each phase (connect, read, write, pool); Rust bounds the whole attempt (request and body) by that time. An attempt can only end sooner, never later.
- Python's `search` calls `hide_queries()` again before each request because tests reset the httpx logger; the Rust filter lives in the daemon's log writer (`hide_queries`), so there is nothing to re-install.
- JSON integers beyond u64 (e.g. `k: 10**30`) arrive as floats through serde_json, so they read as out of range (default k, no year) where Python clamps the big int. Model arguments never carry such numbers.
- A `Retry-After` header of `inf` (or a value past a float's range) makes Python's `int(float(...))` raise OverflowError out of `search`; Rust ignores such a header and reads the body's `retry_after`, as for any other unreadable header.
- Bodies: a UTF-8 byte-order mark is accepted as in `json.loads`; UTF-16/32 bodies and the non-standard `NaN`/`Infinity` literals Python's json accepts are read as "not JSON" (the service's FastAPI answers never carry them: its JSON encoder refuses NaN). A `\uXXXX` escape of a lone surrogate, which `json.loads` keeps as a lone surrogate, reads as U+FFFD (a Rust string cannot hold one), so the answer is still read (`json_body`).
- A number string edged with the ASCII separators \x1c-\x1f (`"2020\x1c"` for `k` or a year): `_int`'s pattern takes it (Python's `\s` counts them) but `int()` does not strip them, so Python raises ValueError out of `params`, `search` and `handle`. Rust cannot raise there without changing `handle`'s signature, so the value reads as no number (the default `k`, no year). Model arguments do not carry these characters.
- Request headers: httpx adds `Accept-Encoding: gzip, deflate`; reqwest (gzip and brotli features) sends `gzip, br`. Both decode what they ask for.
- Logging: messages are the same text through `tracing::info!` (Python's `%.1f` and `%d` formats kept).
