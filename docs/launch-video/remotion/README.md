# eve launch films

Two Remotion cuts share one motion system and brand (`src/Root.tsx`):

| Composition | Length | What it is | Render |
|---|---|---|---|
| **EveLaunch** (`src/Pocket.tsx`) | 93 s | The holistic launch film: the whole eve, desk to pocket. | `npm run render` → `out/eve-launch.mp4` |
| LaunchVideo (`src/Root.tsx`) | 118 s | The Sep 13 cut, built around the math lessons. The show-and-tell deck embeds it. | `npm run render:lessons` → `out/launch-video.mp4` |

```bash
npm ci
npm run studio          # preview both
npm run render          # EveLaunch, 1920×1080, 30 fps
npm run typecheck && npm run verify && npm run verify:render
```

## EveLaunch

Ten chapters. Lessons appear once, as one chip in the orbit; the film covers the whole
product. Every claim maps to a source in the repo, and the three features that are off by
default carry an **opt-in** tag.

| # | Chapter | Starts | What moves | Source |
|---|---|---|---|---|
| 1 | hello | 0:00 | The robot assembles piece by piece, its screen powers on like a CRT, the wordmark stamps in | — |
| 2 | senses | 0:06 | Wake word with sound waves; the eyes lead and the head follows a moving face; a scan finds the mug; a hand pats its head and its eyes turn to hearts | README "What eve does", DESIGN.md gaze policy |
| 3 | life | 0:17 | Day turns to night; it explores, snaps a photo that flies onto the diary page, the diary writes itself, moods change, it sleeps; the real widget render slides in | README, `widget/` |
| 4 | pocket | 0:27 | The camera pushes through the robot's screen into the phone; app tiles fly into a `rundown` reply; a camera photo arrives | docs/stackchan/telegram.md |
| 5 | chrome | 0:36 | Chrome raises "Allow remote debugging?", the phone's *yes* flies over and presses Allow, eve opens its own tab, reads the page, and a screenshot flies back | docs/stackchan/routing.md, attach mode |
| 6 | routes | 0:50 | Requests travel from the robot along three tracks: a code shortcut, your own Chrome, or Codex Computer Use with its three approval replies | routing.md, docs/codex-computer-use/README.md |
| 7 | coding | 1:00 | `new claude` over four texts; a paper plane opens Warp; `claude on` relays a numbered question to the phone and the answer flies back; the robot goes working → needs you → done | telegram.md "Start a coding session" |
| 8 | brain | 1:12 | Notes fly from the phone into the Obsidian vault; a todo is added, then checked off | docs/stackchan/second-brain.md |
| 9 | orbit | 1:21 | Everything else orbits the robot while it winks | README |
| 10 | outro | 1:27 | Wordmark, "On your desk. In your pocket.", phone · desk · Mac linked | — |

The chapter list and timings live in `SCENE_LIST` in `src/Pocket.tsx`; start times are
cumulative. Sound is the existing music bed plus robot chirps synthesised by
`npm run sfx` (`scripts/make-chirps.py`, modelled on the firmware's `chirp.h`), placed in
the `SFX` table. The example data in the film (the rundown counts, prices, file names) is
illustrative.

## LaunchVideo (the lessons cut)

This is the 118-second, 16:9 Remotion cut for eve. It starts with the shared problem—an answer is not the same as knowing what to do next—then balances the everyday desk companion with the teacher-like math loop.

The film is grounded in the repository's implementation as of 2026-09-13:

- `gpt-live-1` receives the voice conversation.
- `gpt-6-astra` routes tools and drives guarded computer use.
- `gpt-5.4-nano` describes the camera when asked.
- Exa is shown as an optional lesson-generation reference search that receives only topic and level.
- The robot firmware, local Mac bridge, memory, approvals, and whiteboard are shown as separate parts of the system.

The supplied instrumental is trimmed from 160 seconds to 118 seconds and fades out over the final three seconds. There is no VO asset in the folder, so the script is carried by burned-in show-and-tell typography and UI captions over the supplied music bed.

## Web export for the show-and-tell deck

`docs/launch-video/reference/show-and-tell.html` plays the film on its second slide and
seeks it from the chapter chips. The deck expects a smaller file next to it, so after
`npm run render:lessons` build the web export (two-pass H.264, 720p, about 12.5 MB) and the
ten chapter stills:

```bash
cd docs/launch-video/remotion
IN=out/launch-video.mp4; OUT=../reference/launch-video.mp4
ffmpeg -y -i $IN -vf scale=1280:-2 -c:v libx264 -preset slow -b:v 760k -maxrate 900k -bufsize 1800k -pass 1 -an -f mp4 /dev/null
ffmpeg -y -i $IN -vf scale=1280:-2 -c:v libx264 -preset slow -b:v 760k -maxrate 900k -bufsize 1800k -pass 2 -c:a aac -b:a 80k -movflags +faststart $OUT
i=1; for t in 1.5 13 23.5 36.5 44.5 55.5 67.5 91.5 103.5 113.5; do
  ffmpeg -y -ss $t -i $IN -frames:v 1 -vf scale=640:-2 -q:v 4 ../reference/film/ch$(printf %02d $i).jpg; i=$((i+1))
done
```

The stills are committed; the web export is ignored by git. Chapter start times in the deck
(0:00, 0:12, 0:22, 0:35, 0:43, 0:54, 1:06, 1:30, 1:42, 1:52) are the `SCENES` table in
`src/Root.tsx` divided by 30 fps. If a scene moves, update both.
