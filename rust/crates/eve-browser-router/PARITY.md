# eve-browser-router
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/browser_router.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `Predict` | `Predict` (= `eve_decider::Predict`: `Fn(&Value, &Value) -> Result<Value, PredictError>`) | An `Err` is the exception the Python predict raised; `PredictError.kind` is its class name. `eve_jev::JevPredict::decider_fn()` fits it |
| `CODEX`, `WEB`, `AUTO` | same `pub const`s | `"codex"`, `"web"`, `AUTO == WEB` |
| `BODIES` | `static BODIES: LazyLock<Value>` | Text verbatim, key order kept |
| `OPTION_TO_BODY` | `const OPTION_TO_BODY: [(&str, &str); 2]` | Python's dict order |
| `questions` | `questions() -> Value` | `py_dumps(questions())` is byte-identical to Python's `json.dumps(questions())` (checked by running both) |
| `BodyAnswer` | `struct BodyAnswer` (`BodyAnswer::new(p_auto, accounts, mac, public, show, interact)`) | Dataclass defaults: `ms` 0.0, `error` "" (`show`/`interact` default 0.0 in Python; in Rust all six are given to `new`, or set as fields) |
| `BodyGates` | `struct BodyGates` (`BodyGates::new` positional in field order; `Default` = 0.9, 0.1, 0.1, 0.9, 0.5, 0.5) | |
| `ask` | `ask(&Predict, goal, &Clock) -> Result<BodyAnswer, RouterError>` | Request whitespace collapsed as `" ".join(str.split())` (Python `isspace`, incl. U+001C–U+001F). A predict error abstains: `BodyAnswer(0, 1, 1, 0, 1, 1, ms, "<Class>: <msg>"[:160])`; a reply that is not a dict is caught the same way (`AttributeError: '<type>' object has no attribute 'get'`). Missing or non-number Nouls fail closed (safety 1.0, public 0.0; a bool is not a number). The Choice's `web_reader` probability accepts a bool as `float(True)`. A truthy non-dict `answers` / `body` / `probabilities` / Noul entry raises `AttributeError` outside Python's try: `Err(RouterError)` |
| `decide` | `decide(&BodyAnswer, &BodyGates) -> &'static str` | Same comparisons (strict maxima, inclusive minima) |
| `GRID`, `MAX_GRID` | same `pub const` arrays | |
| `fit` | `fit(&[BodyAnswer], &[&str]) -> Result<BodyGates, FitError>` | Same grid order, same tie key `(right, public, auto, -acc, -mac, -show, -inter)`, same placeholder start `BodyGates(1.01, 0, 0, 1.01, 0, 0)`. Mismatched lengths raise as `zip(strict=True)` does (`zip() argument 2 is shorter/longer than argument 1`), including `any()` short-circuiting before the mismatch. Checked against Python's `fit` on a 5-answer set |
| `GATES`, `SHIPPED` | same `pub const`s | `BodyGates(0.6, 0.1, 0.1, 0.95, 0.5, 0.2)`, `true` |
| `make_router` | `make_router(Arc<Predict>, Arc<Clock>, BodyGates) -> Router`; `Router::route(goal) -> Result<&'static str, RouterError>` | Pass `GATES` for Python's default |
| `jev_router` | `jev_router(&impl eve_jev::Env) -> Option<Router>` | `eve_jev::route_config(env, "")`, `make_predict(..., timeout_s=2.0)`, `eve_decider::perf_counter`. Any config error is `None`. Pass `eve_jev::ProcessEnv` for Python's `environ=None` |
| (exception) | `RouterError { kind, message }`, `FitError` | The exceptions Python lets escape `ask`/`route` and `fit` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_browser_router.py` | `test_router_needs_every_condition`, `test_a_safety_answer_the_model_leaves_out_keeps_codex`, `test_router_state_is_the_request_only_and_the_bodies_are_structured`, `test_fit_never_allows_an_unsafe_route`, `test_the_shipped_gates_are_the_fitted_ones_and_every_set_is_labelled` | Same names in `src/tests.rs` (fixtures read from `bridge/tests/fixtures/routes`) | `test_the_wrapper_sends_a_routed_task_to_its_named_body_and_keeps_reflexes_first`, `test_named_bodies_alone_are_enough_and_a_broken_router_keeps_codex`: they test `app_reflex.ReflexFirstAgent`'s body seam and belong to the app-reflex crate |
| `bridge/tests/test_web_reader.py` (ref) | the `browser_router.SHIPPED` check | `SHIPPED` asserted in `test_the_shipped_gates_are_the_fitted_ones_and_every_set_is_labelled` | `web_reader.configured` belongs to the web-reader crate |
| (parity) | | `a_failed_predict_abstains_with_its_class_name_and_ms`, `a_reply_that_is_not_a_dict_is_caught_but_misshapen_answers_raise`, `the_choice_probability_takes_a_bool_but_a_noul_does_not`, `the_request_collapses_python_whitespace`, `the_questions_keep_python_order_and_wording`, `constants_and_defaults_match_python`, `decide_uses_strict_maxima_and_inclusive_minima`, `fit_with_nothing_safe_routes_nothing_and_ties_go_strict`, `fit_with_mismatched_lengths_raises_like_a_strict_zip`, `fit_picks_the_gates_python_picks`, `jev_router_is_none_without_a_key_and_asks_with_a_two_second_budget`, `the_ms_is_measured_on_the_clock` | |

## Boundaries
None

## Differences
- The exceptions Python lets escape are values: `ask`/`Router::route` return `Err(RouterError)` where Python raises `AttributeError`, and `fit` returns `Err(FitError)` where Python raises `ValueError`. Callers treat a router error as Python's callers treat a raise (Codex).
- `jev_router` takes the env explicitly instead of defaulting to `os.environ`; pass `eve_jev::ProcessEnv`.
- `BodyAnswer::new` takes `show` and `interact` explicitly (Python defaults them to 0.0).
