# eve-transcripts
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/transcripts.py` | `src/transcripts.rs` (tests in `src/transcripts_tests.rs`) |
| `bridge/src/eve_bridge/memory_index.py` | `src/memory_index.rs` (tests in `src/memory_index_tests.rs`) |
| (Python builtins: `int()` of decimal digits, `str.split/strip/splitlines/casefold`, `json.dumps(ensure_ascii=False)`, `json.loads`, `datetime.fromisoformat`, `os.path.abspath`, `Path.resolve/expanduser`) | `src/py.rs` |

## API

### transcripts.py

| Python | Rust | Notes |
|---|---|---|
| module constants (`MEMORY_DEFAULT` … `SYNCED_UNDER_HOME`, `TALK`, `BUS`, `CHANNELS`, `WHO`, `KINDS`, `RENDER_KINDS`, `MEETING`, `NOTE`, `FORGOTTEN`, `OUTSIDE_LEFT_OUT`, `LOCK_FILE`, `NEVER_INDEX`) | same-named `pub const`s | Values verbatim; tuples are arrays in the same order. |
| `_DAY_FILE`, `_DAY`, `_DAY_NOTE`, `_HHMM`, `_CONV`, `_PHRASE`, `_NOTE_TIME`, `_FRONTMATTER` | private `LazyLock<Regex>` `DAY_FILE`, `DAY`, `DAY_NOTE`, `HHMM`, `CONV`, `PHRASE`, `NOTE_TIME`, `FRONTMATTER` | Python's `$` (which also matches before a final `\n`) is `\n?\z`. |
| `Stamp` | `enum Stamp<'a> { At(Dt), Text(&str) }` | `From<Dt>`, `From<&str>`, `From<&String>`. |
| `class TranscriptConfig` | `struct TranscriptConfig { enabled, root, home }` | `home: Option<PathBuf>` is a Rust-only seam: the home folder the guards check (None = `Path.home()`), so tests never touch `$HOME`. `Default` is Python's defaults. |
| `_on` | private `on` | `1/true/yes/on`, stripped, case-insensitive. |
| `configured(environ=None)` | `configured(Option<&HashMap<String,String>>)` | None reads the process environment. |
| `refusal(root)` | `refusal(&Path) -> String`, plus `refusal_in(&Path, home)` | Same three rules, same messages, same order (symlink, synced folder, git work tree, unreadable parent). `is_symlink`/`exists` ignore exactly pathlib's errnos (ENOENT, ENOTDIR, EBADF, ELOOP); another error is Python's raise (see Differences). |
| `_prepare(root)` | `prepare(&Path) -> String`, plus `prepare_in` | Public: `memory.py` calls `transcripts._prepare`. Error text names Python's exception class (`PermissionError`, …) via `py::os_error_name`. |
| `match(query)` | `match_query(&str) -> Option<Pred>` | `match` is a Rust keyword. Casefold is Unicode default case folding (`caseless`). |
| `class TranscriptStats` | `struct TranscriptStats { appended, failed, rejected }` | Read with `Transcripts::stats()` (a snapshot). |
| `class ConvInfo` | `struct ConvInfo` | Same fields; equality compares instants, as aware datetimes do. |
| `_aware` | folded into `parse_ts` / `py::local_aware` | Every Rust moment is already aware (`DateTime<FixedOffset>`); a naive ISO string is read as local time. |
| `_parse_ts` | `parse_ts(&str) -> Option<Dt>` | Python 3.11 `fromisoformat` forms (`py::fromisoformat`). Public: chat_log reads it. |
| `_flat` | `flat` (and `py::flat`) | Python's whitespace set (Unicode White_Space plus `\x1c`-`\x1f`). |
| `_clip` | `clip` (and `py::clip`) | Counted in code points; `rstrip` then `…`. |
| `_conv_epoch` | `conv_epoch` | Local time of the id's epoch ms; None past year 9999, where `fromtimestamp` raises. |
| `_next_day` | `next_day -> Option<String>` | Any decimal digit, as strptime reads it; None for an impossible date (see Differences). |
| `speaker` | `speaker(&Line) -> String` | |
| `_mode` | `mode(&str)` | |
| `_outside_words` | `outside_words(&Line)` | `eve_tool_gate::RESULT_LINES` / `taints`. |
| `render_line` | `render_line(&Line, style)` | |
| `render` | `render(&[L], style)` | |
| `_parse_line` | `parse_line(&str) -> Option<Line>` | `json.loads` incl. bare `NaN`/`Infinity` (`py::loads`). `Line = serde_json::Map` (insertion-ordered). |
| `class Transcripts` | `struct Transcripts` | `Send + Sync`; share it as `Arc<Transcripts>`. Constructor `Transcripts::new(config, archive, wall)`. |
| `Transcripts.__init__` | `Transcripts::new` | Same guards, lock file, and log lines. |
| `Transcripts.listeners` (list attribute) | `add_listener(Listener)` | A panicking listener is caught and logged, as Python catches `Exception`. |
| `Transcripts.ranker` (attribute) | `set_ranker(Option<Arc<dyn Ranker>>)`, `ranker()` | `trait Ranker` is the seam Python fills with duck typing; `MemoryIndex` implements it. |
| `Transcripts.stats` (attribute) | `stats() -> TranscriptStats` | |
| `SEARCH_BUDGET_SECS` (patched in a test) | `Transcripts::search_budget_secs` field | Defaults to the constant. |
| `Transcripts.now` | `now()` | |
| `Transcripts.day_of` | `day_of(impl Into<Stamp>)` | |
| `Transcripts.path_of` | `path_of` | |
| `Transcripts.days` | `days` | |
| `Transcripts.new_conv` | `new_conv` | `<c>-<epoch ms>-<4 hex>`, never repeated within the process (4096-id memory). |
| `Transcripts._flock` | private `flock` | `libc::flock` on the `.lock` descriptor. |
| `Transcripts._fail` | private `fail` | Logged once an hour, with Python's exception class name. |
| `Transcripts.append(..., tool, now, *, fsync, via, att, mid, outside, src)` | `append(ch, conv, who, kind, text)` and `append_with(..., Append { tool, now, fsync, via, att, mid, outside, src })` | Byte-identical line (`py::dumps`: `", "`/`": "` separators, non-ASCII raw); one `write` under the thread lock and `flock`; fchmod 0600; torn-tail newline; `fsync` is plain `fsync(2)`. |
| `Transcripts._tell` | private `tell` | |
| `Transcripts.close` | `close(ch, conv, now)` | |
| `Transcripts._cached` | private `cached` | Keyed by (mtime ns, size, inode); file read as text mode does (lossy UTF-8, universal newlines). |
| `Transcripts._invalidate` | private `invalidate` | |
| `Transcripts.lines` | `lines(day) -> Vec<Arc<Line>>` | Shared with the cache (`Arc::ptr_eq` holds where Python's `is` does); marks `out` lines in `tool_gate::MARKS`. |
| `Transcripts._conv_day` | private `conv_day` | |
| `Transcripts.conv_lines` | `conv_lines` | |
| `Transcripts.conversations` | `conversations` | |
| `Transcripts.last_turn_at` | `last_turn_at` | |
| `Transcripts.lines_since(ts, exclude_ch, kinds)` | `lines_since(ts, exclude_ch)`, `lines_since_kinds(ts, exclude_ch, kinds)` | |
| `Transcripts.today_block(max_chars, exclude_conv, exclude_tail, kinds, style, now, exclude)` | `today_block(max_chars)`, `today_block_with(max_chars, &Today { … })` | `exclude` is an ordered `Vec<(conv, tail)>`. Lengths in code points. |
| `Transcripts._header` | private `Transcripts::header` | |
| `Transcripts.day_text(day, max_chars, *, outside)` | `day_text(day, max_chars, outside)` | |
| `Transcripts.read(day, start, end, max_chars)` | `read(day, start, end, max_chars)` | |
| `Transcripts._minute` | private free fn `minute` | |
| `Transcripts._doc_days` | private `doc_days` | |
| `Transcripts._parse_doc` | private `Transcripts::parse_doc` | |
| `Transcripts._docs` | private `docs` | |
| `Transcripts.search(query, days_back, limit, channel, *, since, until, loose)` | `search(query)`, `search_with(query, &Search { … })` | Returns `serde_json::Value` with Python's keys and order; capped at 3000 chars of `json.dumps`. |
| `Transcripts._capped` | private `capped` | |
| `Transcripts._keyword` | private `keyword` | Stable sorts as Python's. |
| `Transcripts._neighbour` | private `neighbour` | |
| `Transcripts.find` | `find -> Vec<Line>` | Each with `"day"`. |
| `Transcripts.redact(query, dry_run)` | `redact(query, dry_run) -> usize` | Same rules (eve's answer, Sources scrubbed, torn lines dropped); unchanged lines kept byte for byte. |
| `Transcripts._redact_file` | private `redact_file` | |
| `Transcripts._atomic_write` | private free fn `atomic_write` | Same temp name `.{name}.{pid}.{8 hex}.tmp`, 0600, `O_EXCL|O_NOFOLLOW`, fsync, rename, directory fsync. |
| (the index's view of the store) | `impl DaySource for Transcripts` | |

### memory_index.py

| Python | Rust | Notes |
|---|---|---|
| `HYBRID_ENV`, `TIMEARGS_ENV`, `RETRY_ENV` | same `pub const`s | |
| `SWITCHES`, `DEFAULTS` | `SWITCHES: [(key, var); 3]`, `DEFAULTS: [(key, bool); 3]` | Same order and values (all off: VERDICT.md). |
| other constants (`INDEX_SUBDIR` … `FORGOTTEN`, `STOPWORDS`, `SCHEMA`) | same-named `pub const` / `pub static` | Verbatim. `META` is `meta()`. |
| `_WORD` (`[\p{L}\p{N}_]+`: Python's `\w`, no combining marks), `_MONTHS`, `_MON`, `_WEEKDAYS`, `_WD`, `_NUMBERS`, `_NUM` | private `WORD`, `MONTHS`, `MON`, `WEEKDAYS`, `NUM` and the `num` table | `_MONTHS` keeps dict order; `_MON` sorted longest first, stable. English names, as `calendar` gives in the C locale. |
| Debounce/idle/candidates/min-cos/compact-waits constants (patched in tests) | `Tuning` on `MemoryIndex::tuning` | Defaults are the constants. |
| `switch(name, environ=None)` | `switch(&str, Option<&HashMap>)` | |
| `owner_day` | `owner_day(Dt) -> NaiveDate` | |
| `_num` | private `num` | |
| `_month_range` | private `month_range` | |
| `_shift_month` | private `shift_month` | |
| `_past_date` | private `past_date` | |
| `resolve_when` | `resolve_when(&str, Dt)` | Differential run against the Python over 248 phrase/clock pairs: identical. |
| `_resolve_core` | private `resolve_core` | Same branch order. |
| `resolve_args` | `resolve_args(Option<&str>, Option<&str>, Dt)` | Callers pass `str(v)` of a non-None value. |
| `widen` | `widen` | |
| `terms` | `terms` | |
| `fts_query` | `fts_query` | |
| `class Unit` (`body`, `hash`) | `struct Unit` with `body()`, `hash()` | Hash is sha256 of `text-embedding-3-small|512|<body>`: equal to the Python's, so either daemon reuses the other's vectors. |
| `_flat` | `py::flat` | |
| `_clip` | `py::clip` | |
| `units_of` | `units_of(day, &[L], &dyn Fn(&Line) -> String)` | |
| `Embedder` | `type Embedder = Arc<dyn Fn(&[String], f64) -> anyhow::Result<Vec<Vec<f64>>>>` | |
| `openai_embedder` | `openai_embedder() -> Option<Embedder>` | `eve_openrouter::client(timeout, max_retries=1)`, `/embeddings` with `model`, `input`, `dimensions`; `eve_spend::record_embedding(MEMORY, …)`. Each call runs on its own thread (the blocking client panics on a tokio thread); one client per timeout kept for the process. |
| `_unit_vec` | `unit_vec -> Vec<f32>` | float32, divided by its norm; stored little-endian as numpy's `tobytes()` on the Mac. |
| `index_path` | `index_path` | |
| `class IndexForgetError` | `struct IndexForgetError(String)` (thiserror) | Carries the sqlite3 exception class name. |
| `class MemoryIndex` | `struct MemoryIndex` | Share as `Arc<MemoryIndex>`; implements `Ranker`. |
| `MemoryIndex.__init__` | `MemoryIndex::new(path, Arc<dyn DaySource>, Option<Embedder>)` | `ok()`, `built()`/`set_built()`, `has_worker()` expose `ok`, `_built`, `_worker`. |
| `MemoryIndex._open` | private `open` | Same folder/file modes, pragmas, schema, FTS5 secure-delete, meta check (rusqlite bundled SQLite has FTS5 and ≥ 3.44). |
| `MemoryIndex.close` | `close` | Joins the worker up to 5 s. |
| `MemoryIndex._sig` | private `sig` | `mtime_ns:size:ino`, the same string the Python writes. |
| `MemoryIndex.refresh` | `refresh(Option<&[String]>, embed)` | |
| `MemoryIndex._refresh` | private `refresh_inner` | One transaction, rollback on error. |
| `MemoryIndex._rollback` | inline `ROLLBACK` | |
| `MemoryIndex._drop_day` | private free fn `drop_day` | |
| `MemoryIndex._index_day` | private `index_day` | `reply` stored as `json.dumps(list, ensure_ascii=False)`. |
| `MemoryIndex._gc` | private free fn `gc` | |
| `MemoryIndex.embed_missing` | `embed_missing(budget_secs)` | |
| `MemoryIndex.ensure_built` | `ensure_built` | |
| `MemoryIndex.start` | `start(self: &Arc<Self>)` | Thread named `memory-index`. |
| `MemoryIndex.notify` | `notify` | |
| `MemoryIndex._run` | private `run` | Same debounce / idle-embed loop (a Condvar event for `threading.Event`). |
| `MemoryIndex.search` | `search(query, first_day, last_day, limit, channels) -> Option<Value>` | Same SQL, same 0.3·BM25/best + 0.7·cos, same MIN_COS rule. |
| `MemoryIndex._hit` | private free fn `hit` | |
| `MemoryIndex._matching` | private `MemoryIndex::matching` | |
| `MemoryIndex.count` | `count(&dyn Fn(&str) -> bool)` | |
| `MemoryIndex.forget` | `forget(pred) -> Result<usize, IndexForgetError>` | |
| `MemoryIndex._compact` | private `compact` | `optimize` then `VACUUM`. |
| `MemoryIndex._retry` | private `retry` | Waits from `Tuning::compact_waits`. |
| `MemoryIndex.stats` | `stats() -> IndexStats { units, vectors, days }` | |
| `MemoryIndex._lock` / `_db` (used by tests and the eval) | `with_db(|conn| …)` | |
| `open_index(store, transcripts, embedder, *, embed)` | `open_index(&Path, Arc<dyn DaySource>, Option<Embedder>, embed)` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_transcripts.py` | all 34 | `src/transcripts_tests.rs`: one `test_*` per Python test, same names and assertions | None. `caplog` is a per-thread capture on a global tracing subscriber; `monkeypatch` of HOME is `TranscriptConfig.home` / `refusal_in`; the socket patch is a source scan for network crates; `SEARCH_BUDGET_SECS` is `search_budget_secs`. |
| (beyond the Python file) | — | `test_append_writes_the_optional_keys_in_order_and_marks_outside_words`, `test_listeners_hear_each_day_and_a_panicking_one_costs_nothing`, `test_bus_lines_are_searched_but_never_shown_as_talk`, `test_python_written_lines_load_and_a_naive_time_is_local`, `test_odd_bytes_on_disk_are_read_as_python_reads_them`, `test_conversation_ids_and_days_read_as_python_reads_them`, `test_a_day_file_name_never_spans_a_newline`; `py.rs` unit tests | |
| `bridge/tests/test_memory_search.py` (tests_ref) | `test_switches_are_off_unless_set`, the three resolver tests, `test_units_are_rounds_an_owner_line_with_eves_reply`, `test_hybrid_finds_what_keyword_and_misses`, `test_hybrid_respects_the_range_and_the_char_cap`, `test_falls_back_to_keyword_and_when_embeddings_or_index_are_down`, `test_a_new_line_is_searchable_at_once_and_reaches_the_index_off_the_loop`, `test_a_growing_round_leaves_no_vectors_behind_and_the_worker_embeds_it_once_quiet`, `test_bm25_candidates_come_from_the_searched_days`, `test_vectors_are_kept_by_text_so_a_rebuild_embeds_nothing_twice`, `test_loose_search_ranks_the_rarer_word_first`; the index side of `test_forget_reaches_the_index_rows_and_vectors`, `test_forget_reaches_words_past_the_first_2000_chars_of_a_round`, `test_forget_cleans_an_index_left_on_disk_with_hybrid_off` (as `test_forget_cleans_an_index_opened_with_no_embedder`), `test_a_forget_that_cannot_clean_the_index_says_so`, `test_only_the_daemon_starts_the_index_worker` | `src/memory_index_tests.rs` (same `FakeEmbedder`) plus `test_resolver_knows_the_rest_of_its_set`, `test_words_and_numbers_are_read_as_python_reads_them`, `test_terms_and_the_fts_query`, `test_a_channel_narrowed_search_keeps_to_its_channels`, `test_open_index_follows_the_transcripts_guards`, `test_a_file_of_another_model_is_built_again` | `test_tool_schema_*`, `test_a_range_limits_the_words_and_widens_once`, `test_retry_searches_again_wider_and_looser`, the Telegram three-reads tests and `test_eval_features_flag_sets_every_switch` test `memory.py`, `telegram.py` and `tools/memory_eval.py` (other crates / a tool); the Memory-level halves of the forget tests (`forget_preview`/`forget_apply` layers) belong to eve-memory. |
| `bridge/tests/test_memory_verdict.py` (tests_ref) | `test_every_switch_default_equals_the_verdict`, `test_an_unset_switch_reads_its_default_and_a_set_one_reads_its_value` (memory_index rows) | same names; the first reads `bridge/tests/fixtures/memory_eval/VERDICT.md` | The `records` rows and `test_each_feature_in_the_verdict_has_its_holdout_run` belong to eve-records / the eval fixtures. |
| other `tests_ref` files | — | — | They drive this crate through memory, telegram, dream, link_chat, voice and the CLIs; their behavior is ported with those crates. |

## Boundaries
None. (The embeddings call is an HTTP request through eve-openrouter, as in the Python.)

## Differences
- Calendar-arithmetic overflow (`"99999999999 days ago"`, a year outside 1–9999, `"0000-01-01"`) makes `resolve_when`/`widen` answer None or keep the bound instead of raising `OverflowError`/`ValueError` out of the call.
- `conversations` on a day string that matches `YYYY-MM-DD` but is not a date (`2026-13-45`) returns `[]`; the Python raises `ValueError` from `strptime`.
- A line rewritten by `redact` that held a bare `NaN`/`Infinity` or an integer past 64 bits in some key writes it back as `null` / a float (serde_json holds neither); the Python writes it back as it was. eve never writes such a value (every key it writes is a string, a list of string maps, or `out: 1`).
- `Path.expanduser` of `~user/…` (another account's home) is left as written; only `~` and `~/…` expand.
- Where Python's `refusal` raises (an lstat of the root or a stat of `~/<synced>` failing with an errno pathlib does not ignore, e.g. EACCES), `refusal` returns `the folder could not be checked (<Error>)`, the text `_prepare` gives for that raise, so the folder is refused instead of an exception reaching `open_index`.
- A listener that panics is caught (`catch_unwind`) where Python catches `Exception`; a log names it `panic`.
- The norm in `unit_vec` is summed in f64 before the float32 division (numpy sums in float32 via BLAS): a vector may differ in its last bit; cosines are unchanged at any precision the ranking uses.
- Shared quirk kept on purpose: a raw U+2028/U+2029/`\x1c`–`\x1e`/`\x85` inside a line's text (json.dumps leaves them raw) splits that line under `str.splitlines`, so both daemons skip it as torn when reading. Verified byte for byte: a Python-written and a Rust-written day file (with via/mid/att/out/src, `\x01`, U+2028, quotes, backslashes) are identical before and after the same `redact`.
