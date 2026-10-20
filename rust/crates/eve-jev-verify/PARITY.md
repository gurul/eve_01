# eve-jev-verify
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/jev_verify.py` | `src/lib.rs` (+ `src/py.rs`, `src/pychars.rs`: Python's `str` rules the text checks depend on) |

## API
| Python | Rust | Notes |
|---|---|---|
| `KINDS`, `MIN_JOURNEYS`, `MAX_JOURNEYS`, `MAX_STEPS`, `MAX_EXPECT`, `MAX_CONTROLS`, `MAX_LINES`, `MAX_TEXT_CHARS`, `MAX_EXPECT_LINES`, `MAX_LINE_CHARS`, `SUMMARY_CHARS`, `STEP_WAIT_MS`, `JOURNEY_TIMEOUT_S`, `JEV_TIMEOUT_S`, `EVAL_TIMEOUT_S`, `LOCATE_MS`, `STEP_NONE` | same names | same values |
| `ACTS_ON` | `ACTS_ON` (array of pairs) + `acts_on(do)` | same kinds per step kind |
| `Step` | `Step { do_, target, value }` | `do` is a Rust keyword; `words()`, `as_dict()` same text and keys; `Step::tap` for `Step("tap", x)` |
| `Journey` | `Journey { name, steps, expect }` | `as_dict()` same keys |
| `_text` | `text_of` (private) | str/int/float only, never a bool; a float as Python's `repr` |
| `parse_journeys` | `parse_journeys(&Value)` | same problems, word for word (checked against the Python module on 450 random blocks) |
| `extract_journeys` | `extract_journeys` | last block wins; the JSON error text is CPython's (`eve_ipc::pyjson::loads`) |
| `journeys_json` | `journeys_json` | returns a JSON array |
| `COLLECT_JS`, `TEXT_JS`, `PRESS_JS` | same names | byte-identical (a test reads them out of the Python file); `BLUR_JS` names the `(el) => el.blur()` type sends |
| `Control` | `Control` (`type_` for `type`) | `render()` same text (checked on 200 random controls) |
| `Screen` | `Screen` | `summary()` same |
| `screen_lines` | `screen_lines` | Python's `splitlines` boundaries and `split()` whitespace |
| `_words` | `words_of` (private) | `[^\W\d_]+|\d+` with CPython's character classes, casefolded |
| `relevant_lines` | `relevant_lines(lines, expects, limit)` | `limit` is explicit (Python's default is `MAX_EXPECT_LINES`) |
| `_numbers` | `numbers_of` (private) | |
| `numbers_shown` | `numbers_shown` | |
| `_tokens` | `tokens` (private) | |
| `words_shown` | `words_shown` | |
| `screen_from` | `screen_from(&Value)` | `str(x or default)` truthiness kept; a control that is not an object is skipped |
| `step_questions` | `step_questions` | byte-identical instructions (test compares the whole request) |
| `expect_questions` | `expect_questions` | |
| `expect_state` | `expect_state` | |
| `step_state` | `step_state` | |
| `offered` | `offered` → `IndexMap<"c<n>", Control>` | Telegram's buttons kept past `MAX_CONTROLS` |
| `read_step` | `read_step` | via `eve_typed_ask::choice` / `noul` (made public for this port) |
| `JOURNEY_STEP_GATES`, `EXPECT_SHOWN` | same names | 0.8 / 0.9 / 0.3 / 2.0 / 2.0; 0.6 |
| `JevDown` | `JevDown` (error) | `str(e)[:160] or type(e).__name__` |
| `JevCalls` | `JevCalls` | `as_dict()` returns the same keys in the same order (`calls, input_tokens, ms, errors, usd, p50_ms, p90_ms`); median and p90 as Python computes them |
| `Predict` | `Predict` (`Arc<dyn Fn(&Value, &Value) -> anyhow::Result<Value> + Send + Sync>`) | blocking, run on `spawn_blocking` (Python: `asyncio.to_thread`) |
| `Verifier` | `Verifier::new` / `Verifier::with(predict, step_gates, expect_shown, record)` | asked twice before JevDown; `record()` returns the kept requests |
| `from_env` | `from_env(&impl eve_jev::Env)`, `from_process_env()` | `EVE_APP_JOURNEYS`, `EVE_JEV_TIMEOUT` (at least 6 s), route from eve-jev |
| `JourneyResult` | `JourneyResult` (+ `ExpectResult`) | `passed()`, `issue()`, `owner_line()`, `as_dict()` same text; `data` is `Value::Null` for None |
| `_evidence` | `evidence` (private) | |
| `_norm` | `norm` (private) | |
| `_option_for` | `option_for` | checked on 300 random option lists |
| `Hung` | `Hung` (error) | |
| `_bounded` | `bounded` (private) | `tokio::time::timeout` |
| `read_screen` | `read_screen` (+ `read_screen_within(page, secs)`) | |
| `refind` | `refind` | |
| `_do` | `do_step` (private) | |
| `act` | `act` (+ `act_within(..., secs)`) | `Err` is `Hung` or the page's own error, as Python raises them |
| `_listed` | `listed` (private) | |
| `_not_found` | `not_found` (private) | |
| `walk` | `walk(page, journey, verifier, WalkOptions { errors, on_step, eval_timeout_s })` | `Err` is `JevDown` or the page's own error; `Hung` becomes the journey's failure, as in Python |
| `JourneyRun` | `JourneyRun` | `passed()`, `summary()`, `issues()`, `owner_lines()` (an `IndexMap`), `as_dict()`; `jev` is a JSON object |
| (Playwright page) | `trait Page` + `PageError { kind, message }` | the seven calls jev_verify makes; app_check's port implements it over CDP |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_jev_verify.py` | `test_a_working_app_passes_its_journey_and_the_summary_says_so`, `test_the_broken_add_is_caught_as_an_expectation_not_shown_with_evidence`, `test_a_step_whose_control_is_not_there_fails_at_that_step_and_says_what_is`, `test_jev_down_never_fails_the_app_and_the_journeys_are_not_run`, `test_the_main_buttons_text_is_on_the_screen_jev_reads`, `test_a_control_rerendered_while_jev_answers_is_found_again_not_blamed_on_the_app`, `test_choose_on_a_custom_dropdown_picks_the_value_not_just_the_opener`, `test_choose_in_a_radio_group_named_by_its_legend_picks_the_option`, `test_choose_fails_when_the_value_is_nowhere_after_opening_the_target`, `test_one_failed_jev_request_is_asked_again_before_giving_up`, `test_an_app_that_hangs_in_a_journey_fails_that_journey_in_seconds` | same names in `src/tests.rs`, walked by `walk` on a fake `Page` that models each test page (the splitter, its broken twin, the chooser, the counter, the hanging field, the MainButton picker) | Python runs them through `app_check.check_app` in Chromium; the browser and the sandbox are app-check's. The Jev-down test asserts the `JevDown` walk result (and its "not run" summary); app_check's own handling is app-check's |
| `bridge/tests/test_jev_verify.py` | `test_journeys_parse_strictly_and_drop_what_cannot_be_walked`, `test_the_fitted_gates_act_only_when_the_noul_and_the_choice_both_clear`, `test_the_step_question_names_the_label_and_the_state_holds_the_controls`, `test_the_screen_text_is_short_distinct_lines`, `test_from_env_needs_a_key_and_can_be_turned_off`, `test_jev_reads_a_shortlist_of_the_screen_with_the_near_misses_kept`, `test_a_number_jev_did_not_really_see_vetoes_shown`, `test_an_empty_or_unusable_journeys_block_is_a_problem_not_silence`, `test_the_owner_reads_a_short_line_and_the_repair_round_the_details`, `test_words_there_but_not_in_the_expectations_order_veto_shown` | same names | None |
| `bridge/tests/test_jev_verify.py` | `test_no_journeys_is_the_scripted_check_alone`, `test_a_journey_step_never_lands_on_a_control_tagged_on_an_earlier_screen`, `test_when_jev_goes_down_in_one_journey_the_others_stop_with_the_check`, `test_a_journey_that_breaks_the_page_is_its_own_failure_and_the_others_still_count`, `test_jev_down_in_one_journey_keeps_the_journeys_that_already_failed`, `test_journeys_are_skipped_when_the_scripted_pass_already_found_the_app_hung`, `test_the_owners_photo_is_the_last_screen_of_a_journey_that_passed` | — | they test `app_check.check_app` / `_journeys` (task fan-out, cancellation, the photo, the scripted pass) or COLLECT_JS inside a real DOM: they belong to eve-app-check, which runs a browser |
| (new) | | `test_a_bad_journeys_block_quotes_pythons_json_error`, `test_journeys_are_capped_and_their_text_is_one_short_line`, `test_a_wrong_envelope_is_jev_down`, `test_a_control_gone_for_good_is_the_apps_problem`, `test_choose_on_a_select_without_the_option_says_what_it_has`, `test_errors_and_steps_are_reported_to_the_caller`, `test_the_verifier_records_every_request_when_asked`, `test_controls_render_as_jev_reads_them`, `test_a_busy_screen_offers_at_most_max_controls_with_telegrams_buttons_kept`, `test_a_step_answer_is_read_as_the_click_paths_step_answer`, `test_a_screen_is_read_from_what_the_script_returns`, `test_refind_keeps_the_place_among_controls_that_share_a_label`, `test_a_choose_value_finds_its_option`, `test_the_bill_has_p50_and_p90`, `test_the_page_scripts_are_pythons`, `test_a_walk_can_run_on_its_own_task`, `test_every_text_rule_answers_as_the_python_module_does`; `py::tests::*` | `test_every_text_rule_answers_as_the_python_module_does` replays `tests/fixtures/python_cases.json`, written by `tests/fixtures/gen_cases.py` from the Python module itself (tokens, numbers, words, both vetoes, the shortlist, screen lines, option matching, 450 journeys blocks, JSON errors, control rendering) |
| `bridge/tests/test_apps_maker.py` (ref) | `test_the_prompt_asks_for_journeys_and_the_skeletons_parse`, `test_the_skeleton_walks_its_own_journeys_with_a_literal_verifier`, journeys store tests | — | apps_maker's (eve-brain); they call `parse_journeys`, `journeys_json`, `Verifier` as ported here |
| `bridge/tests/test_miniapp.py` (ref) | `test_the_page_hears_a_journey_problem_in_plain_words_and_a_failed_change_as_a_change` | — | miniapp's (eve-brain); it builds `JourneyResult` / `JourneyRun` and reads `issues()` / `owner_lines()` as ported here |

