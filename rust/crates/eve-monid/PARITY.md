# eve-monid
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/monid.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| constants (`CLI_PATHS`, `NPM_BIN_DIRS`, `RC_TIMEOUT`, `RC_NO_CLI`, `KEYS_SECS`, `BALANCE_SECS`, `KEY_OK_SECS`, `DEFAULT_MODE`, `DEFAULT_PER_DAY`, `DEFAULT_USD_PER_DAY`, `COUNT_FILE`, `SEARCH_*`, `LOCATION_CODE`, `LANGUAGE_CODE`, `MAX_*`, `MIN_SEARCH_SECS`, `ENV_KEEP`, `UNIT_USD`) | same `pub const`s | Values verbatim |
| `Runner` | `trait Runner` (any `Fn(&[String], f64, &HashMap) -> (i32, String, String)`) | |
| `RUNNER`, `_LOCK`, `_USED`, `_LOADED`, `_KEY_OK_AT` | `Monid` (fields `runner`, `state`), one process-wide instance behind the module functions | Tests build their own `Monid` instead of patching globals |
| `find_cli` | `find_cli(Option<&Environ>)`, `Monid::find_cli` | `shutil.which` reads the process PATH (unset: `os.confstr("CS_PATH")`; set but empty: no match); `os.path.expanduser` as posixpath does (`$HOME` when set, even empty, else the password database; `~name` via `getpwnam_r`); `os.access(X_OK)` via `libc::access` |
| `_env` | `cli_env(cli)`, `Monid::cli_env` | Only `ENV_KEEP` (non-empty), `NO_COLOR=1`, PATH = dirname(cli), Homebrew's two, the daemon's PATH, `/usr/bin`, `/bin`, de-duplicated in order |
| `subprocess_runner` | `subprocess_runner(&[String], f64, &HashMap)` | `env_clear` + the given env, stdin null, both pipes read to their end within the timeout, else the child is killed and `RC_TIMEOUT`; a spawn error is `RC_NO_CLI` with `monid could not start (<strerror>)`; a signal death is `-signum` |
| `_json` | `cli_json` (private) | From the first `{` or `[`; trailing text is unreadable, as `json.loads` |
| `_call` | `Monid::call` (private) | Same reasons; `{timeout:.0}` rounds like Python's `.0f` |
| `mode` | `mode(Option<&Environ>)`, `Monid::mode` | `"off"` for 0/off/false/no, else `"fallback"` |
| `per_day` | `per_day(..)`, `Monid::per_day` | `int(float(raw))` clamped 0..=10000; inf/nan/garbage: warning + 20. Python float syntax (underscores between digits, inf/nan any case) |
| `usd_per_day` | `usd_per_day(..)`, `Monid::usd_per_day` | Clamped 0..=100; NaN or garbage: 0.25 |
| `_day` | `local_day` (private) | Local `%Y-%m-%d` |
| `_count_path` | `Monid::count_path` | `<spend ledger folder>/monid-runs.json` (`eve_spend::spend_dir()` by default) |
| `_load` | `Monid::load` (private) | Once per day per process; `int()`/`float()` of the saved values as Python does (numeric strings count, a float run count truncates); max with what is in memory |
| `_save` | `Monid::save` (private) | Bytes as `json.dumps`: `{"day": "...", "runs": n, "usd": x}`, `round(usd, 6)` with Python's float repr; via `monid-runs.tmp` and rename; failure is one warning with the OSError class name |
| `used_today` | `used_today(Option<f64>) -> Used`, `Monid::used_today` | `Used { runs, usd }` serializes as `{"runs", "usd"}` |
| `_take` | `Monid::take` (private) | Same cap messages; keeps the last 7 days in memory |
| `_settle` | `Monid::settle` (private) | `max(0.0, usd - estimate + cost)` |
| `reset_for_tests` | `reset_for_tests()`, `Monid::reset_for_tests` | |
| `available` | `available(environ, runner, timeout)`, `Monid::available` | Python's default timeout is `KEYS_SECS`; a found key is believed for 600 s |
| `_usd` | `usd(&Value)` (pub), `usd_of` | No unit is no price |
| `_cost` | `cost(&Value)` (pub), `cost_of` | `billing.reportedCost`, else `cost` |
| `run` | `run(provider, endpoint, &body, wait_secs, &RunOpts) -> Value`, `Monid::run` | `RunOpts { estimate_usd, feature, environ, runner, now }`; `-i` is `json.dumps(body)` byte for byte (ASCII escapes); spend row `monid`/`<provider><endpoint>` with the same note text |
| `balance` | `balance(environ, runner) -> Value`, `Monid::balance` | |
| `arun` | `arun(..).await`, `Monid::arun(self: Arc<Self>, ..)` | On tokio's blocking pool through `eve_spend::to_thread` (carries the spend job tag, as `asyncio.to_thread` copies the context) |
| `abalance` | `abalance(..).await`, `Monid::abalance` | Same |
| `search_body` | `search_body(&str) -> Value` | Key order kept |
| `_items` | `items_of` (private) | |
| `_clean` | `clean` (private) | Python `str.split()` whitespace (adds `\x1c`-`\x1f` to Rust's) |
| `_CITE`, `_MARKUP` | `CITE`, `MARKUP` | Same patterns |
| `_overview` | `overview` (private) | The look-behind split is done by hand: the text is single-spaced, so two sentences are everything before the second space after `.`/`!`/`?` |
| `parse_search` | `parse_search(&Value) -> Parsed` | `Parsed { answer, sources: Vec<Source { title, url, snippet }> }` serializes to the same dict |
| `web_search` | `web_search(query, budget_secs, &SearchOpts) -> Value`, `Monid::web_search` | A panic inside is caught: `{"ok": false, "reason": "the Monid run failed"}`, as Python's `except Exception` |
| `_web_search` | `Monid::web_search_inner` (private) | websearch's result shape and `"fallback": "monid"` |
| (new) | `py_dumps(&Value)` | `json.dumps` with default separators and `ensure_ascii` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_monid.py` | 23 (one parametrized x7) | 18 ported with the same names in `src/tests.rs` (`test_the_fallback_fires_only_when_both_engines_failed`, `test_a_unitless_cost_is_unpriced_not_dollars`, `test_odd_settings_never_raise`, `test_the_cli_never_gets_eves_secrets`, `test_the_cli_path_keeps_the_daemons_path_and_the_system_dirs`, `test_switched_off_never_runs`, `test_the_daily_run_cap`, `test_the_daily_dollar_cap`, `test_a_reported_cost_replaces_the_estimate`, `test_every_monid_failure_returns_the_original_reason` (the 7 cases as a loop), `test_a_run_is_recorded_in_the_spend_ledger`, `test_an_unpriced_run_is_flagged_and_counted_at_its_list_price`, `test_logs_carry_the_shape_never_the_query_or_the_key`, `test_available_needs_the_cli_and_an_active_key`, `test_balance`, `test_the_async_wrappers_run_off_the_loop`, `test_the_real_runner_kills_on_timeout_and_reports_a_missing_binary`, `test_the_cli_is_found_where_launchd_cannot_see_it`), plus 16 parity tests (`shutil.which`'s empty and unset PATH, `posixpath.expanduser`, count-file bytes and reading a Python-written file, 7-day trim, a run that never started, exit-code reasons and the `-i`/`-w`/timeout argv, `_json` past a notice, every money unit, Python float/int parsing of the switches, `search_body`/`parse_search` edges, `_overview`, the panic guard, `dirname`, `json.dumps` bytes, PATH and `NPM_CONFIG_PREFIX` lookup, the real runner's output and exit code) | `test_a_rate_limit_on_both_engines_falls_back_too`, `test_no_fallback_on_a_success`, `test_no_fallback_after_a_4xx_other_than_429`, `test_openrouter_refusing_the_account_still_gets_monid`, `test_the_reader_never_pays_for_monid`: they test websearch's routing (which engine failures reach Monid, `monid_ok=False`), which is the eve-web crate's code. The Monid side of every one (a call, its shape, no call when off or capped) is covered by the ported tests above, which call `web_search` directly. Where Python asserted websearch's own failure text, the Rust tests assert Monid's reason instead |

## Boundaries
- The `monid` CLI itself (the npm package `@monid-ai/cli`) is run as a subprocess, exactly as the Python does; eve never holds the key.

## Differences
- `run` with a NaN or infinite `wait_secs`, or a `now` that `datetime` cannot hold, returns `{"ok": false, "reason": "the Monid run failed"}` (one warning) where Python's `run` raises (its `web_search` already turns that raise into the same answer). `used_today` with such a `now` returns zeros.
- `subprocess_runner` with an empty argv returns `RC_NO_CLI` and `monid could not start (IndexError)`; Python raises `IndexError`.
- `str()` of a non-string `status`, `title`, `url`, `description` or `markdown` prints JSON for lists and objects rather than Python's repr. The CLI always sends strings there.
- A count file whose values are `Infinity`/`NaN` literals or an out-of-range float run count is read as nothing saved; Python's `json.loads` accepts the literals and `int(inf)` raises an uncaught `OverflowError`. eve never writes either.
- The panic guard's log line names `panic` where Python names the exception class.
- The CLI's JSON is read with serde_json: output nested deeper than 128 levels, `NaN`/`Infinity` literals or lone surrogate escapes count as "monid sent unreadable JSON", where `json.loads` reads them. The CLI never prints those.
- `EVE_MONID_PER_DAY` / `EVE_MONID_USD_PER_DAY` written with non-ASCII Unicode digits are "not a number" (default, one warning), where Python's `float()` reads them.
