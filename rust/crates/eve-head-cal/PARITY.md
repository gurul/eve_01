# eve-head-cal
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/head_cal.py` | `src/lib.rs` (tests in `src/tests.rs`); bin `eve-head-cal` (`src/bin/head_cal.rs`) is `python -m eve_bridge.head_cal` |

## API
| Python | Rust | Notes |
|---|---|---|
| `HOLD_MS` | `HOLD_MS` | `6000` |
| `POSES` | `POSES` (+ `pose(name)`) | same names, (yaw, pitch) values and order |
| `Ipc` | `Ipc<'a>` (`dyn FnMut(Value) -> Option<Value>`), `SharedIpc` for the server threads | |
| `pose_keys` | `pose_keys(name) -> Option<Vec<[i64; 4]>>` | `None` where Python raises `KeyError` |
| `handle` | `handle(path, body, ipc) -> Result<(u16, Value), HandleError>` | same routes, codes and JSON bodies; `str(body.get("pose") or "")` by Python truthiness and `str()`; the error is `unknown pose <repr>`; flips are `bool()` of what was sent. `Err(ReplyNotADict)` is the `AttributeError` Python raises when the daemon answers a move with JSON that is not an object |
| `PAGE` | `PAGE` | verbatim; a test compares it with the Python source |
| `allowed` | `allowed(&Headers, port)` | Host must be `127.0.0.1:<port>`; Origin, when sent, `http://127.0.0.1:<port>`. `Headers` looks names up without case, first match wins, as `HTTPMessage.get` does |
| `serve` | `serve(port, socket_path, open_browser) -> anyhow::Result<i32>` (+ `daemon_ipc`, `run`, `handle_connection`, `open_browser`) | binds 127.0.0.1:port (SO_REUSEADDR, as `HTTPServer`), prints `head calibration at http://127.0.0.1:<port>/ (Ctrl-C to stop)`, opens the browser 0.3 s later, a thread per connection, returns 0 on Ctrl-C. The daemon is reached through `eve_hooks::client::post(req, socket_path, timeout=10.0, connect 3.0)` |
| `serve.Handler.do_GET` / `do_POST` / `_send` / `log_message` | `handle_connection` | `BaseHTTPRequestHandler`'s HTTP/1.0 behaviour: one request per connection; replies carry Server, Date, Content-Type, `Cache-Control: no-store` and no Content-Length; 403 body `{"ok": false, "error": "local access only"}`; POST body `json.loads(bytes)` with its encoding detection, any `ValueError` or non-object → `{}`; replies `json.dumps` with `ensure_ascii` (`dumps_ascii`); request-line, header and error-page handling (400/414/431/501/505, HTTP/0.9 body-only replies, `//` path reduction) as Python 3.13's `http.server`. No request logging, as `log_message` is silenced |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_head_cal.py` | `test_each_extreme_holds_a_pose_inside_the_boards_limits`, `test_a_tap_moves_the_head_and_a_bad_pose_is_refused`, `test_a_flip_goes_to_the_board_and_a_read_sends_nothing_to_set`, `test_no_daemon_is_said_plainly`, `test_only_the_page_itself_may_call_the_api` | same names in `src/tests.rs`, plus `page_is_the_python_page_verbatim`, `move_body_values_follow_python_truthiness_and_str`, `a_move_reply_that_is_not_an_object_is_pythons_attribute_error`, `json_replies_are_ascii_like_python_json_dumps`, `get_serves_the_page_to_this_host_only`, `post_drives_the_api_and_another_origin_is_refused`, `a_bad_body_is_an_empty_one_and_a_bad_length_is_a_crash`, `a_move_reply_that_is_not_an_object_closes_without_a_reply`, `other_methods_and_bad_requests_get_pythons_error_pages`, `header_parsing_follows_the_email_parser`, `content_length_is_read_like_python_int`, `the_server_answers_over_tcp_until_shut_down`, `the_daemon_is_reached_over_its_socket` | none |

The HTTP side was also checked against the live Python server (Python 3.13.14, the daemon's venv): 26 raw
requests (page, refused hosts, moves, bad JSON, bad Content-Length, PUT/HEAD/odd methods, HTTP/0.9, bad and
2.0 versions, malformed and oversized request lines and headers) gave byte-identical replies once the
Server and Date values were masked.

## Boundaries
- `webbrowser.open` is the system opener (`open` on macOS, `xdg-open` elsewhere), spawned best effort.

## Differences
- The `Server` header reads `BaseHTTP/0.6 Rust` instead of `BaseHTTP/0.6 Python/<version>`.
- An uncaught handler error (non-object move reply, bad Content-Length, JSON nested too deep) prints a short
  `socketserver`-style block with the exception line instead of a full Python traceback; the connection
  closes with no reply in both.
- A lone-surrogate `\uD8xx` escape in a POST body reads as `?` (Rust strings cannot hold one; the same rule
  as `eve_hooks::client::loads`). A UTF-8 body carrying raw surrogate bytes (`surrogatepass`) is treated as
  undecodable, so `{}`.
