# Optimization and handheld transcription review

Reviewed 2026-10-03 against baseline `774a9f1`, using three parallel agents for voice/transcription, firmware/iOS, and host code, with the parent reviewing tools/widget code and integrating the changes. Existing uncommitted work was preserved and excluded from this patch.

> The M5StickS3 stick was removed on 2026-10-07, with its firmware, BLE codec and packet queue. The stick rows below are a record of that review; their benchmark scripts are gone.

## Scope and evidence

The initial inventory contains 569 tracked code artifacts: 22 voice/transcription files, 88 firmware/iOS files, 299 host files, and 160 tools/widget/archived files. All have a recorded review or explicit generated/vendor/archived classification in the local `.unlazy/optimization-20261003/` evidence directory. Voice files were read in full. Firmware/iOS executable logic was read, with structural review of species art. Host and tools review combined declaration/I/O inspection with full reads of changed code and selected high-risk paths. This is broad repository coverage, not a claim that every line or dependency received a complete semantic audit. New helpers, tests and benchmark scripts were separately reviewed.

Changes were selected from actual code defects or measured work. Model substitutions, guessed VAD thresholds, caching without invalidation evidence, and removing words by a blacklist were not introduced.

## Handheld transcription

Two concrete code paths can contribute to stray initial words:

- The stick's old 60 ms microphone guard was shorter than the first 100 ms startup transient described by the existing bench record (`GATES.md`, G2.3, 2026-09-30). `MicStartup` now drops exactly 2,400 samples at 24 kHz on every press, preserving samples immediately after that boundary across capture chunks. That earlier recording is inherited evidence; its WAV was unavailable for this review. Speaking inside the startup window can still lose initial audio.
- The bridge previously reused mutable transcription state while server VAD could create several items inside one press. A following press could clear the preceding press's unfinished state. A new regression fails against the original implementation at that premature clear. Calls now disable automatic VAD, wait for `session.updated`, commit one whole press at release, serialize press ownership, and accept completion text only for the committed item ID. Failed sessions are retired and the complete press is uploaded as fallback. Legitimate initial words such as “hello” are retained.

Fake recognizer and loopback WebSocket tests cover rapid presses, delayed and unrelated events, session rejection, timeout, cancellation, short taps, duplicate controls, full PCM recovery and the exact 120-second audio cap. They prove event/audio ownership and routing, not speech-model accuracy. The model remains configurable, defaulting to `gpt-4o-transcribe` with near-field noise reduction.

Manual commit changes the timing: final recognition follows release, and a rapid next press waits for the preceding live finalization (up to its six-second timeout). Historical server-VAD latency figures in the call and stick guides are labeled accordingly. No new live latency, word-error rate or provider cost measurement was made.

