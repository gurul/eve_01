# eve-service
Status: complete

## Modules
| Python | Rust |
|---|---|
| `service.py` | `src/service.rs` |
| `_service_launchd.py` | `src/service_launchd.rs` |
| `_service_systemd.py` | `src/service_systemd.rs` |
| `_service_windows.py` | `src/service_windows.rs` |
| (plistlib's XML writer) | `src/plistxml.rs` |
| (`shutil.which`, `subprocess.run`, `print`, `os.getuid`, `sys.executable`) | `src/host.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `service._backend` | `Backend::for_platform(platform)`, `backend_kind(platform)`; `Service::from_process()` | `sys.platform` is `sys_platform()` (`darwin`, `linux`, `win32`, else the OS name) |
| `service._unsupported_platform_msg` | `service::unsupported_platform_msg(platform)` | Same text, platform as Python's `repr` |
| `service.install_service` | `Service::install_service(serial_port)`; free `service::install_service` | Unsupported platform: message to stderr, 2 |
| `service.uninstall_service` | `Service::uninstall_service()`; free `service::uninstall_service` | |
| `service.install_notes_widget` | `Service::install_notes_widget()`; free fn | Non-launchd: "the notes widget is macOS-only (got '<platform>')." and 2 |
| `service.uninstall_notes_widget` | `Service::uninstall_notes_widget()`; free fn | |
| `service.is_installed` | `Service::is_installed() -> io::Result<bool>`; free fn | Unsupported: false |
| `service.is_loaded` | `Service::is_loaded() -> io::Result<bool>`; free fn | Unsupported: false |
| `service.backend_name` | `Service::backend_name()`; free `service::backend_name()` | `launchd`, `systemd`, `task-scheduler`, or None |
| `service.unit_path` | `Service::unit_path() -> Option<Location>`; free fn | `Location::Path` / `Location::Text` for Python's Path / str |
| `service.log_path` | `Service::log_path() -> Option<Location>`; free fn | journalctl line on Linux is `Location::Text` |
| `_service_launchd.NAME`, `LABEL`, `WIDGET_LABEL`, `LITE_LABEL`, `LITE_LOG_NAME`, `LITE_PATH`, `LITE_THROTTLE_SECS`, `LITE_UMASK` | consts of the same names | |
| `_service_launchd.PLIST_PATH`, `LOG_PATH`, `WIDGET_PLIST_PATH`, `WIDGET_LOG_PATH` | `FullPaths` (`plist_path`, `log_path`, `widget_plist_path`, `widget_log_path`); `FullPaths::process()` | Fixed once per process from `Path.home()`, as at Python's import |
| `_service_launchd._base_plist` | `Launchd::base_plist` | |
| `_service_launchd._build_plist` | `Launchd::build_plist(serial_port)` | Byte for byte (owner golden test + live cross-check against the Python module) |
| `_service_launchd._build_widget_plist` | `Launchd::build_widget_plist()` | |
| `_service_launchd._load` | `Launchd::load(path)` | unload, then `load -w`; "launchctl load failed (<rc>): <stderr>" and 2 |
| `_service_launchd.install` | `Launchd::install(serial_port)` | |
| `_service_launchd.uninstall` | `Launchd::uninstall()` | |
| `_service_launchd.install_widget` | `Launchd::install_widget()` | |
| `_service_launchd.uninstall_widget` | `Launchd::uninstall_widget()` | |
| `_service_launchd.is_installed` | `Launchd::is_installed()` | |
| `_service_launchd.is_loaded` | `Launchd::is_loaded()` | The label anywhere in a `launchctl list` line, as Python |
| `_service_launchd.unit_path` | `Launchd::unit_path()` | |
| `_service_launchd.log_path` | `Launchd::log_path()` | |
| `_service_launchd.agent_path` | `Launchd::agent_path(label)` | HOME read now |
| `_service_launchd.lite_plist_path` | `Launchd::lite_plist_path()` | |
| `_service_launchd.lite_log_path` | `Launchd::lite_log_path()` | |
| `_service_launchd._build_lite_plist` | `Launchd::build_lite_plist()` | Byte for byte (cross-check against the Python module) |
| `_service_launchd._write_plist` | `service_launchd::write_plist(path, data)` | Temp file `.<name>.*.tmp` in the folder, 0644, one rename; temp removed on failure |
| `_service_launchd.install_lite` | `Launchd::install_lite()` | Refusal beside full eve's agent; OSError → "could not be written to <folder> (<ExceptionName>)." and 2 |
| `_service_launchd.uninstall_lite` | `Launchd::uninstall_lite()` | `bootout gui/<uid>/<label>`, `unload` by file when that fails |
| `_service_launchd.lite_installed` | `Launchd::lite_installed()` | |
| `_service_launchd.lite_loaded` | `Launchd::lite_loaded()` | Last tab column matched whole |
| `_service_launchd.installed_lite_plist` | `Launchd::installed_lite_plist() -> Option<plist::Dictionary>` | None when missing, unreadable, not a plist, or not a dict |
| `_service_systemd.NAME`, `UNIT_NAME`, `UNIT_PATH` | `NAME`, `UNIT_NAME`, `Systemd::unit_path` (`unit_path_under(home)`) | |
| `_service_systemd._build_unit` | `Systemd::build_unit(serial_port)` | Byte for byte |
| `_service_systemd._systemctl` | `Systemd::systemctl(args)` | |
| `_service_systemd.install` | `Systemd::install(serial_port)` | |
| `_service_systemd.uninstall` | `Systemd::uninstall()` | |
| `_service_systemd.is_installed` | `Systemd::is_installed()` | |
| `_service_systemd.is_loaded` | `Systemd::is_loaded()` | `is-active --quiet` exit 0 |
| `_service_systemd.unit_path` | `Systemd::unit_path()` | |
| `_service_systemd.log_path` | `Systemd::log_path()` | `journalctl --user -u eve-bridge.service` |
| `_service_windows.NAME`, `TASK_NAME`, `LOG_PATH` | `NAME`, `TASK_NAME`, `TaskScheduler::log_path` (`log_path_under(home)`) | |
| `_service_windows.launcher_path` | `TaskScheduler::launcher_path()` | |
| `_service_windows._batch` | `service_windows::batch(value)` | `%` → `%%` |
| `_service_windows.install` | `TaskScheduler::install(serial_port)`, script text from `TaskScheduler::launcher_script` | |
| `_service_windows.uninstall` | `TaskScheduler::uninstall()` | |
| `_service_windows.is_installed` | `TaskScheduler::is_installed()` | `schtasks /query` |
| `_service_windows.is_loaded` | `TaskScheduler::is_loaded()` | |
| `_service_windows.unit_path` | `TaskScheduler::unit_path()` | `Task Scheduler: eve-bridge-daemon` |
| `_service_windows.log_path` | `TaskScheduler::log_path()` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `test_service.py` | 7 (one parametrized ×2) | `src/tests.rs`: `test_plist_*` (5), `test_backend_name_resolves_per_platform`, `test_is_installed_false_on_unsupported_platform` (both platforms in one test) | None |
| `test_service_lite_plist.py` | 20 (one parametrized ×4) | `src/tests.rs`: every test by the same name; the parametrized `test_lite_loaded_matches_the_label_whole` loops its four cases; the golden tests read `bridge/tests/golden/owner_main.json` | None |
| `test_service_systemd.py` | 7 | `src/tests.rs`: every test by the same name, with the same `_parse_unit` helper | None |
| `test_service_windows.py` | 4 | `src/tests.rs`: every test by the same name, with the `schtasks` fixture as a helper | None |
| `test_notes_widget.py` (tests_ref) | `test_widget_plist_shape`, `test_widget_plist_is_separate_from_daemon_plist` | same names | The rest tests notes_widget.py, not this crate |
| `test_owner_golden.py` (tests_ref) | plist parts | `test_full_eves_plists_are_byte_for_byte_the_golden`, `test_a_full_plist_changed_in_code_does_not_match_the_golden` | The rest is other modules' golden |
| `test_cli_lite.py`, `test_lite_daemon.py` (tests_ref) | they monkeypatch `install_lite`/`lite_loaded`, or read `LABEL` | — | They test cli.py and lite.py; the plist they build is covered above |
| — | — | Rust only: full install/uninstall/widget with a recorder, `is_loaded`, systemd install/uninstall/failure, Windows failure paths, dispatcher paths and messages, the plistlib writer (shapes, control-character refusal, CPython comparison), `every_unit_matches_the_python_modules_byte_for_byte` (runs the Python modules when python3 can import them) | |

## Boundaries
None. launchctl, systemctl and schtasks are run as child processes exactly as the Python runs them.

## Differences
- **The program a unit starts.** Python's units name `sys.executable` with `-m eve_bridge.cli <subcommand>`. Each backend here carries a `Launch`: `Launch::Python(python)` gives Python's line byte for byte (the golden and cross-check tests use it); `Launch::Native(exe)` gives `<exe> <subcommand>`. `from_process()` uses `Launch::current()`, which is `Native(current_exe)` (empty when unknown, as `sys.executable` can be), so a Rust `eve-bridge install --service` installs a unit that starts the Rust binary. The lite plist keeps `PYTHONNOUSERSITE=1` either way, so its bytes match Python's.
- **Process state is passed in.** `HOME`, `CLAUDE_CONFIG_DIR`, `EVE_CLAUDE_CONFIG_DIRS` (a `HomeEnv`), the import-time paths, the label, `which`/`run`/`print`/`getuid` (the `Host` trait) are fields of the backend structs. `from_process()` reads them as Python does: full eve's paths, `UNIT_PATH` and Windows' `LOG_PATH` once per process (Python's import), HOME and the Claude homes at each call.
- **Exceptions are `Err`.** Where Python lets an exception escape (`Path.home()` unresolvable, a control character in a plist string — "strings can't contains control characters; use bytes instead" —, a path that is not UTF-8, an I/O error outside `install_lite`'s `except OSError`, launchctl missing in `uninstall`), the Rust function returns `Err` for the caller to report, instead of a traceback. `is_installed`/`is_loaded`/`lite_loaded` return `io::Result<bool>` because the Python versions can raise from `subprocess.run`.
- **`sys.platform`** is derived from the build target: on FreeBSD the unsupported-platform message says `'freebsd'` where Python says `'freebsd14'`.
- **Captured output** is decoded with `from_utf8_lossy`; Python's `text=True` would raise on undecodable bytes.
- **Windows launcher line endings.** The script is written as `\r\n` lines on every OS. Python's `write_text` on Windows would translate each `\n` again (`\r\r\n`); on macOS and Linux the bytes are the same.
- **`installed_lite_plist`** parses with the `plist` crate (XML or binary, as `plistlib.load`); a malformed file is None in both.
