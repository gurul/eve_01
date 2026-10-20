# eve-caption-pager
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/caption_pager.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `ELLIPSIS` | `ELLIPSIS` | `"…"` |
| `PagerConfig` (frozen dataclass) | `PagerConfig` (`Copy`, `Default`) | Same fields and defaults. `PagerConfig(read_cps=x)` is `PagerConfig::with_read_cps(x)` or struct update syntax. `cols`, `lines`, `max_pages`, `reply_budget_chars` are `usize`. |
| `wrap` | `wrap` | Splits like `str.split()` (Unicode whitespace plus U+001C..U+001F); lengths in code points, as `len`. |
| `complete_prefix` | `complete_prefix` | Returns a `&str` slice of the input; whitespace test matches `str.isspace`. |
| `paginate` | `paginate` | Pages are `Vec<String>` (`Page`) instead of tuples. |
| `_clamp` | private `clamp` | `max(lo, min(hi, x))`, so `lo` wins when `lo > hi` (no `f64::clamp` panic). |
| `page_hold_secs` | `page_hold_secs(chars, &cfg, first, last)` | Keyword-only args become positional. |
| `caption_instructions` | `caption_instructions(&cfg)` | Prompt text verbatim; a test pins the exact string. |
| `ShowPage` | `ShowPage` | `final` field is `r#final`. `to_wire` returns a `serde_json::Value` with Python's key order (`cmd, page, of, lines, hold_ms, chirp, final`). |
| `Clear` | `Clear` | `to_wire` -> `{"cmd":"caption","clear":true}`. |
| `Event` (Union alias) | `enum Event { ShowPage, Clear }` | `to_wire`, `as_show`, `is_clear` helpers. |
| `_Reply` | `Reply` (pub) | Same fields; public because callers and tests read `replies[i].pages`, as the Python tests do. |
| `CaptionPager` | `CaptionPager` | `CaptionPager()` is `CaptionPager::default()`; `CaptionPager(cfg)` is `CaptionPager::new(cfg)`. |
| `CaptionPager.busy` / `.visible` (properties) | `busy()` / `visible()` | |
| `CaptionPager.begin_reply` / `update` / `end_reply` / `reset` / `poll` | same names | `update(now, &text, final)`. |
| `CaptionPager._drop_all`, `_drop_empty_tail`, `_repaginate`, `_chars`, `_deadline`, `_page_changed`, `_of`, `_final_flag`, `_show`, `_step` | private `drop_all`, `drop_empty_tail`, `repaginate` (free fn), `chars`, `deadline`, `page_changed`, `of`, `final_flag`, `show`, `step` | `_step`'s tail recursion is a loop. |
| `CaptionPager._is_last_overall`, `_clear_at` | `is_last_overall`, `clear_at` (pub) | Public because the ported tests call them, as the Python tests do. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_caption_pager.py` | all 16 (23 pytest items with the 8 `test_page_hold_table` params) | `tests/test_caption_pager.rs`: same 16 names; the parametrised table is one test looping its 8 rows. Extra: `test_caption_instructions_verbatim`, `test_wire_format_key_order`, and unit tests in `src/lib.rs` for Python whitespace, code-point lengths and clamp. | None. The seeded fuzz tests use a SplitMix64 test RNG instead of Python's Mersenne Twister, so the random texts differ; the asserted invariants are the same. `caplog` is a `tracing_subscriber` writer capture. |
| `tests_ref` (`test_daemon_*.py`, `test_explore.py`, `test_voice_agent.py`) | They only construct `CaptionPager()` / `PagerConfig(read_cps=...)` and call `caption_instructions` for the daemon and voice agent. | Covered by the crate's API (`CaptionPager::default`, `PagerConfig::with_read_cps`). | The `EVE_CAPTION_CPS` parsing they test lives in `voice_agent.py` and belongs to that crate. |

A differential check (Python vs Rust on 5 texts x 4 streaming rates, every wire event with its time) produced byte-identical output, 272 events.

## Boundaries
None.

## Differences
None in behaviour. `hold_ms` uses round-half-to-even, as Python's `round`. The overflow warning is a `tracing::warn!` with the same message.
