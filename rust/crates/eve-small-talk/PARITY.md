# eve-small-talk
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/small_talk.py` | `src/lib.rs` (tests in `tests/test_small_talk.rs` and `src/lib.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `SHIPPED`, `SWITCH_ENV`, `MODEL_ENV`, `DEFAULT_MODEL`, `FEATURE`, `MAX_TOKENS`, `TIMEOUT_S`, `VOICE_CHARS`, `VOICE_HEADING` | same-named `pub const`s | Values verbatim (`SHIPPED = true`, 60 tokens, 4.0 s, 800 chars). |
| `log` | (none) | The Python module never logs; nothing to port. |
| `enabled(environ=None)` | `enabled(Option<&Environ>) -> bool` | `None` reads the process environment. Same spellings, `str.strip()` + `lower()`. |
| `model_name(environ=None)` | `model_name(Option<&Environ>) -> String` | Stripped `EVE_SMALL_TALK_MODEL`, else DEFAULT_MODEL. |
| `PHRASES` | `PHRASES: &[&str]` | Verbatim, same order. |
| `_DROP`, `_SYMBOL`, `_KEYS`, `_LAUGH` | private `DROP`, `SYMBOL`, `WORD`, `KEYS`, `LAUGH` | Python's `\w` (`isalnum()` or `_`) is spelled `[\p{L}\p{N}_]` and its `\s` `[\s\x1C-\x1F]`, so combining marks split off as in Python. `_LAUGH.fullmatch` is an anchored regex. |
| `_squash(text)` | `squash(&str) -> String` (pub) | `(.)\1+` by hand (Rust regex has no backreferences); a newline run is kept, as Python's `.` skips `\n`. |
| `normalise(text)` | `normalise(&str) -> String` | Differentially checked against Python on ~4,600 inputs (phrases, emoji, skin tones, joiners, marks, odd whitespace): identical. |
| `matches(text)` | `matches(&str) -> bool` | |
| `class State` | `struct State` (`Default`, `PartialEq`, `Copy`) | Same five flags. |
| `gate(text, state)` | `gate(&str, &State) -> &'static str` | "", "no_match", "attachment", "tapped", "waiting". |
| `SYSTEM` | `SYSTEM: &str` | Verbatim (line continuations joined); a test pins the exact text. |
| `voice_from_profile(profile)` | `voice_from_profile(&str) -> String` | `splitlines()`, `strip()`, `lstrip("-*")` and the char-count clip as Python's. |
| `_escape(text)` | private `escape` | `&`, `<`, `>`. |
| `PROVIDER` | `PROVIDER: LazyLock<Value>` | `{"data_collection": "deny"}`. |
| `request(text, owner_voice="", *, model=DEFAULT_MODEL)` | `request(&str, &str, &str) -> Value` | Same body, key order `model`, `max_tokens`, `messages`. Callers pass `""` / `DEFAULT_MODEL` for Python's defaults. |
| `class Answer` (+ `answered`) | `struct Answer` + `Answer::answered()` | `reply`, `reason`, `secs`, `model`, `error`, `cost: Option<f64>`. |
| `_CLIENT` | private `CLIENT` (`Mutex<Option<(key, Client)>>`) | One client per process, rebuilt when the key changes. |
| `_client()` | `client(Option<&Environ>) -> Result<openrouter::Client, openrouter::Error>` | `eve_openrouter::async_client` with timeout TIMEOUT_S, `max_retries` 0. |
| `ask(text, owner_voice="", *, client=None, model=None, timeout_s=TIMEOUT_S, record_spend=True)` | `async fn ask(&str, &str, AskOptions) -> Answer` | `AskOptions::default()` is Python's defaults; it adds `ledger` (spend folder) and `env` as test seams. Never fails; same reasons, `error` = the Python exception's type name. Spend recorded as `small_talk` via `eve_spend::Ledger::record_chat_completion` (provider `openrouter`). |
| (SDK `client.chat.completions.create(**body, extra_body=...)`) | trait `Completions` (`create(body, extra_body)`), implemented for `eve_openrouter::Client` | The real impl merges `extra_body` over the body (its keys win), as the SDK does, and posts to `/chat/completions`. |
| (SDK exception classes) | `CallError { name, message }`, `sdk_error_name(&openrouter::Error)` | `NoKey`, `BadRequestError` 400, `AuthenticationError` 401, `PermissionDeniedError` 403, `NotFoundError` 404, `ConflictError` 409, `UnprocessableEntityError` 422, `RateLimitError` 429, `InternalServerError` 5xx, else `APIStatusError`; `APITimeoutError` / `APIConnectionError` for transport errors. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_small_talk.py` | `test_small_talk_phrases_match`, `test_the_grown_list_matches`, `test_the_grown_list_keeps_answers_and_commands_out`, `test_anything_else_is_not_small_talk`, `test_normalise`, `test_a_match_still_goes_to_the_brain_when`, `test_the_inlet_reads_the_switch` (the switch half), `test_the_request_is_the_voice_and_the_one_message`, `test_the_prompt_asks_for_one_friendly_sentence_and_no_acts`, `test_the_voice_is_the_profiles_talk_section`, `test_ask_answers_and_records_the_spend_as_small_talk`, `test_every_failure_of_the_call_is_a_reason`, `test_no_key_is_a_reason`, `test_a_cancelled_call_stays_cancelled`, `test_the_talking_section_survives_a_profile_longer_than_the_voice_cut` (the `voice_from_profile` half) | same names in `tests/test_small_talk.rs` (parametrized cases as loops; the voice half as `test_the_voice_of_the_talking_section`), plus `test_the_switch_spellings`, `test_the_model_name`, `test_the_system_prompt_is_pythons_verbatim`, `test_no_spend_is_recorded_when_asked_not_to`, `test_the_timeout_edges_follow_wait_for`, `test_the_real_client_merges_extra_body_and_posts_to_openrouter_shape`; unit tests in `src/lib.rs` for `squash`, `splitlines`, the Python `\w` class, whitespace collapse and the SDK error names | The inlet and app wiring tests exercise telegram.py, link_chat.py and records.py and belong to those crates: `test_the_inlet_reads_the_switch` (the inlet half), `test_switched_off_a_hi_goes_to_the_brain_as_today`, `test_a_lane_turn_answers_writes_the_transcript_and_skips_the_brain`, `test_a_phrase_not_on_the_list_never_calls_haiku`, `test_a_broken_lane_falls_through_to_the_brain_unchanged`, `test_eve_waiting_on_the_owner_sends_a_thanks_to_the_brain`, `test_an_attachment_sends_a_hi_to_the_brain`, `test_a_running_task_sends_the_message_to_the_brain`, `test_a_message_queued_behind_a_running_turn_is_the_brains`, `test_the_inlet_state_is_clear_when_nothing_waits`, `test_the_inlet_state_sees_every_wait`, `test_the_chiefs_cards_are_read`, `test_an_unreadable_ledger_is_read_as_waiting`, `test_an_app_hi_is_answered_by_the_lane_in_the_app`, `test_words_on_a_call_are_never_the_lanes`, `test_the_lane_is_fast_against_a_slow_brain`, `test_the_lane_reads_the_voice_from_the_whole_page`, and the `records.talking_section` half of `test_the_talking_section_survives_a_profile_longer_than_the_voice_cut`. |

## Boundaries
None

## Differences
- Cancellation: Python re-raises `CancelledError`; in Rust, dropping or aborting the `ask` future cancels the call (no Answer is produced), which is the same outcome.
- A reply whose `choices` is a non-empty JSON object makes Python's `[0]` raise `KeyError` out of `ask` (the caller then falls through to the brain); Rust returns `Answer { reason: "empty" }`, so the brain answers either way and `ask` keeps its never-fails contract.
- The reply is JSON (`serde_json::Value`) rather than an SDK object; `model_dump()` is the identity.
- Test seams: `AskOptions::ledger` and `AskOptions::env` replace the Python tests' spend-folder fixture and `monkeypatch` of the environment.
- Timeouts follow `asyncio.wait_for` (Python 3.11+): `timeout_s <= 0` is a timeout before the call starts, an infinite or NaN `timeout_s` never fires, and a `CallError` named `TimeoutError` (Python's `except asyncio.TimeoutError` also catches one the call raises) is reason `timeout`, not `error`.
- Unicode data: Rust's tables are Unicode 16, the daemon's Python 3.13 is 15.1, so characters new in 16 (for example U+1C89/U+1C8A) count as letters in Rust and as symbols in Python. A differential run of 63,876 messages, 3,000 profile pages and 3,000 request bodies against the Python module differed only on those two characters.
