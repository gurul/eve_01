# Personality: how eve feels, and how you can tell

Two things make the robot read as alive while it explores: an **affect engine**
on the board (valence, arousal, two drives) that colours its eyes, LEDs, head and
chirps, and a **diary** on the host with a memory of everything it has seen, so
what it writes is specific and its own. A third layer — **agent phases** — makes
the robot act out the voice conversation and the computer-use task the daemon is
really running (see [voice.md](voice.md)).

For the research and measured limits of using a local model to select expressions,
see the [Laya tuning study](laya-emotion/README.md). Its experimental controller is
not connected to the robot; conversation phases still own the face as described below.

## 1. The affect engine (`firmware/claude_pet_stackchan/src/mood.cpp`)

The model is the lightweight one most social robots use — a point in the
valence/arousal circumplex plus a few homeostatic drives — because it fits an
ESP32 in a handful of floats and reads clearly to people.

**State**

| Variable | Range | Timescale | What it is |
|---|---|---|---|
| `v`, `a` | −1..1 | relaxes toward the mood, τ 60 s | the feeling right now (Mini: k = 0.05) |
| `moodV`, `moodA` | −0.5..0.5 | drifts toward the feeling, τ 1 h | the slow baseline; starts slightly cheerful (personality) |
| `social` | 0..1 | +0.0004/s alone | rises with time without a face or a touch; satisfied fast by company |
| `stimulation` | 0..1 | +0.001/s quiet | rises when nothing moves and no view is new; satisfied by motion or a new view |

**Stimuli** are fixed deltas (MiRo, Mini): motion in view raises arousal (+0.2 per
second of solid motion); the owner's face once per visit +0.4 valence, +0.2 arousal;
a stranger a little interest and a little wariness; a touch +0.2 valence; arriving
at a new view +0.1 arousal (a small surprise if already keen); a **sudden big
motion while calm is a startle** (+0.7 arousal, −0.35 valence, 20 s cool-down,
never asleep). The host's diary appraisal (`{"cmd":"emote","dv","da"}`) nudges
the feeling by at most ±0.3 — it can colour, not hijack.

**Drives** past their regime threshold pull the feeling toward a target rather
than pushing without bound: stimulation > 0.6 pulls arousal toward −0.6 (bored),
social > 0.7 pulls valence toward −0.7 (lonely), both with τ 20 s — stronger than
the relaxation toward the mood on purpose, so the robot settles at "bored" or
"lonely" instead of at the rail. A quiet room reads as bored after two or three
minutes and lonely after a quarter of an hour alone.

**Kind** (one active at a time, winner-take-all with recent-event priority):

| Kind | When | Eyes | LEDs | Head | Chirp | Word |
|---|---|---|---|---|---|---|
| startled | a jolt < 1.5 s ago, or a > 0.6 with v < −0.2 | wide, horizontal flicker | red flash, fades over 1.5 s | back and up, fast | sharp double blip | `eek!` |
| surprised | a new view while keen, < 2.5 s ago | wide, curious | white flash, fades | pitch +10 | one high blip | `!` |
| affection | the owner seen < 20 s ago, v > 0.2 | curious, smile | pink heartbeat from the middle of each row outward | — | soft warble | `<3` |
| happy | v > 0.4, a > −0.2 | HAPPY mood (lower-lid smile) | green sparkle | quicker wander | soft warble | `happy` |
| curious | a > 0.2, v ≥ −0.2 | curious, quick blinks and saccades | scanner dot bouncing along each row, faster with arousal | wide, quick wander | two rising notes | `curious...` |
| lonely | v < −0.4, a < 0.2 | TIRED mood, droopy | only the middle two LEDs, dim purple, slow | pitch −10 | slow falling sigh | `lonely...` |
| bored | a < −0.35 | half-closed, slow blinks and saccades | only the middle two LEDs, dim, 0.25 Hz | narrow, slow, pitch −10 | slow falling sigh | `bored...` |
| calm | otherwise | default | breathing wave travelling along the rows, in the feeling's colour and pulse rate | normal | — | `exploring...` |

**The back LEDs** are twelve WS2812 in two rows of six (0–5 left, 6–11 right).
Every state is an animation over the rows — `body.cpp` `patWave` (a glow that
travels along the row), `patScanner`, `patHeartbeat`, `patSparkle`, `patDroop`,
`patAlternate`, `patFlashFade` — composed at 20 Hz; only the LEDs that changed
are written to the PY32 expander.

