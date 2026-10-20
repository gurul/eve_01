# eve-dream
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/dream.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DREAM_HOUR`, `MAX_NIGHTS_PER_WAKE`, `MAX_TRIES`, `FIRST_WAKE_SECS`, `DREAMT_FILE`, `DAYS_SUBDIR`, `REPORT_HEADING` | same-named `pub const`s | Values verbatim. `LOOP_INTERVAL_SECS` (1800.0) is `loop`'s default `interval_secs`, since Rust has no default arguments. |
| `_DAY` | private `DAY` regex | `^\d{4}-\d{2}-\d{2}\n?\z`: Python's `$` also matches before one final newline; `\d` is Unicode digits in both engines. |
| `class DreamReport` (dataclass) | `struct DreamReport` + `DreamReport::new(day)` | Same fields, same defaults. Counts are `usize`; `journal` is `Option<PathBuf>`. |
| `_local(now)` | private `local(When)` | An aware time goes to the local zone (`astimezone()`); a naive one is taken as local already (`py::local_aware`). `When` is `From` an aware `DateTime<FixedOffset>`/`DateTime<Local>` or a `NaiveDateTime`, as Python's `due_nights` takes either. |
| `_count(value)` | (types) | Every count the steps return is a `usize`, so the clamp to `max(0, int(v))` is the type; `DayChange`'s fields are read directly instead of `getattr(change, ..., 0)`. `DayChange.journal` is a `Path`, so it is always kept: an empty path is Python's `Path("")` (`.`), not a file, so that night gets no section and no `days/<day>.md` fallback. |
| `class Dreamer` | `struct Dreamer` | `Dreamer::new(cfg, Arc<Transcripts>, client: Arc<dyn DreamClient + Send + Sync>, index: Option<Arc<dyn DreamIndex>>, wall: Option<Wall>)`; public fields `cfg`, `transcripts`, `client`, `index`, `wall`, `records` as in Python (tests swap `transcripts` and `client`). The night lock, the per-night tries and the loop's warned-once flag are shared state behind an `Arc`. |
| `Dreamer.dreamt_path` (property) | `Dreamer::dreamt_path()` | `<records>/.dreamt`. |
| `Dreamer.dreamt()` | `Dreamer::dreamt() -> HashSet<String>` | Missing or non-UTF-8 ledger is empty; Python's `splitlines` and `strip` (`eve_transcripts::py`). |
| `Dreamer._mark(day)` | private `Dreamer::mark` | `mkdir(0o700, parents)`, then one `write` on `O_WRONLY\|O_APPEND\|O_CREAT\|O_NOFOLLOW\|O_CLOEXEC`, mode 0600. An `OSError` logs and returns false; a non-ASCII day is `LedgerEncodeError` (Python's `UnicodeEncodeError`), which, as in Python, stops the night. |
| `Dreamer.due_nights(now)` | `Dreamer::due_nights(impl Into<When>)` | Same rules: never the current transcript day, yesterday only at or after 04:30, set-aside nights skipped, oldest first, at most 3. |
| `Dreamer.dream(day)` (async) | `async fn Dreamer::dream(&self, day) -> DreamReport` | `spawn_blocking` for `asyncio.to_thread`; the caller's tracing dispatcher is carried onto the worker. An error out of the night or a panic in it is `failed: ["dream"]` with `dream: <day> stopped (<name>)` (a panic names itself `panic`). |
| `Dreamer._night(day)` | `Dreamer::night(&self, day) -> anyhow::Result<DreamReport>` | Public so a caller off the runtime (the CLI) can run a night on its own thread. Forget generation pinned around the steps (`eve_records::forgets_pinned`); the report section and the `dream: <day> report` commit as Python. |
| `Dreamer._run(day, report)` | private `Dreamer::run` | Same steps, same order, same failure names (`read`, `reconcile`, `consolidate`, `index`, `mark`) and log lines. Reads `day_text(day, DAY_TEXT_CHARS, outside=false)`. |
| `Dreamer._summary(report)` (staticmethod) | `Dreamer::summary(&DreamReport)` | Byte-identical line (the CLI prints it). |
| `Dreamer._append_report(day, report)` | private `Dreamer::append_report` | Same lines; a body that does not end in a newline (text mode: `\n` or `\r`) gets one first. Non-UTF-8 journal logs `UnicodeDecodeError`. |
| `Dreamer.loop(shutdown, interval_secs=1800.0)` (async) | `async fn Dreamer::run_loop(watch::Receiver<bool>, interval_secs)` and `run_loop_with(.., due)` | `loop` is a Rust keyword. `asyncio.Event` is a `tokio::sync::watch::Receiver<bool>` (set when it holds true). `run_loop_with` takes the wake's choice of nights (Python's tests patched `due_nights`); an `Err` from it is the wake that raised, warned once a run. |
| `records.reconcile_day` / `records.consolidate` / `records.commit` (as the night calls them) | `trait NightRecords` + `LiveRecords` | The seam Python's tests monkeypatched. `LiveRecords` calls `eve_records`; an `Err` from a fake is a step that raised. |
| `OwnerMemory.ingest_day` / `.tidy` (as the night calls them) | `trait DreamIndex`, implemented for `eve_mem0_memory::OwnerMemory` | `OwnerMemory`'s port never raises; a fake's `Err` is the index failing. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_dream.py` | all 22: `test_constants`, `test_the_current_transcript_day_is_never_due_and_yesterday_waits_for_the_dream_hour`, `test_older_nights_are_due_at_any_hour_and_dreamt_nights_never`, `test_no_transcripts_means_nothing_due`, `test_one_night_runs_its_steps_in_order_in_a_worker_thread_and_marks_the_day`, `test_the_dream_section_is_appended_to_the_journal_with_counts_only`, `test_an_empty_day_is_marked_with_no_model_call`, `test_a_failed_reconcile_is_not_marked_and_is_retried_and_marked_on_the_next_wake`, `test_a_failure_is_logged_once_per_night_and_a_night_that_keeps_failing_steps_aside`, `test_a_raising_reconcile_counts_as_a_failure`, `test_consolidate_or_index_failures_still_mark_the_night`, `test_the_dream_works_with_no_index`, `test_dreaming_a_night_twice_keeps_one_ledger_line`, `test_a_bad_day_string_is_refused_without_a_call`, `test_a_disabled_transcript_store_never_marks_a_night`, `test_the_loop_catches_up_missed_nights_oldest_first_three_a_wake`, `test_the_loop_stops_promptly_on_shutdown`, `test_the_loop_does_not_start_a_night_after_shutdown`, `test_nothing_raises_out_of_the_loop_when_a_step_throws`, `test_the_loop_survives_a_dream_that_raises`, `test_the_night_ends_with_the_report_committed`, `test_the_report_commit_is_real_history` | same names in `src/tests.rs`; plus `test_an_aware_time_in_another_zone_is_read_on_the_local_wall_clock`, `test_an_unreadable_ledger_is_an_empty_one`, `test_a_journal_without_a_final_newline_gets_one_before_the_section`, `test_an_empty_journal_path_is_no_journal_and_gets_no_section`, `test_the_summary_line`, `test_a_ledger_that_cannot_be_written_fails_the_mark_and_the_night_stays_due`, `test_the_loop_runs_the_real_due_nights`, `test_a_day_the_ledger_cannot_encode_stops_the_night`, `test_captured_logs_never_carry_what_was_said` | None. `monkeypatch` of `records.*` is a `NightRecords` fake; of `due_nights` is `run_loop_with`; of `_night` raising is a fake reconcile that panics (the same last net). caplog is a scoped tracing subscriber. |
| `bridge/tests/test_forget_lean.py` (tests_ref) | `test_a_forget_during_the_dreams_model_call_is_not_undone_by_the_dream`, `test_the_dream_after_the_forget_writes_again_from_what_is_left` | same names in `src/tests.rs` | The forget is run by hand (`eve_records::forget_lines` + `squash_history` + `Transcripts::redact`) in place of `memory.Memory.forget_apply` (the memory crate, downstream). The mem0 tests of that file test `OwnerMemory` (the mem0 crate). |
| `bridge/tests/test_taint_v4.py` (tests_ref) | `test_the_dream_reads_no_outside_words` | same name in `src/tests.rs` | The rest of the file tests other crates. |
| `bridge/tests/test_cli_memory.py` (tests_ref) | `test_dream_one_day`, `test_dream_day_refuses_the_day_that_is_not_over` | — | The CLI crate's tests; this crate provides `DreamReport`, `Dreamer::summary` (covered by `test_the_summary_line`) and `due_nights` they use. |
| `bridge/tests/test_lite_memory_parity.py` (tests_ref) | `test_both_start_the_same_dream`, `test_no_memory_or_no_client_is_no_dream_on_either_side` | — | Compare the daemon's and lite's `_start_dream` (the daemon and lite crates). |
| `bridge/tests/test_privacy_guards.py` (tests_ref) | the dream part of the privacy test | — | Drives the whole memory with the real `OpenAIReconcileClient` over a fake `openai` module (the memory and records crates); here the same night steps run against fakes and the real records in the forget tests. |

## Boundaries
None. (mem0 itself sits behind `eve-mem0-memory`'s own boundary.)

## Differences
- A shutdown channel whose sender is dropped ends `run_loop`, as a set event would: an `asyncio.Event` cannot be dropped, and a loop nobody can stop would only outlive the daemon.
- A panic inside a night is caught by the same last net as Python's `except Exception` around `to_thread` and logs `(panic)` as the error name.
- `mark` writes the whole line with `write_all`; Python makes one `os.write` and ignores a short write (a regular file does not write short).
