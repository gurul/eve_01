# Gates: extensive computer protocol evaluation

OWNS: bridge/tools/computer_protocol_eval.py, bridge/tools/computer_protocol_plot.py, bridge/tools/computer_visual_eval.py, docs/computer-protocol-eval/**, docs/computer-protocol-evaluation.md, docs/decisions-computer-use.md, README.md

Fixed before new calls: evaluate all committed AX fixture cases twice with the keyword gate, hosted Jev and Decisions on the same menus; score hybrids from their actual constituent calls. Run the existing real plan/executor browser protocol on nine isolated local task types, three planner models and two repetitions. Record blocked protocols explicitly. Keep stage latency separate from task wall time, and correctness separate from safe abstention. This is a development suite, not an unread holdout. No threshold fitting on these results.

- [x] G1: Complete paired AX evaluations retain every response, error, offered target, measured duration and expected outcome.
  CHECK: bridge/.venv/bin/python bridge/tools/computer_protocol_eval.py --check docs/computer-protocol-eval/ax.json
  EXPECT: AX_EVAL_CHECKED
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=AX_EVAL_CHECKED 310 paired requests per arm

- [x] G2: All 54 isolated browser runs are independently graded against fixture state or task answer patterns; errors and handoffs remain in the report.
  CHECK: bridge/.venv/bin/python bridge/tools/computer_protocol_eval.py --check-browser docs/computer-protocol-eval/browser
  EXPECT: BROWSER_EVAL_CHECKED
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=BROWSER_EVAL_CHECKED 54 independently graded runs

- [x] G3: Graphs and summaries recompute measurements from retained raw data and label fixture, live, tuning and unavailable evidence.
  CHECK: bridge/.venv/bin/python bridge/tools/computer_protocol_plot.py --check docs/computer-protocol-eval
  EXPECT: PLOTS_CHECKED
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=PLOTS_CHECKED

- [x] G4: The evaluation harnesses pass lint, and the Decisions integration and demo report remain verified.
  CHECK: bridge/.venv/bin/ruff check bridge/tools/computer_protocol_eval.py bridge/tools/computer_protocol_plot.py bridge/tools/computer_visual_eval.py bridge/tools/decisions_live_demo.py && bridge/.venv/bin/python -m pytest -q bridge/tests/test_decision_lane.py bridge/tests/test_decisions.py bridge/tests/test_desk_lane.py && bridge/.venv/bin/python bridge/tools/decisions_live_demo.py --check-report docs/decisions-live-results.json
  EXPECT: LIVE_REPORT_CHECKED
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=65 passed in 0.24s | LIVE_REPORT_CHECKED

- [x] G5: The user receives the measured comparison graph, scope, failures and actionable conclusion; docs and README link the evidence.
  EVIDENCE: 2026-10-07: comparison.png and .svg generated from retained raw rows; chart visually inspected and opened in Codex (queued). README and both protocol docs link the evidence. DOC_NUMBERS_AND_LINKS_CHECKED, exit 0; source-derived numbers, failures, repeated-case limits, guarded-vs-raw admission and unmeasured Codex/legacy paths explicit. Final reply embeds the graph. Earlier formatting audit reported AssertionError: 9.06 s; normalized documented Astra median from 9.055 s to half-up 9.06 s and rechecked.

- [x] G6: A separate image-dependent synthetic slice varies layout, theme, absent targets and result verification, with 36 fixed cases repeated twice per provider, retained image inputs and no tuned thresholds.
  CHECK: bridge/.venv/bin/python bridge/tools/computer_visual_eval.py --check docs/computer-protocol-eval/visual.json
  EXPECT: VISUAL_EVAL_CHECKED
  EVIDENCE: exit=0; shell=/bin/zsh; cwd=/Users/gurucharan/Documents/personal/eve; path=8367adf2780f/24 entries; output=VISUAL_EVAL_CHECKED 144 provider responses
