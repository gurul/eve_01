# eve-key-tap
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/key_tap.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `KEY_RETURN` | `KEY_RETURN: u16` | 36 |
| `FLAG_ALTERNATE` | `FLAG_ALTERNATE: u64` | 0x00080000, read by listen_key |
| `FLAG_SECONDARY_FN` | `FLAG_SECONDARY_FN: u64` | 0x00800000 |
| `KEY_KEYPAD_ENTER` | `KEY_KEYPAD_ENTER: u16` | 76 |
| `KEY_DOWN_ARROW` | `KEY_DOWN_ARROW: u16` | 125 |
| `KEY_UP_ARROW` | `KEY_UP_ARROW: u16` | 126 |
| `_enter_keycode` | `enter_keycode()` + `enter_keycode_for(Option<&str>)` | `EVE_ENTER_KEY` (empty/unset → "return"), stripped with Python's whitespace set (`py_strip`, which also drops U+001C..U+001F) and lowercased; "keypad", "keypad-enter" and "enter" → 76, else 36 |
| `TAPPABLE` | `static TAPPABLE: LazyLock<HashMap>` + `tappable_with(enter)` | computed once on first read (Python: at import) |
| `Poster` | `type Poster = Box<dyn Fn(u16, bool) + Send + Sync>` | |
| `_quartz_poster` | `quartz_poster()` + `poster_from(hid_source, force_hid, post)` | macOS: `CGEventSourceCreate(HIDSystemState)`, `CGEventCreateKeyboardEvent` with a NULL source unless `EVE_KEY_SOURCE=hid` (read when the poster is built, as Python does), `CGEventPost(kCGHIDEventTap)`; off macOS `None` |
| `_check_accessibility` | `check_accessibility(prompt)` | macOS: `AXIsProcessTrustedWithOptions({kAXTrustedCheckOptionPrompt: true})` when prompting, else `AXIsProcessTrusted()`; off macOS: the same warning and `true` (Python's ImportError branch) |
| `class KeyTapper` | `struct KeyTapper` | `Default` = `KeyTapper()` |
| `KeyTapper.__init__` | `KeyTapper::new(poster, trust_check)` + `KeyTapper::with_factory(..., factory)` | a built (not injected) poster gets `check_accessibility` unless a check is given; `with_factory` is the seam for the test that patches `_quartz_poster`; `with_keys` pins the allowlist |
| `KeyTapper.diagnose` | `KeyTapper::diagnose() -> i32` | same lines and exit codes (2 no poster, 1 untrusted, 0 ready); `codesign -dv --verbose=2 <resolved>` with a 10 s timeout, stderr-or-stdout searched for "adhoc"; split out as `signature_kind`; missing codesign, timeout or undecodable output leaves "unknown" |
| `KeyTapper._tap_via_system_events` | `KeyTapper::tap_via_system_events(keycode)` | `osascript -e 'tell application "System Events" to key code N'`, 5 s timeout, same warnings (stderr stripped with Python's whitespace set, first 200 chars; exception text as Python's `TimeoutExpired` / `OSError` str). Output is decoded as `text=True` does (strict UTF-8, universal newlines): undecodable output is Python's `UnicodeDecodeError`, logged as a System Events error and returning false |
| `KeyTapper.tap` | `KeyTapper::tap(&mut self, name)` + `tap_with_method(name, method)` | allowlist, poster None → false, trust check prompts only on the first call and is dropped after a pass, `EVE_KEY_METHOD` osascript/system-events → System Events, else down, 30 ms, up; same log lines (`%r` rendered by `py_repr`) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_key_tap.py` | `test_tap_enter_presses_and_releases`, `test_tap_refuses_unknown_key`, `test_tap_next_prev_map_to_arrows`, `test_tap_untrusted_posts_nothing`, `test_no_poster_is_a_clean_noop`, `test_enter_defaults_to_main_return`, `test_enter_can_be_switched_to_keypad`, `test_taps_use_the_null_event_source` | same names in `src/tests.rs`, plus `trust_check_prompts_only_on_the_first_attempt`, `a_passed_trust_check_is_not_asked_again`, `a_built_poster_gets_the_real_trust_check`, `unknown_method_falls_through_to_the_poster`, `keypad_enter_allowlist`, `flag_masks_match_quartz`, `signature_kind_reads_stderr_first`, `py_repr_matches_python`, `timeout_message_matches_python`, `run_with_timeout_captures_and_kills`, `strip_200_counts_characters`, `py_repr_escapes_what_python_does_not_print`, `py_text_matches_python_decode_errors`, `os_error_text_matches_python`, `strip_200_uses_python_whitespace`, `run_with_timeout_covers_held_pipes`, `env_values_strip_python_whitespace` | none. Env monkeypatching became value parameters (`enter_keycode_for`, `force_hid_for`, `tap_with_method`); the fake Quartz module became `poster_from` with a recording post function |
| `bridge/tests/test_daemon_touch.py` (ref) | patches `key_tap.KeyTapper` to a fake | — | exercises the daemon's `key` command, which belongs to the daemon crate |
| `bridge/tests/test_listen_key.py` (ref) | imports `FLAG_ALTERNATE`, `FLAG_SECONDARY_FN` | `flag_masks_match_quartz` | the rest tests listen_key |

## Boundaries
`osascript` (System Events) and `codesign` are subprocesses, exactly as in Python. `run_with_timeout` is `subprocess.run(capture_output=True, timeout=...)`: the deadline covers both the exit and the pipes closing, as `communicate` does, and the child is killed on timeout.

## Differences
- `diagnose` prints the Rust daemon's own binary (`std::env::current_exe`, then resolved with `canonicalize`) where Python prints `sys.executable`; the labels and advice text are kept verbatim, including the python-specific wording. `accessibility trusted:` prints `True`/`False` as Python does.
- Python's trust-check call falls back to a no-argument call on `TypeError`; the Rust `TrustCheck` always takes `prompt` and a check that does not care ignores it.
- The `%r` in "refusing unknown key" escapes the characters CPython's `str.isprintable()` rejects by category range (Cc, Cf, Co, Zs/Zl/Zp, noncharacters); unassigned code points outside those ranges print unescaped, since that needs the full Unicode tables.
