# App router

`bridge/src/eve_bridge/app_router.py` decides, before the chat model runs, whether a request is for one of
the devices eve drives: the TV, the lights, the music or the camera. When it is, the turn is told so and the
Mac agent cannot be started for it. The eval is `bridge/tools/app_route_eval.py`.

It is **off by default** (`app_router.SHIPPED = False`) until its blind holdout has been scored against the
bar at the end of this page.

## Why

On 2026-11-11, on a call, the owner said "Change TV input to HDMI 1." The chat brain is one model for every
door (the call, the Eve Link app, Telegram). It had the TV's tools and `start_task` on the same flat list.
The TV's block said HDMI inputs can't be switched, and did not say what to do instead. The model started the
Mac agent, which took over the Mac's screen for a request that was never about the Mac.

The logs show the same boundary failing 9 times in 58 Mac-agent starts: the TV three times, the Mac's camera
four times ("take a photo from my Mac" opened a camera app on the screen). No router decided "this request
belongs to a device". The [toolset router](toolset-router.md) opens the owner's accounts (calendar, mail,
drive); a device's tools ride every turn and were never routed.

## What it decides

Three things, in one Jev request per turn:

1. **Which thing the request is for**: one of the live devices, the Mac's own screen, or something else.
2. **Which of that device's methods** it asks for (power, volume, opening an app, a colour, a photo, ...).
3. **Whether the device can do it** with the tools eve has for it.

The answer is an `AppRoute`: `app` (`tv`, `lights`, `music`, `camera`, or empty for no claim), `can`,
`method`, `p_app` (the weakest of the claim's edges), Jev's milliseconds and its error. An error, a timeout,
a missing key or a missing answer is **no claim**, and a turn with no claim is byte for byte the turn it was
before the router existed.

## The devices come from the tool schemas

`app_router.THINGS` lists each device with its methods. Each method has the owner's words for it, the tool
that does it and the schema members it stands for:

| Device | App (`app_registry`) | Methods |
|---|---|---|
| `tv` | `firetv` | power, volume, keys, playback, open_app, youtube, typing, status, picture |
| `lights` | `lights` | power, brightness, colour, match, status |
| `music` | `spotify` | play, pause, skip, volume, transfer, mode, status |
| `camera` | `obsbot`, and eve's own `take_photo` | photo, aim, zoom, power, tracking, preset, status |

`test_app_router.py` checks the table against the modules: every value of each control tool's `action` enum
(and every parameter of `lights_set`) is in exactly one method, and every tool the module offers is some
method's. A new action without a method fails that test.

**Which devices are asked about is read from the tools the turn offers** (`app_router.live`). A method is
there only when its tool is offered, and a device is there only when it has a method. So a Mac without a TV
sends no question about a TV. The camera is there when the OBSBOT's tools are offered, or when `take_photo`
is: on a Mac with no OBSBOT, the camera is the Mac's own, and the one thing it can do is take a picture.

## One request, two layers

Jev's documentation recommends putting every question of a decision tree into one request and letting code
read only the chosen branch. Questions in one request are answered in parallel, so the second layer costs no
second round trip.

It is its **own** Jev call, beside the toolset router's and the grade router's. A question added to either
of those would change the call their gates were fitted on.

### The state

```json
{"about":    "A message the owner said or typed to their personal assistant, which works their computer and the devices listed under things. When a previous message is given, the new one may be a follow-up to it: judge what the new message asks for, reading the previous one only as context.",
 "things":   "<one line per live device>",
 "previous": "<the owner's previous message, at most 500 characters>",
 "request":  "<the message, at most 500 characters>"}
```

`things` holds one capability line per live device, built from its methods, for example:

> The TV can do exactly these things and nothing else: turn it on or off; set its volume, mute or unmute it;
> press its remote's keys (up, down, left, right, select, back, home, menu); play, pause, skip, rewind or
> fast-forward what is on; open an app on it by name; search YouTube on it; type into a search box on it;
> tell whether it is on, which app is in front and its volume; send a picture of what its screen shows.

The lines are in the state because a yes/no question (a Noul) sees only the state, never another question's
options. The repository learned that once already (`typed_ask.py`: a list placed only in a Choice's options
left a Noul reading 0.33 for a right answer and 0.37 for a missing one).

### Layer 1: which thing

- `which`, a Choice: "Which of these is the owner's request for?" Its options, in order: something else, the
  computer itself, then each live device. Jev leans to the first option, so the option that claims nothing is
  first.
- `for_mac`, a Noul: is the owner asking for something to be done on the computer itself.
- `for_tv`, `for_lights`, `for_music`, `for_camera`, a Noul each, only for live devices: is the owner asking
  for something to happen on that device, or to be told something about it. Each says "yes also when it is
  something the device may not be able to do", so a request the device can't do is still the device's.

### Layer 2: which method, and can it

Per live device, in the same request:

- `how_<device>`, a Choice over that device's methods.
- `can_<device>`, a Noul: the device's capability line again, then "Is what the owner asks of the device one
  of those things?"

Code reads only the claimed device's two answers.

Every question is in the owner's words for things. None names a tool, a model or an app
([AGENTS.md](../AGENTS.md) rule 7: a name in a prompt is an option the model may fill a slot with); a test
checks it.

## The rule

A Choice always picks something, even when nothing fits, so a Choice alone is never a claim. A device is
**claimed** only when all three hold (`app_router.claims`):

1. the Choice picks it, with a probability at or over the device's `choice` gate;
2. the device's own Noul is at or over its `noul` gate;
3. the device's Noul leads the Mac's Noul by at least its `lead` gate.

Then `can` is true when the device's `can_` Noul is at or over its `can` gate, and `method` is the method
Choice's pick (kept only when `can` is true). `p_app` is the minimum of the Noul and the Choice's
probability. Edges are never multiplied.

No claim when: Jev erred or timed out, an answer is missing or of the wrong type, the Choice picked the
computer or something else, any of the three edges is short, or the claimed device has no tool on this turn.

The gates are in `bridge/src/eve_bridge/app_gates.json`, read once at import. Until the first fit they hold
the starting values (`noul` 0.7, `choice` 0.5, `lead` 0.0, `can` 0.5 for every device).

## What a claim changes in the turn

The router runs in `TelegramInlet._think` as a third thread beside the toolset router's call and the
prompt's building, with the same 0.8 s budget, so it usually adds no wall time. It is not run on a rundown
turn, on a turn with no words, or in eve lite.

A claim never changes the cached prefix. The tool list and the instruction blocks stay byte for byte. Two
things change:

**One line in the turn's developer note** (where the call's note already rides, after the history). Code
builds it from the claim, with only the tools the turn really offers:

