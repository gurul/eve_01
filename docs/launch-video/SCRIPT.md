# eve launch video — script and build brief

**TL;DR.** A 60-second, 16:9 launch video for eve, the desk robot, built in React with
Remotion. It shows what eve does today, then introduces math lessons for kids. The look
is the "Eve Show-and-Tell" cut-paper brand. This file is the brief for the agent that
builds the video: the script is in [Scenes](#scenes), and everything visual is in
[Design language](#design-language) and [Where to find things](#where-to-find-things).

---

## Format

| Setting | Value |
|---|---|
| Length | 60 s main cut (1800 frames). A 30 s cut is listed at the end. |
| Canvas | 1920×1080, 30 fps |
| Optional | 1080×1920 vertical cut that reuses the same scenes, stacked |
| Audio | voice-over (VO), light upbeat music bed, robot chirps as sound effects |
| Captions | burned-in captions for every VO line (many viewers watch muted) |
| Audience | parents, teachers and makers who want a helpful robot for a kid's desk |

## Tone

Warm, playful, honest. Short sentences a 10-year-old can read. eve is a sidekick that
helps a kid think. It gives a hint, checks the work, or shows one step. It never hands over
the whole answer, and the video must never show it doing that.

---

## Design language

The source of truth is `reference/show-and-tell.html` in this folder. Open it in a browser
and read its `<style>` block: every token below comes from there.

### Palette

| Token | Hex | Use |
|---|---|---|
| paper | `#FBF5EC` | the background of every scene |
| sheet | `#FFFDF8` | card fill |
| ink | `#1F3A78` | all text, all borders, arrows |
| ink-soft | `#3E4F82` | secondary text, kickers |
| pink | `#E68AAE` | accent shapes, "Show one step" |
| pink-soft | `#F4C3D4` | heading text-shadow, highlights |
| teal | `#79C6B2` | "works today" tags, "Check my work" |
| sun | `#F1C85B` | "designed" tags, "Give me a hint" |
| sky | `#7FA2DE` | card shadows, decorative shapes |
| desk | `#E9E1D3` | the surround outside the paper |
| robot screen | `#1B2350` | the robot's face screen |
| robot eyes and screen text | `#FF74D4` | pixel eyes, "Hey there!" |

### Type

| Role | Typeface | Weights |
|---|---|---|
| Headings, the "eve" wordmark, big numbers | Grandstander | 700, 900 |
| Body text, captions, button labels | Andika (designed for beginning readers) | 400, 700 |
| Handwritten kickers and things eve says | Gochi Hand | 400 |
| Text on the robot's screen only | VT323 | 400 |

- Headings carry a solid offset text-shadow in pink-soft (about 4–7 px down and right).
- The wordmark is lowercase: **eve**.
- The fonts are bundled in the repo (SIL Open Font License). Load them from local files so
  renders never depend on the network.

### Shapes and objects

- **Cut-paper cards:** sheet fill, 2.5 px ink border, 4–6 px corner radius, a solid offset
  shadow of 7 px down and right in one brand color (sky, sun, teal or pink).
- **Tags:** pill shape, 2.5 px ink border, uppercase 15 px Andika bold with letter spacing.
  "works today" is teal, "designed" is sun.
- **Chunky buttons:** colored fill, ink border, 3 px ink offset shadow. When pressed, the
  button moves down-right into its shadow.
- **Decorative cut-paper shapes** frame the title and end cards: a blue circle with rings, a
  teal blob, sun zigzags and dotted squares, a pink star, a pink hexagon with a wavy
  pattern, a pink arc. Copy them from the first slide's `<svg class="decor">` in
  `reference/show-and-tell.html`.
- **The robot drawing:** the `<svg class="robot">` on the first slide of
  `reference/show-and-tell.html`. It has a lavender label bar reading "eve", a gray
  bezel, a navy screen with two pink pixel eyes and "Hey there!", a neck and a base.

### Motion

- Cards and shapes enter by sliding and settling with a soft spring (a little overshoot,
  then still). Small rotations up to 1° on entry are fine for paper pieces.
- Animate the robot as a whole group: move it up and down, or scale it slightly for a
  "pop". Keep each eye as one solid shape with normal anti-aliasing. Blink an eye by
  scaling that eye vertically.
- Text on the robot's screen types in one character at a time in VT323, with a chirp.
- Scene changes are paper wipes: a colored cut-paper sheet slides across and reveals the
  next scene.
- One motion idea per moment. Leave room to read.

### Real eve, for honesty

`../assets/hero.png` and `../assets/thumbnail.png` are photos of the real robot (an
M5StackChan head) on a desk. Use one real shot in scene 2 so viewers know it is a real
device. Those photos are darker and moodier than the cut-paper brand. Frame them inside a
cut-paper card so they sit inside the brand.

---

## Where to find things

All paths are relative to the repository root (`eveTinkerer/`).

| What | Where |
|---|---|
| Brand source of truth (14 slides; follows the film chapter by chapter and embeds it) | `docs/launch-video/reference/show-and-tell.html` |
| Same brand as a scrolling page | `docs/launch-video/reference/one-pager.html` |
| Published versions (need the owner's login) | Show-and-Tell: https://claude.ai/code/artifact/9b8eb406-2a54-40f7-a1a3-e77faa8eb820 · one-pager: https://claude.ai/code/artifact/3371cd3d-fa06-409e-9c3b-1922e2e3082a |
| Web fonts (woff2) + license | `bridge/src/eve_bridge/learning/web/fonts/` |
| Font files (ttf) + licenses | `widget/Shared/Fonts/` |
| Brand CSS as used in the product | `bridge/src/eve_bridge/learning/web/style.css` |
| Brand in SwiftUI (colors, cards, robot face) | `widget/Shared/EveBrand.swift` |
| Lesson app screenshots, new look | `docs/launch-video/reference/web-dashboard.png`, `web-lesson.png`, `web-lesson-mobile.png` |
| Mac widget renders, new look | `docs/launch-video/reference/widget-notes-medium.png`, `widget-notes-large.png`, `widget-learning-medium.png`, `widget-learning-large.png` |
| Lesson flow (for timing and order) | `docs/eve-math-activity-diagram.png`, `docs/learning.md` |
| Recorded lesson walkthroughs | `docs/learning-demo/*.webm`. These show the flow and timing in the earlier visual design. Rebuild the screens in the new look from the screenshots above. |
| Real robot photos | `docs/assets/hero.png`, `docs/assets/thumbnail.png` |
| Feature facts | `README.md`, `docs/stackchan/voice.md`, `docs/stackchan/vision.md`, `docs/stackchan/personality.md` |
| Robot chirp style | `firmware/claude_pet_stackchan/src/chirp.h` (short R2-D2-style phrases: wake, happy, curious, thinking) |

---

## Scenes

Timecodes are for the 60 s cut. Frame numbers assume 30 fps.

### 1 · Hello (0:00–0:04, frames 0–119)

- **Visual:** paper background. Cut-paper shapes spring in from the corners. The robot pops
  up in the center. Its screen types "Hey there!" and its eyes blink once.
- **On screen:** the wordmark **eve** lands under the robot with the pink-soft shadow.
- **Sound:** a rising two-note wake chirp.
- **VO:** "Meet eve."

### 2 · A real robot on your desk (0:04–0:10, frames 120–299)

- **Visual:** a cut-paper card holding `docs/assets/hero.png`, tilted about 1°. Beside it,
  parts chips pop in one by one: camera · 2 motors · 12 lights · speaker · touch pad ·
  screen face. Then a handwritten line: "+ the eve app on your Mac".
- **On screen kicker:** "your desktop sidekick"
- **VO:** "A little robot for your desk, with ears, hands and a memory."

### 3 · It talks with you (0:10–0:14, frames 300–419)

- **Visual:** a sky-shadow card. A speech line in Gochi Hand: "hey eve, what's the
  weather?" The robot's screen shows a caption. Small chips: web search · remembers your
  chats.
- **Tag:** works today
- **VO:** "Say 'hey eve.' It answers, looks things up, and remembers what you talked about."

### 4 · It helps at the computer (0:14–0:18, frames 420–539)

- **Visual:** a sun-shadow card with a simplified Mac window. A cursor clicks and types by
  itself while the robot's screen says "on it…". A "stop" speech bubble pops, and the
  cursor halts.
- **Tag:** works today
- **VO:** "Ask for a task, and it clicks and types for you. It asks first before anything big."

### 5 · It sees and moves (0:18–0:22, frames 540–659)

- **Visual:** a teal-shadow card. The robot turns toward a drawn mug ("where's my mug?"),
  then bobs side to side. A hand pats its head, and its back lights pulse pink.
- **Tag:** works today
- **VO:** "It follows your face, finds your mug, and loves a head pat."

### 6 · A life of its own (0:22–0:26, frames 660–779)

- **Visual:** a pink-shadow card. The robot looks around, and a thought bubble appears. The
  widget render `widget-notes-medium.png` slides in as if on a desktop.
- **Tag:** works today
- **VO:** "When things are quiet, it explores, keeps a diary, and shares what it noticed."

### 7 · New for kids (0:26–0:30, frames 780–899)

- **Visual:** a paper wipe in sun. The big words **new for kids** land between two zigzag
  shapes (sun on the left, teal on the right).
- **Sound:** a happy trill.
- **VO:** "And now, something new for kids."

### 8 · Why it matters (0:30–0:37, frames 900–1109)

- **Visual:** three stat cards count up in Grandstander 900: **71%** (teal shadow), then
  **44%** (pink shadow), then **1 in 3+** (sun shadow). A quote card slides in beneath.
- **On screen:**
  - 71%: "UC Berkeley Calculus I students ready or nearly ready, 2018–2020"
  - 44%: "ready or nearly ready, 2021–2023"
  - 1 in 3+: "severe math gaps in 2022–23"
  - Quote: "…lessons on fractions and basic algebra you would expect students to learn in
    middle school." — Zvezdelina Stankova, mathematics professor, UC Berkeley
  - Source line, small, bottom: "Stankova op-ed, San Francisco Standard, via the California
    Post, Aug 16, 2026"
- **VO:** "Fraction gaps don't fix themselves. At UC Berkeley, calculus students are
  relearning fractions."
- **Accuracy:** keep the numbers, the labels and the source line exactly as written.

### 9 · The math lesson (0:37–0:47, frames 1110–1409)

- **Visual, in order:**
  1. A kid's speech line: "Okay eve, I want to do a math lesson!"
  2. The lesson screen, rebuilt from `reference/web-lesson.png`: problem strip "Solve
     2x + 3 = 11." in sun, then a dotted whiteboard.
  3. A pen stroke writes "2x = 8" on the board.
  4. The three help buttons press in one at a time: **Give me a hint** (sun) → a hint
     card; **Check my work** (teal) → a check mark; **Show one step** (pink) → eve's
     step "x = 4" appears in its own pink panel, apart from the kid's writing.
  5. The robot's screen mirrors each moment ("hmm…", then "nice!").
- **VO:** "Pick a topic, or bring your own problem. On the whiteboard, eve gives a hint,
  checks your work, or shows one step at a time."

### 10 · Never the whole answer (0:47–0:52, frames 1410–1559)

- **Visual:** a Grandstander line with the key phrase highlighted in pink-soft: "The whole
  answer is **never on the menu**." Below it, Gochi Hand speech lines appear one by one:
  "Show me where you're stuck." · "Is that a 7 or a 1?" · "So how did we do that?"
- **VO:** "eve speaks first, and keeps you thinking. The whole answer is never on the menu."

### 11 · End card (0:52–1:00, frames 1560–1799)

- **Visual:** the title card again (shapes, robot, wordmark). The robot's screen shows
  "see you!" and blinks.
- **On screen:**
  - **eve**
  - "your desktop sidekick"
  - "open source · github.com/gurul/eve"
- **Sound:** the wake chirp, played in reverse as a goodbye.
- **VO:** "eve. Your desktop sidekick."

---

## VO script, one block

> Meet eve. A little robot for your desk, with ears, hands and a memory. Say "hey eve."
> It answers, looks things up, and remembers what you talked about. Ask for a task, and it
> clicks and types for you. It asks first before anything big. It follows your face, finds
> your mug, and loves a head pat. When things are quiet, it explores, keeps a diary, and
> shares what it noticed. And now, something new for kids. Fraction gaps don't fix
> themselves. At UC Berkeley, calculus students are relearning fractions. Pick a topic, or
> bring your own problem. On the whiteboard, eve gives a hint, checks your work, or shows
> one step at a time. eve speaks first, and keeps you thinking. The whole answer is never
> on the menu. eve. Your desktop sidekick.

About 145 words, which reads comfortably in 60 s. If a scene runs short on time, trim the
visual, not the VO line.

## 30 s cut

Scenes 1, 3 and 4 combined (one card with three chips), 7, 9, 10 and 11. Drop scenes 2, 5,
6 and 8.

---

## Claims the video may make

Every claim below is backed by code in this repository. Keep the video inside this list.

| Claim | Status | Evidence |
|---|---|---|
| Wake word "hey eve", captions on its screen | works | `docs/stackchan/voice.md` |
| Web search, remembers past chats | works | `bridge/src/eve_bridge/voice_agent.py`, `recall.py` |
| Does tasks on the Mac, asks before sending, paying or deleting | works | `computer_agent.py`, `README.md` |
| Follows your face, finds objects, head pat reaction | works | `vision.py`, firmware `main.cpp` |
| Dances | works, started from the Mac today (not by voice) | `cli.py` (`move dance`) |
| Explores, moods, diary, photos, widget | works | `explore.py`, `diary.py`, `widget/` |
| Math lessons: learn a topic or bring a problem; hint, check, one step | built on branch `eve-lesson-tool`, live-tested 2026-09-12, not merged yet | `docs/learning.md` |

Before the final render, ask the owner whether math lessons are released. If they are not,
add a small "coming soon" tag (sun) to scenes 7 and 9.

## Build notes for the Remotion agent

- Check the current Remotion version and its docs before you pin any package.
- Suggested structure: one `<Composition>` per cut (60 s, 30 s, vertical). One component
  per scene, placed with `<Sequence from={…} durationInFrames={…}>`. Shared components for
  `PaperCard`, `Tag`, `ChunkyButton`, `Robot` and `CutPaperShapes`.
- Put the palette and type roles above in one tokens file, and use only those values.
- Copy the font files into the video project's `public/` folder and load them with
  `staticFile()` before the first frame renders.
- Put the robot and shape SVGs in React components copied from
  `reference/show-and-tell.html`, so they stay sharp at any size.
- Drive every animation from `useCurrentFrame()` with `spring()` and `interpolate()`, so
  renders are deterministic.
- Add a captions track that matches the VO block word for word.
