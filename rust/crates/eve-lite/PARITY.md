# eve-lite
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/lite.py` | `src/lib.rs` (the module docstring is the crate doc, with the IPC contract verbatim); `setup_logging` and its format in `src/logging.rs`; the Composio bridge as the daemon and the brain hold it in `src/apps.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `EXIT_OK`, `EXIT_IN_USE`, `EXIT_RESTART`, `EXIT_CONFIG` | same-named `const`s | 0, 2, 75, 78 |
| `READY`, `NEEDS_OPENROUTER` | same-named `const`s | lite_web's |
| `FULL_LABEL`, `FULL_PLIST`, `SOCK_ENV`, `DEVICES_FILE`, `TLS_DIR`, `SOCKET_MAX_BYTES` | same-named `const`s | paths as `~` strings, expanded where read (`expanduser`) |
| `CLOSE_MARKERS_SECS`, `DRAIN_SECS`, `PAGE_RETRY_SECS`, `PAGE_STOP_SECS` | same-named `const`s | `PAGE_RETRY_SECS` is also `Seams::page_retry_secs` (the tests patch it) |
| `QUIET_LOGGERS`, `LOG_FORMAT` | same-named `const`s (`src/logging.rs`) | plus `QUIET_TARGETS`: the Rust crates whose INFO lines name URLs and addresses (reqwest, hyper, the WebRTC stack), held at WARNING the same way |
| `PAGE_DOWN`, `STOPPING`, `UNKNOWN_EVENT` | same-named `const`s | verbatim |
| `log` | `tracing` | the same lines at the same levels, under the logger name `eve_bridge.lite` (`logger_name`); states, counts, ports and exception type names only |
| `_no_screen` | `pub fn no_screen` | None |
| `_nothing` | `pub fn nothing` | an empty list |
| `_no_work` | `pub fn no_work` | None |
| `_no_owner` | `pub fn no_owner` | None for any initData |
| `_refuse_launch` | `pub async fn refuse_launch` | `telegram.LITE_NOT_HERE` |
| `build_memory` | `pub fn build_memory`, `build_memory_with(cfg, environ, openers)`, `struct MemoryOpeners`, `struct Mem0Index`, `mem0_of` | full eve's `build_memory_with` (eve-daemon `run.rs`) copied line for line; `tests/memory_parity.rs` holds the copy to the original |
| `start_dream` | `pub fn start_dream`, `start_dream_with(.., make_client)`, `dream_loop`, `struct Background`, `type MakeDreamClient`, `records_client` | the job is named `memory-dream` and held in `Background` until it ends (`add_done_callback(background.discard)`); `dream_loop` is eve-daemon's, copied |
| `make_bridge` | `pub fn make_bridge`, `make_bridge_with(.., make)`, `type MakeBridge`, `real_bridge` | None until composio-connect.json is a file; the bridge is `ConnectApps` (`src/apps.rs`) |
| `full_eve_installed` | `pub fn full_eve_installed` | the plist exists |
| `ipc_path` | `pub fn ipc_path`, `ipc_path_from(spec, env, default)` | `str(spec or env or "").strip()` with Python's whitespace; a TCP spec is the default, with one warning |
| `setup_logging` | `pub fn setup_logging`, `level_of`, `log_filter`, `logger_name`, `PyFormat` | `%(asctime)s %(levelname)s %(name)s: %(message)s`; a bad level is INFO; a subscriber already installed is kept (`try_init`, as `basicConfig` does nothing a second time) |
| `_private_folder` | `fn private_folder` (private) | made 0700 when missing, parents as the umask makes them; one that is there is left alone |
| `LiteDaemon` | `pub struct LiteDaemon`, `struct LiteOptions`, `struct Seams` | `new(options)`; `run() -> Result<i32>`, `shutdown`, `restart` (from any thread: a watch channel), `stopping`, `state`, `exit_code`; the parts behind getters (`memory`, `chat_log`, `watcher`, `apps`, `inlet`, `link`, `calls`, `rtc`, `web`, `local`); `status`, `web_status`, `handle_ipc`; the private steps `_build`, `_serve`, `_start_ipc`, `_keep_page_up`, `_ready`, `_spawn`, `_ended`, `_stop`, `_step`, `_close_open_conversation`, `_drain_background`, `_composio_live`, `_openrouter_rejected`, `_ipc_*` as same-named private methods. `Seams` are the module functions the Python tests monkeypatch (`spend_sync.loop`, `ConnectBridge`, `telegram.make_inlet`, `build_memory`, `phone_call.make_voice`, `records.make_client`, `devices.list`, `PAGE_RETRY_SECS`); `Seams::default()` is the real ones |
| `_IPC` | `LiteDaemon::dispatch` (private `match`) | the six events, nothing else |
| `run_daemon` | `pub fn run_daemon(args: Option<&DaemonArgs>, options: LiteOptions) -> i32`, `struct DaemonArgs` | logging, the firewall (enforce), the profile check, full eve's plist, the profile applied again, the socket in use, then a multi-thread runtime with the loop watchdog, SIGUSR1's thread dump, and SIGINT/SIGTERM as `shutdown` |
| (helpers) | `expanduser`, `error_name`, `close_conversation`, `Environ` | `close_conversation` is the backstop's step (`_close_open_conversation`'s inner `close`) |

