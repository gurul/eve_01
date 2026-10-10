# OpenUI eval verdict, 2026-10-08 (scored run)

**Overall: PASS. All five bar items hold. Item 4 passes by only 0.8 points.**

The earlier verdict is in VERDICT-void.md. That NEW run crashed before any reply was seen, so this is NEW's one scored run.

## Setup
- cases.json sha256 matched cases.sha256 (`9a87e86c...92d8ca`) before and after both runs. The cases and the bar were not edited.
- NEW: worktree wf_726f9bba-544-2 at **46ef062**, which was clean. It ran once. Stdout is in new.txt and the replies are in new-replies.json, exit 0. The crash log is kept as new-crash.txt.
- BASE: a detached worktree at **25d2728**, with `uv sync` run. The only file added was bridge/tools/openui_eval.py (`git status`: `?? bridge/tools/openui_eval.py`). `OPENUI_VALIDATOR` and `--validator` both pointed at NEW's validate.mjs. It ran once. Stdout is in base.txt and the replies are in base-replies.json, exit 0. The worktree has been removed.
- Both arms used model gpt-6-luna at low effort. Every row records gpt-6-luna as its model.

## Results by label
| Label | n | BASE uses UI | NEW uses UI | NEW parse ok |
|---|---|---|---|---|
| ui | 26 | 14/26 (53.8%) | **22/26 (84.6%)** | 22/22 |
| text | 26 | 0/26 (0%) | **0/26 (0%)** | n/a |
| either | 8 | 0/8 | 5/8 (e01 e02 e03 e04 e07) | 5/5 |
| all | 60 | 14/60 | 27/60 | 27/27 |

- Parse failures: NEW 0/27, BASE 0/14.
- Provider errors: NEW 0, BASE 0. The `error` field is empty on all 120 rows.
- BASE 'ui' misses: u03 u04 u06 u12 u17 u18 u19 u21 u22 u23 u25 u26.

## Every NEW 'ui' miss
- **u06**: picking between three dinners. NEW answered with a plain-text recommendation and offered to narrow it down further.
- **u16**: planets in order with their sizes. NEW wrote a Card with a Table, but the closing fence came out as `` ``) `` and not as ```` ``` ````, so the harness scored the reply as an unclosed fence. The app's `splitReply` reads it as an incomplete UI part (`complete: false`). The program body, stray line included, passes validate.mjs (`ok: true`, root Card), so the app would likely still draw the table, but as an unfinished block. This is the one bad-looking block in the run. It counts as a 'ui' miss, and because it is not a complete program it does not count against item 3.
- **u22**: a startup or big-company job decision. NEW asked a plain-text clarifying question and showed no block.
- **u26**: four unit conversions. NEW answered in four plain-text lines.

## NEW 'text' cases that got UI
None (0/26).

## Bar items
| # | Item | Measured | Result |
|---|------|----------|--------|
| 1 | NEW uses UI on >= 80% of 'ui' cases | 22/26 = 84.6% | **PASS** |
| 2 | NEW uses UI on <= 10% of 'text' cases | 0/26 = 0% | **PASS** |
| 3 | 100% of NEW's UI programs parse with the app's parser | 27/27 (22 ui + 5 either), 0 render errors | **PASS** |
| 4 | NEW 'ui' rate >= BASE 'ui' rate + 30 points | 84.6 - 53.8 = +30.8 points | **PASS** (by 0.8 points: one fewer NEW hit would fail it) |
| 5 | Zero provider errors | 0 in both arms | **PASS** |

**Overall: PASS, so NEW meets the bar to ship.**

## Notes, not gated
- Item 4's margin is a single case. BASE's 14/26 is one sample, so read the lift as real but not large.
- u16 points to a hardening step for later: the app or prompt could accept a near-miss closing fence. It is not part of this verdict.
- Nothing was pushed. The daemon was not touched. Nothing ran inside the main checkout except `git worktree add/remove/prune`.
