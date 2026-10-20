# eve-chrome-lane
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/chrome_lane.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `NOTHING_DONE_BUDGET_SECS` | `NOTHING_DONE_BUDGET_SECS: f64 = 15.0` | Verbatim, with its production note |
| `ChromeLaneAgent` | `ChromeLaneAgent` | All methods take `&self` (state behind a `parking_lot::Mutex`, flags atomic), so a door can hold it in an `Arc`, `run` it on one task and `cancel`/`steer` from another |
| `ChromeLaneAgent.__init__(make_planner, make_fallback, on_event, ask_user, prepare=, lane_screenshot=, idle_budget_secs=, before_fallback=)` | `ChromeLaneAgent::new(make_planner, make_fallback, on_event, ask_user)`, `ChromeLaneAgent::with_options(..., ChromeLaneOptions { prepare, lane_screenshot, idle_budget_secs, before_fallback })` | Callables are `MakePlanner`, `MakeFallback`, `OnEvent`, `AskUser`, `Prepare`, `LaneScreenshot`, `BeforeFallback` type aliases; `idle_budget_secs: None` is `NOTHING_DONE_BUDGET_SECS` |
| `ChromeLaneAgent.provider` | `ChromeLaneAgent::PROVIDER` / `provider()` | `"chrome-lane"` |
| `goal`, `final`, `handed_on`, `browser_used`, `on_event`, `ask_user` attributes | `goal()`, `final_answer()`, `handed_on()`, `browser_used()` (always `true`), pub `on_event`, pub `ask_user` | |
| `ChromeLaneAgent.running` | `running()` | `getattr(current, "running", False)` via `LaneBody::running` (default `false`) |
| `ChromeLaneAgent.status` | `status() -> serde_json::Map` | `{**inner, "provider", "handed_on"}`; an inner `provider` key keeps its place, as a dict update does |
| `ChromeLaneAgent.steer` | `steer()` | `false` with no current agent |
| `ChromeLaneAgent.cancel` | `cancel(reason)` | Records the reason, aborts the pending `prepare` (the wait on Chrome's Allow) at once, cancels whoever runs |
| `ChromeLaneAgent.browser_shot` | `browser_shot() -> Option<(Vec<u8>, String, String)>` | Same captions; empty lane bytes are no picture (`if data:`) |
| `ChromeLaneAgent.__getattr__` | `current() -> Option<Body>`, `browser_screenshot()`, `LaneBody::as_any` | Python forwards unknown attributes to whoever ran last; Rust returns that agent (`Body::Planner` / `Body::Fallback`) for a typed or downcast read |
| `ChromeLaneAgent.run` | `run(goal) -> anyhow::Result<String>` | Same flow, events, logs and budget; the `Err` is only ever Codex's own failure, which Python lets propagate |
| `ChromeLaneAgent._attempt` | private `attempt` / `attempt_inner` | `prepare` runs as a `futures::Abortable`; abort by our budget → `("", "")`, by `cancel()` → `("Stopped.", "")`; any error → warning, `("", "")` |
| `ChromeLaneAgent._forward` | private `forwarder()` | Drops the planner's `started`/`final`, marks progress and passes the rest through |
| (duck-typed planner: `run_in_browser`, `cancel`, `browser.plans_started`) | `trait Planner: LaneBody` (`run_in_browser`, `plans_started` default `0`) | |
| (duck-typed Codex: `run`, `browser_screenshot`) | `trait Fallback: LaneBody` (`run`), `BrowserScreenshot { data, suffix }` | |
| (shared `running`/`status`/`steer`/`cancel`) | `trait LaneBody` | Defaults match Python's `getattr`/`hasattr` fallbacks |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_chrome_lane.py` | `test_a_finished_plan_answers_and_codex_never_starts`, `test_part_done_hands_codex_the_goal_and_what_was_done`, `test_nothing_done_gives_codex_the_whole_task`, `test_a_broken_lane_never_costs_the_task`, `test_the_owners_no_is_the_answer_and_codex_does_not_retry_it`, `test_a_cancel_stops_without_handing_on`, `test_stop_while_waiting_for_chromes_allow_is_immediate`, `test_a_lane_task_pictures_eves_tab_never_the_desktop`, `test_after_codex_took_over_the_picture_is_codexs_tab`, `test_a_lane_with_nothing_done_hands_codex_the_task_within_the_budget`, `test_waiting_on_chromes_allow_past_the_budget_hands_codex_the_task`, `test_a_plan_under_way_is_never_cut_off_by_the_budget`, `test_codex_gets_the_owners_chrome_ready_before_it_takes_over`, `test_a_failed_chrome_start_still_hands_codex_the_task`, `test_a_finished_plan_never_starts_the_owners_chrome`, `test_a_stop_while_the_owners_chrome_gets_ready_never_starts_codex` | same names in `src/tests.rs`, plus `test_an_empty_lane_picture_is_no_picture`, `test_status_steer_and_running_reach_whoever_runs`, `test_a_stop_reaches_codex_once_it_took_over`, `test_a_failed_connection_hands_codex_the_task`, `test_helpers_match_python` | |
| `bridge/tests/test_chrome_lane.py` | `test_with_the_lane_off_nothing_changes`, `test_with_the_lane_on_web_goals_take_the_chrome_body_and_the_rest_stay_codex`, `test_chrome_approval_is_asked_in_the_owners_telegram_chat` | - | Test `Daemon._chrome_body` / `Daemon._ask_owner_on_phone`: belong to the `daemon` crate |
| `bridge/tests/test_chrome_lane.py` | `test_a_dead_connection_is_replaced_on_the_next_connect` | - | Tests `BrowserLane.connect`: belongs to the `browser-lane` crate |
| `bridge/tests/test_chrome_lane.py` | `test_the_telegram_screenshot_prefers_the_browser_and_falls_back_to_the_desktop` | - | Tests `TelegramInlet._send_screen`: belongs to the `brain` crate (telegram) |

## Boundaries
None

## Differences
- The "lane failed" warning names the error kind as Python's `type(e).__name__` would only where Rust can tell: `OSError` for a `std::io::Error` root cause, otherwise `Error`. The message (first line, 300 characters) is the same.
- Python's `_attempt` re-raises a `CancelledError` on the connection when neither our budget nor `cancel()` caused it (the whole task cancelled from outside). In Rust a cancel from outside drops the `run` future, so that path does not exist inside `attempt`.
- `LaneBody::steer` defaults to `false` for an agent without steering, where Python would raise `AttributeError`; both real bodies implement it.
