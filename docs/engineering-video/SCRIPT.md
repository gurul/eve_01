# eve, engineered: script

A technical breakdown of eve for engineers, rendered by the `EveEngineering` Remotion
composition. Length 9:27 (17,019 frames at 30 fps), 1920×1080.

**Narration is captioned, not spoken.** The launch films have no voice-over or TTS pipeline
(their README: "There is no VO asset"), so the narration below is burned in as captions and
also shipped as a subtitle track (`eve-engineering.srt`, and muxed into the mp4 as a
`mov_text` stream). The film has no audio.

**Single source.** The narration lives in
`docs/launch-video/remotion/src/engineering/script.json`, one entry per caption.
`make-timeline.mjs` times each caption by its word count (2.9 words a second, 2.2 s
minimum, 0.35 s between captions, 1.4 s title lead per section) and writes
`src/engineering/timeline.json` and the `.srt`. The timestamps below come from that
timeline. Edit `script.json`, not this file's narration, then re-run
`npm run render:engineering`.

The spoken text has no paths or code. On-screen cards carry short code excerpts,
switch names and paths. Each section lists the facts it relies on, with the file that
states each one (paths under `bridge/src/eve_bridge/` unless shown in full).

## Sections

| # | Section | Starts | Length |
|---|---|---|---|
| 1 | What it is | 0:00 | 0:51 |
| 2 | The hub and the agent contract | 0:51 | 1:05 |
| 3 | The request path | 1:55 | 1:23 |
| 4 | Worked example: the Holo driver | 3:18 | 1:15 |
| 5 | The chat brain and consent gates | 4:34 | 0:53 |
| 6 | Memory | 5:27 | 0:51 |
| 7 | The watcher | 6:18 | 0:32 |
| 8 | Verification | 6:50 | 1:17 |
| 9 | Spend and privacy | 8:07 | 0:48 |
| 10 | Run it, and read next | 8:54 | 0:33 |

## what it is (0:00–0:50)

On screen title: *A desk robot with a Mac as its host*

Narration:

- `0:01` A technical tour of eve: how it is built, and why.
- `0:05` eve is a small robot on your desk, with a Mac as its host.
- `0:10` The robot is an M5Stack StackChan: an ESP32-S3 running custom Arduino firmware.
- `0:15` It talks to a Python daemon on the Mac in newline-delimited JSON, over USB serial.
- `0:20` The link watches itself: twenty seconds of silence means it is dead, and repeated silent opens pulse RTS to reset the board.
- `0:28` A reflashed Home Assistant Voice PE is a push-to-talk controller, with a light ring that shows what eve is doing.
- `0:36` Telegram is the pocket door. It long-polls over outbound HTTPS, so the Mac opens no port.
- `0:41` A Telegram Mini App builds small apps and carries push-to-talk calls, and a menu-bar app with a widget shows the diary.

Facts relied on:

- One-sentence description, robot model, Arduino firmware on CoreS3 / ESP32-S3: `README.md:9-15`, `firmware/claude_pet_stackchan/claude_pet_stackchan.ino:1`.
- Newline-delimited JSON over USB serial: `README.md:332,355`; framing `bridge/src/eve_bridge/protocol.py:85-97`; 115200 baud `serial_transport.py:184` (on screen only).
- Link watchdog: `RX_SILENCE_SECS = 20.0`, firmware `[alive]` every 5 s, 3 silent opens in 600 s pulse RTS: `serial_transport.py:53-54,89-93,443`.
- Voice PE as controller with light ring, reflashed from stock ESPHome: `README.md:255-260`, `firmware/eve_voice_pe/eve_voice_pe.ino:1-6`.
- Telegram long-polls over outbound HTTPS, no open port: `README.md:460`.
- Mini App builds apps; Call eve is push to talk: `docs/stackchan/miniapp.md:3-8`, `README.md:111-113`, `bridge/src/eve_bridge/phone_call.py:1-18`.
- Widget: SwiftUI menu-bar app + WidgetKit: `README.md:360`, `widget/project.yml`.

