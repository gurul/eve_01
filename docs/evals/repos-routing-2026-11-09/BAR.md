# Repos routing: pre-registered bar (2026-11-09, before any result)

**Question.** With the repos app on, does the chat brain send the owner's questions about the
code in their own repositories to `repos_ask` (or `repos_list`), and leave alone what belongs
elsewhere: GitHub (pull requests, issues, CI, the remote), the debrief (what the owner worked on
or decided), a coding session (change or fix the code), the web (code that is not the owner's)?

**Arms.** BASE = `main` at `5a851ef` (no repos app). NEW = branch `feat/repos-app` (the repos
app and its instructions). Each arm scored ONCE on `cases.json` with `bridge/tools/intent_eval.py`
(production routing, nothing executes). The harness lends every app whatever its switch says.

**Cases.** Written by an agent that did not see the repos app's tool descriptions or
instructions. Each case's acceptable labels are fixed before any run. A `repos` case's
acceptable labels are `repos` and, where the case's words make it fair, `github` (BASE has no
`repos` label, so BASE scores those cases only when it reaches an accepted fallback).

**NEW ships only if ALL hold:**
1. NEW scores >= 85% on cases whose primary is `repos`.
2. On cases whose primary is not `repos`, NEW reaches `repos` (first action) on at most 2 cases.
3. On cases whose primary is not `repos`, NEW scores >= BASE - 1 case.
4. NEW overall >= 85%.
5. HARD_NEGATIVE_ACTED is 0 and PROVIDER_ERRORS is 0.

Reported, not gated: BASE's score on `repos` cases (what owners got before), and follow-up
behaviour, which a one-message harness cannot score; it is checked live (two turns through the
daemon's `/api/link/turn`) and reported beside the verdict.

If NEW fails, the change does not ship as is: the failing part is reworked and scored on a fresh
set. The bar is never changed after results exist.
