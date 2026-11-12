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

| Line | Measured | Verdict |
|---|---|---|
| 1. device cases named right, >= 0.90 | | VERDICT: pending |
| 2. Mac cases claimed, <= 0.03 | | VERDICT: pending |
| 3. neither cases claimed, <= 0.05 | | VERDICT: pending |
| 4. Jev p95 <= 800 ms | | VERDICT: pending |
| 5. device cases ending in the Mac agent, 0 | | VERDICT: pending |
| 6. Mac cases reaching the Mac agent, >= 0.90 | | VERDICT: pending |
| 7. NEW >= BASE - 3 points | | VERDICT: pending |
