# App router: verdict, round 1 (2026-11-11)

Each arm scored once. Sets: `bridge/tests/fixtures/routes/app_holdout.json` (120, writer B, sha256 `1a6905c4a119044b…`)
and `cases.json` here (40, writer C). Neither writer saw the router, its questions or the tuning set.

| Line | Measured | Verdict |
|---|---|---|
| 1. device cases named right, >= 0.90 | 80/80 | pass |
| 2. Mac cases claimed, <= 0.03 | 0/24 | pass |
| 3. neither cases claimed, <= 0.05 | 0/16 | pass |
| 4. Jev p95 <= 800 ms | 418 ms (p50 262 ms, 0 errors) | pass |
| 5. device cases ending in the Mac agent, 0 | NEW 0/24, BASE 1/24, first action only | pass |
| 6. Mac cases reaching the Mac agent, >= 0.90 | NEW 10/10 | pass |
| 7. NEW >= BASE - 3 points | NEW 90.0%, BASE 85.0% | pass |

Round 1 held every line, and the router shipped on for it. **It is off again** (`app_router.SHIPPED = False`)
until round 2 is scored: see "Round 2" below.

Read with these in mind (the full text is in `docs/app-router.md`, "Verdict"):

- Line 5 was scored on the first action only: the harness reads the first tool a case calls and does not follow
  what it starts. No device case's first action was `start_task`.
- One NEW case, e2e-19 (claimed for the lights), ended in `take_on`, a chief job. That is neither a device's tool
  nor the guarded `start_task` call. A chief job can start the Mac agent through its own path, which the guard
  does not cover and the eval does not follow. The other three of NEW's four `HARD_NEGATIVE_ACTED` are a device's
  own tool called on a request the device can't do.
- Both arms carry the new Limits sentences, so the router's own end-to-end effect here is one case (1/24 to 0/24).
  The guard refused nothing in the run.
- `mac_app_only` (a bare open or quit of an installed Mac app is the Mac's) was added after the holdout, from a
  live check. It fires only on Mac cases in both sets, so no line moves, but it has no blind test of its own.
- The sets are written requests, not recordings. `base.txt` and `new.txt` are the raw runs.

## Round 2

Round 1's sets are seen data now: a review read the holdout's answers and both end-to-end runs. These were changed
after round 1, on what it showed:

- a floor under the fitted gates (`app_router.FLOOR`: noul 0.5, choice 0.5, lead 0.0; `app_gates.json` is kept as
  fitted);
- the guard: `start_task` is refused at most once per turn, the guard lifts once the claimed device's own tool
  has run, and a steered turn or a withdrawn gate question drops the guard, the note line and the claim;
- `mac_app_only`: nothing but a launch or a quit, the planner's own wording, and not for the apps named TV, Music
  and Home;
- the Limits sentences in the TV's and Spotify's blocks;
- the refusal and the note line for a request the device can't do.

Round 2 decides `SHIPPED`: a new blind router holdout (`app_holdout2.json`, scored once with
`tools/app_route_eval.py --holdout2`, the live path's whole decision with a fixed app list) and a new blind
end-to-end set, against the same bar (`BAR.md`, unchanged).

### Round 2 verdict

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
