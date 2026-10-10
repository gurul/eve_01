# "hey eve" — voice and computer control

**Current computer-task backend (2026-09-22):** `start_task` in voice and text now
uses `codex_computer.py`, the public Codex app-server protocol, and the installed
`cua_repl.js` plugin. Codex progress shows as captions only and is never spoken (the voice says "on it" once, then the result); permission
questions wait for an explicit answer; steering and cancellation use Codex turn
methods. Model settings come from Codex. The Python desktop worker, fast lanes,
and `EVE_AGENT_*` tuning described below are retained legacy internals and
are no longer selected by the daemon task factory. They are not a fallback.
See [integration evidence and limitations](../codex-computer-use/README.md).


Say **"hey eve"** and the robot listens. Ask it to do something on your Mac and
it does it — opens the mail, finds a file, fills a form — talking back while it
works, taking corrections mid-task, stopping on "stop". The robot is not the one
pressing the keys; the bridge daemon on the Mac is. The robot acts the part.

```
Mac microphone ─24 kHz─▶ ears.py ─── sherpa-onnx keyword spotter ("hey eve")
                            │                       │ wake
                            │ subscribe()           ▼
                            └──────▶ voice_agent.py ── gpt-live-1 (full duplex, Live API WebSocket)
                                          │ delegates every decision needing a tool to
                                          ▼
                                    Responses backend (gpt-6-astra) ── tools: start_task / steer_task /
                                          │   stop_task / task_status / answer_question / go_explore /
                                          │   end_conversation
                                          ▼
                                    codex_computer.py ── Codex app-server, installed cua_repl.js
                                          │ code
                                          ▼
                                    desktop_worker.py ── PyAutoGUI on the real desktop (child process)
                                          + desktop_helpers.py (Vision OCR, open -a, clipboard)

daemon ─{"cmd":"agent","state":…}─▶ robot: wake · listening · thinking · speaking · working · asking · done · error
```

## What happens, step by step

1. **Wake.** While the robot is connected the daemon keeps one microphone stream open
   and runs every 100 ms block through a streaming keyword spotter. The mic closes
   when the robot drops off the cable and opens again when it is back
   (`EVE_MIC_ALWAYS=1` keeps it open from boot). `eve-bridge mic off`, or
   the *Microphone* switch in the menu-bar app, closes it for good until you turn it
   back on; `eve-bridge mic` says what it is doing right now. On "hey eve" the robot's head comes up
   with a chirp (`wake`) and a conversation opens. A wake is ignored while a
   conversation is already open.
