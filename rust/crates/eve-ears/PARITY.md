# eve-ears
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/ears.py` | `src/lib.rs` (tests in `src/tests.rs`) |
| (sounddevice, the library `ears.py` drives) | `src/audio.rs`: `AudioBackend` seam, `CpalBackend` (CoreAudio through cpal 0.18.2) |
| (sentencepiece, which `encode_phrase` calls) | `src/sentencepiece.rs`: model loading and encoding, reimplemented |
| (Python built-ins whose output reaches files and logs) | `src/py.rs`: `str.strip`, `float()`, `repr()` (escapes from `str.isprintable`'s Unicode 15.1 table), `format(x, "g")`, `str(float)`, `os.path.expanduser`, `pathlib.Path` normalisation |

## API
| Python | Rust | Notes |
|---|---|---|
| `log` | `tracing` | Same messages, word for word, except the sounddevice import warning (see Differences) |
| `DEFAULT_WAKE_WORD`, `DEFAULT_LESSON_WORD`, `DEFAULT_LESSON_THRESHOLD`, `DEFAULT_MODEL_DIR`, `MODEL_URL`, `SAMPLE_RATE`, `BLOCK_SECS`, `DEFAULT_COOLDOWN_SECS`, `DEFAULT_BOOST`, `DEFAULT_THRESHOLD`, `SILENCE_RMS`, `SILENCE_WARN_SECS` | same names | Values verbatim; `block_frames()` is `int(SAMPLE_RATE * BLOCK_SECS)` (2400) |
| `EarsConfig` | `EarsConfig` (`Default`) | Every field and default; `model_dir` is `Path(DEFAULT_MODEL_DIR).expanduser()` |
| `configured` | `configured()` (process env), `configured_from(lookup)` | `env.get(k) or ""` (empty = unset) for every knob except `EVE_LESSON_WORD`, where only an unset variable takes the default, as in Python; `str.strip` (U+001C..U+001F included), `lower`, `float()` (underscores, Unicode digits, inf/nan); warning `ears: EVE_WAKE_THRESHOLD='hot' is not a number; using 0.25` with Python's `repr` and `str(float)`; `EVE_KWS_MODEL_DIR` through pathlib + `expanduser` (HOME, else the password database; `~user`) |
| `keyword_id` | `keyword_id` | Same regex over the stripped, lower-cased phrase; `"wake"` when empty |
| `keyword_line` | `keyword_line(&[String], phrase, boost, threshold)` | `{:g}` is Python's `format(x, "g")` |
| `encode_phrase` | `encode_phrase(phrase, bpe_model) -> Result<Vec<String>>` | `sentencepiece` reimplemented in Rust (`src/sentencepiece.rs`); checked piece for piece against the Python wheel on the KWS `bpe.model` (nmt_nfkc normaliser, unigram) and on two small BPE / unigram fixtures in `tests/data` |
| `write_keywords_file` | `write_keywords_file(config, path)`, `write_keywords_file_with(config, path, encode)` | The `_with` form is the seam the Python test gets by patching `encode_phrase`; the default file is byte-identical to the Python one |
| `model_present` | `model_present(&Path)` | |
| `WakeSpotter` | `WakeSpotter::new(config, keywords_file)` + trait `Spotter` | The k2-fsa `sherpa-onnx` crate 1.13.8 (the wheel's release, linked statically as in eve-voice-gate) with the same config: 1 thread, CPU, `max_active_paths=4`, `keywords_score=1.0`, `keywords_threshold`, `num_trailing_blanks=1`, 16 kHz / 80-dim features; `pub keywords_file`; the missing-model message verbatim. `Spotter` is the duck type the Python tests pass as `spotter=` |
| `WakeSpotter.feed` | `Spotter::feed(&mut self, &[f32], u32) -> Option<String>` | `get_result(...).keyword.strip()`, empty = no hit, `reset` after a hit |
| `WakeGate` | `WakeGate` (`Default`, pub fields) | `suppressed` is `Arc<dyn Fn() -> bool>` |
| `WakeGate.consider` | `consider(Option<&str>, now) -> Option<String>` | |
| `rms_int16` | `rms_int16(&[i16]) -> f64` | numpy's float32 square and pairwise sum reproduced, so the value is bit-identical to `np.mean(a * a)` (tested) |
| `Ears` | `Ears::new(config, on_wake, tokio Handle)` + `with_suppressed`, `with_spotter`, `with_clock`, `with_backend` | `loop.call_soon_threadsafe(on_wake, hit)` is `handle.spawn`; the clock defaults to `eve_voice_gate::monotonic`; the wake ring is `eve_voice_gate::WakeRing` |
| `Ears.subscribe` / `Ears.unsubscribe` | `subscribe() -> PcmQueue`, `unsubscribe(&PcmQueue)` | `PcmQueue` is `asyncio.Queue(maxsize=100)`: `get().await`, `get_nowait`, `put_nowait`, `qsize`, `full`, `empty`; identity comparison as `in self._subs` |
| `Ears.muted` (property) | `muted()` | |
| `Ears._on_block` | `on_block(&[i16], Option<f64>) -> Option<String>` | Same order: rms, silence watch, fan-out (muted), ring, spotter, gate, wake audio snapshot |
| `Ears._watch_silence` | private `Core::watch_silence` | Same warning text; `sys.executable` is `current_exe()` |
| `Ears.listening` (property) | `listening()` | |
| `Ears.prepare` | `prepare()` | |
| `Ears.start` | `start()` | Device lookup, the callback (a panic is logged as `ears: block failed`), `open_input(..., "ears")`, the `listening for 'hey eve' on <device> (24000 Hz, threshold 0.25)` line |
| `Ears.stop` | `stop()` | |
| `Ears.last_wake_audio`, `last_rms`, `gate`, `_silent_since`, `_silence_warned` (attributes) | `last_wake_audio()`, `take_last_wake_audio()`, `set_last_wake_audio()`, `last_rms()`, `gate_counts()`, `silent_since()`, `silence_warned()`, pub `config` | `take_last_wake_audio` is daemon.py's read-then-clear |
| `_offer` | private `offer` | Drop the oldest block when full |
| `open_input` | `open_input(backend, make, what)` | Same retry, same warnings (`<what>: could not open the microphone (...); refreshing the audio device list`, `<what>: audio device refresh failed (...)`, `<what>: microphone opened after the refresh`); public for desk_call |
| `input_device_name` | `input_device_name(backend, device)` | `"?"` when unreadable |
| `_find_input_device` | `find_input_device(backend, needle)` | Index into `query_devices()`, inputs only, case-insensitive substring; public for desk_call |
| `diagnose` | `diagnose(seconds) -> i32`, `diagnose_with(cfg, backend, seconds)` | Same lines (`present=True`, Python `repr`s), same exit codes 0 / 1 / 2 |
| (sounddevice) | `AudioBackend`, `InputStream`, `InputSpec`, `DeviceInfo`, `BlockCallback`, `StatusCallback`, `CpalBackend`, `audio::float_to_int16` | See Boundaries |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_ears.py` | 18 | 18, same names | None. The three real-model tests run when the KWS model, `say` and `afconvert` are present (Python's `skipif`) and print `skipped` otherwise. The two Python `xfail(strict=True)` tests assert the known gap (the spotter does not hear "hey eve"), so they turn red on the day it does, as a strict xfail would |
| `bridge/tests/test_mic.py` (tests_ref) | 4 that test `ears` | `test_mic_always_is_off_by_default_and_reads_the_env`, `test_prepare_loads_nothing_into_the_mic`, `test_start_is_idempotent_and_stop_closes`, `test_stop_resets_the_silence_watch` | The rest test the daemon's mic policy and `cli.describe_mic`: they belong to the daemon and cli crates |
| (parity) | | 19 more: `open_input_refreshes_the_device_table_and_retries_once`, `open_input_gives_up_after_the_second_failure`, `find_input_device_matches_inputs_by_substring_any_case`, `start_logs_the_wake_word_device_and_threshold_and_warns_on_an_unknown_device`, `the_wake_audio_is_the_ring_before_the_wake_and_is_taken_once`, `pcm_queue_get_waits_for_a_block`, `rms_matches_numpy_float32_to_the_last_bit`, `sentencepiece_bpe_matches_the_cpp_library`, `sentencepiece_unigram_matches_the_cpp_library`, `the_kws_bpe_model_encodes_like_the_python_wheel`, `the_default_keywords_file_matches_the_python_one`, `python_formatting_helpers_match`, `the_model_dir_expands_home_like_pathlib`, `configured_reads_every_knob_like_python`, `the_lesson_word_is_left_out_when_it_is_the_wake_word`, `a_missing_model_is_a_clean_error`, `the_converter_cuts_exact_blocks_from_any_device_format`, `the_ears_can_be_shared_across_threads`, and `the_real_microphone_delivers_24k_blocks` (`#[ignore = "live: ..."]`, opens the Mac's microphone; passed on 2026-10-19) | Expected values printed by Python 3.13, numpy 2.5.3, sentencepiece 0.2.2 |

## Boundaries
- **Microphone**: sounddevice/PortAudio becomes cpal/CoreAudio. PortAudio converts the device's format to the requested 24 kHz mono int16 itself; cpal does not, and asking CoreAudio for 24 kHz would change the device's nominal rate under every other app. So the stream opens at the device's default format and `audio.rs` converts: the first channel, resampled with sherpa-onnx's `LinearResampler`, scaled like PortAudio's `Float32_To_Int16_Clip` (`x * 32767`, truncated, clipped; sounddevice leaves PortAudio's dither on, so PortAudio itself would use `Float32_To_Int16_DitherClip`, which adds under one LSB of triangular noise), and cut into blocks of exactly the requested size. A device that already delivers 24 kHz int16 is passed through untouched. The cpal stream lives on its own thread (`eve-mic`), since it is not `Send` everywhere.
- **Device table**: `CpalBackend` lists the devices once and again on `refresh()`, as PortAudio does, so `open_input`'s refresh-and-retry has the same meaning and a device index stays valid until a refresh. The default input is read live.
- **sentencepiece**: no cmake on the Mac to build the C++ binding crate, so the parts eve uses are reimplemented from the C++ sources (protobuf `ModelProto`, precompiled-charsmap normaliser via the darts-clone double array, unigram `EncodeOptimized`, BPE merge with unused-piece resegmentation, char model, unknown-run merge, byte fallback). The word model type is refused with an error.

## Differences
- The import warning reads `ears: audio input not available (<reason>) — no wake word`, not `sounddevice not importable (...) (pip install sounddevice)`: there is no Python package to install. `diagnose` prints `audio input:    NOT AVAILABLE` for the same case.
- Captured samples are not bit-identical to PortAudio's when the device runs at another rate (different resampler, no dither); the spotter and the voice session see the same 24 kHz int16 stream shape.
- `diagnose`: where Python would end in a traceback (the model fails to load, the stream fails to open, no default input device), Rust prints `ears-check: <error>` to stderr and returns 1, or prints `input device:   ?`.
- I/O error text inside log messages is Rust's (`could not write <path>: Permission denied (os error 13)`) rather than Python's `[Errno 13] Permission denied: '<path>'`.
- `PcmQueue` puts land immediately rather than on the loop's next turn (`call_soon_threadsafe`); order and the drop-oldest rule are the same.
- `suppressed` runs with the ears' lock held, so it must not call back into the same `Ears`.
- `EVE_KWS_MODEL_DIR=~someone/...` naming a user the password database does not know: pathlib's `expanduser` raises `RuntimeError("Could not determine home directory.")` out of `configured()`; Rust keeps the path as written (`configured` cannot fail), so the model is reported missing under that path instead.
- `start()` with `EVE_MIC` set: when listing the devices fails, Python's `_find_input_device` raises out of `start()`; Rust treats it as no match (the "no input device matching ...; using the default" warning) and opens the default input.
- `start()` holds the stream slot for the whole call, so concurrent `start()`s from two threads open one stream; `listening()` waits for a `start()` in progress.
