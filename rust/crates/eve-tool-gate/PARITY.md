# eve-tool-gate
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/tool_gate.py` | `src/lib.rs` (Python builtins in `src/py.rs`, tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `ACTING` | `ACTING: LazyLock<BTreeSet<String>>` | eve's own names unioned with `eve_app_registry::acting()` |
| `ACTING_READS` | `ACTING_READS` | |
| `_OWN_WORDS_ACTIONS` | `OWN_WORDS_ACTIONS` | `eve_app_registry::own_words()` |
| `_STATUS_ONLY` | `STATUS_ONLY` | `&[(name, action)]` |
| `CLEAN` | `CLEAN` | eve's own names unioned with `eve_app_registry::clean()` |
| `_SOURCES` | `SOURCES: LazyLock<IndexMap<String, String>>` | same insertion order; the apps' sources last, overriding in place as a dict spread does |
| `COMPOSIO_LITE_SOURCE`, `TASK_RESULT`, `TASK_QUESTION`, `ATTACHED`, `PICTURE` | same names, `&str` | verbatim |
| `acting` | `acting(name, &Args)` | `Args = serde_json::Map<String, Value>` |
| `needs_confirmation` | `needs_confirmation(name, &Args, tainted: &str)` | Python's default `tainted=""` is passing `""`; truthiness of `new_link` is Python's (`"true"`, `1`, `True` ask) |
| `taints` | `taints(name, &Args)` | |
| `source` | `source(name) -> Result<String, ProfileError>`, `source_in(name, &impl EnvMap)` | the error is `profile.is_lite()`'s ProfileError on a malformed EVE_PROFILE, raised only on the `COMPOSIO_` branch as in Python |
| `_lite` | `eve_profile::is_lite_in` inside `source_in` | |
| `SHOW_CHARS` | `SHOW_CHARS` | 1500, counted in characters (code points) |
| `_FILLERS` | `FILLERS` (private) | |
| `_hidden` | `hidden` (private) | `unicodedata.category == "Cf"` via `unicode-general-category` |
| `visible` | `visible(&str)`, `visible_value(&Value)` | `visible_value` is `visible(str(v))` for non-string inputs |
| `_flat` | `flat` / `flat_value` (private) | Python's `str.split()` whitespace set (adds U+001C-U+001F to Rust's) |
| `describe` | `describe(name, &Args) -> Option<String>` | empty-value filter, `json.dumps(ensure_ascii=False, sort_keys=True)` reproduced in `py::dumps_sorted` (Python separators, `repr(float)`) |
| `question` | `question` | verbatim text |
| `about` | `about` | verbatim text |
| `REFUSED`, `TOO_LONG` | `REFUSED`, `TOO_LONG: LazyLock<Value>` | same keys in the same order, same text |
| `SAY_CHARS` | `SAY_CHARS` | 220 |
| `_SAY_HEADS` | `SAY_HEADS` | the apps' `say()` last |
| `_SAY_WORDS`, `_SAY_FIXED` | `SAY_WORDS`, `SAY_FIXED` (private tables) | |
| `_said_value` | `said_value` (private) | bool → yes/no; str/int/float → `str()`; else sorted JSON |
| `say_act` | `say_act` | items sorted by key |
| `spoken_question` | `spoken_question` | verbatim text |
| `_words` | `words` (private) | the regex `[^\w@.]+\|(?<!\w)[.@]\|[.@](?!\w)` evaluated per character (Rust's regex has no lookaround): `.`/`@` kept only between two `\w` characters of the original text |
| `said_whole` | `said_whole` | |
| `SAY_EXACTLY`, `ASKED_ALOUD`, `TOO_LONG_ALOUD` | same names | verbatim |
| `FP_CHARS`, `FP_MIN`, `CARRIED` | same names | |
| `RESULT_LINES` | `RESULT_LINES` | |
| `_TOOL_LINE` | `TOOL_LINE` (private regex) | same pattern |
| `fingerprint` | `fingerprint(&str)` | |
| `Marks` | `Marks` | `Marks::new(size)` / `Default` (400); a `parking_lot::Mutex<VecDeque>` inside so one shared instance takes `&self` from any thread; `add` takes any iterator of strings; deque(maxlen) eviction kept |
| `Marks.add`, `Marks.clear`, `Marks.found` | `Marks::add`, `Marks::clear`, `Marks::found` | |
| `MARKS` | `MARKS: LazyLock<Marks>` | one per process |
| `_texts` | `texts` (private) | strings anywhere in objects and arrays, `data:` URLs skipped |
| `carried` | `carried(parts: IntoIterator<&Value>, marks: Option<&Marks>) -> &'static str` | `None` reads `MARKS` |
| (Python builtins) | `py_truthy`, `py_str` (pub); `py::strip`, `py::split`, `py::capitalize` (title case from a table generated over every code point with CPython 3.13; the rest lowered in context, so a final sigma is `ς`), `py::is_word`, `py::is_format`, `py::dumps_sorted` | `py_str` uses `eve_app_registry::py_repr` for non-strings |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_tool_gate.py` | `test_acting_is_exactly_the_tools_that_change_things`, `test_every_read_taints_and_eve_own_state_does_not`, `test_the_question_says_what_would_run_and_the_history_line_quotes_nothing_read`, `test_the_question_shows_the_whole_act_or_is_never_asked`, `test_carried_finds_a_read_s_transcript_line_and_a_marked_line_and_nothing_else`, `test_spotify_control_taints_when_it_answers_with_the_catalog`, `test_a_gate_question_shows_every_hidden_character`, `test_a_gate_question_without_hidden_characters_is_unchanged` | same names in `src/tests.rs`, plus `test_marks_skip_pictures_short_lines_and_forget_the_oldest`, `test_describe_writes_each_act_as_python_does`, `test_say_act_is_code_s_own_short_line`, `test_python_helpers_match_python` | the `apps_mcp.describe_whole` / `composio_tools.describe_whole` assertions inside `test_a_gate_question_shows_every_hidden_character` belong to those modules' crates. `test_a_call_too_long_to_show_is_refused_unasked`, `test_a_tainted_turn_s_reply_taints_the_next_turn_while_its_history_shows_it`, `test_a_task_s_closing_sentence_is_marked_as_outside_words`, `test_an_attached_picture_taints_the_turn_from_its_start`, `test_remember_after_a_read_runs_without_an_extra_approval`, `test_an_injection_in_a_web_result_cannot_delete_without_the_owner`, `test_a_routine_task_after_a_read_starts_without_extra_approval`, `test_the_owner_s_own_words_are_not_tainted`, `test_a_call_in_the_same_round_as_the_read_was_written_before_it`, `test_reads_stay_free_after_a_read`, `test_a_routine_menu_setting_after_an_events_read_does_not_ask`, `test_an_attached_file_taints_the_turn_from_its_start`, `test_a_gated_call_while_another_question_waits_asks_nothing`, `test_a_routine_calendar_write_runs_both_clean_and_tainted`, `test_a_spotify_play_taints_the_rest_of_the_turn`, `test_a_spotify_play_by_code_is_kept_as_outside_words_and_a_pause_is_not`: these drive `telegram.Inlet` (its `_gate`, `_turn`, `_direct_tool`) with the Telegram rig; they belong to the telegram crate |
| `bridge/tests/test_routine_confirmations.py` (ref) | `test_routine_or_internally_confirmed_actions_have_no_outer_gate`, `test_native_consequential_actions_keep_the_code_gate`, `test_recording_joining_and_answering_ask_only_after_outside_text` | same names | the inlet-driven tests belong to the telegram and voice crates |
| `bridge/tests/test_voice_taint_gate.py` (ref) | `test_said_whole_needs_codes_question_word_for_word_and_nothing_else` | same name | the VoiceSession tests belong to the voice-agent crate |
| `bridge/tests/test_composio_lite_decide.py` (ref) | `test_a_composio_read_is_your_connected_apps_in_lite_and_three_named_apps_in_full` | same name (env passed as a map via `source_in`) | the `composio_tools` tests belong to that crate |
| `bridge/tests/test_app_registry.py` (ref) | `test_the_tool_gate_sets_are_todays` | same name | the rest are eve-app-registry's |
| `test_phone_location.py`, `test_nyt.py`, `test_science_search.py`, `test_menu_tools.py` (ref) | the tool_gate assertions for `phone_location`, `nyt_*`, `science_search`, `read_file` | `test_the_reads_never_ask_and_always_taint` | the rest belong to those modules' crates |
| `test_eve_mcp.py`, `test_rundown.py`, `test_telegram_lite.py`, `test_voice_agent.py` (ref) | use `REFUSED`, `MARKS`, `needs_confirmation`, `TASK_RESULT` through other modules | covered by the tests above | the flows are those modules' |

A one-off differential run (not kept in the tree) compared 4000 random calls against Python 3.13's `tool_gate` for
`acting`, `needs_confirmation`, `taints`, `source`, `question`, `say_act`, `said_whole` and `carried`: all equal. The
same run compared `\w`, `Cf` and `str.isspace()` over every code point against Python 3.13 (Unicode 15.1): `Cf` and
whitespace are identical; `\w` differs only on characters Unicode 16 added.

Verification (2026-10-19) repeated it: 28000 random calls (Greek, Georgian, ligatures, final sigma, hidden
characters, C0 controls, nested arguments, floats) against CPython 3.13's `tool_gate` for `acting`,
`needs_confirmation`, `taints`, `source`, `describe`, `say_act`, `question`, `spoken_question`, `said_whole`,
`_words`, `carried`, `Marks.found`, `visible` and `fingerprint`: all equal after fixing `source`'s capitalization
(it upper-cased Greek letters with ypogegrammeni and Georgian Mkhedruli, and lowered a final sigma as `σ`). Over
every code point, `_words`, `visible` and `fingerprint` differ only on the 4384 characters unassigned in Unicode
15.1.

## Boundaries
None

## Differences
- `args` is a JSON object, so Python's `str(a)` fallback for a `json.dumps` TypeError cannot arise.
- `unicode-general-category` 1.1.0 carries Unicode 16; Python 3.13 carries 15.1. Characters assigned in Unicode 16
  count as `\w` in `said_whole`'s word split here and not in Python. The Cf set is identical.
- `needs_confirmation`'s `tainted` is a `&str` (every caller passes a taint string); Python accepted any truthy value.
