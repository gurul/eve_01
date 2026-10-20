# eve-records
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/recall.py` | `src/recall.rs` (tests in `src/recall_tests.rs`) |
| `bridge/src/eve_bridge/records.py` | `src/records.rs` (tests in `src/records_tests.rs`) |
| (Python builtins records.py leans on: `str()`/`repr()` of a reply value (`repr` escapes what `str.isprintable()` rejects, by Unicode general category), truthiness, iteration, `re.escape`, `type(e).__name__`, `os.environ`) | `src/py.rs` |
| (test fixtures) | `src/testutil.rs` |

## API

### recall.py

| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_STORE`, `DEFAULT_NOTES`, `LEGACY_STORE`, `LEGACY_MEM0`, `MAX_BRIEF_CHARS`, `RECENT_SECS`, `FORGOTTEN` | same-named `pub const`s | Values verbatim. |
| `class RecallConfig` (`store`, `notes`; `transcripts_dir`, `records_dir`, `mem0_dir`, `archive_dir` properties) | `struct RecallConfig { store, notes }` with `transcripts_dir()`, `records_dir()`, `mem0_dir()`, `archive_dir()` | Frozen dataclass → `Clone + Eq + Hash`. |
| `configured(environ=None)` | `configured(Option<&HashMap>)` | `EVE_MEMORY_DIR` / `EVE_NOTES_DIR`, stripped, blank means default, `~` expanded. `None` reads the process environment (`py::environ`: `vars_os`, lossy, so a variable that is not UTF-8 never panics, as `os.environ` never raises; the records module reads it the same way). |
| `legacy_store(environ=None)` | `legacy_store(Option<&HashMap>)` | `EVE_DEBRIEF_DIR` or `~/.config/eve-bridge/debrief`. |
| `legacy_mem0()` | `legacy_mem0()` | |
| `_local(when)` | private `local` | `when.astimezone()`: the instant in the local zone. |
| `_gap_clause(then, now)` | `gap_clause(DateTime<Local>, DateTime<Local>)` | Same thresholds (900 s, 1, 6, 13 days); a clock gone backwards is "". |
| `_journal_title(cfg, day)` | `journal_title(cfg, day)` | Reads `records::latest_day`. |
| `opening_brief(cfg, transcripts, now=None)` | `opening_brief(cfg, Option<&dyn BriefSource>, Option<Dt>)` | `BriefSource` is what the brief reads of `Transcripts` (`enabled`, `last_turn_at`, `day_of`); implemented for `eve_transcripts::Transcripts`, and a test supplies a broken one. Any error is silence, logged at debug. |

### records.py