## the hub (0:50–1:55)

On screen title: *One daemon, one contract*

Narration:

- `0:51` Everything meets in one process: the daemon, one asyncio loop.
- `0:55` At start it launches tasks for IPC, the robot link, the controller, Telegram, the watcher, the Mini App and spend sync.
- `1:03` Blocking work, like the serial port, runs on worker threads, because the loop is shared with every door.
- `1:09` Shutdown is ordered: the camera stops before the transport, the lanes close, then every task is cancelled and gathered.
- `1:16` The nightly memory job is drained, not cancelled: it gets up to twelve seconds to finish.
- `1:22` Every computer-use agent shares one contract: run, steer, cancel and status.
- `1:26` Run takes a goal and returns the answer. Steer corrects a running task. Cancel stops it. Status says where it is.
- `1:34` While it works, an agent emits AgentEvents: a frozen dataclass with a kind, a text and a turn number.
- `1:41` The contract is duck-typed, not a formal protocol. The voice session, Telegram and the robot's screen all read the same events.
- `1:49` So the Firecrawl reader, the Chrome lane, Codex and Holo are interchangeable behind it.

Facts relied on:

- Tasks started in `Daemon.run()`: `daemon.py:374-386,397-437,1337`.
- Blocking serial I/O on worker threads because the loop is shared: `serial_transport.py:19-22`.
- Shutdown order (camera/explore first, lanes close, cancel + gather, conversations 3 s, dream drained up to `DRAIN_SECS = 12.0`, link/IPC last): `daemon.py:443-490,574-585,102-103`; SIGINT/SIGTERM in `cli.py:576-581`.
- `AgentEvent` (frozen dataclass: kind, text, turn; the nine kinds): `agent_contract.py:14-21`.
- run/steer/cancel/status signatures: `codex_computer.py:320-348,592`; also `holo_computer.py:230-251,476`, `web_reader.py:297-322`, `chrome_lane.py:69-103`, `app_reflex.py:271-296`.
- No `Protocol` class for the contract (duck typing): grep of `bridge/src/eve_bridge` found Protocol classes only for unrelated seams (`ipc.py:47`, `phone_call.py`).

## the request path (1:55–3:18)

On screen title: *Cheapest first. Each tier declines unless it is sure.*

Narration:

- `1:56` The path a request takes follows one rule: cheapest first, and each tier declines unless it is sure.
- `2:03` The measurement behind it: a quarter of requests are app launches, and through the planner each took eight to thirty-two seconds.
- `2:10` Tier one is the reflex. Rules in code turn "open Spotify" into one open command. No model, microseconds.
- `2:17` For wording the rules do not know, Jev, a hosted typed-decision model, may name an app. It counts only if the app is installed.
- `2:26` Anything consequential, like send, buy, pay or delete, skips the reflex and goes to a planner that can ask the owner.
- `2:33` Tier two is the body router. Jev fills in a typed form: is this a public reading job? Does it need accounts, the Mac, or interaction?
- `2:42` Code, not the model, applies the gates. Firecrawl gets the task only when every condition holds. An error falls back to the computer.
- `2:51` The gates were fitted, then scored once on a blind holdout: thirty routed, thirty right, zero unsafe.
- `2:57` Otherwise a code rule picks between eve's own Chrome, driven by Playwright in its own tab, and the desktop.
- `3:04` Tier three is the desktop floor: Codex computer use by default, or Holo by a switch. On the owner's Mac today, it is Holo.
- `3:13` The model proposes; allowlists, gates and consent checks in code decide.

Facts relied on:

