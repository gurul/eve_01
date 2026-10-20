# eve-web-auth
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/web_auth.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `COOKIE_MAC`, `COOKIE_DEV`, `MAX_AGE` | same consts | `MAX_AGE: i64 = 34_560_000` |
| `PAIR_SECS`, `PAIR_TRIES`, `LOGIN_SECS`, `LOGIN_KEEP_SECS`, `MAX_LOGINS`, `MAX_DEVICES`, `SEEN_SECS`, `TEXT_CHARS` | same consts | seconds that Python uses as floats are `f64` (`PAIR_SECS`, `LOGIN_SECS`, `LOGIN_KEEP_SECS`) |
| `DOORS`, `DEFAULT_DOOR` | same consts | |
| `TOKEN`, `DEVICE_ID`, `SHA256_HEX`, `CODE_SEPARATORS` | `LazyLock<Regex>` statics, plus `is_token`, `is_device_id` | anchored `\A..\z`, so `is_match` is Python's `fullmatch`; `CODE_SEPARATORS` adds U+001C..U+001F, which Python's `\s` matches and Rust's does not |
| `_flags` | `flags` (private) | `Err(CookieError::NotACookie)` for Python's `ValueError("not an eve lite cookie")` |
| `set_cookie` | `set_cookie(name, token) -> Result<String, CookieError>` | name checked before token, as in Python |
| `clear_cookie` | `clear_cookie(name) -> Result<String, CookieError>` | |
| `cookie_value` | `cookie_value(Option<&str>, name) -> String` | `None` is a missing header |
| `Device` | `Device` | `Serialize` gives `Devices.list()`'s row shape {id, label, host, created, last_seen} |
| `_digest` | `digest` (private) | SHA-256 hex |
| `_text` | `text` (private) | `str.isspace` = `char::is_whitespace` + U+001C..U+001F; category C via `unicode-general-category` |
| `_stamp` | `stamp` (private) | JSON integer ≥ 0 only (bools and floats refused) |
| `_door` | `door` (private) | |
| `_row` | `row` (private) -> `Option<Row>` | `Row` keeps the file's key order |
| `_write_private` | `write_private(path, data) -> io::Result<()>` (pub) | `.name.tmp`, `O_CREAT|O_EXCL|O_NOFOLLOW`, 0600, fchmod, fsync, rename; tmp removed on failure |
| `Devices` | `Devices` | `new(path)` (wall clock), `with_clock(path, clock)`; test seams `with_writer` (Python patched `_write_private`) and `with_max_devices` (Python patched `MAX_DEVICES`) |
| `Devices.add` | `add(label, host: Option<&str>, door) -> io::Result<(id, token)>` | pass `DEFAULT_DOOR` for Python's default |
| `Devices.check` | `check(token: &str, door: Option<&str>) -> Option<Device>` | |
| `Devices.revoke` | `revoke(id_or_all) -> io::Result<Vec<String>>` | rows leave memory before the save, so a failed save still stops the token |
| `Devices.list` | `list() -> Vec<Device>` | |
| `Devices._stat`, `_sync`, `_read`, `_kept`, `_write` | `stat`, `sync`, `read`, `kept`, `write` (private) | file written as `json.dumps(indent=2)` + `"\n"` with `ensure_ascii` escapes (`python_json`), byte for byte; read through `python_json_compat`, which rewrites what `json.loads` takes and serde_json refuses (`NaN`/`Infinity`/`-Infinity`, lone surrogate escapes, the integer `-0`, a number past f64's range such as `1e400`; an integer past 4300 digits stays unreadable, as Python's `json.loads` refuses it with a `ValueError`) so one odd value costs its row, not the whole file, as in Python |
| `_code_digits` | `code_digits(&Value)` (private) | JSON integer or string, as Python's `int`/`str` |
| `PairingWindow` | `PairingWindow` | `new()` (monotonic clock), `with_clock` |
| `PairingWindow.open` | `open()` / `open_for(ttl)` | Python's default `ttl=PAIR_SECS` is `open()` |
| `PairingWindow.close` | `close()` | |
| `PairingWindow.is_open` | `is_open()` | property becomes a method |
| `PairingWindow.remaining` | `remaining()` | property becomes a method |
| `PairingWindow.pair` | `pair(secret: Option<&Value>, code: Option<&Value>) -> bool` | the request body's values as they came; JSON null equals absent (Python's `body.get`) |
| `PairingWindow._live`, `_shut` | `Window::live`, `Window::shut` (private) | |
| `_Login` | `Login` (private) | |
| `MacLogin` | `MacLogin` | `new()` (monotonic clock), `with_clock`; `Debug` shows only the hashes (Python's `vars`) |
| `MacLogin.issue` | `issue()` / `issue_for(ttl)` | |
| `MacLogin.consume` | `consume(Option<&str>) -> bool` | `None` stands for any non-string |
| `MacLogin.used` | `used(Option<&str>) -> bool` | |
| `MacLogin._prune` | `prune` (private) | |
| `time.time` / `time.monotonic` clocks | `Clock`, `wall_clock()`, `monotonic_clock()` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_web_auth.py` | all 31 (the parametrized `test_the_code_pairs_once_as_typed` is one Rust test looping its 5 cases) | `src/tests.rs`: same names, plus `test_cookie_value_keeps_what_follows_the_first_equals_sign`, `test_the_file_is_written_as_python_writes_it`, `test_a_file_that_is_not_a_device_list_admits_no_one`, `test_text_matches_python_isspace_and_categories`, `test_write_private_replaces_a_stale_temporary_file`, `test_code_digits_takes_only_a_six_digit_code`, `test_a_non_string_secret_counts_as_a_wrong_try`, `test_what_python_json_reads_costs_one_row_not_the_file`, `test_a_number_past_f64_costs_one_row_as_in_python` | The `None` / `42` inputs to `check`, `consume`, `used` and `set_cookie`: the Rust signatures take strings, so a non-string cannot be passed (`consume`/`used` take `Option<&str>` and `None` covers it) |
| `bridge/tests/test_cli_lite.py`, `test_lite_web_lan.py`, `test_lite_web_local.py`, `test_lite_web_security.py` (tests_ref) | they drive web_auth through lite, lite_web and the CLI | none here | they test those crates' routes and IPC; the web_auth behavior they rely on (cookie names, `TOKEN`, `PAIR_TRIES`, door refusal, `MacLogin.used`) is covered above |

## Boundaries
None

## Differences
- `_text`'s category C uses Unicode 16 tables (`unicode-general-category` 1.1.0); Python uses its own `unicodedata` version, so a code point assigned between the two versions may be kept by one and dropped by the other.
- A `created`/`last_seen` stamp larger than `i64::MAX` is dropped as malformed (Python's int has no bound). No real file holds one.
- Log lines name Python's exception class for the failure (`JSONDecodeError`, `UnicodeDecodeError`, `PermissionError`, `OSError`, ...), mapped from the errno as CPython's `OSError` subclasses are; the messages are otherwise the same.
- `PairingWindow.pair(code=-0)` from a request body: serde_json gives the float -0.0, so Rust refuses it where Python's int 0 is `000000`. No client sends it.
- A device file nested deeper than serde_json's 128 levels is unreadable in Rust; Python's limit is near 1000. A real file nests 3 deep.
- `PairingWindow.remaining` reads the clock once; Python reads it twice (in `_live` and in the subtraction). Only a clock that moves between the two reads tells them apart.
- `monotonic_clock()` counts from the process's first use, not from boot; only differences between its readings are used.
