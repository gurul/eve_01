# eve-spend-sync
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/spend_sync.py` | `src/lib.rs` (Python-shaped helpers in `src/py.rs`, tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `STATE_FILE`, `SYNC_SECS`, `TIMEOUT_SECS`, `KEEP_DAYS`, `LOOKBACK_DAYS`, `MAX_PAGES` | same `pub const`s | `providers.json`, 3600, 20, 400, 31, 5 |
| `OPENROUTER_KEY_URL`, `OPENROUTER_CREDITS_URL`, `OPENAI_COSTS_URL`, `ANTHROPIC_COST_URL`, `ANTHROPIC_VERSION` | same `pub const`s | Verbatim |
| `Opener` | `trait Opener` (`open(url, headers, timeout) -> Result<Vec<u8>, OpenError>`), `HttpOpener` (default) | `OpenError` is what `urlopen` raises: `Http(code)`, `Unreachable(reason type)`, `TimedOut`, `Raised(type name)` |
| `_warned` | private `WARNED` | Process-wide, separate from eve-spend's, as in Python |
| `SyncError` | `SyncError(String)`; `ProviderError { Sync, Other }`; `PyError(type name)` | `Other`/`PyError` stand for Python's `except Exception` branch, carrying the exception's type name, which is what Python logs and stores |
| `_once` | private `once(key, msg)` | `tracing::warn!`, same messages |
| `get_json` | `get_json(url, headers, Option<&dyn Opener>, Option<Duration>)` | Adds `Accept: application/json` and `User-Agent: eve-spend`; reasons `HTTP <code>`, `unreachable (<type>)`, `timed out`, `unreadable reply` verbatim |
| `_num` | private `num` | Number, never a bool |
| `sync_openrouter` | `sync_openrouter(&mut State, key, now, opener)` | `key_days` (UTC), `key_month`, `account_days` (local day, `first` kept, `last` updated), `at`, error cleared |
| `sync_openai` | `sync_openai(...)` | `start_time` = UTC midnight 30 days back, `bucket_width=1d`, `limit=31`, paged by `page=<next_page>` up to 5 pages |
| `sync_anthropic` | `sync_anthropic(...)` | `starting_at=<date>T00:00:00Z`, `x-api-key`, `anthropic-version`; cents → dollars; `float(str)` parsing (whitespace, `_` between digits, `inf`/`nan`) |
| `state_path` | `state_path()`, `state_path_in(folder)` | `<eve_spend::spend_dir()>/providers.json` |
| `load_state` | `load_state()`, `load_state_from(path)` | `{}` when missing, unreadable or not an object; reads Python's `NaN`/`Infinity` literals (as null) |
| `_prune` | private `prune` | Each day map over 400 days keeps its newest 400, sorted |
| `save_state` | `save_state(&State)`, `save_state_to(path, &State)` | Byte-identical to `json.dumps(state, sort_keys=True)` (separators, `ensure_ascii`, float repr), through `providers.tmp` and a rename |
| `sync_once` | `sync_once(env, now, opener) -> Result<State, PyError>`, `sync_once_in(folder, ...)` | `env: Option<&Env>` (`None` = process env). `Err` only where Python's raises too (see Differences) |
| `loop` | `run_loop(watch::Receiver<bool>, Duration, Option<Env>)`, `run_loop_with(..., SyncFn)` | `loop` is a Rust keyword. Sync on the blocking pool (`eve_spend::carry`), then wait the interval or the shutdown; `spend: a provider sync failed (<type>)` once per type |
| `reported` | `reported(Option<&State>, now, env) -> Map` | Same keys in the same order; `status` `ok`/`error`/`pending`/`no_key`; `month` is int 0 when no day this month, Neumaier sum like CPython 3.12+ |
| `_account_day` | private `account_day` | `max(0.0, x)` semantics kept (NaN and -0.0 give 0.0) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_spend.py` (spend_sync section) | `test_openrouter_key_daily_and_account_delta_between_days`, `test_a_first_day_counts_from_its_own_first_snapshot_and_says_so`, `test_openai_costs_are_read_only_with_an_admin_key_and_paged`, `test_anthropic_amounts_are_cents_and_its_headers_are_the_admin_ones`, `test_a_failure_is_logged_once_kept_as_a_reason_and_breaks_nothing`, `test_no_admin_key_says_which_one_to_add`, `test_the_loop_syncs_at_start_and_stops_on_shutdown` | All 7, same names, in `src/tests.rs`; plus 16 parity tests: byte-identical state file against CPython output, `urlencode` against CPython output, the exact OpenAI URL and fixed headers, `float(str)` amounts and skipped results, unreadable replies, every error reason and its clearing, `datetime` range errors (`ValueError`/`OSError`/`OverflowError`) and a non-iterable `data` (`TypeError`), a non-object row failing the sync, pruning, `reported` month/int-0/bool rules and key order, the account delta floor, CPython `sum`/`round`, the real opener's refused connection, the loop surviving a failed round | The rest of `test_spend.py` belongs to eve-spend |
| `bridge/tests/test_onboarding.py` | uses `OPENROUTER_KEY_URL` / `OPENROUTER_CREDITS_URL` only | The constants are exported verbatim | Onboarding's own requests belong to the onboarding crate |

## Boundaries
None

## Differences
- A JSON value cannot hold NaN or infinity: a figure that rounds to one (an Anthropic amount string `"inf"`, say) is stored and reported as `null` where Python writes `Infinity`; a reply or state file using Python's `NaN`/`Infinity` literals reads them as `null`. A reply number past 1e308 (Python: `inf`) is an unreadable reply.
- `sync_once` returns `Err(PyError("TypeError"))` in the one case Python's "never raises" `sync_once` does raise: a state file whose provider row is not an object (nothing is saved, as in Python). `run_loop` logs it the same way.
- `reported` with a `now` that `datetime` cannot represent uses the current time; Python raises.
- The default opener's timeout is 20 s for the whole request (and 20 s to connect); urllib's is 20 s per socket operation. `unreachable (...)` reasons are mapped from reqwest's error chain to the Python type names (`ConnectionRefusedError`, `gaierror`, `SSLCertVerificationError`, `TimeoutError`, ...); a redirect loop is `HTTP 302` when reqwest gives no code.
- `float(str)` does not accept non-ASCII digits (Python does); `str()` of a non-string `next_page` or currency uses a close Python `repr` (quotes, common escapes) for lists and objects.