**Continuous expression** is read off (v, a) every frame, not looked up per kind:
eyelid openness 0.55 + 0.4·a; blink interval 1.5 s/(1 + a); saccade tempo
2 s/(1 + 1.5·a); LED hue from valence (orange − / green + / cyan calm-positive),
saturation 0.4 + 0.5·|v|, brightness 0.3 + 0.6·(a + 1)/2, pulse 0.25 / 0.5 /
2.5 Hz; look-around amplitude 65 + 45·a degrees (clamped 25..110), tempo 1 + 0.6·a. One chirp per
kind change, at most one every 8 s.

**Where it shows.** Only while exploring, and never over the
listening pose, the attention state, or an agent phase. The engine integrates all
the time, so a startle right before a conversation is still felt afterwards.

## 2. Agent phases (`{"cmd":"agent","state":…}`)

| Phase | Head | Eyes | LEDs | Status word | Chirp |
|---|---|---|---|---|---|
| wake | up (+12) | open, curious | white flash, fades | `yeah?` | wake whistle |
| listening | faces you (last toucher side), pitch +15 | wide (110 px) | blue breathing wave | `listening...` | — |
| thinking | tilted 14° aside, slow side-to-side every 2.5–4 s | quick saccades | cyan scanner dot | `hmm...` | — |
| speaking | up (+10), small bobs every 0.5–0.8 s | HAPPY | white sparkle | caption pages (4 lines × 17, held at reading pace) | one beep-boop phrase per page |
| working | down at the desk (pitch 30), typing glances every 0.7–1.2 s | squint + curious, fast saccades | fast cyan ripple | `on it...` | — |
| asking | up (attention pitch) | wide | left row / right row alternating orange | `yes / no?` | "hm?" |
| done | nod | HAPPY + laugh | green sweep, then solid green | `done!` | beep-boop |
| error | side-to-side wobble | TIRED + flicker + confused | red double flash | `oops` | descending boop |
| idle | back to the persona state | — | — | — | — |

The daemon re-sends the current phase every 10 s while a conversation is open; a
phase that goes 30 s without one (the daemon died or the link dropped mid-conversation)
falls back to the persona.

**A pet showing words is awake** (`src/face.h`). The persona sleeps whenever Claude
Code is idle, and a conversation or a thought caption is not Claude Code activity, so
eve used to close its eyes under its own reply — and the `zzz` status word, whose
row (y 186) sits on the caption band's 4th line (y 181), flashed into the text
whenever a page ran out before the next arrived. Now, while a caption is up and for
4 s after the last one, the face (eyes and status word) presents sleep as idle. The
body keeps its sleep pose, so there is no sleepy chirp and head-drop between pages.

The root cause was on the board, not in the rule: `loop()` reads `now` once, and
`dataPoll()` then stamps each host command with a fresh `millis()`. When the clock
ticked between the two, `now - agentAtMs` wrapped to about four billion and a phase
went stale the instant it arrived — three times in one conversation (board event
ring, 2026-09-10). Every age check against a `dataPoll()` stamp now goes through
`ticks::elapsedMs()` (`src/ticks.h`), which reads a stamp newer than `now` as age 0.

## 3. The diary (`bridge/src/eve_bridge/diary.py`)

The old note was a caption: "a desk with a monitor and a keyboard", once per
changed frame, with no memory. The diary gives the explorer both.

**Memory stream.** One record per frame in
`~/.config/eve-bridge/notes/memory.jsonl`: the thought, 3–6 concrete
observations, what changed, tags, novelty and importance (1–10), whether it was
written, the appraisal (valence, arousal, label), and when it was last retrieved.

**Profile.** `profile.md` with four blocks — ROOM (persistent objects and their
usual state), HUMAN (habits, with evidence ids), SELF (persona, running jokes, open
questions), RULES (what to ignore) — each capped at 2,000 characters.

**Retrieval for the next thought** (no LLM call, ~2k tokens): the profile; the last
three written thoughts; five older records scored by recency × importance ×
relevance (0.995 per hour decay, tag overlap, min-max normalised, equal weights);
two thoughts from the same hour on earlier days; today's unwritten candidates so
they are not repeated.

**One vision call per frame** returns observations, what changed, **three
candidate thoughts with a typicality probability each**, novelty, importance,
tags and the feeling. The daemon takes the least typical candidate that says
something outside the baseline vocabulary and is not a near-repeat (token Jaccard
> 0.6) of the last ten.

