# eve-sound
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/sound.py` | `crates/eve-sound/src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `default_path(environ=None, name="sound")` | `default_path(environ: Option<&HashMap<String, String>>, name: &str) -> PathBuf` | `EVE_<NAME>_FILE` (stripped, `~` expanded), else `$HOME/.config/eve-bridge/<name>.json` |
| `build_sound_cmd(on)` | `build_sound_cmd(on: bool) -> Value` | `{"cmd": "sound", "on": on}` |
| `quiet_caption(msg, muted)` | `quiet_caption(&Map, muted) -> Cow<Map>` | `Cow::Borrowed` is Python's "same object"; owned copy keeps key order; `chirp` tested with Python truthiness |
| `SoundSetting` | `SoundSetting { name, path, on }` | `SoundSetting(path, name)` is `SoundSetting::new(Option<PathBuf>, &str)`; `SoundSetting::at(path)` is the `sound` default |
| `SoundSetting.muted` (property) | `SoundSetting::muted()` | |
| `SoundSetting.load` | `SoundSetting::load(&mut self) -> bool` | missing file: on, silent; unreadable / bad JSON / non-object: warn `"<name>: cannot read <path> (<err>) — <name> stays on"`, on; `muted` read with Python truthiness |
| `SoundSetting.set` | `SoundSetting::set(&mut self, on) -> bool` | mkdir -p, write `<stem>.tmp` with `{"muted": <bool>}\n` (Python `json.dumps` spacing), rename over; on failure warn `"... — the choice lasts until the daemon restarts"` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_sound.py` | `test_missing_file_means_sound_on`, `test_mute_persists_across_instances`, `test_corrupt_file_keeps_sound_on`, `test_non_object_json_does_not_crash_microphone_or_sound_startup`, `test_wire_helpers`, `test_default_path_env_override` | same names in `src/tests.rs` (caplog becomes a captured `tracing` subscriber) | none |
| `bridge/tests/test_daemon_scene.py` (ref) | `test_sound_is_sent_on_connect_and_mute_persists` (file part) | `file_format_matches_python_json_dumps` | the BLE/IPC parts belong to the daemon crate |
| `bridge/tests/test_mic.py` (ref) | uses `SoundSetting(path, name="mic")` | `default_path_names_the_switch_and_strips_and_expands`, the `mic` loop in `test_non_object_json_...` | the microphone itself belongs to the ears crate |
| (new) | | `quiet_caption_follows_python_truthiness`, `muted_value_uses_python_truthiness`, `unwritable_path_keeps_the_choice_in_memory`, `directory_in_place_of_file_warns_and_stays_on` | |

## Boundaries
None. Unix-only (`std::os::unix` for byte paths and `getpwnam_r` for `~user`), as the daemon runs on macOS.

## Differences
- The `(<err>)` text inside the "cannot read" / "cannot save" warnings is Rust's io/serde_json error text, not Python's (`Expecting property name ...`). The non-object case keeps the exact text `expected a JSON object`.
- `json.loads` accepts `NaN`/`Infinity`; serde_json rejects them, so a hand-edited `{"muted": NaN}` reads as on (with a warning) instead of muted. The daemon never writes these.
- `EVE_<NAME>_FILE=~user/...` naming an unknown account: pathlib raises `RuntimeError`; Rust keeps the path as written.
- Log target is `eve_sound` (tracing's module path) rather than the Python logger name `eve_bridge.sound`.
