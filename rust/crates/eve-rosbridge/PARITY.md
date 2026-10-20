# eve-rosbridge
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/rosbridge.py` | `src/lib.rs` (Python text forms in `src/pyfmt.rs`, tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_HOST`, `DEFAULT_PORT`, `DEFAULT_QUEUE_SIZE` | same consts | `127.0.0.1`, `9090`, `64` |
| `NO_BROWSER_ORIGINS` | `NO_BROWSER_ORIGINS: &[Option<&str>]` | `[None]`: only a handshake without an Origin header is accepted |
| `_Sub` | private `struct Sub` | topic, sub_id, bus subscription; `throttle_secs` and `last_sent` (starts at `-inf`) live in the bus callback's closure |
| `_Client` | private `struct Client` | drop-oldest `VecDeque` queue + `Notify` (Python's `asyncio.Queue`), subs, advertised set, writer task, service tasks, `dropped`, `warned`; `name` is `host:port` of the peer or `client` |
| `RosbridgeServer` | `RosbridgeServer` (`Clone`, one shared `Arc`) | |
| `RosbridgeServer.__init__(bus, host, port, queue_size, clock)` | `new(bus, host, port, queue_size)`, `with_defaults(bus)`, `with_clock(.., clock)` | default clock is `monotonic()` (seconds); `queue_size` 0 is unbounded, as `asyncio.Queue(0)` |
| `RosbridgeServer.origins` | `origins()` | |
| `RosbridgeServer.host` / `port` / `queue_size` | `host()` / `port()` / `queue_size()` | `port()` is the OS-given port after `start()` with port 0 |
| `RosbridgeServer.stats` | `stats() -> Stats` (`as_map()` gives the dict in its key order) | `sent`, `dropped`, `clients_total` |
| `RosbridgeServer._clients` | `client_count()` | |
| `RosbridgeServer.start` | `start() -> io::Result<()>` | binds every address the host resolves to (as `create_server`), a second call is a no-op, logs `rosbridge: serving eve's memory at <url>` |
| `RosbridgeServer.stop` | `stop()` | closes every client, stops listening, waits up to 10 s (websockets' close timeout) for connections, logs the same summary line |
| `RosbridgeServer.url` | `url()` | `ws://{host}:{port}` |
| `RosbridgeServer._handle` | private `handle_conn` + `serve_client` + `read_loop` | the websockets handshake (Origin check, 10 s open timeout) then the receive loop |
| `RosbridgeServer._close_client` | private `close_client` | idempotent; unsubscribes from the bus, cancels service calls and the writer, sends close 1000 |
| `RosbridgeServer._write_loop` | private `write_loop` | counts `sent` after each successful send; ends on a send error |
| `RosbridgeServer._enqueue` | private `enqueue` | `json.dumps(ensure_ascii=False)` text; full queue drops the oldest, counts it, warns once per client with the same text |
| `RosbridgeServer._status` | private `status` | `{"op": "status", "level", "msg", ["id"]}` |
| `RosbridgeServer._on_message` | private `on_message` | same error texts; `id` kept when str or int (a bool shows as `True`/`False`); unknown op shows `repr(op)` |
| `RosbridgeServer._known_topic` | private `known_topic` | |
| `RosbridgeServer._op_subscribe` | private `op_subscribe` | `throttle_rate` ms → seconds, never negative; subscribes even to an unknown topic and then says so |
| `RosbridgeServer._op_unsubscribe` | private `op_unsubscribe` | without an id removes every subscription on the topic |
| `RosbridgeServer._op_advertise` / `_op_unadvertise` / `_op_set_level` | private `op_advertise` / `op_unadvertise` / `set_level` arm | silent; an unhashable (list/dict) unadvertise topic ends the connection as the Python `TypeError` does |
| `RosbridgeServer._op_publish` | private `op_publish` | only catalogue topics, only object msgs; `repr(topic)` in the error |
| `RosbridgeServer._op_call_service` | private `op_call_service` | non-object args become `{}`; the call is a task owned by the client |
| `RosbridgeServer._call` | private `call` | `service_response` with `result` and `values`; `no such service`; a failure logs `rosbridge: service %s failed: %s: %s` and answers `{"error": "<Type>: <text>"}` |
| (websockets `serve` options) | `PING_INTERVAL`, `PING_TIMEOUT`, `MAX_SIZE`, `OPEN_TIMEOUT`, `CLOSE_TIMEOUT`, `keepalive_loop`, `check_origin`, `reject`, `py_sockname` | 20 s ping / 20 s pong timeout (close 1011 `keepalive ping timeout`; the next ping waits 20 s minus the last latency, as websockets does), websockets' log lines (`server listening on ('127.0.0.1', 9090)`, `connection open`, `connection rejected (403 Forbidden)`, `opening handshake failed` at error, nothing on a handshake timeout, `server closing`, `server closed`), 1 MiB messages (close 1009 `over size limit (N > 1048576 bytes)`), the 403/400 rejection bodies `Failed to open a WebSocket connection: invalid Origin header: X.\n` with websockets' headers |
| (`json.dumps`, `repr`) | `py_json`, `py_repr`, `py_str_repr`, `py_float_repr` | the outbound wire text and the status-message quoting; `repr()` escapes exactly the code points Python 3.13's `str.isprintable()` rejects (a 713-range table generated from `unicodedata`, Unicode 15.1) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `tests/test_rosbridge.py` | all 11 | same names in `src/tests.rs`; `test_the_reference_client_roslibpy_receives_a_message` drives roslibpy in a `python3` subprocess and is `#[ignore = "live: ..."]` (Python skips it when roslibpy is absent); verified passing with roslibpy 2.1.0 in a scratch venv (`PATH=<venv>/bin:$PATH cargo test -p eve-rosbridge -- --ignored`) | none |
| (extra) | | `an_empty_origin_is_refused_and_two_origins_are_a_bad_request`, `outbound_frames_are_python_json_dumps`, `ids_and_ops_are_shown_as_python_does`, `a_known_prefix_subscription_gets_no_status_and_unsubscribe_without_id_removes_all`, `unadvertise_of_a_list_ends_the_connection_like_a_python_raise`, `a_message_over_one_mebibyte_closes_with_1009`, `a_panicking_service_answers_with_an_error_and_keyerror_is_shown_like_python`, `closing_a_client_cancels_its_service_calls`, `an_unbounded_queue_never_drops_and_the_warning_is_logged_once`, `start_twice_is_a_no_op_port_zero_records_the_port_and_stop_twice_is_safe`, `a_busy_port_fails_to_start_with_an_os_error`, `throttle_uses_the_injected_clock`, `python_text_forms`, `a_client_that_never_answers_pings_is_closed_with_1011_and_one_that_reads_stays`, `socket_names_are_shown_as_python_getsockname` | |

## Boundaries
None

## Differences
- A failed service shows `<Type>: <text>` where Python shows `type(e).__name__`. Rust errors carry no
  Python class name, so the type is `KeyError` for a missing service, `TimeoutError`, `JSONDecodeError`
  or `OSError` for those error types, and `Error` otherwise (a panicking handler included, which
  Python cannot have).
- A port outside 0-65535 makes `start()` return an `io::Error` (`bind(): port must be 0-65535.`)
  where Python raises `OverflowError`, which the daemon's `except OSError` would not catch.
- A request that is not a WebSocket upgrade is dropped by tungstenite without the 426/400 plain-text
  body websockets sends; the Origin rejections (403/400) are byte-for-byte. Responses carry no
  `Server: Python/... websockets/...` header.
- `json.loads` accepts `NaN`, `Infinity`, numbers beyond f64 and UTF-16/32 byte frames; those are
  "message is not valid JSON" here. Integers beyond 64 bits are re-sent as floats.
- `json.loads` turns `-0` into the int `0` (re-sent as `0`, and a usable `id`); serde_json reads it
  as the float `-0.0`, so it is re-sent as `-0.0` and is not taken as an `id`.
- `json.loads` accepts a lone surrogate escape (`"\ud800"`); here that message is "message is not
  valid JSON". (In Python the echo of such a string would then fail to encode in the writer.)
- websockets negotiates permessage-deflate when the client offers it; tungstenite does not, so
  frames go uncompressed. The extension is optional, so every client still connects.
- A Python service may return a non-dict, which is wrapped as `{"value": ...}`; a Rust bus handler
  returns an object by type, so that branch cannot occur.
- A subscribe that arrives while the client is being closed is ignored, so no bus subscription
  can outlive its client (Python could leak one in that race).
