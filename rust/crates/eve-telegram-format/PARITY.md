# eve-telegram-format
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/telegram_format.py` | `src/lib.rs` (the module), `src/html.rs` + `src/html5_entities.rs` (`html.unescape`, the same port eve-nyt and eve-google-rest keep private; the entity table was checked equal to CPython 3.13's `html.entities.html5`, all 2231 names) |

## API
| Python | Rust | Notes |
|---|---|---|
| `MAX_MESSAGE_CHARS`, `PARSE_MODE`, `MAX_TITLE_CHARS`, `MAX_SUBTITLE_CHARS` | same names, `pub const` | `_TAG_HEADROOM` is the private `TAG_HEADROOM` |
| `_EMOJI`, `_DASH`, `_FENCE`, `_HEADING`, `_NUMBERED`, `_BULLET`, `_RULE`, `_QUOTE`, `_CODE_SPAN`, `_LINK`, `_BOLD`, `_ITALIC_STAR`, `_ITALIC_UNDER`, `_STRIKE`, `_TABLE_SEP`, `_TAG` | private `LazyLock` statics of the same names | the patterns with look-around (`_BOLD`, `_ITALIC_*`, `_STRIKE`, the `, (?=…)` in `plain`) run on `fancy-regex` with no backtrack limit, as Python's engine has none; the others on `regex`. `\s` is spelled as Python's `str.isspace` set (it includes U+001C..U+001F) and `\w` as `[\p{L}\p{N}_]`, which equals Python's `\w` over every code point (checked against CPython 3.13) |
| `plain` | `plain(&str) -> String` | Python's `$` (end, or before a final newline) is `\n?\z` |
| `escape` | `escape(&str) -> String` | |
| `_styles` | `styles` (private) | the NUL-held link URLs, as Python |
| `inline` | `inline(&str) -> String` | `_CODE_SPAN.split` rebuilt as the text between spans and the spans, in order |
| `_cells` | `cells` (private) | |
| `_lead` | `lead` (private) | |
| `table_blocks` | `table_blocks<S: AsRef<str>>(&[S]) -> Vec<String>` | |
| `render_body` | `render_body(&str) -> String` | the fence close is shared in the private `push_code` |
| `_clip` | `clip` (private) | |
| `compose` | `compose(body, title: Option<&str>, subtitle: Option<&str>) -> String` | |
| `_open_tags` | `open_tags` (private) | |
| `_safe_cut` | `safe_cut` (private) | over characters |
| `_slice` | `slice` (private) | over characters |
| `split` | `split(&str, limit: usize) -> Vec<String>` | Python's default `limit` is `MAX_MESSAGE_CHARS`; Rust callers pass it |
| `visible` | `visible(&str) -> String` | |

All lengths and cut positions count characters (code points), as Python's `len` and slicing do, so a
piece is at most `limit` characters exactly where Python's is.

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_telegram_format.py` | all 19 | `tests/test_telegram_format.rs`, same names, same assertions | none |
| `bridge/tests/test_watch_security.py`, `bridge/tests/test_watch_reverify.py` | the `"<a href" not in telegram_format.compose(alert, ...)` assertions | `test_a_bare_url_in_an_alert_is_never_a_link` (the formatter's half: a bare or non-http URL never becomes a link) | the defanging of a model's `[words](url)` note is `watch.evaluate`'s job, which the eve-web crate ports and tests |
| `bridge/tests/test_telegram.py` | uses `fmt.compose` / `fmt.visible` on what the bot sent | covered by the tests above | the bot's sending behavior belongs to the eve-brain crate (`telegram.py`) |
| (new) | | `test_matches_python_on_random_inputs`: 3047 cases (`plain`, `inline`, `render_body`, `visible`, `compose`, `table_blocks`, `split` at limits 2..4096) produced by the Python module itself, `tests/fixtures/gen_differential.py` → `tests/fixtures/differential.json`; every output equal. Proven able to fail: a one-character change to `slice` makes 86 cases differ | |
| (new) | | `test_plain_keeps_untouched_text_byte_for_byte`, `test_python_whitespace_is_whitespace`, `test_table_blocks_of_nothing_is_nothing`, `test_a_cut_that_cannot_progress_stops_instead_of_looping` | |

## Boundaries
None

## Differences
- `split` with a limit so small that the reopened tags fill the whole cut window (for example a limit of 10
  under `<i><b><s>`, or any limit of 0 or 1) loops forever in Python. Rust detects a cut that gives back a
  rest it has seen before and sends that rest as it is, over the limit; at limit 0 or 1 the window is one
  character. Every input on which Python returns gives the same pieces (the differential fixture leaves
  out the 53 random cases on which Python never returns).
- `table_blocks` of an empty list returns no blocks; Python raises `IndexError`.
