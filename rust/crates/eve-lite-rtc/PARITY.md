# eve-lite-rtc
Status: complete

## Modules
| Python | Rust |
|---|---|
| `lite_rtc.py` (constants, candidates, helpers) | `src/lib.rs` |
| `lite_rtc.py` (`_BadResponse`, `ChannelWriter`, `_Request`, `_request_id`, `_request_headers`, `LinkChannel`) | `src/link.rs` |
| `lite_rtc.py` (`VoiceTrack`, `media_track`, `InboundAudio`) | `src/voice.rs` |
| `lite_rtc.py` (`RtcCallSocket`) | `src/call.rs` |
| `lite_rtc.py` (`_Peer`, `RtcHub`) | `src/hub.rs` |
| aiortc's `RTCDataChannel` surface (what lite_rtc touches) | `src/channel.rs` (`Channel` trait, `WebrtcChannel` over webrtc-rs) |
| aiortc's Opus codec and PyAV's resampler (native libraries loaded on the first offer) | `src/opus.rs` (libopus through `dlopen`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `MAX_SDP_CHARS` … `MAX_CONTROL_CHARS`, `LINK_LABEL`, `CALL_LABEL`, `NO_AUDIO`, `LINK_PATHS`, `HEADER_NAME_RE`, `DROPPED_REQUEST_HEADERS`, `DROPPED_RESPONSE_HEADERS`, `SAMPLE_RATE`, `VOICE_FRAME_*`, `BLOCK_BYTES`, `PREROLL_BYTES`, `POSTROLL_SECS`, `MAX_PRESS_BYTES`, `MAX_VOICE_BYTES`, `MAX_LAG_SECS` | same names, `lib.rs` | Values verbatim; `SAMPLE_RATE`, `MAX_PRESS_SECS` and `BYTES_PER_SEC` come from `eve-phone-call`, `TURN_PATH`/`MESSAGES_PATH` from `eve-link`. |
| `class RtcError(ValueError)` | `RtcError` (thiserror) | Display is the message lite_web answers 400 with. |
| `available()` | `available()` | Whether libopus loads (webrtc-rs is compiled in). Logs `lite rtc: libopus does not load (OSError); the web app chats over HTTPS` on failure. |
| `configuration()` | `configuration()` | `RTCConfiguration` with `ice_servers == []`. |
| `peer_ipv4(addr)` | `peer_ipv4(&str) -> Option<String>` | strip, `[]`, `%zone`; IPv4-mapped unwrapped (`to_ipv4_mapped`, not the compat form); loopback, unspecified, multicast → None. |
| `rewrite_local_candidates(sdp, peer_addr)` | `rewrite_local_candidates(&str, &str) -> (String, usize)` | Same split on `\n`, CR kept, field 4 rewritten only for `typ host` `.local` names. |
| `reliable(channel)` | `reliable(&dyn Channel)` | ordered, no max packet lifetime, no max retransmits. |
| `_send(channel, data)` | `send(&dyn Channel, Data) -> bool` | Not open, or the stack refuses: false. |
| `_text(obj)` | `text(&Value)` (+ `py::dumps`) | `json.dumps(obj, separators=(",", ":"))` byte for byte, `ensure_ascii` included. |
| `class _BadResponse` | `link::BadResponse` (private) | Carried inside `ParseError::Bad`. |
| `class ChannelWriter` | `ChannelWriter` | `write`, `drain`, `is_closing`, `close`, `finish`, `fail`, `status`, `dead`/`set_dead`, `done`; also `AsyncWrite` (`poll_write` = write, `poll_flush` = drain, `poll_shutdown` = close) because LinkChat writes to a `DynWrite`. `wait_closed`/`get_extra_info` have no caller in Rust (the trait has no such calls). Parser states, limits (16 KiB head, 1 KiB chunk-size line), `int(line, 16)` rules and header folding as Python. |
| `class _Request` | `LinkRequest` | `id`, `reader` (`BodyReader`, the `asyncio.StreamReader`), `writer`, the task (`task_done()`). |
| `_request_id(raw)` | `link::request_id` (private) | JSON integer 0..=0xFFFFFFFF; bool, float, negative → None. |
| `_request_headers(raw)` | `link::request_headers` (private) | Lowercased, regex-checked names, CR/LF/NUL refused, dropped set removed, ≤ 32 headers. |
| `class LinkChannel` | `LinkChannel` | `check`, `send`, `drained`, `cancel`, `close`, `requests()` (ids, oldest first), `request(id)`, `closed()`; refusal bodies `json.dumps({"ok": False, "error": ...})` with Python's spacing; error texts verbatim. `LinkDoor` is the seam for `link.serve` (implemented for `LinkChat`). |
| `Marker` | `Marker` | `FnOnce() -> anyhow::Result<()>`; an error is logged, the voice goes on. |
| `class VoiceTrack` | `VoiceTrack` | `push`, `mark`, `flush`, `take`, `recv` (async, injectable clock and sleep as Python's keywords), `frame`, `queued()`, `dropped()`, `frames()`. `VoiceFrame` stands for `av.AudioFrame` (s16 mono, pts in 1/24000). |
| `media_track(voice)` | `media_track(Arc<VoiceTrack>) -> VoiceMediaTrack` | `kind()`, `ready_state()`, `recv()` (→ `MediaStreamError` once stopped), `stop()`. The peer encodes its frames with `VoiceEncoder` (libopus, 24 kHz mono, VOIP) onto a `TrackLocalStaticSample`. |
| `class InboundAudio` | `InboundAudio` | `convert(packet)`: one Opus packet → 24 kHz mono s16 PCM; a decoder error makes a fresh decoder (Python's fresh resampler on a format change). |
| `class RtcCallSocket` | `RtcCallSocket` (implements `eve_phone_call::CallSocket`) | `recv` (hello first, then the inbox; `Closed` at its end), `send_json` (`said` as a marker, `flush` empties the voice), `send_bytes`, `close`, `closed`, `audio`, `shut`, `talking()`, `same()`; pre-roll ring, 100 ms blocks, post-roll timer (a tokio task for `call_later`), MAX_PRESS_BYTES cap. |
| `class _Peer` | `Peer` | `check`, `start`, `_hear` (`hear`), `_deadline`, `_on_state`, `_on_channel`/`_channel` (`on_channel`), `_serve_call`, `_call_closed`, `close`; `voice_on()`/`set_voice_on()`, `connected()`, `closed()`, `pc()`, `link()`, `call()`. |
| `class RtcHub` | `RtcHub` | `voice_ready`, `stats` (same keys and order), `_served`, `answer` (→ `RtcAnswer { sdp, voice }`, `to_value()` is the JSON), `close_device`, `close`, `_forget`, `_retire`; `HubOptions { max_peers, connect_secs, udp_addrs, available }` for the keywords. `CallDoor` is the seam for PhoneCalls (`voice_ready`, `serve_socket`). |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `test_lite_rtc_loopback.py` | 11 (two parametrized) | `tests/loopback.rs`: 10 (the parametrized cases are loops) | `test_a_webrtc_session_under_the_lite_firewall_starts_no_program_and_opens_no_protected_folder`: it runs the session under `profile.install_firewall`, a CPython audit hook; Rust has no audit hook to install. In `test_the_peers_have_no_ice_server_at_all` the positive control (aiortc's own default adds Google's STUN) is aiortc's behavior and has no webrtc-rs counterpart; the live peer's configuration is checked instead. |
| `test_lite_link_channel.py` | 16 | `tests/link_channel.rs`: 16 | none. `turn.readers` (private to eve-link) is read through `LinkChat::deliver`'s answer instead. |
| `test_rtc_call_socket.py` | 13 | `tests/call_socket.rs`: 13 | none; the microphone arrives as Opus packets (see Differences). |
| `test_voice_track.py` | 6 | `tests/voice_track.rs`: 6 | The aiortc encoder test runs the libopus encoder the peer uses. |
| `test_lite_web_local.py` | (ref) | — | It drives lite_web.py with a FakeRtc; it belongs to eve-lite-web. |
| (none) | — | `src/lib.rs`: 3 (`py::dumps`, `py::int`, `py::compare_digest` against Python) | — |

## Boundaries
- **libopus** is opened at run time with `dlopen` (`opus::LIBRARY_PATHS`: the loader's search, then Homebrew's
  `/opt/homebrew` and `/usr/local` prefixes, then `libopus.so.0`). It stands where aiortc's own Opus binding and
  PyAV's native libraries were: a Mac without it starts, `available()` is false, and every offer is refused with
  `RtcError("WebRTC is not available on this Mac")` so the page chats over HTTPS. No subprocess.

## Differences
- **The microphone is decoded, not resampled.** aiortc decoded the page's Opus to 48 kHz stereo and PyAV resampled
  it to 24 kHz mono. Here libopus decodes each packet straight to 24 kHz mono (an Opus packet does not depend on the
  rate it is decoded at, and libopus downmixes stereo by averaging), which is the same PCM format and level.
  eve's voice is likewise encoded at 24 kHz mono instead of being resampled to 48 kHz stereo first.
- **The microphone's pump starts on the track's first packet** (`on_track`), not at answer time: webrtc-rs makes the
  receiver's track only then. The audio transceiver is found by its media section's mid in the offer.
- **A channel's close waits up to 1 s (`channel::CLOSE_DRAIN_SECS`) for the messages sent before it to leave.**
  aiortc queued its stream reset behind pending data; webrtc-rs's close resets the stream at once and dropped an
  `ended` sent just before it (found by the loopback test). Sends and the close go through one ordered writer task.
- An answer whose request is dropped mid-start (Python's `BaseException` from a cancel) still takes its peer down,
  through a drop guard.