**Write gate.** Written iff novelty ≥ 5 or importance ≥ 7 or the diary has been
silent for 3 h. Unwritten thoughts stay in memory and feed retrieval (the widget's
diary window shows them dimmed).

**Dreams.** Once a day from 21:00, or when the summed importance of new records
passes 150, one text-only call asks the three most salient questions about the day,
answers them as insights citing record ids, rewrites the profile (ADD/UPDATE/DELETE
per fact), and proposes up to two **★ candidates** — one-line claims worth keeping
forever. Insights and candidates land under `## Dreams` in the day's file; the
widget shows them on the Dreams tab.

**Stars.** `highlights.md` is the human's layer: press ★ on a thought in the diary
window and it is appended there, dated, forever. It is always in eve's prompt
context ("starred by my human — never forget, never contradict") and the dreams
pass may not contradict it. eve never stars anything itself; it only proposes.

The layers mirror a debrief memory system: `memory.jsonl` = episodes (raw,
immutable); `YYYY-MM-DD.md` = the day (episodic, immutable once written);
`profile.md` = the semantic layer (rewired nightly, never appended);
`highlights.md` = the starred layer (human-promoted, append-only).

**Appraisal → feeling.** The same call's valence/arousal/label goes to the board as
`{"cmd":"emote"}` and nudges the affect engine (±0.3 max), so a lonely-looking
empty room and the owner walking in feel different on the robot's face.

## 4. Photos: what eve finds cool (`diary.py`, `photos.py`)

A few views are worth more than a sentence. For those eve asks the board for a
full-resolution frame (`{"cmd":"snap"}` → 320×240 at JPEG quality 85, `look.cpp`)
and keeps it. What follows is why the gate is shaped the way it is; every claim
below was checked against its source.

**The default is not to keep.** The brain's default is not to store either:
information enters the hippocampus as early LTP that fades, and is converted to a
lasting memory only when a systems-wide check finds high novelty **or** high
motivational salience, releasing the dopamine that makes it stick (Lisman & Grace,
*The hippocampal-VTA loop*, Neuron 46(5), 2005). eve's gate has the same shape:
two channels either of which can open it, one arithmetic score, one threshold.

**Two kinds of new.** Novelty and surprise are different quantities with different
machinery: novelty is finding no representation of the thing in memory, surprise is
a mismatch with what was predicted here (Barto, Mirolli & Baldassarre, *Novelty or
Surprise?*, Frontiers in Psychology 4:907, 2013). Both feed the score. Novelty is
the model's own 1–10 rating, ranked against its recent ratings — necessary because
LLM ordinal ratings compress hard toward the middle of any rubric (Zhang et al.,
*Auditing Multimodal LLM Raters: Central Tendency Bias*, arXiv:2605.16386, 2026).
Surprise is computed on the robot, from pixels, per waypoint.

**Surprise without a noisy TV.** `ChangeDetector.surprise` keeps the last 30
thumbnails of each waypoint and scores a new one cell by cell: the distance from
that cell's median, divided by its own spread. A cell that always flickers — a
monitor, a fan, leaves in a window — has a large spread, so it contributes almost
nothing. This is the fix for the failure that kills plain prediction-error
curiosity: an agent rewarded for prediction error parks itself in front of a TV
whose channel changes at random and never leaves (Burda et al., *Large-Scale Study
of Curiosity-Driven Learning*, arXiv:1808.04355, 2018, where the maze-with-a-TV was
tested literally; the normalisation trick is from *Exploration by Random Network
Distillation*, arXiv:1810.12894). Bayesian surprise resolves the same paradox for
attention — TV snow carries about twenty times more Shannon information per second
than ordinary television and attracts far less attention (Itti & Baldi, *Bayesian
surprise attracts human attention*, Vision Research 49(10), 2009, which also
reports 72% of human gaze shifts landing on above-average-surprise locations).
Thumbnails are brightness-normalised first, so the sun going in is not a discovery.

**A small change on a still background beats a big change on a busy one.** That
asymmetry is the isolation effect: an item that differs from a uniform context is
remembered better (Hunt, *The subtlety of distinctiveness: What von Restorff really
did*, Psychonomic Bulletin & Review 2(1), 1995). The per-cell spread produces it
for free.

