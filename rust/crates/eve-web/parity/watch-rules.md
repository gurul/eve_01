# eve-web: watch-rules unit
Status: complete

watch.py lines 1-308 (config, the rate limiter), 1508-1926 (`openrouter`, asking a model, a watch and its
conditions) and 2821-2929 (Ticketmaster's pure rules), in `src/watch/config.rs`, `limiter.rs`, `model.rs`,
`rules.rs` and `ticketmaster.rs`. `json_object`, `reply_text` and `plain_link` in model.rs are the architect's and
unchanged.

## API
| Python | Rust | Notes |
|---|---|---|
| watch.WatchConfig | watch::config::WatchConfig | `Default` is the bare `WatchConfig()` |
| watch._number | watch::config::number | |
| watch._browser_on | watch::config::browser_on | see Differences |
| watch._tls_on | watch::config::tls_on | |
| watch.configured | watch::config::configured | `Path.expanduser` is `config::expand_user` |
| watch.RateLimiter | watch::limiter::RateLimiter, LimiterOptions, local_day | `RateLimiter::tokens()` reads `_tokens` for the tests |
| watch.openrouter | watch::model::openrouter, openrouter_ask | |
| watch._model_options | watch::model::model_options | |
| watch._json_object | watch::model::json_object | architect-complete |
| watch._reply_text | watch::model::reply_text | architect-complete |
| watch._plain_link | watch::model::plain_link | architect-complete |
| watch.model_reading | watch::model::model_reading | |
| watch.Watch | watch::rules::Watch | `to_json`, `from_json`; `ref` is `ref_` (serde rename "ref") |
| watch.quote_reading | watch::rules::quote_reading | |
| watch.plain_note | watch::rules::plain_note | |
| watch.money | watch::rules::money | |
| watch.describe | watch::rules::describe | |
| watch.evaluate | watch::rules::evaluate | `shown` is a positional bool |
| watch.now_line | watch::rules::now_line | |
| watch._takes_about | (none) | the Rust Notify seam always takes `about`; nothing to inspect |
| watch._alert_about | watch::rules::alert_about | |
| watch._hours | watch::rules::hours | |
| watch._when | watch::rules::when | |
| watch._stretch | watch::rules::stretch | |
| watch._every_words | watch::rules::every_words | |
| watch._iso | watch::ticketmaster::iso | |
| watch._tribute_words | watch::ticketmaster::tribute_words | |
| watch._tribute | watch::ticketmaster::tribute | |
| watch._sports | watch::ticketmaster::sports | |
| watch._norm | watch::ticketmaster::norm | |
| watch._tm_events | watch::ticketmaster::tm_events | |
| watch.pick_attraction | watch::ticketmaster::pick_attraction | |
| watch.ticketmaster_reading | watch::ticketmaster::ticketmaster_reading | |

`rules::py` (crate-private) holds the Python semantics these sections lean on: `str.split`/`strip` whitespace,
truthiness, `str()`/`repr()` of a JSON value, float `repr`, `format(x, "g")`, `round(x, n)`, `"{:,.Nf}"` and
`datetime.fromtimestamp`. `rules::price_of` is `_price` for a value that may be absent (None for anything but a
number or a string, as `_price` answers, without calling the page reader).

## Tests
All in `tests/watch_rules.rs`.

| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_watch_lean_conditions.py | 8 | 8 (1 ignored until the watcher unit lands: test_a_zero_baseline_does_not_stop_the_other_watches) | none |
| test_watch_lean_limiter.py | 7 | 5 | test_a_stale_gate_cannot_shorten_the_hosts_backoff, test_no_read_goes_to_a_host_inside_its_hold: both replace `Watcher._call` with a coroutine that holds the first read open while a tick runs; the skeleton's Watcher has no `_call` seam. They belong with the watcher unit (or a `call` hook on WatcherDeps). |
| test_watch_lean_ticketmaster.py | 4 | 4 (3 ignored until the watcher unit lands: test_a_sale_open_at_the_first_background_check_is_texted, test_a_reading_shown_in_the_add_reply_still_never_texts, test_the_keyword_search_keeps_games) | none |
| test_watch.py (this unit's pure cases) | the limiter (6), conditions, another currency, quotes, Ticketmaster statuses / far future / attraction, config | test_the_bucket_runs_dry_and_refills, test_one_host_waits_out_its_gap_another_goes_at_once, test_retry_after_holds_the_host_and_backoff_doubles_capped, test_the_daily_model_cap_refuses_and_resets, test_a_clock_that_steps_back_mints_no_tokens, test_the_backoff_ladder_never_overflows, test_conditions_fire_on_the_edge_and_rearm, test_a_reading_in_another_currency_is_no_price, test_a_hand_edited_watch_is_read_defensively, test_quotes_read_minor_units_inside_the_session, test_quotes_read_minor_units_and_unknown_symbols (ignored: page::price), test_ticketmaster_statuses_follow_its_own_definitions, test_ticketmaster_takes_the_lowest_listed_price (ignored: page::price), test_ticketmaster_far_future_dates_open_later, test_ticketmaster_resolves_the_artist, test_a_models_json_is_checked, test_the_switch_reads_the_environment (ignored: websearch / tinyfish / tls bodies) | the Watcher halves of those tests are the watcher unit's |

Added for this unit's API: test_a_first_reading_the_owner_never_saw_fires,
test_search_and_ticketmaster_alerts_carry_the_note_and_the_link, test_to_json_writes_every_field_in_order,
test_tribute_words_and_games, test_iso_reads_ticketmasters_times, test_money_reads_like_a_price,
test_describe_and_now_line, test_plain_note_hides_no_link, test_periods_intervals_and_stretch,
test_when_reads_a_local_time, test_model_options_by_model, test_openrouter_sends_one_completion,
test_openrouter_refusals, test_number_settings_are_clamped, test_expand_user.

The ignored tests name what they wait on; integration runs them with `cargo test -p eve-web --test watch_rules --
--ignored` once the other units are merged, and drops the `#[ignore]`.

## Boundaries
None.

## Differences
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
