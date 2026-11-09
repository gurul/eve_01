# Debrief routing: pre-registered bar (2026-11-09, before any result)

**Question.** Does the chat brain reach the debrief for the owner's questions about past
coding work, and leave it alone for questions that belong elsewhere?

**Arms.** BASE = `7507ae0` (the debrief app as first shipped: `debrief_search` and
`debrief_get`). NEW = `main` after `b2e1509` (six tools on the store's tables, the new
instructions). Each arm scored ONCE on `cases.json` with `bridge/tools/intent_eval.py`
(production routing, nothing executes), `EVE_DEBRIEF=1` in both.

**Cases.** Written by an agent that did not see either arm's tool descriptions or
instructions. `primary` = the right label; `debrief` cases are past coding work;
the rest are hard negatives whose right label is another capability.

**NEW ships only if ALL hold:**
1. NEW scores >= 85% on cases whose primary is `debrief`.
2. NEW scores >= BASE - 1 case on `debrief` cases.
3. On non-debrief cases, NEW reaches `debrief` (first action) on at most 2 cases, and
   no more than BASE + 1.
4. NEW overall >= 85%.
5. HARD_NEGATIVE_ACTED is 0 and PROVIDER_ERRORS is 0.

Reported, not gated: which debrief tool NEW calls first, per case kind.

If NEW fails, the change does not stay as is: the failing part is reverted or reworked
and scored on a fresh set. The bar is never changed after results exist.