- When it can: "This request is for the TV. Its tools this turn: firetv_status, firetv_control,
  firetv_screenshot. It reads as a request to open an app on it by name (firetv_control). Use them now; when
  they can't do what was asked, tell the owner so in one line."
- When it reads as something the device can't do: "This request is for the TV. Its tools this turn: ... It
  reads as something the TV can't do: check it against what those tools take, and when it can't be done with
  them, tell the owner so in one line."

**A guard on `start_task`**, beside the existing mail guard. While a device is claimed for the turn, a
`start_task` call is refused in every round, and the model gets another round with this result:

> not started: this request is for the TV, which you work with firetv_status, firetv_control,
> firetv_screenshot. Use one of those this turn; when they can't do what the owner asked, tell the owner so
> in one line.

On a call, the short "On it." that a started task gets is not said for a refused one. Only `start_task` is
refused; every other tool runs as before. If the owner types more into a running turn (`EVE_CHAT_STEER`),
the guard is dropped for the rest of that turn, because the claim was read from the first message alone.

The log has one line per routed turn, names and numbers only, never the message:

```
telegram: app router: tv (cannot, p 0.95); jev 212 ms
telegram: app router: none (leaning mac 0.88); jev 190 ms
telegram: app router: none; jev 801 ms (timeout)
telegram: start_task refused: the request is for a device (tv)
```

### The limits are in the prompt too