**Feeling multiplies, it does not add.** Emotional arousal acts on consolidation
*after* the event, through noradrenergic modulation of the amygdala — it makes an
event more likely to last rather than being another kind of interest (McGaugh, *The
amygdala modulates the consolidation of memories of emotionally arousing
experiences*, Annual Review of Neuroscience 27, 2004). So `cool = raw × (1 + 0.5 ×
|arousal|)`, sign-independent: being startled counts as much as being delighted.

**Uninterpretable frames score zero.** Attention is not monotone in surprise —
infants look away from events that are too simple *and* from those too complex to
follow (Kidd, Piantadosi & Aslin, *The Goldilocks Effect*, PLoS ONE 7(5), 2012) —
and about 39–40% of frames from a camera nobody is aiming are unusable through
blur, darkness or saturation (Doherty et al., CIVR 2008). A thumbnail whose mean
luma is outside 24–232 or whose spread is under 10 is refused, as is a frame the
model itself labelled `screen` or `nothing`.

**Habituation, in three layers.** Everything is read back out of `memory.jsonl`, so
restarting the daemon cannot make eve forget what it has already photographed.

| Layer | Rule | Why |
|---|---|---|
| The same picture again | brightness-normalised difference below 10 → refused outright, unless importance ≥ 9, which supersedes the old record | a lamp that toggles or a chair that fills every afternoon must not be photographed twice; Mem0's UPDATE rather than ADD (arXiv:2504.19413) |
| The same subject | score ÷ √(1 + photos in 14 days whose tags overlap by ≥ 0.5) | pseudo-counts: the bonus for a state falls as its count grows (Bellemare et al., *Unifying Count-Based Exploration and Intrinsic Motivation*, NeurIPS 2016) |
| The same spot | each recent photo here costs 0.45, recovering over 8 hours, floored at 0.05 | Stanley's 1976 habituation equation as used by Marsland, Nehmzow & Shapiro, *Novelty detection on a mobile robot using habituation* (arXiv:cs/0006007), whose robot stopped reacting to a corridor after a few passes; Kismet habituates its fixation the same way (Breazeal & Scassellati, IJCAI-99) |

The surprise bank habituates on its own: a frame is scored **before** it is banked,
so a change is surprising exactly once and is the new normal within about five
looks. Reading before writing is the order Wang et al. use for exactly this
(*Visual Memorability for Robotic Interestingness via Unsupervised Online
Learning*, ECCV 2020, which defines interestingness as the negative of memory
reading confidence).

**The bar moves with the week.** The threshold is the middle of eve's recent
scores plus 1.5 robust deviations, clamped to 0.40–0.75 and fixed at 0.55 until
there are twenty scores to measure. Surprise-gated robot memory sets its threshold
the same way (Gorlo, Wise, Speranzon & Carlone, *Worth Remembering: Surprise-Gated
Robot Episodic Memory*, 2026), as does EM-LLM's event segmentation (ICLR 2025:
rolling mean plus γ deviations). A lively week makes eve pickier; a dull one does
not make it desperate.

**A first is kept whatever the arithmetic says.** Novelty ≥ 9 with at most one tag
shared with anything in memory floors the score at 0.70. Human midbrain dopamine
responses code absolute stimulus novelty rather than rareness or emotional salience
(Bunzeck & Düzel, *Absolute coding of stimulus novelty in the human substantia
nigra/VTA*, Neuron 51(3), 2006).

**Taste eve learns rather than ships with.** Every claim you ★ in the diary
window is read as a standing vote for the words in it: a tag appearing in a starred
line adds 0.10 to what eve wanted, capped at 0.20. It is the one part of the gate
that changes with eve's life instead of being fixed at build time — the smallest
honest answer to the judge problem, where the metric of what matters is imported
and frozen rather than grown from experience.

**Budget.** At most 2 photos an hour and 8 a day, counted from the records, so a
restart cannot reset the allowance; photos are a strict subset of the 6 diary calls
an hour, so the only extra cost is disk. `EVE_PHOTOS_MAX` (300) and
`EVE_PHOTOS_MAX_MB` (100) cap the shelf; the oldest go first.

**The score.**

```
raw  = 0.30·novelty + 0.20·importance + 0.20·surprise + 0.15·presence + 0.15·want
cool = raw × (1 + 0.5·|arousal|) × quality × habituation
keep = cool ≥ threshold and the budget allows
```

`presence` is 1.0 for a person or an animal, 0.4 for something of eve's that
moved, 0 otherwise. `want` is eve's own 0–1 vote plus the taste bonus — one
bounded input among five, never the decision, because a model asked to score its
own output is not calibrated. Vector, the closest commercial relative, only ever
took a photo when told to (Digital Dream Labs KB, *Photos taken by Vector*, 2020);
this is the same feature with the shutter on the robot's own judgment.

