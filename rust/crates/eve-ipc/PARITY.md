# eve-ipc
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/ipc.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_UNIX_PATH` | `DEFAULT_UNIX_PATH` | `/tmp/eve-bridge.sock` |
| `DEFAULT_TCP_HOST` | `DEFAULT_TCP_HOST` | `127.0.0.1` |
| `DEFAULT_TCP_PORT` | `DEFAULT_TCP_PORT` | `48765` (`u16`) |
| `os.environ.get("EVE_BRIDGE_SOCK")` | `SOCK_ENV` | env name as a constant |
| asyncio `StreamReader` limit `2**16` | `READ_LIMIT` | 65536; same `readline` overrun rule |
| `ConnHandler` | `ConnHandler` | `Arc<dyn Fn(IpcStream) -> BoxFuture<()>>`; `IpcStream` is the accepted Unix or TCP stream (reader and writer in one) |
| `Transport` (Protocol) | `enum Transport { Unix, Tcp }` | `address`, `start_server`, `sync_connect`, `is_in_use`, `cleanup_stale`, plus `as_unix`/`as_tcp` for the `isinstance` checks `lite.py` makes |
| `Transport.start_server` | `Transport::start_server` → `Server` | `Server` is `asyncio.Server`: `close`, `wait_closed` (waits for accepted connections, as Python 3.12+), `serve_forever`, `addresses`, `is_serving`; backlog 100 like asyncio |
| `Transport.sync_connect` | `Transport::sync_connect(Duration)` → `SyncStream` | connect capped by the timeout, then the same timeout on every read and write (`settimeout`) |
| `UnixTransport` | `UnixTransport` | `new` raises the same RuntimeError text on Windows (`SpecError::UnixUnsupported`); `new_on(path, Platform)` for tests |
| `UnixTransport.address` | `UnixTransport::address` | the path |
| `UnixTransport.start_server` | private `UnixTransport::bind`, used by `Transport::start_server` | unlinks an existing file, binds, `chmod 0o600` (errors ignored); EADDRINUSE reads `Address '<path>' is already in use` like asyncio |
| `UnixTransport.sync_connect` | `UnixTransport::sync_connect` | `socket2` connect with timeout |
| `UnixTransport.is_in_use` | `UnixTransport::is_in_use` | missing → false; refused / not found → removes the stale file, false; any other error or success → true; 0.5 s |
| `UnixTransport.cleanup_stale` | `UnixTransport::cleanup_stale` | unlink if present, errors ignored |
| `TcpLoopbackTransport` | `TcpLoopbackTransport` | `new(host, port)`: empty host → `127.0.0.1`; host must be `127.0.0.1` or `localhost`, same ValueError text |
| `TcpLoopbackTransport._ALLOWED_HOSTS` | private `ALLOWED_HOSTS` | same two hosts |
| `TcpLoopbackTransport.address` | `TcpLoopbackTransport::address` | `host:port` |
| `TcpLoopbackTransport.start_server` | private `TcpLoopbackTransport::bind` | binds every resolved address like `asyncio.start_server` (`localhost` → 127.0.0.1 and ::1), SO_REUSEADDR off Windows, IPV6_V6ONLY, skips EADDRNOTAVAIL |
| `TcpLoopbackTransport.sync_connect` | `TcpLoopbackTransport::sync_connect` | AF_INET: first IPv4 address of the host |
| `TcpLoopbackTransport.is_in_use` | `TcpLoopbackTransport::is_in_use` | any connect error → false; 0.5 s |
| `TcpLoopbackTransport.cleanup_stale` | `TcpLoopbackTransport::cleanup_stale` | no-op, as in Python |
| `parse_spec` | `parse_spec` / `parse_spec_on(spec, Platform)` | same `rpartition(":")`, drive-letter, digit, backslash and slash rules; `port out of range: N` prints the int (leading zeros dropped) |
| `default_spec` | `default_spec` / `default_spec_on(Platform)` | |
| `make_transport` | `make_transport(Option<&str>)` / `make_transport_from(spec, env, Platform)` | spec, then non-empty `EVE_BRIDGE_SOCK`, then the default; empty strings count as unset (Python `or`) |
| `sys.platform == "win32"` | `Platform::{Windows, Posix}`, `Platform::current()` | seam for the tests that monkeypatched `sys.platform` |
| ValueError / RuntimeError | `SpecError` | `python_type()` names the Python class; messages verbatim, `{x!r}` through `py_str_repr` |
| `Handler` | `Handler` + `handler(f)` | `Arc<dyn Fn(Value) -> BoxFuture<anyhow::Result<Value>>>`; the request is any JSON value, as `json.loads` gives |
| `IPCServer` | `IpcServer` | `new(handler, socket_path)` resolves through `make_transport`; `with_transport` for an explicit transport |
| `IPCServer.address` | `IpcServer::address` | |
| `IPCServer.start` | `IpcServer::start` | logs `ipc listening at <address>` |
| `IPCServer.serve_forever` | `IpcServer::serve_forever` | errors when not started (Python's assert); returns once stopped, then closes and waits like `async with server` |
| `IPCServer.stop` | `IpcServer::stop` | close, wait for in-flight connections, `cleanup_stale` |
| `IPCServer._on_conn` | `handle_conn` (pub, generic over the stream) | readline (64 KiB limit) → empty: no reply; undecodable: `{"ok": false, "error": "bad json: ..."}`; handler error: `{"ok": false, "error": "<Type>: <msg>"}` and `handler error for req=<repr>` logged |
| `IPCServer._reply` | private `reply` | `json.dumps(obj, ensure_ascii=False) + "\n"` via `dumps`; reset / broken pipe logged at debug `ipc: client hung up before reply was flushed` |
| `json.dumps(..., ensure_ascii=False)` | `dumps` | `", "`/`": "` separators, raw non-ASCII, Python float repr |
| `type(e).__name__` | `NamedError`, `error_type_name` | a handler names its Python class with `NamedError`; `SpecError`, `io::Error` and `serde_json::Error` map to their Python classes; anything else is `Exception` |
| `repr(...)` | `py_repr`, `py_str_repr`, `py_float_repr` | for log lines and messages that show `%r` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_ipc_transport.py` | `test_parse_tcp_loopback_spec`, `test_parse_tcp_empty_host_uses_loopback`, `test_parse_tcp_rejects_non_loopback_host`, `test_windows_default_is_tcp`, `test_windows_rejects_path_spec`, `test_make_transport_honors_env`, `test_posix_default_is_unix_socket`, `test_hook_post_returns_none_for_invalid_transport_spec` | same names in `src/tests.rs` (platform and env passed in, not patched) | `test_hook_post_returns_none_for_invalid_transport_spec`: only the half this crate owns (the spec is refused); `post` itself belongs to the hooks crate |
| `bridge/tests/test_ipc_chat.py` | `test_typed_words_reach_the_chat_as_the_owners`, `test_long_words_are_cut_to_a_telegram_message`, `test_nothing_is_sent_when_it_cannot_be` | none | They test `daemon.IPC_HANDLERS["chat"]`, which lives in `daemon.py`; the daemon crate lists the same file in its plan entry |
| `bridge/tests/test_ipc_dispatch.py` | `test_every_entry_is_the_matching_async_method`, `test_unknown_and_malformed_events_are_refused_as_before`, `test_dispatch_goes_through_the_class_so_a_stub_needs_only_what_the_handler_uses` | none | They test `Daemon._handle_ipc` and `IPC_HANDLERS` in `daemon.py`; ported with the daemon crate. `py_repr` here gives it the `unknown evt: 'nope'` text |
| `bridge/tests/test_eve_mcp.py` (ref) | `test_post_over_a_real_socket_and_its_two_failures` (server side) | `test_post_over_a_real_socket_and_its_two_failures` | `eve_mcp.post` and its exception types belong to the eve-mcp crate |
| `bridge/tests/test_cli_lite.py` (ref) | `serving` fixture (an `IPCServer` on a socket path) | `test_unix_socket_is_owner_only_and_replaces_a_stale_file`, `test_serve_forever_needs_start_and_ends_on_stop` | the CLI behaviour belongs to the cli / lite crates |
| (new) | | `test_spec_edge_cases_follow_the_python_branches`, `test_make_transport_order_is_spec_then_env_then_default`, `test_unix_transport_refused_on_windows`, `test_dumps_matches_python_json_dumps`, `test_py_float_repr`, `test_py_repr`, `test_utf8_errors_read_like_python`, `test_one_line_one_reply`, `test_a_line_without_newline_is_read_at_eof`, `test_an_empty_connection_gets_no_reply`, `test_bad_json_is_refused`, `test_a_non_object_request_still_reaches_the_handler`, `test_handler_faults_become_an_error_reply`, `test_a_line_over_the_reader_limit_gets_no_reply`, `test_is_in_use_removes_a_socket_nobody_accepts_on`, `test_stop_waits_for_a_request_in_flight`, `test_a_client_that_hangs_up_does_not_stop_the_server`, `test_tcp_loopback_round_trip` | |

