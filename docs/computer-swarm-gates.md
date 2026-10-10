# Verified computer-use swarm gates

Engineering evidence is copied from the integration ledger after independent execution.
Publication is checked separately against the Git remote after commit/push; the final response gives the pushed commit.
The scoped local ledger includes that publication gate.

# Gates: computer-use swarm integration

OWNS: bridge/tests/test_computer_swarm_verify.py, bridge/src/eve_bridge/plan_contract.py, bridge/src/eve_bridge/plan_executor.py, bridge/tests/test_plan_executor.py, bridge/tests/test_plan_url_expectation.py, bridge/src/eve_bridge/computer_agent.py, bridge/src/eve_bridge/daemon.py, bridge/tests/test_browser_planner_config.py, bridge/tools/computer_swarm_verify.py, docs/computer-swarm/results/**, docs/computer-swarm-improvements.md, docs/computer-swarm-gates.md, docs/decisions-computer-use.md, docs/computer-protocol-evaluation.md, README.md

- [x] G1: All three leaf ledgers are independently reverified and their patches reviewed against safety and compatibility contracts.
  EVIDENCE: Independently reexecuted all three returned leaf ledgers with --approve --reverify, repo cwd and /bin/zsh: browser45 tests +34 metadata tests/Ruff; Decisions124 tests; evaluation89 tests. Reviewed production diffs and test counterexamples; peer review found/fixed port0 URL normalization and query/hash site-context compatibility. No threshold reduction, fixture edit or permission change. Leaf gates 7/7 met.

- [x] G2: Fresh real browser tasks retain all prior goal successes and improve autonomous completion over the retained 42/54 baseline without crediting clicks as proof.
  CHECK: bridge/.venv/bin/python bridge/tools/computer_swarm_verify.py --browser docs/computer-swarm/results/browser
  EXPECT: BROWSER_IMPROVED
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=} | BROWSER_IMPROVED

- [x] G3: Browser planner defaults select the measured Luna path while explicit overrides and desktop planner settings remain compatible; integration regressions pass.
  CHECK: EVE_LIVE=1 bridge/.venv/bin/python -m pytest -q bridge/tests/test_computer_swarm_verify.py bridge/tests/test_plan_url_expectation.py bridge/tests/test_plan_executor.py bridge/tests/test_browser_planner_config.py bridge/tests/test_computer_agent.py bridge/tests/test_agent_browser_lane.py bridge/tests/test_chrome_lane.py bridge/tests/test_desk_lane.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=........                                                                 [100%] | 152 passed in 8.86s

- [x] G4: Fresh Decisions/grounding evidence and plots retain failures and distinguish model classification, guarded admission and whole-task success.
  CHECK: bridge/.venv/bin/python bridge/tools/computer_swarm_verify.py --evidence docs/computer-swarm/results && bridge/.venv/bin/python bridge/tools/computer_protocol_plot.py --check docs/computer-swarm/results
  EXPECT: SWARM_EVIDENCE_CHECKED
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=SWARM_EVIDENCE_CHECKED | PLOTS_CHECKED

- [x] G5: README/docs describe actual implemented behavior and measured improvements; scoped lint and patch hygiene pass.
  CHECK: bridge/.venv/bin/ruff check bridge/src/eve_bridge/plan_contract.py bridge/src/eve_bridge/plan_executor.py bridge/src/eve_bridge/fast_lane.py bridge/src/eve_bridge/browser_lane.py bridge/src/eve_bridge/desktop_helpers.py bridge/src/eve_bridge/ax_candidates.py bridge/src/eve_bridge/decision_lane.py bridge/src/eve_bridge/computer_agent.py bridge/src/eve_bridge/daemon.py bridge/tools/computer_protocol_eval.py bridge/tools/computer_visual_eval.py bridge/tools/computer_protocol_plot.py bridge/tools/computer_swarm_verify.py bridge/src/eve_bridge/decisions.py bridge/tests/test_browser_planner_config.py bridge/tests/test_plan_url_expectation.py bridge/tests/test_computer_swarm_verify.py bridge/tests/test_computer_protocol_eval.py bridge/tests/test_decisions_observation.py && git diff --cached --check && git diff --check
  EXPECT: All checks passed
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=All checks passed!


## Independently reverified leaves

# Gates: browser swarm leaf

OWNS: bridge/src/eve_bridge/browser_lane.py, bridge/tests/test_browser_lane.py, bridge/tests/test_browser_navigation_freshness.py, docs/computer-swarm/browser-findings.md, .unlazy/swarm-improve/gates/leaf-browser.md

- [x] G1: Navigation/cookie target actions use fresh post-action page state and original success criteria; unsuccessful navigation still hands off without blind repeats.
  CHECK: EVE_LIVE=1 bridge/.venv/bin/python -m pytest -q bridge/tests/test_browser_lane.py bridge/tests/test_browser_navigation_freshness.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=.............................................                            [100%] | 45 passed in 18.51s

- [x] G2: The patch is reread, regressions and counterexamples examined, and findings identify source evidence, actual changes, tests and limits without claiming an unrun live outcome.
  EVIDENCE: Four passes and metadata extension reread recorded in docs/computer-swarm/browser-findings.md. Retained fixture mismatch distinguished from injected navigation race. Real Chromium correct/wrong/no-navigation tests assert one click and original predicates; unknown/persistent errors and frozen clock checked. DOM-resolved relative URL, distinct origin/path, credentials/query/hash omission and snapshot-only current_url controls covered. Source/test diff reread; ruff exit=0 All checks passed. No baseline edits or unrun end-to-end claims.

- [x] G3: Observed URL metadata identifies actual safe http(s) origin/path and current_url reads only the latest successfully collected snapshot; query/fragment/credential destinations are omitted from the planner outline.
  CHECK: EVE_LIVE=1 bridge/.venv/bin/python -m pytest -q bridge/tests/test_browser_navigation_freshness.py && bridge/.venv/bin/ruff check bridge/src/eve_bridge/browser_lane.py bridge/tests/test_browser_navigation_freshness.py
  EXPECT: All checks passed!
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=34 passed in 7.88s | All checks passed!


# Gates: decisions swarm leaf

OWNS: bridge/src/eve_bridge/decision_lane.py, bridge/src/eve_bridge/desktop_helpers.py, bridge/src/eve_bridge/ax_candidates.py, bridge/tests/test_decision_lane.py, bridge/tests/test_decisions_observation.py, docs/computer-swarm/decisions-findings.md, .unlazy/swarm-improve/gates/leaf-decisions.md

- [x] G1: Decisions receives useful observed result evidence, preserves genuine stale-state/focus checks and partial-action handoffs, and does not report success from input alone.
  CHECK: bridge/.venv/bin/python -m pytest -q bridge/tests/test_decision_lane.py bridge/tests/test_decisions.py bridge/tests/test_decisions_observation.py bridge/tests/test_ax_candidates.py bridge/tests/test_fixtures_ax.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=....................................................                     [100%] | 124 passed in 0.75s

- [x] G2: The patch is reread, regressions and counterexamples examined, and findings identify source evidence, actual changes, tests and limits without claiming an unrun live outcome.
  EVIDENCE: Four passes recorded in docs/computer-swarm/decisions-findings.md; reread changed production modules/tests, preserved archive round-trip after transport-equality counterexample, excluded secure descendants/editable readouts, expanded regression run 203 passed (exit 0), ruff All checks passed (exit 0). No thresholds, baseline artifacts or OS permissions changed. Fresh live outcomes left to root integration.


# Gates: evaluation swarm leaf

OWNS: bridge/src/eve_bridge/fast_lane.py, bridge/tests/test_fast_lane.py, bridge/tools/computer_protocol_eval.py, bridge/tools/computer_visual_eval.py, bridge/tools/computer_protocol_plot.py, bridge/tests/test_computer_protocol_eval.py, docs/computer-swarm/evaluation-findings.md, .unlazy/swarm-improve/gates/leaf-evaluation.md

- [x] G1: The comparison handles empty menus without invalid API calls and distinguishes raw grounding from guarded admission; truncated snapshots fail closed and safety counterexamples validate the oracle without relabelling baseline evidence.
  CHECK: bridge/.venv/bin/python -m pytest -q bridge/tests/test_computer_protocol_eval.py bridge/tests/test_fast_lane.py bridge/tests/test_lane_first.py bridge/tests/test_fastlane_docs.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=.................                                                        [100%] | 89 passed in 1.71s

- [x] G2: The patch is reread, regressions and counterexamples examined, and findings identify source evidence, actual changes, tests and limits without claiming an unrun live outcome.
  EVIDENCE: Reviewed evaluator/test/plotter sources and fast_lane diff in four passes. Baseline4 Decisions HTTP400 errors all had empty menus; raw Keyword/hybrids20 wrong versus guarded49 correct/0 wrong. Initial truncated-admission adversarial test failed, then stronger pre/post observation checks plus positive control passed. Final pytest88 passed; scoped ruff All checks passed; baseline AX_EVAL_CHECKED310, VISUAL_EVAL_CHECKED144, PLOTS_CHECKED including regenerated pixel oracle and git diff --check all exit0. Findings identify simulation limits; no unrun live claim. Fetch before tests: upstream gap0/0. No commit/push.