- "tiers, cheapest first, and each tier declines unless it is sure"; a quarter of requests are launches, 8–32 s through the planner: `task_router.py:7-13`, `app_reflex.py:4-5`.
- Reflex rules in code, Jev only for unknown wording, answer counts only if installed: `app_reflex.py:13-16`, `task_router.py:415,456`.
- Consequential words go to the planner (the only tier with ask_user): `task_router.py:31-32,70-74,421-422`.
- Jev = TypeSafe typed-decision model (`jev-latest` / `typesafe/jev-1.13`): `docs/stackchan/routing.md:25-27`, `jev.py:56-59`.
- Body form (public_read_job, needs_accounts, needs_mac, needs_interaction, show_owner, p_auto) and `decide()` failing closed to CODEX: `browser_router.py:70-109,141-149`.
- Gates fitted then scored once on a blind holdout: 30 routed, 30 right, 0 unsafe: `browser_router.py:175-183`.
- Chrome vs desktop by a code rule (`is_browser_only_goal`): `daemon.py:1471-1494,1534`.
- Chrome lane: Playwright, own profile (`EVE_BROWSER_OWN`), own tab, sensitive control refused without approval: `browser_lane.py:1,37-48,570-571`.
- Desktop floor: Codex by default, Holo when `EVE_COMPUTER=holo`: `daemon.py:1288-1310`, `holo_computer.py:66-70`. "Holo on the owner's Mac today": `~/.config/eve-bridge/env` contains `EVE_COMPUTER=holo` (checked 2026-09-28).

## worked example (3:18–4:33)

On screen title: *The Holo lane: wrap the vendor's client*

Narration:

- `3:19` The Holo lane is a worked example of one rule: wrap the vendor's client, do not reimplement it.
- `3:26` Holo's command-line tool ships its own Python, with fifteen dependencies. eve does not import it.
- `3:31` It starts a small driver script under Holo's own interpreter, as a subprocess, and they speak JSON lines.
- `3:38` eve writes one op per line: run, steer, or cancel. The driver answers with events: ready, session, progress, steered or refused, stopping, final, error.
- `3:46` The session request and the turn loop are the ones the vendor ships, not a copy that drifts.
- `3:53` Steer becomes a message on the running session. An early correction is queued; a conflict is retried four times, then reported as refused.
- `4:01` Cancel files the tool's own stop request, then pauses and cancels the session. A closed input pipe does the same, so nothing keeps clicking.
- `4:10` The runtime is kept warm: the daemon spawns it once with a random token, waits for its health check, and reuses it for every task.
- `4:19` The daemon owns it on purpose, because the runtime exits when the process that launched it exits.
- `4:25` Progress lines carry tool names, never arguments, so a typed password never reaches the chat or the robot's screen.

Facts relied on:

- Driver runs under holo's own Python so eve stays free of the CLI's fifteen dependencies; the vendor client builds the session: `holo_driver.py:1-6`, `docs/holo-computer-use.md:46`.
- Interpreter `~/.holo/tools/holo-desktop-cli/bin/python`: `holo_computer.py:43`; spawn command with `--port 18795`: `holo_computer.py:349-354`.
- JSON-lines protocol (ops run/steer/cancel; events ready/session/progress/steered/refused/stopping/final/error): `holo_driver.py:8-14,105,113,119,177,202-203`, `holo_computer.py:356-389`.
- The turn is `session_runner.run_turn`, the vendor's own driver: `holo_driver.py:16-18,42-47`.
- Steer = `send_message`; queued until running; 409 retried up to 4 attempts, then `refused`: `holo_driver.py:73-113`.
- Cancel = CLI `request_stop`, then pause + cancel; SIGTERM / closed stdin do the same: `holo_driver.py:20-22,115-125,147-148,184-185`.
- Warm runtime: spawned once with `secrets.token_urlsafe(32)`, waits for `GET /health` up to 45 s, reused; daemon owns it because it exits with its launching parent: `holo_computer.py:103-180`, `daemon.py:1299-1301`.
- Progress never carries tool arguments: `holo_driver.py:24-26`, `holo_computer.py:88-90`.
- Model `holo4-27b`: `holo_computer.py:37` (on screen only).

## the chat brain (4:33–5:26)

On screen title: *Tools for the model, gates in code*

