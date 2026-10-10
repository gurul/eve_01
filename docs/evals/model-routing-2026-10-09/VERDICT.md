# Exploratory model-role screen: verdict

This run provisionally selects GPT-6 Luna for summary/compaction, task assessment, reflection, web synthesis, and memory extraction, and DeepSeek V4.1 Flash for watches. These are selections among the recorded configurations on a small synthetic screen, not demonstrated production-quality winners. A sampling deviation from [the preregistration](BAR.md) makes this run exploratory; a fresh independent confirmation set is required before treating the comparison as a compliant confirmation experiment.

Independent verification rescored all 57 saved raw responses using the unchanged frozen checker, reconciled every summary value against response usage and elapsed time, checked unique request evidence without publishing it, and reapplied the 10% cost tie rule. All 19 role/model combinations passed all three cases with zero critical errors, zero saved transport errors, and normal stop finishes. Total provider-reported spend was **$0.034552920**. Reported cached input tokens totaled zero. No model calls were made during verification.

## Provisional selections

Cost is the sum for three cases, including reported reasoning cost. Latency is the median of three observed wall times.

| Role | Selected configuration | Reasoning | Three-case cost | Median latency |
|---|---|---|---:|---:|
| memory_extraction | `openai/gpt-6-luna` | low | $0.000213300 | 2.052 s |
| reflection | `openai/gpt-6-luna` | medium | $0.000281900 | 2.790 s |
| summary | `openai/gpt-6-luna` | low | $0.000232600 | 1.811 s |
| task_assessment | `openai/gpt-6-luna` | low | $0.000247500 | 2.938 s |
| watch | `deepseek/deepseek-v4.1-flash` | disabled | $0.000190545 | 0.910 s |
| web_synthesis | `openai/gpt-6-luna` | low | $0.000263900 | 2.094 s |

For watches, Luna was cheapest at $0.000175100. DeepSeek cost 8.82% more, within the preregistered 10% tie group, and had the lower observed median (0.910 s versus 1.709 s). Every other role had only Luna in its cheapest-cost tie group. Three observations do not establish a stable latency advantage.

## Complete comparison

Every row passed 3/3 cases. Costs and times were independently recalculated from saved responses, not copied from the summary without checking it. [results.json](results.json) contains the sanitized measurements, configurations, and mechanically derived selections.

| Role | Model | Three-case cost | Median latency |
|---|---|---:|---:|
| summary | `openai/gpt-5.4-nano` | $0.000557700 | 1.276 s |
| summary | `deepseek/deepseek-v4.1-flash` | $0.000726975 | 2.984 s |
| summary | `anthropic/claude-haiku-5.5` | $0.000404300 | 2.471 s |
| summary | `openai/gpt-6-luna` | $0.000232600 | 1.811 s |
| watch | `google/gemini-3.1-flash-lite` | $0.000598750 | 1.256 s |
| watch | `deepseek/deepseek-v4.1-flash` | $0.000190545 | 0.910 s |
| watch | `anthropic/claude-haiku-5.5` | $0.000328000 | 2.010 s |
| watch | `openai/gpt-6-luna` | $0.000175100 | 1.709 s |
| task_assessment | `openai/gpt-6-astra` | $0.016800000 | 2.176 s |
| task_assessment | `openai/gpt-6-luna` | $0.000247500 | 2.938 s |
| task_assessment | `openai/gpt-6.1-sol` | $0.003360000 | 2.051 s |
| reflection | `google/gemini-3.1-flash-lite` | $0.000741500 | 1.490 s |
| reflection | `openai/gpt-6.1-sol` | $0.004188000 | 2.265 s |
| reflection | `openai/gpt-6-luna` | $0.000281900 | 2.790 s |
| web_synthesis | `google/gemini-3.1-flash-lite` | $0.000802750 | 1.262 s |
| web_synthesis | `google/gemini-3.8-flash` | $0.003852750 | 3.671 s |
| web_synthesis | `openai/gpt-6-luna` | $0.000263900 | 2.094 s |
| memory_extraction | `openai/gpt-5.4-nano` | $0.000587350 | 2.074 s |
| memory_extraction | `openai/gpt-6-luna` | $0.000213300 | 2.052 s |

## Configuration and runner review

All requests used the exact frozen messages and JSON schema, a 2,048-token output cap, and provider-reported usage. Summary, task assessment, web synthesis, and memory extraction requested low reasoning where configured; reflection requested medium for Luna and Sol. Flash-Lite had no explicit reasoning configuration. Watch reasoning was disabled for DeepSeek and Haiku, and set to none for Luna. Actual configurations are preserved in results.json.

The original runner set temperature=0 for Flash-Lite and DeepSeek watches, but omitted it for DeepSeek summaries and Gemini 3.8 synthesis even though the saved catalog lists temperature support for both. This violates the preregistered requirement to equalize supported sampling settings. It cannot be repaired by changing the already observed cases or silently relabeling the results. The measured costs remain valid for those recorded configurations.

