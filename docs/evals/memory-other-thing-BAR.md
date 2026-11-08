# Memory: a detail found for something else is not the answer — the pre-registered bar

Written 2026-11-07, before the change was made or any result with it existed. Nothing here changes after a result
exists.

## The defect

On the memory eval's holdout (q218, a forget question), the owner asked for a code that had been forgotten. The
search found nothing for it, and eve answered with a different code the owner had stored for another purpose,
citing that purpose in its own source line. A confident wrong code is worse than "I don't have that".

## The change

One sentence in the chat brain's memory instructions (`telegram.PROFILE_HEADER` and `telegram.MEMORY_HINT`): a
detail memory holds for a different thing is not the answer, so say it is not in memory instead of offering it.

## How it is judged

1. **Dev probes first** (not the holdout): 6 new questions on the eval persona that ask for a detail never said,
   whose nearest stored neighbour is a detail of the same kind for something else. Each must abstain and must not
   contain the neighbour. Run with and without the change; used to develop the wording.
2. **Holdout once**, with the change, against the same day's row with every switch off after the mem0 date fix
   (`bridge/tests/fixtures/memory_eval/holdout_after_datefix.json`, 94.4%), and the implicit holdout against
   `memory_eval_implicit/holdout_after_datefix.json` (18 of 21).

## The bar

| Gate | Bar |
|---|---|
| q218 | scores 1 |
| Main holdout accuracy | at least 93.4% (no more than 1 point down) |
| Every main holdout category | loses at most 1 question |
| Abstention errors on the main holdout (either direction) | at most 1 more than the baseline row |
| Implicit holdout | at least 17 of 21 (at most 1 question down) |
| Dev probes | at least 5 of 6 abstain with no neighbour named |

Every gate must hold. If one fails, the change is reverted and its numbers are recorded here.

## Verdict, 2026-11-07: shipped

One run with the change, the same evening, every memory switch off:

```
PASS q218 scores 1 | 1
PASS main accuracy >= 93.4% | 95.7% (baseline 94.4%)
PASS every category loses <= 1 | {'abstention': 0, 'buddy_said': 1, 'cross_channel': 0, 'extraction': 0, 'forget': -1, 'knowledge_update': 0, 'multi_session': 0, 'plans': 0, 'temporal': -2}
PASS abstention errors <= baseline + 1 | 5 vs 5
PASS implicit holdout >= 17 of 21 | 18 of 21 (baseline 18)
PASS dev probes >= 5 of 6 | 6 of 6
BAR MET
```

The dev probes (`memory-other-thing/probes.json`: six forget-then-ask questions about other codes and numbers)
abstained 6 of 6 without the change too, so they did not reproduce the defect; the change was judged on the
holdout, once. Result files: `bridge/tests/fixtures/memory_eval/holdout_other_thing.json`,
`bridge/tests/fixtures/memory_eval_implicit/holdout_other_thing.json`, `memory-other-thing/probes_*.json`.
