# eve-computer-agent
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/computer_agent.py` | `src/lib.rs` (constants, prompts, config, errors, response classification, pure helpers), `src/agent.rs` (`ComputerAgent`), `src/worker.rs` (`Worker` seam, `WorkerClient`, `BrowserLaneWorker`), `src/responses.rs` (the Responses creators and TLS-drop retry), `src/grants.rs` (`desktop_grants`), `src/py.rs` (the Python semantics the module leans on); tests in `src/tests.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_MODEL`, `DEFAULT_BROWSER_MODEL`, the timeouts, `RETRYABLE_API_ERRORS`, `REASONING_EFFORTS`, `PROGRESS_SKIP_PREFIXES`, `LANE_FIRST_TIMEOUT_SECS`, `OUTLINE_TIMEOUT_SECS`, `RUN_PLAN_TIMEOUT_SECS`, `PLAN_EXEC_DEFAULT`, `MAX_PLAN_CONFIRMS`, `MAX_BROWSER_REPLANS`, `GOAL_URL` | same `pub const`s / `LazyLock` statics | Values copied verbatim |
| `INSTRUCTIONS_TEMPLATE`, `VERIFY_INSTRUCTIONS`, `VERIFY_SCHEMA`, the fast-lane helper/rule texts, `INSTRUCTIONS`, `AVAILABLE_HELPERS`, `ASK_USER_TOOL`, `TOOLS` | same names | Prompt text verbatim |
| `instructions` | `instructions(fast_lane, decide) -> String` | Same three variants (off, `keyword`, `model`) |
| `tools` | `tools(fast_lane) -> Value` | |
| `AgentConfig` | `AgentConfig` (`Default` = Python's field defaults) | |
| `browser_config` | `browser_config(&cfg, Option<&HashMap>)` | `None` reads the process env; precedence `EVE_BROWSER_MODEL` > `EVE_AGENT_MODEL` > a non-default `cfg.model` > `gpt-6-luna` |
| `_effort` | private `effort(env, key, default)` | strip + lowercase, must be in `REASONING_EFFORTS` |
| `_seconds` | private `seconds(env, key, default, floor)` | Python `float()` parsing (`py_float_parse`), floored |
| `configured` | `configured(Option<&HashMap>)` | Every env var and floor as in Python |
| `WorkerClient` | `WorkerClient` (implements `Worker`) | JSON-lines protocol byte for byte (`py_dumps` request lines, `id` matching, stale-reply skip, grace after a timed-out `lane_first`, 32 MiB line limit). Restart on timeout/EOF once, `WorkerDead` on the second; `FailSafe` on error code `failsafe`. `lane_first_with`, `outline_with`, `run_plan_with` take Python's `timeout=` argument |
| `WorkerDead`, `FailSafe`, `Cancelled` | same names (`thiserror`) | `error_name` reports the Python class names |
| `Classified` | `Classified { kind, calls: Vec<Call>, text, phases }` | `Call::to_dict` is the Python dict |
| `classify_response` | `classify_response(&Value) -> Result<Classified>` | Messages verbatim (`RuntimeError` / `ValueError`) |
| `describe_failure` | `describe_failure(&anyhow::Error)` | Same lines; classes by name through `error_name` |
| `progress_lines` | `progress_lines(&[Value])` | |
| `_same_page` | `same_page(a, b)` | `urlsplit().hostname` minus `www.`, path minus trailing `/`, query |
| `_nothing_left` | `nothing_left(&Value)` | |
| `_closing_say` | `closing_say(&Plan, &[Value])` | |
| `_user` | `user(text)` | |
| `_verdict` | `verdict(&Value) -> Result<(bool, String)>` | Messages verbatim |
| `ComputerAgent` | `ComputerAgent::new` / `with_options(create, AgentOptions)` | `run`, `run_in_browser`, `steer`, `cancel(reason)`, `status`, `bill`, `create`, accessors for the Python attributes. The tiers run in Python's order: browser-first, reflex, lane-first, plan-once, loop with verify. Run log in `json.dumps(ensure_ascii=False)` format |
| `desktop_grants` | `desktop_grants(prompt) -> Value` | `AXIsProcessTrusted`, `CGPreflightScreenCaptureAccess` / `CGRequestScreenCaptureAccess` through framework links |
| `_openai_client` | `responses::openai_client(api_key, client)` | Builds the `eve_openrouter::Client` (OpenRouter-only since 2026-10-08, as in Python) |
| `is_connection_drop` | `responses::is_connection_drop(&anyhow::Error)` | `SslError`, `APIConnectionError`, connection-reset/aborted/broken-pipe io errors |
| `_ClientSlot` | `responses::ClientSlot` (`lease` / `release` / `replace`) | A retired client is closed when its last lease is released |
| `_close_quietly` | inside `ClientSlot::release` | Close errors are swallowed |
| `_slot` | inside `make_response_creator` / `make_stream_creator` | A fixed client is reused; a factory gives a new client per drop |
| `make_response_creator` | `make_response_creator(CreatorOptions) -> Result<CreateResponse>` | `TLS_BACKOFF` [0.3, 1.0], `TLS_RETRIES` 2; the request's `timeout` key becomes the call's timeout |
| `make_stream_creator` | `make_stream_creator(CreatorOptions) -> Result<StreamResponse>` | No retry once a text delta has been read out |
| `plain_response` | `plain_response(Value) -> Value` | Drops `parsed_arguments` and content `parsed` |
| `AgentEvent` (re-export) | `pub use eve_agent_contract::AgentEvent` | Same type, as `test_app_reflex` asserts |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_computer_agent.py` | 44 | 43 with the same names | `test_bill_report_sums_the_runs` tests `tools/bill_report.py`, a script, not this module |
| `bridge/tests/test_worker_client.py` | 6 | All 6, against `python3 bridge/tests/fake_worker_child.py` (the protocol is the same) | None |
| `bridge/tests/test_agent_lane_first.py` | 5 | All 5 | None |
| `bridge/tests/test_agent_plan_once.py` | 8 | All 8 | None |
| `bridge/tests/test_agent_browser_lane.py` | 27 | All 27 | None |
| `bridge/tests/test_task_router.py` (ref) | the 4 agent tests | All 4 (`test_a_complete_reflex_makes_zero_planner_calls` and the rest) | The classifier tests belong to eve-desktop-lanes |
| `bridge/tests/test_response_creators.py` (ref) | 14 | 11 | The 3 `test_a_turn_*` / `test_any_other_failure_keeps_the_generic_line` drive `telegram.Bot`; they belong to the telegram crate |
| `bridge/tests/test_response_retry_lean.py` (ref) | 4 | All 4 | None |
| `bridge/tests/test_browser_planner_config.py` (ref) | 3 | 2 | `test_actual_daemon_browser_factory_uses_browser_config` drives `Daemon._chrome_body`; it belongs to the daemon crate |
| `bridge/tests/test_spend_wiring.py` (ref) | `test_every_computer_agent_model_call_is_metered` | Same name | The others meter other crates |
| `bridge/tests/test_openrouter.py` (ref) | `test_the_brain_and_the_app_builder_build_their_clients_here` | Same name (via `openrouter_client` with an env map) | The others test eve-openrouter |
| `bridge/tests/test_fastlane_docs.py` (ref) | the prompt assertion | inside `test_instructions_variants_track_fast_lane_default` | The doc-file assertions read `docs/`, not this module |
| `bridge/tests/test_desktop_live.py` (ref) | `test_worker_check_prints_ready` | Same name, `#[ignore = "live: ..."]`; plus `test_desktop_grants_names_the_process` | `test_screen_text_finds_the_frontmost_app_in_the_menu_bar` belongs to the desktop helpers |
| `bridge/tests/test_app_reflex.py`, `test_rundown.py`, `test_voice_agent.py` (ref) | use `AgentEvent` | `test_agent_event_is_the_contract_s_type` | The rest belong to their crates |
| `bridge/tests/test_telegram.py`, `test_telegram_chief.py` (ref) | use the creators / `ComputerAgent` | Covered by the creator tests above | They belong to the telegram crates |
| (parity) | | `test_python_semantics_the_agent_leans_on`, `test_error_names_are_the_python_class_names`, `test_a_worker_that_cannot_start_is_a_generic_failure`, `test_a_child_that_never_says_ready_fails_to_start`, `test_a_client_with_no_child_answers_softly`, `test_a_connection_drop_is_told_apart_from_other_failures`, `test_plain_response_drops_what_the_stream_helper_parsed`, `test_an_unwritable_runs_dir_still_runs`, `test_a_status_that_is_not_a_string_is_shown_as_python_repr`, `test_the_release_pass_runs_the_worker_program_with_its_arguments` | |

