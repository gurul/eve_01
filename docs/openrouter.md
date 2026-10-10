# One key: OpenRouter

Eve calls every model with one key: `OPENROUTER_API_KEY`. The owner asked
for this on 2026-10-08: "make everything use my openrouter key, remove open ai
and claude key". Before that change, the chat, the planner, the camera, memory,
notes and calls used `OPENAI_API_KEY` against OpenAI. The app builder used
`ANTHROPIC_API_KEY` against Anthropic.

Put the key in `~/.config/eve-bridge/env`:

```dotenv
OPENROUTER_API_KEY=your-openrouter-key
```

Two tools keep their own sign-ins and do not use this key:

- **Codex** (computer tasks, Codex chat) uses its ChatGPT plan login
  (`EVE_CODEX_AUTH=plan`, the default). `EVE_CODEX_AUTH=api` is an
  optional mode that bills Codex to an `OPENAI_API_KEY`.
- **Claude Code** sessions use the Claude Code login.

One feature still needs `OPENAI_API_KEY` for now: the **desk voice**, the
wake-word conversation. It uses OpenAI's realtime Live API, and OpenRouter does
not carry that API. A later change (wave 2) moves it. Without the key, the desk
voice is off and the daemon logs one line saying so. Every other feature works.

## Models by role

The October 9 configuration favors quality per dollar. The
[model comparison](evals/model-routing-confirmation-2026-10-09/VERDICT.md) records the
benchmark shortlist, synthetic checks, measured cost and limits of the evidence.
These are role defaults; existing environment overrides still apply.

| Role | Default model | Reasoning |
|---|---|---|
| Chat and generated UI cards | `gpt-6-luna` | low |
| App Think | `gpt-6.1-sol` | medium |
| Think hard | `gpt-6-astra` | high |
| Auto/tool/search routing | `typesafe/jev-1.13` on the OpenRouter route | typed decisions |
| Chief task assessment and replanning | `gpt-6-luna` | low |
| Web answer synthesis | `openai/gpt-6-luna` | low |
| Watches: page text, screenshots and search answers | `deepseek/deepseek-v4.1-flash` | disabled |
| Task and diary reflection | `gpt-6-luna` | medium |
| Autonomous app building | `claude-opus-5-5` | high |
| Memory extraction and record compaction | `gpt-6-luna` | low |
| Meeting and room-note summaries, including map/reduce | `gpt-6-luna` | low |
| Camera descriptions, locating and exploration notes | `gpt-6-luna` | low |
| Embeddings | local Gemma for mem0 on this installation; `text-embedding-3-small` for the hybrid index and standard mem0 default | none |

The phone retains `gpt-4o-transcribe` and `deepgram/aura-2`. Set
`EVE_VOICE=0` to disable desk real-time voice and
`EVE_CALL_LIVE_STT=0` to keep phone transcription on the upload path.
Neither switch disables phone calls. The local Gemma embedding experiment is
retained on this installation through `EVE_MEM0_EMBEDDER=gemma`; the standard setting is
`openrouter`. This model update does not rebuild or erase either vector store.

Web synthesis, watch checks and reflection have independent defaults. Changing
one no longer silently changes the others. Reasoning-capable calls reserve
room for internal tokens: web synthesis, chief reflection and scene requests
allow 2,048 total output tokens; exploration notes allow 1,024. Their prompts
still bound the visible answer. Watch replies keep the 200/300-token limits
with reasoning disabled. mem0 sends low effort explicitly and preserves its
2,000-token extraction cap even when the SDK recognizes a reasoning model.

The role-model changes preserve prompt wording. The separate confirmation-policy
update has its own [blind evaluation](evals/routine-confirmations-2026-10-09/VERDICT.md).
This model screen does not establish
an optimum for long documents, open-ended reasoning, live retrieval or coding;
Think, Think hard and app-building models retain their existing roles.

## The helper

`bridge/src/eve_bridge/openrouter.py` builds every model client:

| Function | What it returns |
|---|---|
| `available(env)` | Whether `OPENROUTER_API_KEY` is set. Every feature checks this before it starts. |
| `key(env)` | The key, stripped, or `""`. |
| `model(name)` | The OpenRouter id for a model name (table below). |
| `client()` / `async_client()` | The `openai` SDK client, with base URL `https://openrouter.ai/api/v1`, the key, and eve's `HTTP-Referer` and `X-Title` headers. |
| `anthropic_client()` | The `anthropic` SDK `AsyncAnthropic` client, with base URL `https://openrouter.ai/api` and the same key. |

The clients rewrite the `model` field of each request when they send it. Call
sites keep their model names as they were (`gpt-6-luna`), and OpenRouter
receives `openai/gpt-6-luna`. The rewrite happens in the SDK's request builder,
so it covers the Responses API, streaming, Chat Completions, embeddings,
transcription and speech.

### Model ids

| Call site name | Sent to OpenRouter as |
|---|---|
| `gpt-6-luna`, `gpt-6-astra`, `gpt-6.1-sol`, `gpt-5.4-nano`, `gpt-5-mini` | `openai/<name>` |
| `o3`, `o4-mini` (the o-series) | `openai/<name>` |
| `text-embedding-3-small` | `openai/text-embedding-3-small` (the `dimensions` you ask for: 512 for the search index, 1536 for mem0) |
| `gpt-4o-transcribe`, `gpt-4o-mini-transcribe` | `openai/<name>` |
| `claude-opus-5-5` | `anthropic/claude-opus-5.5` (with a dot, not a dash) |
| an id with a `/` (`deepgram/aura-2`, `google/gemini-3.1-flash-lite`) | unchanged |

