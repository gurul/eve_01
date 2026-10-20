# eve-memory-sources
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/memory_sources.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `KINDS` | `KINDS` | `[&str; 3]`, same order |
| `MAX_SOURCES` | `MAX_SOURCES` | 6 |
| `MAX_TEXT_CHARS` | `MAX_TEXT_CHARS` | 200 (counted in chars, as Python counts code points) |
| `MAX_WHEN_CHARS` | `MAX_WHEN_CHARS` | 40 |
| `_RECALLED` | `RECALLED` (private `LazyLock<Regex>`) | same pattern with `(?s)`; `\s` widened to `[\s\x1c-\x1f]` to match Python's str `\s` |
| `_WHO` | `who_name` (private) | same four names, unknown speaker passes through |
| source dict | `Source` struct | serde `{kind, text, when?}`; `when` omitted when absent; `to_value`/`to_value(&[Source])` give the JSON the Python writes |
| `_clip` | `clip` (private) | `" ".join(s.split())` with Python's `isspace` set, cut to `cap-1` chars + rstrip + `…` |
| `_source` | `source` (private) | callers pass `str(x or "")` already rendered (see `py_str_or_empty`) |
| `_said` | `said` (private) | |
| `_hit` | `hit` (private) | `"line" in hit` / `"text" in hit` by key presence |
| `_recalled` | `recalled` (private) | dict form (`src or day`) and the "(tag) text" string form |
| `from_result` | `from_result(&str, &Value) -> Vec<Source>` | `ok` must be exactly JSON `false` to fail, as `is False` |
| `_list` | `list` (private) | |
| `add` | `add(&mut Vec<Source>, &[Source])` | |
| `sanitize` | `sanitize(&Value) -> Vec<Source>` | |
| `mentions` | `mentions(&Value, impl Fn(&str) -> bool) -> bool` | |
| (Python `str()` / `or`) | `py_str`, `py_str_or_empty` (pub), `truthy`, `py_or`, `py_repr`, `py_float` (private) | render decoded JSON as Python's `str()` does, so a non-string `id`, `src`, `text` reads the same (`True`, `1.0`, `1e+16`, `[1, 'a', None]`) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_memory_sources.py` | `test_a_search_result_becomes_sources_of_each_kind_in_turn`, `test_a_read_is_a_fact_or_a_stretch_said_and_a_failure_is_nothing`, `test_sources_are_capped_in_number_and_length_and_never_repeat`, `test_sanitize_keeps_well_formed_entries_only` | same names in `src/lib.rs`, plus `test_clip_collapses_whitespace_and_cuts_with_an_ellipsis`, `test_said_names_the_speaker_and_joins_day_and_time`, `test_hits_take_facts_without_ids_and_lines_said`, `test_recalled_takes_dicts_bare_strings_and_tagged_strings`, `test_groups_interleave_so_one_kind_never_crowds_out_the_rest`, `test_a_read_with_neither_record_nor_text_is_nothing`, `test_non_string_values_render_as_python_str_does`, `test_mentions_reads_each_source_text`, `test_sources_round_trip_through_json` (expectations checked against the Python module) | `test_a_turn_that_searched_memory_writes_src_on_its_reply_and_one_that_did_not_writes_none`, `test_the_model_requests_are_byte_identical_with_and_without_sources`, `test_a_reply_written_without_a_turn_keeps_no_sources` drive `telegram.py`'s turn loop (crate `eve-brain`); `test_forget_takes_a_source_that_quotes_the_words_and_keeps_the_reply`, `test_a_forgotten_reply_keeps_no_sources` drive `transcripts.py` (crate `eve-transcripts`); `test_the_app_row_carries_sources_on_eve_lines_only_sanitized` drives `link_chat.py` (crate `eve-link`). Those crates list this file in `tests_ref` and port them there. |

## Boundaries
None

## Differences
- Inputs are `serde_json::Value` (decoded JSON), not arbitrary Python objects; every value the daemon passes here is decoded JSON or a dict built from it.
- `repr()` of a string nested inside a non-string value (e.g. a list as a `text`) escapes non-printable characters by an approximation of `str.isprintable` (control, separator and common format characters, private use). Unassigned code points print as-is where Python would escape them.
