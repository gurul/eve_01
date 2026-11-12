# App router

`bridge/src/eve_bridge/app_router.py` decides, before the chat model runs, whether a request is for one of
the devices eve drives: the TV, the lights, the music or the camera. When it is, the turn is told so, and the
first call that would start the Mac agent for it is refused once. The eval is `bridge/tools/app_route_eval.py`.

It is **off by default** (`app_router.SHIPPED = False`): [round 2](#round-2), scored on 2026-11-11, did not hold
the bar (it claimed 4 of 40 hard Mac cases for a device). Round 1 held every
line of the bar on 2026-11-11, and a review of the branch then changed the rule on round 1's data: a floor under
the gates, a guard that refuses once, three rules for a bare Mac app, and new wording for a request a device can't
do. `EVE_APP_ROUTER=on` runs it as a live trial.

Before Jev is asked, one code rule answers: a message that is nothing but opening or quitting an app installed on
this Mac ("open Spotify", "quit Slack") is the Mac's, and no device claims it
([`app_router.mac_app_only`](#the-mac-app-rule)). A message that says where ("open Spotify on the tv") is still
asked.

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

Every question is in the owner's words for things. None names one of eve's tools, a model, or a device's
brand name ([AGENTS.md](../AGENTS.md) rule 7: a name in a prompt is an option the model may fill a slot with).
The test checks exactly that: a fixed list of every tool name eve offers, the model names and the devices' brand
names, none of which may appear in the state or the questions. It does not check that no app is named, and one
is: the TV's method words say "search YouTube on it". That is a right name, the one app the TV's search works in.

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

### The floor

A claim's gates are never looser than `app_router.FLOOR`: `noul` 0.5, `choice` 0.5, `lead` 0.0. It is a rule in
code. `app_router.load_gates` takes the higher of the fitted value and the floor, per field, whatever the file
says. The `can` gate has no floor; it is left as fitted.

| Device | Fitted (`app_gates.json`) | What a turn uses |
|---|---|---|
| `tv` | noul 0.5, choice 0.5, lead -0.1 | noul 0.5, choice 0.5, lead 0.0 |
| `lights` | noul 0.4, choice 0.3, lead -0.2 | noul 0.5, choice 0.5, lead 0.0 |
| `music` | noul 0.15, choice 0.35, lead -0.5 | noul 0.5, choice 0.5, lead 0.0 |
| `camera` | noul 0.2, choice 0.35, lead -0.4 | noul 0.5, choice 0.5, lead 0.0 |

Why: on the tuning set the Choice never picked the lights, the music or the camera for a Mac case or a "neither"
case. Nothing held their gates up, so step 4 of the fit put each one halfway to the bottom of the grid. Gates that
low are the Choice alone deciding, and this repository's own lesson is that a relative Choice is not a gate. The
music could be claimed with its own Noul at 0.15 and the Mac's Noul at 0.65. The fit found no negative; that is
not evidence that there is none: no tuning or round 1 Mac case says "record my screen" or "play the lecture video"
without a Mac word.

`app_gates.json` is kept as fitted. It is the fit's record, and round 1 was scored with it. The floor was chosen
after round 1, on its data: on the kept answers it gives up 6 of 160 tuning and 3 of 80 round 1 device claims
(77/80), and claims no Mac or neither case in either set. Those are seen-data numbers, not a verdict.

### The Mac app rule

`app_router.mac_app_only(text)` answers before Jev is asked: no claim. Jev read a bare "open Spotify" as the
TV's in a live check after round 1. Three rules:

1. **Nothing but a launch or a quit.** "Open Spotify" and "quit Slack" are. "Open Spotify and play something" has
   a rest, and "open Spotify on the tv" names a place: neither is, and both are asked.
2. **The planner's own wording.** It reads `task_router.bare_launch_or_quit`, which is the Mac planner's own
   verdict (`task_router.classify`) that a request is answered by a launch or a quit alone. So the polite and
   spoken lead-ins the planner knows count: "can you open Spotify", "okay, open Spotify.", "could you quit Slack?".
3. **Not for an app whose name is a word for a device or the home.** On a Mac these are the apps named TV, Music
   and Home (`app_router.DEVICE_WORD_APPS`). "Open the tv" or "start the music" may be the device, so they go to
   Jev as they did before the rule.

The rule can only remove claims. In a turn it reads the Mac's installed apps; the tests and the eval pass a list.

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
  reads as something the TV can't do. If that is so, tell the owner in one line that the TV can't do it." The
  line does not ask the model to call a tool to check. In round 1's end-to-end run, both cases claimed this way
  ended in the TV's control tool.

**A guard on `start_task`**, beside the existing mail guard. It is a nudge, not a wall. The first `start_task`
call of a claimed turn is refused, and the model gets another round with one of these results:

> not started: this request is for the TV, which you work with firetv_status, firetv_control,
> firetv_screenshot. Use one of those this turn; when they can't do what the owner asked, tell the owner so
> in one line.

> not started: this request is for the TV, and it reads as something the TV can't do. Tell the owner so in one
> line.

The second is for a claim the device can't do. It names no tool, so it does not send the model to a device the
router judged unable.

The guard refuses **at most once per turn**. A second `start_task` call in the same turn goes through: the model
has read the claim twice and still means the Mac, and the router can be wrong. The guard also lifts, with nothing
refused, **as soon as one of the claimed device's own tools has run** in the turn. "Turn off the lights and open
the budget spreadsheet" is one request with two halves; the Choice has to pick one, and once the lights' tool
has run, the rest may be the Mac's.

On a call, the short "On it." that a started task gets is not said for a refused one. Only `start_task` is
refused; every other tool runs as before.

**New words from the owner drop the claim.** The claim was read from the first message alone. When the owner
types more into a running turn (`EVE_CHAT_STEER`), or withdraws a gated question by typing something else
(`EVE_GATE_WITHDRAW`), three things happen: the guard is dropped, the claim's line is taken out of the note (the
note is the one from before the line, so the line never sits right before the new words), and the turn reports no
claim.

The turn waits for the router for its 0.8 s budget plus 0.2 s, and no longer. An exception or a timeout on that
wait is no claim.

The log has one line per routed turn, names and numbers only, never the message:

```
telegram: app router: tv (cannot, p 0.95); jev 212 ms
telegram: app router: none (leaning mac 0.88); jev 190 ms
telegram: app router: none; jev 801 ms (timeout)
telegram: start_task refused: the request is for a device (tv)
```

### The limits are in the prompt too

The TV's block used to state the HDMI limit with no instruction. It now reads: "Limits: the TV can't switch
HDMI inputs, and it can't search inside any app except YouTube. When the owner asks for either, say so in one
line. In deep standby the TV can't be reached until the remote or Alexa wakes it." The one-line instruction is
attached to the two things the TV can't do. A YouTube search and turning the TV on are things it can do, so they
are not covered by it; round 1's wording listed all three and said "one of these".

Spotify's block ends: "Limits: its actions are exactly the ones listed above. When the owner asks for anything
else, tell them so in one line." The set is stated positively and no missing action is named (round 1's wording
named six, which the owner's rule about names in a prompt forbids).

These two sentences are in every turn, whatever the switch says. The lights' block was left alone: it is part of
the owner golden snapshot, which is recorded only from main's code.

## Settings

| Variable | Values | Effect |
|---|---|---|
| `EVE_APP_ROUTER` | `auto` (default) | On when `app_router.SHIPPED` is True. It is False: [round 2](#round-2) did not hold the bar. |
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
env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) .venv/bin/python tools/app_route_eval.py --holdout2
.venv/bin/python tools/app_route_eval.py --report
```

- `--fit` writes `app_gates.json` and prints `APP_ROUTER_FIT_COMPLETE`.
- `--holdout` asks live Jev **once** over `app_holdout.json`, keeps the answers in
  `app_holdout_answers.json` and the verdict in `app_holdout_result.json`, and refuses to run again once the
  result file exists. It also refuses gates that are not fitted, or that were fitted on other questions. If
  Jev answers no case at all (an outage), nothing is kept and the holdout is not spent.
  This is round 1. It was scored with the gates as fitted, before the floor and the Mac app rule existed.
- `--holdout2` is round 2: live Jev **once** over `app_holdout2.json`, answers in `app_holdout2_answers.json`,
  the verdict in `app_holdout2_result.json`, and a second run is refused. It scores the **live path's whole
  decision**: `mac_app_only` first, then the gates as a turn loads them (fitted, never under the floor). The
  installed apps `mac_app_only` reads are a fixed list of 17 common Mac app names in the tool
  (`app_route_eval.MAC_APPS`), so the score is the same on every host. The result file records that list, the
  floor, the fitted gates and the gates used. Round 1's three files are never rewritten.
- `--report` recomputes the bar from the kept answers of the **latest scored round**, with no network call. It
  prints each bar line with its measured value, then `APP_ROUTER_BAR_OK` when lines 1 to 4 all hold for that
  round; otherwise `APP_ROUTER_BAR_FAILED` and exit 1. Once round 2 is scored, round 1 is printed above it as
  history, marked as seen data; it decides nothing. While only round 1 is scored, the report is round 1's, with
  a note that a turn now loads floored gates, so the numbers are not the live rule's.
- A gates file that changed since a round was scored fails the report for that round: that is a refit after
  the holdout. The floor is not a change of that file. It is a code rule applied at load, and a round scored
  after it records the floor, so a floor that is moved later fails the report too.

A worktree has no `bridge/.venv`; there, use `uv run --project bridge python bridge/tools/app_route_eval.py`
from the root.

### The end-to-end arms

`tools/intent_eval.py` scores which tool the chat brain picks. Two additions serve this router:

- `--door call` scores the call door (the turn is marked as a push-to-talk call's, so the call's note rides
  it). A case may carry `"door": "call" | "app"` to choose for itself.
- `--app-router on|off` forces `EVE_APP_ROUTER` for one arm. Each case's line then shows the claim
  (`router=tv:cannot,refused-start_task-x1`), and the totals add an `APP_ROUTER` line. A refused `start_task`
  starts nothing, so it is not one of the case's actions; the next action is scored. The guard refuses once, so
  a second `start_task` in the same case is an action and is scored as one.

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

### Verdict, round 1

| Line | Measured | Verdict |
|---|---|---|
| 1. device cases named right, >= 0.90 | 80/80 = 100% | pass |
| 2. Mac cases claimed, <= 0.03 | 0/24 | pass |
| 3. neither cases claimed, <= 0.05 | 0/16 | pass |
| 4. Jev p95, <= 800 ms | p95 418 ms, p50 262 ms, 0 errors | pass |
| 5. device-app cases ending in the Mac agent, 0 | NEW 0/24 (BASE 1/24), first action only | pass (see the two notes on it) |
| 6. Mac-task cases reaching the Mac agent, >= 0.90 | NEW 10/10 | pass |
| 7. NEW overall >= BASE overall - 3 points | NEW 36/40 = 90.0%, BASE 34/40 = 85.0% | pass |

Scored once on 2026-11-11: the router holdout (`app_holdout.json`, 120 cases, writer B) and the end-to-end set
(`docs/evals/app-router-2026-11-11/`, 40 cases, writer C, both arms). Gates were fitted on `app_tuning.json` only.

Reported, not gated: can/can't right on 76/80 named device cases (19/23 of the ones the device can't do); no
dataset case carries a method label, so method accuracy is not measured; no device was confused with another.

Three things a reader should know before trusting these numbers:

- **Line 5 was scored on the first action only.** The harness reads the first tool a case's turn calls and does
  not follow what that tool starts. On first actions the line held: no device case's first action was
  `start_task` (0 of 24).
- **One NEW case ended in a chief job.** Line 5's parenthesis names `HARD_NEGATIVE_ACTED`, which the harness counts
  for any action on a case that should have none. That count was 4 in NEW and 9 in BASE. Three of NEW's four are a
  device's own tool called on a request the device can't do (an accepted answer in this set). The fourth, e2e-19,
  was claimed for the lights and ended in `take_on`, a chief job. That is neither a device tool nor the guarded
  `start_task` call. A chief job can start the Mac agent through its own path (`chief.py`), which this guard does
  not cover and the eval does not follow, so "zero cases end in the Mac agent" is shown for first actions and not
  for what a chief job did next.
- **The end-to-end effect is small.** Both arms carry the new Limits sentences, and with those alone the chat
  model sent 1 of 24 device cases to the Mac agent. The router took that to 0. The guard refused no call in the
  run: the note was enough.
- **One rule was added after the holdout.** A live check on real requests showed Jev reading a bare "open
  Spotify" as the TV's. `app_router.mac_app_only` now answers before Jev: a message that is nothing but opening
  or quitting an app installed on this Mac is never claimed. It can only remove claims. On this Mac it fires on
  6 holdout and 13 tuning cases, every one of them a Mac case, so no scored line moves; it is a rule added on seen
  data all the same, and it has no blind test of its own.

Lines 1 to 7 all held in round 1, and `SHIPPED` was True for it. It is False again: see round 2.

## Round 2

Round 1's sets are **seen data** now. A review of the branch read the holdout's answers and the end-to-end runs,
and the changes below were made after round 1, on what it showed:

- **The floor** under the fitted gates (`app_router.FLOOR`), because three devices' gates had no negative to hold
  them up.
- **The guard** refuses once, lifts when the claimed device's own tool has run, and is dropped with the note
  line and the claim when the owner steers the turn or withdraws a gated question.
- **The Mac app rule**: nothing but a launch or a quit, the planner's own wording, and not for the apps named
  TV, Music and Home.
- **The Limits wording** in the TV's and Spotify's blocks.
- **The refusal and the note line** for a request the device can't do: neither tells the model to use a tool.

A rule changed on seen data has no blind verdict. So `SHIPPED` is False, and **round 2 decides it**: a new
router holdout (`app_holdout2.json`, scored once by `--holdout2` with the live path's whole decision) and a new
end-to-end set, both written blind, against the same bar. The bar is not changed.

| Line | Measured | Verdict |
|---|---|---|
| 1. device cases named right, >= 0.90 | 77/80 = 96.2% | pass |
| 2. Mac cases claimed, <= 0.03 | 4/40 = 10.0% | **fail** |
| 3. neither cases claimed, <= 0.05 | 1/20 = 5.0% | pass |
| 4. Jev p95, <= 800 ms | p95 455 ms, p50 269 ms, 0 errors | pass |
| 5. device-app cases ending in the Mac agent, 0 | NEW 1/26 (BASE 3/26, main 3/26) | **fail** |
| 6. Mac-task cases reaching the Mac agent, >= 0.90 | NEW 15/16 (BASE 14/16, main 15/16) | pass |
| 7. NEW overall >= BASE overall - 3 points | NEW 41/48 = 85.4%, BASE 38/48 = 79.2% | pass |

**Round 2 verdict (scored once, 2026-11-11): the bar did not hold. `SHIPPED = False`; the router is off by
default.** `EVE_APP_ROUTER=on` runs it as a live trial.

Sets: `app_holdout2.json` (140 cases, writer D; 26 of its 40 Mac cases never say Mac, laptop, computer, desktop,
screen or browser) and `docs/evals/app-router-2026-11-11/round2/cases.json` (48 cases, writer E). Three end-to-end
arms: `main.txt` is the code before this branch, `base.txt` is this branch with the router off, `new.txt` with it on.

What failed, and what the runs show:

- **Line 2.** Four Mac cases were claimed for a device: a screen recording of a slide walk-through and a video
  call's camera (claimed for the camera), a login in a music app's desktop window and mirroring a window to a
  display (claimed for the TV). These are the shapes round 1's sets did not hold. One plain question about light
  colour was claimed for the lights.
- **Line 5.** One device case still reached the Mac agent with the router on (a camera exposure request the
  router did not claim), against three with it off.
- **A wrong claim cost a Mac task.** In the NEW arm one Mac case was claimed for the TV as something it can't do;
  `start_task` was refused once and the model then told the owner it couldn't, instead of calling again. The
  nudge-once guard bounds the harm to one case here, but it is the harm the bar's line 2 exists to stop.
- **The Limits wording alone measured no gain.** main and BASE both sent 3 of 26 device cases to the Mac agent.
  The sentences ship anyway: they state the tools' real limits and what to do about them, and the arms show no
  loss (39/48 against 38/48 is inside the three-point noise floor).
- **With the router on the end-to-end numbers were the best of the three arms** (device cases to the Mac agent
  1 against 3, overall 85.4% against 79.2% and 81.2%), which is why the switch stays for a trial. It is one run of
  48 cases; the bar decides the default, not this.

What a next round would need: Mac cases like the four above in the tuning set, the layer-1 questions split so
each holds one idea (act, or be told), a refit, and a third blind set. Round 2's sets are seen data now.

## What is not covered

- **The gates are fitted with every device live.** A Mac that lacks one sends fewer options to the Choice,
  so the remaining options' probabilities read a little higher than the fit saw. The device's own Noul and
  its lead over the Mac still have to clear, but the fit does not measure this.
- **The wake-word voice brain** (`voice_agent.py`) is a separate brain with its own prompt and its own tool
  loop. This router does not run there: no claim, no note line, no guard.
- **Chief jobs.** `take_on` starts a chief job, and a chief job can start the Mac agent through its own path. The
  guard refuses only the chat brain's own `start_task` call, so a claimed turn that ends in `take_on` is not
  guarded, and the end-to-end eval scores the first action and does not follow the job (round 1's e2e-19).
- **The layer-1 yes/no questions each hold more than one idea.** Every `for_<device>` Noul asks "something to
  happen, or to be told something", then adds a "yes also" and one to three "no" exclusions; the Mac's has two
  exclusions. One yes/no over several ideas reads lower than each idea alone would, which is consistent with the
  low fitted Noul gates (music 0.15, camera 0.2). Splitting them into one question per idea, combined in code,
  changes what Jev is asked, so it needs a refit and a new holdout. It was not done; the floor is the stopgap.
- **Apps without tools** (Pika) and apps that are not devices (Debrief, Repos) are not in the table.
- **Slot values** ("HDMI 1", "20 percent") stay the chat model's job. The method is a hint, not a call.

## Tests

`bridge/tests/test_app_router.py`: the table against the schemas, the questions, the parsing, the rule, the
fit, the floor, the Mac app rule, the eval tool (both rounds), and the turn on fakes: the incident on a call
(claimed for the TV, `start_task` refused, no agent started, one spoken line), a claim that leads to
`firetv_control`, the guard refusing once and lifting after the device's own tool, a steered turn and a
withdrawn question dropping the claim, and request bodies that are byte for byte the same with no router, no
claim, an error and a timeout. No test reads the Mac's installed apps: `conftest.py` pins the list to none, and
a test about apps passes its own.
