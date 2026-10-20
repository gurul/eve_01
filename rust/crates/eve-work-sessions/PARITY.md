# eve-work-sessions
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/work_sessions.py` | `src/lib.rs` (Python semantics it leans on in `src/py.rs`), `[[bin]] eve-work-sessions` in `src/main.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| constants (`REGISTRY_NAME`, `FILES_NAME`, `UUID_RE`, `MAX_*`, `TITLE_CHARS`, `LINE_CHARS`, `TURN_CHARS`, `TURNS_SHOWN`, `TAIL_BYTES`, `LINE_TAIL_BYTES`, `DEDUPE_SECS`, `GRACE_SECS`, `STOP_WAIT_SECS`, `KEEP_SECS`, `MAX_LISTED`, `STATES`, `ENDED_CODE`, `OFF_LINE`, `ENDED`, `NO_WINDOW`, `NOT_OURS`, `UNKNOWN`, `MASK`, `TERMINAL_OPEN`, `TERMINAL_TYPE`, `SAVED_FIELDS`) | same names as `pub const` | Values and AppleScript sources verbatim. `UUID_RE.match` is `is_uuid` (a single trailing newline passes, as Python's `$` lets it). `CONFIG_DIR` is `config_dir()`. |
| `enabled(environ)` | `enabled(Option<&Environ>)` | `None` is the process environment. |
| `work_root(environ)` | `work_root(Option<&Environ>)` | `EVE_WORK_ROOT` stripped and `~`-expanded through `eve_claude_home::HomeEnv::expanduser` (pathlib-normalised). |
| `files_dir(home)` | `files_dir(Option<&Path>)` | |
| `flat(text, limit)` | `flat(&str, usize)` | Python whitespace (`str.split`), character counts. |
| `clip(text, limit)` | `clip(&str, usize)` | |
| `redact(text)` | `redact(&str)` | The seven token patterns and `_KEY_VALUE` through `regex`; the private-key pattern (its `$`) and `_LONG_RANDOM` (lookaheads) are hand-written matchers with Python's backtracking result. |
| `derived_transcript(cwd, sid, config_dir)` | `derived_transcript(&Path, &str, Option<&Path>) -> Result<PathBuf, HomeDirError>` | `config_dir` None is `eve_claude_home::claude_config_dir`. |
| `_epoch(stamp)` | `epoch` (private) over `py::iso_timestamp` | CPython 3.13's C `fromisoformat` grammar (the daemon's interpreter), then `.timestamp()`. |
| `_texts(content, role)` | `texts` (private) | |
| `read_tail(path, limit)` | `read_tail(&Path, u64) -> (Vec<Map>, bool)` | A UTF-8 BOM is read as an encoding mark, as `json.loads(bytes)` does. |
| `transcript_turns(path, limit, tail, chars)` | `transcript_turns(Option<&Path>)` and `transcript_turns_with(.., limit, tail, chars)` → `(Vec<Turn>, bool)` | `Turn { role, text, at }` serialises as Python's dict. `limit == 0` keeps every turn, as `turns[-0:]` does. |
| `last_line(path)` | `last_line(Option<&Path>)` | |
| `_mtime(path)` | `mtime` (private) | |
| `session_pids(run)` | `session_pids(eve_claude_live::Run) -> Option<IndexMap<String, i64>>` | Uses `eve_claude_live::is_claude`, `PS`, `PROBE_TIMEOUT_SECS`. |
| `launch_argv(sid, task, mcp_config, claude)` | `launch_argv(..) -> Result<Vec<String>, NotASessionId>` | `NotASessionId` is the `ValueError`. |
| `mcp_config(sid, token, python, environ)` | `mcp_config(..)`, plus `mcp_config_native` | Byte-identical JSON through `py::dumps` (ASCII-escaped, `", "` / `": "`). |
| `_write_private(path, text)` | `write_private(&Path, &str)` | Leaf folder 0700 (chmod again), file 0600 (chmod again). |
| `_osascript(script, *args)` | `osascript(String, Vec<String>)` (async) | Exit code (minus the signal), stdout or stderr, stripped, first 200 characters. |
| `_hash(token)` | `hash_token(&str)` | sha256 hex. |
| `Record` | `Record` (`new`, `over()`, `saved()`) | `saved()` keeps `SAVED_FIELDS` order and float-ness (`1000.0`). Loading follows `_load`'s `float()` / `int()` / `str()` / `bool()` coercions exactly (`py::to_float`, `to_int`, `to_str`, `truthy`). |
| `WorkSessions` | `WorkSessions` (a cheap `Clone` handle) built from `Options` | Every Mac seam injectable as in Python: `osascript`, `run`, `kill`, `live`, `clock`, `sleep`, `state`, `environ`, `claude_dir`; `python` is `launch: Launch` (see Differences). `records()` is Python's `self.records`; `row()` is `_row`; routes `list`, `start`, `detail`, `reply`, `stop` answer `(u16, serde_json::Value)`; `note`, `check_token` as in Python. `_load`, `_save`, `_transcript`, `_probe`, `_end`, `_rows` are private (`load`, `save`/`save_locked`, `transcript_of`, `end`, `rows_of`; `_probe` is inlined in `list`). |
| `main(argv, home, execvp)` | `main()` and `main_with(args, home, which, execvp)`; binary `eve-work-sessions launch <sid>` | `shutil.which` is the `which` seam (the `which` crate by default); `os.execvp` is `execvp` (`CommandExt::exec`). Exit codes 2 / 1 / 127 as Python. The task is read as `read_text` reads it (universal newlines: `\r\n` and `\r` become `\n`). |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_work_sessions.py` | `test_a_task_is_never_in_a_shell_line_or_a_script_only_claudes_argv` (all 7 HOSTILE tasks), `test_the_session_runs_in_personal_work_never_the_work_area`, `test_eves_mcp_config_is_private_and_holds_only_a_token_hash_in_the_registry`, `test_the_same_client_id_is_one_session_and_one_terminal`, `test_a_bad_start_is_a_400_and_opens_nothing` (all 7 cases), `test_terminal_failing_is_a_503_and_leaves_nothing_behind`, `test_a_window_whose_id_terminal_did_not_give_runs_but_takes_no_reply`, `test_session_end_is_not_the_end_while_the_process_runs`, `test_states_follow_the_hooks_and_the_list_has_every_live_session`, `test_a_session_whose_process_is_gone_leaves_the_list`, `test_detail_is_the_latest_turns_only_words_clipped_and_without_secrets`, `test_detail_ends_a_session_whose_claude_exited_and_says_ended`, `test_a_session_past_its_grace_without_a_process_is_never_starting`, `test_the_mcp_server_never_has_the_work_folder_on_sys_path`, `test_detail_keeps_the_last_twenty_turns_of_a_long_transcript`, `test_a_transcript_path_is_derived_when_no_hook_gave_one`, `test_a_reply_types_into_that_sessions_tab_only_as_an_argument`, `test_a_reply_is_refused_when_the_session_is_not_live_or_not_ours`, `test_stop_signals_only_that_sessions_process_and_is_idempotent`, `test_a_session_that_ignores_sigterm_gets_sigkill`, `test_ps_finds_session_ids_only_before_the_task`, `test_secrets_are_masked`, `test_the_switch` | same names in `src/tests.rs` | `test_minus_p_keeps_a_planted_stdlib_name_in_the_work_folder_from_loading` tests CPython's `-P` flag, not this module (the line that passes `-P` is asserted byte for byte in the first test). `test_every_session_route_needs_the_link_token_and_no_origin`, `test_the_routes_shapes_wrong_methods_and_unknown_paths`, `test_work_off_is_a_503_on_every_route` test link_chat's HTTP routes (auth, Origin, 405/404, `NO_WORK`): they belong to the `link` crate, which lists this file in its `tests_ref`. |
| `bridge/tests/test_eve_mcp.py` (ref) | `test_the_daemon_answers_only_a_live_sessions_token` (the work-session half: `Record`, `_hash`, `check_token`) | `test_the_daemon_answers_only_a_live_sessions_token` | The IPC handlers (`mcp_tools`, `mcp_call`) belong to the daemon crate. |
| (parity) | | `redact_matches_python_on_the_edges`, `flat_clip_and_uuid_match_python`, `json_dumps_matches_python`, `a_registry_python_wrote_loads_as_python_reads_it_and_saves_as_python_writes_it`, `iso_stamps_read_as_python_3_13_reads_them`, `shlex_quote_matches_python`, `the_native_launch_runs_this_crates_binary_and_server`, `launch_refuses_a_bad_command_line_and_a_missing_task`, `a_hook_transcript_path_is_saved_and_survives_a_restart`, `launch_reads_the_task_with_universal_newlines_as_read_text_does` | Expected values printed by Python (3.13 for `fromisoformat` and `redact`). |

## Boundaries
None. osascript, ps and kill are the same system calls and tools Python uses.

## Differences
- **Launch.** Python's line and MCP server name its own `sys.executable`; a Rust daemon has no interpreter of its own, so `Options::launch` says what to start. `Launch::Python { python }` gives Python's line and `mcp.json` byte for byte (`cd <root> && exec <python> -P -m eve_bridge.work_sessions launch <sid>`, server `<python> -P -m eve_bridge.eve_mcp`); the daemon passes eve's venv interpreter there. Its default is `python3` from `PATH`, where Python defaults to the running interpreter. `Launch::Native { launcher, mcp }` starts this crate's `eve-work-sessions launch <sid>` and an MCP server binary with no arguments (the `eve-mcp` binary of `eve-eve-mcp`, which reads `EVE_MCP_SESSION` / `EVE_MCP_TOKEN` as `eve_mcp.py` does).
- The binary's usage line names itself (`usage: eve-work-sessions launch <session id>`) rather than `python -m eve_bridge.work_sessions`. A task file that is not UTF-8, or an exec that fails, prints one line and exits 1 where Python prints a traceback and exits 1.
- `state` is `Fn() -> Option<Arc<parking_lot::Mutex<eve_state::State>>>`. The daemon's sessions are snapshotted (with `_needs_input_live` evaluated) on the caller's task before the list's worker thread reads transcripts, where Python reads the state on the thread.
- `stop` returns `io::Result`: an `os.kill` error other than no-such-process or not-permitted is an `Err`, as Python lets it raise.
- Where Python would raise out of a route (no home directory for `claude_config_dir`, `~user` in `EVE_WORK_ROOT` naming no user, an `inf` `window_id` in the registry, `hmac.compare_digest` on a non-ASCII stored hash), the Rust port degrades: no derived transcript, the unexpanded path, the row skipped, the token refused.
- `serde_json` rejects what `json.loads` accepts beyond JSON (`NaN`, `Infinity`, lone surrogates) and UTF-16/32 lines, so such a transcript line or registry is skipped / read as unreadable.
- Python's `\w` (`str.isalnum` or `_`) and the `regex` crate's Unicode `\w` differ on a few non-ASCII characters (combining marks, connector punctuation, `No` numerics); this only moves a word boundary next to such a character in the token patterns. `_LONG_RANDOM` uses Python's definition.
- `fromisoformat` week dates (`2026-W42-1`) read as no timestamp (0).

## Verification
`redact`, `flat`, `clip` (6000 generated strings) and `transcript_turns` / `last_line` (a generated 400-line transcript, four tails, three limits) were compared against CPython 3.13 running `work_sessions.py`: identical output.
