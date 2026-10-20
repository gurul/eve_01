# eve-profile
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/profile.py` | `src/lib.rs` (editions, `apply`, the registries, the tool filter), `src/firewall.rs` (the process firewall), `src/pypath.rs` (the `posixpath`/`os` functions the firewall reads paths with, as CPython writes them; `realpath` is 3.13's walk, the daemon's interpreter, which walks on past a symlink loop where 3.11/3.12 stopped) |

## API
| Python | Rust | Notes |
|---|---|---|
| `PROFILE_ENV`, `FULL`, `LITE` | `PROFILE_ENV`, `FULL`, `LITE` | |
| `LITE_CHAT_ID` | `LITE_CHAT_ID: i64` | 1 |
| `ProfileError` (ValueError) | `ProfileError` (thiserror) | same message: `EVE_PROFILE must be 'lite' or 'full', or unset; not '<first 24 chars>'` (Python `repr`, via `eve_app_registry::py_str_repr`) |
| `active` | `active(Option<&HashMap>)`, `active_in(&impl EnvMap)` | None reads the process environment; `str.strip().lower()` with Python's whitespace set |
| `is_lite` | `is_lite(Option<&HashMap>)`, `is_lite_in` | `Result<bool, ProfileError>` (Python raises) |
| `LITE_FORCED` | `LITE_FORCED: LazyLock<IndexMap>` | same names, values and order; every `eve_app_registry::APPS` switch not listed is added as "0" |
| `LITE_DEFAULTS` | `LITE_DEFAULTS` | `EVE_BRIDGE_SOCK` = `~/.eve-lite/ipc.sock` |
| `LITE_SECRET_ALLOW` | `LITE_SECRET_ALLOW: &[&str]` | |
| `SECRET_SUFFIXES`, `SECRET_NAMES` | same | |
| `NOT_SECRET_NAMES` | `NOT_SECRET_NAMES: LazyLock<IndexMap>` | verbatim |
| `is_secret_name` | `is_secret_name` | |
| `_expand` | `expand` (private) | `~/` is the mapping's HOME, else `os.path.expanduser("~")` |
| `apply` | `apply(&mut impl EnvMap)`, `apply_process_env()` | same order of work (secrets, forced, defaults), same sorted result, same one log line (`tracing::info!`); `EnvMap` is implemented for `HashMap`, `IndexMap` (removal keeps the others' order, as `del` on a dict), `BTreeMap` and `ProcessEnv` (`os.environ`) |
| `_why` | `why` (private) | `dict.fromkeys(names, reason)` |
| `_MAC_TASKS`, `_DESK_VOICE`, `_FULL_DAEMON` | `MAC_TASKS`, `DESK_VOICE`, `FULL_DAEMON` (private) | verbatim |
| `LITE_CLASSIFIED` | `LITE_CLASSIFIED: LazyLock<IndexMap>` | every reason verbatim, in the dict's order (a later duplicate keeps the first position) |
| `LITE_TOOL_NAMES`, `LITE_TOOL_PREFIXES` | same, as `&[&str]` | |
| `tool_name` | `tool_name(&Value)` | Python truthiness for `not name` |
| `lite_tool_allowed` | `lite_tool_allowed(&str)`, `lite_tool_allowed_value(&Value)` | a non-string is never allowed |
| `filter_tools` | `filter_tools(iter of &Value) -> Vec<&Value>` (the same objects), `filter_tools_owned` | |
| `EXEC_ALLOW`, `PROTECTED_HOME`, `PROTECTED_ROOTS`, `FIREWALL_MODES` | same, as `&[&str]` | |
| `_SHELL`, `_DATA_VOLUME`, `_MAX_LINKS`, `_FORK_EXEC` | `SHELL`, `DATA_VOLUME`, `MAX_LINKS`, `FORK_EXEC` (private) | |
| `_EXEC_EVENTS`, `_PATH_EVENTS`, `_WATCHED` | `Event` (one variant per process-start event, `Event::File` for the rest), `FileEvent` with `name()` and `positions()` | the watched set is the enum itself |
| `_fold` | `fold` (crate) | ASCII lowercase, else NFC then full Unicode case folding (`caseless`, = `str.casefold`) |
| `_text` | `Arg::text` (private), `Arg` | `Arg::Path` for str/bytes/PathLike, `Arg::Fd`, `Arg::None`, `Arg::Other` (what `os.fspath` refuses) |
| `_absolute` | `absolute` (private) | |
| `_runnable` | `runnable` (private), `pypath::runnable` | `isfile` and `access(X_OK)` |
| `_on_path` | `on_path` (private) | |
| `_programs` | `programs` (private) | each start read the way its own call looks programs up; an unreadable argument is `Unreadable` (Python's TypeError/OSError) and the start is refused |
| `_sqlite_paths` | `sqlite_paths` (private) | `file:` URIs: query and fragment cut, `//host` dropped, percent-decoded |
| `_Reached` | `WalkEnd::Reached` (private) | |
| `_Guard` | `Guard` | `new`, `with_exec_allow` (the tests' patched EXEC_ALLOW), `with_link_check` (the tests' patched `os.lstat`), `call` (`__call__`), `check` → `Option<Refusal>`, `note`, `fault`, `counts`, `reaches(&Arg)`, `in_code`, `toward_code`, `lexical`, `walk`; `Mode` for its mode |
| `_GUARD`, `_GUARD_LOCK` | `GUARD: OnceLock<Guard>`, `GUARD_LOCK` (private) | |
| `_account_home` | `account_home` (crate) | `getpwuid_r` |
| `protected_roots` | `protected_roots(Option<&HashMap>)` | same folders, names and order |
| `code_roots` | `code_roots()` | see Differences |
| `_prime` | (none) | see Differences |
| `_wrap_fork_exec` | `Event::ForkExec` | see Differences |
| `install_firewall` | `install_firewall(&str) -> Result<(), FirewallError>` | bad mode first (`firewall mode is one of ('enforce', 'audit'), not 'off'`), then the profile; full eve logs `firewall: not installed: ...` and installs nothing; audit then enforce tightens, nothing loosens; same log lines |
| `firewall_counts` | `firewall_counts() -> IndexMap<String, u64>` | a copy; {} with no firewall |
| (the hook's call sites) | `audit(&Event)`, `audit_path(FileEvent, paths)`, `audit_command(&Command)`, `popen_event(&Command)`, `spawn`, `output`, `status` | see Boundaries |
| (PermissionError) | `Refused` inside `io::Error` (`PermissionDenied`) | text `[Errno 1] <words>`, `errno()` = EPERM |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_profile_identity.py` | all 10 | `tests/identity.rs`: same names; the fresh-process cases (`test_the_process_environment_is_read_when_none_is_given`, `test_apply_with_no_profile_leaves_this_process_alone`, `test_no_profile_no_firewall_and_lite_has_one`, `test_install_firewall_in_full_eve_says_so_and_installs_nothing`) re-run the test binary for one `child_*` case with only the environment given | None |
| `bridge/tests/test_profile_lite_gate.py` | `test_apply_reports_what_it_changed_and_runs_once`, `test_secrets_are_removed_but_the_openrouter_key`, `test_secret_shape`, `test_the_ipc_socket_default_fills_only_what_is_unset`, `test_the_log_line_carries_names_only`, `test_the_tool_filter`, `test_the_lite_chat_and_the_contract_names`, `test_every_lite_tool_name_is_a_real_tool`, `test_the_kept_switches_keep_their_defaults`, `test_each_forced_switch_reads_as_lite_in_its_own_module`, `test_every_reader_reads_lite_from_the_applied_environment` | `tests/lite_gate.rs`: the first seven by name (`test_the_log_line_carries_names_only` captures its line in a fresh process, `child_log_line`, since tracing caches call-site interest across test threads); `test_every_lite_tool_name_is_a_real_tool` checks each name is a quoted tool name in the Python tool modules; `test_the_kept_switches_keep_their_defaults` checks they are not forced, not set, and classified "kept"; `test_each_forced_app_switch_reads_as_lite_in_the_registry` and `test_every_app_reads_off_from_the_applied_environment` do the two reader tests for the app switches (`eve_app_registry::switched_on`); plus `test_tool_name_reads_each_shape` | the reader checks for the other ~55 switches go through their own modules' `configured()`/`enabled()` (telegram, miniapp, watch, ...), which live in other crates that do not depend on this one's upstream set; they belong with those crates. The kept switches' readers likewise |
| `bridge/tests/test_profile_classified.py` | all 7 | `tests/classified.rs`: same names, scanning the same Python source with a Rust tokenizer of Python (strings, f-strings, implicit joins, imports, `def` scopes, `importlib.import_module`); its module set (135), switches (221) and names (448) were checked identical to Python's `ast` scan (`dump_scan_when_asked` writes them when `EVE_PROFILE_SCAN_DUMP` is set); plus `test_the_tokenizer_reads_python_strings_as_ast_does` | None |
| `bridge/tests/test_profile_firewall.py` | all 20 | `tests/firewall.rs`: same names; the cases that install the firewall re-run the test binary (`child_*`), the rest judge with a `Guard` that is never installed. `test_developer_tools_and_the_macs_programs_are_refused` starts through `Command` (= Popen), `/bin/sh -c` (shell), `Event::System`, and `Event::PosixSpawn` by name and path (git counted 3, sh 2); `test_multiprocessing_spawn_is_a_process_start_too` is `test_a_fork_exec_start_is_a_process_start_too`; `test_this_processs_own_code_stays_importable_inside_a_protected_folder` is `..._readable_...`; plus `test_a_command_is_read_as_popen_reads_its_arguments`, `test_protected_roots_are_each_folder_as_given_resolved_and_on_the_data_volume`, `test_an_audit_guard_lets_an_event_through_and_an_enforcing_one_refuses_it` | `test_the_standard_librarys_own_programs_are_asked_before_the_firewall`: Rust's standard library starts no program to answer platform questions, so there is nothing to prime. In the files case, `open, bytes` and `pathlib` (Python-only spellings of `open`) and `walk`/`glob` (Python helpers that swallow the refusal) are left out; `asyncio` and `os.popen` likewise in the starts case |

## Boundaries
Python's firewall is a `sys.addaudithook` hook: the interpreter itself reports every process start and file open to
it, so every Python call site is covered without being changed. Rust has no audit hook. The Rust guard is the same
judge (same programs, same folders, same symlink walk, same counts, log lines and error text), and a Rust call site is
covered when it goes through it: `eve_profile::spawn`/`output`/`status` (or `audit_command`) for a `Command` (a
`tokio::process::Command` through `audit_command(cmd.as_std())`, as asyncio's starts raise Popen's event), and
`audit_path` before a file operation. Code that does not ask goes around it, as native code (PortAudio, ONNX, ctypes)
goes around Python's hook (the spec's Risk 5). The lite daemon's crates are the ones that must ask: quick_answers'
pmset, watch's ifconfig, lan_net's scutil, and any file they open on a path that came from outside.

## Differences
- `code_roots()`: Python exempts its interpreter's prefixes and the folder eve_bridge is imported from. The Rust
  daemon's code is its executable, so it exempts the executable's folder, plus the Python package's source folder
  (`bridge/src`) of the checkout it was built from when it exists (the assets both daemons serve live there).
- `_prime()` has no Rust counterpart: Python's `platform` module starts `uname -p` and `file` the first time it is
  asked; Rust's standard library starts nothing.
- `_wrap_fork_exec()` has no Rust counterpart to install: Rust has no multiprocessing that starts through
  `_posixsubprocess.fork_exec`; the event exists (`Event::ForkExec`) for any caller that starts programs that way.
- A `Command` cleared with `env_clear()` cannot be seen from outside; its program is looked up on this process's PATH,
  which is also what Rust's own lookup does when the command sets no PATH of its own.
- Paths are handled as UTF-8 text (lossy for anything else); APFS names are always UTF-8.
