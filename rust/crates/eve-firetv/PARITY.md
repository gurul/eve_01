# eve-firetv
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/firetv.py` | `src/lib.rs` (tests in `src/tests.rs`); `src/pyfmt.rs` holds the Python builtins the port depends on (truthiness, `str`, `int`, `round`, `repr`, `splitlines`, `json.dump(indent=2)`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `CONFIG_PATH`, `SHOTS_DIR`, `OFF_REASON`, `NOT_SET_UP_LINE`, `INSTALL_LINE`, `ALLOW_LINE`, `UNREACHABLE_LINE`, `TIMEOUT_LINE`, `DEEP_STANDBY_LINE`, `DEFAULT_PORT`, `ADB_PATHS`, `CONNECT_SECS`, `SHELL_SECS`, `INPUT_SECS`, `MDNS_SECS`, `PROBE_SECS`, `ADB_RESTART_EVERY`, `SCREENCAP_SECS`, `POWER_POLL_SECS`, `POLL_STEP_SECS`, `VOLUME_STEP`, `MAX_REPEAT`, `MAX_TEXT`, `CLEAN_CAP`, `RC_TIMEOUT`, `RC_NO_ADB`, `PNG_SIGNATURE` | same names | values verbatim (the line constants were diffed byte for byte against the Python module) |
| `Runner`, `Capture`, `Probe` (type aliases) | `trait Runner`, `trait Capture`, `trait Probe` | each is implemented for any matching `Fn(..) -> Future` closure, so tests and callers pass closures as Python passes coroutines |
| `tcp_probe` | `tcp_probe()`, `TcpProbe` | tokio `TcpStream::connect` under a timeout; any error is false |
| `find_adb` | `find_adb(environ)` | `EVE_ADB`, then `which adb` on the PATH, then Homebrew's two paths checked with `access(X_OK)` |
| `_spawn` | `spawn()` (private) | tokio process, stdin null; with `merge` stdout and stderr share one OS pipe (Python's `stderr=STDOUT`), so lines keep the order they were written; spawn failure is `RC_NO_ADB` with `adb could not start (<strerror>)`; a timeout kills, waits up to 2 s and answers `RC_TIMEOUT` |
| `_kill` | `kill()` + `Reaper` (private) | a dropped (cancelled) call kills its child and reaps it before the drop returns, like `_kill` on `CancelledError` |
| `adb_runner` | `adb_runner()`, `AdbRunner` | merged output decoded as UTF-8 with replacement |
| `adb_capture` | `adb_capture()`, `AdbCapture` | stdout bytes on rc 0, else stderr + stdout |
| `_sh` | `sh()` (private), `shlex_quote()` | `shlex.quote` rules: `''` for empty, bare when only `[\w@%+=:,./-]` (ASCII), else single-quoted with `'"'"'` |
| `clean` | `clean(text, cap)` | same control-character class (incl. U+200B-U+200F, U+2028-U+202E, U+2066-U+2069), whitespace collapse, cap in code points with `…` |
| `load` | `load(path)` | returns the file's object with `host` stripped, `port` as `int(port or 5555)` (bad values fall back to 5555), `mdns` as `str(mdns or "")`; other keys kept in their order; empty map for no file / not JSON / no host |
| `save` | `save(cfg, path)` | `<name>.tmp` opened `O_CREAT` mode 600, chmod 600, rename; content is `json.dump(cfg, indent=2)` byte for byte (ASCII escapes, `", "`/`": "` layout, no trailing newline) |
| `App`, `App.package` | `App`, `App::package()` | `&'static` fields |
| `APPS`, `YOUTUBE`, `LAUNCHER_PACKAGE`, `PROJECTIVY_PACKAGE`, `SCREENSAVER_PACKAGE`, `PASSTHROUGH_ACTIVITY` | same names | the table verbatim, same order |
| `_app_key` | `app_key()` (private) | |
| `_BY_ALIAS` | `BY_ALIAS` (private) | first name wins (`setdefault`) |
| `find_app` | `find_app()` | |
| `app_name` | `app_name(package, activity)` | |
| `STATE_SCRIPT` | `STATE_SCRIPT` | byte-identical |
| `TvState`, `TvState.volume`, `TvState.public` | `TvState`, `TvState::volume()`, `TvState::public()` | `volume` uses round-half-to-even like Python's `round` |
| `_section` | `section()` (private) | hand-written equivalent of `@@NAME\s*\n(.*?)(?=\n?@@[A-Z]+|\Z)` (the regex crate has no lookahead), checked against the Python on odd inputs |
| `power_word` | `power_word()` | |
| `parse_state` | `parse_state()` | `\d` matches any script's decimal digits and is read like `int()` |
| `said_state` | `said_state()` | |
| `NAV_KEYS`, `KEY_ALIASES`, `MEDIA_KEYS`, `KEYCODE_WAKEUP`, `KEYCODE_SLEEP`, `KEYCODE_VOLUME_MUTE` | same names (ordered arrays) + `nav_key()`, `media_key()`, `key_alias()` lookups | order kept (the CLI's key list prints in it) |
| `ACTIONS`, `PANEL_ACTIONS`, `TOOLS`, `TOOL_NAMES` | same names | `TOOLS` diffed against the Python's JSON |
| `_SHOT_TOOL`, `_SHOT_CHECK`, `_INSTRUCTIONS`, `INSTRUCTIONS`, `INSTRUCTIONS_NO_SCREENSHOT` | `SHOT_TOOL`, `SHOT_CHECK`, `INSTRUCTIONS_TEMPLATE` (private), `INSTRUCTIONS`, `INSTRUCTIONS_NO_SCREENSHOT` | byte-identical to the formatted Python strings |
| `Command` | `Command` (+ `Command::new`, `with_*` builders) | `repeat` defaults to 1 |
| `_LEAD`, `_TAIL`, `_TV`, `_THIS_TV`, `_MUSIC`, `_APP`, `_BIT`, `_COUNT_WORDS`, `_KEY`, `_TIMES`, `_UP`, `_DOWN`, `_PATTERNS` | `LEAD`, `TAIL`, `TV`, `THIS_TV`, `MUSIC`, `APP_RE`, `BIT`, `count_word()`, `KEY`, `TIMES`, `UP`, `DOWN`, `PATTERNS` (private) | patterns verbatim, compiled with `fancy-regex` (backtracking, lookahead) and anchored `^…$` for `re.fullmatch` |
| `_count` | `count()` (private) | |
| `_key` | `key_cmd()` (private) | |
| `_step` | `step()` (private) | |
| `_norm` | `norm()` (private) | |
| `match` | `match_command()` | renamed: `match` is a Rust keyword. Checked against the Python on 2209 phrases (every test phrase in 10 TV wordings and 3 leads, plus edge cases): identical |
| `Reach` | `Reach` | `why` and `line` are `&'static str` (always one of the fixed values) |
| `_GONE`, `_gone` | `GONE_RE` (private), `gone()` | |
| `_lost` | `lost()` | |
| `_am_failed` | `am_failed()` | |
| `_resolved_component` | `resolved_component()` | |
| `_PACKAGE` | `PACKAGE_RE` (private) | |
| `type_text_arg` | `type_text_arg()` | |
| `urllib.parse.quote_plus` (used by `youtube_search`) | `quote_plus()` | |
| `FireTV` | `FireTV` | shared by `&self` (interior state: `parking_lot::Mutex` config, `AtomicBool` ready, `tokio::sync::Mutex` connection lock), so one hub serves Telegram, the voice and the Mini App |
| `FireTV.__init__` | `FireTV::new(cfg)` + `with_runner`, `with_capture`, `with_probe`, `with_adb`, `with_path`, `with_shots_dir`, `with_sleep`, `with_clock` | defaults: real adb runner/capture, TCP probe, `adb`, no path, tokio sleep, monotonic clock |
| `FireTV.serial` | `FireTV::serial()` | also `FireTV::cfg()` for the current config |
| `FireTV._adb` | `FireTV::adb()` (private) | |
| `FireTV._connect` | `FireTV::connect()` (private) | same words read, same outcomes |
| `FireTV.discover` | `FireTV::discover()` | |
| `FireTV._rediscover` | `FireTV::rediscover()` (private) | saves to `path` when the address moved; a failed save is a warning |
| `FireTV._recover` | `FireTV::recover()` (private) | |
| `FireTV._restart_server` | `FireTV::restart_server()` (private) | at most once per `ADB_RESTART_EVERY`, only when the probe opens |
| `FireTV.ensure` | `FireTV::ensure()` | |
| `FireTV.shell` | `FireTV::shell(script, timeout, again)` | |
| `FireTV.state` | `FireTV::state()` | |
| `FireTV.status` | `FireTV::status()` | same keys and order |
| `FireTV.control` | `FireTV::control(action, ControlArgs)` | `ControlArgs` carries Python's keyword arguments |
| `FireTV._log` | `FireTV::log()` (private) | `firetv: <action> -> done|failed in N ms` via `tracing` |
| `FireTV._fail` | `FireTV::fail()`, `FireTV::fail_with()` (private) | |
| `FireTV._send` | `FireTV::send()` (private) | |
| `FireTV._control` | `FireTV::control_inner()` (private) | |
| `FireTV._power` | `FireTV::power()` (private) | polls through the `sleep` seam |
| `FireTV.press` | `FireTV::press()` | |
| `FireTV._volume` | `FireTV::volume()` (private) | |
| `FireTV._mute` | `FireTV::mute()` (private) | |
| `FireTV._before_opening` | `FireTV::before_opening()` (private) | |
| `FireTV._interrupted` | `FireTV::interrupted()` (private) | |
| `FireTV._start` | `FireTV::start()` (private) | |
| `FireTV._resolve` | `FireTV::resolve()` (private) | |
| `FireTV.packages` | `FireTV::packages()` | sorted, deduplicated |
| `FireTV.open_app` | `FireTV::open_app()` | |
| `FireTV.youtube_search` | `FireTV::youtube_search()` | |
| `FireTV.type_text` | `FireTV::type_text()` | |
| `FireTV.screenshot` | `FireTV::screenshot(out_dir, out_path) -> io::Result<Out>` | `Err` only where Python raises (the PNG could not be written) |
| `FireTV.tools` | `FireTV::tools()` | a copy |
| `FireTV.instructions` | `FireTV::instructions(screenshot)` | |
| `FireTV.screenshot_for_model` | `FireTV::screenshot_for_model() -> io::Result<Out>` | |
| `FireTV.handle` | `FireTV::handle(name, &Value) -> io::Result<Out>` | `num()` keeps ints and floats (truncated), drops bools and strings, as Python's |
| `FireTV.panel` | `FireTV::panel(&Value)` | same refusals before any adb call, same fresh-state answer |
| `FireTV.match` | `FireTV::match_command()` | |
| `FireTV.run` | `FireTV::run(&Command)` | |
| `FireTV.close` | `FireTV::close()` | nothing to close |
| `parse_mdns` | `parse_mdns()` | |
| `enabled` | `enabled(environ)` | `EVE_FIRETV` in 0/false/no/off (any case, stripped) is off |
| `make_firetv` | `make_firetv(path, environ, runner, capture)` | |
| `pair` | `pair(host, port, path, out, runner, environ)` | same lines; `out` is a `FnMut(&str)` |
| `cli` | `cli(argv, out, CliOptions)`, `cli_async()` | clap with the argparse subcommands, options and help strings; `cli` runs its own current-thread runtime like `asyncio.run` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_firetv.py` | all 84: `test_connect_then_get_state_once_then_reuses_it` … `test_the_real_probe_tells_an_open_port_from_a_closed_one` (the connection, state, power, keys, volume and mute incl. the three `LossyMuteTv` cases, apps, search, typing, screenshot, `match`, tools, construction, CLI, guards, panel, stale adb server) | the same 84 names in `src/tests.rs`; parametrized tests loop over the same cases. The autouse silent-probe fixture is the `silent_probe()` every fake TV is built with; `monkeypatch` of `fake.shell` is a flag on the fake (`blank_state`, `ignore_set`, `lossy`). Extra: `test_shlex_quote_like_python`, `test_quote_plus_like_python`, `test_section_reads_like_the_python_regex`, `test_load_keeps_other_keys_and_coerces_the_port`, `test_save_writes_what_python_writes`, `test_find_app_by_key_and_package_names`, `test_instructions_text_is_the_pythons`, `test_capture_returns_error_words_on_failure`, and in `pyfmt.rs` `test_float_repr_like_python`, `test_json_dumps_indent_like_python`, `test_int_and_repr_and_splitlines` | none |
| `bridge/tests/test_voice_agent.py` (tests_ref) | uses `firetv.TOOLS`, `INSTRUCTIONS`, `INSTRUCTIONS_NO_SCREENSHOT` | exported as statics; `test_instructions_name_the_limits` checks the no-screenshot text names no screenshot | the session wiring belongs to the voice crate |
| `bridge/tests/test_telegram.py` (tests_ref) | uses `firetv.match`, `Command`, `TOOLS`, `INSTRUCTIONS` | exported (`match_command`, `Command` with `PartialEq`) | the chat routing belongs to the telegram crate |
| `bridge/tests/test_app_registry.py` (tests_ref) | tool names, `EVE_FIRETV`, `NOT_SET_UP_LINE`, "turn on the tv" by code | `TOOL_NAMES`, `enabled`, `NOT_SET_UP_LINE`, `match_command("turn on the tv")` covered by `test_match` | the registry belongs to the app-registry crate |

## Boundaries
- adb itself: every TV command runs Google's `adb` as a subprocess with an argv list, exactly as the Python does (no ADB library on either side).

## Differences
- `control` has no `except OSError` branch: the Rust `Runner` returns `(rc, output)` and cannot fail, and the real runner already turns a spawn failure into `RC_NO_ADB` (`INSTALL_LINE`), which is what the Python reaches in practice.
- Where the Python raises out of the module, Rust returns a value: `screenshot`/`screenshot_for_model`/`handle` return `io::Error` when the PNG cannot be written; the CLI prints `Could not save the picture (...)`, `Could not save the TV to ... (...)` (pair) or `Could not forget the TV (...)` and exits 1 instead of a traceback.
- CLI argument errors are clap's messages (exit 2, help exit 0) rather than argparse's wording; the subcommands, options, defaults and help strings are the same.
- Integers are `i64`: a tool or panel number past ±9.2e18 saturates (Python's ints grow), which every clamp (0-100, ±100, 1-10) turns into the same answer.
