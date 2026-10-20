# eve-focus-terminal
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/focus_terminal.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `_ITERM_SCRIPT` | `ITERM_SCRIPT` | verbatim |
| `_ACTIVATE_SCRIPT` | `ACTIVATE_SCRIPT` | verbatim |
| `_ACTIVATE_BY_NAME_SCRIPT` | `ACTIVATE_BY_NAME_SCRIPT` | verbatim |
| `_DEFAULT_ACTIVATE_TARGETS` | `DEFAULT_ACTIVATE_TARGETS` + `default_activate_targets()` | same seven entries in the same order; `ActivateTarget = (String, Option<String>)` |
| `activate_targets` | `activate_targets()` + `activate_targets_from(raw)` | `EVE_FOCUS_APPS` stripped; empty → defaults; comma split, parts stripped, empties dropped; known names matched case-insensitively keep their default spelling and bundle id; nothing left → defaults |
| `_TERMINAL_SCRIPT` | `TERMINAL_SCRIPT` | verbatim |
| `_app_running` | `app_running(name)` + `FocusBackend::app_running` | `pgrep -x <name>`, stdout/stderr discarded, stdin inherited, true on exit 0; a spawn failure is an `Err` worded like Python's `OSError` (`[Errno 2] No such file or directory: 'pgrep'`) |
| `_osascript` | `osascript(script, args)` + `FocusBackend::osascript` | `osascript -e <script> <args…>`; non-zero exit → `None` with `focus: osascript failed: <first 200 chars of stripped stderr>` at debug; else stripped stdout (lossy UTF-8, as `errors="replace"`; `py_strip` = Python `str.strip()`); spawn failure worded like Python's `OSError` |
| `_AX_RAISE_SCRIPT` | `AX_RAISE_SCRIPT` | verbatim |
| `_UNMINIMIZE_SCRIPT` | `UNMINIMIZE_SCRIPT` | verbatim |
| `focus_session_terminal` | `focus_session_terminal(cwd)` + `focus_session_terminal_with(backend, cwd, targets)` | same decision flow: iTerm2, Terminal, AX title raise over every running candidate, frontmost → unminimize check → activate by bundle id / name, else activate the first running app and unminimize; same log lines (`%r` via `py_repr`); `""` results are falsy as in Python; any backend error is logged `focus: failed non-fatally: …` at warn and swallowed |
| (helper) | `needle_for(cwd)` | `os.path.basename(cwd.rstrip("/")) if cwd else ""` |
| (helper) | `py_repr(s)` | Python `repr()` of a str for the log lines, non-printables (`str.isprintable`) escaped as `\xNN` / `\uNNNN` / `\UNNNNNNNN` |
| (helper) | `py_strip(s)` | Python `str.strip()` whitespace set (Rust's plus U+001C..U+001F), used for `EVE_FOCUS_APPS`, its parts and osascript output |
| (seam) | `trait FocusBackend`, `struct SystemBackend` | the two functions Python tests monkeypatch (`_app_running`, `_osascript`) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_focus_terminal.py` | `test_default_targets_without_env`, `test_env_overrides_order_and_keeps_known_bundle_ids`, `test_env_matches_known_names_case_insensitively`, `test_env_blank_or_garbage_falls_back_to_defaults`, `test_nothing_running_makes_no_osascript_calls`, `test_iterm_match_wins_and_stops`, `test_window_title_match_raises_that_window`, `test_frontmost_with_visible_window_stands_down`, `test_frontmost_but_minimized_restores_the_window`, `test_frontmost_with_no_window_on_this_space_activates_by_bundle_id`, `test_frontmost_ax_error_still_activates`, `test_frontmost_name_only_app_activates_by_name`, `test_not_frontmost_no_match_activates_first_running_and_unminimizes`, `test_second_app_title_match_beats_first_app_activate`, `test_empty_cwd_skips_title_matching` | same names in `src/tests.rs`, plus `iterm_error_falls_through_to_terminal`, `empty_osascript_output_is_falsy`, `empty_cwd_sends_tilde_to_iterm`, `failed_activate_tries_the_next_running_app`, `first_frontmost_app_is_the_one_kept`, `backend_errors_are_non_fatal`, `needle_is_the_cwd_basename`, `py_repr_matches_python`, `env_duplicates_and_unknown_case_are_kept`, `scripts_are_verbatim_from_python`, `default_targets_match_python`, `py_strip_matches_python_whitespace`, `py_repr_escapes_non_printable_like_python`, `spawn_error_reads_like_python_oserror`, `real_pgrep_reports_a_missing_process` (macOS) | none. Env monkeypatching became `activate_targets_from(raw)` and an explicit `targets` argument; the patched `_app_running`/`_osascript` became a `FocusBackend` test harness |
| `bridge/tests/test_daemon_touch.py` (ref) | patches `focus_terminal.focus_session_terminal` to a recorder | — | exercises the daemon's `focus` command, which belongs to the daemon crate |

## Boundaries
`pgrep` and `osascript` (iTerm2, Terminal and System Events AppleScript) are subprocesses, exactly as in Python. Off macOS the same commands are attempted; a missing `osascript` surfaces as the same non-fatal warning.

## Differences
A non-UTF-8 `EVE_FOCUS_APPS` value is decoded lossily (Python keeps the raw bytes via surrogateescape); otherwise none.
