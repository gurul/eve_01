# eve-decider
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/decider.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `STYLES`, `RESERVED`, `QUESTION_ID` | `STYLES`, `RESERVED`, `QUESTION_ID` | Same values and order |
| `OPTION_TOKEN_CAP`, `HEAD_RESERVE`, `TIE_TOLERANCE`, `OPTION_LABEL_CHARS` | same `pub const`s | 48, 16, 1e-4, 40 |
| `STOP_WORDS` | `STOP_WORDS` (array) | Same 18 words |
| `INSTRUCTIONS` | `instruction_template(style)` | Prompt text verbatim; `{goal}` substituted as `.format(goal=…)` does |
| `WARMUP_OBJECTIVE`, `WARMUP_OPTIONS`, `WARMUP_CONTEXT` | same names (`LazyLock` for the last two) | Same 12 options in the same order, same 42-day context |
| `Choice` | `Choice` (struct, `Default`, `Serialize`), `Choice::new(id)` | Same fields and defaults; `probabilities` is an `IndexMap` in option order |
| `objective_tokens` | `objective_tokens(&str) -> HashSet<String>` | `casefold()` reproduced for every code point whose full folding changes the `[a-z0-9]+` words (ß, ẞ, ſ, ŉ, ǰ, ẖ…ẚ, ﬀ…ﬆ) |
| `render_option` | `render_option(&impl OptionFields)`; trait `OptionFields`, struct `OptionControl` | The trait stands in for `getattr(c, "label", "") or ""`; a missing field is `None` |
| `_hint_keys` | `hint_keys` (private) | |
| `render_instructions` | `render_instructions(style, objective, &options) -> Result<String, DeciderError>` | `ValueError` text verbatim: `style must be one of ('jev', 'compact', 'hinted'), not '…'` |
| `render_question` | `render_question(...) -> Result<Value, DeciderError>` | Keys `type`, `instructions`, `criteria` in Python's order |
| `word_count` | `word_count(&str) -> usize` | Splits like Python's `str.split()` (U+001C..U+001F included) |
| `instruction_tokens` | `instruction_tokens(style, objective, Option<&options>, Option<&Tokenize>)` | Python defaults: `objective=""`, `options=None`, `tokenize=None` |
| `option_tokens` | `option_tokens(&str, Option<&Tokenize>)` | `1 + min(48, count(" " + text))` |
| `trim_options` | `trim_options(&options, budget: i64, Option<&Tokenize>) -> (IndexMap, usize)` | Budget may be negative, as in Python |
| `_finite_unit` | `finite_unit` (private) | A JSON bool is never a number, as Python excludes `bool` |
| `Decider` | `Decider::new(predict, DeciderConfig) -> Result<Decider, DeciderError>` | `DeciderConfig::default()` holds Python's keyword defaults: `max_len` 1024, `head_max_len` 256, `style` "compact", `clock` `perf_counter`, no tokenizer. Public `max_len`, `head_max_len`, `style`, `available`, `load_ms`, `warm_ms` |
| `Decider.choose` | `Decider::choose(objective, app, context, &options, recent) -> Result<Choice, DeciderError>` | Caller contract errors are `DeciderError::Value` (`kind()` = "ValueError", messages verbatim); a predict failure is a `Choice` with `error` = `predict raised <Kind>: <msg>` clipped to 200 chars |
| `Decider._parse_choice` | `Decider::parse_choice` (private) | Same rule order and rejection texts, `{chosen!r}` / `{key!r}` via `py_repr` / `py_str_repr` |
| `Decider._lock` | private `parking_lot::Mutex<()>`, `Decider::predict_locked()` | Held only around predict |
| (predict seam) | `Predict` = `Fn(&Value, &Value) -> Result<Value, PredictError>`, `PredictError { kind, message }` | `kind` is the Python exception class name the error reports (`RuntimeError`, `JevError`); `From<anyhow::Error>` gives `Exception` |
| (`time.perf_counter`) | `perf_counter()`, `Clock` | |
| (`repr`) | `py_repr(&Value)`, `py_str_repr(&str)` | Python `repr` of the decoded JSON value (quotes, escapes, float form `1e-05`) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_decider.py` | 17 (one parametrized ×9) | All 17 with the same names in `src/tests.rs` (the parametrized one as a loop), plus 11 parity tests: injected tokenizer drives the budget, jev goal whitespace, predict panic, 200-char clip, exact rejection texts, usage kept on rejection, `input_tokens` int-only, out-of-range confidence, casefold + stop words, warm-up menu shape, Python `repr` | None |
| `bridge/tests/test_fast_lane.py`, `test_fastlane_eval.py`, `test_lane_first.py` (ref) | use `Choice` and a fake Decider | Their decider-facing behavior (`Choice` construction with defaults, `render_option` wording) is covered above | The lane itself belongs to the fast-lane crate |

## Boundaries
None

## Differences
- `predict` returns `serde_json::Value`, which has no NaN or infinity: such a probability arrives as `null` and is rejected with the same `not in [0, 1]` reason Python gives for NaN/inf.
- A panic inside `predict` becomes a `Choice` with `error` = `predict raised panic: <msg>`, the same "unavailable, never a traceback" rule Python applies to any exception.
- Python's type check on option keys and values (`non-empty {str: str} mapping`) is enforced by the type `IndexMap<String, String>`; only the emptiness check remains at run time, with the same message.
- `input_tokens` beyond `i64` saturates at `i64::MAX` (Python keeps the exact int); `overflow` is true either way.
