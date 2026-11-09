# Repos routing, round 2: verdict (2026-11-09)

Fresh blind set (`cases.json`, 40 cases, 24 repos; 16 of the repos cases name a project the
owner's debrief notes also know). NEW = the reworked instructions (code vs notes). Scored
once per arm against `../BAR.md`.

| | BASE `5a851ef` | NEW (reworked) |
|---|---|---|
| repos cases (24) | 1 | 15 (62.5%) |
| non-repos cases (16) | 10 | 9 |
| non-repos cases that reached repos first | — | 2 (two code-change requests listed the repositories first) |
| overall | 11/40 (27.5%) | 24/40 (60.0%) |
| HARD_NEGATIVE_ACTED / PROVIDER_ERRORS | 0 / 0 | 0 / 0 |

1. NEW >= 85% on repos cases: 62.5%. **Fails.**
2. NEW reached repos on at most 2 non-repos cases: 2. Holds.
3. NEW >= BASE - 1 on non-repos cases: 9 vs 10. Holds.
4. NEW overall >= 85%: 60%. **Fails.**
5. HARD_NEGATIVE_ACTED 0 and PROVIDER_ERRORS 0. Holds.

**NEW does not pass. The app stays off by default.** Of the nine repos misses, six went to
the debrief: "why" questions about a design choice and "what changed this week" questions,
which the debrief's notes can also answer. Two were answered with no tool (one about eve
itself, which eve's own self-description covers), one listed a README as a computer task.
How/where/explain questions about named code reached repos in most cases.

The overlap with the debrief is the open problem: a prompt line alone did not separate
them in two rounds. The owner's Mac runs it on (`EVE_REPOS=1`, the owner's choice, as with
the debrief before its eval); saying "the code" or "the repo" in a question routes it best.
