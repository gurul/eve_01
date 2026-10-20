# eve-muse-pi
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/muse_pi.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `ENV`, `ASK`, `SESSION_FILE`, `ANSWER_SECS`, `CONNECT_SECS`, `UNREACHABLE_LINE` | same `pub const`s | Values verbatim |
| `CONFIG_DIR` | `config_dir()` | `~/.config/eve-bridge` |
| `_TARGET`, `_SESSION` | `TARGET`, `SESSION` (private `LazyLock<Regex>`) | Same patterns, anchored (`fullmatch`) |
| `log` | `tracing` | Same messages: `muse: ssh did not start (<ExceptionName>)`, `muse: no answer from the Pi in <n> s`, `muse: asked (<n> chars), answered/no answer, ssh exit <code>`, `muse: ssh failed: <stderr, 200 chars>` |
| `Answer` | `Answer { ok, text }` | `PartialEq` like the frozen dataclass |
| (inline reply strings in `parse` / `ask`) | `OFF_LINE`, `NO_ANSWER_LINE`, `NOT_CONNECTED_LINE`, `LATE_LINE` | Named so callers and tests can compare; text verbatim |
| `target` | `target()`, `target_from(Option<&str>)` | `EVE_MUSE_PI` stripped like `str.strip()`; anything not a plain `user@host` is `""` |
| `available` | `available()` | |
| `session_id` | `session_id(Option<&Path>) -> io::Result<String>` | Reads `muse-session`, keeps a stripped lowercase uuid; otherwise writes a fresh uuid4 plus `\n` (parents made). A write failure is an `Err` (Python raises `OSError`) |
| `command` | `command(host, session) -> Vec<String>` | Same argv |
| `parse` | `parse(&[u8]) -> Answer` | UTF-8 with replacement, Python `splitlines` separators, last non-blank line, Python truthiness for `ok` and `error`, same reasons and order |
| `ask` | `ask(text, AskOptions) -> Answer` (async) | `AskOptions { host, session, timeout, ssh }`; `Default` = Python's defaults. Message on stdin, stdout and stderr read together (as `communicate`), timeout kills and reaps ssh. A failure to make the session file is reported as ssh not starting, as in Python (it is inside the same `try`) |
| (patching `muse_pi.ask` in tests) | `Muse` trait + `PiMuse` | Seam for telegram / link-chat ports: `available()` and `ask(text)` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_muse_pi.py` | `test_target_takes_only_a_plain_user_at_host`, `test_the_session_is_made_once_and_kept`, `test_parse_reads_the_reply_and_never_shows_raw_errors`, `test_ask_runs_ssh_with_the_message_on_stdin`, `test_ask_is_off_without_a_pi` | same names in `src/tests.rs`, plus `a_session_written_by_python_is_kept_and_the_directory_is_made`, `parse_says_each_reason_in_its_own_words`, `parse_reads_only_the_last_non_blank_line`, `splitlines_matches_python`, `the_command_is_key_login_only`, `ask_without_ssh_is_unreachable`, `ask_that_runs_too_long_is_cut_off`, `ask_with_a_failed_ssh_is_unreachable_and_never_shows_stderr`, `ask_survives_a_relay_that_never_reads_stdin`, `returncode_reads_signals_as_negative` | |
| | `test_muse_on_sends_words_to_muse_titled_muse_until_muse_off`, `test_muse_on_without_a_pi_says_so_and_stays_off`, `test_one_relay_at_a_time`, `test_a_failed_ask_is_said_plainly`, `test_the_app_switch_is_quiet`, `test_the_link_door_reads_and_flips_muse` | — | They test `telegram`'s Muse switch and `link_chat`'s `/api/link/muse` door, which belong to those crates (they can stand in for the Pi through the `Muse` trait) |

The env and `PATH` monkeypatching becomes parameters: `target_from(value)` and `AskOptions { host, ssh }`, since Rust tests run in parallel threads and must not mutate the process environment.

## Boundaries
None. (The Pi side is the external `ask.py`, reached over the system `ssh` exactly as Python reaches it.)

## Differences
- `session_id` treats a `muse-session` file that is not valid UTF-8 as malformed and replaces it; Python would raise `UnicodeDecodeError`.
- JSON that Python's `json.loads` accepts but `serde_json` does not (`NaN`/`Infinity` literals, unpaired surrogate escapes) reads as no reply (`UNREACHABLE_LINE`).
- The `ssh` program is an `AskOptions` field (default `ssh` on `PATH`), a test seam; Python finds `ssh` on `PATH` only.
