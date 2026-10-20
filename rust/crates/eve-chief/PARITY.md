# eve-chief
Status: complete

The chief of staff: the switch (`EVE_CHIEF`), the three model tools (`take_on`, `jobs_list`, `close_card`) and
their instructions, the options sheet an assess step writes, and `Chief`, the loop that moves every job card one
step per tick (route, Go, dispatch, run, reflect, replan, close, receipt), reminds about commitments through the
desk, and sends the evening close and the weekly sheet. `make_chief` wires eve's real research (the web reader
and the shipped browser router). Ported from `bridge/src/eve_bridge/chief.py`.
`cargo test -p eve-chief`: 101 tests pass (87 in `src/tests.rs`, 13 in `src/tests_lean.rs`, 1 in `src/py.rs`).

## Modules
| Python | Rust |
|---|---|
| chief.py | src/lib.rs |
| (Python semantics: str(), repr(), strip, `:,.2f`, `:g`, round, json.dumps, time.time, datetime.fromtimestamp) | src/py.rs |
| tests/test_chief.py, the chief halves of test_chief_reflect.py, test_chief_replan.py, test_chief_eval.py | src/tests.rs |
| tests/test_chief_lean.py | src/tests_lean.rs |

## API
| Python | Rust | Notes |
|---|---|---|
| MODES, DEFAULT_MODE, SHIPPED | MODES, DEFAULT_MODE, SHIPPED | the PRE-REGISTERED BAR comment beside SHIPPED is verbatim; a test reads it from lib.rs |
| DEFAULT_ASSESS_MODEL, DEFAULT_ASSESS_EFFORT, ASSESS_EFFORTS | same names | |
| mode | mode(environ: Option<&HashMap>) | None reads the process environment |
| enabled | enabled(environ); enabled_shipped(environ, shipped) | the second is the tests' seam for `monkeypatch.setattr(chief, "SHIPPED", ...)` |
| assess_settings | assess_settings(environ) -> (model, effort) | |
| class Ceiling | Ceiling { usd, secs } | |
| CEILINGS | CEILINGS: [(&str, Ceiling); 4]; each Chief keeps its own copy in `Chief.ceilings` (IndexMap) | see Differences |
| EXECUTORS, ONE_WAY_FLOOR, REFLECT_CEILING, REPLAN_CEILING | same names | |
| ACT_GUARD, READ_ONLY | same names | byte-identical (checked against a CPython dump) |
| MAX_TICK_SECS, ERROR_TICK_SECS, RETRY_TICK_SECS, RECHECK_SECS, RETRY_SEND_SECS, GO_WAIT_SECS, BRIEF_DAY_HOUR | same names | `f64` / `i64` |
| MAX_OPTIONS, MAX_NAME_CHARS, MAX_WHY_CHARS, MAX_LINK_CHARS, MAX_READ_SHOWN, CLOSE_UNTIL | same names | |
| TOOL_NAMES, CLOSE_HOWS | same names | |
| jobs_list's two descriptions | JOBS_LIST_NARROW, JOBS_LIST_WIDE | chosen by eve-menu-tools' `enabled` (on when unset, as Python) |
| tools | tools(live), tools_in(live, environ) | JSON byte-identical with Python's for every live set and both menu-tools modes |
| instructions | instructions(live) | byte-identical |
| class Option | Option_ (alias SheetOption) | `Option` is Rust's prelude type |
| class Sheet, Sheet.picked | Sheet { options, pick, change }, Sheet::picked() | |
| ASSESS_INSTRUCTIONS, ASSESS_SCHEMA | ASSESS_INSTRUCTIONS, ASSESS_SCHEMA (LazyLock<Value>) | key order as Python's dict |
| _plain | private plain / plain_str | |
| sheet_from | sheet_from(text) -> Option<Sheet> | Python's iteration of `options`: an empty dict or string is no options |
| _pick_of | private pick_of | |
| _price | private price_words | `f" for ${v:,.2f}".replace(".00", "")` via py::thousands_2f |
| act_goal, act_digest, go_text | same names | digest is sha256 hex of the same bytes |
| _output_text | output_text | pub, for the tests |
| parse_sheet, sheet_text, sheet_words | same names | sheet_text is `json.dumps` with Python's separators (py::dumps_compact) |
| _day, _brief_day, _at | private day_of, brief_day, at_hhmm | local time, as datetime.fromtimestamp |
| _maybe | none needed | every callback returns a BoxFuture (or is sync); there is no "maybe awaitable" |
| class Chief | Chief (built by Chief::new(ChiefOptions), shared as Arc<Chief>) | |
| Chief.__init__ | Chief::new(ChiefOptions) | ChiefOptions has every keyword of `__init__`, plus `spend` and `estimate` (see Differences) |
| Chief._spawn | private spawn (tokio::spawn, JoinHandle kept in `tasks`) | |
| Chief.wake | wake() | AtomicBool + Notify for asyncio.Event |
| Chief._count, _in_doubt, _ceiling, _within, _spent_rows, _lessons, _phase_checks, _live_watches | private count, in_doubt, ceiling, within, spent_rows, lessons, phase_checks, live_watches | |
| Chief._send | private send -> route word | |
| Chief.handle | handle(name, args, said, said_ref, door) | |
| Chief._close_from_chat, _router_word, _as_data | private close_from_chat, router_word, as_data | |
| Chief.for_turn, heard, owner_spoke, breakpoint | same names | |
| Chief._take_problems | private take_problems | |
| Chief.listing, choices, details | same names | details: an empty `waiting_for` shows nothing, as Python's falsy test |
| Chief._retry_counts, retry_gains | private retry_counts; retry_gains pub | |
| Chief.approve, drop, keep, done, raise_budget, change, reopen | same names | `Chief::drop(&c, id)`: `c.drop(..)` on an Arc resolves to Drop |
| Chief._doubtful, happened, didnt, snooze | private doubtful; happened, didnt, snooze | |
| Chief.on_agent_result, on_phase_result | same names | |
| Chief._on_result, _replan, _reflect, _warn | private on_result, replan, reflect, warn_budget | |
| Chief._advance, _route, _ask_go, answer_word, _go_flow, _over_budget, _dispatch | private advance, route, ask_go, go_flow, over_budget, dispatch; answer_word pub | |
| Chief._run | private run_phase | `run` is the loop |
| Chief._watch_args | watch_args (associated fn) | |
| Chief._assess, _close, _send_receipt, _last_sent, _remind, _pull_pieces | private assess, close, send_receipt, last_sent, remind, pull_pieces | |
| Chief._doubt, _overdue, _resume | private doubt, overdue, resume | |
| Chief.tick, run, drain | tick, run, drain(rounds) | plus in_flight() and settle() for `asyncio.gather(*chief._tasks)` in the tests |
| make_chief | make_chief(MakeOptions) -> Option<Arc<Chief>> | research is eve-web's `web_reader::take_task` + `search_router::answer(reader)` on `eve_spend::to_thread`; router is eve-browser-router's `jev_router` |
| (exceptions by class name in logs and results) | Raised { name, message }, err_name(anyhow::Error) | the log lines name the same Python exception classes |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_chief.py | every test but one | same names in src/tests.rs | test_the_relay_door_feeds_the_chief_its_messages_and_turn_ends drives telegram.TelegramInlet: it belongs to eve-telegram |
| test_chief_reflect.py | test_a_lesson_is_written_and_bounded_to_three_per_key, test_a_retry_carries_the_lessons, test_a_one_way_act_never_retries_by_itself_and_its_next_go_shows_the_lesson, test_a_reflection_error_means_no_lesson_and_no_retry, test_injection_in_executor_output_cannot_change_the_next_goal_beyond_the_notes, test_a_looping_but_confirmed_phase_keeps_a_lesson_and_retries_nothing, test_a_two_way_phase_retries_at_most_twice, test_a_retry_waits_for_the_budget, test_the_model_says_no_retry_and_none_happens, test_a_retry_that_turns_a_failed_check_into_a_confirmed_one_is_counted | same names | test_the_heuristics_repeated_step_and_too_many_steps, test_clean_strips_links_markup_delimiters_and_caps, test_a_lesson_that_widens_the_step_is_not_carried, test_the_reflection_request_marks_the_attempt_untrusted_and_carries_card_fields_only, test_the_reply_must_be_exactly_lesson_and_retry, test_the_memory_file_is_private_compacts_and_skips_torn_lines, test_a_reflector_lesson_that_cannot_be_kept_is_not_returned: chief_reflect alone, in eve-chief-reflect |
| test_chief_replan.py | test_a_replan_adds_a_read_step_that_runs_and_the_act_still_asks_its_go, test_attack_a_page_that_talks_the_replan_into_an_act_changes_nothing, test_a_failed_research_is_replaced_and_the_card_goes_on, test_positive_control_without_the_replanner_a_failed_research_closes_the_card, test_a_finish_takes_the_rest_out_and_the_oracles_still_decide, test_replans_stop_at_the_limit, test_shadow_logs_what_it_would_do_and_changes_nothing, test_attack_the_owner_changes_the_card_while_the_replan_is_out, test_a_replan_cancels_a_yes_given_before_it, test_the_replan_call_is_paid_from_the_card_and_refused_at_its_cap | same names | the pure triggers/request/parse/apply tests are chief_replan alone, in eve-chief-replan |
| test_chief_eval.py | test_the_repo_default_matches_its_kept_verdict, test_the_bar_is_written_beside_shipped_with_the_same_numbers | same names; SHIPPED against bridge/tests/fixtures/chief/capture_result.json, the bar against lib.rs's comment | the scoring and check_default tests drive tools/chief_eval.py, a Python tool that is not part of eve-bridge's runtime |
| test_chief_lean.py | all 13 | same names in src/tests_lean.rs; the Lean constants are read from verification/Eve/Chief.lean | |
| test_chief_record.py, test_chief_desk.py, test_chief_receipt.py | | | the card, ledger, desk and receipt modules: eve-chief-card, eve-chief-ledger, eve-chief-desk, eve-chief-receipt |
| test_telegram_chief.py | | | the Telegram door: eve-telegram |
| (new) | | test_parse_sheet_keeps_only_read_links_and_a_cited_pick, test_output_text_reads_messages_then_the_fallback, test_watch_args_are_the_watchers_search_kind, test_jobs_list_keeps_the_narrow_wording_while_menu_tools_is_off, sheet_from_reads_python_iterables, test_a_research_that_raises_timeout_error_is_answered_as_its_ceiling, py::tests::formats_follow_python | |

