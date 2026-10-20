# eve-music-mode
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/music_mode.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `VOLUME_PER_DETENT`, `VOLUME_DEBOUNCE_SECS`, `PICKER_IDLE_SECS`, `PICKER_SAY_SECS` | same-named `pub const` | Values verbatim (4, 0.4, 10.0, 0.35) |
| `Send`, `Say` type aliases | `SendFn`, `SayFn` | `Arc<dyn Fn(..) -> BoxFuture<anyhow::Result<()>>>`; the injectable sleep is `SleepFn` (default `tokio_sleep()`) |
| (duck-typed `spotify.Spotify`) | trait `SpotifyControl` | `now_playing()` and `control(action, kw)`, free-form `serde_json::Value` dicts as in Python; `kw` is the `**kw` map (`volume`, `device`) |
| `_rgb` | `rgb` (pub) | `"#rrggbb"` → `[r, g, b]`, Spotify green `[30, 215, 96]` otherwise; each pair parsed with Python's `int(s, 16)` rules (whitespace, sign, `0x`, underscores) |
| `MusicMode` | `MusicMode` (cheap `Clone` handle over `Arc`) | Constructor `new(spotify, send, say)` / `with_sleep(.., sleep)`; attributes `on`, `volume`, `picker`, `index` are accessor methods |
| `MusicMode.set` | `MusicMode::set` → `anyhow::Result<String>` | Same lines verbatim; a reported volume is read as Python's `isinstance(vol, int)` (a bool is 0/1); errors from `send`/`now_playing` propagate as Python's exceptions do |
| `MusicMode.on_input` | `MusicMode::on_input(&Value)` | Same branch order: off → put the board back; long hold → off + green sweep; picker; short hold → picker; dial → volume; 1/2/3 clicks |
| `MusicMode._flash` | `flash` (private) | Send errors swallowed (`contextlib.suppress`) |
| `MusicMode._dial_volume` | `dial_volume` (private) | Shadow volume, clamp 0..100, `ring_level` at once, debounced write |
| `MusicMode._write_volume` | `write_volume` (private) | After `VOLUME_DEBOUNCE_SECS`; a refusal clears the shadow and blinks red |
| `MusicMode._open_picker` | `open_picker` (private) | Restricted devices dropped; starts on the active one; none → red blink + "No Spotify device is online." |
| `MusicMode._picker_input` | `picker_input` (private) | Dial wraps (Python `%`, via `rem_euclid`); short hold backs out; one click transfers unless already active |
| `MusicMode._show_pick` | `show_pick` (private) | `{"cmd":"ring_level","n","of","dot":true,"ms":10000,"rgb"}` in Python's key order; name said after `PICKER_SAY_SECS` |
| `MusicMode._say_later` | `say_later` (private) | A newer line cancels one not yet said; say errors swallowed |
| `MusicMode._arm_picker_timer` | `arm_picker_timer` (private) | `PICKER_IDLE_SECS` idle → close the picker |
| `MusicMode._close_picker` | `close_picker` (private) | Clears the dot with `{"cmd":"ring_level","n":0,"of":1,"ms":0}`, send errors swallowed |
| `MusicMode._spawn` | `spawn` (private) | `tokio::spawn`, handles kept for `close` |
| `MusicMode.close` | `MusicMode::close` | Aborts every task and awaits them |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_music_mode.py` | `test_turning_it_on_tells_the_board_and_reads_the_volume`, `test_one_click_toggles_two_skip_three_go_back`, `test_the_dial_is_the_volume_shown_at_once_written_once`, `test_the_volume_is_clamped`, `test_hold_opens_the_picker_dial_moves_click_plays_there`, `test_in_the_picker_a_click_on_the_playing_device_or_a_hold_changes_nothing`, `test_the_picker_times_out`, `test_a_long_hold_leaves_the_mode`, `test_a_refusal_blinks_red`, `test_no_device_online_blinks_and_says_so`, `test_input_while_off_puts_the_board_back`, `test_without_a_voice_the_picker_is_the_ring_alone`, `test_a_device_without_a_colour_is_green` | same names in `src/tests.rs`, plus `rgb_parses_like_python_int_base_16`, `clicks_compare_like_python`, `a_boolean_or_zero_dial_is_not_a_turn`, `the_picker_dial_wraps_both_ways`, `a_refused_volume_write_reads_the_volume_again`, `a_picked_device_says_the_refusal`, `the_real_timer_debounces_one_write`, `a_boolean_volume_is_python_s_int` | - |
| `bridge/tests/test_music_mode.py` | `test_spotify_mode_by_words_and_by_the_tool` | - | Tests `spotify.py`'s `match`/`run`/`handle` (crate `eve-spotify`), which drive this crate's `set` |
| `bridge/tests/test_music_mode.py` | `test_controller_show_keeps_the_mode_only` | - | Tests `controller.py`'s `show`/`_replay` (crate `eve-controller`) |

## Boundaries
None

## Differences
- Concurrency: Python runs every task on one event loop; Rust tasks may run on several worker threads. Each
  cancellable task (debounced volume write, picker timer, pending name) carries a generation number stored in its
  slot before the task can run, and acts only while that slot is still its own, so one that already woke when a
  newer one replaced it (or the picker closed) stands down rather than acting; the observable behavior is
  Python's. The idle timer closes the picker under the same lock that checks its slot.
- An exception inside the debounced volume write was an unretrieved task exception in Python; Rust logs it
  (`music mode: volume write failed`) and, as in Python, leaves the shadow and the ring alone.
- `rgb` parses each pair with ASCII hex digits; Python's `int(s, 16)` also takes other Unicode decimal digits
  (`"#٠٠ff00"`), which no device colour from `spotify.DeviceColors` contains.
