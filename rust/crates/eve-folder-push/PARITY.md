# eve-folder-push
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/folder_push.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `CHUNK_SIZE` | `CHUNK_SIZE: usize = 200` | Verbatim; `constants_match_python` reads the Python file |
| `MAX_TOTAL_BYTES` | `MAX_TOTAL_BYTES: u64 = 1_800_000` | Verbatim |
| `ACK_TIMEOUT_FAST`, `ACK_TIMEOUT_SLOW` | same names (`f64` seconds, 5.0 and 15.0) and `AckTimeouts { fast, slow }` (`Default` = those) | Python reads the module globals at call time and its tests monkeypatch them; Rust passes `AckTimeouts` to `push_character_with` |
| `log` | `tracing` | Same messages: `folder_push: name='…'  files=N  total=N bytes`, `folder_push: <file> done (N/N bytes total)`, `folder_push: char_end acked — stick has switched to '…'` (names in Python `repr`) |
| `_enumerate_files` | `enumerate_files(&Path) -> Result<Vec<PathBuf>, PushError>` | Non-recursive; `is_file` follows symlinks and treats a stat error as "not a file"; dotfiles skipped; sorted by `(0 if manifest.json else 1, name)` in code-point order. A listing error is the `OSError` Python's `iterdir` raises |
| `_pack_name` | `pack_name(&Path) -> Result<String, PushError>` | `manifest.json` that exists (following symlinks), reads as UTF-8 and parses as JSON: its string `name`, `str.strip`ped (Python's whitespace set, including 0x1c-0x1f), when non-empty; otherwise the folder's basename. A read/decode/parse failure falls back to the folder name. A manifest that parses to a non-object raises, as Python's `data.get` does (`AttributeError: '<type>' object has no attribute 'get'`) |
| `push_character` | `push_character(&D, &str)` and `push_character_with(&D, &str, AckTimeouts)`, `D: PushTarget` | `expanduser` (`~`, `~user`, HOME then passwd) and non-strict `resolve` (public `expanduser`, `resolve`); `not a directory: <path>`, `no files to push in <path>`, `folder too large: 1,800,001 bytes > 1,800,000 cap` as `ValueError`; then char_begin (slow) → per file: file, chunks of up to 200 raw bytes base64'd with chunk ack `n` = bytes of this file so far, file_end (fast) → char_end (slow). Wire JSON keys in Python's order (`{"cmd","name","total"}`, `{"cmd","path","size"}`, `{"cmd","d"}`). Returns `PushSummary { name, files, total_bytes }` (`to_json` gives Python's dict) |
| `_send_expect` | `send_expect(&D, Value, ack_type, Duration, Option<i64>) -> Result<Value, PushError>` | Waiter registered before the send; dropping the waiter is Python's `waiter.cancel()`. `ble write failed for cmd:<cmd>` and `<ack_type> rejected: <error or "no detail">` (Python truthiness of `ok` and `error`, `str()` of a non-string error) as `RuntimeError`; a timeout is `TimeoutError` with an empty message |
| (`daemon` argument: `expect_ack`, `ble.send`, `wait_for_ack`) | trait `PushTarget` (`type Waiter`, `expect_ack`, `send`, `wait_for_ack`) | The seam the Python duck-types; the tests' `StubDaemon` implements it |
| (`Daemon.expect_ack`, `Daemon.wait_for_ack`, `_ack_waiters` and the ack-routing branch of `Daemon._handle_ble`) | `AckRouter` (`expect_ack`, `wait_for_ack`, `route`, `pending`) and `AckWaiter` | Ported here because folder push is what they serve and test_serial_lean C exercises them together; the daemon port should hold an `AckRouter` and call `route` for each BLE object that is not a status ack. Oldest matching waiter first; a waiter with `n` skips a truthy-`ok` ack whose `n` is not equal (Python `==`: `200.0` and `True` compare numerically); cancelled (dropped) waiters skipped and pruned on the next register; the entry is removed when `wait_for_ack` ends |
| (exceptions raised) | `PushError` (`Value`, `Runtime`, `Timeout`, `Cancelled`, `Attribute`, `Os`) with `type_name()` | So the daemon's IPC reply keeps `"{type(e).__name__}: {e}"`. `Os` carries the `OSError` subclass by errno (`FileNotFoundError`, `PermissionError`, `IsADirectoryError`, `NotADirectoryError`, …) and Python's `[Errno N] <strerror>: '<path>'` text |
| (`asyncio.TimeoutError` / `CancelledError` from the wait) | `AckWaitError { Timeout, Cancelled }` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_folder_push.py` | `test_enumerate_skips_dotfiles`, `test_enumerate_puts_manifest_first`, `test_pack_name_prefers_manifest`, `test_pack_name_falls_back_to_folder`, `test_pack_name_ignores_bad_manifest`, `test_push_character_sends_canonical_sequence`, `test_push_character_chunks_are_valid_base64`, `test_push_character_rejects_oversize`, `test_push_character_rejects_non_dir`, `test_push_character_rejects_empty` | same names in `src/tests.rs` | None |
| `bridge/tests/test_serial_lean.py` (tests_ref, part C) | `test_c_ack_before_the_sender_resumes_is_not_lost`, `test_c_late_chunk_ack_does_not_satisfy_the_next_chunk` | same names, through `AckRouter` and a firmware-like board | Parts A (serial write lock) and B (status poller) test other crates' modules |
| (Rust only) | | `enumerate_skips_subdirectories_and_follows_file_symlinks`, `enumerate_orders_by_code_point`, `pack_name_strips_and_rejects_blank_or_non_string_names`, `pack_name_non_object_manifest_raises_attribute_error`, `push_character_wire_payloads_match_python`, `push_character_rejected_ack_raises_runtime_error`, `push_character_failed_write_raises_runtime_error`, `push_character_expands_home`, `resolve_folds_dotdot_after_a_missing_part`, `os_error_text_matches_python`, `thousands_matches_python_format`, `router_failure_ack_matches_a_waiter_that_names_n`, `router_oldest_waiter_first_and_float_n_matches`, `router_dropped_waiter_is_skipped_and_pruned`, `constants_match_python`, `py_helpers_follow_python` | |

## Boundaries
None. The BLE write and the daemon are behind the `PushTarget` trait; the daemon port supplies them.

## Differences
- `manifest.json` is parsed with `serde_json`, which rejects the non-standard `NaN`/`Infinity` literals Python's `json` accepts; such a manifest falls back to the folder name instead of using its `name`.
- File names that are not valid UTF-8 are sent lossily converted (Python would carry surrogate escapes and fail to encode them); macOS file systems do not produce such names.
