# eve-web: reader-providers (web_reader.py, firecrawl.py, tinyfish.py)
Status: complete

## Modules
| Python | Rust |
|---|---|
| web_reader.py | src/web_reader.rs |
| firecrawl.py | src/firecrawl.rs |
| tinyfish.py | src/tinyfish.rs |

## API
| Python | Rust | Notes |
|---|---|---|
| firecrawl.api_key | firecrawl::api_key | reads `ctx` |
| firecrawl.usd_per_credit | firecrawl::usd_per_credit |  |
| firecrawl.per_day | firecrawl::per_day |  |
| firecrawl._day | firecrawl::day |  |
| firecrawl._count_path | firecrawl::count_path | `DayCount.path`, else `spend_dir()/firecrawl-calls.json` |
| firecrawl._load | firecrawl::load | `_CALLS` / `_LOADED` are `Ctx.firecrawl_calls` (DayCount) |
| firecrawl._save | firecrawl::save | writes `{"day": ..., "calls": n}` with json.dumps's separators, via a `.tmp` and a rename |
| firecrawl.left | firecrawl::left |  |
| firecrawl.take | firecrawl::take |  |
| firecrawl.meter | firecrawl::meter | into `ctx.ledger` |
| firecrawl._post | firecrawl::post | through `ctx.http`; body is `json.dumps` text (eve_jev::py_dumps) |
| firecrawl._tool | firecrawl::tool | returns `Tool` |
| firecrawl.search | firecrawl::search | returns `Found` |
| firecrawl.run_tool | firecrawl::run_tool | takes `ToolCall` (Python's dict without `name`), returns `Ran` |
| firecrawl.pick_tool | firecrawl::pick_tool | `Result<Option<ToolCall>>`: Err where Python lets FetchError out |
| tinyfish.api_key | tinyfish::api_key |  |
| tinyfish._call | tinyfish::call |  |
| tinyfish.search | tinyfish::search | returns `Vec<SearchResult>` |
| tinyfish.fetch | tinyfish::fetch | returns `(IndexMap, IndexMap)` keyed in Python's dict order |
| web_reader.ReaderConfig | web_reader::ReaderConfig |  |
| web_reader.configured | web_reader::configured, configured_shipped | the browser_router.SHIPPED seam |
| web_reader.search_and_read | web_reader::search_and_read | returns the `pages` list |
| web_reader._urls_in | web_reader::urls_in |  |
| web_reader.answer_parts | web_reader::answer_parts |  |
| web_reader.answer_from | web_reader::answer_from |  |
| web_reader._take_task | web_reader::take_task | `_TASKS` is `Ctx.reader_tasks` (TaskCount) |
| web_reader.WebReaderAgent | web_reader::WebReaderAgent | implements eve_app_reflex::Agent; `__getattr__` forwarding is `current()`; `browser_shot` reads Codex's tab through `ReaderOptions.browser_screenshot` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_web_reader.py | test_it_answers_from_the_pages_with_only_their_links, test_a_page_cannot_add_a_link_of_its_own, test_every_failure_hands_the_whole_task_to_codex (4 cases), test_the_daily_cap_hands_to_codex_without_spending, test_stop_while_reading_is_stopped_not_handed_on, test_the_search_and_fetch_requests, test_a_failed_fetch_answers_from_the_snippets, test_a_failed_search_is_a_fetch_error_from_tinyfish, test_the_switch, test_a_many_source_task_is_answered_by_the_engine_search_with_its_citations, test_an_engine_search_that_did_not_answer_hands_the_task_to_codex, test_a_routed_reader_that_fails_tries_the_engine_search_before_codex, test_a_fitting_tool_is_run_and_answers_with_only_its_own_links, test_a_tool_pick_that_does_not_check_out_runs_nothing (3 cases), test_a_failed_tool_or_unaccepted_terms_falls_back_to_the_pages, test_the_firecrawl_search_request, test_the_alexandria_requests, test_a_firecrawl_failure_is_a_fetch_error_from_firecrawl, test_a_credits_price_is_read_or_left_unpriced, test_the_reader_takes_the_routers_providers_and_the_off_switch_keeps_tinyfish | tests/reader_providers.rs: same names | test_the_daemon_routes_jev_first_then_the_chrome_rule: it drives `Daemon._bodies` (daemon.py), which is eve-daemon's to port and test |
| (added) | — | firecrawl_daily_cap_is_shared_counted_and_survives_a_restart, firecrawl_per_day_reads_numbers_as_python_does, a_spent_day_never_reaches_firecrawl, firecrawl_request_bodies_are_pythons_json, a_tool_that_needs_an_id_looked_up_first_is_not_offered, pick_tool_reads_a_number_given_as_text_and_only_plain_values, run_tool_reports_why_it_failed, meter_records_credits_priced_or_not, tinyfish_search_keeps_only_web_links_and_quotes_the_query, tinyfish_unreadable_answers_are_errors_from_tinyfish, tinyfish_fetch_keys_results_and_errors_by_url, urls_in_reads_string_values_only, answer_parts_lists_the_tools_or_the_tool_data_and_reads_only_page_sources, the_task_cap_keeps_the_last_seven_days, status_steer_and_cancel_forward_to_codex_once_it_took_over | |

Python's `quiet` fixture becomes a `Ctx` per test: a ledger in a temp folder (its rows are read back instead of
patching `spend.record`), a fresh `DayCount` on a temp file and a fresh `TaskCount`. `monkeypatch.setattr(watch,
"http_request", fake)` is `Ctx::with_http`. `pytest.fail` inside an unused fake becomes a flag the test asserts
is still false (a panic inside the router's worker thread would be caught there, as Python's `except Exception`
would not catch `Failed`). caplog is a tracing subscriber on the test's thread.

The tests that run the agent, `test_the_switch` and the `answer_parts` test go through search_router::answer,
websearch::configured, websearch::answer_options and search_router::keep_read_links, which the search-router unit
fills; they pass once that unit is merged.

## Boundaries
None.

## Differences
- `int(float(x))` of an infinite `EVE_FIRECRAWL_PER_DAY` / `EVE_WEB_READER_TASKS` raises OverflowError in Python
  (out of `per_day()` / `configured()`); Rust saturates and clamps (10000 for `inf`, 0 for `-inf`).
- `float()` here reads ASCII literals (digits, single underscores between digits, a point, an exponent, inf,
  infinity, nan, a sign); Python's `float()` also reads non-ASCII Unicode digits. `pick_tool` reads an `id` given
  as text with ASCII digits only (Python's `isdigit()` accepts other digits, and `int()` of some of them raises).
- A search that panics (Rust's form of Python's `RuntimeError`) reaches the reader through search_router; when the
  search's worker thread panics past it, the reader's `why` is `"panic"` (Python logs the exception's class name).
- `str()` of a non-string JSON value (a number as a URL, a list as a title) is Python's `repr` text as far as JSON
  can carry it; JSON's `NaN`/`Infinity` literals, which Python's `json.loads` accepts, are an unreadable answer.
- A `bool` in Firecrawl's `creditsUsed` is not counted as credits (Python's `isinstance(True, int)` is); in
  `creditsCost` it counts as 1 or 0, as in Python's arithmetic.
- `(item.get("metadata") or {}).get(...)` on a non-object `metadata` raises AttributeError in Python (the whole
  search fails); Rust reads it as no `sourceURL`. A non-object entry in the router's `sources` raises the same in
  Python's `_attempt` (the task goes to Codex); Rust skips that entry.