## Boundaries
- The desktop worker child is the Rust `eve-desktop-worker` binary (beside the current executable, else on `PATH`), started with no arguments; Python started `python -m eve_bridge.desktop_worker`. The wire protocol is identical, so either child works (`WorkerClient::with_program`). The release step runs the same program with the same arguments plus `--release` (Python ran `python -m eve_bridge.desktop_worker --release`; with the Rust binary that is `eve-desktop-worker --release`).
- The Responses client is `eve_openrouter::Client` (crate eve-openrouter); the browser lane is `eve_browser_lane::BrowserLane`, wrapped as `BrowserLaneWorker`.
- Spend goes to `eve_spend::Ledger::record_response` under the "browser & Mac tasks" feature; Jev calls are read from `eve_jev::METER`.

## Differences
- Python duck-types the worker (`getattr(worker, "lane_first", None)`); Rust uses the `Worker` trait, whose default methods answer as a missing attribute would (`AttributeError`, or `None` for `lane_first`).
- Monkeypatched module globals are explicit seams in `AgentOptions`: `apps` (installed apps), `ledger`, `meter`, `clock`, `sleep`, `browser`. `None` means the real one.
- `configured` and `browser_config` take an optional env map instead of reading `os.environ` under monkeypatch.
- Exceptions are `anyhow::Error`; `PyError { kind, message }` stands for a Python class where no Rust type exists, and `error_name` maps errors to Python class names for the log and `describe_failure`. An unknown error is `RuntimeError`.
- A TLS drop is detected as a reqwest body or decode error from the client (mapped to `SslError`) rather than `ssl.SSLError`.
- `run_in_browser` returns `Result`: a `FailSafe` from the lane propagates as it did in Python, where it was raised.
- JSON decode error messages in log lines are serde's text, not `json.JSONDecodeError`'s.
- `desktop_grants()["python"]` keeps its key name and holds the real path of the current executable (the binary TCC credits), not `sys.executable`.
- The router asker's warning names the error class `JevError` directly.
- Cancellation is a `watch` channel raced against the in-flight request or exec (`tokio::select!`), where Python cancelled the asyncio task; the observable result (`Stopped.` / `Stopped: <reason>.`, worker closed, `cancelled` log line) is the same. As with `asyncio.wait(FIRST_COMPLETED)`, a call that is already done when the cancel is seen returns its result; the loop's next `_check_cancel` then stops the run.
