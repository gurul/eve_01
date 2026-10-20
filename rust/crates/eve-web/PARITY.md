# eve-web
Status: complete

The routed web search (`search_router`, `websearch`), the web reader body (`web_reader`, `firecrawl`, `tinyfish`) and
the watcher (`watch`): six Python modules, 4649 lines. Every function that read a module global in Python
(`os.environ`, `watch.http_request`, spend's ledger, Firecrawl's day count, web_reader's task count, Monid) takes a
`&Ctx` first (`src/lib.rs`: `Ctx`, `Http`, `SystemHttp`, `Clock`, `Ask`, the clocks). Tests build their own `Ctx`
instead of monkeypatching. Same environment variables, files and formats as Python: `~/.config/eve-bridge/watches.json`
(`json.dump(indent=1)`, ensure_ascii, mode 0600, written through a temp file and a rename), the Firecrawl day count
`firecrawl-calls.json` beside the spend ledger, the spend lines, the OpenRouter, TinyFish, Firecrawl, Yahoo and
Ticketmaster requests. Prompts and tool schemas are verbatim (`src/watch/tools.rs` embeds the same TOOLS JSON).

## Modules
| Python | Rust |
|---|---|
| firecrawl.py | src/firecrawl.rs |
| search_router.py | src/search_router.rs |
| tinyfish.py | src/tinyfish.rs |
| watch.py | src/watch/ (mod.rs, config.rs, limiter.rs, page.rs, net.rs, tls.rs + tls_helper.py, hosted.rs, render.rs, model.rs, rules.rs, ticketmaster.rs, tools.rs, watcher.rs, chat.rs); src/bin/eve-watch.rs is its `__main__` |
| web_reader.py | src/web_reader.rs |
| websearch.py | src/websearch.rs |

## API
| Python | Rust | Notes |
|---|---|---|
| firecrawl.api_key | firecrawl::api_key |  |
| firecrawl.usd_per_credit | firecrawl::usd_per_credit |  |
| firecrawl.per_day | firecrawl::per_day |  |
| firecrawl._day | firecrawl::day |  |
| firecrawl._count_path | firecrawl::count_path |  |
| firecrawl._load | firecrawl::load | state is Ctx.firecrawl_calls (DayCount) |
| firecrawl._save | firecrawl::save |  |
| firecrawl.left | firecrawl::left |  |
| firecrawl.take | firecrawl::take |  |
| firecrawl.meter | firecrawl::meter |  |
| firecrawl._post | firecrawl::post |  |
| firecrawl._tool | firecrawl::tool |  |
| firecrawl.search | firecrawl::search |  |
| firecrawl.run_tool | firecrawl::run_tool |  |
| firecrawl.pick_tool | firecrawl::pick_tool |  |
| search_router.questions | search_router::questions |  |
| search_router.SearchAnswer | search_router::SearchAnswer |  |
| search_router.SearchGates | search_router::SearchGates |  |
| search_router.Route | search_router::Route, RouteFn, RouteError |  |
| search_router.ask | search_router::ask |  |
| search_router.decide | search_router::decide |  |
| search_router.wrong | search_router::wrong |  |
| search_router.fit | search_router::fit |  |
| search_router.make_router | search_router::make_router |  |
| search_router.jev_router | search_router::jev_router |  |
| search_router.mode | search_router::mode |  |
| search_router.available | search_router::available, available_shipped | the SHIPPED seam |
| search_router.Paths | search_router::Paths |  |
| search_router.keep_read_links | search_router::keep_read_links |  |
| search_router._usage | search_router::usage |  |
| search_router._result | search_router::result |  |
| search_router.via | search_router::via |  |
| search_router._firecrawl | search_router::via_firecrawl |  |
| search_router._credits | search_router::credits |  |
| search_router.bounded | search_router::bounded |  |
| search_router.answer | search_router::answer |  |
| tinyfish.api_key | tinyfish::api_key |  |
| tinyfish._call | tinyfish::call |  |
| tinyfish.search | tinyfish::search |  |
| tinyfish.fetch | tinyfish::fetch |  |
| watch.WatchConfig | watch::config::WatchConfig |  |
| watch._number | watch::config::number |  |
| watch._browser_on | watch::config::browser_on |  |
| watch._tls_on | watch::config::tls_on |  |
| watch.configured | watch::config::configured |  |
| watch.RateLimiter | watch::limiter::RateLimiter |  |
| watch.Reading | watch::page::Reading |  |
| watch.FetchError | watch::FetchError | architect-complete; with_status / with_retry / with_host / at |
| watch._price | watch::page::price |  |
| watch._availability | watch::page::availability |  |
| watch._ld_blocks | watch::page::ld_blocks |  |
| watch._drop_comments | watch::page::drop_comments |  |
| watch._drop_blocks | watch::page::drop_blocks |  |
| watch._ld_values | watch::page::ld_values |  |
| watch._Microdata | watch::page::microdata | the HTMLParser subclass is a private tokenizer inside microdata |
| watch.microdata | watch::page::microdata |  |
| watch._attrs | watch::page::attrs |  |
| watch._offers | watch::page::offers |  |
| watch._typed_name | watch::page::typed_name |  |
| watch._any_name | watch::page::any_name |  |
| watch.structured | watch::page::structured |  |
| watch.visible_text | watch::page::visible_text |  |
| watch.main_text | watch::page::main_text |  |
| watch.phrase_on | watch::page::phrase_on |  |
| watch._ip_public | watch::net::ip_public |  |
| watch._local_networks | watch::net::local_networks |  |
| watch._public | watch::net::public |  |
| watch._connect_public | watch::net::connect_public |  |
| watch._PinnedHTTP | watch::net::pinned_addrs + reqwest dns_resolver | the pinned connection is reqwest blocking with a resolver that answers only the checked addresses |
| watch._PinnedHTTPS | watch::net::pinned_addrs + reqwest dns_resolver | as _PinnedHTTP; TLS SNI and certificate checks stay on the host name |
| watch._PinnedHTTPHandler | watch::net::http_request | folded into http_request |
| watch._PinnedHTTPSHandler | watch::net::http_request | folded into http_request |
| watch.check_url | watch::net::check_url |  |
| watch._origin | watch::net::origin |  |
| watch._GuardedRedirects | watch::net::check_redirect, redirect_headers | redirects are followed by hand in http_request |
| watch._retry_after | watch::net::retry_after |  |
| watch.http_request | watch::net::http_request |  |
| watch._exchange | watch::tls (src/watch/tls_helper.py) | one curl_cffi exchange per hop, run by the helper; boundary |
| watch.tls_available | watch::tls::tls_available |  |
| watch._pin | watch::tls::pin |  |
| watch.tls_request | watch::tls::tls_request |  |
| watch.tls_worth | watch::tls::tls_worth |  |
| watch.fetch_hosted | watch::hosted::fetch_hosted |  |
| watch.overlay_choice | watch::render::overlay_choice |  |
| watch._page_of | watch::render::page_of |  |
| watch._clear_overlays | watch::render::clear_overlays |  |
| watch.render | watch::render::render |  |
| watch._GuardProxy | watch::render::GuardProxy | start / shutdown / Drop |
| watch._pipe | watch::render::pipe |  |
| watch._ProxyHandler | watch::render::proxy_handle | one connection per thread |
| watch._relay_one_way | watch::render::relay_one_way |  |
| watch._descendants | watch::render::descendants |  |
| watch._kill_tree | watch::render::kill_tree |  |
| watch._render_here | watch::render::render_here |  |
| watch._render_watchdog | watch::render::render_watchdog |  |
| watch._kill_tree_below | watch::render::kill_tree_below |  |
| watch._render_main | watch::render::render_main, bin eve-watch --render |  |
| watch.openrouter | watch::model::openrouter, openrouter_ask | openrouter_ask is the Ask closure over ctx |
| watch._model_options | watch::model::model_options |  |
| watch._json_object | watch::model::json_object | architect-complete |
| watch._reply_text | watch::model::reply_text | architect-complete |
| watch._plain_link | watch::model::plain_link | architect-complete |
| watch.model_reading | watch::model::model_reading |  |
| watch.Watch | watch::rules::Watch | `ref` is `ref_` (serde rename "ref") |
| watch.quote_reading | watch::rules::quote_reading |  |
| watch.plain_note | watch::rules::plain_note |  |
| watch.money | watch::rules::money |  |
| watch.describe | watch::rules::describe |  |
| watch.evaluate | watch::rules::evaluate |  |
| watch.now_line | watch::rules::now_line |  |
| watch._takes_about | (none) | the Rust Notify seam always takes `about`; nothing to inspect |
| watch._alert_about | watch::rules::alert_about |  |
| watch._hours | watch::rules::hours |  |
| watch._when | watch::rules::when |  |
| watch._stretch | watch::rules::stretch |  |
| watch._every_words | watch::rules::every_words |  |
| watch.Watcher | watch::watcher::Watcher (+ chat tools in watch::chat) |  |
| watch._iso | watch::ticketmaster::iso |  |
| watch._tribute_words | watch::ticketmaster::tribute_words |  |
| watch._tribute | watch::ticketmaster::tribute |  |
| watch._sports | watch::ticketmaster::sports |  |
| watch._norm | watch::ticketmaster::norm |  |
| watch._tm_events | watch::ticketmaster::tm_events |  |
| watch.pick_attraction | watch::ticketmaster::pick_attraction |  |
| watch.ticketmaster_reading | watch::ticketmaster::ticketmaster_reading |  |
| watch.make_watcher | watch::watcher::make_watcher |  |
| web_reader.ReaderConfig | web_reader::ReaderConfig |  |
| web_reader.configured | web_reader::configured, configured_shipped | the SHIPPED seam |
| web_reader.search_and_read | web_reader::search_and_read |  |
| web_reader._urls_in | web_reader::urls_in |  |
| web_reader.answer_parts | web_reader::answer_parts |  |
| web_reader.answer_from | web_reader::answer_from |  |
| web_reader._take_task | web_reader::take_task | counts are Ctx.reader_tasks (TaskCount) |
| web_reader.WebReaderAgent | web_reader::WebReaderAgent | impl eve_app_reflex::Agent |
| websearch.SearchConfig | websearch::SearchConfig |  |
| websearch.configured | websearch::configured |  |
| websearch.tools_for | websearch::tools_for |  |
| websearch.routed_tool | websearch::routed_tool |  |
| websearch._clock | websearch::clock |  |
| websearch.answer_options | websearch::answer_options |  |
| websearch.request | websearch::request |  |
| websearch.parse | websearch::parse |  |
| websearch.search | websearch::search |  |
| websearch.engine_search | websearch::engine_search |  |
| websearch._engine_once | websearch::engine_once |  |
| search_router.Predict | eve_decider::Predict | Jev's predict seam, `Fn(&Value, &Value) -> Result<Value, PredictError>` |
| search_router.TINYFISH, PERPLEXITY, FIRECRAWL, PROVIDERS, MODES, JEV_TIMEOUT_S, READER_ROUTED_SECS, OPTIONS, OPTION_TO_PROVIDER, PROVIDER_TO_OPTION, ABOUT, GRID, MAX_GRID, GATES, SHIPPED, NOT_FOUND, READER_SYSTEM, _LINK | same names (`provider_to_option`, `LINK`) | verbatim |
| search_router._JEV, _JEV_LOCK | private `JEV` (a `parking_lot::Mutex`) | remade when the route's (url, model) changes, as Python |
| websearch.URL, ENGINES, OPENROUTER_ENGINES, DEFAULT_*, HEDGE_SECS, FALLBACK_SECS, FALLBACK_ENGINE, MAX_*, SEARCH_USD, TOOL_NAME, WEB_SEARCH_TOOL, HOSTED_TOOL, ROUTED_REACH, ROUTED_NOTE, SYSTEM, CLOCK, MONID_RESERVE_SECS, MONID_HTTP | same names (`openrouter_engine`, `search_usd` for the dict lookups) | verbatim |
| urllib.request.urlopen (websearch's opener) | websearch::SystemOpener (`Opener` trait; `OpenRequest`, `OpenError`) | a blocking reqwest POST; tests pass a closure |
| search's / engine_search's keyword arguments | websearch::EngineOpts | `hedge_secs` is the HEDGE_SECS seam the tests patch |
| via's / answer's keyword arguments | search_router::AnswerOpts | |
| bounded's `work` | search_router::Work, Abandoned | |
| watch.THREE_DECIMALS, _NUMBER, _SIGNED, _LD_TYPE, _META_RE, _ATTR_RE, _FRAMING_RE, _VOID, MICRODATA_PROPS, OFFER_SCOPES, NOT_THE_ITEM, ITEM_TYPES, _MAIN_OPEN | watch::page constants | patterns verbatim |
| watch._BLOCK_OPEN, _drop_blocks | watch::page::drop_blocks | a linear scanner in place of the cached lookahead regexes |
| watch._OPEN_SOCKETS | (none) | the deadline is reqwest's per-hop timeout over what is left, and a sized-body cut |
| watch._LOCAL_NETS, LOCAL_NETS_SECS, _local_networks | watch::net::local_networks, parse_ifconfig, LOCAL_NETS_SECS | `ifconfig`, cached 60 s |
| watch._PinnedHTTP, _PinnedHTTPS | watch::net::pinned_addrs + reqwest `dns_resolver` | the resolver answers only the checked addresses; SNI and certificates stay on the host name |
| watch._PinnedHTTPHandler, _PinnedHTTPSHandler | watch::net::http_request | folded into it |
| watch._REDIRECT_SAFE_HEADERS | watch::net::REDIRECT_SAFE_HEADERS | |
| urllib.parse.urlsplit | watch::net::urlsplit, SplitUrl, BadUrl | Python 3.13 semantics, `.hostname`, `.port`, `.username`, `.password` |
| watch.TLS_IMPERSONATE, TLS_MAX_HOPS, TLS_REDIRECTS, TOO_LONG | watch::tls constants | |
| watch.OVERLAY_OK, OVERLAY_NEVER, _OVERLAY_JS, OVERLAY_SETTLE_MS, OVERLAY_LOOKS | watch::render constants | verbatim |
| watch.RENDER_TIMEOUT_SECS, RENDER_SETTLE_SECS, RENDER_VIEWPORT, RENDER_DEADLINE_PAD_SECS, PROXY_IDLE_SECS | watch::render constants | |
| `if __name__ == "__main__"` | src/bin/eve-watch.rs | `--render` only, as Python's |
| watch.Notify | watch::watcher::Notify | always called with `about` (the text itself when the caller had none) |
| watch.Watcher.__init__ | Watcher::new | |
| watch.Watcher.watches | Watcher::watches (shared handles), snapshot (copies), push (`watches.append`) | a watch is a `WatchRef` (`Arc<Mutex<Watch>>`); identity is `Arc::ptr_eq` |
| watch.Watcher.load_problem | Watcher::load_problem | |
| watch.Watcher._load | Watcher::load | the file is read by `watcher::pyjson::loads` (Python's json.loads: NaN, Infinity, huge numbers, lone surrogates) |
| watch.Watcher.save | Watcher::save | `watcher::pyjson::dumps_indent1`: `json.dump(indent=1)`, ensure_ascii, floats by repr |
| watch.Watcher.host | Watcher::host | |
| watch.Watcher._next | Watcher::next | |
| watch.Watcher._call | Watcher::call | `spawn_blocking` when `offload`, else inline; a panic in the call goes on unwinding in the caller |
| watch.Watcher.read | Watcher::read | |
| watch.Watcher._read_quote | Watcher::read_quote | |
| watch.Watcher._model_budget | Watcher::model_budget | |
| watch.Watcher._missing | watch::watcher::missing | |
| watch.Watcher._merge | watch::watcher::merge | |
| watch.Watcher._read_page | Watcher::read_page | |
| watch.Watcher._crawl_on | Watcher::crawl_on | |
| watch.Watcher._read_crawled | Watcher::read_crawled | |
| watch.Watcher._fetch_page | Watcher::fetch_page | |
| watch.Watcher._from_html | Watcher::from_html | |
| watch.Watcher._ask_text | Watcher::ask_text | |
| watch.Watcher._read_rendered | Watcher::read_rendered | |
| watch.Watcher._read_search | Watcher::read_search | |
| watch.Watcher._tm_get | Watcher::tm_get | TICKETMASTER_API_KEY from the watcher's ctx |
| watch.Watcher._read_ticketmaster | Watcher::read_ticketmaster | |
| watch.Watcher.check | Watcher::check | |
| watch.Watcher._tell | Watcher::tell | |
| watch.Watcher._kept | Watcher::kept | public (the tests' seam) |
| watch.Watcher._run_one | Watcher::run_one | public (Python's tests call it) |
| watch.Watcher.tick | Watcher::tick | |
| watch.Watcher.run | Watcher::run | |
| watch.Watcher.kinds | Watcher::kinds | |
| watch.Watcher.instructions | Watcher::instructions | |
| watch.Watcher.tools | Watcher::tools | |
| watch.Watcher.handle | Watcher::handle | |
| watch.Watcher._find | Watcher::find | |
| watch.Watcher._no_such | Watcher::no_such | |
| watch.Watcher.pause | Watcher::pause | |
| watch.Watcher.resume | Watcher::resume | |
| watch.Watcher.set_end | Watcher::set_end | |
| watch.Watcher._summary | Watcher::summary | |
| watch.Watcher.remove | Watcher::remove | |
| watch.Watcher._new_id | Watcher::new_id | |
| watch.Watcher.validate | Watcher::validate | `Result<Watch, String>` for Python's `(watch or None, why)` |
| watch.Watcher.add | Watcher::add | |
| watch.Watcher.listing | Watcher::listing | |

Notes:
- `rules::py` (crate-private) holds the Python semantics the watch rules lean on: `str.split`/`strip` whitespace,
  truthiness, `str()`/`repr()` of a JSON value, float `repr`, `format(x, "g")`, `round(x, n)`, `"{:,.Nf}"` and
  `datetime.fromtimestamp`. `rules::price_of` is `_price` for a value that may be absent.
- `watcher::pyjson` (crate-private) is Python's `json.loads` (NaN, Infinity, numbers past a float, lone surrogates)
  and `json.dump(indent=1)` with ensure_ascii. The store and the model's JSON (`model::json_object`) are read with it.
- Seams added where Python's tests monkeypatched a module function: `WatcherDeps` (fetch, ask, clock, sleep, rng,
  limiter, offload, resolve, render, tls, crawl, evaluate), `search_router::Paths` / `AnswerOpts`,
  `websearch::EngineOpts` (the HEDGE_SECS seam), `web_reader::ReaderOptions`, `HttpOptions.rules` /
  `AddressRules` (check_url's and `_ip_public`'s seams), `tls_request_with`.

## Tests
`cargo test -p eve-web`: 217 integration tests in five files plus 25 unit tests in `src/watch/`. 4 are ignored: 3
need a live headless Chromium (`#[ignore = "live: ..."]`) and 1 is the page-reader differential (below).

| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_search_router.py | 24 | tests/search_router.rs, 22 under the same names without `test_` (parametrized cases looped in one test), plus fit_refuses_answers_and_truths_of_different_lengths, keep_read_links_keeps_a_read_link_before_its_punctuation_and_collapses_whitespace, the_reader_hears_not_found_as_no_answer, bounded_returns_the_work_or_its_failure, a_firecrawl_route_runs_the_one_tool_the_model_picks_and_answers_from_it, usage_and_result_read_the_answer_models_calls | test_the_eval_fits_on_every_seen_set_and_scores_the_fresh_holdout_once, test_the_printed_verdict_is_exactly_the_pre_registered_bar: they test tools/route_eval.py, a Python evaluation tool outside the daemon |
| test_websearch.py | 13 | tests/search_router.rs, all 13 under the same names without `test_`, plus an_engine_search_writes_one_spend_line | none |
| test_web_reader.py | 21 | tests/reader_providers.rs, 20 under the same names (parametrized cases looped), plus 15 added (firecrawl_daily_cap_is_shared_counted_and_survives_a_restart, firecrawl_per_day_reads_numbers_as_python_does, a_spent_day_never_reaches_firecrawl, firecrawl_request_bodies_are_pythons_json, a_tool_that_needs_an_id_looked_up_first_is_not_offered, pick_tool_reads_a_number_given_as_text_and_only_plain_values, run_tool_reports_why_it_failed, meter_records_credits_priced_or_not, tinyfish_search_keeps_only_web_links_and_quotes_the_query, tinyfish_unreadable_answers_are_errors_from_tinyfish, tinyfish_fetch_keys_results_and_errors_by_url, urls_in_reads_string_values_only, answer_parts_lists_the_tools_or_the_tool_data_and_reads_only_page_sources, the_task_cap_keeps_the_last_seven_days, status_steer_and_cancel_forward_to_codex_once_it_took_over) | test_the_daemon_routes_jev_first_then_the_chrome_rule: it drives `Daemon._bodies` (daemon.py), eve-daemon's to port and test |
| test_watch.py | 29 | tests/watcher.rs, all 29 under the same names, plus test_make_watcher_builds_the_daemons_watcher_when_on, test_watch_list_and_an_unknown_tool; the pure halves also in tests/watch_rules.rs (the limiter, conditions, currency, quotes, Ticketmaster, config) | none |
| test_watch_lean_conditions.py | 8 | tests/watch_rules.rs, all 8 | none |
| test_watch_lean_limiter.py | 7 | tests/watch_rules.rs (5) and tests/watcher.rs (test_a_stale_gate_cannot_shorten_the_hosts_backoff, test_no_read_goes_to_a_host_inside_its_hold: Python lends a `_call` that holds the first read; here the read runs off the loop and a gate holds it, the same interleaving) | none |
| test_watch_lean_scheduler.py | 6 | tests/watcher.rs, all 6 | none |
| test_watch_lean_ticketmaster.py | 4 | tests/watch_rules.rs, all 4 | none |
| test_watch_reverify.py | 34 | tests/watcher.rs, 30 (2 live-ignored: test_negative_control_the_guard_test_sees_a_leak_when_the_guard_is_open, test_an_overlay_button_that_navigates_is_undone). test_the_render_child_dies_with_its_parent runs the watchdog in a child process (this test binary again): Python patched `os._exit` and `_kill_tree_below`; here the child must exit 3 and its own child must be gone | test_tell_owner_puts_only_the_about_line_in_the_history, test_a_refused_alert_reports_not_sent, test_watch_with_a_bot_suffix_lists_and_a_watch_request_skips_an_open_launch_tree (telegram.py's TelegramInlet: eve-telegram's); test_a_watcher_that_cannot_start_costs_the_watching_not_the_daemon (daemon.py: eve-daemon's) |
| test_watch_security.py | 11 | tests/watch_pages.rs (8: the readers, the guarded HTTP read, the render guard, live-ignored) and tests/watcher.rs (test_one_watch_whose_read_crashes_never_starves_the_others, with a panicking fetch for Python's LookupError; test_a_page_alert_links_only_the_page_the_owner_gave; test_a_search_note_cannot_hide_a_link_behind_words, which composes the alert with eve-telegram-format, a dev-dependency) | none |
| test_watch_shapes_replay.py | 11 | tests/watch_pages.rs (6, the page readers) and tests/watcher.rs (test_one_page_with_a_huge_price_does_not_starve_the_other_watches, test_a_page_that_never_states_its_price_is_reported_not_silent, test_money_never_prints_scientific_notation, test_the_huge_price_in_a_model_answer_is_dropped_not_raised for the model_reading half of test 1) | test_a_watch_command_reaches_eve_while_the_claude_relay_is_on, test_a_watch_command_reaches_eve_while_the_codex_relay_is_on: Telegram's `_handle` (eve-telegram's) |
| test_watch_tinyfish.py | 14 | tests/watch_pages.rs (the 5 fetch_hosted tests, `hosted_` prefix) and tests/watcher.rs (the 8 watcher tests under the same names, and test_the_switches as test_the_hosted_switches) | none |
| test_watch_tls.py | 17 | tests/watch_pages.rs (11, the `tls_` guards over loopback servers, skipped as Python's `importorskip` when the interpreter has no curl_cffi, and which_failures_are_worth_the_chrome_like_read) and tests/watcher.rs (the 5 ladder tests under the same names, and test_the_switch as test_the_tls_switch) | none |

Other test notes:
- Fixtures: `monkeypatch` of env, `spend.record`, `watch.http_request`, `check_url`, `_ip_public`, `w._fetch`,
  `w._tls`, `w._crawl`, `w._render`, `w._ask` become a per-test `Ctx` (its own environment, spend ledger folder,
  Firecrawl count file, task count) and swappable fakes lent through the seams above. `pytest.fail` inside a fake
  that must not run is a panic (or a flag the test asserts, where a worker thread would catch it).
- test_this_macs_own_ipv6_prefixes_are_the_lan: Python patches `IPv6Address.is_global` so 2001:db8:: is global; a
  real global prefix (2606:4700:88c0::/64) stands in. test_a_ticketmaster_price_past_a_float_is_no_price: serde_json
  holds no 1e400, so f64::MAX and 1e300 stand in (both past MAX_PRICE).
- Differential: `the_page_readers_match_python` (ignored; needs `EVE_WATCH_PAGES_DIFF`) compares `structured`,
  `main_text`, `visible_text` and `microdata` with Python's on generated pages. `tests/gen_watch_pages.py` makes
  the case file with the bridge's Python. At integration (2026-10-20) it matched on 6,500 fresh pages (seeds 3, 5,
  9), and a changed expectation made it fail; the watch-pages unit had matched 16,000 pages from its own generator.

## Boundaries
- **Chrome-like TLS read** (`tls_request`, `_exchange`). No Rust crate gives Chrome's TLS fingerprint on this Mac:
  rquest and wreq build BoringSSL, which needs cmake, and this machine has none. So one exchange per hop runs in
  curl_cffi, in `src/watch/tls_helper.py`, compiled in with `include_str!` and run as `python -I -c`. The helper
  prints `{"ready": true}` once curl_cffi has imported, then takes one JSON hop per line (url, the pinned
  `host:port:ip`, seconds left, max_bytes, impersonate) and answers with one JSON line. The URL checks, the pin,
  redirects, the deadline and the check on the address libcurl used stay in Rust. The interpreter is
  `EVE_WATCH_TLS_PYTHON` (Rust only), else the bridge checkout's `bridge/.venv/bin/python3`, else `python3`;
  `tls_available` asks it whether curl_cffi is importable, as Python's `_tls_on` auto does.
- **Render.** The child process is the `eve-watch` bin (`--render URL TIMEOUT`, Python's `python -m
  eve_bridge.watch --render`). It sits beside the running executable, or one folder up from a test binary in
  `deps/`, else it is found on PATH. In place of Playwright it drives headless Chromium over CDP through
  eve-browser-lane's driver (`find_chromium`, `CdpDriver`, a throwaway profile), with `--proxy-server` pointed at
  the guard proxy and `--proxy-bypass-list=<-loopback>`.

## Differences

### Search and the reader
- A routed attempt (or a router) that panics is `{"ok": False, "reason": "panic"}` (`"<provider>: panic"` when
  unwrapped), where Python names the exception's class; a Rust panic has no class.
- Shapes Python fails on with an AttributeError or OverflowError that escapes (a reply whose `choices[0]` or
  `message` is a truthy non-dict, token counts past a float's range in `parse`; a Jev answer whose `answers` or a
  question's entry is not a dict) are read as an unreadable reply (`parse`) or as no answer (`ask`: perplexity)
  instead of raising through `search` / the router. The route and the owner's reason are the ones Python's
  callers end up with; only the log line differs.
- `_usage` counts a token field it cannot read as an int (`"abc"`, a list) as 0 where Python raises ValueError.
- The engines race on two `std::thread`s instead of a ThreadPoolExecutor; as in Python, they do not carry the
  caller's spend job (an executor thread starts with an empty context), while `bounded`'s worker does
  (`eve_spend::carry`, Python's `contextvars.copy_context().run`).
- `SystemOpener` follows redirects as reqwest does; urllib refuses a 307/308 on a POST. OpenRouter does not
  redirect this endpoint.
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

### Watch: config, limiter, rules, Ticketmaster
- `browser_on`'s auto: on when eve_browser_lane's `find_chromium(true)` finds a headless Chromium (Playwright's
  cache, else an installed Chrome or Chromium), where Python asked whether Playwright is importable. The render
  child drives that Chromium over CDP, so this is the same question for the Rust daemon. Its warning names the
  missing Chromium rather than the pip extra.
- `openrouter`'s body is serde_json's compact JSON (non-ASCII as UTF-8), not `json.dumps` (", " separators,
  `ensure_ascii`); the same JSON value reaches OpenRouter.
- `Watch::to_json` writes every float field as a float: a `value` Python held as the int 300 is written `300.0`.
  Both daemons read either back the same way (`from_json` makes every number field a float, every counter an int).
- `_iso`: Python 3.11's `fromisoformat` grammar is followed for calendar dates (extended and basic), any one
  separator, `HH[:MM[:SS[.f]]]` (and basic), and offsets; ISO week dates (`2026-W40-1`) and sub-second offsets
  read as no time. Ticketmaster sends neither.
- A JSON value of the wrong shape where Python would raise AttributeError (a truthy non-object `dates`, `sales`,
  `segment` or `currentTradingPeriod`; a non-list `aliases` or `classifications`) reads as empty here.
- `expand_user("~nosuchuser/…")` leaves the path as it is; Python 3.13's `Path.expanduser` raises RuntimeError.
- `local_day` of a time no calendar can place (Python raises) is "".

### Watch: pages, HTTP, TLS, render
- **JSON numbers no float can hold.** Python keeps an integer past f64 (or `1e400`, `NaN`, `Infinity`) as an int
  or float, and later rules turn it into None. Here it is kept as the string "Infinity", "-Infinity" or "NaN". No
  price, availability or name rule accepts those strings, so the reading is the same (the 16,000-page
  differential agrees).
- **Tag names and attribute names.** These are lowercased ASCII-only. Python's `str.lower()` also folds non-ASCII
  letters in a tag name. No real tag or attribute name has those letters.
- **Render: service workers.** Playwright's `service_workers="block"` has no counterpart in eve-browser-lane's
  driver. Service workers stay on. Every connection they make still goes through the guard proxy, so the SSRF
  guard is unchanged.
- **Render: popups.** Python closes a popup the moment it opens (`context.on("page")`). Here a popup is closed by
  a sweep after `goto` and again after the overlays: every page but the one marked as the one being read is
  closed. Until the sweep, a popup can load, but only through the guard proxy.
- **Render: response status.** Python reads `response.status` from `page.goto`. Here it comes from the Navigation
  Timing entry (`responseStatus`). When there is no entry, 0 is read, which works like Python's `response is
  None`.
- **Render: unexpected errors.** A failure that is not a FetchError, a browser error or an OSError reads as "the
  browser failed (panic)". Python names the exception class there.
- **Render answer line.** `render_main` writes it with serde_json, so non-ASCII text is not `\u`-escaped as
  `json.dumps` would escape it. The parent decodes both forms the same way.
- **Render: decoding the answer.** `render` decodes the shot with strict base64. Python's `b64decode` drops
  characters outside the alphabet. The child always writes valid base64, so the result is the same.
- **Render: the child's stdin.** The child inherits stdin, as Python's Popen does. When the child is killed at its
  deadline, the stdout reader thread is left to end by itself rather than joined.
- **Guard proxy: CONNECT ports.** A CONNECT port that `int()` takes but u16 does not (`4_43`, a port past 65535)
  is denied with 403. In Python it raises inside the handler and the connection just closes. Either way the
  connection is refused.
- **TLS helper start-up.** Importing curl_cffi happens before the deadline starts, as in Python, where the import
  comes first. A helper that is not ready within 30 s reads as "the Chrome-like reader is not installed". A hop
  that has no answer by its deadline plus a 3 s grace kills the helper and reads as "the site took too long to
  answer".
- **http_request: a sized body cut short.** A body shorter than its Content-Length counts as the end of the body,
  as http.client's `IncompleteRead` partial is returned by urllib's `read()`. A broken chunked body is "the site
  sent an unreadable answer".
- **http_request: charsets.** The charset families Python knows are mapped through encoding_rs. A charset neither
  side knows (`bogus-8`) is decoded as UTF-8 with replacement. Python raises LookupError there, and the security
  replay accepts either result.

### Watch: the watcher
- `reply_text`: a non-object choice reads as empty text, and non-string content reads as its JSON text.
- A reader or a rule that raises something other than FetchError: Python catches any `Exception` in `_run_one`, `add`
  and `run`. A Rust reader returns FetchError where Python's would raise one; what stands for the other exceptions is
  a panic, caught (`catch_unwind`) at the same three places with the same effect (an error counted, "an answer I could
  not make sense of", next_at stretched; a tick that panics waits 60 s). Answers Python trips on with AttributeError
  (a Yahoo `chart` or `meta` that is not an object, a model message that is a string) are FetchErrors here ("the quote
  service sent an unreadable answer"; the model reply reads as empty text): still this watch's counted error, with
  that reason as `last_error`.
- `WatcherDeps.evaluate` is a seam Python does not have (its tests monkeypatch the module function); the default is
  `rules::evaluate`.
- `_takes_about`: the Rust Notify always takes `about`, so `about or text` is always passed.
- The store is read with a json.loads equivalent: a number serde_json cannot hold (NaN, ±Infinity, past a float)
  reads as null, which `Watch::from_json` and `last_id` treat exactly as Python treats the number (the field keeps
  its default). A lone surrogate escape reads as U+FFFD, so a re-save writes `�` where Python wrote the
  surrogate back.
- An unreadable file's ids (`"w(\d{1,9})"`): a non-ASCII digit matches but is not counted (Python's `int()` reads
  it).
- `tick`: a watch whose time is up but that the chat removed during an earlier await of the same tick is skipped;
  Python's `list.remove` raises ValueError there and the tick fails (the loop logs it and waits 60 s).
- `_wake`: Python clears the event after each tick, so a set while no wait is pending is lost; `Notify::notify_waiters`
  stores no permit, the same.
- validate: Python's `float(value)` of an integer past a float raises OverflowError out of the tool call; serde_json
  has already read such a number as a float, so it is no positive value.
- `str(x or "")` of a list or dict argument is its JSON text, not Python's repr.
- Log lines name the error kind as Python does where a Python exception class is involved (JSONDecodeError,
  UnicodeDecodeError, PermissionError...); a notify failure is logged by its message.
- `_json_object` reads the model's JSON with `watcher::pyjson` (Python's json.loads), so a price of NaN, Infinity or
  an integer past a float is no price, as Python's `_price` makes it, rather than the whole answer being refused.
  Such a number in another field reads as null: Python would keep it (`currency: NaN` would become "NAN").
