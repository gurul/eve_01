# eve-notes-widget
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/notes_widget.py` (pure half) | `src/lib.rs` |
| `bridge/src/eve_bridge/notes_widget.py` (`run`, the AppKit panel and the watcher thread) | `src/panel.rs` (macOS only) |
| (tests) | `src/tests.rs` |
| (smoke run, the `--once` path against throwaway folders) | `examples/once.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `NOTES_DIR_ENV`, `MEMORY_DIR_ENV` | same-named `pub const` | `EVE_NOTES_DIR`, `EVE_MEMORY_DIR`. |
| `CONFIG_DIR`, `DEFAULT_NOTES_DIR`, `DEFAULT_MEMORY_DIR`, `POSITION_PATH` | `config_dir()`, `default_notes_dir()`, `default_memory_dir()`, `position_path()` | `~/.config/eve-bridge/{notes,memory,widget.json}`; home is `$HOME`, else the account's (`eve_transcripts::py::home`). |
| `TITLE`, `EMPTY_TEXT`, `SAID`, `SEEN`, `KIND_LABELS`, `STARS_LABEL`, `MAX_STARS`, `NO_TIME`, `FORGOTTEN`, `MAX_NOTES`, `WIDGET_SIZE`, `SCREEN_MARGIN`, `POLL_SECS`, `FONT_SIZE` | same-named `pub const` | Values verbatim. `KIND_LABELS` is an array of `(kind, label)`. |
| `LINE_RE`, `FILE_RE` | `pub static LINE_RE`, `FILE_RE` (`LazyLock<Regex>`) | Python's `\s` is `[\s\x1c-\x1f]`; `$` is `\n?\z`. `\d` is Unicode in both. |
| `class Note` (frozen dataclass: `day`, `time`, `text`, `kind=SEEN`) | `struct Note` (`Note::new` = default kind SEEN, `Note::with_kind`) | `kind` stays a string, as in Python. |
| `notes_dir()` | `notes_dir()`, plus `notes_dir_in(&HashMap)` for tests | Unset or empty is the default; `Path(raw).expanduser()`. |
| `memory_dir()` | `memory_dir()`, plus `memory_dir_in(&HashMap)` | Same rules. |
| `_recall_cfg(store)` | private `recall_cfg` | `RecallConfig { store, notes: notes_dir() }`. |
| `stars(store, limit=MAX_STARS)` | `stars(&Path, usize)` | Calls `eve_records::stars`; a panic is an empty list (the Python catches every exception). |
| `parse_conversation(text)` | `parse_conversation(&str)` | Python `splitlines`/`strip` semantics via `eve_transcripts::py`. |
| `journal_dir(store)` | `journal_dir(&Path)` | `<store>/records/days`. |
| `collect_conversations(store, today=None, limit=MAX_NOTES)` | `collect_conversations(&Path, Option<NaiveDate>, usize)` | Reads with `errors="replace"` and universal newlines. Same early return at `limit`. |
| `note_files(directory, today=None)` | `note_files(&Path, Option<NaiveDate>)` | `is_file()` follows symlinks, as in Python. |
| `day_of(path)` | `day_of(&Path)` | Unicode digits read as Python's `int`; year 0 is None (Python's `date` starts at 1). |
| `parse_notes(text, day)` | `parse_notes(&str, NaiveDate)` | |
| `collect_notes(directory, today=None, limit=MAX_NOTES)` | `collect_notes(&Path, Option<NaiveDate>, usize)` | Same warning text on a read error (`notes-widget: cannot read …`). |
| `day_label(day, today=None)` | `day_label(NaiveDate, Option<NaiveDate>)` | `Today · …`, `Yesterday · …`, else ISO. |
| `render_sections(notes, today=None)` | `render_sections(&[Note], Option<NaiveDate>) -> Vec<(String, Vec<Note>)>` | |
| `render_text(notes, today=None, starred=())` | `render_text(&[Note], Option<NaiveDate>, &[String])` | Stars newest first with `★  `. |
| `collect_everything(notes_directory, store, today=None, limit=MAX_NOTES)` | `collect_everything(&Path, &Path, Option<NaiveDate>, usize)` | Stable sort on `(day, time)` descending, as `sorted(..., reverse=True)`. |
| `load_position(path=POSITION_PATH)` | `load_position(&Path)` | Strict UTF-8, `json.loads`, then `float()` of `x`/`y`: numbers, bools, and strings `float()` accepts (whitespace, `_` between digits, Unicode digits, inf/nan). |
| `save_position(x, y, path=POSITION_PATH)` | `save_position(f64, f64, &Path) -> io::Result<()>` | `{"x": …, "y": …}` with `round(_, 1)` and Python's float repr; `path.with_suffix(".json.tmp")` then rename. |
| `Rect` | `type Rect = (f64, f64, f64, f64)` | |
| `default_position(screen, size=WIDGET_SIZE, margin=SCREEN_MARGIN)` | `default_position(Rect, (f64, f64), f64)` | |
| `position_on_screens(pos, screens, size=WIDGET_SIZE)` | `position_on_screens(Option<(f64, f64)>, &[Rect], (f64, f64))` | |
| `run(once=False, directory=None, position_path=POSITION_PATH, store=None)` | `run(bool, Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) -> anyhow::Result<i32>` | Off macOS prints `eve-bridge: notes-widget is macOS-only (AppKit)` to stderr and returns 2. Same stdout lines (`notes-widget: dir=… level=… frame=(…) position_file=…`, `window visible=True number=…`, `--- rendered ---`). |
| `run.NotesTextView` (`menuForEvent_`) | `panel::NotesTextView` (`define_class!`, `menuForEvent:`) | The content view's menu. |
| `run.Controller` (`build`, `openLearning_`, `_context_menu`, `openNotes_`, `_open_first`, `openSaid_`, `openStars_`, `quit_`, `refresh_`, `_attributed`, `windowDidMove_`) | `panel::Controller` (`build`, `openLearning:`, `context_menu`, `openNotes:`, `open_first`, `openSaid:`, `openStars:`, `quit:`, `refresh:`, `attributed`, `windowDidMove:`) | Same panel level (`kCGDesktopIconWindowLevel + 1`, checked equal to pyobjc's: -2147483602), style, collection behaviour, colours, fonts, frames, menu titles and actions. `openLearning:` reads `EVE_LEARNING_PORT` (default 48766) as Python's `int()` does (`py_int_str`: whitespace, sign, `_` between digits). |
| `run.watch` (watchfiles thread) | `panel::watch` | `notify` (FSEvents) on the notes folder and, if it exists, `<store>/records`, recursive; watchfiles' `DefaultFilter` (`change_matters`) and its 300 ms debounce / 50 ms step; each batch posts `refresh:` with `performSelectorOnMainThread`. A failing watcher logs `notes-widget: file watcher stopped (…); polling every 30s`. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_notes_widget.py` | `test_note_files_only_today_and_yesterday_newest_first`, `test_note_files_skips_missing_days`, `test_notes_dir_env_override`, `test_day_of_parses_only_dated_filenames`, `test_parse_notes_keeps_note_lines_in_file_order`, `test_collect_notes_newest_first_across_days`, `test_collect_notes_applies_limit_after_ordering`, `test_collect_notes_missing_dir_is_empty`, `test_render_text_empty_state`, `test_the_two_provenances_are_never_mixed_into_one_list`, `test_starred_claims_are_pinned_above_the_days`, `test_a_day_journal_shows_its_title_and_what_eve_owes`, `test_a_forgotten_journal_line_is_never_shown`, `test_the_stars_come_from_the_records_starred_page`, `test_everything_is_gathered_newest_first_and_absence_is_normal`, `test_render_text_sections_per_day_with_headers`, `test_day_label_falls_back_to_iso_for_other_days`, `test_position_round_trip`, `test_load_position_tolerates_garbage` (5 payloads), `test_default_position_is_top_right_with_margin`, `test_position_on_screens_rejects_offscreen_saved_origin` | same names in `src/tests.rs`, plus `memory_dir_env_override`, `starred_claims_render_newest_first_with_a_star`, `only_the_first_two_debts_are_shown_and_case_does_not_matter`, `collect_conversations_stops_at_the_limit`, `a_journal_line_sorts_below_a_timed_note_of_the_same_day`, `load_position_reads_what_float_accepts`, `the_watcher_ignores_git_and_editor_files`, `the_learning_port_reads_as_python_int_does`, `render_text_without_today_uses_the_local_date`, `run_off_mac_refuses` (non-mac only) | `test_widget_plist_shape`, `test_widget_plist_is_separate_from_daemon_plist`: they test `_service_launchd`, which is another crate's module. |
| `bridge/tests/test_photos.py` (ref) | `test_photo_line_is_an_indented_image_that_is_not_a_note` (the `LINE_RE` part) | `test_photo_line_is_an_indented_image_that_is_not_a_note` | The rest tests photos.py. |
| `bridge/tests/test_diary.py` (ref) | the `parse_notes` assertion in the photo-taker test | `a_photo_line_under_a_diary_line_is_not_a_second_note` | The rest tests diary.py. |

The AppKit half has no unit test (as in Python, where `run()` is exercised by hand); `cargo run -p eve-notes-widget --example once` is the `--once` smoke run against throwaway folders, and was run on macOS: the window was created and visible and the rendered text printed.

## Boundaries
None. The panel uses `objc2` / `objc2-app-kit` directly; the file watcher uses `notify`.

## Differences
- `load_position` on a file holding a bare `NaN`/`Infinity` for `x` or `y`: Python returns `(nan, …)`, Rust returns None (`eve_transcripts::py::loads` reads non-finite literals as null); likewise a number literal past f64 (`1e400`), which Python reads as `inf` and serde_json rejects. The widget behaves the same either way: a NaN position is on no screen, so the default position is used.
- The pyobjc `ImportError` branch of `run` (exit 2 with a pip hint) has no counterpart: the frameworks are linked in.
- `run` returns `Err` where Python would raise: the notes folder cannot be created, or there is no main screen (Python's `AttributeError` on `None`).
- `openLearning:` with a non-numeric `EVE_LEARNING_PORT` logs a warning and opens nothing (Python raises inside the ObjC callback, which pyobjc logs).
