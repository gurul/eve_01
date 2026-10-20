# eve-consent
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/consent.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `YES_WORDS` | `YES_WORDS` | frozenset → `&[&str]`, same members |
| `YES_OPENINGS` | `YES_OPENINGS` | same tuple order |
| `NO_WORDS` | `NO_WORDS` | same members |
| `HOLD_WORDS` | `HOLD_WORDS` | same members |
| `_WORD` | `WORD` (private `LazyLock<Regex>`) | same pattern `[a-z]+(?:'[a-z]+)?` |
| `_words` | `words` (private) | lowercase, curly apostrophe U+2019 → `'`, findall |
| `approves` | `approves` | first word in YES_WORDS or joined words start with an opening; no HOLD word |
| `denies` | `denies` | first word in NO_WORDS |
| `decision` | `decision` | returns `&'static str`: "allow", "deny" or "" |
| `BARE_MAX_WORDS` | `BARE_MAX_WORDS` | 4 |
| `BARE_EXTRA_WORDS` | `BARE_EXTRA_WORDS` | same members |
| `_BARE_WORDS` | `BARE_WORDS` (private `LazyLock<HashSet>`) | union of yes, no, extra and the opening words |
| `bare_decision` | `bare_decision` | same guard, then `decision` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_consent.py` | `test_a_clear_yes`, `test_anything_less_is_not_a_yes`, `test_go_away_is_honestly_a_yes_by_this_rule`, `test_a_clear_no`, `test_neither_defers`, `test_curly_apostrophes_are_read`, `test_a_bare_answer_is_read`, `test_a_sentence_that_opens_with_yes_or_no_is_not_a_bare_answer` | same names in `src/lib.rs` (parametrized cases looped), plus `test_opening_is_a_string_prefix_of_the_joined_words`, `test_word_pattern_takes_one_apostrophe_group` | None |

## Boundaries
None

## Differences
- Python's `_words` calls `str(reply)` so any object is accepted; the Rust functions take `&str`. Callers convert first.
