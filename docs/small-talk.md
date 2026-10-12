# Small talk

`bridge/src/eve_bridge/small_talk.py` answers pure small talk ("hi", "thanks", "good night") with one Haiku 5.5 call
instead of the full chat brain. The eval is `bridge/tools/small_talk_eval.py`. The acceptance ledger is
[small-talk-gates.md](small-talk-gates.md).

## Why

On 2026-10-12 the owner asked: "can simple interactions be handled with haiku for really fast speed, like hi
shouldn't take this long to respond to". Chat turns on the brain took 4.2 to 11.8 s that day. Haiku 5.5 on
OpenRouter answers "hi" in about 1.5 s.

The first design let Haiku read every short message and decide whether to answer or hand it off. It failed its
blind eval (S3). The owner then asked for the simplest design: "can we just route to haiku only for small talk",
"occams razor". This is that design.

## How it works

The lane runs inside a chat turn, in `TelegramInlet._small_talk_lane` (telegram.py), for Eve Link app turns and
Telegram turns. Every code path in `_handle` runs first (stop, code words, relays, the TV, lights, music, quick
answers). Then the lane does three things:

1. **Code routes.** `small_talk.matches` normalises the message and matches it exactly against a fixed list,
   `PHRASES`. Normalising means: lowercase, apostrophes dropped, punctuation and emoji removed when there are words,
   a repeated emoji kept once, a leading or trailing "eve" dropped, and repeated letters collapsed ("heyyy",
   "thank youuu", "goood morning"). The list holds:
   - greetings: hi, hey, hello, yo, hiya, heya, howdy, sup, what's up, whatsup, wassup, hi there, hey there,
     hello there, hi hi, hey hey, hello hello, good morning, good afternoon, good evening, good day, morning,
     evening, gm, top of the morning, happy monday to happy sunday, happy weekend
   - thanks: thanks, thank you, thx, ty, thank u, tysm, thank you so much, thanks so much, thanks a lot,
     thanks a ton, many thanks, cheers, appreciate it, much appreciated
   - goodbyes: bye, bye bye, ok bye, okay bye, thanks bye, ok thanks bye, goodnight, good night, night,
     night night, nighty night, gn, sleep well, sweet dreams, see ya, see you, see you later, see ya later, cya,
     later, talk later, catch you later, ttyl, take care, have a good one
   - how are you: how are you, how's it going, how are you doing, you good, how are things, how's your day,
     how was your day, how have you been, how you doing, you doing ok
   - laughter: lol, haha (and hahaha), hehe (and hehehe), lmao, lmfao, rofl, 😂, 🤣, haha nice, lol nice,
     that's funny, you're funny, good one
   - compliments: you're the best, love you, love ya, good job, nice work, you're awesome, you're amazing,
     you rock, you're the goat, best assistant, legend

   Words that can be an answer to eve or a command are never on the list: ok, yes, no, sure, cool, nice, 👍,
   got it, sounds good, perfect, do it, go ahead, nvm, stop, wait. "What's new" is off it too, since it can ask
   for news or updates.

   Everything else goes to the brain, including words that could answer eve or give a command, and any message
   that adds to a phrase ("thanks, now send it", "good morning what's on today").
2. **Code stands aside** (`small_talk.gate`) even on a match when the message has an attachment, is a tapped
   button's words, or eve is waiting on the owner: a confirmation, a question, a running task, a chief card, a
   relay, the desk holding the Mac. `_small_talk_state` reads these. When it cannot read one, it treats the chat
   as waiting. Words heard on a phone call and app turns on the "think" grade also go to the brain.
3. **One Haiku call** (`small_talk.ask`): model `EVE_SMALL_TALK_MODEL` (default `anthropic/claude-haiku-5.5`),
   through the OpenRouter client, at most 60 output tokens, no thinking, a 4 s timeout, no retries. The prompt is
   eve's persona, the owner's "How they like to talk" profile section when memory is on, and the one message.
   There is no chat history and no clock. Haiku is told to answer only as a friendly reply in one short sentence,
   and never to mention doing, checking, planning or remembering anything.

When the call fails (timeout, provider error, no key, an empty reply, or a reply cut at the token limit), nothing
was written or said. The brain answers the message as it always did.

## What a lane reply does

It is written like any eve reply: the owner's line and eve's line go into the transcript, the chat log and the
history (`_note`). The reply is sent with `_say`. The chief hears the turn. The log line is:

    telegram: turn answered in 1.5 s (small talk, anthropic/claude-haiku-5.5)

A fall-through logs `small talk fell through (<reason>) after <s> s`. Log lines never carry the message or the
reply.

## What leaves the Mac

On a match, the message and the owner's "How they like to talk" section (up to 800 characters) go to Anthropic
through OpenRouter. The request asks for providers that do not keep or train on it (`data_collection: deny`).
Spend is recorded as `small_talk`.

## The switch

| `EVE_SMALL_TALK` | Behaviour |
|---|---|
| `0` | Off: every turn goes to the brain, as before. |
| `1` | On, before or regardless of the bar. |
| unset | On only when `small_talk.SHIPPED` is True. |

`SHIPPED` is False until the bar in S3b of [small-talk-gates.md](small-talk-gates.md) holds on a fresh blind set.

## The eval

    cd bridge && env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) \
        .venv/bin/python tools/small_talk_eval.py tests/fixtures/small_talk_dev.json --coverage 70 --p50 1.8 --p95 3.0

For each case, the eval runs the production gate with the case's state. When the gate passes, it runs the
production call. Then one judge call (`openai/gpt-6-luna`, low effort) asks of each lane reply: does it claim an act?
The set is scored once per `--voice` (default: `none` and `sample`, a fictional "How they like to talk" section).

The bar: needs-brain cases answered by the lane = 0; small-talk coverage >= `--coverage` %; judge yes = 0; p50 and
p95 of the Haiku calls within `--p50` and `--p95`; provider errors = 0. The eval prints `SMALL_TALK_BAR_OK` only
when all hold on every voice.

**Dev set:** `tests/fixtures/small_talk_dev.json` was written for the first design. Most of its small-talk cases
are not on the phrase list, so coverage on it is low by design.

**Blind sets:** `tests/fixtures/small_talk_blind.json` was spent on the first design. The simple design is scored
once on `tests/fixtures/small_talk_blind_v2.json`.

## Verdict

Shipped 2026-10-12 on the blind v2 set (docs/small-talk-gates.md S3b): 0 of 30 brain messages answered by the lane, 80% of small talk caught, no reply claiming an act, p50 1.17 s. The v1 design (Haiku deciding, with history) failed its blind bar and was replaced.
