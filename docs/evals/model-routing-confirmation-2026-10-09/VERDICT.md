# Fresh model-role confirmation verdict

This fresh screen selects GPT-6 Luna for summary/compaction, task assessment, reflection, web synthesis, and memory extraction, and DeepSeek V4.1 Flash for watches under the [frozen decision rule](BAR.md). These are selections among the tested configurations on three short synthetic cases per role; they are not proof of production quality or model equivalence.

Independent offline verification rescored all 57 saved raw responses using the unchanged frozen checker. All 19 role/model configurations passed 3/3 cases with zero critical errors. The audit verified the five frozen file hashes, plan and catalog hashes, exact request and configuration hashes, unique private request identities, returned model/provider metadata, supported sampling settings, usage costs, and every summary value. All returned model aliases matched the requested aliases. The saved run contains no transport errors or truncated finishes; the reviewed runner performs no automatic retries. No network calls were made during this audit.

Total provider-reported cost was **$0.031566910**. Reported cached prompt tokens totaled 0. Costs below sum the three responses; latency is their median observed end-to-end wall time.

## Selected configurations

| Role | Model | Requested reasoning | Three-case cost | Median latency |
|---|---|---|---:|---:|
| memory_extraction | `openai/gpt-6-luna` | low | $0.000231100 | 2.948 s |
| reflection | `openai/gpt-6-luna` | medium | $0.000246800 | 2.619 s |
| summary | `openai/gpt-6-luna` | low | $0.000242500 | 2.831 s |
| task_assessment | `openai/gpt-6-luna` | low | $0.000269000 | 2.364 s |
| watch | `deepseek/deepseek-v4.1-flash` | disabled | $0.000117240 | 4.563 s |
| web_synthesis | `openai/gpt-6-luna` | low | $0.000246400 | 4.310 s |

DeepSeek's watch median was **4.563 s**, slower than Luna's 2.097 s, Flash-Lite's 1.087 s, and Haiku's 2.127 s. It was selected because its three-case cost was the lowest ($0.000117240); Luna cost $0.000172500, outside the 10% cost tie group. The latency tie-break therefore did not apply. Every role's selected model was the sole member of its cheapest-cost tie group. This run supports a cost choice, not a claim that the watch path became faster. The previous exploratory run's DeepSeek latency advantage did not repeat.

## Complete comparison

Every row passed 3/3. [results.json](results.json) contains sanitized per-case costs, durations, token counts, requested configurations, provider names, and the mechanically derived selections. Decimal values are stored as strings to preserve the saved numbers without binary floating-point summation artifacts.

| Role | Model | Three-case cost | Median latency |
|---|---|---:|---:|
| memory_extraction | `openai/gpt-5.4-nano` | $0.000938450 | 1.940 s |
| memory_extraction | `openai/gpt-6-luna` | $0.000231100 | 2.948 s |
| reflection | `google/gemini-3.1-flash-lite` | $0.000686750 | 1.297 s |
| reflection | `openai/gpt-6-luna` | $0.000246800 | 2.619 s |
| reflection | `openai/gpt-6.1-sol` | $0.003936000 | 2.207 s |
| summary | `anthropic/claude-haiku-5.5` | $0.000378800 | 2.864 s |
| summary | `deepseek/deepseek-v4.1-flash` | $0.000546720 | 7.615 s |
| summary | `openai/gpt-5.4-nano` | $0.000573500 | 1.806 s |
| summary | `openai/gpt-6-luna` | $0.000242500 | 2.831 s |
| task_assessment | `openai/gpt-6-astra` | $0.016800000 | 2.251 s |
| task_assessment | `openai/gpt-6-luna` | $0.000269000 | 2.364 s |
| task_assessment | `openai/gpt-6.1-sol` | $0.003360000 | 2.095 s |
| watch | `anthropic/claude-haiku-5.5` | $0.000343400 | 2.127 s |
| watch | `deepseek/deepseek-v4.1-flash` | $0.000117240 | 4.563 s |
| watch | `google/gemini-3.1-flash-lite` | $0.000603250 | 1.087 s |
| watch | `openai/gpt-6-luna` | $0.000172500 | 2.097 s |
| web_synthesis | `google/gemini-3.1-flash-lite` | $0.000676000 | 1.243 s |
| web_synthesis | `google/gemini-3.8-flash` | $0.001198500 | 2.446 s |
| web_synthesis | `openai/gpt-6-luna` | $0.000246400 | 4.310 s |

## Configuration and provenance

The runner sent the exact fresh fixture messages and response schemas, capped every response at 2,048 tokens, and set temperature to zero whenever the saved catalog declared support. Temperature was omitted only for catalog entries without support. This corrects the sampling mismatch documented in the [earlier exploratory report](../model-routing-2026-10-09/VERDICT.md); that earlier set remains spent development data.

Summary, task assessment, web synthesis, and memory extraction requested low reasoning where configured. Reflection requested medium for Luna and Sol; Flash-Lite used its provider default without an explicit reasoning setting. Watch reasoning was disabled for DeepSeek and Haiku and set to none for Luna. Returned model and provider fields were retained privately and checked, but internal effective reasoning effort is not independently observable from these responses. Zero reported reasoning tokens cannot prove that a requested effort was applied. The report records that limitation instead of claiming effective effort was verified.

Requests required provider parameter support and disabled provider fallback, but did not pin a provider. Six concurrent requests, fixed submission order, changing provider load, and three observations per arm constrain latency conclusions. Hashes bind the recorded requests to the frozen inputs; saved metadata is not an independent audit of provider internals. The reviewed runner creates a fresh output directory, refuses prior output, records hashes, and does not retry a response.

Reproduce the audit from the repository root with the saved private artifacts present:

```sh
python3 docs/evals/model-routing-confirmation-2026-10-09/verify-freeze.py
python3 docs/evals/model-routing-confirmation-2026-10-09/verify-results.py
```

The audit re-runs the frozen checker's controls and ten additional controls for corrupted responses, wrong configurations/hashes/models, missing cost, truncation, temperature support, cost-tie eligibility, and detecting an added SWE benchmark. Private request identifiers, response text, credentials, and machine-specific paths are excluded from the published data by an explicit field allowlist.

## Benchmark context and limits

The saved [OpenRouter benchmarks API](https://openrouter.ai/api/v1/benchmarks) snapshot is dated **2026-10-10T00:00:33.175Z**. Its returned benchmark types are GPQA Diamond, verified airline tau-bench, BrowseComp, DSQA, HLE search, and WideSearch. **No DeepSWE metric appears in that supplied snapshot.** Artificial Analysis coding or agentic indices are separate measurements and must not be relabeled as DeepSWE. The [original report](../model-routing-2026-10-09/VERDICT.md#official-benchmark-context) preserves the source-specific benchmark details; their differing model versions, dates, and reasoning settings do not establish Eve role quality.

These 18 tasks are short, synthetic, constrained exact-JSON tests. They do not evaluate production prompts, realistic long contexts or compaction lengths, natural prose, open-ended reflection, image understanding, live retrieval, real tool execution, or real owner memory. All candidates passing three cases can hide meaningful differences. This verdict includes no vision-screen or actual-path-smoke score; those are separate evidence and are not part of the 57 responses. Relative costs can change with production token lengths, caching, and provider pricing.

The fixtures were authored independently before the confirmation outputs and were not tuned afterward. This completed set is now spent development data. Further prompt or configuration tuning needs fresh independent evidence.
