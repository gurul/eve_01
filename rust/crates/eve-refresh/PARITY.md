# eve-refresh
Status: complete

## Modules
| Python | Rust |
|---|---|
| `refresh.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `EVE` | `EVE` (`LazyLock<PathBuf>`) | The checkout the crate was built from, canonicalized |
| `APP` | `APP` | `EVE/ios/EveLink/build/Build/Products/Debug-iphoneos/EveLink.app` |
| `GOOGLE_LIFE` | `GOOGLE_LIFE_DAYS`, `google_life()` | 7 days |
| `LABEL` | `LABEL` | `com.github.eve-bridge.refresh-reminder` |
| `WEEKDAY, HOUR, MINUTE` | `WEEKDAY`, `HOUR`, `MINUTE` | 5, 9, 0 |
| `LOG` | `log_path(home)` | `~/Library/Logs/eve-refresh-reminder.log` |
| `app_expiry` | `app_expiry(app, run) -> io::Result<Option<DateTime<Utc>>>` | `security cms -D -i <profile>`; non-zero exit, unparsable plist, non-dict or missing/non-date `ExpirationDate` → None; a `run` that cannot start the program is `Err`, as `subprocess.run` raises. `run` is the seam the Python test passes; `system_run` is the real one |
| `google_expiry` | `google_expiry(token_path) -> anyhow::Result<Option<DateTime<Utc>>>` | `float(signed_in_at or 0)` with Python's falsy values and `float(str)` parsing; OSError/ValueError → None; AttributeError (top level not an object), TypeError (list/dict stamp), out-of-range timestamp → `Err` as Python raises; `fromtimestamp` rounds half-even to the microsecond |
| `_when` | `when(expiry, now)` (local zone), `when_in(expiry, now, tz)` | `expired %a %b %-d` / `expires %a %b %-d, %-I:%M %p (today\|in 1 day\|in N days)` / `date unknown` |
| `message` | `message(now, app, google)`, `message_in(..., tz)` | Byte for byte against the Python output (TZ=UTC golden) |
| `current` | `current(token_path, now) -> anyhow::Result<String>` | App date first, then Google, as Python evaluates them |
| `remind` | `remind(text) -> io::Result<bool>`; `remind_with(text, run, cfg, token, post)` | osascript `display notification <json.dumps(text)> with title "eve: refresh"` (ensure_ascii escaping, `mac_notification_argv`); then `cfg.enabled && notify::post_once(read_token(), text, cfg.base)`: the token is read only when enabled |
| `plist_path` | `plist_path(&Launchd)` | `Launchd::agent_path(LABEL)` |
| `build_plist` | `build_plist(&Launchd)`, `program_arguments(&Launch)` | Byte for byte against Python's `plistlib.dumps` (golden test); no `RunAtLoad` |
| `schedule` | `schedule(&Launchd) -> anyhow::Result<i32>` | No launchctl → stderr line, 2; mkdir, `service_launchd::write_plist`, `Launchd::load`; "Scheduled: every Friday at 09:00 (<path>)." on 0 |
| `unschedule` | `unschedule(&Launchd) -> anyhow::Result<i32>` | "Not scheduled; nothing to do." / `launchctl unload` then unlink, "Removed: <path>" |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `tests/test_refresh.py` | `test_app_expiry_reads_the_profile`, `test_app_expiry_none_without_a_build`, `test_google_expiry_is_seven_days_after_sign_in`, `test_google_expiry_unknown_without_a_stamp_or_file`, `test_message_names_both_steps_and_how_long_is_left`, `test_plist_runs_the_venv_python_weekly` | same names in `src/tests.rs`, plus `app_expiry_reads_the_real_xml_security_prints`, `app_expiry_unknown_when_security_fails_or_prints_garbage`, `google_expiry_reads_the_stamp_as_python_float_does`, `google_expiry_raises_where_python_does`, `message_matches_python_byte_for_byte`, `mac_notification_quotes_as_json_dumps`, `remind_shows_on_the_mac_and_posts_when_enabled`, `plist_runs_the_rust_binary_natively`, `schedule_writes_and_loads_the_agent_then_unschedule_removes_it`, `schedule_without_launchctl_or_with_a_failed_load_is_2`, `app_is_the_last_device_build_in_this_checkout` | None |

## Boundaries
- `security cms -D` (reads the signing profile) and `osascript` (the Mac notification) are subprocesses, as in Python.
- `launchctl` through `eve_service::Host`.

## Differences
- `ProgramArguments` follows `Launchd::launch`: with `Launch::Python(exe)` it is Python's line byte for byte; the Rust daemon (`Launch::Native`) writes `[<eve-bridge>, "refresh", "remind"]`, as eve-service does for every unit.
- `LOG` is read from the `Launchd`'s HOME when the plist is built rather than fixed at import (the same path unless HOME changes mid-process).
- `EVE` is the checkout the crate was compiled from (`CARGO_MANIFEST_DIR/../../..`) rather than the one a module file was imported from.
- A token file holding a bare `NaN`/`Infinity` literal is invalid JSON here (None); Python's `json.loads` accepts it.
- Plist dates are parsed by the `plist` crate, which wants a full `YYYY-MM-DDTHH:MM:SSZ`; plistlib also accepts truncated dates. Signing profiles always carry the full form.
