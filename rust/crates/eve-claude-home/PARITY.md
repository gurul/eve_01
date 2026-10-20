# eve-claude-home
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/claude_home.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `MULTI_ENV` | `MULTI_ENV` | `"EVE_CLAUDE_CONFIG_DIRS"`. `SINGLE_ENV` (`"CLAUDE_CONFIG_DIR"`) added as a named constant for the literal Python inlines. |
| `claude_config_dir(override=None)` | `claude_config_dir(Option<&OsStr>) -> Result<PathBuf, HomeDirError>`, `claude_config_dir_in(&HomeEnv, ..)` | override > `$CLAUDE_CONFIG_DIR` > `~/.claude`; `override or env` truthiness, `str.strip()` with Python's whitespace set, `Path.expanduser()` and pathlib normalization. |
| `claude_config_dirs(override=None)` | `claude_config_dirs(..) -> Result<Vec<PathBuf>, _>`, `claude_config_dirs_in` | Split on `os.pathsep` (`:`), strip each part, skip blanks, de-duplicate by normalized path string, order kept; empty result falls back to `[claude_config_dir()]` (no override, so `$CLAUDE_CONFIG_DIR` applies). |
| `daemon_config_dirs(override=None)` | `daemon_config_dirs(..)`, `daemon_config_dirs_in` | Multi value (override or env) wins as-is; otherwise `[claude_config_dir(), ~/.claude]` with the default added only when not already present. |
| `settings_path(override=None)` | `settings_path(..)`, `settings_path_in` | `<home>/settings.json`. |
| `transcript_roots(override=None)` | `transcript_roots(..)`, `transcript_roots_in` | `<home>/projects` for each home. |
| (pathlib `Path.home()` / `expanduser`) | `HomeEnv::home_dir`, `HomeEnv::expanduser`, `HomeEnv::from_process` | `~` from `$HOME` (password database when unset; empty `$HOME` gives `/`), `~user` from `getpwnam_r`. |
| (pathlib `RuntimeError`) | `HomeDirError` | Message `Could not determine home directory.` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_claude_home.py` | `test_defaults_to_dot_claude`, `test_env_config_dir_wins_over_default`, `test_explicit_override_wins_over_env`, `test_blank_env_falls_back_to_default`, `test_tilde_is_expanded`, `test_dirs_defaults_to_single_home`, `test_dirs_reads_multi_env`, `test_dirs_dedupes_and_skips_blanks`, `test_dirs_all_blank_falls_back`, `test_multi_env_beats_single_env`, `test_transcript_roots_appends_projects`, `test_hooks_target_the_file_claude_actually_reads` | Same names in `src/tests.rs`, plus `daemon_dirs_union_wrapper_home_with_default`, `daemon_dirs_default_only_once`, `daemon_dirs_multi_env_is_taken_as_is`, `empty_override_falls_through_to_env`, `blank_override_does_not_fall_through_to_env`, `fallbacks_ignore_override_but_read_single_env`, `paths_are_normalized_like_pathlib`, `dedupe_compares_normalized_paths`, `strip_uses_python_whitespace`, `empty_home_is_root`, `undeterminable_home_is_an_error`, `tilde_user_uses_password_database`, `process_forms_read_the_process_environment` | None. The autouse env-clearing fixture became a `HomeEnv` with both variables unset and `$HOME` at a temp dir; `monkeypatch.setenv` became fields on that `HomeEnv`. The extra cases were checked against the Python module under Python 3.13. |

## Boundaries
None

## Differences
- Python raises `RuntimeError` from `Path.expanduser()` when a home cannot be determined; Rust returns `Err(HomeDirError)` from every public function, so callers decide instead of unwinding.
- Values are read as bytes (`OsString`); an undecodable byte is never treated as whitespace, matching Python's surrogateescape decoding.
