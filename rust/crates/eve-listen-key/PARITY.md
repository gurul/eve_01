# eve-listen-key
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/listen_key.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEBOUNCE_SECS` | `DEBOUNCE_SECS: f64` | 0.15 |
| `LISTEN_KEYS` | `LISTEN_KEYS: &[(&str, u64)]` + `listen_key_mask(name)` | same order: option → `FLAG_ALTERNATE`, fn → `FLAG_SECONDARY_FN`, off → 0 (masks from `eve-key-tap`) |
| `Stopper` | `type Stopper = Box<dyn Fn() + Send + Sync>` | callable more than once, like the Python closure |
| `default_listen_key` | `default_listen_key()` + `default_listen_key_for(platform)` | "option" when `sys.platform == "darwin"`, else "off"; `platform()` gives the build's `sys.platform` value |
| `configured_listen_key` | `configured_listen_key()` + `configured_listen_key_from(value, platform)` | `EVE_LISTEN_KEY or default` (empty is falsy), stripped with Python's `str.isspace` set, lowercased; unknown → warning `listen key: unknown EVE_LISTEN_KEY=%r; using %s` (`%r` by `py_repr`) and the platform default |
| `class ListenKeyTracker` | `struct ListenKeyTracker` | `key`, `mask`, `debounce` public; `_down`, `_reported_at` (-inf), `_pending` private |
| `ListenKeyTracker.__init__` | `ListenKeyTracker::new(key, debounce) -> Result<_, UnknownListenKey>`, `for_key(key)`, `configured()` / `Default` | `key: None` reads the env; the key is matched exactly; error text `unknown listen key 'x'` (Python's `ValueError`) |
| `ListenKeyTracker.enabled` | `enabled()` | mask != 0 |
| `ListenKeyTracker.down` | `down()` | |
| `ListenKeyTracker.feed` | `feed(flags: u64, now: f64) -> Option<bool>` | |
| `ListenKeyTracker.flush` | `flush(now)` | |
| `ListenKeyTracker.due_in` | `due_in(now)` | `max(0, debounce - (now - reported_at))` |
| `ListenKeyTracker._settle` | `settle` (private) | same branch order |
| `start_listen_key` | `start_listen_key(on_change, &Handle, tracker)` + `start_listen_key_on(..., platform)` | order of checks and log lines kept: disabled (info) → not darwin (warning) → tap; Python's inner `_feed` / `_flush` are `drive_tracker`, a task on the tokio runtime fed by an unbounded channel from the tap thread; the tap thread (`listen-key-tap`) runs `CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, ListenOnly, CGEventMaskBit(kCGEventFlagsChanged))`, re-enables on `TapDisabledByTimeout`/`TapDisabledByUserInput`, waits at most 5 s for readiness; tap NULL → the Input Monitoring warning verbatim; setup failure → `listen key: tap setup failed` (error) then the warning; success → `listen key: watching %s (EVE_LISTEN_KEY)`; the stopper does `CGEventTapEnable(false)` + `CFRunLoopStop` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_listen_key.py` | `test_press_then_release_reports_both_edges`, `test_same_state_is_never_reported_twice`, `test_other_modifiers_are_ignored`, `test_fn_key_uses_secondary_fn_mask`, `test_release_inside_window_is_held_then_flushed`, `test_chatter_that_settles_back_is_dropped`, `test_at_most_one_edge_per_window`, `test_edge_after_window_passes_immediately`, `test_env_selects_fn`, `test_env_default_is_option_on_darwin`, `test_env_unknown_falls_back_to_default`, `test_off_mode_reports_nothing`, `test_start_returns_none_when_off`, `test_start_returns_none_off_darwin` | same names in `src/tests.rs`, plus `empty_env_value_means_the_default`, `whitespace_only_env_value_is_unknown_and_falls_back`, `unknown_key_off_darwin_falls_back_to_off`, `tracker_rejects_unknown_key_with_python_message`, `listen_keys_table_matches_python`, `default_listen_key_follows_the_build`, `first_edge_always_passes_and_due_in_none_when_idle`, `due_in_never_negative`, `custom_debounce_is_used`, `py_repr_matches_python`, `driver_emits_edges_and_flushes_a_held_release`, `driver_drops_chatter_that_settles_back`, `driver_releases_pending_edge_after_the_sender_closes`, `driver_bounce_storm_ends_in_the_settled_state`, and `live_tap_starts_and_stops` (`#[ignore = "live: ..."]`; passed on this Mac) | `test_daemon_sends_once_per_transition`, `test_daemon_sends_nothing_while_disconnected`, `test_daemon_reset_on_connect_sends_off_and_rearms`: they drive `Daemon._on_listen_key` / `Daemon._reset_listen`, which belong to the daemon crate. Env monkeypatching became value parameters (`configured_listen_key_from`, `start_listen_key_on`) |

## Boundaries
None. The tap is Quartz through `objc2-core-graphics` / `objc2-core-foundation` on macOS; off macOS the platform check returns `None` before any tap code, as in Python.

## Differences
- Python's "pyobjc-framework-Quartz not installed" path has no Rust equivalent: Quartz is linked on macOS builds.
- The Input Monitoring warning names the Rust daemon's binary (`current_exe`, canonicalized) where Python names `os.path.realpath(sys.executable)`.
- Python arms one `call_later` per held edge; Rust keeps one deadline (always `reported_at + debounce`, the same instant every Python timer targets) and re-arms if a wake lands a hair early from float rounding, so a held edge cannot be stranded.
- After the stopper runs, a still-pending edge is released at the end of its window (Python's armed `call_later` fires the same way), and then the loop task ends; Python leaves nothing running either.
- If the tap thread is still setting up when the 5 s readiness wait ends, both return `None` with the Input Monitoring warning. Python's late tap would still marshal flags to the loop and call `on_change`; in Rust the receiver is already dropped, so a late tap's flags go nowhere and the watcher really is off, as the log says.
- `py_repr` follows `str.isprintable` for control, separator, private-use, noncharacter and the common format characters; unassigned code points print as-is (log text only).
