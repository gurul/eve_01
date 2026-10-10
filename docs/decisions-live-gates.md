# Gates: Decisions live computer-use demonstration

OWNS: bridge/tools/decisions_live_demo.py, docs/decisions-live-gates.md, docs/decisions-live-results.json, docs/decisions-live-demo*/**, docs/decisions-live-initial.json, docs/decisions-live-stale.json, docs/decisions-computer-use.md, README.md, bridge/src/eve_bridge/decision_lane.py, bridge/tests/test_decision_lane.py

Scope: visibly run the real Decisions loop on a series of harmless desktop cases, retain observed outcomes and screenshots, and report actual successes, refusals and failures.

- [x] G1: A series of actual desktop requests is executed through eve's Decisions route, with independent observed outcomes and before/after evidence.
  EVIDENCE: 2026-10-07: eight Calculator requests through actual Decisions route, CUA app-bound capture/input, real AX walker. Report retains zero verified completions, one observed unverified result, four action refusals and three safe handoffs; initial foreground and stale-image attempts retained.

- [x] G2: The evidence report classifies each request from both its route result and observed UI state; failures and incomplete actions are retained.
  CHECK: bridge/.venv/bin/python bridge/tools/decisions_live_demo.py --check-report docs/decisions-live-results.json
  EXPECT: LIVE_REPORT_CHECKED
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=LIVE_REPORT_CHECKED

- [x] G3: Any implementation changes retain the tested code admission and handoff behavior; the demo harness passes lint.
  CHECK: bridge/.venv/bin/python -m pytest -q bridge/tests/test_decision_lane.py bridge/tests/test_decisions.py bridge/tests/test_desk_lane.py && bridge/.venv/bin/ruff check bridge/tools/decisions_live_demo.py bridge/src/eve_bridge/decision_lane.py bridge/tests/test_decision_lane.py
  EXPECT: All checks passed
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=65 passed in 0.23s | All checks passed!

- [x] G4: The user sees real screenshots of the series and documentation records its results and limitations.
  EVIDENCE: CUA emitted the actual Calculator screenshot showing 7 after Decisions requested 7; docs/decisions-computer-use.md links before/after reports, failures and the transport limitations. Screenshot display and classification checked; no autonomous-success claim.
