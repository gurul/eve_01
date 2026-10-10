# Memory eval: the verdict

## How BAR.md is read (written before any holdout run)

This section was written on 2026-10-08, before the first holdout run. It turns each gate in BAR.md into a
number that the run's JSON gives. Nothing in it changes after a holdout number exists.

- **Runs.** Every run uses the holdout split (162 questions) once. The baseline runs with every switch off. Each
  feature runs with only its own switch on (`--features <name>`). Each run writes `holdout_<name>.json`, next to
  this file. If a run stops part of the way through, it is run again once, and the table says so.
- **Target category.** The feature's target category accuracy minus the baseline's, in points, must be at least
  +5. Hybrid has two targets (extraction and multi_session). Each target must gain at least +5. There is one
  exception. If the baseline score in a target is above 95%, +5 is not possible. That target then must stay at or
  above the baseline. At least one target of the feature must gain +5. If a feature's only target has a baseline
  above 95%, the feature fails this gate.
- **Every other category.** For each category that is not a target (forget included), the feature minus the
  baseline must be -2 points or better.
- **Abstention errors.** The count of rows with `abstain_gold` true and `abstain_pred` not true (it answered when
  it had to abstain), plus the count of rows with `abstain_gold` false and `abstain_pred` true (it abstained when
  the answer was there). The denominator is every question in the run, because any question can get either
  error. The result must be 10% or less.
- **memory_search p95.** `summary.memory_search_p95_ms` must be 1500 ms or less.
- **Added prompt tokens per turn.** The feature's `summary.tokens_per_turn.in` minus the baseline's. This is the
  mean input tokens per question over all rounds, with tool results and cached tokens included. The result must
  be 500 or less.
- **Forget compliance.** `summary.forget_compliance` must be 1.0 (100%).
- **Default.** A feature that passes every gate gets the default ON in code. A feature that fails a gate gets the
  default OFF. The test `tests/test_memory_verdict.py` checks that each switch's default in code equals the
  Defaults table below.

## Results (holdout, 162 questions, run 2026-10-08)

Each run ran once. No run stopped part of the way, so no run was repeated. Every run used model gpt-6-luna, 0 errors. The baseline, hybrid and timeargs ran at the same time; then retry, validity and sources ran at the same time. Records-format runs (validity, sources) used their own cached dream builds; search runs used the plain build.

### Baseline (every switch off)

Accuracy 81.5% of 162; abstention 100.0% (n 21), eve_said 55.6% (n 18), cross_channel 73.3% (n 15), extraction 84.2% (n 19), forget 88.9% (n 18), knowledge_update 92.9% (n 14), multi_session 68.4% (n 19), plans 88.9% (n 18), temporal 80.0% (n 20). Abstention errors 2+23 of 162 = 15.4%. memory_search p95 9.6 ms. Input tokens per turn 9639. Forget compliance 0.8889. Errors 0. Ask cost $0.0478.

### hybrid (`--features hybrid`): FAIL, default OFF

Overall accuracy 81.5% → 96.3%. Errors 0. Ask cost $0.0570.

| Gate | Number | Result |
|---|---|---|
| Target extraction | 84.2 → 94.7 (+10.5 pts) | pass |
| Target multi_session | 68.4 → 94.7 (+26.3 pts) | pass |
| Every other category ≥ -2 pts | abstention -4.8; eve_said +44.4; cross_channel +26.7; forget +0.0; knowledge_update +7.1; plans +5.5; temporal +20.0 | FAIL |
| Abstention errors ≤ 10% | 3+2 of 162 = 3.1% | pass |
| memory_search p95 ≤ 1500 ms | 525.8 ms (132 calls) | pass |
| Added input tokens per turn ≤ 500 | 9267 - 9639 = -372 | pass |
| Forget compliance 100% | 0.8889 of 18 | FAIL |

### timeargs (`--features timeargs`): FAIL, default OFF

Overall accuracy 81.5% → 82.1%. Errors 0. Ask cost $0.0529.

| Gate | Number | Result |
|---|---|---|
| Target temporal | 80.0 → 85.0 (+5.0 pts) | pass |
| Every other category ≥ -2 pts | abstention +0.0; eve_said +0.0; cross_channel +0.0; extraction +0.0; forget +0.0; knowledge_update +0.0; multi_session -5.3; plans +5.5 | FAIL |
| Abstention errors ≤ 10% | 2+24 of 162 = 16.0% | FAIL |
| memory_search p95 ≤ 1500 ms | 9.6 ms (146 calls) | pass |
| Added input tokens per turn ≤ 500 | 9582 - 9639 = -57 | pass |
| Forget compliance 100% | 0.8889 of 18 | FAIL |

### retry (`--features retry`): FAIL, default OFF

