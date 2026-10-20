# eve-browser-lane
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/browser_lane.py` | `src/browser_lane.rs` (the module: constants, pure functions, `Page` = `_Page`, `LaneCore` + `BrowserLane` = `BrowserLane`, `make_step_asker`) |
| the page scripts in `browser_lane.py` (`_IDENTITY_JS`, `_COLLECT_JS`, `_RECHECK_JS`, `_WEBMCP_JS`, `_CONSENT_JS`, `_FILL_FIELDS_JS`) | `src/js.rs`, byte for byte (a test checks each length and sha256 against Python's) |
| `playwright.sync_api` (what the lane touches of it) | `src/driver.rs` (the traits: `Driver`, `BrowserHandle`, `ContextHandle`, `PageHandle`), `src/cdp.rs` (the real driver: the Chrome DevTools Protocol) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_PROFILE`, `DEFAULT_CHROME_DIR`, `DEFAULT_OWN_CHROME_DIR`, `CHROME_APP`, `OWN_START_SECS`, `READ_WAIT_SECS`, `READ_POLL_SECS`, `READ_ENOUGH_CHARS`, `DEFAULT_DEBUG_PORT`, `CONSENT_TIMEOUT_MS`, `DEFAULT_VIEWPORT`, `MAX_CANDIDATES`, `SNAPSHOT_TIMEOUT_MS`, `NAV_READ_POLL_MS`, `NAV_TIMEOUT_MS`, `SETTLE_CAP_SECS`, `ENTER_NAV_SECS` | same names | same values |
| `KEYS` | `KEYS` (pairs), `key_name(name)` | |
| `SIGN_IN`, `WEB_WORDS`, `CONSENT_TEXT`, `CONSENT_REFUSE`, `CONSENT_CLOSE`, `EMAIL`, `ON_THE_MAC`, `IN_A_BROWSER` | same names (`LazyLock<Regex>`) | patterns verbatim, `re.I` as `(?i)`; `CONSENT_TEXT_PATTERN` is the source the page's own RegExp is built from |
| `STALE_PREFIX`, `BLOCKED_PREFIX` | same | |
| `_VOLATILE_NUMBER`, `_VOLATILE_TIME`, `_VOLATILE_RUN`, `_MONEY` | private statics | `_VOLATILE_NUMBER`'s lookahead via `fancy-regex` |
| `CHALLENGE_TITLES`, `CHALLENGE_TEXT`, `CHALLENGE_HEAD_CHARS`, `SOFT_CHALLENGE_TITLES`, `CHALLENGE_SETTLE_SECS`, `CHALLENGE_POLL_SECS` | same | |
| `ROLE_WORDS` | `ROLE_WORDS` (pairs) | |
| `ACCOUNTS_URL`, `PROFILE_LOOKUP_BUDGET_SECS`, `CHROME_UA`, `CHROME_BINARY`, `BROWSER_APPS`, `EVERYDAY_WORD_APPS`, `SECRET_FIELDS`, `MASK_COLOR` | same | |
| `_volatile_free` | `volatile_free` | |
| `_without_fragment` | `without_fragment(Option<&Value>)` | `str(url or "")` |
| `click_recheck` | `click_recheck(&Value, &Value) -> Vec<String>` | |
| `challenge_kind` | `challenge_kind` | |
| `browser_blocker` | `browser_blocker` | |
| `blocked_line` | `blocked_line` | |
| `consent_choice` | `consent_choice(&[Value]) -> Option<Value>` | |
| `class BrowserLaneConfig` | `struct BrowserLaneConfig` (+ `Default`) | |
| `configured` | `configured(Option<&HashMap>)` | `None` reads the process environment; same env vars (`EVE_BROWSER_ATTACH`, `EVE_BROWSER_OWN`, `EVE_BROWSER_STEEL`, `EVE_CHROME_DIR`, `EVE_CHROME_DEBUG_PORT`, `EVE_CHROME_PROFILE`, `EVE_OWN_CHROME_DIR`) |
| `fetch_accounts` | `fetch_accounts(url, cookie_header, timeout)` | blocking reqwest on the lookup thread (Python's urllib); a non-2xx reply is an error, as urllib raises |
| `accounts_in` | `accounts_in` | |
| `profile_for` | `profile_for(request, &IndexMap, default)` | |
| `_listening` | `listening(port)` | |
| `devtools_endpoint` | `devtools_endpoint(dir, port)`, `devtools_endpoint_with(dir, port, listening)` | the `_with` form is the seam the tests monkeypatched |
| `ensure_chrome_window` | `ensure_chrome_window()`, `ensure_chrome_window_with(run, sleep)` | `run` is the `subprocess.run` seam (`run_command`) |
| `owner_chrome_running` | `owner_chrome_running()`, `owner_chrome_running_with(run)` | |
| `ensure_owner_chrome` | `ensure_owner_chrome()`, `ensure_owner_chrome_with(run, sleep, wait_secs, settle_secs)` | |
| `class AttachError` | `struct AttachError(String)` (`thiserror`) | callers `downcast_ref::<AttachError>()` |
| `own_chrome_command` | `own_chrome_command` | |
| `sign_in_command` | `sign_in_command` | |
| `own_chrome_running` | `own_chrome_running(dir)`, `own_chrome_running_with(dir, run)` | |
| `quit_own_chrome` | `quit_own_chrome(dir, wait_secs)` | `Browser.close` over its own port (`cdp::close_browser_at`) |
| `_live_endpoint` | `live_endpoint(dir)`, `live_endpoint_with(dir, listening)` | |
| `start_own_chrome` | `start_own_chrome(dir, url)`, `start_own_chrome_with(dir, url, run, wait_secs, poll_secs, listening, running)` | |
| `is_web_goal` | `is_web_goal(goal, route_kind)` | |
| `is_browser_only_goal` | `is_browser_only_goal(goal, &[String])` | |
| `navigation_destination` | `navigation_destination(&str)`, `navigation_destination_value(Option<&Value>)` | non-strings are "" |
| `observed_site` | `observed_site(&str)`, `observed_site_value(Option<&Value>)` | |
| `snapshot_from_page` | `snapshot_from_page(&Value, seq, secs) -> Result<Snapshot>` | Python's `KeyError`/`ValueError`/`TypeError`/`AttributeError` on a malformed row come back as `LaneError`s of that kind |
| `outline_lines` | `outline_lines(&Snapshot, limit)`, `OUTLINE_LIMIT` = 80 | |
| `class _Page` | `struct Page` | `Page::new(page).with_clock/with_sleep/with_vault/with_webmcp_probe/with_options` = `_Page(page, clock=, sleep=, vault=, webmcp_probe=)` |
| `_Page._read_dom`, `_collect`, `settle_check`, `_raw_look`, `snapshot`, `_probe_webmcp`, `current_url`, `text_visible`, `focused`, `frontmost_pid`, `screen_changed`, `_locator`, `_blocked`, `_recheck`, `click_candidate`, `focus_and_type`, `fill_from_vault`, `_select`, `clear_consent`, `press`, `settle`, `open_url` | `Page::read_dom`, `collect`, `settle_check`, `raw_look`, `snapshot`, `probe_webmcp`, `current_url`, `text_visible`, `focused`, `frontmost_pid`, `screen_changed`, `locator`, `blocked_refusal`, `recheck`, `click_candidate`, `focus_and_type`, `fill_from_vault`, `select`, `clear_consent`, `press`, `settle`, `open_url`; `impl Senses`, `impl Effectors` | `p.approve` = `approve()`/`set_approve()`; `p.blocked` = `blocked()`; `p._snapshot` = `last_snapshot()` |
| `class BrowserLane` | `struct BrowserLane` (async, its own worker thread) over `struct LaneCore` (the state and the synchronous `_` methods) | `BrowserLane::new(config, LaneOptions{step_asker, launcher, vault})`; `from_core` for a prepared core |
| `BrowserLane._run` | `BrowserLane::run(f)` | one std thread runs every job, in order (the single-thread executor) |
| `_ensure`, `_profiles`, `_pick_profile`, `_use_profile`, `_alive`, `_close`, `_outline`, `_run_plan`, `_blocked_result`, `_page_text`, `_screenshot` | `LaneCore::ensure`, `profiles`, `pick_profile`, `use_profile`, `alive`, `close`, `outline`, `run_plan`, `blocked_result`, `page_text`, `screenshot` | |
| `connected`, `connect`, `use_profile`, `close`, `outline`, `run_plan`, `page_text`, `open_url`, `screenshot`, `plans_started` | `BrowserLane::connected`, `connect` / `connect_answering(prompt)`, `use_profile`, `close`, `outline`, `run_plan`, `page_text`, `open_url`, `screenshot`, `plans_started` | `outline` / `page_text` return `Outline` / `PageText` (Serialize, same keys as the dicts) |
| `_plain_http`, `_page_options`, `_fetch_accounts` and the module functions the tests monkeypatched | `LaneCore.plain_http`, `page_options`, `seams: LaneSeams` (`start_driver`, `devtools_endpoint`, `ensure_chrome_window`, `start_own_chrome`, `steel_start`, `steel_release`, `fetch_accounts`, `profiles`, `profile_lookup_budget_secs`) | |
| `Vault` as the lane uses it | `trait LaneVault` (impl for `eve_vault::Vault`) | |
| `make_step_asker` | `make_step_asker(Option<&HashMap>) -> Option<Box<dyn StepAsk>>` | `EVE_JEV_STEP`; Jev's route via `eve_jev::route_config` |
| (for eve-america's `america_browser.py`, which uses Playwright's `page.route` and `page.on`) | `cdp::Connection::subscribe()` (every protocol event, forwarded to an `mpsc` receiver), `CdpPage::target_id()`, `CdpPage::connection()`, `PageHandle::as_any()` (defaulted `None`; `CdpPage` returns itself) | additive: lets a caller open its own session on the lane's tab for request interception and network events; nothing in the lane itself changed |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `test_browser_lane.py` | all | `tests/browser_lane.rs` (same names; the 8 live ones `#[ignore = "live: ..."]`), `test_an_attach_that_fails_after_connecting_closes_the_driver` in `tests/attach.rs` | none. All live tests were run against Playwright's headless Chromium 1243 on 2026-10-20 and pass |
| `test_browser_attach.py` | all (parametrized cases folded into loops) | `tests/attach.rs`; the stand-in Chrome test in `tests/browser_lane.rs` (live, run and passing) | none |
| `test_browser_challenge.py` | all but the two agent tests | `tests/challenge.rs` | `test_the_agent_says_the_owner_line_and_never_hands_off`, `test_the_agent_stops_before_planning_when_the_outline_is_a_bot_check`: they test computer_agent's handling of a blocked result; that crate's port |
| `test_challenge_settle.py` | all | `tests/challenge.rs` | none |
| `test_browser_navigation_freshness.py` | all | `tests/navigation.rs`; the two live ones in `tests/browser_lane.rs` (run and passing) | none. The live ones serve the pages from a loopback server instead of Playwright's `page.route`, which the lane's driver does not have, so the host they assert is 127.0.0.1 |
| `test_click_recheck.py` | all | `tests/recheck.rs` | none |
| `test_click_recheck_tolerance.py` | all, including the node harness over the real scripts | `tests/recheck.rs` (node tests skip, with a line, when node is missing) | none |
| `test_webmcp_probe.py` | all | `tests/navigation.rs`; the live one in `tests/browser_lane.rs` (loopback pages, host 127.0.0.1; run and passing) | none |
| `test_vault_fill.py` | `test_the_page_refuses_before_touching_the_page_off_site_or_without_a_vault`, `test_live_fill_fills_each_form_and_no_value_leaves_the_page` | `tests/browser_lane.rs` (the live one run and passing, masked screenshot pixels included) | `test_a_fill_step_names_a_vault_handle_and_nothing_else`, `test_the_planner_is_told_the_vaults_lines_in_the_browser_only`, `test_the_executor_runs_a_fill_through_the_page_and_stops_on_a_refusal`: plan_contract and plan_executor, ported with those crates |
| `test_steel_local.py` | the lane's: `test_steel_is_off_unless_asked`, `test_steel_turns_the_lane_on_and_wins_over_own_and_attach`, `test_the_lane_attaches_to_the_steel_session`, `test_steel_not_reachable_is_an_attach_error_with_its_reason`, `test_closing_the_steel_lane_closes_eves_tab_and_releases_the_session`, `test_closing_eves_own_chrome_never_touches_steel`, `test_live_the_lane_reads_a_page_through_local_steel_starting_it_when_down` | `tests/attach.rs`; the live one in `tests/browser_lane.rs` (`#[ignore]`, needs the Steel checkout; not run) | the steel_local tests (eve-steel-local); `test_meet_keeps_eves_own_chrome_when_steel_drives_the_rest` (meet) |
| `test_chrome_lane.py` | `test_a_dead_connection_is_replaced_on_the_next_connect`, and `plans_started` | `tests/attach.rs` (`test_a_dead_connection_is_replaced_on_the_next_connect`, `test_plans_started_counts_every_plan`) | the rest test chrome_lane |
| `test_chrome_consent.py` | `test_the_phone_wait_ends_before_the_connection_gives_up` | `tests/attach.rs` | the rest test chrome_consent |
| `test_side_effect_links.py` | `test_browser_candidates_need_confirmation_by_label_and_by_href`, `test_the_browser_click_refuses_an_upvote_link_without_a_yes` | `tests/recheck.rs` | the label/href tables test ax_candidates (eve-desktop-lanes) |
| `test_side_effect_precision.py` | `test_the_browser_lanes_snapshot_carries_query_bearing_hrefs_to_the_gate` | `tests/recheck.rs` | the rest test ax_candidates / desktop helpers |
| `test_plan_url_expectation.py` | `test_real_browser_url_oracle_rejects_wrong_and_absent_navigation` | `tests/browser_lane.rs` (live, loopback pages; run and passing) | the rest test plan_contract / plan_executor |
| `test_app_reflex.py` | the `is_web_goal` lines | `test_open_it_after_a_shared_link_is_a_web_goal`, `test_web_goals_are_decided_by_code` | the rest test app_reflex |
| (parity additions) | | `the_page_scripts_are_pythons_byte_for_byte`, `snapshot_rows_fail_as_python_fails`, `volatile_free_folds_counts_and_relative_times`, `make_step_asker_follows_eve_jev_step`, `press_names_only_the_keys_a_plan_may_press`, `test_closed_lane_refuses_new_work`, and the `cdp` unit tests (call expressions, US-layout keys, cookie filtering) | |

## Boundaries
- **Playwright is replaced by the Chrome DevTools Protocol.** Python drives Chrome through `playwright.sync_api`; there is no Playwright for Rust, so `src/cdp.rs` speaks CDP over a WebSocket (tokio-tungstenite) and keeps what the lane relies on: `connect_over_cdp` to a browser WebSocket (the owner's Chrome, eve's own Chrome, Steel), `launch_persistent_context` of Playwright's own cached Chromium (its headless shell when headless) or the installed Google Chrome, with Playwright's default switches; contexts by identity; `page.url` from frame events; load states from lifecycle events per document; locator actions with Playwright's actionability waits (attached, visible, stable, enabled, hit-tested) and real mouse and keyboard input; dialogs dismissed; screenshot masks painted in the page. Verified by the live tests above against Chromium 1243.
- `steel_local.start_session` / `release` are eve-steel-local's async functions; the lane runs them on a thread of their own with a current-thread runtime (`LaneSeams::default`).
- Jev's typed step answer is eve-jev + eve-typed-ask; the plan contract and executor are eve-plan-contract and eve-desktop-lanes' `plan_executor`, unchanged on top.

## Differences
- A protocol call that never answers gives up after 30 s (`cdp::CALL_TIMEOUT`) with a `TimeoutError`; Playwright has no bound there.
- `networkidle` is Chrome's own `networkIdle` lifecycle event for the committed document; Playwright computes its own 500 ms idle window. Both are capped by the same `settle` timeout.
- `Senses::focused()` turns a failed read into "nothing focused" (the trait returns `Option`); Python's `focused()` lets the read's exception escape to the executor. `Page::focused()` itself still returns the error.
- A `DevToolsActivePort` that is not UTF-8 is "no endpoint"; Python's `read_text` would raise `UnicodeDecodeError` out of `devtools_endpoint`.
- An unknown error from a job (a Rust panic on the lane thread) comes back as `RuntimeError: a browser lane job panicked` and the lane keeps running; Python's executor would re-raise the exception.
- The live navigation, URL-oracle and WebMCP tests serve their pages from a loopback HTTP server instead of `page.route` interception (the driver has no request interception), so their hosts are 127.0.0.1 rather than `*.test`.
