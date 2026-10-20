# eve-pricing
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/pricing.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `_RATES` | `RATES: [(&str, ClaudeRates); 3]` | Same order; `family_rates(family)` reads a row |
| `_DEFAULT_FAMILY` | `DEFAULT_FAMILY` | `"sonnet"` |
| `family_of` | `family_of(&str) -> &'static str` | Same substring order: opus, haiku, sonnet |
| `estimate_cost` | `estimate_cost(&str, &Value) -> f64` | ANTHROPIC_RATES row first, then the family table |
| `OPENAI_RATES` | `OPENAI_RATES: [(&str, OpenAiRates); 8]` | |
| `JEV_INPUT_PER_M` | `JEV_INPUT_PER_M` | 0.042 |
| `openai_rates` | `openai_rates(&str) -> Option<&'static OpenAiRates>` | Same pattern: dashed date, then `:variant` |
| `responses_tokens` | `responses_tokens(&Value) -> ResponsesTokens` | Serializes as `{"in", "cached", "out"}` |
| `estimate_openai_cost` | `estimate_openai_cost(&str, &Value) -> Option<f64>` | |
| `estimate_jev_cost` | `estimate_jev_cost(i64) -> f64` | |
| `LIVE_PER_MINUTE` | `LIVE_PER_MINUTE` | |
| `TRANSCRIBE_RATES` | `TRANSCRIBE_RATES: [(&str, TranscribeRates); 3]` | |
| `EMBEDDING_RATES` | `EMBEDDING_RATES` | |
| `OPENAI_WEB_SEARCH_PER_CALL` | `OPENAI_WEB_SEARCH_PER_CALL` | 0.01 |
| `ANTHROPIC_RATES` | `ANTHROPIC_RATES: [(&str, ClaudeRates); 1]` | |
| `_named` | `named(table, model_id)` (generic), plus cached `anthropic_named`, `transcribe_named`, `live_named`, `embedding_named` | Same pattern: dashed or compact date suffix; regexes for the fixed tables compile once |
| `estimate_live_cost` | `estimate_live_cost(&str, f64) -> Option<f64>` | |
| `estimate_transcribe_cost` | `estimate_transcribe_cost(&str, Option<&Value>, f64) -> Option<f64>` | Tokens when either key is non-null, else `usage.seconds or seconds` |
| `estimate_embedding_cost` | `estimate_embedding_cost(&str, i64) -> Option<f64>` | |
| `estimate_anthropic_cost` | `estimate_anthropic_cost(&str, &Value) -> Option<f64>` | Only JSON integers count (bools and floats read 0), as Python's `n()` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_pricing.py` | 14 | 14 in `src/tests.rs` (same names) plus 6 parity tests for the spend-meter tables, `_named` suffixes and `int()` field reading (values checked against the Python) | None |

## Boundaries
None

## Differences
- Usage objects are `serde_json::Value`. Python's `estimate_anthropic_cost` also accepts the Anthropic SDK object via `getattr`; Rust callers pass its JSON form.
- Where Python's `int(x or 0)` / `float(x)` would raise (a non-numeric string, a non-empty list, a non-dict `usage`), Rust reads the field as 0 instead of raising. Integers beyond `i64` saturate.
- `estimate_jev_cost` and `estimate_embedding_cost` take `i64`; Python's `int(x or 0)` of `None` is the caller's `0`.
