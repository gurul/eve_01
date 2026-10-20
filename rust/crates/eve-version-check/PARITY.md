# eve-version-check
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/version_check.py` | `src/lib.rs` (the module), `src/py.rs` (the Python value semantics the cache file depends on: `str.strip`, `str()`/`repr()`, `float()`, `repr(float)`, `json.dump` strings and floats) |

## API
| Python | Rust | Notes |
|---|---|---|
| `GITHUB_API` | `GITHUB_API` | verbatim |
| `CACHE_TTL_SECS` | `CACHE_TTL_SECS: f64` | `86400.0` |
| `USER_AGENT` | `USER_AGENT: LazyLock<String>` | `eve-bridge/{eve_meta::VERSION}` |
| `HTTP_TIMEOUT_SECS` | `HTTP_TIMEOUT_SECS` | `3.0` |
| `_cache_path` | `cache_path()`, `cache_path_for(platform, home, xdg_cache_home)` | `~/Library/Caches` on darwin, `~/AppData/Local` on win32, else `$XDG_CACHE_HOME` (empty falls back) or `~/.cache`; then `eve-bridge/update_check.json`. `Path.home()` is `$HOME` as Python reads it |
| `_disabled` | `disabled()` | exactly `EVE_BRIDGE_NO_UPDATE_CHECK=1` |
| `UpdateInfo` | `UpdateInfo { current, latest: Option<String>, fetched_at: f64 }` | `has_update()` and `is_fresh()` are the two properties; `PartialEq` like the frozen dataclass |
| `_parts` | `parts(&str) -> Vec<Part>` | `Part::Int` (unbounded, as normalized digits), `Part::Str`, `Part::Pre(is_release, pre)` for the trailing `(True, "")` / `(False, pre)` tuple |
| `_is_newer` | `is_newer` | Python tuple `>` emulated element by element; the pairs where Python raises `TypeError` (int vs str, a part vs the trailing tuple) return `false`, as the `except` does |
| `_fetch_latest` | `fetch_latest()` (blocking), `tag_from_body(&[u8])` | same URL and headers (`User-Agent`, `Accept: application/vnd.github+json`); any non-2xx final status, transport error or bad JSON is `None` with the same debug log line |
| `_load_cache` | `load_cache()`, `load_cache_at(&Path)` | `str(data["current"])` and `float(data["fetched_at"])` coerced as Python does (`py::str_of`, `py::float_of`) |
| `_save_cache` | `save_cache()`, `save_cache_at(&UpdateInfo, &Path)`, `cache_json(&UpdateInfo)` | byte-identical to `json.dump` defaults: `", "`/`": "` separators, `ensure_ascii`, `repr(float)`; creates parents; failure logged at debug |
| `check` | `check(force)`, `check_with(force, disabled, current, cache, fetch)`, `check_async(force)` | `check_with` is the test seam for Python's monkeypatched `_cache_path` / `_fetch_latest` / env; `check_async` is the daemon's `asyncio.to_thread(check, ...)` (runs on the blocking pool) |
| `render_cli` | `render_cli(&UpdateInfo)` | text verbatim, including the `—` and `→` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_version_check.py` | `test_is_newer` (12 params), `test_update_info_has_update_with_no_latest`, `test_update_info_has_update_with_newer_latest`, `test_update_info_is_fresh_within_ttl`, `test_update_info_is_stale_past_ttl`, `test_cache_roundtrip`, `test_cache_missing_returns_none`, `test_cache_corrupt_returns_none`, `test_check_disabled_by_env`, `test_check_uses_fresh_cache`, `test_check_force_ignores_cache`, `test_check_network_failure_returns_no_latest`, `test_check_stale_cache_triggers_refetch`, `test_check_cached_version_mismatch_triggers_refetch`, `test_render_cli_no_update`, `test_render_cli_has_update`, `test_render_cli_no_latest` | same names in `src/tests.rs` | - |
| `bridge/tests/test_update.py` (tests_ref) | `_fake_info` builds `UpdateInfo` directly | `fake_info_shape_from_test_update` | the rest tests `update.py` (crate eve-update) |
| (extra) | - | `is_newer_matches_python_edge_cases` (18 pairs recorded from Python, incl. the `TypeError` cases), `parts_builds_python_tuple`, `update_info_empty_latest_has_no_update`, `cache_path_per_platform`, `cache_file_bytes_match_python_json_dump` (recorded from Python), `save_cache_creates_parent_dirs_and_writes_python_bytes`, `save_cache_failure_is_swallowed`, `load_cache_reads_python_values`, `load_cache_rejects_unusable_shapes`, `tag_from_body_reads_tag_name`, `user_agent_names_the_package_version`, `check_network_failure_keeps_old_cache`, `check_reports_installed_version`, `fetch_latest_live` (ignored, live), `check_async_runs_off_the_runtime` (ignored, live: writes the real cache); `py.rs`: `float_repr_matches_python`, `strip_takes_python_whitespace`, `str_of_matches_python_str`, `parse_float_matches_python_float`, `float_of_follows_python_float`, `json_str_is_ensure_ascii`, `json_float_writes_python_tokens` | - |

## Boundaries
None

## Differences
- Timeout: urllib's 3 s is a per-socket-operation timeout; Rust uses a 3 s connect timeout and a 3 s whole-request timeout (`reqwest::blocking` has no per-read timeout). The body is a few KB, so the two agree in practice.
- Errors Python lets escape `check()` return a plain result in Rust: a releases body that is valid JSON but not an object (Python: `AttributeError` on `.get`), undecodable bytes in the cache file (Python: `UnicodeDecodeError`), and a cache `latest` that is neither a string nor null (Python carries it and raises later in `has_update`; Rust treats the cache as unusable and refetches). The daemon's update loop would otherwise die on these; the module's stated contract is "never raise".
- `_parts` treats only ASCII digits as numeric. Python's `str.isdigit()` also accepts other Unicode digits (`"٣"` becomes `int` 3, `"²"` raises `ValueError`); Rust compares such pieces as strings. Git tags are ASCII.
- `str()` of a non-string cache `current` (only reached in a hand-edited file, and never equal to the version) follows Python's `repr` for the common characters; exotic non-printable Unicode outside the control, separator and common format ranges is not escaped. Python's `json.load` also accepts the `NaN`/`Infinity` tokens, which serde_json rejects (the cache then reads as missing); Python never writes them, as `time.time()` is finite.
- Debug log text for transport and JSON errors carries reqwest / serde_json messages rather than urllib / json ones.
