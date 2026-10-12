# Small talk

`bridge/src/eve_bridge/small_talk.py` answers short chat messages ("hi", "thanks", "good night") with Haiku 5.5
instead of the full chat brain. The eval is `bridge/tools/small_talk_eval.py`. The acceptance ledger is
[small-talk-gates.md](small-talk-gates.md).

## Why

On 2026-10-12 the owner asked: "can simple interactions be handled with haiku for really fast speed, like hi
shouldn't take this long to respond to".

The daemon log from that day showed the cost:

- One-call chat turns took 4.2 to 11.8 s. That is the Codex subscription path (`EVE_CODEX_SUBSCRIPTION=1`), on
  since 2026-10-11. Before that, on OpenRouter luna, the same turns took 2.3 to 2.6 s.
- Every turn carried 37 to 58 thousand characters of instructions, even for "hi".

Haiku 5.5 on OpenRouter answers a short message under a short prompt in about 1.3 to 1.9 s.

## Where it runs

The lane runs inside a chat turn, in `TelegramInlet._small_talk_lane` (telegram.py). That covers both Eve Link
app turns and Telegram turns. The order is:

1. Every code path in `_handle` runs first: stop, the code words, the relays, a waiting question's answer, the TV,
   the camera, the lights, the music and quick answers. A message one of these claims never reaches the lane.
2. The turn takes the turn lock. Then the lane runs.
3. Only on a hand-off does the turn go on. Then come the grade router, the toolset router, the prompt's building
   and the brain's call. A lane turn runs none of them.

Only a message that came straight out of `_handle`'s last step can use the lane (`_turn(lane=True)`). These never
do:

- the quick answer's fallback when its source fails
- a rundown
- a picture or file turn
- a rundown hand-off
- the America search
- a message that arrived while another of the owner's turns was still in flight

## The code gate

The gate is `small_talk.gate`. It makes no model call and no I/O. Every condition must hold:

| Condition | How the inlet reads it |
|---|---|
| Text only: no picture, file or voice attachment | `inbound.image`, `inbound.uploads` |
| Not a tapped button's words | `inbound.tapped` |
| Not a slash command | the text starts with `/` |
| At most 8 words and 60 characters | `MAX_WORDS`, `MAX_CHARS` |
| No code path owns it | the code paths already ran; the eval asks `code_owned` |
| No confirmation waits | a Claude permission prompt, a question with yes/no buttons or a firm Go, an open purchase |
| No question waits | `_pending_answer`, any live question, a chief card waiting for its Go, a question or a budget, the Claude or Codex relay on |
| Nothing runs that the owner may be steering | a texted task, a chief step marked running, the desk holding the Mac, a "claude on" still deciding |
| Eve's last line was not a question asked in the last 30 minutes | the chat log's last eve line and its time (`QUESTION_WINDOW_S`) |

Two more cases skip the lane: words heard on a phone call, whose answer is read aloud as the brain streams it, and
an app turn on the "think" grade, where the owner asked for the thinking model.

When the inlet cannot tell whether a state exists, it treats the chat as waiting. Two examples: a chief ledger that
cannot be read, and a chat log that cannot be read.

The gate does not try to recognise a greeting from a word list. Haiku decides that.

## The Haiku call

`small_talk.ask` makes one call through the OpenRouter client (`openrouter.py`):

- **Model:** `EVE_SMALL_TALK_MODEL`, default `anthropic/claude-haiku-5.5`.
- **Settings:** no tools, no thinking (`reasoning.enabled: false`), temperature 0, at most 120 output tokens, and
  OpenRouter's provider preference `{"data_collection": "deny"}` (only providers that do not keep or train on the
  request; Haiku 5.5 is still served with it, checked 2026-10-12). There is no `store` flag: that is OpenAI's, and
  it does nothing on an `anthropic/*` model through OpenRouter.
- **Timeout:** 4 s in all. When no reply has come after 2.2 s, a second identical call races the first, and the
  slower one is cancelled (`_hedged`).
- **Spend:** recorded as `small_talk` from OpenRouter's reported cost.
- **Output:** a strict JSON schema: `{"handoff": bool, "reply": str, "reply_mentions_an_act": bool}`.

The system prompt opens with the brain's own persona sentence (a cheerful, curious sidekick that runs the owner's
Mac and looks after their home), so the tone does not change with the model that answers. The lane's job (answer
at once or pass it on) is written as an instruction, and the prompt tells Haiku never to mention it in a reply.
"Who are you" and "what can you do" are handed off: the brain has its own rule for them. On the dev set, with the
sample voice, the judge read Haiku's own answers ("keeping the lights, music and TV in order") as claiming an act.

The prompt is written the way Anthropic models read best: plain instructions in the system turn, and the data in
XML tags in the user turn. The tags are `<voice>`, `<time>` (weekday and part of day), `<conversation>` and
`<message>`.

- `<voice>` carries a short default voice. When memory is on, it adds the "How they like to talk" section of the
  owner's profile, clipped to 800 characters. The section is read from the whole profile page
  (`records.talking_section`: closed facts and hidden lines dropped), not from the voice front's profile, which is
  cut at 3000 characters with new stars on top and would lose this last section first.
- `<conversation>` holds the last 4 owner and eve lines of the conversation going on now, 300 characters each.
  eve's lines are shown as plain text. After a quiet spell of 10 minutes (`idle_close_secs`) or a restart, that
  conversation is over, so Haiku is shown no earlier lines and the tag reads "(nothing yet today)". The gate still
  reads eve's last line, so a question she asked within 30 minutes still sends the answer to the brain.

