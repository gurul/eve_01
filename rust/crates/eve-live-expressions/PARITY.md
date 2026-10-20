# eve-live-expressions
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/live_expressions.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_MODEL` | `DEFAULT_MODEL: &str` (unexpanded) | Expanded in the constructor with the same `Path(...).expanduser()` rules (`py_path`) |
| `BACKENDS`, `DEFAULT_BACKEND`, `OFF_WORDS` | same `pub const`s | |
| (inline) | `DEFAULT_SWITCH_FILE`, `POLICY` | `~/.config/eve-bridge/expressions.json`; `experimental top choice; not calibrated` |
| `phone_expressions_on(env=None)` | `phone_expressions_on(&impl Env)` | `env=None` is `&eve_jev::ProcessEnv`. `(get or "").strip().lower() not in OFF_WORDS`, with Python's whitespace set (Unicode White_Space plus U+001C..U+001F, checked against `str.isspace` over every code point) |
| `backend_from_env(env=None)` | `backend_from_env(&impl Env) -> &'static str` | Same warning text, `%r` of the first 20 characters via `py_repr`, `('jev', 'laya')` |
| `expression_ttl_ms(label, age_s)` | `expression_ttl_ms(&str, f64) -> i64` | `TTL_MS.get(label, TTL_MIN_MS)` via `eve_eye_model::ttl_ms`, `int()` truncation, clamp to 500..6000 |
| `class LiveExpressions` | `struct LiveExpressions` | `LiveExpressions(send, *, connected, muted, phase, path, model_factory, clock, enabled, lazy)` → `LiveExpressions::new(SendFn, Options { connected, muted, phase, path, model_factory, clock, enabled, lazy, env })`; `env` (new, default `os.environ`) is what the constructor and the default load read. Attributes Python reads directly are `pub` (`lazy`, `path`, `backend`, `model_path`) or in `State` behind `state()` (`enabled`, `model`, `ready`, `error`, `pending`, `seq`, `generation`, `last_text`, `context`, `last_sent_at`, `last`, `board`, `board_history`, `sent`, `dropped`, `closed`), with getters `enabled()`, `ready()`, `pending()`, `sent()`, `dropped()`, `error()`, `last()`, `connected()`, `muted()`, `phase()` |
| `LiveExpressions.__init__` | `LiveExpressions::new` | Switch file `Path(path or EVE_EXPRESSIONS_FILE or default).expanduser()` read only when `enabled` is not given; on only for a JSON object whose `enabled` is `true` (unreadable, non-UTF-8, non-JSON, non-object: off). `EVE_EXPRESSION_MODEL` (empty string is `.` as pathlib makes it). `seq = max(1, int(time.time()*1000) & 0xFFFFFFFF)` |
| `LiveExpressions._load` | private `load(backend, model_path, env)`, the default `ModelFactory` | `LiveEyeModel(model_path)` for laya, else `JevEyeModel` from the env (its `model` id kept as `LoadedModel.name`), then `predict(WARM_STATE)` |
| `LiveExpressions._next_id` | private `State::next_id` | `(seq + 1) & 0xFFFFFFFF or 1` |
| `LiveExpressions.offer` | `offer(&self, who, text) -> Option<u32>` | Speakers `user`/`assistant`/`diary`/`demo`; `" ".join(text.split())[-800:]` by code point; same-line dedup; a replaced pending turn counts as dropped; state from `ConversationContext::state(who, text, clock())`, offered at a second `clock()` |
| `LiveExpressions.set_enabled` | `async set_enabled(&self, on) -> Result<(), Raised>` | Resets generation, pending, last_text and context; writes `{"enabled": true}\n` (json.dumps spacing) to `<stem>.tmp` then renames (parents created); a write failure is the `OSError` subclass with Python's `[Errno N] strerror: 'path'` text, after the state changed (as Python's); off + connected sends `{"cmd": "expression", "id": <next>, "clear": true}` and propagates a send exception |
| `LiveExpressions.observe` | `observe(&self, &Value)` | `isinstance(id, int)` (a bool counts); copies kept, last 24 |
| `LiveExpressions.status` | `status(&self) -> Value` | Same keys in the same order; `model` is the Jev id (or `""`) for jev, `str(model_path)` for laya; `phase()` last |
| `LiveExpressions.label` (staticmethod) | `LiveExpressions::label(&Value)` and `LiveExpressions::label_values(&[f64])` | Length 18, numbers only (bool refused), finite, 0..1, `abs(sum - 1) <= 1e-4` with Python 3.12's compensated `sum`, first maximum; else `ValueError: invalid expression probabilities` |
| `LiveExpressions.run` | `async run(self: Arc<Self>)` | Same loop: (lazy) load in a blocking thread, ready/error, lazy load failure drops the pending turn, 10 s retry; wait on the wake; 1.2 s gap then take the newest; drop after 4 s or when disconnected; predict in a blocking thread; drop on switch/generation/age/disconnect after inference; `{"cmd","id","label","ttl_ms","chirp": false}`; `send` returning `False` is `RuntimeError: expression send failed`; `last` with the same keys; errors `f"{type}: {exc}"[:180]`. Log lines verbatim. Dropping or aborting the task sets `closed` (`finally`) |
| — | `Raised`, `SendFn`/`SendFuture`/`send_fn`, `Flag`, `PhaseFn`, `Clock`, `ModelFactory`, `LoadedModel`, `Options`, `Pending`, `State`, `monotonic()` | The duck-typed callables and the exception `type(exc).__name__` / `str(exc)` pair made explicit |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_live_expressions.py` | `test_queue_and_arbitration` (3 params), `test_disabled_during_inference_drops_result`, `test_stale_and_send_failure`, `test_invalid_results` (4 params), `test_every_expression_is_accepted_without_startle`, `test_context_has_speaker_roles_and_expires`, `test_the_expression_backend_is_jev_unless_laya_is_spelled_out` (5 params) | same names in `src/tests.rs` (params looped) | |
| `bridge/tests/test_live_expressions.py` | `test_jev_eye_model_asks_one_choice_over_the_eleven_labels_and_answers_in_label_order` | `test_jev_eye_model_answer_is_accepted_by_the_worker_as_is` (the `LiveExpressions.label(answer)` line) | The request-shape assertions test `JevEyeModel`, ported in eve-eye-model |
| `bridge/tests/test_live_expressions.py` | `test_jev_eye_model_refuses_an_answer_without_probabilities_and_needs_a_key` | None here | Tests eve-eye-model only; ported there |
| `bridge/tests/test_live_expressions.py` | `test_laya_keeps_original_caption_chirps` | None | Tests `Daemon._on_caption` (eve-daemon) |
| `bridge/tests/test_eye_model_labels.py` (ref) | `test_ttls_follow_the_design_and_clamp` (the `expression_ttl_ms` lines), `test_the_picker_sends_the_labels_own_ttl`, `test_every_label_is_a_valid_pick` | same names | The TTL table and design-file lines are eve-eye-model's; `test_the_phone_event_carries_each_new_label` tests `PhoneCalls` (eve-phone-call); the `tools/jev_eyes_eval.py` tests stay Python |
| `bridge/tests/test_phone_expressions.py` (ref) | `test_switching_off_clears_the_face_and_writes_no_file`, `test_no_live_call_sends_nothing`, `test_a_model_fault_sends_nothing`, `test_a_late_pick_sends_nothing`, `test_a_lazy_picker_loads_only_at_the_first_offer_and_a_failed_load_drops_it`, `test_the_phone_switch` (7 params) | same names; the worker half of `test_react_goes_to_the_phone_when_no_board_is_connected` as `test_react_offer_returns_the_pending_event` | The Mini App / WebSocket / `PhoneCalls` / `Daemon` halves (`test_both_sides_...`, `test_a_hello_without_eyes...`, `test_a_picker_fault...`, `test_picks_reach_the_phone...`, `test_sentences_are_offered...`, `test_an_assistant_pick...`, `test_a_barge_in...`, `test_no_target...`, `test_a_malformed_command...`, `test_a_slow_pick...`, `test_the_daemon_wires...`, `test_the_daemon_leaves...`) belong to eve-phone-call, eve-miniapp and eve-daemon; the fake-model worker parts are covered above |
| (parity) | | `the_switch_file_is_read_as_python_reads_it`, `set_enabled_writes_the_switch_atomically_in_pythons_format`, `a_switch_that_cannot_be_written_raises_an_os_error`, `the_switch_path_comes_from_the_env_and_is_expanded`, `python_path_normalization` (expected values from CPython's pathlib), `offer_normalizes_whitespace_keeps_the_last_800_and_skips_repeats`, `event_ids_are_32_bit_and_never_zero`, `observe_keeps_integer_ids_and_the_last_24`, `status_has_pythons_keys_in_order`, `a_sent_cue_is_recorded_and_the_next_waits_its_gap`, `an_invalid_answer_is_a_value_error`, `a_send_exception_is_reported_by_its_class`, `an_aborted_worker_is_closed_and_takes_no_offers`, `errors_are_cut_to_180_characters`, `python_reprs`, `sums_like_python` | |

## Boundaries
None in this crate (the Laya backend's Python helper process is eve-eye-model's boundary).

## Differences
- `run_in_executor` on a one-thread pool becomes `tokio::task::spawn_blocking`; only the worker submits, one call
  at a time, so calls never overlap as before. A panic inside a model or factory is reported as
  `RuntimeError: <panic message>`.
- `time.monotonic` is measured from the first clock read in the process; only differences are used.
- The switch file is parsed by serde_json: a file Python's `json.loads` accepts only through its extensions
  (`NaN`, `Infinity`) reads as off here.
- `~user/...` paths are left as written (pathlib would look the user up); eve's defaults use `~/` only.
- `repr()` in the backend warning approximates `str.isprintable` (controls, separators and common format
  characters are escaped); it is a log line.
- An `observe` argument that is not a JSON object is ignored (Python's `event.get` would raise; the daemon only
  passes dicts).
