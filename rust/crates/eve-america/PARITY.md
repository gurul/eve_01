# eve-america
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/america.py` | `src/lib.rs` (constants, `TOOL`, `INSTRUCTIONS` verbatim, config, the SSE parser, `America`, `make_america`, the HTTP transport) |
| `eve_bridge/america_browser.py` | `src/america_browser.rs` (`FORM`, `FIELD`, `NAME_JS` verbatim, the flow, `AmericaBrowser`, `make_browser`) |
| the Playwright surface `america_browser.py` touches (`page.route`, `page.on("request"/"response"/"requestfailed")`, `get_by_role`, `locator`, `fill`, `click`, `response.body()`) | `src/chrome.rs` (`ChromeLane` over eve-browser-lane's `BrowserLane`, its own CDP session on the lane's tab: `NetPage`, `ChromePage`, `ChromeLocator`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `AMERICA_URL`, `API_URL`, `TOOL_NAMES`, `OFF_REASON`, `MAX_QUESTION_CHARS`, `MAX_JURISDICTION_CHARS`, `MAX_STREAM_BYTES`, `MAX_ANSWER_CHARS`, `TOOL`, `INSTRUCTIONS` | same names | values and prompt text verbatim; `TOOL` is a `LazyLock<Value>`, `tools()` hands out copies |
| `class AmericaConfig` | `struct AmericaConfig { enabled, timeout_secs, transport }`, `AmericaConfig::on(enabled)` | `transport` is `enum Transport { Alexandria, Browser, Api }` with `as_str()`; default alexandria, timeout 60 s |
| `configured` | `configured(Option<&HashMap>) -> Result<AmericaConfig, InvalidTransport>` | same env vars (`EVE_AMERICA`, `EVE_AMERICA_TRANSPORT`); `None` reads the process environment; an unknown transport is `InvalidTransport` (same message) where Python raises `ValueError` |
| `chat_request` | `chat_request(question, jurisdiction) -> Value` | same body, key order kept (`preserve_order`) |
| `class _ProtocolError` | `struct ProtocolError(String)` (`thiserror`) | |
| `_official_source` | `official_source` (private) | `.gov` / `.mil` host test over eve-web's `urlsplit` (Python 3.13's) |
| `parse_stream` | `parse_stream(&[u8]) -> Result<(String, Vec<Source>), ProtocolError>` | `Source { title, url, currency }`, `to_value()` |
| `_failure` | `failure` (private) | same dict; `retrieved_at` is `datetime.isoformat()` with `+00:00` |
| `class America` | `struct America` | `America::new(config)`, `with_transport` / `with_browser` / `with_index` are the constructor's keyword seams; `has_index`, `has_browser` |
| `America.__init__`, `tools`, `instructions`, `handle`, `close` | `America::new`, `tools`, `instructions`, `handle(name, &args) -> anyhow::Result<Value>`, `close` | the httpx client is `trait ChatTransport` (`HttpTransport` on reqwest: no redirects, connect 10 s, read 15 s, the total deadline as an outer timeout); the index is `trait GovIndex` (impl for eve-america-index's `GovernmentIndex`), the browser `trait AskBrowser` |
| `_answered` | `answered` (private) | |
| `make_america` | `make_america(Option<&HashMap>) -> anyhow::Result<Option<America>>`, `index_budget(config)` | the Firecrawl key through `eve_web::firecrawl::api_key`; the index budget is `max(0.1, timeout - 5)`; `None` reads the process environment, as Python's `os.environ` |
| `class BrowserFailure` | `struct BrowserFailure(String)`; `struct BrowserTimeout` for the `asyncio.TimeoutError` the worker budget raises | |
| `class Recipe` | `struct Recipe` (`css`, `role`), `Recipe::locate` | |
| `class AmericaBrowser` | `struct AmericaBrowser` | `new(lane)`, `with_options(...)` (the constructor's keywords), `lane_config`, `timeout_secs`, `last_metrics`, `field_recipe`, `send_recipe`, `remember_chat_id`, `inflight_running`; `impl AskBrowser` |
| `AmericaBrowser.__init__`, `_unique_visible`, `_recover`, `_controls`, `_target`, `_correlate`, `_reset`, `_ask`, `ask`, `close` | `with_options`, `unique_visible`, `recover`, `controls`, `target`, `correlate`, `reset`, `ask_sync` / `flow`, `ask`, `close` | the flow runs on the lane's thread as a `Job`; clock calls in Python's order; a cancelled or timed-out question stops the worker (`StopOnDrop`) and the browser is quarantined as in Python |
| the Playwright objects the flow uses | `trait ChatPage`, `Locator`, `ChatRequest`, `PageResponse`, `Route`, `enum PageEvent`, `trait LaneWorker`, `trait AmericaLane` | tests drive them with fakes, as the Python tests drove fakes |
| `make_browser` | `make_browser(Option<&HashMap>)`, `make_browser_with(environ, make_asker)` | None unless the browser lane attaches (`config.attach`); `EVE_AMERICA_JEV` (default `"1"`) feeds `EVE_JEV_STEP` for `make_step_asker` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `test_america.py` | all 22 | `src/tests/america.rs`, same names | |
| `test_america_browser.py` | all 17 | `src/tests/browser.rs`, same names | |
| `test_america_alexandria.py` | all 5 | `src/tests/alexandria.rs`, same names | the Telegram menu/call-synthesis halves of `test_real_index_bundle_reaches_menu_and_call_synthesis` belong to the telegram crate; the America side (tool result shape the menu reads) is asserted here |
| `test_america_source_guard.py` | both | `src/tests/source_guard.rs`, same names | |
| `test_america_integration.py` | | | all 6 test the daemon's and Telegram brain's registration of the service (`daemon.py`, `telegram_bridge.py`); they belong to those crates' ports |
| `test_america_menu.py` | | | all 10 test the Telegram `/america` command and prompt flow; they belong to the telegram crate's port |
| `test_america_index.py` | | | eve-america-index's tests |
| (parity additions) | | `test_http_transport_posts_the_headers_and_streams_the_reply`, `test_http_transport_does_not_follow_a_redirect`, `test_parse_stream_reports_why_without_the_body`, `test_closed_browser_refuses_new_questions`, `test_a_whole_question_past_its_budget_is_a_timeout_and_stops_the_worker`, `chrome::exact_pattern_escapes_the_fetch_wildcards`, `chrome::chain_json_names_each_step` | |
| (live, `#[ignore]`, run with `-- --ignored`) | | `test_live_flow_sends_once_through_the_route_and_reads_the_finished_stream`, `test_live_restored_history_is_aborted_before_it_leaves_chrome`, `test_live_jev_recovers_renamed_controls_by_role_and_name`, `test_live_a_stream_without_charset_reads_as_playwright_reads_it` | they launch Playwright's headless Chromium against a loopback copy of the form; all 4 pass |