The original runner reused response filenames keyed only by case and model, without binding configuration or prompt hashes. It did not retain returned model/provider metadata. This audit found a complete consistent saved run, unique request identifiers, no retries in the runner, and no arithmetic error. The saved evidence cannot independently prove provider routing or that every requested knob was honored. Reported zero reasoning tokens for some reasoning-enabled candidates also does not establish that the requested reasoning mode was applied. Six concurrent requests, fixed submission order, transient service load, and three samples limit latency comparisons. No provider was pinned.

The reusable [run_screen.py](run_screen.py) addresses repeatability: it is offline by default, reads only OPENROUTER_API_KEY from the process environment after explicit --run, validates candidate settings against saved catalogs, applies temperature=0 wherever supported under the default matched policy, binds exact requests and configurations to hashes, records returned model/provider fields when supplied, and refuses an existing output directory. It performs no automatic retries and never silently reuses previous output. Missing reported costs stay unknown. Provider parameter support is required and provider fallback is disabled; these stricter request settings differ from the exploratory runner.

The runner accepts --cases, --checker, --bar, repeated --catalog, and an optional --models role-to-model-list JSON map. Catalog snapshots must cover every candidate. Without --run it prints only an offline plan. Add --run and --out with a new directory to authorize the planned network requests and costs. --temperature-policy historical reproduces the recorded temperature choices only for diagnostic comparison; it does not correct the original experiment.

```sh
python3 docs/evals/model-routing-2026-10-09/run_screen.py --selftest
python3 docs/evals/model-routing-2026-10-09/run_screen.py --cases CASES_JSON --checker CHECKER_PY --bar BAR_MD --catalog CATALOG_JSON
```

## Official benchmark context

The authenticated [OpenRouter benchmarks API](https://openrouter.ai/api/v1/benchmarks) snapshot has as_of **2026-10-10T00:00:33.175Z**. It includes OpenRouter results plus imported Artificial Analysis and Design Arena data. The returned benchmark_type values are gpqa_diamond, tau_bench_verified_airline, search_browsecomp, search_dsqa, search_hle, and search_widesearch. **No Deep SWE benchmark was returned.** Imported coding or agentic indices must not be described as Deep SWE scores.

Selected official GPQA Diamond entries from that snapshot are below. Model versions, sample sizes, run dates, and benchmark settings differ; these are external context, not a matched comparison at the role screen's low/medium settings. Costs are the benchmark's reported average per task, not prices quoted for Eve requests.

| API model entry | GPQA accuracy | Tasks | Avg. cost/task | Last run (UTC date) |
|---|---:|---:|---:|---|
| `google/gemini-3.8-flash-20260902` | 95.6370% | 191 | $0.068228 | 2026-10-01 |
| `openai/gpt-6-astra-20260903` | 94.4444% | 198 | $0.113332 | 2026-10-01 |
| `openai/gpt-6.1-sol-20260929` | 94.4444% | 198 | $0.021367 | 2026-10-01 |
| `openai/gpt-6-luna` | 89.8477% | 197 | $0.003442 | 2026-10-08 |
| `deepseek/deepseek-v4.1-flash` | 87.8788% | 198 | $0.010644 | 2026-10-08 |
| `anthropic/claude-haiku-5.5-20261007` | 86.5881% | 594 | $0.004286 | 2026-10-08 |
| `google/gemini-3.1-flash-lite-20260507` | 81.5937% | 594 | $0.041374 | 2026-10-01 |
| `openai/gpt-5.4-nano-20260317` | 77.7217% | 594 | $0.013080 | 2026-10-01 |

The snapshot also has dated Luna and DeepSeek entries: Luna-20260922 scored 87.3737% over 198 tasks on October 1; DeepSeek-20260910 scored 88.9450% over 594 tasks on October 3. Those differ from the later alias rows and must not be merged into a single measurement.

On tau_bench_verified_airline, the dated DeepSeek entry reports 76.2222% over 150 tasks at $0.031539/task; the dated Luna entry reports 67.3333% over 50 tasks at $0.009285/task. This exposes a capability/cost tradeoff on that benchmark; it does not predict Eve's watch or memory quality. The imported Artificial Analysis intelligence indices include Luna (Max) 38.1, DeepSeek (Max) 39.5, and Gemini 3.8 Flash (High) 40.9, which again use different settings.

The saved current model catalog confirms the requested model IDs and supported settings for the revised shortlist. Its pricing can differ from benchmark metadata, so the screen ranking uses response usage.cost rather than reconstructed list-price estimates. This text-only screen supplies no evidence for selecting a vision model, regardless of catalog modality support.

## Limits and next decision

The cases are generic constrained synthetic tasks. They do not run production prompts, realistic long contexts or compaction lengths, images, live retrieval, tools, real user memory, or open-ended prose/reflection quality. Strict enum schemas and short, explicit tasks can hide quality differences; all candidates passing 3/3 does not establish equivalence. Token overhead and relative costs may change substantially on production workloads.

Treat this set as spent development data. Keep the provisional configuration choices separate from final deployment evidence, and run a fresh blind confirmation set with supported sampling parameters aligned. Preserve this run as exploratory history rather than replacing its outputs.
