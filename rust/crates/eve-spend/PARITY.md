# eve-spend
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/spend.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_DIR`, `KEEP_DAYS` | `DEFAULT_DIR`, `KEEP_DAYS` | `~/.config/eve-bridge/spend`, 400 |
| `CHAT` … `CHIEF` (18 feature names) | same `pub const`s | Strings verbatim |
| `_JOB`, `_JOB_ID` | thread-local `THREAD_JOB` + tokio task-local `TASK_JOB`, `JOB_ID` | Sequence-numbered so the innermost set wins across both homes |
| `_lock`, `_dir_override`, `_warned` | `LOCK`, `DIR_OVERRIDE`, `WARNED` | Process-wide |
| `set_dir` | `set_dir(Option<PathBuf>)` | |
| `spend_dir` | `spend_dir()`, plus `spend_dir_from(Option<&str>)` | `EVE_SPEND_DIR`, blank means default, `~` expanded from `$HOME` |
| `job` | `job(&str) -> JobGuard` (sync, RAII reset), `in_job(&str, fut)` (async) | A non-`c<N>` id tags nothing; Python's `$` accepts a trailing `\n`, kept |
| `current_job` | `current_job() -> String` | |
| (contextvars copy into `asyncio.to_thread`) | `carry(f)`, `to_thread(f)` | `to_thread` runs on tokio's blocking pool with the tag |
| `_once` | `once(key, msg)` | `tracing::warn!`, same messages |
| `_local_day` | `local_day(f64) -> Option<NaiveDate>` | Local zone; years outside 1..9999 fail like `datetime` |
| `_num` | `num(Option<&Value>) -> i64` | Positive JSON integers only (no bools, no floats) |
| `record` | `record(provider, model, feature, Option<f64>, RecordOpts)`, `Ledger::record` | `RecordOpts { tokens, source, note, now }`. Line bytes match `json.dumps(row, separators=(",", ":"))`: key order, `ensure_ascii` escapes, Python float repr (`1e-05`, `1e+16`, `Infinity`), `round(usd, 8)` |
| `_as_dict` | `as_dict(&Value)`, `sdk_dump(&impl Serialize)` | `sdk_dump` is `model_dump(exclude_none=True)` for typed SDK replies |
| `_usage_of` | `usage_of(&Value)` | |
| `_web_search_calls` | `web_search_calls(Option<&Value>)` | |
| `record_response` | `record_response(feature, &Value, model, provider, Option<i64>)`, `Ledger::record_response` | Python defaults: `model=""`, `provider="openrouter"`, `searches=None`. `usage.cost` counts a bool, as `isinstance(x, (int, float))` does |
| `record_chat_completion` | `record_chat_completion(feature, &Value, model, provider)`, `Ledger::…` | |
| `_reported_cost` | `reported_cost(&Value) -> Option<f64>` | Non-bool number ≥ 0 |
| `record_anthropic` | `record_anthropic(feature, model, &Value, provider) -> Option<f64>`, `Ledger::…` | |
| `record_embedding` | `record_embedding(feature, model, &Value, provider)`, `Ledger::…` | |
| `record_transcription` | `record_transcription(feature, model, &Value, seconds, provider)`, `Ledger::…` | |
| `record_live` | `record_live(feature, model, seconds, note)`, `Ledger::…` | Provider `openai`; `secs` rounds half to even |
| `day_rows` | `day_rows(&str, Option<&Path>) -> Vec<Map<String, Value>>`, `Ledger::day_rows` | Universal newlines; torn and non-object lines skipped |
| `_usd` | `row_usd(&Map) -> f64` | |
| `totals` | `totals(rows) -> Totals` | `Totals` serializes with Python's keys in Python's order; splits sorted by (−usd, name), rounded to 6 places |
| `job_total` | `job_total(card_id, since, Option<today>, Option<&Path>) -> f64` | At most KEEP_DAYS back; rounded to 8 places |
| `day_total` | `day_total(&str, Option<&Path>) -> f64` | |
| `today_total` | `today_total(Option<NaiveDate>) -> f64` | |
| `summary` | `summary(Option<NaiveDate>, days, Option<&Path>) -> Summary` | Python's default `days=30`; `Summary` serializes to the same dict (`{"date", **totals}`) |
| `_money` | `money(Option<f64>) -> String` | `.2f` from $0.10 and for 0, else `.3f`; ties to even like Python |
| `brief` | `brief(&Value, Option<&Value>) -> String`, `brief_summary(&Summary, …)` | Text verbatim |
| `Ledger` (new) | `Ledger::at(dir)` / `Ledger::default()` | The module functions are `Ledger::default()`; tests use a ledger per temp folder instead of the global `set_dir` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_spend.py` | 26 (one parametrized ×5) | 19 ported with the same names in `src/tests.rs` (the parametrized one as a loop), plus 8 parity tests (byte-identical lines against Python output, float repr, note/field clipping, the other helpers, `brief` branches, universal newlines, `EVE_SPEND_DIR` defaults, the `$` card-id quirk) | The 7 spend_sync tests (`test_openrouter_key_daily…`, `test_a_first_day…`, `test_openai_costs…`, `test_anthropic_amounts…`, `test_a_failure_is_logged_once…`, `test_no_admin_key…`, `test_the_loop_syncs…`) belong to the eve-spend-sync crate. `test_a_job_tag_survives_the_search_routers_worker_thread` is ported against `carry` (what `search_router.bounded` uses), with the bare-thread positive control |

## Boundaries
None

## Differences
- Python's helpers take a dict or an SDK object; Rust's take `serde_json::Value` (an SDK struct goes through `sdk_dump`).
- The job tag: Python's contextvar follows an async task automatically. In Rust, synchronous code uses the `job` guard (thread-local); async code that awaits inside the block uses `in_job`. Work handed to another thread carries the tag through `carry` / `to_thread`, as `asyncio.to_thread` copies the context; a bare `std::thread::spawn` loses it, as a bare `threading.Thread` does.
- The helpers' `except Exception` paths ("spend: could not read a reply's usage") cannot fire: the Rust pricing reads odd fields as 0 instead of raising (see eve-pricing's PARITY.md).
- `record_live` with a NaN or infinite `seconds` writes `secs` 0; Python raises from `round()`.
- `day_rows` skips a line that is not valid UTF-8 (Python raises `UnicodeDecodeError` out of the reader) and a line using the `NaN`/`Infinity` literals (Python's `json.loads` accepts them; eve never writes NaN, and `Infinity` only for an infinite cost).
- `str()` of a non-string `p`/`m`/`f` value in `totals` and of a non-string `error` in `brief` prints JSON for lists and objects rather than Python's repr. Rows eve writes always hold strings there.
- The `Summary` and `Totals` serialized by serde_json write floats in Rust's shortest form (`1e-5`), not Python's (`1e-05`); both parse to the same number. The ledger lines themselves are byte-identical.
