# eve-desk-lane
Status: complete

The desk lane: keyword-decided clicks on the frontmost window, tried before Codex. `cargo test -p eve-desk-lane`:
19 tests pass.

## Modules
| Python | Rust |
|---|---|
| desk_lane.py | src/lib.rs (+ src/pyautogui.rs, the PyAutoGUI macOS backend `_real_helpers` configures) |

## API
| Python | Rust | Notes |
|---|---|---|
| desk_lane.ENV | ENV | "EVE_DESK_LANE" |
| desk_lane.DEFAULT | DEFAULT | true |
| desk_lane.MAX_GOAL_CHARS | MAX_GOAL_CHARS | 400, counted in characters |
| desk_lane.enabled | enabled(environ) | `None` reads the process environment; same truthy / falsy sets, anything else DEFAULT |
| desk_lane._real_helpers | real_helpers | `Helpers::new(PyAutoGui(RealQuartz), Backends::default())`; FAILSAFE true, PAUSE 0.05 (pyautogui.rs) |
| desk_lane._trusted | trusted | `AXIsProcessTrusted()` through objc2-application-services; false off the Mac |
| desk_lane.DeskLane | DeskLane (+ DeskHelpers trait, implemented for desktop_helpers::Helpers) | `DeskLane::new()` is `DeskLane()`; `with(make_helpers, trusted)` the test constructor; `with_decisions` adds the `decisions.enabled` / `decisions.from_env` seams; Clone shares the one lane |
| DeskLane._attempt | Inner::attempt_blocking (private) | under a parking_lot Mutex, as Python's threading.Lock |
| DeskLane.attempt | DeskLane::attempt (async) | `asyncio.to_thread` → `tokio::task::spawn_blocking`; returns `Option<Map<String, Value>>` |
| desk_lane.codex_note | codex_note(&Map) | same text, Python `repr()` quoting |
| (pyautogui.click / press / hotkey / size / failSafeCheck / isShiftCharacter / keyboardMapping) | pyautogui::PyAutoGui (Gui impl), is_shift_character, keyboard_mapping | the library calls Helpers makes, ported from pyautogui 0.9.54 `__init__.py` and `_pyautogui_osx.py` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_desk_lane.py | test_the_lane_runs_lane_first_on_one_helpers_for_the_daemons_life, test_without_accessibility_the_lane_is_off_and_never_builds_helpers, test_a_lane_that_cannot_start_is_off_for_good, test_a_goal_is_capped_before_it_reaches_the_router, test_codex_note_names_the_clicks_only_when_there_were_some, test_the_daemon_makes_one_lane_and_the_switch_turns_it_off (the `enabled` half) | same names in src/tests.rs; the enabled half is test_the_switch_turns_it_off_and_defaults_on | the ReflexFirstAgent rig tests (test_a_complete_lane_answers_and_codex_never_starts, test_a_partial_lane_hands_codex_the_task_and_what_was_clicked, test_a_lane_that_did_nothing_leaves_the_task_to_codex_unchanged, test_a_lane_that_raises_never_costs_the_task, test_a_stop_while_the_lane_runs_stops_before_codex, test_a_bare_launch_is_still_the_launch_reflex_and_never_the_lane, test_a_complete_lane_without_a_sentence_is_not_an_answer) test app_reflex and belong to eve-app-reflex; the Daemon._desk half and test_the_chiefs_codex_floor_never_gets_the_lane test daemon.py and belong to eve-daemon |
| test_decision_lane.py (tests_ref) | test_daemon_wiring_only_tries_decisions_after_zero_input_keyword_mismatch; the codex_note assertion of test_unknown_effect_is_partial_and_named_in_codex_handoff | same names | the route tests are in eve-desktop-lanes |
| (new) | | test_decisions_off_or_without_a_client_keep_the_keyword_result, test_a_log_the_result_already_carries_is_kept, test_a_lane_that_panics_or_answers_no_dict_never_costs_the_task, test_real_helpers_answer_through_the_trait; pyautogui.rs: 7 tests of the click / press / hotkey event sequences, fail-safe corners and key map over a fake Quartz | |

## Boundaries
None. PyAutoGUI's macOS backend is ported (src/pyautogui.rs): CGEventCreateMouseEvent / CGEventCreateKeyboardEvent
posted at kCGHIDEventTap with no source, CGDisplayPixelsWide/High(CGMainDisplayID()) for the size, macOS only. Off the
Mac `trusted()` is false (Python: ApplicationServices does not import), so the lane is off before `real_helpers`
would be asked; `real_helpers` answers `ModuleNotFoundError: No module named 'Quartz'` there.

## Differences
- The helpers are a trait (`DeskHelpers`) where Python duck-typed any object with bind / begin / lane_first /
  decisions_first. `bind` takes the log callable only; the display callable is always the no-op Python passed.
- A panic inside the helpers is the exception: `attempt` logs `desk lane: failed (PanicException); Codex takes it`
  and answers None. The lock is released (parking_lot does not poison) and the helpers are kept, as Python's
  `with self._lock` did after an exception.
- `codex_note` over a `clicked` that is neither a list, a string, an object nor null/missing answers "" where Python's
  iteration raised TypeError; the lane never writes such a value.
- pyautogui's cursor position is read from an empty CGEvent's location (global top-left points) instead of
  `NSEvent.mouseLocation()` flipped against `CGDisplayPixelsHigh(0)`: the same point, truncated the same way. Only
  the left button, no interval and no tween are ported, as Helpers never passes others. The fail-safe corners are
  computed when the helpers are built (Python: when pyautogui was first imported).
- Python's `_pyautogui_osx._specialKeyEvent` (media keys through NSEvent) is unreachable from `press` / `hotkey`
  (those names are not in keyboardMapping), so it is not ported.
