# eve's Mini App: your apps

Ask eve for an app, like "make me a habit tracker", and it builds one: a
small, real app that opens inside Telegram and keeps its data on your Mac.
Before you see it, eve uses it on a phone-sized screen and fixes what broke.
You can change, undo, rename or delete an app from the chat or from the Apps
screen, and each app has its own change chat where you keep refining it
("make the buttons bigger", "add categories").

The Mini App had a Claude chat tab as well. It was removed on 2026-09-24 so the
Mini App is only your apps.

Code:
- `bridge/src/eve_bridge/miniapp.py`: the server, sign-in check, spend
  ledger, tunnel and pinned button.
- `apps_maker.py`: what Claude is told, the build and repair loop, and storing
  and serving apps.
- `app_check.py`: the phone check every build goes through.
- `jev_verify.py`: Jev walks the app's journeys and judges what the screen
  shows.
- `miniapp_page.html`: the home screen and the Spending view.
- `bridge/tools/app_eval.py`: builds apps from the command line into a scratch
  folder and writes a report with cost, time, the check's result and the
  journeys Jev walked. `--check DIR` runs only the check (script, then
  journeys) on `DIR/index.html` with `DIR/journeys.json`, for example on a page
  broken by hand as a negative control.
- `bridge/tools/journey_eval.py`: fits and scores Jev's cut-offs on real apps.

Tests: `bridge/tests/test_miniapp.py`, `bridge/tests/test_apps_maker.py`,
`bridge/tests/test_app_check.py`, `bridge/tests/test_jev_verify.py`.

## Opening it

The **menu button** next to the message box stays the `/` command list. The
Mini App is always one tap away three other ways:

- **A pinned message** at the top of the chat has an **Open eve** button.
- **`/apps`**, the first command in the menu, sends the same button. Typing
  "my apps" or "open apps" does too, even while `claude on` is relaying.
- **After a build**, eve sends a picture of the new app with an
  **Open ‹app›** button under it.

Telegram's menu button can be either the command list or one Mini App, never
both, which is why the Mini App gets the pinned message instead.

## Calling eve

The owner asked on 2026-09-25 for voice calls over Telegram, then for push to talk "instead of live",
for "full functionality like i am texting it", and for no texts on a call "unless it's necessary". A
Telegram bot cannot take a Telegram call, so the call is in the Mini App: **Call eve**, top right.

- **Hold to talk, let go to send.** While the button is held the phone's microphone goes to the daemon
  (24 kHz 16-bit audio over a WebSocket, through the same tunnel). A soft two-note blip says it was sent.
- **It is the chat, spoken.** The words are handed to the **Telegram chat's own brain exactly as if you had
  typed them**, so a call can do everything a text can: mail, calendar, Drive, watch, `/meet`, notes, tasks
  on the Mac, app building, memory, and the relays. The brain is told it is on a call, so it answers in one
  to three spoken sentences. Through the Claude relay, a prompt you say on the call has Claude's answers,
  questions, "waiting" notes and permission prompts read out on that call, and you can answer them out loud;
  a prompt you type has them texted only.
- **Only this call's work is read.** A reply to something you typed (in Telegram or the Mac chat window) stays
  in the chat even while a call is open, and so does work begun on an earlier call that finishes on a later
  one (owner, 2026-10-04: "dont read aloud things from telegram unless its something explicit from like the
  action call"; verification/Eve/CallScope.lean). Words are answered on the call that heard them: a desk
  press during a phone call is answered on the desk, which takes the chat back.
- **Spoken, not texted.** On a call eve's replies are read out and do not appear in the chat. What you
  need in writing is still texted: any link, and anything the brain puts after a line of just 📎 (an
  address, a code, numbers to copy). Functional messages still go as they always do: watch alerts and
  lists, screenshots and photos, Meet notes, questions with buttons, task progress. Your words are not
  echoed into the chat; eve's memory transcript keeps the conversation.
- **Hold while eve talks** to interrupt it.
- **Only you.** The call opens only with Telegram's signed initData for an owner, from the Mini App's own
  page. One call at a time, up to an hour.

### Latency

Historical measurements from 2026-09-25 (owner: "how can we improve latency"). Before that change: 5 to 11 s from letting go to eve's voice. The table describes the server-VAD configuration measured then; it is not a benchmark of the current transcription configuration.

| Stage | How | Measured |
|---|---|---|
| Your words to text | OpenAI realtime transcription (`gpt-4o-mini-transcribe`), one session per call, fed while you talk; the server transcribes at pauses | 0.39 to 0.50 s after release, whatever the press's length (uploading after release: 0.51 to 0.92 s, growing with length) |
| The answer | The chat's model (`gpt-6-luna`) streamed: each sentence is spoken as soon as it is written; the prompt cache is warmed when the call opens | first sentence 1.9 s for a plain answer; tool turns take longer (each tool is real work) |
| Text to voice | Deepgram Aura-2 through OpenRouter (`deepgram/aura-2`, voice `aura-2-thalia-en`), streamed; until 2026-10-08 OpenAI `gpt-4o-mini-tts` if it failed (now a failed sentence is not spoken, and the chat has it) | first sound 0.31 to 0.50 s (OpenAI streamed: 1.15 s; Kokoro on OpenRouter: 7 to 10 s) |

Every press is timed in the daemon's log (`call: press … → heard in … → first reply … → first sound …`).
None of OpenRouter's transcription models beat the live session (Deepgram Nova-3 0.69 s, Whisper Large V3
Turbo 1.32 s, both uploaded) in that comparison.

