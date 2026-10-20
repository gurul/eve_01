# eve-link-notes
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/link_notes.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `ACTIONS` | `ACTIONS` | same nine, same order |
| `MAX_BODY_CHARS`, `MAX_READ_CHARS`, `MAX_QUERY_CHARS`, `MAX_PATH_CHARS`, `MAX_ID_CHARS` | same names | same values; lengths and slices are in code points as Python counts them |
| `OFF_LINE`, `CHANGED_LINE`, `TOO_LONG_LINE`, `NOT_A_NOTE` | same names | verbatim |
| `NotesPanel` | `NotesPanel` (`NotesPanel::new(ConfigFn)`, `Default` = `second_brain::configured(None)`) | `config` is `Arc<dyn Fn() -> VaultConfig>`, read per call |
| `NotesPanel._root` | `NotesPanel::root` (private) | a panicking config is the "raises" case: unavailable |
| `NotesPanel.available` | `NotesPanel::available()` | enabled and the root is a folder |
| `NotesPanel.panel` | `NotesPanel::panel(&Map) -> Result<Value, BrainError>` | `asyncio.to_thread` → `spawn_blocking`; ValueError (incl. UnicodeDecodeError) → owner line, OSError → warning log + "That didn't work on the Mac just now.", `line` defaulted last; any other error is `Err`, the exception Python lets escape to link_chat's 500 |
| `NotesPanel._do` | `do_action` (private) | same branch order; `str(x or "")` via `eve_second_brain::{truthy, value_str}` |
| `_read` | `read` (private) | key order ok, path, title, body, front, revision, modified, editable, undo_id |
| `_short` | `short` (private) | `str(value or "").strip()[:limit]` |
| `_owner_line` | `owner_line` (pub) | same prefixes, same lines |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_link_notes.py` | all 13: `test_off_or_missing_vault_is_unavailable_and_says_so`, `test_list_is_every_note_newest_first_with_folders`, `test_a_captures_preview_is_not_its_title_again`, `test_read_splits_the_frontmatter_off_and_save_keeps_it`, `test_a_stale_revision_is_refused_and_nothing_is_written`, `test_create_names_the_page_from_its_title_and_never_overwrites`, `test_delete_goes_to_the_hidden_trash_and_restore_puts_it_back`, `test_move_keeps_the_note_as_it_is`, `test_paths_that_leave_the_vault_or_are_hidden_are_refused`, `test_a_note_too_long_for_one_request_is_read_only`, `test_search_finds_words_and_says_so_when_none_match`, `test_an_emptied_body_leaves_the_frontmatter`, `test_a_save_writes_unix_newlines` (parametrized cases looped) | same names in `src/tests.rs`. Added: `test_available_follows_the_switch_and_the_folder`, `test_a_config_that_fails_is_off`, `test_a_falsy_action_lists_and_odd_ones_are_refused`, `test_save_without_a_string_body_and_without_a_path`, `test_read_reports_its_fields_and_the_undo_id_after_a_save`, `test_a_stale_undo_id_cannot_be_undone`, `test_an_unreadable_vault_is_a_mac_failure`, `test_owner_line_says_each_reason_to_the_owner` | None |
| `bridge/tests/test_link_chat.py` (tests_ref) | the Notes part of the controls test (listed only while the brain is on and its folder exists) | `test_available_follows_the_switch_and_the_folder` | The HTTP routing (404 while off, POST to the panel) belongs to link_chat's crate |

Cross-check (not a cargo test): the added edge cases (read key order, falsy and non-string actions, a file where a folder should be, a bad undo id, a non-string save body, a blank path, a move into the same folder) were run through the Python panel; the outputs match the Rust assertions.

## Boundaries
None

## Differences
- A panic inside the blocking worker comes back as `Err(BrainError::Other { kind: "RuntimeError", .. })`, the same class of outcome as an unexpected Python exception (link_chat answers 500).

## Upstream edits
- `eve-second-brain`: `py` module, `resolve` (`_resolve`), `rel` (`_rel`), `revision` (`_revision`), `latest_edit` (`_latest_edit`), `value_str` and `truthy` made `pub` so `_read` and `str(x or "")` are the module's own. Recorded in that crate's PARITY.md.
