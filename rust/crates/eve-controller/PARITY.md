# eve-controller
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/controller.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `log` | `tracing` records, target `eve_controller` | Same message texts: `controller: looking for the board with USB serial …`, `controller: connected`, `controller: disconnected` (INFO). |
| `INPUT_CMDS` | `INPUT_CMDS: [&str; 4]` | `ptt`, `key`, `focus`, `music`. |
| `MIRRORED_CMDS` | `MIRRORED_CMDS: [&str; 2]` | `agent`, `listen`. |
| `SILENT` | `SILENT: LazyLock<Value>` | `{"cmd": "sound", "on": false}`. |
| `controller_serial(environ=None)` | `controller_serial(Option<&HashMap<String, String>>) -> Option<String>` | `None` reads the process env; `EVE_CONTROLLER_SERIAL` with Python `strip()`; blank → `None`. |
| `mirrored(obj)` | `mirrored(&Value) -> bool` | A `null` `cmd` counts as absent (Python's `.get` returns `None`); then `total` or `time` key. A non-string `cmd` is never in the set. |
| `Controller` (class) | `Controller` | Link state shared through an `Arc`; the boot hook holds a `Weak` (no cycle). |
| `Controller.__init__(serial_number, on_input, link_factory=EveSerial)` | `Controller::new(serial, on_input)`, `Controller::with_link_factory(serial, on_input, &LinkFactory)` | Port `usbsn:<serial>`; `link.on_boot` = replay. Default factory `eve_serial_link` = `EveSerial::new(on_message, port, 115200, {})`, EveSerial's defaults. |
| `link_factory` / the link surface used | `trait ControllerLink` (`port`, `connected`, `wait_connected`, `send`, `run`, `stop`, `set_on_boot`), impl for `EveSerial`; `LinkFactory` | The seam the tests' fake link implements, as the Python tests pass `link_factory`. |
| `Controller.serial_number`, `on_input`, `link`, `last` | pub fields `serial_number`, `on_input`; `link()`, `last()` (a copy) | `last` keeps first-seen key order, as a Python dict. |
| `Controller.connected` | `connected()` | |
| `Controller._on_message(obj)` | closure built in `with_link_factory` | Only `cmd` in `INPUT_CMDS` reaches `on_input`. |
| `Controller.mirror(obj)` | `async mirror(&Value)` | Kind = cmd, else `time`, else `heartbeat`; send errors swallowed. |
| `Controller.show(obj)` | `async show(&Value)` | Only `music_mode` is kept; send errors swallowed. |
| `Controller.send_audio(obj)` | `async send_audio(&Value) -> anyhow::Result<()>` | Errors reach the caller, as Python lets them raise. |
| `Controller._replay()` | `async replay()` | `SILENT`, then each kept message; errors swallowed per send. |
| `Controller.run()` | `async run()` | Link run spawned; wait connected → replay → poll every 1.0 s → `disconnected` + 0.5 s; link task aborted and awaited on exit, also when `run()` is dropped. |
| `Controller.stop()` | `async stop()` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_controller.py` | all 5 | `src/tests.rs`: same names | — |
| `bridge/tests/test_music_mode.py` | `test_controller_show_keeps_the_mode_only` | `src/tests.rs`: same name | The rest test music_mode.py (eve-music-mode). |
| `bridge/tests/test_serial_skip.py` | — | — | `test_the_daemon_joins_the_skip_list_with_the_controller` constructs `Daemon` (eve-daemon); the others test serial_transport (eve-serial-transport). |

Extra Rust tests: `music` input and null/non-string `cmd`, `null` cmd in `mirrored`, kind order and overwrite on replay, send errors swallowed by mirror/show/replay but returned by `send_audio`, `send_audio` only while connected, reconnect replays again and `stop` ends `run`, dropping `run` drops the link task, the real `EveSerial` link on the `usbsn:` port, `strip` of separators.

## Boundaries
None.

## Differences
- `run()` also ends when the link's own `run()` returns while waiting for a connect (it only returns after `stop()`); Python would keep awaiting `wait_connected()` forever there.
- `send_audio` returns `Result` instead of raising.
