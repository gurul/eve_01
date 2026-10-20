# eve-memory
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/memory.py` | `src/memory.rs` (re-exported from `src/lib.rs`; unit tests in its `tests` module, ported tests in `src/memory_tests.rs`, `src/search_tests.rs`, `src/records_tests.rs`, fakes in `src/testutil.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| constants `FORGET_TTL_SECS`, `MAX_PENDING_TOKENS`, `RECENT_CHARS`, `MIGRATED_FILE`, `RETRY_DAYS_BACK`, `NOT_COVERED`, `LAYERS`, `CHAT_LOG_NOT_COVERED`, `REWORD_MIN_WORDS`, `REWORD_MIN_SHARE`, `OWN_SECTION`, `EXAMPLE_MARK`, `TEMPLATE_BANNER` | same-named `pub const`s | Values verbatim; tuples are arrays in the same order. |
| `_WORD`, `_COMMON`, `_DAY`, `_READ_REF`, `_STAR_DATE`, `_FRONTMATTER_END` | private `LazyLock` `WORD`, `COMMON`, `DAY`, `READ_REF`, `STAR_DATE`, `FRONTMATTER_END` | The stop-word list is the Python's text, split the same way. Python's `$` is `\n?\z`, `\Z` is `\z`, `\s` adds `\x1c`-`\x1f`. |
| `_keys(text)` | private `keys` | Unicode casefold (`eve_transcripts::py::casefold`), apostrophes dropped, `[a-z0-9]+`, first four letters. Checked against the Python on ß, ligatures and digits (`test_keys_and_reworded_read_words_as_python_does`). |
| `reworded(query)` | `reworded(&str) -> Option<Pred>` | `Pred` is eve-transcripts' `Box<dyn Fn(&str) -> bool + Send + Sync>`. |
| `enabled(environ=None)` | `enabled(Option<&HashMap>)` | `transcripts.configured(environ).enabled`; None reads the process environment. |
| `_records_mod()` | `trait RecordsStore` + `struct EveRecords` | The lazy import tests swap becomes a trait the `Memory` holds (`MemoryOptions::records`); `EveRecords` calls eve-records (`profile`, `profile_for_voice`, `talking_section`, `star`, `hide`, `unhide`, `hidden`, `hidden_match`, `RecordsReader(cfg, None).search/get`, `forget_lines`, `squash_history`, `seal`). Each method returns `anyhow::Result`, where the Python call could raise. |
| `_tool(name, description, properties)` | private `tool` | Same keys in the same order; `required` is the property names in order. |
| `MEMORY_SEARCH`, `MEMORY_READ`, `FORGET_PREVIEW`, `FORGET_APPLY`, `RECENT_CONVERSATION`, `TOOLS`, `VOICE_TOOLS` | `pub static` `LazyLock<Value>` / `LazyLock<Vec<Value>>` | `json.dumps` of each is byte-identical to the Python (sha256 checked). |
| `_SEARCH_OPENING`, `_SEARCH_KEYWORD`, `_SEARCH_RANKED`, `_SEARCH_TIME`, `_SEARCH_RETRY`, `_SEARCH_CAP`, `_TIME_ARG` | private consts | Verbatim. |
| `memory_search_tool(hybrid, timeargs, retry, *, capped=True)` | `memory_search_tool(bool, bool, bool, capped: bool) -> Value` | All twelve distinct switch combinations hash equal to the Python's (`test_every_tool_schema_is_the_pythons_byte_for_byte`). Rust returns an equal value where Python returns the same object. |
| `_split_frontmatter(text)` | private `split_frontmatter` | |
| `_markdown_files(folder)` | private `markdown_files` | os.walk without following links: `.git` and symlinked folders not entered, symlinked files skipped, unreadable folders skipped. |
| `_atomic_rewrite(path, text)` | `atomic_rewrite(&Path, &str) -> io::Result<()>` | Temp `.{name}.{pid}.{8 hex}.tmp`, `O_EXCL|O_NOFOLLOW|O_CLOEXEC`, 0600 then fchmod to the file's mode, plain `fsync(2)`, rename; the temp file removed when a step after its creation fails (never when the exclusive open itself was refused, as in Python). |
| `class MarkdownForgetError(OSError)` (`removed`) | `struct MarkdownForgetError { removed, failed }` (thiserror) | Same message: `{failed} markdown file(s) could not be rewritten`. |
| `forget_markdown(folder, pred, dry_run=False)` | `forget_markdown(&Path, &dyn Fn(&str)->bool, bool) -> Result<usize, MarkdownForgetError>`, and `forget_markdown_with(…, rewrite)` | Files read as text mode reads them (`errors="replace"`, universal newlines); rows split with `splitlines(keepends=True)`'s boundaries. `forget_markdown_with` is the seam the Python test patches `_atomic_rewrite` through. |
| `class _Pending` | private `struct Pending { query, expires }` | Held in an `IndexMap` so the oldest token is evicted first, as dict order does. |
| `class Memory` | `struct Memory` | Built with `Memory::new(cfg, Arc<Transcripts>, Option<Arc<dyn MeaningIndex>>)` or `Memory::with_options(…, MemoryOptions { clock, background, environ, embedder, records, rewrite })`. Fields `cfg`, `transcripts`, `index`, `hybrid`, `timeargs`, `retry`, `ranked` are public as in Python. The `index` duck type (mem0's `search`, `find`, `forget`, optional `forget_matching`) is `trait MeaningIndex`; `forget_matching` defaults to None (not provided). |
| `Memory.__init__` | `Memory::with_options` | The three switches read once (`memory_index::switch`, from `MemoryOptions::environ` or the process environment). With hybrid on: `open_index(cfg.store, transcripts)`, set as the transcripts' ranker, its `notify` added as a listener, and its worker started only with `background`. |
| `Memory.chat_log` (attribute) | `chat_log()`, `set_chat_log(Option<Arc<dyn ChatLogLayer>>)` | `trait ChatLogLayer { count, forget }`, implemented here for `eve_chat_log::ChatLog`. |
| `Memory.profile`, `profile_for_voice`, `talking_section` | same | "" on any failure, with the same log lines. |
| `Memory.today(max_chars, *, exclude_conv, exclude_tail, style, exclude)` | `today(i64, &Today)` | `eve_transcripts::Today` carries the keywords. |
| `Memory.star` | `star(&str) -> Option<String>` | |
| `Memory.hide`, `unhide`, `hidden`, `_hidden_match` | `hide`, `unhide`, `hidden`, private `hidden_match` | None / false / [] on failure. |
| `Memory.tools(*, voice=False)` | `tools(voice: bool) -> Vec<Value>` | Copies. |
| `Memory.handle_tool(name, args, *, since=None, channel="")` | `handle_tool(&str, &Value, Option<Dt>, &str) -> Value` | A non-object `args` is `{}`; `str(args.get(k) or "")` is `str_or_empty`. An error or a panic inside a tool is `{"ok": false, "reason": "memory is unavailable right now"}` with `memory: <name> failed (<class>)`; an unknown name is `unknown tool '<repr>'`. |
| `Memory.search(query, days_back=None, *, date_from=None, date_to=None)` | `search(&str, Option<&Value>)`, `search_with(&str, days_back, date_from, date_to)` | `days_back` read as `int()` reads it (bool, float cut toward zero, a numeric string with sign and underscores); anything else is 30. The mem0 search runs on a scoped thread named `memory-recall` while the records and the transcripts are read, joined at the end. Keys in Python's order (`time_note`, `range`, `fewer_words`, `widened`, `searched_again`, `truncated`, `note`). A falsy `from`/`to` is no time (Python neither reads it nor calls it unread). |
| `Memory._said` | private `said` | `Transcripts::search_with` with `since`/`until` from the range. |
| `Memory._recalled(row)` (staticmethod) | `Memory::recalled(&Value) -> String` | |
| `Memory.read(ref)` | `read(&str) -> anyhow::Result<Value>` | Whitespace folded as `str.split()` folds it; a records failure is the `Err` `handle_tool` turns into "unavailable", as Python's raise is. |
| `Memory.recent_conversation(since, channel)` | `recent_conversation(Option<Dt>, &str) -> Value` | Lengths in code points. |
| `Memory._ranked_for_forget` | private `ranked_for_forget` | The live index, or the file left on disk opened with `embed=false` (closed after). |
| `Memory._count` | private `count` | Layers in `LAYERS` order, then `chat_log` when attached. |
| `Memory._not_covered` | private `not_covered` | |
| `Memory._follow_sources` | private `follow_sources` | `src: YYYY-MM-DD HH:MM` of each matching line's own wall time (`datetime.fromisoformat`, `eve_transcripts::py::fromisoformat`), plus `reworded`. |
| `Memory.forget_preview(query)` | `forget_preview(&str) -> Value` | Token = `secrets.token_urlsafe(12)` (12 random bytes, base64url, no padding: 16 characters). |
| `Memory.forget_apply(token)` | `forget_apply(&str) -> Value` | Same layer order, failure list, history squash only when something went, and the same `reason` text. A ranked-index failure logs `(IndexForgetError)`, the class name only, as Python does. |
| `_file_count(path)` | private `file_count` | |
| `_same(a, b)` | private `same` | `os.path.realpath` is `eve_transcripts::py::resolve` (non-strict), falling back to the path as given. |
| `_private_dir(path)` | private `private_dir -> io::Result<()>` | mkdir -p, then chmod 0700 with its error ignored. |
| `_owner_stars(text)` | `owner_stars(&str) -> Vec<(String, Option<NaiveDateTime>)>` | A date strptime cannot read (bad month, non-ASCII digits) is cut from the claim and dates nothing, as in Python. |
| `migrate(cfg, legacy_store, legacy_mem0)` | `migrate(&RecallConfig, &Path, &Path) -> io::Result<Value>`, and `migrate_with(…, records, rename)` | Python raises out of `migrate` when the memory root cannot be made or an old folder cannot be listed; Rust returns that `io::Error`. Marker: `{counts…, "at": "<local ISO, seconds>"}` + newline, `O_NOFOLLOW`, 0600. `migrate_with` takes the records module and `os.rename` the Python tests patch. |
| (the seam of `time.monotonic`) | `monotonic() -> f64`, `type Clock` | Seconds since the first call. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_memory.py` | all 35 | same names in `src/memory_tests.rs`. `monkeypatch` of `_records_mod` is `MemoryOptions::records` / `migrate_with(records)`; of `_atomic_rewrite` is `MemoryOptions::rewrite`; of `os.rename` is `migrate_with(rename)`; `m.search = 1/0` is a fake records search that panics; mem0 that cannot open is a `MeaningIndex` whose `forget_matching` errs; caplog is a thread-scoped tracing subscriber. Added: `test_every_tool_schema_is_the_pythons_byte_for_byte`, `test_at_most_sixteen_tokens_wait_and_the_oldest_goes_first`, `test_forget_markdown_reads_as_python_reads_and_keeps_every_other_byte`, `test_a_chat_log_is_a_layer_counted_and_forgotten`, `test_owner_stars_read_only_the_owners_section_and_date_what_is_dated`, and unit tests in `memory.rs` (`test_keys_and_reworded_read_words_as_python_does`, `test_frontmatter_splits_as_python_does`, `test_owner_stars_cut_a_date_strptime_cannot_read`, `test_tool_arguments_are_read_as_python_int_reads_them`) | None. |
| `bridge/tests/test_memory_search.py` | `test_tool_schema_all_off_is_the_old_one_and_each_switch_shapes_it`, `test_forget_reaches_the_index_rows_and_vectors`, `test_forget_reaches_words_past_the_first_2000_chars_of_a_round`, `test_forget_cleans_an_index_left_on_disk_with_hybrid_off`, `test_a_forget_that_cannot_clean_the_index_says_so`, `test_only_the_daemon_starts_the_index_worker`, `test_a_range_limits_the_words_and_widens_once`, `test_retry_searches_again_wider_and_looser`, `test_voice_is_not_told_of_a_cap_it_does_not_have` | same names in `src/search_tests.rs` (the same bag-of-words `FakeEmbedder`, passed as `MemoryOptions::embedder`; the switches as `MemoryOptions::environ`); plus `test_with_hybrid_on_the_transcripts_search_is_ranked_and_new_lines_reach_the_index`. `test_a_forget_that_cannot_clean_the_index_says_so` makes the index refuse the delete with a temp trigger instead of patching `_compact` (private to eve-transcripts, which tests the retry count itself). | The switch, resolver, units and index tests are eve-transcripts' (ported there). `test_telegram_allows_three_memory_reads_a_turn_with_retry_on`, `test_two_telegram_turns_at_once_keep_their_own_memory_count` test `telegram.py` (the brain crate). `test_eval_features_flag_sets_every_switch` tests `tools/memory_eval.py`. |
| `bridge/tests/test_memory_records.py` | `test_forget_follows_the_src_pointer_to_a_fact_in_other_words`, `test_with_sources_off_forget_still_follows_a_src_already_written`, the `Memory._recalled` assertion of `test_mem0_search_shows_src_and_a_forget_reaches_a_memory_by_its_src` | same names in `src/records_tests.rs` (`EVE_MEMORY_SOURCES` through `eve_records::with_seams`); `test_mem0_search_shows_src` | The record-file tests are eve-records' (ported there); the mem0 ingest/search/forget_matching tests are `mem0_memory.OwnerMemory`'s (the mem0-memory crate). |
| `bridge/tests/test_memory_verdict.py` | — | — | Every row checks `memory_index` and `records` switch defaults against the eval's VERDICT.md: ported in eve-transcripts and eve-records. Nothing in it reads memory.py. |
| `bridge/tests/test_memory_eval.py` | — | — | Tests `tools/memory_eval.py` (scoring, split, seal, temp guard, build cache); the tool is not a crate module. Its forget-on-a-copy path is the `forget_preview`/`forget_apply` covered above. |
| `bridge/tests/test_forget_reworded.py` (tests_ref) | all 5 | same names in `src/records_tests.rs`, plus `test_reworded_reaches_the_archive_too` | None. |
| `bridge/tests/test_chat_memory.py` (tests_ref) | `test_forget_removes_lines_from_the_log` | `test_a_chat_log_is_a_layer_counted_and_forgotten` (on the real `eve_chat_log::ChatLog`) | The rest test the chat log and the Telegram inlet. |
| `bridge/tests/test_forget_lean.py` (tests_ref) | — | — | Drives `forget_apply` from inside the dream's model call and mem0's ingest: the dream and mem0-memory crates. |
| `bridge/tests/test_claude_launch.py`, `test_cli_memory.py`, `test_daemon_transcripts.py` (tests_ref) | — | — | They test the CLI, the daemon's memory wiring and claude_launch's use of `TOOLS`' names; ported with those crates. |

Differential checks run during the port (expected values taken from the Python): `_keys` and `reworded` on Unicode and digits, `_split_frontmatter` edge cases, `_owner_stars` with a bad date and non-ASCII digits, and the sha256 of every tool schema.

## Boundaries
None. (git runs inside eve-records; the embeddings call inside eve-transcripts.)

## Differences
- The transcripts hold the ranked index (as ranker and as listener) through a weak reference, so the index (which holds the transcripts) and the transcripts do not keep each other alive. While the `Memory` lives, this is the Python behavior; once it is dropped, its transcripts search by keyword (Python keeps the closed-over index until garbage collection).
- `handle_tool` turns a panic inside a tool into the "unavailable" answer (Python catches `Exception`); the log names it `panic`.
- `days_back` past 64 bits, or a non-finite float, reads as the default (Python keeps the big int, which the transcripts clamp to 3650 days; `int(inf)` raises `OverflowError`, which `handle_tool` turns into "unavailable").
- The Rust transcripts' `redact` and `find` never raise (a file they cannot rewrite is logged by them), so the "transcripts" layer cannot appear in `failed`; Python's try is defensive in the same place.
