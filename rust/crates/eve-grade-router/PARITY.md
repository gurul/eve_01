# eve-grade-router
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/grade_router.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `Predict` | `Predict<'a> = &dyn Fn(&Value, &Value) -> anyhow::Result<Value>` | `eve_jev::JevPredict::anyhow_fn()` fits it; an `Err` is the exception, its class read by `exception_name` |
| `INSTANT`, `THINK`, `GRADES`, `MODES` | same `pub const`s | `"instant"`, `"think"`, `("auto", "on", "off")` |
| `TIMEOUT_S`, `GRID`, `MIN_TUNING_RECALL`, `DEFAULT_GATE`, `SHIPPED` | same `pub const`s | 0.8; the 19 literals `round(0.05 * i, 2)` gives (a test proves equality); 0.95; 0.5; `true` |
| `GATE_FILE` | `static GATE_FILE: LazyLock<PathBuf>` + `GATE_JSON` (the file's bytes, `include_str!`) | The path is the Python module's own file in `bridge/src/eve_bridge/` |
| `ABOUT`, `NOUL` | same `pub const`s | Verbatim |
| `questions` | `questions() -> Value` | `{"think": {"type": "noul", "instructions": NOUL}}` |
| `state` | `state(text, previous) -> Value` | Whitespace collapsed as `" ".join(s.split())` (Unicode whitespace plus `\x1c`-`\x1f`), clipped to 500 characters; keys `about`, `previous`, `request` in order |
| `GradeAnswer` | `struct GradeAnswer { noul, ms, error }` (`Default`, `new`, `failed`) | |
| `GradeAnswer.to_json` | `GradeAnswer::to_json() -> Value` | `round(noul, 4)`, `round(ms, 1)` half to even |
| `GradeAnswer.from_json` | `GradeAnswer::from_json(&Value) -> Result<GradeAnswer, Raised>` | `float(x or 0.0)` (numbers, bools, numeric strings with `float()`'s `_` and whitespace rules), `str(x or "")`; a value `float()` refuses is the `ValueError`/`TypeError` Python raises, a non-object `raw` the `AttributeError` |
| `ask` | `ask(predict, text, previous, &mut clock) -> Result<GradeAnswer, Raised>` | A raising predict, or a reply that is not an object, is an answer with `error` = `"<Class>: <message>"[:160]`; `Err` only where Python raises out of `ask` (a truthy non-object `answers`: `AttributeError`). Noul read only from a non-bool JSON number |
| `decide` | `decide(&a, gate, budget_ms) -> &'static str` | Pass `f64::INFINITY` for Python's default budget |
| `fit` | `fit(&answers, &acceptable) -> Result<(gate, recall), Raised>` | `ValueError` for no think-only case and for `zip(strict=True)` length mismatches, messages verbatim |
| `bar` | `bar(&grades, &acceptable, &answers) -> Result<Bar, Raised>`; `Bar` serializes as the dict, same keys in the same order (`"1"`..`"4"` renamed fields) | The kept holdout `bar` in `grade_result.json` round-trips byte for byte (test) |
| `_GATE` | `GateCache` (`OnceLock<f64>`) and `static GATE` | Read once a process |
| `load_gate` | `load_gate(&Path) -> f64`; `load_shipped_gate()` | DEFAULT_GATE on any failure. See Differences for `load_shipped_gate` |
| `mode` | `mode(&impl eve_jev::Env) -> &'static str` | `EVE_GRADE_ROUTER`, unset or empty is `auto`; stripped (Python's whitespace, `\x1c`-`\x1f` included), lowercased; unknown warns `grade router: EVE_GRADE_ROUTER='x' is not one of ('auto', 'on', 'off'); auto` |
| `Grade` | `struct Grade { grade, noul, ms, asked, error }` (`Default` = instant, not asked) | |
| `Grade.log_line` | `Grade::log_line()` | `"{grade}; jev off"` or `"{grade} (noul {:.2}); jev {:.0} ms"` + `" ({error})"` |
| `_JEV`, `_JEV_LOCK` | `static JEV: LazyLock<Mutex<Option<JevCache>>>` | Remade only when `(url, model)` changes |
| `make_grader` | `make_grader(&impl eve_jev::Env) -> Option<Grader>` | `None` when off, auto while not SHIPPED, or `eve_jev::route_config` fails. Jev predict with `timeout_s` = TIMEOUT_S, clock `perf_counter` |
| `grade_turn` | `grade_turn(text, previous, grader, gate, timeout_s) -> Grade`; `grade_turn_in(&GateCache, loader, …)` is the seam the Python test patches | Blocking (run it with `spawn_blocking`). The grader runs on a thread named `grade-router`, waited on for `max(0, timeout_s)`; errors `"timeout"`, `"not an answer"`, or the raised class name |
| (helper) | `exception_name(&anyhow::Error)`, `Raised`, `perf_counter()` | `type(e).__name__`: from `eve_jev::PredictFailure`, `eve_jev::JevError`, `eve_decider::PredictError`, `Raised`, else `Exception` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_grade_router.py` | 10 (11 with the parametrized case) | All 10 with the same names in `src/tests.rs` (both parametrized cases in one test) | None. `test_the_fitted_gate_ships_inside_the_package` checks pyproject's package-data by text rather than `tomllib`; `test_shipped_equals_the_kept_holdout_verdict` does `grade_eval.check_default` on `tests/fixtures/grade_result.json` and also compares the whole kept `bar` dict |
| `bridge/tests/test_link_grade.py` (ref) | `test_grade_router_off_makes_no_grader` | `test_grade_router_off_makes_no_grader` (plus the no-key and once-per-route cases) | The other tests drive `telegram.TelegramInlet` and the Mini App server: they belong to the telegram/link crates. Their grade-router facts (log line `instant (noul 0.00)…` on failure, timeout is instant) are covered by `the_log_line_without_jev_and_with_an_error` and the timeout test |
| `bridge/tests/test_small_talk.py` (ref) | uses a recording grader | None here | It tests that the small-talk lane and the think grade never call the grader: the inlet's behaviour, not this module's |
| (parity) | | `the_tuning_answers_refit_to_the_shipped_gate` (fit over `grade_tuning_answers.json` gives `grade_gate.json`'s gate and recall), `fit_and_bar_fail_as_python_does`, `an_empty_bar_fails_every_gate`, `ask_on_a_malformed_reply_errs_or_raises_where_python_does`, `ask_sends_the_state_and_the_question_and_names_a_jev_failure`, `a_jev_answer_through_the_real_predict_grades_think`, `a_grader_that_panics_is_not_an_answer`, `the_log_line_without_jev_and_with_an_error`, `mode_reads_eve_grade_router`, `grade_answers_round_trip_their_json`, `load_gate_falls_back_on_a_missing_or_broken_file`, `the_grid_is_pythons_rounded_steps` | |

## Boundaries
None

## Differences
- `load_shipped_gate` (what `grade_turn` reads once) reads `GATE_FILE` when it exists, exactly as Python's `load_gate()`; when the binary runs where that file is absent it uses the copy built into the binary (`GATE_JSON`, the same bytes at build time) instead of falling back to DEFAULT_GATE. That is the property the Python package-data fix exists for.
- `bar` returns a typed `Bar`; with no successful answer `p95_ms` is infinity, which `serde_json` writes as `null` where Python's `json.dumps` writes `Infinity`.
- A grader that panics (Rust's uncaught failure) leaves no answer: `grade_turn` reports `"not an answer"`; Python would have caught an exception and reported its class name.
- `GradeAnswer.from_json`'s `str()` of a non-string `error` prints containers as JSON and very large/small floats in Rust's notation rather than Python's repr. Kept files only hold strings there.
- `grade_turn` with an infinite `timeout_s` (or one past `threading.TIMEOUT_MAX`) waits for the grader with no limit; Python's `Event.wait` raises `OverflowError` out of `grade_turn` there. Callers pass TIMEOUT_S.
- `ask`'s clock is `&mut dyn FnMut() -> f64` in seconds (`time.perf_counter` is `perf_counter()`, seconds since first use).
