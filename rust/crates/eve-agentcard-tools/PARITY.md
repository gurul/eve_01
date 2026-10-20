# eve-agentcard-tools
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/agentcard_tools.py` | `src/lib.rs`; tests in `src/tests.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `CONFIRM_TOOL` | `CONFIRM_TOOL` | `"agentcard_confirm"` |
| `NONCONFIRM_TOOLS` | `NONCONFIRM_TOOLS: [&str; 3]` | Same three names |
| `_CART_HASH` | `CART_HASH` (private `LazyLock<Regex>`) | `\A[0-9a-f]{16}\z`, as `fullmatch` with `\Z` |
| `AskUser` | `AskUser` (boxed `Fn(String, String) -> Future<Result<String, AskError>>`) and the generic `F: FnOnce(String, String) -> Fut` that `handle` takes | Python's `TimeoutError` is `AskError::Timeout`; any other exception is `AskError::Failed` (text never shown) |
| `Service` (Protocol) | `trait Service` with `type Snapshot` | `handle(&str, Value)`, `prepare_confirm(&str, &[String]) -> Prepared<Snapshot>`, `confirm(Snapshot, &Arc<AtomicBool>)`; blocking, run with `spawn_blocking` (Python `asyncio.to_thread`). `Err` or a panic is Python's `Exception` |
| (prepare dict with `snapshot`) | `Prepared<S> { result, snapshot }` | The snapshot object is kept out of the JSON, as `eve_agentcard::Prepared` does |
| (real client) | `impl Service for eve_agentcard::AgentCard` | Python passed the `AgentCard` instance directly; this impl is that binding. `confirm` calls `AgentCard::try_confirm`, so the client's one Python exception (`ConfirmRaised`) is an `Err`, answered as the unknown outcome like Python's `except Exception` |
| `_Confirmation` | `Confirmation` (private) | `chat_id`, `cancelled: Arc<AtomicBool>` (the `threading.Event`), `phase` + question `AbortHandle` under one lock, `done` watch for `wait_idle` |
| `_not_submitted` | `not_submitted(&str) -> Value` | Key order `ok`, `status`, `reason` |
| `AgentCardTools` | `AgentCardTools<S: Service>` (`Clone`, shared `Arc` inner) | |
| `AgentCardTools.__init__` | `AgentCardTools::new(Arc<S>)` | `service()` returns the client |
| `AgentCardTools.active` | `active(Option<i64>)` | |
| `AgentCardTools.stop` | `stop(Option<i64>) -> StopReport` | `StopReport { stopped, in_flight, line }`; `to_value()` gives the Python dict; lines verbatim |
| `AgentCardTools.shutdown` | `shutdown()` | Closes admission for every tool, then `stop(None)` |
| `AgentCardTools.wait_idle` | `async wait_idle()` | Waits on each outstanding lifecycle task's `done` channel; never cancels |
| `AgentCardTools.handle` | `async handle(&str, &Value, Option<i64>, F) -> Value` | Same check order and reasons; the lifecycle task is `tokio::spawn`ed (shield); dropping the caller's future is `CancelledError`: it sets the flag and withdraws the question |
| `AgentCardTools._confirm` | `confirm()` (private async fn) | Holds the FIFO `tokio::sync::Mutex` from preparation until the confirm worker returns; same phase ladder `queued → preparing → asking → confirming`; same messages |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_agentcard_tools.py` | all 22 (parametrized cases looped inside each Rust test) | same names in `src/tests.rs` | — `caplog` checks become "the output holds no private text" (the gateway logs nothing). `test_unknown_tools_and_non_chat_principals…`: `True` and `"101"` cannot be passed to an `Option<i64>`; `None` is tested. `test_a_question_that_suppresses_cancellation…`: a Rust future cannot swallow an abort, so the stubborn question returns "yes" right after Stop instead; the same post-answer check refuses it |
| (Rust only) | | `selection_is_copied_from_the_borrowed_arguments`, `a_panicking_client_is_an_exception_without_text`, `non_object_arguments_and_messages_are_verbatim`, `cart_hash_is_exact`, `real_client_failure_is_returned_without_asking_or_sending` | |

## Boundaries
None.

## Differences
- `chat_id` is `Option<i64>`: Python's runtime `isinstance(chat_id, int) and not bool` check is the type. `None` gets the same "An owner chat is required for purchase approval." reason.
- Python ran on one event loop, so `stop()` and the lifecycle task never interleaved. Rust workers run on several threads, so the cancel flag, the phase and the question handle change together under one per-request lock (`enter(phase)` refuses once Stop has set the flag). The observable outcomes are the same.
- If the question task is aborted without Stop having set the flag (only possible during runtime shutdown), Python re-raised `CancelledError`; Rust sets the flag and returns the "stopped before submission" result.
- When preparation is not ok, Python returned the client's dict, which could carry the snapshot object; Rust returns the JSON part only (the snapshot is never JSON).
