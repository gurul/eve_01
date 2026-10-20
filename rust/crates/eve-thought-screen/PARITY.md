# eve-thought-screen
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/thought_screen.py` | `src/lib.rs`; Python string/number behaviors the refusal reasons rely on in `src/py.rs`; tests in `src/tests.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `FLOOR_IMPORTANCE`, `FLOOR_NOVELTY`, `FLOOR_COOL` | same `pub const`s | `i64`, `i64`, `f64`; values verbatim (6, 7, 0.70) |
| `SUBJECT_OVERLAP`, `SUBJECT_WINDOW_SECS`, `TOPIC_LIMIT`, `TOPIC_WINDOW_SECS`, `SIMILARITY`, `HISTORY`, `MIN_INTERVAL_SECS`, `PER_HOUR`, `UBIQUITY` | same `pub const`s | Values verbatim |
| `_WORD` | `WORD` (private `LazyLock<Regex>`) | `[a-z][a-z'-]+` over the lowercased text |
| `STOPWORDS` | `STOPWORDS: LazyLock<HashSet<&str>>` | Same word list, split the same way |
| `content_words` | `content_words(&str) -> BTreeSet<String>` | `str.lower()` → `to_lowercase()`; `len(w) > 2` in characters |
| `content_sequence` | `content_sequence(&str) -> Vec<String>` | |
| `similarity` | `similarity(&str, &str) -> f64` | Jaccard; 0.0 when either side is empty |
| `trigrams` | `trigrams(&str) -> BTreeSet<(String, String, String)>` | Tuple ordering equals Python's tuple ordering (byte order of UTF-8 = code point order) |
| `tag_overlap` | `tag_overlap(&BTreeSet<String>, &BTreeSet<String>) -> f64` | |
| `Shown` | `Shown { at, text, tags, subject }` | Frozen dataclass → plain value struct; `frozenset` → `BTreeSet<String>` |
| `ThoughtScreen` | `ThoughtScreen` (all fields `pub`, `Default` = the dataclass defaults, `new()`) | Keyword construction is struct-update syntax: `ThoughtScreen { topic_limit: 2, ..Default::default() }`. Integer fields `i64`, so negative/zero values slice like Python (`py::tail_start`, `py::slice_index`) |
| `ThoughtScreen.ubiquitous_tags` | `ubiquitous_tags(&self) -> BTreeSet<String>` | `shown[-history:]`, fewer than 4 → empty, `n >= ubiquity * len` |
| `ThoughtScreen.subject_of` | `subject_of(&self, impl IntoIterator<Item: AsRef<str>>)` | Drops tags blank after Python `strip()` (incl. U+001C..U+001F), lowercases, `narrowed or wanted` |
| `ThoughtScreen.clears_the_floor` | `clears_the_floor(&self, importance, novelty, cool)` | |
| `ThoughtScreen.refusal` | `refusal(&self, now, thought, tags, importance, novelty, cool) -> Option<String>` | Keyword args become positional (Python defaults 0, 0, 0.0). Every reason text is byte-identical: `{:.2f}`/`{:.0f}` via `py::fixed` (half-even on the exact binary value, `nan`/`inf`), `repr()` of the trigram tuple and of `text[:40]` (40 code points) via `py::repr_str`, shared tags `/`-joined in sorted order |
| `ThoughtScreen.offer` | `offer(&mut self, now, thought, tags, importance, novelty, cool) -> bool` | Collects the tags once (the generator fix), counts `refused`, logs `screen: not showing (%s)` at debug through `tracing` |
| `ThoughtScreen.record` | `record(&mut self, now, thought, tags)` | Records without judging; trims to `max(history, per_hour * 8)` with Python's `del shown[:-keep]` semantics (keep 0 trims nothing) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_thought_screen.py` | all 23 | same 23 names in `src/tests.rs` | - (`test_record_shows_..._without_judging_it_again` monkeypatched `refusal` to throw; Rust `record` does not call `refusal` by construction, so the test asserts a thought `refusal` would now refuse is still recorded and `refused` stays 0) |
| (extra) | - | `refusal_reasons_match_python_text`, `whitespace_is_squashed_before_judging_and_recording`, `zero_history_compares_against_everything_like_python_slicing` in `src/tests.rs`; `fixed_matches_python_rounding`, `repr_matches_python`, `squash_splits_like_python`, `slicing_matches_python` in `src/py.rs` | Expected strings taken from running the Python module |
| `bridge/tests/test_daemon_explore.py`, `test_daemon_lesson.py`, `test_daemon_membus.py`, `test_daemon_touch.py` (ref) | construct `ThoughtScreen()` as the daemon's screen | covered by `Default` | Their assertions test the daemon, ported with the daemon crate |

## Boundaries
None

## Differences
- `repr()` of a caption fragment in the "says the same as" reason escapes the characters CPython treats as non-printable from a fixed table (controls, format characters, non-ASCII separators, private use, non-characters); code points unassigned in CPython's Unicode database are printed raw rather than escaped. This affects only a debug/log reason string.
- Python accepts any object as a tag (`str(t)`); Rust takes anything that is `AsRef<str>`.
