# eve-chief-card
Status: complete

The chief of staff's record: the Card, its strict parse from the brain's `take_on` arguments, the add-only door,
revisions, the backbrief, and the stored JSON form. One Python module, ported into `src/lib.rs`; the Python
semantics it leans on (whitespace, `repr`, `casefold`, `int()` on digit strings, `strptime`, `os.path.realpath`)
are in the private `src/py.rs`. `CONSEQUENTIAL` comes from eve-desktop-lanes (`task_router::CONSEQUENTIAL`).
`cargo test -p eve-chief-card`: 32 tests pass (26 integration, 6 unit).

## Modules
| Python | Rust |
|---|---|
| chief_card.py | src/lib.rs (+ src/py.rs, private Python-semantics helpers) |

## API
| Python | Rust | Notes |
|---|---|---|
| KINDS, STATUSES, TERMINAL, WAITING_FOR, PHASE_KINDS_LIVE, PHASE_STATUSES, DOORS, ACT_CANNOT_ASK, DONE_KINDS_LIVE, RESULT_STATUSES, EVIDENCE_KINDS, NUDGE_LEVELS, TAKE_ON_KEYS, PHASE_KEYS, CHECK_KEYS, COMMITMENT_CHECKS | same names, `&[&str]` | |
| CHECK_NEEDS_PHASE | CHECK_NEEDS_PHASE | `&[(check, phase)]` pairs (Python dict) |
| MAX_* , MIN_UI_WORDS, DEADLINE_FORMATS, CUE_FORMAT, DEFAULT_BUDGET_*, BUDGET_WARN_AT, REMINDER_WAIT_MIN | same names | counts are `usize` (MAX_READ_LINKS `u64`), minutes `i64`, dollars `f64` |
| _ID, _WATCH_ID | private `ID`, `WATCH_ID` | `\n?$` keeps Python's `$`-before-a-final-newline |
| class CardError | CardError(String) | thiserror; from_json's error |
| class Refusal | Refusal { reason } | |
| class Live, LIVE | Live { phases, checks }, Live::new, Live::default, LIVE | Vec<String> so a caller can narrow |
| class Check (+ describe) | Check, Check::new, Check::describe | |
| class Evidence | Evidence { kind, r#ref } | key `ref` in JSON |
| class PhaseResult | PhaseResult, PhaseResult::new(status, evidence) | |
| class Approval | Approval, Approval::new(token, rev, at) | `what` defaults to "" |
| class Phase | Phase { n, r#do, … }, Phase::new(n, do, goal) | Python defaults for the rest |
| class Cue | Cue | |
| class Budget | Budget (Default = read budget) | |
| class Spent | Spent (Default) | |
| class Source | Source (Default door "telegram") | |
| class Nudges | Nudges (Default level "push") | |
| class Card (+ terminal, phase) | Card, Card::new, Card::terminal(), Card::phase(n) | `dataclasses.replace` is struct-update syntax |
| verbs | verbs -> BTreeSet<String> | Python frozenset; matched words casefolded and space-squashed |
| door | door(goal, raised, slugs, can_ask), goal_door(goal) | goal_door is door() with Python's defaults |
| higher | higher | |
| grounded | grounded | |
| class _Refused | ParseError::Refusal (private `refused`) | the internal exception becomes the Err |
| _line | private line | |
| _need | private need | |
| _when | private when (+ strptime/strftime) | CPython 3.13 `_strptime` patterns, matched with leftmost-first priority as `re.match` |
| at_time | at_time -> Option<NaiveDateTime> | naive local datetime as Python's |
| home_path | home_path -> Result<Option<PathBuf>, ValueError> | Err is the ValueError Python raises for a NUL in the path |
| _check | private check | |
| _phase | private phase | |
| default_budget | default_budget | |
| _fields | private fields | |
| parse | parse(args, said, &ParseOptions) -> Result<Card, ParseError> | ParseOptions carries live, door_name, said_ref, now, home with Python's defaults |
| missing | missing -> Vec<&str> | |
| revise | revise(card, patch, said, live, now, home) -> Result<Card, ParseError> | patch is a JSON object map |
| reopen | reopen(card, now) | |
| to_args | to_args -> Map<String, Value> | |
| _money | private money | |
| _KIND_VERB, _OPENING_VERBS | private KIND_VERB, OPENING_VERBS | |
| _step | private step | |
| backbrief | backbrief | |
| to_json | to_json -> Value | keys in dataclass field order |
| _num | private num | |
| _str | private string | |
| _opt_str | private opt_string | |
| _result_from | private result_from_opt | |
| result_from | result_from -> Result<PhaseResult, CardError> | |
| _phase_from | private phase_from | |
| from_json | from_json -> Result<Card, CardError> | checks in Python's evaluation order, so the first problem named is the same |
| card_number | card_number -> u64 | |
| is_watch_id | is_watch_id | |
| (Python's ValueError out of parse) | ValueError, ParseError::ValueError | `int()` on a non-decimal digit string or one past 4300 digits, `lstat` on a NUL; same message text |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_chief_record.py (card half) | test_a_whole_desk_job_parses_with_its_doors_budget_and_backbrief, test_the_card_keeps_a_reference_to_the_words_never_the_words, test_one_bad_phase_refuses_the_whole_card, test_the_parse_is_strict, test_a_card_is_made_only_in_a_turn_with_the_owners_words, test_a_commitment_has_no_phases_a_cue_and_only_the_owners_checks, test_a_missing_end_state_or_a_question_makes_the_card_wait, test_the_live_set_is_the_whole_offer, test_file_exists_takes_a_home_path, test_order_the_picked_desk_is_one_way_and_compare_desks_is_two_way, test_the_brain_can_raise_a_door_but_never_lower_one, test_grounded_needs_the_consequential_word_in_the_owners_words, test_the_door_is_add_only_and_a_revision_cannot_lower_it, test_a_revision_cancels_approvals_keeps_state_and_never_changes_the_kind, test_a_phase_inserted_at_the_front_keeps_every_phase_that_ran, test_a_stored_card_round_trips_and_a_bad_one_is_refused, test_routine_acts_use_the_executors_live_confirmation_boundary, test_a_stored_routine_two_way_act_keeps_its_door | same names in tests/chief_card.rs | the ledger tests (Ledger, events, due, for_turn, matching, redact, the P3 file attacks) belong to eve-chief-ledger |
| test_chief_receipt.py | test_a_stored_result_is_checked_strictly | test_a_stored_result_is_checked_strictly | the rest test chief_receipt (eve-chief-receipt) |
| test_link_chat.py | the chief_card.verbs control in the job-lead test | test_verbs_positive_control | the rest test link_chat |
| test_chief.py, test_chief_desk.py, test_chief_reflect.py, test_chief_replan.py, test_model_research_roles.py, test_routine_confirmations.py | read for behavior (backbrief text, door floors) | covered by the tests above | they test chief, desk, reflect, replan and routing through the card; ported with those crates |
| (new, against CPython 3.13 output) | | refusal_messages_name_values_as_python_repr_does, dates_are_read_as_strptime_reads_them, python_truthiness_and_equality_carry_over, the_backbrief_reads_as_python_writes_it, ids_and_stored_fields_follow_python, reopen_and_to_args_follow_python; py.rs unit tests | |

## Boundaries
None.

## Differences
- `backbrief` of a commitment whose cue is within 45 minutes of the end of year 9999 leaves out the "at the latest"
  clause; Python raises OverflowError there.
- `to_json` writes every float field as a float (`5.0`) where Python writes an int a caller passed (`Approval("t", 1,
  5)` → `5`), and a non-finite float as `null` where Python writes `NaN`; both read back the same through `from_json`.
- Integers beyond i64 in a stored card (`rev`, a phase's `n`, an approval's `rev`) are refused by `from_json`;
  Python loads them. `int()` of a huge stored float (`budget.wall_min`, `nudges.sent/ignored`) saturates at i64.
  serde_json reads an integer past u64 as a float, so a refusal naming one shows it in float form.
- The Unicode tables (`\d`, `str.isdecimal`, `str.isprintable`) are the Rust crates' Unicode version, not CPython
  3.13's 15.1; the digit-but-not-decimal table is CPython 3.13's.
- `home_path` with a relative home folder and no readable working directory resolves against `/`; Python raises
  the OSError from `getcwd`.
