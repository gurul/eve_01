# eve-intent
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/intent.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `LEAVE`, `MUTE`, `UNMUTE`, `LOOK`, `LESSON`, `REMEMBER`, `NOTES`, `NONE`, `LABELS` | same-named `pub const`s | Strings and order verbatim. |
| `DEFAULT_MODEL`, `MIN_CONFIDENCE`, `TIMEOUT_SECS`, `MAX_WORDS` | same-named `pub const`s, plus `timeout()` as a `Duration` | `gpt-5.4-nano`, 0.6, 4.0 s, 25. |
| `_LEAD`, `_TRAIL` | `LEAD`, `TRAIL` | Same word sets. |
| `_UNMUTE`, `_MUTE`, `_LEAVE_ANYWHERE`, `_LEAVE_WHOLE`, `_LOOK_START`, `_LOOK_END`, `_NOT_HEAD`, `_LESSON`, `_NOT_LESSON`, `_REMEMBER`, `_NOTES` | `UNMUTE_RE`, `MUTE_RE`, `LEAVE_ANYWHERE`, `LEAVE_WHOLE`, `LOOK_START`, `LOOK_END`, `NOT_HEAD`, `LESSON_RE`, `NOT_LESSON`, `REMEMBER_RE`, `NOTES_RE` (`LazyLock<Regex>`) | Patterns verbatim. `re.match` patterns are all `^`-anchored already; `_NOTES`'s `re.IGNORECASE` is `(?i)`. |
| `_is_head_move(core)` | private `is_head_move(&str)` | |
| `normalize(text)` | `normalize(&str) -> String` | Lowercase, curly quotes folded, `[^a-z0-9' ]+` to a space, whitespace collapsed, "thank you" → "thanks". |
| `_core(words)` | private `core(&[&str]) -> &[&str]` | |
| `fast_intent(text)` | `fast_intent(&str) -> Option<&'static str>` | Same order of checks (core and last clause; anywhere-leave; head move; lesson; remember; notes). Checked against Python on 1816 phrases. |
| `PROMPT` | `PROMPT` | Byte-identical (a test pins Python's SHA-256). |
| `SCHEMA` | `SCHEMA: LazyLock<Value>` | Same keys in the same order. |
| `parse_intent(raw)` | `parse_intent(&str) -> Result<(&'static str, f64), ParseError>` (private `py_loads`, `rewrite_py_json`, `py_float_str`) | `json.loads` as Python reads it: serde first, then a rewrite for what only Python accepts (bare `NaN`/`Infinity`/`-Infinity`, a float literal beyond f64 as an infinity, an integer beyond f64 that only `float()` rejects, unpaired surrogate escapes, the 4300-digit integer limit raised where the scan reaches it). `str(x or NONE)` label rule; `float()` on the confidence with Python's string forms (whitespace, Unicode decimal digits, `_`, inf/nan), bools as 1/0, anything else 0; `max(0, min(1, x))` with Python's NaN behavior. Raises `JSONDecodeError` / `AttributeError` / `OverflowError` / `ValueError` as `ParseError::{Json, NotObject, Overflow, IntDigits}`. Checked against Python on 710 replies. |
| `class OpenAIIntentClient` | `struct OpenAIIntentClient` (`new`, `with_client`, `with_ledger`, `request`, `classify`; implements `IntentClient`) | `new` builds the OpenRouter client with the 4 s timeout and `max_retries(0)`. `classify` posts the request, meters it into spend under `voice` (provider `openrouter`), and parses `output_text or "{}"`. |
| (SDK `client.responses.create`) | trait `Responses`, implemented for `eve_openrouter::Client` and the private `OpenRouterResponses` the real client uses | The seam the Python test swapped (`c._client = ...`). Blocking, like the SDK call it replaces. `OpenRouterResponses` keeps one private runtime for calls made with no runtime current, so the connection pool lives as long as the client. |
| (SDK `Response.output_text`) | `output_text(&Value) -> String` | Every `output_text` part of every `message` item, joined. |
| (duck-typed `client.classify`) | trait `IntentClient` | What `make_classifier` wraps. |
| `Classifier` | `type Classifier = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>` | |
| `make_classifier(client, timeout, min_confidence)` | `make_classifier(Arc<dyn IntentClient>, Duration, f64) -> Classifier` | Over 25 words → None without a call; runs `classify` on tokio's blocking pool with the spend job tag carried (`asyncio.to_thread`); a timeout, error or panic logs `intent: classifier skipped (<Name>: <msg>)` and is None; a label under the floor or `none` is None; a hit logs `intent: <repr> → <label> (<conf>)`. |
| `make_intent_classifier(environ=None)` | `make_intent_classifier(Option<&HashMap<String, String>>) -> Option<Classifier>` | `EVE_INTENT` off-words `0/false/no/off`; no OpenRouter key → None; `EVE_INTENT_MODEL` blank → default. `None` reads the process environment. |
| `run_intent_test(pairs)` | `run_intent_test(&[(String, String)]) -> i32`, plus `run_intent_test_with(pairs, env, client, out, err)` | Same lines on stdout/stderr and exit codes (2 no key, 1 any wrong, 0 ok); `stay` means none. |
| (exception class names / `str(e)` in log and report lines) | `error_name`, `error_text`, `py_repr` | The SDK's class per HTTP status, `Request timed out.` / `Connection error.`, Python `repr()` quoting. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_intent.py` | `test_head_moves`, `test_lesson_requests`, `test_leave_phrases`, `test_mute_phrases`, `test_unmute_phrases`, `test_requests_about_the_computer_are_not_intents`, `test_empty_is_none`, `test_parse_intent_bounds_and_unknown_labels`, `test_classifier_returns_label_above_the_floor`, `test_classifier_below_floor_or_none_is_no_intent`, `test_classifier_timeout_and_error_are_no_intent`, `test_long_turns_are_not_classified`, `test_request_is_not_stored_and_is_strict_json`, `test_classify_parses_the_sdk_answer` | same names in `src/tests.rs` (each parametrized test loops over the same phrase list) | None |
| `bridge/tests/test_spend_wiring.py` (tests_ref) | `test_the_voice_intent_classifier_is_metered` | `test_the_voice_intent_classifier_is_metered` | The other tests in that file meter other modules' clients. |
| `bridge/tests/test_learning_think_aloud.py` (tests_ref) | uses `normalize` | `normalize_matches_python` | The think-aloud behavior belongs to the learning crate. |
| `bridge/tests/test_intent_eval.py` | all | none | It tests `bridge/tools/intent_eval.py`, the Telegram tool-choice eval harness (labels, code routes, the inlet `Rig`), not `intent.py`: none of it calls this module. It belongs with the telegram/menu-tools port. |
| (parity, new) | | `the_prompt_is_byte_identical_to_python`, `the_schema_and_request_serialize_as_python_writes_them`, `remember_and_notes_phrases`, `parse_intent_follows_python_float_and_truthiness`, `an_empty_answer_is_none_and_output_text_joins_message_parts`, `py_repr_quotes_like_python`, `make_intent_classifier_needs_the_key_and_respects_the_switch`, `the_real_client_is_on_openrouter_with_four_seconds_and_no_retries`, `run_intent_test_reports_like_python`, `error_names_follow_the_sdk`, `a_classify_through_the_real_client_reaches_a_local_server`, `parse_intent_reads_json_as_python_does`, `float_reads_unicode_digits_as_python_does`, `run_intent_test_from_inside_a_runtime_does_not_panic`, `the_real_client_keeps_its_runtime_between_sync_calls` | |

## Boundaries
None

## Differences
- `OpenAIIntentClient::classify` is blocking like the SDK call, but drives eve-openrouter's async client: on a tokio blocking-pool thread (where `make_classifier` runs it) it uses the current runtime, and with no runtime one private runtime kept for the client's life. It must not be called directly on an async worker thread; `run_intent_test` moves each call to a plain scoped thread when it runs inside a runtime, so an async `main` can run the intent-test command. Building a reqwest blocking client was avoided because constructing or dropping one inside a runtime panics.
- `make_intent_classifier`'s "openai SDK not importable" branch has no Rust cause; the same warning is logged if the client cannot be built. `run_intent_test` with an unbuildable client prints `intent-test: <error>` and returns 2 where Python would raise.
- The JSON error message text in a `JSONDecodeError` log line is serde's, not Python's (the class name matches).
- The exception names in log lines are mapped from the Rust error (SDK class per status code; an error a test client raises is `Exception`; a timeout is `TimeoutError` with an empty message, as `asyncio.wait_for` gives).
- `py_repr` escapes control, separator, format and private-use characters as Python's `repr()` does, but does not know every unassigned code point Python escapes.
