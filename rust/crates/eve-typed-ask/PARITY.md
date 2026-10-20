# eve-typed-ask
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/typed_ask.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `Predict` | `Predict<'a>` (`dyn Fn(&Value, &Value) -> anyhow::Result<Value>`) | `Err` stands for the raised exception; `PredictError { kind, message }` carries the Python class name |
| (clock argument) | `Clock<'a>` (`dyn Fn() -> f64`), `perf_counter()` | seconds, monotonic |
| `f"{type(e).__name__}: {e}"[:160]` | `error_line` | `PredictError` → its kind; `io::Error` → the OSError subclass its kind maps to (TimedOut → TimeoutError, …); else `Exception` |
| `POSES`, `DIRECTIONS` | `POSES`, `DIRECTIONS` | same order |
| `HeadAnswer` | `HeadAnswer` (+ `HeadAnswer::new`) | `pose()` same branching; `raw` is the answers map |
| `Gates` | `Gates` | `decide` returns `String` |
| `_choice` | `choice` (private) | bool probabilities count as 1.0/0.0, as `isinstance(v, (int, float))` does |
| `_noul` | `noul` (private) | a bool is 0.0 |
| `LAYA_HEAD_QUESTIONS` | `LAYA_HEAD_QUESTIONS` (`LazyLock<Value>`) | byte-identical JSON, same key order (checked against `json.dumps`) |
| `ask_laya_head` | `ask_laya_head` | |
| `JEV_HEAD_QUESTIONS`, `JEV_STATE_NOTE` | same names | byte-identical |
| `ask_jev_head` | `ask_jev_head` | |
| `JEV_HEAD_GATES` | `JEV_HEAD_GATES` | 0.30 / 0.70 |
| `make_jev_head_asker` | `make_jev_head_asker` | returns `impl Fn(&str) -> String` |
| `GATE_GRID`, `DIRECTION_GRID` | same | |
| `fit_gates` | `fit_gates` | tuple-key maximum, ties to the stricter gate; `max_false_moves` is explicit |
| `describe` | `describe` | `anyhow::Result<String>`: a question without `type` is the KeyError |
| `jev_request_questions` | `jev_request_questions` | byte-identical |
| `RequestAnswer` | `RequestAnswer` (+ `new`) | |
| `RequestGates` | `RequestGates` (`Default` = 0.8/0.8/0.8/0.2) | |
| `ask_jev_request` | `ask_jev_request` | failure: risky 1.0, error line |
| `decide_request` | `decide_request` | `&'static str` launch / search / other |
| `JEV_REQUEST_GATES` | `JEV_REQUEST_GATES` | 0.7 / 0.8 / 0.5 / 0.5 |
| `make_jev_request_asker` | `make_jev_request_asker` | `impl Fn(&str, &[String]) -> String` |
| `REQUEST_GRID`, `RISK_GRID` | same | |
| `fit_request_gates` | `fit_request_gates` | |
| `jev_quit_questions` | `jev_quit_questions` | byte-identical |
| `QuitAnswer` | `QuitAnswer` (+ `new`) | |
| `QuitGates` | `QuitGates` (`Default` = 0.9 each) | |
| `ask_jev_quit` | `ask_jev_quit` | |
| `decide_quit` | `decide_quit` | app name, `*`, or `""` |
| `QUIT_GRID` | `QUIT_GRID` | |
| `fit_quit_gates` | `fit_quit_gates` | |
| `JEV_QUIT_GATES` | `JEV_QUIT_GATES` | 0.5 / 0.6 / 0.95 |
| `make_jev_quit_asker` | `make_jev_quit_asker` | |
| `LAYA_REQUEST_KINDS`, `LAYA_RISKY_KINDS`, `LAYA_APP_SHORTLIST` | same (`&[(&str, &str)]`, `&[&str]`, `usize`) | same order |
| `app_shortlist` | `app_shortlist` (+ `sequence_ratio`) | `difflib.SequenceMatcher(None, a, b).ratio()` ported exactly, autojunk included; `str.casefold` for what `[a-z0-9]+` can see |
| `ask_laya_request` | `ask_laya_request` | risky is the sum send + remove + write in that order |
| `STEP_NONE`, `STEP_ABOUT` | same | |
| `jev_step_questions` | `jev_step_questions` | takes `&IndexMap<String, String>` (menu order) |
| `StepAnswer` | `StepAnswer` (+ `new`) | `k`, `requests` as `usize`; `requests` defaults to 1 |
| `StepGates` | `StepGates` (`Default` = 0.5/0.5/0.1/0.7/0.3) | `decide` returns `String` |
| `ask_jev_step` | `ask_jev_step` | keyword arguments become positional: step, app, context, options, recent, clock; error quotes the key with Python `repr` |
| `STEP_GROUP`, `STEP_SHORTLIST`, `GROUPED_MAX_OPTIONS`, `GROUPS_ENV`, `GROUPS_DEFAULT`, `STEP_PART_NONE` | same | 25 / 3 / 100 / `EVE_JEV_GROUPS` / false |
| `ask_jev_step_grouped` | `ask_jev_step_grouped` | `group` is explicit (pass `STEP_GROUP`); same two-round body, finalists in menu order, risk the higher of the rounds |
| `groups_on` | `groups_on` | `Option<&HashMap>`; `None` reads the process env |
| `make_step_asker` | `make_step_asker` → `StepAsker` | `partial` + `.max_options` becomes a struct with `grouped` (Python's `.func`), `max_options` and `ask(...)` |
| `STEP_GRID`, `STEP_MARGIN_GRID` | same | |
| `fit_step_gates` | `fit_step_gates` | `max_wrong` explicit |
| `JEV_STEP_GATES`, `JEV_STEP_DEFAULT` | same | 0.7/0.9/0.3/0.7/0.5, false |
| `COMMAND_RISK_MODES` | same | |
| `_SECRET_PATTERNS` | `SECRET_PATTERNS` (private) | same five patterns, same order |
| `redact_command` | `redact_command` | group 1 kept when the pattern has one |
| `COMMAND_ABOUT` | same | |
| `jev_command_questions` | `jev_command_questions` | byte-identical |
| `CommandAnswer` | `CommandAnswer` (+ `new`, `nouls`) | |
| `CommandGates` | `CommandGates` (`Default` = 0.7 each) | `reached` in fixed order |
| `COMMAND_REASONS` | `COMMAND_REASONS` (`&[(&str, &str)]`) | |
| `CommandVerdict` | `CommandVerdict` (+ `new`) | `decision` is risky / safe / unknown |
| `ask_jev_command` | `ask_jev_command` | folder = basename of posix `normpath(cwd)`; command cut at 2000 chars; "predict returned no answers" |
| `decide_command` | `decide_command` | |
| `make_jev_command_asker` | `make_jev_command_asker` | `gates: Option<CommandGates>` |
| `COMMAND_GRID` | `COMMAND_GRID` | |
| `fit_command_gates` | `fit_command_gates` | minimises (wolves, -(d+e+p+s)), first found wins a tie |
| `JEV_COMMAND_GATES`, `COMMAND_RISK_DEFAULT` | same | 0.6/0.9/0.9/0.8, "shadow" |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_typed_ask_laya_request.py` | `test_shortlist_is_close_names_only`, `test_laya_is_asked_short_choices_and_read_as_a_request_answer`, `test_no_shortlist_means_no_app_question_and_a_failure_abstains` | same names in `tests/typed_ask.rs` | None |
| `bridge/tests/test_command_risk.py` (ref) | `test_the_questions_are_absolute_nouls_over_a_redacted_command`, `test_secrets_never_leave_in_a_command`, `test_any_gate_reached_is_risky_and_an_error_is_unknown`, `test_fit_command_gates_allows_no_miss` | same names | the daemon tests belong to the daemon crate |
| `bridge/tests/test_jev_grouped.py` (ref) | `test_a_menu_that_fits_one_group_is_asked_exactly_as_before`, `test_a_long_menu_is_one_request_of_parallel_groups_then_one_final`, `test_the_final_round_offers_each_groups_top_three_in_menu_order`, `test_every_group_answering_none_abstains_without_a_second_request`, `test_risk_from_the_first_round_is_kept_even_if_the_final_reads_it_lower`, `test_an_answer_outside_its_group_is_an_error_and_never_a_press`, `test_a_failing_predict_abstains`, `test_the_switch_picks_the_asker_and_its_menu` | same names, plus `test_the_step_asker_runs_the_function_the_switch_picked` | the fast_lane tests belong to fast_lane |
| `bridge/tests/test_jev_step.py` (ref) | `test_one_request_carries_the_choice_with_none_and_three_nouls`, `test_the_nouls_can_see_the_controls_because_the_state_lists_them`, `test_a_failing_or_malformed_answer_abstains`, `test_gates_press_only_when_the_absolute_and_the_relative_both_clear`, `test_fit_allows_no_wrong_press_and_prefers_the_stricter_tie` | same names | the lane tests (`run_delegate`) belong to fast_lane |
| `bridge/tests/test_quit_reflex.py` (ref) | `test_decide_quit_and_the_fit_allow_no_wrong_quit`, `test_jev_quit_request_is_absolute_nouls_and_the_installed_list` | same names, plus `test_the_quit_asker_reads_the_app_and_abstains_on_failure` | task_router / app_reflex tests belong there |
| `bridge/tests/test_task_router.py` (ref) | `test_jev_is_asked_in_its_own_idiom_and_gated_by_absolute_answers`, `test_each_model_is_asked_differently_for_a_head_pose` | same names, plus `test_describe_counts_options_and_their_longest_wording`, `test_fit_request_gates_fires_no_unsafe_reflex`, `test_laya_head_reads_the_head_kind_as_its_gate` | the router tests belong to task_router |
| `bridge/tests/test_voice_fast_head.py` (ref) | the typed_ask assertions of the fast-head test | `test_the_jev_head_asker_answers_a_pose_under_the_shipped_gates` | voice_agent / head parts belong there |
| `bridge/tests/test_america_browser.py`, `test_computer_protocol_eval.py`, `test_jev_verify.py` (ref) | use `StepAnswer` as a fake's return value only | covered by the `StepAnswer` tests | nothing typed_ask-specific to port |
| (unit, `src/lib.rs`) | | `sequence_ratio_matches_difflib`, `shortlist_and_redaction_match_cpython`, `normpath_and_basename_follow_posixpath`, `python_repr_and_spaces`, `choice_admits_bools_and_noul_does_not`, `a_result_that_is_not_a_dict_abstains_with_pythons_error` | reference values taken from CPython 3.12 |

## Boundaries
None

## Differences
- A `predict` error's class name comes from `PredictError.kind` (or an `io::Error` kind); any other error reads `Exception: <message>`. Callers that want Python's exact class names wrap their errors in `PredictError`.
- When `answers` is truthy but not a dict, Python raises AttributeError from the first `answers.get(...)` after the `try`; Rust abstains with that same AttributeError line as the error (the askers never raise).
- The `fit_*` functions check the input lengths up front and panic on a mismatch, where Python's `zip(strict=True)` raises ValueError only once a comparison exhausts the shorter list.
- `app<N>` keys are read with ASCII digits only; Python's `str.isdigit()` also admits other Unicode digits.
- `str.casefold` is modelled as lowercase plus the full foldings that produce ASCII letters (ß, ſ, ligatures, …); foldings that only produce non-ASCII text cannot change the `[a-z0-9]+` scan.
- Under `(?i)` Python's `[A-Z]` also matches İ and ı; Rust's regex matches ſ and K (Kelvin) like Python but not those two. Only the redaction patterns use it.
