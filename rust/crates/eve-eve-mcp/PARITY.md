# eve-eve-mcp
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/eve_mcp.py` | `src/lib.rs` (tests in `src/tests.rs`); the `__main__` entry is the `eve-mcp` binary (`src/main.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `PROTOCOL_VERSIONS` | `PROTOCOL_VERSIONS` | same three, newest first |
| `SERVER_INFO` | `server_info()`, `SERVER_NAME`, `SERVER_VERSION` | `{"name": "eve", "version": "1"}` |
| `INSTRUCTIONS` | `INSTRUCTIONS` | verbatim |
| `DEFAULT_CALL_SECS`, `LIST_SECS`, `CONNECT_SECS`, `MAX_ARGS_BYTES` | same names | 420.0, 10.0, 3.0, 48000 |
| `NO_ANSWER`, `NO_OUTCOME`, `NO_DAEMON`, `REFUSED` | same names | verbatim |
| `outside_note` | `outside_note` | verbatim text |
| `class Unanswered` | `PostError::Unanswered` | `PostError::is_unanswered()` is `isinstance(e, Unanswered)`, true for `Lost` too, as `_tools_list`'s `except (Unreachable, Unanswered)` needs |
| `class Lost` | `PostError::Lost(String)` | the cause's Python class name, or `"no answer"` |
| `class Unreachable` | `PostError::Unreachable(String)` | the cause's Python class name |
| (exceptions Python lets propagate) | `PostError::Raised(String)` | `settimeout`'s `ValueError` / `OverflowError`, a `RuntimeError` transport, or what an injected `send` raises |
| `post` | `post(event, timeout, spec)` | `eve_ipc::make_transport(spec)` + `sync_connect(CONNECT_SECS)`; a refused spec (`ValueError`) or a failed connect is Unreachable; `settimeout(timeout)` on every read and write (an OSError from it propagates as `Raised`, since Python's `settimeout` sits outside the `except OSError`); a timeout is Unanswered; any other socket error is Lost once the request went out, Unreachable before; the first line must be a JSON object or it is Lost(`"no answer"`) |
| `json.loads` (on the daemon's line and on stdin) | `loads`, private `try_loads` | `eve_ipc::pyjson::loads` (CPython's scanner: its nesting depth, numbers, strictness), plus bare `NaN` / `Infinity` / `-Infinity` and float literals past f64 (`1e400`), read as `null`; a `ValueError` is a parse error (stdin) or Lost (the daemon's line); a `RecursionError` leaves `serve` (stdin) or `post` (`Raised("RecursionError")`), as Python's does |
| `mcp_tool` | `mcp_tool` | `parameters` when a non-empty object, else `{"type": "object", "properties": {}}`; `description` is `str()` of a truthy value, else `""` |
| `class Server` | `Server` | `Server::new(sid, token)` is `Server(sid, token)` (the real `post`, stdout); `Server::with(sid, token, send, out)` is the `send=` / `out=` seam; `SendFn` is the `send` type |
| `Server.write` | `Server::write` | `json.dumps(msg, ensure_ascii=False) + "\n"` (via `eve_ipc::dumps`), whole line under one lock, flushed |
| `Server._result` / `Server._error` | private `result` / `error` | |
| `Server.handle_line` | `Server::handle_line` | `str.strip()` (with Python's U+001C..U+001F), `-32700 "parse error"`, batches one by one |
| `Server.handle` | `Server::handle` | same validation order: an id-without-method message is dropped, otherwise `-32600 "invalid request"`; notifications dropped; `initialize`, `ping`, `tools/list`, `tools/call` (its own thread), else `-32601 "method not found: <m>"` |
| `Server._tools_list` | private `tools_list` | Unreachable / Unanswered / Lost → `{"tools": []}`; falsy `ok` → `-32001` with `str(error or REFUSED)` (a non-string error as Python's `repr`); `call_secs` taken when an int, float or bool above 0; tools that are objects with a string `name` |
| `Server.call` | `Server::call(name, args)` → `Result<Value, String>` | `Err` names the exception class Python would let out (answered as an internal error) |
| `Server._tools_call` | private `tools_call` | a raised class or a caught panic → `-32603 "internal error (<Class>)"` |
| `Server.serve` | `Server::serve(BufRead)` | universal newlines like Python's text-mode stdin; returns 0 at EOF, `Err` when a reply cannot be written |
| `Server._threads` | `Server::join_calls` | the tests' join over the call threads |
| `_text_result` | `text_result` | |
| `main` | `main()`, `main_with(env, input, make)` | `EVE_MCP_SESSION` / `EVE_MCP_TOKEN` stripped; either empty → the same stderr line and 2 before reading stdin |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_eve_mcp.py` | `test_initialize_ping_notifications_and_unknown_methods`, `test_bad_json_and_bad_requests_are_errors_not_crashes`, `test_tools_list_maps_each_function_tool_and_carries_the_token`, `test_a_refused_session_lists_nothing_and_says_why`, `test_a_call_returns_the_result_and_marks_outside_text`, `test_a_refused_call_is_an_error_result`, `test_a_call_the_daemon_never_answers_says_so`, `test_the_call_waits_as_long_as_the_daemon_said_and_huge_args_never_leave`, `test_a_ping_is_answered_while_a_call_waits_for_the_owner`, `test_post_over_a_real_socket_and_its_two_failures`, `test_a_connection_lost_after_the_request_went_out_is_unknown_not_unreachable`, `test_main_needs_the_session_and_token`, `test_a_client_timeout_never_says_the_call_did_not_run` | same names in `src/tests.rs` (the parametrized ones loop over their cases) | |
| `bridge/tests/test_eve_mcp.py` | `test_the_cli_runs_the_server_before_reading_the_env_file` | none | tests `cli.main(["mcp"])`, which belongs to the cli crate |
| `bridge/tests/test_eve_mcp.py` | `test_a_session_gets_exactly_the_brains_tools_less_the_hidden_ones`, `test_no_tool_a_session_gets_names_a_hidden_one`, `test_a_tool_that_comes_for_the_chat_comes_for_the_session`, `test_a_name_the_session_was_not_offered_is_refused_and_nothing_runs`, `test_a_deletion_asks_the_owner_on_the_phone_and_waits`, `test_recording_asks_because_a_session_counts_as_having_read_outside_text`, `test_a_question_nobody_answers_is_the_owner_did_not_answer`, `test_a_second_question_while_one_waits_asks_nothing`, `test_an_apps_send_still_asks_with_the_whole_email`, `test_a_read_is_marked_with_its_source`, `test_no_owner_chat_means_nothing_runs` | none | they test `TelegramInlet.session_tools` / `session_call` in `telegram.py`; they belong to the telegram crate |
| `bridge/tests/test_eve_mcp.py` | `test_the_daemon_answers_only_a_live_sessions_token`, `test_the_daemon_bounds_a_long_call_and_never_cancels_it`, `test_the_daemon_refuses_while_no_chat_runs`, `test_the_hooks_reach_the_work_registry` | none | they test `daemon.IPC_HANDLERS["mcp_tools" / "mcp_call"]` and `_work_note` in `daemon.py`; they belong to the daemon crate |
| (new) | | `test_args_are_measured_as_python_writes_them`, `test_post_reads_only_the_first_line_and_needs_an_object`, `test_a_wait_python_cannot_hold_is_an_internal_error_and_a_long_one_still_works`, `test_main_serves_the_session_it_names`, `test_the_constants_are_pythons`, `test_batches_responses_and_odd_ids`, `test_serve_reads_lines_as_pythons_text_stdin_splits_them`, `test_tools_list_edges`, `test_call_edges`, `test_a_reply_that_cannot_be_written_ends_serve`, `test_loads_reads_what_python_reads`, `test_loads_reads_nesting_and_numbers_as_cpython_does`, `test_deep_arguments_and_results_go_through_and_a_recursion_error_is_pythons` | |

## Boundaries
None. The daemon is reached over `eve-ipc`'s socket, as in Python.

## Differences
- The binary is `eve-mcp` (`eve-mcp` with `EVE_MCP_SESSION` / `EVE_MCP_TOKEN` set), where Python is started as `python -P -m eve_bridge.eve_mcp`; work_sessions' config names whichever server it starts. Arguments are ignored, as Python's `main(argv)` ignores them.
- A bare `NaN` / `Infinity` / `-Infinity` or a float literal beyond f64 (`1e400`), in the daemon's answer or on stdin, is read as `null`, so it is written back as `null` where Python writes `NaN` / `Infinity`. A lone surrogate escape (`"\ud800"`) becomes U+FFFD (as `eve_ipc::pyjson` reads it), so arguments holding one are sent where Python answers "the arguments are not JSON"; an integer beyond 64 bits is read as a float. Nesting past `eve_ipc::pyjson::MAX_DEPTH` (1,000) is a `RecursionError`, where CPython's limit is near 10,000.
- Input lines that are not valid UTF-8 are decoded lossily (and so answer a parse error or an invalid request); Python's stdin raises `UnicodeDecodeError` and the server exits.
- The socket timeout is SO_RCVTIMEO / SO_SNDTIMEO per read and write, capped at a century (a longer wait is the same wait); Python's `sendall` counts its timeout over the whole send. A request is at most about 48 kB and the daemon reads it at once, so the two agree in practice. A wait Python's `settimeout` refuses (NaN, negative, past about 292 years) is refused the same way, by class name.
- A panic inside a call is answered as `internal error (Exception)`; Python names the exception's class.
- A reply that cannot be written to stdout ends the server with 1 and one stderr line, where Python dies with a traceback (exit 1). A failed write from a call thread is answered (if it can be) with `internal error (<OSError class>)` and otherwise dropped, as Python's thread dies.
