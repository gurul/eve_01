# Gates: vision Decisions computer-use loop

OWNS: bridge/src/eve_bridge/decisions.py, bridge/src/eve_bridge/decision_lane.py, bridge/src/eve_bridge/desk_lane.py, bridge/src/eve_bridge/desktop_helpers.py, bridge/tests/test_decisions.py, bridge/tests/test_decision_lane.py, bridge/tools/decisions_eval.py, docs/decisions-computer-use.md, README.md, docs/decisions-gates.md, docs/decisions-eval*.json

Scope: Add an opt-in image-aware Decisions loop before Codex, retaining bounded live controls and code gates, plus reproducible target/verification comparisons with Jev.

- [x] G1: Decisions requests use inline images and typed questions; malformed, refused and unavailable answers fail closed and usage is recorded.
  CHECK: bridge/.venv/bin/python -m pytest -q bridge/tests/test_decisions.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=.................                                                        [100%] | 17 passed in 0.08s

- [x] G2: The loop selects only observed controls, revalidates before acting, verifies the result, and preserves partial progress on failures without repeating input.
  CHECK: bridge/.venv/bin/python -m pytest -q bridge/tests/test_decision_lane.py bridge/tests/test_desk_lane.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=................................................                         [100%] | 48 passed in 0.20s

- [x] G3: Existing desktop and Jev behavior passes relevant regression checks; changed Python files pass lint.
  CHECK: bridge/.venv/bin/python -m pytest -q bridge/tests/test_fast_lane.py bridge/tests/test_lane_first.py bridge/tests/test_jev_step.py bridge/tests/test_jev_grouped.py bridge/tests/test_desktop_helpers.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=..............................                                           [100%] | 102 passed in 0.38s

- [x] G4: Read-only live comparison records measured target accuracy, verification accuracy, latency and usage, or an explicit service-access failure; documentation states the actual evidence and activation limits.
  EVIDENCE: manual review 2026-10-07; both paid providers answered every synthetic case in two read-only runs. docs/decisions-eval-initial.json retains six Decisions abstentions before predicate wording was revised; docs/decisions-eval.json records Decisions 6/6 target choices, 4/4 verifications, median 190/198 ms, versus Jev 3/6 (three abstentions), 2/4 and 283/296 ms. Independently recomputed summaries from each report's rows and asserted documentation counts, timings, usage and README link: DOCS_AND_LIVE_EVIDENCE_OK, exit 0. Documentation labels both runs as tuning/synthetic, keeps default off and states that real desktop completion, task latency and unseen calibration remain unmeasured.

- [x] G5: Changed Python sources pass the repository lint configuration.
  CHECK: bridge/.venv/bin/ruff check bridge/src/eve_bridge/decisions.py bridge/src/eve_bridge/decision_lane.py bridge/src/eve_bridge/desk_lane.py bridge/src/eve_bridge/desktop_helpers.py bridge/tests/test_decisions.py bridge/tests/test_decision_lane.py bridge/tools/decisions_eval.py
  EXPECT: All checks passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=All checks passed!
