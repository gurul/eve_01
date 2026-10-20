# eve-desktop-lanes
Status: complete

The desktop lanes: the code that clicks labelled controls on the owner's Mac without a planner turn. Seven Python
modules, one Rust module each. `cargo test -p eve-desktop-lanes`: 252 tests pass (1 live test ignored).

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
| plan_executor.Entry | plan_executor::Entry | `to_dict` keeps the key order; note cut to 200 chars, ms `round(…, 1)` |
| plan_executor.PlanResult | plan_executor::PlanResult | `applied` is a method; `to_dict` keeps the key order |
| plan_executor._holds | plan_executor::holds | `wait` explicit (Python default EXPECT_WAIT_SECS) |
| plan_executor._confirm_label | plan_executor::confirm_label | |
| plan_executor._reason | plan_executor::reason | |
| plan_executor._say | plan_executor::say | |
| plan_executor.run_plan | plan_executor::run_plan + PlanOptions | keyword arguments are PlanOptions (Default = Python's defaults) |
| plan_executor._grant | plan_executor::grant | `hasattr(effectors, "approve")` is the `Effectors::grant` method (no-op by default) |
| plan_executor._clear_consent | plan_executor::clear_consent | `getattr(effectors, "clear_consent")` is `Effectors::clear_consent` returning None |
| plan_executor._norm | plan_executor::norm | |
| plan_executor._named_field | plan_executor::named_field | returns a clone; the lookarounds are an explicit ASCII [a-z0-9] boundary check |
| plan_executor._pick_field | plan_executor::pick_field | Python's `is` identity is the pool index |
| task_router.Plan | task_router::Plan | `complete` is a method; `code()` reproduces repr() quoting and `quote_plus` |
| task_router.installed_apps | task_router::installed_apps, installed_apps_in | the default call (cached, Finder added) and the dirs/listdir call (uncached, no Finder) are two functions |
| task_router._strip_filler | task_router::strip_filler | |
| task_router._display_names | task_router::display_names | IndexMap keeps dict order; `str.title()` reproduced |
| task_router.match_app | task_router::match_app | |
| task_router._quit | task_router::quit | |
| task_router._first_clause | task_router::first_clause | |
| task_router._launch | task_router::launch | |
| task_router._search | task_router::search | |
| task_router.find_app_mention | task_router::find_app_mention | |
| task_router.app_mentions | task_router::app_mentions | |
| task_router._explicit_search | task_router::explicit_search | |
| task_router._consequential | task_router::consequential | `CONSEQUENTIAL.match` is a find that must start at 0 |
| task_router.classify | task_router::classify | `model` / `quit_model` are `RouterModel` closures; an Err or a panic abstains |
| (urllib.parse internals: `_check_bracketed_netloc`, `_check_bracketed_host`, the ValueError) | `try_urlsplit`, `check_bracketed_netloc`, `check_bracketed_host` (private) | `urlsplit` is the never-failing public face; `is_account_change` uses the checked one so a ValueError fails closed as the Python's `except` does |
| `SENSITIVE_LABEL.search(text) is not None` | `SENSITIVE_PRESENT` (private `regex::Regex`) | the same existence question with the lookarounds as consumed boundary characters or text edges; `SENSITIVE_LABEL` stays public as the fancy-regex lookaround pattern |
| `ax_walk.scalar` / `fetch` | `AxScalar`, `MacAx::scalar`, `WalkNode`, the `fetch` closure in `ax_walk_with` | |
| pyobjc bridging | `ax_ffi::convert` | CFBoolean → Bool, CFNumber → Int or Float by its type, CFString → Str, AXValue point/size → Point/Size (AXError, range, rect → Null), CFArray → Array, CFAttributedString → its string, AXUIElement → Element |
| desktop_helpers.EMPTY_FRONT | desktop_helpers::Front::default() | |
| desktop_helpers.ax_frontmost (pid half) | desktop_helpers::ax_frontmost_for_pid | the test seam for "every probe 0 → EMPTY_FRONT" |
| Helpers._acting | Helpers::gui_action (gui_click / gui_hotkey / gui_press use it) | the worker routes every other ACTION_NAMES call (write, scroll, dragTo, ...) through gui_action |
| Helpers._action_seq, _last_click | Helpers::action_seq(), last_click() | read-only accessors for the tests and the worker |
| monkeypatch.setenv("EVE_AX_PRESS") | Helpers::ax_press_environ | None reads the process environment, as Python does |
| _LaneAdapter._thumb_before / _thumb0 / _snapshot (assigned by tests) | LaneAdapter::set_thumbs, set_snapshot | |
| fast_lane.run_delegate (exception path) | fast_lane::try_run_delegate | like run_delegate, but a snapshot that fails after step 0 comes back as `Err`, as Python raised it |
| fast_lane.run_script (exception path) | fast_lane::try_run_script | run_script over try_run_delegate; lane_router::route uses it |
| (round(x, n)) | py_round_n (lib.rs) | Python's correctly rounded `round(x, n)`, ties to even, through Rust's fixed-precision formatting |

Every regex constant (CONSEQUENTIAL, TELL_ME, TYPING, POLITE, PRONOUN_QUERY, LOCAL_SCOPE, FILLER, LAUNCH_VERB, SEARCH_LEAD,
SEARCH_TAIL, NAMES_WEB, IN_BROWSER, AND_THEN, VAGUE_QUERY, QUIT_VERB, QUIT_ALL_TARGET, FORCE, QUIT_WORD, IN_APP_SEARCH,
EXPLICIT_SEARCH) is a `LazyLock` with the Python pattern verbatim under `(?i)`; LOCAL_SCOPE is `fancy_regex` for its
negative lookahead. The dict constants (GERUND, BROWSERS, APP_ALIASES) are ordered slices; WRITE_APPS and SHELL_APPS are
slices (Python frozensets, only membership is read).

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_ax_candidates.py | all 27 | same names; plus test_sensitive_agrees_with_the_lookaround_pattern_and_survives_long_labels, test_walk_keeps_each_candidates_live_element, test_walk_without_a_pid_or_a_window_is_empty, test_urlsplit_parse_qsl_unquote_match_python, test_fixture_dict_round_trip_keeps_numbers | the `is_sensitive(None)` / `is_sensitive(42)` asserts and `rank_candidates(kinds="anything")` ValueError: labels are `&str` and `kinds` is an enum, so neither input can be written |
| test_fixtures_ax.py (tests_ref) | all 5 | test_fixtures_exist (added: the parametrised tests loop over the files, so an empty glob must fail), test_no_fixture_is_in_both_sets, test_fixture_rederives_from_its_raw_dump (loops over the 13 fixtures; compares with Python's `==` and also `Snapshot ==`), test_fixture_cases_point_at_real_candidates, test_denylist_catches_a_known_positive_control, test_no_fixture_carries_owner_data | none |
| test_side_effect_links.py (tests_ref) | the is_account_change / needs_confirmation / rank tests (8 of 9, parametrised ones as loops) | same names; `page()` builds the Snapshot browser_lane.snapshot_from_page would | test_the_browser_click_refuses_an_upvote_link_without_a_yes (browser_lane._Page, another crate); test_non_strings_fail_closed keeps only the "missing href is no href" assert (None / 42 labels cannot be written) and adds the urlsplit-ValueError fail-closed case |
| test_side_effect_precision.py (tests_ref) | 13 of 14: the is_account_change / needs_confirmation / Snapshot.href / rank tests, test_gate_asks_for_an_href_only_account_link, test_the_lane_confirms_an_href_only_vote_link_instead_of_clicking_it (tests/ax_candidates.rs), test_the_outline_hint_uses_the_lanes_own_confirmation_rule (tests/desktop_helpers.rs) | same names | test_the_browser_lanes_snapshot_carries_query_bearing_hrefs_to_the_gate (browser_lane.snapshot_from_page, the browser-lane crate) |
| test_desktop_helpers.py | all 30 | tests/desktop_helpers.rs, same names (monkeypatch of `_ax_focused_pid` etc. → `focused_pid_from` with injected probes; `h._element_at = …` → Backends at construction with shared state) | none |
| test_ax_press_fallback.py (tests_ref) | all 11 (the parametrized one runs its 6 cases in a loop) | same names in tests/desktop_helpers.rs, plus test_ax_press_on_reads_the_switch; test_refs_ride_the_snapshot_and_never_reach_its_dict is in tests/ax_candidates.rs | none |
| test_decision_lane.py | test_adapter_observation_uses_captured_retina_frame_in_screen_points, test_pixel_checker_has_positive_and_negative_controls | same names in tests/desktop_helpers.rs (ax_snapshot monkeypatch → Backends.ax_snapshot) | the rest are in tests/fast_lane.rs (next rows) |
| test_desktop_worker.py | the Helpers half of the settle de-duplication and timing tests, test_the_jev_asker_is_configured_only_by_the_owners_switches (Helpers part) | test_open_app_auto_screenshot_settles_once, test_raw_click_still_settles_up_to_1_5s, test_timed_out_settle_reuses_the_latest_frame_without_a_second_wait, test_explicit_settle_timeout_is_false_and_does_not_wait_again_for_video, test_timed_out_settle_is_not_reused_after_a_later_input, test_timed_out_settle_is_not_reused_when_the_frame_is_stale, test_settle_is_not_reused_after_a_later_input, test_reply_carries_timing_with_helpers, test_the_jev_asker_is_configured_only_by_the_owners_switches | the worker's execute/serve/encode parts belong to the desktop worker's crate; the counts of `_settle` calls are asserted through the fake clock's sleeps instead of a spy |
| (new) | — | test_the_acting_wrap_covers_every_input_call, test_what_matches_and_norm, test_off_mac_backends_fail_like_the_missing_import, live_capture_and_ocr_read_the_real_screen (#[ignore = "live: …"]) | |
| test_fast_lane.py | all 36 | same names in tests/fast_lane.rs; plus test_a_raising_decider_is_model_invalid_with_its_kind, test_helpers_valid_key_clip_and_ax_changed | `format_line("maybe")` raising ValueError: LaneStatus is an enum, so test_format_line_templates_and_escaping asserts the enum covers STATUSES instead |
| test_lane_first.py | all 16 | same names | none |
| test_decision_lane.py | 15 route/menu/evidence tests + test_pixel_checker_has_positive_and_negative_controls | same names; plus test_gates_pick_scores_and_evidence | test_adapter_observation_uses_captured_retina_frame_in_screen_points (in tests/desktop_helpers.rs); test_benchmark_counts_abstentions_separately_from_wrong_presses (tools/decisions_eval.py, not a crate module); test_daemon_wiring_only_tries_decisions_after_zero_input_keyword_mismatch (desk_lane, another crate). test_unknown_effect_is_partial_and_named_in_codex_handoff asserts the `clicked` that desk_lane.codex_note reads, not codex_note itself. The pixel checker test also runs here, over LaneAdapter::image_matches. |
| test_jev_step.py (tests_ref) | the 7 test_jev_mode_* tests (run_delegate in jev mode) | same names; they run over the real eve_typed_ask::ask_jev_step with a fake predict | the asker, gate and fit tests belong to eve-typed-ask |
| test_jev_grouped.py (tests_ref) | test_run_delegate_offers_a_grouped_asker_up_to_its_max_and_a_plain_one_25 | same name | the grouped-asker tests belong to eve-typed-ask |
| test_agent_lane_first.py (tests_ref) | the LANE_FIRST_DEFAULT pin | test_shipped_defaults_follow_the_evals | the planner/worker wiring belongs to computer_agent |
| test_fastlane_eval.py (tests_ref) | the FAST_LANE_DEFAULT / LANE_FIRST_DEFAULT constants | test_shipped_defaults_follow_the_evals | the eval harness is tools/fastlane_eval.py, not a crate module |
| (new) | — | test_a_snapshot_failure_after_input_is_named_not_hidden | covers the error path described under Differences |
| test_plan_executor.py | 26 walking/approval/form/consent tests | the same 26 names | the 5 contract tests (`test_a_plan_the_code_cannot_fully_read_is_not_a_plan`, `test_where_text_came_from_is_checked_not_trusted`, `test_text_inside_a_longer_word_is_not_the_owners`, `test_the_planner_can_decline`, `test_the_planner_is_shown_labels_and_roles_never_the_title`) test plan_contract only and are ported in eve-plan-contract |
| test_task_router.py | classify, installed_apps, app_mentions, is_question, routing doc | test_a_bare_launch_of_an_installed_app_is_a_complete_reflex, test_a_launch_with_more_to_do_hands_the_rest_to_the_planner, test_what_looks_like_a_launch_and_is_not, test_an_explicit_web_search_is_a_reflex_with_the_query_found_by_code, test_what_looks_like_a_web_search_and_is_not, test_a_model_is_asked_only_after_the_gates_and_may_only_add_a_bare_launch, test_installed_apps_lists_real_names_once, test_the_routing_doc_states_the_shipped_defaults_and_names_every_knob, test_is_question_is_one_rule_for_both_routers, test_app_names_said_as_one_word_and_every_mention | test_jev_is_asked_in_its_own_idiom_and_gated_by_absolute_answers and test_each_model_is_asked_differently_for_a_head_pose test typed_ask (eve-typed-ask); the four agent tests (reflex runs, reflexes off) drive ComputerAgent (computer-agent crate) |
| test_plan_success_guard.py (ref) | the run_plan tests | test_a_success_check_that_holds_only_after_the_steps_is_complete, test_a_success_check_already_true_before_step_1_is_rejected, test_a_resumed_plan_keeps_the_rejection_it_was_handed, test_the_pre_check_does_not_wait_for_a_check_that_is_false, test_duck_duck_go_by_google_is_not_done, test_duck_duck_go_by_duck_duck_go_is_done, test_the_request_check_replaces_the_planners_choice, test_a_request_check_already_true_before_step_1_is_rejected, test_no_outcome_falls_back_to_the_planners_check | url_query / request_check / round-trip tests are plan_contract's; the agent and desktop_worker resume tests belong to their crates |
| test_plan_url_expectation.py (ref) | test_exact_url_requires_fresh_snapshot_and_does_not_replace_title_predicate | same name | url_matches / parse / plan_request tests are plan_contract's; the live browser test is browser_lane's |
| test_quit_reflex.py (ref) | classify quit parts | test_rules_take_a_bare_quit (all 8 cases), test_everything_else_is_never_a_quit (all 11), test_jev_is_asked_only_behind_the_quit_word_gate | decide_quit / fit / questions are typed_ask's; the wrapper and quit_all are app_reflex's |
| test_site_names.py (ref) | test_the_router_sees_joined_names | same name | the joiner and site table are browser_intent's |
| test_vault_fill.py (ref) | test_the_executor_runs_a_fill_through_the_page_and_stops_on_a_refusal | same name | contract, page and live Chromium tests belong to plan_contract / browser_lane |
| (new) | — | test_the_result_dict_keeps_pythons_keys_and_rounding, test_the_lanes_lines_are_read_back_as_python_reads_them, test_an_unreadable_oracle_is_unknown_except_for_a_url, test_folded_names_are_shown_title_cased_and_the_code_line_quotes_like_python | pin to_dict shape/rounding, the line readers, `_holds`'s unknown-vs-false rule, title-casing and repr() quoting |
| (new, integration) | — | test_route_result_ms_rounds_as_pythons_round (tests/fast_lane.rs) | pins `round(x, n)` parity, ties to even |

Live checks on this Mac (2026-10-19): `eve-ax-candidates --app <name> --json` and `python -m eve_bridge.ax_candidates
--app <name> --json` gave equal `raw`, `snapshot` and `candidates` (secs aside) for Finder, Wispr Flow, Slack and
Google Chrome. `live_capture_and_ocr_read_the_real_screen` (#[ignore]) exercises cg_capture, Frame::pil and
vision_ocr. Differential check during the port: task_router `classify` agreed with the Python on all 965 requests
of bridge/tests/fixtures/routes plus 15 edge cases.

## Boundaries
### ax_candidates
None. The AX walk, NSRunningApplication / NSWorkspace and CGDisplayBounds go through objc2 (`ax_ffi`), macOS only.
Off the Mac `ax_walk` returns the windowless dump, `pid_for_app` 0, `app_identity` ("", "") and
`main_screen_points` None (the Python raised ImportError there).

### desktop_helpers
- Screen capture (CGDisplayCreateImage + CGDataProvider), the window list (CGWindowListCopyWindowInfo) and Vision OCR
  (VNRecognizeTextRequest / VNImageRequestHandler) are objc2 calls in this file's `#[cfg(target_os = "macos")] mod mac`;
  AX and AppKit go through crate::ax_ffi. Off the Mac: cg_capture / vision_ocr / ax_press fail with
  ModuleNotFoundError (pyobjc's missing import), the probes answer 0 and the AX readers None, as the Python's
  `except Exception` branches did.
- pyperclip → arboard (SystemClipboard); subprocess.run → std::process::Command with output drained and a timeout
  (TimeoutExpired, child killed).

### fast_lane, lane_router, decision_lane, plan_executor, task_router
None: pure code over the lane traits.

## Differences
### Whole crate
- Python passes one adapter as both `senses=` and `effectors=`; the Rust traits take `&self` so one adapter can be
  lent as both, and the adapter keeps its state in RefCells.
- `type(e).__name__` in result lines comes from `LaneError.kind` (lib.rs `error_kind`).
- `format_line(status)` and `rank_candidates(kinds=)` take enums, so Python's ValueError for an unknown string
  cannot happen.

### ax_candidates
- `is_sensitive`, `is_account_change` and `needs_confirmation` take `&str` / `&Candidate`, so Python's fail-closed
  branch for a label that is not a string has no caller. `is_sensitive` matches with an automaton
  (`SENSITIVE_PRESENT`) that cannot fail at match time; fancy-regex's VM hits its backtrack limit on long text where
  Python's `re` does not, which would have made long labels "sensitive" for the wrong reason.
- Python's `\s` also matches U+001C..U+001F (`str.isspace()`); Rust's does not. `SPACES_RE`, `URL_RE`'s `\S` and the
  `\s` in the sensitive phrases spell those characters out, so whitespace collapsing and phrase matching agree.
  `\w` / `\d` use Rust's Unicode classes, which differ from Python's only on combining marks.
- A NaN or infinite float cannot sit in a `serde_json::Value`: the walker writes `null` where Python's dump carried
  NaN / Infinity. An infinite frame is no frame either way; a NaN AXValue would read "nan" in Python and "" here
  (the committed fixtures carry one, on an AXScrollBar that is never a candidate). The fixture tests read those
  bare tokens as `null`.
- `urlsplit` does not port `_checknetloc`'s NFKC check (a non-ASCII netloc whose NFKC form introduces `/?#@:`
  raised ValueError, which is_account_change turned into "asks"); there is no Unicode normaliser in this crate's
  dependencies. Bracketed hosts are validated as Python 3.13 does (IPv6 or IPvFuture only).
- `Candidate.frame` is a 4-tuple: `Candidate::from_dict` rejects a frame of any other length (Python built a
  tuple of whatever length it got). A raw dump node that is not an object reads as an empty node (Python raised
  AttributeError).
- A live element is keyed by its `NodePath` (child indices from the root) instead of Python's `id(node)`.
- CLI: argument errors are clap's wording (exit 2, `--help` exit 0, as argparse).
- `str()` of an attribute list in a title slot reads `[<AXUIElement>, ...]` (Python printed pyobjc reprs with
  addresses); no app has been seen to answer a list for AXTitle / AXDescription / AXRole.

### desktop_helpers
- Frame.thumb: Pillow's `convert("L")` is reproduced exactly (fixed-point 601-2 luma); the 1/4 resize uses the image
  crate's CatmullRom (Pillow's default BICUBIC, a = -0.5) with its own edge handling and rounding, so a thumb pixel can
  differ by a grey level or so near an edge. change_box's thresholds and every test's bounds hold.
  Helpers._points uses Lanczos3 (Pillow LANCZOS) likewise.
- `{x!r}` uses a local repr without the Unicode tables: unassigned code points print as themselves instead of `\uXXXX`.
- Helpers.run_plan: a `plan` that is not an object reads as "bad plan: the plan is not an object" (unavailable); Python
  raised AttributeError from `plan.get` before the try.
- Helpers.lane_first: lane_router::route cannot fail in Rust; it returns Python's
  `RouteResult("none", reason="error:<kind>")` itself when a snapshot fails mid-run (see fast_lane below), so
  lane_first's own catch has no trigger and the owner sees the same dict.
- install() returns the names to bind instead of writing into a namespace; the acting wrap is a flag (installed) that
  gui_action honours, so "no double wrapping" holds by construction.
- The lane's `clock=` is the Helpers' own clock read through the adapter (Python passed `self._clock`); the decider and
  step asker are lent to the lane for the duration of a delegate / run_plan call and put back after.
- Backends.run answers the return code (Python's CompletedProcess.returncode is the only field read).

### fast_lane, lane_router, decision_lane
- A snapshot that fails after step 0 (the re-snapshot when too few controls are pressable, the re-snapshot after
  `reobserve`, and the snapshot after settling) raised out of Python's run_delegate. `run_delegate` returns a
  DelegateResult and cannot raise, so it answers `escalate: … reason=no_window ({kind})`, with the applied steps
  still in `steps=` and `last=`. `try_run_delegate` / `try_run_script` return the error instead.
  `lane_router::route` uses them and returns `RouteResult("none", reason="error:{kind}")`: the same dict that
  Helpers.lane_first made when it caught the exception. A failure on the first snapshot is
  `no_window ({kind})` in both languages, as before.
- `decision_lane::Gates::pick` and `_scores` read a non-numeric `risky` / `covered` / `present` / `p` / `margin` as
  that field's default, so the step gets no pick. Python's comparison raised TypeError, which the loop
  caught as `error:TypeError`. Either way the step gets zero input. eve-decisions validates every probability, so
  this case cannot occur from the real client.
- `lane_router::installed_app_names` returns a Vec (case-folded, without repeats, first-seen order), where Python
  returned a frozenset. Membership is the same. When two installed names have the same length, names_other_app picks
  the first one in that order. Python's pick then depended on frozenset iteration order.

### plan_executor, task_router
- run_plan on a click step with neither label_hint nor target: Python's `assert result is not None` raised; Rust ends
  the plan `partial` with reason `no_target` (input count zero). parse_plan never lets such a step through.
- PlanOptions.approved is `HashMap<i64, String>`; a negative key (Python would keep it and never match it) is dropped.
- A router model (`model` / `quit_model`) that panics abstains, as one that returns Err does (Python: any exception).
- `Plan.code()`'s repr() treats control, separator and the common format characters as unprintable; other exotic
  Unicode categories Python's `str.isprintable` also escapes are printed as is (app names and URL-encoded queries never
  carry them).
- installed_apps reads folder names lossily as UTF-8; `~user/…` folders are not expanded (APP_DIRS has only `~/…`).
