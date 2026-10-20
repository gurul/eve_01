# eve-desktop-lanes
Status: partial

Skeleton committed by the architect; four fill units port the bodies (see "Work split"). Each unit records its
tests, boundaries and differences in its own fragment under `parity/` (`parity/<unit>.md`). Integration merges the
fragments into the three sections below and sets `Status: complete` once `cargo test -p eve-desktop-lanes`,
clippy and `python3 tools/check_port.py --crate desktop-lanes` pass.

## Work split
| Unit | Python | Rust files | Tests file |
|---|---|---|---|
| ax-candidates | ax_candidates.py | src/ax_candidates.rs, src/ax_ffi.rs, src/bin/ax_candidates.rs | tests/ax_candidates.rs |
| desktop-helpers | desktop_helpers.py | src/desktop_helpers.rs | tests/desktop_helpers.rs |
| fast-lane | fast_lane.py, lane_router.py, decision_lane.py | src/fast_lane.rs, src/lane_router.rs, src/decision_lane.rs | tests/fast_lane.rs |
| plan-and-task | plan_executor.py, task_router.py | src/plan_executor.rs, src/task_router.rs | tests/plan_and_task.rs |

`src/lib.rs` (LaneError, error_kind, perf_counter, the Python string helpers) is the architect's and complete.

## Modules
| Python | Rust |
|---|---|
| ax_candidates.py | src/ax_candidates.rs (+ src/ax_ffi.rs, the macOS AX/AppKit layer; src/bin/ax_candidates.rs, the CLI) |
| desktop_helpers.py | src/desktop_helpers.rs |
| fast_lane.py | src/fast_lane.rs |
| lane_router.py | src/lane_router.rs |
| decision_lane.py | src/decision_lane.rs |
| plan_executor.py | src/plan_executor.rs |
| task_router.py | src/task_router.rs |

