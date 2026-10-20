# eve-matchers
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/matchers.py` | `src/lib.rs` (plus `src/pyre.rs`, Python `re` semantics for the patterns) |

## API
| Python | Rust | Notes |
|---|---|---|
| `Decision` (`Literal["allow","ask","default"]`) | `enum Decision` | `as_str()`/`Display` give the Python strings; `Decision == "allow"` compares. |
| `DEFAULT_AUTO_ALLOW` | `DEFAULT_AUTO_ALLOW: &[&str]` | Verbatim. |
| `DEFAULT_ALWAYS_ASK` | `DEFAULT_ALWAYS_ASK: &[&str]` | Verbatim. |
| `MatcherConfig` | `struct MatcherConfig { auto_allow, always_ask, strict }` | `Vec<Pattern>` for the tuples of `re.Pattern`; `Default` is the dataclass default (empty, not strict). |
| `re.compile(p)` / `re.Pattern` | `Pattern::new`, `Pattern::search`, `Pattern::as_str` | Python `re` syntax and semantics via `pyre::translate` onto fancy-regex. |
| `_config_path` | `config_path()`, `config_path_from(xdg, home)` | `$XDG_CONFIG_HOME` when non-empty, else `$HOME/.config`; `/eve-bridge/matchers.toml`. `HOME` set but empty is honoured as `expanduser` does. |
| `_compile` | `compile` (private) | Bad regex: `matchers: ignoring bad regex %r (%s)` warning and skipped; a non-string entry is the uncaught `TypeError`. |
| `load_config` | `load_config(path: Option<&Path>) -> Result<MatcherConfig, ConfigError>` | Same fallbacks. `Err` is exactly what Python raises past its `try`: `TypeError` (`'int' object is not iterable`, `first argument must be string or compiled pattern`, `unhashable type: 'list'`) and the `OverflowError` for a repeat count of 4294967295 or more. |
| `_SHELL_META`, `_GLOB`, `_RUNNERS`, `_RUN_OR_WRITE_FLAGS` | `SHELL_META`, `GLOB`, `RUNNERS`, `run_or_write_flags` | Verbatim. |
| `is_simple` | `is_simple` | `str.isspace` is the exact Python set (U+001C..U+001F included). |
| `classify_command` | `classify_command(&str, &MatcherConfig) -> Decision` | Same precedence. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_matchers.py` | all 26 (parametrized cases kept as loops) | `tests/test_matchers.rs` (same names) | None. `test_is_simple_agrees_with_the_lean_model` runs the Lean model against the Rust `is_simple` on a seeded corpus (xorshift, not Python's Mersenne Twister); it skips when `lean` is absent, as Python's does. |
| `bridge/tests/test_permission_matrix.py` (tests_ref) | builds `MatcherConfig(auto_allow=(re.compile(r"^ls\b"),), ...)` for the daemon | covered by `Pattern::new` + `MatcherConfig` in the tests above | The matrix itself tests `daemon.py`, ported with that crate. |
| (new) | — | `tests/parity.rs`: 346 Python patterns against 101 strings and 3013 commands (`is_simple`, `classify_command` default and strict), expected values recorded from CPython 3.13 by `tests/fixtures/gen_fixtures.py`; extra `load_config` TOML-type tests; `pyre` unit tests | — |

## Boundaries
None.

## Differences
- `\N{NAME}` escapes in a user pattern: Python resolves the Unicode name; the port has no name table, so the pattern is treated as a bad regex (warned and skipped). Pinned by `tests/parity.rs` (`KNOWN`).
- `matchers.toml` is parsed by the `toml` crate (TOML 1.1); Python's `tomllib` is TOML 1.0. A file that uses only 1.1 syntax (multi-line inline tables, `\e`/`\xHH` escapes, times without seconds) loads in Rust and falls back to defaults in Python. Parse-error text in the warning differs.
- A `re.error` message in the bad-regex warning matches Python for the errors the translator detects; errors raised by fancy-regex itself carry its wording.
- Unicode tables are the Rust crates' (regex-syntax) rather than CPython 3.13's (Unicode 15.1) for `\w`, `\d`, `\s` and case folding; case-insensitive back-references fold as fancy-regex does.
- A search that exhausts fancy-regex's backtracking limit (Python would keep going) counts as a match for `always_ask` (asks) and as no match for `auto_allow`, with a warning.