**Since 2026-10-08 the ears are uploaded, on OpenRouter.** Every model call moved to `OPENROUTER_API_KEY`
([one key: OpenRouter](../openrouter.md)), and OpenRouter has no realtime transcription. Each press is now
uploaded after release to OpenRouter's `/audio/transcriptions` (`openai/gpt-4o-transcribe`, `language=en`), the
path that was the fallback below; expect the upload figures above (0.51 to 0.92 s) rather than the live ones.
The live ears are kept as an opt-in: `EVE_CALL_LIVE_STT=1` with an `OPENAI_API_KEY`.

The current default is `gpt-4o-transcribe`. Since 2026-10-03, the realtime session waits for its configuration acknowledgment and uses manual commits: one complete button press is one item, including pauses. A new press waits until the preceding item finishes or is discarded, and delayed completions are matched by item ID. This removes the cross-press transcript race. Transcription finishes after release; release-to-text latency needs a new live measurement. If realtime setup, feeding or completion fails, the daemon uploads the whole recorded press. See the [optimization review](../optimization-review.md) for regression evidence and limits.

### When a turn fails

If the model call itself fails, eve says "Something went wrong on my side. Try me again in a moment." and
the log has `telegram: turn failed: <error>`. When the failure is the connection to the model breaking (a
TLS drop, a reset socket, or the SDK's own connection error), the reply says so instead: "My connection to
the model dropped before I could answer. Send that again and I'll pick it up." (`DROPPED_LINE`,
`computer_agent.is_connection_drop`).

A TLS connection dropped while the answer was being read comes out of the OpenAI SDK as a bare `SSLError`
that it does not retry (2026-09-28: three call turns in one morning). `make_response_creator` and
`make_stream_creator` retry it themselves, up to twice, after a pause of 0.3 s and then 1 s
(`TLS_BACKOFF`), and each retry goes out on a new OpenAI client, so a new connection pool and a new TLS
connection (`responses: TLS dropped … retrying in 0.3 s on a new connection` in the log). The first version
retried once, at once, on the same client: on 2026-09-30 the retry went out 120 ms later on that client's
pool and dropped the same way, and the owner got the generic line. The dropped client is closed once no
other call is still using it. The streamed call retries only while nothing has been read out yet; a drop
after the first sentence still fails the turn rather than repeat it.