**A second look, once it has the real picture.** The thought eve forms while
panning comes from the 160x120 streamed frame at the API's coarsest image
setting: enough to notice that something changed, not enough to say what it is.
On the bench a sewing machine across the room came back as "a white object".
So the pan stays cheap and the thinking happens afterwards: when a photo clears
the cool factor, eve sends that full-size picture back at high detail and asks
what is actually there. It replaces the caption, the tags and, when it says it
is confident, the diary sentence itself. Being allowed to look and still be
unsure is part of the deal — the prompt asks it to say what something resembles
rather than invent a specific object, and an unconfident answer leaves the
first thought standing. A failed second look costs nothing: the photo is kept
and the glance's sentence is written.

**Where a photo lives.** `photos/YYYY-MM-DD/HHMMSS-<record id>.jpg` under the notes
directory, with the path in `memory.jsonl` and an indented Markdown image line
under the diary line:

```
- 14:12 yaw=+20 pitch=40 — Someone brought a plant to my desk.
  ![](photos/2026-09-06/141207-118.jpg)
```

The diary line itself is unchanged, so every existing reader still parses it, and
the image line has no timestamp so both readers skip it. The macOS helper copies
the newest 24 referenced photos into the App Group container; the widget leads with
the newest one and the diary window has a Photos tab. `eve-bridge photos` lists
them, `photos open` opens the newest.

## 5. What the literature says (fetched 2026-09-06)

- **Model**: Breazeal 2003 (Kismet: arousal/valence/stance + drives),
  Mitchinson & Prescott (MiRo: one V/A point for emotion, mood and temperament;
  fixed stimulus transforms; light pulse 0.25/0.5/2.5 Hz for arousal),
  Fernández-Rodicio et al. 2022 (Mini: emotion decay k = 0.05, mood k = 0.001,
  modulation profiles), Paplu, Mishra & Berns 2022 (arXiv:2202.09813, motives with
  satisfaction thresholds, angle in V/A → 28 emotion words).
- **Curiosity as behaviour**: Houbre & Pieters 2024 (arXiv:2412.00152, habituation
  and inhibition of return), Oudeyer 2018 (arXiv:1802.10546, novelty and surprise
  as interest measures).
- **Expression**: Rogel et al. 2026 (arXiv:2605.12786, design for V/A legibility
  before naming emotions), Mishra et al. 2024 (arXiv:2410.14337, eyes alone are
  cropped eye-region expressions were recognized less accurately than full faces
  on Furhat; the study did not test adding head motion or colour), Casso et al. 2022 (arXiv:2209.00983, idle
  tempo must track arousal), Ribeiro & Paiva 2019 (arXiv:1904.02898, layer and
  blend animation channels), Wilms & Oberfeld 2018 (colour: arousal rises with
  saturation and brightness; valence best at medium saturation, green/blue > red).
- **Fast/slow split**: Wang et al. 2026 (MistyPilot, arXiv:2603.03640) and
  Broekens et al. 2023 (arXiv:2309.01664, LLM appraisal): the MCU runs the fast
  loop, the host LLM appraises slowly and nudges.
- **Diary**: Park et al. 2023 (arXiv:2304.03442, memory stream + recency/
  importance/relevance retrieval + reflection), Packer et al. 2023 (MemGPT/Letta
  core memory blocks), Chhikara et al. 2025 (Mem0, ADD/UPDATE/DELETE consolidation),
  Zhang et al. 2025 (Verbalized Sampling, arXiv:2510.01171, candidates with
  probabilities beat mode collapse), Kapur et al. 2026 (arXiv:2601.04609,
  specificity as picking the target out of a contrast set), Kang et al. 2026
  (arXiv:2604.12081, store only novel or emotionally salient scenes), Ichikura
  et al. 2023 (arXiv:2309.01948, robot diaries from joint experience read as more
  intimate), Pointeau & Dominey 2017 (iCub autobiographical memory).
- **Open source borrowed from**: Anki Vector's leaked emotion engine (five axes
  decaying to zero, simple mood types for animation selection), FluxGarage
  RoboEyes 1.1.2 (moods, curiosity, idle saccades, flicker), M5Stack-Avatar's
  six-expression vocabulary, xiaozhi-esp32's `{"emotion": tag}` transport.