Narration:

- `4:34` The chat brain sits behind Telegram. Calls and the Voice PE button transcribe speech and hand it over as if typed.
- `4:42` It runs on the OpenAI Responses API with storage off, and up to six tool rounds a turn.
- `4:49` Its tools start, steer and stop computer tasks, take photos, move the robot's head, search memory and start coding sessions.
- `4:56` The door is an allowlist in code: numeric owner IDs, private chats only, no bots, and forwarded messages never reach a model.
- `5:04` Connected apps have a policy per toolkit: Gmail is read only, Calendar may write, Drive asks.
- `5:10` A Gmail write is refused, not asked. A Drive write asks the owner, and anything but a clear yes is a no.
- `5:18` The Mini App binds to localhost behind a tunnel, and checks Telegram's signed init data with an HMAC on every call.

Facts relied on:

- Calls and the Voice PE button hand transcribed speech to the chat brain as if typed: `phone_call.py:8-12`, `docs/voice-pe.md:52-68`.
- Responses API with `store=False`; `MAX_TOOL_ROUNDS = 6`: `docs/stackchan/telegram.md:17,742-748`, `telegram.py:126`.
- Tool list: `telegram.py:375-505,532-533`, memory tools `memory.py:139`.
- Owner allowlist (numeric ids, private chat, no bots, forwarded messages never reach a model): `telegram.py:17-26,668-673`, `docs/stackchan/telegram.md:134,140`.
- Toolkit policy gmail=read, googlecalendar=write, googledrive=ask; a write under read is refused, never asked: `composio_tools.py:66-71`; the gate: `telegram.py:3744-3754`; fail-closed yes check `consent.approves`.
- Mini App binds 127.0.0.1 behind a Cloudflare quick tunnel; `initData` HMAC with the WebAppData key, age and owner checks: `miniapp.py:8-15,180-194`.

## memory (5:26–6:18)

On screen title: *Three stores, a dream, and a real forget*

Narration:

- `5:28` Memory is three stores, each with one job.
- `5:31` Transcripts are the source: every word, written the moment it is said.
- `5:35` Records are the truth: markdown about the owner, in a local git repository. The brains read records; they never write them.
- `5:43` mem0 is the index: search by meaning in a local Qdrant. Only fact extraction and embeddings leave the machine.
- `5:50` At four thirty each night the dream reconciles the day into the records, feeds mem0, and commits. Missed nights are caught up.
- `5:58` Forget is two steps. A preview returns only counts, plus a single-use token that lives ten minutes.
- `6:04` Apply runs only after the owner confirms in a new message, and git history is squashed.
- `6:10` A race where the dream could write forgotten lines back was caught by a Lean model, and fixed.

Facts relied on:

- Three stores and their roles (source / truth / index): `memory.py:3-9`.
- Records in their own local git repo; brains read, never write: `records.py:7-16`.
- mem0 self-hosted on Qdrant; only extraction and embeddings leave the machine; telemetry forced off: `mem0_memory.py:5-8,21-24`.
- Dream at 04:30 (`DREAM_HOUR = 4.5`): reconcile, consolidate, feed mem0, git commit; missed nights caught up oldest first: `dream.py:14-27,37-46,91`.
- Forget: preview returns counts + single-use token (`FORGET_TTL_SECS = 600`); apply only after confirmation in a new message; git history squashed: `memory.py:20-28,68,122-132`.
- Forget/dream race proved in Lean: `records.py:30-32`, `verification/Eve/Forget.lean`, `docs/verification.md:23-28`.

## the watcher (6:18–6:49)

On screen title: *A ladder of readers, cheapest rung first*

Narration:

- `6:19` The watcher checks stock quotes, pages, web searches and Ticketmaster events on a schedule.
- `6:24` A condition fires when crossed, then re-arms. Nothing fires on the first reading.
- `6:29` Fetching starts plain, and switches to a Chrome-like TLS handshake when a site blocks it.
- `6:35` Then readers, cheapest first: structured data, a cheap model, headless Chromium with vision, Firecrawl, web search.
- `6:40` A page that once needed a higher rung goes straight there next time. One limiter holds everything to twelve requests a minute.