The implementation follows the current [OpenAI transcription guide](https://developers.openai.com/api/docs/guides/realtime-transcription), which describes manual commits, item IDs and completion ordering, and the [session API](https://developers.openai.com/api/docs/api-reference/realtime-sessions), which permits null turn detection. Installed SDK 3.19.2 types also accept transcription sessions in `session.updated` and optional input turn detection.

The 2025 paper [Investigation of Whisper ASR Hallucinations Induced by Non-Speech Audio](https://arxiv.org/abs/2501.11378) demonstrates hallucinations with non-speech input and augmented speech in Whisper. It supports investigating capture noise as a possible contributor. It does **not** establish the cause or improvement rate for this project's `gpt-4o-transcribe` model; that connection is an inference needing a live recording comparison.

## Other implemented improvements

| Path | Change | Correctness evidence |
|---|---|---|
| `jsonl_tailer.py` | Seeds historical UUIDs in its first parse instead of rereading history; buffered line reads respect the size snapshot; live batches run in an awaited worker thread | New append-after-sweep race, event-loop ticker, partial record, truncation, malformed metadata, snapshot and allocation tests |
| `audit.py` | Uses a bounded deque for the requested filtered tail; streams complete history | Filter/order tests and allocation tests with an intentionally unbounded positive control |
| Stick playback ring | At most two contiguous copies replace per-sample modulo while the ring lock is held | Independent FIFO comparisons across wrap, capacity and partial/full writes under ASan/UBSan |
| Eve Link packet queue | Amortized FIFO replaces `Array.removeFirst()`; sent payloads are released immediately | Burst, ordering, interruption, compaction and weak-reference lifetime checks |
| Eve Link PCM | Unaligned little-endian loads; odd-length input is rejected | Signed extrema, offset slices, empty/truncated buffers; simulator and macOS compilation |
| Voice PE PCM widening | Multiplication replaces undefined left shifts of negative samples in C++17 | All 65,536 signed 16-bit inputs; old expression triggers UBSan, replacement passes |
| Widget `NoteStore` | Reads thought history backward until 200 valid records; reads only the 40-line meeting header | Unicode/chunk boundary, malformed row, order, missing/empty file, limit and header tests |
| Sound persistence | Rejects non-object saved JSON through existing default recovery | Invalid-shape tests plus a valid saved-mute control |

The transcript parser still retains UUID history for deduplication and must hold its largest line. Audit filtering still scans the file. Widget reads can scan more history when recent rows are invalid; unusually long lines or headers require proportional memory. These are bounded-work improvements for the intended data layout, not universal constant-memory guarantees.

Current primary technical references explain the contracts used: [Python blocking-I/O offloading](https://docs.python.org/3/library/asyncio-eventloop.html#executing-code-in-thread-or-process-pools), [bounded deque behavior](https://docs.python.org/3/library/collections.html#collections.deque), [sized buffered line reads](https://docs.python.org/3/library/io.html#io.IOBase.readline), [Apple Array removal complexity](https://developer.apple.com/documentation/swift/array/removefirst/), [unaligned buffer loads](https://developer.apple.com/documentation/swift/unsaferawbufferpointer), [Foundation file reads](https://developer.apple.com/documentation/foundation/filehandle/read(uptocount:)), [file-end seeking](https://developer.apple.com/documentation/foundation/filehandle/seektoend()), and [Clang signed-shift checks](https://clang.llvm.org/docs/UndefinedBehaviorSanitizer.html). Actual production headers and installed Swift APIs are compiled in the regression checks.

## Reproducing the measurements

These are synthetic local arm64 Mac measurements. Python timings include `tracemalloc`; allocation peaks are Python allocations, not process RSS. Ring and queue measurements are host CPU time, not BLE radio, ESP32 or iPhone latency. Widget timings are seven full refreshes of the same generated files, with equality assertions for the returned 200 thoughts and meeting summary.

| Workload | Before | After |
|---|---:|---:|
| Widget refresh, 84,757,855 bytes of history/transcript, median | 742.79 ms | 1.92 ms |
| Transcript startup, 20,000 complete assistant records / 8,960,000 bytes, maximum traced peak | 22,988,011 bytes | 4,160,981 bytes |
| Audit tail of 20 rows from 20,000 / 4,668,890 bytes, maximum traced peak | 11,472,942 bytes | 31,754 bytes |
| Ring, 200,000 push/pop cycles of 480 samples, median of three | 45.953 ms | 4.655 ms |
| Queue drain, 20,000 packets, median of three | 42.061 ms | 1.374 ms |

Run from the repository root; results will vary by machine and load. Inspect the baseline revision before executing its Python modules.

```sh
bridge/.venv/bin/python bridge/tools/benchmark_io.py --baseline 774a9f1
git show 774a9f1:widget/Shared/NoteStore.swift > /tmp/eve-NoteStore-before.swift
sh widget/Tests/benchmark.sh /tmp/eve-NoteStore-before.swift
```

## Verification and rollout

Native C++ ASan/UBSan checks, Swift reader/relay checks, configured Stick and Voice PE firmware compilation, and unsigned iOS simulator/macOS builds passed. The parent independently reran all 25 original leaf checks. The final working-checkout Python run passed **4,210 tests, with 15 skipped** in 562.32 seconds; Ruff passed for bridge source, tests and the benchmark script.

An isolated checkout containing only this patch passed **4,166 tests, with 15 skipped** in 564.55 seconds. Its native checks, original-view widget build and original-ring Voice PE build also passed. All 34 code/test/benchmark files in the staged patch were byte-identical to the isolated sources; the test environment explicitly imported that checkout's modules. The count difference is from existing local tests excluded from this patch. Skipped tests are not counted as verified, and hardware/provider behavior is outside these offline results.

The first combined suite exposed two existing test assumptions: chief spend rows used real wall time but were queried on the fixture's fixed day; the context test assumed `origin/main` still predated the chief feature. The tests now inject one clock into the spend recorder and compare enabled versus disabled context, retaining their positive row/privacy and remote off-mode compatibility assertions.

Hardware/provider checks were not run: no device was flashed, app installed or daemon restarted. To exercise the fixes on the handheld, update the daemon, stick firmware and Eve Link using their existing deployment instructions, then record repeated presses including quiet pauses and rapid interruptions. Compare reference text, first-word retention and release-to-text timing. A code regression fix does not prove that every intermittent model hallucination is eliminated.

Further candidates were reviewed but lacked enough evidence for safe implementation: lesson summary projection/schema changes, chief event caching with redaction/concurrent-update invalidation, serial initialization cancellation, and browser polling under slow responses. These need representative workloads or device/browser checks before tuning.

## Daemon footprint and the Zig question (2026-10-13)

Asked whether rewriting the daemon in Zig, as the fx coding agent is written, would make it faster or smaller.
Measured on the live daemon, read-only: **no**. The daemon is idle, not compute-bound. It averaged 0.6% CPU over
4.5 h, and in `sample <pid> 10` the main thread waited in `kevent` for 7585 of 7650 samples. A turn's 2.7 s
median (n=67, the `turn answered in` log lines) is network and model time; the local Python steps are single-digit
milliseconds. `import eve_bridge.cli` takes 0.15 s, and the daemon starts once. A native port would remove none
of that.

The memory was one model. `footprint <pid>` reported 2.07 GB, 1.19 GB of it GPU memory. That was the opt-in
EmbeddingGemma embedder (`EVE_MEM0_EMBEDDER=gemma`): sentence-transformers put its fp32 weights on MPS by
default. Changes made:

| Change | Measured |
|---|---|
| Gemma embedder pinned to CPU (`mem0_memory.py`) | Fresh process 1.85 GB → 0.62 GB footprint, no GPU. Vectors identical to MPS (cosine 1.0, so the existing store still matches). A 3-text batch takes 29 ms on CPU vs 63 ms on MPS. |
| Serial "board unplugged?" warning back-off uses a deadline (`serial_transport.py`) | It compared total time missing with a gap capped at 600 s, so after 10 minutes it warned on every 3 s pass: 70,584 of the log's 105,971 lines. A test fails on the old code and passes on the new one. |
| `awatch(..., step=500)` in `jsonl_tailer.py` | The watch thread woke every 50 ms with nothing to do. Its own 1600 ms debounce already sets the latency. |

Not changed:

- **Desk-call mic open (a 3.5 s stall).** `Pa_OpenStream` runs on the loop thread. Moving it to a worker thread
  adds an await before the mic opens, which `verification/Eve/DeskCall.lean` (P1) rules out. Doing it needs a
  close-wins flag on `Mic` and an updated model.
- **Cold-import stalls at start.** One stack shows the loop thread blocked on the import lock while a warm-up
  thread imported the same module. More warm-up threads move the wait, they do not remove it.
- **fx itself** could be added as a separate coding lane (`fx ask --json`, or ACP). That is a new feature, not an
  optimization. libfx exposes Node-API and WASM only, no C ABI.
