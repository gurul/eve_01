# eve-chief-ledger
Status: complete

The chief of staff's ledger: `cards.json` (atomic, `json.dump(indent=1)` bytes), the append-only `events.jsonl`
(`json.dumps(separators=(",", ":"), ensure_ascii=False)` bytes, fsynced), the incremental JSONL reader, and the
read-backs (`due`, `for_turn`, `matching`). One Python module, ported into `src/lib.rs`; the Python semantics the
files depend on (json dump/load incl. non-finite numbers, float repr, `round`, `repr`, `str.split`, `casefold`,
`splitlines`, pathlib `expanduser`, naive `timestamp()`, OSError names) are in the private `src/py.rs`. Cards,
parse, revise, reopen, `to_json`/`from_json`, `at_time` and `card_number` come from eve-chief-card.
`cargo test -p eve-chief-ledger`: 37 tests pass (31 ledger, 6 py helpers).

## Modules
| Python | Rust |
|---|---|
| chief_ledger.py | src/lib.rs (+ src/py.rs, private Python-semantics helpers; tests in src/tests.rs) |

## API
| Python | Rust | Notes |
|---|---|---|
| DEFAULT_DIR, CARDS_FILE, EVENTS_FILE, EVENT_KINDS, STATE_FIELDS, PHASE_STATE_FIELDS, MAX_CARDS, TURN_LINE_CHARS, MAX_EVENT_TEXT | same names | `&str` / `&[&str]` / `usize` |
| _ID_IN_TEXT, _WORD, _STOP, _WAITING_WORDS, _DOING | private ID_IN_TEXT, WORD, STOP, waiting_word(), doing() | the `_ids_in_events` pattern is ID_IN_EVENTS, `\s` widened to Python's (U+001C..U+001F) |
| class LedgerError | LedgerError(String) | thiserror; same messages ("could not save the cards (PermissionError)") |
| (ValueError / KeyError raised by Ledger) | Error::{Value, Key, Ledger} | same message text; KeyError's text is the bare key |
| chief_dir | chief_dir(Option<&HashMap<String, String>>) | None reads the process env; `EVE_CHIEF_DIR` stripped, empty is unset; pathlib-normalized and `~`/`~user` expanded |
| _content_words | private content_words | Python casefold (ß, ſ, ligatures) before `[a-z0-9]+` |
| _day | private day | local date of an epoch float (`fromtimestamp` rounding) |
| _scalar | private scalar | numbers/bools/null kept; anything else `str()` (dicts/lists as Python repr), squashed, cut to 200 chars |
| class JsonlRows | JsonlRows::new, JsonlRows::rows(&Path) -> Vec<Map> | (dev, inode) + byte offset; shrink or replace reads afresh; unterminated tail parsed but re-read; non-dict/torn lines skipped |
| JsonlRows._parse | private JsonlRows::parse | `json.loads` incl. NaN/Infinity/1e999 (read as null) |
| JsonlRows.rows | JsonlRows::rows | |
| class Ledger | Ledger | Send + Sync; one parking_lot Mutex for Python's RLock |
| Ledger.__init__ | Ledger::new(Option<&Path>), Ledger::with_clock(Option<&Path>, Clock) | Clock = `Arc<dyn Fn() -> f64>` (time.time default) |
| Ledger.dir | Ledger::dir() | |
| Ledger.cards_path / events_path | Ledger::cards_path() / events_path() | |
| Ledger._ids_in_events | private ids_in_events | |
| Ledger._aside | private aside | cards.json.bad-<int(clock)>, moved or copied + chmod 600 |
| Ledger._load | private load | same shape checks, problem texts and log lines; a non-finite number reads as a stand-in every chief_card check refuses, so the card (not the file) is set aside, as in Python |
| Ledger._save | private save | tempfile `.cards-*.json` in the folder, 0o600, rename; folder mkdir 0o700 |
| Ledger._commit | private commit | |
| Ledger.take_problem | Ledger::take_problem | |
| Ledger.event | Ledger::event(EventKind, cid, &[(&str, Value)]) -> bool | kind is the EventKind enum; `"x".parse::<EventKind>()` gives Python's ValueError text |
| (EVENT_KINDS as a type) | EventKind (ALL, as_str, FromStr, Display) | |
| Ledger.events | Ledger::events(Option<&str>) -> Vec<Map<String, Value>> | |
| Ledger.get | Ledger::get -> Option<Card> | a clone |
| Ledger.all | Ledger::all | |
| Ledger.open | Ledger::open | |
| Ledger.new | Ledger::new_card(card, why) | renamed: `new` is the constructor; Python's default `why` is "take_on" |
| Ledger._trim | private Ledger::trim | |
| Ledger._put | private put | |
| Ledger.revise | Ledger::revise(cid, patch, why, said, live, home) -> Result<Result<Card, Refusal>, Error> | inner Err is Python's returned Refusal; Error::Value is chief_card's ValueError |
| Ledger.replan | Ledger::replan(cid, expect, phases, event_fields) -> Result<Option<Card>, LedgerError> | |
| Ledger.reopen | Ledger::reopen(cid, why) | |
| Ledger._need | private need | |
| Ledger.update | Ledger::update(cid, why, StateUpdate) | StateUpdate holds exactly STATE_FIELDS (builder methods) |
| Ledger.update_phase | Ledger::update_phase(cid, n, why, PhaseUpdate) | PhaseUpdate holds exactly PHASE_STATE_FIELDS |
| Ledger.claim | Ledger::claim(cid, n, rev, token, what, executor, event_fields) -> Result<Option<Card>, Error> | |
| Ledger.due | Ledger::due | cue times as CPython's naive `timestamp()` (DST gap/fold rules) |
| Ledger.for_turn | Ledger::for_turn | |
| Ledger._describe | Ledger::describe | public: chief.py calls it |
| Ledger.matching | Ledger::matching | |
| Ledger.redact | Ledger::redact(ids) -> Result<usize, LedgerError> | text-mode read (universal newlines, U+FFFD), `splitlines(keepends=True)`, rewrite via `.events-*.jsonl` + rename |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_chief_record.py (ledger half) | test_the_ledger_is_in_the_temp_folder_during_tests, test_positive_control_unset_the_ledger_resolves_to_the_config_folder, test_new_files_the_card_under_a_fresh_id_and_logs_created, test_ids_are_never_reused, test_a_failed_replace_leaves_the_old_file_and_memory_as_they_were, test_an_unreadable_file_moves_to_bad_and_is_reported_once, test_one_card_that_does_not_load_is_skipped_not_the_whole_list, test_state_changes_do_not_bump_rev_and_a_revision_does, test_an_event_that_cannot_be_written_says_so, test_events_carry_scalars_capped, test_due_finds_cues_and_nudges_that_have_come, test_for_turn_is_one_short_line, test_matching_finds_the_card_the_owners_words_are_about, test_redact_removes_the_cards_and_their_events, test_attack_a_number_too_big_sets_the_card_aside_instead_of_crashing, test_attack_a_card_that_does_not_load_is_kept_aside_and_reported, test_attack_a_file_of_the_wrong_shape_is_moved_aside_not_overwritten, test_events_parse_only_the_lines_appended_since_the_last_read | same names in src/tests.rs | The env fixture is a passed map (tests never set the process env). The failed `os.replace` is a swapped replace fn (`set_replace`). The json.loads counter is JsonlRows' parsed-line counter. "not a state field" and the Spent/Budget/Nudges type checks are compile-time (StateUpdate/PhaseUpdate). The card half belongs to eve-chief-card |
| (new, against CPython output) | | the_cards_file_is_written_as_json_dump_indent_1, an_event_line_is_compact_json_with_ids_first, the_new_dir_is_made_700, a_cards_list_that_is_null_is_the_wrong_shape_and_a_missing_one_is_empty, a_card_that_is_not_an_object_is_named_by_its_place, jsonl_rows_skip_foreign_lines_and_read_a_missing_file_as_empty, claim_starts_a_queued_phase_once_and_only_with_its_go, claim_starts_nothing_when_the_dispatch_line_cannot_be_written, replan_applies_only_to_the_card_it_was_asked_about, the_store_gives_the_oldest_finished_cards_way_at_the_cap, describe_says_what_each_open_card_waits_on, redact_keeps_torn_lines_and_reads_text_as_python_does, for_turn_with_a_time_python_cannot_convert_has_nothing_due; py.rs unit tests | |
| test_chief.py, test_chief_desk.py, test_chief_lean.py, test_telegram_chief.py | read for behavior (claim, replan, due, for_turn, first_brief over a Ledger) | covered by the tests above | they test chief, chief_desk and the Telegram door through the ledger; ported with those crates |