## Boundaries
- The page is the `Page` trait. Python drives Playwright directly; here app_check's port supplies the page (Chromium over CDP) and runs the same page scripts. No subprocess.
- `src/pychars.rs` is data generated from CPython 3.13.14 (Unicode 15.1.0) by `gen_pychars.py`: `isalpha`, `isdecimal`, `isdigit`, `isnumeric` and `casefold`, so `\w`, `\d` and casefolding count the same words Python's `re` does.

## Differences
- `act` and `read_screen` bound the page by an explicit `eval_s` (`*_within`, `WalkOptions::eval_timeout_s`) where the Python tests monkeypatch the module's `EVAL_TIMEOUT_S`; the default is the same 5 s.
- `screen_from` skips a control that is not a JSON object, reads a `controls` that is not a list and a non-object result as empty, and reads an `options` that is a number or a bool as no options, where Python raises AttributeError or TypeError (COLLECT_JS never returns any of these).
- `offered` (and so `Verifier::pick`) offers nothing for a step whose kind is not tap, type or choose, where Python's `ACTS_ON[step.do]` raises KeyError. `parse_journeys` never makes such a step; only a hand-built `Step` can.
- An action error whose text is empty reads `(Kind: )` where Python's `str(e).splitlines()[0]` raises IndexError out of `act`.
- `extract_journeys` reads JSON with `eve_ipc::pyjson` (CPython's messages, at the same positions); `NaN`/`Infinity` and integers past 64 bits, which `json.loads` accepts, are refused or become floats there; a block nested past 1,000 levels is a problem line where Python's RecursionError would raise out of the maker.
- `Verifier.ask` treats a predict that panics like one that raised.
