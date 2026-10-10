# OpenRouter parity: verdict

Run 2026-10-08 against the bar in openrouter-parity-BAR.md (committed before any run, in bridge/docs/evals at the
time; moved here with this verdict).

| Arm | Run 1 | Run 2 | Total |
|---|---|---|---|
| direct (a157686, OpenAI key) | 15/16 | 16/16 | 31/32 |
| openrouter (e8b097f, OPENAI_API_KEY and ANTHROPIC_API_KEY blank) | 15/16 | 16/16 | 31/32 |

The one miss in each arm is the same case, `message-before-last`. No provider error in the openrouter arm (one TLS
retry happened in each arm, on the local network). With the OpenAI key blank, every answer in that arm came through
OpenRouter.

**Bar 1 (openrouter >= direct - 2): PASS (31 >= 29). Bar 2 (no provider error): PASS. The switch ships.**
