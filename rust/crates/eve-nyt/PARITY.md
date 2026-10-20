# eve-nyt
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/nyt.py` | `src/lib.rs` (the module), `src/transport.rs` (the httpx transport seam), `src/py.rs` (Python value semantics), `src/html.rs` + `src/html5_entities.rs` (`html.unescape`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `_nullable` | `nullable` (private) | |
| `_function` | `function` (private) | `SEARCH_TOOL` .. `HEADLINES_TOOL`, `KEY_TOOLS`, `TOOLS` are `LazyLock<Value>`; byte-for-byte equal to Python's (fixture test) |
| `QueryFilter` | `QueryFilter::filter(&self, &str) -> String` | Python rewrites a `LogRecord` in place; Rust returns the line to log |
| `hide_queries` | `hide_queries(make_writer) -> HideQueries<_>` | Python adds the filter to the httpx/httpcore loggers; Rust wraps the daemon's `tracing_subscriber` writer so every line passes `QueryFilter` |
| `enabled` | `enabled(Option<&Environ>)` | `None` reads the process environment |
| `api_key` | `api_key(Option<&Environ>)` | |
| `tools` | `tools(Option<&Environ>)` | |
| `BadArgs` | `BadArgs(String)` | thiserror; the message is the reason |
| `clean_text` | `clean_text(Option<&Value>, usize)`, `clean_str(&str, usize)` | `str()` of non-strings as Python prints them |
| `_int` | `to_int` | |
| `_day` | `day` | |
| `https_url` | `https_url` | |
| `_today` | `today` (private) | |
| `_wall` | `wall` (private) | `now.timestamp()` as Python computes it (microseconds / 1e6) |
| `_key` | `key` | |
| `parse_date` | `parse_date -> Result<Option<NaiveDate>, BadArgs>` | |
| `_image` | `image` | |
| `_widest` | `widest` (private) | first of equals, as `max` |
| `article_image` | `article_image` | |
| `top_image` | `top_image` | |
| `popular_image` | `popular_image` | |
| `book_image` | `book_image` | |
| `cache_root` | `cache_root(Option<&Path>)`, `cache_root_in(xdg, home)` | the argument is Python's `CACHE_DIR` |
| `_cache_path` | `cache_path(root, parts)` | |
| `cache_get` | `cache_get` | |
| `cache_put` | `cache_put` | atomic: `.tmp-*.json` in the folder, then rename; same compact JSON `{"saved":..,"data":..}` |
| `_Spacer` | `Spacer` (`new`, `with_clock`, `reserve`, `hint`, `day_full`, `reset`) | `KEY_LIMITER`, `RSS_LIMITER` are `LazyLock<Arc<Spacer>>` |
| `_pacing` | `pacing` (private) | |
| `_busy` | `busy` (private) | |
| `_retry_after` | `retry_after(Option<&str>)` | |
| `Got` | `Got` | |
| `_TooBig` | `Attempt::TooBig` (private) | |
| `fetch` | `fetch(url, query, limiter, transport, sleep, &FetchSpec)` | Python's keyword arguments are `FetchSpec` (attempt/budget seconds, max bytes, what, accept) |
| `_json` | `parse_json` (private) | |
| `_unreadable` | `unreadable` (private) | |
| `_keyed` | `keyed` (private) | |
| `lucene_quote` | `lucene_quote` | |
| `_canonical` | `canonical` (private) | |
| `build_fq` | `build_fq` | |
| `search_params` | `search_params -> Result<Map, BadArgs>` | ordered like Python's dict |
| `_byline` | `byline` (private) | |
| `_headline` | `headline` (private) | |
| `article` | `article` | |
| `nyt_search` | `nyt_search(args, key, &Opts)` | Python's `**kw` (transport, sleep, limiter, rss_limiter, now) and `CACHE_DIR` are `Opts` |
| `archive_month` | `archive_month` | |
| `_archive_row` | `archive_row` (private) | |
| `nyt_archive` | `nyt_archive` | |
| `books_date` | `books_date` | |
| `_buy_link` | `buy_link` (private) | |
| `_on_nytimes` | `on_nytimes` (private) | with `hostname`, `urlsplit(..).hostname` |
| `book` | `book` | |
| `_list_names` | `list_names -> Vec<ListName>` | cached as `[{"slug","name"}]`, as Python |
| `match_list` | `match_list(&str, &[ListName])` | takes the cleaned argument, as the tool passes it |
| `_overview` | `overview` (private) | |
| `nyt_bestsellers` | `nyt_bestsellers` | |
| `popular_path` | `popular_path` | |
| `popular_row` | `popular_row` | |
| `nyt_popular` | `nyt_popular` | |
| `resolve_section` | `resolve_section` | with `top_to_rss`, `rss_to_top` for `TOP_TO_RSS` / `RSS_TO_TOP` |
| `_facets` | `facets` (private) | |
| `top_row` | `top_row` | |
| `FeedRefused` | `FeedRefused(String)` | |
| `parse_rss` | `parse_rss(&[u8]) -> Result<Vec<Value>, FeedRefused>` | roxmltree after the same size and DOCTYPE/ENTITY refusals; decodes BOM / declared encoding as expat; ElementTree `.text` semantics |
| `_top_stories` | `top_stories` (private) | returns `TopStories::{Rows, Failed(Got), Other}` for Python's list / Got / other cached value |
| `_rss` | `rss` (private) | |
| `nyt_headlines` | `nyt_headlines` | |
| `handle` | `handle(name, &Value, Option<&Environ>, &Opts)` | the key-leak gate is `refuse_key_leak` (public, so it can be tested without patching `_HANDLERS`) |

Constants (`SWITCH`, `KEY_ENV`, the URLs, `USER_AGENT`, tool names, reasons, limits, sizes, the specs' value lists,
`SECTION_ALIASES`, `NOTES` as `note()`, `HIDDEN`, `FALLBACK_STATUSES`) keep their names and values.

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_nyt.py` | 54 | `tests/test_nyt.rs`: the 46 tests of nyt.py's own behavior, same names, plus `test_the_schemas_are_pythons_byte_for_byte`, `test_a_cache_python_wrote_is_read_and_ours_has_its_shape`, `test_a_stale_or_broken_cache_is_fetched_again`, `test_retry_after_reads_like_python`, `test_python_whitespace_includes_the_ascii_separators`, and `live_headlines_read_the_real_feed` (ignored: live); `tests/golden.rs`: 5 tests against outputs recorded from the Python module (`tests/fixtures/golden.json`, `tests/fixtures/tools.json`); `tests/differential.rs`: 9 tests replaying `tests/fixtures/differential.json` (written by `tests/fixtures/gen_differential.py` from the Python module): about 3,800 random and edge inputs to the pure functions (text cleaning, integers, links, dates, keys, the arguments, list matching, every row builder, RSS feeds in five encodings) and 39 whole `handle` calls against the same mocked replies, compared value, key order and requests sent; `src/py.rs`: 3 unit tests | `test_the_taint_gate_reads_them_as_outside_text_that_never_asks` (tool_gate), `test_their_results_are_kept_like_web_searchs_and_marked_as_outside_words` (telegram, transcripts, voice_agent), `test_the_chat_brain_offers_and_answers_them_and_the_turn_is_tainted`, `test_the_chat_brain_without_a_key_offers_only_headlines`, `test_the_chat_brain_off_offers_none`, `test_the_tool_choice_lines_ride_only_with_their_tools` (telegram), `test_out_loud_they_are_offered_never_in_a_lesson_and_answered` (voice_agent), `test_the_intent_eval_has_their_label` (tools/intent_eval.py): each tests another module's wiring of these tools and belongs to that module's crate |

