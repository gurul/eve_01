# eve-eye-model
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/eye_model.py` | `src/lib.rs` (tests in `src/tests.rs`; laya-mlx side: `src/laya_eye_helper.py`, embedded with `include_str!`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `CRITERIA` | `CRITERIA: [(&str, &str); 18]`, `criterion(label)` | Same order and text (checked byte for byte against the Python module's `json.dumps`) |
| `LABELS` | `LABELS: [&str; 18]`, `label_index(label)` | `tuple(CRITERIA)`, the wire order |
| `DEFAULT_TTL_MS`, `TTL_MIN_MS`, `TTL_MAX_MS` | same `pub const`s (`i64`) | 4000, 500, 6000 |
| `TTL_MS` | `TTL_MS: [(&str, i64); 18]`, `ttl_ms(label)` | In LABELS order, as the Python dict is |
| `QUESTION` | `QUESTION: LazyLock<Value>` | Byte-identical JSON, same key order |
| `EYE_CHOICE` | `EYE_CHOICE: LazyLock<Value>` | Byte-identical JSON, same key order |
| `WARM_STATE` | `WARM_STATE: &str` | |
| `class JevEyeModel` | `struct JevEyeModel` (pub `model`), `JevEyeOptions`, `impl EyeModel` | `JevEyeModel(env, opener=, timeout_s=)` → `JevEyeModel::new(&env, JevEyeOptions { opener, timeout_s, meter, ledger })`; `JevEyeModel()` → `JevEyeModel::from_env()`. Route and key from `eve_jev::route_config(env, "")`; `timeout_s or jev.timeout_from_env(env)`; construction fails with `JevError` when no key is set. `predict` reads `(body.get("answers") or {}).get("eye")["probabilities"]`, takes `float(probs.get(label) or 0.0)` for each label with Python's truthiness and `float()` (strings parsed, `True` is 1, a list or dict is `TypeError`, a truthy non-dict `answers` is `AttributeError`), sums like Python 3.12+ `sum()` (Neumaier), refuses a non-finite, non-positive or negative distribution, normalizes, and picks the first maximum. Messages verbatim: `jev returned no eye probabilities`, `jev returned unusable eye probabilities` |
| `JevEyeModel.predict` | `EyeModel::predict(&self, &str) -> Result<EyeAnswer, EyeError>` | `EyeAnswer { label, probabilities, ms }`, `to_value()` gives Python's dict in key order. `EyeError::kind()` is `type(exc).__name__` (`ValueError`, `JevError` or what Jev let through, `TypeError`, `AttributeError`, laya's own) and `Display` is `str(exc)`, so the worker's `f"{type(exc).__name__}: {exc}"` reads the same |
| `class ConversationContext` | `struct ConversationContext` (pub `turns`) | `turns` is a `Vec` in first-seen role order (a dict keeps insertion order on update) |
| `ConversationContext.state` | `ConversationContext::state(&mut self, who, text, now: f64)` | `previous[-400:]` by code point; `now - at <= 90`; only `user` / `assistant` are remembered. Checked against the Python on a 60-turn script with non-BMP text |
| `class LiveEyeModel` | `struct LiveEyeModel` (pub `path`), `impl EyeModel` | `LiveEyeModel(path)` → `LiveEyeModel::new(path)` (or `with_python(path, python, env)`). Loads `laya_mlx.Agent(str(path), dtype="float16", device="gpu", compile=False)`, sets the scorer to `mx.float32` and `mx.eval`s its parameters, in the helper process (see Boundaries). A failed load is the helper's exception by class and text (`ModuleNotFoundError: No module named 'laya_mlx'`, …) |
| `LiveEyeModel.predict` | `EyeModel::predict` | `agent.prepare(state, QUESTION)`, the `max_len` budget check, `forward(collate_items(...))`, float64 yes-minus-no scores, the finiteness check and numpy's softmax run verbatim in the helper; the label is the first maximum (`argmax`). Messages verbatim: `eye context exceeds token budget`, `nonfinite eye scores` |
| — | `trait EyeModel`, `enum EyeError`, `struct EyeAnswer`, `struct JevEyeOptions`, `default_python()` | The contract Python duck-types (`model.predict(state)` returning a dict) and the seams |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_eye_model_labels.py` | `test_labels_are_the_original_eleven_then_the_seven_in_wire_order`, `test_the_dropped_label_is_gone_everywhere`, `test_new_criteria_are_the_designs_text_and_the_old_ones_are_untouched`, `test_every_question_carries_every_label`, `test_ttls_follow_the_design_and_clamp`, `test_blind_state_is_built_as_production_builds_it` | same names in `src/tests.rs` (reading `docs/expressions-design.json` as Python does) | The `ev.FOLD` assertion in `test_the_dropped_label_is_gone_everywhere`, the `expression_ttl_ms` lines of `test_ttls_follow_the_design_and_clamp` and the `ev.blind_state` half of `test_blind_state_is_built_as_production_builds_it` test `tools/jev_eyes_eval.py` (not part of eve_bridge, not in the plan) and `live_expressions` (eve-live-expressions); the context half is ported with the expected string written out |
| `bridge/tests/test_eye_model_labels.py` | `test_the_picker_sends_the_labels_own_ttl`, `test_the_phone_event_carries_each_new_label`, `test_every_label_is_a_valid_pick` | None here | They exercise `LiveExpressions` and `PhoneCalls`: eve-live-expressions and eve-phone-call |
| `bridge/tests/test_eye_model_labels.py` | `test_fold_covers_every_label_onto_the_six_and_none_new_to_startled`, `test_legacy_scoring_folds_and_the_verdict_holds_both_bars`, `test_blind_scoring_math_and_bar`, `test_blind_set_must_be_well_formed`, `test_blind_mode_prints_the_marker_and_exits_by_the_bar`, `test_dev_set_is_well_formed_and_covers_each_new_label` | None | They test the evaluation tool `tools/jev_eyes_eval.py`, which is outside eve_bridge and stays Python |
| `bridge/tests/test_live_expressions.py` (ref) | `test_context_has_speaker_roles_and_expires`, `test_jev_eye_model_asks_one_choice_over_the_eleven_labels_and_answers_in_label_order`, `test_jev_eye_model_refuses_an_answer_without_probabilities_and_needs_a_key` | same names; the `LiveExpressions.label` acceptance is checked by a helper that applies its rules | The worker's own tests belong to eve-live-expressions |
| `bridge/tests/test_phone_expressions.py` (ref) | (imports `LABELS` only) | Covered by the label tests | The phone path belongs to eve-phone-call / eve-live-expressions |
| (parity) | | `constants_match_the_python_module_byte_for_byte`, `conversation_context_matches_python_on_a_long_script`, `context_keeps_the_first_seen_role_order_trims_to_400_and_skips_diary`, `probabilities_are_read_with_pythons_float_and_falsiness`, `a_missing_or_malformed_answer_raises_as_python_does`, `a_jev_failure_passes_through_by_its_class`, `the_timeout_is_the_argument_else_eve_jev_timeout_else_three_seconds`, `the_model_and_route_come_from_jev_route_config`, `python_float_strings`, `sums_like_python_and_argmax_takes_the_first`, `the_answer_serialises_as_pythons_dict`, `laya_loads_as_python_does_and_answers_in_label_order`, `laya_refuses_an_over_budget_context_and_nonfinite_scores_then_keeps_answering`, `a_failed_laya_load_raises_its_own_exception`, `a_missing_interpreter_is_a_load_failure`, `a_helper_that_dies_is_reloaded_on_the_next_turn`, `the_real_laya_checkpoint_matches_the_python_model` (`#[ignore = "live: ..."]`) | The laya tests run the real helper under `python3` with fake `laya_mlx` / `mlx` modules on PYTHONPATH (load kwargs, dtype and eval calls asserted). The live test was run on 2026-10-19 against `~/.config/eve-bridge/models/laya-multilingual-mlx`: label and all 18 probabilities equal to the Python `LiveEyeModel`'s |

