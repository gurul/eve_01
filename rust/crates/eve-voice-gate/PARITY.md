# eve-voice-gate
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/voice_gate.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `log` | `tracing` | Same messages: `voice gate: EVE_VOICE_GATE=… is not one of off\|shadow\|on; off`, `voice gate[mode]: … score=… cut=… (why, enrolled … s)`, `voice gate: … — open for the rest of this conversation`, `voice gate: N s of speech rejected and none accepted — open for this conversation`, `voice gate[mode]: enrolled … s from the wake` / `no usable wake voice — open`, `voice gate: off for this conversation (Kind: message)` |
| `SAMPLE_RATE`, `BLOCK_SECS`, `MODES`, `DEFAULT_MODE`, `DEFAULT_MODEL`, `MODEL_URL` | same names | Values verbatim (24000, 0.1, off/shadow/on, off, `~/.config/eve-bridge/models/nemo_en_titanet_small.onnx`, the sherpa-onnx release URL with its `recongition` spelling) |
| (`"EVE_VOICE_GATE"`, `"EVE_VOICE_GATE_MODEL"` literals) | `MODE_VAR`, `MODEL_VAR` | |
| `GateConfig` | `GateConfig` (`Default`, `with_mode`) | Every field and default verbatim; `mode`/`model` are `String` as in Python (any string may be set directly) |
| `configured` | `configured()` (process env), `configured_from(lookup)` | Python's `str.strip()` (U+001C..U+001F included) and `lower()`; empty mode → `off`; unknown mode → warning with Python's `repr` and `off`; model: unset/empty → default, then stripped, empty → default |
| `model_present` | `model_present(&GateConfig)` | `os.path.expanduser` (HOME, else the password database; `~user`) then `isfile` |
| `Embedder` (Protocol) | trait `Embedder: Send` | `embed(&mut self, &[u8]) -> Result<Option<Vec<f64>>, GateError>`: `Ok(None)` is Python's `None`, `Err` is an exception (class name + message) |
| `SherpaEmbedder` | `SherpaEmbedder` | The official k2-fsa `sherpa-onnx` crate at 1.13.8, the release of the Python wheel the daemon runs; static link. Missing model: `FileNotFoundError` with Python's message; fewer than 4800 samples or not ready: `None`; one thread, CPU |
| `rms` | `rms(&[u8]) -> f64` | Native-endian int16, integer sum of squares, a trailing odd byte ignored |
| `cosine` | `cosine(&[f64], &[f64]) -> Result<f64, GateError>` | CPython 3.12+ compensated `sum()`; different lengths are `zip(strict=True)`'s `ValueError` with its message |
| `_unit` | private `unit` | Zero vector returned as is |
| `trim_to_speech` | `trim_to_speech(&[u8], f64, usize) -> Vec<u8>` | |
| `Decision` | `Decision` | `secs` rounded to 2, `score` to 3 (Python's `round`) |
| `Stats` | `Stats` | |
| `SpeakerGate` | `SpeakerGate` | `new(config, embedder)` (time.monotonic, 4800-byte blocks) and `with_clock(config, embedder, clock, block_bytes)`; pub `config`, `embedder`, `bypass`, `lenient`, `stats` as in Python |
| `SpeakerGate.enrol_seed` | `enrol_seed(&[u8]) -> Result<bool, GateError>` | An embedder error propagates, as in Python (caught by `build`) |
| `SpeakerGate.state` (property) | `state() -> &'static str` | `open` / `provisional` / `locked` |
| `SpeakerGate.reset` | `reset()` | |
| `SpeakerGate.process` | `process(&[u8]) -> Vec<Vec<u8>>` | Held blocks released when judging stops (not in shadow); an error opens the gate for the conversation, `stats.opened = "error: <Kind>"`, held audio forwarded with the current block exactly once (Python's `self._held[-1] is block`, tracked with a flag) |
| `SpeakerGate._shadow` | private `shadow` | |
| `SpeakerGate._process` | private `process_inner` | |
| `SpeakerGate._count` | private `count` | |
| `SpeakerGate._end_segment` | private `end_segment` | |
| `SpeakerGate._embed` | private `embed` | |
| `SpeakerGate._judge` | private `judge` | Same branches and `why` values |
| `SpeakerGate._rejudge` | private `rejudge` | Window and re-score tails use Python's `seq[-int(x / BLOCK_SECS):]` slicing (0 is the whole segment) |
| `SpeakerGate._adapt` | private `adapt` | The `assert` is `GateError("AssertionError")`; length mismatch is `zip(strict=True)`'s ValueError |
| `SpeakerGate._decide` | private `decide` | |
| `SpeakerGate.summary` | `summary() -> serde_json::Value` | Same keys in the same order; floats rounded to 1 |
| `build` | `build(config, wake_audio, clock, embedder) -> Option<SpeakerGate>`, `build_default(config, wake_audio)` | `embedder` None loads `SherpaEmbedder`; never fails |
| `_RING_SECS` | private `RING_SECS` | 3.0 |
| `WakeRing` | `WakeRing` (`new(secs)`, `Default` = 3 s, `add`, `snapshot`, `clear`) | `maxlen = max(1, round(secs / BLOCK_SECS))`, ties to even |
| (`time.monotonic`) | `monotonic()` | Seconds since first use; only differences are used |
| (exceptions) | `GateError { kind, message }` | `kind` is the Python class name that reaches the log and `stats.opened` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_voice_gate.py` | 21 | 20, same names | `test_the_session_appends_what_the_gate_lets_through` drives `voice_agent`'s session (`_pump_mic`, `FakeConnection` from `test_voice_agent.py`): it belongs to the crate that ports `voice_agent` (brain) |
| (parity) | | 13 more: `configured_strips_lowers_and_falls_back_like_python`, `defaults_match_the_python_dataclass`, `rms_and_cosine_match_python`, `sums_are_compensated_like_python_3_12`, `rounding_and_slicing_match_python`, `repr_is_pythons`, `expanduser_reads_home`, `the_summary_has_pythons_keys_in_order`, `a_decision_records_rounded_numbers`, `a_takeover_scenario_matches_python_number_for_number`, `an_error_mid_hold_forwards_the_held_block_once`, `a_missing_model_means_no_gate`, `the_sherpa_embedder_embeds_two_seconds` (`#[ignore = "live: …"]`, needs the model file; checks 192 numbers against the Python wheel's output) | Expected values printed by Python 3.13 / sherpa-onnx 1.13.8 |

## Boundaries
None. sherpa-onnx is linked natively through its official Rust crate (static; the build downloads k2-fsa's release archive, or uses `SHERPA_ONNX_LIB_DIR` / `SHERPA_ONNX_ARCHIVE_DIR`).

## Differences
- `sherpa-rs` 0.6.8 (sherpa-onnx 1.12.9) was tried and rejected: its NeMo feature normalisation computes the variance as E[x²]−E[x]², which goes negative and returns an all-NaN TitaNet embedding for 24 kHz input; 1.13.x (the Python wheel's release) fixed it. The official crate is used instead.
- The Rust and Python embeddings of the same audio agree to about 1e-7 (onnxruntime builds differ), not bit for bit.
- `WakeRing::new` with a NaN or infinite length makes a one-block ring where Python raises.
- `py_repr` in the unknown-mode warning escapes ASCII control characters and C1 controls as Python does, but leaves other non-printable Unicode as is (log text only).
- The Python `clock` and `embedder` are attributes; in Rust the clock is a `Box<dyn Fn() -> f64 + Send>` given at construction and the embedder is the pub `embedder` field.
