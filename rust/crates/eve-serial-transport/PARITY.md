# eve-serial-transport
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/serial_transport.py` | `src/lib.rs` (transport, port pick, open), `src/posix.rs` (the pyserial `Serial` port handle), `src/pystr.rs` (`str.strip` / `repr` helpers) |

## API
| Python | Rust | Notes |
|---|---|---|
| `IncomingHandler` | `IncomingHandler = Arc<dyn Fn(Map<String, Value>) -> HandlerFuture>` | A line starting with `{` that parses is always an object, so the handler takes the map. |
| `RECONNECT_SECS`, `FORCED_HOLDOFF_SECS`, `RX_TRIP_PULSE_COUNT`, `RX_TRIP_WINDOW_SECS`, `RX_SILENCE_SECS`, `MAX_LINE_BYTES`, `USB_SERIAL_PREFIX` | same-named `pub const`s | Same values. The three timings are also carried per transport in `Timing` (default = the constants) so tests can shrink them, where Python tests monkeypatch the module. |
| `_ESP32S3_VID` | `ESP32S3_VID: u16 = 0x303A` | |
| `_IS_CRASH`, `_IS_BOOT`, `_IS_MOTION` | `IS_CRASH`, `IS_BOOT`, `IS_MOTION` (`LazyLock<Regex>`) | Same patterns; `re.IGNORECASE` → `(?i)`; `search` → `is_match` (unanchored, `^` only at the start). |
| `_read_chunk(ser)` | `read_chunk(&dyn SerialHandle) -> Result<Vec<u8>, SerialError>` | `read(1)`, then `read(in_waiting)` when anything waits. |
| `serial_skip(environ=None)` | `serial_skip(Option<&HashMap<String, String>>) -> BTreeSet<String>` | `None` reads the process env. Comma split, Python `strip()` (incl. U+001C..U+001F), lowercased, empties dropped. |
| `_resolve_port(pattern, skip_serials)` | `resolve_port(&str, &BTreeSet<String>) -> Option<String>`; logic in `resolve_port_with(pattern, skip, glob_fn, list_ports)` | Same branches: `usbsn:` by serial on a `/cu.` node (None if the listing fails); literal path as given; glob sorted; skip filter; ESP32-S3 preferred with the same WARNING; all-vendor-no-Espressif → None; listing failure with a skip → None, without → glob's first. |
| `glob.glob` (inside `_resolve_port`) | `glob_ports(pattern)` | `glob` crate with `require_literal_leading_dot` (Python never matches a leading dot with a wildcard), sorted. |
| `list_ports.comports()` (inside `_resolve_port`) | `comports() -> Result<Vec<PortInfo>, String>`, `PortInfo { device, vid, serial_number }` | `serialport::available_ports()` (IOKit on macOS: `idVendor`, `USB Serial Number`). On macOS only the `/dev/cu.*` callout node is kept, as pyserial's `list_ports_osx` lists only `IOCalloutDevice`. |
| `_open_port(port, baud, pulse)` | `open_port(&PortOpener, &Sleeper, port, baud, pulse)` | DTR up (failure closes the handle and errors); pulse RTS up, sleep 0.1, RTS down, DTR up (failure logged `serial: RTS pulse failed: …`); `reset_input_buffer` errors ignored. |
| `serial.Serial(port, baud, timeout=1, write_timeout=2, exclusive=True)` | `PosixSerial::open(port, baud)` / `open_with(..)`, `open_real` (the default `Seams::open`) | pyserial serialposix on the raw fd: `O_RDWR\|O_NOCTTY\|O_NONBLOCK` + close-on-exec, `flock(LOCK_EX\|LOCK_NB)` (no `TIOCEXCL`), pyserial's termios flag set (raw, 8N1, no xon/xoff, no rtscts, VMIN 1 / VTIME 0), DTR then RTS asserted in one try (EINVAL/ENOTTY from DTR ends it with RTS untouched, as pyserial's single `try`), `TCIFLUSH`, two non-blocking abort pipes. `read` = pyserial's select loop on `poll` with the abort pipe; `write` = write then wait writable within `write_timeout`, `SerialError::WriteTimeout` (`Write timeout`) on expiry. Error texts follow pyserial: `[Errno 2] could not open port P: [Errno 2] No such file or directory: 'P'`, `Could not exclusively lock port`, `read failed: …`, `write failed: …`, `device reports readiness to read but returned no data (…)`, `Attempting to use a port that is not open`. Custom (non-termios) bauds on macOS go through `IOSSIOSPEED`. |
| `serial.SerialTimeoutException` / `SerialException` | `SerialError::{WriteTimeout, Serial(String)}` | `_note_deaf`'s `isinstance` check is `matches!(err, WriteTimeout)`. |
| pyserial `Serial` surface used here | `trait SerialHandle` (`read`, `in_waiting`, `write`, `is_open`, `cancel_read`, `close`, `set_dtr`, `set_rts`, `reset_input_buffer`, `port`) | The seam the tests' `FakeSerial` implements, as the Python tests patch `serial.Serial`. |
| `time.monotonic` | `monotonic()`, `Seams::monotonic` | CPython's clock: `CLOCK_UPTIME_RAW` (= `mach_absolute_time`, paused while the Mac sleeps) on macOS, `CLOCK_MONOTONIC` elsewhere. |
| `EveSerial` (class) | `EveSerial` (`Clone`, shared `Arc` state) | `EveSerial::new(on_message, port, baud, skip_serials)`; `with_seams(.., Seams, Timing)` for the test seams. |
| `EveSerial.wired` | `EveSerial::WIRED` / `wired()` | `true`. |
| `EveSerial.on_message`, `on_boot`, `port_pattern`, `baud`, `skip_serials` | constructor arg, `set_on_boot`, `port_pattern()`, `baud()`, `skip_serials()` | |
| `EveSerial.connected` | `connected()` | handle present, open, no drop pending. |
| `EveSerial.wait_connected()` | `async wait_connected()` | `watch` channel in place of `asyncio.Event`. |
| `EveSerial.send(obj, codec=None)` | `async send(&Value, Option<&str>) -> Result<bool, LookupError>` | `eve_protocol::encode`; `Err` where Python's `encode` raises `LookupError`. Handle captured once, re-checked under the lock. |
| `EveSerial._write_holding_lock` | inside `send`: the owned send-lock guard moves into the blocking write job | The lock is released only when the write has returned, even when the sender's future is dropped (cancelled); a failure after cancellation is still logged and still condemns its handle (Serial.lean A). |
| `EveSerial.run()` | `async run()` | Same loop: resolve off the runtime, missing-port warnings with the 30 s → doubling → 600 s deadline back-off, `device reappeared` INFO, pulse WARNING, open off the runtime, connect, read loop with `RX_SILENCE_SECS` watchdog, line split, `MAX_LINE_BYTES` discard, close on exit (also when the future is dropped, as Python's `finally` runs on cancel), holdoff `reopen_holdoff or RECONNECT_SECS`. |
| `EveSerial._dispatch_line(line)` | `dispatch_line(&[u8])` | UTF-8 with replacement, Python `strip()`; non-JSON → `stick: …` at WARNING (crash) / INFO (motion) / DEBUG; boot hook debounced 5 s; JSON → spawned `on_message`; bad JSON → DEBUG `serial: bad json: <repr(text[:120])>`. |
| `EveSerial._spawn(coro)` | `Inner::spawn` + `pending_tasks()` | `tokio::spawn`; a counter in place of the `_tasks` set (tokio keeps spawned tasks alive itself). |
| `EveSerial._close_current()` | `Inner::close_current` | |
| `EveSerial._request_drop(why)` | `Inner::request_drop` | First reason wins; clears connected; `cancel_read`. Never closes. |
| `EveSerial._note_rx_trip(port)` | `Inner::note_rx_trip` | Sliding 600 s window; 3 trips of one port arm the pulse and clear the list. |
| `EveSerial._note_deaf(ser, err)` | `Inner::note_deaf` | |
| `EveSerial.pulse_reset(why)` | `pulse_reset(&str)` | DTR down, RTS up, 0.1 s, RTS down on the live handle, then `RTS reset: <why>` drop. Blocks the caller 0.1 s, as Python blocks the loop. |
| `EveSerial.force_reconnect(why)` | `force_reconnect(&str)` | |
| `EveSerial.stop()` | `async stop()`, plus `request_stop()` for sync callers | |
| `EveSerial._ser`, `_drop_reason`, `_pulse_on_open` (read by tests/daemon) | `handle()`, `drop_reason()`, `pulse_on_open()` / `set_pulse_on_open()`, `adopt(handle)` | `adopt` = the reader's `_ser = ser; _drop_reason = None`. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_serial_transport.py` | all 19 | `src/tests.rs`: same names | — |
| `bridge/tests/test_serial_lean.py` | `test_a_cancelled_sender_keeps_the_port_until_its_write_returns` | `src/tests.rs`: same name | `test_b_*` (2) drive `Daemon._status_poller` / `_handle_ble` (eve-daemon); `test_c_*` (2) drive `folder_push` + `Daemon.wait_for_ack` (eve-folder-push / eve-daemon). |
| `bridge/tests/test_serial_skip.py` | `test_the_setting_is_a_comma_list_of_serials_any_case`, `test_with_the_robot_unplugged_the_stick_is_not_opened`, `test_with_the_robot_plugged_in_it_is_still_found` | `src/tests.rs`: same names | `test_the_daemon_joins_the_skip_list_with_the_controller` constructs `Daemon` (eve-daemon). |

Extra Rust tests (`src/tests.rs`): literal port, `usbsn:` needs a callout node and a working list, unknown-vendor pick and ESP32-S3 preference WARNING texts, `read_chunk` drains `in_waiting`, `open_port` modem-line order and flush, DTR failure closes, `stick:` log levels and the `bad json` repr line, boot-hook debounce, newline-less spew discard, `pulse_reset` order and first-reason-wins, `send` bytes and unknown codec, `wait_connected` and close on stop, failed-open log and retry, glob sorting and dot files, the regexes, the real USB listing (callout nodes only on macOS). `monotonic_is_pythons_clock` checks the clock against `mach_absolute_time`; `a_lone_surrogate_escape_is_bad_json` pins the surrogate difference. `src/posix.rs`: the one-try DTR/RTS raise, then the real handle over a pty: pyserial's open error text, the exclusive lock, read/`in_waiting`/timeout, write, `cancel_read` waking a blocked reader, write timeout on an undrained pty, closed-port errors. `src/pystr.rs`: `strip`, `[:n]`, `repr`.

## Boundaries
None. The port handle is pyserial's posix behavior written on the raw fd (`libc`); enumeration uses the `serialport` crate's IOKit/udev listing. Non-unix hosts have no handle: `open_real` returns an error there (the Python daemon runs on macOS).

## Differences
- `run()`'s sleeps (missing-port retry, failed-open retry, reopen holdoff) end early when `stop()` is called; Python's sleep runs out first and then sees the stop flag. Same outcome, sooner.
- JSON lines are parsed with serde_json: a line with `NaN`/`Infinity`, an integer beyond 64 bits (arrives as a float), or nesting past 128 levels is treated as bad JSON (DEBUG) where Python would accept it (or, for deep nesting, hit `RecursionError` and reconnect).
- `repr()` in the `bad json` DEBUG line knows printable characters by approximation (control, separator, surrogate-free format and private-use ranges); an unassigned code point prints raw where Python escapes it.
- `IOSSIOSPEED` uses the header value `_IOW('T', 2, speed_t)` (`0x80085402`, 8-byte `speed_t`); pyserial passes `0x80045402` with a 4-byte int. Only for non-termios bauds; the daemon uses 115200.
- On Linux a baud rate without a termios `B*` constant fails to open (`Invalid baud rate: N`); pyserial would use `TCSETS2`. eve's boards run at 115200.
- A JSON line holding a lone surrogate escape (`"\ud800"`) is bad JSON (DEBUG) here; Python's `json.loads` accepts it and hands the handler a `str` with the surrogate, which a Rust `String` cannot hold. The firmware's JSON writer does not emit such escapes.
- `glob_ports` uses the `glob` crate: a pattern it cannot compile (an unclosed `[`, or `**` inside a component) matches nothing, where Python's `fnmatch` reads the `[` literally and treats `**` as `*`. The configured patterns are `/dev/cu.usbmodem*`-style.
- `send` returns `Err(LookupError)` for an unknown codec instead of raising.
- Log records go to the `eve_serial_transport` tracing target (Python: logger `eve_bridge.serial_transport`); message texts are identical.
