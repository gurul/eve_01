# eve-link-reactions
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/link_reactions.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `MAX_KEPT` | `MAX_KEPT` | 200 |
| `MAX_MINTED` | `MAX_MINTED` | 512 |
| `MAX_EMOJI_CHARS` | `MAX_EMOJI_CHARS` | 16 UTF-16 units |
| `KEY_RE` | `KEY_RE` (the pattern text), `key_ok(&str)` | Hand-written matcher, no regex. It also accepts one trailing `"\n"`, because Python's `$` matches before a final newline. |
| `FILE_NAME` | `FILE_NAME` | `link-reactions.json` |
| `TRANSIENT` | `TRANSIENT: &[&str]`, `is_transient(&str)` | Holds only 👀 (U+1F440) |
| `_EMOJI_CATEGORIES` | private `emoji_char` | So, Sk, Mn, Me and Cf, from `unicode-general-category`. A code point below U+2000 is refused. |
| `clean(emoji)` | `clean(&str) -> &str`, `clean_value(&Value) -> &str` | `clean_value` takes `Any`: a value that is not a string gives "" |
| `class Reactions` | `struct Reactions` | Thread-safe: a `parking_lot::Mutex` around the state, and a separate one around the listeners. Share it in an `Arc`. |
| `Reactions.__init__(path=None, *, clock=time.time)` | `Reactions::new(Option<&Path>)`, `Reactions::with_clock(path, clock)`, `Default` | `version = int(clock() * 1000)`, truncated as Python's `int()` does |
| `Reactions.version` (attribute) | `Reactions::version() -> i64` | |
| `Reactions.listeners` (list) | `add_listener(Listener)`, `has_listener(&Listener)`, `remove_listener(&Listener)` | `Listener = Arc<dyn Fn(&str) -> anyhow::Result<()> + Send + Sync>`. Identity is `Arc::ptr_eq`, which gives link_chat's `fn not in listeners` check. Listeners are called outside the lock, on a copy of the list. An `Err` or a panic is logged and ignored. |
| `Reactions.attach(path)` | `Reactions::attach(&Path)`, `Reactions::path()` | Loads the file once and ignores a second call. A reaction set before the attach wins: as `dict.update`, a key already loaded keeps its place and takes the newer emoji. Only the last `MAX_KEPT` are kept. Attaching never writes the file. |
| `Reactions._load(path)` | private `load` | A missing file loads as empty with no log. On an OS, UTF-8 or JSON error it logs `link reactions: <name> not read (<PythonErrorName>); starting empty`. JSON that is not an object loads as empty. A row is kept only when its key passes `KEY_RE`, its emoji passes `clean` and is not transient. File order is kept. |
| `Reactions._save()` | private `Inner::save` | Writes `<name>.tmp` with `O_WRONLY|O_CREAT|O_TRUNC|O_NOFOLLOW|O_CLOEXEC` and mode 0600, then renames it over the file. The bytes are the same as `json.dump(..., ensure_ascii=False)`: `{"k": "🏆", "k2": "✍"}`. Transient rows are left out. On an OSError it logs `link reactions: not saved (<PythonErrorName>); kept in memory`. |
| `Reactions.mint(key)` | `Reactions::mint(&str) -> Result<i64, BadKey>` | `BadKey` is `ValueError("bad message key")`. Ids count down from -1. Each key keeps the same id while it is remembered. The oldest of more than `MAX_MINTED` are forgotten. |
| `Reactions.is_app(message_id)` (staticmethod) | `Reactions::is_app(i64) -> bool` | `< 0` |
| `Reactions.set(message_id, emoji)` | `Reactions::set(i64, &str) -> Result<(), SetError>` | `SetError::NotAnEmoji` is `ValueError("not an emoji")`. `SetError::NoSuchMessage` is `KeyError("no such app message")`. The key is popped and inserted again as the newest, even when nothing changed. When nothing changed there is no version bump, no save and no listener call. Otherwise it logs `link reactions: <emoji>|cleared`. |
| `Reactions.get(key)` | `Reactions::get(&str) -> String` | "" when there is no reaction |
| `Reactions.snapshot()` | `Reactions::snapshot() -> serde_json::Map<String, Value>` | Ordered with the oldest set first, as the dict is, so it serializes as Python's does |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_link_reactions.py` | `test_one_reaction_per_message_a_new_one_replaces_it_and_empty_takes_it_off` | `one_reaction_per_message_a_new_one_replaces_it_and_empty_takes_it_off` | |
| | `test_an_unknown_id_or_a_word_is_refused` | `an_unknown_id_or_a_word_is_refused` | |
| | `test_clean_keeps_only_a_short_emoji` | `clean_keeps_only_a_short_emoji` | |
| | `test_reactions_are_kept_in_their_file_and_junk_in_it_is_dropped` | `reactions_are_kept_in_their_file_and_junk_in_it_is_dropped` | |
| | `test_eyes_are_never_kept_in_the_file_so_a_crash_cannot_leave_them_on` | `eyes_are_never_kept_in_the_file_so_a_crash_cannot_leave_them_on` | The check `telegram.SEEN_REACTION in TRANSIENT` uses the 👀 literal here. The telegram crate owns that constant. |
| | `test_only_the_newest_reactions_are_kept` | `only_the_newest_reactions_are_kept` | |
| | `test_a_receipt_on_an_app_message_lands_in_the_app_not_in_telegram`, `test_an_app_message_id_is_never_a_telegram_reply_link`, `test_words_heard_from_the_app_get_an_id_and_telegram_words_do_not`, `test_a_star_from_the_app_is_a_trophy_on_the_owners_bubble_with_no_second_model_call`, `test_take_notes_starting_is_a_quill_in_the_app_and_still_the_models_answer_in_telegram`, `test_an_app_take_notes_start_while_already_running_is_said_not_a_silent_quill`, `test_an_app_take_notes_stop_or_status_is_not_a_receipt`, `test_a_photo_from_the_app_wears_eyes_while_eve_reads_it_and_they_go_when_the_turn_ends`, `test_a_telegram_photo_for_eve_still_gets_no_eyes`, `test_a_reaction_wakes_the_long_poll_and_an_old_version_answers_at_once`, `test_the_app_client_id_is_the_turn_id_only_when_it_is_a_plain_unused_id` | — | These test `TelegramInlet` (`_receipt`, `_unreact`, `_say`, `hear`, `_turn`, `_image`) and the `link_chat` door (`/api/link/messages`, long poll, `turn_id`, `split_cursor`). The telegram and link-chat crates port them. |
| (new) | — | `the_file_is_byte_for_byte_what_python_writes`, `a_python_written_file_is_read_in_its_order`, `attach_keeps_a_reaction_set_before_the_file_was_found_and_happens_once`, `attach_keeps_only_the_newest_when_the_file_holds_too_many`, `a_reset_reaction_moves_to_the_newest_place`, `minted_ids_count_down_and_the_oldest_are_let_go`, `keys_follow_python_key_re`, `a_failing_listener_never_costs_the_reaction`, `a_file_that_cannot_be_written_keeps_the_reaction_in_memory`, `the_version_starts_at_the_wall_clock_in_milliseconds` | |

## Boundaries
None

## Differences
- **Unicode version.** The general category comes from `unicode-general-category` 1.1.0, which uses Unicode 16.0. The live daemon's Python 3.13 uses `unicodedata` 15.1. I compared every code point from U+2000 to U+10FFFF once against Python 3.13's table. Rust also accepts 772 code points that Unicode 16 newly assigned to So, Sk, Mn, Me or Cf, such as U+2427 to U+2429 and the U+1CC00 symbols. Python 3.13 sees them as unassigned. Rust refuses U+1171E, because Unicode 16 moved it from Mn to Mc. Neither change touches an emoji that Telegram or the app uses as a reaction. A Python that ships Unicode 16 (3.14 or later) agrees with Rust.
- **Odd JSON in the file.** Python's `json.loads` accepts `NaN`/`Infinity` and lone surrogate escapes. `serde_json` rejects both, so such a file loads as empty with the "not read" warning. Python would drop only the bad rows, and a lone surrogate would make its `attach` raise. Neither daemon ever writes such a file.
- **Listener failure log.** Python logs the exception's class name. Rust logs `Error` when a listener returns `Err` and `panic` when it panics.
