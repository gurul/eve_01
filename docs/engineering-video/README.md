# eve, engineered (the engineering video)

A 9:27 technical breakdown of eve for engineers: how the system is built and why. It is
not the launch film. It covers the hardware and doors, the daemon and the agent contract, the
request path (reflex → Jev body router → desktop floor), the Holo driver as a worked
example, the chat brain and its consent gates, the three memory stores, the watcher, the
Lean verification (with the TLS-retry model as the worked example), spend and privacy, and
how to run it.

| | |
|---|---|
| Output | `docs/launch-video/remotion/out/eve-engineering.mp4` (git-ignored) |
| Format | 1920×1080, 30 fps, H.264, 17,019 frames (567.3 s) |
| Narration | Captions burned in, plus an English `mov_text` subtitle track in the mp4. No audio. |
| Subtitles | `docs/engineering-video/eve-engineering.srt` (80 cues) |
| Script | [SCRIPT.md](SCRIPT.md): timed narration, and the facts and file citations each section relies on |

The launch films have no voice-over or TTS pipeline, so this film carries its narration as
captions and a subtitle track rather than speech.

## Render

```bash
cd docs/launch-video/remotion
npm ci                          # once
npm run render:engineering      # timeline → Remotion render → mux the .srt → out/eve-engineering.mp4
npm run typecheck
ffprobe -v error -show_entries format=duration:stream=codec_type,codec_name,width,height out/eve-engineering.mp4
```

`render:engineering` runs three steps:

1. `make-timeline.mjs` reads the narration and writes the timeline and the `.srt`.
2. `remotion render` renders to `out/eve-engineering.video.mp4`.
3. `ffmpeg` muxes the `.srt` in as a subtitle stream, writes `out/eve-engineering.mp4`,
   and removes the intermediate file.

Other commands:

- Preview: `npx remotion studio src/engineering-index.tsx`
- One frame: `npm run render:engineering:still -- --frame=5167`
- A full render takes several minutes on an M-series Mac.

## Files

| File | What it is |
|---|---|
| `docs/launch-video/remotion/src/engineering/script.json` | The narration, one entry per caption, per section. **Edit this to change what is said.** |
| `docs/engineering-video/make-timeline.mjs` | Times each caption from its word count and writes `timeline.json` and the `.srt`. |
| `docs/launch-video/remotion/src/engineering/timeline.json` | Generated. Frame timings the composition reads. |
| `docs/launch-video/remotion/src/Engineering.tsx` | The `EveEngineering` composition: ten scenes, diagrams, code cards, captions and a progress bar. It reuses `Root.tsx`'s motion system (paper background, wipes, type, cards, pills, the robot). |
| `docs/launch-video/remotion/src/engineering-index.tsx` | The composition's own entry point, so the launch films' `index.tsx` is untouched. |

Every visual beat is keyed to a caption index, not to a fixed frame. After you change the
narration, re-run the render and the scenes re-time themselves. If you add or remove a
caption inside a section, check that section's `Beat from={n}` indices in `Engineering.tsx`
still point at the right caption.

## Keeping it true

Each claim in the narration is backed by a file listed in [SCRIPT.md](SCRIPT.md). Three
figures were measured on 2026-09-28 and will go stale:

- **3,467 passing tests:** `uv run --frozen pytest -q` in `bridge/`.
- **20 Lean models:** the files in `verification/Eve/`.
- **250 theorems:** `grep -cE '^\s*theorem '` over those files.

Re-measure these before you re-render for a new audience.
