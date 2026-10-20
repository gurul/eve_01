# eve-web: search-router unit
Status: complete

`search_router.py` (all) and `websearch.py` (all) in `src/search_router.rs` and `src/websearch.rs`. The API rows
are already in `PARITY.md`; this fragment adds the unit's tests, boundaries and differences.

## API notes
| Python | Rust | Notes |
|---|---|---|
| search_router.Predict | eve_decider::Predict | Jev's predict seam, `Fn(&Value, &Value) -> Result<Value, PredictError>` |
| search_router.TINYFISH, PERPLEXITY, FIRECRAWL, PROVIDERS, MODES, JEV_TIMEOUT_S, READER_ROUTED_SECS, OPTIONS, OPTION_TO_PROVIDER, PROVIDER_TO_OPTION, ABOUT, GRID, MAX_GRID, GATES, SHIPPED, NOT_FOUND, READER_SYSTEM, _LINK | same names (`provider_to_option`, `LINK`) | verbatim |
| search_router._JEV, _JEV_LOCK | private `JEV` (a `parking_lot::Mutex`) | remade when the route's (url, model) changes, as Python |
| websearch.URL, ENGINES, OPENROUTER_ENGINES, DEFAULT_*, HEDGE_SECS, FALLBACK_SECS, FALLBACK_ENGINE, MAX_*, SEARCH_USD, TOOL_NAME, WEB_SEARCH_TOOL, HOSTED_TOOL, ROUTED_REACH, ROUTED_NOTE, SYSTEM, CLOCK, MONID_RESERVE_SECS, MONID_HTTP | same names (`openrouter_engine`, `search_usd` for the dict lookups) | verbatim |
| urllib.request.urlopen (websearch's opener) | websearch::SystemOpener (`Opener` trait; `OpenRequest`, `OpenError`) | a blocking reqwest POST; tests pass a closure |
| search's / engine_search's keyword arguments | websearch::EngineOpts | `hedge_secs` is the HEDGE_SECS seam the tests patch |
| via's / answer's keyword arguments | search_router::AnswerOpts | |
| bounded's `work` | search_router::Work, Abandoned | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_search_router.py | test_the_state_is_the_request_and_jev_always_sees_all_three_options_as_structure, test_the_question_never_varies_with_the_keys_and_a_missing_pick_is_masked (3 params in one loop), test_decide_needs_both_the_choice_and_the_noul_and_else_is_perplexity, test_the_router_never_picks_what_is_not_available_and_a_failure_is_perplexity, test_fit_allows_zero_wrong_routes_and_prefers_the_stricter_tie, test_wrong_is_the_costly_direction_only, test_the_placeholder_gates_route_nothing, test_available_follows_the_mode_the_keys_and_the_engine, test_a_routed_quick_read_answers_in_the_brains_result_shape, test_a_routed_provider_that_fails_is_followed_by_one_perplexity_search (3 params in one loop; the RuntimeError reader is a panicking closure), test_a_pages_non_answer_is_followed_by_perplexity_too, test_a_router_that_fails_or_names_a_missing_provider_is_perplexity, test_routing_off_is_the_engine_for_the_brain_and_tinyfish_for_the_reader, test_links_the_search_did_not_read_are_removed_whichever_provider_answered, test_a_slow_routed_provider_is_abandoned_at_its_budget_and_perplexity_answers, test_the_budget_is_the_brains_setting_and_longer_but_bounded_for_the_reader, test_the_firecrawl_cap_is_one_count_for_both_surfaces_per_local_day, test_a_spent_firecrawl_day_routes_to_perplexity_with_the_reason_logged, test_cost_usd_is_a_number_only_when_every_part_is_priced, test_the_tuning_set_is_labelled_and_mixed, test_the_firecrawl_cap_survives_a_restart, test_a_tool_that_needs_an_id_looked_up_first_is_not_offered | tests/search_router.rs: same names without `test_`; plus fit_refuses_answers_and_truths_of_different_lengths, keep_read_links_keeps_a_read_link_before_its_punctuation_and_collapses_whitespace, the_reader_hears_not_found_as_no_answer, bounded_returns_the_work_or_its_failure, a_firecrawl_route_runs_the_one_tool_the_model_picks_and_answers_from_it, usage_and_result_read_the_answer_models_calls | test_the_eval_fits_on_every_seen_set_and_scores_the_fresh_holdout_once, test_the_printed_verdict_is_exactly_the_pre_registered_bar: they test tools/route_eval.py, a Python evaluation tool outside the daemon, not this crate |
| test_websearch.py | test_configured_uses_perplexity_with_an_openrouter_key_and_no_search_without, test_an_openai_answer_model_is_allowed_through_openrouter, test_the_request_is_one_chat_call_with_the_search_server_tool_and_the_clock, test_parse_reads_the_answer_and_the_citations_once_each, test_search_posts_once_and_never_raises, test_configured_lists_the_routed_providers_and_the_tool_says_what_they_reach (both SHIPPED params: `available_shipped(.., false)` for the unshipped case), test_search_keeps_its_shape_and_cost_fields_for_every_provider (3 params in one loop), test_the_off_switch_is_todays_engine_search_exactly, test_a_hung_or_unreachable_engine_search_is_tried_once_on_the_other_engine, test_a_slow_engine_is_raced_by_the_other_and_the_first_answer_wins, test_a_fast_engine_never_starts_the_other, test_the_whole_search_gives_up_at_its_budget, test_answer_options_asks_mercury_for_low_reasoning_with_room | tests/search_router.rs: same names without `test_`; plus an_engine_search_writes_one_spend_line | None |

Seams: Python's `monkeypatch` of `spend.record`, `firecrawl._CALLS`, `jev_router`, `web_reader.search_and_read`,
`firecrawl.search`, `watch.openrouter` and `HEDGE_SECS` becomes a per-test `Ctx` (its own spend folder, Firecrawl
count file and environment with EVE_MONID=0, conftest's `_no_paid_monid_runs`), `Paths`, `AnswerOpts.route` and
`EngineOpts.hedge_secs`. `test_the_budget_is_the_brains_setting...` spied on `bounded`; the Rust test shows the
same two budgets by what they let through (a 0.3 s reader is abandoned under the brain's 0.05 s and waited for
under the reader's READER_ROUTED_SECS). `test_search_keeps_its_shape...` patched `jev_router` under
`websearch.search`; the Rust test calls `search_router::answer` with the route seam, the same path `search` takes
when `providers` is set.

## Boundaries
None.

## Differences
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
