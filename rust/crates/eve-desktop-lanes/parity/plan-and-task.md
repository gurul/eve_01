# plan-and-task (plan_executor.py, task_router.py)
Status: complete

## Modules
| Python | Rust |
|---|---|
| plan_executor.py | src/plan_executor.rs |
| task_router.py | src/task_router.rs |

## API
| Python | Rust | Notes |
|---|---|---|
| plan_executor.Entry | plan_executor::Entry | `to_dict` keeps the key order; note cut to 200 chars, ms `round(…, 1)` |
| plan_executor.PlanResult | plan_executor::PlanResult | `applied` is a method; `to_dict` keeps the key order |
| plan_executor._holds | plan_executor::holds | `wait` explicit (Python default EXPECT_WAIT_SECS) |
| plan_executor._confirm_label | plan_executor::confirm_label | |
| plan_executor._reason | plan_executor::reason | |
| plan_executor._say | plan_executor::say | |
| plan_executor.run_plan | plan_executor::run_plan + PlanOptions | keyword arguments are PlanOptions (Default = Python's defaults) |
| plan_executor._grant | plan_executor::grant | `hasattr(effectors, "approve")` is the `Effectors::grant` method (no-op by default) |
| plan_executor._clear_consent | plan_executor::clear_consent | `getattr(effectors, "clear_consent")` is `Effectors::clear_consent` returning None |
| plan_executor._norm | plan_executor::norm | |
| plan_executor._named_field | plan_executor::named_field | returns a clone; the lookarounds are an explicit ASCII [a-z0-9] boundary check |
| plan_executor._pick_field | plan_executor::pick_field | Python's `is` identity is the pool index |
| task_router.Plan | task_router::Plan | `complete` is a method; `code()` reproduces repr() quoting and `quote_plus` |
| task_router.installed_apps | task_router::installed_apps, installed_apps_in | the default call (cached, Finder added) and the dirs/listdir call (uncached, no Finder) are two functions |
| task_router._strip_filler | task_router::strip_filler | |
| task_router._display_names | task_router::display_names | IndexMap keeps dict order; `str.title()` reproduced |
| task_router.match_app | task_router::match_app | |
| task_router._quit | task_router::quit | |
| task_router._first_clause | task_router::first_clause | |
| task_router._launch | task_router::launch | |
| task_router._search | task_router::search | |
| task_router.find_app_mention | task_router::find_app_mention | |
| task_router.app_mentions | task_router::app_mentions | |
| task_router._explicit_search | task_router::explicit_search | |
| task_router._consequential | task_router::consequential | `CONSEQUENTIAL.match` is a find that must start at 0 |
| task_router.classify | task_router::classify | `model` / `quit_model` are `RouterModel` closures; an Err or a panic abstains |

Every regex constant (CONSEQUENTIAL, TELL_ME, TYPING, POLITE, PRONOUN_QUERY, LOCAL_SCOPE, FILLER, LAUNCH_VERB, SEARCH_LEAD,
SEARCH_TAIL, NAMES_WEB, IN_BROWSER, AND_THEN, VAGUE_QUERY, QUIT_VERB, QUIT_ALL_TARGET, FORCE, QUIT_WORD, IN_APP_SEARCH,
EXPLICIT_SEARCH) is a `LazyLock` with the Python pattern verbatim under `(?i)`; LOCAL_SCOPE is `fancy_regex` for its
negative lookahead. The dict constants (GERUND, BROWSERS, APP_ALIASES) are ordered slices; WRITE_APPS and SHELL_APPS are
slices (Python frozensets, only membership is read).

## Tests
| Python file | Python tests | Rust tests (tests/plan_and_task.rs) | Not ported (why) |
|---|---|---|---|
| test_plan_executor.py | 26 walking/approval/form/consent tests | the same 26 names | the 5 contract tests (`test_a_plan_the_code_cannot_fully_read_is_not_a_plan`, `test_where_text_came_from_is_checked_not_trusted`, `test_text_inside_a_longer_word_is_not_the_owners`, `test_the_planner_can_decline`, `test_the_planner_is_shown_labels_and_roles_never_the_title`) test plan_contract only and are ported in eve-plan-contract |
| test_task_router.py | classify, installed_apps, app_mentions, is_question, routing doc | test_a_bare_launch_of_an_installed_app_is_a_complete_reflex, test_a_launch_with_more_to_do_hands_the_rest_to_the_planner, test_what_looks_like_a_launch_and_is_not, test_an_explicit_web_search_is_a_reflex_with_the_query_found_by_code, test_what_looks_like_a_web_search_and_is_not, test_a_model_is_asked_only_after_the_gates_and_may_only_add_a_bare_launch, test_installed_apps_lists_real_names_once, test_the_routing_doc_states_the_shipped_defaults_and_names_every_knob, test_is_question_is_one_rule_for_both_routers, test_app_names_said_as_one_word_and_every_mention | test_jev_is_asked_in_its_own_idiom_and_gated_by_absolute_answers and test_each_model_is_asked_differently_for_a_head_pose test typed_ask (eve-typed-ask); the four agent tests (reflex runs, reflexes off) drive ComputerAgent (computer-agent crate) |
| test_plan_success_guard.py (ref) | the run_plan tests | test_a_success_check_that_holds_only_after_the_steps_is_complete, test_a_success_check_already_true_before_step_1_is_rejected, test_a_resumed_plan_keeps_the_rejection_it_was_handed, test_the_pre_check_does_not_wait_for_a_check_that_is_false, test_duck_duck_go_by_google_is_not_done, test_duck_duck_go_by_duck_duck_go_is_done, test_the_request_check_replaces_the_planners_choice, test_a_request_check_already_true_before_step_1_is_rejected, test_no_outcome_falls_back_to_the_planners_check | url_query / request_check / round-trip tests are plan_contract's; the agent and desktop_worker resume tests belong to their crates |
| test_plan_url_expectation.py (ref) | test_exact_url_requires_fresh_snapshot_and_does_not_replace_title_predicate | same name | url_matches / parse / plan_request tests are plan_contract's; the live browser test is browser_lane's |
| test_quit_reflex.py (ref) | classify quit parts | test_rules_take_a_bare_quit (all 8 cases), test_everything_else_is_never_a_quit (all 11), test_jev_is_asked_only_behind_the_quit_word_gate | decide_quit / fit / questions are typed_ask's; the wrapper and quit_all are app_reflex's |
| test_site_names.py (ref) | test_the_router_sees_joined_names | same name | the joiner and site table are browser_intent's |
| test_vault_fill.py (ref) | test_the_executor_runs_a_fill_through_the_page_and_stops_on_a_refusal | same name | contract, page and live Chromium tests belong to plan_contract / browser_lane |
| (new) | — | test_the_result_dict_keeps_pythons_keys_and_rounding, test_the_lanes_lines_are_read_back_as_python_reads_them, test_an_unreadable_oracle_is_unknown_except_for_a_url, test_folded_names_are_shown_title_cased_and_the_code_line_quotes_like_python | pin to_dict shape/rounding, the line readers, `_holds`'s unknown-vs-false rule, title-casing and repr() quoting |

55 tests. With the sibling units' fast_lane, lane_router and ax_candidates filled in, all 55 pass; on the skeleton
alone the 35 that reach `run_delegate`, `editable_pool`, `normalise` or `is_question` stop at those `todo!()`s.
Differential check during the port: `classify` agreed with the Python on all 965 requests of
bridge/tests/fixtures/routes plus 15 extra edge cases (every Plan field, `code()` and `sentence()`).

## Boundaries
None.

## Differences
- run_plan on a click step with neither label_hint nor target: Python's `assert result is not None` raised; Rust ends
  the plan `partial` with reason `no_target` (input count zero). parse_plan never lets such a step through.
- PlanOptions.approved is `HashMap<i64, String>`; a negative key (Python would keep it and never match it) is dropped.
- A router model (`model` / `quit_model`) that panics abstains, as one that returns Err does (Python: any exception).
- `Plan.code()`'s repr() treats control, separator and the common format characters as unprintable; other exotic
  Unicode categories Python's `str.isprintable` also escapes are printed as is (app names and URL-encoded queries never
  carry them).
- installed_apps reads folder names lossily as UTF-8; `~user/…` folders are not expanded (APP_DIRS has only `~/…`).
