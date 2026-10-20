# eve-chief-replan
Status: complete

The chief's replanner: when a replan is due (`triggers`), the request body sent to the model (`request`), the
strict reading of the reply (`parse`), and what a reply may change (`apply`). Ported as one module in `src/lib.rs`.
It uses eve-chief-card for the Card, Phase, Refusal, the door (`goal_door`) and the caps.
`cargo test -p eve-chief-replan`: 22 tests pass (18 integration, 4 unit).

## Modules
| Python | Rust |
|---|---|
| chief_replan.py | src/lib.rs |

## API
| Python | Rust | Notes |
|---|---|---|
| MODES, DEFAULT_MODE, ENV | MODES, DEFAULT_MODE, ENV | |
| MAX_REPLANS, CEILING_USD, CEILING_SECS | same names | `usize`, `f64`, `f64` |
| AFTER, ADDABLE, DECISIONS, TRIGGERS | same names, `&[&str]` | |
| MAX_WHY_CHARS, MAX_DONE_TEXT, MAX_OUTPUT_TOKENS | same names | |
| _LINK, _WORDS | private LINK, WORDS | LINK's `\S` excludes FS/GS/RS/US, as Python's does |
| mode | mode() (process env), mode_in(&HashMap) (`environ=`), mode_of(Option<&str>) | Python whitespace strip, lowercase |
| triggers | triggers(card, phase, outcomes, retrying) -> Vec<&'static str> | |
| fingerprint | fingerprint(do, goal) | casefold exact for the ASCII words kept; golden values from CPython |
| INSTRUCTIONS | INSTRUCTIONS | verbatim |
| SCHEMA | SCHEMA (LazyLock<Value>) | key order as Python's dict |
| _plain | private plain | |
| remaining | remaining -> Vec<&Phase> | |
| request | request(card, phase, why, model, effort, lessons) -> Value | `input` is `json.dumps(..., ensure_ascii=False)` byte for byte (private `dumps`, Python separators) |
| class Step | Step { keep: i64, do, goal }, Step::kept, Step::new | |
| class Decision | Decision { decision, why, steps }, Decision::new | |
| parse | parse -> Option<Decision> | |
| class Plan | Plan { phases, kinds_before, kinds_after, added, replaced } | tuples become Vecs |
| apply | apply(card, phase, d, tried) -> Result<Option<Plan>, Refusal> | `Ok(None)` is Python's None ("no change"); `Err` the Refusal |
| _checked | private checked | |
| _plan | private plan_of | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_chief_replan.py | test_triggers_fire_on_news_and_failure_only, test_keep_and_the_same_list_change_nothing, test_a_revise_adds_a_read_step_before_the_act_and_renumbers, test_a_revise_that_breaks_a_rule_is_refused, test_a_finish_is_refused_while_the_owners_act_remains_and_allowed_without_one, test_a_finish_that_leaves_a_check_with_nothing_to_read_is_refused, test_a_failed_step_is_replaced_in_place_never_kept_or_repeated, test_a_revise_past_the_step_cap_is_refused, test_a_reply_outside_the_shape_is_no_decision, test_the_request_carries_the_card_never_the_owners_words_and_only_live_kinds, test_the_switch_reads_on_shadow_and_off_and_falls_back_to_the_default, test_the_default_is_the_blind_evals_verdict, test_a_replan_cancels_a_yes_given_before_it (plan half) | same names in tests/chief_replan.rs (the parametrized cases as loops); the default check recomputes tools/chief_replan_eval.py's bar from the kept rows and the holdout's sha256 | test_a_replan_adds_a_read_step_that_runs_and_the_act_still_asks_its_go, test_attack_a_page_that_talks_the_replan_into_an_act_changes_nothing, test_a_failed_research_is_replaced_and_the_card_goes_on, test_positive_control_without_the_replanner_a_failed_research_closes_the_card, test_a_finish_takes_the_rest_out_and_the_oracles_still_decide, test_replans_stop_at_the_limit, test_shadow_logs_what_it_would_do_and_changes_nothing, test_attack_the_owner_changes_the_card_while_the_replan_is_out, test_the_replan_call_is_paid_from_the_card_and_refused_at_its_cap, and the ledger half of test_a_replan_cancels_a_yes_given_before_it: they drive Chief._replan and Ledger.replan, ported with eve-chief and eve-chief-ledger |
| (new, against CPython 3.13 output) | | test_refusal_messages_are_pythons, test_the_request_is_the_bytes_python_sends, test_a_reply_inside_the_shape_is_read_around_its_prose, test_a_new_goal_is_cut_to_the_line_cap_before_its_door_and_fingerprint, test_triggers_skip_a_terminal_card_an_act_and_a_one_way_step; unit tests for casefold/fingerprint, dumps, plain, mode | |
| test_chief_desk.py (tests_ref) | uses DEFAULT_MODE in its env | DEFAULT_MODE | the desk tests belong to eve-chief-desk |

## Boundaries
None.

## Differences
- `parse` of a reply whose `keep` is an integer outside i64: Python keeps the exact integer and `apply` refuses it
  as "step N is not a remaining step it may keep" with N in full. Rust reads an integer in (i64::MAX, u64::MAX] as
  i64::MAX (the same refusal, naming 9223372036854775807), and an integer past u64 or below i64::MIN, which
  serde_json reads as a float, as no decision. Either way the card does not change.
- `parse` of a reply carrying an escaped lone surrogate (`"\ud800"`) in a string: Python accepts it, serde_json
  refuses the reply (no decision). A reply nested past serde_json's depth of 128 is no decision; Python reads it,
  then refuses it for its shape too (no nesting fits the schema), or raises RecursionError past about 1000 levels.
