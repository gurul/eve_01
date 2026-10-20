# eve-ble
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/ble.py` | `crates/eve-ble/src/lib.rs` (`EveBle`, the reconnect loop, chunking, the radio reset); `src/btle.rs` (what bleak's `BleakScanner` / `BleakClient` do, on btleplug); `src/decoder.rs` + `data/*.dec.bin` (the pending-byte state of CPython's incremental decoders) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_NAME_PREFIX` | `DEFAULT_NAME_PREFIX: &str = "Claude"` | |
| `SCAN_TIMEOUT_SECS` | `SCAN_TIMEOUT_SECS: f64` | 3.0 on Windows, 10.0 elsewhere, as `sys.platform == "win32"` decides. |
| `RECONNECT_BACKOFF_BASE_SECS`, `RECONNECT_BACKOFF_MAX_SECS`, `STABLE_CONNECTION_SECS` | same consts | 3.0, 60.0, 30.0. |
| `RADIO_RESET_AFTER_MISSES`, `RADIO_RESET_MAX_ATTEMPTS`, `RADIO_RESET_COOLDOWN_SECS` | same consts | 5, 3, 120.0. |
| `IncomingHandler` | `IncomingHandler = Arc<dyn Fn(Value) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>` | Receives whatever `LineAssembler` parsed. |
| `EveBLE` (class) | `EveBle<T: BleTransport = BtleplugTransport>` | `EveBle::new(on_message, name_prefix, address)` is `EveBLE(on_message, name_prefix=..., address=None)`; `with_transport` adds the radio and clock seams. `on_message`, `name_prefix`, `address` are public fields as in Python. |
| `EveBLE.connected` (property) | `connected(&self) -> bool` | Client present and `is_connected`. |
| `EveBLE.wait_connected` | `async fn wait_connected(&self)` | `asyncio.Event` is a `tokio::sync::watch<bool>`. |
| `EveBLE.send(obj, codec=None)` | `async fn send(&self, &Value, Option<&str>) -> Result<bool, LookupError>` | `Ok(false)` when not connected or a write fails (`ble send failed: …` logged); `Err` is the `LookupError` `protocol.encode` raises, which escapes `send` in Python too. Chunk size `max(20, mtu_size - 3)`, a yield after each Write Without Response, writes serialized by a lock. |
| `EveBLE.run` | `async fn run(&self)` | Same loop: miss counting, radio-reset eligibility (threshold since last reset, attempt cap, cooldown), backoff doubling to 60s, the `continue` paths that skip the reconnect wait, the connected event cleared the instant the drop is seen, reset state cleared on find and after disconnect, stable-connection backoff reset, same log lines. Python's `last_reset_ts = 0.0` sentinel is `Option<f64>`. |
| `EveBLE.stop` | `async fn stop(&self)` | Sets the stop flag, disconnects a live client, ignores errors. |
| `EveBLE._try_reset_radio` | `EveBle::try_reset_radio` (private) → `BleTransport::try_reset_radio` → `pub async fn try_reset_radio(&dyn Clock) -> bool` | Off Windows: `false`, no sleep. On Windows: the WinRT `BluetoothAdapter.GetDefaultAsync` / `GetRadioAsync` / `SetStateAsync(Off)`, 2s, `SetStateAsync(On)`, 2s, with the same branches and log lines (an enum logs as its integer, as `str()` of a Python 3.11+ IntEnum does). |
| `EveBLE._find_device` | `EveBle::find_device` (private) → `BleTransport::find_device`; `BtleplugTransport::find_device` | Address given: first device whose address equals it case-insensitively (`find_device_by_address`). Else first whose `(adv.local_name or d.name) or ""` starts with the prefix (btleplug `advertisement_name` or `local_name`). Only devices heard during this scan; `None` after the timeout. On macOS the address is the CoreBluetooth UUID, upper-case as bleak prints it. A powered-off adapter raises `Bluetooth device is turned off`, as bleak does. |
| `EveBLE._on_notify` | `EveBle::spawn_reader` (private) | A fresh `LineAssembler` per connection; each parsed object goes to the handler on its own task. |
| `EveBLE._dispatch` | `dispatch` (private) | Handler errors and panics are logged as `on_message handler crashed`, never raised. |
| `BleakClient(device)` context | `BleTransport::connect` → `BtleplugClient` (`BleClient`) | Connect with bleak's 10s default (`CONNECT_TIMEOUT_SECS`), cancel the attempt on failure, discover services; on exit `disconnect`, whose error replaces the body's as `__aexit__`'s does. `start_notify` / `write_gatt_char` find the characteristic by UUID with bleak's `Characteristic … was not found!` error. |
| `_codec_safe_chunks(data, max_size, codec=None)` | `codec_safe_chunks(&[u8], usize, Option<&str>) -> Result<Vec<&[u8]>, ChunkError>` | `None` and exactly `"utf-8"` take the UTF-8 splitter; any other name goes through the incremental-decoder path, aliases included (`"utf8"`, `"cp936"`, `"sjis"`), as `codecs.getincrementaldecoder` resolves them. `ChunkError::MaxSize` is `ValueError("max_size must be positive")`; `ChunkError::Lookup` is `LookupError("unknown encoding: …")`, raised even for empty data. |
| `_utf8_safe_chunks(data, max_size)` | `utf8_safe_chunks(&[u8], usize) -> Result<Vec<&[u8]>, ChunkError>` | |
| `_is_utf8_continuation(byte)` | `is_utf8_continuation(u8) -> bool` (private) | |
| `_utf8_codepoint_size(lead_byte)` | `utf8_codepoint_size(u8) -> usize` (private) | |
| `codecs.getincrementaldecoder(codec)(errors="replace")` + `getstate()[0]` | `decoder::IncrementalDecoder` (`feed`, `has_pending`) | gbk/big5/shift_jis: CPython's lead bytes and completing pairs, generated and proven against CPython 3.13.14 by `tools/gen_decoder_tables.py` (every byte, every pair, random strings). UTF-8: CPython's non-final buffering (valid prefixes only, E0/ED/F0/F4 second-byte ranges). |
| `asyncio.sleep`, `time.monotonic` | `Clock` trait, `TokioClock` | The seam the radio-reset tests patch. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_ble_radio_reset.py` | `test_reset_fires_at_threshold_not_before`, `test_no_reset_below_threshold`, `test_failed_reset_does_not_arm_guard_and_still_sleeps`, `test_successful_reset_resets_backoff_to_base`, `test_reset_retries_when_first_reset_does_not_help`, `test_reset_attempts_capped`, `test_reset_rearmed_after_disconnect`, `test_connected_event_cleared_after_disconnect` | `src/tests.rs`: same names, with the `_Harness` / `_FakeClient` / fake clock as `Harness` + `FakeTransport` / `FakeClient` / `FakeClock` | None |
| `bridge/tests/test_protocol.py` (tests_ref, the ble half) | `test_utf8_safe_chunks_do_not_split_cjk_at_boundary`, `test_utf8_safe_chunks_keeps_codepoint_when_max_size_is_tiny`, `test_ble_send_never_cuts_a_double_byte_character` (gbk, big5, shift_jis) | `src/tests.rs`: same names, the parametrized one as `..._gbk`, `..._big5`, `..._shift_jis` | The rest of the file tests `protocol.py` and is ported in eve-protocol. |

Extra Rust tests: `codec_safe_chunks` outputs recorded from CPython (aliases, invalid UTF-8, invalid CJK pairs, shift_jis katakana, empty), its errors, the decoder tables' shape, UTF-8 prefix rules, `send` when disconnected / failing / with an unknown codec / below 20-byte MTU, notification assembly and dispatch (including a failing handler) through `run`, `wait_connected`, `stop`, a connect error's backoff, the off-Windows radio reset, `Send` futures on the real transport, constants. `differential_against_python_ble` (`#[ignore = "live: ..."]`, needs `EVE_BLE_PYTHON`) compares `codec_safe_chunks` with the bridge's `_codec_safe_chunks` on 3000 random payloads in seven codec settings; it passes against CPython 3.13.14.

## Boundaries
None. The decoder tables are data generated once from CPython (`tools/gen_decoder_tables.py`), not a runtime call. bleak is replaced by btleplug in-process.

## Differences
- Codecs: only `utf-8`, `gbk`, `big5`, `shift_jis` and their CPython aliases are known (eve-protocol's `Codec`). Another codec Python knows raises `LookupError` here; callers only pass `protocol.CJK_CODECS` values.
- `send` takes a `serde_json::Value`, as eve-protocol's `encode` does.
- Notification objects are dispatched with `tokio::spawn`, so on a multi-threaded runtime two handlers started from one notification may begin in either order; `asyncio.create_task` starts them in order. Each still runs concurrently, as in Python.
- `BleClient::is_connected` is a flag kept by the client (cleared by the adapter's disconnect event, a once-a-second check, or `disconnect`), because btleplug's own check is async and `connected` is read synchronously by the daemon.
- The Windows radio reset compiles only on Windows and was written against `windows` 0.62 (the version btleplug uses there); it has not been built on Windows from this Mac.