### Spending

`spend.py` records each call at the call site, as before. The provider in each
row is now `openrouter`. When OpenRouter's reply includes `usage.cost`, the
meter uses that figure (`src: reported`). The model name in each row is the call
site's name, so `pricing.py` can still price a reply that has no cost figure.
Two kinds of row keep the provider `openai`: the desk voice's Live sessions and
the call's optional live ears.

## What changed, feature by feature

| Feature | Before | After |
|---|---|---|
| Telegram brain, app chat, calls (`telegram.py`, `computer_agent.make_response_creator`) | `AsyncOpenAI()` on `OPENAI_API_KEY` | `openrouter.async_client()`; the chat door opens only with `OPENROUTER_API_KEY` |
| Chief of staff, replanner (`chief.py`, `chief_replan.py`) | the door's OpenAI creator | the same creator, now on OpenRouter |
| Browser lane planner (`computer_agent.ComputerAgent`) | turns chained by `previous_response_id` | stateless: each turn resends the full history with `store: False` and the encrypted reasoning, because OpenRouter rejects `previous_response_id` |
| think_hard (`think.py`) | OpenAI | OpenRouter |
| Voice intent, camera scene, exploring notes, diary (`intent.py`, `scene.py`, `explore.py`, `diary.py`) | OpenAI | OpenRouter |
| Memory dream (`records.py`, `eve-bridge memory dream`) | OpenAI | OpenRouter |
| mem0 (`mem0_memory.py`) | both clients pinned to `api.openai.com` | both clients replaced with OpenRouter clients; the same `text-embedding-3-small` at 1536 dimensions |
| Hybrid memory search embeddings (`memory_index.py`) | `api.openai.com` | OpenRouter, the same model and dimensions |
| Meet summary, room-notes transcription and summary (`meet.py`, `notes.py`) | OpenAI | OpenRouter (`openai/gpt-4o-mini-transcribe` for the notes) |
| Room-notes speaker labels (`notes.py` diarization) | `gpt-4o-transcribe-diarize` | **off**: OpenRouter has no diarizing transcriber; notes are a plain transcript |
| Call ears (`phone_call.py`) | OpenAI realtime transcription (live ears), upload as fallback | each press is uploaded to OpenRouter (`openai/gpt-4o-transcribe`, `language=en`); the live ears are opt-in (`EVE_CALL_LIVE_STT=1` and an `OPENAI_API_KEY`) |
| Call voice (`phone_call.py`) | `deepgram/aura-2` on OpenRouter, OpenAI `gpt-4o-mini-tts` as fallback | `deepgram/aura-2` only; a failed sentence is not spoken, and the chat still has it |
| Tutor (`learning/tutor.py`) | OpenAI by default, OpenRouter opt-in | OpenRouter only (`openai/gpt-6-astra`) |
| App builder (`apps_maker.py`, `miniapp.py`) | `AsyncAnthropic()` on `ANTHROPIC_API_KEY` | `openrouter.anthropic_client()`, `anthropic/claude-opus-5.5`; the Mini App turns on with `OPENROUTER_API_KEY` |
| Web search answer model, watch model (`websearch.py`, `watch.py`) | `openai/*` ids refused | `openai/*` ids allowed; without an OpenRouter key, search is `off` |
| Desk lane visual judge (`decisions.py`, `desk_lane.py`) | OpenAI's Decisions beta when `EVE_DECISIONS=1` | **off**, with one log line. OpenRouter has no Decisions API, and Jev is not a drop-in replacement (see below). Keyword clicks still work; the rest goes to Codex. |
| Desk voice (`voice_agent.py`) | OpenAI Live API | unchanged until wave 2; off without `OPENAI_API_KEY` |

### Why the desk lane judge is off, and not on Jev

The judge sends screenshots and asks predicate questions such as "is this click
risky?". Jev (`jev.py`, on OpenRouter's `/api/alpha/decisions`) takes a text
state and choice questions. Moving the judge to Jev would mean writing a new
safety judgement, so the judge is off instead. The tools
`tools/decisions_eval.py` and `tools/decisions_live_demo.py` still call OpenAI's
Decisions beta with an OpenAI key, for development only.

### What you will notice

- **No speaker labels in room notes.** Notes still have the full transcript and
  the write-up.
- **Calls take a little longer to hear you.** Uploading a press adds about
  0.2 to 0.3 s compared with the live ears, which returned text 0.39 to 0.50 s
  after release.
- **No second call voice.** If Deepgram on OpenRouter fails for a sentence,
  that sentence is not spoken, and the chat has it.
- **The desk voice needs `OPENAI_API_KEY`** until wave 2.

## How to verify

1. Remove `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` from the env file, keep
   `OPENROUTER_API_KEY`, and restart the daemon. The log has one line about the
   desk voice and none about the chat, memory, notes or the Mini App.
2. Text eve on Telegram. It answers. On OpenRouter's activity page, the call
   appears as `openai/gpt-6-luna`.
3. Run the guard test. It fails if a production module reads either old key, or
   talks to `api.openai.com` or `api.anthropic.com`, outside the allowed list:

   ```bash
   cd bridge && uv run python -m pytest -q tests/test_openrouter.py
   ```

   The allowed list is: `codex_auth.py` (the optional `api` mode),
   `voice_agent.py` (the desk voice until wave 2), `phone_call.py` (the opt-in
   live ears), `spend_sync.py` (optional admin keys for the providers' own cost
   figures, never a model call), and `decisions.py` (development tools only).
