# eve-read-policy
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/read_policy.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `is_within` | `is_within(path: &str, base: &str) -> bool` | Same lexical rule: `Path(os.path.normpath(Path(x).expanduser()))` on both sides, then `p == b or b in p.parents`. `is_within_with(path, base, &dyn HomeLookup)` is the seam for `~` lookup; `SystemHome` reads `$HOME`, then `getpwuid_r`, and `getpwnam_r` for `~user`. |

Private helpers reproduce the CPython pieces exactly: pathlib's POSIX parse
(`//` root kept distinct from `/`, empty and `.` parts dropped, so `./~/x`
still expands), `Path.expanduser` (raises when the home is not found, which
becomes `false`), `posixpath.expanduser` (`HOME` set but empty gives `/`), and
`posixpath.normpath`.

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_read_policy.py` | `test_within_direct_child`, `test_within_equal_path`, `test_not_within_sibling`, `test_not_within_parent`, `test_relative_paths_rejected`, `test_empty_rejected`, `test_dotdot_that_climbs_out_is_not_within` | same names in `src/lib.rs` `tests`, plus parity edges (`tilde_*`, `empty_home_env_means_root`, `named_user_tilde`, `double_slash_root_is_distinct`, `root_base_contains_everything_absolute`, `trailing_slashes_and_dots_fold`, `normpath_matches_posixpath`, `system_home_lookup_runs`, `system_named_user_resolves`) | None |

The parity-edge expectations were checked against the daemon's CPython 3.13
(`bridge/.venv`) on 2026-10-19; all 20 cases agreed.

## Boundaries
None

## Differences
None. A non-UTF-8 `$HOME` is decoded lossily where Python would use
surrogate escapes; such a home cannot match a `&str` path either way.
