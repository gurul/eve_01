# eve-chat-log
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/chat_log.py` | `src/lib.rs` (Python builtins in `src/py.rs`, tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `CHAT_LOG_DEFAULT`, `DEFAULT_FILE`, `MAX_LINES`, `MAX_DAYS`, `TEXT_CHARS`, `NOTE_CHARS`, `VERSION`, `SEED_BOOT`, `SEED_CHANNEL` | same-named `pub const`s | Values verbatim. |
| `WHO`, `SEED_KINDS` | `WHO: [&str; 3]`, `SEED_KINDS: [&str; 2]` | Same order. |
| `SEED_WHO` | `SEED_WHO: [(&str, &str); 2]` | owner → user, eve → eve. |
| `_LINK` | private `LINK` | `(?i)https?://\S+\|www\.\S+`, with Python's `\s` (U+001C-U+001F count as space). |
| `_off` | private `off` | `strip().lower()` in `0/false/no/off`. |
| `_number` | private `number` | `float(str(raw).strip())` via `py::float_str` (Python's `float()`: underscores, any Unicode decimal digit, `inf`/`nan`); a missing value is `str(None)`, which float() refuses: the default. |
| `class ChatLogConfig` | `struct ChatLogConfig { enabled, path, max_lines, max_days }` | `Default` is Python's defaults. |
| `configured(environ=None)` | `configured(Option<&HashMap<String, String>>) -> Result<ChatLogConfig, ConfigError>` | None reads the process environment. `Err(ConfigError::Overflow)` where Python's `int(float("inf"))` raises OverflowError (an infinite `EVE_CHAT_LOG_LINES`). The path is spelled as `pathlib` spells it (`py::pure`). |
| `note_text(text, cap=NOTE_CHARS)` | `note_text(&str, cap: usize)` | Links → `(link)`, `" ".join(split())`, `flat[:cap - 1].rstrip() + "…"`; a cap of 0 slices `flat[:-1]` as Python does. |
| `class Line` | `struct Line` | Same fields in the same order; `extra` is a `serde_json::Map` (always written `{}`, never read, as in Python). |
| `class ChatLog` | `struct ChatLog` | `Send + Sync`; one `parking_lot::Mutex` over the state where Python holds `threading.Lock`. |
| `ChatLog.__init__(path, *, max_lines, max_days, wall)` | `ChatLog::new(path)`, `ChatLog::with_options(path, Options { max_lines, max_days, wall })` | `wall: Arc<dyn Fn() -> f64>` (`time.time` by default). `max(1, max_lines)`; the path expanded and spelled as pathlib does. |
| `ChatLog.path`, `.max_lines`, `.max_days`, `.boot` | same `pub` fields | `boot` is 8 hex characters (`secrets.token_hex(4)`). |
| `ChatLog.generation`, `.refused` | `generation()`, `refused()` | Read under the lock. |
| `ChatLog.lines` | `lines(chat) -> Vec<Line>` | Clones; refresh and prune first. |
| `ChatLog.count` | `count(pred)` | Refresh, no prune, as in Python. |
| `ChatLog.append(chat, who, text, *, kind, conv, key, at, quotes, outside)` | `append(chat, who, text)`, `append_with(chat, who, text, Append { kind, conv, key, at, quotes, outside })` | Same replace-by-key rule, `TEXT_CHARS - 1` characters plus `…`, returns the line as kept. |
| `ChatLog.seed(chat, store)` | `seed(chat, Option<&dyn SeedStore>) -> usize` | `trait SeedStore` (enabled, day_of, days, lines) is the seam Python fills by duck typing; `Transcripts` implements it. Same filters, sort, renumbering (quotes move with their line), and the same slice `picked[len - room:]` (see Differences). |
| `ChatLog.forget` | `forget(pred) -> usize` | |
| `ChatLog.clear` | `clear(Option<i64>) -> usize` | |
| `ChatLog._prune` | private `prune` | Age first, then the newest `max_lines` per chat. |
| `ChatLog._file_stamp` | private `file_stamp` | `(st_ino, st_mtime_ns, st_size)`. |
| `ChatLog._refresh` | private `refresh` | Re-reads when the stamp moved; the seq never goes back; generation moves. |
| `ChatLog._load` | private `load` | `transcripts.refusal` of the folder when it exists; same log lines. |
| `ChatLog._read` | private `read` | Strict UTF-8, `json.loads` (NaN/Infinity accepted, via `eve_transcripts::py::loads`), same tolerance of bad rows; `seeded` keeps JSON integers only (not bools, not floats). |
| `ChatLog._save` | private `save` | `transcripts._prepare` for a missing folder (`eve_transcripts::prepare`); the file is `json.dumps(ensure_ascii=False, separators=(",", ":"))` byte for byte (floats in `repr`); temp `.{name}.{pid}.{8 hex}.tmp`, `O_EXCL\|O_NOFOLLOW\|O_CLOEXEC` 0600, fchmod, plain `fsync(2)`, rename; a failed write logged on the first and every 100th. |
| `_parse` | private `parse` | Python's `int()`/`float()`/`str()`/`bool()` of each JSON field (`py::int_value`, `py::float_value`, `eve_tool_gate::py_str`, `py_truthy`); an outside line is marked in `eve_tool_gate::MARKS` on every read. |
| `open_configured(environ=None)` | `open_configured(Option<&HashMap>) -> Result<Option<ChatLog>, ConfigError>` | None with the log off, with Python's log line. |
| (Python builtins) | `py::float_str`, `py::int_str`, `py::int_value`, `py::float_value`, `py::pure`, `py::parent`, `py::write_str`, `py::write_float`, `py::write_value` | `float()`, `int()`, `PurePosixPath`, `Path.parent`, compact `json.dumps`. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_chat_memory.py` (ref) | `test_forget_removes_lines_from_the_log`, `test_the_log_file_is_private_and_bounded`, `test_a_torn_file_starts_empty_and_never_crashes`, `test_a_forget_by_another_process_is_not_undone_by_the_next_write`, `test_the_switch_off_restores_the_old_behaviour`, `test_the_seed_runs_once_and_forget_reaches_the_seeded_copy`, `test_the_seed_moves_a_quote_with_the_line_it_points_at`, `test_live_before_that_with_memory_on_and_a_brand_new_log`, `test_live_before_that_with_memory_on_and_the_log_already_full`, `test_the_daemon_lends_the_log_to_the_door_and_to_forget` | same names in `src/tests.rs` (the chat log's part of each: memory's forget preview/apply is `count`/`forget`; the seed's ordering, voice exclusion and conv dedupe; the daemon's part is `test_the_daemon_opens_the_file_the_environment_names`) | the parts that drive `telegram.Inlet` (history window, gap notes, `recent_messages`, quotes tagging), `memory.Memory.forget_*` and `cli._open_memory_store` belong to the telegram, memory and cli crates; `test_before_that_*`, `test_a_restart_*`, `test_a_hand_off_*`, `test_codex_traffic_*`, `test_with_memory_on_*`, `test_forget_reaches_the_history_*`, `test_the_history_window_*`, `test_the_tool_*`, `test_a_rundown_*`, `test_the_call_warm_up_*`, `test_that_link_*`, `test_an_agents_words_*`, `test_recent_messages_*`, `test_the_cli_forget_*`, `test_and_before_that_twice_*`, `test_last_night_*`, `test_a_time_range_*` are entirely Inlet/Memory/CLI flows |
| `bridge/tests/test_app_media.py` (ref) | the `"media" not in chat_log.SEED_KINDS` assertion | `test_media_is_not_a_kind_the_seed_copies` | the rest is link_chat's and telegram's |
| `bridge/tests/test_link_chat.py`, `bridge/tests/test_telegram_lite.py` (ref) | construct `ChatLog(path, wall=…)` for their rigs | covered by the constructor in every test above | the flows are link_chat's and telegram's |
| (none: `tests` is empty in plan.json) | | `test_configured_reads_the_bounds_as_python_does`, `test_the_seed_keeps_to_its_bounds`, `test_the_seed_takes_the_newest_that_fit_with_pythons_slice`, `test_the_seed_never_fails_on_a_clock_it_cannot_hold`, `test_a_note_with_the_same_key_replaces_the_newest_line`, `test_append_refuses_an_unknown_speaker_and_empty_text_and_cuts_long_text`, `test_clear_empties_one_chat_or_all`, `test_outside_words_are_marked_again_when_the_file_is_read`, `test_a_refused_folder_keeps_the_log_in_ram_only`, `test_the_file_is_byte_for_byte_what_python_writes`, `test_rows_are_read_as_python_reads_them`, `test_a_time_python_wrote_is_written_back_with_the_same_digits`, `test_python_number_parsing`, `test_note_text_takes_links_out_and_caps_the_line` | |

The expected values of `test_the_file_is_byte_for_byte_what_python_writes` and `test_rows_are_read_as_python_reads_them`
were produced by running chat_log.py. A one-off differential run (not kept in the tree) replayed 300 random sequences
of append (keys, outside, `at`, quotes, long and whitespace-only text, unknown speakers), forget, clear and lines,
with random bounds and clock jumps past `max_days`, through chat_log.py and this crate: every return value, the final
file byte for byte, and the generation were equal. A second run seeded 120 random logs from random transcripts written
by transcripts.py (channels, kinds, forgotten lines, outside lines, old days, long text, kept lines in the same
conversations): every count and the file after the seed were equal. That run found serde_json's default float reader
off by one unit in the last place for some times; the crate turns on serde_json's `float_roundtrip`, as eve-places and
eve-browser-intent do.

## Boundaries
None

## Differences
- Python's seed slices `picked[len(picked) - room:]`. When `room` is more than the rows picked but less than twice
  as many, the start is negative and lands inside the list, so only the last `2 * len - room` rows are copied instead
  of all of them (for example 3 rows and room for 4 copies 1). The port keeps this slice exactly (`py_tail`), so both
  daemons seed the same lines; it is recorded here as a defect of the Python, not changed.
- Rows Python cannot hold the same way: a `seq`, `chat` or `quotes` past i64 (Python's ints are unbounded) and a
  `seq`/`chat`/`quotes` of `Infinity` are dropped as unreadable rows. Python raises OverflowError on `int(inf)` out of
  `_read` (it is not one of the caught exceptions), which would stop the daemon's start; the port drops the row.
- A bare `NaN`/`Infinity` time in the file is read as `null` by the shared JSON reader and the row is dropped; Python
  keeps an `Infinity` time (a line that never ages out). Neither daemon writes one unless a caller passes such an
  `at`.
- A text holding a lone surrogate (`\ud800` in the file) makes the whole file unreadable JSON here (Rust strings
  cannot hold one), so the log starts empty; Python reads it, and then fails to encode it on the next save.
- `configured` returns the OverflowError as `Err(ConfigError::Overflow)` instead of raising.
