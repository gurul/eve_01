# eve-codex-warm
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/codex_warm.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_MAX_AGE_SECS`, `REFRESH_SECS` | `DEFAULT_MAX_AGE_SECS`, `REFRESH_SECS` | 900.0 and 60.0 seconds |
| `configured` | `configured()`, `configured_from(lookup)` | `EVE_CODEX_PREWARM` (default `"1"`; stripped, lowercased, off for `0`/`false`/`no`/`off`) and `EVE_CODEX_WARM_SECS` (empty → 900; parsed like Python `float()`: whitespace, `_` between digits, `inf`/`nan`; a bad value → 900); age is `max(60.0, age)` with Python's NaN result (60) |
| `_nobody` | `nobody(String) -> String`, `nobody_ask() -> AskUser` | Answers `""` |
| `WarmCodex` | `WarmCodex<A: WarmAgent>` (cheap `Clone`, shared state) | Agent class as factory → `Factory<A>` = `Fn(OnEvent<A::Event>, AskUser) -> A`; the agent's `prewarm`/`warm`/`discard` and callback assignment are the `WarmAgent` trait |
| `WarmCodex.__init__` | `WarmCodex::new(factory, enabled, max_age)`, `WarmCodex::with_factory(f)` (Python defaults) | `enabled`, `max_age` read via `enabled()`, `max_age()` |
| `WarmCodex._spawn` | private `spawn` | Job handles kept so `close()` waits for them |
| `WarmCodex.kick` | `kick()` | Clears `in_use`, then `_kick` |
| `WarmCodex._kick` | private `kick_inner()` | Nothing when disabled, in use, ready, or a warm-up is in flight |
| `WarmCodex._warm` | private `warm_up(rx)` | Factory with a no-op `on_event` and `nobody`; a called-off warm-up discards its agent; a failed one logs `codex-warm: warm-up failed (<name>); tasks start cold until the next refresh`; success logs `codex-warm: an agent is ready` |
| `WarmCodex.take` | `take(on_event, ask_user) -> A` | Sets `in_use`, calls off a warm-up in flight, hands out the ready agent only if `warm(max_age)`, discards a stale one in a job, else builds a cold agent |
| `WarmCodex.refresh_loop` | `refresh_loop(Duration)`, `refresh_loop_default()` | Discards a stale ready agent, kicks (never while in use), sleeps; `finally: close()` is a drop guard that spawns `close()` when the loop's task is cancelled |
| `WarmCodex.close` | `close()` | Calls off the warm-up, discards the ready agent, awaits every job (errors ignored, as `return_exceptions=True`) |
| (`pool._jobs` gathered by tests; `pool._ready` read by tests) | `join_jobs()`, `is_ready()` | Public helpers for the daemon and tests |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_codex_warm.py` | `test_take_hands_the_warm_agent_this_tasks_callbacks`, `test_a_dead_warm_agent_is_discarded_not_handed_out`, `test_kick_is_single_and_disabled_does_nothing`, `test_failed_warm_up_leaves_tasks_cold`, `test_close_discards_the_ready_agent`, `test_the_refresh_loop_never_warms_a_second_app_server_while_a_task_runs`, `test_configured` | same names in `src/tests.rs`, plus `configured_reads_the_switch_like_python`, `configured_parses_the_age_like_python_float`, `nobody_answers_nothing`, `a_stale_ready_agent_is_replaced_by_the_refresh_loop`, `a_disabled_refresh_loop_returns_at_once`, `take_calls_off_a_warm_up_that_never_finishes` | None |
| `bridge/tests/test_codex_api_metering.py` (tests_ref) | `test_warm_process_keeps_its_launch_auth_identity` | — | It tests `CodexComputerAgent`'s launch environment and spend metering through a real pool; that agent is ported in `eve-codex-computer`, which owns the test. The pool behavior it relies on (kick → prewarm once, take hands that agent over, close) is covered here |

## Boundaries
None

## Differences
- Python cancels a warm-up with `Task.cancel()` and `_warm` catches `CancelledError` to discard. Rust signals a watch channel; the warm-up stops awaiting `prewarm` and awaits `discard`. If the cancel lands just as `prewarm` succeeded (possible only on a multi-threaded runtime), the agent is discarded rather than stored, which is what Python's single-threaded loop guarantees anyway.
- The failure log names the error with `WarmAgent::failure_name` (Python: `type(e).__name__`). The default is `Error`; the Codex agent overrides it to give its error kind. No error detail is logged, as in Python.
- `kick`/`take` called with no tokio runtime log a warning and skip the job (Python would need a running loop as well).
