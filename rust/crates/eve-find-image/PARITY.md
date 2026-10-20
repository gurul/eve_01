# eve-find-image
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/find_image.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `SWITCH`, `TOOL_NAME`, `TOOL_NAMES`, `API`, `USER_AGENT`, `TIMEOUT_S`, `MIN_WIDTH`, `MIN_HEIGHT`, `MAX_SUBJECT_CHARS`, `OFF_REASON` | same-named `pub const` | Values verbatim. |
| `_PHOTO` | `PHOTO` (private `LazyLock<Regex>`) | `(?i)\.(jpe?g|png|webp)\n?\z`: Python's `$` also matches before one trailing newline. |
| `TOOLS` | `TOOLS: LazyLock<Vec<Value>>` | Schema verbatim, key order kept (`preserve_order`). |
| `enabled` | `enabled(Option<&HashMap<String, String>>) -> bool` | `None` reads the process env; strip + lower, off for 0/false/no/off. |
| `tools` | `tools(Option<&HashMap<String, String>>) -> Vec<Value>` | |
| `pick` | `pick(&Value) -> Option<Value>` | Stable sort by `index` (default 0), https + photo suffix (query string ignored), at least 800 x 400, `page_url` with spaces as underscores. |
| `handle` | `async handle(name, &Value args, Option<&HashMap> environ, Option<&dyn Http> http) -> Value` | Same reasons verbatim; subject whitespace-squeezed (Python `str.split()` whitespace) and clipped to 120 chars; same 8 query params in the same order. |
| (`http=` callable parameter) | `trait Http` + `FetchError { name }` | The test seam. `FetchError.name` is the exception class name Python logs. |
| (httpx default client) | `ReqwestHttp` | GET, User-Agent, 6 s connect + read timeout (httpx per-phase), no redirects followed, non-2xx is an error (`raise_for_status`), body decoded as JSON. |
| | `params(subject)` | The MediaWiki query, exposed for tests. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_find_image.py` | `test_a_photo_sized_picture_is_kept_with_its_page`, `test_a_drawn_small_or_plain_http_picture_is_refused`, `test_the_best_ranked_good_page_wins`, `test_handle_says_no_plainly_and_never_raises`, `test_live_wikipedia_gives_a_landmark_its_picture` (ignored: live), `test_the_brain_is_offered_the_tool_when_on` | same names in `tests/test_find_image.rs`, plus `test_the_lower_index_wins_among_good_pages`, `test_an_empty_or_odd_payload_has_no_picture`, `test_handle_asks_wikipedia_and_answers_with_the_picture`, `test_the_tool_schema_is_pythons`, and unit tests in `src/lib.rs` (regex, subject cleaning, switch words, param order, int parsing) | `test_an_event_keeps_ticketmasters_widest_16_9_picture` tests `events.trim` (the events crate); `test_only_an_app_turn_is_offered_the_picture_tool` tests `telegram` tool lists (the telegram crate). |

## Boundaries
None

## Differences
- A malformed MediaWiki payload that would make Python raise inside `pick` (a truthy non-dict `payload`/`query`, a non-numeric `width`/`height` string, a non-dict page) is skipped in Rust: the page is passed over, or the answer is "no high-quality picture". The module promises never to raise; Rust keeps that promise in these cases too.
- A `pages` value that is not a list (formatversion 1's dict, which the query never asks for) yields no picture rather than Python's error.
- The logged failure name for the real call is mapped from reqwest's error kind to the nearest httpx class name (`ConnectTimeout`, `ReadTimeout`, `ConnectError`, `HTTPStatusError`, `JSONDecodeError`, `TransportError`).
- A non-string, non-number `subject` (an array or object) is rendered as JSON rather than Python's `repr`.
