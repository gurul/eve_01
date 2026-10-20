# eve-photos
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/photos.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `PHOTOS_DIR`, `DEFAULT_MAX_PHOTOS`, `DEFAULT_MAX_MB`, `JPEG_MAGIC` | same-named `pub const` | Values verbatim. |
| `class PhotoConfig` | `struct PhotoConfig` (`Default`, `PartialEq`) | `max_photos` / `max_bytes` as `i64`; default 300 / 104857600. |
| `_env_int` | `env_int` (private) | `int(float(raw))`, blank → default, junk → warning `photos: NAME='raw' is not a number; using N`, negatives → 0. |
| `configured` | `configured(Option<&HashMap>)` + `configured_from(&HashMap)` | `None` reads the process environment. Same warnings, `%r` and `%g` reproduced. |
| `photos_root` | `photos_root` | |
| `relative_path` | `relative_path(&NaiveDateTime, i64)` | `photos/YYYY-MM-DD/HHMMSS-<id>.jpg`. |
| `photo_line` | `photo_line` | `"  ![](rel)"`. |
| `save` | `save(notes_dir, when, record_id, jpeg, Option<PhotoConfig>)` | Same refusals (cap 0, not a JPEG), chmod 0700 on `photos/`, write failure → `None` with a warning, then prune. |
| `all_photos` | `all_photos` | Day dirs sorted, `.jpg` suffix only (Python `Path.suffix` semantics: `.jpg` alone is not one). |
| `prune` | `prune` | Oldest first until both caps hold; removes an emptied day dir; same info/debug log lines. |
| `_rmdir_if_empty` | `rmdir_if_empty` (private) | |
| `usage` | `usage` → `(usize, u64)` | |
| `format_usage` | `format_usage` | `"N photo(s), X.X MB"`. |
| `iter_recent` | `iter_recent(notes_dir, last: i64)` → `Vec<PathBuf>` | Newest first; `last=0` → all; negative follows Python slicing. Returns a Vec instead of a lazy iterator. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_photos.py` | all 10 | `src/tests.rs`: the same 10 names (`test_relative_path_sorts_by_day_then_time` … `test_iter_recent_is_newest_first`), plus 7 more: `configured_parses_like_python_int_of_float`, `photo_config_default_matches_python`, `format_usage_counts_and_megabytes`, `all_photos_ignores_other_files_and_loose_files`, `iter_recent_zero_and_negative_follow_python_slicing`, `py_repr_quotes_like_python`, `configured_strips_like_python_str_strip` | None. `caplog` becomes a scoped `tracing` subscriber; `notes_widget.LINE_RE` is copied verbatim into the test (notes_widget is another crate). |
| `bridge/tests/test_diary.py` (tests_ref) | uses `PhotoConfig`, the relative path and the image line | covered by the tests above | The diary behaviour belongs to the diary crate. |

## Boundaries
None.

## Differences
- `EVE_PHOTOS_MAX=inf` / `EVE_PHOTOS_MAX_MB=inf`: Python raises `OverflowError` from `int(...)` and crashes the caller; Rust saturates to `i64::MAX` (effectively no cap).
- An unreadable `photos/` or day directory: Python's `iterdir` raises out of `all_photos`/`prune`; Rust treats it as empty.
- Python's `float()` also accepts non-ASCII Unicode decimal digits (`"١٢"`); Rust accepts ASCII digits only, so such a value is junk (warning + default).
- `configured(None)` reads only `EVE_PHOTOS_MAX` and `EVE_PHOTOS_MAX_MB` from the process environment, lossily as UTF-8 (a non-UTF-8 value is junk, as its surrogate-escaped str is in Python).
- OS error text in log lines uses Rust's `io::Error` wording rather than Python's `[Errno N] ...`.
