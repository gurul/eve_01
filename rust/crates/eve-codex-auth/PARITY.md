# eve-codex-auth
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/codex_auth.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `ENV_KEYS`, `COPY_PATHS`, `EXCLUDED_NAMES` | `ENV_KEYS`, `COPY_PATHS`, `EXCLUDED_NAMES` | Same values, same order. |
| `_warned` | `WARNED` (private static), `Warned` type | Tests pass their own `Warned` instead of resetting a global. |
| `SetupError` | `SetupError`, wrapped in `Error::Setup` | `Error` also carries `Io` (OSError) and `Value` (ValueError), the three kinds the CLI catches. |
| `eve_home` | `eve_home()`, `eve_home_in(home)` | `~/.config/eve-bridge/codex-home`; `Path.home()` is `$HOME`, else the password database. |
| `api_ready` | `api_ready(home)` | Rejects a symlinked home or auth.json, nlink != 1, bad JSON/UTF-8, `auth_mode != "apikey"`, non-string or blank key (Python `str.strip` whitespace set). |
| `_once` | `once` (private) | Same warning text: `codex: <reason>; using the ChatGPT plan. Run eve-bridge codex-home for API setup.` via `tracing::warn!`. |
| `launch_environment` | `launch_environment(Option<&HashMap>)`, `launch_environment_with(env, eve_home, warned)` | Returns `(IndexMap, AuthMode)`; child env keeps `ENV_KEYS` order; `EVE_CODEX_AUTH` default `plan`. `AuthMode` compares equal to `"plan"`/`"api"`. |
| `_private_directory` | `private_directory` (private) | Symlink check over the path and every ancestor, mkdir 0700 (parents 0777&umask), chmod 0700. |
| `_copy_runtime` | `copy_runtime` (private) | Same exclusion, in-home, credential-name, cycle and hard-link checks with identical messages; `shutil.copy2` = copy + mode + atime/mtime; `.json`/`.toml` copies repoint the source home to the Eve home (read with universal newlines, as `read_text` does). |
| `_api_config` | `api_config` (private) | Same line filter (top-level only, before the first `[`), same prepended two lines, same TOML round-trip guard, chmod 0600. |
| `setup` | `setup(binary, Option<&HashMap>)`, `setup_in(home, binary, env)` | Same order of checks, staging in `.codex-setup-*` beside the Eve home, replace-then-move, login `[binary, "login", "--with-api-key"]` with the key + `\n` on stdin, env cleared to `ENV_KEYS` + `CODEX_HOME`, stdout/stderr discarded, 60 s timeout, then chmod 0600 on auth.json. |
| (`Path.resolve`) | `resolve(path)` | Port of non-strict `posixpath.realpath`, so dangling links and loops behave as in Python. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `tests/test_codex_auth.py` | `test_default_plan_environment_is_exactly_unchanged`, `test_api_ready_selects_only_private_home`, `test_missing_or_unsigned_home_falls_back_and_logs_once` (7 params), `test_invalid_setting_falls_back` (3 params), `test_setup_copies_runtime_not_auth_and_passes_key_only_on_stdin`, `test_repeat_refreshes_config_and_preserves_api_signin_without_key`, `test_no_key_means_no_login`, `test_failed_login_does_not_echo_output_or_key`, `test_destination_symlink_cannot_reach_personal_home`, `test_source_runtime_link_to_credentials_is_refused`, `test_hard_linked_auth_is_never_written`, `test_source_hard_link_to_auth_is_not_copied`, `test_cyclic_runtime_link_fails_cleanly_before_login`, `test_internal_runtime_links_are_copied_not_shared` | same names in `src/tests.rs` (params looped inside one test); extra: `test_missing_binary_could_not_run`, `test_login_timeout_kills_child`, `test_symlinked_parent_of_destination_is_refused`, `test_source_runtime_link_outside_home_is_refused`, `test_read_only_runtime_file_copies_with_mode_and_mtime`, `test_missing_source_config_still_writes_api_config`, `test_api_config_keeps_table_keys_and_refuses_unsafe_rewrite`, `test_resolve_matches_python_non_strict`, `test_splitlines_keepends_matches_python`, `test_api_ready_rejects_symlinked_home_and_hard_links`, `test_api_key_whitespace_only_is_not_ready` | `test_cli_setup_uses_loaded_env_and_does_not_change_mode` tests `cli.py`; it belongs to the cli crate. The fake CLI is a `/bin/sh` script using builtins only (the child has no PATH); it records whether `OPENAI_API_KEY` was present instead of the full list of env names. |
| `tests/test_codex_api_metering.py` (ref) | `test_missing_api_home_falls_back_in_both_environment_and_spend` (the codex_auth part) | `test_missing_api_home_keeps_personal_codex_home` | The rest patches `launch_environment` from codex_computer; it belongs to that crate. |

## Boundaries
None. The Codex CLI login is a subprocess in Python too.

## Differences
- TOML is parsed by the `toml` crate (TOML 1.1) instead of `tomllib` (TOML 1.0), so a few 1.1-only constructs that `tomllib` would reject are accepted. The round-trip guard is unchanged.
- An invalid config.toml raises `Error::Value("Codex config.toml is not valid TOML.")` rather than the decoder's message, which quotes the offending line and could hold a secret. The Python CLI never prints that message either.
- Python's `str(SetupError)` is the message; in Rust `Error::Setup(e).to_string()` gives the same message.
