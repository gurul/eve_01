# eve-envfile
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/envfile.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_PATH` | `DEFAULT_PATH` | `"~/.config/eve-bridge/env"`, expanded when used |
| (`"EVE_ENV_FILE"` literal) | `ENV_FILE_VAR` | Set and not empty: that file; empty or unset: the default |
| `_KEY_RE` | private `is_key` | `^[A-Za-z_][A-Za-z0-9_]*$` over the whole string, ASCII only |
| `_LINE_BREAKS` | `LINE_BREAKS` | The 10 characters `str.splitlines()` breaks on; a test checks all of Unicode against Python's output |
| `_write_lock` | private `WRITE_LOCK` (`parking_lot::Mutex<()>`) | Process-wide, as in Python |
| `log` | `tracing` | Same messages: `env file: loaded …`, `env file: cannot read …`, `env file: set … in …`, `env file: removed … from …`. Names only, never values |
| `parse_env_file` | `parse_env_file(&str) -> IndexMap<String, String>` | Python's `str.splitlines` / `str.strip` / `lstrip` (`py_splitlines`, `py_strip`, `py_lstrip`, `is_py_space`, public); dict order kept (a repeated name keeps its first position, last value) |
| `load_env_file` | `load_env_file(Option<&Path>, &mut impl Environ) -> Result<Vec<String>, EnvFileError>` | `Environ` stands for the `MutableMapping`; `ProcessEnv` is `os.environ`, and `HashMap`/`BTreeMap`/`IndexMap` of `String` implement it. Missing file: `[]`; other read error: warning with Python's `[Errno N] text: 'path'` and `[]`; not UTF-8: `Err(Decode)` (Python's `UnicodeDecodeError` escapes too); BOM dropped (`utf-8-sig`) |
| `env_path` | `env_path(&impl Environ) -> Result<PathBuf, EnvFileError>` | Fails only as `Path.expanduser` raises `RuntimeError` |
| `set_key` | `set_key(&Path, &str, &str) -> Result<(), EnvFileError>` | Same line rules, same quoting, same refusals and messages |
| `remove_key` | `remove_key(&Path, &str) -> Result<bool, EnvFileError>` | |
| `_check_name` | private `check_name` | `EnvFileError::InvalidName`, Python's message verbatim |
| `_name_of` | private `name_of` | Through `parse_env_file`, as in Python |
| `_quoted` | private `quoted` | Quotes when Python's `strip()` would change the value (U+001C..U+001F included) or it is quote-wrapped |
| `_real` | private `real`, public `expanduser`, `realpath` | `expanduser` is `pathlib.Path.expanduser` (HOME, else the password database; `~user` from the password database; empty HOME is `/`; no home is `EnvFileError::HomeDir`, Python's `RuntimeError`), with pathlib's normalization. `realpath` is Python 3.13's non-strict `os.path.realpath`: missing parts kept, `..` after resolving, loops left as written |
| `_lines` | private `read_lines` | Missing file: no lines; any other error, undecodable bytes included, is returned so the file is never overwritten |
| `_replace` | private `replace_file` (+ `mkstemp`, `mkdir_parents`) | `.{name}.XXXXXXXX.tmp` from Python's 37 characters, `O_EXCL\|O_NOFOLLOW`, 0600; write, `fsync(2)`, chmod 0600, rename; temporary removed on any failure. A missing folder is made 0700 (its new parents get the default mode, as pathlib does) |
| `_sync_folder` | private `sync_folder` | `fsync` on the folder, errors ignored |
| (ValueError / RuntimeError / OSError) | `EnvFileError` | `InvalidName`, `InvalidValue`, `Decode`, `EmbeddedNull` (all `is_value_error()`), `HomeDir`, `Io` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_envfile.py` | 7 | 7, same names | None |
| `bridge/tests/test_envfile_set_key.py` | 20 (3 parametrized) | 20, same names; parametrized cases run as loops | The `None` case of `test_a_name_the_parser_would_not_read_is_refused`: a Rust name is always a `&str`. HOME changes (`monkeypatch.setenv`) go through a private home parameter, the `os.replace` patch through a rename parameter |
| `bridge/tests/test_suite_isolation.py` (tests_ref) | envfile part of `test_the_owners_env_file_is_never_loaded` | `test_an_unset_env_file_reads_the_default_under_home_and_another_file_still_loads` | The conftest guard and the `cli`/`composio_tools` parts belong to those crates |
| `test_install_lite_sh.py`, `test_lite_web_security.py`, `test_notes_detailed.py`, `test_onboarding.py` (tests_ref) | use `parse_env_file` / `load_env_file` as helpers | covered by the tests above | They test the installer, lite web, notes and onboarding |
| (parity) | | 14 more: `py_space_is_pythons_isspace`, `splitlines_matches_python`, `parse_edge_cases_match_python`, `quoting_is_minimal`, `a_bom_and_crlf_file_is_rewritten_with_plain_newlines`, `expanduser_matches_pathlib`, `realpath_matches_python`, `an_unreadable_env_file_loads_nothing`, `a_non_utf8_env_file_is_an_error_on_load`, `decode_errors_match_pythons_text`, `a_nul_value_is_refused_by_the_process_environment_only`, `oserror_text_matches_python`, `repr_matches_python`, `the_empty_env_file_var_means_the_default` | Expected values taken from Python 3.13 output |

## Boundaries
None

## Differences
- Python's `environ` defaults to `os.environ`; Rust takes it explicitly (`&mut ProcessEnv` for the process). `ProcessEnv` refuses a NUL in a value with `EnvFileError::EmbeddedNull`, where `os.environ` raises `ValueError`; it also refuses a key `std::env::set_var` would panic on, which the parser never produces.
- `~` expansion reads `HOME` from the process environment, as Python does; it is not read from the `environ` argument.
- The `UnicodeDecodeError` text is rebuilt from the first bad sequence (`invalid start byte`, `invalid continuation byte`, `unexpected end of data`, with Python's `bytes in position a-b` range for a run longer than one byte); `decode_errors_match_pythons_text` checks it against Python 3.13 for 17 files. A file that is only the start of a BOM reads as empty, as Python's incremental `utf-8-sig` decoder does.
- The path in the `cannot read` warning is quoted as Python's `repr()` quotes it; which characters are escaped is exact for Latin-1, separators and the common format characters, and the rest of Unicode is taken as printable (Python escapes a few more unassigned or private-use code points).
- Off Unix (not a target for the daemon) there is no chmod and the home comes from `USERPROFILE`.