## Boundaries
- **Playwright is replaced by the Chrome DevTools Protocol.** The page comes from eve-browser-lane's `BrowserLane` (owner's or eve's own Chrome, same config). `chrome.rs` opens a second flattened session on the lane's tab: `Fetch.enable` with the exact API URL pattern is `page.route` (`Fetch.continueRequest` / `Fetch.failRequest("Failed")` are `route.continue_()` / `route.abort()`), `Network.*` events are `page.on("request"/"response"/"requestfailed")`, and `Network.getResponseBody` / `getRequestPostData` are `response.body()` / `request.post_data`. The session is attached for each question and detached when it ends, so the idle tab's protocol events never queue up between questions. This needed a small additive upstream edit in eve-browser-lane (event subscription, the tab's target id and connection, `PageHandle::as_any`), recorded in its PARITY.md.
- `get_by_role(role, name=)` is an in-page locator (`LOCATE_JS`): implicit and explicit roles, accessible name from `aria-labelledby` / `aria-label` / labels / text / `title` / `placeholder`, visibility, strictness; the matched element is marked with a temporary `data-eve-america` attribute so eve-browser-lane's actionable `fill` / `click` act on it.
- Python's `import playwright` availability check (None when the optional extra is missing) has no counterpart: the CDP driver is always built in, so only `config.attach` decides.
- Jev's step answer is eve-desktop-lanes' `StepAsk` with eve-typed-ask's `JEV_STEP_GATES` (risky zeroed for the send step, as Python does).

## Differences
- JSON frames are parsed with serde_json, which refuses `NaN` / `Infinity`, lone surrogates and nesting deeper than 128; Python's `json.loads` accepts them. Every such frame was already a protocol failure or a non-answer, so no answer changes.
- `handle` returns `anyhow::Result<Value>`: an error from the index or the browser that is not their own failure type comes back as `Err`, where Python lets the exception escape `handle`.
- The role locator approximates Playwright's accessible-name engine and does not pierce shadow DOM; america.gov's form has none.
- During `fill` / `click` the target carries a temporary `data-eve-america` attribute, removed right after.
- On failure Rust removes the route and listeners first and then resets the page; Python resets in `except` and removes them in `finally`, where a closed page makes the removal a swallowed error. The end state (no route or listener left, page reset) is the same.
- A `text/event-stream` reply without a charset is decoded by Chrome as windows-1252 and re-encoded as UTF-8, so non-ASCII text arrives garbled; Playwright's `response.body()` does the same, so the Python sees the same bytes (live test `test_live_a_stream_without_charset_reads_as_playwright_reads_it`).
- `test_source_guard_supplements_public_dns_check` asserts the host the DNS rule saw (`agency.gov`) where Python's patched `check_url` saw the URL; eve-web's `AddressRules.resolve` takes the host.