Within ported tests: `nyt.tools()` with the process environment (after `delenv`) is not asserted, because tests must
not mutate the process environment; the same branch runs with an explicit empty map. The log test captures
`tracing` output written through `hide_queries` instead of httpx's records.

## Boundaries
None.

## Differences
- Logging: there is no httpx logger to filter. `hide_queries` wraps the log writer instead, and this crate never logs
  a URL or query itself (as in Python).
- Query strings are encoded by the `url` crate (`application/x-www-form-urlencoded`: space as `+`), which differs
  from Python's `quote_plus` only for `*` (left as is) and `~` (escaped); NYT decodes both the same.
- An integer in JSON above `i64::MAX` reads as `i64::MAX` (Python keeps the exact value; every use clamps it), and a
  JSON integer too large for `u64` arrives as a float (serde_json) and so is not an integer.
- `Retry-After: inf` makes Python raise `OverflowError` out of `fetch`; Rust treats it as no Retry-After.
- A JSON answer with a lone surrogate escape (`"\ud800"`) is unreadable in Rust; Python's `json.loads` accepts it.
- `_int` of a string that Python's `\s` lets through but `int()` refuses (e.g. `"\x1c0"`) raises `ValueError` out of the call in Python; Rust reads it as no number.
- A corrupt cache file whose rows are not objects is skipped row by row where Python would raise.
- Timeouts: httpx applies its timeout to each operation; Rust wraps the request and each body chunk in the same
  per-attempt timeout, which is the same behavior.
