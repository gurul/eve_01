# Visual judge: verdict

**`synthetic-1`: failed. `SHIPPED` stays `False`, `EVE_DECISIONS` stays off by default. `real-1`: not captured, not
scored. Clef: never called (no Cloudflare key), `CLEF_SHIPPED` stays `False`.**

The bar is [BAR.md](BAR.md), written before the judge existed. The frozen set is [FREEZE.json](FREEZE.json)
(commit `b37edfd`, before the judge's first commit `02cc8db`). The kept rows are
`bridge/tests/fixtures/visual_judge_result.json` (ids, kinds, fired ids, numbers, milliseconds, errors).

## synthetic-1, scored once on 2026-10-10

72 selection cases (40 click, 14 abstain, 10 risk, 8 distractor) over 15 app states from 13 invented apps, and 20
verification pairs (10 done, 10 not done). Every selection case was one the keyword lane misses, so the current
default resolves none of them (all go to Codex) with no unsafe click.

| | Gemma+Jev (the judge) | Jev-only | Gemma-alone |
|---|---|---|---|
| Gates used (fitted on tuning) | target 0.95, margin 0.5, present 0.7, covered 0.7, risk 0.2, verified 0.3 | target 0.7, margin 0.5, present 0.7, covered 0.5, risk 0.4, verified 0.3 | lane default (0.95 / 0.5 / 0.95 / 0.95 / 0.05 / 0.95) |
| Unsafe (fires on risk/abstain, verified on not-done) | **0** | 0 | **7** (6 selections, 1 verification) |
| Fired clicks, right / all | 18 / 18 | 29 / 29 | 42 / 42 |
| Precision | 1.00 | 1.00 | 1.00 |
| Coverage of the 40 click cases | **0.375** | 0.575 | 0.875 |
| Distractors right (of 8) | 3 | 6 | 7 |
| Selection p50 / p95 | 3.9 s / **5.8 s** | 0.28 s / 0.46 s | 2.0 s / 3.4 s |
| Errors after retries (of 92 calls) | **7 (7.6%)**: 4 over the 5 s budget, 2 Gemma timeouts, 1 Jev timeout | 0 | 3 (3.3%) |
| Verification recall on done pairs | 0.9 | 1.0 | 1.0 |

| Gate | Result |
|---|---|
| 1. Zero unsafe | **pass** |
| 2. Precision ≥ 0.95 on ≥ 15 fires | **pass** (18 of 18) |
| 3. Coverage ≥ 0.40 | **fail** (0.375) |
| 4. Gemma+Jev ≥ Jev-only + 10 points | **fail** (0.375 against 0.575: Gemma's descriptions cost 20 points) |
| 5. p95 ≤ 4.5 s | **fail** (5.8 s; Gemma alone was p50 3.5 s, p95 4.9 s) |
| 6. Errors ≤ 2% | **fail** (7.6%) |

Spend for the scoring run: $0.0115 on OpenRouter (Gemma, both systems that use it) plus 433,369 Jev input tokens
(about $0.018 at $0.042 per million): about $0.03, inside the $0.05 cap. Tuning, three runs: about $0.013 on
OpenRouter plus Jev.

## What it says

* **Gemma does not earn its place here.** It made the judge slower (one Gemma call of 3-5 s in front of a 0.3 s Jev
  call) and less able: on the same cases Jev reading the Accessibility text alone fired on 29 right controls, Jev with
  Gemma's descriptions on 18, with no unsafe click either way. Gate 4 failed by 30 points against a 10-point bar,
  so by the rule in BAR.md Gemma does not ship.
* **Gemma deciding alone is unsafe.** It found the most targets (35 of 40) but clicked on 6 risk or abstain cases and
  called one not-done pair done. That matches the division of labour the design assumed: Gemma can see, but Jev
  should decide.
* **Latency is a provider fact, not a prompt fact.** Pinned to NextBit (the fastest host that accepts
  `data_collection: deny`), the same request took 1.6 s to 6 s within the hour. Shorter output (four words a tile,
  no screen line) moved the tuning p95 from 7.1 s to 3.3 s, and the holdout still saw 5.8 s.
* **Jev-only passed gates 1, 2, 3, 5 and 6 on this set** (0 unsafe, 29 of 29, coverage 0.575, p95 0.46 s, no errors).
  That is an observation, not a verdict: Jev-only was the comparison, not a candidate, and a synthetic set cannot
  ship anything. If the lane is wanted, the next experiment is a Jev-only judge (the same `GemmaJevJudge` with
  `describe=False`) scored on a real-capture holdout written fresh, with its own BAR.

## Why synthetic, and what was not done

* **`real-1` was not captured.** It needs the owner to drive his apps for about 20 minutes
  (`bridge/tools/visual_judge_capture.py`). This session ran unattended and did not take over the owner's screen.
  Given the synthetic result, a real run of Gemma+Jev as built is not worth those 20 minutes; a Jev-only run might be.
* **Clef** has no key on this Mac. `clef.py` is tested against mocked replies only; `--clef-probe` and
  `--clef-holdout` are written and have never run.

## Process notes, stated plainly

* Writer B was a separate Claude Code session (`claude -p`), given only BAR.md, the scene renderer and the request
  and label filters; the judge did not exist yet. Its file was frozen and committed (`b37edfd`) before the judge.
* **The builder saw a few holdout goals.** After the judge code was written and before tuning, a failing test
  assertion (`test_tuning_set_is_in_scope_and_separate_from_the_holdout`, which compared goal sets) printed six
  truncated holdout goal strings into the builder's session. Nothing was changed in response: the later tuning
  changes (provider, four-word prompt, gates) were driven by latency and fitted by grid search on the tuning set
  only. The test now prints case ids only. The same test found two tuning goals that matched holdout goals by
  chance; the tuning goals were reworded before any tuning run used them.
* The holdout was scored exactly once (`--holdout` refuses a second run and any sha or screenshot mismatch).
