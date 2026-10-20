# eve-bridge

Status: complete

The `eve-bridge` command line: `bridge/src/eve_bridge/cli.py` and `__main__.py`. The binary `eve-bridge` has the
same argparse tree as the Python in both profiles. `python3 tools/check_cli.py` compares every `--help` path:
286 of 286 match in the full profile and 18 of 18 in the lite profile. A scratch diff of all 304 help pages
(stdout, stderr and exit code) against the Python was byte-identical at COLUMNS=50, 80 and 130, and so were
`app` and `vault` help and usage errors at 50, 80, 100 and 130 columns. A differential fuzz of 1,600 random
command lines (subcommands, options, `--`, abbreviations, negative numbers, help) over the commands that need no
daemon matched the Python in output and exit code, except `places forget` at end of input (see Differences).

## Modules

| Python | Rust |
|---|---|
| `eve_bridge.__main__` | `src/main.rs` (the bin target) |
| `eve_bridge.cli`: `main`, the full-profile parser and its commands | `src/cli.rs` |
| `eve_bridge.cli`: `memory tail/recall/forget/dream/reindex` | `src/cli/memory.rs` |
| `eve_bridge.cli`: eve lite's parser and commands (`_lite_*`, `web *`, `doctor`, `lite uninstall`) | `src/cli/lite.rs` |
| `argparse` (stdlib), as cli.py uses it | `src/argparse.rs`: Python 3.13's parsing (the `O`/`A`/`-` string pattern, `_match_arguments_partial`'s regex matching of positionals, the first `--` taken out as 3.13 does, `nargs=N` options taking option-like strings), usage wrapping, HelpFormatter layout and error text, exit 2 |
| argparse help for the app CLIs (lights, spotify, firetv, obsbot/cam) | `src/apphelp.rs`: converts the app's clap `Command` to an argparse parser for `--help` only |
| `app_template.cli`'s and `vault.cli`'s argparse parsers | `apphelp::app_template_parser`, `apphelp::vault_parser`: `main` parses `app ...` and `vault ...` with them first (help and usage errors at the terminal's width), then hands a command line that parses to eve-app-template or eve-vault |
| `print`/`input`/`sys.stdin.isatty()`/`client.post`, as tests patch them | `src/io.rs`: the `Io` seam (out, err, post, ask, stdin_tty) passed to each command |
| `json.dumps`, `str()`, truthiness and `time.strftime`, as cli.py prints values | `src/py.rs` |

## API

Every top-level def, class and constant in cli.py and `__main__.py`.

| Python | Rust | Notes |
|---|---|---|
| `__main__` (`raise SystemExit(main())`) | `main.rs`: `std::process::exit(cli::main(None))` | |
| `main` | `cli::main(argv)` | `mcp` and `vault-mcp` run before the env file is read. Then `load_env_file`, `eve_profile::apply` (EX_CONFIG 78 on an unknown profile), and lite or full dispatch. App words go to the app's own `cli`, with `--help` rendered by `apphelp`. `full_parser(env)` and `full_main(argv, io, env)` are the parser and dispatch, split out so tests can pass an env |
| `_run_daemon` | `run_daemon`, `daemon_options` | `daemon_options` builds `eve_daemon::DaemonOptions` from the parsed args |
| `_run_push_character` | `run_push_character` | |
| `_render_diag` | `render_diag(d, connected, io)` | |
| `SPECIES` | `SPECIES` | |
| `_run_photos` | `run_photos` | |
| `_run_notes` | `run_notes` | |
| `_run_move` | `run_move` | |
| `describe_mic` | `describe_mic(resp)` | |
| `_run_mic` | `run_mic` | |
| `_run_sound` | `run_sound` | |
| `_run_explore` | `run_explore` | |
| `NOT_REACHABLE` | `memory::NOT_REACHABLE` | |
| `_run_memory` | `memory::run_memory` (`tail_topics`, `tail_line`, `recall_answer`) | tokio-tungstenite rosbridge client. Ctrl-C returns 0 |
| `_open_memory_store` | `memory::open_memory_store(environ, io)` | Returns `Store { cfg, memory, index }` |
| `_run_memory_store` | `memory::run_memory_store(args, io, seams)` | `Seams` stands in for what the Python tests monkeypatch: the store, the dream client and the dreamer |
| `_run_lesson` | `run_lesson(args, io)` | |
| `_run_species` | `run_species` | |
| `_run_diag` | `run_diag` | |
| `_run_chrome_profile` | `run_chrome_profile` | |
| `_run_steel_profile` | `run_steel_profile` | |
| `_own_chrome_signed_in` | `own_chrome_signed_in` | |
| `_run_unpair` | `run_unpair` | |
| `_run_link` | `run_link`, `run_link_with(io, token_path, latest, which)` | |
| `_run_notify` | `run_notify`, `run_notify_with(cmd, io, cfg, token, post)` | |
| `_run_refresh` | `run_refresh` | |
| `_run_apps` | `run_apps` | |
| `_places` | `run_places` | |
| `EX_CONFIG` | `lite::EX_CONFIG` | |
| `LITE_INSTALL_LINE` | `lite::LITE_INSTALL_LINE` | |
| `LITE_KEPT` | `lite::LITE_KEPT` | |
| `WEB_OPEN_WAIT_SECS` | `lite::WEB_OPEN_WAIT_SECS` | |
| `IPC_POLL_SECS` | `lite::IPC_POLL_SECS` | |
| `OPEN` | `lite::OPEN` | |
| `OSADECOMPILE` | `lite::OSADECOMPILE` | |
| `LAUNCH_SERVICES` | `lite::LAUNCH_SERVICES` | |
| `_CS_DARWIN_USER_TEMP_DIR` | `lite::CS_DARWIN_USER_TEMP_DIR` | |
| `_BUNDLE_ID` | `lite::BUNDLE_ID` | |
| `_OPAQUE` | `lite::OPAQUE` | |
| `LITE_NOT_ANSWERING`, `LITE_REFUSED`, `LITE_NO_HOOKS`, `LITE_NO_HOOKS_GONE`, `LITE_UPDATE`, `LITE_MAC_ONLY`, `LITE_NOT_LOADED`, `LITE_NO_CODE`, `LITE_NO_BROWSER`, `LITE_LOGIN_UNUSED` | the same names in `lite` | Text copied verbatim. `LITE_UPDATE` is a `LazyLock<String>` |
| `_seconds` | `lite::seconds` | The `--wait` conversion |
| `_lite_parser` | `lite::lite_parser` | |
| `_lite_main` | `lite::lite_main(argv, applied, ctx, io)` | `Ctx` carries what the Python tests patch: env, HOME, launchd, platform, subprocess runner, browser opener, poll intervals and daemon entry |
| `_say_profile` | `lite::say_profile`, `profile_line`, `removed_secrets` | |
| `_lite_socket` | `Ctx::lite_socket` | |
| `_lite_ask` | `lite::lite_ask` | |
| `_lite_answered` | `lite::lite_answered` | |
| `_shown` | `lite::shown`, `shown_str` | |
| `_plain_line` | `lite::plain_line` | |
| `_is_port` | `lite::is_port` | |
| `_tilde` | `Ctx::tilde` | |
| `_run_web_open` | `lite::run_web_open` | |
| `_login_used` | `lite::login_used` | |
| `_login_page` | `lite::login_page` | |
| `_user_tmp` | `lite::user_tmp` | |
| `_stop_on_signals` | `lite::StopOnSignals` | A guard. SIGTERM, SIGHUP and SIGINT unlink the page (async-signal-safe), then `_exit(128+sig)` |
| `_default_browser` | `lite::default_browser` | |
| `_open_in_browser` | `lite::open_in_browser` | |
| `_when` | `lite::when` | |
| `_run_web_devices` | `lite::run_web_devices` | |
| `_run_web_revoke` | `lite::run_web_revoke` | |
| `_run_web_lan` | `lite::run_web_lan` | |
| `_is_file` | `lite::is_file` | |
| `_lite_status` | `lite::lite_status` | |
| `_env_file_profile` | `lite::env_file_profile` | |
| `_sets_lite` | `lite::sets_lite` | |
| `_loose` | `lite::loose` | |
| `_lite_doctor` | `lite::lite_doctor` | |
| `_kept_by_lite` | `lite::kept_by_lite` | |
| `_mark_kept` | `lite::mark_kept` | |
| `_app_is_ours` | `lite::app_is_ours` | |
| `_has_content` | `lite::has_content` | |
| `_remove` | `lite::remove` | |
| `_yes` | `lite::yes` | Says no when stdin is not a terminal |
| `_lite_uninstall` | `lite::lite_uninstall` | |

## Tests

Run: 91 unit tests (`src/cli/tests.rs`, `argparse`, `py`) and 5 process tests (`tests/cli_process.rs`, which use the
built binary). No daemon, launchctl, osadecompile or open runs in any test. Stand-ins answer on real Unix sockets in
temporary folders. HOME is always temporary. install-lite.sh runs only as `--dry-run` (as the Python tests run it),
on a Mac.

Ported:
- `test_cli_lite.py`: every test except the one listed below. The test names are the Python names. The profile
  case `test_a_profile_that_is_neither_full_nor_lite_exits_78_before_any_command` became the process tests
  `test_an_unknown_profile_stops_with_ex_config` and `…_in_the_env_file_…`. The installer halves (install-lite.sh
  `--dry-run` and `--uninstall --dry-run` in the test's HOME) are run as in the Python, on a Mac.
  - `test_web_open_deletes_the_page_on_ctrl_c` and `test_sigterm_and_sighup_end_web_open_through_its_cleanup` run
    in a forked child (a signal ends the process it is sent to, and Rust tests share one): Ctrl-C while `web open`
    waits for the browser, and SIGTERM and SIGHUP under `StopOnSignals`, each delete the page and exit 128+signal;
    the handlers come back when the guard goes.
  - `test_web_open_through_lite_daemons_own_ipc` answers with eve-web-auth's real `MacLogin` (the code issued is the
    one the page carries, the browser consumes it once, `web_login_used` then says so). The whole `LiteDaemon` is not
    started: its page server is private to eve-lite, whose own tests run that IPC end to end.
- `test_cli_memory.py`: all six tests, plus the bus helpers (`test_memory_bus_lines`).
- tests_ref:
  - test_diag_render: all five.
  - test_mic: `describe_mic`.
  - test_daemon_lesson: the lesson CLI's ten tests.
  - test_link_pair_cli: both tests.
  - test_notify: the CLI's `topic`/`test` and the unpaired case.
  - test_chat_memory: `_open_memory_store` must be set up first, and the chat log is on, or off with `EVE_CHAT_LOG=0`.
  - test_suite_isolation: the CLI's part is that `mcp` runs before the env file. It is in `tests/cli_process.rs`.
  - test_learning_search: the `species`/`move` request shapes it relies on.
- Added: `test_app_help_is_argparse_shaped` checks the clap-to-argparse help bridge, and `test_version_and_help_exit_zero`.

Not ported, with the reason:
- `test_importing_the_cli_loads_no_part_of_full_eves_daemon`: a Python import-laziness check. A Rust binary has no
  lazy imports, so there is nothing to observe.

## Boundaries

- launchctl (`eve_service::Host`): install, uninstall, status, `web open` and the doctor go through `Launchd`. Tests
  use a fake `Host`.
- `/usr/bin/open` and `osadecompile` (`Ctx::run`): opening the login page, and reading Eve.app's script in
  `lite uninstall`.
- The daemon's IPC socket (`eve_hooks::client::post`) and the rosbridge websocket (`memory tail`/`recall`).
- The BLE board, the Voice PE and the robot: reached only through the daemon. The CLI never opens them itself.
- Browsers: Chrome's profile and Steel (`chrome-profile`, `steel-profile`) through `eve_steel`/`eve_chrome`.
- Process signals: SIGINT (the interrupt flag; KeyboardInterrupt returns 0), and SIGTERM/SIGHUP/SIGINT during
  `web open`.
- The env file (`eve_env_file::load_env_file`) and the profile (`eve_profile::apply`).

## Differences

- An uncaught exception that ended the Python with a traceback (for example `head-cal` with a port out of range)
  prints `eve-bridge: <error>` here and exits 1.
- `move --keys` with JSON that does not parse: the message after `--keys:` is serde_json's text, not Python's
  `json.JSONDecodeError` text. The exit code is the same.
- The app CLIs (lights, spotify, firetv, obsbot/cam) parse with clap. `--help` at any level is converted to
  argparse's layout and matches the Python. A usage error (a bad choice, a missing argument) is still clap's
  wording, with exit 2 as before.
- `doctor` prints `eve-bridge <version>: <path of this binary>` where the Python printed its interpreter's version
  and path. There is no Python in the Rust install.
- Ctrl-C during `web open`: the Python's KeyboardInterrupt deleted the page through its `finally` and ended with a
  traceback, the process killed by SIGINT. Here `StopOnSignals` deletes the page and exits 130, with no traceback.
- `steel-profile` drives Steel through eve-steel's built-in CDP client where the Python used Playwright.
- `places forget`: end of input at its yes/no prompt prints `eve-bridge: EOFError: EOF when reading a line` and exits
  1, where the Python ended with EOFError's traceback (exit 1).
- `memory tail/recall` name the error class in "not reachable (…)" by its Python equivalent
  (`ConnectionRefusedError`, `TimeoutError`, `OSError`). A websocket failure that has no Python counterpart is
  named `OSError`.
- An env file that cannot be read prints `eve-bridge: <error>` and exits 1, which is the Python's own message
  path. The error text is the Rust io error text.
- A chat log that cannot be opened in `_open_memory_store` is always reported as `(OverflowError)`. The Python
  named the class of whatever it raised. eve-chat-log's `open_configured` returns one error kind, so there is no
  class to name.

## Upstream edits

Each is small, additive and recorded in that crate's PARITY.md. Each crate's tests were re-run and pass.
- `eve-mem0-memory`: `OwnerMemory::is_open()`.
- `eve-lights`: `cli::clap_command()`.
- `eve-obsbot`: `clap_command()`.
- `eve-spotify`: `clap_command()`.
- `eve-firetv`: `clap_command()`.
