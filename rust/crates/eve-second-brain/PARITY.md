# eve-second-brain
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/second_brain.py` | `src/lib.rs` (logic), `src/texts.rs` (prompt text, skeleton, SOPs, packs, tool schemas, generated verbatim from the Python module), `src/py.rs` (Python string, path, glob, fnmatch and OSError semantics the module relies on) |

## API
| Python | Rust | Notes |
|---|---|---|
| `SECOND_BRAIN_DEFAULT` | `SECOND_BRAIN_DEFAULT` | `false` |
| `DEFAULT_VAULT` | `DEFAULT_VAULT` (`LazyLock<PathBuf>`) | `$HOME/Documents/Second Brain` |
| `VAULT_GUIDE`, `OBSIDIAN_DIR`, `TODOS_FILE`, `JOURNAL_DIR`, `INBOX_DIR`, `PROJECTS_DIR`, `AREAS_DIR`, `PROCESSES_DIR`, `PACKS_DIR`, `ARCHIVE_DIR`, `HISTORY_DIR`, `TRASH_DIR` | same names | same strings |
| `PRIORITIES`, `DEFAULT_PRIORITY`, `KINDS`, `WORKFLOWS`, `SELECTS` | same names (arrays) | same order |
| `DAILY_PLAN_CALENDAR`, `WORKFLOW_NOTES`, `INSTRUCTIONS_BLOCK` | same names | verbatim |
| `MAX_*`, `CHARS_PER_TOKEN`, `DEFAULT_MAX_PER_GLOB` | same names | same values; lengths are counted in code points as Python does |
| `_NOTE_LOCK` | `NOTE_LOCK` (private `parking_lot::ReentrantMutex`) | re-entrant and process-wide, as the RLock |
| `PARA`, `PARA_FOLDERS` | same names | same rows |
| `_WORD`, `_PRIORITY`, `_FRONTMATTER`, `_HEADING`, `_OPEN_TODO`, `_SAFE_SEGMENT` | private `LazyLock<Regex>` | `$` without MULTILINE kept as `\n?\z` where it can matter (`_SAFE_SEGMENT`) |
| `CaptureResult` | `CaptureResult` | `Serialize`, fields path, kind, title |
| `Hit` | `Hit` | `Serialize`, fields path, title, snippet, score |
| `InboxItem` | `InboxItem` | `Serialize` |
| `Include` | `Include` (`Include::new`, `Include::with`) | default select `filename_desc`, count `None` |
| `PackDef` | `PackDef` (`PackDef::new`) | dataclass defaults for include/exclude/prefer_files/max_per_glob |
| `Compiled` | `Compiled` | xml, files, tokens, dropped |
| `VaultConfig` | `VaultConfig` | enabled, root |
| `_guide_text` | `guide_text` (private) | `texts::GUIDE_TEMPLATE` with the PARA rows |
| `_todos_skeleton` | `todos_skeleton` (private) | verbatim |
| `_vision_template` | `vision_template` (private) | verbatim |
| `_SOPS`, `_PACKS` | `sop`, `pack_text` (private) over `texts::SOP_*`, `texts::PACK_*` | verbatim |
| `_write_new` | `write_new` (private) | never overwrites; records the posix relative path |
| `ensure_obsidian` | `ensure_obsidian` | `Result<Vec<String>, BrainError>` |
| `ensure_vault` | `ensure_vault` | `Result<Vec<String>, BrainError>` |
| `_resolve` | `resolve` (private) | Python's non-strict `realpath` port (`py::resolve`); same three refusals |
| `_rel` | `rel` (private) | resolved relative posix path, ValueError text as 3.13's `relative_to` |
| `split_frontmatter` | `split_frontmatter` | `(IndexMap<String, String>, String)` |
| `_set_frontmatter` | `set_frontmatter` (private) | |
| `title_of` | `title_of` | |
| `slugify` | `slugify` | NFKD via `unicode-normalization`, ASCII only, six words, 48 chars |
| `_unclashed` | `unclashed` (private) | `-2` … `-999`, then FileExistsError(name) |
| `classify_capture` | `classify_capture` | `(&'static str, String)`; slices by code point |
| `_priority_of` | `priority_of` (private) | |
| `_append_under` | `append_under` (private) | |
| `capture` | `capture(root, text, kind, source, now)` | Python defaults are `kind="note"`, `source="telegram"`, `now=None` |
| `_notes` | `notes` (private) | `rglob("*.md")`, sorted part by part, hidden and archive filters |
| `search` | `search(root, query, limit, include_archive)` | defaults `MAX_HITS`, `false` |
| `read_note` | `read_note(root, rel_path, max_chars)` | `Result<Value, BrainError>`; default `MAX_NOTE_CHARS` |
| `_revision` | `revision` (private) | sha256 hex of the UTF-8 text |
| `_history_dir` | `history_dir` (private) | refuses symlinked history folders |
| `_latest_edit` | `latest_edit` (private) | |
| `_atomic_text` | `atomic_text` / `atomic_text_real` (private) | `.eve-` temp file in the same folder, fsync, keep mode (0600 for new), `os.replace` |
| `edit_note` | `edit_note` | |
| `_write_with_undo` | `write_with_undo` (private) | record JSON written byte-for-byte as `json.dumps(..., ensure_ascii=False)`; `.failed` on a failed write |
| `undo_note` | `undo_note` | `.undone` after restore |
| `_preview` | `preview` (private) | |
| `list_notes` | `list_notes` | `Vec<NoteRow>` (path, folder, title, preview, modified) — the dict's keys in order |
| `note_folders` | `note_folders` | |
| `_markdown_note` | `markdown_note` (private) | |
| `write_note` | `write_note` | |
| `create_note` | `create_note(root, folder, title, body)` | default body "" |
| `_trash_dir` | `trash_dir` (private) | |
| `trash_note` | `trash_note` | `record.json` is `{"path": "…"}` as Python writes it |
| `restore_note` | `restore_note` | `.restored` after restore |
| `move_note` | `move_note` | |
| `inbox` | `inbox` | |
| `_status_of` | `status_of` (private) | |
| `active_projects` | `active_projects` | |
| `todos` | `todos` | `IndexMap<String, Vec<String>>` keyed P0..P3 |
| `_destination` | `destination` (private) | resolved, symlinked folders refused |
| `_move` | `move_with_status` (private) | |
| `file_note` | `file_note` | |
| `_scalar` | `scalar` (private) | returns the private `Yaml` value (Python's `Any`) |
| `_parse_block` | `parse_block` (private) | |
| `parse_pack` | `parse_pack` | Python's truthiness, `str()`, `int()` and iteration reproduced on `Yaml` (a scalar `include: abc` iterates characters, a non-iterable is a TypeError) |
| `load_pack` | `load_pack` | |
| `_excluded` | `excluded` (private) | `fnmatch.fnmatch` port (`py::fnmatch`) |
| `_matches` | `matches` (private) | Python 3.13 `Path.glob` port (`py::glob`); negative counts slice from the end as Python's `[:n]` |
| `compile_pack` | `compile_pack` | |
| `workflow_prompt` | `workflow_prompt(root, name, extra, now)` | |
| `SECOND_BRAIN_TOOLS` | `SECOND_BRAIN_TOOLS` (`LazyLock<Vec<Value>>`) | parsed from the verbatim JSON in `texts.rs` |
| `SECOND_BRAIN_TOOL_NAMES` | `SECOND_BRAIN_TOOL_NAMES` | schema order; a test asserts it matches the schemas |
| `_clip` | `clip_text` (private) | |
| `dispatch` | `dispatch(root, name, &Value) -> Value` | holds the lock; never fails |
| `_dispatch` | `dispatch_inner` (private) | `str(args.get(k) or "")` reproduced for non-string JSON args, floats printed as Python's `repr` (`py::float_repr`) |
| `configured` | `configured(Option<&HashMap<String, String>>)` | `None` reads the process environment; `EVE_VAULT` gets `expanduser` |
| (exceptions) | `BrainError::{Value, Os, Other}` | ValueError / OSError subclass / anything else; `type_name()` gives Python's class name |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_second_brain.py` | all 43: `test_ensure_vault_creates_skeleton_and_is_idempotent`, `test_ensure_vault_inside_existing_obsidian_vault_adds_no_second_obsidian`, `test_default_is_off_and_configured_reads_env`, `test_classify_capture_rules`, `test_slugify`, `test_capture_note_writes_inbox_file_with_frontmatter`, `test_capture_idea_is_an_inbox_note_of_kind_idea`, `test_capture_todo_priorities`, `test_capture_journal_appends_to_todays_page`, `test_capture_rejects_empty_and_unknown_kind`, `test_search_scores_filename_over_body_and_skips_archive`, `test_search_snippet_is_clipped`, `test_read_note_refuses_escapes_and_obsidian`, `test_inbox_oldest_first`, `test_active_projects_excludes_done_and_archived`, `test_file_note_moves_updates_status_and_handles_clashes`, `test_a_symlinked_project_folder_is_not_a_way_out_of_the_vault`, `test_file_note_rejects_unknown_folder_and_escapes`, `test_parse_pack_on_the_decks_example`, `test_parse_pack_rejects_nonsense`, `test_builtin_packs_parse_and_load`, `test_compile_pack_orders_selects_and_excludes`, `test_compile_pack_respects_budget_and_reports_drops`, `test_compile_pack_modified_desc_and_count_and_max_per_glob`, `test_workflow_prompt_for_every_name_has_sop_and_context`, `test_every_tool_is_strict_named_and_dispatched`, `test_dispatch_never_raises_and_returns_ok_false_for_bad_input`, `test_dispatch_round_trip`, `test_dispatch_clips_long_notes`, `test_instructions_block_mentions_the_tools_and_workflows`, `test_edit_shopping_list_keeps_one_file_and_exact_frontmatter`, `test_todo_completion_reopen_and_priority_change`, `test_edit_refuses_stale_ambiguous_missing_or_frontmatter_matches`, `test_undo_restores_bytes_and_walks_history_without_toggling`, `test_undo_refuses_later_obsidian_changes_and_wrong_note`, `test_edit_paths_history_and_hidden_search`, `test_history_symlink_and_failed_backup_leave_note_unchanged`, `test_clipped_reads_support_targeted_edits_without_truncation`, `test_two_simultaneous_edits_cannot_overwrite_each_other`, `test_todo_capture_and_edit_cannot_silently_lose_a_change`, `test_failed_note_write_keeps_original_and_does_not_offer_failed_undo`, `test_edit_rechecks_after_backup_when_an_external_editor_changes_note`, `test_the_daily_plan_is_written_in_one_pass_with_the_calendar` | same names in `src/tests.rs` (parametrized cases looped). The monkeypatches of `_atomic_text`, `os.replace` and `_append_under` are thread-local hooks in `seams` (unit tests only). Added: `test_configured_expands_the_home_folder`, `test_parse_pack_follows_pythons_types`, `test_fnmatch_and_glob_follow_python`, `test_history_record_is_pythons_json`, `test_a_crlf_note_reads_as_python_reads_it`, `test_str_of_a_json_float_is_pythons` | None |
| `bridge/tests/test_link_notes.py` (tests_ref) | the second_brain half: `test_list_is_every_note_newest_first_with_folders`, `test_a_captures_preview_is_not_its_title_again`, `test_read_splits_the_frontmatter_off_and_save_keeps_it`, `test_a_stale_revision_is_refused_and_nothing_is_written`, `test_create_names_the_page_from_its_title_and_never_overwrites`, `test_delete_goes_to_the_hidden_trash_and_restore_puts_it_back`, `test_move_keeps_the_note_as_it_is`, `test_paths_that_leave_the_vault_or_are_hidden_are_refused`, `test_an_emptied_body_leaves_the_frontmatter`, `test_a_save_writes_unix_newlines` | same names, calling second_brain directly | The NotesPanel wording, `test_off_or_missing_vault_is_unavailable_and_says_so`, `test_a_note_too_long_for_one_request_is_read_only` and `test_search_finds_words_and_says_so_when_none_match` belong to link_notes' crate |
| `bridge/tests/test_link_chat.py`, `test_rundown.py`, `test_telegram.py`, `test_tool_routing.py` (tests_ref) | only construct `VaultConfig`, read `SECOND_BRAIN_TOOL_NAMES` / `INSTRUCTIONS_BLOCK`, or patch `dispatch` | covered by the tool and config tests above | The rest tests those crates |

Cross-check (not a cargo test): the same scenario (every capture kind, classify/slugify/title/frontmatter on odd text, search, inbox, list_notes, folders, every workflow prompt, an odd pack with `**`, `[!a]` and a negative count, six malformed packs, twelve dispatch calls including error paths, edit, undo, file, create) was run through Python 3.13 and this crate on fresh vaults: the JSON outputs were identical line for line, the vault trees hashed identically, and the history records matched byte for byte.

## Boundaries
None

## Differences
- A panic inside a tool is caught and reported as `"<tool> failed: panic"` where Python would name the exception class.
- `json.loads` errors on a malformed history or trash record carry serde_json's wording, not Python's JSONDecodeError text (both are refusals with `ok: false`).
- Pack integers take every Unicode decimal digit as Python's `\d` and `int()` do, and an integer beyond i64 keeps its exact decimal text for `str()`; `int()` of one saturates to i64::MIN/MAX in the `PackDef` fields (Python's ints are unbounded). Every use of those fields only compares or slices with them, so the compiled pack is the same.
- A strict read of invalid UTF-8 is reported with Python's exact UnicodeDecodeError text, but as `BrainError::Value`, so the dispatch log line names it `ValueError` rather than `UnicodeDecodeError` (the reason the owner sees is identical).
- The regex `\s` class is Unicode White_Space, which leaves out U+001C..U+001F that Python's `\s` includes; `strip`/`split`/`splitlines` do follow Python exactly.
- `repr()` in error messages escapes the common non-printable characters; Python's full Unicode printability table is not reproduced for rare format characters.

## Upstream edits (by dependents)
- For `eve-link-notes` (link_notes.py's `_read` calls `_resolve`, `_rel`, `_revision`, `_latest_edit`): those four, the `py` module (`#[doc(hidden)]`) and the `str(x or "")` helpers `value_str` and `truthy` are now `pub`. Visibility only; no behavior changed, tests re-run green.