Facts relied on:

- Kinds quote / page / search / ticketmaster: `watch.py:1-18`.
- Fires on crossing, re-arms, nothing on the first reading: `watch.py:23-25`.
- Plain fetch, then `curl_cffi` Chrome-like handshake on 401/403 or timeout: `docs/stackchan/watch.md:70-78`.
- Reader ladder (structured data, cheap model, headless Chromium + vision, Firecrawl at most 30 a day, web search) and sticky rung: `docs/stackchan/watch.md:91-155`.
- Global limiter 12 requests a minute: `watch.py:98`. (The 48-model-calls-a-day cap at `watch.py:103` was cut from the narration for length.)
- Eight Watch* Lean models: `verification/Eve/Watch*.lean`, `docs/stackchan/watch.md:31-32`.

## verification (6:49–8:06)

On screen title: *Proofs of the bug, proofs of the fix*

Narration:

- `6:51` eve's state machines are modelled in Lean 4: twenty models, two hundred fifty theorems.
- `6:56` Each follows one pattern. "Current violates" is a concrete trace on the old code after which the property is false: the bug, proved.
- `7:04` "Fixed invariant" proves the property on the fixed code for every trace, by induction over any list of events.
- `7:11` Each counterexample is replayed as a pytest against the real Python. It failed before the fix, and passes now.
- `7:18` A checker rejects sorry, and any proof resting on an axiom beyond the standard three.
- `7:24` The worked example: on the twenty-eighth of September, three phone-call turns died on a bare TLS error.
- `7:30` Always retrying is wrong: the reply is read aloud as it streams, so a retry after speech repeats the sentence.
- `7:37` The model has three events: drop, text and done. The spec: never repeat text, and fail only after two drops, or a drop after text.
- `7:46` No retry breaks it on one drop. The naive retry breaks it on text, drop, text.
- `7:52` The fix retries once, only while nothing has been spoken. It holds for every trace, and replay tests pin both counterexamples.
- `8:00` Around the models, the Python suite stands at three thousand four hundred sixty-seven passing tests.

Facts relied on:

- Measured 2026-09-28: 20 model files in `verification/Eve/`; `cat verification/Eve/*.lean | grep -cE '^\s*theorem '` = 250; toolchain `leanprover/lean4:v4.34.1`.
- current_violates / fixed_invariant / replay-as-pytest: `docs/verification.md:3,8-17`.
- `check-all.mjs` requires the pattern; `check.mjs` rejects `sorry`/`admit` and any axiom beyond propext, Classical.choice, Quot.sound: `verification/check-all.mjs:11-31`, `verification/check.mjs:9-10,23-35,54-56`.
- TLS incident and model: `verification/Eve/ResponseRetry.lean:5-11,26-29,46-47,67-70,83-94,107-133`; commit 9736dd9 "Calls: retry once when TLS drops mid-request".
- Code it mirrors: `computer_agent.py:1518-1567` (`TLS_RETRIES = 1`, `if attempt == TLS_RETRIES or spoke: raise`).
- Replay tests: `bridge/tests/test_holo_lean.py:91-115`, `bridge/tests/test_response_creators.py`.
- Test count measured by running `uv run --frozen pytest -q` in `bridge/` on 2026-09-28: `3467 passed, 15 skipped in 507.72s` (working tree with the uncommitted edits listed by `git status`).

## spend and privacy (8:06–8:54)

On screen title: *One meter, and what leaves the Mac*

Narration:

