# Debrief routing: verdict (2026-11-09)

Scored once per arm on `cases.json` (40 cases, written blind), against `BAR.md`.
Raw output: `new.txt`, `base.txt`.

| | BASE `7507ae0` | NEW `b2e1509` |
|---|---|---|
| debrief cases (26) | 24 (92%) | **26 (100%)** |
| non-debrief cases that reached debrief first (14) | 0 | **0** |
| overall | 37/40 (92.5%) | **38/40 (95.0%)** |
| HARD_NEGATIVE_ACTED / PROVIDER_ERRORS | 0 / 0 | 0 / 0 |

1. NEW >= 85% on debrief cases: 100%. **Holds.**
2. NEW >= BASE - 1 case: 26 vs 24. **Holds.**
3. NEW reached debrief on at most 2 non-debrief cases, and <= BASE + 1: 0. **Holds.**
4. NEW overall >= 85%: 95.0%. **Holds.**
5. HARD_NEGATIVE_ACTED 0 and PROVIDER_ERRORS 0. **Holds.**

**NEW ships.** BASE's two misses went to web search ("whats the latest on storybox")
and to memory ("is the kokoro voice stuff finished"). NEW's two failures are outside
the debrief: a coding-fix request answered with a question (BASE started a computer
task instead; both fail), and one personal to-do question sent to the notes vault
(BASE passed it).

Reported, not gated, NEW's first debrief tool by kind: time questions went to
`debrief_day` 8 of 9 (one to `debrief_status`); when-last to `debrief_when` 4 of 4;
status to `debrief_status` 5 of 5; open threads to `debrief_status` 2, `debrief_day` 1;
decisions and lessons to `debrief_search` 3, after a memory lookup first on 2.

The toolset router (Jev) timed out on 4 cases in each arm; those turns ran on
keywords, as production does past its budget. This set is spent: development data
from now on.
