# eve-decisions
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/decisions.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `ENDPOINT`, `MODEL`, `ENV`, `NONE`, `POLICY` | same `pub const`s | Strings verbatim |
| `log` | `tracing` (target `eve_decisions`) | |
| `DecisionError` | `DecisionError` (+ `Failure`) | `Failure::Decision` is Python's `DecisionError`; `Failure::Other(kind)` is any other exception, carrying only `type(e).__name__` |
| `enabled` | `enabled(Option<&HashMap<String, String>>)` | `None` reads the process env; default `"0"`, trimmed, lowercased, in 1/true/yes/on |
| `image_url` | `image_url(&DynamicImage) -> Result<String, Failure>` | RGB, Pillow's `thumbnail((1600, 1600))` sizes (`thumbnail_size`, checked against Pillow), PNG, base64 data URL |
| `predicate` | `predicate(name, instructions) -> Value` | |
| `selection_questions` | `selection_questions(iter of (key, description)) -> Vec<Value>` | Prompt text verbatim; order kept, `none` last |
| `Answer` | `Answer { values, ms, usage, error }` | `Answer::new(values)`, `Answer::with_error` for fakes |
| `probability` | `probability(Option<&Value>)`, `probability_f64(f64)` | Booleans and non-numbers refused |
| `parse` | `parse(&Value, &[Value]) -> Result<Map, Failure>` | Same checks in the same order, same messages; `KeyError`/`TypeError` for malformed questions |
| `Client` | `Client` (`new`, `with_model`, `with_post`, `with_meter`, `with_ledger`, `evaluate`, `close`) + `Evaluate` trait | `post=` is the `Post` trait (closures implement it); `meter=` a closure over the reply body; default meter is `eve_spend::record("openai", model, "computer_decisions", None, tokens=int usage, note="Decisions beta; rate unverified")`. `Evaluate` is the seam lanes take, so tests can hand canned `Answer`s |
| `Client.evaluate` | `Client::evaluate(&Map, &[DynamicImage], &[Value]) -> Answer` | Never fails; error text truncated to 120 chars; usage kept even when parsing fails |
| `Client.close` | `Client::close` (also on `Drop`) | |
| `_warned` | `WARNED: AtomicBool` (private) | `from_env_with` takes its own flag for tests |
| `from_env` | `from_env(Option<&HashMap>) -> Option<Client>` (always `None`), `from_env_with` | Logs `OFF_WARNING` once a process when `EVE_DECISIONS` is on |
| (`sum(probs.values())` in `parse`) | `py_sum` | CPython 3.12+ Neumaier-compensated float sum (the live venv is 3.13), so the 0.02 tolerance decides the same way |
| (`json.dumps(context, ensure_ascii=False)`) | `py_dumps(&Value)` | `", "`/`": "` separators, Python float repr, UTF-8 kept |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_decisions.py` | 6 (two parametrized: 11 faults, 2 exceptions) | All 6 with the same names in `src/tests.rs` (parametrized ones as loops), plus 9 parity tests: `enabled` strings, the choice total summed as Python sums floats, Pillow thumbnail sizes, bounded RGB PNG, context text byte-for-byte (key order, `screen_text` override, float repr, non-ASCII), choice `p`/`margin`, question shape, failure kinds (no key, bad JSON, `KeyError`, integer-only token metering), a real refused loopback connection from async code | The `nan` fault: JSON cannot carry NaN, so it is sent as null and `probability_f64(NaN)` is asserted directly. The log-once test captures `tracing` output with a per-test flag instead of resetting the global |
| `bridge/tests/test_decisions_observation.py` | 14 | none here | Every test drives `decision_lane` (`lane.evidence`, `lane.route`, `snapshot_from_raw`); they belong to the eve-desktop-lanes crate. Its only use of this module, `decisions.Answer({...})`, is `Answer::new` |
| `bridge/tests/test_decision_lane.py`, `test_computer_protocol_eval.py` (tests_ref) | — | — | Use only `Answer`, `NONE`, `selection_questions` and a fake `Client`; covered here, the rest belongs to their crates |

## Boundaries
None

## Differences
- The thumbnail is resampled with the `image` crate's Catmull-Rom (Pillow's default bicubic) without Pillow's `reducing_gap` pre-reduction, and PNG bytes differ; sizes match Pillow exactly. An empty image fails with `ValueError` before encoding.
- A reply body using the `NaN`/`Infinity` literals fails as `JSONDecodeError` (and its usage is lost) where Python's `json.loads` accepts them and then fails with `invalid probability`. Either way the judgment authorizes nothing.
- Transport failure kinds are mapped from `reqwest` to httpx names (`ConnectTimeout`, `ReadTimeout`, `ConnectError`, `LocalProtocolError`, `ReadError`, otherwise `TransportError`). The 3 s timeout is set as connect and total timeout; httpx applies it per phase.
- A question `name` must be a string (Python accepts any hashable); every question eve builds uses strings.
- When called from inside a tokio runtime, the HTTP request runs on a scoped plain thread, because reqwest's blocking client cannot run there; Python reaches it through `asyncio.to_thread`.
