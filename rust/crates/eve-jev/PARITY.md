# eve-jev
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/jev.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `ROUTES` | `ROUTES: [(&str, Route); 2]`, `route(name)` | Same order, URLs, key names and default models |
| `DEFAULT_ROUTE`, `DEFAULT_TIMEOUT_S`, `MAX_LEN`, `HEAD_MAX_LEN`, `RETRY_STATUS`, `RETRIES` | same `pub const`s | "typesafe", 3.0, 32000, 30000, {429, 500, 502, 503, 529}, 2 |
| `JevError` | `JevError(String)` (`thiserror`) | One line, no traceback. A `predict` failure is `PredictFailure::Jev(JevError)` |
| `Meter` | `Meter` (`parking_lot::Mutex` state), `MeterSnapshot` | |
| `Meter.add` | `Meter::add(&Value, ms, error)` | Tokens only from a non-bool JSON integer under an object `usage`, clamped at 0 |
| `Meter.snapshot` | `Meter::snapshot() -> MeterSnapshot` | Serializes as `{"calls", "input_tokens", "ms", "errors"}`; `ms` rounded half to even like `round()` |
| `Meter.delta` | `Meter::delta(&before, &after)` | |
| `METER` | `static METER: Meter` | Process-wide; `PredictOptions.meter` can point a predict elsewhere (tests) |
| `meter_spend` | `meter_spend(url, model, &payload)`, `meter_spend_in(&Ledger, …)` | Reported `usage.cost` (non-bool number), else `estimate_jev_cost(tokens)` when usage is non-empty, else unpriced with note "no usage in the reply"; provider `openrouter` when the URL contains `openrouter.ai`, else `typesafe`; feature `eve_spend::JEV` |
| `timeout_from_env` | `timeout_from_env(&impl Env) -> f64` | Python `float()` parsing incl. `_` digit groups and `inf`/`nan`; same warning text (`%r` of the first 20 chars, `%.1f`) via `tracing::warn!` |
| `route_config` | `route_config(&impl Env, route) -> Result<(url, key, model), JevError>` | Messages verbatim: `EVE_JEV_ROUTE must be one of ('typesafe', 'openrouter'), not '…'`, `<KEY> is not set (route <name>)` |
| `make_predict` | `make_predict(url, key, model, PredictOptions) -> JevPredict`; `JevPredict::predict(&state, &questions) -> Result<Value, PredictFailure>` | `PredictOptions::default()` holds Python's defaults (`timeout_s` 3.0, real HTTPS, `time.sleep`). Opener seam: trait `Opener` (`Fn(&Request, timeout_s) -> Result<Vec<u8>, OpenError>`); `OpenError` is urllib's `HTTPError` / `URLError` / caught `ValueError` / an exception urllib lets through. Body is byte-for-byte `json.dumps` (`py_dumps`: `", "`/`": "`, `ensure_ascii`, Python float repr). Retries 0.2 s, 0.4 s. `decider_fn()` adapts to `eve_decider::Predict` (kind `JevError`), `anyhow_fn()` to the typed askers' seam |
| `normalize` | `normalize(Value, &questions) -> Value` | Top-level answers are gathered in the questions' order; an existing `answers` key keeps its position, as `{**rest, "answers": …}` does |
| `load` | `load(&impl Env, LoadOptions) -> Result<Decider, LoadError>` | `LoadOptions::default()` is Python's (`style` "jev", route "", timeout 0 → env). `LoadError.kind` is `RuntimeError` (no key, failed warm-up) or `ValueError` (bad style, from `Decider`); messages verbatim. Pass `ProcessEnv` for `os.environ` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_jev.py` | 6 | All 6 with the same names in `src/tests.rs` | None |
| `bridge/tests/test_jev_grouped.py` | 9 | None here | They test `typed_ask.ask_jev_step_grouped`, `make_step_asker` and `fast_lane.run_delegate` with a fake predict; no line exercises `jev.py`. They belong to eve-typed-ask and eve-fast-lane |
| `bridge/tests/test_jev_step.py` | 12 | None here | Same: `typed_ask.ask_jev_step`, `StepGates`, `fit_step_gates` and the lane's jev mode, with a fake predict. They belong to eve-typed-ask and eve-fast-lane |
| `bridge/tests/test_spend_wiring.py` (ref) | `test_a_jev_decision_is_metered_on_its_routes_bill` (×2) | `test_a_jev_decision_is_metered_on_its_routes_bill` (both cases), plus `a_reply_without_usage_is_recorded_unpriced_with_a_note` | The other spend-wiring tests meter other crates |
| `bridge/tests/test_computer_agent.py` (ref) | `test_the_run_log_ends_with_the_bill`, `test_jev_calls_during_a_run_are_billed` (their `jev.METER` part) | `the_meter_counts_calls_tokens_ms_and_errors_and_the_delta_is_key_by_key` | The run log and bill belong to eve-computer-agent |
| `bridge/tests/test_desktop_worker.py`, `test_live_expressions.py` (ref) | use `jev.load` / an opener | Covered by the `load` and opener tests | The worker and eye model belong to their crates |
| (parity) | | `route_config_reads_the_route_argument_first_and_reports_names_as_python_does`, `timeout_from_env_parses_like_python_float`, `the_request_body_is_python_json_dumps`, `failures_are_metered_as_errors_and_a_bare_exception_passes_through_unmetered`, `the_decider_sees_a_jev_error_by_its_python_class`, `normalize_keeps_rest_order_and_an_existing_answers_slot`, `load_reads_the_timeout_from_env_and_rejects_a_bad_style_as_value_error`, `the_real_transport_reports_an_unreachable_host_as_one_line`, `the_real_transport_works_inside_a_tokio_runtime`, `routes_and_constants_match_python` | |

## Boundaries
None

## Differences
- The transport is reqwest's blocking client instead of urllib. Its `timeout` bounds the whole request (urllib's bounds each socket operation). Failures map to urllib's classes: connect/send failures and connect timeouts are `URLError` (`… unreachable: <reason>`, where `<reason>` is the innermost error's text rather than urllib's `[Errno N] …` wording); a timeout waiting for the reply is the bare `TimeoutError: timed out` urllib lets through (unmetered, as in Python); a broken body read is a bare `OSError`.
- reqwest follows redirects (307/308 keep the POST); urllib refuses to redirect a POST on 307/308 and raises `HTTPError`. Neither route redirects.
- reqwest sends its own `User-Agent`/`Accept-Encoding` headers rather than urllib's `Python-urllib/3.x` and `identity`.
- Called from inside a tokio runtime, the HTTP work runs on a scoped thread so the blocking client is never used on a runtime worker. Python's predict is called from plain threads.
- A reply using the JSON literals `NaN` / `Infinity` is undecodable (`JSONDecodeError`); Python's `json.loads` accepts them.
- `normalize` gathers top-level answers in the questions' key order; Python iterates a `set`, whose order is arbitrary.
- `timeout_from_env` does not accept non-ASCII Unicode digits, which Python's `float()` does.
- A process env value that is not valid UTF-8 reads as unset (Python's `os.environ` would surrogate-escape it).
