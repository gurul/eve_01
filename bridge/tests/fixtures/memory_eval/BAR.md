# Memory eval: the pre-registered bar

Written 2026-10-08, after the holdout was sealed (questions.sha256, commit 5aeae43) and before the baseline
finished or any memory feature was built. Nothing here changes after a holdout result exists.

The features it judges (the plan, from research on how OpenAI, Anthropic, Google, Letta, mem0, Zep and the
literature build LLM memory), each behind its own switch:

1. Hybrid search over raw transcript lines (BM25 + embeddings, one owner line with its reply as a unit).
2. Valid-from / valid-to on record facts; superseded facts closed, not deleted.
3. Source pointers on every fact (day and time of the line it came from).
4. Time-aware search arguments (a from/to range resolved in code from "last week", "in May").
5. Search more than once, then abstain (a rephrased search before "I don't have that"; at most 3 memory calls a turn).

## How a feature is judged

- Development uses the **dev** split only (78 questions). The **holdout** (162) is run once per feature, at the end,
  with the feature on, against the baseline's holdout row (run once, with every feature off, on the same day).
- Scores are code-scored (tools/memory_eval.py): answer keys, abstention, evidence retrieval, forget compliance.

## The bar (holdout), per feature

| Gate | Bar |
|---|---|
| Target category (the one the feature is for) | at least +5 points over the baseline |
| Every other category | down no more than 2 points |
| Abstention errors (answered when it should abstain, or abstained when the answer was there) | at most 10% of abstention-relevant questions |
| memory_search p95 latency | at most 1.5 s |
| Added prompt tokens per turn | at most 500 |
| Forget compliance (a forgotten fact never comes back) | 100% |

Target categories: 1 → extraction and multi_session; 2 → knowledge_update; 3 → multi_session; 4 → temporal;
5 → abstention. A feature that fails stays off (its switch's default), with its numbers recorded here.
