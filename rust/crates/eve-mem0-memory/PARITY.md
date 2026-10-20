# eve-mem0-memory
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/mem0_memory.py` | `src/mem0_memory.rs` (the index), `src/python.rs` + `src/mem0_helper.py` (mem0 itself, in a helper Python process; see Boundaries); tests in `src/tests.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_MODEL`, `EXTRACT_MAX_OUTPUT_TOKENS`, `DEFAULT_EMBED_MODEL`, `EMBED_DIMS`, `EMBEDDERS`, `GEMMA_MODEL`, `GEMMA_REVISION`, `GEMMA_DIMS`, `GEMMA_QUERY`, `GEMMA_DOCUMENT`, `GEMMA_HOME`, `OWNER`, `INGESTED_CONVS`, `HISTORY_DB`, `MAX_HITS`, `MIN_SCORE`, `SEARCH_TIMEOUT_SECS`, `MAX_SEARCHES_IN_FLIGHT`, `RETRY_AFTER_SECS`, `TIMEOUT_LOG_EVERY_SECS`, `CHUNK_MESSAGES`, `MESSAGE_CHARS`, `GET_ALL_CAP`, `STAR`, `CUSTOM_INSTRUCTIONS`, `PLACEHOLDER_KEY` | same-named `pub const`s | Values verbatim. `GEMMA_QUERY`/`GEMMA_DOCUMENT` are also in the helper, where the encoding happens. |
| `_DAY` | private `DAY: LazyLock<Regex>` | `\d` is Unicode in both. |
| `class Mem0ForgetError` | `struct Mem0ForgetError(String)` (`thiserror`) | `forget`/`forget_matching` return `Result<usize, Mem0ForgetError>` where Python raises: `"mem0 is not open"`, or the listing's exception class name. |
| `class Mem0Config` | `struct Mem0Config { enabled, home, model, embed_model, embedder }`, `Default`, `Mem0Config::new(enabled, home)` | Same defaults (`~/.config/eve-bridge/memory/mem0` expanded, `gpt-6-luna`, `text-embedding-3-small`, `openrouter`); frozen dataclass → `Clone + Eq + Hash`. |
| `embedder_of(environ=None)` | `embedder_of(Option<&HashMap>) -> String` | Stripped, lower-cased; anything outside `EMBEDDERS` is `openrouter`. `None` reads the process environment. |
| `configured(recall_cfg, environ=None)` | `configured(&RecallConfig, Option<&HashMap>) -> Mem0Config` | On with `eve_transcripts::configured(env).enabled` (`EVE_MEMORY`, or the older `EVE_RECORDS`); `EVE_MEM0` in 0/false/no/off turns it off; gemma home `mem0/gemma` with `GEMMA_MODEL`. |
| `available()` | `python::available()`, `available_with(&Path)` | `importlib.util.find_spec("mem0")` run in the mem0 interpreter (never importing mem0); see Differences for the one-minute memory. |
| `mem0_config(cfg)` | `mem0_config(&Mem0Config) -> Value` | Same dict, same key order (serialised byte-identically); paths are `str(Path)`. |
| `make_private(home)` | `make_private(&Path)` | mkdir -p; a symlinked home is left alone; home and every folder 0700, every file 0600, top-down, symlinks never followed, errors ignored. |
| `open_mem0(cfg)` | `open_mem0(&Mem0Config) -> anyhow::Result<Box<dyn Mem0Backend>>` | `cfg.home` created, then `PythonMem0::open`: the helper runs Python's `open_mem0` verbatim (`MEM0_TELEMETRY=False` and `MEM0_DIR=<home>/home` set before the import, the import refused while telemetry reads on, `HF_HUB_DISABLE_TELEMETRY` for gemma, `Memory.from_config(mem0_config(cfg))`). A failure is the Python exception by class (`NoKey`, `ModuleNotFoundError`, `RuntimeError`, …) as a `Mem0Error`. |
| `on_openrouter(memory, embeddings=True)` | helper `on_openrouter(memory, init, embeddings=True)` | Verbatim, including `bounded_create` (`max_completion_tokens = EXTRACT_MAX_OUTPUT_TOKENS` unless the call set a limit) and `RuntimeError("mem0: its model clients cannot be moved to OpenRouter")`. `openrouter.client()` is the helper's `openrouter_client`: the openai SDK on `eve_openrouter::BASE_URL` with `eve_openrouter::HEADERS`, key from `OPENROUTER_API_KEY` (stripped; `NoKey` without), and the model rewrite in `_build_request` with ids computed by `eve_openrouter::model` for the config's two models. |
| `on_gemma(embedder)` | helper `on_gemma(embedder)` | Verbatim: `search` encodes as `query`, everything else as `document`, normalised; takes the Memory or the embedder. |
| `meter(memory, cfg)` | helper `meter(memory, init)` + `python::record_spend(&Ledger, &Mem0Config, kind, model, &reply)` | The helper wraps the two `create`s exactly as Python does (once; `_eve_metered`; the two "not metered" warnings) and sends each reply as a spend line; Rust records it with `eve_spend` (`record_chat_completion(MEMORY, reply, m or cfg.model, "openrouter")`, `record_embedding(MEMORY, m or cfg.embed_model, {"usage"}, "openrouter")`). The reply crosses as `spend._as_dict` / `_usage_of` would read it. |
| `_rows(found)` | private `rows(&Value) -> Result<Vec<Map>>` | `found.get("results", [])` of a dict, else `found or []`, dict rows only; a value Python cannot iterate is its `TypeError`. |
| `_norm(text)` | private `norm` | Python's whitespace split/join, `strip(" .,;:!")`, Unicode casefold. |
| `_src_of(row)` | private `src_of` | |
| `_match_text(row)` | private `match_text` | `<memory> {src: …} {src: …}` (src, then src_before). |
| `_sources_on()` | private `sources_on` | `eve_records::records::sources_on(None)` (the process environment, or the thread's `eve_records::Seams`). |
| `_stamp(ts)` | private `stamp` | `datetime.fromisoformat(str(ts)).strftime("%Y-%m-%d %H:%M")` via `eve_transcripts::py::fromisoformat`; "" when unreadable. |
| `_day_of(row)` | private `day_of` | `day`, then `note`, then `created_at`. |
| `class OwnerMemory` | `struct OwnerMemory` (`Send + Sync`), `OwnerMemory::new(cfg, Option<Factory>)`, `with_clock(cfg, factory, Clock)`, `cfg()`, `home()`, `set_search_timeout(secs)` | `factory` is `Arc<dyn Fn(&Mem0Config) -> Result<Box<dyn Mem0Backend>>>` (None = `open_mem0`); `clock` monotonic seconds. Every call to mem0 is under one mutex. `set_search_timeout` is the seam Python's tests use by patching `SEARCH_TIMEOUT_SECS`. |
| `OwnerMemory._memory` | private `Inner::memory` | Opens on first use; a failure is retried after RETRY_AFTER_SECS; same two `make_private` calls and log lines (`mem0: on — … extracts, … embeds, stored in …`, `mem0: unavailable (<Class>) — tried again in 10 minutes`). |
| `OwnerMemory.warm` | `warm()` | Opens and embeds `"warm"` as a search; never fails. |
| `OwnerMemory._search_now` | private `Inner::search_now` | `search(query, filters={"user_id": "owner"}, top_k=limit)`; drops empty text, scores under MIN_SCORE (`float(score or 0)`, unreadable = 0), ★ candidates and repeats by `norm`; hits `{"text", "day", "channel"[, "src"]}`. |
| `OwnerMemory.search` | `search(&str, usize) -> Vec<Value>`; `impl eve_records::MeaningSearch` (limit MAX_HITS) | A `mem0-search` thread waited for SEARCH_TIMEOUT_SECS; at MAX_SEARCHES_IN_FLIGHT stuck searches it answers `[]` at once; the timeout log at most once an hour; `max(1, min(limit, MAX_HITS))`. |
| `OwnerMemory._all` | private `all` | `get_all(filters={"user_id": "owner"}, top_k=GET_ALL_CAP)`. |
| `OwnerMemory.find` | `find(&dyn Fn(&str) -> bool) -> Vec<Found>` | `Found { id, text }` is Python's `{"id", "text"}`. |
| `OwnerMemory._done` | private `done` | Lines of `<home>/ingested_convs`, universal newlines. |
| `OwnerMemory._mark` | private `mark` | `O_WRONLY|O_CREAT|O_APPEND|O_NOFOLLOW`, 0600, one `conv\n`. |
| `OwnerMemory.messages` (static) | `OwnerMemory::messages(&[impl Borrow<Line>]) -> Vec<Message>` | `Message { role, content }` (`to_value()` is the `{"role", "content"}` dict); `out` lines become `transcripts.OUTSIDE_LEFT_OUT`; clipped to MESSAGE_CHARS code points. |
| `OwnerMemory.line_units` (static) | `OwnerMemory::line_units(&[impl Borrow<Line>]) -> Vec<Unit>` | `Unit = (Vec<Message>, src, src_before)`. |
| `OwnerMemory.ingest_day(transcripts, day)` | `ingest_day(&dyn Conversations, &str) -> usize` | `trait Conversations` (`conversations`, `conv_lines`) is what Python duck-types; implemented for `eve_transcripts::Transcripts`. Same chunking, metadata `{day, channel, conv[, src][, src_before]}`, forget-generation check, mark-only-when-all-went-in, and log lines. |
| `OwnerMemory._delete` | private `delete` | Each failure logged, the rest still go. |
| `OwnerMemory.tidy` | `tidy() -> usize` | Oldest first by `str(created_at or "")` (stable). |
| `OwnerMemory.forget(ids)` | `forget(&[impl AsRef<str>]) -> Result<usize, Mem0ForgetError>` | Blank ids dropped; nothing asked is `Ok(0)` without opening. |
| `OwnerMemory.forget_matching(match)` | `forget_matching(&dyn Fn(&str) -> bool) -> Result<usize, Mem0ForgetError>` | List and delete under one hold of the lock. |
| `OwnerMemory._scrub(ids)` | private `scrub` (+ `scrub_history`, `vacuum`) | rusqlite on the same `history.db`: busy timeout 10 s, `PRAGMA secure_delete = ON`, `DELETE FROM history/history_old WHERE memory_id = ?` where that column exists, `DELETE FROM messages`, one commit; then `VACUUM` on history.db and every `*.sqlite` under `qdrant/` (sorted, symlinks skipped). Failures logged with the `sqlite3` class name. |
| `_shared`, `_shared_lock` | private `SHARED: LazyLock<Mutex<HashMap<String, Arc<OwnerMemory>>>>` | Keyed by `str(cfg.home)`. |
| `shared(recall_cfg, environ=None)` | `shared(&RecallConfig, Option<&HashMap>) -> Option<Arc<OwnerMemory>>`, `shared_with(…, available)` | `shared_with` takes the installed-check as a seam (Python's tests patch `available`). |
| — | `trait Mem0Backend` (`search`, `get_all`, `add`, `delete`, `embed`), `struct Mem0Error { kind, message }`, `error_name(&anyhow::Error)`, `type Factory`, `type Clock` | The mem0 `Memory` calls eve makes, an exception from the mem0 side by class name, and the `type(e).__name__` a log line prints. |
| — | `python::PythonMem0` (`open`, `open_at`, `init`, `call`, `impl Mem0Backend`), `python::default_python()`, `PYTHON_ENV`, `HELPER` | The boundary (below). `open_at` takes the OpenRouter base URL (the live test's local server); `open` is `eve_openrouter::BASE_URL`. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_mem0_memory.py` | all 19 | same names in `src/tests.rs`. `test_telemetry_is_off_and_mem0_home_is_ours_before_the_real_import` → `test_telemetry_is_off_and_mem0_home_is_ours_before_the_import` (the helper driven over its protocol with fake `mem0`/`openai` packages on `PYTHONPATH`) plus the live `test_real_mem0_opens_with_telemetry_off_and_its_home_ours` (real mem0ai, `#[ignore]`; run and passing against the bridge venv's mem0ai 2.2.1 with `EVE_MEM0_PYTHON`). `test_extraction_and_embeddings_go_to_openrouter_with_no_openai_key` → `test_extraction_and_embeddings_go_to_openrouter_metered_with_the_extraction_budget` (fake openai: base URL, key, headers, `openai/` ids, `reasoning_effort`, `max_completion_tokens`, spend lines). `test_the_retired_ingest_is_gone` reads this crate's source for the retired names. | — |
| `bridge/tests/test_mem0_embedder.py` | all 6 | same names (the gemma encode test runs the helper's `on_gemma` in python3; the gemma OpenRouter/meter test runs the helper's `on_openrouter` + `meter` with the fake openai) | — |
| `bridge/tests/test_memory_records.py` | the 4 mem0 tests | same names (`EVE_MEMORY_SOURCES` set through `eve_records::with_seams`) | the rest test records/memory (other crates) |
| `bridge/tests/test_spend_wiring.py` | `test_mem0s_extraction_and_embeddings_are_metered`, `test_a_mem0_without_the_expected_clients_is_left_alone` | `test_mem0s_extraction_and_embeddings_are_metered` (record_spend), `test_meter_sends_each_reply_once_and_a_mem0_without_the_clients_is_left_alone` (the helper's `meter` in python3, its spend lines recorded) | the rest meter other modules |
| `bridge/tests/test_records.py` | `test_keyword_hits_still_return_when_the_meaning_search_times_out` | same name | the rest test eve-records |
| `bridge/tests/test_forget_lean.py` | `test_a_forget_between_the_indexs_read_and_its_add_is_not_undone` | same name (the forget is `forget_matching` on the index; the Memory facade is eve-memory's) | the rest test other layers |
| `bridge/tests/test_memory.py` | `test_forget_apply_is_not_ok_when_mem0_could_not_be_opened` | its mem0 half is `test_a_forget_mem0_cannot_open_for_is_a_failure_never_a_done_zero` | `forget_apply` is eve-memory's; the migration tests move mem0's folder (eve-memory) |
| `bridge/tests/test_model_memory_roles.py` | `test_real_mem0_sends_low_effort_and_budget_without_changing_embeddings`, `test_local_embeddings_keep_the_same_extraction_settings` | both, same names. The first is live (`#[ignore]`): the real mem0ai in the helper against a local OpenRouter stand-in, both parametrized models, with its `temperature`/`top_p` positive control (the helper's test op `unflag_reasoning`); run and passing against mem0ai 2.2.1. Its fake-package twin is `test_extraction_and_embeddings_go_to_openrouter_metered_with_the_extraction_budget` | — |
| `bridge/tests/test_daemon_transcripts.py`, `bridge/tests/test_privacy_guards.py` | (they patch `mem0_memory.shared`/`open_mem0` to test the daemon and the dream) | — | They test eve-daemon and eve-dream; this crate's seams (`shared_with`, `Factory`) are what those ports use |
| (new) | — | `test_a_score_is_read_as_pythons_float_and_odd_rows_are_skipped`, `test_make_private_never_follows_a_symlink`, `test_warm_opens_the_store_and_embeds_once_and_never_raises`, `test_messages_keep_only_said_lines_of_the_owner_and_eve`, `test_line_units_stamp_each_line_with_its_own_wall_time`, `test_a_history_database_that_is_not_one_is_logged_and_the_forget_still_counts`, `test_embedder_of_reads_the_switch`, `test_the_helper_is_this_crates_source`, `test_a_mem0_whose_telemetry_stays_on_is_refused`, `test_no_openrouter_key_is_no_mem0`, `test_the_index_over_the_helper_ingests_searches_and_forgets`, `test_the_owners_words_cross_the_helper_in_utf8_whatever_the_locale` | — |

## Boundaries
- **mem0 (mem0ai, Python only).** mem0's `Memory` — its extraction and update prompts, Qdrant's local mode (pickled
  points in SQLite) and its history database — is a Python library whose on-disk store a Rust reimplementation could
  not share with the Python daemon. So the opened Memory lives in a helper Python process running
  `src/mem0_helper.py` (passed with `-c`, cwd `/`, one argument: the init JSON with `mem0_config(cfg)`, the home,
  `MEM0_DIR`, the embedder, the OpenRouter base URL, headers, key variable name and model ids, and the extraction
  budget). Spawning it is `open_mem0`; its first line is `{"status": "ready"}` or the exception by class and text.
  Each request is one JSON line (`search`, `get_all`, `add`, `delete`, `embed`; `generate` and `probe` for the
  tests) answered by `{"result": …}` or `{"kind", "error"}` (`unflag_reasoning` is a third test-only op); `{"log"}` and `{"spend"}` lines may come first and are
  logged / recorded in the spend ledger by Rust. Library output on stdout goes to stderr so it cannot corrupt the
  protocol. Both pipes are UTF-8 whatever the locale (under `LC_ALL=C` Python would read the pipe as ASCII). Dropping the Memory closes stdin (mem0 closes its store and the helper exits; killed after 5 s).
  Everything around mem0 — which conversations go in, the search's cap, filters and timeout, tidy, forget, the
  history scrub, the meter's ledger lines — is Rust.
- **Which Python.** `EVE_MEM0_PYTHON` (new; the interpreter that has mem0ai), else the bridge checkout's venv
  (`bridge/.venv/bin/python3`, resolved from this crate's location at build time) when it exists, else `python3` on
  PATH. The helper inherits the daemon's environment (`OPENROUTER_API_KEY`, which mem0ai also reads itself).

## Differences
- `MEM0_TELEMETRY` and `MEM0_DIR` are set in the helper's environment, where mem0 runs, not in the daemon's own
  (Python sets them in the daemon process, which is where mem0 runs there).
- `available()` remembers its answer per interpreter for 60 s (each check is a short Python process); Python's
  `find_spec` sees an install or removal at once.
- The helper's model-id rewrite maps the config's two model names through `eve_openrouter::model`; any other name
  is sent unchanged (mem0 only ever sends those two). Python maps every name through `openrouter.model`.
- If the helper process dies, that call fails (`RuntimeError: mem0 helper exited …`) and the next call opens mem0
  again in a new helper; Python's in-process Memory cannot die separately from the daemon.
- `warm` embeds under the index's lock (the helper is one process and answers one call at a time); Python embeds
  after releasing it.
- `ingested_convs` is read as lossy UTF-8; Python raises `UnicodeDecodeError` out of `ingest_day` on a file that is
  not UTF-8 (eve only writes conversation ids, which are ASCII).
- Results cross the helper boundary as JSON (`json.dumps(…, default=str)`): a non-JSON value mem0 returned (a
  datetime) arrives as its `str()`. mem0ai 2.2 already returns ISO strings for its dates.
- `who` holding a list (unhashable) makes Python's `roles.get` raise `TypeError` out of `messages`; here such a line
  is skipped like any other non-speaker. Transcripts only ever hold strings there.
