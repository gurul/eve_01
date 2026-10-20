# eve-chief-desk
Status: complete

The chief's attention gate: every message the chief would send on its own goes through `Desk::offer`, which
answers with one route and the rule that decided it and logs one line to `attention.jsonl`; plus the first brief,
the evening close and the weekly sheet, composed by code from the ledger. One Python module, ported into
`src/lib.rs`; the Python semantics it leans on (`datetime.fromtimestamp`, naive `timestamp()`,
`strptime("%H:%M")` and its error texts, `int()`, `json.dumps(separators=(",", ":"))` with ensure_ascii, `round`,
`repr`, `tomllib`'s TOML 1.0, text-mode reads, `expanduser`, OSError names, truthiness) are in the private
`src/py.rs`. Cards, `at_time`, `Check::describe` and `Nudges` come from eve-chief-card; `Ledger`, `JsonlRows`,
`chief_dir` and `Clock` from eve-chief-ledger. `cargo test -p eve-chief-desk`: 37 tests pass (32 desk, 5 py
helpers).

## Modules
| Python | Rust |
|---|---|
| chief_desk.py | src/lib.rs (+ src/py.rs, private Python-semantics helpers; tests in src/tests.rs) |

## API
| Python | Rust | Notes |
|---|---|---|
| PUSH_MODES, DEFAULT_PUSH_MODE, QUIET_START, QUIET_END, MANDATE_FILE, MANDATE_KEYS, ATTENTION_FILE, EVENING_CLOSE_AT, WEEKLY_SHEET_AT, ITEM_KINDS, UNPROMPTED, ROUTES, RULES, BREAKPOINTS | same names, `&str` / `&[&str]` | |
| PUSHES_PER_DAY, STRICT_PUSHES_PER_DAY, MAX_PUSHES_PER_DAY, MAX_IGNORED, STALE_DAYS | same names, `i64` | |
| WEEKLY_SHEET_DAY | WEEKLY_SHEET_DAY: u32 | 6 = Sunday (`datetime.weekday`) |
| BRIEF_LINES | BRIEF_LINES: usize | |
| BREAKPOINT_WINDOW_SECS, IDLE_SECS, REMINDER_WAIT_SECS, URGENT_SECS, NUDGE_GAP_SECS | same names, `f64` | REMINDER_WAIT_SECS = chief_card::REMINDER_WAIT_MIN × 60 |
| class Mandate | Mandate { quiet_start, quiet_end, pushes_per_day } (Default = the defaults) | |
| STRICT | STRICT: LazyLock<Mandate> | |
| class Item | Item { kind, card_id, queued_at, deadline_at, firm, nudges, card_open }, Item::new(kind, card_id) | struct-update syntax for the keyword arguments |
| class Route | Route { route, rule, at }, Route::new, Route::at | route and rule are `&'static str` from ROUTES / RULES |
| class Choice | Choice { label, acts }, Choice::new | |
| class Sheet | Sheet { text, ids: Vec<String> } | Python tuple |
| (ValueError, OverflowError, OSError raised by the module) | DeskError { name, message } | `name` is the Python type, `message` its text |
| push_mode | push_mode(Option<&HashMap<String, String>>) -> String | None reads the process environment |
| _hhmm | private hhmm (+ py::strptime_hhmm) | CPython 3.13 `_strptime` `%H:%M` regex with its alternation order; Unicode `\d`; the same ValueError texts |
| load_mandate | load_mandate(&Path) -> (Mandate, String) | same problem lines; the `toml` crate behind `py::tomllib_text`, which refuses what `tomllib` refuses and rewrites the numbers past i64 / f64 that `tomllib` holds (checked against Python 3.13 on 95 edge files) |
| _time | private row_time | |
| _minutes | private minutes_of (+ split_hm) | `int()` of each part as Python reads it |
| in_quiet | in_quiet(&Mandate, f64) -> Result<bool, DeskError> | Err where Python raises |
| quiet_ends | quiet_ends(&Mandate, f64) -> Result<f64, DeskError> | naive local `replace` / `+ 1 day` / `timestamp()` as CPython reckons a DST gap or fold |
| on_expiry | on_expiry(&[Choice]) -> Option<Choice> | |
| class Desk | Desk (Send + Sync) | one parking_lot Mutex for the moments and one for the log writes (Python's RLock) |
| Desk.__init__ | Desk::new(Option<&Path>, DeskOptions { mode, mandate, clock, environ }) | Clock is eve_chief_ledger::Clock |
| Desk.dir | Desk::dir() | |
| Desk.mode, Desk.mandate | pub fields | |
| Desk.path | Desk::path() | |
| Desk.take_problem | Desk::take_problem | |
| Desk._append | private Desk::append | mkdir 0o700, open O_APPEND/O_CREAT 0o600, write, fsync(2) |
| Desk._rows | private Desk::rows | JsonlRows: only lines appended since the last read are parsed |
| Desk.pushes_today | Desk::pushes_today(Option<f64>) -> Result<usize, DeskError> | |
| Desk.breakpoint | Desk::breakpoint(kind, Option<f64>) -> Result<(), DeskError> | "unknown breakpoint 'lunch'" |
| Desk.owner_said | Desk::owner_said(Option<f64>) | |
| Desk.at_breakpoint | Desk::at_breakpoint(f64) -> bool | |
| Desk.acted | Desk::acted(card_id, Option<f64>) | |
| Desk.sent | Desk::sent(&Nudges, f64) -> Nudges | static, as Python's staticmethod |
| Desk.ignored | Desk::ignored(&Nudges, f64) -> Nudges | |
| Desk.offer | Desk::offer(&Item, Option<f64>) -> Route | never fails: a DeskError or a panic in the decision is `batch`/`error` |
| Desk._decide | private Desk::decide | same order of rules |
| _due_at | private due_at | |
| _waiting_word | private waiting_word | |
| _fit | private fit | |
| first_brief | first_brief(&Ledger, f64) -> String | |
| evening_close | evening_close(&Ledger, f64) -> Result<String, DeskError> | Err where `fromtimestamp` raises |
| weekly_sheet | weekly_sheet(&Ledger, f64) -> Sheet | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_chief_desk.py | test_at_2300_an_item_is_batched_and_at_1000_it_goes_now, test_quiet_hours_cross_midnight_and_a_daytime_window_works_too, test_the_third_push_of_a_day_is_batched, test_in_shadow_the_would_pushes_are_silent_and_counted, test_the_push_count_survives_a_reload, test_unprompted_items_wait_for_a_breakpoint, test_with_no_breakpoint_in_45_minutes_the_reminder_is_sent_anyway, test_backoff_doubles_and_three_ignored_go_to_the_sheet, test_the_owners_own_action_cancels_queued_nudges, test_only_a_firm_commitment_due_soon_may_nudge, test_an_expired_decision_takes_the_option_that_does_not_act, test_any_error_falls_back_to_batch, test_a_mandate_that_does_not_parse_is_strict_and_said_once, test_push_mode_reads_the_switch, test_attention_lines_carry_ids_and_rules_never_text, test_the_first_brief_is_three_lines_at_most_from_the_ledger, test_the_evening_close_and_the_weekly_sheet, test_docs_mandate_defaults_equal_the_constants, test_docs_name_the_live_sets_and_carry_no_owner_data, test_attack_a_bad_row_in_the_attention_log_is_skipped (the 4 parametrized rows in one test), test_attack_a_budget_warning_is_unprompted, test_pushes_today_parses_only_new_attention_lines | same names in src/tests.rs | The monkeypatched `at_breakpoint` is a test-only flag that makes it panic. The `json.loads` counter of the D6 test is a line rewritten in place (same file, same length) that must not be read again, plus a fresh-reader control |
| test_chief_desk.py | test_docs_name_every_chief_setting_the_code_reads_with_its_default | same name | ported for the settings whose defaults live here and upstream (EVE_CHIEF_PUSH, EVE_CHIEF_DIR); every EVE_CHIEF* setting the Python reads must still have a Knobs row. EVE_CHIEF, EVE_CHIEF_ASSESS_* and EVE_CHIEF_REPLAN default in chief.py / chief_replan.py, downstream crates |
| (new, against CPython 3.13 output) | | attention_lines_are_the_bytes_python_writes, the_mandate_problem_lines_read_as_python_writes_them, a_time_the_clock_cannot_read_is_an_error_and_batched, the_first_brief_names_what_is_proposed_when_there_is_room, the_evening_close_and_the_weekly_sheet_are_unprompted_rules, a_long_line_is_cut_at_a_word, a_desk_with_no_folder_reads_eve_chief_dir, the_desk_reads_the_clock_when_no_time_is_given, in_quiet_is_the_lean_models_quiet_for_every_minute (from test_chief_lean.py); py.rs: strptime_hhmm_reads_and_refuses_as_python_does, tomllib_text_refuses_and_rewrites_as_tomllib_reads, int_of_str_follows_int, json_and_floats_follow_python, fromtimestamp_refuses_what_python_refuses | |
| test_chief.py, test_telegram_chief.py | read for behavior (the desk inside Chief and the Telegram door) | covered by the tests above | they test chief.py and telegram.py through the desk; ported with those crates |
| test_chief_lean.py | read for behavior (Chief.lean's `quiet` against `in_quiet` over all 1440 minutes; the model run reloading a Desk at each offer) | covered by test_quiet_hours_cross_midnight_and_a_daytime_window_works_too and test_the_push_count_survives_a_reload | the Lean model replays belong to the chief crate |

## Boundaries
None.

## Differences
- `in_quiet`, `quiet_ends`, `pushes_today`, `breakpoint` and `evening_close` return `Result` where Python raises
  (a time `fromtimestamp` cannot read, a hand-built mandate whose times are not `H:M`, an unknown breakpoint).
  `offer` catches these as Python does, and also catches a panic, as `batch`/`error`.
- The clock and every time are f64, so an attention line's `t` is always written as a float; a Python clock
  returning an int writes `5` where Rust writes `5.0`. Both read back the same.
- `mandate.toml` is read with the `toml` crate (TOML 1.1) behind `py::tomllib_text`, which refuses what the
  crate reads and CPython 3.13's `tomllib` rejects (a BOM, `\x` and `\e` escapes, a newline or trailing comma in
  an inline table, a time without seconds, a leap second, the year 0000), so the same files are "couldn't read";
  and which rewrites an integer outside i64 to `0.5` and a float literal past f64 to `inf` before the crate
  parses, since `tomllib` holds them. The rewrite keeps every problem line (`pushes_per_day must be 0 to 10`,
  `not a time`, `unknown setting`), but the parsed value is not Python's big number; the desk never keeps it.
- `Desk::new` with a folder `~user/...` for a user with no home folder keeps the path as written; pathlib's
  `expanduser` raises RuntimeError there, so Python's Desk cannot be built.
- `Desk::sent`/`ignored` with an `ignored` count so large that `2 ** ignored` does not fit a float gives an
  infinite `next_at`; Python raises OverflowError.
- A due time (`_due_at`) whose naive `timestamp()` CPython cannot compute is skipped rather than raising.
- `fromtimestamp` uses chrono's local time zone; for times before 1970 or past 2038 it follows the zone database
  rather than the C library's `localtime`.
