# eve-rundown
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/rundown.py` | `crates/eve-rundown/src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `SKILL` | `SKILL` | The skill file's text, embedded with `include_str!` from `bridge/src/eve_bridge/skills/rundown/SKILL.md` (Python holds the path and reads it per call; same bytes). |
| `OPEN_TASK` | `OPEN_TASK` | `regex`; Python's `\s` written as `[\s\x1c-\x1f]` throughout so U+001C-U+001F count as whitespace, as in `re`. |
| `TASK_DATE` | `TASK_DATE` | Same pattern, `(?i)`. |
| `READ_META_TOOLS` | `READ_META_TOOLS` | |
| `DIRECT_APPS` | `DIRECT_APPS` | |
| `_MONTHS` | `MONTHS` (private, built from `MONTH_NAMES`) | |
| `_MONTH` | `month_pattern()` (private) | Names sorted longest first with a stable sort, as `sorted(..., key=len, reverse=True)`. |
| `_ORD` | `ORD` (private) | |
| `_YEAR` | `year_pattern()` (private) | |
| `_NAMED` | `NAMED` (private) | `regex` (no lookaround). |
| `_NUMERIC` | `NUMERIC` (private) | `fancy-regex` for the lookbehind/lookahead. |
| `_ISO` | `ISO` (private) | `fancy-regex`. |
| `_NOT_DUE` | `NOT_DUE` (private) | |
| `_DATE_SLOT` | `DATE_SLOT` (private) | Python's trailing `$` written `\n?\z` (Python's `$` also matches before one final newline). |
| `_yearless` | `yearless` (private) | Nearest occurrence, a tie going forward; years outside 1..=9999 are not dates, as in Python. |
| `_dated` | `dated` (private) | Two-digit years are 20xx. |
| `task_date` | `task_date(&str, NaiveDate) -> Option<NaiveDate>` | Tasks markers parsed with `eve_google_rest::pytime::date_fromisoformat` (CPython's `date.fromisoformat`). |
| `_WHAT_IS` | `WHAT_IS` (private) | |
| `_PLAN` | `PLAN` (private) | |
| `_DAILY` | `DAILY` (private) | `fullmatch` as `^(?:...)\z`. |
| `matches` | `matches(&str) -> bool` | U+2019 to `'`, `str.strip()`, `lower()`, whitespace runs to one space, `rstrip(".!? ")`. |
| `tools` | `tools(&[Value], Option<&dyn Direct>) -> Vec<Value>` | `direct` is the `Direct` trait (`reads(name)`), implemented for `eve_apps_mcp::AppsBridge`. |
| `allows` | `allows(&str, &Map<String, Value>, Option<&dyn Direct>) -> bool` | Uses `eve_apps_mcp::split_name`, `eve_composio_tools::{MULTI_EXECUTE, is_read_only, slug_words}`; `str(t.get('tool_slug', ''))` via `eve_google_rest::py::py_str`. |
| `TODO_GROUPS` | `TODO_GROUPS` | |
| `todo_context` | `todo_context(Option<&Path>, &str) -> Result<Value, eve_google_rest::Error>` | Same keys and key order (`available`, the four groups, `skipped_files`, then `truncated_tasks` when set); same 5000-file, 1 MiB, 200-task and 2000-character limits; `os.walk` order (a folder's files sorted, then its kept subfolders sorted); symlinks, dot entries and `09-archive` skipped; non-UTF-8 notes counted in `skipped_files`; `str.splitlines()` line ends. The error is `date.fromisoformat`'s ValueError, raised only when a vault is present, as in Python. |
| `sources` | `sources(&[&str], bool) -> String` | Text checked against the Python output for six combinations. |
| `context` | `context(Option<&Path>, &DateTime<FixedOffset>, Option<&Value>, &[&str], bool) -> String` | |
| `outside` | `outside(&Value) -> bool` | Python truthiness via `eve_google_rest::py::truthy`. |
| `context_and_outside` | `context_and_outside(...) -> (String, bool)`, plus `context_and_outside_in(..., Zone)` | The day, `start_inclusive` and `end_exclusive` are read in the Mac's zone with `eve_google_rest::Zone::localize` (CPython's gap/fold rules); `timezone` is `localtime_r`'s `tm_zone`, as `astimezone().tzname()`. JSON written by `eve_apps_mcp::py::dumps(.., ensure_ascii=false)` (Python's `", "`/`": "` separators). The `_in` variant takes a named zone so tests need not set `TZ`. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_rundown.py` | `test_obsidian_dates_completed_fences_and_symlinks`, `test_rundown_tool_policy_reads_four_sources_and_refuses_mutations`, `test_written_dates_are_due_dates`, `test_lines_without_a_date_stay_undated`, `test_mount_tam_is_upcoming_with_its_date_not_backlog`, `test_asking_for_todays_plan_is_the_rundown`, `test_rundown_context_says_when_vault_todos_or_canvas_titles_fed_it` | same names in `src/tests.rs` (the parametrized date tests loop over the same `DATED` / `UNDATED` lists) | `test_rundown_loads_skill_and_reads_sources_even_with_codex_selected`, `test_rundown_declines_a_write_without_executing_or_asking`, `test_codex_progress_is_delivered_to_task_owner`, `test_what_is_my_plane_today_reads_the_calendar_in_one_pass`, `test_a_rundown_from_vault_todos_alone_is_kept_as_outside_words`: they drive the Telegram rig (`test_telegram.Rig`) and test `telegram.py`'s rundown turn, so they belong to the telegram crate. |
| `bridge/tests/test_canvas.py` (ref) | `test_the_rundown_carries_canvas_deadlines_only_when_on` (the `rundown.context` half) | `test_the_rundown_carries_canvas_deadlines_only_when_on` | The Telegram turn half belongs to the telegram crate. |
| `bridge/tests/test_menu_tools.py` (ref) | `test_daily_rundown_hands_the_message_to_a_read_only_rundown_turn` | none | Patches `context_and_outside` and tests the telegram/menu hand-off; belongs to those crates. |
| (new) | | `sources_names_only_the_tools_present`, `the_skill_gets_its_sources_step_and_the_data`, `direct_mail_and_calendar_reads_only`, `a_mixed_composio_call_is_refused`, `a_bad_day_is_an_error_only_with_a_vault`, `tasks_stop_at_two_hundred_and_nested_fences_and_archives_are_skipped`, `a_dst_day_is_not_twenty_four_hours_and_names_its_zone`, `may_as_a_verb_and_other_edges` | Expected values taken from the Python module. |

During the port, `task_date` and `matches` were also checked against the Python module on 26,000 generated inputs, and the full `context_and_outside` text on a fixture vault (nested folders, CRLF, a vertical tab, fences, a symlinked folder, Unicode names, a Canvas entry with a float) under `TZ=America/Los_Angeles` and the Mac's own zone: byte-identical.

## Boundaries
None.

## Differences
- Digits: the patterns match Unicode decimal digits as Python's do, but the numbers are read with Rust's ASCII parser, so a date written in non-ASCII digits (for example Arabic-Indic) is not a date in Rust where Python would read it.
- A month name that matches only through Unicode case folding (a long s, `ſept`) raises `KeyError` in Python; Rust passes over it.
- `tools` drops an entry that is not a JSON object, where Python raises `AttributeError`.
- A file or folder name that is not valid UTF-8 is shown with replacement characters in `note`, where Python keeps surrogate escapes.
