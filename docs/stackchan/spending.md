# What eve spends

The owner asked on 2026-09-24: "build in a dashboard that tracks costs (ik some
of them are other keys but can u track pricing, i want to see how much i am
spending everyday)".

eve now records the cost of every paid call it makes, as the call happens. It
also reads each provider's own total when it can. You can see both in two
places:

- **The Mini App.** The header's "$X spent today" is now everything eve spent
  today, not only app builds. Tap it to open the **Spending** view:
  - today's total, split by provider and by feature
  - yesterday's total and this month's total
  - a bar chart of the last 30 days (tap a bar to see that day)
  - what each provider says, next to eve's own figure
  - a short list of what is not tracked
- **Telegram.** `/spend` (or `spend`, `spending`) answers straight from the
  ledger, with no model call. It shows today, yesterday, this month, the top
  three features today and OpenRouter's own figure for today. It works while
  the Claude or Codex relay is on, the same way `/apps` does.

Code: `bridge/src/eve_bridge/spend.py` (the meter and the reader),
`spend_sync.py` (the providers' figures) and `pricing.py` (the rates). Tests:
`bridge/tests/test_spend.py` and `test_spend_wiring.py`, plus the voice, chat,
`/spend` and Mini App tests in their own files.

## How it is tracked

Each paid call writes one short line to a per-day file,
`~/.config/eve-bridge/spend/<local date>.jsonl`. `EVE_SPEND_DIR`
moves the folder. A line looks like this:

```json
{"t": 1790000000, "p": "openrouter", "m": "gpt-6-luna", "f": "chat", "usd": 0.00042,
 "src": "reported", "tok": {"in": 3100, "cached": 2900, "out": 80}}
```

- Since 2026-10-08 every model call goes through OpenRouter on one key
  ([one key: OpenRouter](../openrouter.md)), so the provider is `openrouter` and the
  dollars are OpenRouter's own `usage.cost` when the reply carries it (`reported`).
  The desk voice's Live sessions are the one exception: they are still OpenAI's, and
  stay `openai`. Older lines keep the provider they were written with.

- A line holds the provider, model, feature, dollars and token counts, and
  sometimes a short note. It never holds your words or eve's answer.
- `src` is `reported` when the provider sent its own cost with the reply (for
  example OpenRouter's `usage.cost`). It is `priced` when eve worked out the
  cost from the token counts, using the rates below.
- If a model has no rate, the line gets `"usd": null` and `"unpriced": true`,
  and the log notes it once. eve never guesses a price. The dashboard shows
  these calls as "without a price, not in the total" and names the model.
- Days follow your local clock. Providers count in UTC days, so near midnight
  the two figures can differ.
- Writing a line can never make a turn fail. If the write fails, eve logs one
  line and moves on.

### Where each cost comes from

| Feature | What is metered | How |
|---|---|---|
| chat | Every model call in a Telegram text turn (`telegram.py`) | Responses usage × rate |
| thinking | Each round of `think_hard` (`think.py`) | Responses usage × rate, plus hosted web searches |
| voice | The Live session's billed seconds (`session.usage.updated` / `session.closed`). Each delegated backend call (`response.completed`). The intent classifier (`intent.py`). | $0.05 per minute, billed per second. Backend and classifier: usage × rate. A session that sent no usage event is timed by eve's own clock and marked "clock-timed". |
| browser & Mac tasks | Every model call of the Responses computer agent: the loop, the plan and the checks (`computer_agent.py`) | Responses usage × rate |
| search | Each web search through OpenRouter (`websearch.py`) | OpenRouter's own `usage.cost`. Without it, the line is unpriced (the search fee alone would not be the full cost). |
| jev | Each Jev decision (`jev.py`, the route's host is the provider) | OpenRouter's `usage.cost` when present, else input tokens × Jev's rate. Output is free. |
| app builder | Each build or repair round (`miniapp.py`, `apps_maker.py`), Claude through OpenRouter | OpenRouter's `usage.cost`, else Anthropic usage × rate |
| memory | The nightly dream (`records.py`). mem0's fact extraction and embeddings (`mem0_memory.py`, which wraps mem0's own clients). | usage × rate |
| camera | Describe and locate (`scene.py`) | Responses usage × rate |
| exploring | Explore notes and diary thoughts (`explore.py`, `diary.py`) | Responses usage × rate |
| transcription, room notes | Room-note transcription and the write-up (`notes.py`); the speaker pass (`gpt-4o-transcribe-diarize`) is off since 2026-10-08 | OpenRouter's `usage.cost`, else transcription token usage × rate, or the clip's length × $0.003 a minute when the reply has neither |
| lessons | Tutor turns (`learning/tutor.py`) and Exa practice searches (`learning/search.py`) | usage × rate, or OpenRouter's or Exa's own cost |
| codex | Each Codex computer task and Codex chat turn | Counted with no price (see below) |

## Where the prices come from

The rates are in `pricing.py`. Each one has a comment that gives its source and
the date it was checked. All are USD per million tokens unless stated.

- **OpenAI**, standard tier, from developers.openai.com/api/docs/pricing,
  checked 2026-09-24:

  | Model | Input | Cached input | Output |
  |---|---|---|---|
  | gpt-6-astra | $10 | $1 | $50 |
  | gpt-6-sol | $2 | $0.20 | $10 |
  | gpt-6-luna | $0.10 | $0.01 | $0.50 |
  | gpt-5-mini | $0.25 | $0.025 | $2 |
  | gpt-5.4-nano | $0.20 | $0.02 | $1.25 |

  Other OpenAI prices eve uses:
  - gpt-live-1: $0.05 a minute, billed per second. Its backend calls are
    charged at the backend model's own rate.
  - gpt-4o-mini-transcribe: $1.25 input and $5 output per million tokens, or
    about $0.003 a minute.
  - text-embedding-3-small: $0.02.
  - Hosted web search: $10 per 1,000 calls.
- **Anthropic**, from platform.claude.com/docs/en/about-claude/pricing,
  checked 2026-09-24. claude-opus-5-5:

  | Input | Output | Cache read | 5-minute cache write | 1-hour cache write |
  |---|---|---|---|---|
  | $4 | $20 | $0.20 | $5 | $8 |

- **Jev**: $0.042 per million input tokens, output free (docs.typesafe.ai,
  2026-09-21).
- **OpenRouter** and **Exa**: eve uses the cost the provider reports with the
  reply, so no rate table is needed.

To add a model, add its row to the right table in `pricing.py` with its source
and date. Until you do, its calls are recorded unpriced.

## The providers' own figures

The daemon asks each provider for its own total when it starts and then every
hour. It does this on a worker thread. The answers are saved in
`~/.config/eve-bridge/spend/providers.json` and are never mixed into
eve's own ledger.

- **OpenRouter** (whenever `OPENROUTER_API_KEY` is set):
  - `GET /api/v1/key` gives eve's key's own `usage_daily` and
    `usage_monthly`, for the current UTC day and UTC month.
  - `GET /api/v1/credits` gives the whole account's `total_usage`, across every
    key, including keys other tools use. The account's spend for a day is the
    change in `total_usage` since the previous day's last check. If eve has
    no check from the day before, it counts from today's first check and shows
    "(since the first check today)".
- **OpenAI** and **Anthropic**: only when an admin key is set (see the next
  section).

If a provider cannot be read, eve logs one line for that provider and reason,
and the provider's row shows the reason (for example "HTTP 403"). Nothing else
is affected. Keys go in request headers only. They are never logged or saved.

## Adding the OpenAI and Anthropic totals

eve's normal keys cannot read either provider's cost report. Checked
2026-09-24:
- OpenAI's costs endpoint returned 403 for eve's key, because the key lacks
  the `api.usage.read` scope.
- Anthropic's `cost_report` returned 401, because it needs an Admin API key.

To see those totals:

1. **OpenAI**: create an **Admin key** at platform.openai.com → Settings →
   Organization → Admin keys. Only an organization owner can do this.
2. **Anthropic**: create an **Admin API key** (it starts with `sk-ant-admin`)
   in the Claude Console → Settings → Admin keys. Only an organization admin
   can do this.
3. Add them to `~/.config/eve-bridge/env`:

   ```
   EVE_OPENAI_ADMIN_KEY=<the OpenAI admin key>
   EVE_ANTHROPIC_ADMIN_KEY=<the Anthropic admin key>
   ```

4. Restart the daemon. The first sync runs at start. The Spending view then
   shows each provider's total for today (UTC day) and this month, next to
   eve's own figure.

An admin key can read and change the organization's settings. It is only ever
sent to the cost endpoints:
- OpenAI: `GET https://api.openai.com/v1/organization/costs` with
  `start_time` and `bucket_width=1d`.
- Anthropic: `GET https://api.anthropic.com/v1/organizations/cost_report` with
  `starting_at` and `bucket_width=1d`, and the `x-api-key` and
  `anthropic-version: 2023-06-01` headers. Anthropic's `amount` is in cents.

Leave the variables unset and eve never asks either provider.

## Settings

| Setting | Default | What it does |
|---|---|---|
| `EVE_SPEND_DIR` | `~/.config/eve-bridge/spend` | Where the ledger and `providers.json` live. |
| `OPENROUTER_API_KEY` | none | Already used for search and Jev. With it set, OpenRouter's own figures are read hourly. |
| `EVE_OPENAI_ADMIN_KEY` | none | An OpenAI Admin key. With it set, OpenAI's own cost report is read hourly. |
| `EVE_ANTHROPIC_ADMIN_KEY` | none | An Anthropic Admin API key. With it set, Anthropic's own cost report is read hourly. |

## What is not tracked

- **Codex** runs on your ChatGPT plan and is not billed per call. eve counts
  each Codex task and Codex chat turn with no price (`"note": "ChatGPT plan,
  not per-call"`), so the dashboard can show how often Codex ran without making
  up a cost.
- **Composio** bills on its own account. eve's Composio calls are not model
  calls and are not metered.
- **Other keys** show up only in a provider's own figure: OpenRouter's
  whole-account row, or OpenAI's and Anthropic's totals once admin keys are
  set. eve's meter only sees eve's own calls.
- Claude Code's own usage has a separate estimate (`jsonl_tailer.py`, shown in
  the statusline). That estimate is not part of this dashboard. It prices a
  model `ANTHROPIC_RATES` names (Claude Opus 5.5) at that row, and any other
  Claude model at the older per-family rates in `pricing.py`.