2. **Talk.** A Live session (`gpt-live-1`) hears the same microphone. The model is
   full duplex — it listens while it speaks and does its own turn-taking, so there is
   no VAD to configure. It answers chit-chat itself and delegates anything needing a
   tool to a Responses backend (`gpt-6-astra`) that owns the tools: the seven for tasks,
   exploring and ending, plus `look`, `move_head`, `look_around`, `find`, `take_photo` and
   `set_sound` ([vision.md](vision.md)), `web_search` (Perplexity through OpenRouter, or OpenAI's built-in search without an OpenRouter key), and `think_hard`. The voice is
   the receptionist: a question that needs today's facts (weather, a score, a price) is
   delegated and the backend searches the web itself, server-side; a genuinely hard
   question (maths, code, logic, a plan) goes through `think_hard` to `think.py` — one
   Responses call at `high` effort with web search, up to 90 s — while the voice says
   "let me check" and keeps listening. The backend picks the cheapest thing that answers,
   in order: itself, web search, `think_hard`, and only then `start_task` — computer use
   is the last resort, for a request the Mac itself must carry out or show, never for a
   question (owner request 2026-09-10; a six-phrase routing probe against the real backend
   put every question on the cheap path and every Mac request on `start_task`).
   gpt-live-1 takes no images, so a small image model
   (`gpt-6-luna`) describes the camera and keeps the newest view; nothing is pushed to
   the voice, which reads a view only through `look` when you ask what it sees. Every
   finished turn of yours is also checked for
   "go away" and "mute" in any words (`intent.py`): a goodbye closes the conversation once
   eve's own goodbye has been said, and mute silences every sound while the head and
   lights keep moving.
   **eve does not speak through the Mac**: gpt-live-1 has no text-only mode, so the
   daemon reads `session.output_transcript.delta` and never plays the audio it is
   sent. Each reply is shown as pages of 4 lines x 17 characters in
   large type under its eyes (the eyes move up to make room); page 0 fills word by
   word as the model writes, every page stays up for its character count at
   12 characters/second (2 s minimum), the last page 6-10 s, and the robot
   beep-boops once per page (`CHIRP_TALK`). Pages are timed by the bridge
   (`caption_pager.py`), not by the model's writing speed; a follow-up reply queues
   behind the page on screen; talking over it clears it. The robot faces you while
   `listening`, glances aside for `thinking`, bobs while `speaking` (until the last
   page has been held). `EVE_VOICE_OUTPUT=audio` brings back a
   spoken voice through the Mac speaker; that path is **half-duplex** (the mic is
   not forwarded while eve talks plus 0.4 s, because the Mac mic hears the Mac
   speaker and eve answered its own greeting in a loop on the bench).
3. **Work.** When you ask for something on the computer, the voice model calls
   `start_task(goal)`. A `gpt-6-astra` loop drives a persistent worker process on
   the desktop. Its first turn already carries the screen size, the frontmost app
   and window title, the local time and a screenshot, so the model acts at once
   instead of starting with a look. Each step is a few lines of Python using the
   helpers — `open_app`, `open_url`, `frontmost`, `screen_text` (Vision OCR in
   click coordinates), `find_text`, `click_text`, `click_element` (a coordinate click that snaps to the control under the point and refuses if it is not what the model named — handyman's grounding trick, on the Accessibility tree), `wait_for`, `wait_settled`,
   `type_text` (clipboard paste, so accents and emoji survive), `zoom`, `observe` —
   plus raw PyAutoGUI. Every step that clicks, types or presses keys ends with a
   screenshot taken after the screen settles and an `[after your input]` line: the
   frontmost app, where the screen changed (`changed around (x,y,w,h)` in click
   coordinates) or `unchanged`, and — after a raw click — what it landed on and in
   which app (`clicked on AXScrollArea in Warp`). A step that only looked ends with
   `[after]`. The change detector compares 1/4-scale grey thumbnails with a
   24-pixel noise floor and ignores the menu-bar strip, so a 14x14-point checkbox
   toggle registers and a text caret does not. A typical task is 2–4 model turns. Each helper logs
   one short sentence ("opened Spotify", "typed 12 characters"); while the robot
   is `working` each of those lines is shown on its screen as a caption page (four
   lines of 17 characters, held at reading pace) when no reply is up, at most one
   every 1.5 s, after the initial "on it…". Turn 1 and recovery turns (an
   error, a repeated step, a "no", a steer, a failed final-answer check) run at
   high reasoning effort, the rest at medium. **Before a final answer is spoken**
   on a task that sent any input, a separate call checks it against a fresh
   screenshot, with none of the task's history, and returns `{valid, guidance}`.
   An unconfirmed answer goes back to the agent once, with what the screen
   actually shows; a second miss is spoken as "I couldn't confirm that on
   screen." A check that errors lets the answer through. This exists because
   four logged runs ended "Spotify is playing" right after an input that changed
   nothing on screen. Meanwhile you can keep talking: **"use Safari instead"** →
   `steer_task` (delivered with the very next step), **"how's it going?"** →
   `task_status`, **"stop"** → `stop_task`: the in-flight model request or step is
   interrupted within ~100 ms, the worker is killed, and every key and mouse
   button is released.
4. **Ask.** Before anything consequential — sending, paying, deleting, posting —
   the task calls `ask_user`. The robot looks up (`asking`, "yes / no?"), the
   question is spoken, and your spoken answer goes back through `answer_question`.
   With nobody listening the answer is always no. The question is released on every
   exit, including a failed send, so the idle close and the goodbye are never
   blocked by a question that was never asked. A backend stream `error` ends the
   backend's response like `response.failed` does: the face settles and a held-back
   reply is sent (`verification/Eve/Voice.lean`).
5. **Done.** The task's final sentence is handed back to the voice model to show
   as a caption; the robot nods (`done`) or winces (`error`), and the conversation
   closes right after.
6. **Stopping it listening.** Any of: say *bye*, *thanks*, *stop listening*, *go to
   sleep* or *be quiet*; **touch the robot** (a tap or a hold on the pet ends the
   conversation and any task it is running, at once); 20 quiet seconds with no task
   running; the 10-minute cap. `eve-bridge mic off` closes the microphone until
   `mic on` (persisted, like mute); `EVE_VOICE=0` never opens it at all.
7. **Sending it exploring.** *"hey eve, go explore"* (or *look around*, *go play*):
   eve answers with a two-word send-off and calls `go_explore`, which closes the
   conversation like `end_conversation`; once the robot has dropped the conversation
   pose the daemon starts a manual explore — the same pan-and-diary loop it runs
   when Claude has been idle, but right now and for as long as you leave it (a
   touch, the listen key, the next wake word or `eve-bridge explore
   stop` end it). Refused while a task is running: stop the task first. The same
   thing from the shell is `eve-bridge explore`.

## Setup

Everything is installed with the bridge (`pip install -e bridge`). Three things
are per machine:

| Need | How |
|---|---|
| Keyword model (19 MB, once) | `mkdir -p ~/.config/eve-bridge/models && curl -L https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models/sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01.tar.bz2 \| tar xj -C ~/.config/eve-bridge/models` |
| **Microphone** for the daemon's python | macOS attributes the grant to the interpreter binary: System Settings → Privacy & Security → Microphone → add the resolved venv python (`eve-bridge ears-check` prints it, and the daemon warns `ears: the microphone has been silent for 10 s` when the grant is missing) |
| **Accessibility** + **Screen Recording** for the same python | needed by the desktop worker to click and to screenshot. Until both are granted, a task says out loud that it cannot see the screen yet |
| `OPENAI_API_KEY` | in `~/.config/eve-bridge/env` (mode 600) — the wake word works without it, the conversation does not. The desk voice is OpenAI's realtime Live API, the one feature still on this key until it moves to OpenRouter; everything else uses `OPENROUTER_API_KEY` ([one key: OpenRouter](../openrouter.md)) |

Check the ears end to end:

```
$ eve-bridge ears-check
wake word:      'hey eve'  (EVE_WAKE_WORD)
model dir:      ~/.config/eve-bridge/models/sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01  present=True
input device:   MacBook Pro Microphone
keywords file:  …/keywords-hey_eve.txt -> ▁HE Y ▁BU D D Y :2 #0.25 @hey_eve

Say 'hey eve' within 8 s ...
HEARD IT at 1.4 s — ears are working.
```

### When eve hears you and then sits still

The log says `ears: heard 'hey_eve'` and nothing moves for many seconds, then
everything lands at once: that is the daemon's event loop held by one synchronous
call. The loop writes the log, so it cannot say which call. A thread watches it from
outside (`loop_watchdog.py`): when the loop stops ticking for more than 2 s the log
gets `event loop stalled for N s — the loop thread is at:` followed by that thread's
Python stack, again every 10 s while it lasts, then `event loop resumed after a N s
stall`. Read the innermost frames; they name the call.

```bash
grep -A40 "event loop stalled" ~/Library/Logs/eve-bridge.log | tail -60
kill -USR1 $(pgrep -f "eve_bridge.cli daemon")   # every thread's stack, into the same log
```

`kill -USR1` is for a stall the watchdog cannot see into, a C call that holds the GIL.

The two it caught on its first day (2026-09-21), both now off the loop:

- **The first "hey eve" of a fresh daemon waited 19–22 s** (0–2 s before 2026-09-19).
  `AsyncOpenAI().live` imports the SDK's whole `resources` package the first time it is
  touched, and inside the daemon, sharing the GIL with vision and the keyword spotter,
  that import took 12–20 s on the loop thread. The daemon now imports it in a thread at
  start (`voice_agent.warm_live_import`) and `open_session` awaits the same call in a
  thread, so a wake that beats the warm-up waits without freezing anything else.
- **The daemon froze for 41 s right after the serial port opened.** The transcript
  tailer's initial sweep read every `*.jsonl` under `~/.claude/projects` (869 files,
  791 MB here) on the loop; the board sat unsynced and the microphone stayed closed
  until it finished. The sweep and the history seed now run in a thread.

## The voice gate: only the person who woke eve

eve's live voice is OpenAI's **Live API** (`gpt-live-1`), not the Realtime API: it has no turn detection, no
noise reduction and no speaker setting a client can touch — only `input_audio.append`, `mute` and `unmute`
([live conversations](https://developers.openai.com/api/docs/guides/live-conversations), fetched 2026-09-21).
The model hears whatever is appended. So a television, a call or a second person were answered; and because
any transcribed speech resets the idle timer and counts as a barge-in, a TV kept a conversation open to its
ten-minute cap and a stranger's sentence wiped eve's caption mid-reply. Isolation has to happen on the Mac,
before the append. `voice_gate.py` does it, behind `EVE_VOICE_GATE`:

**Would the Realtime API do it instead?** No. It has what Live lacks for *noise and turns* — input noise
reduction (`near_field` for a headset, `far_field` for a laptop or room microphone), `server_vad` with a
threshold, `semantic_vad` with an eagerness, and turn detection that can be switched off so the client drives
turns — and its input transcription can use `gpt-4o-transcribe-diarize` for speaker *labels*. None of that
restricts the model to one person: a label on a transcript does not stop the model hearing and answering whoever
spoke, and a second person or a television at conversational level passes any noise gate. Checked 2026-09-21
against the installed SDK's types (`openai` 3.13.0: `types/live/` has no turn, noise or speaker field;
`types/realtime/` has all of the above and no speaker verification) and the
[Realtime VAD guide](https://developers.openai.com/api/docs/guides/realtime-vad). So the gate is client-side on
either API, and moving to Realtime would mean rewriting the voice session to gain noise handling only.

```
wake         ears.py keeps the last 3 s of microphone audio in RAM; on "hey eve" that snapshot is the seed
provisional  until 3 s of voice are enrolled NOTHING is rejected. The speech that starts within 10 s of the
             wake — the query itself — is forwarded and enrolled, whatever it scores
locked       speech is cut into segments by energy. The first 0.6 s of a segment is held back and embedded
             (sherpa-onnx TitaNet-small, already a dependency, ~10 ms). A match is forwarded — the held audio
             in one burst, then live. No match yet: hold on to 1.5 s and judge again; nothing is rejected on
             0.6 s. Still no match: SILENCE of the same length goes to the model, which wants a continuous
             stream and needs quiet to close a turn. A going-on segment is re-scored every 0.5 s: two low
             scores cut it (someone took over), one good score opens it (the owner spoke up)
adapt        only speech close to the enrolled voice AND to the seed moves it: a TV cannot walk it
reset        at the end of the conversation the voice is dropped. Nothing about a voice touches the disk
```

**It is for queries, not for notes.** In a think-aloud lesson eve is a listener taking notes on whoever is
speaking, so nothing is judged there; a conversation opened without a wake word (the lesson toggle) has no
seed and is ungated too. While a yes/no is awaited or a task is running, a short answer is never held back:
"stop" must not depend on sounding like yourself.

**It fails open, except where it is sure.** No model file, an embedder that raised, a wake snapshot with no
voice in it, a segment too short to judge, or nothing accepted at all since the wake (the enrolment is what is
wrong): all of those forward the audio as before. It closes only on a confident mismatch — the long, plainly
different segments, which are the TV and the other person. Once it has heard its owner, a TV that talks for a
minute is rejected for a minute; the conversation then idles out as it should, and the next "hey eve"
enrols afresh.

**What was measured, and what was not.** `tools/voice_gate_eval.py` replays recordings through the real gate
and the real model. On sherpa-onnx's three sample speakers (clean, read, Mandarin), each in turn the owner with
a 0.8 s wake:

| owner | owner speech silenced (bar ≤ 5 %) | other speech forwarded (bar ≤ 20 %; gate off = 100 %) |
|---|---|---|
| speaker 1 | 0.0 % | 9.8 % |
| speaker 2 (halting; his own speech scored 0.26–0.39) | 4.7 % | 0.0 % |
| speaker 3 | 0.0 % | 13.6 % |

The replay is what shaped the design — the first version judged on the 0.7 s wake seed and silenced 54 % of
speaker 2, which is why the seed is never evidence and nothing is rejected on 0.6 s — and **the cut-offs were
set on this same replay, so these are tuning-set numbers**, from three voices that are not yours, in no room
at all. That is why it ships `off`. Run `shadow` for a few days: the daemon log carries every decision's score
(`voice gate[shadow]: locked 1.4 s silence score=0.12 …`, never words, never audio), and the line at the end of
each conversation says how many seconds `on` would have silenced. Then score your own recordings — you at 0.5,
1.5 and 3 m, one-word answers, another person, the TV, eve's own voice from the Mac speaker — with
`voice_gate_eval.py --check` before switching it `on`.

**Limits.** Overlapping speech is not separated: you talking over the TV is forwarded whole or silenced whole.
The first sentence after the wake is always heard, whoever says it. One-word turns pass on context, not on
identity. And the gate changes what reaches the model, not what the model does with it, so the prompt also
tells eve that other voices are background.

## Knobs

Voice (including its backend), Telegram and deep reasoning receive a system
context with the configured location, timezone and local clock snapshot. Set
`EVE_LOCATION` and `EVE_TIMEZONE` in `~/.config/eve-bridge/env`
and restart the daemon. The location is a user-supplied default, not GPS.
Timezone conversion accounts for daylight saving. Context refreshes for each
new voice session and each text/reasoning request; during a voice session,
Eve uses a fresh lookup for current time instead of reusing its start time.

| Variable | Default | Purpose |
|---|---|---|
| `EVE_LOCATION` | unset | Owner's default location for local questions; kept in the local env file |
| `EVE_TIMEZONE` | Mac's timezone | IANA timezone, such as `America/Los_Angeles`; invalid values fall back to the Mac's timezone |
| `EVE_VOICE` | on | `0` disables the microphone and the wake word entirely |
| `EVE_WAKE_WORD` | `hey eve` | any short English phrase; it is tokenised into the spotter's keywords file at startup (no training) |
| `EVE_WAKE_THRESHOLD` | `0.25` | spotter threshold; lower = more sensitive (bench: 0.25 fired on a synthesized clip, stayed quiet on a control sentence) |
| `EVE_MIC` | default input | substring of the input device name to use |
| `EVE_MIC_ALWAYS` | off | `1`: open the microphone at boot and keep it open; default: only while the robot is connected |
| `EVE_MIC_FILE` | `~/.config/eve-bridge/mic.json` | where the owner's `mic on/off` choice is kept |
| `EVE_KWS_MODEL_DIR` | see above | where the sherpa-onnx model lives |
| `EVE_LIVE_MODEL` | `gpt-live-1` | the voice model (Live API) |
| `EVE_MEMORY` | `0` | `1`: keep every word in the transcripts, give the voice the profile, today's lines and the memory tools, and dream nightly ([memory.md](memory.md)); `EVE_RECORDS=1` is a legacy alias |
| `EVE_LIVE_BACKEND_MODEL` | `gpt-6-astra` | the Responses backend that owns the tools (`gpt-5-mini` answers faster, calls tools less reliably) |
| `EVE_LIVE_BACKEND_EFFORT` | `low` | reasoning effort for that backend |
| `EVE_LIVE_WEB_SEARCH` | on | `0`: the backend gets no `web_search` tool, so live facts are answered from training only |
| `EVE_THINK` | on | `0`: no `think_hard`; hard questions get the low-effort backend only |
| `EVE_THINK_MODEL` | `gpt-6-astra` | the slow brain behind `think_hard` (voice and Telegram) |
| `EVE_THINK_EFFORT` | `high` | its reasoning effort (`none` … `xhigh`) |
| `EVE_THINK_TIMEOUT_SECS` | `90` | how long one `think_hard` may take (10-300); past it the backend answers as best it can |
| `EVE_VOICE_OUTPUT` | `captions` | `captions`: text to the robot's screen + beeps, silent Mac; `audio`: spoken through the Mac speaker |
| `EVE_VOICE_NAME` | `marin` | the Live voice (heard only in audio mode) |
| `EVE_CAPTION_CPS` | `12` | reading rate the caption page hold times are derived from (5-30); lower = pages stay longer |
| `EVE_VOICE_IDLE_SECS` | `20` | close the conversation after this much silence with no task running (floor 5) |
| `EVE_SCENE` | on | `0`: eve gets no camera descriptions during a conversation ([vision.md](vision.md)) |
| `EVE_SCENE_MODEL` | `gpt-6-luna` | the image model that describes and locates for the voice |
| `EVE_SCENE_INTERVAL_SECS` | `3` | minimum gap between two camera descriptions (1-60) |
| `EVE_SCENE_STALE_SECS` | `15` | a view older than this is reported as out of date (3-300) |
| `EVE_INTENT` | on | `0`: goodbye / mute by the phrase table only, no classifier call |
| `EVE_INTENT_MODEL` | `gpt-5.4-nano` | the goodbye / mute classifier |
| `EVE_SOUND_FILE` | `~/.config/eve-bridge/sound.json` | where the owner's mute choice is kept |
| `EVE_COMPUTER_CONTROL` | on | `0` keeps the conversation but refuses `start_task` |
| `EVE_AGENT_MODEL` | `gpt-6-astra` | the computer-use model |
| `EVE_AGENT_MAX_TURNS` | `25` | step cap per task |
| `EVE_AGENT_MAX_SECS` | `180` | wall-clock cap per task (floor 30) |
| `EVE_AGENT_EXEC_TIMEOUT` | `60` | seconds one `exec_py` step may run before the worker session is restarted (floor 10) |
| `EVE_AGENT_REASONING` | `medium` | reasoning effort for ordinary steps (`none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`); `medium` costs ~0.5 s a step over `low` (logged medians 3.87 s vs 3.37 s) |
| `EVE_AGENT_PLAN_REASONING` | `high` | reasoning effort for turn 1 and recovery turns (after an error, a repeated step, a "no", a steer, or a failed final-answer check) |
| `EVE_AGENT_VERIFY` | on | check a final answer against a fresh screenshot before it is spoken, on any task that clicked, typed or pressed keys; `0` turns it off |
| `EVE_AGENT_VERIFY_REASONING` | `low` | reasoning effort for that check |
| `EVE_AGENT_RUNS_DIR` | `~/.config/eve-bridge/agent-runs` | one JSONL action log per task: goal, every code block, text results, questions, answers, final line — never the screenshots |
| `EVE_FAST_LANE` | `0` | the fast lane: `delegate` in the planner's helpers; the default is the holdout eval's decision (see [The fast lane](#the-fast-lane-clicks-under-the-planner)) |
| `EVE_REFLEXES` | `1` | before the planner: launch an installed app or open a web search in code, about 2 s instead of 9–32 s; the default is `tools/route_eval.py`'s decision on an unseen holdout ([routing.md](routing.md)) |
| `EVE_ROUTER_MODEL` | `off` | `jev`: ask Jev, in its own idiom, about a request the rules did not recognise; it may only add a bare launch. The request's words leave the Mac ([routing.md](routing.md#ask-each-one-in-its-own-idiom)) |
| `EVE_LANE_FIRST` | `1` | the router: before the planner's first turn, the lane tries to finish a request whose every word one labelled control accounts for; the default is the router eval's decision (see [Lane first](#lane-first-the-router-before-the-planner)) |
| `EVE_FAST_LANE_DECIDE` | `keyword` | who picks a lane step: `keyword` (the code gate alone, no model loaded or asked), `model` (the gate first, the decider on the rest) or `jev` (the gate proposes, hosted Jev can refuse — [routing.md](routing.md#plan-once-execute-with-jev)) |
| `EVE_VOICE_GATE` | `off` | `shadow` or `on`: only the person who said the wake word reaches the model ([below](#the-voice-gate-only-the-person-who-woke-eve)). `shadow` judges and logs every decision and changes no audio. Needs `~/.config/eve-bridge/models/nemo_en_titanet_small.onnx` (40 MB); without it eve hears as it always has |
| `EVE_VOICE_GATE_MODEL` | that path | another sherpa-onnx speaker-embedding model |
| `EVE_PLAN_EXEC` | `0` | `1`: plan once, execute with no planner turn between steps ([routing.md](routing.md#plan-once-execute-with-jev)) |
| `EVE_DECIDER` | `unset` | the decider behind `model`: `jev` is the only value (TypeSafe's hosted model, `jev.py`; it is sent the window title and the menu's labels, and is never loaded in `keyword` mode). Unset, `model` mode stays off. The local Laya decider was removed from the lane on 2026-09-24 |
| `EVE_FAST_LANE_STYLE` | `jev` | how `model` mode words its question to the decider: `jev` (the style Jev scored best with), `compact` or `hinted` |

## What eve remembers of talking with you

Every conversation used to start from nothing. With `EVE_MEMORY=1` it does
not. The full design — the three stores, the nightly dream, forget, the one-time
move from the old store, and what leaves the Mac — is in [memory.md](memory.md).
For the voice:

```
a conversation opens
  └─ recall.py: one clause — the gap since the last word on either channel, and
     the title of that day's dream journal when it was before today
  └─ the Live front gets that clause, the short voice profile (≤ 3,000 chars) and
     today's lines on both channels, worded for voice (≤ 4,000)
  └─ the backend gets the full profile and today's lines (≤ 16,000), and the tools
     memory_search · memory_read · recent_conversation · forget_preview · forget_apply
every turn closes
  └─ both sides, and the results eve answered from, go into
     ~/.config/eve-bridge/memory/transcripts/<day>.jsonl at once
the conversation ends (goodbye, silence, a restart)
  └─ a close marker; the nightly dream turns the day into the records and the index
```

- **Every word is kept, on this Mac.** The owner reversed the old distilled-only
  rule on 2026-09-23: a failed model call or a restart used to lose a conversation
  for good. The words are written the moment a turn closes, in a folder that refuses
  to sit in a git tree or a synced folder.
- **The voice can look things up.** A question about the owner or about what was
  said before goes to the memory tools first, before the web or `think_hard`.
  `memory_search` returns dated record lines, memories found by meaning, and the
  words themselves; `recent_conversation` has what the owner texted since this
  conversation began. A lookup gets 2.5 s; past that the backend is told it took too
  long.
- **Starring is by voice.** Say *"remember that"*, *"don't forget that"*, *"keep that
  in mind"* and the claim from the previous turn goes into `records/starred.md`,
  dated, and is on the profile from the next conversation (or the next Telegram
  message). It is anaphoric on purpose: the claim is in the turn before, not in the
  words "remember that". Two independent paths catch it — a phrase table and the
  classifier — so it does not depend on the model choosing a tool. "I remember that"
  and "remember when you said that?" promote nothing.
- **Think out loud keeps nothing.** In a lesson the learner's words are never
  written, starred or given the memory tools.
- **A conversation less than fifteen minutes after the last one gets no time
  clause**, because to a person that is one conversation.
- **Silence is the right answer when there is nothing.** With memory off, or before
  anything was said, the session prompt is byte-identical to before, so the first
  ever conversation sounds exactly as it always did. eve never announces that it
  has no memories.
- **The stars and the dream journal are visible in the widget**, under Talking —
  [widget.md](widget.md#the-two-provenances).

## Taking notes on the room

Say **"start taking notes"** and eve stops being a conversationalist and becomes a
recorder. Say **"stop taking notes"**, tap it, or run `eve-bridge take-notes stop`,
and it writes the meeting up.

```
"start taking notes"
  └─ the conversation closes (the recorder needs the microphone)
       └─ notes.py subscribes to the same 24 kHz stream ears.py already runs
            12 s segments ─▶ gpt-4o-mini-transcribe ─▶ a running transcript
            each segment's tail primes the next, so names stay spelled the same
"stop taking notes"  (or a tap, or the command)
  └─ the transcript is written at once ─▶ ~/.config/eve-bridge/memory/transcripts/meetings/<day>/<HHMM>-notes.md
       └─ the speaker pass: the whole recording ─▶ gpt-4o-transcribe-diarize (AAC pieces of ~20 min)
            the file is rewritten with "[MM:SS] Speaker N: text"
            └─ the write-up (one call, or 10-minute windows past 40 min)
                 the file is rewritten with the notes on top and renamed <HHMM>-<title>.md
```

**Why 12-second segments.** While anything is subscribed to the microphone the wake
word is bypassed (`ears._on_block`: *"muted: never wake on our own voice"*), so eve
cannot hear its own name while recording and learns to stop only by reading its own
transcript. The segment length is therefore the stop latency. Two stops have none: the
command, and a tap on the robot.

**What is kept.** Detailed notes **and the full transcript**, because the point of notes
on a meeting is being able to go back to the words (owner, 2026-10-04: notes that are
"detailed", that "preserve full transcript", and that tell "how many different people are
talking"). The file sits beside the conversation transcripts, so `memory_search` finds it
and forget reaches it ([memory.md](memory.md)).

The transcript reaches disk **before** any paid call (`status: transcript` in the front
matter), and the file is rewritten twice: with the speaker labels, then with the notes on
top (`status: notes`). Each write is atomic (a temporary file, then a rename) and 0600, so
a write-up that fails or hangs leaves the transcript, never nothing.
`EVE_NOTES_KEEP_TRANSCRIPT=0` drops the transcript from the final file, and only once
the notes exist.

**The notes.** The shape follows three open-source note takers: Hyprnote's detailed mode,
the OpenAI Cookbook's speaker-aware meeting notes, and Meetily. The model first decides
whether the recording is a **conversation** (two or more people) or a **memo** (one person
thinking out loud), and the sections follow:

| Section | Conversation | Memo |
|---|---|---|
| Title and summary | yes | yes |
| Speakers (each voice, with what it did) | yes | only with two or more voices |
| Topics, each with its points | yes | yes |
| Decisions, actions (who, what, by when, at what minute) | first | after ideas |
| Ideas, to-dos | when there are any | first |
| Open questions, quotes, follow-ups, not made out | yes | yes |

Numbers, names and dates are kept as said, nothing is invented, and the transcript is
treated as data, never as instructions. Every quote is checked in code against the
transcript line it names; a quote whose words are not in that line is dropped. Detail
scales with length. Past 40 minutes the transcript is written up in 10-minute windows
that overlap by 30 s, and the windows' notes are merged by one more call (or in code, if
that call fails).

**How many people are talking (off since 2026-10-08).** Since every model call moved to
`OPENROUTER_API_KEY` ([one key: OpenRouter](../openrouter.md)), notes have no speaker
labels: OpenRouter has no diarizing transcriber, and the owner accepted plain
transcripts. The code for the pass stays (a transcription client with a `diarize` method
still gets it); the daemon's client has none. What the pass did, for when it returns: the
live 12 s loop has no speaker labels, and it stays as it is, since the spoken stop depends
on it. The session also kept its audio (0600, in a 0700 temporary folder, deleted once the
notes are written) and at stop sent the whole recording once to OpenAI's
`gpt-4o-transcribe-diarize`:

- The audio goes as AAC (macOS `afconvert`) in pieces of about 20 minutes, each under the
  API's 25 MB. Without `afconvert`, or if it fails, it goes as WAV pieces of about 8
  minutes.
- Labels reset with each request. So after the first piece, eve cuts a 2-10 s clip of
  each of its first speakers (up to 4) and passes them to every later piece as known
  speakers named "Speaker 1" to "Speaker 4". The same voice keeps its label for the whole
  recording. Labels are numbered in order of first appearance.
- With `EVE_NOTES_OWNER_VOICE` (a 2-10 s clip of you) the clip is reference 1 on
  every piece, and your lines carry `EVE_NOTES_OWNER_NAME`.
- The transcript of record is the diarized one, one line per turn:
  `[MM:SS] Speaker N: text`. The front matter gets `speakers: N` and `diarized: yes`.
  The live transcript (`[MM:SS] text`) is kept under **Live transcript** when the pass
  missed a piece (`diarized: partial`; the gap is filled from the live lines) or its word
  count differs from the live one by more than a fifth. When the pass fails, the live
  transcript is the transcript (`diarized: no`).
- A daemon stopping mid-notes skips the pass and the write-up and writes the transcript
  at once.
- Nothing heard is logged, only counts.

NVIDIA's Nemotron diarization was considered first. It has no hosted API and is not
verified on Apple Silicon, and eve runs no audio model locally, so it does not fit.

**Cost.** The live loop is $0.003 a minute, the speaker pass $0.006 a minute of audio, and
the write-up is the summary model's tokens (up to 8,000 out per call). An hour of notes is
about $0.55 plus the write-up.

**Limits.** 25 MB a request. At most 4 known voices carry their label across pieces; a
fifth voice, or a voice that never speaks for 2 s at a stretch in the first piece, can get
a new label in a later piece. A word that straddles two 20-minute pieces can be cut. The
pass is one request per piece, so a 3-hour recording takes about 9 requests and a few
minutes after the stop.

**Ordering.** Segments are transcribed one at a time by a single worker. Transcribing
them concurrently scrambles the transcript, because a slow segment lands after the one
that followed it, and it leaves every context prompt empty.

**Reading them back.** They are in the widget under **Notes**, each with *Open*, *Save a
copy…* and *Show in Finder* — the save is a copy, so nothing moves out from under
anything that links to it. From the shell, `eve-bridge take-notes list`.

| Variable | Default | Purpose |
|---|---|---|
| `EVE_NOTES_SEGMENT_SECS` | `12` | segment length, which is also the spoken-stop latency (4-60) |
| `EVE_NOTES_MAX_MINUTES` | `180` | a recording stops itself here even if nobody does |
| `EVE_NOTES_KEEP_TRANSCRIPT` | on | `0` drops the transcript from the final file once the notes exist |
| `EVE_NOTES_MODEL_STT` | `gpt-4o-mini-transcribe` | the live transcription model, through OpenRouter (`openai/gpt-4o-mini-transcribe`) |
| `EVE_NOTES_DIARIZE` | on | `0` turns the speaker pass off (it is off anyway since 2026-10-08: no diarizing model on OpenRouter) |
| `EVE_NOTES_OWNER_VOICE` | unset | path to a 2-10 s clip of the owner's voice: the owner's lines are named |
| `EVE_NOTES_OWNER_NAME` | `Owner` | the name those lines carry |

## The fast lane (clicks under the planner)

`gpt-6-astra` plans; it does not need to spend a 3.5 s turn (logged median, p90 6 s) on
"click Week, then click Today". The fast lane is a helper the planner can call from
`exec_py` — `delegate(objective, …)` — that runs a narrow series of clicks on labelled
controls inside the frontmost app, in well under a second a step. The division of labour
is fixed: **code owns the menu** (the Accessibility tree of the focused window, filtered
and ranked in `ax_candidates.py`), **the keyword gate picks** (code; in `model` mode a
typed-decision model picks where the gate cannot — `decider.py` over hosted Jev, one
`choice` question per step; in `jev` mode Jev can refuse the gate's pick), **the planner
plans** and speaks the final sentence. Until 2026-09-24 the model was a local Laya
checkpoint on MLX; it lost to the keyword gate (below) and was removed from the lane. Every judgement that could cost you something is a code
oracle in `fast_lane.py`, never the model's: the lane can only *add* stops to the
planner's `ask_user` contract.

It ships **off** (`EVE_FAST_LANE=0`) because the offline eval below says so; the
code is in place, the numbers are honest, and one line turns it on. The **router** below
is a separate switch and ships **on**: it is the same lane, run *instead of* the planner
rather than under it.

### Lane first: the router before the planner

The paired A/B further down is why. Under the planner the lane lost: the planner spends a
3.4 s turn to call the helper, so a two-click Calendar task went from 13.1 s to 16.9 s. A
planner turn is about 2.8 s of fixed cost before its first token and 0.8 s of writing (184
logged turns, 2026-09-21), and the median task is three turns, so the only way to win big is
to not take the turns. `lane_router.py` decides, in code, when that is safe: when your own
words leave nothing to interpret.

```
"switch to week view"              one control, Week, accounts for every word   → click it
"year view and then next year"     two clauses, each fully accounted for        → two clicks
"open notes"                       names another installed app                  → planner
"find the email from Sam"          words no control accounts for                → planner
"what is on my calendar"           a question                                   → planner
```

The rule: refuse a question, an empty or long request, and one that names an installed app
other than the frontmost. Then the keyword gate must pick exactly one control, and that
control's label and role, the app's name and a short list of generic words ("button",
"tab", "page", …) must account for **every** word of the request. The control's value never
counts, so "is week selected" is not covered by a selected Week. The whole request is tried
first ("desktop and dock" is one row); only if that finds nothing is it split on "and",
"then" and commas into at most four clauses, each under the same rule. Every gate below
still applies to every step, so a sensitive label, a dialog or a System Settings value stops
the router with zero input and the planner — with `ask_user` — takes over. The router never
types and never presses a key.

What happens next depends on what it did:

| Route | Meaning | Then |
|---|---|---|
| `complete` | every clause was clicked, and each click was *seen* to change something (the Accessibility diff or the screen diff said so; unknown is not proof), or its control was already selected | eve says "Done. I clicked Week." — **no planner call, no final-answer check** |
| `partial` | something was clicked, then a clause stopped or a click changed nothing | the planner starts from the screenshot with a `[note]` naming what was already clicked, and its final answer is checked |
| `none` / `refused` | nothing was clicked | the planner runs exactly as before; the cost was one Accessibility snapshot (0.04–0.43 s, Safari up to 0.9 s) |

Measured on the fixtures by the real router (`tools/fastlane_eval.py --fixtures
tests/fixtures/ax --router`, effectors that record instead of click): holdout 23 engaged of
82, **23 right** (Wilson 95 % 85.7–100 %); select 26 of 73, 26 right; no click on any
abstain-expected case, no sensitive click. It engages on about a third of the click cases and
declines the rest (no match 15, a tie 15, uncovered words 15, a confirm 10, a dialog 2 on
holdout). The bar was fixed before the first run — precision ≥ 90 % on ≥ 10 engaged holdout
cases, zero sensitive clicks — and `--check-default` asserts `LANE_FIRST_DEFAULT` equals the
decision. Two cautions the numbers do not remove: the holdout had already been sliced by
overlap and distractor before the cover rule was written, and the fixture goals were authored
as lane objectives, not transcribed from speech.

Measured live on this Mac, through the real `ComputerAgent` and a real worker, with a planner
that fails the run if it is ever called (`--live-route`): "switch to week view" **1.16 s**
wall including the worker's start, and the A/B's own two-click task, "go to year view and
then next year", **1.54 s** against 13.1 s with the planner alone — zero planner calls both
times.

### What the planner sees

In `keyword` mode (the default) the planner names the controls and the lane clicks them:

```
delegate(steps=["Year", "next year"], approve=None)
→ script: {complete|partial|none}; applied=[{action}, …]; step {i}/{n} {that step's own line}
```

Each step is a control's label exactly as the screenshot shows it, and each is its own
objective, so the gate cannot confuse step 2's control with step 1's — which is what a
single objective for a whole run ("year view, then next year") did. One planner turn then
covers up to six clicks. The script stops at the first step that does not act; `partial`
and `none` carry that step's own line, which is one of the five shapes below. Keyword mode
only clicks: `text=` or `key=` answers `unavailable`, because the gate has never been
measured on which field to type into, and the planner types with `type_text`.

In `model` mode the planner hands over one objective and the decider picks each step:

```
delegate(objective, text=None, key=None, done_when=None, approve=None, max_steps=4)
```

One line comes back, in one of five shapes (values single-line, quotes escaped; the
payload states are `none | pending | applied`, and a step applies its payload at most once):

```
done: matched="{done_when}" via={ax|ocr|title}; steps={n}; last={action}; text={state}; key={state}
stopped: reason={one_step|dry_run}; steps={n}; last={action}; pick={id role "label"}; text=…; key=…
escalate: app="{app}"; reason={no_window|no_candidate|dialog_open|truncated|model_invalid|abstain|
          stalled|stalled_2|ambiguous_target|no_match|uncovered|hit_test_failed|focus_changed|
          step_cap}; top=[…≤3]; steps={n}; last={action}; text=…; key=…
confirm: "{label}" needs the human's yes; app="{app}"; action={click|type|press}; steps={n};
         last={action}; text=…; key=…
unavailable: {reason}
```

`last` is the last input actually applied (or `none`); `steps` counts steps that applied
input; a `confirm` guarantees the blocked action did not run; `unavailable` guarantees zero
input. `done_when` is a distinctive marker that is *not* on screen yet and appears when
the objective is met (a control label that becomes selected, text that becomes visible, a
window title); with `done_when=None` the lane takes exactly one step and returns
`stopped`, which the planner treats as partial progress. On `confirm` the planner calls
`ask_user` and, on a clear yes, calls `delegate` again with identical arguments plus
`approve=<that label>`; `approve` is consumed by the first click of that label. On
`escalate` the planner looks at the screenshot and acts itself; a `confirm` or an
`escalate` is a recovery turn (high reasoning effort), like an error. The prompt only
mentions `delegate` when the lane is on, so a planner without the helper never reads its name.

### The gates in code

- **Sensitive labels fail closed.** A word-bounded table in `ax_candidates.SENSITIVE_LABEL`
  (delete, remove, trash, send, submit, pay, sign in / out, share, export, install, quit,
  close window, OK, continue, apply, archive, … including "Don't Save" and "Move to Trash")
  is never offered to the model. When the objective matches a withheld control at least as
  well as anything offered, the lane returns `confirm` for it with zero input.
- **Dialogs are never operated.** A sheet or a dialog window escalates `dialog_open` with
  its first line of text, before any menu is built.
- **System Settings only navigates.** Rows, cells, tabs, links and Back may be clicked;
  any value control (a Dark button, a toggle) returns `confirm`. To the Accessibility tree
  "Dark" and "Language & Region" are the same thing — a button with no value — and the A/B
  lost three lane runs of four to that. So the doors are a closed set in code
  (`SYSTEM_SETTINGS_PANE_BUTTONS`: the sub-panes of General — About, Software Update,
  Storage, Date & Time, Language & Region, Sharing, Startup Disk, Time Machine, …). Opening
  one changes nothing; everything inside it still confirms. The sensitive-label table
  outranks the set: "Privacy & Security" and "Login Items & Extensions" still ask.
- **Return into a message confirms.** `key="return"` is allowed only after the lane itself
  typed into a search or address field this run and no dialog is open; into a text area
  it returns `confirm` with zero presses. Key combos are never offered.
- **Re-hit-test at the click.** The element under the candidate's centre must still be
  there, in the same app, with the same title (whitespace-collapsed, case-folded) — else
  `escalate hit_test_failed` and nothing is clicked. The click goes through the same wrapped
  PyAutoGUI as the planner's own code, so the auto-screenshot and the `[after]` line apply.
- **Focus check before a paste.** `text` is pasted only if the focused element is the
  candidate (frame within 2 points, an editable role); otherwise `escalate focus_changed`.
- **No repeat, no stall, a step cap.** The previous step's control is dropped from the next
  menu whether or not the screen changed; two unchanged steps → `stalled_2`; two
  `reobserve` answers → `stalled`; `max_steps` (clamped 1–6) → `step_cap` after a last
  `done_when` check.
- **In `keyword` mode nobody guesses.** A tie escalates `ambiguous_target`, zero shared
  words escalates `no_match`, a router pick that leaves a word unexplained escalates
  `uncovered` — all with zero input, and no model is loaded or asked.
- **The model can only pick.** Its answer is rejected unless it is one of the offered keys
  with finite, consistent probabilities above the thresholds (`p_top ≥ 0.60`,
  `margin ≥ 0.15`, set by the eval); anything else escalates `ambiguous_target` or
  `model_invalid`. A step decided by the keyword gate (below) still passes every gate above.

### What else it adds

- **Local timing in the run log.** Every `exec_py` reply from the worker now carries a
  `timing` dict in milliseconds — `capture`, `resize`, `encode`, `ocr`, `ax`, `settle`,
  `act`, `decide`, `exec` — and the agent writes it beside each result. Before this only
  the model's `api_secs` and the verifier's `secs` were timed.
- **One settle, not two.** `open_app` / `open_url` settle inside the helper and the worker's
  auto-screenshot used to settle again (0.3 s typical, 1.5 s worst, from the run logs). A
  settle that finished is remembered with the input sequence number; the reply reuses that
  frame when no input happened since and it is under 0.3 s old. A raw click still waits up
  to 1.5 s. A timed-out wait also reuses its last fresh frame, so video and animation
  do not trigger a second wait. `wait_settled()` still returns false on timeout;
  new input or a frame older than the reuse window requires another wait.
- **Accurate OCR for clicks.** `click_text` polls accurate OCR directly. Fast OCR can
  miss a label that `screen_text` already read; previously that meant a three-second
  timeout and another model turn. `wait_for` retains its fast polling path.

### The eval, and why it ships off

The fixtures are real Accessibility walks of 12 app states (Calendar month and week,
System Settings General and Appearance, Safari on a Wikipedia article and on a page built to
distract, Finder, Notes, Music, Reminders, a Reminders sync dialog, Mail compose, Photos),
captured 2026-09-21 on this Mac, with every personal label replaced by a synthetic one of
the same shape, and 155 cases authored against them (73 in `select/`, which chooses the
prompt style and the thresholds; 82 in `holdout/`, read once for the ship decision — 34
cases share no word with their target, 20 have a wrong control that shares more). The
format and the capture recipe are in `bridge/tests/fixtures/ax/README.md`;
`tests/test_fixtures_ax.py` re-derives every snapshot from its raw dump with the current code.

```bash
cd bridge
.venv/bin/python tools/fastlane_eval.py --fixtures tests/fixtures/ax --min-select 30 --min-holdout 40 \
    --min-apps 5 --min-no-overlap 12 --min-distractor 6      # every style over both sets, the real decider
.venv/bin/python tools/fastlane_eval.py --fixtures tests/fixtures/ax --check-default   # the ship decision
```

The real decider the eval loads is the one `model` mode loads: hosted Jev (`jev.load`, a network
call per case; `--fake-predictor` runs offline). The numbers below are the run of 2026-09-21, when
the lane's model was the local Laya checkpoint (FP16 on MLX; load ~400 ms, first decision 1.5 s
cold or 30 ms with a warm Metal cache, then 8–14 ms a decision, ~1 GiB resident in the worker).
Jev on the same fixtures, in `jev` mode, is in [routing.md](routing.md#plan-once-execute-with-jev):

| Decision path | select top-1 | holdout top-1 | holdout gated top-1 / coverage |
|---|---|---|---|
| Model alone, first wording ("click radio button: Week", full context, compact style) | 31.5 % | 25.6 % | 25.0 % / 87.8 % |
| Model alone, shipped wording ("Week (radio button, selected)", title-only context, compact style) | 60.3 % | — | — |
| **Shipped: keyword gate first, model on the rest** | 69.9 % | 62.2 % | 72.3 % / 79.3 % at p_min 0.60, margin 0.15 |
| Keyword gate alone (unique best token overlap) | 90 % precise at 66 % coverage | 88.9 % precise, decided 54.9 % | — |

The keyword gate — take the one offered control whose label shares strictly the most words
with the objective, ask the model only on ties and misses — is a code oracle that decides
more than half the cases at ~89 %; the model alone gets 25.7 % of the rest on holdout, and
0 of the 9 holdout cases where a click is expected but no word is shared with its label —
exactly the cases the keyword gate cannot help with (it abstains there by construction, also
0 %). On the 10 abstain-expected holdout cases the model abstains half the time, the keyword
gate 90 %.
Cost-weighted (a correct click saves 3.5 s, a wrong one costs 4.55 s) the shipped path is
worth +0.61 s a case on holdout.

Sliced by who decided (same run, `--results-out`): keyword picks 88.9 % right (n = 45), model
picks 29.7 % (n = 37), model picks above the thresholds 26.7 % (n = 15); no app and no case
class reaches 80 % for the model (best: shared-word cases 40 %, n = 10; Safari and Calendar
0 %). At +3.5 s a right click and −4.55 s a wrong one that is about **+2.6 s per
keyword-decided step and −2.5 s per model-decided step**, which is why `keyword` is the
default decide mode and why Laya was later removed from the lane altogether. The keyword gate itself is 93–95 %
when the goal shares a word with the label and no wrong control shares more, and 0 % on the
distractor cases — the router's cover rule is what removes those.

The lane ships enabled only if all four hold on holdout: gated top-1 ≥ 80 %, coverage
≥ 70 %, model ≥ keyword + 10 points on the no-shared-word cases, cost > 0. Two fail
(72.3 %, and 0 vs 0 on the no-shared-word clicks), so `FAST_LANE_DEFAULT` is `False` and `--check-default`
asserts the shipped constant matches the decision. What would flip it: a decider that beats
the keyword gate where labels and objectives share no words — a checkpoint tuned on UI
menus, or a different small model — measured on the same holdout; the wiring, the
fixtures and the gates need no change.

### Install

Nothing to install: the keyword gate is code, and `EVE_FAST_LANE=1` turns the helper on
(its ready line says `fast_lane: ready (keyword gate)`). For `model` mode, set
`EVE_FAST_LANE_DECIDE=model`, `EVE_DECIDER=jev` and a Jev route (`TYPESAFE_API_KEY`,
or `EVE_JEV_ROUTE=openrouter` with `OPENROUTER_API_KEY`): the desktop worker loads the
decider in a daemon thread at start (`fast_lane: loading`, later replies `ready (load … warm …)`
or `failed: …`) and `delegate` answers `unavailable` until it is ready. Live checks: `tools/fastlane_eval.py
--live-snapshot Calendar` (a real walk: 124 nodes, 59 pressable, 0.08–0.17 s here; System
Settings 195 nodes 0.18 s; Safari on Wikipedia 3632 nodes 0.4–0.9 s once the page has
settled) and `--live-delegate Calendar "switch to week view" --done-when Week`.

### Deferred

- Ending a task on `done_when` without the planner's final sentence.
- Re-planning asynchronously while the lane runs; OCR candidates for apps with a broken
  Accessibility tree; page links beyond the same document (`allow_page_links` is off).
- Carrying stall and step accounting across retries of the same objective.
- Executing a `steps=[…]` script while the planner is still writing it. A planner turn is
  ~2.8 s before the first token and ~0.8 s of writing, so streaming can save at most that
  0.8 s a turn; putting six clicks in one turn already saves five turns. Measure first.
- The router typing into a search field ("search for jazz"): it only clicks today.

### Measured live, 2026-09-21

- One delegate step on Calendar (month → week): snapshot 34 ms, decide 0 ms (the keyword gate),
  click 128 ms, settle 210 ms — about 0.4 s against a 3.5 s planner turn; a dry run clicks nothing and
  a repeat from week view returns `done` with zero clicks.
- Paired A/B through the real planner (4 runs per arm, ABBA, state reset between runs), tasks that
  need two labelled clicks: Calendar year view + next year — lane off 13.1 s median, lane on 16.9 s
  (the planner spends a turn to call the helper, and used it in one run of four). System Settings
  General → Language & Region — off 13.3 s, on 14.6 s, and three of four lane runs stopped at
  `confirm: "Language & Region"` because the System Settings rule lets the lane click only rows, cells,
  tabs, links and Back; a pane-navigation button counts as a value change. So on these tasks the
  lane did not save wall time, and that rule is the first thing to revisit (a navigation button
  inside General is not a setting).
- The screen-diff fallback never worked before 2026-09-21: the lane asks "did the screen
  change?" after its post-click snapshot, and the adapter compared that snapshot's frame with
  a fresh capture — the settled screen with itself. Only a control whose Accessibility value
  flips (a radio button) was ever verified; a plain button read as unknown. The router's first
  live two-click run found it ("next year" clicked, `unverified`); the adapter now compares
  the step's two snapshots.
- Shadow verifier over the eight lane-on runs: the local verdict said true every time and the
  planner's check agreed every time (p_true 0.98–1.0) — no negative case yet, so nothing can be
  said about its discrimination.

## Safety

The task runs on your real desktop, so the guard rails are real too:

- **Confirmation** of consequential actions through `ask_user`, spoken; no listener → no.
- **The taint gate out loud** (`VoiceSession._gate`, `tool_gate.py`; details in
  [telegram](telegram.md)): after eve reads something it did not write, an acting
  call waits for your spoken yes to a question code wrote. The voice has no
  verbatim speech path, so code checks its output transcript said the act whole
  (`tool_gate.said_whole`) before your yes counts. A yes answers only the question
  asked last (the gate's or the task's), once.
- **Step cap** (25), **60 s** per code block, and a **fail-safe corner**: throw the
  mouse into a screen corner and PyAutoGUI aborts the current step and the task ends.
- **Stop** kills the worker and then releases every key and mouse button (a separate
  `--release` pass); a stuck or crashed step restarts the session once; a task ends
  after 25 steps or 3 minutes.
- `exec_py` runs unrestricted Python as the daemon user — the helpers exist so the
  model has no reason to shell out.
- **Untrusted screen**: the instructions tell the model to treat everything it reads
  on screen as data, never as instructions.
- **The fast lane only adds stops**: when it is on, `delegate` never clicks a sensitive
  label, a dialog, or a System Settings value — it returns `confirm` with zero input and the
  planner must `ask_user`; every click is re-hit-tested and every paste focus-checked.
- **Action log** per task under `agent-runs/` (code and text only).
- Nothing runs without the wake word; the spotter is muted while eve itself is
  talking so it cannot wake on its own voice.

## Cost, roughly

Wake-word spotting is local (about 1 % of a core). A conversation costs $0.05 per
minute of session, billed per second — about five times the Realtime-era rate it
replaced, and captions mode pays it for speech it never plays. The
Responses backend is billed separately, as are `gpt-6-astra` task tokens: one
screenshot is a few thousand input tokens, a typical 6-step task well under a dollar.
A web search is billed per call on top of the backend's tokens, and a `think_hard` is
one high-effort `gpt-6-astra` call (cents, not dollars) — every call sets `store=False`.
A fast-lane step decided by the keyword gate is code: it costs nothing and takes well under a
millisecond, which is why the lane exists. In `model` or `jev` mode a step is one Jev request
($0.042 per M input tokens, about 240 ms).

## Why these parts (research, 2026-09-06)

- **sherpa-onnx keyword spotting** needs no training for a new phrase (the phrase
  becomes BPE tokens in a keywords file) and has macOS arm64 wheels. openWakeWord's
  ONNX path returns near-zero scores on Apple Silicon (issue #336) and is 2.5 years
  stale; Picovoice Porcupine dropped its free tier on 2026-06-30;
  **livekit-wakeword** (Apache-2.0) is the upgrade path when a GPU training run per
  phrase is acceptable — it reports ~100x fewer false accepts than openWakeWord.
- **gpt-6-astra** is on `v1/responses` only (not Live), and OpenAI's own guide
  recommends the code-execution recipe (a plain `exec_py` function tool running
  PyAutoGUI locally) for it; the loop here follows `openai/openai-cua-sample-app`.
  The `beta.threads.tasks` / `computer_control_v1` API shapes seen in some snippets
  do not exist in the SDK (openai 3.13.0).
- **Mid-task steering**: Astra supports `response.steer` over the Responses
  WebSocket; this build queues steers and delivers them with the next step's tool
  results instead, which lands within seconds because every step is a turn boundary
  and keeps the plain HTTP loop.
- **Live voice** (2026-09-10): `gpt-live-1` on `POST /live/sessions`, a different
  endpoint from Realtime — `client.live.connect()`, `session.start`,
  `session.input_audio.append`, 24 kHz PCM16. It is full duplex, so there is no
  turn-detection block and no `output_modalities`. Tools belong to the delegated
  Responses backend, not to the voice model. Transcript deltas carry no done event,
  so `voice_agent._Turns` closes a turn on a speaker change or 1.2 s of quiet.
- **On-device perception** (measured 2026-09-06 on a 3024x1964 display): Apple
  Vision `VNRecognizeTextRequest` reads the full 2x capture in 0.31 s (accurate)
  or 0.04 s (fast, used for polling), and `CGDisplayCreateImage` captures it in
  0.04 s — pixel-identical to `pyautogui.screenshot()` at a third of the time. A
  local look costs a fraction of a second where a model turn costs 3–5 s.

Related: [personality.md](personality.md) for how the robot acts each phase out,
[build.md](build.md) for the `agent` wire verb.
