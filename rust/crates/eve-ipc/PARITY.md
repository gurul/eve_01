# eve-ipc
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/ipc.py` | `src/lib.rs`, plus `src/pyjson.rs` for the `json.loads` it calls (tests in `src/tests.rs`) |

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
| `Transport.start_server` | `Transport::start_server` → `Server` | `Server` is `asyncio.Server`: `close` (and, as Python 3.13's `cleanup_socket`, removes the Unix socket file if its inode is still the one bound), `wait_closed` (waits for accepted connections, as Python 3.12+), `serve_forever` (refuses a closed server or a second caller, like asyncio's RuntimeErrors), `addresses`, `is_serving`; backlog 100 like asyncio |
| `Transport.sync_connect` | `Transport::sync_connect(Duration)` → `SyncStream` | connect capped by the timeout, then the same timeout on every read and write (`settimeout`); a zero timeout is non-blocking, as `settimeout(0.0)`; `SyncStream::set_timeout` is the client's later `s.settimeout(timeout)` |
| `UnixTransport` | `UnixTransport` | `new` raises the same RuntimeError text on Windows (`SpecError::UnixUnsupported`); `new_on(path, Platform)` for tests |
| `UnixTransport.address` | `UnixTransport::address` | the path |
| `UnixTransport.start_server` | private `UnixTransport::bind`, used by `Transport::start_server` | unlinks an existing file, binds, `chmod 0o600` (errors ignored); EADDRINUSE reads `[Errno 48] Address '<path>' is already in use`, the `str()` of asyncio's OSError |
| `UnixTransport.sync_connect` | `UnixTransport::sync_connect` | `socket2` connect with timeout |
| `UnixTransport.is_in_use` | `UnixTransport::is_in_use` | missing → false; refused / not found → removes the stale file, false; any other error or success → true; 0.5 s |
| `UnixTransport.cleanup_stale` | `UnixTransport::cleanup_stale` | unlink if present, errors ignored |
| `TcpLoopbackTransport` | `TcpLoopbackTransport` | `new(host, port)`: empty host → `127.0.0.1`; host must be `127.0.0.1` or `localhost`, same ValueError text |
| `TcpLoopbackTransport._ALLOWED_HOSTS` | private `ALLOWED_HOSTS` | same two hosts |
| `TcpLoopbackTransport.address` | `TcpLoopbackTransport::address` | `host:port` |
| `TcpLoopbackTransport.start_server` | private `TcpLoopbackTransport::bind` | binds every resolved address like `asyncio.start_server` (`localhost` → 127.0.0.1 and ::1), SO_REUSEADDR off Windows, IPV6_V6ONLY, skips EADDRNOTAVAIL; errors read as asyncio's: `[Errno N] error while attempting to bind on address ('127.0.0.1', P): [errno n] <strerror lowercased>` and `could not bind on any address out of [...]` |
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
| `IPCServer.serve_forever` | `IpcServer::serve_forever` | errors when not started (Python's assert) or when the server is closed; returns once stopped, then closes and waits like `async with server` |
| `IPCServer.stop` | `IpcServer::stop` | close, wait for in-flight connections, `cleanup_stale` |
| `IPCServer._on_conn` | `handle_conn` (pub, generic over the stream) | readline (64 KiB limit) → empty: no reply; undecodable: `{"ok": false, "error": "bad json: ..."}` with CPython's `UnicodeDecodeError` / `JSONDecodeError` text; nesting past `pyjson::MAX_DEPTH` (Python's `RecursionError`), an over-long line or a read error: no reply, logged `Unhandled exception in client_connected_cb: <Type>: <msg>`; handler error or panic (in the call or the future): `{"ok": false, "error": "<Type>: <msg>"}` and `handler error for req=<repr>` logged |
| `IPCServer._reply` | private `reply` | `json.dumps(obj, ensure_ascii=False) + "\n"` via `dumps`; reset / broken pipe logged at debug `ipc: client hung up before reply was flushed`; any other write error logged as unhandled, as asyncio does |
| `json.loads(line.decode("utf-8"))` | private `decode_request`, `pyjson::loads` | an iterative port of CPython 3.13's C scanner: same messages, character positions and `line L column C (char P)` format, BOM refusal, `Extra data`, `Illegal trailing comma`, surrogate-pair joining, the 4300-digit int limit; checked against 4,479 cases recorded from CPython (`tests/data/pyjson_cases.jsonl`, written by `tests/data/gen_pyjson_cases.py`) |
| `json.dumps(..., ensure_ascii=False)` | `dumps` | `", "`/`": "` separators, raw non-ASCII, Python float repr |
| `type(e).__name__` / `str(e)` | `NamedError`, `error_type_name`, `io_error_type_name`, `error_message`, `py_os_error_str` | a handler names its Python class with `NamedError`; `SpecError`, `LoadsError`, `io::Error` (by errno, as `OSError(errno, ...)` picks the subclass) and `serde_json::Error` (`JSONDecodeError` for syntax) map to their Python classes; anything else is `Exception`. An OS error's text is `[Errno N] <strerror>` |
| `repr(...)` | `py_repr`, `py_str_repr`, `py_float_repr` | for log lines and messages that show `%r` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_ipc_transport.py` | `test_parse_tcp_loopback_spec`, `test_parse_tcp_empty_host_uses_loopback`, `test_parse_tcp_rejects_non_loopback_host`, `test_windows_default_is_tcp`, `test_windows_rejects_path_spec`, `test_make_transport_honors_env`, `test_posix_default_is_unix_socket`, `test_hook_post_returns_none_for_invalid_transport_spec` | same names in `src/tests.rs` (platform and env passed in, not patched) | `test_hook_post_returns_none_for_invalid_transport_spec`: only the half this crate owns (the spec is refused); `post` itself belongs to the hooks crate |
| `bridge/tests/test_ipc_chat.py` | `test_typed_words_reach_the_chat_as_the_owners`, `test_long_words_are_cut_to_a_telegram_message`, `test_nothing_is_sent_when_it_cannot_be` | none | They test `daemon.IPC_HANDLERS["chat"]`, which lives in `daemon.py`; the daemon crate lists the same file in its plan entry |
| `bridge/tests/test_ipc_dispatch.py` | `test_every_entry_is_the_matching_async_method`, `test_unknown_and_malformed_events_are_refused_as_before`, `test_dispatch_goes_through_the_class_so_a_stub_needs_only_what_the_handler_uses` | none | They test `Daemon._handle_ipc` and `IPC_HANDLERS` in `daemon.py`; ported with the daemon crate. `py_repr` here gives it the `unknown evt: 'nope'` text |
| `bridge/tests/test_eve_mcp.py` (ref) | `test_post_over_a_real_socket_and_its_two_failures` (server side) | `test_post_over_a_real_socket_and_its_two_failures` | `eve_mcp.post` and its exception types belong to the eve-mcp crate |
| `bridge/tests/test_cli_lite.py` (ref) | `serving` fixture (an `IPCServer` on a socket path) | `test_unix_socket_is_owner_only_and_replaces_a_stale_file`, `test_serve_forever_needs_start_and_ends_on_stop`, `test_close_removes_only_the_socket_it_bound` | the CLI behaviour belongs to the cli / lite crates |
| (new) | | `test_spec_edge_cases_follow_the_python_branches`, `test_make_transport_order_is_spec_then_env_then_default`, `test_unix_transport_refused_on_windows`, `test_dumps_matches_python_json_dumps`, `test_py_float_repr`, `test_py_repr`, `test_utf8_errors_read_like_python`, `test_one_line_one_reply`, `test_a_line_without_newline_is_read_at_eof`, `test_an_empty_connection_gets_no_reply`, `test_bad_json_is_refused`, `test_a_non_object_request_still_reaches_the_handler`, `test_handler_faults_become_an_error_reply`, `test_a_line_over_the_reader_limit_gets_no_reply`, `test_is_in_use_removes_a_socket_nobody_accepts_on`, `test_stop_waits_for_a_request_in_flight`, `test_a_client_that_hangs_up_does_not_stop_the_server`, `test_tcp_loopback_round_trip`, `test_loads_matches_cpython_on_recorded_cases`, `test_bad_json_reply_reads_like_python`, `test_loads_keeps_what_python_keeps`, `test_deep_nesting_is_a_recursion_error_with_no_reply`, `test_a_request_nested_too_deep_gets_no_reply`, `test_error_type_and_text_follow_python`, `test_tcp_bind_conflict_reads_like_asyncio` | |

