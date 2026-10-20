# eve-chief-receipt
Status: complete

The chief's proof: the oracles that decide from evidence whether a card is done, the receipt composed from their
verdicts, and the adapters that turn an executor's output into the one `PhaseResult` shape. One Python module,
ported into `src/lib.rs`; the Python semantics it leans on (`str.strip`/`split`, `casefold`, `int()` on a str,
`str()` and truth of a JSON value, `f"{x:.Nf}"`) are in the private `src/py.rs`. Card, Check, Evidence,
PhaseResult, `home_path` and `is_watch_id` come from eve-chief-card. `cargo test -p eve-chief-receipt`: 27 tests
pass (24 integration, 3 unit).

## Modules
| Python | Rust |
|---|---|
| chief_receipt.py | src/lib.rs (+ src/py.rs, private Python-semantics helpers) |

## API
| Python | Rust | Notes |
|---|---|---|
| OUTCOMES | OUTCOMES | `&[&str]` |
| FAILED_RESULTS | FAILED_RESULTS | `&[&str]` |
| _URL | private `URL` | `\s` is Python's str whitespace (adds FS/GS/RS/US); `\n?$` keeps Python's `$` before a final newline |
| class Verdict | Verdict { check, outcome, why }, Verdict::new | outcome is a String from OUTCOMES |
| PhaseResult, Evidence (re-exported) | `pub use eve_chief_card::{Evidence, PhaseResult}` | |
| _link | private link | |
| _results | private results | |
| read_set | read_set -> BTreeSet<String> | sorted, as chief.py sorts it |
| _gate | private gate | |
| check | check(check, card, &CheckOptions) -> Result<Verdict, ValueError> | CheckOptions carries owner_done, live_watches (`Option<&HashSet<String>>`), home with Python's defaults; Err is the ValueError Python lets out. The read_links count is read as `int()` reads a str: it strips ASCII C whitespace and non-ASCII str whitespace, never FS..US |
| verify | verify(card, &CheckOptions) -> Result<Vec<Verdict>, ValueError> | |
| status_of | status_of -> &'static str | |
| steps_not_run | steps_not_run -> Vec<(usize, String, String)> | |
| card_status | card_status -> &'static str | |
| close | close(card, &CheckOptions) -> Result<&'static str, ValueError> | |
| _minutes | private minutes | `round` half to even, as Python's |
| receipt | receipt -> String | |
| _cap | private cap | `str(text or "")[:limit]` in code points; `str()` of a float is `repr`, shortest digits with ties half to even as CPython's dtoa |
| _usd | private usd | |
| from_search | from_search(out: &Value, secs) -> PhaseResult | |
| from_agent | from_agent(final_text, ui_evidence: &[Value], &AgentRun) -> PhaseResult | AgentRun carries ok (default true), usd, secs, act |
| from_watch | from_watch(out: &Value) -> PhaseResult | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_chief_receipt.py | test_read_links, test_cited_pick, test_ui_seen, test_watch_armed, test_file_exists, test_guru_says_done, test_every_live_done_kind_has_an_oracle, test_the_executors_sentence_alone_is_unverifiable, test_one_unverifiable_check_makes_the_card_unverified_never_done, test_a_handed_on_phase_is_unverifiable, test_a_phase_that_did_not_happen_fails_its_check, test_the_receipt_is_composed_from_the_verdicts, test_from_search_takes_the_sources_as_the_read_links, test_from_agent_and_from_watch, test_attack_a_failed_refused_or_skipped_step_makes_the_card_unverified, test_attack_ui_seen_reads_only_the_screen_each_act_ended_on, test_attack_ui_seen_needs_at_least_two_words, test_attack_file_exists_needs_a_file_below_home_made_or_changed_since_the_card | same names in tests/chief_receipt.rs (parametrized tests loop over their cases) | test_a_stored_result_is_checked_strictly tests chief_card.result_from; it is ported in eve-chief-card |
| test_chief_lean.py | the executor's-word replay (patches chief_receipt.from_agent) | covered by test_from_agent_and_from_watch and test_the_executors_sentence_alone_is_unverifiable | the replay drives chief.py; it is ported with eve-chief |
| (new, checked against CPython 3.13 output) | | a_hand_edited_count_is_read_as_python_int_reads_it, a_link_is_trimmed_of_closing_punctuation_and_matched_whole, the_gate_names_the_first_phase_that_did_not_happen, the_receipt_writes_time_and_money_as_python_formats_them, the_adapters_take_python_values_as_python_does, the_screen_is_matched_after_python_case_folding; py.rs unit tests | |

## Boundaries
None.

## Differences
- Python's `check`, `verify` and `close` raise ValueError out of `int()` on a hand-edited read_links count or out of
  `home_path` on a NUL in a path; here they return `Err(ValueError)` with the same message.
- `from_search` with `sources` set to a truthy number or `true` raises TypeError in Python (not iterable); here it
  reads as no sources.
- `receipt` of a card whose `spent.wall_s` is infinite raises OverflowError in Python (`round(inf)`); here it writes
  "inf min". A stored card cannot hold one (`from_json` refuses non-finite numbers).
- `casefold` is the `caseless` crate's Unicode full case folding; its Unicode version may differ from CPython
  3.13's 15.1 for characters added since.
