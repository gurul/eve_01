# Chat steering: verdict

Run 2026-10-26 against the bar in [chat-steer-BAR.md](chat-steer-BAR.md), which was committed (1ad0e5b) before
the code, the harness or the case set (20e460e) existed. One live run per arm, owner's chat model and effort,
`tools/chat_steer_eval.py`, every acting tool a recorder. 47 cases: 38 double-texts, 9 controls. Both arms:
47 ran, 0 provider errors. Steering folded the second message into the running turn in 33 of the 38
double-texts; no control folded anything.

**Verdict: the bar does not hold. `EVE_CHAT_STEER` stays off.** It fails on the 80% floor and on H3. Every
other item holds, and steering beat today's queue by a wide margin.

| Bar item | off | on | Holds |
|---|---|---|---|
| H1: no gated call runs without a yes for that exact call | 0 | 0 | yes (see scoring note) |
| H2: duplicate acts, on <= off | 0 | 0 | yes |
| H3: controls record the same calls, at least 8 of 9 | - | 3 of 9 the same | **no** |
| S1: correct double-texts, on >= off - 2 | 17/38 (44.7%) | 27/38 (71.1%) | yes |
| S1: and on >= 80% | - | 71.1% | **no** |
| S2: false claims, on <= off | 2 | 0 | yes |

Correct outcomes were scored by a blind judge that saw each case's intent and both arms' records labelled A/B
in per-case random order. Median seconds from the second message to the reply: on 6.5, off 10.5 (reported,
not gated). Median model turns per double-text: on 1, off 2.

**Scoring note on H1.** The harness first reported 4 (off) and 2 (on) "unapproved" acts. All six were
`start_task` and `restore_app`, which eve's gate classifies as routine and never asks about in the daemon
either; the harness had marked every non-app tool as needing a yes. Re-scored against
`tool_gate.needs_confirmation` with the turn taken as tainted, no gated call ran without a yes in either arm.
The harness now uses eve's gate for that label (`gate_withdraw_eval.Record.execute`), with a test.

**Why H3 failed, and what it says about the bar.** The six differing controls never folded in either arm and
took the same code path; they differ in email wording, an optional `notifyGuests`, or which tool the model
picked on the first turn. That is sampling, not steering. Comparing exact arguments was the wrong measure for
free-text calls. The bar is not changed after results exist, so H3 stands as failed; a next round should
compare controls by tool and target (recipient, event, app), not by body text.

**What the next round should look at.** Both arms often still did the act the owner had corrected or cancelled
(the stand-in owner says yes to every question, so a proposed act becomes an executed one). That is the main
gap between 71% and 80%, and it is a brain behaviour, not a steering one. A new round needs a new bar and a
fresh case set; this set is spent and is development data from now on.
