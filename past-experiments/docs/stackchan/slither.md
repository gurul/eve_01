# slither.io: a real-time eval of the local decider

**Archived 2026-09-26** to `past-experiments/`, with its tool and test. The local decider it measured was retired (the fast lane's Laya decider was removed on 2026-09-24), so nothing runs it. Paths below are the ones it had in `bridge/`.

Not a product feature. `bridge/tools/slither_eval.py` measures how the local
typed-decision model (laya-mlx, the checkpoint the eye expressions use; the
computer-use fast lane used it until 2026-09-24) holds up at twenty decisions a
second on a live game against other people. The
pattern is the sampling-rollout one from drone racing: **code samples candidate
actions, rolls each forward, scores risk and gain; the model picks among the
survivors; a deterministic shield executes only safe picks.** Kills are the
primary metric, length second. Nothing here runs inside the daemon.

## Run

```
cd bridge
.venv/bin/python tools/slither_eval.py --dry-run --secs 10        # read, plan, print; move nothing
.venv/bin/python tools/slither_eval.py --episodes 3 --max-secs 300
```

The eval launches its **own Chrome** (a throwaway profile, a remote-debugging
port) on slither.io and reads the game's state over the DevTools protocol: one
`Runtime.evaluate` per decision runs the feature extraction inside the page and
returns a compact JSON (0.8 ms round trip). Input is **real HID input** through
Quartz `CGEventPost` (0.004 ms a post; pyautogui's move cost 12.8 ms): the mouse
is moved to steer and the button is **held to boost**, so do not touch the mouse
while it runs. It counts down three seconds before each episode and releases the
button on every exit including Ctrl-C. Needs internet and the checkpoint at
`~/.config/eve-bridge/models/laya-multilingual-mlx`.

## What the game exposes (build game1107249518.js, verified 2026-09-21)

`window.slither` (own snake: `xx, yy, ang, sp, ssp, sc, sct, fam, pts`),
`window.slithers` (every snake with `pts[]`, `dead_amt`), `window.foods`
(`xx, yy, sz`), `window.grd` (map radius; the centre is `(grd, grd)`),
`window.playing`, `window.rank`, the score tables `fpsls`/`fmlts`. The `snake`
/ `snakes` / `setAcceleration` names from older bots do not exist. Measured:
cruise 180 world units/s, boost 330 units/s (speed 14), a 90° turn in ~0.35 s
at scale 1, width `round(sc·29)`; the y axis points down and `ang` grows
clockwise on screen. The game refuses to boost at the minimum length (10).

## What the model sees

One `predict` per decision, two questions in the batch:

- **State**, always opening with the controls so the model knows what a heading
  and a boost mean here:
  `Slither.io. The snake steers toward the mouse. Holding the mouse button boosts speed but burns length. Length 120. Boosting: no. Nearest big head 300 behind. Smaller snake (len 5) 282 right. Wall: far. Centre 2000 away. Kills 2.`
- **move** (choice, at most 8 options, at least 4 safe): the top candidates by
  code score, each a fragment — `Safe 900. Food 12 ahead. Best.`,
  `Danger: big head 80. Collision.`, `Cut off smaller snake (len 12) — boost`,
  `Trap: circle the small snake (len 9)`, `Dead snake food (mass 80) — boost`,
  `Food cluster (mass 70)`, `Head to the map centre (dist 3500) — boost`.
- **boost** (noul): "Should the snake boost now by holding the mouse button?
  Boosting is faster but burns length; boost to cut off a smaller snake, grab a
  big food cluster, or escape a bigger snake."

Length is the game's own score (what the screen shows).

## Candidates, rollout, phases

Sixteen relative headings × {cruise, boost}, plus cut-off intercepts of smaller
snakes (predict the target head's path; reach a point 100 ahead of it before it
does; kill score = P(intercept) × its length), a trap (circle a smaller snake
inside our reach), death-burst rushes (a dead snake's food, unless a bigger head
is closer), the best three food clusters (mass²/distance) and the map centre
(where the players are; its score rises with our distance). Every candidate is
rolled forward 0.6 s against enemy body points, the enemies' **predicted** head
positions (bigger heads inflated by 60) and the wall (radius `grd − 300`); risk
is the minimum clearance, gain is the food swept within 150 of the path (free).

Objective, a strict priority in code (Guru, 2026-09-21): **(1) TERMINATE**
— whenever a feasible kill exists (a smaller head within reach whose intercept
we win), the cut-off / trap option outranks every food option, in every state,
from length 12 up (a boost must be affordable); **(2) EAT** — otherwise the best
food cluster or death burst, and food within 150 of the path is always free;
**(3) SURVIVE** — the shield below, with the tolerance against *bodies* relaxed
to 0.7 × DANGER for at most 0.6 s while a cut-off is being executed (never
against a head, never the wall). The mode (`terminate` / `eat`) is timed per
episode. Hard turns (beyond 67°) and reverse are offered only when danger is
ahead.

## Planner, controller, reflexes (the minecraft-agent split)

Three layers, like rmalde/minecraft-agent's planner / JEV / reflex split:

- **Planner** — `gpt-6-astra` at low reasoning effort, in its own background
  task, one call as soon as the previous returns plus a 2 s floor (measured
  5-6 s a call). It gets a structured situation summary (our length and speed,
  distance to the centre, the three nearest smaller snakes with length,
  distance, relative heading and closing speed, the nearest bigger heads, the
  best food clusters and any burst, recent shield interventions and reflexes,
  the current objective and how long it has run, the last five log lines) and
  answers a strict JSON directive: objective (`hunt` / `eat` / `burst` /
  `centre` / `escape`), a target (a snake id, a cluster or burst or point by
  dx/dy, or none), a boost policy (`aggressive` / `normal` / `conserve`), an
  optional waypoint and an 80-character note for the log.
- **Controller** — laya at 20 Hz keeps picking the concrete heading and boost
  among the code-built candidates; the planner's target gets a fixed bonus
  (above ordinary food, below a feasible kill or a death burst), its boost
  policy widens or narrows the boost gate. Between planner updates the last
  directive keeps being executed. Never on the tick path: the planner's
  latency does not touch the loop rate.
- **Reflexes** — code, always on, override both: `wall` (the edge within 250
  along the commanded heading → turn to the centre, boost off), `head_crossing`
  (a bigger head's predicted path within collision reach of ours inside 0.5 s →
  straight away, boost), `body_clearance` (no safe candidate → the clearest),
  `stuck` (heading swung more than 180° over 3 s with net displacement under
  150 → a 1.5 s straight run toward the waypoint or the centre). Every firing
  is logged with its rule.
- **Reflection** — after each episode, `gpt-6-astra` at high effort reads the
  narrative log, the planner's decisions, the change log and the episode table,
  plus the knob schema (name, value, bounds, meaning) and the rule toggles, and
  returns one structured change. It is applied only inside the bounds (a
  refusal is logged), persisted to the series settings file and written to the
  change log with the diagnosis. GPT text never reaches `eval`, `exec` or the
  mouse: it only picks from the schema.

```
.venv/bin/python tools/slither_eval.py --episodes 1 --max-secs 300 --series series.jsonl \
    --settings settings.json --changelog CHANGELOG.md --reflect        # one episode of a learning series
.venv/bin/python tools/slither_eval.py --no-gpt --episodes 3            # code only, no planner, no reflection
```

The narrative log (`<stamp>-ep<N>.log`, beside the JSONL) carries `[planner]`,
`[controller]`, `[reflex]`, `[shield]`, `[burst]` and `[death]` lines plus a
`[state]` line every 2 s.

## Shield, commitment, boost

## Measured (2026-09-21)

Loop: 19.5-19.6 decisions/s on every episode, loop p95 20-24 ms (read 0.8 ms,
plan 0.5 ms, model 17 ms, act 0.03 ms), zero state-read failures; the planner
(gpt-6-astra, low effort) answered in 3-4 s p50, 5-16 s p95, 12-46 calls per
episode, off the tick path.

The ten-episode learning series (one change per episode, GPT reflection from
episode 3, a change that made things worse reverted before the next):

| ep | peak | kills | survival s | food/min | bursts seen/collected | change played |
|---|---|---|---|---|---|---|
| 1 | 948 | 6 | 113 | 516 | 0 | starting state |
| 2 | 45 | 0 | 27 | 51 | 0 | burst piece threshold 20 → 10 |
| 3 | 240 | 0 | 62 | 221 | 747/35 | planner + reflexes + narrative log |
| 4 | 402 | 0 | 99 | 250 | 875/60 | DANGER_BASE 60 → 120 |
| 5 | 217 | 1 | 60 | 215 | 0 | HORIZON 0.6 → 1.0 (reverted) |
| 6 | 177 | 0 | 59 | 174 | 600/26 | stuck reflex off (reverted) |
| 7 | 168 | 1 | 43 | 250 | 899/48 | CLEARANCE_WEIGHT 0.05 → 0.2 (reverted) |
| 8 | 581 | 3 | 163 | 226 | 708/50 | stuck reflex off (retest, kept) |
| 9 | 911 | 2 | 154 | 375 | 6986/483 | BURST_PRIORITY 500 → 1500 (kept) |
| 10 | 171 | 1 | 56 | 172 | 1892/63 | relaxation off (reverted) |

The tenth game was not the best: peak 171 against 911 in episode 9 and 948 in
episode 1. Single-episode peaks on identical code ranged 65-948 in this
session, so no single change is proven; the shipped series settings are
episode 9's (DANGER_BASE 120, BURST_PRIORITY 1500, stuck reflex off). Every
episode died the same way: boxed in at the centre with every candidate unsafe
for the last 1-3 s, then a head or body collision. The numbers are reported,
not passed or failed (`GATES.md` G15).

## Recordings

One JSONL per episode under `~/.config/eve-bridge/slither-runs/`, one line
per decision: the state sentence, the menu with the code scores, the model's
probabilities and boost score, mode, proposed vs executed, held / switched,
the smoothed heading, boost and why, and read / plan / model / act / loop
milliseconds. A `read_error` line marks a skipped tick.

```
jq -r '[.n, .kills, .length, .mode, .executed, .boost, .boost_reason, .model_ms] | @tsv' <run>.jsonl | head
```
