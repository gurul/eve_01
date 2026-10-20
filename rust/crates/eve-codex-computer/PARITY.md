# eve-codex-computer
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/codex_computer.py` | `src/lib.rs` (the module), `src/process.rs` (the app-server child behind a spawner seam), `src/py.rs` (Python's reading of protocol JSON: `.get`, `[]`, `in`, `==`, hashing, `json.dumps`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `OWNER_ACTION_INSTRUCTIONS`, `INSTRUCTIONS`, `BROWSER_INSTRUCTIONS`, `VAULT_INSTRUCTIONS` | same names, plus `INSTRUCTIONS_WITH_VAULT` | verbatim (extracted from the Python source with `ast`); `INSTRUCTIONS` is base + owner policy + browser rules, as `INSTRUCTIONS += ...` builds it |
| `PLAN_NOTE` | `PLAN_NOTE` | |
| `meter_codex` | `meter_codex(what)`, `meter_codex_in(&Ledger, what)` | `('chatgpt', 'codex', CODEX, None, note='ChatGPT plan, not per-call (<what>)')` |
| `class CodexUnavailable` | `CodexError::Unavailable(message)` | `CodexError` also carries `Timeout` (`TimeoutError`), `Cancelled`, `Os` (`OSError` and its subclass name), `Runtime` (`RuntimeError`) and `Py` (an exception Python lets escape); `class_name()` is `type(e).__name__` |
| `APP_ACCESS_TOOLS`, `ONCE_ANSWERS`, `PERSIST_ANSWERS` | same names | arrays; `PERSIST_ANSWERS` as `(answer, scope)` pairs |
| `browser_origin` | `browser_origin(&Value) -> PyResult<Option<String>>` | same checks in the same order; `urlsplit` is `eve_vault::pyurl` (Python 3.13's); `Err` where Python raises (a non-dict `params`, an unhashable `codex_request_type` in the set test) |
| `app_persistence` | `app_persistence(&Value) -> PyResult<BTreeSet<String>>` | `Err` for an unhashable `tool_name`, as `in frozenset` raises |
| `routine_access` | `routine_access(&Value) -> PyResult<bool>` | the browser-origin probe is a modified copy, the request is not mutated |
| `VAULT_TOOL_ASK` | `VAULT_TOOL_ASK` | `re.match` with `$` also matches before a final newline: `\n?\z` |
| `vault_tool_call` | `vault_tool_call(&Value) -> PyResult<bool>` | server name and tool names from `eve_vault_mcp::{SERVER_NAME, TOOL_NAMES}` |
| `executable` | `executable()`, `executable_from(configured, bundled, which)`, `which_codex(path)` | `EVE_CODEX_BIN`, then `/Applications/ChatGPT.app/Contents/Resources/codex`, then `shutil.which('codex')` (unset `PATH` is `CS_PATH`, empty finds nothing), then `codex` |
| `external_environment` | `external_environment()` | `eve_codex_auth::launch_environment(None).0` |
| `class CodexComputerAgent` | `struct CodexComputerAgent` (a cloneable handle) | `new(on_event, ask_user, Options)` / `try_new` (raises like `Vault()`); `Options` holds the keyword arguments (`binary`, `max_secs` 600, `rpc_timeout` 150, `site_access`, `routine_permissions`) and the seams Python's tests patch (`spawner`, `launch_environment`, `ledger`, `vault`) |
| `CodexComputerAgent.provider` | `provider()`, `PROVIDER` | `"codex"` |
| `CodexComputerAgent.__init__` attributes | `running()`, `goal()`, `final_text()`, `last_commentary()`, `thread_id()`, `turn_id()`, `ui_evidence()`, `routine_permissions()`, `site_access()`, `browser_used()`, `browser_screenshot()`, `browser_tab_id()`, `binary()`, `set_vault()`, `set_callbacks()` | `EVE_CODEX_ROUTINE_ACCESS` (default `allow`), `EVE_CODEX_SITE_ACCESS` (default: the routine value): `access_settings` |
| `_thread_started` | `State::thread_started` | |
| `_begin_metering`, `_meter_event`, `_finish_metering` | `State::begin_metering`, `State::meter_event`, `State::finish_metering` + `Shared::finish_run` | same validity rules (`type(x) is int`, non-negative, cached <= input, monotonic totals, delta replace not add); priced by `eve_pricing::estimate_openai_cost` |
| `_emit` | `Shared::emit` | `AgentEvent::with_text(kind, text)`; called with no lock held |
| `status` | `status() -> Value` | `{running, provider, goal, last, final, thread_id}` in that order |
| `_background` | `Shared::background` | finished jobs pruned; `_close` aborts and awaits them |
| `steer`, `_steer` | `steer(text) -> bool`, `Shared::steer` | progress `Codex could not confirm the correction; it was not retried.` on an unconfirmed correction |
| `cancel` | `cancel(reason)` | a `watch` flag the running task selects on; never reset, as `_cancelled` |
| `_send` | `Shared::send` | lines are `json.dumps(msg)` (`", "`/`": "`, ASCII-escaped) + `\n`; `Codex is not connected.` / `Codex disconnected; the task was not retried.` |
| `_rpc` | `Shared::rpc` | ids from one counter; the waiter leaves the table however it ends; `rpc_timeout` -> `Timeout` |
| `_read` | `Shared::read`, `Shared::dispatch`, `read_line` | asyncio's 32 MiB line limit (`LINE_LIMIT`); a last line without newline is still read; any failure fails every waiter and the task with `Invalid Codex event stream.`, EOF with `Codex disconnected before the task finished; no retry was made.`; request ids looked up as Python's dict does (`1 == 1.0 == True`) |
| `_notification` | `State::notification` | |
| `_browser_capture` | `State::browser_capture`, `b64decode_validate` | `b64decode(validate=True)` measured against CPython; `eve_telegram_images::validate` |
| `_answer_request` | `Shared::answer_request`, `Shared::answer` | every message, choice text and JSON-RPC code verbatim; a failure (malformed form, broken `ask_user`) answers `-32603 User input unavailable` |
| `_launch` | `Shared::launch` | `<binary> app-server --listen stdio://`, exactly the launch environment, stderr to /dev/null; `initialize` with `{"clientInfo": {"name": "eve_computer_use", "version": "0.1.0"}}` then `initialized` |
| `_thread_params` | `Shared::thread_params` | `{ephemeral, cwd, sandbox: read-only, approvalPolicy: on-request, developerInstructions}` (+ `config.mcp_servers.eve_vault` when the vault has items) |
| `_vault_changed` | `Shared::vault_changed` | |
| `_start`, `_open_thread` | `Shared::start`, `Shared::open_thread` | pages `mcpServerStatus/list` with `limit: 100` and `cursor`; `NO_CUA` message verbatim |
| `prewarm`, `warm`, `discard` | `prewarm()`, `warm(max_age)`, `discard()`; also `impl eve_codex_warm::WarmAgent`, `warm_factory`, `ask_from_warm` | |
| `run` | `run(goal) -> Result<String, CodexError>` | `Err` only where Python raises out of `run()` |
| `_close` | `Shared::close` | jobs, `turn/interrupt` (3 s), terminate then kill (3 s), reader, waiters |
| (`asyncio.create_subprocess_exec`) | `process::{Spawner, Spawned, Child, os_spawner}` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_codex_computer.py` | `test_inventory_pagination_progress_result_and_no_credentials`, `test_missing_tool_never_starts_a_turn_positive_control_in_previous_test`, `test_approvals_are_once_explicit_and_unknown_forms_fail_closed`, `test_cancel_while_approval_waits_interrupts_turn_and_cleans_up`, `test_steering_and_disconnect`, `test_browser_image_is_bound_to_the_observed_task_tab_and_verified_bytes`, `test_new_cua_action_invalidates_previous_browser_picture` | same names in `src/tests.rs` (the fake `Process` is `Fake`, answering each written line synchronously as Python's does) | `test_daemon_factory_routes_shared_task_contract_to_codex`: tests `Daemon._make_agent` (crate `eve-daemon`) |
| `bridge/tests/test_codex_api_metering.py` | `test_api_computer_uses_process_home_and_prices_full_turn_once`, `test_plan_metering_stays_unpriced_and_is_recorded_before_turn`, `test_warm_process_keeps_its_launch_auth_identity`, `test_unknown_model_or_absent_usage_is_unpriced` (3 cases), `test_malformed_usage_never_becomes_free_or_guessed_spend` (6 cases), `test_cancellation_collects_interrupt_usage_and_records_once` (agent case), `test_interrupt_ack_without_completion_never_prices_partial_usage` (agent case), `test_failed_api_launch_counts_one_unpriced_task`, `test_reusing_computer_agent_starts_fresh_usage_baseline`, `test_missing_api_home_falls_back_in_both_environment_and_spend` | same names | `test_chat_prices_deltas_and_ignores_old_or_unrelated_events`, `test_chat_missing_or_reset_counter_does_not_charge_previous_turns`, `test_rerouted_models_are_priced_only_when_usage_is_unambiguous` and the `chat_mode=True` cases: they test `CodexChat` (crate `eve-codex-chat`) |
| `bridge/tests/test_codex_permissions.py` | `test_persistent_choices_go_to_codex`, `test_once_and_denial_never_persist`, `test_managed_restriction_removes_always_without_downgrading_choice`, `test_unknown_audio_action_scopes_cannot_be_remembered`, `test_risk_warning_is_preserved_and_forms_still_fail_closed`, `test_owner_site_preference_avoids_prompt_without_saving_or_forging_review`, `test_site_preference_does_not_approve_other_permissions`, `test_only_bare_http_origins_receive_site_preference`, `test_site_preference_does_not_approve_stale_threads_or_forms`, `test_routine_app_access_does_not_interrupt_chat_or_call_tasks`, `test_file_transfer_preference`, `test_routine_preference_never_inherits_action_or_strict_permissions`, `test_routine_preference_rejects_stale_and_nonempty_forms`, `test_owner_defaults_and_opt_out`, `test_both_agents_receive_owner_action_confirmation_policy`, `test_routine_preference_keeps_explicit_user_questions` | same names (parametrized cases as loops; env through `access_settings`) | the `chat=True` cases and the `CHAT_INSTRUCTIONS` half: crate `eve-codex-chat` |
| `bridge/tests/test_vault_mcp.py` | `test_codex_threads_get_the_vault_server_only_when_the_vault_has_items`, `test_codex_a_warm_thread_is_stale_once_the_vault_gains_or_loses_its_items`, `test_codex_accepts_only_the_vault_servers_own_tool_approvals` | same names | the server's own tests: crate `eve-vault-mcp` |
| `bridge/tests/test_spend_wiring.py` | `test_codex_is_counted_without_a_price` | same name | the rest test other modules |
| `bridge/tests/test_self_context.py`, `bridge/tests/test_telegram_chief.py` | (they check that other modules build a `CodexComputerAgent`) | | their modules' crates |
| (parity additions) | | `a_second_run_while_running_is_a_runtime_error_and_status_reports_the_task`, `requests_go_out_as_python_json_dumps_writes_them`, `a_malformed_event_stream_fails_the_task_without_a_fallback`, `a_failed_or_interrupted_turn_is_never_reported_as_done`, `completion_without_ui_evidence_says_so`, `a_task_past_max_secs_times_out_and_is_closed`, `prewarm_then_run_only_starts_the_turn_and_warm_tracks_the_process`, `a_failed_prewarm_closes_what_it_started`, `dropping_a_run_stops_it_in_the_background`, `executable_prefers_the_setting_then_chatgpt_then_path`, `read_line_keeps_asyncios_limit_and_last_partial_line`, `malformed_forms_raise_where_python_raises`, `questions_list_their_options_and_unknown_methods_are_refused`, `a_broken_question_transport_never_becomes_approval`, `vault_tool_approvals_are_accepted_without_asking`, `base64_validate_is_pythons`, and `py::tests::*` | |

## Boundaries
None. The app-server is the same external `codex` binary Python starts, over the same stdio JSON lines.

## Differences
- The thread's `eve_vault` MCP server entry is `eve_vault_mcp::server_config("")`: the Rust `eve-vault-mcp` binary with no arguments (that crate's documented choice), where Python starts `<python> -I -m eve_bridge.vault_mcp`.
- `CodexComputerAgent::new` runs without a vault (logged) when `Vault()` cannot be made (an `EVE_VAULT_KEYCHAIN` with spaces or quotes), where Python's constructor raises; `try_new` raises as Python does.
- Cancellation (`cancel()` or dropping the `run()` future) stops the task body; it does not interrupt `run()`'s cleanup, where Python's `CancelledError` can land inside `_close`. A dropped `run()` future finishes the stop and the close on a background task.
- Protocol values of an unexpected JSON type are read as Python would read them where that decides a branch (`.get` on a non-dict, `in`, hashing, `==` across int/float/bool); where Python would carry a non-string onward (an agent message `text` that is not a string, a non-string permission `message`), Rust takes the text as Python's `str()` of it, or answers the request `-32603` without first emitting the `ask` event. A non-hashable turn id is compared, never raised on.
- `serde_json` refuses what `json.loads` accepts beyond JSON (`NaN`, `Infinity`): such a line is "Invalid Codex event stream.".
- `ask_user` is fallible (`Ask` returns `anyhow::Result<String>`), standing in for a Python `ask_user` that raises.