## Boundaries
None.

## Differences
- The clock returns f64, so an event's `t` and a card's `created`/`updated` are always written as floats; a
  Python clock returning an int would write `5` where Rust writes `5.0`. Both read back the same.
- A non-finite number in an event line (`NaN`, `Infinity`, `1e999`) reads back as `null` from `events()` and
  `JsonlRows::rows`; Python keeps the float. Nothing reads those fields as numbers.
- An event field named twice in the slice keeps its first value (Python's keyword arguments cannot repeat, and
  `claim(..., phase=...)` would raise TypeError there).
- An `EVE_CHIEF_DIR` or folder starting with `~unknown-user` is kept literally; pathlib raises RuntimeError.
- `for_turn` with a time `fromtimestamp` cannot convert treats today as "" (no commitment is due); Python raises.
- A cards file holding a lone surrogate escape (`"\ud800"`) is unreadable to serde_json, so Rust moves it aside
  where Python loads it. CPython itself cannot write such a card to events.jsonl (utf-8 encode fails), and nothing
  eve writes holds one.
- An integer beyond u64/i64 (`rev` of 2**70) reads as a float in Rust and that card is set aside; Python keeps
  the int. No eve number reaches that size.
- serde_json's nesting limit (128) makes a deeper cards file "unreadable" (moved aside); CPython raises
  RecursionError past ~1000 levels.