- `8:08` Every model call goes through one meter: a line per call with provider, model, feature, dollars and tokens. Never a prompt or an answer.
- `8:16` Cost comes from the provider, or a price table, or stays empty when unknown, rather than guessed.
- `8:23` What leaves the Mac depends on the lane. Reflex rules send nothing. Jev sends the request's words, so it is the owner's switch.
- `8:31` Firecrawl gets a query of up to five hundred characters, and the pages go to a cheap model to answer.
- `8:38` The Chrome lane's planner gets screenshots and the prompt. Holo runs on a hosted model API; its local screenshot folders are deleted.
- `8:46` Telegram is not end-to-end encrypted, Mini App traffic crosses Cloudflare's tunnel, and mem0 sends extraction and embeddings to OpenAI.

Facts relied on:

- One meter, a JSONL line per call, never a prompt or an answer; reported / priced / `usd: null`: `spend.py:8-20`.
- Reflex rules send nothing; Jev sends the request (off by default for that reason): `docs/stackchan/routing.md:34`, `task_router.py:347`.
- Firecrawl query cut to 500 characters; pages answered by `google/gemini-3.1-flash-lite` via OpenRouter: `web_reader.py:45,142,230-233`, `websearch.py:41`.
- Chrome lane planner gets screenshots and the prompt: `docs/stackchan/routing.md:34`.
- Holo runs on the hosted Models API; local run directories deleted: `holo_computer.py:4,423-427`, `docs/holo-computer-use.md:60-61`.
- Telegram not end-to-end encrypted: `docs/stackchan/telegram.md:742-748`. Cloudflare terminates the tunnel's TLS: `docs/stackchan/miniapp.md:631-661`.
- mem0: `mem0_memory.py:7-8,21-22`.

## run it (8:54–9:27)

On screen title: *Switches, and where to read next*

Narration:

- `8:55` To run it, the switches are environment variables. Telegram, the Mini App and memory ship off; each is one switch.
- `9:03` The computer switch picks Codex or Holo. The router switch lets Jev word reflexes. The own-browser switch gives eve its own Chrome.
- `9:11` The Firecrawl reader turns on when its key is set.
- `9:14` Read next: the routing doc, the Holo doc, the memory docs, and the verification doc for all twenty models. Each model is a small, exact story of a bug and its fix.

Facts relied on:

- Defaults: `EVE_TELEGRAM` off (`telegram.py:101`), `EVE_MINIAPP` 0 (`miniapp.py:137`), `EVE_MEMORY` off (`memory.py:86`), `EVE_COMPUTER` codex (`holo_computer.py:67-70`), `EVE_ROUTER_MODEL` off (`task_router.py:347`, `app_reflex.py:204`), `EVE_BROWSER_OWN` 0 (`browser_lane.py:224-229`), `EVE_WEB_READER` auto = on with `FIRECRAWL_API_KEY` and a passed holdout (`web_reader.py:86-95`).
- Where to read next: `docs/stackchan/routing.md`, `docs/holo-computer-use.md`, `docs/memory-bus.md`, `bridge/src/eve_bridge/memory.py`, `docs/verification.md`, `verification/Eve/`.

## Left out, or softened, because it could not be verified

- **Codex data flow.** No file states what Codex computer use sends off the Mac, so the privacy
  section does not list Codex.
- **Holo screenshots to H Company.** The code says Holo runs on a hosted Models API and looks at
  screenshots of the Mac, but no file says in so many words where the screenshots go. The
  narration says "runs on a hosted model API" and stops there.
- **A per-task bill.** `spend.py` records every call but no per-task bill was found in it, so
  the film does not claim one.
- **A total bug count.** `docs/verification.md` gives no single number of bugs found, so the
  film gives none.
- **A 30-day forget.** 30 days is only the default memory search window, not a forget
  policy, so it is not mentioned.
- **Lean build status.** The proofs were not re-run for this film (`node check-all.mjs` was not
  executed). The 20 / 250 figures are counts of files and theorem declarations. Separately,
  `verification/Eve.lean` imports only the twelve non-Watch models, so a bare `lake build`
  may not compile the eight Watch models; `check-all.mjs` compiles each file directly.
- **The agent contract as a Protocol.** It is not one. The film says it is duck-typed.
