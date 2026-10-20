# eve-hooks
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/hooks/__init__.py` | `src/lib.rs` (shared Python-semantics helpers, `run_main`; tests in `src/tests.rs`) |
| `bridge/src/eve_bridge/hooks/_client.py` | `src/client.rs` |
| `bridge/src/eve_bridge/hooks/notification.py` | `src/notification.rs`, bin `eve-hook-notification` (`src/bin/notification.rs`) |
| `bridge/src/eve_bridge/hooks/permission_request.py` | `src/permission_request.rs`, bin `eve-hook-permission-request` |
| `bridge/src/eve_bridge/hooks/posttooluse.py` | `src/posttooluse.rs`, bin `eve-hook-posttooluse` |
| `bridge/src/eve_bridge/hooks/pretooluse.py` | `src/pretooluse.rs`, bin `eve-hook-pretooluse` |
| `bridge/src/eve_bridge/hooks/session_end.py` | `src/session_end.rs`, bin `eve-hook-session-end` |
| `bridge/src/eve_bridge/hooks/session_start.py` | `src/session_start.rs`, bin `eve-hook-session-start` |
| `bridge/src/eve_bridge/hooks/stop.py` | `src/stop.rs`, bin `eve-hook-stop` |
| `bridge/src/eve_bridge/hooks/user_prompt_submit.py` | `src/user_prompt_submit.rs`, bin `eve-hook-user-prompt-submit` |

Each binary is `python -m eve_bridge.hooks.<name>`: JSON on stdin, the daemon over the eve-ipc transport
(`EVE_BRIDGE_SOCK`, default `/tmp/eve-bridge.sock`), a decision line on stdout, exit 0. The installer port
decides which command it writes into Claude Code's settings.json.

## API
| Python | Rust | Notes |
|---|---|---|
| `_client.DEFAULT_TIMEOUT_SECS` | `client::DEFAULT_TIMEOUT_SECS` | `3.0` |
| `_client._clean` | `client::replace_lone_surrogates` (inside `client::loads`) | Python's `json.loads` keeps a lone-surrogate escape and `_clean` turns it into `?`; Rust strings cannot hold one, so each lone `\uD800`–`\uDFFF` escape inside a JSON string becomes a literal `?` before parsing. Real pairs are left for the parser |
| `_client.read_hook_input` | `client::read_hook_input()` / `client::read_hook_input_from(&[u8])` | empty → `{}`; UTF-8 with errors replaced; a `ValueError` from `json.loads` → `{}`; nesting past the recursion limit is `RecursionError`, which Python does not catch, so it is returned as `Err(HookError)` (traceback, exit 1); any other JSON value is returned as is, as `json.loads` gives it |
| `_client.post` | `client::post(event, socket_path, timeout, connect_timeout)` | `make_transport(socket_path)`, connect capped by `connect_timeout`, then `timeout` on every read and write; sends `json.dumps(event, ensure_ascii=False) + "\n"` (`eve_ipc::dumps`); reads 4096-byte chunks until a newline or EOF; first line parsed; `None` on any transport error, timeout, invalid spec, negative timeout, empty line, bad UTF-8 or bad JSON. Python's keyword defaults are explicit arguments (both `DEFAULT_TIMEOUT_SECS`) |
| `json.loads` | `client::loads` | `eve_ipc::pyjson::loads` (the port of CPython's scanner: same accepted syntax, BOM refusal, 4300-digit int limit, `RecursionError` on deep nesting) after the lone-surrogate rule above |
| `notification.main` | `notification::main` / `notification::run` | `{"evt": "notification", session_id, message, notification_type}`; `notification_type` is `type or notification_type or ""` by Python truthiness |
| `permission_request.DENY_MESSAGE` | `permission_request::DENY_MESSAGE` | verbatim |
| `permission_request.main` | `permission_request::main` / `permission_request::run` | posts `permissionrequest` with `BLOCK_TIMEOUT_SECS`; `allow` / `deny` print the `PermissionRequest` decision; anything else prints nothing |
| `posttooluse.main` | `posttooluse::main` / `posttooluse::run` | `{"evt": "posttooluse", session_id, tool_use_id, tool_name}` |
| `pretooluse.BLOCK_TIMEOUT_SECS` | `pretooluse::BLOCK_TIMEOUT_SECS` | `320.0` |
| `pretooluse._question` | `pretooluse::question` | `str(x or "").strip()` via `py_str`/`py_strip`/`py_truthy`; options iterate like Python (list items, str chars, dict keys; a truthy number or bool raises `TypeError: '<t>' object is not iterable`) |
| `pretooluse._summarize` | `pretooluse::summarize` | question line, then `command`/`file_path`/`path`/`url`, then the first non-empty string value; a str input as is; else `""` |
| `pretooluse.main` | `pretooluse::main` / `pretooluse::run` | posts `pretooluse` with `BLOCK_TIMEOUT_SECS`; `allow`/`deny`/`ask` print `hookSpecificOutput` with the reason, byte for byte `json.dumps(out) + "\n"` |
| `pretooluse._decision_reason` | `pretooluse::decision_reason` | three strings verbatim |
| `session_end.main` | `session_end::main` / `session_end::run` | `{"evt": "session_end", session_id}` |
| `session_start.main` | `session_start::main` / `session_start::run` | `{"evt": "session_start", session_id, transcript_path, cwd}` |
| `stop.main` | `stop::main` / `stop::run` | `{"evt": "turn_end", session_id, transcript_path}` |
| `user_prompt_submit.main` | `user_prompt_submit::main` / `user_prompt_submit::run` | `{"evt": "turn_begin", session_id, prompt}`; `prompt` is `(prompt or "").strip()` with Python's whitespace set |
| `payload.get(k, "")` | `payload_str`, `payload_get` | a present value (null, number, …) is passed through unchanged; a payload that is not a dict raises `AttributeError: '<t>' object has no attribute 'get'` |
| uncaught exception → traceback, exit 1 | `HookError`, `run_main` | `run` returns `Err(HookError)` where Python would raise; `run_main` prints `Traceback (most recent call last):` and `<Type>: <msg>` to stderr and exits 1 |
| `monkeypatch` of `post` / `read_hook_input` / `sys.stdout` | `run(raw, &mut Poster, &mut dyn Write)` | `Poster` is `FnMut(event, timeout) -> Option<Value>` |
| truthiness, `str()`, `str.strip()`, `type().__name__` | `py_truthy`, `py_str`, `py_strip`, `py_type_name` | `py_str` uses `eve_ipc::py_repr` for non-strings; `py_strip` adds U+001C..U+001F to Rust's whitespace set |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| (no primary test file in plan.json) | | `test_read_hook_input_empty_and_garbage_give_an_empty_dict`, `test_read_hook_input_replaces_bad_utf8_and_lone_surrogates`, `test_py_helpers_match_python`, `test_post_sends_one_python_json_line_and_reads_one_reply`, `test_post_reads_a_reply_ended_by_eof`, `test_post_returns_none_for_empty_bad_or_slow_replies`, `test_post_returns_none_when_nobody_listens`, `test_post_over_tcp_loopback`, `test_pretooluse_event_and_blocking_timeout`, `test_pretooluse_writes_each_decision_with_its_reason`, `test_pretooluse_defers_without_a_usable_decision`, `test_a_reply_that_is_not_a_dict_fails_like_python`, `test_summarize_picks_the_hint_like_python`, `test_question_follows_pythons_str_and_or`, `test_permission_request_event_and_deny_text`, `test_each_notice_hook_sends_its_event`, `test_missing_fields_default_to_empty_and_present_values_pass_through`, `test_a_payload_that_is_not_a_dict_fails_like_python`, `test_client_module_constants`, `test_read_hook_input_parses_like_cpython_json_loads`, `test_post_with_a_zero_timeout_does_not_block` | |
| `bridge/tests/test_ipc_transport.py` (ref) | `test_hook_post_returns_none_for_invalid_transport_spec` | same name | the other tests there belong to eve-ipc |
| `bridge/tests/test_relay_questions.py` (ref) | `test_permission_hook_output_is_the_shape_claude_code_validates` | same name | `test_installer_registers_the_question_and_every_dialog` belongs to the installer crate; the daemon tests belong to the daemon crate |
| `bridge/tests/test_telegram.py` (ref) | `test_a_question_for_the_owner_is_streamed_as_a_question` (the `_summarize` half) | same name | the relay half tests telegram.py |

## Boundaries
None

## Differences
- Python's `json.loads` accepts `NaN`, `Infinity` and `-Infinity`; `eve_ipc::pyjson` refuses them (a `Value` cannot hold them), so such a payload reads as `{}` here. Claude Code writes its payload with `JSON.stringify`, which never emits them. Integers past 64 bits are read as floats.
- Nesting: CPython raises `RecursionError` near 9,990 levels, the port at 1,001 (`eve_ipc::pyjson::MAX_DEPTH`). Past the limit a hook payload ends the hook with the traceback and exit 1 in both. In `post`, Python lets a reply's `RecursionError` escape; here such a reply is `None` (the daemon's replies are flat dicts).
- The lone-surrogate replacement is also applied to dict keys; `_clean` only cleaned values. Keys are never sent or printed by any hook, so only a collision between a cleaned key and a literal `?` key could differ.
- A reply with a lone-surrogate escape is read with `?` in its place; Python kept the surrogate. Only exact `"allow"`/`"deny"`/`"ask"` are acted on, so the outcome is the same.
- An uncaught error prints only the last traceback line (no stack frames).
