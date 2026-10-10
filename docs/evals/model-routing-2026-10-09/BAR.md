# Model role screening: preregistered bar

Written before candidate responses or scores were available. An independent fixture author did not inspect changed implementation, model outputs, or sibling reports. The task brief and scope plan supplied the six roles. All people, places, dates, events, and source excerpts in the cases are synthetic.

This is a small screening set, not a robust quality evaluation or proof that the winner is best in production. It measures constrained factual extraction and decision consistency; it does not measure natural prose quality, broad coverage, live web retrieval, image understanding, long-context behavior, or actual tool execution. The reflection cases measure evidence discipline and bounded improvements, not open-ended insight.

## Frozen decision rule

- Roles: summary, watch, task_assessment, reflection, web_synthesis, memory_extraction. Each role has exactly three cases. Summary includes compaction.
- Send each candidate the exact supplied messages and response format. Do not send the expected answers or critical paths. Use the intended production reasoning effort, record it, and keep sampling settings identical across candidates wherever supported. Record unsupported settings; do not silently substitute a different model or configuration.
- Score each candidate once per case. Do not tune prompts, change expected answers, or retry a wrong answer. Transport failures may be recorded separately and retried only when no model response was obtained; keep every attempt's cost and outcome.
- A candidate is eligible for a role only if all three cases pass every schema and factual assertion, with zero critical errors. Any mismatch fails its case. An error on a preregistered critical path, malformed JSON, or invalid root structure is a critical error. Missing fields and unsupported extra fields fail the schema.
- Rank eligible candidates by total provider-reported cost for that role's three successful API responses, including reasoning tokens and any costs incurred by transport retries when reported. Missing provider-reported cost makes cost ranking inconclusive; do not treat missing cost as zero. Preserve raw usage metadata and request identifiers. Record all failed attempts separately.
- Let C be the lowest total cost among eligible candidates with complete cost data. Candidates costing at most 1.10 times C form the cost tie group. Choose the lowest median end-to-end wall latency within that group. If the medians tie exactly, prefer the lower total cost; if both tie, report a tie. When C is zero, only zero-cost candidates belong to the cost tie group. Report the absolute costs and times rather than only the ranking.
- If no candidate passes all three cases for a role, retain that role's current configuration and report no eligible winner. If only one candidate is eligible, describe it as the sole passing candidate, not a statistically established winner.
- Failed outputs become development data. Any later tuned prompt or configuration needs a fresh independent set and a new preregistered bar. Keep this set and its results for historical comparison.

Critical paths are fixed in cases.json. They identify false facts, unsupported certainty, unsafe action decisions, or attribution failures of particular concern. They supplement the all-assertions pass rule; they do not relax it.

## Runner and checker contract

Python 3.10+ and its standard library are sufficient. No model client, network, credentials, third-party package, or shell subprocess is used by check.py.

Read cases.json. For each case send case.messages and case.response_format to the candidate model. The response format contains the same schema included in the user message, so the task remains explicit even if a provider cannot enforce response_format; record that capability difference. Model name, effort, usage, cost, and wall time belong in runner results, not in the answer being scored.

Score a saved raw response:

```sh
python3 docs/evals/model-routing-2026-10-09/check.py --case watch-price-availability --response response.json
```

The checker emits one JSON result containing case_id, role, passed, critical_error, and errors. Exit 0 means pass, 1 means the candidate answer failed, and 2 means a command/manifest/input-file error. Leading/trailing whitespace is allowed. Markdown fences, duplicate JSON keys, nonstandard numeric constants, extra keys, wrong types, reordered arrays, omitted facts, or different enum values fail. Array ordering and duplicate rules are stated in every system message. This strictness is part of the screen, not evidence of broad model quality.

For an in-process runner, import check.py and call load_manifest(), then score(case, parsed_response). For identical parsing to the CLI, use score_text(case, raw_response_text). A response already parsed by another JSON library may have lost duplicate-key information, so score_text is preferred.

Inspect the frozen inputs and test the oracle before evaluating candidates:

```sh
python3 docs/evals/model-routing-2026-10-09/check.py --validate
python3 docs/evals/model-routing-2026-10-09/check.py --selftest
```

The selftest accepts every expected fixture, mutates each scalar expected value, removes every required field, injects unsupported fields, reverses multivalue arrays, duplicates array items, checks explicit consent errors, and rejects invalid/duplicate-key JSON. Expected fixtures only test the checker; they are never model observations and must never appear as scored candidate results.