| Python | Rust | Notes |
|---|---|---|
| constants (`DEFAULT_MODEL` … `GC_TIMEOUT_SECS`, `TYPES`, `PROFILE_ID`, `STARRED_FILE`, `HIDDEN_FILE`, `DAYS_DIR`, `FORGOTTEN`, `INDEX_HEADING`, `FRESH_STARS_HEADING`, `TALKING_HEADING`, `VALIDITY_ENV`, `SOURCES_ENV`, `MAX_CLOSES_PER_NIGHT`, `FACT_KEYS`, `DEFAULTS`, `FORGETS_FILE`, `_PUSH_PREFIXES`) | same-named `pub const`s (`PUSH_PREFIXES`) | Values verbatim. `DEFAULTS` is an array of `(var, bool)`. |
| `_ID`, `_DAY`, `_DAY_FILE`, `_LINK`, `_WORD`, `_FRONTMATTER`, `_STAR_LINE`, `_STAR_DATE`, `_UPDATED`, `_META`, `_SRC` | private `LazyLock<Regex>` | Python's `\s` is `[\s\x1c-\x1f]`; Python's `$` is `\n?\z` (or absorbed by a trailing `\s*`); `\Z` is `\z`. |
| `_switch(name, environ=None)` | `switch(&str, Option<&HashMap>)` | 1/true/yes/on, stripped, any case; unset or blank reads `DEFAULTS`. |
| `validity_on`, `sources_on` | `validity_on`, `sources_on` (`Option<&HashMap>`) | `None` reads the environment (or the thread's `Seams::env`). |
| `class Fact` (`closed`) | `struct Fact` (`closed(today)`) | |
| `parse_fact` | `parse_fact` | |
| `render_fact` | `render_fact` | |
| `fact_display` | `fact_display` | |
| `_today` | private `today` | The memory day (starts 04:00). |
| `_date_or_empty` | private `date_or_empty` | |
| `class Record` (`links` property) | `struct Record { id, r#type, aliases, facts, updated }`, `links()`, `Record::new` | |
| `parse_record` | `parse_record` | |
| `render_record` | `render_record` | |
| `records_dir` | `records_dir` | |
| `load_records` | `load_records -> Records` | `Records = IndexMap<String, Record>`: file-name order, as the Python dict. |
| `_write` | private `write` | Temp file `.<name>.XXXX.tmp` in the folder, 0600, rename. |
| `_make_dir` | private `make_dir` | 0700. |
| `_locked` | private `locked` → `Held` guard | A process-wide reentrant lock plus a per-thread depth; `flock(LOCK_EX)` on the folder at depth 0. |
| `_generation` | private `generation` | (in-process count, `.forgets` text). |
| `_bump` | private `bump` | Writes `<n+1>\n` (any length of digits). |
| `_pinned` | private `pinned` | |
| `forgets_pinned` (context manager) | `forgets_pinned(cfg, f)`, and the guard form `pin_forgets(cfg) -> ForgetsPin` | Thread-local pin and night budget, as `threading.local`. |
| `_norm` | private `norm` | Unicode casefold. |
| `_star_rows` | private `star_rows` (shared parser `dated_rows`) | |
| `_star_line` | private `star_line` | |
| `star(cfg, text, when=None)` | `star(cfg, &str, Option<NaiveDateTime>)` | `when` is local wall time (Python's naive datetime). `O_APPEND|O_NOFOLLOW|O_CLOEXEC`, 0600, header on an empty file, newline repair. |
| `stars(cfg, limit=MAX_STARS)` | `stars(cfg, usize)` | |
| `_hidden_rows` | private `hidden_rows` | |
| `hidden` | `hidden` | |
| `_matcher` | private `matcher` | `eve_transcripts::match_query`. |
| `hidden_match` | `hidden_match -> Option<Pred>` | |
| `hide` | `hide(cfg, &str, Option<NaiveDateTime>)` | |
| `unhide` | `unhide` | |
| `_drop_hidden` | private `drop_hidden` | |
| `_tokens` | private `tokens` | |
| `search(records, query, limit=MAX_HITS, hide=None)` | `search(&Records, &str, usize, Option<&dyn Fn(&str)->bool>) -> Vec<Value>` | Hits are `{"id", "line"[, "src"]}` objects; stable sort by (−score, closed, id). |
| `get` | `get` | |
| `render_index` | `render_index` | |
| `_clip_lines` | private `clip_lines` | Code points. |
| `_fresh_block` | private `fresh_block` | |
| `_profile_page` | private `profile_page` | |
| `_open_only` | private `open_only` | |
| `profile` | `profile` | |
| `_drop_section` | private `drop_section` | |
| `profile_for_voice` | `profile_for_voice` | |
| `talking_section` | `talking_section` | |
| `_journal_title` | private `journal_title` | |
| `latest_day` | `latest_day -> Option<(String, String)>` | |
| `_recalled_line` | private `recalled_line` | |
| `class RecordsReader` (`profile`, `search`, `get`) | `struct RecordsReader<'a> { cfg, recall: Option<&dyn MeaningSearch> }` | `MeaningSearch` is the mem0 index's `search(query) -> rows`; a failing one is logged and is no meaning hits. `get`'s reason is `no record '<repr>'`. |
| `_git` | `git(cwd, args, timeout) -> Result<GitOutput, GitError>` | `GIT_*` dropped from the environment; `toolchain.git_usable` asked first (refused as `OSError` before any process); timeout kills the child (`TimeoutExpired`). |
| `_own_repo` | private `own_repo` | |
| `ensure_repo` | `ensure_repo` | |
| `seal` | `seal -> Result<(), GitError>` | Remotes removed, `pushInsteadOf` for every prefix with value pattern `^<re.escape(prefix)>$`, pre-push hook, `core.hooksPath`. |
| `commit` | `commit -> Option<String>` | |
| `squash_history` | `squash_history` | |
| `_as_found` | private `as_found` | |
| `_R_*`, `_C_*` prompt pieces, `RECONCILE_PROMPT`, `CONSOLIDATE_PROMPT` | private consts, `pub static RECONCILE_PROMPT` / `CONSOLIDATE_PROMPT` | Verbatim: every combination's sha256 equals the Python's (`test_every_prompt_and_schema_is_the_pythons_byte_for_byte`). |
| `_fact_piece` | private `fact_piece` | |
| `reconcile_prompt(validity=False, sources=False)` | `reconcile_prompt(bool, bool)` | |
| `consolidate_prompt` | `consolidate_prompt(bool, bool)` | |
| `_LIST`, `_RECORD`, `_FACT_FIELDS`, `_PLANS` | private `list_schema`, `record_base`, `fact_field`, `plans_schema` | |
| `_record_schema` | private `record_schema` | |
| `reconcile_schema`, `consolidate_schema`, `RECONCILE_SCHEMA`, `CONSOLIDATE_SCHEMA` | same names (`pub static` for the constants) | `json.dumps` of each combination is byte-identical (sha256 checked). Rust returns an equal value where Python returns the same object. |
| `class DayChange` | `struct DayChange { changed, created, corrections, journal, closed }` | |
| `render_profile` | `render_profile(&Map, &Records, day)` | |
| `_reindex_profile` | private `reindex_profile` | |
| `render_journal` | `render_journal(&Map, day)` | |
| `_plan_rows` | private `plan_rows` | |
| `_fact_line` | private `fact_line` | |
| `_cap_facts` | private `cap_facts` | |
| `_record_from` | private `record_from` | |
| `_srcs_of` | private `srcs_of` | |
| `day_srcs` | `day_srcs` | First 16 code points of each line. |
| `_new_closures` | private `new_closures` | (record index, fact index) pairs. |
| `_closures` | private `closures` | |
| `_hold_closures` | private `hold_closures` | |
| `_raw` | private `raw_of` | |
| `_NIGHT`, `_closes_left` | thread-local `NIGHT_CLOSED`, private `closes_left` | |
| `_spend_closes` | private `spend_closes` | |
| `_same` | private `same` | |
| `apply_day(cfg, result, day, srcs=None)` | `apply_day(cfg, &Map, day, Option<&HashSet>) -> io::Result<DayChange>` | Python raises `OSError` out; Rust returns it. |
| `_call` | private `call` | `json.loads` via `eve_transcripts::py::loads`; a failure logs `records: the <method> call failed (<Python class>)`. |
| `_render_all` | private `render_all` | |
| `reconcile_day` | `reconcile_day(cfg, &dyn DreamClient, day, day_text) -> Option<DayChange>` | `DreamClient` is the `reconcile(body)` / `consolidate(body)` duck type. |
| `_rewrite_links` | private `rewrite_links` | |
| `consolidate` | `consolidate(cfg, &dyn DreamClient) -> (usize, usize)` | |
| `_forget_head` | private `forget_head` | |
| `_forget_in` | private `forget_in` | |
| `forget_lines(cfg, match, dry_run=False)` | `forget_lines(cfg, &dyn Fn(&str)->bool, bool) -> usize` | |
| `class OpenAIReconcileClient` (`reconcile`, `consolidate`, `_call`) | `struct OpenAIReconcileClient` implementing `DreamClient`; `new(model, api_key, consolidation_model) -> Result<_, NoKey>` | One strict-JSON Responses call each (`dream_day` / `dream_consolidate`), `max_output_tokens` 16000, reasoning low, `store: false`, 180 s timeout; metered with `eve_spend` as `memory` under the model name given. `opts` and `ledger` are public so a test points them at a local server and a temp ledger. |
| `make_client(environ=None, model=DEFAULT_MODEL)` | `make_client(Option<&HashMap>, &str)` | |
| (monkeypatch targets: `os.environ`, `shutil.which`, `toolchain.git_usable`, `toolchain.sys.platform`, `subprocess.run`) | `Seams` + `with_seams(seams, f)` | A thread-scoped override of what Python tests patch; production never sets it. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_recall.py` | `test_memory_off_says_nothing`, `test_an_empty_or_missing_store_says_nothing`, `test_the_gap_is_named_the_way_a_person_would`, `test_a_conversation_minutes_ago_is_not_worth_mentioning`, `test_a_future_last_turn_is_silence_not_an_error`, `test_either_channel_counts_but_a_bus_line_does_not`, `test_a_broken_store_is_silence`, `test_the_journal_title_of_the_last_day_rides_along`, `test_a_journal_from_another_day_is_not_quoted`, `test_a_talk_earlier_today_never_carries_a_title`, `test_a_forgotten_bare_or_questioning_title_is_left_out`, `test_a_long_brief_is_cut_to_length_on_a_word`, `test_the_stamp_file_is_gone`, `test_configured_reads_the_memory_dir_and_expands_the_paths`, `test_the_legacy_locations_are_the_migrations_source` | same names in `src/recall_tests.rs` (`test_the_stamp_file_is_gone` scans the Rust source), plus `test_the_gap_clause_reads_the_calendar_not_the_hours` | `test_no_memory_leaves_the_prompt_byte_identical`, `test_a_brief_reaches_the_prompt_with_the_rules_that_keep_it_short`, `test_captions_mode_still_gets_its_own_instructions_after_the_memory` test `voice_agent.session_config` / `memory_block` (the voice-agent crate). |
| `bridge/tests/test_records.py` | all 34 but one | same names in `src/records_tests.rs`; plus `test_a_star_closes_an_open_last_line_and_never_follows_a_link`, `test_a_failed_call_is_logged_with_its_python_name`, `test_an_empty_reply_is_a_failed_call`, `test_a_forget_during_the_dream_drops_its_write`, `test_a_git_that_hangs_is_timed_out`, `test_a_bad_profile_is_no_git_by_name`; `src/py.rs` unit tests | `test_keyword_hits_still_return_when_the_meaning_search_times_out` tests `mem0_memory.OwnerMemory`'s search timeout (the mem0 crate); here a failing `MeaningSearch` is covered by `test_recalled_memories_carry_their_day_and_channel`. `test_the_openai_client_…` uses a local HTTP server instead of a fake `openai` module. `test_without_git_records_are_still_written` sets `Seams::which_git` instead of patching `shutil.which`. |
| `bridge/tests/test_memory_records.py` (tests_ref) | `test_an_old_record_file_parses_and_renders_back_byte_for_byte`, `test_an_old_record_searches_as_before_with_no_src_and_no_label`, `test_with_both_switches_off_the_prompts_and_schemas_are_the_plain_ones`, `test_each_switch_adds_its_own_fields_to_the_fact_schema_and_prompt`, `test_the_meta_round_trips_and_only_known_keys_count`, `test_validity_closes_the_old_fact_hides_it_from_the_profile_and_labels_it_in_search`, `test_the_guard_writes_a_busy_night_with_the_closures_over_the_limit_held_open`, `test_a_valid_to_set_ahead_counts_against_the_night`, `test_reconcile_and_consolidate_of_one_night_share_the_closure_budget`, `test_with_validity_off_a_valid_to_still_loads_and_the_profile_shows_open_lines_only`, `test_sources_keep_a_src_that_points_at_a_real_line_and_drop_one_that_does_not` | same names in `src/records_tests.rs`, plus `test_every_prompt_and_schema_is_the_pythons_byte_for_byte` (sha256 of all four combinations, taken from the Python) | The forget-follows-src tests go through `memory.Memory`, and the mem0 ingest/search tests through `mem0_memory.OwnerMemory`: the memory and mem0 crates. |
| `bridge/tests/test_toolchain.py` (tests_ref) | `test_off_macos_records_run_git_as_before`, `test_records_git_raises_oserror_before_any_process`, `test_records_degrade_with_one_log_line_each`, `test_in_eve_lite_records_start_no_git`, `test_where_git_is_usable_records_keep_their_history` | same names in `src/records_tests.rs` (patches are `Seams`) | The rest test eve-toolchain itself. |
| `bridge/tests/test_memory_verdict.py` (tests_ref) | the records rows of `test_every_switch_default_equals_the_verdict` and `test_an_unset_switch_reads_its_default_and_a_set_one_reads_its_value` | same names (the first reads `bridge/tests/fixtures/memory_eval/VERDICT.md`) | The memory_index rows are eve-transcripts'; `test_each_feature_in_the_verdict_has_its_holdout_run` checks eval fixtures. |
| `bridge/tests/test_small_talk.py` (tests_ref) | `test_the_talking_section_survives_a_profile_longer_than_the_voice_cut` (the records half) | same name | `small_talk.voice_from_profile` belongs to the small-talk crate. |
| `bridge/tests/test_model_memory_roles.py`, `test_spend_wiring.py`, `test_privacy_guards.py` (tests_ref) | `test_records_extract_and_compact_with_independent_models`, `test_the_dream_is_metered_as_memory`, the `store` check of the privacy guard | same names (`store: false` is asserted in `test_the_openai_client_never_stores_and_uses_one_schema_per_call`) | The rest of those files test other modules. |
| other `tests_ref` files | — | — | They drive records through memory, dream, notes, CLIs and the daemon; their behavior is ported with those crates. |

Differential checks run during the port (not kept as tests, as they need the Python): `parse_fact`/`render_fact`/`fact_display`, `parse_record`/`render_record` (CRLF and odd frontmatter included), `render_journal`, `render_profile` and `_fact_line` over malformed model replies; and, on one seeded records folder (CRLF record, closed facts, a hidden line, fresh and undated stars), `profile`, `profile_for_voice`, `talking_section`, `RecordsReader.search/get`, `stars`, `latest_day`, `forget_lines` (dry run and real) and every file afterwards. All identical to the Python.

## Boundaries
- git runs as a subprocess, as in the Python (`git -C <records> …`), behind the same `toolchain.git_usable` guard.

## Differences
- The in-process forget count is kept per records folder instead of once per process. eve has one records folder, so the two agree; the change keeps a forget in one folder (a test's) from voiding a dream in another running at the same time. The on-disk `.forgets` generation is unchanged.
- A file that is not valid UTF-8 reads as unreadable everywhere (skipped, or no stars / no hidden lines); Python raises `UnicodeDecodeError` out of `load_records`, `star`, `latest_day` and the journal title. Reads use universal newlines, as `read_text` does.
- A bad `EVE_PROFILE` (neither full nor lite) makes git "unavailable (ProfileError)" with one log line, and the records are kept without history; Python lets the `ProfileError` escape from `ensure_repo` / `commit` / `reconcile_day`.
- A `day` that matches `YYYY-MM-DD` but is not a date (`2026-13-45`) returns None with a log line before anything is done; Python makes the as-found commit, then raises `ValueError` from `strptime`.
- `OpenAIReconcileClient` builds its blocking HTTP client per call on a thread of its own (a blocking client must never be created or dropped on an async runtime); Python builds one SDK client in `__init__`. `new` returns `NoKey` where `openrouter.client` raises it.
- `iter_of`: a model reply's list field holding a number or `true` is treated as empty; Python raises `TypeError` (not iterable). A string or an object iterates as Python does (characters, keys).
- `star` / `hide` take a naive local `when`; Python also accepts an aware datetime (its wall time is used either way).