## Boundaries
- **laya-mlx (Python-only MLX runtime).** `LiveEyeModel`'s agent lives in a helper Python process running
  `src/laya_eye_helper.py` (passed with `-c`, cwd `/`, arguments: the model path and `QUESTION` as JSON). Spawning
  it is the constructor (the Python load code runs there verbatim); it answers one JSON object per line: a status
  line, then `{"probabilities": [...]}` or `{"kind", "error"}` per `{"state"}` request. Library output on stdout
  is redirected to stderr so it cannot corrupt the protocol. Dropping the model kills the process.
- **Which Python.** `EVE_EXPRESSION_PYTHON` (new; the interpreter that has laya-mlx), else the bridge checkout's
  venv (`bridge/.venv/bin/python3`, resolved from this crate's location at build time) when it exists, else
  `python3` on PATH. A missing interpreter is a load failure (`FileNotFoundError`), as a missing laya_mlx is.

## Differences
- `LiveEyeModel`'s `ms` includes the pipe round trip to the helper (well under a millisecond).
- If the helper process dies mid-turn, that turn fails with `RuntimeError: laya eye helper exited (…)` and the
  next `predict` loads a fresh helper; Python's in-process agent cannot die separately from the daemon.
- Probabilities cross the helper boundary as JSON: Python's `json.dumps` writes the shortest round-trip repr and
  serde_json's `float_roundtrip` feature (enabled in this crate's manifest) reads back the same f64.
- `float()` of a string accepts ASCII digits only; Python's also takes other Unicode decimal digits.
- An integer probability too large for an f64 reads as infinity (then "unusable eye probabilities"); Python's
  `float()` raises `OverflowError` for it. Jev sends floats.