## Boundaries
None

## Differences
- `bad json: ...` for malformed JSON carries serde_json's message (`expected value at line 1 column 1`), not Python's `JSONDecodeError` text (`Expecting value: line 1 column 1 (char 0)`). The UTF-8 decode case does match Python's `UnicodeDecodeError` text.
- `json.loads` accepts `NaN`/`Infinity`, lone surrogate escapes and integers past 64 bits; serde_json refuses the first two (the client gets `bad json`) and reads big integers as floats.
- A handler that panics is answered `{"ok": false, "error": "Exception: <panic message>"}`; Python has no panic. Error class names for errors that are not `NamedError`/`SpecError`/`io::Error`/`serde_json::Error` are reported as `Exception`.
- A request line over the 64 KiB limit, or a read error, is logged as `ipc: could not read the request: ...` instead of asyncio's `Unhandled exception in client_connected_cb`; in both the connection closes with no reply.
- `parse_spec` takes ASCII digits as a port; Python's `str.isdigit` also accepts other Unicode decimal digits.
- `IpcServer::serve_forever` returns `Ok(())` when the server is stopped; Python's raises `CancelledError` out of the cancelled `serve_forever`.
- `py_str_repr` treats control, format, separator and private-use characters as unprintable; Python also escapes unassigned code points. It only shapes log lines and error text.
