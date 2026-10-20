# eve-claude-launch
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/claude_launch.py` | `src/lib.rs`, `src/difflib.rs` (Python's `difflib.get_close_matches`, used by `match`); tests in `src/tests.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_AREAS` | `DEFAULT_AREAS` | `["personal", "work"]` |
| `MAX_CHOICES`, `MAX_RECENT`, `FLOW_TIMEOUT_SECS`, `WARP_CONFIG_NAME` | same names | 10, 5, 300.0, `"eve-claude"` |
| `RECENT_PATH` | `RECENT_PATH` (`LazyLock<PathBuf>`) | `~/.config/eve-bridge/claude-launch.json`, fixed on first read as Python fixes it at import |
| `TRIGGER` | `TRIGGER` (`LazyLock<Regex>`), `trigger_argument` | Same pattern, `(?is)`; Python's `$` (end, or before a final newline) is `\n?\z`, Python's `\s` is its `str.isspace` set. `trigger_argument` is `TRIGGER.match(t)` with `group(1) or ""` |
| `GENERAL_WORDS`, `LIST_WORDS` | same names | Verbatim |
| `_NUMBERED`, `_TAPPED` | private `NUMBERED`, `TAPPED` | Same patterns, same `$` and `\s` treatment |
| `code_root` | `code_root(&impl Environ)`, `code_root_from_process` | `EVE_CODE_ROOT` stripped, `~` expanded (pathlib's `expanduser`, from `eve-envfile`); else `~/Documents`. `environ=None` is `ProcessEnv` |
| `area_names` | `area_names(&impl Environ) -> Vec<String>` | `EVE_CODE_AREAS` split on `,`, stripped, blanks dropped; default `personal,work` |
| `folders` | `folders(&Path) -> Vec<String>` | Directories (symlinks followed) not starting with `.`, sorted by `str.lower`; unreadable area (or a failing entry, as `iterdir` raises mid-way): `[]` |
| `_norm` | private `norm` | Lower case, Python whitespace, `_`, `-`, `.` removed |
| `match` | `match_names` | `match` is a Rust keyword. Exact (first only), prefix, substring (by `(len, lower)`), then `difflib.get_close_matches(n=5, cutoff=0.7)` over a dict of normal forms (later duplicate replaces the value in the earlier key's place, via `IndexMap`) |
| `menu` | `menu -> Vec<(String, usize)>` | |
| `HARNESSES`, `AREA_HARNESS` | `HARNESSES`, `AREA_HARNESS` (const pairs), `harness_command` | Command lines verbatim; `harness_command(name)` is `HARNESSES[name]` |
| `_H`, `_INSTEAD_OF`, `_NAMED`, `_BARE_INSTEAD` | private `H`, `INSTEAD_OF`, `NAMED`, `BARE_INSTEAD` | Same patterns, `(?i)`, searched in the same order |
| `harness_name` | `harness_name -> &'static str` | |
| `split_harness` | `split_harness -> (String, Option<&'static str>)` | Phrase cut out, then `.strip(" ,.")` |
| `default_harness` | `default_harness(&Path, &Path) -> &'static str` | First part under root, lower-cased, through `AREA_HARNESS`; not under root or root itself: `claude` |
| `Recent` | `Recent { folder, harness }` | `Eq + Hash`, as the frozen dataclass |
| `load_recent` | `load_recent(&Path)`, `load_recent_default()` | Unreadable/undecodable/invalid JSON: `[]`; rows need a string `folder` and a known `harness`; folder normalized as `Path(str)` does; first 5 |
| `remember` | `remember(&Path, &str, &Path)` | Same file: `json.dumps(..., indent=2) + "\n"` with `ensure_ascii` (`\uXXXX`, lower-case hex, surrogate pairs, `\u007f`); parents made; a write failure is logged as `claude-launch: could not save recent folders (<Python exception class>)` |
| `Step` | `Step` (`Step::new`) | Same fields and defaults: `title` `"New Claude session"`, `buttons` empty |
| `LaunchFlow` | `LaunchFlow` (`new`, `with_areas`, `with_recent`, `with_clock`) | Same public fields. `choices` holds `Choice::Recent` / `Choice::Folder` (Python's mixed list). `clock` is `Arc<dyn Fn() -> f64>`, default `monotonic()` (the system monotonic clock, `CLOCK_UPTIME_RAW` on macOS, which is what `time.monotonic` reads). Methods: `expired`, `start`, `request`, `answer`, and private `existing_areas`, `label`, `ask_area`, `ask_scope`, `scope_answer`, `list`, `pick`, `open` (Python's `_existing_areas`, `_label`, `_ask_area`, `_ask_scope`, `_scope_answer`, `_list`, `_pick`, `_open`). Every owner-facing string verbatim |
| `terminal_app` | `terminal_app(&impl Environ) -> String` | `EVE_CLAUDE_TERMINAL` `warp`/`terminal` (stripped, lower-cased), else `warp` when `/Applications/Warp.app` exists |
| `warp_config` | `warp_config(&Path, &str) -> String` | Byte for byte, scalars as `json.dumps` (ASCII-escaped) |
| `_run` | `run(Vec<String>) -> io::Result<(i32, String)>` | Exit code (a signal is minus its number, as `returncode`); stderr, else stdout, lossily decoded, stripped, first 200 characters. A program that cannot start is an `Err`, as Python raises |
| `open_session` | `open_session(&Path, &str, OpenOptions) -> io::Result<String>`, `open_session_with(..., run)` | Keyword arguments are `OpenOptions { command, app, warp_dir, recent_path }`; `run` is the test seam. `Path.resolve()` is `eve_envfile::realpath` (Python 3.13's non-strict `realpath`). Same messages: not a folder, Warp config write failure with the Python exception class, `I couldn't open Warp/Terminal: <err or 'no reason given'>`, the success line. Terminal: `cd <shlex.quote(folder)> && <command>`, backslashes and quotes escaped for AppleScript, then `activate` |
| (`shlex.quote`) | `shlex_quote` | Safe set `[A-Za-z0-9_@%+=:,./-]`, else single-quoted with `'"'"'` |
| (`type(e).__name__` for `OSError`) | `py_error_name` | errno to Python's `OSError` subclass name |
| (`str.capitalize`) | private `capitalize` | Title case of the first character (table generated from Python for the 135 characters where it differs from upper case), rest lowered in context (final sigma) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_claude_launch.py` | 32 (`test_split_harness` parametrized ×7) | 24, same names (`test_split_harness` loops over its 7 cases): matching, menu, folders, the tree, the harness rule, `request`, trigger words, Warp config, `open_session` ×3, recent list, tap buttons | `test_telegram_walks_the_tree_without_a_model`, `test_telegram_one_line_with_override`, `test_telegram_cancel_closes_the_tree`, `test_telegram_tool_path_asks_then_the_reply_answers`, `test_every_offered_tool_is_accepted_when_the_model_calls_it`, `test_claude_on_with_nothing_running_walks_the_tree_then_joins_what_opens`, `test_claude_on_with_several_sessions_asks_which_and_follows_only_that_one`, `test_claude_on_with_one_session_joins_it_at_once`: they drive `TelegramInlet` (and `memory.TOOLS`), which the telegram crate ports; they belong with it |
| `bridge/tests/test_work_sessions.py` (tests_ref) | `code_root({})` used in a work-sessions assertion | `test_code_root_defaults_to_documents_under_home` | The rest tests `work_sessions` |
| `bridge/tests/test_telegram.py` (tests_ref) | drives the tree through `TelegramInlet` | covered by the tree tests above | Telegram's own behavior belongs to the telegram crate |
| (parity) | | 16 more: `more_phrasings_match_python`, `environment_settings_parse_as_python`, `close_spellings_rank_as_difflib_does`, `recent_file_is_written_as_python_writes_it`, `malformed_recent_rows_are_skipped`, `an_unwritable_recent_file_is_not_an_error`, `shlex_quote_matches_python`, `capitalize_matches_python`, `trigger_argument_is_the_rest_of_the_words`, `a_question_expires_after_five_minutes`, `edge_answers_follow_python`, `list_step_text_and_title`, `many_hits_are_capped_at_ten`, `open_session_refuses_a_missing_folder`, `open_session_reports_an_unwritable_warp_folder`, `run_returns_code_and_first_stream` | Expected values from Python 3 output |

## Boundaries
`open_session` runs `open warp://launch/eve-claude` or `osascript` as subprocesses, as the Python does. Nothing else.

## Differences
- A reply of non-ASCII decimal digits (for example Arabic-Indic `٣`) is matched as a folder name; Python's `isdigit`/`int` would read it as a number (and crash on digits like `²` that `int` refuses). The phone sends ASCII digits.
- `load_recent` treats a `"recent"` value that is a number, boolean or null, or a `harness` that is a list or object, as no rows; Python raises `TypeError` there. A JSON file with `NaN`/`Infinity` or a lone surrogate escape, which Python's `json` accepts, is unreadable here and counts as no recent sessions.
- `_label` of the code root itself raises `IndexError` in Python; here it is the folder's path.
- When the home folder cannot be found, Python raises at import (`RECENT_PATH`) or in `code_root`; here `RECENT_PATH` falls back to `~/.config/...` literally and `code_root` to the unexpanded path. `open_session` still fails (an `io::Error`) where Python's `Path.home()` would raise.
- `open_session` with an unknown harness and no `command` returns an `InvalidInput` error (`KeyError: '<name>'`), Python's `KeyError`.
- Folder names that are not UTF-8 are read lossily (APFS stores only UTF-8 names).
