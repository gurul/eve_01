# Talking to eve from the iPhone: Hold to talk and the Action Button

Eve Link is the iPhone app that talks to eve from anywhere: hold to talk in
the app, or press the Action Button. It uses the iPhone's own microphone and
speaker on a call to the daemon through the Mini App's tunnel (`/api/stick`,
see [the call](#the-call)).

Eve Link first carried an M5StickS3 push-to-talk button over Bluetooth. The
owner removed the stick on 2026-10-07 (*"thats bloat it will just function
primarily from action on phone"*). The names `/stick`, `/api/stick` and
`stick-link.json` are from that time and are kept, so a paired phone keeps
working.

The owner asked for this on 2026-10-04: *"i want to essentially be able to
hold to talk with action button"*: hold the button to open the microphone,
release it to send.

## What the Action Button can and cannot do

**iOS does not tell an app when the Action Button is released.** When you
hold the Action Button, iOS runs the assigned action **once**, after the
hold threshold. The app receives no press-down event and no release event.
This holds for Controls and Shortcuts on the Lock Screen, the Home Screen and
inside other apps. Zello, the reference walkie-talkie app, has the same
limit: its Action Button mode is a toggle.

The only API that reports the Action Button's press and release is
`AVCaptureEventInteraction`. It works only while the app is in front with a
camera capture session running, and Apple limits it to camera apps. Eve
Link does not use it.

So Eve Link gives you the closest thing:

| Where | What you do | What happens |
|---|---|---|
| **Action Button** (anywhere) | Hold it until it fires, then talk | The microphone should open while your finger is still down (expected, not yet seen on the iPhone 17 Pro; see [Status](#status)). A Live Activity shows **Listening…**. |
| | Stop talking | After a 1.2 s pause, the turn is sent. eve answers on the phone's speaker. |
| | Or press again while it listens | Sends at once. |
| | Press while eve speaks | eve stops, and a new turn starts. |
| The big button in the app's **Talk** sheet (top right) | Hold, talk, let go; or tap | Hold: true hold-to-talk, the press opens the microphone and the release sends. If the touch is lost (an alert appears, the phone locks or the app leaves the screen), the turn is sent as if you let go. Tap: a turn that ends when you pause, as the Action Button. |
| **Live Activity** (Lock Screen, Dynamic Island) | Tap **Send** / **Talk** / **Interrupt** | The same as a press of the Action Button. |
| `evelink://talk` | Open the link (for example, a Shortcuts **Open URLs** action) | Opens the app and does the same as a press of the Action Button. |
| Any of the above, while eve takes notes | Press (or hold) | Stops the notes. eve writes them up and texts them. See [Notes on the go](#notes-on-the-go). |

Other limits:

- An Action Button, Live Activity or `evelink://talk` turn that hears
  nothing for 8 seconds hangs up, and nothing is sent. A **Hold to talk**
  turn is not hung up for silence: you end it by letting go.
- Every phone turn is sent after 60 seconds, held or not.
- A turn shorter than 0.35 s is a tap, and the daemon ignores it.
- The call hangs up after 45 quiet seconds. The
  microphone indicator stays on while the call is open, because one audio
  engine records and plays for the whole call.
- If the audio route changes during a call (AirPods or a Bluetooth headset
  connect), the phone restarts its microphone in the new format. If it
  cannot, or if a phone call or Siri interrupts the audio, the turn so far is
  sent and the phone's audio stops. The next press starts it again.
- None of the turn limits above apply while eve takes notes on the phone:
  no pause ends them, no silence hangs up, and there is no 60 s or 45 s
  limit. See [Notes on the go](#notes-on-the-go).

## Setting it up

You need the daemon with the Telegram door and the Mini App on
(`EVE_TELEGRAM=1`, `EVE_MINIAPP=1`,
[Mini App setup](stackchan/miniapp.md#turning-it-on)).

1. Build and install Eve Link: `sh ios/EveLink/build.sh --install`, with
   the iPhone unlocked and on the same network or a cable. It is signed with
   the team in `ios/EveLink/project.yml`. The app includes a widget
   extension, **EveLinkControls**. Automatic signing creates its App ID
   (`…EveLink.Controls`) on the first build. Build from this checkout's
   `main`: an older worktree builds an older app.
2. Pair the app with eve: on the Mac, run `eve-bridge link pair`, open the
   link on the iPhone and tap **Open in Eve Link**
   ([pairing](#pairing-the-phone)). If you skip this, the Action Button
   shows *"Pair Eve Link first: on the Mac, run eve-bridge link pair."*
3. Open Eve Link once, tap **Talk** (top right) and touch the big button. iOS asks for the
   microphone; allow it. The Action Button cannot show that prompt. Until you
   allow the microphone, it says *"Open Eve Link once and allow the
   microphone."*
4. Assign the button: **Settings > Action Button**. Swipe to **Controls**,
   choose **Eve Link > Talk to eve**. Alternatively, choose
   **Shortcut > Eve Link > Talk to eve**. Siri also accepts *"Talk to
   Eve Link"*.
5. Keep **Live Activities** on for Eve Link (Settings > Apps > Eve Link).
   iOS stops a recording that an intent starts unless the app shows a Live
   Activity.

## How it works

- `Intents/TalkIntent.swift`: `TalkIntent` is both an `AudioRecordingIntent`
  and a `LiveActivityIntent`. `AudioRecordingIntent` lets an intent start the
  microphone without opening the app, provided that the app shows a Live
  Activity for as long as it records. `LiveActivityIntent` makes the system
  run the intent in the app's process, where the call is, and not in the
  widget extension. The file is compiled into both targets, as Apple requires
  for the intent of a control. Its `authenticationPolicy` is `.alwaysAllowed`.
- `Controls/EveLinkControls.swift`: the **Talk to eve** control
  (`ControlWidgetButton`, with the action hint *Hold to Talk*) and the call's
  Live Activity, which has a button that runs the same intent.
- `App/PhoneLink.swift`: the app's single `Relay`, which the intent can reach
  outside the view tree. It also starts and updates the Live Activity, and it
  holds the App Shortcut. At launch it ends any Live Activity left over from a
  process that iOS stopped. If the owner swipes the Live Activity away, the
  next turn requests a new one.
- `App/PhoneAudio.swift`: one `AVAudioEngine` for each call. The microphone
  tap is converted to 24 kHz mono 16-bit PCM, the call's format
  (`PCM16.sampleRate`). eve's reply plays on a player node. The audio session is
  `.playAndRecord` / `.voiceChat` with voice processing, so echo cancellation
  keeps eve's voice out of the microphone.
- `Shared/Relay.swift`: `press()`, `holdBegan()` and `holdEnded()` send the
  call's `talk`, audio and `done` frames, and eve's reply plays on the
  phone's speaker.
- `Shared/PhoneTurn.swift`: the logic that can be tested: URL routing,
  `PressAction` (what a press does next), `TurnOwner` (whether the phone's
  turn is live), `TurnHold`
  (the finger on Hold to talk, and whether the address retry may start the
  turn again), and
  `EndOfSpeech` (RMS silence detection: 1.2 s of quiet after speech, 8 s with
  no speech, 60 s cap; a held turn has only the 60 s cap). It is tested by
  `ios/EveLink/Tests/run.sh`.
- `project.yml` adds `NSMicrophoneUsageDescription`,
  `NSSupportsLiveActivities` and the `audio` background mode, so the
  microphone, the WebSocket and the reply keep running with the phone locked.

## Pairing the phone

On the Mac, run **`eve-bridge link pair`**. It prints the pairing link:
eve's current address (read from the rendezvous below) with the link token in
the fragment, and a QR code when `qrencode` is installed (`brew install
qrencode`). Open it on the iPhone (scan the QR code, or AirDrop the link to
yourself) and tap **Open in Eve Link**. The app saves eve's address and the
link token in the Keychain. The link holds the token, so send it only to your own
phone. This works with Telegram off (`EVE_TELEGRAM=app`, 2026-10-08).

With Telegram on, sending **`/stick`** to eve does the same: it replies with an
**Update Eve Link** button that opens the same page.

The Mini App's quick tunnel gets a new address each time the daemon starts.
Once you have paired, the pinned **eve: your apps.** message carries the
**Update Eve Link** button too, and it is edited to the new address with the
Open button.

You don't need to tap it after a restart. The phone finds the new address by
itself (added 2026-10-04):

- **The daemon posts its address.** Whenever the tunnel comes up, and again every 6 hours,
  the daemon posts the tunnel's `https://….trycloudflare.com` address to an
  [ntfy.sh](https://ntfy.sh) topic. ntfy keeps a message for 12 hours, so the repost keeps
  it there. The topic name is `evelink-` plus 32 hex characters of HMAC-SHA256(link token,
  "eve link rendezvous"). Only a device that holds the token can work it out, and the name
  reveals nothing about the token. The address alone is useless without the token.
- **The app reads it.** Eve Link reads the topic when it launches, and whenever a call
  finds the old address gone. It takes a new address only when it is a bare quick-tunnel
  origin, and then starts the turn again, once.
- **The button still works.** **Update Eve Link** remains the manual fallback.
- **Settings.** `EVE_LINK_RENDEZVOUS=0` turns the rendezvous off.
  `EVE_LINK_RENDEZVOUS_URL` points it at your own ntfy server.

For an address that never changes, use a named Cloudflare tunnel or Tailscale
Funnel ([why the tunnel](stackchan/miniapp.md#why-the-cloudflare-tunnel)).

`/stick new` makes a new token. The old one stops working at once. Use it if a
phone is lost.

## The call

Eve Link opens `wss://<tunnel>/api/stick`. Its first message is
`{"token": …}`, and after that it speaks the Mini App call's own protocol
(`talk`, binary PCM, `done`, `ping`, `end`; `state`, `heard`, `said` (a sentence of eve's, before its sound, for voice mode's captions), `flush`,
`ended`). Audio is 24 kHz mono 16-bit PCM both ways, batched into 100 ms
WebSocket frames. A hello of `{"token": …, "can": ["notes"]}` says the app can
take notes on the phone; it adds `notes_stop` up, and the `notes` state and
`pong` down ([notes on the go](#notes-on-the-go)).

**eve's ears:** calls use `gpt-4o-transcribe`; `EVE_CALL_STT_MODEL`
overrides it. Near-field noise reduction suits a microphone held close. Server
VAD is off: the end of a turn commits the whole turn, so a pause cannot make an
extra transcription item. A broken realtime session falls back to uploading the
turn's complete recording.

## The link token

`stick_link.py` keeps one random token (`secrets.token_urlsafe(32)`) in
`~/.config/eve-bridge/stick-link.json`, mode 0600, next to the Mini App's
ledger. It is made on the first `/stick` and replaced by `/stick new`.
`/api/stick` accepts only this token, compared in constant time. It never
accepts Telegram's initData, and `/api/call` never accepts the token.

- **A browser cannot use `/api/stick`.** Browsers send an `Origin` on every
  WebSocket and the app sends none, so a request with an Origin gets 403.
- **The server never sees the token in a URL.** The pairing link is
  `https://<tunnel>/stick#t=<token>`, and the fragment never leaves the phone.
  The page at `/stick` is static and has a strict CSP (`default-src 'none'`)
  and `Referrer-Policy: no-referrer`. It reads the fragment, clears it from
  the address bar, and offers `evelink://pair?u=<origin>&t=<token>`.
- **On the phone,** the token is in the Keychain, readable after the first
  unlock (so a locked phone can still call) and only on this device.

## Chat in the app

The owner asked on 2026-10-08 to text eve in Eve Link instead of Telegram.
The app draws the conversation with OpenUI in a web view, and its native code
does all the networking. The daemon's half is `link_chat.py`. It adds HTTP
paths (the chat, the conversation, jobs and apps) to the Mini App's server, next to `/api/stick`, and the app reaches them
through the same tunnel.

**Auth.** Each request carries `Authorization: Bearer <link token>`, the token
from [the link token](#the-link-token), checked in constant time.

| Answer | When |
|---|---|
| 403 | The request has any `Origin` header. This check runs before the token is read, so a web page cannot use these paths. |
| 401 | The token is missing or wrong, or no phone was ever paired. |
| 503 `{"ok": false, "error": …}` | For `turn` and `messages`: the Telegram chat (the brain) is not running or no owner has texted it yet, or, for `messages`, memory is off (`EVE_MEMORY`). For `jobs`: the chief is off. For `apps`: the app builder is off. |

The token never reaches the app's JavaScript. The page asks native code, and
native code makes the call.

**`POST /api/link/turn`** with body `{"text": …, "client_id": …}`. The text is
1 to 4000 characters after trimming. `client_id` is at most 64 characters.
eve handles the text as a message the owner typed in the Mac chat window:
`hear(text, on_call=False)`, marked as the app's. The answer streams back as
`text/event-stream` in OpenAI Chat Completions chunks:

```
data: {"choices":[{"index":0,"delta":{"content":"…"}}]}
data: [DONE]
```

Each line eve says in the turn is one chunk. A blank line separates each
line from the one before. A `: ping` comment goes out after 15 quiet seconds.
The stream ends with `[DONE]` in three cases:

- No turn the owner started is still in flight.
- The turn asks the owner something, such as a chief's Go or an approval.
- 300 seconds have passed.

When the app sends the same `client_id` again within 10 minutes, for example on
a retry after a drop, eve does not hear the text twice. The app gets the first
turn's reply instead. eve keeps these replies in memory only, so after a
daemon restart the same `client_id` starts a new turn.

**`GET /api/link/messages?after=<cursor>&wait=<0..25>`** returns
`{"messages": [...], "cursor": …, "busy": …}`. Each message has `id`, `role`
(`owner` or `eve`), `text`, `t` (epoch seconds), `via` (`app`, `telegram`,
`voice` or `other`) and `kind` (`say`, `tool` or `image`). The list is read
from the transcript, so it also shows what was said in Telegram, on a call, or
by a task that answered later.

- **Without `after`:** the newest 100 lines, however far back (older pages: `?before=<message id>`, docs/app-chat.md
  "The whole history, infinite scroll"),
  oldest first.
- **With `after`:** newer lines only, at most 200. The next poll gets the rest.
- **With `wait`:** a long poll. It wakes when a transcript line is written,
  and it reads the files again every 2 seconds to see writes from another
  process.

Ids and the cursor have the form `<day>:<line index>`. The cursor is the last
line looked at, so polling works across the 04:00 day boundary. Forgotten
lines, command words, Claude's relay lines and memory-bus lines are left out.
`busy` is true while a turn the owner started is in flight.

**Reply routing.** When the owner writes in the app, the answer goes to the
app, not to Telegram:

- Everything the app turn makes eve say goes to the open stream and is not
  posted to Telegram. Telegram shows no "typing…" for the turn either.
- The transcript keeps the owner's words and eve's lines with
  `"via": "app"`. That field is new, and only app lines have it. Older lines,
  which do not have it, load as before.
- When a question from the app turn waits on the owner, the app gets the
  question and the replies that answer it, such as `Reply "yes" or "no".` The
  stream then ends. The owner's next text from the app answers the question
  through `hear`'s usual answer handling. A firm question, such as the chief's
  Go, still takes only a plain yes or no. The rest of the reply streams on that
  next turn's stream.
- A question asked outside an app turn, such as a chief's Go from a running
  card, still goes to Telegram with buttons. The owner can answer it either in
  Telegram or with a text from the app.
- When no stream is reading, the line goes to Telegram as before, so the owner
  still gets it and a notification. This happens when the phone hung up
  mid-turn, or when a task the turn started finishes minutes later.
- Photos, files, Web App buttons and a task's progress card (with its Stop
  button) stay in Telegram for now. The app has no channel for them yet.

**Speed dial.** `POST /api/link/turn` also takes `"grade"`. It sets how hard
the turn thinks. Any value that is not one of these three is a 400.

| `grade` | The turn |
|---|---|
| `auto` (the default) | Jev grades the turn `think` or `instant` (below). Graded `instant`, it is the chat as it is: `EVE_TELEGRAM_MODEL` at `EVE_TELEGRAM_EFFORT`, with `think_hard`. Graded `think`, it runs as `think` does. |
| `instant` | The same model at `low` effort. `think_hard` is not offered, and a call to it is refused. |
| `think` | `EVE_APP_THINK_MODEL` (default `gpt-6.1-sol`) at `EVE_APP_THINK_EFFORT` (default `medium`; `low`, `medium`, `high`, `xhigh` or `max`). |

The grade applies to every model call of the turn. The instructions and the
other tools do not change. Quick answers by code (the time, a sum, the
weather) still answer first, with no model call, in every grade.

**Jev grades `auto`.** For an `auto` app turn that reaches the model, eve
asks Jev whether the message needs thinking through (planning several steps,
money or dates under constraints, trade-offs, code, careful writing, costly
advice) or a short reply right away (`bridge/src/eve_bridge/grade_router.py`).

- Graded `think`, the turn runs on the think model and effort, exactly as the
  `think` grade does. Graded `instant`, the turn is the `auto` turn as it was.
- Jev runs in its own thread beside the toolset router and the prompt's
  building, so it usually adds no wall time. When the toolset router is off or
  keywords-only, the turn can wait up to 0.8 s for the grade. The grade waits
  0.8 seconds at most. A Jev error, a timeout or no Jev route grades `instant`.
- Only `auto` app turns are graded. The `instant` and `think` grades do not ask
  Jev. Telegram, voice and Mac chat turns are never graded.
- The grader is made once, when the chat starts. `EVE_GRADE_ROUTER=off`
  turns it off, and every `auto` turn is then the chat as it was.
- eve logs one line per graded turn, such as
  `telegram: grade: think (noul 0.91); jev 312 ms`. The line has numbers only,
  never the message.
- While the grader is on, your app message and the one before it go to Jev on
  every `auto` app turn, as they already do for the toolset router.

The grade was fitted on a tuning set of 220 messages and then scored once on a
blind holdout of 100 messages written by a different writer, against a bar set
before any answer existed. On the holdout: think recall 0.943 (bar 0.90),
over-think 0 (bar 0.15), Jev p95 482 ms (bar 800 ms) and 0 errors (bar 2%).

**Jobs.** The app's Work view shows the chief's cards. Both paths answer 503
when the chief is off (`EVE_CHIEF`).

- **`GET /api/link/jobs`** returns `{"jobs": [...], "needs_you": <count>}`.
  Each job is the chief's own card data (`id`, `title`, `status`,
  `waiting_for`, `phases`, `checks`, `spent`, `budget` and the rest) and also:
  `rev`, `updated`, `line` (one status line, such as "Waiting on your Go",
  "Researching" or "Done"), `steps`, `steps_done`, `needs_you` and `actions`.
  `needs_you` is true when the card waits on a Go, a budget raise or an
  answer, when its receipt is not confirmed, or when an act may have happened.
  `actions` lists the ones that do something now, from `go`, `done`, `drop`,
  `raise`, `happened` and `didnt`. A card whose act may already have happened
  gets `happened`, `didnt` and `drop`, never `go`, as Telegram's buttons do. Open cards come first, the ones that need the owner first, then
  the newest. The last 20 done cards follow, newest first. A dropped card is
  never listed.
- **`POST /api/link/jobs/<id>/<action>`**, where the action is `go`, `done`,
  `drop`, `raise`, `happened` or `didnt`. Each runs the chief's own method, as
  the Telegram words `go c12`, `done c12`, `drop c12`, `raise c12`,
  `happened c12` and `didnt c12` do. The answer is that
  method's dict, such as `{"ok": true, "id": "c12", "phase": 3}` or
  `{"ok": false, "reason": …}`. A Go keeps every chief rule. The body may carry
  `{"rev": <n>}`, the revision the app showed: a card changed since then is
  refused. A Go on a card whose act may already have happened is refused with
  `{"ok": false, "reason": …}`: the chief's `approve(via="id")` would run the
  act again, and a tap is not the typed `go c12`. Answer `happened` or `didnt`
  first. A Go given in quiet hours is recorded, and the act waits for the
  morning. An unknown card is a 404.

**Apps.** Both paths answer 503 when the app builder is off.

- **`GET /api/link/apps`** returns `{"apps": [{"slug", "name", "updated"}]}`,
  the owner's built apps, last changed first.
- **`POST /api/link/apps/<slug>/open`** returns `{"url": …}`: the app's page at
  the tunnel's address, `/apps/<slug>/?t=<app token>`, with a fresh 24-hour app
  token bound to that app. An app that does not exist is a 404.

**Generative UI.** `ios/EveLink/Web/scripts/gen-prompt.mjs` writes
`bridge/src/eve_bridge/openui_prompt.txt` from OpenUI's chat library, the
same library the app renders with, trimmed to the components a phone chat uses.
An app turn adds three things to the brain's instructions, in this order:

1. When to use OpenUI, with one small example (`link_chat.APP_RULE`).
2. That file.
3. A one-line reminder.

The texting style in the persona, which forbids lists, is swapped for an app
style. The block goes before the day's transcript, so consecutive app turns
keep it in the prompt cache. Telegram turns are unchanged. When the file is
missing or empty, nothing is added, and eve logs one warning. See
[app-chat.md](app-chat.md#when-a-reply-uses-openui).

## Notes on the go

The owner asked on 2026-10-04: "when i say start taking notes the phone should
hold the port until i click action button to turn it off, this allows me to
take notes on the go".

Say **"start taking notes"** on an Eve Link call. eve answers, and from then
on the call is a recorder, not push to talk: everything the phone's microphone
hears goes into the notes, with no press and no end of speech. Press the
**Action Button** again to stop. eve writes the notes up, says "Notes saved."
on the call, and texts them to you in Telegram. The text starts with the title,
the length and how many people spoke ("(42 min, 3 speakers)"), then carries the
summary and every section of the notes: speakers, topics, decisions, actions,
open questions, quotes, ideas, to-dos and follow-ups. Long notes go as several
messages, each under Telegram's limit, up to 8; the last one names the file
that holds the full transcript. The file is written beside the desk's notes, in
`transcripts/meetings/<date>/`, marked `source: eve-link`, so
`memory_search` finds it. The notes, the speaker pass and the file are the
room's: see [taking notes on the room](stackchan/voice.md#taking-notes-on-the-room)
for the format, the environment variables, the cost and the limits.

The transcript is on disk before the write-up starts. After you stop, eve
splits the recording by voice (one request per 20 minutes of audio), then
writes the notes. Each transcript line reads `[MM:SS] Speaker N: text`.

Other ways to stop: say "stop taking notes" into the phone (heard within one
12 s segment), text "stop taking notes", run `eve-bridge notes stop`, or
press **End**, which hangs up and writes the notes. The notes stop by
themselves after 180 minutes (`EVE_NOTES_MAX_MINUTES`).

**If the call drops** (no signal, a lift, a network change), eve keeps what
it already heard and waits 60 s for the phone to call back. If it does, the
same notes carry on. If it does not, eve writes up what it heard and texts
it. The phone notices a drop when a send or receive fails, or when nothing
(not even a `pong`) has come back for 25 s. Audio sent into a connection that
was already dead before the phone noticed can be lost: up to about 25 s.

A call that goes quiet for 30 s during the notes (the phone stalled) is closed
without an `ended`, so the phone treats it as a drop and calls back. eve
sends `ended` during notes only for **End**.

**If eve stops or restarts** during the notes, it writes them at once with
the transcript only (no summary, so it does not hold the restart up), within
30 s, and texts them as usual. `stopped_by` is "the daemon stopped".

**If the file cannot be written** (a full disk), eve never says "Notes
saved". It says it could not save them, and the Telegram text carries the
transcript itself, since it is then the only copy.

While notes are on:

- The call's one-hour limit does not apply. The hour counts from when the
  notes stop.
- The call gives the chat back. A text you get is not read out into the
  recording, and the desk's button is free.
- "Start taking notes" said at the desk, or texted, still records the desk's
  microphone, as before. So does a call from the Mini App.

**The wire** (`bridge/src/eve_bridge/phone_call.py`, `PhoneNotes`):

| Direction | Message | Meaning |
|---|---|---|
| up | `{"token": …, "can": ["notes"]}` | The hello. Without `"notes"`, a request for notes on this call is refused with "update the app", because an older build would record nothing. |
| down | `{"type":"state","state":"notes","started":<unix s>,"words":<n>}` | Notes are on. It is sent when they start, after eve's spoken reply, and when the phone calls back during the notes. `started` is when they began, for the timer. |
| up | binary PCM, 24 kHz mono 16-bit | The notes' audio. It needs no `talk` or `done`. |
| up | `{"type":"notes_stop"}` | The Action Button: stop and write up. A `talk` during the notes does the same. |
| down | `{"type":"state","state":"thinking","note":"Writing up your notes."}` | Stopped. The write-up is running. |
| down | `{"type":"state","state":"listening","note":"Notes saved: <title>"}` | Written and texted. "Notes saved." follows as audio. If nothing was said, the note is "Nothing was said, so there are no notes." |
| up | `{"type":"end"}` | End: stop and write up. A connection that breaks is not an `end`. |
| up | `{"type":"ping"}` | Every 10 s, as on any call. |
| down | `{"type":"pong"}` | Only while notes are on: the phone's proof that the connection is alive. |
| down | `{"type":"state","state":"listening","note":"Notes not saved to a file; I texted them to you."}` | The file could not be written. The text carries the transcript. |

While notes are on, eve may still send `speaking` (its reply to "start taking
notes"), then `notes` again. The app keeps the microphone running through it.

**On the phone** (`ios/EveLink/Shared/Relay.swift`, the notes section):

- When the `notes` state arrives, the app goes into notes mode. The
  microphone streams to the call in 100 ms frames with no end of speech: no
  1.2 s pause, no 8 s silence hang-up, no 60 s cap, and no 45 s hang-up
  timer. The audio engine is the call's, so it keeps recording with the
  phone locked (the `audio` background mode).
- The Live Activity and the Dynamic Island show **Taking notes** in a
  recording red, with a timer that counts from when eve started the notes
  (`started`), and a **Stop** button. An `AudioRecordingIntent` may record
  only while its Live Activity is up. If the panel was swiped away, a press
  brings it back first, and when iOS will not start it from the background the
  press continues in the foreground (as a start does). The app also asks for
  it again every minute, but iOS refuses that from the background, so it is a
  best effort only.
- The app's Talk sheet shows **Taking notes** with the same timer and a **Stop
  and write up** button in place of the talk button.
- Any press stops the notes (`PressAction.stopNotes`): the Action Button,
  the island's **Stop**, the in-app Stop or Hold to talk, or `evelink://talk`.
  The app sends the audio it has, then `notes_stop`, and shows
  "Writing up your notes…". It goes back to a normal call when eve sends
  `listening` with "Notes saved".
- **End** hangs up. eve stops the notes and writes them up.
- If the connection drops, the app keeps the microphone on, holds up to 60 s
  of its audio, and calls eve back (1, 2, 4, then 8 s apart, for up to
  55 s). Each failed attempt keeps the call-back going, also after a Stop.
  When eve says `notes` again, the held audio is sent first. A Stop (or a
  second Stop) pressed while the call is down is sent after the held audio,
  never ahead of it: nothing is queued for a call that is not up. If eve
  answers with no notes running (it already wrote them up), or the call
  does not come back, the app ends the notes and says eve will text what
  it heard.
- If a phone call or Siri interrupts the audio, the notes pause. The app keeps
  its audio session active so a locked phone is not suspended, and starts the
  microphone again when the interruption ends. This is not verified on the
  phone: iOS may still refuse to restart the microphone from the background.
- If the app is told notes are on while a turn of yours is still open, the
  turn is dropped on both sides, and your next press after the notes is a
  new turn.
- Every step is a `notes:` line in **Diagnostics**.

## The Lock Screen and the Dynamic Island

A call shows a minimal Live Activity, like the app icon (owner, 2026-10-08: "everything should be the
minimalist type"): white on black, eve's two white pill eyes, one line of words, plain symbols. The eyes narrow
while eve thinks and dim while the call is quiet. Every line is one line: the timer has a fixed width and never
wraps (it used to break onto two lines in the compact island). The only colour is a small red dot while notes
record.

| State | Compact island (right side) | Expanded island and Lock Screen |
|---|---|---|
| Listening | a running timer | "Listening…", "Pause or press to send", a round **↑** (send) and **×** |
| Thinking | a pulsing "…" | what eve heard, a round **mic** and **×** |
| Speaking | an animated waveform | what eve heard, a round **■** (cut in) and **×** |
| Taking notes | a red dot and a running timer | "Taking notes", "Press to stop", a round **■** and **×** |

The round white button is a press (`TalkIntent`); its symbol is what a press does now.
**×** hangs up the call (`EndCallIntent`).

## When something goes wrong

- **Live Activities are off** (Settings > Eve Link > Live Activities). iOS will not let a
  recording intent run without its Live Activity, so the Action Button opens Eve Link and
  starts the turn there, and the app says how to turn them back on. The first build crashed
  in this case (two crash logs on 2026-10-04); this is the fix.
- **eve restarted.** The phone finds the new address by itself
  ([the rendezvous](#pairing-the-phone)) and starts the turn again, once.
  A turn that ends itself (a press) starts again by itself. A turn a finger holds starts
  again only while the finger is still on **Hold to talk**; this includes an Action Button
  turn that the finger took over. A hold that failed to start (the microphone did not
  start) does not count as a finger. If you start a new turn (the Action Button, Hold to
  talk) or the call opens again while the phone looks up the address, the lost turn
  is not started over it (`TurnGeneration`). These rules are proved in
  `verification/Eve/LinkTurn.lean` and checked by `ios/EveLink/Tests/run.sh`.
- **The diagnostics log.** The **Diagnostics** section at the bottom of the app lists every
  step of a press: the intent ran, the decision, the microphone, the Live Activity, the
  call. It is also written, line by line, to `Library/Caches/diag.log` in the app's
  container, so it survives a crash. Copy it off a cable-connected phone with:
  `xcrun devicectl device copy from --device <id> --domain-type appDataContainer
  --domain-identifier com.github.eve-bridge.EveLink --source Library/Caches/diag.log
  --destination diag.log`. It never contains the token or any audio.
- **"Eve Link is no longer available."** iOS shows this when the app's signature has
  expired. A build installed from the Mac with a personal team is signed for 7 days.
  Run `sh ios/EveLink/build.sh --install` again with the phone unlocked. If
  `devicectl list devices` shows the phone as `connected (no DDI)` and not `available`, the
  script finds no phone: run `xcrun devicectl device install app --device <id>
  ios/EveLink/build/Build/Products/Debug-iphoneos/EveLink.app` yourself.

## Status

Verified on the owner's iPhone 17 Pro, iOS 26.7.1, on 2026-10-04:

- In-app Hold to talk makes a full round trip: listening, thinking, speaking, listening
  again in about 4 s.
- The Action Button opens the call from the Lock Screen, and the Live Activity shows it.
- With Live Activities on, the press no longer crashes.

Still to confirm on the phone:

- Does the Control start the microphone from the **locked** phone without
  Face ID?
- Does the recording keep going after the intent returns, and does the reply
  play while the phone is locked?
- Is the Live Activity's **Send** button responsive?
- Is the 1.2 s pause right for how you speak? The RMS threshold (0.015) was
  not measured on the phone's microphone, which applies voice processing.
- Does the intent fire while the finger is still down, after the hold?
- Notes on the go have not been tried on the phone: a long recording with the
  phone locked, the Live Activity staying up for the whole session, the call
  back after a drop, and the microphone restarting after an interruption.
- Audio interruptions (an incoming phone call) and route changes (AirPods
  connecting) are handled in code: the engine restarts, or the turn is sent
  and the audio stops. Neither has been tried on the phone.
