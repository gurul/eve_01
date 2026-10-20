# eve-installer
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/installer.py` | `src/lib.rs` |
| (`json.dump(indent=2, ensure_ascii=False)`, `repr`/`str` of parsed JSON) | `src/pyjson.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `MARKER` | `MARKER` | `"eve_bridge.hooks."` |
| `LEGACY_MARKERS` | `LEGACY_MARKERS` | Built with `concat!` from the same two halves, so the old name is not spelled out in the source. |
| `HOOK_TIMEOUT_SECS` | `HOOK_TIMEOUT_SECS` | 330; the report-only timeout 10 is `NOTIFY_TIMEOUT_SECS`. |
| `HOOK_DEFS` | `HOOK_DEFS: &[HookDef]` | Same tuples in the same order; the matcher is `Option<&str>` (None for Python's `None`). |
| `_python_executable` | `python_executable(exe, platform)`; `HookLaunch::Python(exe)` | `sys.executable` is the string in `HookLaunch::Python`; backslashes become `/` on `win32`. |
| `_hook_command` | `hook_command(python, module)`; `HookLaunch::command(module)` / `command_for(module, platform)` | Python form is `<python> -I -m <module>` byte for byte. `HookLaunch::Native(dir)` is `<dir>/eve-hook-<name> # <module>` (see Differences). `native_binary(module)` maps a module to its eve-hooks binary. |
| `_is_our_entry` | `is_our_entry(&Value) -> Result<bool>` | Err (`AttributeError: '<type>' object has no attribute 'get'`) where Python raises on a non-dict entry. |
| `_is_legacy` | `is_legacy(&Value) -> Result<bool>` | Same. |
| `_load_settings` | `load_settings(path)` | `{}` when missing; parse errors propagate. |
| `_save_settings` | `save_settings(path, data)` | Creates the parent, writes `json.dump(indent=2, ensure_ascii=False)` + `\n` in place (truncate, not rename). Floats as Python's `repr` (`pyjson::dumps_indent2`, `py_float_repr`). |
| `_backup` | `backup(path)` | `settings.json.ccbb-backup-%Y%m%d-%H%M%S` (local time) beside the file; content, permissions, access and modification times (`shutil.copy2`). |
| `install_hooks` | `install_hooks(config_dir)`; `install_hooks_in(env, config_dir, launch, console)`; `install_hooks_at(path, launch, console)`; `merge_hooks(data, launch, console)` | Same messages, return codes (2 for a missing config home), legacy cleanup, empty-group cleanup, matcher-group creation (`hooks` key before `matcher`), idempotency check and stale-entry replacement. Errors where Python raises on malformed settings. |
| `uninstall_hooks` | `uninstall_hooks(config_dir)`; `uninstall_hooks_in(env, config_dir, console)`; `uninstall_hooks_at(path, console)`; `strip_hooks(data)` | Python truthiness kept (`hooks.get(event) or []`, `group.get("hooks") or []`: falsy events and groups without hooks are dropped); removals keep key order (`shift_remove`, like `dict.pop`); nothing written when nothing of ours was found. |
| `show_status` | `show_status(config_dir)`; `show_status_in(env, config_dir, service, console)`; `print_installed(data, console)`; `installed_lines(data)` | Same lines, printed one by one (`print_installed`), so on malformed settings the lines before the bad entry are out before the error, as in Python. The matcher is printed as Python's `str()` (`py_str`), so `null` prints `None`. The service section uses `eve_service::Service` (`backend_name`, `is_installed`, `is_loaded`, `unit_path`, `log_path`); the service is built only after the hook lines are printed, where Python imports the module. |
| `_find_matcher_group` | `find_matcher_group(entries, matcher) -> Option<usize>` | Index instead of the dict; absent matcher only matches a group without a `matcher` key. |
| (`print` / `print(file=sys.stderr)`) | `Console` trait, `StdConsole`, `Captured` | Tests read `Captured` as pytest reads `capsys`. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_installer.py` | `test_install_from_scratch`, `test_install_is_idempotent`, `test_uninstall_removes_only_our_entries`, `test_uninstall_drops_empty_hooks_block`, `test_uninstall_when_nothing_to_remove`, `test_install_replaces_the_hooks_from_before_the_rename`, `test_hooks_run_isolated_so_the_session_folder_is_not_on_the_import_path` | Same names in `src/tests.rs` | None. The `temp_settings` fixture sets `HomeEnv.claude_config_dir` to a temp home, so config-home resolution is still exercised without touching the process environment. The isolation probe runs `python3` from `PATH`. |
| `bridge/tests/test_relay_questions.py` (ref) | `test_installer_registers_the_question_and_every_dialog` | Same name | The other tests there belong to the hooks and daemon crates. |
| `bridge/tests/test_cli_lite.py` (ref) | monkeypatches `install_hooks`/`uninstall_hooks`/`show_status` to prove lite never calls them | none here | They test the CLI's lite paths, which belong to the bridge crate. |
| (Rust only) | | `install_and_uninstall_write_the_bytes_python_writes` (golden files in `tests/data/` produced by the Python module from the same fixture, with `sys.executable = /venv/bin/python`), `floats_are_written_as_python_repr`, `install_refuses_a_config_home_that_does_not_exist`, `install_creates_the_settings_file_when_the_home_has_none`, `explicit_config_dir_wins_over_the_environment`, `entries_get_the_decision_timeout_and_matchers_as_defined`, `a_hook_entry_that_is_not_a_dict_raises_as_python_does`, `uninstall_without_a_settings_file_returns_2`, `uninstall_with_only_foreign_hooks_writes_nothing`, `uninstall_drops_falsy_events_and_groups_without_hooks`, `find_matcher_group_treats_absent_matcher_only_as_absent`, `entries_are_ours_by_marker_and_legacy_by_the_old_name`, `python_executable_uses_forward_slashes_on_windows`, `native_commands_name_the_eve_hooks_binaries_and_carry_the_marker`, `native_and_python_installs_replace_each_other`, `backup_is_named_for_the_local_second_and_keeps_the_times`, `status_lists_our_hooks_and_an_unsupported_platform`, `status_without_settings_or_hooks_or_service`, `status_names_the_loaded_unit_and_its_log`, `status_prints_matchers_as_python_str`, `status_prints_the_lines_found_before_a_bad_entry` | |

## Boundaries
None

## Differences
- **The hook command.** Python writes `sys.executable -I -m eve_bridge.hooks.<name>`. A Rust build has no interpreter, so the process forms (`install_hooks`, etc.) use `HookLaunch::current()`: the eve-hooks binaries next to the running binary, written as `<dir>/eve-hook-<name> # eve_bridge.hooks.<name>`. The path is quoted as `shlex.quote` quotes it. The trailing shell comment carries the module name, so `MARKER` identifies the entry exactly as it identifies Python's. Each daemon's install replaces the other's entry for the same module, and each daemon's uninstall removes it. Claude Code runs hook commands through a shell, which drops the comment. A caller that wants Python's exact line passes `HookLaunch::Python(<python path>)`.
- Malformed settings (top level not a dict, a non-dict hook entry, an event that is not a list) are returned as `Err` with Python's exception text, where Python raises a traceback. The text is Python 3.13's (the daemon's venv): an empty dict as a group's `hooks` gives `'dict' object has no attribute 'append'`, where Python before 3.12 says `unhashable type: 'slice'`. Checked against the Python module on 31 malformed settings files for install, uninstall and status (return code, output, exception text, file bytes). The backup made before the error stays, as in Python.
- `serde_json` limits: integers beyond 64 bits are read as floats, numbers beyond the f64 range and `NaN`/`Infinity` literals are rejected, and nesting deeper than 128 levels is rejected. Python's `json` accepts all of these. None of them occur in a Claude Code settings file.
- `backup` copies content, permissions and times. On Linux `shutil.copy2` also copies extended attributes; the Rust version does not.
