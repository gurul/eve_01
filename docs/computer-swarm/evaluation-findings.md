# Evaluation improvements from retained evidence

The retained [AX rows](../computer-protocol-eval/ax.json) contain 20 wrong raw keyword selections and 20 wrong selections in each raw keyword composite. The actual guarded router's retained [summary](../computer-protocol-eval/summary.json) records 49 admitted, correct inputs across 155 fixture cases and zero wrong inputs. Raw target grounding and guarded admission answer different questions; a composite built from raw selections does not represent the shipped router.

All four baseline Decisions errors are `Decisions HTTP 400` on empty real-control menus. Fresh evaluation runs now abstain locally on an empty menu without calling either provider. The original raw files, source screens and baseline results are unchanged.

`computer_protocol_eval.py --out` now writes schema 2: the five retained raw arms plus **Guarded keyword** and **Guarded keyword → Decisions (AX only)**. These additional arms run the actual routers with frozen fixture observations and recording effectors. The Decisions fallback runs only on an eligible zero-input keyword handoff and makes its own target call with its own filtered menu and AX evidence. It never reuses a model answer against a different menu. It sends no screen image in this AX slice. Fresh observations, image equality, focus and a positive simulated process ID are explicit fixture assumptions. Verification is forced false locally; no provider verification call or operating-system input runs. These are admission measurements, never real task completions. Whole-task success must come from the separate browser/live evaluations.

Each admission row retains candidate IDs, route status/reason, already-selected labels, actual target evidence/questions/answer, provider time, local admission time and total time. The checker replays those recorded answers through the actual admission code, reconstructs the exact menu/evidence, compares all admitted IDs, and independently recomputes summaries. Historical schema 1 remains accepted. The plot checker re-renders charts from recomputed rows in a temporary directory and compares pixels, so matching dimensions and a stale summary label cannot certify an old graph. The plotter retains the raw comparison and writes a separate `admission.png`/`admission.svg` with the simulation limits printed on the chart.

The AX and visual checkers now reject duplicate, missing or extra cases/providers; mismatched source labels and menus; invalid/nonfinite probabilities or times; predictions that differ from their gates; altered source images; and altered success flags. Visual correctness is reconstructed from the authored image oracle and recorded model answer. A report's own `correct` flag cannot establish correctness.

Adversarial tests found an existing admission defect: the keyword lane would click a matching label in a truncated accessibility tree. Omitted controls could contain another matching target. `fast_lane.py` now rejects truncated observations before selecting or declaring an already-selected control complete. A truncated post-action observation preserves the applied input while declining completion. Tests retain a complete-snapshot positive control.

The leaf's automated evidence covers both report formats, provider no-call behavior with a nonempty positive control, actual keyword/Decisions admission, duplicate-label ambiguity, low confidence, uncovered requests, risk, unoffered targets, questions, app launches, dialogs, truncation, deletion, source-image tampering, corrupted coverage and timing, plus the existing fast-lane and router regressions. Fresh paid provider trials are performed by the integration task after all leaves pass; this leaf makes no claim about an unrun live improvement.

Run from the repository root:

```sh
bridge/.venv/bin/python bridge/tools/computer_protocol_eval.py --out docs/computer-swarm/results/ax.json
bridge/.venv/bin/python bridge/tools/computer_protocol_eval.py --check docs/computer-swarm/results/ax.json
bridge/.venv/bin/python bridge/tools/computer_visual_eval.py --out docs/computer-swarm/results/visual.json
bridge/.venv/bin/python bridge/tools/computer_visual_eval.py --check docs/computer-swarm/results/visual.json
bridge/.venv/bin/python bridge/tools/computer_protocol_plot.py docs/computer-swarm/results
bridge/.venv/bin/python bridge/tools/computer_protocol_plot.py --check docs/computer-swarm/results
```

The plot folder must also contain the complete independently graded browser run files. Neither report checker relabels the baseline as deployment holdout data. No provider probability or risk threshold was changed.