## API
| Python | Rust | Notes |
|---|---|---|
| ax_candidates.is_sensitive | ax_candidates::is_sensitive | takes &str (Python's non-str fail-closed branch has no Rust caller) |
| ax_candidates._verb_shaped | ax_candidates::verb_shaped | |
| ax_candidates._titles_its_destination | ax_candidates::titles_its_destination | UrlParts is urlsplit's result |
| ax_candidates.is_account_change | ax_candidates::is_account_change | |
| ax_candidates.is_web_page | ax_candidates::is_web_page | |
| ax_candidates.needs_confirmation | ax_candidates::needs_confirmation | |
| ax_candidates.Candidate | ax_candidates::Candidate | to_dict / from_dict; impl eve_decider::OptionFields |
| ax_candidates.Snapshot | ax_candidates::Snapshot | `_refs`/`_readouts`/`_hrefs` are `refs`/`readouts`/`hrefs`, excluded from PartialEq; `ref` is `element_ref` |
| ax_candidates.snapshot_from_dict | ax_candidates::snapshot_from_dict | |
| ax_candidates.is_pressable | ax_candidates::is_pressable | |
| ax_candidates.pressable | ax_candidates::pressable | |
| ax_candidates._text | ax_candidates::text | |
| ax_candidates._role_name | ax_candidates::role_name | |
| ax_candidates._descendant_text | ax_candidates::descendant_text | |
| ax_candidates._value_text | ax_candidates::value_text | |
| ax_candidates._frame | ax_candidates::frame | |
| ax_candidates._on_screen | ax_candidates::on_screen | |
| ax_candidates.snapshot_from_raw | ax_candidates::snapshot_from_raw | refs keyed by NodePath (Python id(node)) |
| ax_candidates.ax_snapshot | ax_candidates::ax_snapshot | |
| ax_candidates.objective_tokens | ax_candidates::objective_tokens | |
| ax_candidates.score_candidate | ax_candidates::score_candidate | |
| ax_candidates.rank_candidates | ax_candidates::rank_candidates | `kinds` is the Kinds enum |
| ax_candidates.describe | ax_candidates::describe | |
| ax_candidates.context_summary | ax_candidates::context_summary | |
| ax_candidates._app_identity | ax_candidates::app_identity | |
| ax_candidates.pid_for_app | ax_candidates::pid_for_app | |
| ax_candidates.main_screen_points | ax_candidates::main_screen_points | |
| ax_candidates.ax_walk | ax_candidates::ax_walk, ax_walk_with + AxWalkBackend | the backend trait is the seam the tests' fake ApplicationServices drove |
| ax_candidates.main | ax_candidates::main, bin eve-ax-candidates | |
| desktop_helpers.Frame | desktop_helpers::Frame | |
| desktop_helpers.change_box | desktop_helpers::change_box | |
| desktop_helpers.app_name_for_pid | desktop_helpers::app_name_for_pid | |
| desktop_helpers._what | desktop_helpers::what | |
| desktop_helpers.cg_capture | desktop_helpers::cg_capture | |
| desktop_helpers.vision_ocr | desktop_helpers::vision_ocr | |
| desktop_helpers._ax_focused_pid | desktop_helpers::ax_focused_pid | |
| desktop_helpers._window_list_pid | desktop_helpers::window_list_pid | |
| desktop_helpers._workspace_pid | desktop_helpers::workspace_pid | |
| desktop_helpers._focused_pid | desktop_helpers::focused_pid, focused_pid_from | probes injectable |
| desktop_helpers.ax_frontmost | desktop_helpers::ax_frontmost | |
| desktop_helpers.ax_element_at | desktop_helpers::ax_element_at | |
| desktop_helpers.ax_press_on | desktop_helpers::ax_press_on | |
| desktop_helpers.ax_read | desktop_helpers::ax_read | |
| desktop_helpers.ax_press | desktop_helpers::ax_press | |
| desktop_helpers._matches | desktop_helpers::matches | |
| desktop_helpers.Helpers | desktop_helpers::Helpers (+ Backends, Gui, Clipboard, DelegateCall) | install() returns the bound names; gui_click/gui_hotkey/gui_press are the wrapped PyAutoGUI calls |
| desktop_helpers.ax_focused_element | desktop_helpers::ax_focused_element | |
| desktop_helpers._norm | desktop_helpers::norm | |
| desktop_helpers._LaneAdapter | desktop_helpers::LaneAdapter | implements Senses, Effectors, DecisionSenses |
| fast_lane.Thresholds | fast_lane::Thresholds | |
| fast_lane.Step | fast_lane::Step | |
| fast_lane.DelegateResult | fast_lane::DelegateResult | status is LaneStatus |
| fast_lane.MenuItem | fast_lane::MenuItem | |
| fast_lane._q | fast_lane::q | |
| fast_lane.describe_pick | fast_lane::describe_pick | |
| fast_lane._action_text | fast_lane::action_text | |
| fast_lane.format_line | fast_lane::format_line + LineFields | status is the LaneStatus enum |
| fast_lane._clip | fast_lane::clip | |
| fast_lane.gate | fast_lane::gate + GateState | |
| fast_lane.is_search_field | fast_lane::is_search_field | |
| fast_lane._same_control | fast_lane::same_control | |
| fast_lane.editable_pool | fast_lane::editable_pool | |
| fast_lane._shares_token | fast_lane::shares_token | |
| fast_lane.lane_context | fast_lane::lane_context | |
| fast_lane.jev_context | fast_lane::jev_context | |
| fast_lane._overlap | fast_lane::overlap | |
| fast_lane.keyword_verdict | fast_lane::keyword_verdict | |
| fast_lane.keyword_pick | fast_lane::keyword_pick | |
| fast_lane.uncovered_tokens | fast_lane::uncovered_tokens | |
| fast_lane._ax_changed | fast_lane::ax_changed | |
| fast_lane._jev_verdict | fast_lane::jev_verdict | |
| fast_lane._valid_key | fast_lane::valid_key | |
| fast_lane._Run | fast_lane::Run (crate) | |
| fast_lane._done_check | fast_lane::done_check (crate) | |
| fast_lane._timed_snapshot | fast_lane::timed_snapshot (crate) | |
| fast_lane._build_menu | fast_lane::build_menu (crate) | |
| fast_lane.run_delegate | fast_lane::run_delegate + DelegateOptions | senses/effectors/decider/asker are the Senses, Effectors, LaneDecider, StepAsk traits |
| fast_lane.ScriptResult | fast_lane::ScriptResult | |
| fast_lane._script_line | fast_lane::script_line | |
| fast_lane.run_script | fast_lane::run_script + ScriptOptions | |
| lane_router.RouteResult | lane_router::RouteResult | |
| lane_router.normalise | lane_router::normalise | |
| lane_router.split_clauses | lane_router::split_clauses | |
| lane_router.is_question | lane_router::is_question | |
| lane_router.refuse_reason | lane_router::refuse_reason | |
| lane_router.installed_app_names | lane_router::installed_app_names, installed_app_names_in | |
| lane_router.names_other_app | lane_router::names_other_app | |
| lane_router.spoken_sentence | lane_router::spoken_sentence | |
| lane_router._reason_of | lane_router::reason_of | |
| lane_router._from_script | lane_router::from_script | |
| lane_router.route | lane_router::route + RouteOptions | |
| decision_lane.Gates | decision_lane::Gates | |
| decision_lane.evidence | decision_lane::evidence | |
| decision_lane.menu | decision_lane::menu | |
| decision_lane.same_state | decision_lane::same_state | |
| decision_lane._scores | decision_lane::scores | |
| decision_lane.route | decision_lane::route + DecisionSenses | client is &mut dyn eve_decisions::Evaluate |
| plan_executor.Entry | plan_executor::Entry | |
| plan_executor.PlanResult | plan_executor::PlanResult | |
| plan_executor._holds | plan_executor::holds | |
| plan_executor._confirm_label | plan_executor::confirm_label | |
| plan_executor._reason | plan_executor::reason | |
| plan_executor._say | plan_executor::say | |
| plan_executor.run_plan | plan_executor::run_plan + PlanOptions | |
| plan_executor._grant | plan_executor::grant | Effectors::grant is Python's `approve` attribute |
| plan_executor._clear_consent | plan_executor::clear_consent | |
| plan_executor._norm | plan_executor::norm | |
| plan_executor._named_field | plan_executor::named_field | |
| plan_executor._pick_field | plan_executor::pick_field | |
| task_router.Plan | task_router::Plan | |
| task_router.installed_apps | task_router::installed_apps, installed_apps_in | |
| task_router._strip_filler | task_router::strip_filler | |
| task_router._display_names | task_router::display_names | |
| task_router.match_app | task_router::match_app | |
| task_router._quit | task_router::quit | |
| task_router._first_clause | task_router::first_clause | |
| task_router._launch | task_router::launch | |
| task_router._search | task_router::search | |
| task_router.find_app_mention | task_router::find_app_mention | |
| task_router.app_mentions | task_router::app_mentions | |
| task_router._explicit_search | task_router::explicit_search | |
| task_router._consequential | task_router::consequential | |
| task_router.classify | task_router::classify | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
(merged from parity/*.md at integration)

## Boundaries
(merged from parity/*.md at integration)

## Differences
- Python passes one adapter as both `senses=` and `effectors=`; the Rust traits take `&self` so one adapter can be
  lent as both, and the adapter keeps its state in RefCells.
- `type(e).__name__` in result lines comes from `LaneError.kind` (lib.rs `error_kind`).
- `format_line(status)` and `rank_candidates(kinds=)` take enums, so Python's ValueError for an unknown string
  cannot happen.
(more merged from parity/*.md at integration)