## Boundaries
None

## Differences
- A `serde_json::Value` cannot hold everything `json.loads` returns. A lone surrogate escape (`"\ud800"`) becomes U+FFFD; Python keeps the surrogate. An integer past 64 bits becomes the nearest float, as serde_json does. `NaN`, `Infinity`, `-Infinity`, a float literal that overflows to infinity, and an integer too large for a float are refused once the rest of the line has parsed (so a syntax error elsewhere still reads as Python's): `bad json: Out of range float values are not JSON compliant: nan` (Python's own text for that value, from `json.dumps(allow_nan=False)`); Python accepts them.
- Nesting: CPython 3.13 raises `RecursionError` near 9,994 levels inside the daemon's event loop; the port raises it at 1,001 (`pyjson::MAX_DEPTH`), because a deeper `Value` would overflow a 2 MiB task stack when cloned, printed or dropped. Both send no reply.
- A handler that panics is answered `{"ok": false, "error": "Exception: <panic message>"}`; Python has no panic. Error class names for errors that are not `NamedError`/`SpecError`/`LoadsError`/`io::Error`/`serde_json::Error` are reported as `Exception`.
- An unhandled connection error (over-long line, read or write error, `RecursionError`) is logged as one line, `Unhandled exception in client_connected_cb: <Type>: <msg>`; asyncio logs the same first words with the transport and a traceback.
- `parse_spec` takes ASCII digits as a port; Python's `str.isdigit` also accepts other Unicode digits (and then `int()` either reads them or raises).
- `IpcServer::serve_forever` returns `Ok(())` when the server is stopped; Python's raises `CancelledError` out of the cancelled `serve_forever`. Its RuntimeError texts are `server is closed` / `server is already being awaited on serve_forever()`, without asyncio's `<Server sockets=(...)>` repr.
- `Server::close` stops the accept tasks; their listening sockets close when the runtime drops them, a moment later, where asyncio closes them inside `close()`. The Unix socket file is removed inside `close()`, so a new client sees `ENOENT` at once, as with Python 3.13.
- `py_str_repr` treats control, format, separator and private-use characters as unprintable; Python also escapes unassigned code points. It only shapes log lines and error text.