## Tests
| Python | Rust | Ported | Not ported, and why |
|---|---|---|---|
| `test_lite_daemon.py` (26) | `tests/daemon.rs` (21), `tests/structure.rs` (2), `src/tests.rs` (11) | 26: each has a same-named Rust test (the two import checks are one, below), every daemon test in a child process of the test binary (HOME, EVE_PROFILE and the key are the process's, as the fixture sets `os.environ`). The network guard is an HTTP proxy every client in the child goes through, refusing and recording hosts; `the_network_guard_catches_a_lookup` is its positive control. `the_lines_are_plain` covers the three parametrized cases. Extra: `the_backstop_closes_a_conversation_left_open` | Weaker in places Python reaches into objects Rust keeps private: the count of LinkChat/PhoneCalls/RtcHub constructions (each is built once by construction in `build`), `d.link._inlet() is d.inlet`, `d.rtc.link is d.link`, `isinstance(d.inlet.api, MutedBotApi)`, `calls.check_owner`, and `inlet._stopping` after the stop. `a_stop_during_a_late_page_start_leaves_no_page_open` cannot patch `LocalServer._bind` and `RtcHub.close` to slow them: the stop lands in the retry's window instead, and the page must be shut either way. `the_stop_runs_in_order_and_closes_an_open_conversation` observes the order from the Composio bridge's close (the page down, the peers closed, every task done, the socket still there) rather than spying on each method, and the brain's own close marker runs (Python patches it away; the backstop is tested on its own). `eve_lite_never_imports_full_eves_daemon` and `no_import_of_lite_reaches_full_eves_daemon` are one test of the Cargo graph (`no_dependency_of_lite_reaches_full_eves_daemon`): Rust links statically, so there is no run-time import to observe |
| `test_lite_boot_firewall.py` (2) | `tests/boot_firewall.rs` (2) | 2, same names: `run_daemon` in a child with shims on PATH, the proxy as the network guard, the mock OpenRouter exchange, the controls (`/usr/bin/true` and `~/Documents/private.txt` refused, counted as `exec:true` and `path:~/Documents`) | The banned-module list (`bleak`, `eve_bridge.daemon`, `sounddevice`, `Quartz` in `sys.modules`) has no Rust counterpart; the Cargo graph test covers it |
| `test_lite_memory_parity.py` (6, 8 cases) | `tests/memory_parity.rs` (7) | the statement check (source text of each copied item against eve-daemon's `run.rs`, comments aside) and its positive controls; file for file on two homes in a child per edition (lite, full); memory off on both sides; the dream started, held and ended by the shutdown; no memory or no client is no dream on either side | The dream comparison runs eve-daemon's `dream_loop` beside lite's rather than its `Daemon::start_dream` (which needs a whole Daemon); the statement check covers the copy |
| `test_lite_brain.py` (14) | `tests/brain.rs` (6) | the six that need no turn: `own_tools_and_tools_for_filter_in_lite_and_not_in_full_eve`, `the_checker_catches_a_mac_tool_and_an_app_name`, `each_swapped_text_is_still_in_link_chats_app_block`, `the_lite_app_block_names_the_web_app_and_keeps_the_rest`, `the_apps_block_is_built_from_the_names_the_bridge_lists`, `lite_instructions_say_what_eve_lite_is_in_a_positive_set`. The checker's universe is eve's own tools, eve lite's, Connect's and the web search (Python's adds every crate's tool names). `a_lite_doc_upload_reaches_the_brain_as_its_words_not_a_mac_task` is ported in eve-link-uploads | Not ported: the eight turn-driven tests (`a_lite_turn_offers_exactly_eve_lites_tools`, `whatever_else_is_lent_...`, `every_lite_app_turns_instructions_name_only_what_it_offers` ×6 shapes, `a_push_to_talk_turn_reads_the_web_apps_call_note`, `a_call_turn_reads_an_earlier_card_in_lite_words_...`, `full_eves_call_turn_reads_the_card_...`, `full_eves_call_note_and_style_are_its_own`): they run brain turns through eve-brain's test rig (`tests/support`, `lite_rig` in `tests/telegram_lite.rs`), which is eve-brain's and not exported; they test telegram.py under the lite profile, not lite.py |
| `test_lite_link_channel.py` (16) | eve-lite-rtc `tests/link_channel.rs` | 16, there | It tests lite_rtc.LinkChannel, not lite.py |
| `test_lite_e2e.py` | — | — | Headless Chromium against the built web app (gate L16): a browser harness, not a unit of this crate |
| `test_lite_requirements.py` | — | — | The Python release's wheel closure (uv.lock, install-lite.sh): nothing of it exists in a Rust build |
| `test_cli_lite.py` (ref) | — | — | The CLI's (eve-bridge); `run_daemon` is what it calls, tested here |

## Boundaries
None. Every function and class of lite.py is ported.

## Differences
- The brain's goodbye. Python's `_stop` cancels the brain's task and `gather`s it, and the inlet's own `finally` runs its goodbye (`TelegramInlet._shutdown`) inside that gather. eve-brain's inlet starts its goodbye from a guard dropped with the cancelled task, on a task of its own, so the daemon then waits up to CLOSE_MARKERS_SECS for the inlet's conversation to close (`inlet.conversation()` is None) before it closes Composio and runs the backstop. Same order, same outcome; no upstream edit.
- `_ipc_status` warms the credit read through `LocalServer::status()` (the page's own status read, which reads the credit under the same conditions), because lite_web's `_credit` is private to that crate.
- `make_voice` and `make_watcher` and the default `make_inlet` and `build_memory` are run under `catch_unwind`: a panic in one is that part's exception, named `panic` in the log line.
- `run_daemon`: an error out of `run` (Python's exception propagating out of `run_until_complete`) is logged by type and the exit code is 1; `profile.apply` failing (a ProfileError that Python would raise out of run_daemon) is EXIT_CONFIG with the error's line; a runtime that cannot start is 1 with one line.
- The runtime is tokio's multi-thread one (Python's single event loop): `shutdown` and `restart` are a watch channel, which wakes the waiter from any thread without `call_soon_threadsafe`.
- The brain reads Composio through `eve_composio_tools::ComposioBridge` (its tools, its accounts note, its calls), which `ConnectApps` builds over the same ConnectBridge the daemon starts and closes; Python lends the ConnectBridge object itself (duck-typed).
- The process firewall (eve-profile) judges the calls made through its functions (`firewall::spawn/status/output`, `audit_path`); Rust has no audit hook over every `open` and `exec`. The boot test's controls go through it.
- The network guard in the tests is an HTTP(S) proxy set in the child's environment (every reqwest client honours it), not a patched `getaddrinfo`.
- The daemon task names keep Python's (`lite-brain`, `lite-watch`, `lite-spend-sync`, `lite-page`, `lite-ipc`, `memory-dream`).
