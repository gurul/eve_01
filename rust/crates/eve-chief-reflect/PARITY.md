# eve-chief-reflect
Status: complete

The chief's Reflexion (Shinn et al. 2023): when a reflection is due (`triggers`, `heuristic`), the lesson as data
(`clean`, `carryable`, `carried`, `notes`, `with_notes`), the episodic memory `lessons.jsonl` (`Memory`), and the
one model call (`request`, `parse`, `Reflector`). One Python module, ported into `src/lib.rs`; the Python
semantics the file and the prompt depend on (`str.split`/`splitlines`/`isspace`, `casefold`, the `\w` and `\b` of
a str pattern, `str()`/`repr()` of a JSON value, float repr, `float()` of a str, `round(x, 3)`, `json.dumps`'
default separators, pathlib `expanduser`, OSError subclass names) are in the private `src/py.rs`.
`cargo test -p eve-chief-reflect`: 22 tests pass (17 module tests in src/tests.rs, 5 py helpers).

## Modules
| Python | Rust |
|---|---|
| chief_reflect.py | src/lib.rs (+ src/py.rs, private Python-semantics helpers; tests in src/tests.rs) |

## API
| Python | Rust | Notes |
|---|---|---|
| OMEGA, MAX_LESSON_CHARS, MAX_RETRIES, MAX_TRIALS, REPEAT_LIMIT, STEP_LIMIT, LESSONS_FILE, COMPACT_AT, MODEL, EFFORT, MAX_TOKENS, OVERRIDE_MAX_TOKENS, MAX_OUTPUT_CHARS, MAX_STEPS_SHOWN, NOTES_HEAD, NOTES_END, FAILED_OR_OVER, TRIGGERS | same names | values verbatim |
| REFLECT_SYSTEM | REFLECT_SYSTEM | the prompt verbatim |
| _LINK | private LINK + sub_links | `\b` checked by hand with Python's `\w` (letters, numbers, `_`); `\S` excludes U+001C..U+001F as Python's |
| _MARKUP, _CONTROL, _STEP_WORDS | private MARKUP, CONTROL, STEP_WORDS | |
| class Reflection | struct Reflection { lesson, retry } | |
| class Lesson | struct Lesson (+ Lesson::new) | `do` is `r#do`; `n`, `trial` are i64 |
| heuristic | heuristic(&[S]) -> &'static str | steps casefolded (Unicode full folding) before `[a-z0-9]+` |
| triggers | triggers(&PhaseResult, &[O], &[S]) -> Vec<&'static str> | |
| needs_retry | needs_retry | |
| may_retry | may_retry(&Phase, usize) | |
| clean | clean(&str); clean_value(Option<&Value>) for `clean(text: Any)` | `str(text or "")` of a non-str is Python's str()/repr() |
| carryable | carryable | eve_chief_card::verbs subset |
| carried | carried | |
| notes | notes | |
| with_notes | with_notes(goal, lessons, scope: Option<&str>) | |
| class Memory | struct Memory { pub dir } | Memory::new(folder, clock) (None: chief_ledger::chief_dir, EVE_CHIEF_DIR), Memory::at(folder); ReentrantMutex for RLock |
| Memory.path | Memory::path | |
| Memory._rows | private Memory::rows | invalid UTF-8 replaced; Python splitlines; torn/non-dict/bool-int/missing-key lines skipped; `float(t or 0.0)` incl. str t |
| Memory.add | Memory::add(&Lesson) -> bool | mkdir 0o700 (parents default mode), O_APPEND/O_CREAT 0o600, fsync; same line bytes; warning "chief: a lesson was not written (<OSError name>)" |
| Memory._compact | private Memory::compact | last OMEGA per key, stable sort by t, temp `.lessons-*.jsonl` in the folder, chmod 600, rename; warning "chief: lessons were not compacted (...)" |
| Memory.for_key, for_phase, for_attempt, latest, count | same names | count returns IndexMap<(executor, do), usize> |
| _strip | private strip | |
| request | request(card, phase, result, checks, steps, why, model) -> Value | body key order and the user message's `json.dumps(..., ensure_ascii=False)` bytes (", " / ": ") as Python's |
| parse | parse(&Value) -> Option<Reflection> | |
| class Reflector | struct Reflector + ReflectorOptions { ask, model, clock, offload, spend } | `ask` None is watch.openrouter (watch_ask: eve_web::watch::openrouter on Ctx::global()); offload is spawn_blocking (asyncio.to_thread); spend is the eve_spend::Ledger `spend.record_chat_completion(CHIEF, ..., provider "openrouter")` writes to |
| Reflector.reflect | async Reflector::reflect(card, phase, result, checks, executor, trial, why, steps) | log lines "chief: the reflection call failed (<name>); no lesson" and "chief: the reflection reply was not the agreed JSON; no lesson" |
| (an `ask` exception) | AskError { name, message }, From<FetchError>; type Ask | `name` is the exception class the log names |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_chief_reflect.py | test_a_lesson_is_written_and_bounded_to_three_per_key | test_a_lesson_is_written_and_bounded_to_three_per_key, test_a_reflector_writes_one_lesson_keyed_executor_and_phase_kind | the "through the chief" half (events log) drives chief.py: eve-chief |
| test_chief_reflect.py | test_a_reflection_error_means_no_lesson_and_no_retry | test_a_reflection_error_means_no_lesson_and_no_retry | the chief's no-retry / ledger status half: eve-chief |
| test_chief_reflect.py | test_injection_in_executor_output_cannot_change_the_next_goal_beyond_the_notes | same name | the chief's revise/research_calls half: eve-chief |
| test_chief_reflect.py | test_the_heuristics_repeated_step_and_too_many_steps | same name | |
| test_chief_reflect.py | test_clean_strips_links_markup_delimiters_and_caps | same name, + test_clean_matches_python_on_edges | |
| test_chief_reflect.py | test_a_lesson_that_widens_the_step_is_not_carried | same name | chief.ACT_GUARD copied verbatim into the test (eve-chief is downstream) |
| test_chief_reflect.py | test_the_reflection_request_marks_the_attempt_untrusted_and_carries_card_fields_only | same name, + test_the_request_shows_the_last_twelve_steps_and_counts_them_all | |
| test_chief_reflect.py | test_the_reply_must_be_exactly_lesson_and_retry | same name | |
| test_chief_reflect.py | test_the_memory_file_is_private_compacts_and_skips_torn_lines | same name, + test_the_lessons_file_is_the_bytes_python_writes_and_reads | |
| test_chief_reflect.py | test_a_reflector_lesson_that_cannot_be_kept_is_not_returned | same name | |
| test_chief_reflect.py | test_a_retry_carries_the_lessons | test_a_retry_carries_its_own_lessons_first_then_the_keys (Memory.for_attempt) | the retry loop is chief.py's: eve-chief |
| test_chief_reflect.py | test_a_one_way_act_never_retries_by_itself_and_its_next_go_shows_the_lesson, test_a_looping_but_confirmed_phase_keeps_a_lesson_and_retries_nothing, test_a_two_way_phase_retries_at_most_twice | test_needs_retry_and_may_retry (the rules) | the chief's dispatch, Go and ledger: eve-chief |
| test_chief_reflect.py | test_a_retry_waits_for_the_budget, test_the_model_says_no_retry_and_none_happens, test_a_retry_that_turns_a_failed_check_into_a_confirmed_one_is_counted | (none) | chief.py's budget, retry and retry_gains: eve-chief |
| test_model_research_roles.py | test_reflector_sends_medium_reasoning_and_allows_model_overrides (4 params) | same name (the 4 cases in one loop) | |
| test_chief.py, test_chief_lean.py, test_telegram_chief.py | (use Reflector/Memory as the chief's seam) | (none) | they test chief.py and the Telegram door: eve-chief, eve-telegram |

## Boundaries
None

## Differences
- `parse` of a reply whose `choices` is a non-empty JSON object: Python raises KeyError out of `parse` (and so
  out of `Reflector.reflect`); Rust returns None (no lesson, no retry), the module's rule for any other bad reply.
- `Reflector::reflect` with `offload`: a panic inside `ask` is no lesson (Python has no panics; an exception
  there is already no lesson).
- lessons.jsonl lines Python could read but serde_json cannot (NaN/Infinity literals, numbers past f64 range,
  ints past i64, lone-surrogate escapes) are skipped as torn. Neither daemon writes them.
- `float()` of a str `t` with non-ASCII digits is skipped (Python converts them). Neither daemon writes one.
- `~user` in a given folder that names no user is left unexpanded (pathlib raises RuntimeError).
