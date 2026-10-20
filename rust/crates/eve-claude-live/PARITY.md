# eve-claude-live
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/claude_live.py` | `crates/eve-claude-live/src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `PS`, `LSOF` | `PS`, `LSOF` | Same absolute paths. |
| `PROBE_TIMEOUT_SECS` (1.5) | `PROBE_TIMEOUT_SECS: f64 = 1.5` | Each subprocess call. |
| `GRACE_SECS` (30.0) | `GRACE_SECS: f64 = 30.0` | |
| `Run` (callable alias) | `Run<'a> = &dyn Fn(&[String], Duration) -> Result<RunOutput, RunError>` | The `subprocess.run` seam. `RunOutput { returncode, stdout }`; `RunError::{Os, Timeout}` stands for the caught `OSError` / `SubprocessError`, and `RunError::type_name()` gives the Python class name for the debug log. `subprocess_run` is the default (stdout captured, stderr captured and dropped, child killed on timeout, returncode negative signal when killed). |
| `RUNTIMES` | `RUNTIMES: [&str; 3]` | `node`, `bun`, `deno`. |
| `_is_claude(args)` | `is_claude(&str) -> bool` | Public: `work_sessions` calls it. Same three rules (basename `claude`; runtime + basename `claude`; `claude-code/cli` in words 1..3). |
| `HEADLESS_FLAGS` | `HEADLESS_FLAGS: [&str; 5]` | Same five flags. |
| `interactive(args)` | `interactive(&str) -> bool` | Flag compared before any `=`. |
| `live_cwds(run)` | `live_cwds(Run) -> Option<HashSet<String>>` | |
| `live_processes(run)` | `live_processes(Run) -> Option<IndexMap<String, bool>>` | Same argv for ps (`-axww -o pid=,args=`) and lsof (`-a -b -w -d cwd -Fn -p <pids>`), lsof output read whatever its exit code, None on a failed/blind probe; insertion order kept like the dict. Verified live on this Mac: same map, same order as the Python. |
| `_resolved(path)` | private `resolved(&str) -> PathBuf` over `realpath` | `realpath` is a line-for-line port of CPython 3.13 `posixpath.realpath(strict=False)` (missing parts kept, `..` after resolving, symlink loops left unresolved); falls back to the path as given when `getcwd` fails. |
| `alive(cwd, live)` | `alive(&str, impl IntoIterator<Item: AsRef<str>>) -> bool` | Same folder or a parent of it, both resolved. |
| `picker_sessions_off_loop(state, probe)` | `async picker_sessions_off_loop(&mut S, Option<BoxedProbe>)` and `picker_sessions_off_loop_with(&mut S, Option<BoxedProbe>, processes)` | Probe on `spawn_blocking` (as `asyncio.to_thread`), pruning on the caller's task. `_with` takes the `live_processes` call as a parameter (the seam the Python tests patch). A panic in the probe resumes on the caller, as the exception would. |
| `picker_sessions(state, live, *, probe, now, unknown)` | `picker_sessions(&mut S, Option<HashSet<String>>, Option<Probe>, Option<f64>, &[String]) -> Vec<String>` | Same order (stable newest-first), grace, drop via `session_end`, the info log line, and sorted `unknown` appended when no kept folder is in it. `live` and `probe` both None probe the Mac with `live_cwds(subprocess_run)`. |
| (`state: Any`) | trait `SessionBook` + `KnownSession`, implemented for `eve_state::State` | What the picker reads (`sessions`) and calls (`session_end`). |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_claude_live.py` | `test_the_claude_processes_and_their_folders_are_read`, `test_a_pid_gone_since_ps_still_gives_the_rest`, `test_a_probe_that_cannot_tell_says_so`, `test_a_session_in_a_subfolder_of_a_live_process_is_alive`, `test_the_picker_drops_dead_sessions_and_forgets_them`, `test_a_blind_probe_keeps_every_session`, `test_the_daemon_lends_the_live_list_to_the_inlet`, `test_a_session_run_through_the_npm_bin_shim_is_a_claude_process`, `test_the_probe_runs_off_the_event_loop_and_the_pruning_on_it`, `test_each_folder_says_whether_an_owner_can_type_there`, `test_after_a_restart_claude_on_still_finds_the_open_session`, `test_a_known_session_is_not_offered_twice`, `test_a_blind_probe_offers_only_what_the_hooks_told` | Same names without the `test_` prefix in `src/tests.rs`, plus `a_blind_boxed_probe_keeps_every_session`, `equal_start_times_keep_the_state_order`, `an_empty_cwd_is_never_in_the_picker`, `unknown_folders_follow_in_sorted_order` (expectations checked against the Python), `realpath_folds_dots_and_follows_symlinks` (expectations recorded from Python 3.13 `Path.resolve`), `the_log_name_is_the_last_folder`, `the_runner_captures_stdout_and_the_exit_code`, `the_runner_times_out_and_names_its_errors_as_python_does`, and ignored live `the_live_probe_reads_this_mac` | `test_the_daemon_lends_the_live_list_to_the_inlet`: the `Daemon._make_telegram` wiring belongs to the daemon crate; its claude_live assertions (only the live folder is offered, the dead session leaves the state) are ported against `picker_sessions_off_loop_with`. |

## Boundaries
`ps` and `lsof` are run as subprocesses, exactly as the Python does (this is the module's design, not a fallback).

## Differences
- Subprocess output is decoded as lossy UTF-8; Python's `text=True` would raise `UnicodeDecodeError` (uncaught) on invalid bytes in a process's arguments or folder name.
- Output is split with `str::lines` (`\n`, `\r\n`); Python's `splitlines` also splits on rarer separators (`\v`, `\f`, `\x1c`..., ` `) that ps and lsof do not emit.
