# eve-update
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/update.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `package_repo_root` | `package_repo_root()`, `package_repo_root_from(&Path)` | walks up from the module's directory (this crate's `src/` in the checkout it was built from, the analog of `Path(__file__).parent`); canonicalized like `resolve()`, walked as given when that fails; a `.git` file (worktree) counts, as `exists()` does |
| `install_target` | `install_target(platform, machine: Option<&str>)`, `default_install_target()`, `platform_machine()`, `sys_platform()` | `.[laya]` only for `darwin` + `arm64`; `platform_machine` is `os.uname().machine` on Unix (`arm64` on Apple silicon), `PROCESSOR_ARCHITEW6432`/`PROCESSOR_ARCHITECTURE` on Windows, as `platform.machine()` |
| `_run` | `run(cmd, cwd) -> (i32, String, String)` | missing program or cwd: rc 127 and Python's `FileNotFoundError` text (`[Errno 2] No such file or directory: '<name>'`, naming the cwd when the chdir is what failed); signal deaths are `-signum` like `returncode` |
| `_git_status_clean` | `git_status_clean(repo)`, `git_status_clean_with(host, repo)`, `Host::git_status_clean` | `git status --porcelain`, clean = rc 0 and output blank after `str.strip()` |
| `_current_branch` | `current_branch(repo)`, `current_branch_with(host, repo)`, `Host::current_branch` | `git branch --show-current`, stripped; `""` on failure |
| `_detect_service_backend` | `detect_service_backend()`, `detect_service_backend_for(host, platform, home)`, `Host::detect_service_backend` | launchd plist `~/Library/LaunchAgents/com.github.eve-bridge.daemon.plist`; `schtasks /query /tn eve-bridge-daemon` on win32; systemd unit `~/.config/systemd/user/eve-bridge.service` otherwise; `home()` is `Path.home()` (`$HOME`, then the password database) |
| `_restart_service` | `restart_service(backend)`, `restart_service_with(host, backend)`, `Host::restart_service` | same commands (`launchctl kickstart -k gui/<uid>/com.github.eve-bridge.daemon`, `systemctl --user restart eve-bridge.service`, `schtasks /end` then `/run`), same messages, `unknown error` fallback, `{backend!r}` via `py::str_repr` |
| `run_update` | `run_update(yes) -> i32`, `run_update_using(host, yes)`, `run_update_with(host, yes, out, err)` | every message verbatim (incl. `—` and `→`), same stream (stdout/stderr), same exit codes 0/1/2, same `end=""` join of the Steps line, prompt `\nProceed? [y/N] ` on stdout; `y`/`yes` after `strip().lower()` |
| (seam) | `trait Host`, `struct System { python }` | `Host` carries every effect the Python tests monkeypatch (`check`, `package_repo_root`, `_run`, `_git_status_clean`, `_current_branch`, `_detect_service_backend`, `_restart_service`, `sys.stdin.isatty`, `input`, `sys.executable`); defaults are the real ones and the composed helpers go through `Host::run`, as unpatched module functions go through `_run` |
| (constants) | `LAUNCHD_LABEL`, `SYSTEMD_UNIT`, `SCHEDULER_TASK` | the literals the Python inlines |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_update.py` | `test_package_repo_root_finds_dotgit`, `test_git_status_clean_when_no_output`, `test_git_status_clean_false_when_dirty`, `test_git_status_clean_false_on_git_failure`, `test_run_update_already_on_latest`, `test_run_update_network_failure`, `test_run_update_not_a_git_checkout`, `test_run_update_refuses_dirty_tree`, `test_run_update_no_tty_no_yes_bails`, `test_run_update_user_declines`, `test_run_update_full_flow_with_yes`, `test_run_update_git_pull_fails` | same names in `src/tests.rs` (monkeypatches become a fake `Host`; capsys becomes the `out`/`err` writers; several assertions tightened to the exact Python text) | - |
| `bridge/tests/test_cli_lite.py` (tests_ref) | `test_lite_update_refuses_with_the_install_line` | - | tests the `cli` crate's lite guard, which must refuse before `run_update` is called; nothing in this crate runs |
| (extra) | - | `package_repo_root_none_outside_a_checkout`, `package_repo_root_finds_nearest_dotgit_file_or_dir`, `package_repo_root_walks_an_unresolvable_path_as_given`, `install_target_laya_only_on_apple_silicon`, `install_target_defaults_to_this_machine`, `platform_machine_reads_uname`, `run_captures_rc_stdout_stderr`, `run_uses_cwd`, `run_missing_program_is_127_with_python_text`, `run_missing_cwd_names_the_cwd` (both texts recorded from CPython), `run_signal_is_negative`, `git_status_clean_whitespace_only_is_clean`, `git_status_clean_real_git_on_a_fresh_repo`, `current_branch_strips_and_blanks_on_failure`, `detect_service_backend_darwin_needs_the_plist`, `detect_service_backend_linux_needs_the_unit`, `detect_service_backend_windows_asks_schtasks`, `restart_service_launchd_kickstarts_the_label`, `restart_service_systemd`, `restart_service_scheduler_ends_then_runs`, `restart_service_unknown_backend_uses_repr`, `run_update_older_latest_is_already_latest`, `run_update_eof_at_the_prompt_aborts`, `run_update_user_accepts_with_yes_in_any_case`, `run_update_pip_failure_says_the_repo_is_updated`, `run_update_without_backend_asks_for_a_manual_restart`, `run_update_failed_restart_is_exit_2`, `system_host_defaults_to_python3` | - |

## Boundaries
- The update itself is subprocesses, as in Python: `git`, `<python> -m pip install -e <target>`, `launchctl` / `systemctl` / `schtasks`.

## Differences
- `sys.executable`: a Rust process has no interpreter of its own, so the pip step runs `System::python`. The caller passes eve's venv interpreter (`System { python }` with `run_update_using`); The printed `$ <python> -m pip install -e <target>` line names that interpreter. This is the Python-parity path (`Host::native() == false`); `run_update(yes)`, which the Rust CLI calls, takes the native path below instead.
- `package_repo_root` starts from the source directory the binary was built from (`CARGO_MANIFEST_DIR/src`) instead of the installed module file. A binary moved off its checkout (or the checkout removed) finds no `.git` and reports "isn't a git checkout", the analog of a wheel install.
- `_run`: an `OSError` other than `FileNotFoundError` (e.g. permission denied) propagates out of Python and crashes the command with a traceback; Rust returns rc 126 with the same `[Errno N] <text>: '<name>'` shape, so `run_update` prints its usual "failed" line and exits 2. Output is decoded as lossy UTF-8 where Python's `text=True` could raise `UnicodeDecodeError`.
- Ctrl-C at the confirmation prompt: Python catches `KeyboardInterrupt`, prints `\nAborted.` and exits 1; the Rust process takes the default SIGINT action and exits on the signal. End of input (`EOFError`) is handled identically (`\nAborted.`, exit 1).
- `launchd` restart off Unix (unreachable: launchd is only detected on darwin): Python's `os.getuid` raises `AttributeError`; Rust returns `(false, "launchctl failed: os.getuid is unavailable on this platform")`.
- **The Rust daemon's own update takes a native path** (`run_update`, `System { native: true }`): after
  `git pull` it runs `cargo build --release --locked` in `rust/` for `NATIVE_PACKAGES` (eve-bridge, eve-hooks and the helper binaries the daemon starts beside itself: eve-desktop-worker, eve-web's eve-watch, eve-vault-mcp, eve-eve-mcp's eve-mcp, eve-work-sessions), signs
  `rust/target/release/eve-bridge` (`EVE_CODESIGN_IDENTITY`, else the first Apple Development identity, so
  macOS keeps its permissions across rebuilds), refreshes `bridge/.venv` with `install.sh`'s extras for the
  Python helpers (a failure there warns and goes on), then restarts. Python's `pip install -e` path is
  unchanged behind `Host::native() == false` and keeps its ported tests; the native path has its own five.