The TV's block used to state the HDMI limit with no instruction. It now reads: "Limits: HDMI inputs can't be
switched, search works in YouTube only, and in deep standby the TV can't be reached (the remote or Alexa
wakes it). When the owner asks for one of these, say so in one line." Spotify's block gained a matching
sentence for what its tool has no action for (liking or saving, the queue, editing a playlist, shuffle,
repeat, seeking). These two changes are in every turn, whatever the switch says. The lights' block was left
alone: it is part of the owner golden snapshot, which is recorded only from main's code.

## Settings

| Variable | Values | Effect |
|---|---|---|
| `EVE_APP_ROUTER` | `auto` (default) | On when `app_router.SHIPPED` is True. It is False until the bar below holds. |
| | `on` | On before or despite the bar, for a live trial. |
| | `off` | No call, no note line, no guard. |
| `EVE_JEV_ROUTE`, `OPENROUTER_API_KEY` / `TYPESAFE_API_KEY` | | The Jev route (`jev.route_config`). With no key, there is no router. |

`1`, `true` and `yes` read as `on`; `0`, `false` and `no` as `off`; anything else is `auto` with a warning.
eve lite forces it off (`profile.LITE_FORCED`), and its inlet builds no router even when one is switched on
by hand.

Privacy: while the router is on, the owner's message and the previous message go to Jev on every routed
turn, as they already do for the toolset router. Nothing else of the turn is sent.

## Fitting the gates

`tools/app_route_eval.py --fit` asks live Jev once per tuning case
(`bridge/tests/fixtures/routes/app_tuning.json`) and keeps the answers in `app_tuning_answers.json`. An
answer is reused only when the case's id, a hash of its two texts and a hash of everything else Jev is asked
(`app_router.fingerprint`: the about line, the capability lines, every question) all match. A reworded
question is asked again. The toolset eval's cache is keyed by id alone, and a new question there silently
reads 0.0.

A case is `{"id", "door", "previous", "text", "app": "tv|lights|music|camera|mac|none", "can": true|false|null,
"why"}`. An optional `"method"` is scored when a case carries one.

The rule (`app_router.fit`), per device:

1. A gate triple (`noul`, `choice`, `lead`) is allowed only if it claims **no** tuning case whose truth is
   something else: a Mac case, a "neither" case or another device's case.
2. Of the allowed triples, keep those that claim the most of the device's own cases.
3. Of those, take the strictest, S (the highest gates).
4. For each gate alone, find how far below S it could go with the other two at S and rule 1 still holding.
   Take the grid point halfway, rounded to the strict side. If the three halfway gates together break rule 1,
   keep S.

Zero wrong claims comes before recall: when a Mac case reads exactly like a device's weakest case, the
device case is given up. A device that no gates can claim without a wrong claim is never claimed, and the
fit says so. `--fit` stops without writing if the fitted gates claim any Mac or "neither" tuning case.

The `can` gate: it never goes above the lowest `can_` Noul of a tuning case that the device can do, so no
doable tuning case is called undoable. Under that limit it sits halfway to the highest Noul of a case the
device can't do, rounded low.

The grids: `noul` and `choice` on 0.05 to 0.95 in steps of 0.05; `lead` on -1.0 (never binds), then -0.5 to
0.9 in steps of 0.1.

## Commands

From `bridge/`, with the Jev key scoped to the one command:

```bash
env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) .venv/bin/python tools/app_route_eval.py --fit
env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) .venv/bin/python tools/app_route_eval.py --holdout
.venv/bin/python tools/app_route_eval.py --report
```

- `--fit` writes `app_gates.json` and prints `APP_ROUTER_FIT_COMPLETE`.
- `--holdout` asks live Jev **once** over `app_holdout.json`, keeps the answers in
  `app_holdout_answers.json` and the verdict in `app_holdout_result.json`, and refuses to run again once the
  result file exists. It also refuses gates that are not fitted, or that were fitted on other questions. If
  Jev answers no case at all (an outage), nothing is kept and the holdout is not spent.
- `--report` recomputes the bar from the kept answers with no network call. It prints each bar line with its
  measured value, then `APP_ROUTER_BAR_OK` when lines 1 to 4 all hold; otherwise `APP_ROUTER_BAR_FAILED` and
  exit 1. A gates file that changed since the holdout fails the report: that is a refit after the holdout.

A worktree has no `bridge/.venv`; there, use `uv run --project bridge python bridge/tools/app_route_eval.py`
from the root.

