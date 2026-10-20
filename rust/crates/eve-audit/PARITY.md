# eve-audit
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/audit.py` | `src/lib.rs` (Python `json`/float helpers in `src/pyjson.rs`; tests in `src/tests.rs` and `tests/render_memory.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `default_path` | `default_path()`, `default_path_from(override, xdg_data_home, home)` | `EVE_BRIDGE_AUDIT` (non-empty), pathlib-normalized first and then a leading `~`/`~user` component expanded (`path_expanduser`), as `Path(x).expanduser()` does, so `./~/x` expands too; macOS `~/Library/Logs/eve-bridge-audit.jsonl`; Windows `~/AppData/Local/eve-bridge/audit.jsonl`; else `$XDG_DATA_HOME` (non-empty) or `~/.local/share`, then `eve-bridge/audit.jsonl` |
| (`Path.expanduser`) | `expanduser(&str)` | `posixpath.expanduser`: `$HOME`, else passwd; unknown user leaves the path alone |
| `AuditLog` | `AuditLog { path }`, `AuditLog::new(Option<PathBuf>)` | `_failure_logged` is an `AtomicBool` read with `failure_logged()`, so `record` takes `&self` |
| `AuditLog.record` | `AuditLog::record(Record)`, `AuditLog::record_at(Record, &DateTime)`, `entry_line(Record, &DateTime)` | `Record` holds the keyword arguments. Line bytes equal `json.dumps(entry, ensure_ascii=False) + "\n"`: key order, `", "`/`": "` separators, control-char escapes, Python float repr (shortest round-trip digits, nearest with ties to even, cross-checked against CPython on 20,000 values), `round(elapsed_s, 3)` ties-to-even, `jev` only when non-empty. `ts` is `isoformat(timespec="milliseconds")` in local time. Session cut to 8 and hint to 200 characters (code points). mkdir + append errors are swallowed and warned once: `audit log write failed (will not retry-log): <path>: [Errno N] <strerror>: '<file>'`, where `<file>` is the directory pathlib's `mkdir(parents=True)` recursion failed on (`mkdir_parents`), the file `open` failed on, or absent for a failed write (`py_oserror`, `py_repr_str`) |
| `_ANSI_RESET` … `_ANSI_YELLOW` | `ANSI_RESET`, `ANSI_DIM`, `ANSI_RED`, `ANSI_GREEN`, `ANSI_YELLOW` | |
| `iter_entries` | `iter_entries(Option<&Path>, Filters) -> Entries` (`Iterator<Item = io::Result<Entry>>`) | Streams line by line. Text-mode reading kept: universal newlines, invalid UTF-8 replaced with U+FFFD, Python `str.strip()` whitespace, non-object and undecodable lines skipped, bare `NaN`/`Infinity`/`-Infinity` accepted as `json.loads` does. Missing file yields nothing; an open/read error is yielded once (where the generator raises) |
| `_fmt_decision` | `fmt_decision` (private) | `(dec or "—")[:5]` padded to 5; green allow, red deny, dim otherwise |
| `_fmt_source` | `fmt_source` (private) | `[:11]` padded to 11; yellow for `telegram`/`stick`, red for `timeout`, dim otherwise |
| `format_entry` | `format_entry(&Entry, ascii_only, width)` | Same columns, `max(20, width - 40)` hint width, `…` truncation, newline → space; widths counted in characters |
| `_terminal_width` | `terminal_width(default)` | `ioctl(TIOCGWINSZ)` on fd 1, as `os.get_terminal_size()`; `default` (120) when it fails |
| `render` | `render(&RenderOptions, &mut impl Write, out_isatty) -> io::Result<i32>`, `render_stdout(&RenderOptions)` | `RenderOptions` defaults match (`last = 20`). Header dim only when `out` is a tty and not `--ascii`; empty-file message verbatim; `last > 0` keeps a bounded tail (VecDeque), `<= 0` streams everything; follow mode polls every 0.5 s, re-reads from the start when the file shrinks, flushes per line |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_audit.py` | `test_default_path_returns_path`, `test_default_path_respects_env_override`, `test_record_appends_jsonl`, `test_record_long_hint_is_truncated`, `test_record_creates_parent_dir`, `test_record_swallows_io_errors`, `test_record_decision_can_be_none`, `test_iter_entries_yields_in_order_and_filters`, `test_iter_entries_skips_corrupt_lines`, `test_iter_entries_missing_file_yields_nothing`, `test_format_entry_truncates_long_hint`, `test_format_entry_shows_time_tool_decision_hint`, `test_format_entry_ascii_omits_ansi`, `test_format_entry_color_emits_ansi`, `test_render_prints_header_and_last_n_entries`, `test_render_missing_file_says_empty`, `test_render_tails_matching_entries_in_order`, `test_render_non_positive_last_keeps_the_complete_history`, `test_render_tail_memory_does_not_scale_with_history`, `test_iter_entries_skips_non_object_json` | Same names in `src/tests.rs`; the memory test is `tests/render_memory.rs` (a counting global allocator stands in for tracemalloc, keeping the positive control). Added: `entry_line_matches_python_json_dumps_bytes`, `entry_line_omits_empty_jev_and_rounds_elapsed`, `python_float_repr_and_round`, `record_truncates_by_characters_not_bytes`, `iter_entries_reads_like_python_text_mode`, `format_entry_columns_match_python`, `render_header_is_dim_on_a_tty_unless_ascii`, `render_follow_streams_new_matching_entries_and_survives_truncation`, `default_path_*` (incl. `default_path_override_normalizes_before_expanding_like_pathlib`), `expanduser_matches_posixpath`, `write_failure_message_names_the_path_python_names` | None. The env-override test calls `default_path_from` instead of setting the process env |

## Boundaries
None.

## Differences
- Follow mode ends when `RenderOptions::stop` is set, where Python caught `KeyboardInterrupt` and returned 0. The CLI's Ctrl-C handler sets the flag; without one, SIGINT ends the process as usual.
- A field of the wrong JSON type in a hand-edited log (a number `ts`, a list `hint`, a truthy non-string `decision`) renders as if absent; Python raises `TypeError` and the viewer stops.
- Bare `NaN`/`Infinity` values read back as `null` (a JSON value cannot hold them); only the displayed string fields matter to the viewer, so output is the same. A line with a lone `\ud800` escape, which Python's `json.loads` accepts, is skipped.
- An `EVE_BRIDGE_AUDIT` starting with `~unknown-user` is kept literally; pathlib raises `RuntimeError` ("Could not determine home directory.") and the daemon fails to start.
- `elapsed_s` is always a float, so it is written as `2.0` where a Python caller passing the int `2` would get `2`. No caller passes `elapsed_s` today.