## Boundaries
- eve-chief-card, eve-chief-ledger, eve-chief-desk, eve-chief-receipt, eve-chief-reflect, eve-chief-replan: the
  record, the store, the attention gate, the oracles, the lessons and the replanner, as chief.py imports them.
- eve-consent (`bare_decision`), eve-menu-tools (`enabled`), eve-pricing (`estimate_openai_cost`), eve-spend (the
  job tag, `record_response`, the rows read back).
- make_chief only: eve-web (web_reader, search_router), eve-browser-router (`jev_router`), eve-jev (`ProcessEnv`).
- The door's pieces are callbacks it lends: start_task / stop_task (the Mac task seam), the watcher's `add` and its
  ids, the model call (`create`), the question (`ask`) and the send (`notify`).

No upstream crate was edited.

## Differences
- Upstream edit (eve-brain's inlet verify): `Chief::in_doubt`, `router_word`, `as_data` and `doubtful` (Python's `_in_doubt`, `_router_word`, `_as_data`, `_doubtful`) are `pub`, so the app's Projects tab (telegram.py's `_LinkChief`) reads them instead of a copy. Additive; no behavior changes.
- `CEILINGS` is a module dict in Python that tests patch (`monkeypatch.setitem`); in Rust it is a const table, and
  each Chief copies it into its own `ceilings` map, which a test changes before the chief is shared.
