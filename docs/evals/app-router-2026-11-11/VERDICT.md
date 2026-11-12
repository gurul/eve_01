# App router: verdict (2026-11-11)

Each arm scored once. Sets: `bridge/tests/fixtures/routes/app_holdout.json` (120, writer B, sha256 `1a6905c4a119044b…`)
and `cases.json` here (40, writer C). Neither writer saw the router, its questions or the tuning set.

| Line | Measured | Verdict |
|---|---|---|
| 1. device cases named right, >= 0.90 | 80/80 | pass |
| 2. Mac cases claimed, <= 0.03 | 0/24 | pass |
| 3. neither cases claimed, <= 0.05 | 0/16 | pass |
| 4. Jev p95 <= 800 ms | 418 ms (p50 262 ms, 0 errors) | pass |
| 5. device cases ending in the Mac agent, 0 | NEW 0/24, BASE 1/24 | pass |
| 6. Mac cases reaching the Mac agent, >= 0.90 | NEW 10/10 | pass |
| 7. NEW >= BASE - 3 points | NEW 90.0%, BASE 85.0% | pass |

**Ships on** (`app_router.SHIPPED = True`).

Read with these in mind (the full text is in `docs/app-router.md`, "Verdict"):

- Line 5's parenthesis names a harness counter that also counts a device's own tool; the line itself held.
- Both arms carry the new Limits sentences, so the router's own end-to-end effect here is one case (1/24 to 0/24).
  The guard refused nothing in the run.
- `mac_app_only` (a bare open or quit of an installed Mac app is the Mac's) was added after the holdout, from a
  live check. It fires only on Mac cases in both sets, so no line moves, but it has no blind test of its own.
- The sets are written requests, not recordings. `base.txt` and `new.txt` are the raw runs.
