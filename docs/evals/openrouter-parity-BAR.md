# OpenRouter parity: pre-registered bar

Written 2026-10-08 before any parity run. Amended once, also before any run: the chief's capture set was
dropped because it is a holdout scored exactly once (tools/chief_eval.py), and every other brain eval in tools/ is a
holdout too; chat_memory_eval is the re-runnable dev set, so it runs twice per arm instead. The switch moves the same model ids from OpenAI's API to OpenRouter
(owner: every call on their own OpenRouter key). It ships only if both evals clear this bar.

Arms: **direct** = commit a157686 (OpenAI key, api.openai.com). **openrouter** = feat/openrouter-only e8b097f
(OPENROUTER_API_KEY only; OPENAI_API_KEY and ANTHROPIC_API_KEY unset). One run per arm, same fixtures, same day.

1. `tools/chat_memory_eval.py` (16 recall cases, the chat brain with its tools), run **twice** per arm:
   openrouter passes over both runs >= direct passes over both runs - 2 (of 32).
2. No run in the openrouter arm may end in a provider error (HTTP 4xx/5xx, a refused parameter); a case that errors
   counts as a fail and blocks the switch until its cause is understood.

Latency is reported, not gated. The bar is not changed after results exist.