Code: `bridge/src/eve_bridge/phone_call.py` (the WebSocket, the call, the ears and the voice),
`TelegramInlet.listen` / `hear`, `split_for_call` and `SentenceStream` in `telegram.py`,
`make_stream_creator` in `computer_agent.py`, and the call screen in `miniapp_page.html`. Tests:
`bridge/tests/test_phone_call.py`, `bridge/tests/test_phone_call_turns.py` and the call tests in `test_telegram.py`. Spend shows as "phone calls".
Switches: `EVE_CALL_LIVE_STT` (off; `1` with an `OPENAI_API_KEY` turns on OpenAI's live ears),
`EVE_CALL_OR_TTS_MODEL`, `EVE_CALL_OR_VOICE`, `EVE_CALL_STT_MODEL`, `EVE_CALL_LANGUAGE` (`en`: the language
calls and Voice PE presses are transcribed in. Without it, the model guesses from each press and sometimes
hears English as another language. Set it empty to let the model guess again).

## Lights

With [lights](../lights.md) set up, a **Lights** card sits at the top of the
home page:

- **All** is selected by default, so a tap changes every light at once. Pick
  a room or one light to change only those.
- **On** and **Off**, ten colour swatches (the last two are warm white and
  white), and a **Brightness** slider. The slider sends its value when you let
  go.
- The last swatch, the rainbow one, opens a **colour wheel**: the hue goes
  round the ring (red at the top) and the colour is deepest at the edge and
  white at the centre. Drag on it and the lights follow while you drag. One
  change is sent at a time, and only the newest colour waits behind it, so a
  Bluetooth light never replays the whole drag. The colour you let go on is
  always sent last. With a keyboard, the arrow keys turn the hue and set how
  deep the colour is. The wheel is [iro.js](https://github.com/jaames/iro.js)
  5.5.2 (MPL-2.0). It is copied into the package
  (`bridge/src/eve_bridge/miniapp_iro.min.js`, with its licence beside
  it) and eve serves it at `/iro.js`, never from a CDN, because the home page
  holds your signed Telegram data. A test pins the file's SHA-256. When eve
  does not serve it, the rainbow swatch does not appear and the other swatches
  still work. The reply names the nearest colour: "Lights cyan
  (#19ffff)."
- The line under the card shows each light's state when the page opens, and
  after a tap it shows what changed ("Lights green.") and which light did not
  answer, and why.

The card calls `/api/lights`, which only an owner's signed `initData` opens,
like the rest of the home page. An app's token never reaches it. The page gets
the lights' names and rooms only, never an address or a key. Without lights
the card does not appear.

## Music

With [Spotify](../spotify.md) logged in, a **Music** card sits under Lights.
It gives the phone the same controls as spotKnob:

- The album art, track, artist and album that are playing now.
- **⏮ ▶/⏸ ⏭**: previous, play or pause, next.
- A **Volume** slider for the playing device. It sends the value when you let
  go.
- **Playing on**: your Spotify Connect devices, with the playing one
  selected. Pick another to move the music there. A device that Spotify won't
  let eve control is shown as **no remote control** and can't be picked.
- A key of each device's colour. The Voice PE's ring uses these colours in
  its device picker.
- **Voice PE Spotify mode**: turns [Spotify mode](../voice-pe.md#spotify-mode)
  on the Voice PE on or off. It appears only when a Voice PE is connected as
  eve's controller.

After each tap the card reloads the player and shows a line such as
"Paused." or "Playing on Kitchen Speaker.". The card calls `/api/spotify`, and
only the owner's signed `initData` opens it. The card only controls playback:
it can't search, and an app's token never reaches it. Album art is shown only
from Spotify's image host (`i.scdn.co`). Without a Spotify login, the card
does not appear.

## TV remote

With the [Fire TV](../firetv.md) paired, a **TV** card sits under Music. It is
the TV's remote on the phone:

- A status line, such as "TV on — YouTube, volume 44%".
- A **power** button: red when the TV is on.
- App chips (YouTube, Netflix, Prime Video, Disney+, Hulu, Spotify and the
  rest of eve's app table). The app in front is highlighted.
- A round **d-pad** with **OK**, then **Back**, **Home** and **Menu**.
- **Rewind**, **Play/Pause** and **Fast-forward**.
- **Volume −**, **Mute**, **Volume +** and a **Volume** slider.

Every control is at least 44 px. A tap reacts at once with a light haptic,
taps go to the TV in order, and repeated taps on one d-pad key go as one press
with a count. After each tap the card reads the TV again and shows its line;
when the TV refuses or can't be reached, eve's line shows in red. The card
calls `/api/firetv`, and only the owner's signed `initData` opens it. An app's
token never reaches it. It can't type or search: those stay in the chat.
Without a paired TV, the card does not appear. See
[Fire TV](../firetv.md#mini-app-remote).

## Making an app

Ask in the chat ("make me a habit tracker with streaks", "build a workout
log"), or type it into **What should I build?** on the Apps tab.
`claude-opus-5-5` writes one self-contained HTML file. Its instructions cover
Telegram's Mini App runtime, how to save data with `window.eve`, a design
system, and a small reference app that itself passes the phone check. Each
request also carries today's date, the local time and time zone
(`EVE_TIMEZONE`), and a guess at units and currency from
`EVE_LOCATION`.

A build is a loop:

1. **Write.** Claude writes the file, and 2 to 4 journeys: the app's core
   purpose as steps and the text it must show afterwards (below).
2. **Check in code.** The file must call `eve.load` and `eve.save`, have a
   `<title>`, and load nothing except from `cdn.jsdelivr.net` and
   `telegram.org`. A library from jsdelivr must be an npm package at an exact
   version (`/npm/chart.js@4.4.1/…`): a range, `@latest`, no version or a
   GitHub path (`/gh/…`) can change after the app was tested. This one is a
   gate, not a hint: a version that loads anything else is never saved.
3. **Use it on a phone.** The phone check (below) opens the app and uses it.
   Then Jev walks the journeys (below).
4. **Repair.** If anything went wrong, the problems go back to Claude as a
   numbered list, each with the tap that caused it, and Claude answers with
   edits to the file. A journey that failed is one of those problems. There
   are at most 2 repair rounds.
5. **Keep the best.** The version with the fewest problems is saved. A version
   the phone check really ran on always beats one it could not run on.

On the Apps tab the progress line shows the stage in words, with a clock:
"Thinking it through…", "Writing it… 12k" (characters so far), "Testing it on
a phone…", "Fixing 2 problems…". When it is done, the line says "Ready: ‹app›.
Tested on a phone.", or names the first thing that may still not work. A clean
app opens by itself when you are still on the page and not in the middle of
something else (an app's change chat, or words typed for the next build);
otherwise an **Open ‹app›** button waits under the line. What you typed while
it was building stays in the box.

If the page goes away mid-build (the phone locks, you close Telegram), the
build goes on and its result comes to the chat instead. Opened again, the page
shows "Still building …" with the clock, holds **Build it** and the app's
**…** until it is done, then says the result is in the chat.

From the chat, eve sends one short line when the new app is being tested
and one per repair round, then the picture. The picture's caption says it is a
test run with sample entries, and names the first problem left, if any. A
failed build says why (for example, the API key was refused), and a failed
change says the app is unchanged.

Measured on 2026-09-24, the first three with `tools/app_eval.py` and the last
through the Apps tab's own route (`/api/make`). All passed the check on the
first round; no repair round was needed:

| Build | Time | Cost | Phone check |
|---|---|---|---|
| New habit tracker | 226 s | $0.59 | 12 taps, 1 save (an earlier checker) |
| New habit tracker | 267 s | $0.66 | 25 taps, 8 saves |
| Change: "add a monthly calendar view for each habit, and a best-streak number" | 183 s | $0.58 | 18 taps, 8 saves |
| New tip splitter | 156 s | $0.41 | 11 taps, 11 saves |

## Changing, undoing, renaming and deleting

On the Apps tab, each app shows its icon and a short description. Tap **…**
next to it for:

- **Change.** Opens the app's change chat (below). Claude gets the current
  file, what you asked of the app before, and a shortened sample of its saved
  data, and returns the whole new file. The new version must open your
  existing data as it is; the phone check starts from that data, so a change
  that breaks it is caught.
- **Rename.** Only the name changes. A renamed app keeps your name through
  later changes. Its address stays the same.
- **Undo last change.** Shown only when there is an earlier version. The app
  goes back to the version before its last change. The version you had is not
  kept, so the page asks first. Saved data is not touched. If your data was
  moved to a newer shape since that change, the question says entries saved
  since then may not open in the older version (changing the app again brings
  them back). While an undo is on its way the sheet's buttons wait, and the
  server refuses a second undo sent for the same version.
- **Delete.** The page asks first (Telegram's popup, with a red **Delete**),
  then moves the app's whole folder to eve's trash,
  `~/.config/eve-bridge/apps/.trash/<name>-<date>-<time>/`. Nothing is
  erased: **Undo** next to "Deleted ‹app›." brings it back with its data, for
  15 seconds; after that, ask in the chat ("bring back the reading list").

The same things work from the chat: "add a weekly chart to my habit tracker",
"undo the last change to the habit tracker", "rename the workout log to Gym",
"delete the reading list", "bring back the reading list". From the chat,
undo, rename and delete happen without a question, but only on an app the
words name for sure: its exact name, or the only app whose name has every word
you used. "Delete the water app" with a Water Log and a Water Plants does
nothing, and eve asks which one. A change ("add a chart to the water log")
still takes the closest match, since the old version stays for undo.

While an app is being changed, delete, rename and undo for that app are
refused until the change is done, whether the change was started from the chat
or from the Apps tab.

## Each app's change chat

Owner, 2026-09-24: "a chat box for each app where you keep refining it (make
the buttons bigger, add categories)". Every app has one. It is a view of the
home page, `/?chat=<name>`, and there are four ways in:

- **Inside the app, Telegram's "…" menu** has a **Settings** item (Telegram
  7.0 and later) that opens the app's chat.
- **Inside the app, a small ✎ pencil** in the top right corner, beside the
  app's title, does the same. It is a 44 px target inside the safe area, it
  scrolls away with the page, and it stays clear of the MainButton at the
  bottom. It lives in a closed shadow root with its position set inline and
  `!important`, so the app's own CSS or a re-rendered page cannot hide it by
  accident. The corner belongs to the app first: while one of the app's own
  controls sits under the pencil (a header's Settings button, a dialog's
  close button), the pencil hides, so a tap there reaches the app. The maker
  is told to keep the top-right 56 by 56 px of every view free, and the phone
  check reports a control there as a problem. The app only navigates to the
  chat; it is never given the power to change apps or spend.
- **Change** in the app's **…** sheet on the Apps tab.
- **A Change button in the chat with eve.** After a change from the chat,
  the message keeps its **Open ‹app›** button and gets a second one,
  **Change**, that opens the same view. So does the message a change started
  on the page sends when the page went away before it finished. A change that
  failed gets the **Change** button too, so its chat is one tap away.

The view shows the app's icon and name with an **Open** button, the number of
requests and what the app has cost so far, and a thread:

- **Your requests**, with when you asked. An undo shows as a short note.
- **How each build went:** the version it made ("Changed. This is version
  3."), whether it was tested on a phone and how many of Jev's journeys passed
  ("Tested on a phone; 3 of 3 journeys passed."), the first thing that may
  still not work, and its cost. A journey's problem is one plain line
  ("Couldn't tap "Add expense" in "Split a dinner"."); the details go to
  Claude's repair round, not to you. A change that saved nothing says so
  ("Couldn't change it: … The app is unchanged."), with the reason (Claude's
  rate limit, a refused key) and what it cost, including rounds that ran
  before the failure. The reason also stays under the thread.
- **The change running now**, with the same stages in words and a clock as
  the Apps tab. A change started from the chat with eve, or before the page
  was reopened, shows too, and the thread fills in when it ends. A change of
  another app shows only in that app's chat. While a change runs the thread
  is checked every 4 s but redrawn only when something in it changed, so a
  place you scrolled to, or focus on a button, is kept.

Type the next change in **What should change?** at the bottom and press
**Change** (or Enter; Shift+Enter is a new line). It runs the same build, with
the same stages, as a change from anywhere else. When it is done, an **Open
‹app›** button under the result goes straight back into the app, and **Undo
this change** sits beside it when the change may have broken something. If the
build did not start (another build is running), your words go back into the
box. Only one build runs at a time, so **Change** waits while **Build it** or
another app's change is running, and says so. If the app was deleted
meanwhile, the chat says so and stops taking changes.

The composer follows the phone's visible height (`visualViewport`), so the
keyboard never covers it. Everything in the thread, your words, Claude's and
the check's, is set as text, never as HTML. Telegram's Back button, or **‹**,
goes back to the list of apps. The view works in the light and dark themes.

The thread comes from `POST /api/apps/<name>/history` (owner only, signed
`initData`, like every other home-page call). It returns the app (with a fresh
Open address), the thread, and the change running on it now, if any. Each
build is recorded in the app's `meta.json` under `builds` (the newest 30):
when, the request, whether a page was saved, the version number, the phone
check's line, the journeys result (passed, total, or why they did not run),
how many problems are left and the first two, cost, time and repair rounds.
A build that saved nothing is recorded there too, but not added to the
requests, which tell the next build what the app was asked to do. Apps built
before this was added show their earlier requests without results, under one
note that says their results were not kept then.

Each app lives in `~/.config/eve-bridge/apps/<name>/`:

| File | What it holds |
|---|---|
| `index.html` | The app |
| `versions/<n>.html` | Up to 10 earlier versions, newest has the highest number |
| `meta.json` | Its name, icon, description, dates, what you asked for, its version number, how each build went (for its change chat), and the data version (`v`) each earlier version was written for |
| `data.json` | Its saved data |

Apps save through `window.eve`, which is added to every app:
`await eve.load()` returns the last saved value, and
`await eve.save(value)` stores any JSON up to 1 MB. Every load and save
carries the app's own key (see "An app is fenced in" below), so an app reads
and writes its own data and nobody else's.

Telegram's Back button always leads somewhere: inside an app it goes back
through the app's own screens, and on the app's first screen it goes back to
eve's list of apps, where Change, Undo and the other apps are. The pencil
and Telegram's Settings item go to the app's change chat.

## The phone check

`app_check.py` opens the app in headless Chromium at an iPhone's size (390 by
844, with touch), sandboxed exactly as the server serves it, so browser storage
fails there as it does on the phone. It stands in for Telegram (theme colors, MainButton,
BackButton, haptics, and popups that answer yes) and for `window.eve`, whose
storage is kept in Python so a reload cannot lose it. Then it uses the app
the way a person would in the first minute:

- It fills every visible field, and taps every button, checkbox and tab it can
  reach (at most 40 taps, and at most 3 of a row of look-alike buttons). It
  presses the MainButton when a view is used up, and Back to leave it. Delete
  and reset buttons come last, after the rest of the view has been used.
- It opens the app again with the data it saved, and takes a picture. Then it
  opens it once more at a small Android phone's width (360).
- The picture sent to the chat is the last screen of the first journey that
  passed (below): the app doing what it is for, such as the splitter showing
  who owes whom. With no journey passed, it is the reopened screen above.

What counts as a problem is what a person would see: an error on the page, a
tap that reloads the whole page, a request to a host the app may not reach, a
library that didn't load, a page wider than the phone or laid out for a
desktop, an app that never saves after being used, an app that freezes
(nothing finishes within 60 s), an app that crashes the browser tab (memory
that grows without end), an app that opens a WebSocket, and a control of the
app's under eve's Change pencil (the top-right 56 by 56 px). A crash keeps
everything found before it. Only a checker that could not start at all counts
as "not tested".

It needs Playwright and Chromium in the daemon's environment. Without them the
check reports "not tested" and the build is saved after the code checks only.
A correct small app takes about 3.5 s to check; the habit trackers above took
6 to 9 s.

## Jev verifies: does the app do what it is for?

Owner, 2026-09-24: "Can u use jev as the verification layer instead of
claude". The phone check above fills every field and taps every button
blindly. It finds crashes and apps that never save, but it cannot tell whether
an expense splitter shows who owes whom. That is how an expense splitter
passed the check and still "doesn't work".

So the work is divided:

- **Claude writes and repairs the code.** With each app it writes 2 to 4
  **journeys**, in a ```` ```journeys ```` block after the HTML: the steps a
  person takes (`tap "Add expense"`, `type "30" into "Amount"`, `choose "Sam"
  in "Paid by"`), each from a fresh app with no data, and the literal text that
  must be on screen afterwards (`"Alex owes Sam $15.00"`): the result itself,
  never a toast or a confirmation line. A change returns the journeys updated
  too. A block that is empty or does not parse keeps the journeys the app
  had, and is a problem until a good one comes; a repair round's block never
  drops a journey (one it leaves out is still walked). They are kept as `journeys.json` beside the page
  and versioned with it, so Undo restores both.
- **Jev uses the app and judges it.** Jev is TypeSafe's hosted typed-decision
  model (`jev.py`), on this Mac's route `EVE_JEV_ROUTE` (OpenRouter).
  After the scripted pass, each journey runs in its own fresh copy of the same
  sandbox. At each step Jev gets the step and the labelled controls on screen
  that fit the step (a tap is offered buttons, rows and tabs; a type is
  offered fields), and answers in one request: a **choice** of the control,
  and an absolute **noul**, "is a control with this label on screen at all?".
  Code acts only when both clear their cut-offs. A **choose** on anything but
  a `<select>` (a dropdown the app draws, chips) opens the control, then picks
  the option by its text on the next screen; a radio group named by its
  legend is picked by the option's text straight away. A control the app
  re-rendered while Jev answered (a clock, a running timer) is found again by
  its label and used, not blamed on the app. After the last step, one
  request asks one noul per expectation over the screen's text. Code then
  vetoes a "shown" for a number that is not on the screen, because Jev is
  weak at counting, and for words that are there but not in the
  expectation's order (`Streak ended · best 1 day` for `1-day streak`).
- **Code decides.** A journey that does not go through becomes a problem in
  the same report, such as `Journey "Split a dinner" failed at step 3 of 7:
  could not find "Add expense" …` or `… the screen does not show "Alex owes
  Sam $15.00". The screen showed: …`. The repair round sends it to Claude. The
  summary line counts journeys: `passed: 4 fields filled, 12 taps, 3 saves,
  journeys 3/3`.

A page that breaks under one journey (a crashed tab, an app stuck in a loop:
each read of the page waits at most 5 s) fails that journey; the others
still count. Jev being down never blocks a build. A failed request is asked
once more; with no key, no network or a timeout after that, the check still
returns and says `journeys not run (why)`, and a journey that had already
finished and failed is still a problem. An app that hung in the scripted pass
is not walked at all. An app built before
journeys has none, and the scripted check runs alone.
`EVE_APP_JOURNEYS=0` turns journeys off.

### How the cut-offs were measured

Jev is a literal reader and loses accuracy in a large state, so each question
is asked the way it is built for. The label is inside the question, the
controls are in the state, and an expectation is judged against a shortlist of
the screen: the lines that share a word or number with it, plus their
neighbours. Before that shortlist, Jev missed 3 of 54 texts that were really
on screen, all deep in screens of 66 to 73 lines.

The cut-offs were fitted with `tools/journey_eval.py` on the 17 apps built
overnight (11 kinds). Claude wrote journeys for them under the maker's own
prompt (63 journeys). The journeys were walked with a code oracle acting in
Jev's place, and every step the oracle could not place was labelled by hand
(15 of 349: list rows named by the words before their details). Negatives were
built on the same screens: another app's label, the right control removed,
an expectation with a number changed or from another app, and each
expectation read against the first screen, before any step ran. The fit used
6 kinds, allowing zero wrong acts and zero false "shown". The other 5 kinds
were held out:

| Question | Held out | Result |
|---|---|---|
| Which control is the step (present ≥ 0.8, choice ≥ 0.9, margin ≥ 0.3) | 367 steps: 127 real, 240 with no such control | 124 right, 3 wrong: precision and recall 97.6% |
| The choice alone, no noul gate | same | 11 wrong, precision 92.0% |
| Is the text shown (noul ≥ 0.6, plus the number and word-order vetoes) | 181: 54 shown, 127 not | 53 of 54, 0 false: precision 100%, recall 98.1% |
| The noul alone at 0.6 | same | 53 of 54, 6 false (5 of them rewords) |

The text cut-off was refitted after a live build let `1-day streak` pass over
`Streak ended · best 1 day` (Jev read 0.56 to 0.62 across ten asks; the cut
was 0.4). The calibration had no case where the words and numbers stay and
the meaning goes, so two were added on the same screens: two names or numbers
trading places ("swap"), and the phrase broken up around its number
("reword"). The lowest cut with no false "shown" on the fitted kinds moved
from 0.4 to 0.6. Jev still read up to 0.89 for a reword, so the word-order
veto is what stops that class. The labelled positives are the ones a
normalized text match found (plus hand labels for number formats), and that
plain match also scores 54 of 54 with 0 false here: on this set Jev adds no
catch the code does not make.

The 3 wrong acts were all one case: a habit's row was removed, and its check
toggle "Drink water: done today" was taken for "Drink water". The held-out
misses were read to design the shortlist and the veto, so by this project's
rule the held-out figures are tuning numbers. The unread measurement is the
fresh set below.

**The fresh set.** Six apps the new maker built with its own journeys (two
expense splitters, two habit trackers, a plant watering tracker, a savings goal
tracker) were scored once under the shipped cut-offs, with nothing fitted: 21
journeys, 411 steps and 132 expectations. Steps: 141 of 141 right, and none of
the 270 steps with a missing or removed control was acted on. Expectations:
45 of 45 shown, and none of the 87 absent texts passed. Scored again after the
refit, with the swap and reword cases added (166 expectations): 45 of 45
shown, none of the 121 absent passed; without the word-order veto, 11 of the
25 rewords would have. Every step on this set
matched its label exactly, so it is easier than the calibration set, where 15
steps named a row by the words before its details.

**Live builds, 2026-09-24** (`tools/app_eval.py`, `claude-opus-5-5` at effort
high, real Jev). The last three ran on the code as shipped; the four before
them ran an hour earlier, before the last fixes to the check:

| App | Build | Claude | Repairs | Journeys | Jev (final check) |
|---|---|---|---|---|---|
| Expense splitter | 417 s | $1.02 | 0 | 4/4 | 33 calls, 247 ms p50, $0.0009 |
| Grocery list with categories | 188 s | $0.43 | 0 | 4/4 | 31 calls, 212 ms p50, $0.0009 |
| Workout log | 293 s | $0.69 | 0 | 4/4 | 33 calls, 233 ms p50, $0.0010 |
| Expense splitter (earlier) | 287 s | $0.79 | 1 (it never saved) | 4/4 | 36 calls, 225 ms p50, $0.0010 |
| Plant watering (earlier) | 220 s | $0.51 | 0 | 3/3 | 18 calls, 233 ms p50, $0.0005 |
| Habit tracker (earlier) | 200 s | $0.46 | 0 | 3/3 | 15 calls, 262 ms p50, $0.0005 |
| Savings goals (earlier) | 290 s | $0.68 | 0 | 4/4 | 34 calls, 255 ms p50, $0.0010 |

Walking the journeys added 4 to 8 s to a check. On the last three builds Jev
cost $0.0027 in all, against $2.14 for Claude.

**The negative control.** The last splitter was copied twice. In one copy, its
save-expense function was made to return before doing anything. The scripted
pass found nothing wrong with that copy: 13 fields filled, 12 taps, 7 saves.
That is how the owner's splitter passed. The journeys failed it 2 of 4: after
"Save expense" the form was still open, so "Settle up" was not on screen. The
two journeys that do not add an expense still passed. The untouched copy passed
4 of 4. With Jev's key made invalid, the same broken copy came back `passed:
… journeys not run (Jev did not answer: HTTP 401 …)`: the build is not
blocked, and nothing but the journeys would have caught it.

### From Eve Link

The Eve Link iPhone app (the Action Button and Hold to talk) makes the same call, at
`/api/stick`. That door takes the link token `/stick` makes instead of initData, and
refuses any request that carries an Origin, so no page can use it. Once paired,
the pinned message has a second button, **Update Eve Link**, which is edited to the
tunnel's new address on every start. See [the Action Button](../action-button.md#the-link-token).

## Turning it on

```sh
brew install cloudflared
pip install -e "bridge[miniapp]"           # the Anthropic SDK, and Playwright for the phone check
python -m playwright install chromium
```

Then add these to `~/.config/eve-bridge/env` and restart the daemon:

| Setting | Default | What it does |
|---|---|---|
| `EVE_MINIAPP` | `0` | `1` turns it on. It also needs the Telegram door and `OPENROUTER_API_KEY`. |
| `OPENROUTER_API_KEY` | none | Your OpenRouter key: Claude builds the apps through OpenRouter (`anthropic/claude-opus-5.5`; [one key: OpenRouter](../openrouter.md)). It stays on the Mac and never reaches the phone. `ANTHROPIC_API_KEY` is no longer read. |
| `EVE_MINIAPP_MODEL` | `claude-opus-5-5` | The Claude model for building. |
| `EVE_MINIAPP_MAKE_EFFORT` | `high` | Effort when building or changing an app. |
| `EVE_MINIAPP_DAILY_USD` | none | Unset or `0`: no cap, spend is only tracked. A number above 0 is a daily cap on building. |
| `EVE_TIMEZONE`, `EVE_LOCATION` | the Mac's time zone; none | Used in each build's context (dates, units, currency). |
| `EVE_CLOUDFLARED` | found on `PATH` | The path to `cloudflared`. |

At start the daemon logs `miniapp: live at https://….trycloudflare.com; the
pinned Open button points there`.

## Why the Cloudflare tunnel

Telegram only opens Mini Apps from a public `https://` address. eve runs on
your Mac, behind your home network, where the phone can't reach it directly.

The daemon serves everything on `127.0.0.1` only, and a **Cloudflare quick
tunnel** (`cloudflared tunnel --url`) gives it a public address. A quick tunnel
needs no account and no domain, but its address changes every time it starts.
So on each start the daemon edits the pinned message's button to the new
address, and when it stops the pinned message says the apps are offline.

For an address that never changes, you'd need a named Cloudflare tunnel on
your own domain, or Tailscale Funnel.

## Who can use it

Only the numeric ids in `EVE_TELEGRAM_OWNER`. Every API call (build,
list, load, save, delete, rename, undo, an app's history) must carry `initData` that
Telegram signed for this bot:

- **The signature:** the HMAC-SHA256 has to verify with the bot token
  (Telegram's `WebAppData` scheme).
- **Its age:** `auth_date` must be under 24 hours old and no more than 5
  minutes in the future.
- **Who it's from:** the user inside it has to be an owner.

Anything else gets `403`, and Claude is never called. A call from a page
that is not eve's home page (a sandboxed app, or another site) gets `403`
too, even with a valid `initData`.

### An app is fenced in

An app is code Claude wrote, plus any library it loads from jsdelivr, so it
never gets your `initData`, which answers for everything above:

- **Its own key.** An app opens as `/apps/<name>/?t=<key>`. The key is signed
  with the bot token, bound to that one app, and lasts 24 hours. It opens that
  app's page and answers its load and save, and nothing else: not the list of
  apps, not another app's data or history, not build, delete or the chat. The
  pencil and the Settings item only open the home page's change chat, which
  asks for your signed `initData` like everything else there.
- **Sandboxed.** The page is served with a Content-Security-Policy `sandbox`
  (no `allow-same-origin`), so it runs in an opaque origin: it cannot read the
  home page's storage (your chat history) or call the API as the home page.
  The same policy lets it load only inline code, Telegram's script, `eve.js`
  and `cdn.jsdelivr.net/npm/`, and connect only to its own load and save.
- **Opened through the home page.** An Open button in the chat points at
  `/?open=<name>`. The home page gets Telegram's signed sign-in, then opens the
  app with a fresh key, passing on Telegram's version, platform, theme and
  your first name, but nothing signed.
- **No key, no page.** `/apps/<name>/` without a valid key answers the same
  for every name (a redirect to `/?open=<name>`), so a link someone saw shows
  neither the app nor whether it exists. Old Open buttons keep working through
  that redirect.

## What it costs

There is no spend limit unless you set one. Owner, 2026-09-24: "no claude
limit, just track spend".

After each answer and each build round, its cost is worked out from the usage
the API reports, at list price. For `claude-opus-5-5` that's $4 per million
input tokens, $20 per million output, $0.20 per million cache reads, and for
cache writes $5 per million when the cache lasts 5 minutes or $8 per million
when it lasts an hour (the build prompt's cache). A model that isn't in the price table is charged at
the highest price, so it is never under-counted.

Every day's build total is kept in `~/.config/eve-bridge/miniapp-spend.json`
(the last 400 days), so a restart loses nothing. With
`EVE_MINIAPP_DAILY_USD` set above 0, building stops for the day once that
total is reached, and the header adds "builds $X.XX of $Y.YY".

The header's "$X.XX spent today" is **all** of eve's spend today (chat,
voice, search, builds, memory and the rest), from the daily spend meter. It is
not only the builds. Each build round is also written to that meter under
"app builder". A model with no price in `pricing.py` is recorded there without
a price, never guessed. Tap the figure to open the **Spending** view:
- today by provider and by feature
- the last 30 days as a bar chart
- this month's total
- each provider's own figure where eve can read one

It is owner-only, like every other route (`/api/spend`, signed `initData`),
and it returns totals only, never prompts. See
[what eve spends](spending.md).

A line in the chat marks each of $5, $20, $50, $100, $200, $500 and $1000
that a day's spend passes ("Claude has cost $21.40 today … Nothing is
capped"). It is a note, not a limit, so a runaway (a leaked key, a loop) is
seen the same day.

The build instructions are cached: a change made within 5 minutes of another
build reads about 7,700 tokens from cache instead of paying for them again.

Measured on 2026-09-24:
- A build or change cost $0.41 to $0.66 (the table above).
- Jev, walking the journeys: 15 to 36 requests per check, about $0.0005 to
  $0.001 per check ($0.042 per million input tokens, output free).

## What leaves the Mac

- **To Anthropic:** what you ask to be built, and, when you change an app, its current file, what you asked
  of it before, and a shortened sample of its saved data. When a build is
  repaired, the problems the phone check found go too. All of it goes over the
  Claude API with your key.
- **To Cloudflare:** a quick tunnel ends TLS at Cloudflare's edge, so traffic
  between your phone and the Mac, including app data, passes through
  Cloudflare's servers.
- **To Telegram:** the pinned message, the Open buttons and their addresses,
  and the picture of each finished app. The phone check takes it, so it shows
  the made-up entries of a journey that passed. With no journey passed it is
  the phone check's reopened screen, which, when an app was changed, shows
  your own saved data too.
  What you do inside the Mini App doesn't go through Telegram.
- **To TypeSafe, through OpenRouter (Jev):** during a build's check, the
  labels of the app's controls, a few words of its screen, each journey
  step, and the screen's text lines after the steps. Each journey starts from
  an empty app, and its values are made up: a change shows Claude a sample of
  your saved data, so a journey that holds a name or text from that data
  (and not from the app's own page) is never walked and goes back to Claude
  as a problem. The screen text holds the example values the journey typed. Turn it off with
  `EVE_APP_JOURNEYS=0`.
- **The phone check** runs on the Mac. The only requests it lets out are to
  `cdn.jsdelivr.net`, when an app loads a library from there. Every HTTP
  request goes through the check's own router, WebSockets are answered and
  closed by the check, Chromium resolves no other name (an IP address is
  refused too), and WebRTC is kept off the network. When an app is changed,
  the check runs it with your real saved data, so none of that can leave.

The bot token and the API key never appear in the log. The Bot API URL is
rewritten to `bot<token>` by the process-wide scrubber (`hide_token`).

## Limits

- **One build at a time:** a second build waits until the first is done. This
  covers **Build it** and every app's change chat together.
- **Closing the page doesn't stop a build:** it finishes, is saved, shows up
  in Your apps, and its result comes to the chat.
- **Old buttons expire:** the address changes on every restart. The pinned
  button follows it, but an Open button in an older message leads nowhere
  once the address has changed, so use the pinned one or `/apps`.
- **An app left open for over 24 hours** stops saving (its key expired);
  opening it again from eve's list gives it a new one.
- **Stopping the daemon** cuts a build that is still running; the stop waits
  at most 2 seconds for open connections.
- **Apps can't reach outside services:** an app can't call other APIs or send
  notifications. It works with what you enter and what it saves.
- **Undo is one step at a time,** and at most 10 earlier versions are kept.