Overall accuracy 81.5% → 95.1%. Errors 0. Ask cost $0.0546.

| Gate | Number | Result |
|---|---|---|
| Target abstention (baseline above 95%: must hold) | 100.0 → 95.2 (-4.8 pts) | FAIL |
| At least one target +5 | none gained 5 | FAIL |
| Every other category ≥ -2 pts | eve_said +38.9; cross_channel +26.7; extraction +15.8; forget +0.0; knowledge_update +7.1; multi_session +26.3; plans +5.5; temporal +10.0 | pass |
| Abstention errors ≤ 10% | 3+3 of 162 = 3.7% | pass |
| memory_search p95 ≤ 1500 ms | 12.3 ms (168 calls) | pass |
| Added input tokens per turn ≤ 500 | 10051 - 9639 = +412 | pass |
| Forget compliance 100% | 0.8889 of 18 | FAIL |

### validity (`--features validity`): FAIL, default OFF

Overall accuracy 81.5% → 73.5%. Errors 0. Ask cost $0.0537.

| Gate | Number | Result |
|---|---|---|
| Target knowledge_update | 92.9 → 85.7 (-7.2 pts) | FAIL |
| At least one target +5 | none gained 5 | FAIL |
| Every other category ≥ -2 pts | abstention +0.0; eve_said -27.8; cross_channel -20.0; extraction -15.8; forget +11.1; multi_session -15.8; plans +0.0; temporal +0.0 | FAIL |
| Abstention errors ≤ 10% | 0+41 of 162 = 25.3% | FAIL |
| memory_search p95 ≤ 1500 ms | 8.1 ms (161 calls) | pass |
| Added input tokens per turn ≤ 500 | 9430 - 9639 = -208 | pass |
| Forget compliance 100% | 1.0 of 18 | pass |

### sources (`--features sources`): FAIL, default OFF

Overall accuracy 81.5% → 74.7%. Errors 0. Ask cost $0.0540.

| Gate | Number | Result |
|---|---|---|
| Target multi_session | 68.4 → 57.9 (-10.5 pts) | FAIL |
| At least one target +5 | none gained 5 | FAIL |
| Every other category ≥ -2 pts | abstention +0.0; eve_said -16.7; cross_channel -6.7; extraction -5.3; forget +5.5; knowledge_update -7.2; plans -11.1; temporal -10.0 | FAIL |
| Abstention errors ≤ 10% | 1+37 of 162 = 23.5% | FAIL |
| memory_search p95 ≤ 1500 ms | 9.1 ms (149 calls) | pass |
| Added input tokens per turn ≤ 500 | 9239 - 9639 = -400 | pass |
| Forget compliance 100% | 0.9444 of 18 | FAIL |

## Summary

| Feature | Target | Other categories | Abstention errors | p95 | Tokens | Forget | Verdict |
|---|---|---|---|---|---|---|---|
| hybrid | pass | FAIL (abstention -4.8) | pass | pass | pass | FAIL (88.9%) | OFF |
| timeargs | pass (+5.0) | FAIL (multi_session -5.3) | FAIL (16.0%) | pass | pass | FAIL (88.9%) | OFF |
| retry | FAIL (abstention at ceiling, -4.8) | pass | pass | pass | pass | FAIL (88.9%) | OFF |
| validity | FAIL (-7.2) | FAIL | FAIL (25.3%) | pass | pass | pass | OFF |
| sources | FAIL (-10.5) | FAIL | FAIL (23.5%) | pass | pass | FAIL (94.4%) | OFF |

No feature clears every gate, so every switch stays OFF.

Notes, written after the results. They do not change a verdict.

- The forget gate cannot pass for a search-only feature. The baseline itself fails it: 2 of the 18 forget
  questions (q223, q224) fail with every switch off. A search switch does not change what the forget deletes.
  Hybrid and retry fail the bar on this gate and on one question each (abstention, 1 of 21 = -4.8 points).
- Hybrid and retry give the largest gains: overall 81.5% to 96.3% (hybrid) and 95.1% (retry). Retrieval hit
  rate goes from 31.7% to 71.5% (hybrid) and 68.3% (retry).
- Validity and sources lose accuracy in most categories (overall 73.5% and 74.7%). Most of the new misses are
  retrieval misses, and the abstention errors are abstaining when the answer was there.
- One holdout run per feature; a category is 14 to 21 questions, so one question is 5 to 7 points.

## Defaults

| Feature | Variable | Default |
|---|---|---|
| hybrid | `EVE_MEMORY_HYBRID` | OFF |
| timeargs | `EVE_MEMORY_TIMEARGS` | OFF |
| retry | `EVE_MEMORY_RETRY` | OFF |
| validity | `EVE_MEMORY_VALIDITY` | OFF |
| sources | `EVE_MEMORY_SOURCES` | OFF |