### The end-to-end arms

`tools/intent_eval.py` scores which tool the chat brain picks. Two additions serve this router:

- `--door call` scores the call door (the turn is marked as a push-to-talk call's, so the call's note rides
  it). A case may carry `"door": "call" | "app"` to choose for itself.
- `--app-router on|off` forces `EVE_APP_ROUTER` for one arm. Each case's line then shows the claim
  (`router=tv:cannot,refused-start_task-x1`), and the totals add an `APP_ROUTER` line. A refused `start_task`
  starts nothing, so it is not one of the case's actions; the next action is scored.

```bash
cd bridge
env $(grep -E '^(OPENROUTER_API_KEY|TICKETMASTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) \
  uv run python tools/intent_eval.py CASES.json --app-router off > base.txt     # BASE
env $(grep -E '^(OPENROUTER_API_KEY|TICKETMASTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) \
  uv run python tools/intent_eval.py CASES.json --app-router on > new.txt       # NEW
```

## The pre-registered bar

Written 2026-11-11 by the orchestrator, before any tuning or holdout result existed. Copied verbatim.

> Router level, scored ONCE on the blind holdout (writer B, 120 cases, never seen by the builder), gates fitted on the tuning set only:
> 1. On holdout cases whose truth is a device app: the router names that app (right app, whether or not it can do it) on >= 0.90 of them.
> 2. On holdout cases whose truth is the Mac: the router claims a device app (which would refuse the Mac agent) on <= 0.03 of them.
> 3. On holdout cases whose truth is neither: the router claims a device app on <= 0.05 of them.
> 4. Jev latency p95 <= 800 ms over the holdout run; an error or timeout yields no claim.
> SHIPPED is True only if all four hold. Reported, not gated: can/can't accuracy, method accuracy, per-app confusion.
>
> End to end (intent_eval, blind set of 40 written by writer C, app door and call door, base arm = router off, new arm = router on, each arm scored once):
> 5. NEW: zero device-app cases end in the Mac agent (HARD_NEGATIVE_ACTED = 0 for those cases).
> 6. NEW: Mac-task cases still reach the Mac agent on >= 0.90.
> 7. NEW overall accuracy >= BASE overall accuracy - 3 points (the documented noise floor).
> The router is switched on by default only if 1-7 all hold. The bar is never changed after results exist.

### Verdict

| Line | Measured | Verdict |
|---|---|---|
| 1. device cases named right, >= 0.90 | | VERDICT: pending |
| 2. Mac cases claimed, <= 0.03 | | VERDICT: pending |
| 3. neither cases claimed, <= 0.05 | | VERDICT: pending |
| 4. Jev p95, <= 800 ms | | VERDICT: pending |
| 5. device-app cases ending in the Mac agent, 0 | | VERDICT: pending |
| 6. Mac-task cases reaching the Mac agent, >= 0.90 | | VERDICT: pending |
| 7. NEW overall >= BASE overall - 3 points | | VERDICT: pending |

Reported, not gated: can/can't accuracy, method accuracy, per-app confusion: VERDICT: pending.

`SHIPPED`: VERDICT: pending (False until lines 1 to 4 hold; on by default only if 1 to 7 all hold).

## What is not covered

- **The gates are fitted with every device live.** A Mac that lacks one sends fewer options to the Choice,
  so the remaining options' probabilities read a little higher than the fit saw. The device's own Noul and
  its lead over the Mac still have to clear, but the fit does not measure this.
- **The wake-word voice** (`voice_agent.py`) is a separate brain with its own prompt. This router does not
  run there.
- **Apps without tools** (Pika) and apps that are not devices (Debrief, Repos) are not in the table.
- **Slot values** ("HDMI 1", "20 percent") stay the chat model's job. The method is a hint, not a call.

## Tests

`bridge/tests/test_app_router.py`: the table against the schemas, the questions, the parsing, the rule, the
fit, the eval tool, and the turn on fakes: the incident on a call (claimed for the TV, `start_task` refused,
no agent started, one spoken line), a claim that leads to `firetv_control`, and request bodies that are
byte for byte the same with no router, no claim, an error and a timeout.
