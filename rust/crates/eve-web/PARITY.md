# eve-web
Status: partial

The routed web search, the web reader body and the watcher: six Python modules (4649 lines). The architect committed
the skeleton (every type, constant, prompt and signature). Five fill units port the bodies (see "Work split"). Each
unit records its tests, boundaries and differences in its own fragment under `parity/` (`parity/<unit>.md`).
Integration merges the fragments into the sections below and sets `Status: complete` after
`cargo test -p eve-web`, clippy and `python3 tools/check_port.py --crate web` pass.

## Work split
| Unit | Python | Rust files | Tests file | Python tests |
|---|---|---|---|---|
| search-router | search_router.py, websearch.py | src/search_router.rs, src/websearch.rs | tests/search_router.rs | test_search_router.py, test_websearch.py |
| reader-providers | web_reader.py, firecrawl.py, tinyfish.py | src/web_reader.rs, src/firecrawl.rs, src/tinyfish.rs | tests/reader_providers.rs | test_web_reader.py |
| watch-rules | watch.py 1-308, 1508-1926, 2821-2929 (config, limiter, model, Watch rules, Ticketmaster rules) | src/watch/config.rs, src/watch/limiter.rs, src/watch/model.rs, src/watch/rules.rs, src/watch/ticketmaster.rs | tests/watch_rules.rs | test_watch_lean_conditions.py, test_watch_lean_limiter.py, test_watch_lean_ticketmaster.py |
| watch-pages | watch.py 310-1505 (page reading, guarded HTTP, TLS read, hosted read, render) | src/watch/page.rs, src/watch/net.rs, src/watch/tls.rs, src/watch/tls_helper.py, src/watch/hosted.rs, src/watch/render.rs, src/bin/eve-watch.rs | tests/watch_pages.rs | test_watch_security.py, test_watch_tls.py, test_watch_shapes_replay.py, test_watch_tinyfish.py |
| watcher | watch.py 2023-2818, 2932-2941 (Watcher, make_watcher) | src/watch/watcher.rs, src/watch/chat.rs | tests/watcher.rs | test_watch.py, test_watch_lean_scheduler.py, test_watch_reverify.py |

Architect-complete (no unit edits them): `src/lib.rs` (Ctx, Http, SystemHttp, Clock, Ask, the clocks),
`src/watch/mod.rs` (section-1 constants, FetchError), `src/watch/tools.rs` (TOOLS, INSTRUCTIONS_BLOCK,
TICKETMASTER_LINE), and in `src/watch/model.rs` the bodies of `json_object`, `reply_text` and `plain_link`
(watch-rules owns the rest of that file and keeps those three).

Every function that read a module global in Python (`os.environ`, `watch.http_request`, spend's ledger, Firecrawl's
day count, web_reader's task count, Monid) takes a `&Ctx` first. Tests build their own `Ctx` instead of
monkeypatching.

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

## Boundaries
- Chrome-like TLS read (`tls_request`, `_exchange`): the Rust TLS-impersonation crates need cmake and BoringSSL,
  which this machine lacks. One exchange per hop runs in `src/watch/tls_helper.py` with curl_cffi, passed to the
  interpreter with `-c`. The interpreter is `EVE_WATCH_TLS_PYTHON`, else `bridge/.venv/bin/python3`, else `python3`.
  Address pinning and the redirect checks stay in Rust.
- Render (`render`, `_render_here`): the child is the `eve-watch` bin (`--render URL TIMEOUT`). It drives headless
  Chromium over CDP through eve-browser-lane's driver behind the GuardProxy, in place of Playwright.

## Differences
- `reply_text`: a non-object choice reads as empty text, and non-string content reads as its JSON text.
- Render: Playwright's `service_workers="block"` has no CDP equivalent in eve-browser-lane's driver (unit to confirm).
- JSON written with serde_json, not `json.dumps`: units record any `ensure_ascii` or float-format difference.

## Tests
Filled by integration from the unit fragments.