Haiku hands off whenever a good answer needs more than chatting:

- a tool, an action or a task
- the owner's own data (mail, calendar, notes, files, memory, location)
- a fact it would have to look up
- anything about a job eve is doing or did
- who or what eve is, or what it can do
- advice or real thought
- a message it is unsure how to read

Haiku also reads its own reply back (`reply_mentions_an_act`).

### What code checks after the call

Code decides; the model only proposes. A reply becomes a hand-off when any of these is true:

- It is not exactly the schema's object (`parse`).
- Haiku asked to hand off, or flagged its own reply.
- It is empty, or longer than 240 characters.
- Its words claim an act (`claims_action`), such as "I've", "I'll", "done", "sent", "checked", "sorted" or "we'll".
- The call timed out, the provider failed, or there is no key.

A hand-off says nothing. The owner's line is already written, so the brain does not write it again. The brain is
shown the same words and the same history as before the lane existed: its history ends at the message, even when a
code path wrote lines during the Haiku call.

## What leaves the Mac

Every message that passes the gate is sent to OpenRouter, which passes it to Anthropic (Haiku 5.5). The request
holds:

- the message
- the last 4 owner and eve lines of the conversation going on now, up to 300 characters each. eve's lines can
  repeat mail, calendar or contact details from earlier turns.
- the "How they like to talk" section of the owner's profile, when memory is on
- the weekday and the part of the day

This is so on a hand-off too, and twice when the hedge fires at 2.2 s. The brain's own call is separate: with
`EVE_CODEX_SUBSCRIPTION=1` it goes to OpenAI through Codex, otherwise to OpenRouter.

## What a lane reply does

A lane reply is written like any eve reply:

- the owner's line goes into the transcript and the chat log (`_note`) as soon as the gate passes, before the
  Haiku call, as the brain writes it before its own. A message a code path answers during the call (the lights,
  the music) lands after it, in the order the owner sent them. eve's line follows the reply. The history, the
  app's message list and the nightly dream all see them
- the reply is sent with `_say`, so it shows in the app or in Telegram the same way
- the chief hears the turn (`_chief_heard`), so the first message of the day still gets the brief

When the history Haiku read holds outside words (`tool_gate.carried`), its reply is kept as outside words too.

The log line follows the same format as the brain's:

    telegram: turn answered in 1.5 s (small talk, anthropic/claude-haiku-5.5)
    telegram: small talk handed off (handoff) after 1.4 s

As with every log line, these carry reasons and seconds, never the message or the reply.

## The switch

| `EVE_SMALL_TALK` | Behaviour |
|---|---|
| `0` | Off: every turn goes to the brain, as before. |
| `1` | On, before or regardless of the bar. |
| unset | On only when `small_talk.SHIPPED` is True. |

`SHIPPED` is False until the pre-registered bar holds on the blind set. The bar is S3 in
[small-talk-gates.md](small-talk-gates.md), and it is quoted in the module docstring.

## The cost of a hand-off

A short message that needs the brain now waits for Haiku's hand-off first. On the dev set that added a median of
1.46 s (47 hand-offs). Messages over 8 words, and anything a code path or the gate turns away, pay nothing.

## The eval

    cd bridge && env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) \
        .venv/bin/python tools/small_talk_eval.py tests/fixtures/small_talk_dev.json --coverage 80 --p50 1.8 --p95 3.0

The eval scores the set once per owner voice, and the bar must hold on each. Production adds the profile's "How they
like to talk" section to the prompt when memory is on, so the empty voice alone is not the production prompt.
`--voice` takes `none` (the default voice alone), `sample` (`SAMPLE_VOICE`, a fictional section in the profile's
format, longer than the owner's and leaning chatty) or a file holding such a section. It can be repeated; the
default is `none` and `sample`.

For each case the eval runs:

1. the production gate, with the case's state
2. when the gate passes, the production call
3. for every lane reply, one judge call (`openai/gpt-6-luna`, low effort) with a fixed yes/no question: does the
   reply claim an act?

Calls run three at a time, so the latency is each call's own, not time spent in a queue. The eval prints every
miss, the totals, the hand-off latency and the cost. It prints `SMALL_TALK_BAR_OK` only when every bar holds.

**Dev set:** `tests/fixtures/small_talk_dev.json` holds 97 cases (40 small talk, 57 needs the brain), with fictional
names. Three ask who or what eve is and one whether it can read the mail; all four are the brain's. A run on
2026-10-12, with 94 cases and the empty voice only, before the persona change, passed:

| Bar | Result |
|---|---|
| needs-brain cases answered by the lane | 0 |
| small-talk coverage | 95.1% |
| judge yes | 0 |
| p50 / p95 latency | 1.51 s / 1.83 s |
| provider errors | 0 |

**What tuning on the dev set changed:**

- **Thinking off.** Haiku 5.5 thought on some short messages and spent all 120 tokens doing it. The content came
  back empty.
- **Temperature 0 and a shorter voice.** Without these the judge read about 1 reply in 40 as claiming an act.
  Examples: "keeping an eye on things", "slept well", "we'll see what the week looks like".
- **The hedged call.** 2 of 87 calls ran past 4 s.

**Blind set:** `tests/fixtures/small_talk_blind.json` is the holdout. A separate agent wrote it without the code or
the prompt. It is scored once.