- `SHIPPED` is a const; `enabled_shipped(environ, shipped)` is the seam the switch test uses instead of patching it.
- `ChiefOptions.spend` names the spend ledger the chief records to and reads back (Python uses the spend module's
  one global ledger); the default is spend's configured folder. `ChiefOptions.estimate` defaults to
  eve-pricing's `estimate_openai_cost`.
- Callbacks that raise return `Err(Raised { name, .. })`; the log lines and results carry `name` where Python
  writes `type(e).__name__`. A missing executor where Python would call None gives `TypeError`.
- Test-only seams (cfg(test)): `fail_advance` (Python patches `_advance` to raise for one card) and `from_agent`
  (the positive controls patch `chief_receipt.from_agent`). The "unwritten dispatch line" test makes events.jsonl
  read-only rather than patching `Ledger.event`.
- tests_lean's generated traces draw from SplitMix64, not CPython's Mersenne Twister, so seed N is a different
  trace from Python's seed N; the count (240 seeds x 40 moves, desk 3 modes x 12 seeds) and the coverage gates are
  the same.
- `_maybe` has no counterpart: every async callback is a BoxFuture.
- `parse_sheet` and `sheet_from` read JSON with serde_json, which refuses the `NaN` / `Infinity` literals and
  numbers past f64 that Python's `json.loads` accepts; such a reply is no sheet here (an assess step fails) where
  Python would drop the non-finite price. The strict schema never asks for either.
