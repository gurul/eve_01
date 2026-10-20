# eve-notes
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/notes.py` | `src/notes.rs` (re-exported from `src/lib.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| constants (`SAMPLE_RATE`, `DEFAULT_MODEL`, `DEFAULT_SUMMARY_MODEL`, `DIARIZE_MODEL`, `DEFAULT_SEGMENT_SECS`, `DEFAULT_MAX_MINUTES`, `OVERLAP_SECS`, `TRANSCRIBE_TIMEOUT_SECS`, `SUMMARY_TIMEOUT_SECS`, `SUMMARY_MAX_OUTPUT_TOKENS`, `SILENCE_RMS`, `MAX_SAME_NAME`, `LONG_RECORDING_SECS`, `WINDOW_SECS`, `WINDOW_OVERLAP_SECS`, `DIARIZE_*`, `AAC_BITRATE`, `REF_MIN_SECS`, `REF_MAX_SECS`, `MAX_KNOWN_SPEAKERS`, `MAX_OWNER_VOICE_BYTES`, `DEFAULT_OWNER_NAME`, `MAX_TURN_CHARS`, `DIFFERS_RATIO`, `MAX_OVERLAP_WORDS`, `LIST_KEYS`) | same names, `pub const` | values verbatim |
| `_STOP` | `STOP` (private `LazyLock<Regex>`) | same pattern, `(?i)` for `re.IGNORECASE` |
| `SUMMARY_PROMPT`, `COMBINE_PROMPT` | `SUMMARY_PROMPT`, `COMBINE_PROMPT` | byte-identical; a test reads `notes.py` and compares |
| `_LIST`, `_NULLABLE` | `list_schema()`, `nullable_schema()` (private) | |
| `_obj` | `obj` | same key order: type, additionalProperties, properties, required |
| `SUMMARY_SCHEMA` | `SUMMARY_SCHEMA` (`LazyLock<Value>`) | same keys in the same order |
| `_key` | `key` | |
| `stitch` | `stitch` | |
| `wav_seconds` | `wav_seconds` | reads the RIFF header as Python's `wave` does; 0.0 where it raises |
| `AudioSource` | `AudioSource` trait (+ `MicQueue` trait for the queue it hands out, `same_queue`) | `listening()` defaults to true, as `getattr(ears, "listening", True)` |
| `PushedAudio` | `PushedAudio` (+ `BlockQueue`, the unbounded `asyncio.Queue`) | `put`, `subscribe`, `unsubscribe`, `listening` |
| `TranscribeClient` | `TranscribeClient` trait | optional `combine`/`diarize` are `has_combine()`/`has_diarize()` plus default methods that fail with `AttributeError`, as calling a missing method does |
| `OpenAINotesClient` | `OpenAINotesClient` | `new(model, summary_model, api_key)`; `with_sdk` replaces `_client` (the `NotesSdk` seam; `OpenRouterSdk` is the real one); `ledger` is the spend meter; same request bodies |
| `OpenAINotesClient.transcribe` | `TranscribeClient::transcribe` | multipart `model` (+ `prompt` when non-empty) and `file=segment.wav`; metered as `transcription` by tokens or the clip's seconds |
| `OpenAINotesClient._respond` / `summarize` / `combine` | `respond` (private) / `summarize` / `combine` | Responses API, `json_schema` strict `notes`, `max_output_tokens` 8000, `reasoning.effort` low; metered as `room notes`; empty reply → `RuntimeError("empty summary")` |
| `make_notes_client` | `make_notes_client(Option<&HashMap>)` | `None` reads the process env; same warning text |
| `NotesConfig` | `NotesConfig` (`Default` = the dataclass defaults) | |
| `_off` | `off` | |
| `configured` | `configured(Option<&HashMap>)` | `float()` parsing (whitespace, `_`, inf/nan, Unicode `Nd` digits), clamping with Python's NaN handling, same warning |
| `to_wav` | `to_wav(pcm, rate)` | the bytes Python's `wave` writes (odd lengths too) |
| `rms` | `rms` | same sampling step |
| `wants_stop` | `wants_stop` | |
| `stamp` | `stamp` | |
| `Line` | `Line` (`render()`) | |
| `render_lines` | `render_lines` | |
| `Diarized` | `Diarized` | |
| `SpeakerNames` | `SpeakerNames` (`new`, `name`) | |
| `owner_reference` | `owner_reference` | same mime table, warnings name the OSError class |
| `_data_url` | `data_url` | |
| `plan_pieces` | `plan_pieces` | |
| `_wav_piece_secs` | `wav_piece_secs` | |
| `encode_pieces` | `encode_pieces(pcm, offset, rate, max_bytes, which, run)` → `Vec<Piece>` | `which`/`run` seams as in Python (`default_which` = `shutil.which`, `default_run` = `subprocess.run(check=True, capture_output=True, timeout=300)`, `RunError` names `CalledProcessError`/`TimeoutExpired`/OSError) |
| `_segments` | `segments` → `Vec<Segment>` | `float(x or 0)` semantics; a bad number skips the segment |
| `pick_reference` | `pick_reference` | |
| `diarize_recording` | `diarize_recording(read, total, client, owner, &DiarizeOpts)` | `read` returns `io::Result` (a failed read raises, as in Python) |
| `differs` | `differs` | |
| `_clean_text` | `clean_text` | |
| `_nullable` | `nullable` | |
| `_items` | `items` (pub: meet.py uses it) | bools count, as `isinstance(True, int)` |
| `_at` | `at` | Unicode digits as Python's `\d` and `int()` |
| `_words` | `words` | |
| `check_quotes` | `check_quotes` | |
| `clean_summary` | `clean_summary` → `Map` | same key order |
| `_parse` | `parse` | `ValueError` / `JSONDecodeError` as `NotesError` |
| `windows` | `windows` | |
| `_dedupe` | `dedupe` | key = `json.dumps(sort_keys=True)` (ASCII-escaped) case-folded |
| `merge_partials` | `merge_partials` | |
| `write_up` | `write_up` / `write_up_with(.., WriteUpOpts)` | combine input is `json.dumps(parts, ensure_ascii=False)` byte for byte |
| `NotesState` | `NotesState` | |
| `RoomNotes` | `RoomNotes` (a cheap `Clone` handle) + `RoomNotesOptions` | `start` (needs a tokio runtime), `request_stop`, `stop`, `task`, `minutes`, `status`, `close`, `active`; fields read as `state()`, `summary()`, `transcript()`, `speakers()`, `diarized()`, `lines()`, `quick()`/`set_quick()`, `spool_dir()`. Private: `_executor` → `Inner::executor` (2-permit semaphore + `spawn_blocking`), `_can_diarize`, `_open_spool`, `_drop_spool`, `_run`, `_transcribe_worker` (in `run`), `_segment`, `_live`, `_diarize_sync`, `_finish`, `_rewrite` |
| `_slug` | `slug` | |
| `notes_dir` | `notes_dir` | `transcripts/meetings` |
| `_action_text` | `action_text` | |
| `_quote_text` | `quote_text` | |
| `note_sections` | `note_sections` → `Vec<(&str, Vec<Row>)>` | `Row::Topic` is Python's `(heading, points)` tuple |
| `render` | `render(summary, transcript, state, &RenderOpts)` | keyword arguments in `RenderOpts`; byte-identical (test against Python's output) |
| `_replace` | `replace` | atomic, 0600, `O_EXCL|O_NOFOLLOW|O_CLOEXEC`, fsync, temp removed on failure |
| `_rename_to_title` | `rename_to_title` | hard link, never over another file |
| `write_notes` | `write_notes(cfg, summary, transcript, state, &RenderOpts)` | 0700 folders, reserved name then atomic write |
| `recent` | `recent(cfg, limit)` → `io::Result<Vec<PathBuf>>` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `tests/test_notes.py` | all 22 | `src/tests.rs`: same names | none |
| `tests/test_notes_detailed.py` | `test_schema_and_render_round_trip`, `test_a_conversation_and_a_memo_get_their_own_sections`, `test_the_prompt_states_its_values_as_sets_and_carries_no_banned_title`, `test_a_quote_that_is_not_in_its_line_is_dropped`, `test_a_long_recording_is_written_up_in_overlapping_windows_then_merged`, `test_a_failed_merge_is_done_in_code_and_a_short_recording_is_one_call`, `test_the_transcript_is_on_disk_before_the_summary_and_survives_its_crash`, `test_labels_stay_one_voice_across_pieces_through_known_speakers`, `test_the_owner_is_reference_one_on_every_piece`, `test_a_bad_reference_is_dropped_and_the_piece_tried_again`, `test_speaker_names_follow_first_appearance`, `test_without_afconvert_or_when_it_fails_the_pieces_go_as_wav_under_the_limit`, `test_afconvert_makes_a_small_aac_piece`, `test_a_session_ends_with_the_speaker_pass_and_its_count`, `test_the_pass_can_be_turned_off_and_the_owner_named`, `test_a_failed_pass_keeps_the_live_transcript` | `src/tests_detailed.rs`: same names, plus `render_writes_the_bytes_python_writes`, `the_prompts_are_byte_identical_to_the_python`, `the_combine_input_is_pythons_json_dumps`, `the_owner_clip_is_read_as_a_data_url_by_its_suffix` | `test_the_texted_notes_carry_every_section_in_messages_under_the_limit`, `test_the_texted_notes_cap_out_at_the_file_and_carry_the_words_when_there_is_no_file` test `phone_call.notes_messages` (the phone-call crate); `test_live_one_real_diarize_call_on_two_synthetic_voices` is `live` and skips itself in Python too, since `OpenAINotesClient` has no `diarize` |
| (differential, no Python test) | `notes.py`'s own outputs for edge inputs, recorded in `tests/fixtures/python_reference.json` | `src/tests_python_reference.rs`: `the_pure_functions_match_pythons_outputs` (`clean_summary`, `render`, `note_sections`, `merge_partials`, `_segments`, `_at`, `_items`, `stitch`, `wants_stop`, `_slug`, `stamp`, `configured`) | |
| `tests/test_model_memory_roles.py` (ref) | `test_room_notes_summary_and_reduce_use_summary_model` | `src/tests_ref.rs`: same name, both parametrizations, through the real OpenRouter client against a local server | the other tests are other modules' |
| `tests/test_spend_wiring.py` (ref) | `test_room_notes_transcription_by_tokens_or_by_the_clip_and_the_summary` | `src/tests_ref.rs`: same name | the other tests are other modules' |
| `tests/test_phone_notes.py` (ref) | `test_pushed_audio_feeds_only_a_listening_session` | `src/tests.rs`: `the_pushed_audio_feeds_only_a_listening_session` | the rest drive `phone_call`/`miniapp` (their crates) |

## Boundaries
- `afconvert` (macOS) is run as a subprocess for the AAC pieces of the speaker pass, exactly as the Python does; without it the pieces go as WAV.

## Differences
- `RoomNotes.start()` must be called inside a tokio runtime (Python needs a running asyncio loop for the same reason).
- The worker pool is a 2-permit semaphore over `spawn_blocking` rather than a `ThreadPoolExecutor(max_workers=2)`; a call's timeout still counts its queueing, and `close()` fails queued calls (`CancelledError`) while running ones finish, as `shutdown(wait=False, cancel_futures=True)` does.
- `recent` returns `io::Result`: an unreadable folder is an error where Python's `iterdir` raises.
- When the spool file cannot be created, its temporary folder is removed; the Python leaves the empty `eve-notes-*` folder behind (it only records the folder once both steps succeed).
- Numbers in a model reply are parsed by the workspace's `serde_json`, which is built without `float_roundtrip`: a float with 17 significant digits can come back one unit in the last place off Python's `json.loads` (for example `2.0**70` in a list `_items` reads). Turning the feature on belongs in the root `Cargo.toml`.
- Log lines name a test fake's plain `anyhow` error as `Exception` where Python would print the fake's own exception class.
