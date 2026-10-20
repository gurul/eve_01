# eve-matchers
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/matchers.py` | `src/lib.rs` (plus `src/pyre.rs`, Python `re` syntax and semantics for the patterns, and `src/pycase.rs`, Python's IGNORECASE rules) |

## API
| Python | Rust | Notes |
|---|---|---|
| `Decision` (`Literal["allow","ask","default"]`) | `enum Decision` | `as_str()`/`Display` give the Python strings; `Decision == "allow"` compares. |
| `DEFAULT_AUTO_ALLOW` | `DEFAULT_AUTO_ALLOW: &[&str]` | Verbatim. |
| `DEFAULT_ALWAYS_ASK` | `DEFAULT_ALWAYS_ASK: &[&str]` | Verbatim. |
| `MatcherConfig` | `struct MatcherConfig { auto_allow, always_ask, strict }` | `Vec<Pattern>` for the tuples of `re.Pattern`; `Default` is the dataclass default (empty, not strict). |
| `re.compile(p)` / `re.Pattern` | `Pattern::new`, `Pattern::search`, `Pattern::as_str` | Python `re` syntax and semantics via `pyre::translate` onto fancy-regex. |
| `_config_path` | `config_path()`, `config_path_from(xdg, home)` | `$XDG_CONFIG_HOME` when non-empty, else `Path.home()/.config`; `/eve-bridge/matchers.toml`. `Path.home()` is `expanduser("~")`: `$HOME` with trailing slashes stripped, `/` when that leaves it empty, the password database when unset. |
| `_compile` | `compile` (private) | Bad regex: `matchers: ignoring bad regex %r (%s)` warning and skipped; a non-string entry is the uncaught `TypeError`. |
| `load_config` | `load_config(path: Option<&Path>) -> Result<MatcherConfig, ConfigError>` | Same fallbacks. TOML is read as `tomllib` reads it: TOML 1.0, so the 1.1 syntax the `toml` crate would accept (`toml_1_0_only`) is a parse failure that keeps the defaults. `Err` is exactly what Python raises past its `try`: `TypeError` (`'int' object is not iterable`, `first argument must be string or compiled pattern`, `unhashable type: 'list'`) and the `OverflowError` for a repeat count of 4294967295 or more. |
| `_SHELL_META`, `_GLOB`, `_RUNNERS`, `_RUN_OR_WRITE_FLAGS` | `SHELL_META`, `GLOB`, `RUNNERS`, `run_or_write_flags` | Verbatim. |
| `is_simple` | `is_simple` | `str.isspace` is the exact Python set (U+001C..U+001F included). |
| `classify_command` | `classify_command(&str, &MatcherConfig) -> Decision` | Same precedence. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_matchers.py` | all 26 (parametrized cases kept as loops) | `tests/test_matchers.rs` (same names, plus TOML-type, TOML 1.0, IGNORECASE and path tests) | None. `test_is_simple_agrees_with_the_lean_model` runs the Lean model against the Rust `is_simple` on a seeded corpus (xorshift, not Python's Mersenne Twister); it skips when `lean` is absent, as Python's does. |
| `bridge/tests/test_permission_matrix.py` (tests_ref) | builds `MatcherConfig(auto_allow=(re.compile(r"^ls\b"),), ...)` for the daemon | covered by `Pattern::new` + `MatcherConfig` in the tests above | The matrix itself tests `daemon.py`, ported with that crate. |
| (new) | — | `tests/parity.rs`: 389 Python patterns against 113 strings and 3013 commands (`is_simple`, `classify_command` default and strict), expected values recorded from CPython 3.13 by `tests/fixtures/gen_fixtures.py`; extra `load_config` TOML-type tests; `pyre`, `pycase` and `lib` unit tests. The translator was also checked against CPython 3.13 on about 190,000 random patterns (outside the repo); what still differs is listed under Differences | — |

## Boundaries
None.

## Differences
- `\N{NAME}` escapes in a user pattern: Python resolves the Unicode name; the port has no name table, so the pattern is treated as a bad regex (warned and skipped). Pinned by `tests/parity.rs` (`KNOWN`).
- The text of a TOML parse error in the `failed to parse` warning is the `toml` crate's, not `tomllib`'s. Which files parse is the same: the TOML 1.1 additions are refused before parsing.
- A `re.error` message in the bad-regex warning matches Python for the errors the translator detects; errors raised by fancy-regex itself carry its wording.
- Unicode tables are Rust's (Unicode 17 in `char::to_lowercase`/`to_uppercase`, regex-syntax for `\w`, `\d`, `\s`) rather than CPython 3.13's (Unicode 15.1). Only characters added after 15.1 can answer differently.
- IGNORECASE literals and classes follow Python exactly (lowercase plus `_EXTRA_CASES`, `src/pycase.rs`), but a case-insensitive back-reference (`(?i)(s)\1`) compares as fancy-regex folds case.
- Three corners of `re` that a translation onto fancy-regex does not reproduce, none of which a Bash allow or ask list plausibly uses:
  - A conditional on a group from inside that same group (`(a(?(1)x|y))`): Python takes the "no" branch until the group has closed once; fancy-regex takes the "yes" branch.
  - CPython's search prefilter compiles the first character class with the pattern's global flags, so `(?a:\W)` at the very start of a pattern also requires the first character to be non-word in Unicode terms (`(?a:\W)` does not match `é` in Python). The port applies the scoped ASCII class only.
  - fancy-regex refuses some look-behinds that Python accepts because it cannot prove their width, when they contain an empty or zero-width construct around a back-reference, atomic group or look-around (`(?<!()?+)`, `(?<!(?>){,2}x)`). Such a pattern is warned about and skipped.
- A search that exhausts fancy-regex's backtracking limit (Python would keep going) counts as a match for `always_ask` (asks) and as no match for `auto_allow`, with a warning.
