# Repos routing, round 1: verdict (2026-11-09)

Scored once per arm on `cases.json` (40 cases, written blind), against `../BAR.md`.
Before the scored run the harness refused the set (CONFLICT): the four code-change cases
were grouped `hard_negative`, which may not accept an acting label. They were regrouped
`neighbour` before any case was scored; no text or label changed.

| | BASE `5a851ef` | NEW (first instructions) |
|---|---|---|
| repos cases (22) | 2 | 11 (50%) |
| non-repos cases (18) | 12 | 11 |
| non-repos cases that reached repos first | — | 0 |
| overall | 14/40 (35%) | 22/40 (55%) |
| HARD_NEGATIVE_ACTED / PROVIDER_ERRORS | 1 / 0 | 1 / 0 |

1. NEW >= 85% on repos cases: 50%. **Fails.**
2. NEW reached repos on at most 2 non-repos cases: 0. Holds.
3. NEW >= BASE - 1 on non-repos cases: 11 vs 12. Holds.
4. NEW overall >= 85%: 55%. **Fails.**
5. HARD_NEGATIVE_ACTED 0: 1 (rp-26, "make a new github repo", started a computer task in
   both arms; a request to act, mis-grouped as a hard negative). **Fails.**

**NEW does not ship as is.** Ten of NEW's eleven misses went to the debrief, every one
naming a project the debrief notes also know (eve, claude-debrief, the car); the two
invented repositories both reached repos. The instructions are reworked to separate the
two (the notes say what the owner did and decided; repos reads the code, its git history
included) and scored on a fresh blind set in `../round2/`.

Not about repos: the four code-change cases expected one lane (`coding_session`) and
production picks Codex or a computer task (BASE 1/4, NEW 0/4); two of three general
programming questions were answered with no tool in both arms.
