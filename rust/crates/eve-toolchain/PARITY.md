# eve-toolchain
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/toolchain.py` | `src/lib.rs` (the module), `src/pyglob.rs` (CPython's `glob.glob` and `fnmatch.fnmatchcase`, which `developer_dirs` reads `XCODE_GLOB` with) |

## API
| Python | Rust | Notes |
|---|---|---|
| `SHIM_DIR` | `SHIM_DIR`, `Toolchain::shim_dir` | |
| `SHIMS` | `SHIMS` (`LazyLock<BTreeSet<&str>>`) | same 78 names |
| `XCODE_SHIMS` | `XCODE_SHIMS` | same 17 names, xcrun included |
| `DEVELOPER_DIR_ENV` | `DEVELOPER_DIR_ENV` | |
| `SELECT_LINK` | `SELECT_LINK`, `Toolchain::select_link` | |
| `XCODE_GLOB` | `XCODE_GLOB`, `Toolchain::xcode_glob` | |
| `CLT_DIR` | `CLT_DIR`, `Toolchain::clt_dir` | |
| `GIT_IN_DEVELOPER_DIR` | `GIT_IN_DEVELOPER_DIR` | `"usr/bin/git"` |
| `sys.platform` (read by `git_usable`) | `sys_platform()`, `Toolchain::platform` | "darwin" / "linux" / "win32" |
| `_program` | `program` | `eve_profile::pypath::runnable`: `isfile` + `access(X_OK)` |
| `developer_dirs` | `developer_dirs(Option<&HashMap>)`, `Toolchain::developer_dirs(&impl EnvMap)` | `None` = process environment; `str.strip()` on `DEVELOPER_DIR`; `sorted(glob(...))` |
| `dev_tools_present` | `dev_tools_present`, `Toolchain::dev_tools_present` | starts no process |
| `which` | `which(name, Option<&HashMap>)`, `which_in(name, &impl EnvMap)` | `os.get_exec_path` rules: unset PATH is `os.defpath` (`/bin:/usr/bin`), an empty entry is the working folder |
| `is_shim` | `is_shim`, `Toolchain::is_shim` | `realpath` + `dirname`, compared with `str.casefold()` (the `caseless` crate, as eve-profile uses) |
| `git_usable` | `git_usable(Option<&HashMap>) -> Result<bool, ProfileError>`, `git_usable_in`, `Toolchain::git_usable` | lite → false; not darwin → true; else which + shim + tools |
| (module globals, monkeypatched in tests) | `Toolchain` (`system()`, `Default`) | the locations and platform as fields, so tests and callers can point them elsewhere without mutating process state |
| `glob.glob` (stdlib) | `glob` | recursive=False, hidden names skipped, wildcard parts list folders only, literal parts need `lexists` |
| `fnmatch.fnmatchcase` (stdlib) | `fnmatchcase` | CPython 3.13 `translate` semantics, including its range chunking and empty-range removal |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_toolchain.py` | `test_no_developer_directory_anywhere_is_no_tools`, `test_the_command_line_tools_count`, `test_a_git_that_cannot_run_is_not_the_tools`, `test_any_xcode_in_applications_counts`, `test_the_folder_xcode_select_chose_counts`, `test_developer_dir_counts_as_xcrun_reads_it`, `test_dev_tools_present_and_git_usable_start_no_process`, `test_which_follows_subprocess_rules`, `test_is_shim_follows_links_and_ignores_case`, `test_git_usable_is_never_true_in_eve_lite`, `test_a_git_outside_usr_bin_is_git_without_the_tools`, `test_usr_bin_git_counts_only_with_the_tools`, `test_no_git_at_all_is_not_usable`, `test_off_macos_git_runs_as_it_always_has`, `test_this_mac`, `test_the_shim_names_match_this_macs_usr_bin` | same names in `src/tests.rs`; the empty-PATH-entry half of `test_which_follows_subprocess_rules` is `tests/which_cwd.rs` (it changes the working folder, so it runs alone in its own process). Added: `test_xcodes_in_applications_come_sorted`, `test_a_relative_select_link_reads_from_its_folder`, `test_a_bad_profile_is_an_error_as_in_python`, `test_sys_platform_names_this_build_as_python_does`, `pyglob::tests::fnmatchcase_matches_python` (expected values taken from CPython 3.13), `pyglob::tests::glob_lists_folders_and_skips_hidden` | `test_dev_tools_present_and_git_usable_start_no_process` uses a Python audit hook; Rust has none, so the port scans this crate's code (and eve-profile's `pypath`) for any way to start a process, with a positive control. `test_off_macos_records_run_git_as_before`, `test_records_git_raises_oserror_before_any_process`, `test_records_degrade_with_one_log_line_each`, `test_in_eve_lite_records_start_no_git`, `test_where_git_is_usable_records_keep_their_history` test `records._git` and belong to eve-records (its port calls `git_usable` here). |
| `bridge/tests/test_no_devtool_spawns.py` (ref) | `SHIMS`/`XCODE_SHIMS` membership | `test_the_shim_names_match_this_macs_usr_bin` checks the counts and xcrun | the source scan itself reads Python files; it is not this crate's |
| `bridge/tests/test_install_lite_sh.py`, `test_lite_memory_parity.py` (ref) | call `git_usable` | covered by the `git_usable` tests | they test install.sh and lite memory |

## Boundaries
None. Nothing here starts a process; every answer comes from files.

## Differences
- `git_usable` returns `Result<bool, ProfileError>`: Python raises `ProfileError` (a `ValueError`) out of `profile.is_lite` when `EVE_PROFILE` is neither full nor lite; Rust returns it.
- `which`: Python answers None when a mapping holds both `"PATH"` and `b"PATH"` (`os.get_exec_path` raises ValueError). A Rust environment holds text names only, so that case cannot arise. Non-UTF-8 PATH bytes are read lossily.
- The module globals Python's tests monkeypatch are fields of `Toolchain`; the free functions use the real locations.
