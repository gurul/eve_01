# fast-lane unit (fast_lane.py, lane_router.py, decision_lane.py)

Rust: src/fast_lane.rs, src/lane_router.rs, src/decision_lane.rs. Tests: tests/fast_lane.rs (81 tests; all pass
once the ax-candidates and plan-and-task units are merged, because the lane calls their ax_candidates and
task_router functions).

## API additions (beyond the skeleton's rows)
| Python | Rust | Notes |
|---|---|---|
| fast_lane.run_delegate (exception path) | fast_lane::try_run_delegate | like run_delegate, but a snapshot that fails after step 0 comes back as `Err`, as Python raised it |
| fast_lane.run_script (exception path) | fast_lane::try_run_script | run_script over try_run_delegate; lane_router::route uses it |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_fast_lane.py | all 36 | same names in tests/fast_lane.rs; plus test_a_raising_decider_is_model_invalid_with_its_kind, test_helpers_valid_key_clip_and_ax_changed | `format_line("maybe")` raising ValueError: LaneStatus is an enum, so test_format_line_templates_and_escaping asserts the enum covers STATUSES instead |
| test_lane_first.py | all 16 | same names | none |
| test_decision_lane.py | 15 route/menu/evidence tests + test_pixel_checker_has_positive_and_negative_controls | same names; plus test_gates_pick_scores_and_evidence | test_adapter_observation_uses_captured_retina_frame_in_screen_points (owned by desktop-helpers); test_benchmark_counts_abstentions_separately_from_wrong_presses (tools/decisions_eval.py, not a crate module); test_daemon_wiring_only_tries_decisions_after_zero_input_keyword_mismatch (desk_lane, another crate). test_unknown_effect_is_partial_and_named_in_codex_handoff asserts the `clicked` that desk_lane.codex_note reads, not codex_note itself. The pixel checker test uses a local copy of _LaneAdapter.image_matches, because that function belongs to desktop-helpers. |
| test_jev_step.py (tests_ref) | the 7 test_jev_mode_* tests (run_delegate in jev mode) | same names; they run over the real eve_typed_ask::ask_jev_step with a fake predict | the asker, gate and fit tests belong to eve-typed-ask |
| test_jev_grouped.py (tests_ref) | test_run_delegate_offers_a_grouped_asker_up_to_its_max_and_a_plain_one_25 | same name | the grouped-asker tests belong to eve-typed-ask |
| test_agent_lane_first.py (tests_ref) | the LANE_FIRST_DEFAULT pin | test_shipped_defaults_follow_the_evals | the planner/worker wiring belongs to computer_agent |
| test_fastlane_eval.py (tests_ref) | the FAST_LANE_DEFAULT / LANE_FIRST_DEFAULT constants | test_shipped_defaults_follow_the_evals | the eval harness is tools/fastlane_eval.py, not a crate module |
| (new) | — | test_a_snapshot_failure_after_input_is_named_not_hidden | covers the error path described under Differences |

## Boundaries
None. The lane is pure code over the Senses / Effectors / LaneDecider / StepAsk / DecisionSenses / Evaluate traits.

## Differences
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
- `RouteResult.to_dict()["ms"]` rounds half away from zero; Python's `round(ms, 1)` rounds half to even. Step
  timings (`round(x, 2)`) differ the same way. These are timing values only.
